/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO — Tauri IPC Bridge
   Wraps Tauri invoke() calls with stateful mock fallbacks so index.html works
   as a standalone preview in any browser (no backend required).
   ══════════════════════════════════════════════════════════════════════════════ */

const Bridge = (() => {
  // Tauri v2 IPC resolution, most-direct first:
  //   1. __TAURI_INTERNALS__.invoke — the raw IPC shim, always injected first
  //   2. __TAURI__.core.invoke      — the withGlobalTauri API bundle
  //   3. __TAURI__.invoke           — legacy/compat shape
  // Event listeners live under __TAURI__.event (bundle) only.
  const T = typeof window.__TAURI__ !== 'undefined' ? window.__TAURI__ : null;
  const tauriInvoke =
    (typeof window.__TAURI_INTERNALS__ !== 'undefined' && window.__TAURI_INTERNALS__.invoke) ||
    T?.core?.invoke ||
    T?.invoke ||
    null;
  const isTauri = typeof tauriInvoke === 'function';
  const listeners = {};
  const mockQueue = [];
  const mockLikes = new Set();

  function emit(event, payload) {
    (listeners[event] || []).forEach((cb) => cb({ event, payload }));
  }

  let onError = null;

  async function invoke(cmd, args = {}) {
    if (isTauri) {
      try {
        return await tauriInvoke(cmd, args);
      } catch (err) {
        if (onError) onError(`${cmd}: ${err}`);
        console.warn(`[musico] ${cmd} failed:`, err);
        return null;
      }
    }
    return mockCommand(cmd, args);
  }

  function listen(event, callback) {
    const tauriListen = T?.event?.listen ?? T?.core?.listen ?? null;
    if (typeof tauriListen === 'function') {
      return tauriListen(event, callback);
    }
    (listeners[event] = listeners[event] || []).push(callback);
    // Returns an unsubscribe function (same contract as Tauri listen)
    return () => {
      listeners[event] = (listeners[event] || []).filter((cb) => cb !== callback);
    };
  }

  // Mock data for standalone HTML preview
  const DEMO_SONGS = [
    { id: '1', title: 'Midnight City', artist: 'M83', album: 'Hurry Up, We\'re Dreaming', duration_secs: 244 },
    { id: '2', title: 'Teardrop', artist: 'Massive Attack', album: 'Mezzanine', duration_secs: 329 },
    { id: '3', title: 'Breathe (In the Air)', artist: 'Pink Floyd', album: 'Dark Side of the Moon', duration_secs: 165 },
    { id: '4', title: 'Intro', artist: 'The xx', album: 'xx', duration_secs: 127 },
    { id: '5', title: 'Motion Picture Soundtrack', artist: 'Radiohead', album: 'Kid A', duration_secs: 421 },
    { id: '6', title: 'Blue (Da Ba Dee)', artist: 'Eiffel 65', album: 'Europop', duration_secs: 220 },
    { id: '7', title: 'Crystalised', artist: 'The xx', album: 'xx', duration_secs: 227 },
    { id: '8', title: 'Holocene', artist: 'Bon Iver', album: 'Bon Iver', duration_secs: 337 },
    { id: '9', title: 'Svefn-g-englar', artist: 'Sigur Rós', album: 'Ágætis byrjun', duration_secs: 610 },
    { id: '10', title: 'Porcelain', artist: 'Moby', album: 'Play', duration_secs: 238 },
    { id: '11', title: 'Teardrop', artist: 'José González', album: 'Veneer', duration_secs: 191 },
    { id: '12', title: 'Everything In Its Right Place', artist: 'Radiohead', album: 'Kid A', duration_secs: 252 },
  ];

  function mockCommand(cmd, args) {
    switch (cmd) {
      case 'get_library':
        return Promise.resolve(DEMO_SONGS);
      case 'get_queue':
        return Promise.resolve([...mockQueue]);
      case 'play_song':
        emit('playback-event', { type: 'Playing', song: args.song });
        return Promise.resolve(null);
      case 'resume':
        emit('playback-event', { type: 'Resumed' });
        return Promise.resolve(null);
      case 'pause':
        emit('playback-event', { type: 'Paused', position_secs: 0 });
        return Promise.resolve(null);
      case 'next':
      case 'previous':
        return Promise.resolve(null);
      case 'add_to_queue':
        mockQueue.push(args.song);
        emit('queue-changed', [...mockQueue]);
        return Promise.resolve(null);
      case 'play_next':
        mockQueue.unshift(args.song);
        emit('queue-changed', [...mockQueue]);
        return Promise.resolve(null);
      case 'remove_from_queue': {
        const i = mockQueue.findIndex((s) => s.id === args.songId);
        if (i >= 0) mockQueue.splice(i, 1);
        emit('queue-changed', [...mockQueue]);
        return Promise.resolve(null);
      }
      case 'clear_queue':
        mockQueue.length = 0;
        emit('queue-changed', []);
        return Promise.resolve(null);
      case 'get_recommendations':
        return Promise.resolve(DEMO_SONGS.slice(3, 7));
      case 'set_song_like': {
        const { songId, liked } = args || {};
        if (liked) mockLikes.add(songId); else mockLikes.delete(songId);
        return Promise.resolve(true);
      }
      case 'get_liked_ids':
        return Promise.resolve([...mockLikes]);
      case 'online_search': {
        const q = (args?.query || '').toLowerCase();
        if (args?.continuation) return Promise.resolve({ tracks: [], next_continuation: null });
        const found = DEMO_SONGS.filter(s =>
          s.title.toLowerCase().includes(q) || s.artist.toLowerCase().includes(q));
        const results = (found.length ? found : DEMO_SONGS).map((s, i) => ({
          video_id: `demo${s.id}${i}`,
          title: s.title,
          artist: s.artist,
          album: s.album,
          duration_secs: s.duration_secs,
          thumbnail: null,
          cached: false,
        }));
        // Page-1 mock keeps a fake continuation so "Load more" is testable.
        return Promise.resolve({ tracks: results, next_continuation: 'mock-cont-2' });
      }
      case 'online_play': {
        const t = args?.track || {};
        mockQueue.push({
          id: `yt:${t.video_id}`, title: t.title, artist: t.artist,
          album: t.album || '', duration_secs: t.duration_secs || 0,
          file_path: `/mock/${t.video_id}.m4a`,
        });
        emit('queue-changed', [...mockQueue]);
        emit('playback-event', {
          type: 'Playing',
          song: { id: `yt:${t.video_id}`, title: t.title, artist: t.artist,
                  album: t.album || '', duration_secs: t.duration_secs || 0,
                  file_path: `/mock/${t.video_id}.m4a` },
        });
        return Promise.resolve(true);
      }
      case 'online_enqueue': {
        const t2 = args?.track || {};
        mockQueue.push({
          id: `yt:${t2.video_id}`, title: t2.title, artist: t2.artist,
          album: t2.album || '', duration_secs: t2.duration_secs || 0,
          file_path: `/mock/${t2.video_id}.m4a`,
        });
        emit('queue-changed', [...mockQueue]);
        return Promise.resolve(true);
      }
      case 'online_available':
        return Promise.resolve(true);
      case 'online_clear_cache':
        return Promise.resolve(0);
      case 'start_scan':
        emit('library-updated', DEMO_SONGS);
        return Promise.resolve(true);
      case 'get_cover':
        return Promise.resolve(null);
      case 'get_settings':
        return Promise.resolve({
          music_folder: null, volume: 0.68, palette: 'nebula', art_tint: true,
          smart_recs: true, eq_enabled: true, eq_preset: 'Flat', normalization: 'Off',
          crossfade_enabled: false, crossfade_duration: 3.0, library_view_mode: 'list',
          shuffle_mode: 0, repeat_mode: 0, theme: 'dark',
        });
      case 'save_settings':
        return Promise.resolve(null);
      case 'pick_folder':
      case 'set_music_folder':
      case 'set_playback_modes':
        return Promise.resolve(null);
      case 'get_stats':
        return Promise.resolve({ total_plays: 0, total_listened_secs: 0, total_skips: 0,
          streak_days: 0, avg_listen_ratio: 0, top_songs: [], top_artists: [], daily_minutes: [] });
      case 'get_playlists':
        return Promise.resolve([]);
      case 'get_lyrics':
        return Promise.resolve(null);
      case 'set_eq':
      case 'set_normalization':
        return Promise.resolve(null);
      case 'get_playback_state':
        return Promise.resolve({
          current_song: null,
          status: 'Stopped',
          position_secs: 0,
          duration_secs: 0,
          volume: 0.68,
        });
      case 'search_library': {
        const q = (args.query || '').toLowerCase();
        return Promise.resolve(
          DEMO_SONGS.filter(s =>
            s.title.toLowerCase().includes(q) ||
            s.artist.toLowerCase().includes(q) ||
            s.album.toLowerCase().includes(q)
          )
        );
      }
      default:
        return Promise.resolve(null);
    }
  }

  return { invoke, listen, isTauri, DEMO_SONGS, setErrorHandler: (fn) => { onError = fn; } };
})();
