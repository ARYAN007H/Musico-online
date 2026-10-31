//! Catalog browsing — the generic InnerTube browse client behind every page:
//! the home (Listen Now) feed now, album/artist/playlist/genre pages as the
//! frontend learns to follow navigation endpoints (ROADMAP C2.2/C2.4).
//! Cache policy is stale-while-revalidate: fresh for 5 minutes, then pages
//! are served stale (up to 6 h) while ONE background refresh per key runs —
//! reopening Home is always instant, and a transient bad re-parse can't
//! blank a page that has a good stale copy.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::client::{innertube_context, innertube_post, BROWSE_URL};
use super::models::{BrowsePage, ShelfItem};
use super::parse::{parse_browse_page, parse_genre_page};

/// Browse pages stay fresh for this long.
const BROWSE_TTL: Duration = Duration::from_secs(300);
/// How long past the TTL a stale page may still be served while a background
/// refresh runs.  Editorial catalog pages change slowly; instant beats fresh.
const BROWSE_STALE_MAX: Duration = Duration::from_secs(6 * 3600);
/// Cache entry cap (cleared wholesale when exceeded).
const BROWSE_CACHE_CAP: usize = 48;

/// What the cache holds for a key right now.
#[derive(Debug)]
enum CacheHit<T> {
    Fresh(T),
    Stale(T),
    Miss,
}

/// Pure staleness decision — unit-tested without a clock.  Fresh under the
/// TTL, stale-serveable up to TTL + STALE_MAX, dead beyond that.
fn is_stale_serveable(age: Duration) -> bool {
    age > BROWSE_TTL && age <= BROWSE_TTL + BROWSE_STALE_MAX
}

/// Fetches and parses a browse page.  `params` is the opaque chip/filter
/// token InnerTube uses for genre/mood sub-pages.
pub fn browse(browse_id: &str, params: Option<&str>) -> Result<BrowsePage, String> {
    if browse_id.trim().is_empty() {
        return Err("browse: empty browseId".into());
    }

    let trimmed = browse_id.trim();
    let cache_key = (trimmed.to_string(), params.unwrap_or("").to_string());
    match browse_cache_get(&cache_key) {
        CacheHit::Fresh(page) => return Ok(page),
        CacheHit::Stale(page) => {
            spawn_browse_refresh(cache_key, trimmed.to_string(), params.map(str::to_string));
            return Ok(page);
        }
        CacheHit::Miss => {}
    }

    let page = fetch_browse(trimmed, params)?;
    if page.shelves.is_empty() {
        // Don't cache an empty parse: a transient failure or shape drift must
        // not poison (browseId, params) for the whole TTL.  (If a stale copy
        // existed, the SWR path above already served it — this branch only
        // runs on a true miss.)
        log::warn!(
            "InnerTube browse parsed 0 shelves for {browse_id} — API shape may have changed"
        );
        return Ok(page);
    }
    browse_cache_put(cache_key, &page);
    Ok(page)
}

fn fetch_browse(browse_id: &str, params: Option<&str>) -> Result<BrowsePage, String> {
    let mut body = serde_json::json!({ "context": innertube_context() });
    body["browseId"] = serde_json::Value::String(browse_id.to_string());
    if let Some(p) = params.map(str::trim).filter(|p| !p.is_empty()) {
        body["params"] = serde_json::Value::String(p.to_string());
    }
    let json = innertube_post(BROWSE_URL, body)?;
    Ok(parse_browse_page(&json))
}

/// Keys with a background refresh currently running (single-flight per key —
/// many stale serves must not spawn a refresh stampede).
static REFRESHING: LazyLock<Mutex<HashSet<(String, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn spawn_browse_refresh(key: (String, String), browse_id: String, params: Option<String>) {
    {
        let mut g = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if !g.insert(key.clone()) {
            return;
        }
    }
    let spawn_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("browse-refresh".into())
        .spawn(move || {
            match fetch_browse(&browse_id, params.as_deref()) {
                Ok(page) if !page.shelves.is_empty() => browse_cache_put(spawn_key.clone(), &page),
                Ok(_) => log::debug!("browse refresh parsed empty for {browse_id}; keeping stale"),
                Err(e) => log::debug!("browse refresh failed for {browse_id}: {e}; keeping stale"),
            }
            REFRESHING.lock().unwrap_or_else(|e| e.into_inner()).remove(&spawn_key);
        });
    if spawned.is_err() {
        REFRESHING.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
    }
}

/// The home (Listen Now) feed: editorial shelves personalized by YouTube
/// Music — our server-side recommendation layer.
pub fn home_feed() -> Result<BrowsePage, String> {
    browse("FEmusic_home", None)
}

/// The mood/genre category grid (Browse tab): ~50 category cards, each
/// carrying the browseId+params of a shelf page the generic parser handles.
/// Cached separately from browse pages — genre responses are navigation
/// buttons, not shelves, so the generic BrowsePage cache can't hold them.
/// Same stale-while-revalidate policy as browse.
pub fn genres() -> Result<Vec<ShelfItem>, String> {
    match genres_cache_get() {
        CacheHit::Fresh(items) => return Ok(items),
        CacheHit::Stale(items) => {
            spawn_genres_refresh();
            return Ok(items);
        }
        CacheHit::Miss => {}
    }
    let items = fetch_genres()?;
    if items.is_empty() {
        // Don't cache an empty parse: shape drift or a transient failure must
        // not blank the Browse tab for the whole TTL.
        log::warn!("InnerTube genres parsed 0 categories — API shape may have changed");
        return Ok(items);
    }
    genres_cache_put(&items);
    Ok(items)
}

fn fetch_genres() -> Result<Vec<ShelfItem>, String> {
    let mut body = serde_json::json!({ "context": innertube_context() });
    body["browseId"] = serde_json::Value::String("FEmusic_moods_and_genres".into());
    let json = innertube_post(BROWSE_URL, body)?;
    Ok(parse_genre_page(&json))
}

static GENRES_CACHE: LazyLock<Mutex<Option<(Instant, Vec<ShelfItem>)>>> =
    LazyLock::new(|| Mutex::new(None));
static GENRES_REFRESHING: AtomicBool = AtomicBool::new(false);

fn genres_cache_get() -> CacheHit<Vec<ShelfItem>> {
    let Some(cache_ok) = GENRES_CACHE.lock().ok() else {
        return CacheHit::Miss;
    };
    let Some((fetched, items)) = cache_ok.as_ref() else {
        return CacheHit::Miss;
    };
    let age = fetched.elapsed();
    if age <= BROWSE_TTL {
        return CacheHit::Fresh(items.clone());
    }
    if is_stale_serveable(age) {
        return CacheHit::Stale(items.clone());
    }
    drop(cache_ok);
    if let Ok(mut g) = GENRES_CACHE.lock() {
        *g = None;
    }
    CacheHit::Miss
}

fn genres_cache_put(items: &[ShelfItem]) {
    if let Ok(mut cache) = GENRES_CACHE.lock() {
        *cache = Some((Instant::now(), items.to_vec()));
    }
}

fn spawn_genres_refresh() {
    if GENRES_REFRESHING.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("genres-refresh".into())
        .spawn(|| {
            match fetch_genres() {
                Ok(items) if !items.is_empty() => genres_cache_put(&items),
                Ok(_) => log::debug!("genres refresh parsed empty; keeping stale"),
                Err(e) => log::debug!("genres refresh failed: {e}; keeping stale"),
            }
            GENRES_REFRESHING.store(false, Ordering::Relaxed);
        });
    if spawned.is_err() {
        GENRES_REFRESHING.store(false, Ordering::Relaxed);
    }
}

static BROWSE_CACHE: LazyLock<Mutex<HashMap<(String, String), (Instant, BrowsePage)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn browse_cache_get(key: &(String, String)) -> CacheHit<BrowsePage> {
    let Some(cache_ok) = BROWSE_CACHE.lock().ok() else {
        return CacheHit::Miss;
    };
    let Some(entry) = cache_ok.get(key) else {
        return CacheHit::Miss;
    };
    let age = entry.0.elapsed();
    if age <= BROWSE_TTL {
        return CacheHit::Fresh(entry.1.clone());
    }
    if is_stale_serveable(age) {
        return CacheHit::Stale(entry.1.clone());
    }
    let page_key = key.clone();
    drop(cache_ok);
    if let Ok(mut cache) = BROWSE_CACHE.lock() {
        cache.remove(&page_key);
    }
    CacheHit::Miss
}

fn browse_cache_put(key: (String, String), page: &BrowsePage) {
    if let Ok(mut cache) = BROWSE_CACHE.lock() {
        if cache.len() >= BROWSE_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), page.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_serving_has_hard_bounds() {
        // Fresh inside the TTL.
        assert!(!is_stale_serveable(Duration::from_secs(0)));
        assert!(!is_stale_serveable(BROWSE_TTL));
        // Stale-serveable window: (TTL, TTL + STALE_MAX].
        assert!(is_stale_serveable(BROWSE_TTL + Duration::from_secs(1)));
        assert!(is_stale_serveable(BROWSE_TTL + BROWSE_STALE_MAX));
        // Beyond the window the entry is dead — a real fetch must happen.
        assert!(!is_stale_serveable(BROWSE_TTL + BROWSE_STALE_MAX + Duration::from_secs(1)));
    }

    #[test]
    fn browse_cache_roundtrip_and_ttl() {
        let key = ("FEtest".into(), "".into());
        let page = BrowsePage {
            title: "Cached".into(),
            shelves: Vec::new(),
            next_continuation: None,
            header: None,
        };
        browse_cache_put(key.clone(), &page);
        assert!(matches!(browse_cache_get(&key), CacheHit::Fresh(_)));
        assert!(matches!(browse_cache_get(&("FEother".into(), "".into())), CacheHit::Miss));
    }

    #[test]
    fn browse_rejects_empty_id() {
        assert!(browse("", None).is_err());
        assert!(browse("   ", None).is_err());
    }

    #[test]
    fn genres_cache_roundtrip() {
        let items = vec![ShelfItem {
            kind: crate::online::models::ItemKind::Unknown,
            title: "Chill".into(),
            subtitle: String::new(),
            thumbnail: None,
            video_id: None,
            browse_id: Some("FEmusic_moods_and_genres_category".into()),
            params: Some("ggSUChAK".into()),
            playlist_id: None,
        }];
        genres_cache_put(&items);
        let got = match genres_cache_get() {
            CacheHit::Fresh(items) => items,
            other => panic!("cached items survive the TTL, got {other:?}"),
        };
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "Chill");
        assert_eq!(got[0].params.as_deref(), Some("ggSUChAK"));
    }
}
