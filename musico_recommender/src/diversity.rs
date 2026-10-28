//! Final-list diversity — Maximal Marginal Relevance re-ranking with a
//! per-artist cap.
//!
//! Pure score-sorted lists cluster around the current song's sound and often
//! return five tracks by one artist.  MMR greedily selects candidates by
//!
//!   argmax  λ·relevance − (1−λ)·max_similarity_to_already_selected
//!
//! which keeps high scorers but refuses near-duplicates, and the artist cap
//! guarantees the list stays a playlist, not a discography.

use crate::models::RecommendedSong;
use crate::vector_store;

/// Relevance vs. diversity trade-off.  0.75 leans relevance while still
/// enforcing noticeable variety.
const LAMBDA: f32 = 0.75;

/// Maximum tracks from one artist in a returned batch (empty artist = no cap,
/// since we can't tell artists apart).
const MAX_PER_ARTIST: usize = 2;

/// Selects `top_n` entries from `candidates` (already sorted by final_score
/// descending) via greedy MMR with the per-artist cap.
pub(crate) fn mmr_select(candidates: &[RecommendedSong], top_n: usize) -> Vec<RecommendedSong> {
    if candidates.is_empty() || top_n == 0 {
        return Vec::new();
    }

    // Pre-compute weighted vectors once for the diversity term.
    let vecs: Vec<Vec<f32>> = candidates
        .iter()
        .map(|c| c.record.feature_vector.to_weighted_vec())
        .collect();

    // Normalise relevance against the pool's best so λ operates on [0, 1].
    let max_score = candidates
        .iter()
        .map(|c| c.final_score)
        .fold(f32::MIN, f32::max)
        .max(1e-6);

    let mut selected: Vec<usize> = Vec::with_capacity(top_n);
    let mut artist_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut remaining: Vec<usize> = (0..candidates.len()).collect();

    while selected.len() < top_n && !remaining.is_empty() {
        let mut best_idx = remaining[0];
        let mut best_mmr = f32::MIN;

        for &i in &remaining {
            let c = &candidates[i];
            let artist = c.record.artist.as_str();
            if !artist.is_empty()
                && artist_counts.get(artist).copied().unwrap_or(0) >= MAX_PER_ARTIST
            {
                continue;
            }

            let relevance = c.final_score / max_score;
            let max_sim = selected
                .iter()
                .map(|&j| vector_store::cosine_similarity(&vecs[i], &vecs[j]))
                .fold(0.0_f32, f32::max);

            let mmr = LAMBDA * relevance - (1.0 - LAMBDA) * max_sim;
            if mmr > best_mmr {
                best_mmr = mmr;
                best_idx = i;
            }
        }

        // Every remaining candidate is artist-capped — fall back to the
        // highest-scored one so the list can still be filled.
        if !remaining.contains(&best_idx) {
            best_idx = remaining[0];
        }

        remaining.retain(|&i| i != best_idx);
        let artist = candidates[best_idx].record.artist.clone();
        if !artist.is_empty() {
            *artist_counts.entry(artist).or_insert(0) += 1;
        }
        selected.push(best_idx);
    }

    selected.into_iter().map(|i| candidates[i].clone()).collect()
}

/// Convenience: candidates from records + scores (used by tests).
#[cfg(test)]
use crate::models::SongRecord;

#[cfg(test)]
fn candidate(record: &SongRecord, score: f32) -> RecommendedSong {
    RecommendedSong {
        record: record.clone(),
        final_score: score,
        similarity_score: score,
        session_match: 0.5,
        affinity: 0.5,
        cooldown: 0.0,
        profile_match: 0.5,
        artist_match: 0.5,
        cluster_match: 0.5,
        context_match: 0.5,
        dominant_signal: "similarity",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FeatureVector;
    use chrono::Utc;

    fn record(id: &str, artist: &str, axis: usize) -> SongRecord {
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
            id: id.into(),
            file_path: format!("/{id}"),
            title: id.into(),
            artist: artist.into(),
            album: String::new(),
            duration_secs: 100,
            feature_vector: fv,
            indexed_at: Utc::now(),
            replay_gain_db: 0.0,
        }
    }

    #[test]
    fn picks_respect_artist_cap() {
        let candidates: Vec<RecommendedSong> = (0..6)
            .map(|i| {
                // All six by one artist, all orthogonally different sound.
                candidate(&record(&format!("s{i}"), "SameArtist", i), 1.0 - i as f32 * 0.01)
            })
            .collect();

        let picked = mmr_select(&candidates, 6);
        assert_eq!(picked.len(), 6);
        let count = picked
            .iter()
            .filter(|p| p.record.artist == "SameArtist")
            .count();
        assert_eq!(count, 6, "cap falls back when every candidate shares one artist");
    }

    #[test]
    fn diverse_candidates_all_get_in_before_lower_scored() {
        // Four distinct-sounding songs by four artists with descending scores,
        // then a near-clone of the top song (same axis) with a high score.
        let mut candidates = vec![
            candidate(&record("a", "A", 0), 1.00),
            candidate(&record("b", "B", 1), 0.95),
            candidate(&record("c", "C", 2), 0.90),
            candidate(&record("d", "D", 3), 0.85),
        ];
        // Clone of "a": identical vector, different artist, slightly lower score.
        candidates.push(candidate(&record("a2", "E", 0), 0.99));

        let picked = mmr_select(&candidates, 4);
        let ids: Vec<&str> = picked.iter().map(|p| p.record.id.as_str()).collect();
        assert!(
            !ids.contains(&"a2") || picked.len() == 5,
            "near-clone of the top pick should be deprioritised"
        );
        // The top three distinct songs must all make the cut of 4.
        for want in ["a", "b", "c"] {
            assert!(ids.contains(&want), "expected {want} in {ids:?}");
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(mmr_select(&[], 5).is_empty());
    }
}
