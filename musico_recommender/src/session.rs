//! Listening session tracking with an exponential moving average (EMA)
//! centroid for mood-locking.
//!
//! v2 behaviour:
//! - **The centroid only moves on song *ends*, weighted by how much of the
//!   song was actually heard.**  A skip no longer drags the mood centroid
//!   toward music the user explicitly rejected (the old code updated the EMA
//!   at play-start, so fast-skipping through an album pulled recommendations
//!   toward it).
//! - Song history (for exclusion) is still pushed at play-start.
//! - Session-only bookkeeping (skip streak, recommendation attribution) lives
//!   in `SessionExt` under a separate key so the persisted `SessionState`
//!   layout stays frozen.

use chrono::Utc;
use uuid::Uuid;

use crate::errors::RecommenderError;
use crate::models::{bin_err, SessionExt, SessionState, SongRecord, Store};
use crate::vector_store;

/// Maximum number of song IDs retained in the session history (for exclusion).
const MAX_SESSION_HISTORY: usize = 10;

/// Centroid EMA rate for a *fully* heard song; scales down linearly with the
/// heard ratio.  A 40%-heard song contributes 0.08 + 0.32×0.4 ≈ 0.21.
const CENTROID_ALPHA_BASE: f32 = 0.08;
const CENTROID_ALPHA_SCALE: f32 = 0.32;
/// Plays below this ratio never move the centroid (skip classification).
const CENTROID_MIN_RATIO: f32 = 0.3;

const KEY_EXT: &[u8] = b"ext";

// ---------------------------------------------------------------------------
// Session core
// ---------------------------------------------------------------------------

fn persist_session(store: &Store, state: &SessionState) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(state).map_err(|e| bin_err("session", e))?;
    store
        .sessions
        .insert(b"current", bytes)
        .map_err(RecommenderError::DbError)?;
    Ok(())
}

/// Loads the current session from the `sessions` tree, if one exists.
pub(crate) fn load_session(store: &Store) -> Result<Option<SessionState>, RecommenderError> {
    match store.sessions.get(b"current").map_err(RecommenderError::DbError)? {
        Some(bytes) => {
            match bincode::deserialize::<SessionState>(&bytes) {
                Ok(state) => Ok(Some(state)),
                Err(e) => {
                    log::warn!("Corrupted session state: {}. Clearing and starting fresh.", e);
                    let _ = store.sessions.remove(b"current");
                    Ok(None)
                }
            }
        }
        None => Ok(None),
    }
}

/// Creates a fresh listening session with empty history and no centroid.
pub(crate) fn start_session(store: &Store) -> Result<SessionState, RecommenderError> {
    let state = SessionState {
        session_id: Uuid::new_v4().to_string(),
        started_at: Utc::now(),
        song_history: Vec::new(),
        centroid: None,
    };
    persist_session(store, &state)?;
    Ok(state)
}

/// Adds `song_id` to the session history (play-start).  Consecutive repeats
/// of the same id are collapsed so history stays informative.  The centroid
/// is *not* touched here — see [`update_centroid`].
pub(crate) fn push_history(
    store: &Store,
    state: &mut SessionState,
    song_id: &str,
) -> Result<(), RecommenderError> {
    if state.song_history.last().map(String::as_str) == Some(song_id) {
        return Ok(());
    }
    state.song_history.push(song_id.to_string());
    if state.song_history.len() > MAX_SESSION_HISTORY {
        state.song_history.remove(0);
    }
    persist_session(store, state)?;
    Ok(())
}

/// Moves the session centroid toward a song that just *ended*, weighted by
/// how much of it the user actually heard.  Skips (ratio < 0.3) don't move it.
pub(crate) fn update_centroid(
    store: &Store,
    state: &mut SessionState,
    record: &SongRecord,
    ratio: f32,
) -> Result<(), RecommenderError> {
    if ratio < CENTROID_MIN_RATIO {
        return Ok(());
    }
    let alpha = (CENTROID_ALPHA_BASE + CENTROID_ALPHA_SCALE * ratio).clamp(0.0, 0.4);
    let new_vec = record.feature_vector.to_weighted_vec();

    state.centroid = Some(match &state.centroid {
        Some(old) => old
            .iter()
            .zip(new_vec.iter())
            .map(|(o, n)| o * (1.0 - alpha) + n * alpha)
            .collect(),
        None => new_vec,
    });
    persist_session(store, state)?;
    Ok(())
}

/// Cosine similarity between the candidate and the session centroid.
/// Returns `0.5` (neutral) when no centroid exists yet.
pub fn get_session_similarity(state: &SessionState, candidate: &SongRecord) -> f32 {
    match &state.centroid {
        Some(centroid) => {
            let candidate_vec = candidate.feature_vector.to_weighted_vec();
            vector_store::cosine_similarity(centroid, &candidate_vec)
                .clamp(0.0, 1.0)
        }
        None => 0.5,
    }
}

// ---------------------------------------------------------------------------
// Session extension (skip streak + rec attribution) — separate key so the
// frozen SessionState bincode layout is untouched.
// ---------------------------------------------------------------------------

/// Loads the session extension state; missing/corrupt → default.
pub(crate) fn load_ext(store: &Store) -> SessionExt {
    store
        .sessions
        .get(KEY_EXT)
        .ok()
        .flatten()
        .and_then(|b| bincode::deserialize::<SessionExt>(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn save_ext(store: &Store, ext: &SessionExt) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(ext).map_err(|e| bin_err("session ext", e))?;
    store.sessions.insert(KEY_EXT, bytes).map_err(RecommenderError::DbError)?;
    Ok(())
}

/// Updates the skip streak from a finished listen and returns the new value.
pub(crate) fn record_skip_streak(
    store: &Store,
    ext: &mut SessionExt,
    ratio: f32,
) -> Result<u32, RecommenderError> {
    if ratio < 0.3 {
        ext.skip_streak += 1;
    } else {
        ext.skip_streak = 0;
    }
    save_ext(store, ext)?;
    Ok(ext.skip_streak)
}

/// Remembers which signal drove each song of the latest recommendation batch
/// (capped, so the map stays tiny).
pub(crate) fn remember_recs(
    store: &Store,
    ext: &mut SessionExt,
    sources: Vec<(String, String)>,
) -> Result<(), RecommenderError> {
    ext.rec_sources = sources;
    if ext.rec_sources.len() > 24 {
        ext.rec_sources.drain(..ext.rec_sources.len() - 24);
    }
    save_ext(store, ext)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FeatureVector;

    fn record(axis: usize) -> SongRecord {
        let mut fv = FeatureVector {
            version: 1,
            mfcc: [0.0; 13],
            spectral_centroid: 0.0,
            spectral_rolloff: 0.0,
            zero_crossing_rate: 0.0,
            rms_energy: 0.0,
            tempo_bpm: 0.0,
            chroma: [0.0; 12],
        };
        fv.mfcc[axis] = 1.0;
        SongRecord {
            id: format!("s{axis}"),
            file_path: format!("/s{axis}"),
            title: format!("S{axis}"),
            artist: String::new(),
            album: String::new(),
            duration_secs: 100,
            feature_vector: fv,
            indexed_at: Utc::now(),
            replay_gain_db: 0.0,
        }
    }

    #[test]
    fn skips_do_not_move_centroid() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let mut session = start_session(&store).unwrap();

        // Seed the mood with a fully-heard axis-0 song.
        update_centroid(&store, &mut session, &record(0), 1.0).unwrap();
        let before = session.centroid.clone().unwrap();

        // Rapid-skip an axis-7 song — centroid must not move.
        update_centroid(&store, &mut session, &record(7), 0.05).unwrap();
        assert_eq!(session.centroid, Some(before));
    }

    #[test]
    fn centroid_anchors_on_full_listen_and_shifts_gradually() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let mut session = start_session(&store).unwrap();

        update_centroid(&store, &mut session, &record(0), 1.0).unwrap();
        let seed = session.centroid.clone().unwrap();
        // First full listen anchors the centroid exactly (MFCCs are ×2 in
        // the weighted vector, hence 2.0).
        assert!((seed[0] - 2.0).abs() < 1e-6, "got {}", seed[0]);

        // A half-heard different song shifts the centroid partway.  (The
        // weighted vector doubles MFCCs, so compare against the seed.)
        update_centroid(&store, &mut session, &record(5), 0.5).unwrap();
        let after = session.centroid.clone().unwrap();
        assert!(after[5] > 0.0 && after[5] < seed[0] * 0.25, "shift must be partial, got {}", after[5]);
        assert!(after[0] > seed[0] * 0.7, "seed influence should dominate, got {}", after[0]);
    }

    #[test]
    fn history_dedups_consecutive_repeats() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let mut session = start_session(&store).unwrap();

        push_history(&store, &mut session, "a").unwrap();
        push_history(&store, &mut session, "a").unwrap();
        push_history(&store, &mut session, "b").unwrap();
        push_history(&store, &mut session, "b").unwrap();
        assert_eq!(session.song_history, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn session_similarity_neutral_without_centroid() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let session = start_session(&store).unwrap();
        let sim = get_session_similarity(&session, &record(3));
        assert!((sim - 0.5).abs() < 1e-6, "no centroid must be neutral 0.5, got {sim}");
    }

    #[test]
    fn ext_skip_streak_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let mut ext = load_ext(&store);
        assert_eq!(ext.skip_streak, 0);

        assert_eq!(record_skip_streak(&store, &mut ext, 0.1).unwrap(), 1);
        assert_eq!(record_skip_streak(&store, &mut ext, 0.1).unwrap(), 2);
        assert_eq!(record_skip_streak(&store, &mut ext, 0.9).unwrap(), 0, "a real listen resets");

        remember_recs(&store, &mut ext, vec![("s1".into(), "artist".into())]).unwrap();
        let reloaded = load_ext(&store);
        assert_eq!(reloaded.rec_sources.len(), 1);
        assert_eq!(reloaded.skip_streak, 0, "streak persisted through the reset");
    }
}
