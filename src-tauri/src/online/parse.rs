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

// ---------------------------------------------------------------------------
// Browse pages (home feed, albums, artists, playlists) — Phase 2 catalog
// ---------------------------------------------------------------------------

use crate::online::models::{BrowsePage, HeaderMeta, ItemKind, Shelf, ShelfItem};

/// Parses a browse response into a page.  Shelf containers are collected
/// by name ANYWHERE in the tree (`musicCarouselShelfRenderer`,
/// `musicShelfRenderer`, `musicPlaylistShelfRenderer`) so container-path
/// drift between endpoints doesn't break the parse.  Unknown item renderers
/// are skipped.
pub(crate) fn parse_browse_page(json: &serde_json::Value) -> BrowsePage {
    let mut title = String::new();
    for header_key in [
        "musicResponsiveHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicDetailHeaderRenderer",
    ] {
        let mut headers = Vec::new();
        collect_values(json, header_key, &mut headers);
        if let Some(h) = headers.first() {
            let t = h
                .pointer("/title/runs")
                .and_then(|v| v.as_array())
                .map(|runs| {
                    runs.iter()
                        .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if !t.is_empty() {
                title = t;
                break;
            }
        }
    }

    // Single walk in document order — three separate key passes would group
    // shelves by type (all carousels, then lists) and scramble page layout.
    let mut containers = Vec::new();
    collect_shelves_in_order(json, &mut containers);
    let mut shelves = Vec::new();
    for c in containers {
        if let Some(shelf) = parse_shelf(c) {
            if !shelf.items.is_empty() {
                shelves.push(shelf);
            }
        }
    }

    let header = parse_header(json);
    let next_continuation = json
        .pointer("/continuationContents/sectionListContinuation/continuations/0/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    BrowsePage { title, shelves, next_continuation, header }
}

/// First browseEndpoint (browseId + params) found in a runs object.
fn find_first_browse_link(runs_obj: &serde_json::Value) -> Option<(String, Option<String>)> {
    let runs = runs_obj.get("runs").and_then(|v| v.as_array())?;
    runs.iter().find_map(|r| {
        let be = r.pointer("/navigationEndpoint/browseEndpoint")?;
        Some((
            be.get("browseId").and_then(|v| v.as_str())?.to_string(),
            be.get("params").and_then(|v| v.as_str()).map(str::to_string),
        ))
    })
}

/// Parses the detail-page hero block (whichever header renderer the page
/// uses): title, joined subtitle text, largest thumbnail, and the first
/// navigation link embedded in the subtitle runs (artist / owner link).
fn parse_header(json: &serde_json::Value) -> Option<HeaderMeta> {
    for header_key in [
        "musicResponsiveHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicDetailHeaderRenderer",
    ] {
        let mut headers = Vec::new();
        collect_values(json, header_key, &mut headers);
        let Some(h) = headers.first() else { continue };

        let title = h
            .pointer("/title/runs")
            .and_then(|v| v.as_array())
            .map(|runs| {
                runs.iter()
                    .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        // First subtitle block: joined text + first navigation link.  (On
        // album pages the artist link often lives in straplineTextOne, not
        // the subtitle — scan both, subtitle text wins for display.)
        let mut subtitles = Vec::new();
        collect_values(h, "subtitle", &mut subtitles);
        let mut straplines = Vec::new();
        collect_values(h, "straplineTextOne", &mut straplines);
        let nav = straplines
            .first()
            .and_then(|sub| find_first_browse_link(sub))
            .or_else(|| subtitles.first().and_then(|sub| find_first_browse_link(sub)));
        let (subtitle, _unused_nav) = match subtitles.first() {
            Some(sub) => {
                let runs = sub.get("runs").and_then(|v| v.as_array());
                let text = runs
                    .map(|rs| {
                        rs.iter()
                            .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                (text, ())
            }
            None => (String::new(), ()),
        };

        let thumbnail = h
            .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
            .and_then(|v| v.as_array())
            .and_then(|thumbs| thumbs.last())
            .and_then(|t| t.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| upscale_thumbnail(u));

        return Some(HeaderMeta {
            title,
            subtitle,
            thumbnail,
            navigate_browse_id: nav.as_ref().map(|(id, _)| id.clone()),
            navigate_params: nav.as_ref().and_then(|(_, p)| p.clone()),
        });
    }
    None
}

/// Parses the mood/genre landing page: `musicNavigationButtonRenderer`
/// items are category cards (title + browseId + params), not shelf content —
/// they need their own pass.
pub(crate) fn parse_genre_page(json: &serde_json::Value) -> Vec<ShelfItem> {
    let mut buttons = Vec::new();
    collect_values(json, "musicNavigationButtonRenderer", &mut buttons);
    let mut out = Vec::new();
    for b in buttons {
        let title = b
            .pointer("/buttonText/runs/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let browse_id = b
            .pointer("/clickCommand/browseEndpoint/browseId")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let params = b
            .pointer("/clickCommand/browseEndpoint/params")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if title.is_empty() || browse_id.is_none() {
            continue;
        }
        out.push(ShelfItem {
            kind: crate::online::models::ItemKind::Unknown,
            title,
            subtitle: String::new(),
            thumbnail: None,
            video_id: None,
            browse_id,
            params,
            playlist_id: None,
        });
    }
    out
}

/// Collects every shelf container in document order.  Does NOT recurse into
/// a matched container (nested shelves would duplicate as separate sections).
fn collect_shelves_in_order<'a>(node: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    const KEYS: [&str; 3] = [
        "musicCarouselShelfRenderer",
        "musicShelfRenderer",
        "musicPlaylistShelfRenderer",
    ];
    match node {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if KEYS.contains(&k.as_str()) {
                    out.push(v);
                    continue;
                }
                collect_shelves_in_order(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_shelves_in_order(item, out);
            }
        }
        _ => {}
    }
}

/// Parses one shelf container: header title + mixed card/row contents.
fn parse_shelf(container: &serde_json::Value) -> Option<Shelf> {
    let mut title = String::new();
    let mut header_runs = Vec::new();
    collect_values(
        container.get("header").unwrap_or(&serde_json::Value::Null),
        "runs",
        &mut header_runs,
    );
    if let Some(runs) = header_runs.first() {
        title = runs
            .as_array()
            .map(|rs| {
                rs.iter()
                    .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
    }

    let mut items = Vec::new();
    let mut two_row = Vec::new();
    let mut list_row = Vec::new();
    collect_values(container.get("contents").unwrap_or(&serde_json::Value::Null), "musicTwoRowItemRenderer", &mut two_row);
    collect_values(container.get("contents").unwrap_or(&serde_json::Value::Null), "musicResponsiveListItemRenderer", &mut list_row);
    for it in two_row {
        let item = parse_two_row_item(it);
        if item.title.is_empty() {
            continue; // blank clickable cards are worse than missing ones
        }
        items.push(item);
    }
    for it in list_row {
        // List rows carry their own overlay play targets; map the generic
        // list-item parse into a ShelfItem (song/album/artist by nav shape).
        if let Some(track) = parse_list_item(it) {
            let nav_browse = it
                .pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/browseEndpoint")
                .or_else(|| it.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/browseEndpoint"));
            let browse_id = nav_browse
                .and_then(|b| b.get("browseId"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let params = nav_browse
                .and_then(|b| b.get("params"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let kind = ItemKind::from_ids(browse_id.as_deref(), Some(&track.video_id));
            items.push(ShelfItem {
                kind,
                title: track.title,
                subtitle: format!("{} {}", track.artist, track.album).trim().to_string(),
                thumbnail: track.thumbnail,
                video_id: Some(track.video_id),
                browse_id,
                params,
                playlist_id: None,
            });
        }
    }

    if title.is_empty() && items.is_empty() {
        return None;
    }
    Some(Shelf { title, items })
}

/// Parses one `musicTwoRowItemRenderer` — the standard editorial card
/// (album / playlist / artist / single covers on home and browse pages).
fn parse_two_row_item(renderer: &serde_json::Value) -> ShelfItem {
    let title = runs_text(renderer.get("title").unwrap_or(&serde_json::Value::Null));
    let subtitle = runs_text(renderer.get("subtitle").unwrap_or(&serde_json::Value::Null));

    let nav = renderer.get("navigationEndpoint").unwrap_or(&serde_json::Value::Null);
    let browse_id = nav
        .pointer("/browseEndpoint/browseId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let params = nav
        .pointer("/browseEndpoint/params")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let video_id = nav
        .pointer("/watchEndpoint/videoId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let playlist_id = nav
        .pointer("/watchEndpoint/playlistId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            browse_id
                .as_ref()
                .filter(|b| b.starts_with("VL"))
                .map(|b| b.trim_start_matches("VL").to_string())
        });

    let thumbnail = renderer
        .pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|thumbs| thumbs.last())
        .and_then(|t| t.get("url"))
        .and_then(|u| u.as_str())
        .map(|u| upscale_thumbnail(u));

    ShelfItem {
        kind: ItemKind::from_ids(browse_id.as_deref(), video_id.as_deref()),
        title,
        subtitle,
        thumbnail,
        video_id,
        browse_id,
        params,
        playlist_id,
    }
}

#[cfg(test)]
mod browse_tests {
    use super::*;

    /// Raw-string fixtures — literal wire JSON, immune to macro brace-counting.
    const HOME_PAGE: &str = r#"{
      "contents": {
        "singleColumnBrowseResultsRenderer": {
          "tabs": [{
            "tabRenderer": {
              "content": {
                "sectionListRenderer": {
                  "contents": [
                    {
                      "musicCarouselShelfRenderer": {
                        "header": {
                          "musicCarouselShelfHeaderRenderer": {
                            "title": { "runs": [{ "text": "Late Night Lounge" }] }
                          }
                        },
                        "contents": [
                          { "musicTwoRowItemRenderer": {
                              "title": { "runs": [{ "text": "Chill Mix" }] },
                              "subtitle": { "runs": [{ "text": "Your daily mix" }] },
                              "navigationEndpoint": { "browseEndpoint": { "browseId": "VLPLabc123", "params": "ggMEgA" } },
                              "thumbnailRenderer": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                                { "url": "https://yt3.googleusercontent.com/cover=w60-h60-l90-rj" } ] } } }
                          } },
                          { "musicTwoRowItemRenderer": {
                              "title": { "runs": [{ "text": "Discovery Mix" }] },
                              "subtitle": { "runs": [{ "text": "New music for you" }] },
                              "navigationEndpoint": { "browseEndpoint": { "browseId": "VLPLdef456", "params": "ggMEgA" } },
                              "thumbnailRenderer": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                                { "url": "https://yt3.googleusercontent.com/cover2=w60-h60-l90-rj" } ] } } }
                          } }
                        ]
                      }
                    },
                    {
                      "musicCarouselShelfRenderer": {
                        "header": {
                          "musicCarouselShelfHeaderRenderer": {
                            "title": { "runs": [{ "text": "Trending" }] }
                          }
                        },
                        "contents": [
                          { "musicTwoRowItemRenderer": {
                              "title": { "runs": [{ "text": "Top Hits" }] },
                              "subtitle": { "runs": [{ "text": "Various Artists" }] },
                              "navigationEndpoint": { "browseEndpoint": { "browseId": "MPREb_album1" } },
                              "thumbnailRenderer": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                                { "url": "https://yt3.googleusercontent.com/cover3=w60-h60-l90-rj" } ] } } }
                          } }
                        ]
                      }
                    }
                  ]
                }
              }
            }
          }]
        }
      }
    }"#;

    #[test]
    fn parses_home_carousel_shelves() {
        let json: serde_json::Value = serde_json::from_str(HOME_PAGE).unwrap();
        let page = parse_browse_page(&json);
        assert_eq!(page.shelves.len(), 2);
        assert_eq!(page.shelves[0].title, "Late Night Lounge");
        assert_eq!(page.shelves[0].items.len(), 2);
        let item = &page.shelves[0].items[0];
        assert_eq!(item.kind, crate::online::models::ItemKind::Playlist);
        assert_eq!(item.title, "Chill Mix");
        assert_eq!(item.subtitle, "Your daily mix");
        assert_eq!(item.browse_id.as_deref(), Some("VLPLabc123"));
        assert!(item.thumbnail.as_deref().unwrap().contains("w544"));
        assert_eq!(page.shelves[1].items[0].kind, crate::online::models::ItemKind::Album);
        assert!(item.to_track().is_none(), "playlist cards are not directly playable");
    }

    #[test]
    fn classifies_item_kinds_by_id_shape() {
        use crate::online::models::ItemKind;
        assert_eq!(ItemKind::from_ids(Some("MPREb_x"), None), ItemKind::Album);
        assert_eq!(ItemKind::from_ids(Some("VLPLx"), None), ItemKind::Playlist);
        assert_eq!(ItemKind::from_ids(Some("PLx"), None), ItemKind::Playlist);
        assert_eq!(ItemKind::from_ids(Some("UCxyz"), None), ItemKind::Artist);
        assert_eq!(ItemKind::from_ids(None, Some("dQw4w9WgXcQ")), ItemKind::Song);
        assert_eq!(ItemKind::from_ids(None, None), ItemKind::Unknown);
    }

    #[test]
    fn playable_items_convert_to_tracks() {
        let json: serde_json::Value = serde_json::from_str(r#"{
 "title": {
  "runs": [
   {
    "text": "One More Time"
   }
  ]
 },
 "subtitle": {
  "runs": [
   {
    "text": "Daft Punk"
   }
  ]
 },
 "navigationEndpoint": {
  "watchEndpoint": {
   "videoId": "wU26xVT_vBU",
   "playlistId": "RDAMVMwU26xVT_vBU"
  }
 },
 "thumbnailRenderer": {
  "musicThumbnailRenderer": {
   "thumbnail": {
    "thumbnails": [
     {
      "url": "https://yt3.googleusercontent.com/x=w60-h60-l90-rj"
     }
    ]
   }
  }
 }
}"#).unwrap();
        let item = parse_two_row_item(&json);
        assert_eq!(item.kind, crate::online::models::ItemKind::Song);
        assert_eq!(item.playlist_id.as_deref(), Some("RDAMVMwU26xVT_vBU"));
        let track = item.to_track().expect("song converts");
        assert_eq!(track.video_id, "wU26xVT_vBU");
        assert_eq!(track.artist, "Daft Punk");
    }

    #[test]
    fn shelf_order_follows_document_order_across_types() {
        // musicShelf FIRST, then two carousels — output must keep that order
        // (three key-grouped passes would emit carousels before the list).
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "musicShelfRenderer": {
           "contents": [
            {
             "musicResponsiveListItemRenderer": {
              "playlistItemData": {
               "videoId": "q1"
              },
              "flexColumns": [
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "Quick pick",
                    "navigationEndpoint": {
                     "watchEndpoint": {
                      "videoId": "q1"
                     }
                    }
                   }
                  ]
                 }
                }
               }
              ]
             }
            }
           ]
          }
         },
         {
          "musicCarouselShelfRenderer": {
           "header": {
            "musicCarouselShelfHeaderRenderer": {
             "title": {
              "runs": [
               {
                "text": "Carousel A"
               }
              ]
             }
            }
           },
           "contents": [
            {
             "musicTwoRowItemRenderer": {
              "title": {
               "runs": [
                {
                 "text": "A1"
                }
               ]
              },
              "navigationEndpoint": {
               "browseEndpoint": {
                "browseId": "VLPLa1"
               }
              },
              "subtitle": {
               "runs": [
                {
                 "text": "subtitle"
                }
               ]
              }
             }
            }
           ]
          }
         },
         {
          "musicCarouselShelfRenderer": {
           "header": {
            "musicCarouselShelfHeaderRenderer": {
             "title": {
              "runs": [
               {
                "text": "Carousel B"
               }
              ]
             }
            }
           },
           "contents": [
            {
             "musicTwoRowItemRenderer": {
              "title": {
               "runs": [
                {
                 "text": "B1"
                }
               ]
              },
              "navigationEndpoint": {
               "browseEndpoint": {
                "browseId": "VLPLb1"
               }
              },
              "subtitle": {
               "runs": [
                {
                 "text": "subtitle"
                }
               ]
              }
             }
            }
           ]
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 }
}"#).unwrap();
        let page = parse_browse_page(&json);
        let titles: Vec<&str> = page.shelves.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["", "Carousel A", "Carousel B"],
            "document order preserved: quick-picks list first");
    }

    #[test]
    fn empty_title_cards_are_skipped() {
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "musicCarouselShelfRenderer": {
           "header": {
            "musicCarouselShelfHeaderRenderer": {
             "title": {
              "runs": [
               {
                "text": "Shelf"
               }
              ]
             }
            }
           },
           "contents": [
            {
             "musicTwoRowItemRenderer": {
              "navigationEndpoint": {
               "browseEndpoint": {
                "browseId": "VLPLx"
               }
              }
             }
            },
            {
             "musicTwoRowItemRenderer": {
              "title": {
               "runs": [
                {
                 "text": "Real Card"
                }
               ]
              },
              "navigationEndpoint": {
               "browseEndpoint": {
                "browseId": "VLPLy"
               }
              },
              "subtitle": {
               "runs": [
                {
                 "text": "subtitle"
                }
               ]
              }
             }
            }
           ]
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 }
}"#).unwrap();
        let page = parse_browse_page(&json);
        assert_eq!(page.shelves.len(), 1);
        assert_eq!(page.shelves[0].items.len(), 1, "blank card skipped");
        assert_eq!(page.shelves[0].items[0].title, "Real Card");
    }

    #[test]
    fn skips_empty_and_garbage_shelves() {
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "garbage": true
         },
         {
          "musicCarouselShelfRenderer": {
           "contents": []
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 }
}"#).unwrap();
        let page = parse_browse_page(&json);
        assert!(page.shelves.is_empty());
    }

    #[test]
    fn parses_page_header_title_and_song_rows() {
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "musicShelfRenderer": {
           "contents": [
            {
             "musicResponsiveListItemRenderer": {
              "playlistItemData": {
               "videoId": "v1"
              },
              "flexColumns": [
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "A Song",
                    "navigationEndpoint": {
                     "watchEndpoint": {
                      "videoId": "v1"
                     }
                    }
                   }
                  ]
                 }
                }
               }
              ]
             }
            }
           ]
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 },
 "header": {
  "musicImmersiveHeaderRenderer": {
   "title": {
    "runs": [
     {
      "text": "Daft Punk"
     }
    ]
   }
  }
 }
}"#).unwrap();
        let page = parse_browse_page(&json);
        assert_eq!(page.title, "Daft Punk");
        assert_eq!(page.shelves.len(), 1);
        assert_eq!(page.shelves[0].items[0].kind, crate::online::models::ItemKind::Song);
        assert!(page.shelves[0].items[0].to_track().is_some());
    }

    #[test]
    fn parses_genre_category_buttons() {
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "musicNavigationButtonRenderer": {
           "buttonText": {
            "runs": [
             {
              "text": "Chill"
             }
            ]
           },
           "clickCommand": {
            "browseEndpoint": {
             "browseId": "FEmusic_moods_and_genres_category",
             "params": "ggSUChAK"
            }
           }
          }
         },
         {
          "musicNavigationButtonRenderer": {
           "buttonText": {
            "runs": [
             {
              "text": "Workout"
             }
            ]
           },
           "clickCommand": {
            "browseEndpoint": {
             "browseId": "FEmusic_moods_and_genres_category",
             "params": "ggUQChB"
            }
           }
          }
         },
         {
          "musicNavigationButtonRenderer": {
           "buttonText": {
            "runs": []
           },
           "clickCommand": {
            "browseEndpoint": {
             "browseId": "X"
            }
           }
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 }
}"#).unwrap();
        let items = parse_genre_page(&json);
        assert_eq!(items.len(), 2, "empty-title buttons skipped");
        assert_eq!(items[0].title, "Chill");
        assert_eq!(items[0].browse_id.as_deref(), Some("FEmusic_moods_and_genres_category"));
        assert_eq!(items[0].params.as_deref(), Some("ggSUChAK"));
    }

#[test]
    fn parses_album_header_and_playable_tracks() {
        // Fixture generated from the live album page captured 2026-10-29
        // (musicResponsiveHeaderRenderer + playlistItemData tracklist).
        let json: serde_json::Value = serde_json::from_str(r#"{
 "contents": {
  "singleColumnBrowseResultsRenderer": {
   "tabs": [
    {
     "tabRenderer": {
      "content": {
       "sectionListRenderer": {
        "contents": [
         {
          "musicShelfRenderer": {
           "contents": [
            {
             "musicResponsiveListItemRenderer": {
              "trackingParams": "CLcBEMn0AhgAIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
              "overlay": {
               "musicItemThumbnailOverlayRenderer": {
                "background": {
                 "verticalGradient": {
                  "gradientLayerColors": [
                   "0",
                   "0"
                  ]
                 }
                },
                "content": {
                 "musicPlayButtonRenderer": {
                  "playNavigationEndpoint": {
                   "clickTrackingParams": "CMoBEMjeAiITCLyz6f333ZcDFfMngwMdUK4YkcoBBMFIUPU=",
                   "watchEndpoint": {
                    "videoId": "fujCuMpi4vw",
                    "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
                    "index": 0,
                    "playerParams": "ygYQQjcyQ0UxMTJCQTUwRDUyNg%3D%3D",
                    "playlistSetVideoId": "B72CE112BA50D526",
                    "loggingContext": {
                     "vssLoggingContext": {
                      "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
                     }
                    },
                    "watchEndpointMusicSupportedConfigs": {
                     "watchEndpointMusicConfig": {
                      "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                     }
                    }
                   }
                  },
                  "trackingParams": "CMoBEMjeAiITCLyz6f333ZcDFfMngwMdUK4YkQ==",
                  "playIcon": {
                   "iconType": "PLAY_ARROW"
                  },
                  "pauseIcon": {
                   "iconType": "PAUSE"
                  },
                  "iconColor": 4294967295,
                  "backgroundColor": 0,
                  "activeBackgroundColor": 0,
                  "loadingIndicatorColor": 14745645,
                  "playingIcon": {
                   "iconType": "VOLUME_UP"
                  },
                  "iconLoadingColor": 0,
                  "activeScaleFactor": 1,
                  "buttonSize": "MUSIC_PLAY_BUTTON_SIZE_SMALL",
                  "rippleTarget": "MUSIC_PLAY_BUTTON_RIPPLE_TARGET_SELF",
                  "accessibilityPlayData": {
                   "accessibilityData": {
                    "label": "Play This Land (The Lion King) (Piano & Violin Version) - Laraland"
                   }
                  },
                  "accessibilityPauseData": {
                   "accessibilityData": {
                    "label": "Pause This Land (The Lion King) (Piano & Violin Version) - Laraland"
                   }
                  }
                 }
                },
                "contentPosition": "MUSIC_ITEM_THUMBNAIL_OVERLAY_CONTENT_POSITION_CENTERED",
                "displayStyle": "MUSIC_ITEM_THUMBNAIL_OVERLAY_DISPLAY_STYLE_PERSISTENT"
               }
              },
              "flexColumns": [
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "This Land (The Lion King) (Piano & Violin Version)",
                    "navigationEndpoint": {
                     "clickTrackingParams": "CLcBEMn0AhgAIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                     "watchEndpoint": {
                      "videoId": "fujCuMpi4vw",
                      "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
                      "loggingContext": {
                       "vssLoggingContext": {
                        "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
                       }
                      },
                      "watchEndpointMusicSupportedConfigs": {
                       "watchEndpointMusicConfig": {
                        "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                       }
                      }
                     }
                    }
                   }
                  ]
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               },
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {},
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               },
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "485 plays"
                   }
                  ],
                  "accessibility": {
                   "accessibilityData": {
                    "label": "485 plays"
                   }
                  }
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               }
              ],
              "fixedColumns": [
               {
                "musicResponsiveListItemFixedColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "3:42"
                   }
                  ],
                  "accessibility": {
                   "accessibilityData": {
                    "label": "3 minutes, 42 seconds"
                   }
                  }
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH",
                 "size": "MUSIC_RESPONSIVE_LIST_ITEM_FIXED_COLUMN_SIZE_SMALL"
                }
               }
              ],
              "menu": {
               "menuRenderer": {
                "items": [
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Start mix"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "MIX"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CMkBEJvzBRgAIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "watchEndpoint": {
                     "videoId": "fujCuMpi4vw",
                     "playlistId": "RDAMVMfujCuMpi4vw",
                     "params": "wAEB",
                     "loggingContext": {
                      "vssLoggingContext": {
                       "serializedContextData": "GhFSREFNVk1mdWpDdU1waTR2dw%3D%3D"
                      }
                     },
                     "watchEndpointMusicSupportedConfigs": {
                      "watchEndpointMusicConfig": {
                       "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                      }
                     }
                    }
                   },
                   "trackingParams": "CMkBEJvzBRgAIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuServiceItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Play next"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "QUEUE_PLAY_NEXT"
                   },
                   "serviceEndpoint": {
                    "clickTrackingParams": "CMcBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "queueAddEndpoint": {
                     "queueTarget": {
                      "videoId": "fujCuMpi4vw",
                      "onEmptyQueue": {
                       "clickTrackingParams": "CMcBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "watchEndpoint": {
                        "videoId": "fujCuMpi4vw"
                       }
                      }
                     },
                     "queueInsertPosition": "INSERT_AFTER_CURRENT_VIDEO",
                     "commands": [
                      {
                       "clickTrackingParams": "CMcBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "addToToastAction": {
                        "item": {
                         "notificationTextRenderer": {
                          "successResponseText": {
                           "runs": [
                            {
                             "text": "Song will play next"
                            }
                           ]
                          },
                          "trackingParams": "CMgBEMrHAyITCLyz6f333ZcDFfMngwMdUK4YkQ=="
                         }
                        }
                       }
                      }
                     ]
                    }
                   },
                   "trackingParams": "CMcBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuServiceItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Add to queue"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ADD_TO_REMOTE_QUEUE"
                   },
                   "serviceEndpoint": {
                    "clickTrackingParams": "CMUBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "queueAddEndpoint": {
                     "queueTarget": {
                      "videoId": "fujCuMpi4vw",
                      "onEmptyQueue": {
                       "clickTrackingParams": "CMUBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "watchEndpoint": {
                        "videoId": "fujCuMpi4vw"
                       }
                      }
                     },
                     "queueInsertPosition": "INSERT_AT_END",
                     "commands": [
                      {
                       "clickTrackingParams": "CMUBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "addToToastAction": {
                        "item": {
                         "notificationTextRenderer": {
                          "successResponseText": {
                           "runs": [
                            {
                             "text": "Song added to queue"
                            }
                           ]
                          },
                          "trackingParams": "CMYBEMrHAyITCLyz6f333ZcDFfMngwMdUK4YkQ=="
                         }
                        }
                       }
                      }
                     ]
                    }
                   },
                   "trackingParams": "CMUBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "toggleMenuServiceItemRenderer": {
                   "defaultText": {
                    "runs": [
                     {
                      "text": "Save to library"
                     }
                    ]
                   },
                   "defaultIcon": {
                    "iconType": "BOOKMARK_BORDER"
                   },
                   "defaultServiceEndpoint": {
                    "clickTrackingParams": "CMMBEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Save this for later"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Save favorites to your library after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CMQBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CMQBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "toggledText": {
                    "runs": [
                     {
                      "text": "Remove from library"
                     }
                    ]
                   },
                   "toggledIcon": {
                    "iconType": "BOOKMARK"
                   },
                   "toggledServiceEndpoint": {
                    "clickTrackingParams": "CMMBEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "feedbackEndpoint": {
                     "feedbackToken": "AB9zfpKU8lPp2RF9nguHUXDHeqotZm8VYiOLcv4Dr4m91WLmZcQd9p1BR73_aJgAX00AgL1c1LCNzlYI9TWuhvg_O6Cn7U_JTg"
                    }
                   },
                   "trackingParams": "CMMBEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                   "isToggled": false
                  }
                 },
                 {
                  "menuServiceItemDownloadRenderer": {
                   "serviceEndpoint": {
                    "clickTrackingParams": "CMIBENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "offlineVideoEndpoint": {
                     "videoId": "fujCuMpi4vw",
                     "onAddCommand": {
                      "clickTrackingParams": "CMIBENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                      "getDownloadActionCommand": {
                       "videoId": "fujCuMpi4vw",
                       "params": "CAI%3D"
                      }
                     }
                    }
                   },
                   "trackingParams": "CMIBENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                   "badgeIcon": {
                    "iconType": "PREMIUM_STANDALONE_CAIRO"
                   }
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Save to playlist"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ADD_TO_PLAYLIST"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CMABEMOUBhgFIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Save this for later"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Make playlists and share them after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CMEBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CMEBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "trackingParams": "CMABEMOUBhgFIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Go to artist"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ARTIST"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CL8BEJD7BRgGIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "browseEndpoint": {
                     "browseId": "UCms9PSwf7v4YNcHM_P8AYcw",
                     "browseEndpointContextSupportedConfigs": {
                      "browseEndpointContextMusicConfig": {
                       "pageType": "MUSIC_PAGE_TYPE_ARTIST"
                      }
                     }
                    }
                   },
                   "trackingParams": "CL8BEJD7BRgGIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "View song credits"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "PEOPLE_GROUP"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CL4BEK-jChgHIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "browseEndpoint": {
                     "browseId": "MPTCfujCuMpi4vw",
                     "browseEndpointContextSupportedConfigs": {
                      "browseEndpointContextMusicConfig": {
                       "pageType": "MUSIC_PAGE_TYPE_TRACK_CREDITS"
                      }
                     }
                    }
                   },
                   "trackingParams": "CL4BEK-jChgHIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Share"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "SHARE"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CL0BEJH7BRgIIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "shareEntityEndpoint": {
                     "serializedShareEntity": "CgtmdWpDdU1waTR2dw%3D%3D",
                     "sharePanelType": "SHARE_PANEL_TYPE_UNIFIED_SHARE_PANEL"
                    }
                   },
                   "trackingParams": "CL0BEJH7BRgIIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 }
                ],
                "trackingParams": "CLkBEKc7IhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                "topLevelButtons": [
                 {
                  "likeButtonRenderer": {
                   "target": {
                    "videoId": "fujCuMpi4vw"
                   },
                   "likeStatus": "INDIFFERENT",
                   "trackingParams": "CLoBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJE=",
                   "likesAllowed": true,
                   "dislikeNavigationEndpoint": {
                    "clickTrackingParams": "CLoBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Not a fan?"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Improve your recommendations after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CLwBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CLwBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "likeCommand": {
                    "clickTrackingParams": "CLoBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Like this song"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Improve recommendations and save music after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CLsBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CLsBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   }
                  }
                 }
                ],
                "accessibility": {
                 "accessibilityData": {
                  "label": "Action menu"
                 }
                }
               }
              },
              "playlistItemData": {
               "playlistSetVideoId": "B72CE112BA50D526",
               "videoId": "fujCuMpi4vw"
              },
              "itemHeight": "MUSIC_RESPONSIVE_LIST_ITEM_HEIGHT_MEDIUM",
              "index": {
               "runs": [
                {
                 "text": "1"
                }
               ]
              },
              "multiSelectCheckbox": {
               "checkboxRenderer": {
                "onSelectionChangeCommand": {
                 "clickTrackingParams": "CLgBEL6-CSITCLyz6f333ZcDFfMngwMdUK4YkcoBBMFIUPU=",
                 "updateMultiSelectStateCommand": {
                  "multiSelectParams": "CAMSKU9MQUs1dXlfbXkwMEtnQWJhcWlCNUpZb0dHYnd1R016N0RnYk0wZXVZ",
                  "multiSelectItem": "Ch8KC2Z1akN1TXBpNHZ3EhBCNzJDRTExMkJBNTBENTI2"
                 }
                },
                "checkedState": "CHECKBOX_CHECKED_STATE_UNCHECKED",
                "trackingParams": "CLgBEL6-CSITCLyz6f333ZcDFfMngwMdUK4YkQ=="
               }
              }
             }
            },
            {
             "musicResponsiveListItemRenderer": {
              "trackingParams": "CKMBEMn0AhgBIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
              "overlay": {
               "musicItemThumbnailOverlayRenderer": {
                "background": {
                 "verticalGradient": {
                  "gradientLayerColors": [
                   "0",
                   "0"
                  ]
                 }
                },
                "content": {
                 "musicPlayButtonRenderer": {
                  "playNavigationEndpoint": {
                   "clickTrackingParams": "CLYBEMjeAiITCLyz6f333ZcDFfMngwMdUK4YkcoBBMFIUPU=",
                   "watchEndpoint": {
                    "videoId": "dfB4COTGt7A",
                    "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
                    "index": 1,
                    "playerParams": "ygYQNDcxMjcyNTc1QjU0QTg1Qw%3D%3D",
                    "playlistSetVideoId": "471272575B54A85C",
                    "loggingContext": {
                     "vssLoggingContext": {
                      "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
                     }
                    },
                    "watchEndpointMusicSupportedConfigs": {
                     "watchEndpointMusicConfig": {
                      "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                     }
                    }
                   }
                  },
                  "trackingParams": "CLYBEMjeAiITCLyz6f333ZcDFfMngwMdUK4YkQ==",
                  "playIcon": {
                   "iconType": "PLAY_ARROW"
                  },
                  "pauseIcon": {
                   "iconType": "PAUSE"
                  },
                  "iconColor": 4294967295,
                  "backgroundColor": 0,
                  "activeBackgroundColor": 0,
                  "loadingIndicatorColor": 14745645,
                  "playingIcon": {
                   "iconType": "VOLUME_UP"
                  },
                  "iconLoadingColor": 0,
                  "activeScaleFactor": 1,
                  "buttonSize": "MUSIC_PLAY_BUTTON_SIZE_SMALL",
                  "rippleTarget": "MUSIC_PLAY_BUTTON_RIPPLE_TARGET_SELF",
                  "accessibilityPlayData": {
                   "accessibilityData": {
                    "label": "Play Building a Family (Life as a House) (Piano & Violin Version) - Laraland"
                   }
                  },
                  "accessibilityPauseData": {
                   "accessibilityData": {
                    "label": "Pause Building a Family (Life as a House) (Piano & Violin Version) - Laraland"
                   }
                  }
                 }
                },
                "contentPosition": "MUSIC_ITEM_THUMBNAIL_OVERLAY_CONTENT_POSITION_CENTERED",
                "displayStyle": "MUSIC_ITEM_THUMBNAIL_OVERLAY_DISPLAY_STYLE_PERSISTENT"
               }
              },
              "flexColumns": [
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "Building a Family (Life as a House) (Piano & Violin Version)",
                    "navigationEndpoint": {
                     "clickTrackingParams": "CKMBEMn0AhgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                     "watchEndpoint": {
                      "videoId": "dfB4COTGt7A",
                      "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
                      "loggingContext": {
                       "vssLoggingContext": {
                        "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
                       }
                      },
                      "watchEndpointMusicSupportedConfigs": {
                       "watchEndpointMusicConfig": {
                        "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                       }
                      }
                     }
                    }
                   }
                  ]
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               },
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {},
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               },
               {
                "musicResponsiveListItemFlexColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "254 plays"
                   }
                  ],
                  "accessibility": {
                   "accessibilityData": {
                    "label": "254 plays"
                   }
                  }
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH"
                }
               }
              ],
              "fixedColumns": [
               {
                "musicResponsiveListItemFixedColumnRenderer": {
                 "text": {
                  "runs": [
                   {
                    "text": "1:54"
                   }
                  ],
                  "accessibility": {
                   "accessibilityData": {
                    "label": "1 minute, 54 seconds"
                   }
                  }
                 },
                 "displayPriority": "MUSIC_RESPONSIVE_LIST_ITEM_COLUMN_DISPLAY_PRIORITY_HIGH",
                 "size": "MUSIC_RESPONSIVE_LIST_ITEM_FIXED_COLUMN_SIZE_SMALL"
                }
               }
              ],
              "menu": {
               "menuRenderer": {
                "items": [
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Start mix"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "MIX"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CLUBEJvzBRgAIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "watchEndpoint": {
                     "videoId": "dfB4COTGt7A",
                     "playlistId": "RDAMVMdfB4COTGt7A",
                     "params": "wAEB",
                     "loggingContext": {
                      "vssLoggingContext": {
                       "serializedContextData": "GhFSREFNVk1kZkI0Q09UR3Q3QQ%3D%3D"
                      }
                     },
                     "watchEndpointMusicSupportedConfigs": {
                      "watchEndpointMusicConfig": {
                       "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"
                      }
                     }
                    }
                   },
                   "trackingParams": "CLUBEJvzBRgAIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuServiceItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Play next"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "QUEUE_PLAY_NEXT"
                   },
                   "serviceEndpoint": {
                    "clickTrackingParams": "CLMBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "queueAddEndpoint": {
                     "queueTarget": {
                      "videoId": "dfB4COTGt7A",
                      "onEmptyQueue": {
                       "clickTrackingParams": "CLMBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "watchEndpoint": {
                        "videoId": "dfB4COTGt7A"
                       }
                      }
                     },
                     "queueInsertPosition": "INSERT_AFTER_CURRENT_VIDEO",
                     "commands": [
                      {
                       "clickTrackingParams": "CLMBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "addToToastAction": {
                        "item": {
                         "notificationTextRenderer": {
                          "successResponseText": {
                           "runs": [
                            {
                             "text": "Song will play next"
                            }
                           ]
                          },
                          "trackingParams": "CLQBEMrHAyITCLyz6f333ZcDFfMngwMdUK4YkQ=="
                         }
                        }
                       }
                      }
                     ]
                    }
                   },
                   "trackingParams": "CLMBEL7uBRgBIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuServiceItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Add to queue"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ADD_TO_REMOTE_QUEUE"
                   },
                   "serviceEndpoint": {
                    "clickTrackingParams": "CLEBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "queueAddEndpoint": {
                     "queueTarget": {
                      "videoId": "dfB4COTGt7A",
                      "onEmptyQueue": {
                       "clickTrackingParams": "CLEBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "watchEndpoint": {
                        "videoId": "dfB4COTGt7A"
                       }
                      }
                     },
                     "queueInsertPosition": "INSERT_AT_END",
                     "commands": [
                      {
                       "clickTrackingParams": "CLEBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                       "addToToastAction": {
                        "item": {
                         "notificationTextRenderer": {
                          "successResponseText": {
                           "runs": [
                            {
                             "text": "Song added to queue"
                            }
                           ]
                          },
                          "trackingParams": "CLIBEMrHAyITCLyz6f333ZcDFfMngwMdUK4YkQ=="
                         }
                        }
                       }
                      }
                     ]
                    }
                   },
                   "trackingParams": "CLEBEPvvBRgCIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "toggleMenuServiceItemRenderer": {
                   "defaultText": {
                    "runs": [
                     {
                      "text": "Save to library"
                     }
                    ]
                   },
                   "defaultIcon": {
                    "iconType": "BOOKMARK_BORDER"
                   },
                   "defaultServiceEndpoint": {
                    "clickTrackingParams": "CK8BEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Save this for later"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Save favorites to your library after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CLABEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CLABEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "toggledText": {
                    "runs": [
                     {
                      "text": "Remove from library"
                     }
                    ]
                   },
                   "toggledIcon": {
                    "iconType": "BOOKMARK"
                   },
                   "toggledServiceEndpoint": {
                    "clickTrackingParams": "CK8BEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "feedbackEndpoint": {
                     "feedbackToken": "AB9zfpKpxKjVQux0zetpXXY7dq410-vA2UwMV29KQK7fazUsieBpZbz59apjKVeBG9zSvqExRmZHIPGQb375DFanlKYC2M2MZQ"
                    }
                   },
                   "trackingParams": "CK8BEIT_BRgDIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                   "isToggled": false
                  }
                 },
                 {
                  "menuServiceItemDownloadRenderer": {
                   "serviceEndpoint": {
                    "clickTrackingParams": "CK4BENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "offlineVideoEndpoint": {
                     "videoId": "dfB4COTGt7A",
                     "onAddCommand": {
                      "clickTrackingParams": "CK4BENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                      "getDownloadActionCommand": {
                       "videoId": "dfB4COTGt7A",
                       "params": "CAI%3D"
                      }
                     }
                    }
                   },
                   "trackingParams": "CK4BENGqBRgEIhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                   "badgeIcon": {
                    "iconType": "PREMIUM_STANDALONE_CAIRO"
                   }
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Save to playlist"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ADD_TO_PLAYLIST"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CKwBEMOUBhgFIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Save this for later"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Make playlists and share them after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CK0BEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CK0BEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "trackingParams": "CKwBEMOUBhgFIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Go to artist"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "ARTIST"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CKsBEJD7BRgGIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "browseEndpoint": {
                     "browseId": "UCms9PSwf7v4YNcHM_P8AYcw",
                     "browseEndpointContextSupportedConfigs": {
                      "browseEndpointContextMusicConfig": {
                       "pageType": "MUSIC_PAGE_TYPE_ARTIST"
                      }
                     }
                    }
                   },
                   "trackingParams": "CKsBEJD7BRgGIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "View song credits"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "PEOPLE_GROUP"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CKoBEK-jChgHIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "browseEndpoint": {
                     "browseId": "MPTCdfB4COTGt7A",
                     "browseEndpointContextSupportedConfigs": {
                      "browseEndpointContextMusicConfig": {
                       "pageType": "MUSIC_PAGE_TYPE_TRACK_CREDITS"
                      }
                     }
                    }
                   },
                   "trackingParams": "CKoBEK-jChgHIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 },
                 {
                  "menuNavigationItemRenderer": {
                   "text": {
                    "runs": [
                     {
                      "text": "Share"
                     }
                    ]
                   },
                   "icon": {
                    "iconType": "SHARE"
                   },
                   "navigationEndpoint": {
                    "clickTrackingParams": "CKkBEJH7BRgIIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                    "shareEntityEndpoint": {
                     "serializedShareEntity": "CgtkZkI0Q09UR3Q3QQ%3D%3D",
                     "sharePanelType": "SHARE_PANEL_TYPE_UNIFIED_SHARE_PANEL"
                    }
                   },
                   "trackingParams": "CKkBEJH7BRgIIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                  }
                 }
                ],
                "trackingParams": "CKUBEKc7IhMIvLPp_ffdlwMV8yeDAx1QrhiR",
                "topLevelButtons": [
                 {
                  "likeButtonRenderer": {
                   "target": {
                    "videoId": "dfB4COTGt7A"
                   },
                   "likeStatus": "INDIFFERENT",
                   "trackingParams": "CKYBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJE=",
                   "likesAllowed": true,
                   "dislikeNavigationEndpoint": {
                    "clickTrackingParams": "CKYBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Not a fan?"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Improve your recommendations after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CKgBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CKgBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   },
                   "likeCommand": {
                    "clickTrackingParams": "CKYBEKVBGAkiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
                    "modalEndpoint": {
                     "modal": {
                      "modalWithTitleAndButtonRenderer": {
                       "title": {
                        "runs": [
                         {
                          "text": "Like this song"
                         }
                        ]
                       },
                       "content": {
                        "runs": [
                         {
                          "text": "Improve recommendations and save music after signing in"
                         }
                        ]
                       },
                       "button": {
                        "buttonRenderer": {
                         "style": "STYLE_BLUE_TEXT",
                         "isDisabled": false,
                         "text": {
                          "runs": [
                           {
                            "text": "Sign in"
                           }
                          ]
                         },
                         "navigationEndpoint": {
                          "clickTrackingParams": "CKcBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiRygEEwUhQ9Q==",
                          "signInEndpoint": {
                           "hack": true
                          }
                         },
                         "trackingParams": "CKcBEPBbIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
                        }
                       }
                      }
                     }
                    }
                   }
                  }
                 }
                ],
                "accessibility": {
                 "accessibilityData": {
                  "label": "Action menu"
                 }
                }
               }
              },
              "playlistItemData": {
               "playlistSetVideoId": "471272575B54A85C",
               "videoId": "dfB4COTGt7A"
              },
              "itemHeight": "MUSIC_RESPONSIVE_LIST_ITEM_HEIGHT_MEDIUM",
              "index": {
               "runs": [
                {
                 "text": "2"
                }
               ]
              },
              "multiSelectCheckbox": {
               "checkboxRenderer": {
                "onSelectionChangeCommand": {
                 "clickTrackingParams": "CKQBEL6-CSITCLyz6f333ZcDFfMngwMdUK4YkcoBBMFIUPU=",
                 "updateMultiSelectStateCommand": {
                  "multiSelectParams": "CAMSKU9MQUs1dXlfbXkwMEtnQWJhcWlCNUpZb0dHYnd1R016N0RnYk0wZXVZ",
                  "multiSelectItem": "Ch8KC2RmQjRDT1RHdDdBEhA0NzEyNzI1NzVCNTRBODVD"
                 }
                },
                "checkedState": "CHECKBOX_CHECKED_STATE_UNCHECKED",
                "trackingParams": "CKQBEL6-CSITCLyz6f333ZcDFfMngwMdUK4YkQ=="
               }
              }
             }
            }
           ]
          }
         }
        ]
       }
      }
     }
    }
   ]
  }
 },
 "header": {
  "musicResponsiveHeaderRenderer": {
   "thumbnail": {
    "musicThumbnailRenderer": {
     "thumbnail": {
      "thumbnails": [
       {
        "url": "https://yt3.googleusercontent.com/K8K_pVSIRF1XLq7rUVU5rCjtptRzr11z5CNVJaBWBPKQbabKQqNvhDyY6IC6WRWHvNQ9JQ01-1vBunOEcg=w60-h60-l90-rj",
        "width": 60,
        "height": 60
       },
       {
        "url": "https://yt3.googleusercontent.com/K8K_pVSIRF1XLq7rUVU5rCjtptRzr11z5CNVJaBWBPKQbabKQqNvhDyY6IC6WRWHvNQ9JQ01-1vBunOEcg=w120-h120-l90-rj",
        "width": 120,
        "height": 120
       },
       {
        "url": "https://yt3.googleusercontent.com/K8K_pVSIRF1XLq7rUVU5rCjtptRzr11z5CNVJaBWBPKQbabKQqNvhDyY6IC6WRWHvNQ9JQ01-1vBunOEcg=w226-h226-l90-rj",
        "width": 226,
        "height": 226
       },
       {
        "url": "https://yt3.googleusercontent.com/K8K_pVSIRF1XLq7rUVU5rCjtptRzr11z5CNVJaBWBPKQbabKQqNvhDyY6IC6WRWHvNQ9JQ01-1vBunOEcg=w544-h544-l90-rj",
        "width": 544,
        "height": 544
       }
      ]
     },
     "thumbnailCrop": "MUSIC_THUMBNAIL_CROP_UNSPECIFIED",
     "thumbnailScale": "MUSIC_THUMBNAIL_SCALE_UNSPECIFIED",
     "trackingParams": "CBQQhL8CIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
    }
   },
   "buttons": [
    {
     "toggleButtonRenderer": {
      "isToggled": false,
      "isDisabled": false,
      "defaultIcon": {
       "iconType": "BOOKMARK_BORDER"
      },
      "toggledIcon": {
       "iconType": "BOOKMARK"
      },
      "trackingParams": "CBIQmE0YACITCLyz6f333ZcDFfMngwMdUK4YkQ==",
      "defaultNavigationEndpoint": {
       "clickTrackingParams": "CBIQmE0YACITCLyz6f333ZcDFfMngwMdUK4YkcoBBMFIUPU=",
       "modalEndpoint": {
        "modal": {
         "modalWithTitleAndButtonRenderer": {
          "title": {
           "runs": [
            {
             "text": "Save this for later"
            }
           ]
          },
          "content": {
           "runs": [
            {
             "text": "Save favorites to your library after signing in"
            }
           ]
          },
          "button": {
           "buttonRenderer": {
            "style": "STYLE_BLUE_TEXT",
            "isDisabled": false,
            "text": {
             "runs": [
              {
               "text": "Sign in"
              }
             ]
            },
            "navigationEndpoint": {
             "clickTrackingParams": "CBMQ8FsiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
             "signInEndpoint": {
              "hack": true
             }
            },
            "trackingParams": "CBMQ8FsiEwi8s-n9992XAxXzJ4MDHVCuGJE="
           }
          }
         }
        }
       }
      },
      "accessibilityData": {
       "accessibilityData": {
        "label": "Save to library"
       }
      },
      "toggledAccessibilityData": {
       "accessibilityData": {
        "label": "Remove from library"
       }
      }
     }
    },
    {
     "musicPlayButtonRenderer": {
      "playNavigationEndpoint": {
       "clickTrackingParams": "CBEQyN4CGAEiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
       "watchPlaylistEndpoint": {
        "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY"
       }
      },
      "trackingParams": "CBEQyN4CGAEiEwi8s-n9992XAxXzJ4MDHVCuGJE=",
      "playIcon": {
       "iconType": "PLAY_ARROW"
      },
      "pauseIcon": {
       "iconType": "PAUSE"
      },
      "iconColor": 4278387459,
      "backgroundColor": 0,
      "activeBackgroundColor": 0,
      "loadingIndicatorColor": 14745645,
      "playingIcon": {
       "iconType": "PAUSE"
      },
      "iconLoadingColor": 0,
      "activeScaleFactor": 1,
      "accessibilityPlayData": {
       "accessibilityData": {
        "label": "Play Versiones para Piano y Viol\u00edn (Vol. I)"
       }
      },
      "accessibilityPauseData": {
       "accessibilityData": {
        "label": "Pause Versiones para Piano y Viol\u00edn (Vol. I)"
       }
      }
     }
    },
    {
     "menuRenderer": {
      "items": [
       {
        "menuNavigationItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Shuffle play"
           }
          ]
         },
         "icon": {
          "iconType": "MUSIC_SHUFFLE"
         },
         "navigationEndpoint": {
          "clickTrackingParams": "CBAQmvMFGAAiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "watchPlaylistEndpoint": {
           "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
           "params": "wAEB8gECKAE%3D"
          }
         },
         "trackingParams": "CBAQmvMFGAAiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuNavigationItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Start mix"
           }
          ]
         },
         "icon": {
          "iconType": "MIX"
         },
         "navigationEndpoint": {
          "clickTrackingParams": "CA8Qm_MFGAEiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "watchPlaylistEndpoint": {
           "playlistId": "RDAMPLOLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
           "params": "wAEB"
          }
         },
         "trackingParams": "CA8Qm_MFGAEiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuServiceItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Play next"
           }
          ]
         },
         "icon": {
          "iconType": "QUEUE_PLAY_NEXT"
         },
         "serviceEndpoint": {
          "clickTrackingParams": "CA0Qvu4FGAIiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "queueAddEndpoint": {
           "queueTarget": {
            "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
            "onEmptyQueue": {
             "clickTrackingParams": "CA0Qvu4FGAIiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
             "watchEndpoint": {
              "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
              "loggingContext": {
               "vssLoggingContext": {
                "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
               }
              }
             }
            }
           },
           "queueInsertPosition": "INSERT_AFTER_CURRENT_VIDEO",
           "commands": [
            {
             "clickTrackingParams": "CA0Qvu4FGAIiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
             "addToToastAction": {
              "item": {
               "notificationTextRenderer": {
                "successResponseText": {
                 "runs": [
                  {
                   "text": "Album will play next"
                  }
                 ]
                },
                "trackingParams": "CA4QyscDIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
               }
              }
             }
            }
           ]
          }
         },
         "trackingParams": "CA0Qvu4FGAIiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuServiceItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Add to queue"
           }
          ]
         },
         "icon": {
          "iconType": "ADD_TO_REMOTE_QUEUE"
         },
         "serviceEndpoint": {
          "clickTrackingParams": "CAsQ--8FGAMiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "queueAddEndpoint": {
           "queueTarget": {
            "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
            "onEmptyQueue": {
             "clickTrackingParams": "CAsQ--8FGAMiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
             "watchEndpoint": {
              "playlistId": "OLAK5uy_my00KgAbaqiB5JYoGGbwuGMz7DgbM0euY",
              "loggingContext": {
               "vssLoggingContext": {
                "serializedContextData": "GilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D"
               }
              }
             }
            }
           },
           "queueInsertPosition": "INSERT_AT_END",
           "commands": [
            {
             "clickTrackingParams": "CAsQ--8FGAMiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
             "addToToastAction": {
              "item": {
               "notificationTextRenderer": {
                "successResponseText": {
                 "runs": [
                  {
                   "text": "Album added to queue"
                  }
                 ]
                },
                "trackingParams": "CAwQyscDIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
               }
              }
             }
            }
           ]
          }
         },
         "trackingParams": "CAsQ--8FGAMiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuNavigationItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Save to playlist"
           }
          ]
         },
         "icon": {
          "iconType": "ADD_TO_PLAYLIST"
         },
         "navigationEndpoint": {
          "clickTrackingParams": "CAkQw5QGGAQiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "modalEndpoint": {
           "modal": {
            "modalWithTitleAndButtonRenderer": {
             "title": {
              "runs": [
               {
                "text": "Save this for later"
               }
              ]
             },
             "content": {
              "runs": [
               {
                "text": "Make playlists and share them after signing in"
               }
              ]
             },
             "button": {
              "buttonRenderer": {
               "style": "STYLE_BLUE_TEXT",
               "isDisabled": false,
               "text": {
                "runs": [
                 {
                  "text": "Sign in"
                 }
                ]
               },
               "navigationEndpoint": {
                "clickTrackingParams": "CAoQ8FsiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
                "signInEndpoint": {
                 "hack": true
                }
               },
               "trackingParams": "CAoQ8FsiEwi8s-n9992XAxXzJ4MDHVCuGJE="
              }
             }
            }
           }
          }
         },
         "trackingParams": "CAkQw5QGGAQiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuNavigationItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Go to artist"
           }
          ]
         },
         "icon": {
          "iconType": "ARTIST"
         },
         "navigationEndpoint": {
          "clickTrackingParams": "CAgQkPsFGAUiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "browseEndpoint": {
           "browseId": "UCms9PSwf7v4YNcHM_P8AYcw",
           "browseEndpointContextSupportedConfigs": {
            "browseEndpointContextMusicConfig": {
             "pageType": "MUSIC_PAGE_TYPE_ARTIST"
            }
           }
          }
         },
         "trackingParams": "CAgQkPsFGAUiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       },
       {
        "menuNavigationItemRenderer": {
         "text": {
          "runs": [
           {
            "text": "Share"
           }
          ]
         },
         "icon": {
          "iconType": "SHARE"
         },
         "navigationEndpoint": {
          "clickTrackingParams": "CAcQkfsFGAYiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
          "shareEntityEndpoint": {
           "serializedShareEntity": "EilPTEFLNXV5X215MDBLZ0FiYXFpQjVKWW9HR2J3dUdNejdEZ2JNMGV1WQ%3D%3D",
           "sharePanelType": "SHARE_PANEL_TYPE_UNIFIED_SHARE_PANEL"
          }
         },
         "trackingParams": "CAcQkfsFGAYiEwi8s-n9992XAxXzJ4MDHVCuGJE="
        }
       }
      ],
      "trackingParams": "CAYQpzsYAiITCLyz6f333ZcDFfMngwMdUK4YkQ==",
      "accessibility": {
       "accessibilityData": {
        "label": "Action menu"
       }
      }
     }
    }
   ],
   "title": {
    "runs": [
     {
      "text": "Versiones para Piano y Viol\u00edn (Vol. I)"
     }
    ]
   },
   "subtitle": {
    "runs": [
     {
      "text": "Album"
     },
     {
      "text": " \u2022 "
     },
     {
      "text": "2026"
     }
    ]
   },
   "trackingParams": "CAQQneEIGAAiEwi8s-n9992XAxXzJ4MDHVCuGJE=",
   "straplineTextOne": {
    "runs": [
     {
      "text": "Laraland",
      "navigationEndpoint": {
       "clickTrackingParams": "CAQQneEIGAAiEwi8s-n9992XAxXzJ4MDHVCuGJHKAQTBSFD1",
       "browseEndpoint": {
        "browseId": "UCms9PSwf7v4YNcHM_P8AYcw",
        "browseEndpointContextSupportedConfigs": {
         "browseEndpointContextMusicConfig": {
          "pageType": "MUSIC_PAGE_TYPE_ARTIST"
         }
        }
       }
      }
     }
    ]
   },
   "straplineThumbnail": {
    "musicThumbnailRenderer": {
     "thumbnail": {
      "thumbnails": [
       {
        "url": "https://yt3.googleusercontent.com/YTmSmviqdsqMyVhmXsyYaBIOtd8pBXy1hgslVa1f9aG2AdWzsmKbhNEgWz5EzH9i0JTzVo_DVw=w60-h60-l90-rj",
        "width": 60,
        "height": 60
       },
       {
        "url": "https://yt3.googleusercontent.com/YTmSmviqdsqMyVhmXsyYaBIOtd8pBXy1hgslVa1f9aG2AdWzsmKbhNEgWz5EzH9i0JTzVo_DVw=w120-h120-l90-rj",
        "width": 120,
        "height": 120
       },
       {
        "url": "https://yt3.googleusercontent.com/YTmSmviqdsqMyVhmXsyYaBIOtd8pBXy1hgslVa1f9aG2AdWzsmKbhNEgWz5EzH9i0JTzVo_DVw=w226-h226-l90-rj",
        "width": 226,
        "height": 226
       },
       {
        "url": "https://yt3.googleusercontent.com/YTmSmviqdsqMyVhmXsyYaBIOtd8pBXy1hgslVa1f9aG2AdWzsmKbhNEgWz5EzH9i0JTzVo_DVw=w544-h544-l90-rj",
        "width": 544,
        "height": 544
       }
      ]
     },
     "thumbnailCrop": "MUSIC_THUMBNAIL_CROP_UNSPECIFIED",
     "thumbnailScale": "MUSIC_THUMBNAIL_SCALE_UNSPECIFIED",
     "trackingParams": "CAUQhL8CIhMIvLPp_ffdlwMV8yeDAx1QrhiR"
    }
   },
   "secondSubtitle": {
    "runs": [
     {
      "text": "9 songs"
     },
     {
      "text": " \u2022 "
     },
     {
      "text": "24 minutes"
     }
    ]
   }
  }
 }
}"#).unwrap();
        let page = parse_browse_page(&json);
        let hdr = page.header.expect("album header parses");
        assert!(!hdr.title.is_empty(), "header title present");
        assert!(hdr.subtitle.contains("Album"), "subtitle describes the album: {}", hdr.subtitle);
        assert!(hdr.thumbnail.is_some(), "header art present");
        assert_eq!(page.title, hdr.title, "page title mirrors header");
        let mut playable = 0;
        for shelf in &page.shelves {
            for item in &shelf.items {
                if item.video_id.is_some() {
                    playable += 1;
                }
            }
        }
        assert!(playable >= 2, "track rows are playable, got {playable}");
    }
}
