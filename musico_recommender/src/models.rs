//! Shared data structures used across the recommendation engine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Internal database handle
// ---------------------------------------------------------------------------

/// Wraps the sled database and its named trees for data isolation.
/// Each data domain gets its own B-tree so scans never cross domains.
///
/// Engine v2 adds the `likes`, `artists`, `context`, and `clusters` trees.
/// `open_tree` creates missing trees, so pre-v2 databases gain them on open
/// without any migration.  Existing persisted struct layouts (`SongRecord`,
/// `SessionState`, `PlayEvent`) are frozen — bincode has no serde defaults,
/// so a layout change would make every record fail to deserialize.
#[derive(Clone)]
pub(crate) struct Store {
    /// Raw sled handle (default tree holds metadata counters).
    pub db: sled::Db,
    /// `{song_id}` → bincode `SongRecord`
    pub songs: sled::Tree,
    /// `{song_id}:{event_id}` → bincode `PlayEvent`
    pub events: sled::Tree,
    /// `current` → bincode `SessionState`, `ext` → bincode `SessionExt`
    pub sessions: sled::Tree,
    /// `{song_id}` → bincode `SongScoreCache`
    pub scores: sled::Tree,
    /// `{song_id}` → `1` (liked) or `2` (disliked)
    pub likes: sled::Tree,
    /// `a:{artist}` → bincode `ArtistScoreCache`
    pub artists: sled::Tree,
    /// `b:{bucket}` → bincode `ContextBucket`, `b:list` → bucket id list
    pub context: sled::Tree,
    /// `centers` → bincode cluster centers, `assign:{song_id}` → u8,
    /// `aff:{cluster}` → bincode `ClusterScore`
    pub clusters: sled::Tree,
}

impl Store {
    pub fn open(path: &str) -> Result<Self, sled::Error> {
        let db = sled::open(path)?;
        let songs = db.open_tree("songs")?;
        let events = db.open_tree("events")?;
        let sessions = db.open_tree("sessions")?;
        let scores = db.open_tree("scores")?;
        let likes = db.open_tree("likes")?;
        let artists = db.open_tree("artists")?;
        let context = db.open_tree("context")?;
        let clusters = db.open_tree("clusters")?;
        Ok(Self { db, songs, events, sessions, scores, likes, artists, context, clusters })
    }

    /// O(1) song count via a counter in the default tree.
    pub fn song_count(&self) -> Result<usize, sled::Error> {
        match self.db.get(b"meta:song_count")? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.as_ref().try_into().unwrap_or([0u8; 8]);
                Ok(u64::from_le_bytes(arr) as usize)
            }
            None => Ok(0),
        }
    }

    /// Atomically increment the song counter.
    pub fn increment_song_count(&self) -> Result<(), sled::Error> {
        let count = (self.song_count()? + 1) as u64;
        self.db.insert(b"meta:song_count", &count.to_le_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Audio feature vector
// ---------------------------------------------------------------------------

/// A normalised (all values in `[0.0, 1.0]`) feature vector extracted from an
/// audio file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeatureVector {
    /// Schema version — bump when extraction logic changes.
    pub version: u8,
    /// Mel-Frequency Cepstral Coefficients 1-13.
    pub mfcc: [f32; 13],
    /// Weighted mean of frequencies — brightness indicator.
    pub spectral_centroid: f32,
    /// Frequency below which 85 % of spectral energy is concentrated.
    pub spectral_rolloff: f32,
    /// Rate of sign-changes — noisiness indicator.
    pub zero_crossing_rate: f32,
    /// Root-mean-square energy — perceived loudness.
    pub rms_energy: f32,
    /// Estimated tempo in BPM (autocorrelation method).
    pub tempo_bpm: f32,
    /// 12-bin chroma vector — harmonic / key profile.
    pub chroma: [f32; 12],
}

impl FeatureVector {
    /// Current extraction schema version.
    pub const CURRENT_VERSION: u8 = 1;

    /// Flatten with dimensional weighting: MFCC×2, chroma×1.5, scalars×1.
    pub fn to_weighted_vec(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(30);
        for &c in &self.mfcc { v.push(c * 2.0); }
        for &c in &self.chroma { v.push(c * 1.5); }
        v.push(self.spectral_centroid);
        v.push(self.spectral_rolloff);
        v.push(self.zero_crossing_rate);
        v.push(self.rms_energy);
        v.push(self.tempo_bpm);
        v
    }

    /// Flatten without weighting (raw values).
    pub fn to_flat_vec(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(30);
        for &c in &self.mfcc { v.push(c); }
        for &c in &self.chroma { v.push(c); }
        v.push(self.spectral_centroid);
        v.push(self.spectral_rolloff);
        v.push(self.zero_crossing_rate);
        v.push(self.rms_energy);
        v.push(self.tempo_bpm);
        v
    }
}

// ---------------------------------------------------------------------------
// Song record
// ---------------------------------------------------------------------------

/// A fully indexed song record. Stored in the `songs` tree keyed by UUID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongRecord {
    pub id: String,
    pub file_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u32,
    pub feature_vector: FeatureVector,
    pub indexed_at: DateTime<Utc>,
    /// Track loudness in dB RMS, used for ReplayGain normalization.
    /// Target: -18.0 dBFS. Gain = 10^((target - track_db) / 20).
    #[serde(default)]
    pub replay_gain_db: f32,
}

// ---------------------------------------------------------------------------
// Play event
// ---------------------------------------------------------------------------

/// A single play-event capturing how much of a track the user listened to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayEvent {
    pub event_id: String,
    pub song_id: String,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub listened_secs: u32,
    pub song_duration_secs: u32,
    pub was_skipped: bool,
    pub listen_ratio: f32,
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Tracks the current listening session with an EMA centroid for mood-lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub song_history: Vec<String>,
    pub centroid: Option<Vec<f32>>,
}

// ---------------------------------------------------------------------------
// Recommendation types
// ---------------------------------------------------------------------------

/// Parameters for a recommendation request.
#[derive(Debug, Clone)]
pub struct RecommendationRequest {
    pub current_song_id: String,
    pub session_state: SessionState,
    pub top_n: usize,
    pub exclude_ids: Vec<String>,
    /// Adaptive signal weights (loaded from the DB by the caller).
    pub weights: SignalWeights,
    /// Artist of the currently playing song, for same-artist fatigue.
    pub anchor_artist: String,
    /// Consecutive skips in this session — ≥2 temporarily boosts exploration.
    pub skip_streak: u32,
}

/// In-memory index of the whole library: records + pre-weighted vectors + a
/// string→position map, plus cluster assignments and like states.  Lets a
/// recommendation pass score every song with zero sled reads.
///
/// ~0.5 KB per song — 10 000 songs ≈ 5 MB, well inside the app's RAM budget.
#[derive(Default)]
pub(crate) struct SongCache {
    pub records: Vec<SongRecord>,
    /// Weighted feature vectors, parallel to `records`.
    pub vecs: Vec<Vec<f32>>,
    pub index: std::collections::HashMap<String, usize>,
    pub clusters: std::collections::HashMap<String, u8>,
    pub likes: std::collections::HashMap<String, LikeState>,
}

impl SongCache {
    /// Adds a record; no-op when the id is already cached.
    pub fn push(&mut self, record: SongRecord) {
        if self.index.contains_key(&record.id) {
            return;
        }
        let vec = record.feature_vector.to_weighted_vec();
        self.index.insert(record.id.clone(), self.records.len());
        self.vecs.push(vec);
        self.records.push(record);
    }

    pub fn get(&self, id: &str) -> Option<&SongRecord> {
        self.index.get(id).map(|&i| &self.records[i])
    }

    pub fn vec_of(&self, id: &str) -> Option<&Vec<f32>> {
        self.index.get(id).map(|&i| &self.vecs[i])
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn all_records(&self) -> Vec<SongRecord> {
        self.records.clone()
    }
}

/// A scored recommendation candidate.
#[derive(Debug, Clone)]
pub struct RecommendedSong {
    pub record: SongRecord,
    pub final_score: f32,
    pub similarity_score: f32,
    pub session_match: f32,
    pub affinity: f32,
    pub cooldown: f32,
    /// Cosine match against the long-term taste profile, [0, 1].
    pub profile_match: f32,
    /// Normalised per-artist affinity, [0, 1] (0.5 = neutral / unknown).
    pub artist_match: f32,
    /// Normalised pseudo-genre cluster affinity, [0, 1] (0.5 = neutral).
    pub cluster_match: f32,
    /// Time-of-day context match, [0, 1] (0.5 = neutral).
    pub context_match: f32,
    /// Signal that contributed most to this candidate's score.
    pub dominant_signal: &'static str,
}

// ---------------------------------------------------------------------------
// Score cache (internal)
// ---------------------------------------------------------------------------

/// Pre-computed scores for a song, stored in the `scores` tree.
/// Eliminates the need to scan events on every recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SongScoreCache {
    /// Aggregate affinity clamped to [-5, +5].
    pub affinity: f32,
    /// Total number of play events.
    pub total_plays: u32,
    /// Unix timestamps (secs) of all plays, for cooldown computation.
    pub play_timestamps: Vec<i64>,
}

impl Default for SongScoreCache {
    fn default() -> Self {
        Self { affinity: 0.0, total_plays: 0, play_timestamps: Vec::new() }
    }
}

// ---------------------------------------------------------------------------
// Analysis result (internal, combined decode + metadata)
// ---------------------------------------------------------------------------

/// Result of a single-pass audio analysis combining feature extraction and
/// metadata reading.
#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub feature_vector: FeatureVector,
    pub duration_secs: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Track loudness in dB RMS for normalization.
    pub rms_db: f32,
}

// ---------------------------------------------------------------------------
// Engine v2 state — all in dedicated trees/keys, never inside the frozen
// SongRecord / SessionState bincode layouts (see Store docs).
// ---------------------------------------------------------------------------

/// Per-artist aggregate scores, stored in the `artists` tree.
/// Key: `a:{artist}` (empty artist is never indexed).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtistScoreCache {
    /// Sum of listen-ratio weights, clamped to [-5, +5] on read.
    pub affinity: f32,
    pub plays: u32,
    pub skips: u32,
    pub last_played_ts: i64,
}

/// Listening-context bucket keyed by `hour/4 × weekday|weekend` (12 buckets).
/// Tracks what kind of music (tempo / energy / brightness) the user plays in
/// each slot of their day, plus per-bucket affinity.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextBucket {
    pub plays: u32,
    pub affinity_sum: f32,
    pub mean_tempo: f32,
    pub mean_energy: f32,
    pub mean_centroid: f32,
}

/// Pseudo-genre cluster aggregate, stored in the `clusters` tree (`aff:{id}`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClusterScore {
    pub plays: u32,
    pub skips: u32,
    pub affinity_sum: f32,
}

/// A cluster centre: mean weighted feature vector + member count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterCenter {
    pub id: u8,
    pub mean: Vec<f32>,
    pub members: u32,
}

/// Explicit like state for a song, stored in the `likes` tree.
/// Discriminants match the on-disk byte encoding (`1` = liked, `2` = disliked,
/// anything else / absent = neutral).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LikeState {
    Disliked = 2,
    Neutral = 0,
    Liked = 1,
}

impl LikeState {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => LikeState::Liked,
            2 => LikeState::Disliked,
            _ => LikeState::Neutral,
        }
    }

    pub fn to_byte(self) -> u8 {
        self as u8
    }
}

/// Adaptive scoring weights, persisted under the default-tree key
/// `adapt:weights`.  Updated online by [`crate::adapt`] from recommendation
/// outcomes; always renormalised to sum 1.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalWeights {
    pub similarity: f32,
    pub affinity: f32,
    pub profile: f32,
    pub session: f32,
    pub artist: f32,
    pub cluster: f32,
    pub context: f32,
    pub discovery: f32,
    /// Multiplier on the discovery weight — grows when recommendations get
    /// skipped (widen the net), shrinks when they land (tighten it).
    pub exploration: f32,
}

impl Default for SignalWeights {
    fn default() -> Self {
        Self {
            similarity: 0.22,
            affinity: 0.16,
            profile: 0.14,
            session: 0.12,
            artist: 0.10,
            cluster: 0.08,
            context: 0.08,
            discovery: 0.10,
            exploration: 1.0,
        }
    }
}

impl SignalWeights {
    /// The seven signal weights as named slots (excludes the exploration
    /// multiplier).  Single source of truth for scoring + adaptation.
    pub fn slots(&self) -> [(&'static str, f32); 8] {
        [
            ("similarity", self.similarity),
            ("affinity", self.affinity),
            ("profile", self.profile),
            ("session", self.session),
            ("artist", self.artist),
            ("cluster", self.cluster),
            ("context", self.context),
            ("discovery", self.discovery),
        ]
    }

    /// Renormalises the signal weights to sum to 1.0 (exploration is a
    /// multiplier, not a weight).
    pub fn normalize(&mut self) {
        let sum: f32 = self.slots().iter().map(|(_, w)| w).sum();
        if sum > 1e-6 {
            let inv = 1.0 / sum;
            self.similarity *= inv;
            self.affinity *= inv;
            self.profile *= inv;
            self.session *= inv;
            self.artist *= inv;
            self.cluster *= inv;
            self.context *= inv;
            self.discovery *= inv;
        }
    }
}

/// Session-only bookkeeping kept under the sessions-tree key `ext`, separate
/// from the frozen `SessionState` layout: consecutive-skip streak and which
/// signal drove each recent recommendation (for the adaptation feedback loop).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionExt {
    pub skip_streak: u32,
    /// `(song_id, dominant_signal)` for the most recent recommendation batch.
    pub rec_sources: Vec<(String, String)>,
}

/// Maps a bincode failure to a `sled::Error` so every tree can use `?` with
/// the same `RecommenderError::DbError` wrapping.
pub(crate) fn bin_err(what: &str, e: bincode::Error) -> sled::Error {
    sled::Error::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("{what} serialize: {e}"),
    ))
}
