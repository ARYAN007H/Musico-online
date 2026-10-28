//! ALL InnerTube response→model parsing lives here — the only file that
//! should ever need a `fix(innertube)` patch when YouTube changes shapes.
//! Parsing is tolerant by design: malformed items are skipped, unknown shelf
//! types degrade to empty, and both the initial and continuation response
//! shapes are handled.

use super::models::{OnlineTrack, SearchResultPage};
use super::stream::cached_file;

/// Parses a search response — handles BOTH the initial shape
/// (`contents.tabbedSearchResultsRenderer…`) and the continuation shape
/// (`continuationContents.musicShelfContinuation…`).
pub(crate) fn parse_search_page(json: &serde_json::Value) -> SearchResultPage {
    let mut tracks = Vec::new();
    let mut next_continuation = None;

    // Initial shape.
    if let Some(tabs) = json
        .pointer("/contents/tabbedSearchResultsRenderer/tabs")
        .and_then(|v| v.as_array())
    {
        for tab in tabs {
            let Some(sections) = tab
                .pointer("/tabRenderer/content/sectionListRenderer/contents")
                .and_then(|v| v.as_array())
            else { continue };
            for section in sections {
                let Some(shelf) = section.get("musicShelfRenderer") else { continue };
                collect_shelf_items(shelf, &mut tracks);
                next_continuation = shelf_continuation(shelf).or(next_continuation);
            }
        }
    }

    // Continuation shape.
    if let Some(shelf) = json.pointer("/continuationContents/musicShelfContinuation") {
        collect_shelf_items(shelf, &mut tracks);
        next_continuation = shelf_continuation(shelf).or(next_continuation);
    }

    SearchResultPage { tracks, next_continuation }
}

fn collect_shelf_items(shelf: &serde_json::Value, out: &mut Vec<OnlineTrack>) {
    let Some(items) = shelf.get("contents").and_then(|v| v.as_array()) else { return };
    for item in items {
        if let Some(renderer) = item.get("musicResponsiveListItemRenderer") {
            if let Some(track) = parse_list_item(renderer) {
                out.push(track);
            }
        }
    }
}

fn shelf_continuation(shelf: &serde_json::Value) -> Option<String> {
    shelf
        .pointer("/continuations/0/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Parses one `musicResponsiveListItemRenderer`:
/// - flexColumns[0]: title runs (first run carries the watchEndpoint videoId)
/// - flexColumns[1]: "Artist • Album • 3:45" (album may be absent; the last
///   dot-separated part is the duration when it looks like m:ss)
/// - thumbnail.musicThumbnailRenderer: ascending-size thumbnail list
fn parse_list_item(renderer: &serde_json::Value) -> Option<OnlineTrack> {
    let flex = renderer.get("flexColumns")?.as_array()?;

    // ---- Title + videoId ----
    let first = flex.first()?.pointer("/musicResponsiveListItemFlexColumnRenderer/text")?;
    let title = runs_text(first)
        .split('\n')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }

    let mut video_id = renderer
        .pointer("/playlistItemData/videoId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if video_id.is_none() {
        if let Some(runs) = first.get("runs").and_then(|v| v.as_array()) {
            for run in runs {
                if let Some(id) = run
                    .pointer("/navigationEndpoint/watchEndpoint/videoId")
                    .and_then(|v| v.as_str())
                {
                    video_id = Some(id.to_string());
                    break;
                }
            }
        }
    }
    let video_id = video_id?;
    if video_id.is_empty() {
        return None;
    }

    // ---- Artist / album / duration ----
    let (mut artist, mut album, mut duration_secs) = (String::new(), String::new(), None);
    if let Some(second) = flex
        .get(1)
        .and_then(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text"))
    {
        let joined = runs_text(second);
        let parts: Vec<&str> = joined.split(" • ").map(str::trim).collect();
        if let Some(first_part) = parts.first() {
            artist = first_part.to_string();
        }
        // The last time-like part is the duration; whatever sits between the
        // artist and the duration is the album.
        let has_duration = parts.last().and_then(|p| parse_duration(p));
        if let Some(secs) = has_duration {
            duration_secs = Some(secs);
        }
        let middle_end = if has_duration.is_some() {
            parts.len().saturating_sub(1)
        } else {
            parts.len()
        };
        let middle: Vec<&str> =
            parts.iter().skip(1).take(middle_end.saturating_sub(1)).copied().collect();
        if !middle.is_empty() {
            album = middle.join(" • ");
        }
    }

    // ---- Thumbnail (largest variant, upscaled for cover art) ----
    let thumbnail = renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|thumbs| thumbs.last())
        .and_then(|t| t.get("url"))
        .and_then(|u| u.as_str())
        .map(|u| upscale_thumbnail(u));

    let cached = cached_file(&video_id).is_some();

    Some(OnlineTrack {
        video_id,
        title,
        artist,
        album,
        duration_secs,
        thumbnail,
        cached,
    })
}

pub(crate) fn runs_text(text_obj: &serde_json::Value) -> String {
    if let Some(simple) = text_obj.get("simpleText").and_then(|v| v.as_str()) {
        return simple.to_string();
    }
    text_obj
        .get("runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// ytimg/yt3 thumbnail URLs embed their size (`…=w60-h60-l90-rj`); rewrite to
/// a size that survives the cover cache (544 px).  Best-effort: on any mismatch
/// the original URL is kept.
fn upscale_thumbnail(url: &str) -> String {
    const TARGET: &str = "=w544-h544-p-l90-rj";
    match url.find('=') {
        Some(eq) if eq > 0 => format!("{}{}", &url[..eq], TARGET),
        _ => url.to_string(),
    }
}

/// Parses "3:45" / "1:02:03" into seconds.
pub(crate) fn parse_duration(s: &str) -> Option<u32> {
    let mut total = 0u32;
    for part in s.split(':') {
        let n: u32 = part.trim().parse().ok()?;
        total = total * 60 + n;
    }
    (total > 0).then_some(total)
}

/// Parses `playlistPanelVideoRenderer` items out of a `next` (radio) response.
pub(crate) fn parse_radio_response(json: &serde_json::Value) -> Vec<OnlineTrack> {
    let mut renderers = Vec::new();
    collect_values(json, "playlistPanelVideoRenderer", &mut renderers);

    let mut tracks = Vec::new();
    for r in renderers {
        let Some(video_id) = r.get("videoId").and_then(|v| v.as_str()) else { continue };
        if video_id.is_empty() {
            continue;
        }
        let title = r
            .pointer("/title/runs")
            .and_then(|v| v.as_array())
            .map(|runs| {
                runs.iter()
                    .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let artist = r
            .pointer("/shortBylineText/runs/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let duration_secs = r
            .pointer("/lengthText/simpleText")
            .and_then(|v| v.as_str())
            .and_then(parse_duration);
        let thumbnail = r
            .pointer("/thumbnail/thumbnails")
            .and_then(|v| v.as_array())
            .and_then(|thumbs| thumbs.last())
            .and_then(|t| t.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| upscale_thumbnail(u));

        tracks.push(OnlineTrack {
            video_id: video_id.to_string(),
            title,
            artist,
            album: String::new(),
            duration_secs,
            thumbnail,
            cached: cached_file(video_id).is_some(),
        });
    }
    tracks
}

/// Depth-first collector for a key name anywhere in the response tree.
fn collect_values<'a>(node: &'a serde_json::Value, key: &str, out: &mut Vec<&'a serde_json::Value>) {
    match node {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == key {
                    out.push(v);
                }
                collect_values(v, key, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_values(item, key, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_formats() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("0:59"), Some(59));
        assert_eq!(parse_duration("854M plays"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn upscales_thumbnail_urls() {
        assert_eq!(
            upscale_thumbnail("https://yt3.googleusercontent.com/abc=w60-h60-l90-rj"),
            "https://yt3.googleusercontent.com/abc=w544-h544-p-l90-rj"
        );
        assert_eq!(upscale_thumbnail("https://example.com/no-params"), "https://example.com/no-params");
    }

    fn shelf_item(video_id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": video_id },
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [{ "text": title, "navigationEndpoint": { "watchEndpoint": { "videoId": video_id } } }] } } },
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [
                            { "text": "Daft Punk" }, { "text": " • " },
                            { "text": "Discovery" }, { "text": " • " },
                            { "text": "5:21" }
                        ] } } }
                ],
                "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                    { "url": "https://yt3.googleusercontent.com/x=w60-h60-l90-rj" }
                ] } } }
            }
        })
    }

    #[test]
    fn parses_initial_search_shape_with_continuation() {
        let fixture = serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {
                                            "continuations": [{ "nextContinuationData": { "continuation": "TOK1" } }],
                                            "contents": [
                                                shelf_item("vid1", "One More Time"),
                                                shelf_item("vid2", "Aerodynamic")
                                            ]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });

        let page = parse_search_page(&fixture);
        assert_eq!(page.tracks.len(), 2);
        assert_eq!(page.next_continuation.as_deref(), Some("TOK1"));
        let t = &page.tracks[0];
        assert_eq!(t.video_id, "vid1");
        assert_eq!(t.title, "One More Time");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.album, "Discovery");
        assert_eq!(t.duration_secs, Some(321));
        assert!(t.thumbnail.as_deref().unwrap().contains("w544"));
    }

    #[test]
    fn parses_continuation_shape() {
        let fixture = serde_json::json!({
            "continuationContents": {
                "musicShelfContinuation": {
                    "continuations": [{ "nextContinuationData": { "continuation": "TOK2" } }],
                    "contents": [shelf_item("vid3", "Digital Love")]
                }
            }
        });
        let page = parse_search_page(&fixture);
        assert_eq!(page.tracks.len(), 1);
        assert_eq!(page.tracks[0].video_id, "vid3");
        assert_eq!(page.next_continuation.as_deref(), Some("TOK2"));
    }

    #[test]
    fn parse_skips_malformed_items() {
        let fixture = serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": { "tabs": [{
                    "tabRenderer": { "content": { "sectionListRenderer": { "contents": [{
                        "musicShelfRenderer": { "contents": [
                            { "musicResponsiveListItemRenderer": { "flexColumns": [] } },
                            { "garbage": true }
                        ]}
                    }]}}}
                    }]}
            }
        });
        assert!(parse_search_page(&fixture).tracks.is_empty());
    }

    #[test]
    fn parses_radio_response() {
        let fixture = serde_json::json!({
            "contents": {
                "singleColumnMusicWatchNextResultsRenderer": {
                    "tabbedRenderer": {
                        "watchNextTabbedResultsRenderer": {
                            "tabs": [{
                                "tabRenderer": {
                                    "content": {
                                        "musicQueueRenderer": {
                                            "content": {
                                                "playlistPanelRenderer": {
                                                    "contents": [{
                                                        "playlistPanelVideoRenderer": {
                                                            "videoId": "rel1",
                                                            "title": { "runs": [{ "text": "Get Lucky" }] },
                                                            "shortBylineText": { "runs": [{ "text": "Daft Punk" }] },
                                                            "lengthText": { "simpleText": "4:08" },
                                                            "thumbnail": { "thumbnails": [
                                                                { "url": "https://yt3.ggpht.com/y=w120-h120" }
                                                            ] }
                                                        }
                                                    }, {
                                                        "playlistPanelVideoRenderer": {
                                                            "videoId": "",
                                                            "title": { "runs": [{ "text": "broken" }] }
                                                        }
                                                    }]
                                                }
                                            }
                                        }
                                    }
                                }
                            }]
                        }
                    }
                }
            }
        });

        let tracks = parse_radio_response(&fixture);
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.video_id, "rel1");
        assert_eq!(t.title, "Get Lucky");
        assert_eq!(t.artist, "Daft Punk");
        assert_eq!(t.duration_secs, Some(248));
    }
}
