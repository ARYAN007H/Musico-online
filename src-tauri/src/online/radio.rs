//! Related tracks via the InnerTube `next` endpoint (`RD<videoId>` radio
//! playlists) — the engine behind online autoplay and personal stations.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use super::client::{http_client, innertube_context, innertube_post, NEXT_URL};
use super::models::OnlineTrack;
use super::parse::parse_radio_response;
use super::stream::cached_file;

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

    let client = http_client()?;
    let json = innertube_post(&client, NEXT_URL, body)?;
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
}
