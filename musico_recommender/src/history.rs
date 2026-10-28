//! Play-event logging, skip detection, affinity scoring, and cooldown decay.
//!
//! Events are keyed `{song_id}:{event_id}` in the dedicated `events` tree so
//! that per-song scans are O(events_for_song) instead of O(all_events).
//!
//! Pre-computed scores are cached in the `scores` tree and updated on every
//! new event, making reads O(1) during recommendation.

use chrono::Utc;

use crate::errors::RecommenderError;
use crate::models::{PlayEvent, SongScoreCache, Store};

// ---------------------------------------------------------------------------
// Skip classification weights
// ---------------------------------------------------------------------------

fn listen_ratio_weight(ratio: f32) -> f32 {
    if ratio < 0.10 {
        -3.0
    } else if ratio < 0.40 {
        -1.0
    } else if ratio < 0.80 {
        0.0
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// Score cache helpers
// ---------------------------------------------------------------------------

fn load_score_cache(store: &Store, song_id: &str) -> Result<SongScoreCache, RecommenderError> {
    match store.scores.get(song_id.as_bytes()).map_err(RecommenderError::DbError)? {
        Some(bytes) => {
            match bincode::deserialize::<SongScoreCache>(&bytes) {
                Ok(cache) => Ok(cache),
                Err(e) => {
                    log::warn!("Corrupted score cache found in DB for song {}: {}. Resetting.", song_id, e);
                    let _ = store.scores.remove(song_id.as_bytes());
                    Ok(SongScoreCache::default())
                }
            }
        }
        None => Ok(SongScoreCache::default()),
    }
}

fn save_score_cache(store: &Store, song_id: &str, cache: &SongScoreCache) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(cache).map_err(|e| {
        RecommenderError::DbError(sled::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("score cache serialize: {e}"),
        )))
    })?;
    store.scores.insert(song_id.as_bytes(), bytes).map_err(RecommenderError::DbError)?;
    Ok(())
}

/// Loads all events for a specific song by scanning its key prefix in the
/// `events` tree.  O(events_for_this_song), typically 1–20.
fn get_events_for_song(store: &Store, song_id: &str) -> Result<Vec<PlayEvent>, RecommenderError> {
    let prefix = format!("{song_id}:");
    let mut events: Vec<PlayEvent> = Vec::new();
    for entry in store.events.scan_prefix(prefix.as_bytes()) {
        let (_, value) = entry.map_err(RecommenderError::DbError)?;
        let ev: PlayEvent = match bincode::deserialize(&value) {
            Ok(e) => e,
            Err(_) => continue,
        };
        events.push(ev);
    }
    events.sort_by_key(|e| e.started_at);
    Ok(events)
}

/// Recomputes affinity from all events for a song.
fn compute_affinity(events: &[PlayEvent]) -> f32 {
    let mut score = 0.0_f32;
    for (i, ev) in events.iter().enumerate() {
        score += listen_ratio_weight(ev.listen_ratio);

        // Replay detection: next play within 30s of this play ending.
        if i + 1 < events.len() {
            let end_time = ev.started_at + chrono::Duration::seconds(ev.listened_secs as i64);
            let next_start = events[i + 1].started_at;
            let gap = (next_start - end_time).num_seconds();
            if (0..=30).contains(&gap) {
                score += 2.0;
            }
        }
    }
    score.clamp(-5.0, 5.0)
}

// ---------------------------------------------------------------------------
// Event logging
// ---------------------------------------------------------------------------

/// Persists a [`PlayEvent`] in the `events` tree under key
/// `{song_id}:{event_id}`, then updates the score cache for that song.
pub(crate) fn log_play_event(store: &Store, event: &PlayEvent) -> Result<(), RecommenderError> {
    let key = format!("{}:{}", event.song_id, event.event_id);
    let value = bincode::serialize(event).map_err(|e| {
        RecommenderError::DbError(sled::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("event serialize: {e}"),
        )))
    })?;
    store.events.insert(key.as_bytes(), value).map_err(RecommenderError::DbError)?;

    // ---- Refresh score cache for this song ----
    let events = get_events_for_song(store, &event.song_id)?;
    let affinity = compute_affinity(&events);
    let total_plays = events.len() as u32;
    let play_timestamps: Vec<i64> = events.iter().map(|e| e.started_at.timestamp()).collect();

    let cache = SongScoreCache { affinity, total_plays, play_timestamps };
    save_score_cache(store, &event.song_id, &cache)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Score reads (O(1) from cache)
// ---------------------------------------------------------------------------

/// Returns the pre-computed affinity for a song. O(1) cache read.
/// Returns `0.0` for unheard songs.
pub(crate) fn get_song_affinity(store: &Store, song_id: &str) -> Result<f32, RecommenderError> {
    let cache = load_score_cache(store, song_id)?;
    Ok(cache.affinity)
}

/// Computes cooldown from cached timestamps. O(1) cache read + O(plays) math.
/// Returns `0.0` for never-played songs.
pub(crate) fn get_cooldown_score(store: &Store, song_id: &str) -> Result<f32, RecommenderError> {
    let cache = load_score_cache(store, song_id)?;
    if cache.play_timestamps.is_empty() {
        return Ok(0.0);
    }

    let now = Utc::now().timestamp();
    let decay_constant_secs = 72.0 * 3600.0_f64;

    let mut recent_plays = 0u32;
    let mut last_play_ts: i64 = 0;

    for &ts in &cache.play_timestamps {
        let age_secs = (now - ts) as f64;
        if age_secs < decay_constant_secs {
            recent_plays += 1;
        }
        if ts > last_play_ts {
            last_play_ts = ts;
        }
    }

    if last_play_ts == 0 {
        return Ok(0.0);
    }

    let hours_since_last = (now - last_play_ts) as f64 / 3600.0;
    let raw = recent_plays as f64 * (-hours_since_last / 72.0).exp();

    Ok((raw as f32).clamp(0.0, 1.0))
}

/// Returns `true` if `listen_ratio` classifies as a skip (< 0.40).
pub fn is_skip(listen_ratio: f32) -> bool {
    listen_ratio < 0.40
}

/// Returns the total play count for a song. O(1) cache read.
pub(crate) fn play_count(store: &Store, song_id: &str) -> Result<usize, RecommenderError> {
    let cache = load_score_cache(store, song_id)?;
    Ok(cache.total_plays as usize)
}
