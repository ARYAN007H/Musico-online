//! Per-artist taste aggregation — the song-level affinity cache generalised
//! to artists.  Songs with no plays of their own still inherit their artist's
//! reputation, which is the dominant cold-start signal in real libraries.
//!
//! Layout in the `artists` tree: `a:{artist}` → bincode `ArtistScoreCache`.
//! Empty artist strings are never indexed.

use chrono::Utc;

use crate::errors::RecommenderError;
use crate::models::{ArtistScoreCache, Store};

const KEY_PREFIX: &str = "a:";

fn key(artist: &str) -> String {
    format!("{KEY_PREFIX}{artist}")
}

fn load(store: &Store, artist: &str) -> Result<ArtistScoreCache, RecommenderError> {
    Ok(store
        .artists
        .get(key(artist).as_bytes())?
        .and_then(|b| bincode::deserialize::<ArtistScoreCache>(&b).ok())
        .unwrap_or_default())
}

fn save(store: &Store, artist: &str, c: &ArtistScoreCache) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(c).map_err(|e| {
        crate::models::bin_err("artist cache", e)
    })?;
    store.artists.insert(key(artist).as_bytes(), bytes)?;
    Ok(())
}

/// Same scale as the song-level listen weights.
fn listen_signal(ratio: f32) -> f32 {
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

/// Folds a listen into the artist's aggregate.
pub(crate) fn bump(
    store: &Store,
    artist: &str,
    ratio: f32,
    was_skip: bool,
) -> Result<(), RecommenderError> {
    if artist.is_empty() {
        return Ok(());
    }
    let mut c = load(store, artist)?;
    c.affinity = (c.affinity + listen_signal(ratio)).clamp(-5.0, 5.0);
    c.plays += 1;
    if was_skip {
        c.skips += 1;
    }
    c.last_played_ts = Utc::now().timestamp();
    save(store, artist, &c)
}

/// Artist affinity match for scoring, [0, 1] (0.5 = unknown artist).
pub(crate) fn affinity_match(store: &Store, artist: &str) -> Result<f32, RecommenderError> {
    if artist.is_empty() {
        return Ok(0.5);
    }
    let c = load(store, artist)?;
    if c.plays == 0 {
        return Ok(0.5);
    }
    Ok((0.5 + c.affinity / 10.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_artist_is_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        assert!((affinity_match(&store, "Nobody").unwrap() - 0.5).abs() < 1e-6);
        assert!((affinity_match(&store, "").unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn completions_love_skips_hate() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        for _ in 0..6 {
            bump(&store, "Aurora", 1.0, false).unwrap();
        }
        let loved = affinity_match(&store, "Aurora").unwrap();
        assert!(loved > 0.55, "completion-heavy artist should be above neutral, got {loved}");

        for _ in 0..6 {
            bump(&store, "Skipper", 0.05, true).unwrap();
        }
        let hated = affinity_match(&store, "Skipper").unwrap();
        assert!(hated < 0.45, "skip-heavy artist should fall below neutral, got {hated}");
        assert!(loved > hated);
    }

    #[test]
    fn affinity_clamps_at_five() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        for _ in 0..40 {
            bump(&store, "Loved", 1.0, false).unwrap();
        }
        let c = load(&store, "Loved").unwrap();
        assert!((c.affinity - 5.0).abs() < 1e-4, "affinity must clamp to +5, got {}", c.affinity);
        assert!((affinity_match(&store, "Loved").unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_artist_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        bump(&store, "", 1.0, false).unwrap();
        assert!(store.artists.get(b"a:").unwrap().is_none());
    }
}
