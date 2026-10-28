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

pub mod client;
pub mod models;
pub mod parse;
pub mod radio;
pub mod search;
pub mod stream;

pub use models::{OnlineTrack, SearchResultPage};
pub use radio::{radio_tracks, remember_radio_play};
pub use search::search;
pub use stream::{clear_cache, resolve_stream, to_song_info, yt_dlp_available};

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
}
