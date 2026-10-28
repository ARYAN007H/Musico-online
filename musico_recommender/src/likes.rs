//! Explicit like/dislike storage and its effect on scoring.
//!
//! Likes are the loudest preference signal the user can give, so they act on
//! three levels:
//! 1. **Song level** — like adds a fixed affinity bonus at read time
//!    (no score-cache rewrite); dislike subtracts more, and disliked songs
//!    are hard-excluded from recommendations.
//! 2. **Profile level** — likes/dislikes feed the taste profile via
//!    [`crate::profile::refresh_like_vectors`].
//! 3. **Cooldown level** — liked songs tolerate far more repetition
//!    (cooldown × `LIKED_COOLDOWN_SCALE`).
//!
//! Layout in the `likes` tree: `{song_id}` → one byte
//! (`1` = liked, `2` = disliked; see [`LikeState`]).

use crate::errors::RecommenderError;
use crate::models::{LikeState, Store};

/// Added to the song's cached affinity (range [-5, +5]) when liked.
/// Deliberate feedback deserves to outweigh a handful of passive listens.
pub const LIKE_AFFINITY_BONUS: f32 = 3.0;
/// Subtracted from the song's cached affinity when disliked.
pub const DISLIKE_AFFINITY_PENALTY: f32 = 3.0;
/// Liked songs are replay-friendly: their cooldown is scaled down hard.
pub const LIKED_COOLDOWN_SCALE: f32 = 0.25;

/// Writes the like state.  `Neutral` removes the entry.
pub(crate) fn set(store: &Store, song_id: &str, state: LikeState) -> Result<(), RecommenderError> {
    match state {
        LikeState::Neutral => {
            store.likes.remove(song_id.as_bytes())?;
        }
        s => {
            store
                .likes
                .insert(song_id.as_bytes(), &[s.to_byte()])?;
        }
    }
    store.likes.flush()?;
    Ok(())
}

/// Reads the like state for a song.
pub(crate) fn get(store: &Store, song_id: &str) -> Result<LikeState, RecommenderError> {
    Ok(store
        .likes
        .get(song_id.as_bytes())?
        .and_then(|b| b.first().copied())
        .map(LikeState::from_byte)
        .unwrap_or(LikeState::Neutral))
}

/// All song ids with their non-neutral like state.
pub(crate) fn all(store: &Store) -> Result<Vec<(String, LikeState)>, RecommenderError> {
    let mut out = Vec::new();
    for entry in store.likes.iter() {
        let (key, value) = entry?;
        let state = LikeState::from_byte(value.first().copied().unwrap_or(0));
        if state != LikeState::Neutral {
            out.push((String::from_utf8_lossy(&key).to_string(), state));
        }
    }
    Ok(out)
}

/// The affinity contribution a song's like state adds to its cached affinity.
pub(crate) fn affinity_delta(state: LikeState) -> f32 {
    match state {
        LikeState::Liked => LIKE_AFFINITY_BONUS,
        LikeState::Disliked => -DISLIKE_AFFINITY_PENALTY,
        LikeState::Neutral => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        set(&store, "a", LikeState::Liked).unwrap();
        set(&store, "b", LikeState::Disliked).unwrap();
        assert_eq!(get(&store, "a").unwrap(), LikeState::Liked);
        assert_eq!(get(&store, "b").unwrap(), LikeState::Disliked);
        assert_eq!(get(&store, "c").unwrap(), LikeState::Neutral);

        let all_states = all(&store).unwrap();
        assert_eq!(all_states.len(), 2);

        set(&store, "a", LikeState::Neutral).unwrap();
        assert_eq!(get(&store, "a").unwrap(), LikeState::Neutral);
        assert_eq!(all(&store).unwrap().len(), 1);
    }

    #[test]
    fn affinity_deltas_match_spec() {
        assert!((affinity_delta(LikeState::Liked) - LIKE_AFFINITY_BONUS).abs() < 1e-6);
        assert!((affinity_delta(LikeState::Disliked) + DISLIKE_AFFINITY_PENALTY).abs() < 1e-6);
        assert!(affinity_delta(LikeState::Neutral).abs() < 1e-6);
    }
}
