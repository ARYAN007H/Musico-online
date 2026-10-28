/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — core/shell.js
   Apple Music-style app chrome rendered with Preact + htm (vendored, no
   bundler):  <Sidebar> — grouped navigation;  <TopBar> — the persistent
   transport LCD (history arrows, art+title, transport, seek, volume, queue,
   fullscreen).  Both consume the store; transport actions delegate to
   window.App (legacy controller) so there is exactly one source of truth.
   ══════════════════════════════════════════════════════════════════════════════ */

import { html, render, useState, useEffect } from '../../vendor/htm-preact.js';
import Store from './store.js?v=0.6.0';
import Router from './router.js?v=0.6.0';

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
      { view: 'radio', label: 'Radio', icon: 'radio', soon: true },
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
  const view = useStoreValue('view', Router.current());
  return html`
    <div class="side-brand">
      <svg class="brand-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
        <circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/><path d="M12 3v2.5M12 18.5V21"/>
      </svg>
      <div class="brand-text">
        <div class="brand-name">MUSICO</div>
        <div class="brand-sub">v0.6.0</div>
      </div>
    </div>
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
    ? `url("${pb.thumb}") center/cover no-repeat, ${pb.artGrad || 'var(--surface-raised)'}`
    : (pb.artGrad || 'var(--surface-raised)');
  return html`<div class="lcd-art" style=${{ background: bg }} aria-hidden="true" />`;
}

function TopBar() {
  const view = useStoreValue('view', Router.current());
  const pb = useStoreValue('playback', { hasSong: false });
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
        <button class="lcd-btn ${view === 'queue' ? 'lcd-on' : ''}" title="Playing Next" aria-label="Playing Next queue"
                onClick=${() => Router.navigate(view === 'queue' ? 'browse' : 'queue')}>
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

// ─── Mount ────────────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
  const sidebarRoot = document.getElementById('sidebar-root');
  const topbarRoot = document.getElementById('topbar-root');
  if (sidebarRoot) render(html`<${Sidebar} />`, sidebarRoot);
  if (topbarRoot) render(html`<${TopBar} />`, topbarRoot);
});
