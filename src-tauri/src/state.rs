//! # Musico — Shared Tauri State
//!
//! Thread-safe state shared between Tauri command handlers, the playback
//! event loop, and the frontend. `PlaybackEngine` is internally synchronized
//! (channels + atomics), so it lives behind a plain `Arc` — no Mutex.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use musico_playback::queue::PlaybackQueue;
use musico_playback::{PlaybackEngine, SongInfo};
use musico_recommender::MusicRecommender;
use serde::{Deserialize, Serialize};

use crate::config::AppSettings;

/// Application-level state managed by Tauri.
pub struct AppState {
    pub engine: Option<Arc<PlaybackEngine>>,
    pub library: Mutex<Vec<SongMeta>>,
    pub music_folder: Mutex<PathBuf>,
    pub queue: Mutex<PlaybackQueue>,
    pub settings: Mutex<AppSettings>,
    pub recommender: Option<Arc<Mutex<MusicRecommender>>>,
    /// True while a library scan is running (prevents concurrent scans).
    pub scanning: Arc<AtomicBool>,
    /// Shared MPRIS metadata, filled by the event bridge once D-Bus is up.
    /// Unit type off Linux — MPRIS is a D-Bus interface and doesn't exist there.
    #[cfg(target_os = "linux")]
    pub mpris_state: Mutex<Option<crate::mpris::MprisState>>,
    #[cfg(not(target_os = "linux"))]
    pub mpris_state: Mutex<Option<()>>,
}

impl AppState {
    pub fn new() -> Self {
        let settings = AppSettings::load();
        let music_dir = settings
            .music_folder
            .as_ref()
            .map(PathBuf::from)
            .or_else(dirs_or_default)
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        let engine = PlaybackEngine::new().ok().map(Arc::new);
        if let (Some(eng), vol) = (&engine, settings.volume) {
            let _ = eng.set_volume(vol.clamp(0.0, 1.0));
        }

        let recommender = init_recommender();

        Self {
            engine,
            library: Mutex::new(Vec::new()),
            music_folder: Mutex::new(music_dir),
            queue: Mutex::new(PlaybackQueue::new()),
            settings: Mutex::new(settings),
            recommender,
            scanning: Arc::new(AtomicBool::new(false)),
            mpris_state: Mutex::new(None),
        }
    }
}

/// Opens the sled DB at ~/.local/share/musico/db. If the DB still holds
/// UUID-era song ids (from the Iced app), it is cleared once so all ids
/// agree with the stable path-hash scheme; a marker file records this.
fn init_recommender() -> Option<Arc<Mutex<MusicRecommender>>> {
    let db_dir = dirs::data_dir()?.join("musico").join("db");
    let _ = std::fs::create_dir_all(&db_dir);
    let marker = db_dir.join(".stable-ids");
    let rec = Arc::new(Mutex::new(match MusicRecommender::new(&db_dir.to_string_lossy()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Recommender unavailable: {e}");
            return None;
        }
    }));
    if !marker.exists() {
        if let Ok(mut r) = rec.lock() {
            let _ = r.clear_song_index();
            log::info!("Cleared pre-stable-id index (one-time migration)");
        }
        let _ = std::fs::write(&marker, b"1");
    }
    Some(rec)
}

fn dirs_or_default() -> Option<PathBuf> {
    dirs::audio_dir().or_else(|| {
        std::env::var("HOME").ok().map(|h| PathBuf::from(h).join("Music"))
    })
}

/// Song metadata sent to the frontend — serializable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongMeta {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: f32,
    pub file_path: String,
}

impl From<SongMeta> for SongInfo {
    fn from(m: SongMeta) -> Self {
        SongInfo {
            id: m.id,
            file_path: m.file_path,
            title: m.title,
            artist: m.artist.clone(),
            album: m.album.clone(),
            duration_secs: m.duration_secs,
            cover_art: None,
        }
    }
}

impl From<SongInfo> for SongMeta {
    fn from(s: SongInfo) -> Self {
        SongMeta {
            id: s.id,
            title: s.title,
            artist: s.artist,
            album: s.album,
            duration_secs: s.duration_secs,
            file_path: s.file_path,
        }
    }
}

/// Persisted app settings live in `crate::config::AppSettings` (loaded/saved
/// to `~/.config/musico/settings.json`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_meta_roundtrip() {
        let m = SongMeta {
            id: "abc".into(),
            title: "T".into(),
            artist: "A".into(),
            album: "L".into(),
            duration_secs: 12.5,
            file_path: "/tmp/x.mp3".into(),
        };
        let info: SongInfo = m.clone().into();
        assert_eq!(info.title, "T");
        assert!(info.cover_art.is_none());
        let back = SongMeta::from(info);
        assert_eq!(back.id, "abc");
        assert_eq!(back.duration_secs, 12.5);
    }
}
