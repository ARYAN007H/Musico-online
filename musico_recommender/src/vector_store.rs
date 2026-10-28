//! Song vector storage, cosine-similarity search, and deduplication.
//!
//! Songs are stored in the dedicated `songs` sled tree keyed by UUID.
//! A secondary index `path:{file_path}` in the default tree prevents
//! duplicate indexing of the same file.

use chrono::Utc;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::errors::RecommenderError;
use crate::extractor;
use crate::models::{SongRecord, Store};


/// Deterministic song id derived from the file path. Same file → same id
/// across launches and re-scans, so listening history survives re-indexing.
/// (Replaces per-record UUIDs; ids are opaque strings everywhere downstream.)
pub fn stable_song_id(path: &str) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("{:016x}", h.finish())
}

// ---------------------------------------------------------------------------
// Cosine similarity
// ---------------------------------------------------------------------------

/// Euclidean norm of a vector.
pub fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Computes cosine similarity between two equal-length vectors.
/// Returns `0.0` if either vector has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vectors must be the same length");

    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (&ai, &bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        na += ai * ai;
        nb += bi * bi;
    }
    let denom = (na * nb).sqrt();
    if denom < 1e-8 { 0.0 } else { dot / denom }
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------

/// Extracts features from `path`, reads metadata in a single pass, and
/// persists a new [`SongRecord`].  Returns the existing record if the file
/// has already been indexed (dedup guard).
pub(crate) fn index_song(store: &Store, path: &str) -> Result<SongRecord, RecommenderError> {
    // ---- Dedup guard: check secondary path index ----
    let path_key = format!("path:{path}");
    if let Some(id_bytes) = store.db.get(path_key.as_bytes()).map_err(RecommenderError::DbError)? {
        let existing_id = String::from_utf8_lossy(&id_bytes).to_string();
        if let Some(rec) = get_song_by_id(store, &existing_id)? {
            return Ok(rec);
        }
        // ID referenced but record missing — fall through to re-index.
    }

    // ---- Single-pass analysis (metadata + features) ----
    let result = extractor::analyze_file(path)?;

    let id = stable_song_id(path);
    let record = SongRecord {
        id: id.clone(),
        file_path: path.to_string(),
        title: result.title,
        artist: result.artist,
        album: result.album,
        duration_secs: result.duration_secs,
        feature_vector: result.feature_vector,
        indexed_at: Utc::now(),
        replay_gain_db: result.rms_db,
    };

    let value = bincode::serialize(&record).map_err(|e| {
        RecommenderError::DbError(sled::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("bincode serialize: {e}"),
        )))
    })?;

    // Write to songs tree.
    store
        .songs
        .insert(id.as_bytes(), value)
        .map_err(RecommenderError::DbError)?;

    // Write secondary path index.
    store
        .db
        .insert(path_key.as_bytes(), id.as_bytes())
        .map_err(RecommenderError::DbError)?;

    // Increment counter.
    store.increment_song_count().map_err(RecommenderError::DbError)?;

    store.songs.flush().map_err(RecommenderError::DbError)?;

    Ok(record)
}

/// Indexes a song without flushing — used by batch operations.
pub(crate) fn index_song_no_flush(store: &Store, path: &str) -> Result<SongRecord, RecommenderError> {
    let path_key = format!("path:{path}");
    if let Some(id_bytes) = store.db.get(path_key.as_bytes()).map_err(RecommenderError::DbError)? {
        let existing_id = String::from_utf8_lossy(&id_bytes).to_string();
        if let Some(rec) = get_song_by_id(store, &existing_id)? {
            return Ok(rec);
        }
    }

    let result = extractor::analyze_file(path)?;
    let id = stable_song_id(path);
    let record = SongRecord {
        id: id.clone(),
        file_path: path.to_string(),
        title: result.title,
        artist: result.artist,
        album: result.album,
        duration_secs: result.duration_secs,
        feature_vector: result.feature_vector,
        indexed_at: Utc::now(),
        replay_gain_db: result.rms_db,
    };

    let value = bincode::serialize(&record).map_err(|e| {
        RecommenderError::DbError(sled::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("bincode serialize: {e}"),
        )))
    })?;

    store.songs.insert(id.as_bytes(), value).map_err(RecommenderError::DbError)?;
    store.db.insert(path_key.as_bytes(), id.as_bytes()).map_err(RecommenderError::DbError)?;
    store.increment_song_count().map_err(RecommenderError::DbError)?;

    Ok(record)
}

/// Indexes a song from a pre-computed [`AnalysisResult`], bypassing the dedup
/// guard.  Used after clearing indices for a full re-scan.
pub(crate) fn index_from_result(
    store: &Store,
    path: &str,
    result: crate::models::AnalysisResult,
) -> Result<SongRecord, RecommenderError> {
    let id = stable_song_id(path);
    let record = SongRecord {
        id: id.clone(),
        file_path: path.to_string(),
        title: result.title,
        artist: result.artist,
        album: result.album,
        duration_secs: result.duration_secs,
        feature_vector: result.feature_vector,
        indexed_at: Utc::now(),
        replay_gain_db: result.rms_db,
    };

    let value = bincode::serialize(&record).map_err(|e| {
        RecommenderError::DbError(sled::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("bincode serialize: {e}"),
        )))
    })?;

    store.songs.insert(id.as_bytes(), value).map_err(RecommenderError::DbError)?;
    let path_key = format!("path:{path}");
    store.db.insert(path_key.as_bytes(), id.as_bytes()).map_err(RecommenderError::DbError)?;
    store.increment_song_count().map_err(RecommenderError::DbError)?;
    store.songs.flush().map_err(RecommenderError::DbError)?;

    Ok(record)
}

// ---------------------------------------------------------------------------
// Retrieval
// ---------------------------------------------------------------------------

/// Loads every [`SongRecord`] from the `songs` tree.
pub(crate) fn get_all_songs(store: &Store) -> Result<Vec<SongRecord>, RecommenderError> {
    let mut songs = Vec::new();
    for entry in store.songs.iter() {
        let (key, value) = entry.map_err(RecommenderError::DbError)?;
        match bincode::deserialize::<SongRecord>(&value) {
            Ok(record) => songs.push(record),
            Err(e) => {
                log::warn!("Corrupted song record in DB: {:?}: {}. Skipping.", key, e);
                let _ = store.songs.remove(key);
            }
        }
    }
    Ok(songs)
}

/// Loads a single [`SongRecord`] by UUID.
pub(crate) fn get_song_by_id(store: &Store, song_id: &str) -> Result<Option<SongRecord>, RecommenderError> {
    match store.songs.get(song_id.as_bytes()).map_err(RecommenderError::DbError)? {
        Some(bytes) => {
            match bincode::deserialize::<SongRecord>(&bytes) {
                Ok(record) => Ok(Some(record)),
                Err(e) => {
                    log::warn!("Corrupted song record for ID {}: {}. Skipping.", song_id, e);
                    let _ = store.songs.remove(song_id.as_bytes());
                    Ok(None)
                }
            }
        }
        None => Ok(None),
    }
}
