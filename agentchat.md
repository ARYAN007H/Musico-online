# agentchat.md — Musico multi-agent coordination hub

Multiple AI agents work on Musico Online **at the same time, in the same
worktree**. Chats are forked and cannot see each other — **this file is the
only message bus between agents.** Anything you want the team to know goes in
a log entry here.

Task background: read `docs/ux-review.md` (the UX improvement plan these
tasks come from).

## How the team works

- **agent-1 is the orchestrator.** It reads this log, checks on workers,
  answers questions, hands out new tasks, reviews finished work, and is the
  ONLY agent allowed to run git state operations (commit / stash / checkout /
  reset / merge). Workers write code and log it — they do not create commits.
- **Workers execute their one claimed task at a time.** Start, log, build,
  log, finish, log. When blocked or unsure, log a QUESTION entry and move on
  to a non-conflicting part of your task while waiting.
- The user (Aryan) relays between chats when needed and makes product
  decisions (e.g. the chrome/pill-vs-card call).

## Joining — do this FIRST, before touching any code

1. Pick a name: `agent-<word>` where `<word>` is any random word of your
   choice **not already in the Roster** (agent-1 is reserved for the
   orchestrator).
2. Append a log entry: *"I'm agent-\<word\>, claiming task T\<n\>: \<one
   line\>"*.
3. Flip your task's status line to `IN PROGRESS — agent-<word>` in Tasks.
4. One task per agent at a time. Finish or hand it back before taking another.

## The rules (breaking these breaks another agent's work)

- **A pre-existing agent is already fixing bugs in this worktree.** It is not
  registered here — DO NOT DISTURB IT. Every file currently listed as
  modified in `git status` is potentially its territory. Therefore:
  - Never run `git checkout/restore/reset/stash/clean` on files you didn't
    claim. Ever. (Uncommitted work from other agents can be destroyed.)
  - If your task needs a file that's already modified in `git status`,
    re-read it immediately before editing, make **narrow additive edits**,
    and note the overlap in your log.
- `src-tauri/src/commands.rs` is a shared hotspot (every new Tauri command
  registers there). Append-only: add your command, never reorder or reformat
  existing code.
- Edit the smallest region that does the job — no whole-file rewrites, no
  drive-by refactors, no reformatting.
- Only touch `agentchat.md` in these places: your roster line, your task
  status line, and the Log section (append + trim per retention rule).
- If two tasks need the same file, log a `CONFLICT` entry and wait for
  agent-1 to split the work.
- The chrome restructure (bottom pill vs mini-dock card, `ux-review.md`
  "Chrome proposal") is **BLOCKED — awaiting a decision from Aryan.** Nobody
  picks it up until agent-1 unblocks it.

## Logging — mandatory, every meaningful step

Append entries at the **bottom** of the Log section (newest last). Get a
timestamp with `date -u +"%Y-%m-%d %H:%M"`. Log when you: claim a task,
finish each meaningful chunk (files + what changed), get blocked (QUESTION),
and finish (DONE + how to verify).

Entry format — one block per event:

```
### 2026-10-31 14:02 UTC — agent-falcon
- STATUS: CLAIMED | WORKING | BLOCKED | QUESTION | CONFLICT | DONE
- TASK: T1
- FILES: src-tauri/src/online/search.rs, frontend/js/online.js
- WHAT: concrete one/two lines — what changed, what's next
```

**Retention: only the 25 most recent log entries stay saved.** After
appending, count the `### ` entry headers; if more than 25, delete the
oldest entries from the top until 25 remain. Only trim by this
oldest-first rule — never delete a specific agent's entry otherwise.

## Roster

| Agent | Role | Status |
|---|---|---|
| agent-1 | orchestrator — reviews logs, guides, assigns tasks, git decisions | active |
| agent-falcon | worker — search & catalog backend/frontend | active |
| agent-cinder | worker — player & now-playing UX | active |
| (unknown) | pre-existing bug-fix agent — DO NOT DISTURB | active, unregistered |

## Tasks — round 1 (2026-10-31)

Statuses: `OPEN` → `IN PROGRESS — agent-<word>` → `DONE (verify: …)`.
Your task is pre-assigned; claim it via a log entry, then start.

### T1 — Search typeahead (P0 #2 in ux-review.md) — OPEN
**Owner: agent-falcon.** Search currently shows nothing until you finish and
submit; AM/Spotify show suggestions after ~2 chars.

1. **Verify first.** Live-verify the InnerTube `music/get_search_suggestions`
   endpoint (flagged TO-VERIFY in ROADMAP.md line 85). Follow the same
   verification pattern used for lyrics in `src-tauri/src/online/lyrics.rs`
   and the client plumbing in `src-tauri/src/online/client.rs`. If the
   endpoint doesn't behave, fall back to n-grams over recent searches +
   library (documented fallback in ux-review.md) and say so in your log.
2. **Backend.** Add a typed method (probably `src-tauri/src/online/search.rs`
   + `client.rs`) and a Tauri command `search_suggestions` registered
   append-only in `src-tauri/src/commands.rs`.
3. **Frontend.** Typeahead dropdown in the search view
   (`frontend/js/online.js`): ≥2 chars, ~200 ms debounce, keyboard
   navigation (↑/↓/Enter/Escape), click runs the search. Match the AM/Spotify
   feel; reuse existing styling tokens.

**Accept:** typing in Search shows live suggestions; clicking one runs the
search; API failure degrades silently (no error UI); no console errors.

### T2 — Player quick wins, then synced lyrics (P1 #5/#6 in ux-review.md) — OPEN
**Owner: agent-cinder.**

1. **Artist link from the player (P1 #5c).** The artist name in the expanded
   now-playing view should navigate to the artist page — the data is already
   in trackMap entries (`frontend/js/app.js`, view in
   `frontend/css/now-playing.css`).
2. **Volume pill in the expanded player (P1 #6).** No volume control exists
   while the big player is open. Add a compact slider row under the transport
   (`frontend/css/now-playing.css` + `frontend/index.html`), wired to the
   existing volume path (see `frontend/js/bridge.js` / `knob.js`).
3. **Main event — LRCLIB synced lyrics (P1 #5a).** YTM only exposes unsynced
   text; LRCLIB.net's free API has synced LRC for most catalog. New module
   `src-tauri/src/online/lrclib.rs` (match by artist + title + duration),
   command registered append-only in `commands.rs`, karaoke-highlighted
   synced lyrics in the lyrics view, falling back to the existing unsynced
   YTM text when no LRC match. This is the single highest-impact player
   upgrade — do 1–2 first as warm-up, then this.

**Accept:** artist name navigates to the artist page; volume works in the
expanded player; (stretch) synced lyrics highlight tracks playback within
~200 ms and fall back cleanly.

## Backlog — unassigned, do NOT self-assign (agent-1 hands these out in round 2)

- P0 #1 first-run onboarding: "pick your vibe" row + YTM charts shelf
  (`FEmusic_charts`) on Home
- P1 #3 local sled playlists with add-from-anywhere context menu
- P1 #7 "Because you liked X" shelf headers on Home
- P1 #8 per-track download state icons on detail pages
- P2: view transitions (respect `prefers-reduced-motion`), skeletons on
  Home/detail, `/` `l` `q` keyboard shortcuts, track title in window title
- BLOCKED on Aryan: chrome proposal (pill vs card decision)

## Log

<!-- append below, newest last, keep max 25 entries -->

### 2026-10-31 — agent-1
- STATUS: WORKING
- TASK: orchestration
- FILES: agentchat.md, docs/ux-review.md
- WHAT: Created the coordination hub. Roster: agent-falcon → T1 (search
  typeahead), agent-cinder → T2 (player quick wins + LRCLIB synced lyrics).
  Remember: a bug-fix agent is already active in this worktree — no git
  state ops by workers, narrow additive edits only.
