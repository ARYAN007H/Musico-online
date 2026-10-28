//! # Musico Online — InnerTube (YT Music) search/radio + yt-dlp stream resolution
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
//! This is ToS-gray-area scraping code: it lives in the private Musico-online
//! repo, never in the public Musico release.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::state::AppState;
use musico_playback::SongInfo;

// ---------------------------------------------------------------------------
// InnerTube constants
// ---------------------------------------------------------------------------

/// Public web client key used by music.youtube.com itself (not a secret).
const INNERTUBE_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const INNERTUBE_CLIENT: &str = "WEB_REMIX";
/// Fallback client version — used only when the homepage auto-refresh fails
/// and no previous value is cached.
const INNERTUBE_VERSION_FALLBACK: &str = "1.20261025.12.00";
const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search";
const NEXT_URL: &str = "https://music.youtube.com/youtubei/v1/next";
const HOME_URL: &str = "https://music.youtube.com/";

/// Songs filter param (chip: "Songs").
const SONGS_FILTER: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
/// Video filter param (chip: "Videos").
const VIDEOS_FILTER: &str = "EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D";

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(240);
/// Search results stay fresh for this long before a re-fetch.
const SEARCH_TTL: Duration = Duration::from_secs(300);
/// How often the live client version is re-fetched.
const VERSION_TTL: Duration = Duration::from_secs(24 * 3600);
/// Cache size cap (LRU-pruned after each download).
const CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// After pruning kicks in, shrink to this fraction of the cap (hysteresis).
const CACHE_PRUNE_TARGET: f64 = 0.8;
/// A downloaded file smaller than this is considered broken.
const MIN_FILE_BYTES: u64 = 10 * 1024;
/// Radio picks avoid this many recently played online tracks.
const RADIO_REMEMBER: usize = 12;
/// Search cache entry cap (LRU by insertion — cleared wholesale when exceeded).
const SEARCH_CACHE_CAP: usize = 64;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A track found via online search or radio.  Sent to the frontend as-is and
/// back on play, so no server-side result cache is needed.
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
    /// Already present in the local stream cache — instant playback.
    #[serde(default)]
    pub cached: bool,
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

/// One page of search results.  `next_continuation` is the opaque token for
/// the next page (pass it to [`search`]); `None` = end of results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultPage {
    pub tracks: Vec<OnlineTrack>,
    #[serde(default)]
    pub next_continuation: Option<String>,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

fn innertube_post(client: &reqwest::blocking::Client, url: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let full = format!("{url}?prettyPrint=false&key={INNERTUBE_KEY}");
    let mut last_err = String::new();
    // One retry: transient TLS/DNS/5xx blips are common enough to matter.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400));
        }
        let resp = match client.post(&full).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("request failed: {e}");
                continue;
            }
        };
        let status = resp.status();
        if status.is_server_error() {
            last_err = format!("YouTube Music returned HTTP {status}");
            continue;
        }
        if !status.is_success() {
            return Err(format!("YouTube Music returned HTTP {status}"));
        }
        return resp.json().map_err(|e| format!("bad response body: {e}"));
    }
    Err(last_err)
}

/// The live InnerTube client version, auto-refreshed from the
/// music.youtube.com homepage (24 h TTL).  Falls back to the last seen value,
/// then to [`INNERTUBE_VERSION_FALLBACK`].
pub fn client_version() -> String {
    static CACHE: LazyLock<RwLock<Option<(Instant, String)>>> =
        LazyLock::new(|| RwLock::new(None));

    if let Some((at, v)) = CACHE.read().ok().and_then(|g| g.clone()) {
        if at.elapsed() < VERSION_TTL {
            return v;
        }
    }

    let fresh = http_client().ok().and_then(|client| {
        let html = client.get(HOME_URL).send().ok()?.text().ok()?;
        extract_client_version(&html)
    });

    let value = match fresh {
        Some(v) => v,
        None => match CACHE.read().ok().and_then(|g| g.clone()) {
            Some((_, stale)) => stale,
            None => INNERTUBE_VERSION_FALLBACK.to_string(),
        },
    };
    if let Ok(mut g) = CACHE.write() {
        *g = Some((Instant::now(), value.clone()));
    }
    value
}

/// Pulls `"clientVersion":"1.20241001.01.00"` out of the homepage HTML.
/// Regex-free: find the marker, read until the closing quote.
fn extract_client_version(html: &str) -> Option<String> {
    const MARKER: &str = "\"clientVersion\":\"";
    let start = html.find(MARKER)? + MARKER.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let v = &rest[..end];
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
}

fn innertube_context() -> serde_json::Value {
    serde_json::json!({
        "client": {
            "clientName": INNERTUBE_CLIENT,
            "clientVersion": client_version(),
            "hl": "en",
            "gl": "US",
        }
    })
}

// ---------------------------------------------------------------------------
// Search (InnerTube, with continuation + TTL cache)
// ---------------------------------------------------------------------------

/// Searches YouTube Music.  Pass `continuation` (from a previous page's
/// `next_continuation`) to fetch the next page.  First pages are cached for
/// 5 minutes — repeated searches are instant.
pub fn search(query: &str, videos: bool, continuation: Option<&str>) -> Result<SearchResultPage, String> {
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

/// Parses a search response — handles BOTH the initial shape
/// (`contents.tabbedSearchResultsRenderer…`) and the continuation shape
/// (`continuationContents.musicShelfContinuation…`).
fn parse_search_page(json: &serde_json::Value) -> SearchResultPage {
    let mut tracks = Vec::new();
    let mut next_continuation = None;

    // Initial shape.
    if let Some(tabs) = json
        .pointer("/contents/tabbedSearchResultsRenderer/tabs")
        .and_then(|v| v.as_array())
    {
        for tab in tabs {
            let Some(sections) = tab
                .pointer("/tabRenderer/content/sectionListRenderer/contents")
                .and_then(|v| v.as_array())
            else { continue };
            for section in sections {
                let Some(shelf) = section.get("musicShelfRenderer") else { continue };
                collect_shelf_items(shelf, &mut tracks);
                next_continuation = shelf_continuation(shelf).or(next_continuation);
            }
        }
    }

    // Continuation shape.
    if let Some(shelf) = json.pointer("/continuationContents/musicShelfContinuation") {
        collect_shelf_items(shelf, &mut tracks);
        next_continuation = shelf_continuation(shelf).or(next_continuation);
    }

    SearchResultPage { tracks, next_continuation }
}

fn collect_shelf_items(shelf: &serde_json::Value, out: &mut Vec<OnlineTrack>) {
    let Some(items) = shelf.get("contents").and_then(|v| v.as_array()) else { return };
    for item in items {
        if let Some(renderer) = item.get("musicResponsiveListItemRenderer") {
            if let Some(track) = parse_list_item(renderer) {
                out.push(track);
            }
        }
    }
}

fn shelf_continuation(shelf: &serde_json::Value) -> Option<String> {
    shelf
        .pointer("/continuations/0/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
        .map(str::to_string)
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
        let middle: Vec<&str> =
            parts.iter().skip(1).take(middle_end.saturating_sub(1)).copied().collect();
        if !middle.is_empty() {
            album = middle.join(" • ");
        }
    }

    // ---- Thumbnail (largest variant, upscaled for cover art) ----
    let thumbnail = renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|thumbs| thumbs.last())
        .and_then(|t| t.get("url"))
        .and_then(|u| u.as_str())
        .map(|u| upscale_thumbnail(u));

    let cached = cached_file(&video_id).is_some();

    Some(OnlineTrack {
        video_id,
        title,
        artist,
        album,
        duration_secs,
        thumbnail,
        cached,
    })
}

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

/// ytimg/yt3 thumbnail URLs embed their size (`…=w60-h60-l90-rj`); rewrite to
/// a size that survives the cover cache (544 px).  Best-effort: on any mismatch
/// the original URL is kept.
fn upscale_thumbnail(url: &str) -> String {
    const TARGET: &str = "=w544-h544-p-l90-rj";
    match url.find('=') {
        Some(eq) if eq > 0 => format!("{}{}", &url[..eq], TARGET),
        _ => url.to_string(),
    }
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
// Radio (related tracks via the `next` endpoint)
// ---------------------------------------------------------------------------

/// Fetches YouTube Music's radio/related list for a seed track (up to ~50
/// tracks).  The seed itself and recently-played online picks are excluded.
pub fn radio_tracks(seed_video_id: &str) -> Result<Vec<OnlineTrack>, String> {
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": seed_video_id,
        "playlistId": format!("RD{seed_video_id}"),
    });

    let client = http_client()?;
    let json = innertube_post(&client, NEXT_URL, body)?;
    let mut tracks = parse_radio_response(&json);
    tracks.retain(|t| t.video_id != seed_video_id);

    // Rotate past what was played recently (best-effort variety).
    let recent = RADIO_RECENT.lock().map(|g| g.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
    if !recent.is_empty() {
        let fresh: Vec<OnlineTrack> = tracks.iter().filter(|t| !recent.contains(&t.video_id)).cloned().collect();
        if fresh.len() >= 3 {
            tracks = fresh;
        }
    }

    // Shuffle so repeat radio sessions differ.
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    tracks.shuffle(&mut rng);
    Ok(tracks)
}

static RADIO_RECENT: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(RADIO_REMEMBER)));

/// Records that an online track was auto-played (radio variety memory).
pub fn remember_radio_play(video_id: &str) {
    if let Ok(mut q) = RADIO_RECENT.lock() {
        q.push_back(video_id.to_string());
        while q.len() > RADIO_REMEMBER {
            q.pop_front();
        }
    }
}

/// Parses `playlistPanelVideoRenderer` items out of a `next` response.
fn parse_radio_response(json: &serde_json::Value) -> Vec<OnlineTrack> {
    let mut renderers = Vec::new();
    collect_values(json, "playlistPanelVideoRenderer", &mut renderers);

    let mut tracks = Vec::new();
    for r in renderers {
        let Some(video_id) = r.get("videoId").and_then(|v| v.as_str()) else { continue };
        if video_id.is_empty() {
            continue;
        }
        let title = r
            .pointer("/title/runs")
            .and_then(|v| v.as_array())
            .map(|runs| {
                runs.iter()
                    .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let artist = r
            .pointer("/shortBylineText/runs/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let duration_secs = r
            .pointer("/lengthText/simpleText")
            .and_then(|v| v.as_str())
            .and_then(parse_duration);
        let thumbnail = r
            .pointer("/thumbnail/thumbnails")
            .and_then(|v| v.as_array())
            .and_then(|thumbs| thumbs.last())
            .and_then(|t| t.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| upscale_thumbnail(u));

        tracks.push(OnlineTrack {
            video_id: video_id.to_string(),
            title,
            artist,
            album: String::new(),
            duration_secs,
            thumbnail,
            cached: cached_file(video_id).is_some(),
        });
    }
    tracks
}

/// Depth-first collector for a key name anywhere in the response tree.
fn collect_values<'a>(node: &'a serde_json::Value, key: &str, out: &mut Vec<&'a serde_json::Value>) {
    match node {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    out.push(v);
                }
                collect_values(v, key, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_values(item, key, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Stream resolution (yt-dlp) — locking, verification, probing, pruning
// ---------------------------------------------------------------------------

/// Disk cache for downloaded streams: ~/.cache/musico/online/{videoId}.*
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

/// Per-video download locks: two concurrent requests for the same video wait
/// on the same mutex; the second one then finds the fresh cache entry.
fn video_lock(video_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(video_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
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
/// present, otherwise runs yt-dlp (best AAC, falling back to best audio).
/// Concurrent calls for the same video are serialized; one retry covers
/// transient extractor failures.  Blocks — call from a worker thread.
pub fn resolve_stream(video_id: &str, timeout: Duration) -> Result<PathBuf, String> {
    let _guard = video_lock(video_id);

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

    let mut last_err = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(600));
        }
        match run_yt_dlp(&url, &template, timeout) {
            Ok(()) => {}
            Err(e) => {
                last_err = e;
                continue;
            }
        }

        // Verify the output is real and non-trivial before trusting it.
        match cached_file(video_id) {
            Some(path) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if size < MIN_FILE_BYTES {
                    let _ = std::fs::remove_file(&path);
                    last_err = format!("downloaded file suspiciously small ({size} B)");
                    continue;
                }
                prune_cache(cache_cap_bytes());
                return Ok(path);
            }
            None => {
                last_err = "yt-dlp reported success but no file appeared in cache".into();
            }
        }
    }
    Err(last_err)
}

fn run_yt_dlp(url: &str, template: &Path, timeout: Duration) -> Result<(), String> {
    let mut child = Command::new("yt-dlp")
        .args([
            "--no-playlist",
            "--no-progress",
            "--quiet",
            "-f",
            "140/bestaudio[ext=m4a]/bestaudio",
            "-o",
        ])
        .arg(template)
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch yt-dlp: {e}"))?;

    // Poll instead of a wait-thread: lets us kill the child directly on
    // timeout (portable — no external `kill` binary needed on Windows).
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // reap so no zombie remains
                    break Err(format!("yt-dlp timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => break Err(format!("yt-dlp wait: {e}")),
        }
    }?;

    if !status.success() {
        return Err(format!("yt-dlp exited with {status} — stream unavailable or region-locked"));
    }
    Ok(())
}

/// Cache cap — `MUSICO_ONLINE_CACHE_MB` env override (for tests / small disks).
fn cache_cap_bytes() -> u64 {
    std::env::var("MUSICO_ONLINE_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or(CACHE_MAX_BYTES)
}

/// LRU-prunes the stream cache to the cap by mtime (oldest first), keeping
/// everything above the hysteresis target.  Returns bytes freed.
pub fn prune_cache(max_bytes: u64) -> u64 {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };

    let mut files: Vec<(PathBuf, u64, SystemTime)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            !n.ends_with(".part") && !n.ends_with(".ytdl")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((e.path(), meta.len(), meta.modified().ok()?))
        })
        .collect();

    let total: u64 = files.iter().map(|(_, s, _)| s).sum();
    if total <= max_bytes {
        return 0;
    }
    let target = (max_bytes as f64 * CACHE_PRUNE_TARGET) as u64;

    files.sort_by_key(|(_, _, mtime)| *mtime); // oldest first
    let mut freed = 0u64;
    for (path, size, _) in files {
        if total - freed <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    if freed > 0 {
        log::info!("Pruned {} MB from the online stream cache", freed / 1024 / 1024);
    }
    freed
}

/// Clears the stream cache entirely; returns bytes removed.
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
// Post-download polish — duration probe + cover art
// ---------------------------------------------------------------------------

/// Best-effort duration probe of a downloaded file via symphonia.
pub fn probe_duration_secs(path: &Path) -> Option<f32> {
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .and_then(|t| {
            t.codec_params
                .n_frames
                .map(|n| n as f32 / t.codec_params.sample_rate.unwrap_or(44100) as f32)
        })
        .filter(|d| *d > 0.0)
}

/// Downloads the track thumbnail into the shared cover cache so get_cover /
/// vinyl label / MPRIS art all work for online songs.  Best-effort.
pub fn cache_cover_art(song_id: &str, thumbnail: Option<&str>) {
    let Some(url) = thumbnail else { return };
    let cover = crate::scanner::cover_path(song_id);
    if cover.exists() {
        return;
    }
    let Some(parent) = cover.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(client) = http_client() else { return };
    let Ok(resp) = client.get(url).timeout(Duration::from_secs(8)).send() else { return };
    if !resp.status().is_success() {
        return;
    }
    if let Ok(bytes) = resp.bytes() {
        if bytes.len() > 1024 {
            let _ = std::fs::write(&cover, &bytes);
        }
    }
}

/// Builds the final SongInfo for the engine: probes duration when the search
/// metadata lacked it, and warms the cover cache.
pub fn to_song_info(track: &OnlineTrack, file_path: &Path) -> SongInfo {
    let duration = track
        .duration_secs
        .map(|d| d as f32)
        .or_else(|| probe_duration_secs(file_path))
        .unwrap_or(0.0);
    cache_cover_art(&track.song_id(), track.thumbnail.as_deref());
    SongInfo {
        id: track.song_id(),
        file_path: file_path.to_string_lossy().to_string(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration_secs: duration,
        cover_art: None,
    }
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
            cached: false,
        };
        assert_eq!(t.song_id(), "yt:abc123");
        assert_eq!(t.watch_url(), "https://www.youtube.com/watch?v=abc123");
    }

    #[test]
    fn extracts_client_version_from_html() {
        let html = r#"ytcfg.set({"INNERTUBE_CONTEXT":{"client":{"clientName":"WEB_REMIX","clientVersion":"1.20261025.12.00","hl":"en"}}});"#;
        assert_eq!(extract_client_version(html).as_deref(), Some("1.20261025.12.00"));
        assert_eq!(extract_client_version("no version here"), None);
        assert_eq!(extract_client_version(r#""clientVersion":""#), None);
        // Rejects non-numeric junk after the marker.
        assert_eq!(extract_client_version(r#""clientVersion":"abc""#), None);
    }

    #[test]
    fn upscales_thumbnail_urls() {
        assert_eq!(
            upscale_thumbnail("https://yt3.googleusercontent.com/abc=w60-h60-l90-rj"),
            "https://yt3.googleusercontent.com/abc=w544-h544-p-l90-rj"
        );
        assert_eq!(upscale_thumbnail("https://example.com/no-params"), "https://example.com/no-params");
    }

    fn shelf_item(video_id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": video_id },
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [{ "text": title, "navigationEndpoint": { "watchEndpoint": { "videoId": video_id } } }] } } },
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [
                            { "text": "Daft Punk" }, { "text": " • " },
                            { "text": "Discovery" }, { "text": " • " },
                            { "text": "5:21" }
                        ] } } }
                ],
                "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                    { "url": "https://yt3.googleusercontent.com/x=w60-h60-l90-rj" }
                ] } } }
            }
        })
    }

    #[test]
    fn parses_initial_search_shape_with_continuation() {
        let fixture = serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {
                                            "continuations": [{ "nextContinuationData": { "continuation": "TOK1" } }],
                                            "contents": [
                                                shelf_item("vid1", "One More Time"),
                                                shelf_item("vid2", "Aerodynamic")
                                            ]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });

        let page = parse_search_page(&fixture);
        assert_eq!(page.tracks.len(), 2);
        assert_eq!(page.next_continuation.as_deref(), Some("TOK1"));
        let t = &page.tracks[0];
        assert_eq!(t.video_id, "vid1");
        assert_eq!(t.title, "One More Time");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.album, "Discovery");
        assert_eq!(t.duration_secs, Some(321));
        assert!(t.thumbnail.as_deref().unwrap().contains("w544"));
    }

    #[test]
    fn parses_continuation_shape() {
        let fixture = serde_json::json!({
            "continuationContents": {
                "musicShelfContinuation": {
                    "continuations": [{ "nextContinuationData": { "continuation": "TOK2" } }],
                    "contents": [shelf_item("vid3", "Digital Love")]
                }
            }
        });
        let page = parse_search_page(&fixture);
        assert_eq!(page.tracks.len(), 1);
        assert_eq!(page.tracks[0].video_id, "vid3");
        assert_eq!(page.next_continuation.as_deref(), Some("TOK2"));
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
        assert!(parse_search_page(&fixture).tracks.is_empty());
    }

    #[test]
    fn parses_radio_response() {
        let fixture = serde_json::json!({
            "contents": {
                "singleColumnMusicWatchNextResultsRenderer": {
                    "tabbedRenderer": {
                        "watchNextTabbedResultsRenderer": {
                            "tabs": [{
                                "tabRenderer": {
                                    "content": {
                                        "musicQueueRenderer": {
                                            "content": {
                                                "playlistPanelRenderer": {
                                                    "contents": [{
                                                        "playlistPanelVideoRenderer": {
                                                            "videoId": "rel1",
                                                            "title": { "runs": [{ "text": "Get Lucky" }] },
                                                            "shortBylineText": { "runs": [{ "text": "Daft Punk" }] },
                                                            "lengthText": { "simpleText": "4:08" },
                                                            "thumbnail": { "thumbnails": [
                                                                { "url": "https://yt3.ggpht.com/y=w120-h120" }
                                                            ] }
                                                        }
                                                    }, {
                                                        "playlistPanelVideoRenderer": {
                                                            "videoId": "",
                                                            "title": { "runs": [{ "text": "broken" }] }
                                                        }
                                                    }]
                                                }
                                            }
                                        }
                                    }
                                }
                            }]
                        }
                    }
                }
            }
        });

        let tracks = parse_radio_response(&fixture);
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.video_id, "rel1");
        assert_eq!(t.title, "Get Lucky");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.duration_secs, Some(248));
    }

    #[test]
    fn radio_recent_ring_buffer_caps() {
        RADIO_RECENT.lock().unwrap().clear();
        for i in 0..(RADIO_REMEMBER + 5) {
            remember_radio_play(&format!("v{i}"));
        }
        let q = RADIO_RECENT.lock().unwrap();
        assert_eq!(q.len(), RADIO_REMEMBER);
        assert!(!q.contains(&"v0".to_string()), "oldest entries evicted");
        assert!(q.contains(&format!("v{}", RADIO_REMEMBER + 4)));
    }

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

    #[test]
    fn prune_cache_respects_cap_and_removes_oldest() {
        let dir = cache_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("pruneold.m4a");
        let new = dir.join("prunenew.m4a");
        std::fs::write(&old, vec![0u8; 1024]).unwrap();
        // Small delay guarantees the newer file has a strictly greater mtime.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&new, vec![0u8; 512]).unwrap();

        // 1536 bytes held, 1536 cap → nothing to prune.
        assert_eq!(prune_cache(1536), 0);
        // Shrink the cap so only `new` fits.
        let freed = prune_cache(640);
        assert!(freed >= 1024, "old file should be pruned, freed {freed}");
        assert!(!old.exists(), "oldest file removed");
        assert!(new.exists(), "newest file survives");
        let _ = std::fs::remove_file(&new);
    }

    #[test]
    fn per_video_lock_is_shared() {
        let a = video_lock("locktest");
        let b = video_lock("locktest");
        let c = video_lock("othervideo");
        assert!(Arc::ptr_eq(&a, &b), "same video → same lock");
        assert!(!Arc::ptr_eq(&a, &c), "different video → different lock");
    }

    #[test]
    fn probe_duration_rejects_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("notaudio.m4a");
        std::fs::write(&p, b"definitely not an audio file").unwrap();
        assert!(probe_duration_secs(&p).is_none());
        assert!(probe_duration_secs(Path::new("/nonexistent/file.m4a")).is_none());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// End-to-end live check against YouTube Music.  Opt-in (hits the network):
    ///   MUSICO_LIVE_TESTS=1 cargo test -p musico-tauri live_ -- --ignored --nocapture
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
