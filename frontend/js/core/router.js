/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — core/router.js
   Hash router with a real history stack (Apple Music-style back/forward
   arrows).  Uses history.pushState so we know our exact position; popstate
   drives back/forward.  The view-rendering callback is provided by app.js at
   init time (legacy navigateTo), so this module owns ONLY navigation state.
   ══════════════════════════════════════════════════════════════════════════════ */

const MusicoRouter = (() => {
  const DEFAULT_VIEW = 'browse';
  const listeners = new Set();
  let pos = 0;        // current index into history entries we created
  let top = 0;        // furthest index reached (for canGoForward)

  function parseHash() {
    const h = decodeURIComponent(window.location.hash.replace(/^#\/?/, ''));
    return h || DEFAULT_VIEW;
  }

  function emit(view) {
    for (const cb of listeners) {
      try { cb(view); } catch (e) { console.warn('[router] listener failed:', e); }
    }
  }

  /** Navigate to `view`, pushing a history entry. */
  function navigate(view, { push = true } = {}) {
    view = String(view || DEFAULT_VIEW);
    if (view === parseHash() && push) return; // no-op re-navigation
    if (push) {
      pos += 1;
      top = pos; // pushState truncates the browser's forward stack — mirror that
      history.pushState({ idx: pos }, '', `#/${view}`);
      emit(view);
    } else {
      // Replace current entry (initial load / corrections).
      history.replaceState({ idx: pos }, '', `#/${view}`);
      emit(view);
    }
  }

  // Position bookkeeping lives entirely in handlePop (driven by
  // history.state.idx) — pre-adjusting here would double-count.
  function back() { if (canGoBack()) history.back(); }
  function forward() { if (canGoForward()) history.forward(); }
  const canGoBack = () => pos > 0;
  const canGoForward = () => pos < top;

  function handlePop() {
    const st = history.state;
    if (st && typeof st.idx === 'number') {
      pos = st.idx;
      top = Math.max(top, pos);
    }
    emit(parseHash());
  }

  /**
   * Boot: normalise the URL to `#/default` (no history entry) and start
   * listening.  `onRoute` is called immediately with the current view.
   */
  function init(onRoute) {
    listeners.add(onRoute);
    window.addEventListener('popstate', handlePop);
    if (!window.location.hash) {
      history.replaceState({ idx: 0 }, '', `#/${DEFAULT_VIEW}`);
    } else if (!history.state || typeof history.state.idx !== 'number') {
      history.replaceState({ idx: 0 }, '', window.location.hash);
      top = 0; pos = 0;
    }
    emit(parseHash());
  }

  return { navigate, back, forward, canGoBack, canGoForward, init, current: parseHash };
})();

window.MusicoRouter = MusicoRouter;
export default MusicoRouter;
