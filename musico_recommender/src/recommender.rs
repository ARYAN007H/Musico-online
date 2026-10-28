//! Final recommendation pipeline — engine v2.
//!
//! Seven signals, adaptive weights, all scored from an in-memory library
//! cache (zero sled reads per candidate for the similarity/session/profile
//! math; O(1) cached reads for affinity/cooldown/artist/cluster/context):
//!
//! 1. **similarity**  — cosine to the playing track (or session centroid /
//!    taste profile when nothing plays)
//! 2. **profile**     — cosine to the long-term taste profile, scaled by how
//!    much the profile has learned yet
//! 3. **session**     — cosine to the skip-aware session mood centroid
//! 4. **artist**      — per-artist affinity (artists inherit their songs' rep)
//! 5. **cluster**     — pseudo-genre (online k-means) affinity
//! 6. **context**     — time-of-day sound match (tempo/energy/brightness)
//! 7. **discovery**   — smooth novelty decay, boosted when the user keeps
//!    skipping (skip streak + adaptive exploration multiplier)
//!
//! Then: cooldown gate (softened for liked songs), same-artist fatigue,
//! greedy MMR re-rank with a per-artist cap, and per-song dominant-signal
//! attribution that feeds the online weight-learning loop.

use chrono::Local;

use crate::context;
use crate::diversity::mmr_select;
use crate::errors::RecommenderError;
use crate::history;
use crate::likes;
use crate::models::{LikeState, RecommendationRequest, RecommendedSong, SongCache, Store};
use crate::profile;
use crate::session;
use crate::vector_store;

/// Candidates kept for the MMR stage — wide enough for the artist cap and
/// diversity term to bite, small enough to keep the pass O(pool).
const MMR_POOL: usize = 24;

/// Above this library size, candidate generation samples instead of scoring
/// everything (scoring is O(N × 30) which is still trivial at 5 000).
const FULL_SCAN_LIMIT: usize = 5_000;

/// Skips this streak long? Extra exploration multiplier per additional skip.
const SKIP_STREAK_BOOST: f32 = 0.15;
const SKIP_STREAK_BOOST_CAP: f32 = 1.6;

pub(crate) fn recommend(
    store: &Store,
    req: &RecommendationRequest,
    cache: &SongCache,
) -> Result<Vec<RecommendedSong>, RecommenderError> {
    if store.song_count()? < 3 {
        return Err(RecommenderError::InsufficientLibrary);
    }
    if cache.is_empty() {
        return Ok(Vec::new());
    }

    // ---- Anchors & shared context ----
    let anchor_vec = cache.vec_of(&req.current_song_id).cloned();
    let anchor_artist = req.anchor_artist.clone();
    let profile_vec = profile::effective(store)?;
    let profile_strength = profile::confidence(store)?;
    let bucket = context::bucket_for(Local::now());

    // Session artist fatigue counts: how often each artist appeared in the
    // recent session history.
    let mut session_artist_counts: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::new();
    for id in &req.session_state.song_history {
        if let Some(rec) = cache.get(id) {
            if !rec.artist.is_empty() {
                *session_artist_counts.entry(rec.artist.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut exclude: std::collections::HashSet<&str> =
        req.exclude_ids.iter().map(String::as_str).collect();
    if !req.current_song_id.is_empty() {
        exclude.insert(req.current_song_id.as_str());
    }

    // ---- Candidate generation ----
    let candidate_idx = select_candidates(store, req, cache, &anchor_vec, &profile_vec)?;

    // ---- Scoring ----
    let mut candidates: Vec<RecommendedSong> = Vec::new();
    for i in candidate_idx {
        let record = &cache.records[i];
        let vec = &cache.vecs[i];
        if exclude.contains(record.id.as_str()) {
            continue;
        }
        let like_state = cache.likes.get(&record.id).copied().unwrap_or(LikeState::Neutral);
        if like_state == LikeState::Disliked {
            continue; // hard exclusion — the user said no
        }

        let similarity_score = anchor_vec
            .as_ref()
            .map(|a| vector_store::cosine_similarity(a, vec).clamp(0.0, 1.0))
            .unwrap_or(0.5);
        let profile_match = profile_vec
            .as_ref()
            .map(|p| vector_store::cosine_similarity(p, vec).clamp(0.0, 1.0))
            .unwrap_or(0.5);
        let session_match = session::get_session_similarity(&req.session_state, record);
        let artist_match = crate::artists::affinity_match(store, &record.artist)?;
        let cluster_match =
            clusters_match(store, cache.clusters.get(&record.id).copied())?;
        let context_match = context::match_score(store, bucket, record)?;

        let like_delta = likes::affinity_delta(like_state);
        let raw_affinity = history::get_song_affinity(store, &record.id)? + like_delta;
        let affinity = ((raw_affinity + 5.0) / 10.0).clamp(0.0, 1.0);

        let play_count = history::play_count(store, &record.id)?;
        let discovery = if play_count == 0 {
            1.0
        } else {
            (1.0 - play_count as f32 / 6.0).max(0.0)
        };

        let mut cooldown = history::get_cooldown_score(store, &record.id)?;
        if like_state == LikeState::Liked {
            cooldown *= likes::LIKED_COOLDOWN_SCALE;
        }
        if cooldown > 0.7 {
            continue;
        }

        // Same-artist fatigue: the anchor's artist gets tired fastest;
        // any artist dominating the session history tires more slowly.
        let fatigue = match session_artist_counts.get(record.artist.as_str()).copied() {
            Some(n) if !record.artist.is_empty() && record.artist == anchor_artist && n >= 2 => 0.12,
            Some(n) if n >= 3 => 0.08,
            _ => 0.0,
        };

        let w = &req.weights;
        let exploration = w.exploration
            * if req.skip_streak >= 2 {
                (1.0 + SKIP_STREAK_BOOST * (req.skip_streak - 1).min(4) as f32)
                    .min(SKIP_STREAK_BOOST_CAP)
            } else {
                1.0
            };

        let contributions: [(&'static str, f32); 8] = [
            ("similarity", w.similarity * similarity_score),
            ("affinity", w.affinity * affinity),
            ("profile", w.profile * profile_strength * profile_match),
            ("session", w.session * session_match),
            ("artist", w.artist * artist_match),
            ("cluster", w.cluster * cluster_match),
            ("context", w.context * context_match),
            ("discovery", w.discovery * exploration * discovery),
        ];
        let final_score = contributions.iter().map(|(_, c)| c).sum::<f32>() - fatigue;
        let dominant_signal = contributions
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(name, _)| *name)
            .unwrap_or("similarity");

        candidates.push(RecommendedSong {
            record: record.clone(),
            final_score,
            similarity_score,
            session_match,
            affinity,
            cooldown,
            profile_match,
            artist_match,
            cluster_match,
            context_match,
            dominant_signal,
        });
    }

    // ---- Rank, diversify, return ----
    candidates.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(MMR_POOL);
    Ok(mmr_select(&candidates, req.top_n))
}

/// Chooses which songs enter the scoring pass.  Small libraries score in
/// full (global optimum, and O(N×30) is microseconds); large ones sample a
/// union of the most promising slices.
fn select_candidates(
    store: &Store,
    req: &RecommendationRequest,
    cache: &SongCache,
    anchor_vec: &Option<Vec<f32>>,
    profile_vec: &Option<Vec<f32>>,
) -> Result<Vec<usize>, RecommenderError> {
    if cache.len() <= FULL_SCAN_LIMIT {
        return Ok((0..cache.len()).collect());
    }

    let mut picked: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    let mut top_by = |target: &[f32], n: usize| {
        let mut scored: Vec<(usize, f32)> = cache
            .vecs
            .iter()
            .enumerate()
            .map(|(i, v)| (i, vector_store::cosine_similarity(target, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        picked.extend(scored.into_iter().take(n).map(|(i, _)| i));
    };
    if let Some(a) = anchor_vec {
        top_by(a, 50);
    }
    if let Some(p) = profile_vec {
        top_by(p, 40);
    }

    // Rotating discovery sample: changes daily, stable within a session, so
    // large libraries still surface fresh material over time.
    let day_seed = (chrono::Utc::now().timestamp() / 86_400) as u64;
    let session_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        req.session_state.session_id.hash(&mut h);
        h.finish()
    };
    let mut rng = rand::rngs::StdRng::seed_from_u64(day_seed ^ session_hash);
    use rand::{seq::SliceRandom, SeedableRng};
    let mut all: Vec<usize> = (0..cache.len()).collect();
    all.shuffle(&mut rng);
    picked.extend(all.into_iter().take(60));

    // Silence the unused-store lint — kept for API symmetry with the small
    // path and future affinity-ranked candidate slices.
    let _ = store;
    Ok(picked.into_iter().collect())
}

/// Cluster match, with the unknown-assignment case neutral.
fn clusters_match(store: &Store, cluster: Option<u8>) -> Result<f32, RecommenderError> {
    crate::clusters::affinity_match(store, cluster)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{FeatureVector, SessionState};
    use crate::SignalWeights;
    use chrono::Utc;

    fn fv(axis: usize, val: f32) -> FeatureVector {
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
        fv.mfcc[axis] = val;
        fv
    }

    fn record(id: &str, artist: &str, axis: usize) -> crate::models::SongRecord {
        crate::models::SongRecord {
            id: id.into(),
            file_path: format!("/{id}"),
            title: id.into(),
            artist: artist.into(),
            album: String::new(),
            duration_secs: 100,
            feature_vector: fv(axis, 1.0),
            indexed_at: Utc::now(),
            replay_gain_db: 0.0,
        }
    }

    fn setup(ids: &[(&str, &str, usize)]) -> (Store, SongCache) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let mut cache = SongCache::default();
        for (_i, (id, artist, axis)) in ids.iter().enumerate() {
            let rec = record(id, artist, *axis);
            let bytes = bincode::serialize(&rec).unwrap();
            store.songs.insert(rec.id.as_bytes(), bytes).unwrap();
            store.increment_song_count().unwrap();
            cache.push(rec);
        }
        // Keep the tempdir alive until the test ends by leaking it (test-only).
        std::mem::forget(tmp);
        (store, cache)
    }

    fn request(current: &str, top_n: usize) -> RecommendationRequest {
        RecommendationRequest {
            current_song_id: current.into(),
            session_state: SessionState {
                session_id: "sess".into(),
                started_at: Utc::now(),
                song_history: vec![current.into()],
                centroid: None,
            },
            top_n,
            exclude_ids: vec![current.into()],
            weights: SignalWeights::default(),
            anchor_artist: String::new(),
            skip_streak: 0,
        }
    }

    #[test]
    fn too_small_library_errors() {
        let (store, cache) = setup(&[("a", "A", 0), ("b", "B", 1)]);
        assert!(matches!(
            recommend(&store, &request("a", 3), &cache),
            Err(RecommenderError::InsufficientLibrary)
        ));
    }

    #[test]
    fn dislikes_are_hard_excluded() {
        let (store, mut cache) = setup(&[
            ("playing", "P", 0),
            ("loved", "L", 0),      // most similar
            ("other1", "O1", 1),
            ("other2", "O2", 2),
        ]);
        likes::set(&store, "loved", LikeState::Disliked).unwrap();
        cache.likes.insert("loved".into(), LikeState::Disliked);

        let recs = recommend(&store, &request("playing", 3), &cache).unwrap();
        assert!(!recs.iter().any(|r| r.record.id == "loved"));
        assert!(!recs.is_empty());
    }

    #[test]
    fn liked_song_rises_to_top() {
        let (store, mut cache) = setup(&[
            ("playing", "P", 0),
            ("liked", "L", 0),   // same sound as the anchor, but explicitly liked
            ("neutral", "N", 0), // equally similar, no like
            ("filler", "F", 5),
        ]);
        likes::set(&store, "liked", LikeState::Liked).unwrap();
        cache.likes.insert("liked".into(), LikeState::Liked);

        let recs = recommend(&store, &request("playing", 2), &cache).unwrap();
        assert_eq!(recs[0].record.id, "liked", "explicit like must beat raw similarity");
    }

    #[test]
    fn works_without_anchor_or_profile() {
        let (store, cache) = setup(&[
            ("a", "A", 0),
            ("b", "B", 1),
            ("c", "C", 2),
            ("d", "D", 3),
        ]);
        let mut req = request("", 3);
        req.exclude_ids.clear();
        let recs = recommend(&store, &req, &cache).unwrap();
        assert!(!recs.is_empty(), "cold start must still return picks");
    }

    #[test]
    fn attribution_is_present_on_every_rec() {
        let (store, cache) = setup(&[
            ("a", "A", 0),
            ("b", "B", 1),
            ("c", "C", 2),
            ("d", "D", 3),
        ]);
        let recs = recommend(&store, &request("a", 2), &cache).unwrap();
        assert!(recs.iter().all(|r| !r.dominant_signal.is_empty()));
    }

    #[test]
    fn skip_streak_boosts_discovery_contribution() {
        let (store, cache) = setup(&[
            ("a", "A", 0),
            ("b", "B", 1),
            ("c", "C", 2),
            ("d", "D", 3),
            ("e", "E", 4),
        ]);

        let mut calm = request("a", 4);
        calm.skip_streak = 0;
        let mut frantic = request("a", 4);
        frantic.skip_streak = 4;

        // All four non-anchor candidates are never-played (discovery = 1.0),
        // so the only scoring difference between the two runs is the
        // skip-streak exploration multiplier on the discovery signal.
        let calm_recs = recommend(&store, &calm, &cache).unwrap();
        let frantic_recs = recommend(&store, &frantic, &cache).unwrap();
        let score_of = |rs: &[RecommendedSong], id: &str| {
            rs.iter()
                .find(|r| r.record.id == id)
                .map(|r| r.final_score)
                .expect("candidate must be ranked in both runs")
        };
        let c_calm = score_of(&calm_recs, "c");
        let c_frantic = score_of(&frantic_recs, "c");
        assert!(
            c_frantic > c_calm,
            "skip streak should lift discovery-driven scores: calm {c_calm} vs frantic {c_frantic}"
        );
    }
}
