//! Download manager (C3.3): tracks explicit "Download" actions on online
//! tracks so the UI can show per-download state.  A download IS a stream
//! cache fill: `resolve_stream` does the work (yt-dlp, verify, prune, cover
//! art via `to_song_info` on playback), the manager only booksends the
//! state machine and remembers outcomes for the settings readout.
//!
//! State machine: `resolving → done | failed(reason)`.  A failed download
//! is retried by invoking the command again — there is no mid-stream pause.

use std::collections::HashMap;
use std::sync::Mutex;

/// One download's progress through the state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStage {
    Resolving,
    Done,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadEntry {
    pub stage: DownloadStage,
    pub title: String,
}

static DOWNLOADS: Mutex<Option<HashMap<String, DownloadEntry>>> = Mutex::new(None);

fn with_map<T>(f: impl FnOnce(&mut HashMap<String, DownloadEntry>) -> T) -> Option<T> {
    let mut guard = DOWNLOADS.lock().ok()?;
    let map = guard.get_or_insert_with(HashMap::new);
    Some(f(map))
}

/// Marks a download as started (resolving).  Re-downloading a Done or Failed
/// entry resets it — retry semantics are just "invoke again".
pub fn start(video_id: &str, title: &str) {
    with_map(|map| {
        map.insert(
            video_id.to_string(),
            DownloadEntry { stage: DownloadStage::Resolving, title: title.to_string() },
        );
    });
}

/// Books the final outcome of a download.
pub fn finish(video_id: &str, result: Result<(), String>) {
    with_map(|map| {
        if let Some(entry) = map.get_mut(video_id) {
            entry.stage = match result {
                Ok(()) => DownloadStage::Done,
                Err(e) => DownloadStage::Failed(e),
            };
        }
    });
}

/// Snapshot of one download's state (None = never downloaded this session).
pub fn state(video_id: &str) -> Option<DownloadEntry> {
    with_map(|map| map.get(video_id).cloned()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_transitions() {
        start("vid1", "Song One");
        assert_eq!(state("vid1").map(|e| e.stage), Some(DownloadStage::Resolving));
        assert_eq!(state("vid1").map(|e| e.title), Some("Song One".into()));

        finish("vid1", Ok(()));
        assert_eq!(state("vid1").map(|e| e.stage), Some(DownloadStage::Done));

        // Retry after failure: start resets, finish records the reason.
        start("vid1", "Song One");
        finish("vid1", Err("yt-dlp choked".into()));
        assert_eq!(
            state("vid1").map(|e| e.stage),
            Some(DownloadStage::Failed("yt-dlp choked".into()))
        );
    }

    #[test]
    fn unknown_video_is_none() {
        assert_eq!(state("never-seen"), None);
    }
}
