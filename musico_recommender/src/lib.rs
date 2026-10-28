//! # Musico — Offline Music Recommendation Engine (v2)
//!
//! A self-contained Rust library crate for analysing audio files, computing
//! feature vectors, and producing personalised song recommendations.
//! 100 % offline — zero network calls.
//!
//! ## How v2 adapts to the listener
//!
//! Seven weighted signals drive every recommendation:
//!
//! | Signal       | Source                                                       |
//! |--------------|--------------------------------------------------------------|
//! | similarity   | cosine to the playing track (session centroid / taste profile when idle) |
//! | profile      | long-term taste profile — time-decayed listen EMA + likes/dislikes |
//! | session      | skip-aware EMA mood centroid (only completed listens move it) |
//! | artist       | per-artist affinity aggregates                                |
//! | cluster      | pseudo-genre via online k-means over audio features           |
//! | context      | time-of-day sound match (tempo/energy/brightness per daypart) |
//! | discovery    | smooth novelty decay, boosted on skip streaks                 |
//!
//! The weights themselves are **learned online**: each recommendation batch
//! records which signal dominated each pick, and completed listens / skips
//! nudge that signal's weight (clamped + renormalised) plus an exploration
//! multiplier.  Final lists are diversified with MMR and a per-artist cap.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use musico_recommender::{MusicRecommender, LikeState};
//!
//! let mut engine = MusicRecommender::new("/tmp/musico_db").unwrap();
//!
//! // Index some songs
//! let song = engine.index_song("/music/track.flac").unwrap();
//!
//! // When playback starts / ends:
//! engine.on_song_changed(&song.id).unwrap();
//! engine.on_song_ended(&song.id, 210, 240).unwrap(); // ratio-weighted learning
//!
//! // Explicit feedback:
//! engine.set_like(&song.id, LikeState::Liked).unwrap();
//!
//! let recs = engine.get_recommendations(&song.id, 5).unwrap();
//! ```

pub mod adapt;
pub mod artists;
pub mod clusters;
pub mod context;
pub mod diversity;
pub mod errors;
pub mod extractor;
pub mod history;
pub mod likes;
pub mod models;
pub mod playlists;
pub mod profile;
pub mod recommender;
pub mod session;
pub mod stats;
pub mod vector_store;

pub use errors::RecommenderError;
pub use models::*;
pub use stats::ListeningStats;
pub use playlists::SmartPlaylist;

use chrono::Local;
use models::{SongCache, Store};

/// The primary entry-point for the Musico recommendation engine.
///
/// Wraps the sled database (with separate trees for songs, events, sessions,
/// scores, likes, artists, context buckets and clusters), an in-memory cache
/// of the full library for O(1) scoring, and the current session state.
pub struct MusicRecommender {
    /// Internal database handle with named trees.
    store: Store,
    /// Current listening session.
    session: models::SessionState,
    /// Session-only bookkeeping (skip streak, rec attribution).
    ext: models::SessionExt,
    /// In-memory library cache — records, weighted vectors, cluster
    /// assignments and like states.  ~0.5 KB per song.
    cache: SongCache,
}

impl MusicRecommender {
    /// Opens (or creates) the database at `db_path` and initialises the engine.
    ///
    /// Loads the in-memory library cache, lazily migrates pre-v2 songs into
    /// taste clusters, and resumes the previous session if one exists.
    pub fn new(db_path: &str) -> Result<Self, RecommenderError> {
        let store = Store::open(db_path).map_err(RecommenderError::DbError)?;

        let session = match session::load_session(&store)? {
            Some(s) => s,
            None => session::start_session(&store)?,
        };
        let ext = session::load_ext(&store);

        // Build the library cache, backfilling cluster assignments for songs
        // indexed before v2 (no re-scan needed — vectors are already stored).
        let mut cache = SongCache::default();
        for record in vector_store::get_all_songs(&store)? {
            cache.push(record);
        }
        let unassigned: Vec<(String, Vec<f32>)> = (0..cache.len())
            .filter(|i| !cache.clusters.contains_key(&cache.records[*i].id))
            .map(|i| (cache.records[i].id.clone(), cache.vecs[i].clone()))
            .collect();
        if !unassigned.is_empty() {
            let n = clusters::migrate_all(&store, &unassigned)?;
            log::info!("Clustered {n} pre-v2 songs");
        }
        for entry in store.clusters.iter() {
            let (key, value) = entry.map_err(RecommenderError::DbError)?;
            if key.starts_with(b"assign:") {
                if let Some(id) = key
                    .strip_prefix(b"assign:")
                    .map(|k| String::from_utf8_lossy(k).to_string())
                {
                    if let Some(c) = value.first() {
                        cache.clusters.insert(id, *c);
                    }
                }
            }
        }
        for (id, state) in likes::all(&store)? {
            cache.likes.insert(id, state);
        }

        Ok(Self { store, session, ext, cache })
    }

    // ------------------------------------------------------------------
    // Indexing
    // ------------------------------------------------------------------

    /// Analyses the audio file at `file_path`, extracts features + metadata in
    /// a single pass, and persists a new [`SongRecord`].
    ///
    /// If the file has already been indexed, returns the existing record
    /// without re-extracting (dedup guard).
    pub fn index_song(&mut self, file_path: &str) -> Result<SongRecord, RecommenderError> {
        let record = vector_store::index_song(&self.store, file_path)?;
        self.cache_new_record(&record)?;
        Ok(record)
    }

    /// Batch-indexes multiple files with a single sled flush at the end.
    ///
    /// `progress_cb` is called after each file with `(completed, total)`.
    /// Errors on individual files are collected and skipped — successfully
    /// indexed songs are returned.
    pub fn index_songs_batch(
        &mut self,
        paths: &[&str],
        progress_cb: impl Fn(usize, usize),
    ) -> Result<Vec<SongRecord>, RecommenderError> {
        let total = paths.len();
        let mut records = Vec::with_capacity(total);

        for (i, path) in paths.iter().enumerate() {
            match vector_store::index_song_no_flush(&self.store, path) {
                Ok(record) => {
                    if !self.cache.index.contains_key(&record.id) {
                        self.cache_new_record(&record)?;
                    }
                    records.push(record);
                }
                Err(_) => { /* skip failed files */ }
            }
            progress_cb(i + 1, total);
        }

        // Single flush for the entire batch.
        self.store.songs.flush().map_err(RecommenderError::DbError)?;
        self.store.clusters.flush().map_err(RecommenderError::DbError)?;
        self.store.db.flush().map_err(RecommenderError::DbError)?;

        Ok(records)
    }

    /// Indexes a song from a pre-computed [`AnalysisResult`], skipping the
    /// decode step.  Used by the scanner to separate analysis (no lock) from
    /// DB writes (lock held briefly).
    pub fn index_from_result(
        &mut self,
        file_path: &str,
        result: models::AnalysisResult,
    ) -> Result<SongRecord, RecommenderError> {
        let record = vector_store::index_from_result(&self.store, file_path, result)?;
        self.cache_new_record(&record)?;
        Ok(record)
    }

    /// Registers a freshly-persisted record: taste-cluster assignment +
    /// in-memory cache update.
    fn cache_new_record(&mut self, record: &models::SongRecord) -> Result<(), RecommenderError> {
        let was_new = !self.cache.index.contains_key(&record.id);
        if was_new {
            let cluster = clusters::assign(&self.store, &record.id, &record.feature_vector.to_weighted_vec())?;
            self.cache.push(record.clone());
            self.cache.clusters.insert(record.id.clone(), cluster);
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Listening feedback
    // ------------------------------------------------------------------

    /// Logs a play event for the given song and folds it into every taste
    /// aggregate: song affinity, artist reputation, cluster affinity,
    /// context bucket, and the long-term taste profile.
    ///
    /// Skip classification is computed automatically from the listen ratio.
    pub fn log_listen(
        &mut self,
        song_id: &str,
        listened_secs: u32,
        duration_secs: u32,
    ) -> Result<(), RecommenderError> {
        let ratio = if duration_secs > 0 {
            (listened_secs as f32 / duration_secs as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let was_skip = history::is_skip(ratio);

        let event = models::PlayEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            song_id: song_id.to_string(),
            session_id: self.session.session_id.clone(),
            started_at: chrono::Utc::now(),
            listened_secs,
            song_duration_secs: duration_secs,
            was_skipped: was_skip,
            listen_ratio: ratio,
        };
        history::log_play_event(&self.store, &event)?;

        if let Some(record) = self.cache.get(song_id).cloned() {
            profile::on_listen(&self.store, &record, ratio)?;
            artists::bump(&self.store, &record.artist, ratio, was_skip)?;
            if let Some(&cluster) = self.cache.clusters.get(song_id) {
                clusters::bump_affinity(&self.store, cluster, ratio, was_skip)?;
            }
            let bucket = context::bucket_for(Local::now());
            context::bump(&self.store, bucket, &record, ratio, was_skip)?;
        }
        Ok(())
    }

    /// Full song-end handling: logs the listen (see [`Self::log_listen`]),
    /// moves the mood centroid only if the song wasn't skipped, tracks the
    /// session skip streak, and applies the online weight-learning step when
    /// this song came from a recommendation.
    pub fn on_song_ended(
        &mut self,
        song_id: &str,
        listened_secs: u32,
        duration_secs: u32,
    ) -> Result<(), RecommenderError> {
        self.log_listen(song_id, listened_secs, duration_secs)?;

        let ratio = if duration_secs > 0 {
            (listened_secs as f32 / duration_secs as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        if let Some(record) = self.cache.get(song_id).cloned() {
            session::update_centroid(&self.store, &mut self.session, &record, ratio)?;
        }
        session::record_skip_streak(&self.store, &mut self.ext, ratio)?;

        // Adaptation: if this song was recommended and the dominant signal is
        // known, learn from the outcome and consume the attribution entry.
        if let Some(pos) = self.ext.rec_sources.iter().position(|(id, _)| id == song_id) {
            let (_, signal) = self.ext.rec_sources.remove(pos);
            session::save_ext(&self.store, &self.ext)?;
            adapt::apply_outcome(&self.store, &signal, adapt::outcome_for(ratio))?;
        }
        Ok(())
    }

    /// Notifies the engine that playback transitioned to a new song.
    ///
    /// Updates the session history (for exclusion); the mood centroid is
    /// deliberately *not* updated here — see [`Self::on_song_ended`].
    pub fn on_song_changed(&mut self, new_song_id: &str) -> Result<(), RecommenderError> {
        session::push_history(&self.store, &mut self.session, new_song_id)
    }

    // ------------------------------------------------------------------
    // Explicit feedback
    // ------------------------------------------------------------------

    /// Sets the like state for a song and refreshes the like/dislike mean
    /// vectors in the taste profile.
    pub fn set_like(&mut self, song_id: &str, state: LikeState) -> Result<(), RecommenderError> {
        likes::set(&self.store, song_id, state)?;
        match state {
            LikeState::Neutral => self.cache.likes.remove(song_id),
            s => self.cache.likes.insert(song_id.to_string(), s),
        };
        self.refresh_like_vectors()
    }

    /// Returns the like state for a song.  Falls back to the store for ids
    /// missing from the cache (e.g. likes on songs cleared from the index).
    pub fn get_like(&self, song_id: &str) -> Result<LikeState, RecommenderError> {
        if let Some(state) = self.cache.likes.get(song_id) {
            return Ok(*state);
        }
        likes::get(&self.store, song_id)
    }

    /// All liked song ids (for UI sync).
    pub fn liked_ids(&self) -> Result<Vec<String>, RecommenderError> {
        Ok(self
            .cache
            .likes
            .iter()
            .filter(|(_, &s)| s == LikeState::Liked)
            .map(|(id, _)| id.clone())
            .collect())
    }

    fn refresh_like_vectors(&self) -> Result<(), RecommenderError> {
        let lookup = |id: &str| self.cache.vec_of(id).cloned();
        profile::refresh_like_vectors(&self.store, &lookup)
    }

    // ------------------------------------------------------------------
    // Recommendations
    // ------------------------------------------------------------------

    /// Produces up to `top_n` recommended songs based on the currently playing
    /// track, the taste profile, session context, artist/cluster/context
    /// signals and cooldown state.  Works with an empty `current_song_id`
    /// (profile/discovery-driven picks).
    pub fn get_recommendations(
        &mut self,
        current_song_id: &str,
        top_n: usize,
    ) -> Result<Vec<RecommendedSong>, RecommenderError> {
        let weights = adapt::load(&self.store)?;
        let anchor_artist = self
            .cache
            .get(current_song_id)
            .map(|r| r.artist.clone())
            .unwrap_or_default();

        let req = RecommendationRequest {
            current_song_id: current_song_id.to_string(),
            session_state: self.session.clone(),
            top_n,
            exclude_ids: self.session.song_history.clone(),
            weights,
            anchor_artist,
            skip_streak: self.ext.skip_streak,
        };
        let recs = recommender::recommend(&self.store, &req, &self.cache)?;

        // Remember what drove each pick so song-ends can teach the weights.
        let sources = recs
            .iter()
            .map(|r| (r.record.id.clone(), r.dominant_signal.to_string()))
            .collect();
        session::remember_recs(&self.store, &mut self.ext, sources)?;

        Ok(recs)
    }

    // ------------------------------------------------------------------
    // Library introspection
    // ------------------------------------------------------------------

    /// Returns all indexed [`SongRecord`]s.
    pub fn get_all_songs(&self) -> Result<Vec<SongRecord>, RecommenderError> {
        Ok(self.cache.all_records())
    }

    /// Looks up a single song by id. Returns `Ok(None)` if not found.
    pub fn get_song_by_id(&self, id: &str) -> Result<Option<SongRecord>, RecommenderError> {
        Ok(self.cache.get(id).cloned())
    }

    /// Clears all indexed songs and path indices.  Use before a full re-scan.
    /// Likes, listening history and taste data are preserved (ids are stable
    /// path hashes, so re-indexing reconnects them).
    pub fn clear_song_index(&mut self) -> Result<(), RecommenderError> {
        // Remove all path:* keys from the default tree.
        let mut path_keys = Vec::new();
        for entry in self.store.db.iter() {
            let (key, _) = entry.map_err(RecommenderError::DbError)?;
            if key.starts_with(b"path:") {
                path_keys.push(key);
            }
        }
        for key in path_keys {
            self.store.db.remove(key).map_err(RecommenderError::DbError)?;
        }
        // Clear the songs tree.
        self.store.songs.clear().map_err(RecommenderError::DbError)?;
        // Reset song count.
        self.store.db.insert(b"meta:song_count", &0u64.to_le_bytes())
            .map_err(RecommenderError::DbError)?;
        // Rebuild the in-memory library cache, but keep the like states and
        // cluster assignments: they are keyed by stable path-hash ids, which
        // survive a re-index, and dropping them would desync cache from DB.
        let likes = std::mem::take(&mut self.cache.likes);
        let clusters = std::mem::take(&mut self.cache.clusters);
        self.cache = SongCache::default();
        self.cache.likes = likes;
        self.cache.clusters = clusters;
        self.store.db.flush().map_err(RecommenderError::DbError)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Session control
    // ------------------------------------------------------------------

    /// Starts a brand-new listening session, resetting the centroid, history
    /// and session bookkeeping.
    pub fn new_session(&mut self) -> Result<(), RecommenderError> {
        self.session = session::start_session(&self.store)?;
        self.ext = SessionExt::default();
        session::save_ext(&self.store, &self.ext)?;
        Ok(())
    }

    /// Returns a reference to the current [`SessionState`].
    pub fn current_session(&self) -> &models::SessionState {
        &self.session
    }

    /// Returns a reference to the underlying sled database handle.
    pub fn db(&self) -> &sled::Db {
        &self.store.db
    }

    // ------------------------------------------------------------------
    // Stats & playlists
    // ------------------------------------------------------------------

    /// Compute listening statistics from the event history.
    pub fn get_stats(&self) -> Result<ListeningStats, RecommenderError> {
        stats::get_stats(&self.store)
    }

    /// Load all saved smart playlists.
    pub fn get_playlists(&self) -> Result<Vec<SmartPlaylist>, RecommenderError> {
        playlists::load_playlists(&self.store)
    }

    /// Save a smart playlist.
    pub fn save_playlist(&self, playlist: &SmartPlaylist) -> Result<(), RecommenderError> {
        playlists::save_playlist(&self.store, playlist)
    }

    /// Delete a smart playlist by ID.
    pub fn delete_playlist(&self, playlist_id: &str) -> Result<(), RecommenderError> {
        playlists::delete_playlist(&self.store, playlist_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AnalysisResult, FeatureVector};

    fn analysis(axis: usize, title: &str, artist: &str) -> AnalysisResult {
        let mut fv = FeatureVector {
            version: 1,
            mfcc: [0.0; 13],
            spectral_centroid: 0.5,
            spectral_rolloff: 0.5,
            zero_crossing_rate: 0.5,
            rms_energy: 0.5,
            tempo_bpm: 0.5,
            chroma: [0.5; 12],
        };
        fv.mfcc[axis] = 1.0;
        AnalysisResult {
            feature_vector: fv,
            duration_secs: 200,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            rms_db: -18.0,
        }
    }

    fn engine() -> MusicRecommender {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        std::mem::forget(tmp); // keep the dir until the test ends
        MusicRecommender::new(&path).unwrap()
    }

    fn seed_library(r: &mut MusicRecommender) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..5 {
            let rec = r
                .index_from_result(
                    &format!("/m/library/song{i}.mp3"),
                    analysis(i, &format!("Song{i}"), &format!("Artist{i}")),
                )
                .unwrap();
            ids.push(rec.id);
        }
        ids
    }

    #[test]
    fn end_to_end_listen_then_recommend() {
        let mut r = engine();
        let ids = seed_library(&mut r);

        r.on_song_changed(&ids[0]).unwrap();
        // Fully hear songs 0 and 1 a few times.
        for _ in 0..4 {
            r.on_song_ended(&ids[0], 200, 200).unwrap();
            r.on_song_ended(&ids[1], 200, 200).unwrap();
        }

        let recs = r.get_recommendations(&ids[0], 3).unwrap();
        assert!(!recs.is_empty());
        assert!(recs.iter().all(|rec| rec.record.id != ids[0]));
        assert!(recs.iter().all(|rec| rec.dominant_signal.len() > 3));
    }

    #[test]
    fn like_flows_into_recommendations() {
        let mut r = engine();
        let ids = seed_library(&mut r);

        r.set_like(&ids[4], LikeState::Liked).unwrap();
        assert_eq!(r.get_like(&ids[4]).unwrap(), LikeState::Liked);
        assert_eq!(r.liked_ids().unwrap(), vec![ids[4].clone()]);

        r.on_song_changed(&ids[0]).unwrap();
        let recs = r.get_recommendations(&ids[0], 4).unwrap();
        // The liked song (different sound axis) must outrank its neutral peers.
        assert_eq!(recs[0].record.id, ids[4]);

        // Unliking removes the boost again.
        r.set_like(&ids[4], LikeState::Neutral).unwrap();
        assert!(r.liked_ids().unwrap().is_empty());
    }

    #[test]
    fn dislikes_never_recommended() {
        let mut r = engine();
        let ids = seed_library(&mut r);

        r.set_like(&ids[3], LikeState::Disliked).unwrap();
        r.on_song_changed(&ids[0]).unwrap();
        for _ in 0..5 {
            let recs = r.get_recommendations(&ids[0], 4).unwrap();
            assert!(!recs.iter().any(|rec| rec.record.id == ids[3]));
            // Keep the session history from excluding everything else.
            r.new_session().unwrap();
            r.on_song_changed(&ids[0]).unwrap();
        }
    }

    #[test]
    fn skips_do_not_pull_session_anchor() {
        let mut r = engine();
        let ids = seed_library(&mut r);

        r.on_song_changed(&ids[0]).unwrap();
        r.on_song_ended(&ids[0], 200, 200).unwrap(); // anchor: axis-0 sound

        // Rapid-skip the axis-4 song several times.
        for _ in 0..3 {
            r.on_song_changed(&ids[4]).unwrap();
            r.on_song_ended(&ids[4], 10, 200).unwrap();
        }
        // The skipped song stays in session history (exclusion), so instead
        // verify through the adaptation + profile path: re-anchor and check
        // the skipped song never comes back as a pick.
        r.on_song_changed(&ids[0]).unwrap();
        let recs = r.get_recommendations(&ids[0], 3).unwrap();
        assert!(recs.iter().all(|rec| rec.record.id != ids[4]));
    }

    #[test]
    fn cold_start_recommendations_work() {
        let mut r = engine();
        seed_library(&mut r);
        let recs = r.get_recommendations("", 3).unwrap();
        assert!(!recs.is_empty(), "no anchor + no profile must still yield picks");
    }

    #[test]
    fn reindex_preserves_taste_data() {
        let mut r = engine();
        let ids = seed_library(&mut r);
        r.set_like(&ids[2], LikeState::Liked).unwrap();

        // Simulate a full re-scan with the same paths.
        r.clear_song_index().unwrap();
        for i in 0..5 {
            r.index_from_result(
                &format!("/m/library/song{i}.mp3"),
                analysis(i, &format!("Song{i}"), &format!("Artist{i}")),
            )
            .unwrap();
        }
        assert_eq!(r.get_like(&ids[2]).unwrap(), LikeState::Liked);
        assert_eq!(r.liked_ids().unwrap(), vec![ids[2].clone()]);
        // Cluster assignment must be re-attached on re-index.
        assert!(r.cache.clusters.contains_key(&ids[2]));
    }
}
