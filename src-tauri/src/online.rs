//! # Musico Online — InnerTube (YT Music) search + yt-dlp stream resolution
//!
//! Search talks to YouTube Music's InnerTube API with the WEB_REMIX client
//! identity (the same technique Metrolist/InnerTune use).  Playback resolves
//! a stream via the system `yt-dlp` binary, downloads the AAC (m4a) variant
//! into a disk cache, and hands the file to the local playback engine — so
//! the entire audio path stays the proven offline decoder stack.
//!
//! This is ToS-gray-area scraping code: it lives in the private Musico-online
//! repo, never in the public Musico release.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::state::AppState;
use musico_playback::SongInfo;

// ---------------------------------------------------------------------------
// InnerTube constants
// ---------------------------------------------------------------------------

/// Public web client key used by music.youtube.com itself (not a secret).
const INNERTUBE_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const INNERTUBE_CLIENT: &str = "WEB_REMIX";
const INNERTUBE_VERSION: &str = "1.20241001.01.00";
const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search";

/// Songs filter param (chip: "Songs") — base64 of the search filter proto.
const SONGS_FILTER: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
/// Video filter param (chip: "Videos").
const VIDEOS_FILTER: &str = "EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D";

/// User-Agent — plain browser UA; InnerTube does not care beyond presence.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A track found via online search.  Sent to the frontend as-is and back on
/// play, so no server-side result cache is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineTrack {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_secs: Option<u32>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl OnlineTrack {
    /// Stable song id in the local stack — distinct namespace from the
    /// path-hash ids of the local library.
    pub fn song_id(&self) -> String {
        format!("yt:{}", self.video_id)
    }

    /// Stream URL for yt-dlp.
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.video_id)
    }
}

// ---------------------------------------------------------------------------
// Search (InnerTube)
// ---------------------------------------------------------------------------

/// Searches YouTube Music for songs matching `query`.
pub fn search(query: &str, videos: bool) -> Result<Vec<OnlineTrack>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": INNERTUBE_CLIENT,
                "clientVersion": INNERTUBE_VERSION,
                "hl": "en",
                "gl": "US",
            }
        },
        "query": query.trim(),
        "params": if videos { VIDEOS_FILTER } else { SONGS_FILTER },
    });

    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let url = format!("{SEARCH_URL}?prettyPrint=false&key={INNERTUBE_KEY}");
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("Search request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("YouTube Music returned HTTP {status}"));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("Bad search response: {e}"))?;

    let tracks = parse_search_response(&json);
    if tracks.is_empty() {
        log::warn!("InnerTube search parsed 0 results for {query:?} — API shape may have changed");
    }
    Ok(tracks)
}

/// Walks `tabbedSearchResultsRenderer → … → musicShelfRenderer` contents and
/// pulls out song items.  Tolerant to missing fields; skips what it can't
/// parse instead of failing the whole search.
fn parse_search_response(json: &serde_json::Value) -> Vec<OnlineTrack> {
    let mut tracks = Vec::new();

    let Some(tabs) = json
        .pointer("/contents/tabbedSearchResultsRenderer/tabs")
        .and_then(|v| v.as_array())
    else {
        return tracks;
    };

    for tab in tabs {
        let Some(shelf_items) = tab
            .pointer("/tabRenderer/content/sectionListRenderer/contents")
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        for section in shelf_items {
            let Some(items) = section
                .pointer("/musicShelfRenderer/contents")
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            for item in items {
                if let Some(renderer) = item.get("musicResponsiveListItemRenderer") {
                    if let Some(track) = parse_list_item(renderer) {
                        tracks.push(track);
                    }
                }
            }
        }
    }
    tracks
}

/// Parses one `musicResponsiveListItemRenderer`:
/// - flexColumns[0]: title runs (first run carries the watchEndpoint videoId)
/// - flexColumns[1]: "Artist • Album • 3:45" (album may be absent; the last
///   dot-separated part is the duration when it looks like m:ss)
/// - thumbnail.musicThumbnailRenderer: ascending-size thumbnail list
fn parse_list_item(renderer: &serde_json::Value) -> Option<OnlineTrack> {
    let flex = renderer.get("flexColumns")?.as_array()?;

    // ---- Title + videoId ----
    let first = flex.first()?.pointer("/musicResponsiveListItemFlexColumnRenderer/text")?;
    let title = runs_text(first)
        .split('\n')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }

    let mut video_id = renderer
        .pointer("/playlistItemData/videoId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if video_id.is_none() {
        if let Some(runs) = first.get("runs").and_then(|v| v.as_array()) {
            for run in runs {
                if let Some(id) = run
                    .pointer("/navigationEndpoint/watchEndpoint/videoId")
                    .and_then(|v| v.as_str())
                {
                    video_id = Some(id.to_string());
                    break;
                }
            }
        }
    }
    let video_id = video_id?;
    if video_id.is_empty() {
        return None;
    }

    // ---- Artist / album / duration ----
    let (mut artist, mut album, mut duration_secs) = (String::new(), String::new(), None);
    if let Some(second) = flex
        .get(1)
        .and_then(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text"))
    {
        let joined = runs_text(second);
        let parts: Vec<&str> = joined.split(" • ").map(str::trim).collect();
        if let Some(first_part) = parts.first() {
            artist = first_part.to_string();
        }
        // The last time-like part is the duration; whatever sits between the
        // artist and the duration is the album.
        let has_duration = parts.last().and_then(|p| parse_duration(p));
        if let Some(secs) = has_duration {
            duration_secs = Some(secs);
        }
        let middle_end = if has_duration.is_some() {
            parts.len().saturating_sub(1)
        } else {
            parts.len()
        };
        let middle: Vec<&str> = parts.iter().skip(1).take(middle_end.saturating_sub(1)).copied().collect();
        if !middle.is_empty() {
            album = middle.join(" • ");
        }
    }

    // ---- Thumbnail (largest variant) ----
    let thumbnail = renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|thumbs| thumbs.last())
        .and_then(|t| t.get("url"))
        .and_then(|u| u.as_str())
        .map(str::to_string);

    Some(OnlineTrack {
        video_id,
        title,
        artist,
        album,
        duration_secs,
        thumbnail,
    })
}

/// Concatenates the text of all runs in a `runs` object (or returns
/// `simpleText` when the response uses that shape).
fn runs_text(text_obj: &serde_json::Value) -> String {
    if let Some(simple) = text_obj.get("simpleText").and_then(|v| v.as_str()) {
        return simple.to_string();
    }
    text_obj
        .get("runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Parses "3:45" / "1:02:03" into seconds.
fn parse_duration(s: &str) -> Option<u32> {
    let mut total = 0u32;
    for part in s.split(':') {
        let n: u32 = part.trim().parse().ok()?;
        total = total * 60 + n;
    }
    (total > 0).then_some(total)
}

// ---------------------------------------------------------------------------
// Stream resolution (yt-dlp)
// ---------------------------------------------------------------------------

/// Disk cache for downloaded streams: ~/.cache/musico/online/{videoId}.m4a
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("musico")
        .join("online")
}

fn cached_file(video_id: &str) -> Option<PathBuf> {
    let dir = cache_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip partial downloads and metadata sidecars.
            if name.starts_with(video_id)
                && !name.ends_with(".part")
                && !name.ends_with(".ytdl")
            {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Returns `true` when the yt-dlp binary is available and runnable.
pub fn yt_dlp_available() -> bool {
    Command::new("yt-dlp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Resolves `video_id` to a local audio file: returns the cached file when
/// present, otherwise runs yt-dlp (best AAC, falling back to best audio) and
/// returns the downloaded path.  Blocks for the duration of the download —
/// call from a worker thread.  Hard-kills yt-dlp after `timeout`.
pub fn resolve_stream(video_id: &str, timeout: Duration) -> Result<PathBuf, String> {
    if !yt_dlp_available() {
        return Err("yt-dlp is not installed — install it (e.g. pacman -S yt-dlp) to play online music".into());
    }
    if let Some(path) = cached_file(video_id) {
        return Ok(path);
    }

    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cache dir: {e}"))?;
    let template = dir.join("%(id)s.%(ext)s");
    let url = format!("https://www.youtube.com/watch?v={video_id}");

    let mut child = Command::new("yt-dlp")
        .args([
            "--no-playlist",
            "--no-progress",
            "--quiet",
            "-f",
            "140/bestaudio[ext=m4a]/bestaudio",
            "-o",
        ])
        .arg(&template)
        .arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch yt-dlp: {e}"))?;

    // Wait with a hard timeout so a wedged download can't pin the worker.
    let (tx, rx) = std::sync::mpsc::channel();
    let pid = child.id();
    std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send(status);
    });
    let status = rx
        .recv_timeout(timeout)
        .map_err(|_| {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            format!("yt-dlp timed out after {}s", timeout.as_secs())
        })?
        .map_err(|e| format!("yt-dlp wait: {e}"))?;

    if !status.success() {
        return Err(format!("yt-dlp exited with {status} — stream unavailable or region-locked"));
    }

    cached_file(video_id)
        .ok_or_else(|| "yt-dlp reported success but no file appeared in cache".to_string())
}

/// Builds a SongInfo for the playback engine from a downloaded track.
pub fn to_song_info(track: &OnlineTrack, file_path: &PathBuf) -> SongInfo {
    SongInfo {
        id: track.song_id(),
        file_path: file_path.to_string_lossy().to_string(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration_secs: track.duration_secs.map(|d| d as f32).unwrap_or(0.0),
        cover_art: None,
    }
}

/// Clears the stream cache; returns bytes removed.
pub fn clear_cache() -> Result<u64, String> {
    let dir = cache_dir();
    let mut freed = 0u64;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                freed += meta.len();
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(freed)
}

// ---------------------------------------------------------------------------
// Tests (pure parsing — no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_formats() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("0:59"), Some(59));
        assert_eq!(parse_duration("854M plays"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn song_ids_are_namespaced() {
        let t = OnlineTrack {
            video_id: "abc123".into(),
            title: "T".into(),
            artist: "A".into(),
            album: String::new(),
            duration_secs: Some(10),
            thumbnail: None,
        };
        assert_eq!(t.song_id(), "yt:abc123");
        assert_eq!(t.watch_url(), "https://www.youtube.com/watch?v=abc123");
    }

    #[test]
    fn parses_real_search_shape() {
        // Minimal fixture mirroring the live WEB_REMIX response shape
        // (verified against music.youtube.com on 2026-10-28).
        let fixture = serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {
                                            "contents": [{
                                                "musicResponsiveListItemRenderer": {
                                                    "playlistItemData": { "videoId": "wU26xVT_vBU" },
                                                    "flexColumns": [
                                                        {
                                                            "musicResponsiveListItemFlexColumnRenderer": {
                                                                "text": { "runs": [
                                                                    { "text": "One More Time", "navigationEndpoint": { "watchEndpoint": { "videoId": "wU26xVT_vBU" } } }
                                                                ]}
                                                            }
                                                        },
                                                        {
                                                            "musicResponsiveListItemFlexColumnRenderer": {
                                                                "text": { "runs": [
                                                                    { "text": "Daft Punk", "navigationEndpoint": {} },
                                                                    { "text": " • " },
                                                                    { "text": "Discovery" },
                                                                    { "text": " • " },
                                                                    { "text": "5:21" }
                                                                ]}
                                                            }
                                                        }
                                                    ],
                                                    "thumbnail": {
                                                        "musicThumbnailRenderer": {
                                                            "thumbnail": { "thumbnails": [
                                                                { "url": "https://yt3.googleusercontent.com/small" },
                                                                { "url": "https://yt3.googleusercontent.com/big" }
                                                            ]}
                                                        }
                                                    }
                                                }
                                            }]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });

        let tracks = parse_search_response(&fixture);
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.video_id, "wU26xVT_vBU");
        assert_eq!(t.title, "One More Time");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.album, "Discovery");
        assert_eq!(t.duration_secs, Some(321));
        assert_eq!(t.thumbnail.as_deref(), Some("https://yt3.googleusercontent.com/big"));
    }

    #[test]
    fn parse_skips_malformed_items() {
        let fixture = serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": { "tabs": [{
                    "tabRenderer": { "content": { "sectionListRenderer": { "contents": [{
                        "musicShelfRenderer": { "contents": [
                            { "musicResponsiveListItemRenderer": { "flexColumns": [] } },
                            { "garbage": true }
                        ]}
                    }]}}}
                    }]}
            }
        });
        assert!(parse_search_response(&fixture).is_empty());
    }
}
