//! Related tracks via the InnerTube `next` endpoint (`RD<videoId>` radio
//! playlists) — the engine behind online autoplay and personal stations.
//!
//! Radio smoothness: while a `yt:` track plays with nothing queued, a
//! speculative next pick is fetched + pre-resolved in the background
//! ([`warm_next`]); the advance ([`take_warm_pick`]) then becomes a cache
//! hit instead of a fetch+download dead-air gap.  One warm slot; a replaced
//! warm's download finishes naturally (bounded waste: one track).

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use super::client::{innertube_context, innertube_post, NEXT_URL};
use super::models::OnlineTrack;
use super::parse::parse_radio_response;

/// Radio picks avoid this many recently played online tracks.
pub(crate) const RADIO_REMEMBER: usize = 12;

/// Fetches YouTube Music's radio/related list for a seed track (up to ~50
/// tracks).  The seed itself and recently-played online picks are excluded.
pub fn radio_tracks(seed_video_id: &str) -> Result<Vec<OnlineTrack>, String> {
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": seed_video_id,
        "playlistId": format!("RD{seed_video_id}"),
    });

    let json = innertube_post(NEXT_URL, body)?;
    let mut tracks = parse_radio_response(&json);
    tracks.retain(|t| t.video_id != seed_video_id);

    // Rotate past what was played recently (best-effort variety).
    let recent = RADIO_RECENT.lock().map(|g| g.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
    if !recent.is_empty() {
        let fresh: Vec<OnlineTrack> = tracks.iter().filter(|t| !recent.contains(&t.video_id)).cloned().collect();
        if fresh.len() >= 3 {
            tracks = fresh;
        }
    }

    // Shuffle so repeat radio sessions differ.
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    tracks.shuffle(&mut rng);
    Ok(tracks)
}

static RADIO_RECENT: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(RADIO_REMEMBER)));

/// Records that an online track was auto-played (radio variety memory).
pub fn remember_radio_play(video_id: &str) {
    if let Ok(mut q) = RADIO_RECENT.lock() {
        q.push_back(video_id.to_string());
        while q.len() > RADIO_REMEMBER {
            q.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// Radio warmup — the next pick, downloaded before it is needed
// ---------------------------------------------------------------------------

/// One speculative next radio pick.  Replaced (not cancelled) by newer
/// warms — the superseded download runs to completion, bounded waste of one
/// track.
struct WarmPick {
    seed: String,
    track: OnlineTrack,
}

static WARM: Mutex<Option<WarmPick>> = Mutex::new(None);

/// Pops the warmed pick when it belongs to `seed_video_id` (the track that
/// just ended).  `None` → the advance fetches live.
pub fn take_warm_pick(seed_video_id: &str) -> Option<OnlineTrack> {
    let mut g = WARM.lock().ok()?;
    if g.as_ref().is_some_and(|w| w.seed == seed_video_id) {
        g.take().map(|w| w.track)
    } else {
        None
    }
}

/// Fetches YouTube Music's next radio pick for `seed_video_id` and
/// pre-resolves its stream (and cover art) in the background.  Fire and
/// forget: failures are silent — the advance falls back to a live fetch.
pub fn warm_next(seed_video_id: String) {
    let tracks = match radio_tracks(&seed_video_id) {
        Ok(t) => t,
        Err(e) => {
            log::debug!("radio warm: fetch failed: {e}");
            return;
        }
    };
    let Some(track) = tracks.into_iter().next() else {
        return;
    };
    if let Ok(mut g) = WARM.lock() {
        *g = Some(WarmPick { seed: seed_video_id, track: track.clone() });
    }

    if let Err(e) = super::stream::resolve_track(&track, false) {
        log::debug!("radio warm: resolve failed: {e}");
        // A pick that can't download would fail the advance anyway — don't
        // let it poison the slot.
        if let Ok(mut g) = WARM.lock() {
            if g.as_ref().is_some_and(|w| w.track.video_id == track.video_id) {
                *g = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_recent_ring_buffer_caps() {
        RADIO_RECENT.lock().unwrap().clear();
        for i in 0..(RADIO_REMEMBER + 5) {
            remember_radio_play(&format!("v{i}"));
        }
        let q = RADIO_RECENT.lock().unwrap();
        assert_eq!(q.len(), RADIO_REMEMBER);
        assert!(!q.contains(&"v0".to_string()), "oldest entries evicted");
        assert!(q.contains(&format!("v{}", RADIO_REMEMBER + 4)));
    }

    #[test]
    fn warm_slot_matches_seed_and_pops() {
        let track = OnlineTrack {
            video_id: "warmvid".into(),
            title: "Warm".into(),
            artist: "A".into(),
            album: String::new(),
            duration_secs: None,
            thumbnail: None,
            cached: false,
        };
        *WARM.lock().unwrap() = Some(WarmPick { seed: "seedA".into(), track: track.clone() });
        // Wrong seed → no pick, slot untouched.
        assert!(take_warm_pick("seedB").is_none());
        assert!(WARM.lock().unwrap().is_some());
        // Right seed → pick consumed, slot empty.
        let got = take_warm_pick("seedA").expect("warm pick consumed");
        assert_eq!(got.video_id, "warmvid");
        assert!(WARM.lock().unwrap().is_none());
        assert!(take_warm_pick("seedA").is_none(), "second take is empty");
    }
}
