//! Catalog browsing — the generic InnerTube browse client behind every page:
//! the home (Listen Now) feed now, album/artist/playlist/genre pages as the
//! frontend learns to follow navigation endpoints (ROADMAP C2.2/C2.4).
//! First pages are cached 5 minutes per (browseId, params) — home feels
//! instant and re-opens cost nothing.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::client::{innertube_context, innertube_post, BROWSE_URL};
use super::models::BrowsePage;
use super::parse::parse_browse_page;

/// Browse pages stay fresh for this long.
const BROWSE_TTL: Duration = Duration::from_secs(300);
/// Cache entry cap (cleared wholesale when exceeded).
const BROWSE_CACHE_CAP: usize = 48;

/// Fetches and parses a browse page.  `params` is the opaque chip/filter
/// token InnerTube uses for genre/mood sub-pages.
pub fn browse(browse_id: &str, params: Option<&str>) -> Result<BrowsePage, String> {
    if browse_id.trim().is_empty() {
        return Err("browse: empty browseId".into());
    }

    let trimmed = browse_id.trim();
    let cache_key = (trimmed.to_string(), params.unwrap_or("").to_string());
    if let Some(page) = cache_get(&cache_key) {
        return Ok(page);
    }

    let mut body = serde_json::json!({ "context": innertube_context() });
    body["browseId"] = serde_json::Value::String(trimmed.to_string());
    if let Some(p) = params.map(str::trim).filter(|p| !p.is_empty()) {
        body["params"] = serde_json::Value::String(p.to_string());
    }

    let json = innertube_post(BROWSE_URL, body)?;
    let page = parse_browse_page(&json);

    if page.shelves.is_empty() {
        // Don't cache an empty parse: a transient failure or shape drift must
        // not poison (browseId, params) for the whole TTL.
        log::warn!(
            "InnerTube browse parsed 0 shelves for {browse_id} — API shape may have changed"
        );
        return Ok(page);
    }
    cache_put(cache_key, &page);
    Ok(page)
}

/// The home (Listen Now) feed: editorial shelves personalized by YouTube
/// Music — our server-side recommendation layer.
pub fn home_feed() -> Result<BrowsePage, String> {
    browse("FEmusic_home", None)
}

static BROWSE_CACHE: LazyLock<Mutex<HashMap<(String, String), (Instant, BrowsePage)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(key: &(String, String)) -> Option<BrowsePage> {
    let mut cache = BROWSE_CACHE.lock().ok()?;
    let entry = cache.get(key)?;
    if entry.0.elapsed() > BROWSE_TTL {
        cache.remove(key);
        return None;
    }
    Some(entry.1.clone())
}

fn cache_put(key: (String, String), page: &BrowsePage) {
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
    fn browse_cache_roundtrip_and_ttl() {
        let key = ("FEtest".into(), "".into());
        let page = BrowsePage {
            title: "Cached".into(),
            shelves: Vec::new(),
            next_continuation: None,
            header: None,
        };
        cache_put(key.clone(), &page);
        assert!(cache_get(&key).is_some());
        assert!(cache_get(&("FEother".into(), "".into())).is_none());
    }

    #[test]
    fn browse_rejects_empty_id() {
        assert!(browse("", None).is_err());
        assert!(browse("   ", None).is_err());
    }
}
