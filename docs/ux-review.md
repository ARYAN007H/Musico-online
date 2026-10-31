# Musico Online — UX review vs Apple Music & Spotify (2026-10-31)

End-to-end walkthrough of the current build (post events-fix, single-player
rule, Suggested/Queue/Lyrics in the expanded player). Each finding: what AM/Spotify
do, where we stand, and the concrete fix. Ordered by impact for a first-time user.

## What already matches the big players

- **Shell IA** — sidebar (Home/Search/Radio/Library/Playlists), top back/forward
  arrows, persistent transport, Playing Next panel: the AM skeleton is right.
- **Catalog depth** — home feed, genre/mood grid, artist/album/playlist detail,
  radio from any song: the "millions of albums" backbone works and follows
  real navigation endpoints, which makes shape drift survivable.
- **Player plumbing** — click-to-sound (<1 s cached), pre-resolve, radio
  autoplay continuation, downloads with a storage cap. The hard part is done.

## P0 — first-session killers (fix before showing anyone)

1. **Empty first run is a dead end.** With no library and no history, Home
   shows "Tuning your feed…" then a sparse feed; Songs/Made-for-you are empty.
   AM/Spotify never show an empty screen — they seed. **Fix:** first-run
   onboarding: one "pick your vibe" row (3 genre chips → searches → saves
   seeds); put the YTM editorial charts (`FEmusic_charts`) on Home as a
   guaranteed-nonempty shelf.
2. **Search has no suggestions.** Typing shows nothing until you finish and
   hit results; AM/Spotify typeahead after 2 chars. **Fix:** InnerTube
   `music/get_search_suggestions` (endpoint already flagged TO-VERIFY in the
   roadmap — same two-step pattern as lyrics, likely a `browse`-style call),
   fall back to n-grams over recent searches + library.
3. **Playlists are read-only.** No create, no add-to-playlist anywhere
   (the "+" in the expanded player only queues). **Fix (P1):** local sled
   playlists (`lib:playlists` tree) with add-from-any-context-menu; YTM cloud
   playlists later.
4. **Recently-played rows errored** ("missing field file_path") — fixed this
   session: online entries replay via the catalog path, local ones need
   `file_path` in the history entry.

## P1 — the gap that makes it feel "not finished"

5. **Now Playing richness.** Shipped this session: Suggested (auto-filled
   related tracks), Queue tab, Lyrics button. Still missing vs AM:
   - **Synced lyrics** with karaoke highlight (YTM exposes unsynced text
     only — options: LRCLIB.net free API has synced LRC for most catalog;
     match by artist+title+duration. This is the single highest-impact
     player upgrade.)
   - **Up-next reordering / jump-to** in the queue tab (store ops exist).
   - **Artist link from the player** — tapping the artist should navigate to
     the artist page (data already in trackMap entries).
6. **Volume pill in the expanded player.** There is no volume control when
   the big player is open (AM has the slider; Spotify the speaker popover).
   Add a compact slider row under the transport.
7. **Like/discoverability of the recommender.** Liking is one star icon; AM
   shows "Made for You" improving visibly. Our taste engine is strong but
   invisible — surface "Because you liked X" shelf headers on Home.
8. **Downloads UX.** Storage settings live in Settings; Spotify surfaces
   per-album download state inline. Show a small download icon on
   detail-page tracks (state machine events already exist).

## P2 — polish that closes the "feels cheap" gap

9. **Transitions.** View switches are instant swaps; AM cross-fades 200 ms.
   Add opacity/transform transitions respecting `prefers-reduced-motion`.
10. **Skeletons** exist for search; add them to Home/detail retries.
11. **Keyboard** — Space/n/p work; add `/` focus search, `l` like,
    `q` toggle queue (AM parity sheet exists in ROADMAP §C7.3).
12. **Window title / taskbar media integration** — MPRIS works; add the
    track title to the window title.

## Chrome proposal (your pill/card idea — my counter)

Your idea: pill on Home only; side card (mini dock) elsewhere with like /
add-to-playlist; card hides when the big player opens.

My recommendation — **one persistent surface, richer, everywhere** (this is
what AM and Spotify both do, and it's what makes them feel stable):

- Keep the bottom pill on ALL pages (users learn its position; hiding it per
  page makes the app feel inconsistent).
- Kill the bottom-left mini dock entirely — it duplicates the pill and is the
  reason the screen feels crowded. Move its one unique feature (the vinyl,
  which is our identity) into the expanded player, where it already lives.
- Add to the pill the two things your expanded card wanted: like is already
  there; add "+" (add-to-playlist menu) between like and expand.
- The expanded player hides the pill (already shipped) and is opened by
  clicking the LCD or the expand button.

Why not pill-only-on-Home: on Search/Library you constantly skip/queue while
browsing; losing the transport there is the single most-missed feature in
every web player clone. If you want the Home screen to feel calmer, the
cheaper win is a slightly shorter pill (56 px) with more margins on Home.

## Common-person feature requests to expect (roadmap fit)

| They will ask for… | Effort | Where it lands |
|---|---|---|
| "Make a playlist" | M | P1 #3 (sled playlists) |
| "Show the words while it plays" | M | LRCLIB synced lyrics (P1 #5) |
| "Play that song I heard" | S | search-from-lyrics via LRCLIB search |
| "Sleep timer" | S | engine `stop_after` + countdown event |
| "Crossfade between songs" | already | engine crossfade — verify UI toggle |
| "Why this song?" /电台 naming | S | radio chips: "Started from X" (exists) |
| "Night mode / accent colors" | already | palette system — surface 3 presets |
| "Minimize to tray" | M | tauri tray plugin (desktop parity) |
