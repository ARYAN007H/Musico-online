/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO — Main Application Controller
   View routing, state management, UI population, event handlers
   ══════════════════════════════════════════════════════════════════════════════ */

const App = (() => {
  // ─── State ─────────────────────────────────────────────────────────────────
  // Liked songs: the Rust backend is the source of truth (feeds the
  // recommender); localStorage is a mirror for offline/preview mode.
  let likedSongs;
  try { likedSongs = new Set(JSON.parse(localStorage.getItem('musico-likes') || '[]')); }
  catch (e) { likedSongs = new Set(); }

  // One-time merge of localStorage likes into the backend, then adopt the
  // backend's set (covers likes made in other sessions).  In preview mode the
  // bridge mocks make this a no-op round-trip.
  async function syncLikesWithBackend() {
    try {
      const backendIds = await Bridge.invoke('get_liked_ids');
      const backend = new Set(backendIds || []);
      for (const id of likedSongs) {
        if (!backend.has(id)) {
          await Bridge.invoke('set_song_like', { songId: id, liked: true });
        }
      }
      const merged = new Set(await Bridge.invoke('get_liked_ids'));
      if (merged.size > 0 || backend.size > 0) {
        likedSongs = merged;
        state.likedSongs = merged;
        try { localStorage.setItem('musico-likes', JSON.stringify([...merged])); } catch (e) {}
      }
    } catch (e) { /* no backend — keep localStorage as-is */ }
  }

  const state = {
    currentView: 'browse',
    isPlaying: false,
    currentSong: null,
    position: 0,
    duration: 0,
    volume: 0.68,
    shuffleMode: 0, // 0=off, 1=shuffle, 2=smart
    repeatMode: 0,  // 0=off, 1=one, 2=all
    isLiked: false,
    likedSongs,
    likedOnly: false,
    library: [],
    queue: [],
    recommendations: [],    sortField: 'title',
    sortAscending: true,
    viewMode: 'list',
    searchQuery: '',
    palette: 'nebula',
    settings: {},
  };

  // ─── Generated Art ──────────────────────────────────────────────────────────
  // Deterministic gradient per song (hash of id → curated palette pair) so the
  // app has real visual identity even with zero embedded covers. Real covers
  // (get_cover) override this on the vinyl + dock via loadCover().
  const ART_PALETTES = [
    ['#8b7ad6', '#4a3f8f'], ['#e0709e', '#8f2f56'], ['#4fbf95', '#1a6b4c'],
    ['#54a8e0', '#1f5f8a'], ['#e0a04f', '#8f5a1f'], ['#9d8cff', '#c46b8a'],
    ['#52c4b0', '#2a7a9e'], ['#b08968', '#5a3d6e'],
  ];

  function artFor(song) {
    let h = 0;
    const key = String(song?.id ?? 'x');
    for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) | 0;
    const [a, b] = ART_PALETTES[Math.abs(h) % ART_PALETTES.length];
    return `linear-gradient(135deg, ${a}, ${b})`;
  }

  // ─── Initialization ────────────────────────────────────────────────────────
  async function init() {
    setupNavigation();
    // Router owns history; _activateView renders.  Emits the current view now.
    if (window.MusicoRouter) {
      window.MusicoRouter.init((view) => _activateView(view));
    } else {
      _activateView(state.currentView);
    }
    setupSettingsTabs();
    setupSearch();
    setupSortControls();
    setupViewToggle();
    setupTransportControls();
    setupToggles();
    setupPaletteSwatches();
    setupSettingsPersistence();
    setupEq();
    setupScanButton();
    setupLikedFilter();
    setupQueueClear();

    // Load library
    state.library = await Bridge.invoke('get_library');
    renderSongList(state.library);
    renderAlbumGrid(state.library);
    generateWaveform();

    // Publish autoplay for the Playing Next panel
    try {
      const st = await Bridge.invoke('get_settings');
      window.MusicoStore?.set('autoplay', !!st?.autoplay);
    } catch (e) { /* preview mock path */ }

    // Merge likes into the backend recommender before first rec request
    await syncLikesWithBackend();

    // Load recommendations
    state.recommendations = await Bridge.invoke('get_recommendations');
    renderRecommendations();

    // Update library count
    document.getElementById('lib-count').textContent = `${state.library.length} songs`;

    // Live engine events (position, song changes, queue updates)
    initPlaybackEvents();

    // Restore engine truth on startup (volume, mid-play state)
    const ps = await Bridge.invoke('get_playback_state');
    if (ps) {
      Knob.setVolume(ps.volume ?? 0.68, false);
      if (ps.current_song) {
        state.currentSong = ps.current_song;
        state.duration = ps.duration_secs;
        state.position = ps.position_secs;
        setNowPlayingUi(ps.current_song);
        updateProgress();
      }
      state.isPlaying = ps.status === 'Playing';
      updatePlayState();
    }

    renderQueue(await Bridge.invoke('get_queue'));

    // Persisted settings: apply palette, toggles, folder, view mode
    state.settings = (await Bridge.invoke('get_settings')) || {};
    applySettings(state.settings);
  }

  // ─── Settings: apply + persist ────────────────────────────────────────────
  const TOGGLE_KEYS = {
    'toggle-eq': 'eq_enabled',
    'toggle-crossfade': 'crossfade_enabled',
    'toggle-art-tint': 'art_tint',
    'toggle-smart-rec': 'smart_recs',
  };

  function applySettings(s) {
    if (!s) return;
    document.documentElement.setAttribute('data-theme', s.theme === 'dark' ? 'dark' : 'light');
    try { localStorage.setItem('musico-theme', s.theme === 'dark' ? 'dark' : 'light'); } catch (e) {}
    document.querySelectorAll('[data-theme-pill]').forEach((p) =>
      p.classList.toggle('active', p.dataset.themePill === (s.theme === 'dark' ? 'dark' : 'light')));
    if (s.palette) {
      document.documentElement.setAttribute('data-palette', s.palette);
      document.querySelectorAll('.swatch').forEach((sw) =>
        sw.classList.toggle('selected', sw.dataset.palette === s.palette));
    }
    Object.entries(TOGGLE_KEYS).forEach(([id, key]) => {
      const el = document.getElementById(id);
      if (el) el.classList.toggle('on', !!s[key]);
    });
    if (s.music_folder) {
      const tag = document.getElementById('folder-tag');
      if (tag) {
        const short = s.music_folder.replace(/^\/home\/[^/]+/, '~');
        tag.lastChild.textContent = ` ${short}`;
      }
    }
    if (s.library_view_mode === 'grid') {
      document.getElementById('toggle-grid')?.click();
    }
    if (typeof s.shuffle_mode === 'number') {
      state.shuffleMode = s.shuffle_mode;
      document.getElementById('btn-shuffle')?.classList.toggle('active', s.shuffle_mode > 0);
    }
    if (typeof s.repeat_mode === 'number') {
      state.repeatMode = s.repeat_mode;
      document.getElementById('btn-repeat')?.classList.toggle('active', s.repeat_mode > 0);
    }
    updateModeIndicator();
    // Normalization pills
    document.querySelectorAll('[data-normalization]').forEach((pill) => {
      pill.classList.toggle('active', pill.dataset.normalization === s.normalization);
    });
    // EQ: restore preset curve
    const presetName = s.eq_preset && EQ_PRESETS[s.eq_preset] ? s.eq_preset : 'Flat';
    document.querySelectorAll('#eq-presets .sort-pill').forEach((p) => {
      p.classList.toggle('active', p.dataset.eqPreset === presetName);
    });
    const gains = EQ_PRESETS[presetName];
    document.querySelectorAll('.eq-slider').forEach((sl, i) => {
      if (gains && gains[i] !== undefined) {
        sl.value = gains[i];
        sl.parentElement.querySelector('.eq-band-value').textContent = gains[i];
      }
    });
    if (s.eq_enabled && typeof Bridge !== 'undefined') applyEq(gains ?? []);
    // Crossfade slider
    const slider = document.querySelector('.s-slider');
    if (slider && s.crossfade_duration) {
      slider.value = s.crossfade_duration;
      const val = document.querySelector('.s-value');
      if (val) val.textContent = `${Number(s.crossfade_duration).toFixed(1)}s`;
    }
  }

  function persistSettings(patch) {
    Object.assign(state.settings, patch);
    Bridge.invoke('save_settings', { settings: state.settings });
  }

  function setupSettingsPersistence() {
    // Toggles
    Object.entries(TOGGLE_KEYS).forEach(([id, key]) => {
      const el = document.getElementById(id);
      if (!el) return;
      el.addEventListener('click', () => persistSettings({ [key]: el.classList.contains('on') }));
    });

    // Theme pills
    document.querySelectorAll('[data-theme-pill]').forEach((pill) => {
      pill.addEventListener('click', () => {
        document.querySelectorAll('[data-theme-pill]').forEach((p) => p.classList.remove('active'));
        pill.classList.add('active');
        const theme = pill.dataset.themePill;
        document.documentElement.setAttribute('data-theme', theme);
        try { localStorage.setItem('musico-theme', theme); } catch (e) {}
        persistSettings({ theme });
      });
    });

    // Music folder row → native picker
    document.getElementById('row-music-folder')?.addEventListener('click', async () => {
      const tag = document.getElementById('folder-tag');
      const chosen = await Bridge.invoke('pick_folder');
      if (chosen && tag) {
        const short = chosen.replace(/^\/home\/[^/]+/, '~');
        tag.lastChild.textContent = ` ${short}`;
      }
    });

    // Normalization pills (data-normalization keeps this independent of card order)
    document.querySelectorAll('[data-normalization]').forEach((pill) => {
      pill.addEventListener('click', () => {
        pill.parentElement.querySelectorAll('.sort-pill').forEach((p) => p.classList.remove('active'));
        pill.classList.add('active');
        persistSettings({ normalization: pill.dataset.normalization });
        Bridge.invoke('set_normalization', { mode: pill.dataset.normalization });
      });
    });

    // Crossfade duration slider (toggling crossfade stays an explicit choice)
    const slider = document.querySelector('.s-slider');
    slider?.addEventListener('input', () => {
      const val = document.querySelector('.s-value');
      if (val) val.textContent = `${Number(slider.value).toFixed(1)}s`;
      persistSettings({ crossfade_duration: Number(slider.value) });
    });
  }

  // ─── Playback Event Handling (replaces fake progress simulation) ──────────
  function initPlaybackEvents() {
    Bridge.listen('playback-event', (e) => handlePlaybackEvent(e.payload ?? e));
    Bridge.listen('queue-changed', (e) => renderQueue(e.payload ?? e));
  }

  function handlePlaybackEvent(ev) {
    if (!ev || !ev.type) return;
    switch (ev.type) {
      case 'Playing': {
        const song = ev.song;
        const isNew = !state.currentSong || state.currentSong.id !== song.id;
        state.currentSong = song;
        state.isPlaying = true;
        if (isNew) {
          state.duration = song.duration_secs;
          state.position = 0;
          state.isLiked = likedSongs.has(song.id);
          publishHistory(song);
          setNowPlayingUi(song);
          loadCover(song);
          loadLyrics(song);
          generateWaveform(song.id);
          updateSongListPlaying();
        }
        updatePlayState();
        break;
      }
      case 'PositionUpdate':
      case 'Seeked':
        // Don't fight the user's thumb mid-drag; their position wins.
        if (window.Seek && typeof Seek.isDragging === 'function' && Seek.isDragging()) break;
        state.position = ev.position_secs;
        updateProgress();
        publishPlayback();
        updateLyricsHighlight(ev.position_secs);
        break;
      case 'Paused':
      case 'Stopped':
      case 'SongEnded':
        state.isPlaying = false;
        if (ev.type !== 'Paused') {
          state.position = 0;
          updateProgress();
        }
        updatePlayState();
        break;
      case 'Resumed':
        state.isPlaying = true;
        updatePlayState();
        break;
      case 'VolumeChanged':
        Knob.setVolume(ev.volume, false);
        state.volume = ev.volume;
        publishPlayback();
        break;
      case 'Error':
        showError(ev.message || 'Playback error');
        break;
    }
  }

  // ─── Store publishing (Preact shell consumption) ──────────────────────────
  function publishPlayback() {
    const s = state;
    let thumb = null;
    if (s.currentSong && String(s.currentSong.id).startsWith('yt:')) {
      thumb = window.Online?.trackMap?.get(String(s.currentSong.id).slice(3))?.thumbnail || null;
    }
    window.MusicoStore?.set('playback', {
      hasSong: !!s.currentSong,
      id: s.currentSong?.id || null,
      title: s.currentSong?.title || '',
      artist: s.currentSong ? (s.currentSong.artist || '') : '',
      artGrad: s.currentSong ? artFor(s.currentSong) : null,
      thumb,
      isPlaying: !!s.isPlaying,
      position: s.position || 0,
      duration: s.duration || 0,
      volume: s.volume,
    });
  }

  function publishHistory(song) {
    if (!song) return;
    let thumb = null;
    if (String(song.id).startsWith('yt:')) {
      thumb = window.Online?.trackMap?.get(String(song.id).slice(3))?.thumbnail || null;
    }
    const entry = {
      id: song.id, title: song.title, artist: song.artist,
      album: song.album || '', duration_secs: song.duration_secs,
      artGrad: artFor(song), thumb,
    };
    const hist = (window.MusicoStore?.get('history') || []).filter((h) => h.id !== song.id);
    hist.unshift(entry);
    window.MusicoStore?.set('history', hist.slice(0, 20));
  }

  // ─── Cover Art (disk-cached thumbnails, fetched per song) ─────────────────
  const coverCache = new Map();

  function loadCover(song) {
    if (coverCache.has(song.id)) {
      Vinyl.setAlbumArt(coverCache.get(song.id));
      return;
    }
    Bridge.invoke('get_cover', { songId: song.id }).then((url) => {
      coverCache.set(song.id, url ?? null);
      if (state.currentSong && state.currentSong.id === song.id) {
        Vinyl.setAlbumArt(url ?? null);
      }
    });
  }

  // ─── Lyrics (sidecar .lrc, karaoke highlight) ─────────────────────────────
  let lyricLines = []; // { time_secs, el }

  function loadLyrics(song) {
    lyricLines = [];
    const section = document.getElementById('lyrics-section');
    const list = document.getElementById('lyrics-list');
    section.style.display = 'none';
    Bridge.invoke('get_lyrics', { songId: song.id }).then((dto) => {
      if (!dto || !dto.lines || !dto.lines.length) return;
      if (state.currentSong && state.currentSong.id !== song.id) return;
      list.innerHTML = '';
      lyricLines = dto.lines.map((line) => {
        const el = document.createElement('div');
        el.className = 'lyric-line';
        el.textContent = line.text;
        list.appendChild(el);
        return { time_secs: line.time_secs, el };
      });
      section.style.display = '';
      updateLyricsHighlight(state.position);
    });
  }

  function updateLyricsHighlight(position) {
    if (!lyricLines.length) return;
    let active = -1;
    for (let i = 0; i < lyricLines.length; i++) {
      if (lyricLines[i].time_secs <= position) active = i;
      else break;
    }
    lyricLines.forEach((l, i) => {
      l.el.classList.toggle('active', i === active);
      l.el.classList.toggle('past', active !== -1 && i < active);
    });
    if (active >= 0) {
      lyricLines[active].el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    }
  }

  // Push the song's identity into all the Now Playing + mini player surfaces
  function setNowPlayingUi(song) {
    document.getElementById('np-title').textContent = song.title;
    document.getElementById('np-artist').textContent = `${song.artist} · ${song.album}`;
    publishPlayback();
    document.getElementById('time-total').textContent = formatTime(song.duration_secs);
    document.getElementById('time-current').textContent = formatTime(state.position);

    document.getElementById('mini-title').textContent = song.title;
    document.getElementById('mini-artist').textContent = song.artist;

    // One art identity across vinyl label + dock (real covers override later)
    const grad = artFor(song);
    document.getElementById('vinyl-label')?.style.setProperty('--art-grad', grad);
    const miniArt = document.getElementById('mini-art');
    if (miniArt) miniArt.style.background = grad;
    // Art square (normal-mode now playing) + reset any online thumbnail
    const npArt = document.getElementById('np-art');
    if (npArt) {
      npArt.style.background = grad;
      const icon = npArt.querySelector('.np-art-icon');
      if (icon) icon.style.display = '';
    }

    // Reflect persisted like state for this song
    state.isLiked = likedSongs.has(song.id);
    const likeBtn = document.getElementById('btn-like');
    likeBtn.classList.toggle('liked', state.isLiked);
    likeBtn.setAttribute('aria-pressed', String(state.isLiked));
    likeBtn.querySelector('svg').setAttribute('fill', state.isLiked ? 'currentColor' : 'none');
  }

  // ─── Queue Rendering ──────────────────────────────────────────────────────
  function renderQueue(songs) {
    state.queue = songs || [];
    window.MusicoStore?.set('queue', state.queue);
    const container = document.getElementById('queue-list');
    if (!container) return;

    const totalSecs = state.queue.reduce((acc, s) => acc + (s.duration_secs || 0), 0);
    document.getElementById('queue-count').textContent = state.queue.length
      ? `${state.queue.length} songs · ${Math.round(totalSecs / 60)} min`
      : '0 songs';
    const clearBtn = document.getElementById('btn-clear-queue');
    if (clearBtn) clearBtn.hidden = state.queue.length === 0;

    if (state.queue.length === 0) {
      container.innerHTML = `
        <div class="queue-empty" id="queue-empty">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>
          <div style="text-align:center;">
            <div style="font-weight:var(--weight-semi); color:var(--text-primary); margin-bottom:4px;">Queue is empty</div>
            <div style="font-size:var(--text-caption);">Add songs from Library or let Smart Radio fill it</div>
          </div>
        </div>`;
      return;
    }

    container.innerHTML = '';
    state.queue.forEach((song, i) => {
      const isPlaying = state.currentSong?.id === song.id;
      const row = document.createElement('div');
      row.className = `song-row${isPlaying ? ' playing' : ''}`;
      row.setAttribute('role', 'button');
      row.tabIndex = 0;
      if (isPlaying) row.setAttribute('aria-current', 'true');
      row.setAttribute('aria-label', `Play ${song.title} by ${song.artist}`);
      row.innerHTML = `
        <div class="song-num">${isPlaying
          ? '<div class="eq-bars"><span></span><span></span><span></span></div>'
          : i + 1}</div>
        <div class="song-art" style="background:${artFor(song)}">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>
        </div>
        <div class="song-info">
          <div class="song-name">${song.title}</div>
          <div class="song-detail">${song.artist} · ${song.album}</div>
        </div>
        <button class="song-queue-btn" title="Remove from queue" aria-label="Remove ${song.title} from queue">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M18 6L6 18M6 6l12 12"/></svg>
        </button>`;

      const activate = (e) => {
        if (e.target.closest('.song-queue-btn')) {
          Bridge.invoke('remove_from_queue', { songId: song.id });
          return;
        }
        playSong(song);
      };
      row.addEventListener('click', activate);
      row.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(e); }
      });
      container.appendChild(row);
    });
  }

  function setupQueueClear() {
    const btn = document.getElementById('btn-clear-queue');
    if (!btn) return;
    btn.addEventListener('click', () => {
      if (!state.queue.length) return;
      Bridge.invoke('clear_queue');
      showToast('Queue cleared');
    });
  }

  // ─── Scan Button (async, progress via engine events) ──────────────────────
  function setupScanButton() {
    const btn = document.getElementById('btn-scan');
    if (!btn) return;

    Bridge.listen('scan-progress', (e) => {
      const p = e.payload ?? e;
      document.getElementById('lib-count').textContent = `Scanning ${p.done}/${p.total}…`;
    });

    Bridge.listen('library-updated', (e) => {
      state.library = e.payload ?? e;
      renderSongList(state.library);
      renderAlbumGrid(state.library);
      document.getElementById('lib-count').textContent = `${state.library.length} songs`;
      btn.disabled = false;
      // Refresh recs now that the library exists
      Bridge.invoke('get_recommendations').then((recs) => {
        state.recommendations = recs;
        renderRecommendations();
      });
      const hint = document.getElementById('scan-hint');
      if (hint) hint.textContent = `${state.library.length} songs · Scanned just now`;
    });

    Bridge.listen('index-progress', (e) => {
      const p = e.payload ?? e;
      const hint = document.getElementById('scan-hint');
      if (hint) hint.textContent = `Learning your library… ${p.done}/${p.total}`;
    });

    Bridge.listen('index-complete', () => {
      const hint = document.getElementById('scan-hint');
      if (hint && state.library.length) hint.textContent = `${state.library.length} songs · Ready`;
      Bridge.invoke('get_recommendations').then((recs) => {
        state.recommendations = recs;
        renderRecommendations();
      });
    });

    btn.addEventListener('click', () => {
      btn.disabled = true;
      document.getElementById('lib-count').textContent = 'Scanning…';
      Bridge.invoke('start_scan');
    });
  }

  // ─── Navigation ────────────────────────────────────────────────────────────
  // Delegated: covers Preact-rendered sidebar items, the static bottom pill,
  // and anything future.  The router owns history; _activateView owns DOM.
  function setupNavigation() {
    document.addEventListener('click', (e) => {
      // .nav-item scope: other [data-view] attrs (library list/grid toggles)
      // are view preferences, not navigation.
      const item = e.target.closest('.nav-item[data-view]');
      if (!item || item.disabled) return;
      navigateTo(item.dataset.view);
    });
  }

  function navigateTo(viewName) {
    // Router owns the history stack; _activateView does the DOM switch.
    if (window.MusicoRouter) MusicoRouter.navigate(viewName);
    else _activateView(viewName);
  }

  function _activateView(viewName) {
    // Catalog detail routes (#/browse/<payload>) render the Preact DetailView.
    if (window.MusicoRouter?.isDetail?.(viewName)) {
      const target = window.MusicoRouter.parseDetailRoute(viewName);
      if (target) {
        const prev = window.MusicoStore?.get('detail');
        const changed = !prev || prev.browseId !== target.browseId
          || (prev.params || '') !== (target.params || '');
        window.MusicoStore?.set('detail', target);
        if (changed) window.MusicoStore?.set('detailPage', null);
      }
      document.querySelectorAll('.view').forEach(v => v.classList.remove('active'));
      document.getElementById('view-detail')?.classList.add('active');
      document.querySelectorAll('#bottom-nav .nav-item').forEach(n => n.classList.remove('active'));
      state.currentView = 'detail';
      // Publish so the topbar arrows re-render for the pushed history entry.
      window.MusicoStore?.set('view', 'detail');
      return;
    }

    state.currentView = viewName;
    window.MusicoStore?.set('view', viewName);

    // Update views
    document.querySelectorAll('.view').forEach(v => v.classList.remove('active'));
    const target = document.getElementById(`view-${viewName}`);
    if (target) target.classList.add('active');

    // Bottom pill active state (sidebar highlight is Preact/store-driven)
    document.querySelectorAll('#bottom-nav .nav-item').forEach(n => n.classList.remove('active'));
    const activeNav = document.querySelector(`#bottom-nav .nav-item[data-view="${viewName}"]`);
    if (activeNav) activeNav.classList.add('active');

    // Dock lives in the sidebar (≥860px) — show whenever a song exists.
    // body.has-mini only matters in compact mode where the strip overlays content.
    const miniPlayer = document.getElementById('mini-player');
    miniPlayer.classList.toggle('hidden', !state.currentSong);
    document.body.classList.toggle('has-mini', !!state.currentSong && window.innerWidth < 860);

    if (viewName === 'playlists') renderStats();
  }

  // ─── Settings Tabs ─────────────────────────────────────────────────────────
  function setupSettingsTabs() {
    document.querySelectorAll('.settings-tab').forEach(tab => {
      tab.addEventListener('click', () => {
        document.querySelectorAll('.settings-tab').forEach(t => t.classList.remove('active'));
        tab.classList.add('active');

        document.querySelectorAll('.settings-panel').forEach(p => p.classList.remove('active'));
        const panel = document.getElementById(`panel-${tab.dataset.tab}`);
        if (panel) panel.classList.add('active');
      });
    });
  }

  // ─── Search ────────────────────────────────────────────────────────────────
  function setupSearch() {
    const input = document.getElementById('search-input');
    const bar = document.getElementById('search-bar');
    const clear = document.getElementById('search-clear');

    input.addEventListener('input', () => {
      state.searchQuery = input.value;
      bar.classList.toggle('has-query', input.value.length > 0);
      filterLibrary();
    });

    clear.addEventListener('click', () => {
      input.value = '';
      state.searchQuery = '';
      bar.classList.remove('has-query');
      filterLibrary();
    });

    input.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        input.value = '';
        state.searchQuery = '';
        bar.classList.remove('has-query');
        filterLibrary();
        input.blur();
      }
    });
  }

  function filterLibrary() {
    const q = state.searchQuery.toLowerCase();
    let filtered = state.library;
    if (state.likedOnly) {
      filtered = filtered.filter(s => likedSongs.has(s.id));
    }
    if (q) {
      filtered = filtered.filter(s =>
        s.title.toLowerCase().includes(q) ||
        s.artist.toLowerCase().includes(q) ||
        s.album.toLowerCase().includes(q)
      );
    }

    const sorted = sortSongs(filtered);
    renderSongList(sorted);
    renderAlbumGrid(sorted);

    const scope = state.likedOnly ? 'liked song' + (filtered.length === 1 ? '' : 's') : 'songs';
    document.getElementById('lib-count').textContent = (q || state.likedOnly)
      ? `${filtered.length} ${q ? 'results' : scope}`
      : `${state.library.length} songs`;
  }

  // ─── Liked filter pill ──────────────────────────────────────────────────────
  function setupLikedFilter() {
    const pill = document.getElementById('filter-liked');
    if (!pill) return;
    pill.addEventListener('click', () => {
      state.likedOnly = !state.likedOnly;
      pill.classList.toggle('active', state.likedOnly);
      pill.setAttribute('aria-pressed', String(state.likedOnly));
      filterLibrary();
    });
  }

  // ─── Sort ──────────────────────────────────────────────────────────────────
  function setupSortControls() {
    document.querySelectorAll('.sort-pill[data-sort]').forEach(pill => {
      pill.addEventListener('click', () => {
        const field = pill.dataset.sort;
        if (state.sortField === field) {
          state.sortAscending = !state.sortAscending;
        } else {
          state.sortField = field;
          state.sortAscending = true;
        }

        // Update pills
        document.querySelectorAll('.sort-pill[data-sort]').forEach(p => {
          p.classList.remove('active');
          p.querySelector('.sort-arrow')?.remove();
        });
        pill.classList.add('active');
        const arrow = document.createElement('span');
        arrow.className = 'sort-arrow';
        arrow.textContent = state.sortAscending ? ' ↑' : ' ↓';
        pill.appendChild(arrow);

        filterLibrary();
      });
    });
  }

  function sortSongs(songs) {
    return [...songs].sort((a, b) => {
      let va = a[state.sortField] || '';
      let vb = b[state.sortField] || '';
      if (state.sortField === 'duration' || state.sortField === 'duration_secs') {
        va = a.duration_secs || 0;
        vb = b.duration_secs || 0;
        return state.sortAscending ? va - vb : vb - va;
      }
      va = String(va).toLowerCase();
      vb = String(vb).toLowerCase();
      const cmp = va.localeCompare(vb);
      return state.sortAscending ? cmp : -cmp;
    });
  }

  // ─── View Toggle ───────────────────────────────────────────────────────────
  function setupViewToggle() {
    document.getElementById('toggle-list').addEventListener('click', () => {
      state.viewMode = 'list';
      document.getElementById('toggle-list').classList.add('active');
      document.getElementById('toggle-grid').classList.remove('active');
      document.getElementById('song-list-view').style.display = '';
      document.getElementById('album-grid-view').style.display = 'none';
      persistSettings({ library_view_mode: 'list' });
    });

    document.getElementById('toggle-grid').addEventListener('click', () => {
      state.viewMode = 'grid';
      document.getElementById('toggle-grid').classList.add('active');
      document.getElementById('toggle-list').classList.remove('active');
      document.getElementById('song-list-view').style.display = 'none';
      document.getElementById('album-grid-view').style.display = '';
      persistSettings({ library_view_mode: 'grid' });
    });
  }

  // ─── Transport Controls ────────────────────────────────────────────────────
  function setupTransportControls() {
    document.getElementById('btn-play').addEventListener('click', togglePlay);
    document.getElementById('mini-play').addEventListener('click', togglePlay);
    document.getElementById('btn-prev').addEventListener('click', () => Bridge.invoke('previous'));
    document.getElementById('btn-next').addEventListener('click', () => Bridge.invoke('next'));
    document.getElementById('mini-prev')?.addEventListener('click', () => Bridge.invoke('previous'));
    document.getElementById('mini-next').addEventListener('click', () => Bridge.invoke('next'));
    document.getElementById('mini-expand')?.addEventListener('click', () => navigateTo('now-playing'));

    // Add the current song to the queue (the "+" under the title)
    document.getElementById('btn-queue-current')?.addEventListener('click', () => {
      if (!state.currentSong) return;
      Bridge.invoke('add_to_queue', { song: state.currentSong });
      showToast(`Added "${state.currentSong.title}" to queue`);
    });

    document.getElementById('btn-shuffle').addEventListener('click', cycleShuffle);
    document.getElementById('btn-repeat').addEventListener('click', cycleRepeat);

    document.getElementById('btn-like').addEventListener('click', () => {
      if (!state.currentSong) return;
      state.isLiked = !state.isLiked;
      const songId = state.currentSong.id;
      if (state.isLiked) {
        likedSongs.add(songId);
        showToast('Added to Liked');
      } else {
        likedSongs.delete(songId);
        showToast('Removed from Liked');
      }
      // Backend feed (profile/affinity/cooldown signals) + localStorage mirror.
      Bridge.invoke('set_song_like', { songId, liked: state.isLiked });
      try { localStorage.setItem('musico-likes', JSON.stringify([...likedSongs])); } catch (e) {}
      const btn = document.getElementById('btn-like');
      btn.classList.toggle('liked', state.isLiked);
      btn.setAttribute('aria-pressed', String(state.isLiked));
      btn.querySelector('svg').setAttribute('fill', state.isLiked ? 'currentColor' : 'none');
      if (state.likedOnly) filterLibrary();
    });

    // Clicking the mini player (not its buttons) returns to Now Playing
    document.getElementById('mini-player')?.addEventListener('click', (e) => {
      if (e.target.closest('button')) return;
      navigateTo('now-playing');
    });

    // Keyboard shortcuts
    document.addEventListener('keydown', (e) => {
      if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;
      switch (e.key) {
        case ' ': e.preventDefault(); togglePlay(); break;
        case 'n': case 'N': Bridge.invoke('next'); break;
        case 'p': case 'P': Bridge.invoke('previous'); break;
        case 's': case 'S': cycleShuffle(); break;
        case 'r': case 'R': cycleRepeat(); break;
        case 'ArrowUp': e.preventDefault(); Knob.setVolume(Math.min(1, state.volume + 0.05)); break;
        case 'ArrowDown': e.preventDefault(); Knob.setVolume(Math.max(0, state.volume - 0.05)); break;
      }
    });
  }

  function togglePlay() {
    if (!state.currentSong) {
      if (state.library.length > 0) playSong(state.library[0]);
      return;
    }
    // Optimistic flip; Paused/Resumed engine events confirm it
    state.isPlaying = !state.isPlaying;
    updatePlayState();
    Bridge.invoke(state.isPlaying ? 'resume' : 'pause');
  }

  function playSong(song) {
    state.currentSong = song;
    state.isPlaying = true;
    state.position = 0;
    state.duration = song.duration_secs;
    state.isLiked = false;

    setNowPlayingUi(song);
    generateWaveform(song.id);
    updatePlayState();
    updateSongListPlaying();
    updateProgress();

    Bridge.invoke('play_song', { song });
  }

  function updatePlayState() {
    publishPlayback();
    const playing = state.isPlaying;

    // Vinyl
    Vinyl.setPlaying(playing);

    // Play button icon
    const playIcon = document.getElementById('play-icon');
    if (playing) {
      playIcon.innerHTML = '<rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/>';
    } else {
      playIcon.innerHTML = '<polygon points="8,5 20,12 8,19"/>';
    }

    // Mini player
    const miniPlay = document.getElementById('mini-play');
    if (playing) {
      miniPlay.querySelector('svg').innerHTML = '<rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/>';
    } else {
      miniPlay.querySelector('svg').innerHTML = '<polygon points="8,5 20,12 8,19"/>';
    }

    // Dock visibility (sidebar card ≥860px / overlay strip <860px)
    const miniPlayer = document.getElementById('mini-player');
    miniPlayer.classList.toggle('hidden', !state.currentSong);
    document.body.classList.toggle('has-mini', !!state.currentSong && window.innerWidth < 860);
  }

  function cycleShuffle() {
    state.shuffleMode = (state.shuffleMode + 1) % 3;
    document.getElementById('btn-shuffle').classList.toggle('active', state.shuffleMode > 0);
    updateModeIndicator();
    Bridge.invoke('set_playback_modes', { shuffle: state.shuffleMode, repeat: state.repeatMode });
  }

  function cycleRepeat() {
    state.repeatMode = (state.repeatMode + 1) % 3;
    document.getElementById('btn-repeat').classList.toggle('active', state.repeatMode > 0);
    updateModeIndicator();
    Bridge.invoke('set_playback_modes', { shuffle: state.shuffleMode, repeat: state.repeatMode });
  }

  function updateModeIndicator() {
    const modes = [];
    if (state.shuffleMode === 1) modes.push('<span class="mode-active">Shuffle</span>');
    if (state.shuffleMode === 2) modes.push('<span class="mode-active">Smart Radio ✦</span>');
    if (state.repeatMode === 1) modes.push('<span class="mode-active">Repeat One</span>');
    if (state.repeatMode === 2) modes.push('<span class="mode-active">Repeat All</span>');
    document.getElementById('mode-indicator').innerHTML = modes.join(' · ');

    // Keep the buttons' labels truthful — the third shuffle mode (Smart Radio)
    // is invisible otherwise.
    const shuffleNames = ['Shuffle: Off', 'Shuffle: On', 'Shuffle: Smart Radio'];
    const repeatNames = ['Repeat: Off', 'Repeat: One', 'Repeat: All'];
    const shuffleBtn = document.getElementById('btn-shuffle');
    const repeatBtn = document.getElementById('btn-repeat');
    if (shuffleBtn) {
      shuffleBtn.title = shuffleNames[state.shuffleMode];
      shuffleBtn.setAttribute('aria-label', shuffleNames[state.shuffleMode]);
      shuffleBtn.setAttribute('aria-pressed', String(state.shuffleMode > 0));
    }
    if (repeatBtn) {
      repeatBtn.title = repeatNames[state.repeatMode];
      repeatBtn.setAttribute('aria-label', repeatNames[state.repeatMode]);
      repeatBtn.setAttribute('aria-pressed', String(state.repeatMode > 0));
    }
  }

  // ─── Progress (driven entirely by engine PositionUpdate events) ───────────
  function updateProgress() {
    const pct = state.duration > 0 ? (state.position / state.duration) * 100 : 0;

    // Seek bar
    document.getElementById('seek-fill').style.width = `${pct}%`;
    document.getElementById('time-current').textContent = formatTime(state.position);

    // Hide the thumb at position zero (it would overhang the track) + keep ARIA in sync
    const seekContainer = document.querySelector('.seek-container');
    if (seekContainer) seekContainer.classList.toggle('at-start', pct < 0.5);
    const seekSlider = document.getElementById('seek-wrapper');
    if (seekSlider) {
      seekSlider.setAttribute('aria-valuemax', String(Math.round(state.duration)));
      seekSlider.setAttribute('aria-valuenow', String(Math.round(state.position)));
      seekSlider.setAttribute('aria-valuetext', formatTime(state.position));
    }

    // Mini player progress
    document.getElementById('mini-player').style.setProperty('--mini-progress', `${pct}%`);

    // Tonearm tracks the grooves outward → inward
    Vinyl.setProgress(pct / 100);

    // Update waveform
    updateWaveformProgress(pct);
  }

  // ─── Toggles ───────────────────────────────────────────────────────────────
  function toggleSwitch(el) {
    el.classList.toggle('on');
    el.setAttribute('aria-checked', String(el.classList.contains('on')));
  }

  function setupToggles() {
    document.querySelectorAll('.s-toggle').forEach(toggle => {
      toggle.addEventListener('click', () => toggleSwitch(toggle));
      toggle.addEventListener('keydown', (e) => {
        if (e.key === ' ' || e.key === 'Enter') {
          e.preventDefault();
          toggleSwitch(toggle);
        }
      });
    });
  }

  // ─── Palette Swatches ──────────────────────────────────────────────────────
  function setupPaletteSwatches() {
    document.querySelectorAll('.swatch').forEach(swatch => {
      swatch.addEventListener('click', () => selectPalette(swatch));
      swatch.addEventListener('keydown', (e) => {
        if (e.key === ' ' || e.key === 'Enter') {
          e.preventDefault();
          selectPalette(swatch);
        }
      });
    });
  }

  function selectPalette(swatch) {
    const palette = swatch.dataset.palette;
    state.palette = palette;
    document.documentElement.setAttribute('data-palette', palette);

    document.querySelectorAll('.swatch').forEach(s => {
      s.classList.remove('selected');
      s.setAttribute('aria-checked', 'false');
    });
    swatch.classList.add('selected');
    swatch.setAttribute('aria-checked', 'true');
    persistSettings({ palette });
  }

  // ─── Render Song List ──────────────────────────────────────────────────────
  function renderSongList(songs) {
    const container = document.getElementById('song-list-view');
    container.innerHTML = '';

    if (songs.length === 0) {
      container.innerHTML = `
        <div class="empty-state">
          <div class="empty-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>
          </div>
          <div class="empty-title">${state.searchQuery ? 'No results found' : state.likedOnly ? 'No liked songs yet' : 'Your library is empty'}</div>
          <div class="empty-desc">${state.searchQuery
            ? `Nothing matching "${state.searchQuery}"`
            : state.likedOnly
              ? 'Tap the heart on a song to keep it here'
              : ''}</div>
          ${!state.searchQuery && !state.likedOnly ? `
            <button class="neu-btn neu-btn-pill" id="empty-pick-folder">Choose your music folder</button>` : ''}
        </div>`;
      document.getElementById('empty-pick-folder')?.addEventListener('click', () => navigateTo('settings'));
      return;
    }

    songs.forEach((song, i) => {
      const isPlaying = state.currentSong?.id === song.id;
      const row = document.createElement('div');
      row.className = `song-row${isPlaying ? ' playing' : ''}`;
      row.dataset.id = song.id;
      row.dataset.index = i + 1;
      row.setAttribute('role', 'button');
      row.tabIndex = 0;
      if (isPlaying) row.setAttribute('aria-current', 'true');
      row.setAttribute('aria-label', `Play ${song.title} by ${song.artist}`);
      row.innerHTML = `
        <div class="song-num">${isPlaying
          ? '<div class="eq-bars"><span></span><span></span><span></span></div>'
          : i + 1}</div>
        <div class="song-art" style="background:${artFor(song)}">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>
        </div>
        <div class="song-info">
          <div class="song-name">${song.title}</div>
          <div class="song-detail">${song.artist} · ${song.album}</div>
        </div>
        <div class="song-duration">${formatTime(song.duration_secs)}</div>
        <button class="song-queue-btn" title="Add to queue" aria-label="Add ${song.title} to queue">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 5v14M5 12h14"/></svg>
        </button>`;

      const activate = (e) => {
        if (e.target.closest('.song-queue-btn')) {
          Bridge.invoke('add_to_queue', { song });
          showToast(`Added "${song.title}" to queue`);
          return;
        }
        playSong(song);
      };
      row.addEventListener('click', activate);
      row.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(e); }
      });

      container.appendChild(row);
    });
  }

  function renderAlbumGrid(songs) {
    const container = document.getElementById('album-grid-view');
    container.innerHTML = '';

    songs.forEach(song => {
      const card = document.createElement('div');
      card.className = 'album-card';
      card.setAttribute('role', 'button');
      card.tabIndex = 0;
      card.setAttribute('aria-label', `Play ${song.title} by ${song.artist}`);
      card.innerHTML = `
        <div class="album-card-art" style="background:${artFor(song)}">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>
        </div>
        <div class="album-card-title">${song.title}</div>
        <div class="album-card-artist">${song.artist}</div>`;

      const activate = () => playSong(song);
      card.addEventListener('click', activate);
      card.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(); }
      });
      container.appendChild(card);
    });
  }

  function updateSongListPlaying() {
    document.querySelectorAll('#song-list-view .song-row').forEach((row) => {
      const isPlaying = row.dataset.id === state.currentSong?.id;
      row.classList.toggle('playing', isPlaying);
      if (isPlaying) row.setAttribute('aria-current', 'true');
      else row.removeAttribute('aria-current');

      const num = row.querySelector('.song-num');
      if (isPlaying) {
        num.innerHTML = '<div class="eq-bars"><span></span><span></span><span></span></div>';
        row.querySelector('.song-name').style.color = 'var(--accent-text)';
      } else {
        num.textContent = row.dataset.index;
        row.querySelector('.song-name').style.color = '';
      }
    });
  }

  // ─── Render Recommendations ────────────────────────────────────────────────
  function renderRecommendations() {
    const container = document.getElementById('recs-list');
    container.innerHTML = '';

    state.recommendations.forEach((song) => {
      const card = document.createElement('div');
      card.className = 'rec-card';
      card.setAttribute('role', 'button');
      card.tabIndex = 0;
      card.setAttribute('aria-label', `Play ${song.title} by ${song.artist}`);
      card.innerHTML = `
        <div class="rec-art" style="background:${artFor(song)}">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>
        </div>
        <div class="rec-name">${song.title}</div>
        <div class="rec-artist">${song.artist}</div>`;

      const activate = () => playSong(song);
      card.addEventListener('click', activate);
      card.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(); }
      });
      container.appendChild(card);
    });
  }

  // ─── Waveform ──────────────────────────────────────────────────────────────
  function generateWaveform(seed = 'default') {
    const container = document.getElementById('waveform');
    container.innerHTML = '';
    const numBars = 48;

    let hash = 0;
    for (const c of seed) hash = ((hash << 5) - hash + c.charCodeAt(0)) | 0;

    for (let i = 0; i < numBars; i++) {
      hash = (hash * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFF;
      const h = 6 + Math.abs(hash % 28);
      const bar = document.createElement('div');
      bar.className = 'wave-bar';
      bar.style.height = `${h}px`;
      bar.dataset.index = i;
      container.appendChild(bar);
    }
  }

  function updateWaveformProgress(pct) {
    const bars = document.querySelectorAll('.wave-bar');
    const total = bars.length;
    const activeIdx = Math.floor((pct / 100) * total);

    bars.forEach((bar, i) => {
      bar.classList.remove('played', 'current');
      if (i < activeIdx) bar.classList.add('played');
      else if (i === activeIdx) bar.classList.add('current');
    });
  }

  // ─── Stats & Playlists View ───────────────────────────────────────────────
  function renderStats() {
    Bridge.invoke('get_stats').then((s) => {
      const grid = document.getElementById('stats-grid');
      const empty = document.getElementById('stats-empty');
      const streak = document.getElementById('stats-streak');
      const topList = document.getElementById('top-songs-list');
      const topEmpty = document.getElementById('top-songs-empty');
      if (!grid) return;
      if (!s || s.total_plays === 0) {
        grid.style.display = 'none';
        empty.style.display = '';
        streak.textContent = '';
        if (topList) topList.innerHTML = '';
        // Top Songs needs an explicit empty state too — a header-only card
        // reads as broken, not empty.
        if (topList) topList.style.display = 'none';
        if (topEmpty) topEmpty.style.display = '';
        return;
      }
      grid.style.display = '';
      empty.style.display = 'none';
      if (topList) topList.style.display = '';
      if (topEmpty) topEmpty.style.display = 'none';
      const hours = (s.total_listened_secs / 3600).toFixed(1);
      const cells = [
        ['Plays', s.total_plays],
        ['Listened', `${hours} h`],
        ['Skips', s.total_skips],
        ['Completion', `${Math.round(s.avg_listen_ratio * 100)}%`],
        ['Streak', `${s.streak_days} d`],
        ['Top artist', s.top_artists[0] ? s.top_artists[0][0] : '—'],
      ];
      grid.innerHTML = cells.map(([k, v]) => `
        <div style="text-align:center;">
          <div style="font-size:var(--text-title); font-weight:var(--weight-bold); color:var(--accent);">${v}</div>
          <div style="font-size:var(--text-caption); color:var(--text-secondary);">${k}</div>
        </div>`).join('');
      streak.textContent = s.streak_days > 0 ? `🔥 ${s.streak_days}-day streak` : '';

      const list = document.getElementById('top-songs-list');
      list.innerHTML = '';
      (s.top_songs || []).slice(0, 5).forEach((song, i) => {
        const row = document.createElement('div');
        row.className = 'song-row';
        row.setAttribute('role', 'button');
        row.tabIndex = 0;
        row.setAttribute('aria-label', `Play ${song.title} by ${song.artist}`);
        row.innerHTML = `
          <div class="song-num">${i + 1}</div>
          <div class="song-art" style="background:${artFor(song)}">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>
          </div>
          <div class="song-info">
            <div class="song-name">${song.title}</div>
            <div class="song-detail">${song.artist} · ${song.plays} plays</div>
          </div>`;
        const libSong = state.library.find((l) => l.id === song.id);
        const activate = () => { if (libSong) playSong(libSong); };
        row.addEventListener('click', activate);
        row.addEventListener('keydown', (e) => {
          if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(); }
        });
        list.appendChild(row);
      });
    });

    Bridge.invoke('get_playlists').then((playlists) => {
      const container = document.getElementById('playlists-list');
      if (!container) return;
      if (playlists.length === 0) return; // keep the empty state
      document.getElementById('playlists-empty').style.display = 'none';
      container.innerHTML = '';
      playlists.forEach((pl) => {
        const row = document.createElement('div');
        row.className = 'song-row';
        row.innerHTML = `
          <div class="song-num">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" style="width:16px;height:16px;"><path d="M8 6h13M8 12h13M8 18h13"/></svg>
          </div>
          <div class="song-info">
            <div class="song-name">${pl.name}</div>
            <div class="song-detail">${pl.rules.length} rules${pl.max_songs ? ` · max ${pl.max_songs}` : ''}</div>
          </div>`;
        container.appendChild(row);
      });
    });
  }

  // ─── EQ (10-band, presets, vertical sliders) ──────────────────────────────
  const EQ_BANDS = ['32', '64', '125', '250', '500', '1K', '2K', '4K', '8K', '16K'];
  const EQ_PRESETS = {
    'Flat': [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    'Bass Boost': [6, 5, 4, 2, 0, 0, 0, 0, 0, 0],
    'Vocal': [-2, -1, 0, 2, 4, 4, 3, 1, 0, -1],
    'Treble': [0, 0, 0, 0, 0, 1, 2, 4, 5, 6],
    'Rock': [5, 4, 2, 0, -1, -1, 0, 2, 4, 5],
    'Classical': [4, 3, 2, 0, 0, 0, -1, -1, 2, 3],
  };

  function currentEqGains() {
    return [...document.querySelectorAll('.eq-slider')].map((s) => Number(s.value));
  }

  function applyEq(gains) {
    const enabled = document.getElementById('toggle-eq')?.classList.contains('on') ?? false;
    Bridge.invoke('set_eq', { enabled, gains });
  }

  function setupEq() {
    const container = document.getElementById('eq-bands');
    if (!container) return;
    container.innerHTML = EQ_BANDS.map((label, i) => `
      <div class="eq-band">
        <input type="range" class="eq-slider" data-idx="${i}" min="-12" max="12" step="1" value="0"/>
        <span class="eq-band-value">0</span>
        <span class="eq-band-label">${label}</span>
      </div>`).join('');

    container.querySelectorAll('.eq-slider').forEach((slider) => {
      slider.addEventListener('input', () => {
        slider.parentElement.querySelector('.eq-band-value').textContent = slider.value;
        // Custom curve — clear preset highlight
        document.querySelectorAll('#eq-presets .sort-pill').forEach((p) => p.classList.remove('active'));
        applyEq(currentEqGains());
      });
    });

    document.querySelectorAll('#eq-presets .sort-pill').forEach((pill) => {
      pill.addEventListener('click', () => {
        document.querySelectorAll('#eq-presets .sort-pill').forEach((p) => p.classList.remove('active'));
        pill.classList.add('active');
        const gains = EQ_PRESETS[pill.dataset.eqPreset] ?? EQ_PRESETS['Flat'];
        container.querySelectorAll('.eq-slider').forEach((s, i) => {
          s.value = gains[i];
          s.parentElement.querySelector('.eq-band-value').textContent = gains[i];
        });
        persistSettings({ eq_preset: pill.dataset.eqPreset });
        applyEq(gains);
      });
    });

    // EQ toggle also applies the current curve
    document.getElementById('toggle-eq')?.addEventListener('click', () => {
      applyEq(currentEqGains());
    });
  }

  // ─── Utility ───────────────────────────────────────────────────────────────
  function formatTime(secs) {
    const s = Math.floor(secs);
    const m = Math.floor(s / 60);
    const sec = s % 60;
    return `${m}:${sec.toString().padStart(2, '0')}`;
  }

  // Expose for other modules and browser-console testing
  return {
    init,
    state,
    playSong,
    togglePlay,
    updateProgress,
    navigateTo,
    formatTime,
    handlePlaybackEvent,
    renderQueue,
    artFor,
    showToast,
    transport: {
      toggle: () => togglePlay(),
      next: () => Bridge.invoke('next'),
      prev: () => Bridge.invoke('previous'),
      seek: (secs) => {
        state.position = secs;
        updateProgress();
        Bridge.invoke('seek', { positionSecs: secs });
      },
    },
  };
})();

// Modules (Preact shell) resolve globals via window, not script scope.
window.App = App;

  // ─── Toasts (errors, engine warnings, action feedback) ────────────────────
  let toastTimer = null;

  function showToast(message) {
    let toast = document.getElementById('musico-toast');
    if (!toast) {
      toast = document.createElement('div');
      toast.id = 'musico-toast';
      toast.setAttribute('role', 'status');
      toast.setAttribute('aria-live', 'polite');
      toast.style.cssText = [
        'position:fixed', 'bottom:150px', 'left:50%', 'transform:translateX(-50%)',
        'max-width:80%', 'padding:10px 18px', 'border-radius:12px',
        'background:var(--surface-raised, #222)', 'color:var(--text-primary, #fff)',
        'box-shadow:0 8px 32px rgba(0,0,0,0.25)', 'font-size:var(--text-caption, 12px)',
        'border:1px solid var(--border-subtle, transparent)',
        'z-index:var(--z-modal, 1000)', 'opacity:0', 'transition:opacity 0.25s ease',
        'pointer-events:none', 'text-align:center',
      ].join(';');
      document.body.appendChild(toast);
    }
    toast.textContent = message;
    toast.style.opacity = '1';
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toast.style.opacity = '0'; }, 2600);
  }

  function showError(message) {
    showToast(message);
  }

  // Boot
  document.addEventListener('DOMContentLoaded', () => {
    Bridge.setErrorHandler(showError);
    App.init();
  });
