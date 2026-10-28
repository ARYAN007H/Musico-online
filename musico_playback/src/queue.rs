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
