//! yt-dlp stream resolution: per-video locks, retry, size verification, the
//! size-capped LRU disk cache, and post-download polish (duration probe +
//! cover-art caching).  All blocking — callers must be on worker threads.
//!
//! Resource discipline:
//! - one process-wide cap on concurrent yt-dlp children (batch self-limits,
//!   but interactive play, pre-resolve and radio warmup spawn independently)
//! - resolve budgets are TOTAL wall-clock bounds, split across attempts
//!   (the old per-attempt timeout silently multiplied by the retry loop)
//! - killed downloads leave `.part`/`.ytdl` sidecars — swept by prune, counted
//!   by usage reporting, so failed downloads can't grow the disk forever

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use super::client::http_client;
use super::models::OnlineTrack;
use musico_playback::SongInfo;

/// Cache size cap (LRU-pruned after each download).
const CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// After pruning kicks in, shrink to this fraction of the cap (hysteresis).
const CACHE_PRUNE_TARGET: f64 = 0.8;
/// A downloaded file smaller than this is considered broken.
const MIN_FILE_BYTES: u64 = 10 * 1024;
/// Partial-download sidecars older than this are residue from killed runs
/// (the timeout path kills yt-dlp mid-download by design) — swept by prune.
const SIDECAR_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// Upper bound on a single art download — thumbnails are ~100 KB; never
/// buffer an unbounded response body.
const MAX_ART_BYTES: u64 = 5 * 1024 * 1024;

/// Resolve attempts for background work.  Budgets are TOTAL wall-clock
/// bounds split across attempts — a "240 s" budget is 2 × 120 s, not the
/// ~8 min the old per-attempt timeout silently produced.
pub(crate) const RESOLVE_ATTEMPTS: u32 = 2;

/// Interactive play: ONE attempt, bounded — a wedged download must fail the
/// click with an honest error instead of pinning "resolving" for minutes.
pub const PLAY_RESOLVE_BUDGET: Duration = Duration::from_secs(90);

/// Background resolve (queue fill, batch, radio warmup): two attempts of
/// this length — 240 s total, matching the documented budget.
const BG_PER_ATTEMPT: Duration = Duration::from_secs(120);

/// Global cap on concurrent yt-dlp child processes.  The batch manager
/// self-limits to 2, but interactive play, pre-resolve and radio warmup
/// each spawn independently — without a process-wide cap they stack.
const MAX_YT_DLP_PROCS: usize = 3;

static YT_DLP_SLOTS: LazyLock<(Mutex<usize>, Condvar)> =
    LazyLock::new(|| (Mutex::new(MAX_YT_DLP_PROCS), Condvar::new()));

/// A held yt-dlp process slot; released on drop.
struct YtDlpSlot {
    _private: (),
}

impl Drop for YtDlpSlot {
    fn drop(&mut self) {
        let (lock, cv) = &*YT_DLP_SLOTS;
        let mut free = lock.lock().unwrap_or_else(|e| e.into_inner());
        *free += 1;
        cv.notify_one();
    }
}

fn acquire_yt_dlp_slot() -> YtDlpSlot {
    let (lock, cv) = &*YT_DLP_SLOTS;
    let mut free = lock.lock().unwrap_or_else(|e| e.into_inner());
    while *free == 0 {
        free = cv.wait(free).unwrap_or_else(|e| e.into_inner());
    }
    *free -= 1;
    YtDlpSlot { _private: () }
}

/// Runtime cap override set from the storage settings UI (MB).  Precedence
/// in [`cap_from`]: `MUSICO_ONLINE_CACHE_MB` env > this override > default.
static CAP_OVERRIDE_MB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sets the runtime cache cap in MB (0 = back to default).
pub fn set_cache_cap_override_mb(mb: u64) {
    CAP_OVERRIDE_MB.store(mb, std::sync::atomic::Ordering::Relaxed);
}

/// Pure cap precedence — env wins over the settings override, default last.
fn cap_from(env_mb: Option<u64>, override_mb: u64) -> u64 {
    let mb = env_mb.unwrap_or(if override_mb == 0 { CACHE_MAX_BYTES / (1024 * 1024) } else { override_mb });
    mb.max(64).saturating_mul(1024 * 1024)
}

/// The effective cap in MB (what the storage UI shows as the ceiling).
pub fn effective_cap_mb() -> u64 {
    cache_cap_bytes() / (1024 * 1024)
}

/// Prunes the stream cache to the currently effective cap.
pub fn prune_cache_to_cap() -> u64 {
    prune_cache(cache_cap_bytes())
}

/// Cache cap — `MUSICO_ONLINE_CACHE_MB` env override (for tests / small disks)
/// beats the storage-settings slider; both beat the 1 GB default.
fn cache_cap_bytes() -> u64 {
    cap_from(
        std::env::var("MUSICO_ONLINE_CACHE_MB").ok().and_then(|v| v.parse::<u64>().ok()),
        CAP_OVERRIDE_MB.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Total bytes on disk in the stream cache — including partial-download
/// sidecars, so the storage UI shows disk truth (the sweeper bounds them).
pub fn cache_usage_bytes() -> u64 {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Disk cache for downloaded streams: ~/.cache/musico-online/online/{videoId}.*
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(crate::identity::DATA_SUBDIR)
        .join("online")
}

/// In-memory index of the stream cache: video id → file.  Search parsing
/// badges every result row with `cached` — a hash lookup here replaces a
/// directory scan per row.  Every miss still falls back to one scan, so the
/// index cannot go stale in a way that matters; prune/clear keep it exact.
static CACHE_INDEX: LazyLock<RwLock<HashMap<String, PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Stream-cache hit test.  Exact `{videoId}.` prefix — a short or empty id
/// from a malformed response must never match another video's file.  Shared
/// with the parser so search results can be badged as cached before any
/// download happens.
pub(crate) fn cached_file(video_id: &str) -> Option<PathBuf> {
    if video_id.is_empty() {
        return None;
    }
    let prefix = format!("{video_id}.");
    if let Ok(index) = CACHE_INDEX.read() {
        if let Some(path) = index.get(video_id) {
            // Re-validate: a prune or external wipe may have removed it.
            if path.exists() {
                return Some(path.clone());
            }
        }
    }
    let found = scan_dir_for(&prefix);
    if let Ok(mut index) = CACHE_INDEX.write() {
        match &found {
            Some(path) => {
                index.insert(video_id.to_string(), path.clone());
            }
            None => {
                index.remove(video_id);
            }
        }
    }
    found
}

fn scan_dir_for(prefix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache_dir()).ok()?;
    entries.flatten().map(|e| e.path()).find(|path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| {
                n.starts_with(prefix) && !n.ends_with(".part") && !n.ends_with(".ytdl")
            })
    })
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

/// Cached yt-dlp presence: the check spawns a process per call and resolve
/// runs on every play — a hidden per-play cost.  Both results cached; a user
/// who installs yt-dlp mid-session waits out the TTL (or restarts).
const YTDLP_CHECK_TTL: Duration = Duration::from_secs(600);
static YTDLP_CHECK: LazyLock<Mutex<Option<(Instant, bool)>>> = LazyLock::new(|| Mutex::new(None));

/// Returns `true` when the yt-dlp binary is available and runnable.
pub fn yt_dlp_available() -> bool {
    if let Ok(guard) = YTDLP_CHECK.lock() {
        if let Some((at, ok)) = *guard {
            if at.elapsed() < YTDLP_CHECK_TTL {
                return ok;
            }
        }
    }
    let ok = Command::new("yt-dlp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if let Ok(mut guard) = YTDLP_CHECK.lock() {
        *guard = Some((Instant::now(), ok));
    }
    ok
}

/// Resolves `video_id` to a local audio file: returns the cached file when
/// present, otherwise runs yt-dlp (best AAC, falling back to best audio).
/// `budget` is the TOTAL wall-clock bound, split across
/// [`RESOLVE_ATTEMPTS`] attempts.  Concurrent calls for the same video are
/// serialized; one retry covers transient extractor failures.  Blocks —
/// call from a worker thread.
pub fn resolve_stream(video_id: &str, budget: Duration) -> Result<PathBuf, String> {
    resolve_with(video_id, RESOLVE_ATTEMPTS, budget / RESOLVE_ATTEMPTS)
}

/// Track-aware resolve: hides the cover-art fetch under the download (the
/// thumbnail URL is in hand here, and the art must be on disk before the
/// `Playing` event — the frontend caches a null cover forever on a miss).
/// `interactive` selects the one-attempt bounded budget for click-to-play.
pub fn resolve_track(track: &OnlineTrack, interactive: bool) -> Result<PathBuf, String> {
    let cover = CoverJob::spawn(&track.song_id(), track.thumbnail.as_deref());
    let result = if interactive {
        resolve_with(&track.video_id, 1, PLAY_RESOLVE_BUDGET)
    } else {
        resolve_with(&track.video_id, RESOLVE_ATTEMPTS, BG_PER_ATTEMPT)
    };
    cover.join();
    result
}

/// Joins a background cover fetch spawned alongside a download.
struct CoverJob(Option<std::thread::JoinHandle<()>>);

impl CoverJob {
    fn spawn(song_id: &str, thumbnail: Option<&str>) -> Self {
        let sid = song_id.to_string();
        let url = thumbnail.map(str::to_string);
        let handle = std::thread::Builder::new()
            .name("online-cover".into())
            .spawn(move || cache_cover_art(&sid, url.as_deref()))
            .ok();
        CoverJob(handle)
    }

    fn join(self) {
        if let Some(handle) = self.0 {
            let _ = handle.join();
        }
    }
}

fn resolve_with(video_id: &str, attempts: u32, per_attempt: Duration) -> Result<PathBuf, String> {
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
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(600));
        }
        // Held for the attempt's whole run — the process-wide child cap.
        let _slot = acquire_yt_dlp_slot();
        match run_yt_dlp(&url, &template, per_attempt) {
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

/// LRU-prunes the stream cache to the cap by mtime (oldest first), keeping
/// everything above the hysteresis target.  Also sweeps stale `.part`/`.ytdl`
/// sidecars from killed downloads (they never count toward the LRU math but
/// must not accumulate forever).  Returns bytes freed.
pub fn prune_cache(max_bytes: u64) -> u64 {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    let sidecar_cutoff = SystemTime::now() - SIDECAR_MAX_AGE;

    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut swept: Vec<(PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if n.ends_with(".part") || n.ends_with(".ytdl") {
            if meta.modified().ok().is_some_and(|m| m < sidecar_cutoff) {
                swept.push((entry.path(), meta.len()));
            }
            continue;
        }
        let Some(mtime) = meta.modified().ok() else { continue };
        files.push((entry.path(), meta.len(), mtime));
    }
    let mut swept_bytes = 0u64;
    for (path, size) in &swept {
        if std::fs::remove_file(path).is_ok() {
            swept_bytes += size;
        }
    }
    if swept_bytes > 0 {
        log::info!("Swept {} KB of stale download sidecars", swept_bytes / 1024);
    }

    let total: u64 = files.iter().map(|(_, s, _)| s).sum();
    let mut freed = swept_bytes;
    if total > max_bytes {
        let target = (max_bytes as f64 * CACHE_PRUNE_TARGET) as u64;

        files.sort_by_key(|(_, _, mtime)| *mtime); // oldest first
        let mut lru_freed = 0u64;
        let mut pruned_paths = Vec::new();
        for (path, size, _) in files {
            if total - lru_freed <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                lru_freed += size;
                pruned_paths.push(path);
            }
        }
        freed += lru_freed;
        if !pruned_paths.is_empty() {
            if let Ok(mut index) = CACHE_INDEX.write() {
                index.retain(|_, v| !pruned_paths.contains(v));
            }
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
    if let Ok(mut index) = CACHE_INDEX.write() {
        index.clear();
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

/// Art may only come from YouTube's image CDN over https — the URL arrives
/// from response JSON, and this client must never be pointed elsewhere.
fn is_safe_art_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            parsed.scheme() == "https"
                && parsed
                    .host_str()
                    .is_some_and(|h| h == "ytimg.com" || h.ends_with(".ytimg.com"))
        }
        Err(_) => false,
    }
}

/// Downloads the track thumbnail into the shared cover cache so get_cover /
/// vinyl label / MPRIS art all work for online songs.  Best-effort.  The
/// response body is size-capped ([`MAX_ART_BYTES`]) before it hits disk.
pub fn cache_cover_art(song_id: &str, thumbnail: Option<&str>) {
    let Some(url) = thumbnail else { return };
    if !is_safe_art_url(url) {
        log::debug!("cover art url rejected (not an https ytimg url)");
        return;
    }
    let cover = crate::scanner::cover_path(song_id);
    if cover.exists() {
        return;
    }
    let Some(parent) = cover.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(client) = http_client() else { return };
    let Ok(mut resp) = client.get(url).timeout(Duration::from_secs(8)).send() else { return };
    if !resp.status().is_success() {
        return;
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    if resp.take(MAX_ART_BYTES + 1).read_to_end(&mut bytes).is_err() {
        return;
    }
    if bytes.len() > 1024 && (bytes.len() as u64) <= MAX_ART_BYTES {
        let _ = std::fs::write(&cover, &bytes);
    }
}

/// Builds the final SongInfo for the engine: probes duration when the search
/// metadata lacked it, and warms the cover cache (a no-op when
/// [`resolve_track`] already fetched it under the download).
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

#[cfg(test)]
mod cap_tests {
    use super::cap_from;
    const GB_MB: u64 = 1024;

    #[test]
    fn env_wins_over_override_and_default() {
        assert_eq!(cap_from(Some(256), 4096), 256 * 1024 * 1024);
    }

    #[test]
    fn override_wins_over_default() {
        assert_eq!(cap_from(None, 512), 512 * 1024 * 1024);
        assert_eq!(cap_from(None, 0), GB_MB * 1024 * 1024, "0 = built-in default");
    }

    #[test]
    fn floor_is_64_mb() {
        assert_eq!(cap_from(Some(1), 0), 64 * 1024 * 1024);
        assert_eq!(cap_from(None, 1), 64 * 1024 * 1024);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// These tests share the REAL cache dir with each other (and the prune
    /// test's byte-count assertions) — cargo runs test threads in parallel,
    /// so every test that writes or counts cache files holds this gate.
    static CACHE_DIR_GATE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn cached_file_matches_exact_prefix_only() {
        let _gate = CACHE_DIR_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = cache_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("exactmatch1.m4a");
        let b = dir.join("exactmatch12.m4a");
        std::fs::write(&a, b"data").unwrap();
        std::fs::write(&b, b"data").unwrap();

        let hit = cached_file("exactmatch1").expect("exact file found");
        assert!(hit.ends_with("exactmatch1.m4a"), "wrong file matched: {hit:?}");
        // Repeat hit comes from the index and must be identical.
        assert_eq!(cached_file("exactmatch1"), Some(hit));
        // A shorter id never resolves onto another video's file.
        assert!(cached_file("exactmatch").is_none());
        // Empty ids match nothing (the old starts_with("") matched everything).
        assert!(cached_file("").is_none());

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        // Post-cleanup, the index must not resurrect the deleted file.
        assert!(cached_file("exactmatch1").is_none());
    }

    #[test]
    fn stale_sidecars_are_swept_fresh_survive() {
        let _gate = CACHE_DIR_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = cache_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let stale = dir.join("sidetest.m4a.part");
        std::fs::write(&stale, vec![0u8; 2048]).unwrap();
        let old = SystemTime::now() - Duration::from_secs(25 * 3600);
        {
            let f = std::fs::File::options().write(true).open(&stale).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        }
        let fresh = dir.join("sidetest_fresh.m4a.part");
        std::fs::write(&fresh, vec![0u8; 2048]).unwrap();

        // Cap far above any real content — only the sweep can free bytes.
        let freed = prune_cache(u64::MAX / 2);
        assert!(freed >= 2048, "stale sidecar swept, freed {freed}");
        assert!(!stale.exists(), "stale sidecar removed");
        assert!(fresh.exists(), "live download's sidecar untouched");
        let _ = std::fs::remove_file(&fresh);
    }

    #[test]
    fn yt_dlp_slots_cap_concurrency() {
        // Drain all slots, confirm a 4th acquisition blocks, then release.
        let s1 = acquire_yt_dlp_slot();
        let s2 = acquire_yt_dlp_slot();
        let s3 = acquire_yt_dlp_slot();
        let acquired = Arc::new(AtomicUsize::new(0));
        let a2 = acquired.clone();
        let h = std::thread::spawn(move || {
            let _p = acquire_yt_dlp_slot();
            a2.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(acquired.load(Ordering::SeqCst), 0, "4th yt-dlp child must wait");
        drop(s1);
        h.join().expect("waiter proceeds after a slot frees");
        drop(s2);
        drop(s3);
        // Churn restores full capacity (Condvar wakeup regression guard).
        for _ in 0..MAX_YT_DLP_PROCS * 2 {
            let _p = acquire_yt_dlp_slot();
        }
    }

    #[test]
    fn art_urls_are_allowlisted() {
        assert!(is_safe_art_url("https://i.ytimg.com/vi/abc/maxresdefault.jpg"));
        assert!(is_safe_art_url("https://ytimg.com/x.jpg"));
        assert!(!is_safe_art_url("http://i.ytimg.com/vi/abc/x.jpg"), "https only");
        assert!(!is_safe_art_url("https://evil.example.com/a.jpg"));
        assert!(!is_safe_art_url("file:///etc/passwd"));
        assert!(!is_safe_art_url("not a url"));
        // A lookalike host must not pass the suffix check.
        assert!(!is_safe_art_url("https://evilytimg.com/a.jpg"));
    }

    #[test]
    fn prune_cache_respects_cap_and_removes_oldest() {
        let _gate = CACHE_DIR_GATE.lock().unwrap_or_else(|e| e.into_inner());
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
