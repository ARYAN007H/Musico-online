//! Data types crossing the IPC boundary.  Serialization shapes are load-bearing:
//! the frontend sends [`OnlineTrack`] back verbatim on play/enqueue.

use serde::{Deserialize, Serialize};

/// A track found via online search or radio.  Sent to the frontend as-is and
/// back on play, so no server-side result cache is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineTrack {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_secs: Option<u32>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    /// Already present in the local stream cache — instant playback.
    #[serde(default)]
    pub cached: bool,
}

impl OnlineTrack {
    /// Stable song id in the local stack — distinct namespace from the
    /// path-hash ids of the local library.
    pub fn song_id(&self) -> String {
        format!("yt:{}", self.video_id)
    }

    /// Stream URL for yt-dlp.
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.video_id)
    }
}

/// One page of search results.  `next_continuation` is the opaque token for
/// the next page (pass it to [`crate::online::search`]); `None` = end of results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultPage {
    pub tracks: Vec<OnlineTrack>,
    #[serde(default)]
    pub next_continuation: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_ids_are_namespaced() {
        let t = OnlineTrack {
            video_id: "abc123".into(),
            title: "T".into(),
            artist: "A".into(),
            album: String::new(),
            duration_secs: Some(10),
            thumbnail: None,
            cached: false,
        };
        assert_eq!(t.song_id(), "yt:abc123");
        assert_eq!(t.watch_url(), "https://www.youtube.com/watch?v=abc123");
    }
}
