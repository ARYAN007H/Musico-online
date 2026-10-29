//! User catalog library (C4.1): albums / artists / tracks the user saved
//! from the online catalog.  Trees `lib:album` / `lib:artist` / `lib:track`
//! in the recommender DB — one bincode `CatalogEntry` per kind, keyed by the
//! InnerTube id.  A generic entry type keeps one code path for all kinds;
//! the frontend re-sorts (Recently Added = added_at desc, A–Z = title).
//!
//! Persistence contract: entries are re-navigable — `browse_id` + `params`
//! (and `video_id` for tracks) are stored so Library pages can deep-link
//! back into the catalog after a restart.

use serde::{Deserialize, Serialize};

use crate::errors::RecommenderError;
use crate::models::Store;

/// One saved catalog object (album, artist or track).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogEntry {
    /// InnerTube id: MPREb_* (album), UC* (artist) or video id (track).
    pub id: String,
    /// "album" | "artist" | "track"
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    /// Detail-page navigation target (albums / artists / playlists).
    #[serde(default)]
    pub browse_id: Option<String>,
    #[serde(default)]
    pub params: Option<String>,
    /// Playable id for tracks (yt: video ids).
    #[serde(default)]
    pub video_id: Option<String>,
    /// Epoch millis — Recently Added ordering.
    pub added_at: i64,
}

fn tree_name(kind: &str) -> Result<&'static str, RecommenderError> {
    match kind {
        "album" => Ok("lib:album"),
        "artist" => Ok("lib:artist"),
        "track" => Ok("lib:track"),
        other => Err(RecommenderError::Invalid(format!(
            "unknown library kind: {other}"
        ))),
    }
}

/// Inserts or updates an entry (same id = replace, added_at refreshed).
pub(crate) fn put(store: &Store, entry: &CatalogEntry) -> Result<(), RecommenderError> {
    if entry.id.is_empty() || entry.title.is_empty() {
        return Err(RecommenderError::Invalid("library entry needs id and title".into()));
    }
    let tree = store.db.open_tree(tree_name(&entry.kind)?)?;
    let bytes = bincode::serialize(entry)
        .map_err(|e| RecommenderError::Serialization(e.to_string()))?;
    tree.insert(entry.id.as_bytes(), bytes)
        .map_err(RecommenderError::DbError)?;
    Ok(())
}

pub(crate) fn remove(store: &Store, kind: &str, id: &str) -> Result<(), RecommenderError> {
    let tree = store.db.open_tree(tree_name(kind)?)?;
    tree.remove(id.as_bytes()).map_err(RecommenderError::DbError)?;
    Ok(())
}

/// All entries of a kind, newest first.
pub(crate) fn list(store: &Store, kind: &str) -> Result<Vec<CatalogEntry>, RecommenderError> {
    let tree = store.db.open_tree(tree_name(kind)?)?;
    let mut out = Vec::new();
    for pair in tree.iter() {
        let (_, value) = pair.map_err(RecommenderError::DbError)?;
        match bincode::deserialize::<CatalogEntry>(&value) {
            Ok(entry) => out.push(entry),
            Err(e) => log::warn!("library: dropping undecodable {kind} entry: {e}"),
        }
    }
    out.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.title.cmp(&b.title)));
    Ok(out)
}

/// Which of `ids` are saved (preserves input order) — drives Add-button state.
pub(crate) fn contains(store: &Store, kind: &str, ids: &[String]) -> Result<Vec<String>, RecommenderError> {
    let tree = store.db.open_tree(tree_name(kind)?)?;
    Ok(ids
        .iter()
        .filter(|id| tree.contains_key(id.as_bytes()).unwrap_or(false))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str, title: &str, added_at: i64) -> CatalogEntry {
        CatalogEntry {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            subtitle: "Subtitle".into(),
            thumbnail: None,
            browse_id: Some("MPREb_test".into()),
            params: None,
            video_id: (kind == "track").then(|| format!("yt:{id}")),
            added_at,
        }
    }

    fn tmp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().to_str().unwrap()).expect("open tmp store");
        (dir, store)
    }

    #[test]
    fn roundtrip_put_list_remove() {
        let (_dir, store) = tmp_store();
        put(&store, &entry("a1", "album", "Alpha", 100)).unwrap();
        put(&store, &entry("a2", "album", "Beta", 200)).unwrap();
        put(&store, &entry("t1", "track", "Track", 300)).unwrap();

        let albums = list(&store, "album").unwrap();
        assert_eq!(albums.len(), 2, "kind isolation");
        assert_eq!(albums[0].id, "a2", "newest first");
        assert_eq!(albums[1].title, "Alpha");

        let tracks = list(&store, "track").unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].video_id.as_deref(), Some("yt:t1"));

        remove(&store, "album", "a1").unwrap();
        assert_eq!(list(&store, "album").unwrap().len(), 1);

        let hit = contains(&store, "album", &["a2".into(), "zz".into()]).unwrap();
        assert_eq!(hit, vec!["a2".to_string()]);
    }

    #[test]
    fn re_put_updates_entry_in_place() {
        let (_dir, store) = tmp_store();
        put(&store, &entry("x", "artist", "Old Title", 1)).unwrap();
        put(&store, &entry("x", "artist", "New Title", 2)).unwrap();
        let artists = list(&store, "artist").unwrap();
        assert_eq!(artists.len(), 1, "same id replaces");
        assert_eq!(artists[0].title, "New Title");
    }

    #[test]
    fn rejects_unknown_kind_and_empty_fields() {
        let (_dir, store) = tmp_store();
        assert!(put(&store, &entry("e", "playlist", "Nope", 1)).is_err());
        assert!(put(&store, &entry("", "album", "No id", 1)).is_err());
        assert!(put(&store, &entry("e", "album", "", 1)).is_err());
    }

    #[test]
    fn list_10k_entries_stays_fast() {
        let (_dir, store) = tmp_store();
        for i in 0..10_000u32 {
            let e = entry(&format!("id{i}"), "track", &format!("Song {:05}", 9_999 - i), i as i64);
            put(&store, &e).unwrap();
        }
        let t0 = std::time::Instant::now();
        let all = list(&store, "track").unwrap();
        let elapsed = t0.elapsed();
        assert_eq!(all.len(), 10_000);
        assert_eq!(all[0].title, "Song 00000", "added_at desc wins");
        println!("10k list took {elapsed:?}");
        assert!(elapsed.as_secs() < 5, "10k listing took {elapsed:?}");
    }
}
