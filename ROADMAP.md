# MUSICO ONLINE — Roadmap: a clean-room Apple Music-class desktop player on free catalog

> **North star:** a production-grade desktop music app with the information architecture,
> polish, and intelligence of Apple Music — Home/Listen Now editorial feed, Browse genres,
> artist/album pages, radio, full-library search, Playing Next queue, downloads/offline, and
> taste-driven recommendations — running entirely on free sources (YouTube Music catalog via
> InnerTube ≈ 100M+ songs, streams via yt-dlp). No ads, no accounts, no tracking.
>
> This document is the plan of record. Each phase ends at a hard gate; do not advance with a
> red gate. Phases are sized so one focused session can land one or two chunks.

---

## 0. Definition of "production grade" (the acceptance bar for v1.0)

| Budget | Target | Measured how |
|---|---|---|
| Cold start → Home feed interactive | < 1.5 s (cached feed) / < 4 s (fresh fetch) | stopwatch instrumentation in CI smoke + manual |
| Click-to-sound | < 150 ms cached, < 4 s p95 fresh download | timestamp from click to `Playing` event |
| RAM | < 250 MB steady with 10k-row lists rendered | `/usr/bin/time -v` soak |
| Idle CPU | < 1.5% | soak |
| Stability | zero uncaught panics in an 8 h soak; every fallible path returns a user-visible error state | soak + error-injection tests |
| Offline behavior | app is fully usable offline (downloads, local library, cached art); network features show non-blocking empty states | airplane-mode test |
| Catalog scale | any search/browse page paginates indefinitely; no O(N) UI jank past 10k items | synthetic library test |
| Data hygiene | stream cache ≤ cap (1 GB default), art cache ≤ 200 MB, DB compacts | unit tests + storage manager UI |

Non-goals (deliberately out): accounts, cloud sync, social, lyrics licensing, video podcasts,
DRM content. Everything else in Apple Music's desktop IA is in scope or explicitly deferred to
§8.

---

## 1. Product spec — what "feels like Apple Music" means concretely

### 1.1 Window chrome & navigation IA (cloned from AM desktop)
- **Left sidebar** (fixed 240 px, collapsible): search field on top; sections
  - *Apple Music* → **Home (Listen Now)**, **Browse**, **Radio**
  - *Library* → **Recently Added**, **Artists**, **Albums**, **Songs**, **Made For You**, **Downloads**
  - *Playlists* → user playlists list
- **Top transport bar (the "LCD")**: back/forward history arrows, shuffle/repeat, prev/play/next,
  seek bar with time, volume; center LCD shows art + title/artist (click → Now Playing);
  right side: queue toggle, lyrics toggle, fullscreen.
- **Content area**: editorial shelves (hero rows, horizontal card carousels), detail pages
  (album/artist/playlist), list pages with sticky section headers (A–Z jump rail like AM).
- **Right queue panel** ("Playing Next"): slides in, shows history + upcoming, drag-free reorder
  via buttons, "Autoplay" toggle at the bottom (keeps radio-continuing).
- **Now Playing / fullscreen player**: our existing skeuomorphic vinyl stays (identity!), now
  with AM-style animated lyrics when lyrics exist.
- **Search**: full-page search with scope tabs (Top Results / Songs / Albums / Artists /
  Library / Downloads), live suggestions, recent searches.

### 1.2 Feature matrix — AM feature → our implementation → data source

| Apple Music feature | Our implementation | Source | Status |
|---|---|---|---|
| Listen Now shelves (For You, New Releases, Top Charts, mixes) | home feed shelves + locally-computed Made-For-You inserts | `browse FEmusic_home` (VERIFIED) + local engine | plan |
| Made For You (Favorites/Discovery/New mixes) | computed from taste profile + listen history (local v2 engine extended to `yt:` tracks) | local sled DB | plan |
| Browse (genres & moods grid) | 50 mood/genre cards → genre shelves | `browse FEmusic_moods_and_genres` (VERIFIED: 50 categories) | plan |
| Album page (tracklist, credits, more by artist) | detail page + related shelves | `browse MPREb_*` (VERIFIED: tracklists) | plan |
| Artist page (hero, top songs, discography) | detail page | `browse UC*` (VERIFIED: immersive header + shelves) | plan |
| Playlist pages | detail page | `browse VL*` / `RD*` (playlist shape shared w/ album) | plan |
| Radio (station per song/artist) | autoplay-continue, already v0.5 | `next RD<id>` (VERIFIED, shipped) | **shipped** |
| Search + suggestions | paged search (shipped) + scope tabs + suggest | `search` + continuations (VERIFIED, shipped); suggestions endpoint TO-VERIFY | partial |
| Playing Next queue | queue panel UI + autoplay toggle | local queue (shipped core) + radio | plan |
| Downloads / offline | download manager + downloaded library + offline mode | yt-dlp cache (shipped single-track) → batch + DB | partial |
| Library (added albums/songs) | "Add to Library" persisting catalog objects in sled | local DB | plan |
| Animated lyrics | fullscreen lyrics with karaoke highlight | sidecar `.lrc` (shipped local); YTM lyrics TO-VERIFY | partial |
| EQ / normalization | port from main app | local | shipped in main, port |
| Keyboard/a11y polish | AM-equivalent shortcuts | local | partial |

---

## 2. Data sources — inventory, verification status, risk

### 2.1 InnerTube (music.youtube.com/youtubei/v1, client `WEB_REMIX`, version auto-refreshed — shipped)
| Endpoint | Purpose | Status |
|---|---|---|
| `search` + continuations | songs/videos/albums/artists/playlists, paged | **VERIFIED + shipped** (both shapes) |
| `next` (`RD<id>`) | radio / related tracks (50 per fetch) | **VERIFIED + shipped** |
| `browse FEmusic_home` | editorial home shelves + mood chips | **VERIFIED 2026-10-28** (2+ carousel shelves, 11 chips) |
| `browse FEmusic_moods_and_genres` | 50 mood/genre categories w/ params | **VERIFIED 2026-10-28** (musicNavigationButtonRenderer) |
| `browse UC*` (artist) | hero, top tracks, discography shelves | **VERIFIED 2026-10-28** |
| `browse MPREb_*` (album) | tracklist | **VERIFIED 2026-10-28** |
| `browse VL*` (playlist) | playlist detail | shape shared with album — TO-VERIFY |
| `music/get_search_suggestions` | typeahead | TO-VERIFY |
| `music/get_related` | richer related panels | TO-VERIFY (fallback: `next` radio, shipped) |
| YTM lyrics payload (musixmatch-backed) | synced lyrics | TO-VERIFY (fallback: local `.lrc`, shipped) |

**Design rule:** the client never hardcodes page layouts — it follows
`navigationEndpoint.browseEndpoint {browseId, params}` found in items, exactly like the real
client. This makes shape drift survivable: new shelf types degrade to "unsupported shelf,
skipped" instead of breaking a page.

### 2.2 Streams — yt-dlp (shipped)
itag 140 m4a → disk cache → symphonia. Hardened in v0.5 (locks, retry, verify, LRU prune,
portable kill). Batch download = N sequential resolves at ≤2 concurrent, same machinery.

### 2.3 Risk register
| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| InnerTube response-shape drift | certain, ongoing | page parse breaks | navigation-following parser, per-shelf tolerance, `fix(innertube)` patches isolated in one module; live test (`MUSICO_LIVE_TESTS=1`) run before every release |
| PO-token / bot-check walls on some clients | medium | streams fail | yt-dlp already handles most; fallback chain of client identities; last resort: document + degrade gracefully |
| Rate limiting / IP throttle | medium | slow fetches | request coalescing, TTL caches (shipped for search), self-throttle (max 2 concurrent network ops), jittered backoff |
| Genre/home content geo-locked or empty | low | empty shelves | empty-state UI per shelf, never a blank page |
| yt-dlp breaks | certain over time | playback fails | surfaced install/update hint (shipped); version check on boot |
| Legal exposure | inherent | repo takedown | standalone repo, personal-use framing (Metrolist/InnerTune precedent); streaming code stays OUT of the offline Musico release repo |

---

## 3. Architecture

### 3.1 Module layout (Rust)
```
src-tauri/src/
  online/                    ← grows out of online.rs (Phase 0)
    mod.rs                   public surface used by commands/event_bridge
    client.rs                HTTP, client-version refresh, retry, throttle (single choke point)
    models.rs                OnlineTrack, Album, Artist, Playlist, Shelf, Section, Paged<T>
    parse.rs                 ALL response→model parsing (the only drift-sensitive file)
    catalog.rs               browse: home, genre, artist, album, playlist + navigation-following
    search.rs                search + suggestions (moves from online.rs)
    radio.rs                 next/RD radio (moves from online.rs)
    stream.rs                yt-dlp resolve/download/verify/prune + download manager (batch)
    artcache.rs              thumbnail→cover pipeline (shared cache, size tiers)
    store.rs                 sled trees for online state (see 3.2)
  library/                   ← Phase 4: "your library" of added catalog objects + downloads DB
  event_bridge.rs            unchanged shape + autoplay hooks (shipped) + download progress events
  commands.rs                thin IPC layer only — no logic
```
Threading contract (unchanged, enforced): network/yt-dlp ONLY on `spawn_blocking`; UI commands
return in <10 ms; everything long-running reports via events (`online-status`,
`download-progress`, `feed-updated`).

### 3.2 Sled schema (new trees in the existing DB)
| Tree | Key → Value | Purpose |
|---|---|---|
| `lib:albums` | albumId → CatalogAlbum (+added_at) | added-to-library albums |
| `lib:artists` | channelId → CatalogArtist | followed artists |
| `lib:tracks` | videoId → CatalogTrack (ref album, added_at, play counts) | added songs |
| `downloads` | videoId → DownloadRecord{state, bytes, path, quality, added_at} | download manager |
| `feed:home` | shelfId → cached shelf JSON (TTL in value) | offline home feed |
| `history:online` | ts:videoId → PlayEvent-like | taste signals for yt: tracks |
Recommender's existing trees (profile/likes/…) are reused as-is — `yt:` ids already flow
through affinity/likes; Phase 5 adds feature vectors for downloaded tracks via the background
indexer so the whole v2 engine works on online music.

### 3.3 Cache tiers
1. In-memory: parsed shelves/models, LRU 64 entries (search already does this).
2. Disk art: `~/.cache/musico-online/covers` (200 MB cap, LRU).
3. Disk streams: `~/.cache/musico-online/online` (1 GB cap default, LRU — shipped).
4. DB: library/history/feeds (sled).
**Decision (Phase 0): this app moves to its own data dirs** (`~/.local/share/musico-online`,
`~/.cache/musico-online`, `~/.config/musico-online`) + binary `musico-online` + app id
`com.aryan.musico-online`, so it coexists with the public Musico install instead of fighting
over the same DB. Migration: first run copies nothing (clean start is fine — listen history is
small; the public app keeps its own).

### 3.4 Frontend architecture (Phase 1 decision)
Keep **no-build vanilla ESM** (repo ethos) but replace ad-hoc IIFEs with a 3-file micro-kernel:
`store.js` (pub/sub state), `router.js` (hash routes + history stack for AM's back/forward),
`ui.js` (`h()`-style element builder + shelf/card components). If Phase 2's shelf variety
exceeds what this handles cleanly, vendor **Preact + htm via ESM** (still no bundler) — that is
the pre-agreed escalation, not a rewrite.

---

## 4. Recommendation strategy (matching Apple Music's *feel*)

Three layers, blended:
1. **Catalog intelligence (server-side, free):** YT's own related/radio (`next`, shipped) and
   home-feed ranking. Cheap, scale-proof, no local data needed.
2. **Personal engine (local, ours):** the v2 recommender (8 adaptive signals, online weight
   learning — shipped for local files) extended to online tracks:
   - downloaded tracks get feature vectors via the existing background indexer (analysis is
     already id-based — `yt:` ids just work);
   - listens/skips/likes on streamed (not downloaded) tracks feed affinity/artist/context/
     profile using metadata-only proxies (genre chip, artist, tempo from search payload when
     present) until a download provides real audio features;
   - "personal station" = taste-profile vector → seed → radio, re-ranked by profile cosine.
3. **Editorial layer:** home shelves from `FEmusic_home`, with **Made-For-You inserts** computed
   daily: *Favorites Mix* (top-affinity, cooldown-rotated), *Discovery Mix* (high
   profile-match × zero plays), *Time-of-Day Mix* (context buckets — already learn), *New
   Releases For You* (home feed ∩ followed artists). These are local queries + one catalog
   hydration, cheap enough to compute on app start.

Feel-target: every list AM personalizes, we personalize — from local signals, with zero
accounts.

---

## 5. Design system (Phase 1 deliverable)
- **Aesthetic:** AM-clean — generous whitespace, SF-style type scale (system font stack),
  12-col content grid, cards with 12 px radius + soft shadow; our palettes remain (Nebula
  default keeps brand identity; add `--accent-text` per palette, already exists).
- **Light + dark** (theme system shipped; AM-clean token pass over both).
- **Shelf components:** hero banner (2:1 art + overlay title), medium card (art + 2-line meta),
  wide list row (art + meta + actions), grid card (genre/mood).
- **Motion:** 200 ms standard easing; carousel scroll-snap; skeleton shimmer (shipped pattern);
  `prefers-reduced-motion` respected (shipped).
- **Density:** list virtualization (`IntersectionObserver` windowing) for 10k+ rows.

---

## 6. Phases & chunks (each chunk lands tested; gates are hard)

### Phase 0 — Groundwork (repo hygiene before visible work)
- **C0.1 App identity split**: binary `musico-online`, app id, own data dirs, desktop entry
  "Musico Online". Gate: both apps runnable side-by-side, `cargo test` green.
- **C0.2 Split `online.rs` → `online/` modules** (behavior-identical refactor, all 18 tests
  move with the code). Gate: zero test-count change, no warnings.
- **C0.3 `client.rs` choke point**: throttle (≤2 concurrent), jittered retry, single timeout
  policy, request log for debugging. Gate: unit tests with a mock transport.

### Phase 1 — Apple Music shell (visible foundation)
- **C1.1 Frontend micro-kernel** (`store/router/ui`), sidebar IA, top LCD transport, view
  router with history back/forward. Gate: 15+ browser assertions on nav/LCD.
- **C1.2 AM-clean theme tokens + typography + light/dark pass.** Gate: visual screenshot
  review at 3 widths; contrast ≥ 4.5:1.
- **C1.3 Playing Next queue panel + autoplay toggle.** Gate: queue e2e (add/remove/reorder,
  autoplay continues radio — already wired).

### Phase 2 — Catalog foundation (the "millions of albums" backbone) — ✅ COMPLETE
> Shipped: C2.1 (PR #13), C2.2 (PR #15), C2.4 (PR #17), C2.5 (PR #15+#17).
> C2.3 (artist page hero/top-songs/discography layout polish) is folded into the
> generic DetailView — a dedicated artist-layout pass lands with Phase 4's library
> pages. The C2.5 3-deep-navigation gate is satisfied by the existing suites:
> detail→detail→home back/forward (browser_detail_test), genre→detail→home
> (genres_test), ctx-menu navigation (browser_ctx_test).
- **C2.1 `catalog.rs` + `parse.rs`: home feed → shelves/models** (hero/medium/list renderers;
  unknown shelf types skipped). Gate: live home renders; live test fixture added.
- **C2.2 Album + playlist detail pages** (tracklist, play/shuffle-all, add-to-library).
  Gate: live album page e2e + parse fixtures.
- **C2.3 Artist page** (hero, top songs, discography carousel). Gate: same.
- **C2.4 Browse page**: mood/genre grid → genre shelf pages. Gate: 50 categories render.
- **C2.5 Navigation-following engine**: all in-app links route via captured
  `browseEndpoint`s; back/forward works across pages. Gate: 3-deep navigation e2e.

### Phase 3 — Playback parity — ✅ COMPLETE
> Shipped: C3.1 (PR #21), C3.2 (PR #22), C3.3 (PR #23). Gates: context-menu actions
> verified across home/search/detail/queue (browser_ctx_test 14), cached advance
> measured 238 ms (< 1 s budget, live test), download state machine + storage UI
> (browser_storage_test 9). Pre-download exposed and fixed a real transport bug
> (mid-body timeouts were fatal; now retryable, 20 s budget).
- **C3.1 Play-from-anywhere** (context menus: play next, play last, start radio).
- **C3.2 Pre-download next**: when a `yt:` track is within 1 of playing, resolve in background.
  Gate: measured gap between consecutive online tracks < 1 s (cached).
- **C3.3 Download manager (single tracks) + storage settings UI** (cap slider, clear button,
  per-download state events). Gate: pause/resume-by-retry semantics, progress UI assertions.

### Phase 4 — Library & downloads at scale
- **C4.1 Library DB + pages**: Recently Added / Albums / Artists / Songs (A–Z jump rail,
  virtualized lists). Gate: 10k-row synthetic test < 250 MB RAM.
- **C4.2 Batch downloads**: album/playlist download buttons, ≤2 concurrent, queue UI,
  retry-failed. Gate: 15-track album downloads unattended.
- **C4.3 Offline mode**: network-off switch, offline-filtered views, queued "download when
  online". Gate: airplane-mode e2e.
- **C4.4 Unified search** across downloads + library + online with scope tabs. Gate: merged
  result ranking test.

### Phase 5 — Listen Now intelligence
- **C5.1 Online listens → taste signals** (metadata proxies; history tree).
- **C5.2 Background audio analysis for downloads** → full v2 engine over `yt:` ids.
- **C5.3 Made-For-You mixes** (4 daily mixes, §4.3) rendered as home inserts.
- **C5.4 Personal station** (profile-seeded radio, re-ranked). Gate: relevance fixture tests
  (favorites mix ⊃ top-affinity tracks; discovery mix ∩ played = ∅).

### Phase 6 — Search & suggestions
- **C6.1** scope tabs + top results (blend: exact-title > artist match > popularity).
- **C6.2** suggestions endpoint (or n-gram over recent+library if endpoint fails verification)
  + recent searches UI. Gate: typeahead < 100 ms cached.

### Phase 7 — Production hardening & v1.0
- **C7.1 Error/empty/loading states audit** — every view × {loading, empty, error, offline}.
- **C7.2 Soak + budgets** — hit §0 table; fix regressions.
- **C7.3 Keyboard + a11y pass** (AM shortcut map + focus rings + screen-reader labels).
- **C7.4 Port EQ/normalization + MPRIS art for online** (mostly re-pointing existing code).
- **C7.5 Release v1.0-online**: version, release notes, live-test gate in CI (manual dispatch).

### Phase 8 — post-1.0 candidates (not blocking)
Synced YTM lyrics if the payload verifies · animated fullscreen lyrics · smart playlists ·
"year in review" stats page · queue drag-reorder · multiple windows.

---

## 7. Sequencing & dependencies
```
P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7   (P5 depends on P2+P3; P6 needs P2 search models;
                                          P4.4 needs P4.1; P7 last, always)
```
Each phase = 1–3 sessions. Phases 2–4 are where "millions of albums" becomes literally true
(browse anywhere → play anything); Phase 5 is where it starts feeling like *yours*.

## 8. Verification protocol (every session)
1. `cargo test --workspace` green (engine e2e needs `scripts/gen_test_tone.sh` first).
2. Live gate before pushing: `MUSICO_LIVE_TESTS=1 cargo test -p musico-online live_ -- --ignored`.
3. Browser assertion suite for any UI chunk (Playwright, mock Bridge for offline runs).
4. Isolated-HOME smoke run of the binary on any backend/identity change.
5. Release tags only from green CI; re-push tag alone if the workflow doesn't fire (known GitHub quirk).
