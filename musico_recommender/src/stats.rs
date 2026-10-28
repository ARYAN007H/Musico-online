//! Listening statistics — aggregate queries over PlayEvent history.
//!
//! All data comes from the existing `events` sled tree — zero new data collection.

use std::collections::HashMap;
use chrono::{DateTime, Utc};

use crate::errors::RecommenderError;
use crate::models::{PlayEvent, Store};

/// Aggregated listening statistics.
#[derive(Debug, Clone, Default)]
pub struct ListeningStats {
    /// Total songs played (including repeats).
    pub total_plays: u32,
    /// Total seconds of actual listening.
    pub total_listened_secs: u64,
    /// Total songs skipped (listen_ratio < 0.3).
    pub total_skips: u32,
    /// Top songs by play count: (song_id, title_hint, play_count).
    pub top_songs: Vec<(String, u32)>,
    /// Top artists by play count: (artist, play_count).
    pub top_artists: Vec<(String, u32)>,
    /// Daily listen counts for heatmap: (date_string "YYYY-MM-DD", minutes_listened).
    pub daily_minutes: Vec<(String, u32)>,
    /// Current listening streak (consecutive days with >= 1 play).
    pub streak_days: u32,
    /// Average listen ratio across all plays.
    pub avg_listen_ratio: f32,
}

/// Load all play events from the events tree.
fn load_all_events(store: &Store) -> Result<Vec<PlayEvent>, RecommenderError> {
    let mut events = Vec::new();
    for entry in store.events.iter() {
        let (_, value) = entry.map_err(RecommenderError::DbError)?;
        if let Ok(ev) = bincode::deserialize::<PlayEvent>(&value) {
            events.push(ev);
        }
    }
    Ok(events)
}

/// Compute full listening statistics from the event history.
pub(crate) fn compute_stats(store: &Store) -> Result<ListeningStats, RecommenderError> {
    let events = load_all_events(store)?;

    if events.is_empty() {
        return Ok(ListeningStats::default());
    }

    let mut stats = ListeningStats::default();
    let mut song_counts: HashMap<String, u32> = HashMap::new();
    let mut daily_secs: HashMap<String, u64> = HashMap::new();
    let mut ratio_sum = 0.0_f64;

    for ev in &events {
        stats.total_plays += 1;
        stats.total_listened_secs += ev.listened_secs as u64;
        if ev.was_skipped {
            stats.total_skips += 1;
        }
        ratio_sum += ev.listen_ratio as f64;

        *song_counts.entry(ev.song_id.clone()).or_insert(0) += 1;

        let date_key = ev.started_at.format("%Y-%m-%d").to_string();
        *daily_secs.entry(date_key).or_insert(0) += ev.listened_secs as u64;
    }

    stats.avg_listen_ratio = (ratio_sum / events.len() as f64) as f32;

    // Top songs by play count (top 20).
    let mut song_list: Vec<(String, u32)> = song_counts.into_iter().collect();
    song_list.sort_by(|a, b| b.1.cmp(&a.1));
    song_list.truncate(20);
    stats.top_songs = song_list;

    // Daily minutes for heatmap (last 365 days).
    let mut daily: Vec<(String, u32)> = daily_secs
        .into_iter()
        .map(|(date, secs)| (date, (secs / 60) as u32))
        .collect();
    daily.sort_by(|a, b| a.0.cmp(&b.0));
    // Only keep last 365 entries.
    if daily.len() > 365 {
        daily = daily.split_off(daily.len() - 365);
    }
    stats.daily_minutes = daily;

    // Compute streak.
    stats.streak_days = compute_streak(&stats.daily_minutes);

    Ok(stats)
}

/// Compute listening statistics for a specific time window.
pub(crate) fn compute_stats_for_period(
    store: &Store,
    since: DateTime<Utc>,
) -> Result<ListeningStats, RecommenderError> {
    let events = load_all_events(store)?;
    let filtered: Vec<&PlayEvent> = events.iter().filter(|e| e.started_at >= since).collect();

    if filtered.is_empty() {
        return Ok(ListeningStats::default());
    }

    let mut stats = ListeningStats::default();
    let mut song_counts: HashMap<String, u32> = HashMap::new();

    for ev in &filtered {
        stats.total_plays += 1;
        stats.total_listened_secs += ev.listened_secs as u64;
        if ev.was_skipped {
            stats.total_skips += 1;
        }
        *song_counts.entry(ev.song_id.clone()).or_insert(0) += 1;
    }

    let mut song_list: Vec<(String, u32)> = song_counts.into_iter().collect();
    song_list.sort_by(|a, b| b.1.cmp(&a.1));
    song_list.truncate(20);
    stats.top_songs = song_list;

    Ok(stats)
}

/// Compute the current listening streak (consecutive days ending today).
fn compute_streak(daily: &[(String, u32)]) -> u32 {
    if daily.is_empty() {
        return 0;
    }

    let today = Utc::now().date_naive();
    let mut streak = 0u32;
    let mut check_date = today;

    // Walk backwards from today.
    loop {
        let date_str = check_date.format("%Y-%m-%d").to_string();
        if daily.iter().any(|(d, mins)| d == &date_str && *mins > 0) {
            streak += 1;
            check_date = check_date.pred_opt().unwrap_or(check_date);
        } else {
            break;
        }
    }

    streak
}

/// Expose the Store's events tree for the stats module.
/// This allows the recommender lib to provide stats access.
pub(crate) fn get_stats(store: &Store) -> Result<ListeningStats, RecommenderError> {
    compute_stats(store)
}
