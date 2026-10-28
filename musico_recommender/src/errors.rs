//! Custom error types for the Musico recommendation engine.

use thiserror::Error;

/// Unified error type for all operations in the crate.
#[derive(Error, Debug)]
pub enum RecommenderError {
    /// File-system or general I/O failure.
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Audio decoding failure (symphonia could not process the file).
    #[error("Decode error: {0}")]
    DecodeError(String),

    /// Embedded database (sled) error.
    #[error("Database error: {0}")]
    DbError(#[from] sled::Error),

    /// A referenced entity (song, session, event) was not found.
    #[error("Not found: {0}")]
    NotFound(String),

    /// The library contains fewer than the minimum required songs (3) for
    /// meaningful recommendations.
    #[error("Insufficient library: need at least 3 indexed songs")]
    InsufficientLibrary,
}
