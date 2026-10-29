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

// ─── Catalog objects (browse pages, home feed) ───────────────────────────────

/// What a shelf item links to — derived from the navigation endpoint's id
/// shape (VL* = playlist, MPREb* = album, UC* = artist, videoId = playable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Song,
    Video,
    Album,
    Artist,
    Playlist,
    Unknown,
}

impl ItemKind {
    /// Classifies by InnerTube id conventions.
    pub fn from_ids(browse_id: Option<&str>, video_id: Option<&str>) -> Self {
        if video_id.is_some() {
            return ItemKind::Song;
        }
        match browse_id {
            Some(b) if b.starts_with("MPREb") => ItemKind::Album,
            Some(b) if b.starts_with("VL") || b.starts_with("PL") => ItemKind::Playlist,
            Some(b) if b.starts_with("UC") => ItemKind::Artist,
            _ => ItemKind::Unknown,
        }
    }
}

/// One card/row in a shelf.  Carries its own navigation endpoint so the
/// frontend (and detail pages) can follow InnerTube links generically —
/// the navigation-following backbone of Phase 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelfItem {
    pub kind: ItemKind,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    /// Playable id (songs / videos).
    #[serde(default)]
    pub video_id: Option<String>,
    /// Detail-page target (albums / artists / playlists).
    #[serde(default)]
    pub browse_id: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
    /// Radio/playlist id for playback contexts (RD…/VL…).
    #[serde(default)]
    pub playlist_id: Option<String>,
}

impl ShelfItem {
    /// Converts a playable item into an [`OnlineTrack`] for the playback path.
    pub fn to_track(&self) -> Option<OnlineTrack> {
        self.video_id.as_ref().map(|video_id| OnlineTrack {
            video_id: video_id.clone(),
            title: self.title.clone(),
            artist: self.subtitle.clone(),
            album: String::new(),
            duration_secs: None,
            thumbnail: self.thumbnail.clone(),
            cached: false,
        })
    }
}

/// A titled horizontal shelf (carousel or list) from a browse page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<ShelfItem>,
}

/// A parsed browse page: header title + every shelf found in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowsePage {
    #[serde(default)]
    pub title: String,
    pub shelves: Vec<Shelf>,
    #[serde(default)]
    pub next_continuation: Option<String>,
    /// Detail-page header (album/playlist/artist hero block), when present.
    #[serde(default)]
    pub header: Option<HeaderMeta>,
}

/// The hero block of a detail page: art, title, subtitle ("Album • 2026"),
/// and the first navigation link found in the subtitle runs (the artist /
/// owner link — lets the UI link through without another endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderMeta {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub navigate_browse_id: Option<String>,
    #[serde(default)]
    pub navigate_params: Option<String>,
}
