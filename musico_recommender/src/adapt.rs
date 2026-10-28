//! Adaptive signal weights — the engine learns which of its signals actually
//! predict what the user wants, and shifts weight accordingly.
//!
//! Every recommendation batch remembers, per returned song, which signal
//! contributed most (`dominant_signal`).  When that song later ends, the
//! outcome (completed vs skipped) nudges that signal's weight:
//!
//! - completed listen (ratio ≥ 0.7)  → outcome +1 → signal gains weight
//! - fast skip (ratio ≤ 0.2)         → outcome −1 → signal loses weight
//! - anything in between             → no adaptation
//!
//! Weights live under the default-tree key `adapt:weights`, are clamped to
//! [0.02, 0.55] each and renormalised to sum 1.0 after every nudge, so the
//! loop can't collapse or explode.  A separate `exploration` multiplier
//! (applied to the discovery weight at scoring time) widens the net after
//! skips and tightens it after completions.

use crate::errors::RecommenderError;
use crate::models::{bin_err, SignalWeights, Store};

const KEY: &[u8] = b"adapt:weights";

/// Step size per outcome.
const LR: f32 = 0.04;
const W_MIN: f32 = 0.02;
const W_MAX: f32 = 0.55;
const EXPLORATION_MIN: f32 = 0.6;
const EXPLORATION_MAX: f32 = 2.0;

/// Loads the persisted weights, falling back to defaults (and persisting them)
/// when missing or corrupt.
pub(crate) fn load(store: &Store) -> Result<SignalWeights, RecommenderError> {
    match store.db.get(KEY)? {
        Some(bytes) => match bincode::deserialize::<SignalWeights>(&bytes) {
            Ok(mut w) => {
                w.normalize();
                Ok(w)
            }
            Err(e) => {
                log::warn!("Corrupt signal weights: {e}. Resetting to defaults.");
                let w = SignalWeights::default();
                save(store, &w)?;
                Ok(w)
            }
        },
        None => {
            let w = SignalWeights::default();
            save(store, &w)?;
            Ok(w)
        }
    }
}

pub(crate) fn save(store: &Store, w: &SignalWeights) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(w).map_err(|e| bin_err("signal weights", e))?;
    store.db.insert(KEY, bytes)?;
    Ok(())
}

/// Maps a signal name to its weight slot (single source of truth for both
/// read and write paths).
fn slot_mut<'a>(w: &'a mut SignalWeights, signal: &str) -> Option<&'a mut f32> {
    match signal {
        "similarity" => Some(&mut w.similarity),
        "affinity" => Some(&mut w.affinity),
        "profile" => Some(&mut w.profile),
        "session" => Some(&mut w.session),
        "artist" => Some(&mut w.artist),
        "cluster" => Some(&mut w.cluster),
        "context" => Some(&mut w.context),
        "discovery" => Some(&mut w.discovery),
        _ => None,
    }
}

/// Applies one learning step: `outcome` is +1 (completed), −1 (skipped) or 0
/// (ignore).  Nudges the dominant signal, clamps, renormalises, and adapts
/// the exploration multiplier in the opposite direction of the outcome
/// (skips widen the net, completions tighten it).
pub(crate) fn apply_outcome(
    store: &Store,
    signal: &str,
    outcome: f32,
) -> Result<(), RecommenderError> {
    if outcome.abs() < f32::EPSILON {
        return Ok(());
    }
    let mut w = load(store)?;

    if let Some(slot) = slot_mut(&mut w, signal) {
        *slot = (*slot + LR * outcome).clamp(W_MIN, W_MAX);
    }
    w.normalize();

    w.exploration = (w.exploration - 0.08 * outcome).clamp(EXPLORATION_MIN, EXPLORATION_MAX);
    save(store, &w)
}

/// Classification of a finished listen into an adaptation outcome.
pub(crate) fn outcome_for(ratio: f32) -> f32 {
    if ratio >= 0.7 {
        1.0
    } else if ratio <= 0.2 {
        -1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_listen_gains_dominant_signal_weight() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        let before = load(&store).unwrap();
        assert!((before.similarity - 0.22).abs() < 1e-6);

        for _ in 0..10 {
            apply_outcome(&store, "similarity", 1.0).unwrap();
        }
        let after = load(&store).unwrap();
        assert!(
            after.similarity > before.similarity,
            "repeated completions should raise similarity weight: {} -> {}",
            before.similarity,
            after.similarity
        );
        let sum: f32 = after.slots().iter().map(|(_, w)| w).sum();
        assert!((sum - 1.0).abs() < 1e-4, "weights must stay normalised, sum = {sum}");
        assert!(after.exploration < 1.0, "completions should tighten exploration");
    }

    #[test]
    fn skips_drain_signal_and_widen_exploration() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        for _ in 0..10 {
            apply_outcome(&store, "artist", -1.0).unwrap();
        }
        let w = load(&store).unwrap();
        assert!(
            w.artist < 0.12,
            "skip-heavy dominant signal should lose weight, got {}",
            w.artist
        );
        assert!(w.exploration > 1.0, "skips should widen exploration, got {}", w.exploration);
        assert!(w.artist >= W_MIN);
    }

    #[test]
    fn weights_cannot_hit_absolute_zero_or_take_over() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        for _ in 0..200 {
            apply_outcome(&store, "discovery", -1.0).unwrap();
        }
        let w = load(&store).unwrap();
        assert!(w.discovery >= W_MIN - 1e-6);
        for v in [w.similarity, w.profile, w.session, w.artist, w.cluster, w.context] {
            assert!(v >= W_MIN - 1e-6);
            assert!(v <= W_MAX + 1e-6);
        }
    }

    #[test]
    fn outcome_classification() {
        assert_eq!(outcome_for(1.0), 1.0);
        assert_eq!(outcome_for(0.75), 1.0);
        assert_eq!(outcome_for(0.5), 0.0);
        assert_eq!(outcome_for(0.1), -1.0);
    }

    #[test]
    fn unknown_signal_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        apply_outcome(&store, "vibes", 1.0).unwrap();
        let w = load(&store).unwrap();
        assert!((w.exploration - SignalWeights::default().exploration + 0.08).abs() < 1e-4);
    }
}
