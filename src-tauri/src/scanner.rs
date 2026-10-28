//! # Musico — Library Scanner
//!
//! Light metadata pass over the music folder: probe each file with Symphonia
//! for tags (title/artist/album), duration, and embedded cover art. No packet
//! decoding — full feature analysis is the recommender's job (C5).
//!
//! Cover art is downscaled and cached on disk (`~/.cache/musico/covers/`) so
//! the app's RAM stays flat no matter how big the library is.
//!
//! Scanning runs on its own thread and reports progress via Tauri events:
//! `scan-progress` (per few files) and `library-updated` (final Vec<SongMeta>).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, StandardVisualKey};
use symphonia::core::probe::Hint;
use tauri::{AppHandle, Emitter, Manager};
use walkdir::WalkDir;

use crate::state::{AppState, SongMeta};

const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "alac", "wma", "mp4", "m4b",
];
const EVENT_PROGRESS: &str = "scan-progress";
const EVENT_DONE: &str = "library-updated";
const INDEX_PROGRESS: &str = "index-progress";
const INDEX_DONE: &str = "index-complete";

/// Stable song id derived from the file path — the recommender uses the same
/// derivation (`musico_recommender::vector_store::stable_song_id`), so library
/// ids, play events, and indexed records all agree.
pub fn stable_id(path: &Path) -> String {
    musico_recommender::vector_store::stable_song_id(&path.to_string_lossy())
}

/// Disk path for a song's cached cover thumbnail.
pub fn cover_path(song_id: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("musico")
        .join("covers")
        .join(format!("{song_id}.jpg"))
}

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

struct RawMeta {
    title: String,
    artist: String,
    album: String,
    duration_secs: f32,
    cover: Option<Vec<u8>>,
}

/// Per-file cache entry so re-launches skip re-probing unchanged files.
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    meta: SongMeta,
    mtime: u64,
    size: u64,
}

fn cache_file() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("musico")
        .join("library.json")
}

fn load_cache() -> std::collections::HashMap<String, CacheEntry> {
    let Ok(json) = std::fs::read_to_string(cache_file()) else {
        return Default::default();
    };
    match serde_json::from_str::<Vec<CacheEntry>>(&json) {
        Ok(entries) => entries
            .into_iter()
            .map(|e| (e.meta.file_path.clone(), e))
            .collect(),
        Err(_) => Default::default(),
    }
}

fn save_cache(entries: &[CacheEntry]) {
    if let Some(parent) = cache_file().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(entries) {
        let _ = std::fs::write(cache_file(), json);
    }
}

fn file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mtime, meta.len()))
}

/// Probe-only metadata extraction. Fast (~ms per file); returns None for
/// files Symphonia cannot parse (they are skipped by the scan).
fn extract_metadata(path: &Path) -> Option<RawMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;

    let fallback_title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown".into());

    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut cover: Option<Vec<u8>> = None;

    // Container/stream-level tags first…
    if let Some(meta_log) = probed.metadata.get() {
        if let Some(rev) = meta_log.current() {
            for tag in rev.tags() {
                if let Some(key) = &tag.std_key {
                    match key {
                        StandardTagKey::TrackTitle if title.is_empty() => title = tag.value.to_string(),
                        StandardTagKey::Artist if artist.is_empty() => artist = tag.value.to_string(),
                        StandardTagKey::Album if album.is_empty() => album = tag.value.to_string(),
                        _ => {}
                    }
                }
            }
            for visual in rev.visuals() {
                if visual.usage == Some(StandardVisualKey::FrontCover) {
                    cover = Some(visual.data.to_vec());
                    break;
                }
                if cover.is_none() {
                    cover = Some(visual.data.to_vec());
                }
            }
        }
    }

    // …then format-level tags (some containers store them there).
    {
        let fmt_meta = probed.format.metadata();
        if let Some(rev) = fmt_meta.current() {
            for tag in rev.tags() {
                if let Some(key) = &tag.std_key {
                    match key {
                        StandardTagKey::TrackTitle if title.is_empty() => title = tag.value.to_string(),
                        StandardTagKey::Artist if artist.is_empty() => artist = tag.value.to_string(),
                        StandardTagKey::Album if album.is_empty() => album = tag.value.to_string(),
                        _ => {}
                    }
                }
            }
            if cover.is_none() {
                for visual in rev.visuals() {
                    if visual.usage == Some(StandardVisualKey::FrontCover) {
                        cover = Some(visual.data.to_vec());
                        break;
                    }
                    if cover.is_none() {
                        cover = Some(visual.data.to_vec());
                    }
                }
            }
        }
    }

    if title.is_empty() {
        // Filename fallback: "Artist - Title" → (artist, title); else title only.
        if let Some(pos) = fallback_title.find(" - ") {
            artist = fallback_title[..pos].to_string();
            title = fallback_title[pos + 3..].to_string();
        } else {
            title = fallback_title;
        }
    }
    if artist.is_empty() {
        artist = "Unknown Artist".into();
    }
    if album.is_empty() {
        album = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown Album".into());
    }

    let duration_secs = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .and_then(|t| {
            t.codec_params
                .n_frames
                .map(|n| n as f32 / t.codec_params.sample_rate.unwrap_or(44100) as f32)
        })
        .unwrap_or(0.0);

    Some(RawMeta { title, artist, album, duration_secs, cover })
}

/// Downscale + cache a cover to disk. Keeps RAM flat for huge libraries.
fn cache_cover(song_id: &str, data: &[u8]) {
    let Ok(img) = image::load_from_memory(data) else { return };
    let thumb = img.thumbnail(256, 256);
    let out = cover_path(song_id);
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = thumb.save_with_format(&out, image::ImageFormat::Jpeg);
}

/// Kick off a scan on its own thread. Returns false if one is already running.
pub fn start_scan(handle: AppHandle) -> bool {
    {
        let state = handle.state::<AppState>();
        if state
            .scanning
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
    }
    let folder = {
        let state = handle.state::<AppState>();
        let guard = state.music_folder.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };
    let h = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("scanner".into())
        .spawn(move || run_scan(folder, h))
        .is_ok();
    if !spawned {
        handle.state::<AppState>().scanning.store(false, Ordering::SeqCst);
    }
    spawned
}

fn run_scan(folder: PathBuf, handle: AppHandle) {
    let state = handle.state::<AppState>();

    let paths: Vec<PathBuf> = WalkDir::new(&folder)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && is_audio_file(e.path()))
        .map(|e| e.path().to_path_buf())
        .collect();
    let total = paths.len();
    log::info!("Scanning {} audio files in {}", total, folder.display());

    // Instant library from cache while we verify incrementally.
    let mut cache = load_cache();

    let mut songs: Vec<SongMeta> = Vec::with_capacity(total);
    let mut entries: Vec<CacheEntry> = Vec::with_capacity(total);
    let mut rescanned = 0usize;

    for path in &paths {
        let path_str = path.to_string_lossy().to_string();
        let id = stable_id(path);

        let cached = std::fs::metadata(path)
            .ok()
            .and_then(|m| {
                file_fingerprint(path).zip(cache.get(&path_str))
            })
            .filter(|((mtime, size), entry)| *mtime == entry.mtime && *size == entry.size)
            .map(|(_, entry)| entry);

        let meta = match cached {
            Some(entry) => entry.meta.clone(),
            None => {
                rescanned += 1;
                let Some(meta) = extract_metadata(path) else {
                    log::warn!("Skipping unparseable file: {}", path.display());
                    continue;
                };
                if let Some(cover) = meta.cover {
                    cache_cover(&id, &cover);
                }
                SongMeta {
                    id,
                    title: meta.title,
                    artist: meta.artist,
                    album: meta.album,
                    duration_secs: meta.duration_secs,
                    file_path: path_str.clone(),
                }
            }
        };

        if let Some((mtime, size)) = file_fingerprint(path) {
            entries.push(CacheEntry { meta: meta.clone(), mtime, size });
        }
        songs.push(meta);

        if (songs.len()) % 5 == 0 || songs.len() == total {
            let _ = handle.emit(
                EVENT_PROGRESS,
                serde_json::json!({
                    "done": songs.len(),
                    "total": total,
                    "current": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                }),
            );
        }
    }

    *state.library.lock().unwrap_or_else(|e| e.into_inner()) = songs.clone();
    state.scanning.store(false, Ordering::SeqCst);
    save_cache(&entries);
    log::info!(
        "Scan complete: {} songs ({} cached, {} rescanned)",
        songs.len(),
        songs.len() - rescanned,
        rescanned
    );
    let _ = handle.emit(EVENT_DONE, &songs);

    // Recommendations depend on feature extraction — run it in the background.
    spawn_indexing(handle, songs);
}

/// Background feature-extraction pass. Analysis (decode + FFT) runs OUTSIDE
/// the recommender lock — only the DB write takes the lock briefly — so a
/// slow file can never wedge recommendations. Emits `index-progress` /
/// `index-complete`.
fn spawn_indexing(handle: AppHandle, songs: Vec<SongMeta>) {
    let spawned = std::thread::Builder::new()
        .name("indexer".into())
        .spawn(move || {
            let state = handle.state::<AppState>();
            let Some(rec) = state.recommender.clone() else {
                log::info!("Recommender unavailable — skipping indexing");
                return;
            };

            let todo: Vec<String> = {
                let mut r = rec.lock().unwrap_or_else(|e| e.into_inner());
                match r.get_all_songs() {
                    Ok(indexed) => {
                        let have: std::collections::HashSet<String> =
                            indexed.into_iter().map(|s| s.file_path).collect();
                        songs
                            .iter()
                            .filter(|s| !have.contains(&s.file_path))
                            .map(|s| s.file_path.clone())
                            .collect()
                    }
                    Err(e) => {
                        log::warn!("Index lookup failed: {e}");
                        return;
                    }
                }
            };

            let total = todo.len();
            if total == 0 {
                let _ = handle.emit(INDEX_DONE, serde_json::json!({"indexed": 0}));
                return;
            }
            log::info!("Indexing {} songs for recommendations", total);

            let mut indexed_count = 0usize;
            for (i, path) in todo.iter().enumerate() {
                // Expensive part — no lock held.
                let result = match musico_recommender::extractor::analyze_file(path) {
                    Ok(r) => r,
                    Err(e) => {
                        log::warn!("Index analysis failed for {path}: {e}");
                        continue;
                    }
                };
                // Brief lock — DB write only.
                let ok = rec
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .index_from_result(path, result)
                    .is_ok();
                if !ok {
                    log::warn!("Index write failed: {path}");
                } else {
                    indexed_count += 1;
                }
                log::debug!("[{}/{}] indexed {}", i + 1, total, path);
                if (i + 1) % 10 == 0 {
                    log::info!("Indexing progress {}/{}", i + 1, total);
                }
                if (i + 1) % 3 == 0 || i + 1 == total {
                    let _ = handle.emit(
                        INDEX_PROGRESS,
                        serde_json::json!({"done": i + 1, "total": total}),
                    );
                }
            }

            log::info!("Indexing complete: {indexed_count} songs newly indexed");
            let _ = handle.emit(INDEX_DONE, serde_json::json!({"indexed": indexed_count}));
        })
        .is_ok();
    if !spawned {
        log::warn!("Failed to spawn indexer thread");
    }
}

/// Unit tests run against generated tones and real library files.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_test_tone() {
        let wav = std::env::var("MUSICO_TEST_WAV")
            .unwrap_or_else(|_| "/tmp/musico-test-tone.wav".into());
        if !Path::new(&wav).exists() {
            return; // tone not generated on this machine
        }
        let meta = extract_metadata(Path::new(&wav)).expect("tone should parse");
        assert!((meta.duration_secs - 3.0).abs() < 0.2, "duration {}", meta.duration_secs);
        // No tags in the generated file → filename fallback
        assert_eq!(meta.artist, "Unknown Artist");
        assert!(meta.cover.is_none());
    }
}
