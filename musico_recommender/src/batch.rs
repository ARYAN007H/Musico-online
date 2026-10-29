//! Batch downloads (C4.2): album/playlist "Download All" runs as a
//! persistent, resumable batch with at most 2 concurrent resolves.
//!
//! Persistence: each batch lives in the `downloads` sled tree (key =
//! batch id → bincode `BatchJob`).  Pending work survives restarts and is
//! listed by [`resume_pending`] for the app to re-run at startup;
//! failures are retried per track via [`retry_failed`].  Cancel is
//! cooperative — workers finish their current track, then stop.
//!
//! The resolver is injected ([`run`]) so tests run without yt-dlp and the
//! app layer can emit progress events per settled track.

use serde::{Deserialize, Serialize};

use crate::errors::RecommenderError;
use crate::models::Store;

/// How many resolves a batch runs at once.
pub const BATCH_CONCURRENCY: usize = 2;

/// One track inside a batch: `state` ∈ "pending" | "done" | "failed:{reason}".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchTrack {
    pub video_id: String,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchJob {
    pub id: String,
    pub label: String,
    pub tracks: Vec<BatchTrack>,
    /// false once cancelled or fully settled.
    pub active: bool,
}

impl BatchJob {
    pub fn done_count(&self) -> usize {
        self.tracks.iter().filter(|t| t.state == "done").count()
    }
    pub fn failed_ids(&self) -> Vec<String> {
        self.tracks
            .iter()
            .filter(|t| t.state.starts_with("failed:"))
            .map(|t| t.video_id.clone())
            .collect()
    }
    pub fn pending_ids(&self) -> Vec<String> {
        self.tracks
            .iter()
            .filter(|t| t.state == "pending")
            .map(|t| t.video_id.clone())
            .collect()
    }
}

fn tree(store: &Store) -> Result<sled::Tree, RecommenderError> {
    store.db.open_tree("downloads").map_err(RecommenderError::DbError)
}

fn save(tree: &sled::Tree, job: &BatchJob) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(job)
        .map_err(|e| RecommenderError::Serialization(e.to_string()))?;
    tree.insert(job.id.as_bytes(), bytes).map_err(RecommenderError::DbError)?;
    Ok(())
}

fn load(tree: &sled::Tree, id: &str) -> Result<Option<BatchJob>, RecommenderError> {
    match tree.get(id.as_bytes()).map_err(RecommenderError::DbError)? {
        Some(bytes) => Ok(bincode::deserialize::<BatchJob>(&bytes).ok()),
        None => Ok(None),
    }
}

/// Creates a batch (all tracks pending) and persists it.
/// `tracks` are (video_id, title) pairs.
pub fn create(
    store: &Store,
    id: &str,
    label: &str,
    tracks: &[(String, String)],
) -> Result<BatchJob, RecommenderError> {
    if id.is_empty() {
        return Err(RecommenderError::Invalid("batch id required".into()));
    }
    let job = BatchJob {
        id: id.to_string(),
        label: label.to_string(),
        tracks: tracks
            .iter()
            .filter(|(vid, _)| !vid.is_empty())
            .map(|(vid, title)| BatchTrack {
                video_id: vid.clone(),
                title: title.clone(),
                state: "pending".into(),
            })
            .collect(),
        active: true,
    };
    save(&tree(store)?, &job)?;
    Ok(job)
}

pub fn get(store: &Store, id: &str) -> Result<Option<BatchJob>, RecommenderError> {
    load(&tree(store)?, id)
}

/// All batches, newest first (ids are `batch-{millis}`; reverse key order).
pub fn list(store: &Store) -> Result<Vec<BatchJob>, RecommenderError> {
    let tree = tree(store)?;
    let mut out = Vec::new();
    for pair in tree.iter().rev() {
        let (_, value) = pair.map_err(RecommenderError::DbError)?;
        if let Ok(job) = bincode::deserialize::<BatchJob>(&value) {
            out.push(job);
        }
    }
    Ok(out)
}

/// Marks one track done/failed and persists.  Returns the updated job.
pub fn record(
    store: &Store,
    id: &str,
    video_id: &str,
    result: Result<(), String>,
) -> Result<Option<BatchJob>, RecommenderError> {
    let tree = tree(store)?;
    let Some(mut job) = load(&tree, id)? else { return Ok(None) };
    if let Some(t) = job.tracks.iter_mut().find(|t| t.video_id == video_id) {
        t.state = match result {
            Ok(()) => "done".into(),
            Err(e) => format!("failed:{e}"),
        };
    }
    if job.pending_ids().is_empty() {
        job.active = false;
    }
    save(&tree, &job)?;
    Ok(Some(job))
}

/// Re-enqueues every failed track (retry-failed semantics).
pub fn retry_failed(
    store: &Store,
    id: &str,
) -> Result<Option<BatchJob>, RecommenderError> {
    let tree = tree(store)?;
    let Some(mut job) = load(&tree, id)? else { return Ok(None) };
    let mut changed = false;
    for t in &mut job.tracks {
        if t.state.starts_with("failed:") {
            t.state = "pending".into();
            changed = true;
        }
    }
    if changed {
        job.active = true;
        save(&tree, &job)?;
    }
    Ok(Some(job))
}

/// Cooperative cancel: no new resolves start; in-flight ones finish and are
/// still recorded.
pub fn cancel(store: &Store, id: &str) -> Result<Option<BatchJob>, RecommenderError> {
    let tree = tree(store)?;
    let Some(mut job) = load(&tree, id)? else { return Ok(None) };
    job.active = false;
    save(&tree, &job)?;
    Ok(Some(job))
}

/// Runs a batch with at most [`BATCH_CONCURRENCY`] concurrent resolvers.
/// `resolve` is injected so tests run without yt-dlp; `progress` fires
/// after every track settles.  Blocks until the batch settles or is
/// cancelled — run it on a worker thread.
pub fn run<F, G>(
    store: &Store,
    id: &str,
    resolve: &F,
    progress: &G,
) -> Result<Option<BatchJob>, RecommenderError>
where
    F: Fn(&str) -> Result<(), String> + Sync,
    G: Fn(&BatchJob) + Sync,
{
    loop {
        let Some(job) = get(store, id)? else { return Ok(None) };
        let pending = job.pending_ids();
        if !job.active || pending.is_empty() {
            return Ok(Some(job));
        }
        // Take up to BATCH_CONCURRENCY pending tracks this round.
        let wave: Vec<String> = pending.into_iter().take(BATCH_CONCURRENCY).collect();
        let mut results: Vec<(String, Result<(), String>)> = Vec::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|vid| scope.spawn(move || (vid.clone(), resolve(vid))))
                .collect();
            for h in handles {
                if let Ok(r) = h.join() {
                    results.push(r);
                }
            }
        });
        let mut latest = job;
        for (vid, r) in results {
            if let Some(updated) = record(store, id, &vid, r)? {
                latest = updated;
                progress(&latest);
            }
        }
    }
}

/// Batches that still have pending work — the app re-runs these at startup
/// so unfinished batches survive restarts.
pub fn resume_pending(store: &Store) -> Result<Vec<String>, RecommenderError> {
    Ok(list(store)?
        .into_iter()
        .filter(|j| j.active && !j.pending_ids().is_empty())
        .map(|j| j.id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().to_str().unwrap()).expect("open tmp store");
        (dir, store)
    }

    fn tracks(n: u32) -> Vec<(String, String)> {
        (1..=n).map(|i| (format!("t{i}"), format!("Track {i}"))).collect()
    }

    #[test]
    fn create_record_and_persistence() {
        let (_dir, store) = tmp_store();
        let job = create(&store, "batch-1", "Album", &tracks(3)).unwrap();
        assert_eq!(job.pending_ids().len(), 3);

        record(&store, "batch-1", "t1", Ok(())).unwrap();
        record(&store, "batch-1", "t2", Err("disk full".into())).unwrap();
        let job = get(&store, "batch-1").unwrap().unwrap();
        assert_eq!(job.done_count(), 1);
        assert_eq!(job.failed_ids(), vec!["t2".to_string()]);
        assert_eq!(job.pending_ids(), vec!["t3".to_string()]);
        assert!(job.active, "pending work keeps the batch active");
    }

    #[test]
    fn batch_settles_and_deactivates() {
        let (_dir, store) = tmp_store();
        create(&store, "batch-2", "Album", &tracks(2)).unwrap();
        record(&store, "batch-2", "t1", Ok(())).unwrap();
        let job = record(&store, "batch-2", "t2", Ok(())).unwrap().unwrap();
        assert!(!job.active, "no pending work → inactive");
        assert_eq!(job.done_count(), 2);
    }

    #[test]
    fn retry_failed_requeues_only_failures() {
        let (_dir, store) = tmp_store();
        create(&store, "batch-3", "Album", &tracks(3)).unwrap();
        record(&store, "batch-3", "t1", Ok(())).unwrap();
        record(&store, "batch-3", "t2", Err("x".into())).unwrap();
        record(&store, "batch-3", "t3", Err("y".into())).unwrap();
        let job = retry_failed(&store, "batch-3").unwrap().unwrap();
        assert_eq!(job.pending_ids(), vec!["t2".to_string(), "t3".to_string()]);
        assert_eq!(job.done_count(), 1, "done track stays done");
    }

    #[test]
    fn run_processes_everything_and_respects_concurrency_cap() {
        let (_dir, store) = tmp_store();
        create(&store, "batch-4", "Album", &tracks(6)).unwrap();
        let concurrent = AtomicUsize::new(0);
        let max_concurrent = AtomicUsize::new(0);
        let resolve = |_: &str| {
            let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        };
        let job = run(&store, "batch-4", &resolve, &|_| {}).unwrap().unwrap();
        assert_eq!(job.done_count(), 6);
        assert!(!job.active);
        assert!(
            max_concurrent.load(Ordering::SeqCst) <= BATCH_CONCURRENCY,
            "concurrency cap violated"
        );
    }

    #[test]
    fn run_records_failures_and_state_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        {
            let store = Store::open(&path).unwrap();
            create(&store, "batch-5", "Album", &tracks(2)).unwrap();
            let resolve = |vid: &str| if vid == "t2" { Err("boom".into()) } else { Ok(()) };
            let job = run(&store, "batch-5", &resolve, &|_| {}).unwrap().unwrap();
            assert_eq!(job.done_count(), 1);
        }
        // Reopen: state survived (persistence contract).
        {
            let store = Store::open(&path).unwrap();
            let job = get(&store, "batch-5").unwrap().unwrap();
            assert_eq!(job.done_count(), 1);
            assert_eq!(job.failed_ids(), vec!["t2".to_string()]);
            assert!(resume_pending(&store).unwrap().is_empty(), "settled batch doesn't resume");
        }
    }

    #[test]
    fn cancel_stops_new_work_and_keeps_pending_for_retry() {
        let (_dir, store) = tmp_store();
        create(&store, "batch-6", "Album", &tracks(4)).unwrap();
        cancel(&store, "batch-6").unwrap();
        let job = run(&store, "batch-6", &|_| Ok(()), &|_| {}).unwrap().unwrap();
        assert_eq!(job.done_count(), 0, "cancelled batch processes nothing");
        assert_eq!(job.pending_ids().len(), 4, "pending work is kept for retry");
    }

    #[test]
    fn resume_pending_flags_unfinished_batches() {
        let (_dir, store) = tmp_store();
        create(&store, "batch-7", "Album", &tracks(1)).unwrap();
        assert_eq!(resume_pending(&store).unwrap(), vec!["batch-7".to_string()]);
        record(&store, "batch-7", "t1", Ok(())).unwrap();
        assert!(resume_pending(&store).unwrap().is_empty());
    }

    #[test]
    fn rejects_empty_id_and_blank_tracks() {
        let (_dir, store) = tmp_store();
        assert!(create(&store, "", "Album", &tracks(1)).is_err());
        let job = create(&store, "batch-8", "Album", &[(String::new(), "No id".into())]).unwrap();
        assert!(job.tracks.is_empty(), "blank video ids are dropped");
    }
}
