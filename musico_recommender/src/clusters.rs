//! Pseudo-genre clustering — an online k-means (k=8) over the weighted feature
//! vectors.  Gives the engine "genre-like" taste signals without any metadata
//! tags: songs that sound alike land in the same cluster, and per-cluster
//! affinity tracks how much the user enjoys each sound-world.
//!
//! Layout in the `clusters` tree:
//! - `centers`          → bincode `Vec<ClusterCenter>`
//! - `assign:{song_id}` → single byte, the cluster id
//! - `aff:{cluster}`    → bincode `ClusterScore`
//!
//! Assignment is sequential/online (each song joins the nearest centre, which
//! then nudges toward it).  Not globally optimal, but O(1) per song, requires
//! no batch re-fit, and is stable enough for taste aggregation.  Cluster ids
//! are opaque and never reordered.

use crate::errors::RecommenderError;
use crate::models::{bin_err, ClusterCenter, ClusterScore, Store};
use crate::vector_store;

/// Maximum number of clusters.
pub const N_CLUSTERS: u8 = 8;

const KEY_CENTERS: &[u8] = b"centers";

fn load_centers(store: &Store) -> Result<Vec<ClusterCenter>, RecommenderError> {
    match store.clusters.get(KEY_CENTERS)? {
        Some(bytes) => Ok(bincode::deserialize(&bytes)
            .map_err(|e| bin_err("cluster centers", e))?),
        None => Ok(Vec::new()),
    }
}

fn save_centers(store: &Store, centers: &[ClusterCenter]) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(&centers).map_err(|e| bin_err("cluster centers", e))?;
    store.clusters.insert(KEY_CENTERS, bytes)?;
    Ok(())
}

/// Nearest centre by cosine similarity (ties → first).
fn nearest<'a>(centers: &'a [ClusterCenter], vec: &[f32]) -> Option<&'a ClusterCenter> {
    centers
        .iter()
        .map(|c| (c, vector_store::cosine_similarity(&c.mean, vec)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(c, _)| c)
}

/// Assigns a song to a cluster, creating a new centre while we are below
/// `N_CLUSTERS`, and updates the winning centre's mean incrementally.
/// Returns the assigned cluster id.
pub(crate) fn assign(
    store: &Store,
    song_id: &str,
    vec: &[f32],
) -> Result<u8, RecommenderError> {
    let mut centers = load_centers(store)?;

    let cluster_id = match centers.len() as u8 {
        0 => 0,
        n if n < N_CLUSTERS => n,
        _ => {
            let found = nearest(&centers, vec).map(|c| c.id);
            found.ok_or(RecommenderError::NotFound("no cluster centers".into()))?
        }
    };

    let is_new = !centers.iter().any(|c| c.id == cluster_id);
    if is_new {
        centers.push(ClusterCenter {
            id: cluster_id,
            mean: vec.to_vec(),
            members: 1,
        });
    } else {
        let center = centers
            .iter_mut()
            .find(|c| c.id == cluster_id)
            .expect("center exists");
        center.members += 1;
        // Incremental mean: mean += (vec - mean) / members.
        let inv = 1.0 / center.members as f32;
        for (m, &x) in center.mean.iter_mut().zip(vec.iter()) {
            *m += (x - *m) * inv;
        }
        // Renormalise so cosine comparisons stay scale-invariant.
        let n = vector_store::norm(&center.mean);
        if n > 1e-6 {
            for m in center.mean.iter_mut() {
                *m /= n;
            }
        }
    }
    save_centers(store, &centers)?;

    store
        .clusters
        .insert(format!("assign:{song_id}").as_bytes(), &[cluster_id])?;

    Ok(cluster_id)
}

/// Reads a song's cluster assignment, if it has one.
pub(crate) fn cluster_of(store: &Store, song_id: &str) -> Result<Option<u8>, RecommenderError> {
    Ok(store
        .clusters
        .get(format!("assign:{song_id}").as_bytes())?
        .and_then(|b| b.first().copied()))
}

/// Folds a listen into a cluster's affinity aggregate.
pub(crate) fn bump_affinity(
    store: &Store,
    cluster_id: u8,
    ratio: f32,
    was_skip: bool,
) -> Result<(), RecommenderError> {
    let key = format!("aff:{cluster_id}");
    let mut score: ClusterScore = store
        .clusters
        .get(key.as_bytes())?
        .and_then(|b| bincode::deserialize(&b).ok())
        .unwrap_or_default();
    score.plays += 1;
    if was_skip {
        score.skips += 1;
    }
    score.affinity_sum += listen_signal(ratio);
    let bytes = bincode::serialize(&score).map_err(|e| bin_err("cluster score", e))?;
    store.clusters.insert(key.as_bytes(), bytes)?;
    Ok(())
}

/// Same scale as the song-level listen weights so aggregates are comparable.
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

/// Cluster affinity match for scoring, [0, 1] (0.5 = neutral / unheard).
/// Mean per-listen signal spans roughly [-3, +1.5]; maps around 0.5.
pub(crate) fn affinity_match(store: &Store, cluster_id: Option<u8>) -> Result<f32, RecommenderError> {
    let Some(id) = cluster_id else { return Ok(0.5) };
    let score: Option<ClusterScore> = store
        .clusters
        .get(format!("aff:{id}").as_bytes())?
        .and_then(|b| bincode::deserialize(&b).ok());
    match score {
        Some(s) if s.plays > 0 => {
            let avg = s.affinity_sum / s.plays as f32;
            Ok((0.5 + avg / 4.0).clamp(0.0, 1.0))
        }
        _ => Ok(0.5),
    }
}

/// One-time pass over all songs: assigns every unassigned song, evolving the
/// centres as it goes.  Called from `MusicRecommender::new` so pre-v2
/// databases get full coverage without a re-scan.
pub(crate) fn migrate_all(
    store: &Store,
    songs: &[(String, Vec<f32>)],
) -> Result<usize, RecommenderError> {
    let mut assigned = 0;
    for (id, vec) in songs {
        if cluster_of(store, id)?.is_none() {
            assign(store, id, vec)?;
            assigned += 1;
        }
    }
    if assigned > 0 {
        store.clusters.flush()?;
    }
    Ok(assigned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_first(val: f32, second: f32) -> Vec<f32> {
        let mut v = vec![0.0_f32; 30];
        v[0] = val;
        v[1] = second;
        v
    }

    #[test]
    fn creates_centers_up_to_k_then_assigns_nearest() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        for i in 0..8u8 {
            let mut v = vec![0.0; 30];
            v[i as usize] = 1.0;
            let c = assign(&store, &format!("s{i}"), &v).unwrap();
            assert_eq!(c, i, "song {i} should found its own cluster");
        }
        // A vector close to cluster 0's shape joins cluster 0.
        let c = assign(&store, "near0", &vec_first(1.0, 0.05)).unwrap();
        assert_eq!(c, 0);
    }

    #[test]
    fn assignment_persists_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        assign(&store, "x", &vec_first(1.0, 0.0)).unwrap();
        assert_eq!(cluster_of(&store, "x").unwrap(), Some(0));
        assert_eq!(cluster_of(&store, "missing").unwrap(), None);
    }

    #[test]
    fn affinity_match_moves_with_skips_and_completions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        assert!((affinity_match(&store, Some(3)).unwrap() - 0.5).abs() < 1e-6);

        for _ in 0..6 {
            bump_affinity(&store, 3, 1.0, false).unwrap();
        }
        let loved = affinity_match(&store, Some(3)).unwrap();
        assert!(loved > 0.7, "completed listens should raise affinity, got {loved}");

        for _ in 0..8 {
            bump_affinity(&store, 4, 0.02, true).unwrap();
        }
        let hated = affinity_match(&store, Some(4)).unwrap();
        assert!(hated < 0.2, "skip-heavy cluster should tank affinity, got {hated}");
    }

    #[test]
    fn migrate_all_assigns_only_unassigned() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        let songs: Vec<(String, Vec<f32>)> = (0..5)
            .map(|i| (format!("m{i}"), vec_first(1.0, i as f32 * 0.1)))
            .collect();
        let first = migrate_all(&store, &songs).unwrap();
        assert_eq!(first, 5);
        let second = migrate_all(&store, &songs).unwrap();
        assert_eq!(second, 0, "second pass must be a no-op");
    }
}
