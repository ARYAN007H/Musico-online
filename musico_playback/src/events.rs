//! Command and event enums for inter-thread communication.
//!
//! Commands flow **GUI → decoder thread** via `crossbeam_channel`.
//! Events flow **decoder thread → GUI** via a separate channel.

use crate::state::SongInfo;

/// Commands sent from the GUI thread to the decoder thread.
#[derive(Debug)]
pub enum PlaybackCommand {
    /// Load and begin playing a new song immediately.
    Play(SongInfo),
    /// Pause playback at the current position.
    Pause,
    /// Resume playback from the paused position.
    Resume,
    /// Stop playback entirely and reset state.
    Stop,
    /// Seek to a specific position in seconds.
    Seek(f32),
    /// Set the master volume (`0.0` to `1.0`).
    SetVolume(f32),
    /// Mute audio output (volume is remembered).
    Mute,
    /// Unmute audio output.
    Unmute,
    /// Hint to the decoder to begin pre-decoding the next song.
    /// Foundation for gapless playback — not fully implemented yet.
    PreloadNext(SongInfo),
    /// Update EQ gains (10-band, values in dB).
    SetEQ {
        enabled: bool,
        gains_db: [f32; 10],
    },
    /// Set normalization gain for the current track.
    /// `gain_factor` = 10^((target_db - track_db) / 20).
    SetNormGain(f32),
    /// Update crossfade configuration.
    SetCrossfade(crate::crossfade::CrossfadeConfig),
}

/// Events sent from the playback engine back to the GUI/app layer.
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    /// A new song has started playing.
    Playing(SongInfo),
    /// Playback was paused.
    Paused {
        /// Position at which playback was paused.
        position_secs: f32,
    },
    /// Playback was resumed.
    Resumed,
    /// Playback was stopped.
    Stopped,
    /// A seek operation completed.
    Seeked {
        /// New playback position after the seek.
        position_secs: f32,
    },
    /// Periodic position update (emitted every ~250ms while playing).
    PositionUpdate {
        /// Current playback position in seconds.
        position_secs: f32,
        /// Total seconds of actual listening (pauses excluded).
        listened_secs: u32,
    },
    /// The current song has finished playing.
    ///
    /// The GUI **must** use this to call
    /// `musico_recommender::log_listen(song_id, listened_secs, duration_secs)`
    /// and request the next recommendation.
    SongEnded {
        /// UUID of the song that ended.
        song_id: String,
        /// Total seconds the user actually heard.
        listened_secs: u32,
        /// Total duration of the song.
        duration_secs: f32,
    },
    /// The decoder has begun pre-filling the ring buffer.
    BufferingStarted,
    /// The ring buffer is sufficiently filled; playback is starting.
    BufferingComplete,
    /// A non-fatal error occurred during playback.
    Error(String),
    /// Volume was changed.
    VolumeChanged(f32),
}
