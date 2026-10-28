# Musico Online

**Private companion repo to [Musico](https://github.com/ARYAN007H/Musico)** — adds free online
music playback on top of the offline player. Kept private because the streaming layer is
ToS-gray-area scraping (same technique as Metrolist / InnerTune / RiMusic), and that code should
never ship in the public release.

## Robustness & capability notes (v2 polish, 2026-10-28)

- **Auto-refreshed client version** — `client_version()` scrapes the live
  `INNERTUBE_CLIENT_VERSION` from the music.youtube.com homepage (24 h TTL,
  stale-value + constant fallbacks), so searches don't die of a frozen version stamp.
- **Search pagination** — `online_search` returns `{tracks, next_continuation}`;
  the Browse screen gets a "Load more" button. Both initial and continuation
  response shapes are parsed.
- **Online radio** — `radio_tracks()` uses the InnerTube `next` endpoint
  (`RD<videoId>` playlist) for related tracks. When a `yt:` song ends with an
  empty queue, the advance policy auto-continues via radio on a worker thread
  (never blocks the event loop; local-library fallback on failure). A 12-entry
  ring buffer avoids repeating recent picks.
- **Download hardening** — per-video locks (no duplicate concurrent downloads),
  one retry on transient failures, size verification (≥10 KB), hard timeout.
- **Metadata polish** — duration probed from the downloaded file via symphonia
  (fixes the seek bar when search metadata lacks it); thumbnails upscaled to
  544 px and written into the shared cover cache, so `get_cover`, the vinyl
  label and MPRIS art all work for online songs.
- **Cache hygiene** — 1 GB LRU prune (mtime-based, `MUSICO_ONLINE_CACHE_MB`
  env override), hysteresis target 80%.
- **Search UX** — live debounced search, `/` shortcut, Songs/Videos pill,
  search-history chips (localStorage), per-row add-to-queue (background
  resolve), cached badges.

## What's here

Everything from the main app (Tauri shell, playback engine, recommender v2) plus an online layer:

- **InnerTube search** (`src-tauri/src/online.rs`) — talks to `music.youtube.com/youtubei/v1/search`
  with the `WEB_REMIX` client identity, Songs/Videos filters, tolerant parsing of
  `musicResponsiveListItemRenderer` items (title, videoId, artist, album, duration, thumbnails).
- **Stream resolution via yt-dlp** — resolves the AAC `m4a` variant (itag 140, falls back to
  best-audio), downloads into `~/.cache/musico/online/{videoId}.m4a`, then hands the file to the
  **existing offline playback engine** (symphonia) — no streaming plumbing, no new audio path.
  Cached files replay instantly; hard timeout kills wedged downloads.
- **Browse home screen** — search-first home view (`frontend/js/online.js`, `css/online.css`)
  with skeleton shimmer, quick-pick chips, and click-to-play rows. Now Playing shows a compact
  gradient/thumbnail art square.
- **Fullscreen-only vinyl** — the skeuomorphic vinyl record appears exclusively in the expanded
  player (expand button in Now Playing, `Esc` or ✕ to close); normal mode hides it.

## Requirements

- yt-dlp on `$PATH` (`pacman -S yt-dlp`). The Browse screen warns when it's missing.
- Network access to `music.youtube.com` (search) and `youtube.com` (stream).

## Commands added

| Command | Purpose |
|---|---|
| `online_search(query, videos?)` | InnerTube search → `Vec<OnlineTrack>` |
| `online_play(track)` | resolve + download + play through the normal queue path |
| `online_available()` | yt-dlp presence check |
| `online_clear_cache()` | wipe downloaded streams, returns bytes freed |

Events: `online-status` (`resolving` / `playing` / `error`).

## Know-fragile bits

- InnerTube is undocumented; a YouTube response-shape change can break parsing —
  `online.rs` logs a warning when a search parses to 0 results. Client version constant:
  `INNERTUBE_VERSION`.
- yt-dlp itself needs frequent updates when YouTube changes stream extraction.

## Build

```sh
cargo build -p musico-tauri
cargo test --workspace   # 19 tests incl. live audio-device e2e
```
