//! Playback queue management.
//!
//! The queue is a helper for the GUI/app layer. The playback engine itself
//! only knows about the *current* song — queue navigation is the caller's
//! responsibility.

use crate::state::SongInfo;
use std::collections::VecDeque;

/// An ordered queue of songs with history tracking for previous/next navigation.
pub struct PlaybackQueue {
    /// Songs waiting to be played.
    songs: VecDeque<SongInfo>,
    /// Songs already played this session (most recent last).
    history: Vec<SongInfo>,
    /// Index of the currently playing song within `history`, if any.
    current_index: Option<usize>,
}

impl PlaybackQueue {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self {
            songs: VecDeque::new(),
            history: Vec::new(),
            current_index: None,
        }
    }

    /// Appends a song to the back of the queue.
    pub fn push_back(&mut self, song: SongInfo) {
        self.songs.push_back(song);
    }

    /// Inserts a song at the front of the queue (play next).
    pub fn push_front(&mut self, song: SongInfo) {
        self.songs.push_front(song);
    }

    /// Pops the next song from the front of the queue.
    ///
    /// The popped song is pushed onto the history stack so that
    /// `previous()` can retrieve it.
    pub fn next(&mut self) -> Option<SongInfo> {
        let song = self.songs.pop_front()?;
        self.history.push(song.clone());
        self.current_index = Some(self.history.len() - 1);
        Some(song)
    }

    /// Returns the previous song from the history stack.
    ///
    /// Moves backward through history. Returns `None` if at the beginning.
    pub fn previous(&mut self) -> Option<SongInfo> {
        match self.current_index {
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.current_index = Some(new_idx);
                Some(self.history[new_idx].clone())
            }
            _ => None,
        }
    }

    /// Removes the song at the given index from the upcoming queue.
    pub fn remove_at(&mut self, index: usize) -> Option<SongInfo> {
        self.songs.remove(index)
    }

    /// Clears both the queue and history.
    pub fn clear(&mut self) {
        self.songs.clear();
        self.history.clear();
        self.current_index = None;
    }

    /// Clears only the upcoming songs; history (and thus Previous) survives.
    pub fn clear_upcoming(&mut self) {
        self.songs.clear();
    }

    /// Returns a reference to the current queue contents.
    pub fn current_queue(&self) -> &VecDeque<SongInfo> {
        &self.songs
    }

    /// Returns an iterator over the upcoming songs.
    pub fn iter(&self) -> impl Iterator<Item = &SongInfo> {
        self.songs.iter()
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    /// Returns the number of songs in the queue.
    pub fn len(&self) -> usize {
        self.songs.len()
    }

    /// Shuffles the upcoming songs randomly while preserving history.
    pub fn shuffle(&mut self) {
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        let mut songs: Vec<SongInfo> = self.songs.drain(..).collect();
        songs.shuffle(&mut rng);
        self.songs = songs.into();
    }

    /// Peek at the next song without removing it. Used for gapless preloading.
    pub fn peek_next(&self) -> Option<SongInfo> {
        self.songs.front().cloned()
    }
}

impl Default for PlaybackQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SongInfo;

    fn song(id: &str) -> SongInfo {
        SongInfo {
            id: id.into(),
            file_path: format!("/tmp/{id}.mp3"),
            title: id.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_secs: 180.0,
            cover_art: None,
        }
    }

    #[test]
    fn play_next_inserts_before_appended() {
        let mut q = PlaybackQueue::new();
        q.push_back(song("a"));
        q.push_back(song("b"));
        q.push_front(song("next"));
        assert_eq!(q.current_queue()[0].id, "next", "push_front plays next");
        assert_eq!(q.current_queue()[1].id, "a");
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn next_pops_in_order_and_tracks_history() {
        let mut q = PlaybackQueue::new();
        q.push_back(song("a"));
        q.push_front(song("next"));
        assert_eq!(q.next().map(|s| s.id), Some("next".into()));
        assert_eq!(q.next().map(|s| s.id), Some("a".into()));
        assert_eq!(q.next().map(|s| s.id), None, "queue exhausted");
        assert_eq!(q.previous().map(|s| s.id), Some("next".into()), "one step back");
        assert_eq!(q.previous().map(|s| s.id), None, "history start");
    }

    #[test]
    fn clear_upcoming_keeps_history() {
        let mut q = PlaybackQueue::new();
        q.push_back(song("a"));
        q.push_back(song("b"));
        assert_eq!(q.next().map(|s| s.id), Some("a".into()));
        assert_eq!(q.next().map(|s| s.id), Some("b".into()));
        q.push_back(song("c"));
        q.clear_upcoming();
        assert!(q.is_empty(), "upcoming dropped");
        assert_eq!(q.previous().map(|s| s.id), Some("a".into()), "history kept");
    }

    #[test]
    fn remove_at_deletes_exactly_one() {
        let mut q = PlaybackQueue::new();
        q.push_back(song("a"));
        q.push_back(song("b"));
        q.push_back(song("c"));
        assert_eq!(q.remove_at(1).map(|s| s.id), Some("b".into()));
        let ids: Vec<_> = q.current_queue().iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["a".to_string(), "c".to_string()]);
        assert_eq!(q.remove_at(9).map(|s| s.id), None, "out of range is a no-op");
    }

    #[test]
    fn peek_next_does_not_consume() {
        let mut q = PlaybackQueue::new();
        q.push_back(song("a"));
        assert_eq!(q.peek_next().map(|s| s.id), Some("a".into()));
        assert_eq!(q.peek_next().map(|s| s.id), Some("a".into()));
        assert_eq!(q.len(), 1);
    }
}
