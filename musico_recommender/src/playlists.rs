//! Smart Playlists — rule-based, auto-updating collections.
//!
//! Playlists are defined by a set of filters and persist in the sled database.

use serde::{Deserialize, Serialize};
use crate::errors::RecommenderError;
use crate::models::{SongRecord, Store};

/// Possible filter operators for smart playlist rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterOp {
    Contains(String),
    Equals(String),
    GreaterThan(f64),
    LessThan(f64),
}

/// A field on which a filter can operate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterField {
    Title,
    Artist,
    Album,
    DurationSecs,
}

/// A single filter rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterRule {
    pub field: FilterField,
    pub op: FilterOp,
}

/// A smart playlist definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartPlaylist {
    pub id: String,
    pub name: String,
    pub rules: Vec<FilterRule>,
    /// Maximum number of songs (0 = unlimited).
    pub max_songs: usize,
    /// Sort by this field (defaults to title).
    pub sort_by: String,
    pub sort_ascending: bool,
}

impl SmartPlaylist {
    /// Create a new empty playlist.
    pub fn new(name: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            rules: Vec::new(),
            max_songs: 0,
            sort_by: "title".to_string(),
            sort_ascending: true,
        }
    }

    /// Evaluate a song against all rules. Returns true if it matches.
    pub fn matches(&self, record: &SongRecord) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        self.rules.iter().all(|rule| rule.matches(record))
    }

    /// Resolve this playlist against a library of songs.
    pub fn resolve(&self, library: &[SongRecord]) -> Vec<SongRecord> {
        let mut results: Vec<SongRecord> = library.iter()
            .filter(|s| self.matches(s))
            .cloned()
            .collect();

        // Sort.
        match self.sort_by.as_str() {
            "artist" => results.sort_by(|a, b| {
                if self.sort_ascending { a.artist.cmp(&b.artist) } else { b.artist.cmp(&a.artist) }
            }),
            "album" => results.sort_by(|a, b| {
                if self.sort_ascending { a.album.cmp(&b.album) } else { b.album.cmp(&a.album) }
            }),
            "duration" => results.sort_by(|a, b| {
                let cmp = a.duration_secs.cmp(&b.duration_secs);
                if self.sort_ascending { cmp } else { cmp.reverse() }
            }),
            _ => results.sort_by(|a, b| {
                if self.sort_ascending { a.title.cmp(&b.title) } else { b.title.cmp(&a.title) }
            }),
        }

        if self.max_songs > 0 {
            results.truncate(self.max_songs);
        }

        results
    }

    /// Export as M3U playlist string.
    pub fn to_m3u(&self, songs: &[SongRecord]) -> String {
        let mut lines = vec!["#EXTM3U".to_string()];
        for s in songs {
            lines.push(format!("#EXTINF:{},{} - {}", s.duration_secs as i32, s.artist, s.title));
            lines.push(s.file_path.clone());
        }
        lines.join("\n")
    }
}

impl FilterRule {
    pub fn matches(&self, record: &SongRecord) -> bool {
        match (&self.field, &self.op) {
            (FilterField::Title, FilterOp::Contains(s)) => {
                record.title.to_lowercase().contains(&s.to_lowercase())
            }
            (FilterField::Artist, FilterOp::Contains(s)) => {
                record.artist.to_lowercase().contains(&s.to_lowercase())
            }
            (FilterField::Album, FilterOp::Contains(s)) => {
                record.album.to_lowercase().contains(&s.to_lowercase())
            }
            (FilterField::Title, FilterOp::Equals(s)) => record.title.eq_ignore_ascii_case(s),
            (FilterField::Artist, FilterOp::Equals(s)) => record.artist.eq_ignore_ascii_case(s),
            (FilterField::Album, FilterOp::Equals(s)) => record.album.eq_ignore_ascii_case(s),
            (FilterField::DurationSecs, FilterOp::GreaterThan(v)) => (record.duration_secs as f64) > *v,
            (FilterField::DurationSecs, FilterOp::LessThan(v)) => (record.duration_secs as f64) < *v,
            _ => true, // Unknown combination — pass through.
        }
    }
}

// ─── Preset Playlists ────────────────────────────────────────────────────────

/// Built-in smart playlist presets.
pub fn builtin_playlists() -> Vec<SmartPlaylist> {
    vec![
        {
            let mut p = SmartPlaylist::new("Short Songs");
            p.rules.push(FilterRule {
                field: FilterField::DurationSecs,
                op: FilterOp::LessThan(180.0),
            });
            p.sort_by = "duration".to_string();
            p.sort_ascending = true;
            p
        },
        {
            let mut p = SmartPlaylist::new("Long Jams");
            p.rules.push(FilterRule {
                field: FilterField::DurationSecs,
                op: FilterOp::GreaterThan(300.0),
            });
            p.sort_by = "duration".to_string();
            p.sort_ascending = false;
            p
        },
    ]
}

// ─── Persistence ─────────────────────────────────────────────────────────────

const PLAYLISTS_TREE: &str = "playlists";

/// Save a playlist to the database.
pub fn save_playlist(store: &Store, playlist: &SmartPlaylist) -> Result<(), RecommenderError> {
    let tree = store.db.open_tree(PLAYLISTS_TREE).map_err(RecommenderError::DbError)?;
    let data = bincode::serialize(playlist).map_err(|e| {
        RecommenderError::DecodeError(format!("Failed to serialize playlist: {e}"))
    })?;
    tree.insert(playlist.id.as_bytes(), data).map_err(RecommenderError::DbError)?;
    Ok(())
}

/// Load all playlists from the database.
pub fn load_playlists(store: &Store) -> Result<Vec<SmartPlaylist>, RecommenderError> {
    let tree = store.db.open_tree(PLAYLISTS_TREE).map_err(RecommenderError::DbError)?;
    let mut playlists = Vec::new();
    for entry in tree.iter() {
        let (_, value) = entry.map_err(RecommenderError::DbError)?;
        if let Ok(pl) = bincode::deserialize::<SmartPlaylist>(&value) {
            playlists.push(pl);
        }
    }
    Ok(playlists)
}

/// Delete a playlist.
pub fn delete_playlist(store: &Store, playlist_id: &str) -> Result<(), RecommenderError> {
    let tree = store.db.open_tree(PLAYLISTS_TREE).map_err(RecommenderError::DbError)?;
    tree.remove(playlist_id.as_bytes()).map_err(RecommenderError::DbError)?;
    Ok(())
}
