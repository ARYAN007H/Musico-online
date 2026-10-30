/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — core/shell.js
   Apple Music-style app chrome rendered with Preact + htm (vendored, no
   bundler):  <Sidebar> — grouped navigation;  <TopBar> — the persistent
   transport LCD (history arrows, art+title, transport, seek, volume, queue,
   fullscreen).  Both consume the store; transport actions delegate to
   window.App (legacy controller) so there is exactly one source of truth.
   ══════════════════════════════════════════════════════════════════════════════ */

import { html, render, useState, useEffect, useRef, useMemo } from '../../vendor/htm-preact.js';
import Store from './store.js?v=0.6.1';
import Router from './router.js?v=0.6.1';

// ─── Shared hooks ─────────────────────────────────────────────────────────────

function useStoreValue(key, fallback) {
  const [v, setV] = useState(Store.snapshotOf(key, fallback));
  useEffect(() => Store.subscribe((k, val) => { if (k === key) setV(val); }), [key]);
  return v;
}

// ─── Sidebar ──────────────────────────────────────────────────────────────────

const NAV_GROUPS = [
  {
    label: 'Listen',
    items: [
      { view: 'browse', label: 'Home', icon: 'globe' },
      { view: 'now-playing', label: 'Now Playing', icon: 'disc' },
      { view: 'genres', label: 'Browse', icon: 'grid' },
    ],
  },
  {
    label: 'Library',
    items: [
      { view: 'library', label: 'Library', icon: 'books' },
      { view: 'queue', label: 'Queue', icon: 'list' },
      { view: 'playlists', label: 'Playlists', icon: 'note' },
    ],
  },
];

const ICONS = {
  globe: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M2 12h20M12 2a15.3 15.3 0 014 10 15.3 15.3 0 01-4 10 15.3 15.3 0 01-4-10 15.3 15.3 0 014-10z"/></svg>`,
  disc: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3"/></svg>`,
  grid: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></svg>`,
  radio: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="2"/><path d="M16.24 7.76a6 6 0 010 8.49M7.76 16.24a6 6 0 010-8.49M19.07 4.93a10 10 0 010 14.14M4.93 19.07a10 10 0 010-14.14"/></svg>`,
  books: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 19.5A2.5 2.5 0 016.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 014 19.5v-15A2.5 2.5 0 016.5 2z"/></svg>`,
  list: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/></svg>`,
  note: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>`,
  gear: html`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 010 2.83 2 2 0 01-2.83 0l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"/></svg>`,
};

function NavItem({ item, active }) {
  return html`
    <button
      class="nav-item ${active ? 'active' : ''} ${item.soon ? 'soon' : ''}"
      data-view="${item.view}"
      disabled=${item.soon || undefined}
      title=${item.soon ? 'Coming soon' : item.label}
      aria-current=${active ? 'page' : undefined}
    >
      ${ICONS[item.icon]}
      <span>${item.label}</span>
    </button>`;
}

function Sidebar() {
  const view = useStoreValue('activeNav', Router.current());
  return html`
    <div class="side-brand">
      <svg class="brand-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
        <circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/><path d="M12 3v2.5M12 18.5V21"/>
      </svg>
      <div class="brand-text">
        <div class="brand-name">MUSICO</div>
        <div class="brand-sub">v0.6.11</div>
      </div>
    </div>
    <div id="sidebar-search-slot"></div>
    <nav class="side-nav" aria-label="Main">
      ${NAV_GROUPS.map((g) => html`
        <div class="nav-group" key=${g.label}>
          <div class="nav-group-label">${g.label}</div>
          ${g.items.map((it) => html`<${NavItem} key=${it.view} item=${it} active=${view === it.view} />`)}
        </div>
      `)}
    </nav>
    <div class="nav-group nav-group-bottom">
      <div class="nav-group-label">Settings</div>
      <${NavItem} item=${{ view: 'settings', label: 'Settings', icon: 'gear' }} active=${view === 'settings'} />
    </div>`;
}

// ─── TopBar (the LCD) ─────────────────────────────────────────────────────────

function fmtTime(secs) {
  const s = Math.max(0, Math.floor(secs || 0));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

function ArtThumb({ pb }) {
  const bg = pb.thumb
    ? `url("${pb.thumb}") center/cover no-repeat, ${pb.artGrad || 'rgba(255,255,255,0.06)'}`
    : (pb.artGrad || 'rgba(255,255,255,0.06)');
  return html`<div class="lcd-art" style=${{ background: bg }} aria-hidden="true" />`;
}

function TopBar() {
  const view = useStoreValue('view', Router.current());
  const pb = useStoreValue('playback', { hasSong: false });
  const panelOpen = useStoreValue('queuePanel', false);
  const [scrub, setScrub] = useState(null); // 0..1000 while dragging
  // Volume throttle (mirrors knob.js: stream input events, flush on change)
  const volThrottle = { last: 0, pending: null };
  const app = () => window.App || {};
  const transport = () => app().transport || {};
  const frac = scrub !== null ? scrub / 1000
    : (pb.duration > 0 ? Math.min(1, pb.position / pb.duration) : 0);

  const canBack = Router.canGoBack();
  const canFwd = Router.canGoForward();

  return html`
    <header class="topbar" role="banner">
      <div class="topbar-left">
        <button class="lcd-btn" id="nav-back" title="Back" aria-label="Go back"
                disabled=${!canBack || undefined} onClick=${() => Router.back()}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="15 18 9 12 15 6"/></svg>
        </button>
        <button class="lcd-btn" id="nav-forward" title="Forward" aria-label="Go forward"
                disabled=${!canFwd || undefined} onClick=${() => Router.forward()}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="9 18 15 12 9 6"/></svg>
        </button>
        <button class="lcd-btn" title="Previous" aria-label="Previous track"
                onClick=${() => transport().prev?.()}>
          <svg viewBox="0 0 24 24" fill="currentColor"><path d="M6 6h2v12H6zM9.5 12l8.5 6V6z"/></svg>
        </button>
        <button class="lcd-btn lcd-play" title=${pb.isPlaying ? 'Pause' : 'Play'}
                aria-label=${pb.isPlaying ? 'Pause' : 'Play'}
                onClick=${() => transport().toggle?.()}>
          ${pb.isPlaying
            ? html`<svg viewBox="0 0 24 24" fill="currentColor"><path d="M7 5h4v14H7zM13 5h4v14h-4z"/></svg>`
            : html`<svg viewBox="0 0 24 24" fill="currentColor"><polygon points="8,5 20,12 8,19"/></svg>`}
        </button>
        <button class="lcd-btn" title="Next" aria-label="Next track"
                onClick=${() => transport().next?.()}>
          <svg viewBox="0 0 24 24" fill="currentColor"><path d="M16 6h2v12h-2zM6 18l8.5-6L6 6z"/></svg>
        </button>
      </div>

      <div class="topbar-lcd" id="lcd">
        ${pb.hasSong ? html`<${ArtThumb} pb=${pb} />` : html`<div class="lcd-art lcd-art-empty" aria-hidden="true" />`}
        <div class="lcd-meta" aria-live="polite">
          <div class="lcd-title">${pb.hasSong ? pb.title : 'Not Playing'}</div>
          <div class="lcd-artist">${pb.hasSong ? pb.artist : 'Play something — it is all free'}</div>
          <div class="lcd-seek-row">
            <span class="lcd-time">${fmtTime(pb.position)}</span>
            <input type="range" class="lcd-seek" min="0" max="1000" step="1"
                   value=${Math.round(frac * 1000)}
                   aria-label="Seek" aria-valuemin="0" aria-valuemax="100"
                   aria-valuenow=${Math.round(frac * 100)}
                   disabled=${!pb.hasSong || undefined}
                   onInput=${(e) => setScrub(Number(e.target.value))}
                   onChange=${(e) => {
                     setScrub(null);
                     const secs = (Number(e.target.value) / 1000) * (pb.duration || 0);
                     transport().seek?.(secs);
                   }} />
            <span class="lcd-time">${fmtTime(pb.duration)}</span>
          </div>
        </div>
      </div>

      <div class="topbar-right">
        <button class="lcd-btn ${panelOpen ? 'lcd-on' : ''}" title="Playing Next" aria-label="Playing Next queue"
                onClick=${() => Store.set('queuePanel', !panelOpen)}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h13M3 12h13M3 18h9"/><path d="M19 12v6M22 15l-3-3-3 3"/></svg>
        </button>
        <button class="lcd-btn" title="Fullscreen player" aria-label="Fullscreen player"
                onClick=${() => window.Online?.toggleFullscreen?.()}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M8 3H5a2 2 0 00-2 2v3M16 3h3a2 2 0 012 2v3M8 21H5a2 2 0 01-2-2v-3M16 21h3a2 2 0 002-2v-3"/></svg>
        </button>
        <div class="lcd-vol-wrap">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" class="lcd-vol-icon"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 010 7.07"/></svg>
          <input type="range" class="lcd-vol" min="0" max="100" step="1"
                 value=${Math.round((pb.volume ?? 0.68) * 100)}
                 aria-label="Volume"
                 onInput=${(e) => {
                   const v = Number(e.target.value) / 100;
                   const now = Date.now();
                   volThrottle.pending = v;
                   if (now - volThrottle.last >= 100) {
                     volThrottle.last = now;
                     window.Knob?.setVolume?.(v, true);
                     volThrottle.pending = null;
                   }
                 }}
                 onChange=${(e) => {
                   // Flush the final value (knob.js parity)
                   const v = Number(e.target.value) / 100;
                   window.Knob?.setVolume?.(v, true);
                   volThrottle.pending = null;
                 }} />
        </div>
      </div>
    </header>`;
}

// ─── Playing Next panel ───────────────────────────────────────────────────────

function fmtQueueTime(secs) {
  const s = Math.max(0, Math.floor(secs || 0));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

function QueueRow({ song, current, removable, onPlay }) {
  const bg = song.thumb
    ? `url("${song.thumb}") center/cover no-repeat, ${song.artGrad || 'rgba(255,255,255,0.06)'}`
    : (song.artGrad || 'rgba(255,255,255,0.06)');
  const id = String(song.id || '');
  const vid = id.startsWith('yt:') ? id.slice(3) : null;
  return html`
    <div class="qp-row ${current ? 'qp-current' : ''}" role="button" tabindex="0"
         aria-label="Play ${song.title}" onClick=${onPlay}
         data-ctx=${removable ? 'qrow' : null} data-qid=${song.id} data-vid=${vid}
         data-title=${song.title} data-artist=${song.artist || null}
         onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onPlay(); } }}>
      <div class="qp-art" style=${{ background: bg }} aria-hidden="true" />
      <div class="qp-meta">
        <div class="qp-title">${song.title}</div>
        <div class="qp-sub">${song.artist}</div>
      </div>
      <span class="qp-dur">${fmtQueueTime(song.duration_secs)}</span>
    </div>`;
}

function QueuePanel() {
  const open = useStoreValue('queuePanel', false);
  const queue = useStoreValue('queue', []);
  const history = useStoreValue('history', []);
  const pb = useStoreValue('playback', { hasSong: false });
  const autoplay = useStoreValue('autoplay', true);
  if (!open) return null;

  const playSong = (song) => window.App?.playSong?.(song);
  // Drop exactly ONE leading copy of the current song (it may legitimately
  // be queued again for replay).
  const upcoming = queue[0]?.id === pb.id ? queue.slice(1) : queue;

  return html`
    <aside class="queue-panel" role="complementary" aria-label="Playing Next">
      <div class="qp-header">
        <div class="qp-title-lg">Playing Next</div>
        <button class="lcd-btn" title="Close" aria-label="Close Playing Next"
                onClick=${() => Store.set('queuePanel', false)}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
        </button>
      </div>

      <div class="qp-scroll">
        ${pb.hasSong ? html`
          <div class="qp-section">Now Playing</div>
          <${QueueRow} song=${{ ...pb, title: pb.title, duration_secs: pb.duration }} current=${true}
                       onPlay=${() => {}} />` : null}

        <div class="qp-section">Playing Next</div>
        ${upcoming.length ? upcoming.map((song, i) => html`
          <${QueueRow} key=${song.id + i} song=${song} removable=${true}
                       onPlay=${() => playSong(song)} />`)
          : html`<div class="qp-empty">Queue is empty — play something.</div>`}

        <div class="qp-section">Recently Played</div>
        ${history.length ? history.map((song, i) => html`
          <${QueueRow} key=${song.id + 'h' + i} song=${song} onPlay=${() => playSong(song)} />`)
          : html`<div class="qp-empty">Nothing yet.</div>`}
      </div>

      <button class="qp-autoplay" aria-pressed=${autoplay}
              title="Toggle autoplay"
              onClick=${() => {
                const nv = !autoplay;
                Store.set('autoplay', nv);
                // Sync the legacy snapshot: persistSettings round-trips the
                // whole settings object — a stale copy would silently revert.
                if (window.App?.state?.settings) window.App.state.settings.autoplay = nv;
                window.Bridge?.invoke?.('set_autoplay', { enabled: nv });
              }}>
        <input type="checkbox" checked=${autoplay} tabindex="-1" aria-hidden="true"
               style="pointer-events: none" />
        <span>Autoplay — keep the music going</span>
      </button>
    </aside>`;
}

// ─── Home (Listen Now) feed ───────────────────────────────────────────────────

// Feed cache in the store: 5-minute TTL mirrors the backend browse cache.
const FEED_TTL = 5 * 60 * 1000;

async function loadHomeFeed(force = false) {
  const cached = Store.get('homeFeed');
  if (!force && cached && Date.now() - cached.at < FEED_TTL) return cached;
  Store.set('homeFeedLoading', true);
  try {
    // Bridge.invoke resolves null on IPC failure (it swallows rejections) —
    // a null page or zero shelves is a failure, never a cacheable result.
    const page = await Bridge0().invoke('online_home');
    const shelves = page?.shelves || [];
    if (!page || shelves.length === 0) {
      Store.set('homeFeedLoading', false);
      if (!Store.get('homeFeed')) {
        Store.set('homeFeedError', 'The feed is unreachable right now.');
      }
      return Store.get('homeFeed') || { at: 0, shelves: [] };
    }
    const entry = { at: Date.now(), shelves };
    Store.set('homeFeed', entry);
    Store.set('homeFeedError', null);
    Store.set('homeFeedLoading', false);
    return entry;
  } catch (e) {
    Store.set('homeFeedLoading', false);
    Store.set('homeFeedError', String(e));
    return Store.get('homeFeed') || { at: Date.now(), shelves: [] };
  }
}

function Bridge0() { return window.Bridge || { invoke: async () => ({ shelves: [] }) }; }

function ShelfCard({ item, hero }) {
  const bg = item.thumbnail
    ? `url("${item.thumbnail}") center/cover no-repeat, rgba(255,255,255,0.06)`
    : 'rgba(255,255,255,0.06)';
  const playable = item.video_id;
  const navigable = item.browse_id;
  const onClick = () => {
    if (playable) {
      const track = {
        video_id: item.video_id, title: item.title, artist: item.subtitle,
        album: '', duration_secs: null, thumbnail: item.thumbnail, cached: false,
      };
      window.Online?.playTrack?.(track, null);
    } else if (navigable) {
      Router.navigate(Router.detailRoute({
        browseId: item.browse_id, params: item.params || null, title: item.title,
      }));
    } else {
      window.App?.showToast?.('This item has no playable or browsable target');
    }
  };
  return html`
    <div class="shelf-card ${item.kind === 'artist' ? 'shelf-artist' : ''} ${hero ? 'shelf-card-hero' : ''}"
         role="button" tabindex="0" aria-label="${item.title}"
         data-ctx=${playable ? 'song' : (navigable ? 'nav' : null)}
         data-kind=${item.kind || null}
         data-thumb=${item.thumbnail || null}
         data-video-id=${item.video_id || null}
         data-title=${item.title}
         data-artist=${item.subtitle || null}
         data-browse-id=${item.browse_id || null}
         data-params=${item.params || null}
         onClick=${onClick}
         onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onClick(); } }}>
      <div class="shelf-art" style=${{ background: bg }} aria-hidden="true">
        ${item.kind === 'artist' ? html`<div class="shelf-art-artist" />` : null}
        ${hero ? html`<button type="button" class="shelf-play" tabindex="-1"
                             aria-hidden="true"
                             onClick=${(e) => { e.stopPropagation(); onClick(); }}>
          <svg viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
        </button>` : null}
      </div>
      <div class="shelf-card-title">${item.title}</div>
      <div class="shelf-card-sub">${item.subtitle}</div>
    </div>`;
}

function Shelf({ shelf, hero }) {
  return html`
    <section class="shelf" aria-label=${shelf.title}>
      <h2 class="shelf-title ${hero ? 'shelf-title-hero' : ''}">${shelf.title}</h2>
      <div class="shelf-row ${hero ? 'shelf-row-hero' : ''}">
        ${shelf.items.map((item, i) => html`<${ShelfCard} key=${item.video_id || item.browse_id || i} item=${item} hero=${hero} />`)}
      </div>
    </section>`;
}

/* Infinite home (#41): the FEmusic_home payload carries only 2-3 shelves for
   fresh sessions — after the real feed, lazily append section shelves from
   the 50 mood/genre category pages (existing online_browse command; no new
   backend surface).  Two categories per sentinel intersection, capped. */
const HOME_EXTRA_MAX = 14;
const HOME_EXTRA_BATCH = 2;

async function loadHomeExtra() {
  const cur = Store.get('homeExtra') || { sections: [], next: 0, done: false, loading: false };
  if (cur.loading || cur.done) return;
  Store.set('homeExtraLoading', true);
  try {
    let genres = Store.get('genres');
    if (!(genres && genres.length)) {
      const g = await Bridge0().invoke('online_genres');
      genres = Array.isArray(g) ? g : [];
      Store.set('genres', genres);
    }
    const sections = [...cur.sections];
    const seen = new Set(sections.map((x) => x.title));
    let next = cur.next;
    let added = 0;
    while (next < genres.length && added < HOME_EXTRA_BATCH && sections.length < HOME_EXTRA_MAX) {
      const cat = genres[next++];
      if (seen.has(cat.title)) continue;
      const page = await Bridge0().invoke('online_browse', { browseId: cat.browse_id, params: cat.params });
      const shelf = page?.shelves?.find((sh) => sh.items && sh.items.length);
      if (shelf) {
        sections.push({ title: cat.title, items: shelf.items });
        added++;
      }
    }
    Store.set('homeExtra', {
      sections,
      next,
      done: next >= genres.length || sections.length >= HOME_EXTRA_MAX,
      loading: false,
    });
  } catch (e) {
    const cur2 = Store.get('homeExtra') || { sections: [], next: 0 };
    Store.set('homeExtra', { ...cur2, done: true, loading: false });
  }
}

function HomeFeed() {
  const feed = useStoreValue('homeFeed', null);
  const loading = useStoreValue('homeFeedLoading', false);
  const error = useStoreValue('homeFeedError', null);
  const extra = useStoreValue('homeExtra', null);
  const extraLoading = useStoreValue('homeExtraLoading', false);
  const sentinelRef = useRef(null);

  useEffect(() => { loadHomeFeed(); }, []);

  // Grow the page when the sentinel nears the viewport (the view pane clips
  // to the window, so viewport-intersection == end-of-feed).
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !feed?.shelves?.length) return undefined;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((en) => en.isIntersecting)) loadHomeExtra();
    }, { rootMargin: '600px' });
    io.observe(el);
    return () => io.disconnect();
  }, [feed, extra]);

  if (error && !feed?.shelves?.length) {
    return html`<div class="home-status">
      Couldn't load the feed — ${error}
      <button class="home-retry" onClick=${() => loadHomeFeed(true)}>Retry</button>
    </div>`;
  }
  if (!feed?.shelves?.length) {
    return html`<div class="home-status">${loading ? 'Tuning your feed…' : ''}</div>`;
  }
  return html`
    <div class="home-feed">
      ${feed.shelves.map((s, i) => html`<${Shelf} key=${s.title + i} shelf=${s} hero=${i === 0} />`)}
      ${(extra?.sections || []).map((sec, i) => html`
        <${Shelf} key=${'x' + sec.title + i} shelf=${{ title: sec.title, items: sec.items }} />`)}
      ${!(extra?.done) && feed.shelves.length
        ? html`<div class="home-sentinel" ref=${sentinelRef}>
            <div class="home-status">${extraLoading ? 'Loading more for you…' : ''}</div>
          </div>`
        : html`<div class="home-status" style="padding: var(--space-24) 0 0;">
            That's the curation for now — Browse has 50 more moods.
          </div>`}
    </div>`;
}

// ─── Detail pages (album / playlist / artist) ─────────────────────────────────

const DETAIL_TTL = 5 * 60 * 1000;
const DETAIL_CACHE_CAP = 12;
let detailSeq = 0; // stale-response guard: only the newest load may write

async function loadDetail(target, force = false) {
  const my = ++detailSeq;
  const key = `${target.browseId}|${target.params || ''}`;
  const cache = Store.get('detailCache') || {};
  const hit = cache[key];
  if (hit && !force && Date.now() - hit.at < DETAIL_TTL) {
    Store.set('detailPage', hit.page);
    Store.set('detailError', null);
    Store.set('detailLoading', false);
    return;
  }
  Store.set('detailLoading', true);
  Store.set('detailError', null);
  try {
    // Bridge.invoke resolves null on IPC failure — treat as an error.
    const page = await Bridge0().invoke('online_browse', {
      browseId: target.browseId, params: target.params || null,
    });
    if (my !== detailSeq) return; // a newer navigation superseded this load
    if (!page) throw new Error('The page is unreachable right now.');
    Store.set('detailPage', page);
    Store.set('detailError', null);
    // Cap the cache: drop expired entries first, then the oldest.
    const pruned = Object.entries({ ...cache, [key]: { at: Date.now(), page } })
      .filter(([, v]) => Date.now() - v.at < DETAIL_TTL)
      .sort((a, b) => a[1].at - b[1].at)
      .slice(-DETAIL_CACHE_CAP);
    Store.set('detailCache', Object.fromEntries(pruned));
  } catch (e) {
    if (my !== detailSeq) return;
    Store.set('detailPage', null);
    Store.set('detailError', String(e).replace('Error: ', ''));
  }
  if (my === detailSeq) Store.set('detailLoading', false);
}

function DetailTrackRow({ item, index }) {
  const bg = item.thumbnail
    ? `url("${item.thumbnail}") center/cover no-repeat, rgba(255,255,255,0.06)`
    : 'rgba(255,255,255,0.06)';
  const play = () => {
    window.Online?.playTrack?.({
      video_id: item.video_id, title: item.title, artist: item.subtitle,
      album: '', duration_secs: item.duration_secs || null, thumbnail: item.thumbnail,
      cached: false,
    }, null);
  };
  return html`
    <div class="dt-row" role="button" tabindex="0" aria-label="Play ${item.title}"
         data-ctx="song" data-video-id=${item.video_id}
         data-title=${item.title} data-artist=${item.subtitle || null}
         onClick=${play}
         onKeyDown=${(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); play(); } }}>
      <span class="dt-index">${index + 1}</span>
      <div class="dt-art" style=${{ background: bg }} aria-hidden="true" />
      <div class="dt-meta">
        <div class="dt-title">${item.title}</div>
        ${item.subtitle ? html`<div class="dt-sub">${item.subtitle}</div>` : null}
      </div>
      <button class="dt-enqueue" title="Add to queue (downloads in background)"
              aria-label="Add ${item.title} to queue"
              onClick=${(e) => {
                e.stopPropagation();
                window.Online?.enqueueTrack?.({
                  video_id: item.video_id, title: item.title, artist: item.subtitle,
                  album: '', duration_secs: item.duration_secs || null, thumbnail: item.thumbnail,
                  cached: false,
                }, e.currentTarget);
              }}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
      </button>
    </div>`;
}

function DetailView() {
  const target = useStoreValue('detail', null);
  const page = useStoreValue('detailPage', null);
  const loading = useStoreValue('detailLoading', false);
  const error = useStoreValue('detailError', null);

  useEffect(() => {
    if (target) loadDetail(target);
  }, [target?.browseId, target?.params]);

  if (!target) return null;

  // The FIRST fully-playable shelf is the page's tracklist; later ones
  // (artist "Songs" vs "Videos", discographies) render as carousels —
  // Apple Music keeps them visually separate.
  const tracks = [];
  const shelves = [];
  let tracksTaken = false;
  for (const shelf of page?.shelves || []) {
    const playableRows = shelf.items.filter((it) => it.video_id);
    if (!tracksTaken && shelf.items.length > 0 && playableRows.length === shelf.items.length) {
      tracks.push(...shelf.items);
      tracksTaken = true;
    } else {
      shelves.push(shelf);
    }
  }
  const header = page?.header || { title: target.title || '' };
  const art = header.thumbnail
    ? `url("${header.thumbnail}") center/cover no-repeat, rgba(255,255,255,0.06)`
    : 'rgba(255,255,255,0.06)';

  const playAll = () => {
    if (tracks.length) window.Online?.playTrack?.({
      video_id: tracks[0].video_id, title: tracks[0].title, artist: tracks[0].subtitle,
      album: '', duration_secs: null, thumbnail: tracks[0].thumbnail, cached: false,
    }, null);
  };
  const queueAll = () => {
    for (const t of tracks) {
      window.Online?.enqueueTrack?.({
        video_id: t.video_id, title: t.title, artist: t.subtitle,
        album: '', duration_secs: null, thumbnail: t.thumbnail, cached: false,
      }, null, { silent: true });
    }
    window.App?.showToast?.(`Queued ${tracks.length} tracks — streams resolve in the background`);
  };

  // C4.2: batch-download the whole tracklist (≤ 2 concurrent, resumable).
  const downloadAll = () => {
    if (!tracks.length) return;
    window.Bridge?.invoke?.('online_download_batch', {
      label: header.title || 'Download',
      tracks: tracks.map((t) => ({
        video_id: t.video_id, title: t.title, artist: t.subtitle,
        album: '', duration_secs: null, thumbnail: null, cached: false,
      })),
    }).then((id) => {
      if (id !== null) {
        window.App?.showToast?.(`Downloading ${tracks.length} tracks — progress in Settings`);
        invalidateLibraryLists();
      }
    });
  };

  return html`
    <div class="detail">
      <div class="detail-hero">
        <div class="detail-art" style=${{ background: art }} aria-hidden="true" />
        <div class="detail-head">
          <div class="detail-kind">${(header.subtitle || target.kind || 'Page').toUpperCase()}</div>
          <h1 class="detail-title">${header.title}</h1>
          ${header.subtitle && header.subtitle !== header.title ? html`
            <div class="detail-sub">${header.subtitle}</div>` : null}
          ${tracks.length ? html`
            <div class="detail-actions">
              <button class="detail-play" onClick=${playAll} aria-label="Play all">
                <svg viewBox="0 0 24 24" fill="currentColor"><polygon points="8,5 20,12 8,19"/></svg>
                Play
              </button>
              <button class="detail-queueall" onClick=${queueAll} aria-label="Queue all tracks">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
                Queue all
              </button>
              <button class="detail-downloadall" onClick=${downloadAll} aria-label="Download all tracks">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3"/></svg>
                Download all
              </button>
            </div>` : null}
        </div>
      </div>

      ${loading ? html`<div class="home-status">Loading…</div>` : null}
      ${error ? html`<div class="home-status">
        Couldn't load this page — ${error}
        <button class="home-retry" onClick=${() => loadDetail(target)}>Retry</button>
      </div>` : null}

      ${tracks.length ? html`
        <div class="dt-list">
          ${tracks.map((t, i) => html`<${DetailTrackRow} key=${t.video_id + i} item=${t} index=${i} />`)}
        </div>` : null}

      ${shelves.map((sh, i) => html`<${Shelf} key=${sh.title + i} shelf=${sh} />`)}
    </div>`;
}

// ─── Browse (mood/genre grid) ─────────────────────────────────────────────────

let genresSeq = 0;

async function loadGenres() {
  const seq = ++genresSeq;
  Store.set('genresLoading', true);
  try {
    const items = await Bridge0().invoke('online_genres');
    if (seq !== genresSeq) return; // superseded by a newer load
    // Bridge.invoke swallows IPC rejections (resolves null), and the backend
    // never caches an empty parse — null/[] is a failure, never a result.
    if (!items || items.length === 0) {
      if (!(Store.get('genres') || []).length) {
        Store.set('genresError', 'Couldn\u2019t load categories.');
      }
    } else {
      Store.set('genres', items);
      Store.set('genresError', null);
    }
  } catch (e) {
    if (seq !== genresSeq) return;
    if (!(Store.get('genres') || []).length) Store.set('genresError', String(e));
  }
  if (seq === genresSeq) Store.set('genresLoading', false);
}

function GenresView() {
  const genres = useStoreValue('genres', null);
  const loading = useStoreValue('genresLoading', false);
  const error = useStoreValue('genresError', null);
  const activeNav = useStoreValue('activeNav', null);

  // Fetch on first activation of the Browse tab — not at app boot, so users
  // who never open Browse never pay the InnerTube round-trip.
  useEffect(() => {
    if (activeNav === 'genres' && !(genres && genres.length) && !loading) loadGenres();
  }, [activeNav]);

  const open = (item) => Router.navigate(Router.detailRoute({
    browseId: item.browse_id, params: item.params || null, title: item.title,
  }));

  if (error && !(genres && genres.length)) {
    return html`<div class="home-status">
      Couldn't load categories — ${error}
      <button class="home-retry" onClick=${loadGenres}>Retry</button>
    </div>`;
  }
  if (!(genres && genres.length)) {
    return html`<div class="home-status">${loading ? 'Loading categories…' : ''}</div>`;
  }
  return html`
    <div class="genres">
      <header class="browse-header">
        <h1 class="browse-title">Browse</h1>
        <p class="browse-sub">Moods & genres — editorial radio for every moment.</p>
      </header>
      <div class="genre-grid">
        ${genres.map((g, i) => html`
          <button class="genre-card" key=${g.browse_id + i}
                  data-ctx="nav" data-browse-id=${g.browse_id}
                  data-params=${g.params || null} data-title=${g.title}
                  style=${{ '--hue': String((g.title || '').split('').reduce((a, c) => (a * 31 + c.charCodeAt(0)) % 360, 7)) }}
                  onClick=${() => open(g)} aria-label="Browse ${g.title}">
            <span class="genre-name">${g.title}</span>
          </button>`)}
      </div>
    </div>`;
}

// ─── Context menu (C3.1 play-from-anywhere) ──────────────────────────────────

function saveToLibrary(kind, partial) {
  return window.Bridge?.invoke?.('library_add', { entry: { ...partial, kind } })
    .then((ok) => {
      if (ok === null) return ok;
      window.App?.showToast?.(`Saved to Library — ${partial.title || kind}`);
      invalidateLibraryLists();
      return ok;
    });
}

// Library pages re-fetch when this revision counter moves (any add/remove).
function invalidateLibraryLists() {
  ['libAlbums', 'libArtists', 'libTracks'].forEach((k) => Store.set(k, null));
  Store.set('libRev', (Store.get('libRev') || 0) + 1);
}

function trackFromDataset(ds) {
  return {
    video_id: ds.videoId, title: ds.title || '', artist: ds.artist || '',
    album: '', duration_secs: null, thumbnail: null, cached: false,
  };
}

function ctxItemsFor(el) {
  const ds = el.dataset;
  const Online = window.Online || {};
  const Router = window.MusicoRouter;
  if (ds.ctx === 'song' && ds.videoId) {
    // Prefer the searched track (carries thumbnail/duration) over the
    // dataset stub — playNext/startRadio write this back into trackMap.
    const track = Online.trackMap?.get(ds.videoId) || trackFromDataset(ds);
    return [
      { label: 'Play', action: () => Online.playTrack?.(track, null) },
      { label: 'Play Next', action: () => Online.playNext?.(track) },
      { label: 'Play Last', action: () => Online.enqueueTrack?.(track) },
      { label: 'Download', action: () => Online.downloadTrack?.(track) },
      { label: 'Add to Library', action: () => saveToLibrary('track', {
          id: ds.videoId, title: ds.title || '', subtitle: ds.artist || '',
          video_id: ds.videoId, browse_id: null, params: null,
          thumbnail: ds.thumb || null,
        }) },
      { sep: true },
      { label: 'Start Radio', action: () => Online.startRadio?.(track) },
    ];
  }
  if (ds.ctx === 'nav' && ds.browseId) {
    const kind = ds.kind;
    const open = () => Router?.navigate(Router?.detailRoute({
      browseId: ds.browseId, params: ds.params || null, title: ds.title || '',
    }));
    const items = [{ label: 'Open', action: open }];
    // Only album/artist cards: the backend library has no playlist/genre
    // kind, and a silent wrong-kind save shows up nowhere.
    if (kind === 'album' || kind === 'artist') {
      items.push({ label: 'Add to Library', action: () => saveToLibrary(kind, {
        id: ds.browseId, title: ds.title || '', subtitle: ds.artist || '',
        thumbnail: ds.thumb || null,
        browse_id: ds.browseId, params: ds.params || null,
      }) });
    }
    return items;
  }
  if (ds.ctx === 'qrow' && ds.qid) {
    const items = [];
    if (ds.vid) {
      items.push({ label: 'Start Radio', action: () => Online.startRadio?.({
        video_id: ds.vid, title: ds.title || '', artist: ds.artist || '',
        album: '', duration_secs: null, thumbnail: null, cached: false,
      }) });
    }
    items.push({
      label: 'Remove from Queue',
      action: () => window.Bridge?.invoke?.('remove_from_queue', { songId: ds.qid }),
    });
    return items;
  }
  return [];
}

function ContextMenu() {
  const menu = useStoreValue('ctxMenu', null);

  useEffect(() => {
    const onCtx = (e) => {
      // A right-click anywhere replaces/closes whatever menu is open.
      if (Store.get('ctxMenu')) Store.set('ctxMenu', null);
      const el = e.target.closest?.('[data-ctx]');
      if (!el) return;
      const items = ctxItemsFor(el);
      if (!items.length) return;
      e.preventDefault();
      Store.set('ctxMenu', { x: e.clientX, y: e.clientY, items });
    };
    const close = () => { if (Store.get('ctxMenu')) Store.set('ctxMenu', null); };
    const onKey = (e) => { if (e.key === 'Escape') close(); };
    document.addEventListener('contextmenu', onCtx);
    document.addEventListener('click', close);
    document.addEventListener('keydown', onKey);
    window.addEventListener('blur', close);
    return () => {
      document.removeEventListener('contextmenu', onCtx);
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('blur', close);
    };
  }, []);

  if (!menu) return null;
  const W = 208, ITEM_H = 34, SEP_H = 9, PAD = 8;
  const h = PAD * 2 + menu.items.reduce(
    (acc, it) => acc + (it.sep ? SEP_H : ITEM_H), 0);
  const x = Math.max(8, Math.min(menu.x, window.innerWidth - W - 8));
  const y = Math.max(8, Math.min(menu.y, window.innerHeight - h - 8));
  return html`
    <div class="ctx-menu" style=${{ left: x + 'px', top: y + 'px' }} role="menu">
      ${menu.items.map((it, i) => it.sep
        ? html`<div class="ctx-sep" key=${'s' + i} />`
        : html`<button class="ctx-item" role="menuitem" key=${i}
                       onClick=${it.action}>${it.label}</button>`)}
    </div>`;
}

// ─── Library (C4.1: saved albums / artists / tracks) ─────────────────────────

const LIB_TABS = [
  { id: 'recent', label: 'Recently Added' },
  { id: 'albums', label: 'Albums' },
  { id: 'artists', label: 'Artists' },
  { id: 'songs', label: 'Songs' },
];

const AZ = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('');

function libLetter(t) {
  const c = (t.title || '?').trim().charAt(0).toUpperCase();
  return /[A-Z]/.test(c) ? c : '#';
}

// Per-key sequence guards — album/artist/track loads run in parallel and
// must not invalidate each other's completions.
const libLoadSeq = {};

async function loadLibraryKind(kind, storeKey) {
  const seq = (libLoadSeq[storeKey] = (libLoadSeq[storeKey] || 0) + 1);
  Store.set(storeKey + 'Loading', true);
  try {
    const items = await Bridge0().invoke('library_list', { kind });
    if (seq !== libLoadSeq[storeKey]) return; // a newer load superseded this one
    // Only an array is a result; an empty one is a fresh library.  Anything
    // else (IPC null from a missing bridge, garbage) is an error with Retry.
    if (Array.isArray(items)) {
      Store.set(storeKey, items);
      Store.set(storeKey + 'Error', null);
    } else {
      Store.set(storeKey, []);
      Store.set(storeKey + 'Error', 'Couldn\u2019t load the library.');
    }
  } catch (e) {
    if (seq !== libLoadSeq[storeKey]) return;
    Store.set(storeKey, []);
    Store.set(storeKey + 'Error', String(e));
  }
  if (seq === libLoadSeq[storeKey]) Store.set(storeKey + 'Loading', false);
}

function LibraryCard({ entry, onOpen }) {
  const bg = entry.thumbnail
    ? `url("${entry.thumbnail}") center/cover no-repeat, rgba(255,255,255,0.06)`
    : 'rgba(255,255,255,0.06)';
  return html`
    <div class="lib-card" role="button" tabindex="0" data-letter=${libLetter(entry)}
         aria-label="${entry.title}" onClick=${onOpen}
         onKeyDown=${(e) => { if (e.key === 'Enter') onOpen(); }}>
      <div class="lib-card-art" style=${{ background: bg }} aria-hidden="true" />
      <div class="lib-card-title">${entry.title}</div>
      <div class="lib-card-sub">${entry.subtitle || ''}</div>
    </div>`;
}

function LibRow({ entry, onPlay, onOpen }) {
  const open = () => onOpen && entry.browse_id && onOpen();
  return html`
    <div class="lib-row" role="button" tabindex="0" data-letter=${libLetter(entry)}
         onClick=${open}
         onKeyDown=${(e) => { if (e.key === 'Enter') open(); }}>
      <button class="lib-row-play" aria-label="Play ${entry.title}"
              onClick=${(e) => { e.stopPropagation(); onPlay(); }}>
        <svg viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>
      </button>
      <div class="lib-meta">
        <div class="lib-title">${entry.title}</div>
        <div class="lib-sub">${entry.subtitle || ''}</div>
      </div>
    </div>`;
}

function AZRail({ entries, rootSelector, jumpFallback }) {
  const letters = [...new Set(entries.map(libLetter))].sort();
  const jump = (l) => {
    const root = document.querySelector(rootSelector);
    const first = root && root.querySelector(`[data-letter="${l}"]`);
    if (first) {
      first.scrollIntoView({ behavior: 'smooth', block: 'start' });
    } else if (jumpFallback && jumpFallback(l)) {
      // Windowed lists: the letter's rows render after the window grows —
      // scroll once they exist.
      setTimeout(() => {
        document.querySelector(rootSelector)
          ?.querySelector(`[data-letter="${l}"]`)
          ?.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }, 60);
    }
  };
  return html`
    <div class="az-rail" aria-hidden="true">
      ${AZ.map((l) => html`
        <button class=${'az-letter' + (letters.includes(l) ? '' : ' az-off')}
                key=${l} tabIndex=${-1} onClick=${() => jump(l)}>${l}</button>`)}
    </div>`;
}

function openCatalogEntry(entry) {
  if (!entry.browse_id) return;
  window.MusicoRouter.navigate(window.MusicoRouter.detailRoute({
    browseId: entry.browse_id, params: entry.params || null, title: entry.title,
  }));
}

const SONGS_WINDOW = 80;

function MergedSongs({ search }) {
  const local = useStoreValue('localLibrary', []);
  const saved = useStoreValue('libTracks', []);
  const loadErr = useStoreValue('libTracksError', null);
  const loading = useStoreValue('libTracksLoading', false);
  const [shown, setShown] = useState(SONGS_WINDOW);
  const sentinelRef = useRef(null);

  useEffect(() => { setShown(SONGS_WINDOW); }, [search]);

  // Merge local songs and saved online tracks, A–Z by title.  Rows keep a
  // reference to the original object — no copies, windowing bounds the DOM.
  // Memoized: the merge+sort runs only when the libraries change, not on
  // every keystroke (the filter below is a cheap pass over a sorted array).
  const merged = useMemo(() => {
    const rows = [];
    for (const meta of local || []) {
      rows.push({
        key: `l${meta.id}`, title: meta.title || '', subtitle: meta.artist || '',
        letter: libLetter({ title: meta.title || '?' }), meta,
        onPlay: () => window.App?.playSong?.(meta),
      });
    }
    for (const e of saved || []) {
      rows.push({
        key: `s${e.id}`, title: e.title || '', subtitle: e.subtitle || '',
        letter: libLetter(e), entry: e,
        onPlay: () => playCatalogTrackEntry(e),
      });
    }
    rows.sort((a, b) => a.title.localeCompare(b.title));
    return rows;
  }, [local, saved]);
  const filtered = search
    ? merged.filter((r) => `${r.title} ${r.subtitle}`.toLowerCase().includes(search))
    : merged;
  const visible = filtered.slice(0, shown);

  // IntersectionObserver windowing: grow the window as the sentinel nears
  // the viewport — 10k rows never render at once.
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || shown >= filtered.length) return undefined;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((en) => en.isIntersecting)) {
        setShown((n) => Math.min(n + SONGS_WINDOW, filtered.length));
      }
    }, { rootMargin: '600px' });
    io.observe(el);
    return () => io.disconnect();
  }, [shown, filtered.length]);

  return html`
    <div class="lib-with-rail">
      <div class="lib-rows">
        ${visible.map((r, i) => html`
          ${(i === 0 || visible[i - 1].letter !== r.letter)
            ? html`<div class="lib-letter-header" key=${'h' + r.letter + i}
                        data-letter=${r.letter}>${r.letter}</div>`
            : null}
          <${LibRow} key=${r.key} entry=${r} onPlay=${r.onPlay}
                     onOpen=${r.entry ? () => openCatalogEntry(r.entry) : null} />
        `)}
        ${shown < filtered.length
          ? html`<div class="lib-sentinel" ref=${sentinelRef} key="sentinel">
              <div class="home-status">Loading more…</div>
            </div>`
          : filtered.length === 0
            ? (loadErr && !(local || []).length
                ? html`<div class="home-status">Couldn't load saved tracks — ${loadErr}
                    <button class="home-retry" onClick=${() => window.MusicoStore?.set('libRev', (window.MusicoStore?.get('libRev') || 0) + 1)}>Retry</button>
                  </div>`
                : loading && !(local || []).length
                  ? html`<div class="home-status">Loading library…</div>`
                  : html`<div class="lib-empty">No songs${search ? ' match your search' : ''}.</div>`)
            : null}
      </div>
      <${AZRail} entries=${filtered} rootSelector=".lib-with-rail"
                  jumpFallback=${(letter) => {
                    const idx = filtered.findIndex((r) => r.letter === letter);
                    if (idx < 0) return false;
                    setShown(Math.max(shown, idx + SONGS_WINDOW + 20));
                    return true;
                  }} />
    </div>`;
}

function playCatalogTrackEntry(e) {
  window.Online?.playTrack?.({
    video_id: e.video_id, title: e.title, artist: e.subtitle,
    album: '', duration_secs: null, thumbnail: e.thumbnail, cached: false,
  }, null);
}

function LibraryCatalog() {
  const tab = useStoreValue('libTab', 'recent');
  const albums = useStoreValue('libAlbums', null);
  const artists = useStoreValue('libArtists', null);
  const tracks = useStoreValue('libTracks', null);
  const active = useStoreValue('activeNav', null);
  const rev = useStoreValue('libRev', 0);
  const loadErr = useStoreValue('libAlbumsError', null);
  const loading = useStoreValue('libAlbumsLoading', false);
  const search = useStoreValue('libSearch', '');

  // The library header's search box (legacy DOM) filters the merged list.
  useEffect(() => {
    const input = document.getElementById('search-input');
    if (!input) return undefined;
    const onInput = () => Store.set('libSearch', input.value.trim().toLowerCase());
    input.addEventListener('input', onInput);
    const clearBtn = document.getElementById('search-clear');
    const onClear = () => Store.set('libSearch', '');
    clearBtn?.addEventListener('click', onClear);
    return () => {
      input.removeEventListener('input', onInput);
      clearBtn?.removeEventListener('click', onClear);
    };
  }, []);

  // Load a kind's list the first time its tab (or Library itself) is shown,
  // and again whenever a save invalidates the revision.
  useEffect(() => {
    if (active !== 'library') return;
    // Tracks are needed on the Songs tab too (merged list) — no tab gate.
    if (!albums) loadLibraryKind('album', 'libAlbums');
    if (!artists) loadLibraryKind('artist', 'libArtists');
    if (!tracks) loadLibraryKind('track', 'libTracks');
  }, [active, tab, rev]);

  // The merged Songs list replaces the legacy local-only list everywhere
  // (C4.1c); its sort pills and view toggle have no merged-list meaning.
  useEffect(() => {
    ['song-list-view', 'album-grid-view'].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.style.display = 'none';
    });
    ['.sort-bar', '.view-toggle'].forEach((sel) => {
      const el = document.querySelector(`#view-library ${sel}`);
      if (el) el.style.display = 'none';
    });
  }, [tab]);

  if (active !== 'library') return null;

  // Recently Added = everything saved (albums, artists, tracks), newest first.
  // Lists default to null until first load — never spread them bare.
  const recent = [...(albums || []), ...(artists || []), ...(tracks || [])]
    .sort((a, b) => (b.added_at || 0) - (a.added_at || 0));

  const cards = (entries, onOpen) => html`
    <div class="lib-grid">
      ${entries.map((e) => html`<${LibraryCard} key=${e.kind + e.id} entry=${e} onOpen=${onOpen} />`)}
    </div>`;

  const statusHtml = (emptyText) => loadErr
    ? html`<div class="home-status">Couldn't load the library — ${loadErr}
        <button class="home-retry" onClick=${() => invalidateLibraryLists()}>Retry</button>
      </div>`
    : loading && !recent.length
      ? html`<div class="home-status">Loading library…</div>`
      : html`<div class="lib-empty">${emptyText}</div>`;

  let body = null;
  if (tab === 'recent') {
    body = recent.length ? cards(recent, (e) => openCatalogEntry(e)) : statusHtml('Nothing saved yet — add albums and artists from Browse.');
  } else if (tab === 'albums') {
    body = albums && albums.length ? html`
      <div class="lib-with-rail">${cards(albums, (e) => openCatalogEntry(e))}<${AZRail} entries=${albums} rootSelector=".lib-with-rail" /></div>`
      : statusHtml('No saved albums yet.');
  } else if (tab === 'artists') {
    body = artists && artists.length ? html`
      <div class="lib-with-rail">
        <div class="lib-rows">
          ${artists.map((e) => html`<${LibRow} key=${e.id} entry=${e} onOpen=${openCatalogEntry}
              onPlay=${() => window.App?.showToast?.('Artist radio lands with C4.4')} />`)}
        </div>
        <${AZRail} entries=${artists} rootSelector=".lib-with-rail" />
      </div>`
      : statusHtml('No followed artists yet.');
  } else {
    body = html`<${MergedSongs} search=${search} />`;
  }

  return html`
    <div class="lib-catalog">
      <div class="lib-tabs" role="tablist">
        ${LIB_TABS.map((t) => html`
          <button class=${'lib-tab' + (tab === t.id ? ' active' : '')} key=${t.id}
                  role="tab" aria-selected=${tab === t.id}
                  onClick=${() => Store.set('libTab', t.id)}>${t.label}</button>`)}
      </div>
      ${body}
    </div>`;
}

// ─── Batch downloads queue UI (C4.2, lives in Settings → storage card) ───────

function BatchQueue() {
  const batches = useStoreValue('batches', null);
  let refreshTimer = null;
  const refreshNow = () => {
    window.Bridge?.invoke?.('online_batches').then((list) => {
      Store.set('batches', Array.isArray(list) ? list : []);
    });
  };
  // Trailing debounce: a 50-track album fires 50 progress events.
  const refresh = () => {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(refreshNow, 250);
  };

  useEffect(() => {
    refreshNow();
    const onProgress = () => refresh();
    // listen() returns the unlisten fn (Tauri: a Promise of one).
    const un = window.Bridge?.listen?.('download-progress', onProgress);
    return () => {
      clearTimeout(refreshTimer);
      Promise.resolve(un).then((f) => f?.());
    };
  }, []);

  if (!batches || !batches.length) return null;
  // The backend serializes raw BatchJobs — counts derive from track states.
  const summarize = (b) => {
    const done = b.tracks.filter((t) => t.state === 'done').length;
    const failed = b.tracks.filter((t) => t.state.startsWith('failed:')).length;
    const running = b.active && b.tracks.some((t) => t.state === 'pending');
    return { done, failed, running };
  };
  return html`
    <div class="batch-list">
      ${batches.slice(0, 3).map((b) => {
        const { done, failed, running } = summarize(b);
        return html`
        <div class="batch-row" key=${b.id}>
          <div class="lib-meta">
            <div class="lib-title">${b.label}</div>
            <div class="lib-sub">${done} of ${b.tracks.length} downloaded
              ${failed ? html` · <span class="batch-failed">${failed} failed</span>` : null}
              ${running ? ' · downloading…' : ''}
            </div>
          </div>
          <div class="batch-progress" aria-hidden="true">
            <div class="batch-progress-fill" style=${{ width: `${b.tracks.length ? Math.round((100 * done) / b.tracks.length) : 0}%` }} />
          </div>
          ${failed ? html`
            <button class="batch-btn" aria-label="Retry failed downloads"
                    onClick=${() => { window.Bridge?.invoke?.('online_batch_retry', { id: b.id }); refresh(); }}>Retry</button>` : null}
          ${running ? html`
            <button class="batch-btn" aria-label="Cancel download"
                    onClick=${() => { window.Bridge?.invoke?.('online_batch_cancel', { id: b.id }); refresh(); }}>Cancel</button>`
            : b.tracks.some((t) => t.state === 'pending') ? html`
            <button class="batch-btn" aria-label="Resume download"
                    onClick=${() => { window.Bridge?.invoke?.('online_batch_retry', { id: b.id }); refresh(); }}>Resume</button>` : null}
        </div>`;
      })}
    </div>`;
}

// ─── Mount ────────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
  const sidebarRoot = document.getElementById('sidebar-root');
  const topbarRoot = document.getElementById('topbar-root');
  const panelRoot = document.getElementById('panel-root');
  const homeRoot = document.getElementById('home-root');
  const detailRoot = document.getElementById('detail-root');
  const genresRoot = document.getElementById('genres-root');
  if (sidebarRoot) render(html`<${Sidebar} />`, sidebarRoot);
  if (topbarRoot) render(html`<${TopBar} />`, topbarRoot);
  if (panelRoot) render(html`<${QueuePanel} />`, panelRoot);
  if (homeRoot) render(html`<${HomeFeed} />`, homeRoot);
  if (detailRoot) render(html`<${DetailView} />`, detailRoot);
  if (genresRoot) render(html`<${GenresView} />`, genresRoot);
  const ctxRoot = document.getElementById('ctx-root');
  if (ctxRoot) render(html`<${ContextMenu} />`, ctxRoot);
  const libRoot = document.getElementById('library-catalog');
  if (libRoot) render(html`<${LibraryCatalog} />`, libRoot);
  const batchRoot = document.getElementById('batch-root');
  if (batchRoot) render(html`<${BatchQueue} />`, batchRoot);
});
