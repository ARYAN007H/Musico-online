//! Error types for the playback engine.
//!
//! All public fallible operations return `Result<T, PlaybackError>`.

/// All errors that can occur within the playback pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    /// No audio output device was found on this system.
    #[error("No audio output device found")]
    NoOutputDevice,

    /// Failed to build a CPAL output stream.
    #[error("Failed to build CPAL stream: {0}")]
    StreamBuild(String),

    /// Failed to decode an audio file.
    #[error("Failed to decode audio file '{path}': {reason}")]
    DecodeFailed {
        /// Path to the file that failed.
        path: String,
        /// Human-readable reason for the failure.
        reason: String,
    },

    /// The audio format is not supported.
    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(String),

    /// A seek operation targeted a position outside the valid range.
    #[error("Seek failed: position {0:.1}s is out of range")]
    SeekOutOfRange(f32),

    /// An error occurred during resampling.
    #[error("Resampler error: {0}")]
    ResamplerError(String),

    /// The internal command/event channel has been disconnected.
    #[error("Playback engine channel disconnected")]
    ChannelDisconnected,

    /// A generic I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
