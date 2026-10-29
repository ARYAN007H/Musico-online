//! yt-dlp stream resolution: per-video locks, retry, size verification, the
//! size-capped LRU disk cache, and post-download polish (duration probe +
//! cover-art caching).  All blocking — callers must be on worker threads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
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
    mb.max(64) * 1024 * 1024
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

/// Total bytes of completed downloads in the stream cache (partial files
/// and yt-dlp sidecars excluded).
pub fn cache_usage_bytes() -> u64 {
    let dir = cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            !n.ends_with(".part") && !n.ends_with(".ytdl")
        })
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

/// Streams-cache hit test.  Shared with the parser so search results can be
/// badged as cached before any download happens.
pub(crate) fn cached_file(video_id: &str) -> Option<PathBuf> {
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
