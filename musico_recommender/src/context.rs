//! Listening context — what time of day / which kind of day the user is in.
//!
//! Twelve buckets: 6 four-hour dayparts × weekday/weekend.  Each bucket
//! tracks the mean tempo / energy / brightness of music played there plus a
//! per-bucket affinity aggregate, so "calm mornings, loud evenings" becomes a
//! first-class recommendation signal.
//!
//! Layout in the `context` tree:
//! - `b:{bucket_id}` → bincode `ContextBucket`
//! - `b:known`       → bincode `Vec<u8>` of buckets with data (fast init)

use chrono::{Datelike, DateTime, Local, Timelike};

use crate::errors::RecommenderError;
use crate::models::{bin_err, ContextBucket, SongRecord, Store};

/// Six four-hour dayparts × weekday|weekend.
pub const N_BUCKETS: usize = 12;

/// Buckets with at least this many plays contribute a context signal;
/// below that the signal stays neutral (0.5).
const MIN_PLAYS: u32 = 3;

/// Computes the bucket id for a local timestamp:
/// `daypart*2 + is_weekend`, dayparts = hour/4 ∈ 0..6.
pub fn bucket_for(dt: DateTime<Local>) -> u8 {
    let hour = dt.hour() as usize / 4; // 0..=5
    let weekend = matches!(dt.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
    (hour * 2 + weekend as usize) as u8
}

fn key(bucket: u8) -> String {
    format!("b:{bucket}")
}

fn load_bucket(store: &Store, bucket: u8) -> Result<ContextBucket, RecommenderError> {
    Ok(store
        .context
        .get(key(bucket).as_bytes())?
        .and_then(|b| bincode::deserialize::<ContextBucket>(&b).ok())
        .unwrap_or_default())
}

fn save_bucket(store: &Store, bucket: u8, b: &ContextBucket) -> Result<(), RecommenderError> {
    let bytes = bincode::serialize(b).map_err(|e| bin_err("context bucket", e))?;
    store.context.insert(key(bucket).as_bytes(), bytes)?;
    Ok(())
}

/// Folds a listen into its bucket: affinity aggregate + incremental means of
/// tempo / RMS energy / spectral centroid.
pub(crate) fn bump(
    store: &Store,
    bucket: u8,
    record: &SongRecord,
    ratio: f32,
    was_skip: bool,
) -> Result<(), RecommenderError> {
    let mut b = load_bucket(store, bucket)?;
    let n = b.plays as f32;
    let fv = &record.feature_vector;

    let update = |mean: f32, value: f32| (mean * n + value) / (n + 1.0);
    b.mean_tempo = update(b.mean_tempo, fv.tempo_bpm);
    b.mean_energy = update(b.mean_energy, fv.rms_energy);
    b.mean_centroid = update(b.mean_centroid, fv.spectral_centroid);
    b.plays += 1;
    if was_skip {
        b.affinity_sum -= 1.0;
    } else if ratio >= 0.8 {
        b.affinity_sum += 1.0;
    }

    save_bucket(store, bucket, &b)?;
    Ok(())
}

/// Per-bucket affinity, [0, 1] (0.5 = neutral).
fn bucket_affinity(b: &ContextBucket) -> f32 {
    if b.plays == 0 {
        return 0.5;
    }
    let avg = b.affinity_sum / b.plays as f32;
    (0.5 + avg / 4.0).clamp(0.0, 1.0)
}

/// Context match for a candidate song in a bucket, [0, 1] (0.5 = neutral).
///
/// Blends the bucket's affinity with a feature match: songs whose
/// tempo/energy/brightness resemble what the user usually plays at this hour
/// score higher.
pub(crate) fn match_score(
    store: &Store,
    bucket: u8,
    record: &SongRecord,
) -> Result<f32, RecommenderError> {
    let b = load_bucket(store, bucket)?;
    if b.plays < MIN_PLAYS {
        return Ok(0.5);
    }

    let fv = &record.feature_vector;
    let tempo_match = (-(fv.tempo_bpm - b.mean_tempo).abs() / 0.22)
        .exp()
        .clamp(0.0, 1.0);
    let energy_match = 1.0 - ((fv.rms_energy - b.mean_energy).abs() / 0.5).min(1.0);
    let bright_match = 1.0 - ((fv.spectral_centroid - b.mean_centroid).abs() / 0.5).min(1.0);
    let feature_match = 0.45 * tempo_match + 0.35 * energy_match + 0.2 * bright_match;

    Ok((0.55 * bucket_affinity(&b) + 0.45 * feature_match).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FeatureVector;
    use chrono::TimeZone;

    fn record(fv: FeatureVector) -> SongRecord {
        SongRecord {
            id: "t".into(),
            file_path: "/t".into(),
            title: "T".into(),
            artist: String::new(),
            album: String::new(),
            duration_secs: 100,
            feature_vector: fv,
            indexed_at: chrono::Utc::now(),
            replay_gain_db: 0.0,
        }
    }

    fn fv(tempo: f32, energy: f32) -> FeatureVector {
        FeatureVector {
            version: 1,
            mfcc: [0.0; 13],
            spectral_centroid: 0.5,
            spectral_rolloff: 0.5,
            zero_crossing_rate: 0.5,
            rms_energy: energy,
            tempo_bpm: tempo,
            chroma: [0.0; 12],
        }
    }

    #[test]
    fn buckets_split_dayparts_and_weekends() {
        // Wednesday 02:00 → daypart 0, weekday → 0.
        let wed_night = Local.with_ymd_and_hms(2026, 10, 28, 2, 0, 0).unwrap();
        assert_eq!(bucket_for(wed_night), 0);
        // Wednesday 09:00 → daypart 2, weekday → 4.
        let wed_morning = Local.with_ymd_and_hms(2026, 10, 28, 9, 0, 0).unwrap();
        assert_eq!(bucket_for(wed_morning), 4);
        // Saturday 21:00 → daypart 5, weekend → 11.
        let sat_evening = Local.with_ymd_and_hms(2026, 10, 31, 21, 0, 0).unwrap();
        assert_eq!(bucket_for(sat_evening), 11);
    }

    #[test]
    fn sparse_bucket_is_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();
        let m = match_score(&store, 4, &record(fv(0.5, 0.5))).unwrap();
        assert!((m - 0.5).abs() < 1e-6, "empty bucket must be neutral, got {m}");
    }

    #[test]
    fn learns_daypart_sound() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_str().unwrap()).unwrap();

        // Mornings in this bucket: slow, quiet music, well received.
        let calm = fv(0.2, 0.2);
        for _ in 0..5 {
            bump(&store, 4, &record(calm.clone()), 1.0, false).unwrap();
        }

        let similar = match_score(&store, 4, &record(fv(0.22, 0.25))).unwrap();
        let opposite = match_score(&store, 4, &record(fv(0.95, 0.95))).unwrap();
        assert!(similar > 0.65, "similar-sounding song should match, got {similar}");
        assert!(opposite < similar, "energetic song should match worse");
    }
}
