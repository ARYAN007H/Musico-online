//! Shared playback state — the single source of truth.
//!
//! Protected by `Arc<Mutex<PlaybackState>>`, read by the GUI thread and
//! written by the decoder thread.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Current playback status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackStatus {
    /// No song loaded, engine is idle.
    Stopped,
    /// A song is actively playing.
    Playing,
    /// Playback is paused at the current position.
    Paused,
    /// The decoder is pre-filling the ring buffer before playback starts.
    Buffering,
}

/// Metadata about the currently loaded song.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongInfo {
    /// UUID from `musico_recommender`.
    pub id: String,
    /// Absolute filesystem path to the audio file.
    pub file_path: String,
    /// Track title (from metadata tags, or filename fallback).
    pub title: String,
    /// Artist name.
    pub artist: String,
    /// Album name.
    pub album: String,
    /// Duration of the track in seconds.
    pub duration_secs: f32,
    /// Raw cover art bytes (JPEG or PNG) extracted from embedded tags.
    #[serde(skip)]
    pub cover_art: Option<Vec<u8>>,
}

/// The single source of truth for all playback state.
///
/// The GUI reads a clone of this on every tick. The decoder thread is
/// the sole writer.
#[derive(Debug, Clone)]
pub struct PlaybackState {
    /// Current playback status.
    pub status: PlaybackStatus,
    /// Info about the song currently loaded (if any).
    pub current_song: Option<SongInfo>,
    /// Current playback position in seconds (updated ~every 100ms by the
    /// decoder thread).
    pub position_secs: f32,
    /// Total duration of the current song in seconds.
    pub duration_secs: f32,
    /// Master volume, clamped to `[0.0, 1.0]`.
    pub volume: f32,
    /// Whether output is muted (volume is remembered but output is silent).
    pub muted: bool,
    /// `Instant` when the current *uninterrupted* play segment started.
    /// Reset on resume, cleared on pause/stop.
    pub listen_start: Option<Instant>,
    /// Accumulated seconds of actual playback (pauses excluded).
    pub listened_secs: u32,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            current_song: None,
            position_secs: 0.0,
            duration_secs: 0.0,
            volume: 1.0,
            muted: false,
            listen_start: None,
            listened_secs: 0,
        }
    }
}

impl PlaybackState {
    /// Returns the total seconds the user has actually *heard* of this song.
    ///
    /// If currently playing, this adds the elapsed time since `listen_start`
    /// to the accumulated `listened_secs`. This is the value reported to
    /// `musico_recommender` for skip detection.
    pub fn elapsed_listen_secs(&self) -> u32 {
        let extra = match (self.status, self.listen_start) {
            (PlaybackStatus::Playing, Some(start)) => start.elapsed().as_secs() as u32,
            _ => 0,
        };
        self.listened_secs + extra
    }
}
