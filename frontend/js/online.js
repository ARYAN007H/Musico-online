/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — Browse (YouTube Music via InnerTube) + fullscreen vinyl
   Depends on: Bridge, App (for artFor/showToast/navigateTo). Loaded after app.js.
   ══════════════════════════════════════════════════════════════════════════════ */

const Online = (() => {
  // videoId → OnlineTrack, filled by search; lets us decorate the now-playing
  // UI with the real thumbnail when the engine reports a `yt:<id>` song.
  const trackMap = new Map();
  let searchSeq = 0;

  // ─── Search ─────────────────────────────────────────────────────────────────
  async function search(query, { videos = false } = {}) {
    const seq = ++searchSeq;
    setStatus('Searching YouTube Music…');
    renderSkeleton();
    try {
      const tracks = await Bridge.invoke('online_search', { query, videos });
      if (seq !== searchSeq) return; // a newer search superseded this one
      if (!tracks || !tracks.length) {
        setStatus(`No results for “${query}”.`);
        renderResults([]);
        return;
      }
      tracks.forEach((t) => trackMap.set(t.video_id, t));
      setStatus(`${tracks.length} results`);
      renderResults(tracks);
    } catch (e) {
      if (seq !== searchSeq) return;
      setStatus(`Search failed: ${e}`);
      renderResults([]);
    }
  }

  function setStatus(text) {
    const el = document.getElementById('browse-status');
    if (el) el.textContent = text || '';
  }

  // ─── Results rendering ──────────────────────────────────────────────────────
  function fmtTime(secs) {
    if (!secs && secs !== 0) return '';
    const m = Math.floor(secs / 60);
    const s = String(Math.floor(secs % 60)).padStart(2, '0');
    return `${m}:${s}`;
  }

  function renderSkeleton() {
    const wrap = document.getElementById('browse-results');
    if (!wrap) return;
    wrap.innerHTML = Array.from({ length: 6 }, () => `
      <div class="browse-row skeleton">
        <div class="browse-thumb skel-box"></div>
        <div class="browse-meta">
          <div class="skel-box skel-line"></div>
          <div class="skel-box skel-line short"></div>
        </div>
      </div>`).join('');
  }

  function renderResults(tracks) {
    const wrap = document.getElementById('browse-results');
    if (!wrap) return;
    if (!tracks.length) {
      wrap.innerHTML = '';
      return;
    }
    wrap.innerHTML = tracks.map((t, i) => {
      const dur = t.duration_secs ? fmtTime(t.duration_secs) : '';
      const sub = [t.artist, t.album, dur].filter(Boolean).join(' · ');
      const thumb = t.thumbnail
        ? `<img class="browse-thumb" src="${escapeAttr(t.thumbnail)}" alt="" loading="lazy"/>`
        : `<div class="browse-thumb browse-thumb-fallback">${svgMusic()}</div>`;
      return `
        <div class="browse-row" role="button" tabindex="0" data-video-id="${escapeAttr(t.video_id)}" data-idx="${i}"
             aria-label="Play ${escapeAttr(t.title)} by ${escapeAttr(t.artist)}">
          ${thumb}
          <div class="browse-meta">
            <div class="browse-row-title">${escapeHtml(t.title)}</div>
            <div class="browse-row-sub">${escapeHtml(sub)}</div>
          </div>
          <button class="browse-play" title="Play now" aria-label="Play ${escapeAttr(t.title)}">
            <svg viewBox="0 0 24 24" fill="currentColor"><polygon points="8,5 20,12 8,19"/></svg>
          </button>
        </div>`;
    }).join('');

    wrap.querySelectorAll('.browse-row').forEach((row) => {
      const track = tracks[Number(row.dataset.idx)];
      const activate = () => playTrack(track, row);
      row.addEventListener('click', activate);
      row.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(); }
      });
    });
  }

  function renderSkeletonCleanup() {}

  // ─── Playback ────────────────────────────────────────────────────────────────
  async function playTrack(track, rowEl) {
    if (!track) return;
    trackMap.set(track.video_id, track);
    document.querySelectorAll('.browse-row.loading').forEach((r) => r.classList.remove('loading'));
    rowEl?.classList.add('loading');
    App.showToast(`Fetching “${track.title}”…`);
    const ok = await Bridge.invoke('online_play', { track });
    rowEl?.classList.remove('loading');
    if (ok === null) {
      // Bridge.invoke already surfaced the error toast
    } else {
      App.navigateTo('now-playing');
    }
  }

  // ─── online-status events (resolving / playing / error) ─────────────────────
  Bridge.listen('online-status', (e) => {
    const p = e.payload ?? e;
    if (p.stage === 'error') {
      App.showToast(`Online playback failed: ${p.message || 'unknown error'}`);
    }
  });

  // ─── Now-playing decoration for online tracks ────────────────────────────────
  // The engine reports `yt:<videoId>` songs without thumbnails; we know them.
  Bridge.listen('playback-event', (e) => {
    const p = e.payload ?? e;
    if (p.type !== 'Playing' || !p.song) return;
    if (!String(p.song.id).startsWith('yt:')) return;
    const track = trackMap.get(String(p.song.id).slice(3));
    if (!track?.thumbnail) return;
    const art = document.getElementById('np-art');
    if (!art) return;
    art.style.background = `url("${track.thumbnail}") center/cover no-repeat, var(--surface-raised)`;
    const icon = art.querySelector('.np-art-icon');
    if (icon) icon.style.display = 'none';
  });

  // ─── Fullscreen player (vinyl lives here and only here) ──────────────────────
  function setFullscreen(on) {
    document.body.classList.toggle('player-fullscreen', on);
    const btn = document.getElementById('btn-fullscreen');
    if (btn) btn.setAttribute('aria-pressed', String(on));
  }

  function toggleFullscreen() {
    setFullscreen(!document.body.classList.contains('player-fullscreen'));
  }

  function setupFullscreen() {
    document.getElementById('btn-fullscreen')?.addEventListener('click', toggleFullscreen);
    document.getElementById('btn-fs-close')?.addEventListener('click', () => setFullscreen(false));
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && document.body.classList.contains('player-fullscreen')) {
        setFullscreen(false);
      }
    });
  }

  // ─── Wiring ──────────────────────────────────────────────────────────────────
  function svgMusic() {
    return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>';
  }

  function escapeHtml(s) {
    return String(s ?? '').replace(/[&<>"']/g, (c) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    }[c]));
  }

  function escapeAttr(s) {
    return escapeHtml(s);
  }

  function init() {
    const form = document.getElementById('browse-form');
    const input = document.getElementById('browse-input');
    form?.addEventListener('submit', (e) => {
      e.preventDefault();
      const q = input.value.trim();
      if (q) search(q);
    });
    document.querySelectorAll('.browse-chip').forEach((chip) => {
      chip.addEventListener('click', () => {
        input.value = chip.dataset.q;
        search(chip.dataset.q);
      });
    });

    setupFullscreen();

    // Surface yt-dlp availability once (helps first-run setup).
    Bridge.invoke('online_available').then((ok) => {
      if (!ok) {
        const el = document.getElementById('browse-status');
        if (el) el.textContent = 'yt-dlp is not installed — online playback needs it (pacman -S yt-dlp).';
      }
    });
  }

  document.addEventListener('DOMContentLoaded', init);

  return { search, playTrack, toggleFullscreen, trackMap };
})();
