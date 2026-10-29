//! # Musico Online — InnerTube (YT Music) catalog + yt-dlp stream resolution
//!
//! Module map (ROADMAP Phase 0 — each concern lives in exactly one file):
//! - [`client`]  — HTTP, InnerTube auth/version, retry & throttle choke point
//! - [`models`]  — data types crossing the IPC boundary
//! - [`parse`]   — ALL response→model parsing (the only drift-sensitive file)
//! - [`search`]  — search + continuations + TTL result cache
//! - [`radio`]   — related tracks via the `next` endpoint
//! - [`stream`]  — yt-dlp resolve/download/verify/prune + duration/cover polish
//!
//! Search and radio talk to YouTube Music's InnerTube API with the `WEB_REMIX`
//! client identity (the same technique Metrolist/InnerTune use).  The client
//! version is **auto-refreshed** from the music.youtube.com homepage (24 h TTL)
//! so response-shape drift doesn't come with a frozen version stamp.
//! Playback resolves a stream via the system `yt-dlp` binary, downloads the
//! AAC (m4a) variant into a size-capped disk cache, probes the real duration
//! and thumbnail into the cover cache, and hands the file to the local
//! playback engine — the audio path stays the proven offline decoder stack.
//!
//! Robustness inventory:
//! - one retry on transient search/next failures; 5-min TTL search cache
//! - per-video download locks (no duplicate concurrent downloads)
//! - downloaded files verified (exists, > 10 KB) and duration-probed
//! - cache pruning (LRU by mtime, default 1 GB, `MUSICO_ONLINE_CACHE_MB` env override)
//! - radio picks avoid the last 12 played online tracks (static ring buffer)
//!
//! This is ToS-gray-area scraping code: it lives in this standalone repo,
//! never in the public Musico (offline) release repo.

pub mod catalog;
pub mod client;
pub mod download;
pub mod models;
pub mod parse;
pub mod radio;
pub mod search;
pub mod stream;

pub use catalog::{browse, genres, home_feed};
pub use models::{BrowsePage, ItemKind, OnlineTrack, SearchResultPage, Shelf, ShelfItem};
pub use radio::{radio_tracks, remember_radio_play};
pub use search::search;
pub use stream::{clear_cache, prune_cache_to_cap, resolve_stream, to_song_info, yt_dlp_available};
#[allow(unused_imports)]
use stream::cached_file;

#[cfg(test)]
mod live_tests {
    use super::*;

    /// End-to-end live check against YouTube Music.  Opt-in (hits the network):
    ///   MUSICO_LIVE_TESTS=1 cargo test -p musico-online live_ -- --ignored --nocapture
    #[test]
    #[ignore = "network test — run with MUSICO_LIVE_TESTS=1"]
    fn live_search_and_radio() {
        let page = search("daft punk one more time", false, None).expect("search");
        assert!(!page.tracks.is_empty(), "live search returned 0 results");
        let first = &page.tracks[0];
        assert!(!first.video_id.is_empty() && !first.title.is_empty());
        println!("live search: {} results, cont={:?}", page.tracks.len(), page.next_continuation.is_some());

        if let Some(tok) = &page.next_continuation {
            let page2 = search("daft punk one more time", false, Some(tok)).expect("continuation");
            println!("continuation: {} more results", page2.tracks.len());
        }

        let radio = radio_tracks(&first.video_id).expect("radio");
        assert!(radio.len() >= 3, "radio returned {} tracks", radio.len());
        assert!(radio.iter().all(|t| t.video_id != first.video_id), "seed excluded");
        println!("radio: {} related tracks", radio.len());
    }

    /// Live album detail page: header + playable tracklist (C2.2 path).
    #[test]
    #[ignore = "network test — run with MUSICO_LIVE_TESTS=1"]
    fn live_album_detail() {
        let page = browse("MPREb_bKCCaVsjXZ8", None).expect("album browse");
        let hdr = page.header.expect("album header parses");
        assert!(!hdr.title.is_empty());
        let playable: usize = page.shelves.iter().map(|s| s.items.iter().filter(|i| i.video_id.is_some()).count()).sum();
        assert!(playable >= 5, "album page returned {playable} playable tracks");
        println!("album: {:?} | {} playable tracks | {} shelves", hdr.title, playable, page.shelves.len());
    }

    /// Live genre grid: the Browse tab categories.
    #[test]
    #[ignore = "network test — run with MUSICO_LIVE_TESTS=1"]
    fn live_genres() {
        let items = genres().expect("genres");
        assert!(items.len() >= 50, "genres returned {} categories (ROADMAP C2.4 gate)", items.len());
        assert!(items.iter().all(|i| i.browse_id.is_some() && !i.title.is_empty()));
        println!("genres: {} categories — first: {:?}", items.len(), items[0].title);
    }

    /// Live home feed: the Listen Now shelves (editorial + personalized).
    #[test]
    #[ignore = "network test — run with MUSICO_LIVE_TESTS=1"]
    fn live_home_feed() {
        let page = home_feed().expect("home feed");
        assert!(page.shelves.len() >= 2, "home returned {} shelves", page.shelves.len());
        let items: usize = page.shelves.iter().map(|s| s.items.len()).sum();
        assert!(items >= 10, "home returned only {items} items");
        println!("home: {} shelves, {items} items — first: {:?}", page.shelves.len(), page.shelves[0].title);
    }

    /// C4.2 gate: a 15-track batch downloads unattended with ≤2 concurrent
    /// resolves, all tracks landing done in the persistent batch record.
    #[test]
    #[ignore = "network + yt-dlp heavy — run with MUSICO_LIVE_TESTS=1"]
    fn live_batch_download_15_tracks() {
        let tracks = radio_tracks("dQw4w9WgXcQ").expect("radio tracks");
        let picked: Vec<(String, String)> = tracks
            .iter()
            .filter(|t| t.duration_secs.is_some_and(|d| (60..=600).contains(&d)))
            .take(15)
            .map(|t| (t.video_id.clone(), t.title.clone()))
            .collect();
        assert!(picked.len() >= 10, "need at least 10 normal-length tracks, got {}", picked.len());

        let dir = tempfile::tempdir().expect("tempdir");
        let store = musico_recommender::models::Store::open(dir.path().to_str().unwrap())
            .expect("store");
        let id = "batch-live-15";
        musico_recommender::batch::create(&store, id, "Live gate", &picked)
            .expect("create batch");

        let max_concurrent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mc = max_concurrent.clone();
        let inf = in_flight.clone();
        let resolve = move |vid: &str| {
            inf.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            mc.fetch_max(inf.load(std::sync::atomic::Ordering::SeqCst), std::sync::atomic::Ordering::SeqCst);
            let r = resolve_stream(vid, std::time::Duration::from_secs(240)).map(|_| ());
            inf.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            r
        };
        let job = musico_recommender::batch::run(&store, id, &resolve, &|_| {})
            .expect("batch run")
            .expect("job");

        println!(
            "batch: {}/{} done, {} failed, max concurrent {}",
            job.done_count(),
            job.tracks.len(),
            job.failed_ids().len(),
            max_concurrent.load(std::sync::atomic::Ordering::SeqCst),
        );
        assert!(
            max_concurrent.load(std::sync::atomic::Ordering::SeqCst) <= musico_recommender::batch::BATCH_CONCURRENCY,
            "concurrency cap violated"
        );
        assert_eq!(job.done_count(), job.tracks.len(), "all tracks must download");
        // Clean the cache so the run leaves no residue for other tests.
        for vid in picked.iter().map(|(v, _)| v) {
            if let Some(p) = cached_file(vid) {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// C3.2 gate: advancing into an already-cached online track must take
    /// < 1 s — the pre-resolve worker turns the advance into exactly this
    /// cache hit.  Downloads one real track, then times the cache-hit path.
    #[test]
    #[ignore = "network + yt-dlp test — run with MUSICO_LIVE_TESTS=1"]
    fn live_cached_resolve_under_a_second() {
        let tracks = radio_tracks("dQw4w9WgXcQ").expect("radio tracks");
        // Duration-bounded pick: radio shuffles and can return multi-hour
        // mixes — resolve a normal-length track or the gate hangs ~480 s.
        let vid = tracks
            .iter()
            .find(|t| t.duration_secs.is_some_and(|d| (60..=600).contains(&d)))
            .or_else(|| tracks.first())
            .expect("at least one related track")
            .video_id
            .clone();
        let t0 = std::time::Instant::now();
        resolve_stream(&vid, std::time::Duration::from_secs(240)).expect("first resolve");
        let cold = t0.elapsed();
        let t1 = std::time::Instant::now();
        resolve_stream(&vid, std::time::Duration::from_secs(240)).expect("cached resolve");
        let warm = t1.elapsed();
        println!("cold resolve {cold:?}, cached {warm:?}");
        assert!(
            warm < std::time::Duration::from_secs(1),
            "cached resolve took {warm:?} — C3.2 gate is < 1 s"
        );
        // Leave no residue: the unit prune test assumes a clean cache dir.
        let path = cached_file(&vid);
        if let Some(p) = path {
            let _ = std::fs::remove_file(p);
        }
    }
}
