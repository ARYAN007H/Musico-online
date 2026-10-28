/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — core/store.js
   Tiny observable store: the state hub between the legacy app.js controller
   (classic script) and the Preact shell (ESM).  Both sides meet here:
   app.js publishes snapshots; Preact components subscribe and re-render.
   ══════════════════════════════════════════════════════════════════════════════ */

const MusicoStore = (() => {
  const listeners = new Set();
  const state = new Map();

  function get(key) {
    return state.get(key);
  }

  function set(key, value) {
    if (Object.is(state.get(key), value)) return;
    state.set(key, value);
    for (const cb of listeners) {
      try { cb(key, value); } catch (e) { console.warn('[store] listener failed:', e); }
    }
  }

  function subscribe(cb) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  }

  /** Convenience for React-style useSyncExternalStore consumers. */
  function snapshotOf(key, fallback) {
    const v = state.get(key);
    return v === undefined ? fallback : v;
  }

  return { get, set, subscribe, snapshotOf };
})();

window.MusicoStore = MusicoStore;
export default MusicoStore;
