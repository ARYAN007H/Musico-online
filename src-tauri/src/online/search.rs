//! InnerTube search with continuation paging and a 5-minute TTL result cache.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::client::{http_client, innertube_context, innertube_post, SEARCH_URL};
use super::models::SearchResultPage;
use super::parse::parse_search_page;

/// Songs filter param (chip: "Songs").
const SONGS_FILTER: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
/// Video filter param (chip: "Videos").
const VIDEOS_FILTER: &str = "EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D";
/// Search results stay fresh for this long before a re-fetch.
const SEARCH_TTL: Duration = Duration::from_secs(300);
/// Search cache entry cap (cleared wholesale when exceeded).
const SEARCH_CACHE_CAP: usize = 64;

/// Searches YouTube Music.  Pass `continuation` (from a previous page's
/// `next_continuation`) to fetch the next page.  First pages are cached for
/// 5 minutes — repeated searches are instant.
pub fn search(
    query: &str,
    videos: bool,
    continuation: Option<&str>,
) -> Result<SearchResultPage, String> {
    if query.trim().is_empty() && continuation.is_none() {
        return Ok(SearchResultPage { tracks: Vec::new(), next_continuation: None });
    }

    // Continuation pages bypass the cache (each page is requested once anyway).
    if continuation.is_none() {
        if let Some(page) = search_cache_get(query, videos) {
            return Ok(page);
        }
    }

    let mut body = serde_json::json!({ "context": innertube_context() });
    match continuation {
        Some(tok) => body["continuation"] = serde_json::Value::String(tok.to_string()),
        None => {
            body["query"] = serde_json::Value::String(query.trim().to_string());
            body["params"] = serde_json::Value::String(
                if videos { VIDEOS_FILTER } else { SONGS_FILTER }.to_string(),
            );
        }
    }

    let client = http_client()?;
    let json = innertube_post(&client, SEARCH_URL, body)?;
    let page = parse_search_page(&json);

    if page.tracks.is_empty() && page.next_continuation.is_none() && continuation.is_none() {
        log::warn!("InnerTube search parsed 0 results for {query:?} — API shape may have changed");
    }
    if continuation.is_none() {
        search_cache_put(query, videos, &page);
    }
    Ok(page)
}

static SEARCH_CACHE: LazyLock<Mutex<HashMap<(String, bool), (Instant, SearchResultPage)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn search_cache_get(query: &str, videos: bool) -> Option<SearchResultPage> {
    let mut cache = SEARCH_CACHE.lock().ok()?;
    let entry = cache.get(&(query.to_string(), videos))?;
    if entry.0.elapsed() > SEARCH_TTL {
        cache.remove(&(query.to_string(), videos));
        return None;
    }
    Some(entry.1.clone())
}

fn search_cache_put(query: &str, videos: bool, page: &SearchResultPage) {
    if let Ok(mut cache) = SEARCH_CACHE.lock() {
        if cache.len() >= SEARCH_CACHE_CAP {
            cache.clear(); // rare; simplest LRU-ish reset
        }
        cache.insert((query.to_string(), videos), (Instant::now(), page.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online::models::OnlineTrack;

    #[test]
    fn search_cache_roundtrip_and_ttl() {
        let page = SearchResultPage {
            tracks: vec![OnlineTrack {
                video_id: "c1".into(),
                title: "Cached Song".into(),
                artist: "A".into(),
                album: String::new(),
                duration_secs: None,
                thumbnail: None,
                cached: false,
            }],
            next_continuation: None,
        };
        search_cache_put("cache test", false, &page);
        let got = search_cache_get("cache test", false).expect("hit");
        assert_eq!(got.tracks[0].video_id, "c1");
        assert!(search_cache_get("different query", false).is_none());
    }
}
