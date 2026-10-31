//! YTM song lyrics via InnerTube — the payload is Musixmatch-backed plain
//! text (UNSYNCED; YouTube does not expose timed lines).  Two-step flow,
//! mirroring what the web player does (verified live 2026-10-31):
//!   1. `next` for the song's watch queue (`RDAMVM<id>`) → the "Lyrics" tab
//!      carries a `browseEndpoint` whose browseId (`MPLYt_<queue-id>`) is
//!      NOT derivable from the video id — it must be parsed out.
//!   2. `browse` on that id → `musicDescriptionShelfRenderer` → text.
//! Shape-tolerant: tabs and shelves are collected by name anywhere in the
//! tree, so cosmetic drift degrades to "no lyrics", never a parse panic.

use serde_json::Value;

use super::client::{innertube_context, innertube_post, BROWSE_URL, NEXT_URL};
use super::parse::collect_values;

/// Fetches the lyrics text for a song.  `None` = no lyrics available
/// (instrumental, region-locked shelf, or shape drift).
pub fn fetch_lyrics(video_id: &str) -> Option<String> {
    let video_id = video_id.trim();
    if video_id.is_empty() {
        return None;
    }

    // Step 1: the watch-next response carries the Lyrics tab's browse target.
    let body = serde_json::json!({
        "context": innertube_context(),
        "videoId": video_id,
        "playlistId": format!("RDAMVM{video_id}"),
    });
    let first = innertube_post(NEXT_URL, body).ok()?;

    let mut tabs = Vec::new();
    collect_values(&first, "tabRenderer", &mut tabs);
    let lyrics_tab = tabs.iter().find(|t| tab_title(t).eq_ignore_ascii_case("lyrics"))?;

    // Tolerant extraction: the MPLYt browseId lives somewhere inside the
    // tab's endpoint subtree — don't pin the exact path.
    let mut browse_id = None;
    collect_strings_under(lyrics_tab, "browseId", &mut |s| {
        if s.starts_with("MPLYt_") {
            browse_id = Some(s.to_string());
        }
    });
    let browse_id = browse_id?;

    // Step 2: the lyrics page for that browse target.
    let body = serde_json::json!({
        "context": innertube_context(),
        "browseId": browse_id,
    });
    let page = innertube_post(BROWSE_URL, body).ok()?;

    let mut shelves = Vec::new();
    collect_values(&page, "musicDescriptionShelfRenderer", &mut shelves);
    let text = shelves
        .iter()
        .filter_map(|shelf| {
            shelf
                .pointer("/description/runs")
                .and_then(|v| v.as_array())
                .map(|runs| {
                    runs.iter()
                        .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                        .collect::<String>()
                })
        })
        .find(|s| !s.trim().is_empty())?;
    Some(text)
}

/// A tabRenderer's title — a bare string, simpleText, or runs, tolerantly
/// (live responses use a bare string; the others guard against drift).
fn tab_title(tab: &Value) -> String {
    let title = tab.get("title").unwrap_or(&Value::Null);
    if let Some(s) = title.as_str() {
        return s.to_string();
    }
    if let Some(s) = title.get("simpleText").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    title
        .get("runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Walks `node` depth-first, invoking `out` for every string value stored
/// under `key`.
fn collect_strings_under(node: &Value, key: &str, out: &mut impl FnMut(&str)) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    if let Some(s) = v.as_str() {
                        out(s);
                    }
                }
                collect_strings_under(v, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_strings_under(v, key, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tab_title_reads_string_simpletext_and_runs() {
        assert_eq!(tab_title(&json!({ "title": "Lyrics" })), "Lyrics");
        assert_eq!(tab_title(&json!({ "title": { "simpleText": "Lyrics" } })), "Lyrics");
        assert_eq!(
            tab_title(&json!({ "title": { "runs": [{ "text": "Ly" }, { "text": "rics" }] } })),
            "Lyrics"
        );
        assert_eq!(tab_title(&json!({})), "");
    }

    #[test]
    fn finds_mplyt_browse_id_anywhere_under_the_tab() {
        let tab = json!({
            "title": { "simpleText": "Lyrics" },
            "endpoint": {
                "clickTrackingParams": "x",
                "browseEndpoint": {
                    "browseId": "MPLYt_6AEOVn4k62q",
                    "pageType": "MUSIC_PAGE_TYPE_LYRICS",
                },
            },
        });
        let mut browse_id = None;
        collect_strings_under(&tab, "browseId", &mut |s| {
            if s.starts_with("MPLYt_") {
                browse_id = Some(s.to_string());
            }
        });
        assert_eq!(browse_id.as_deref(), Some("MPLYt_6AEOVn4k62q"));
    }

    #[test]
    fn ignores_non_lyrics_browse_ids() {
        let tab = json!({ "endpoint": { "browseEndpoint": { "browseId": "MPTRt_x" } } });
        let mut browse_id = None;
        collect_strings_under(&tab, "browseId", &mut |s| {
            if s.starts_with("MPLYt_") {
                browse_id = Some(s.to_string());
            }
        });
        assert!(browse_id.is_none());
    }

    #[test]
    fn empty_and_whitespace_ids_are_rejected() {
        assert!(fetch_lyrics("").is_none());
        assert!(fetch_lyrics("   ").is_none());
    }
}
