/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO — Seek Bar Controller
   Click-to-seek and drag-to-seek on the progress bar
   ══════════════════════════════════════════════════════════════════════════════ */

const Seek = (() => {
  const wrapper = document.getElementById('seek-wrapper');
  const fill = document.getElementById('seek-fill');
  const thumb = document.getElementById('seek-thumb');

  let isDragging = false;

  function init() {
    if (!wrapper) return;

    // Click on track to seek
    wrapper.addEventListener('mousedown', onStart);
    wrapper.addEventListener('touchstart', onStart, { passive: false });

    document.addEventListener('mousemove', onDrag);
    document.addEventListener('touchmove', onDrag, { passive: false });

    document.addEventListener('mouseup', onEnd);
    document.addEventListener('touchend', onEnd);
  }

  function onStart(e) {
    isDragging = true;
    updatePosition(e);
    if (thumb) thumb.style.transform = 'translateY(-50%) scale(1.15)';
  }

  function onDrag(e) {
    if (!isDragging) return;
    e.preventDefault();
    updatePosition(e);
  }

  function onEnd() {
    if (!isDragging) return;
    isDragging = false;
    if (thumb) thumb.style.transform = '';

    // Send seek command (Tauri v2 expects camelCase arg keys)
    if (App.state.duration > 0) {
      Bridge.invoke('seek', { positionSecs: App.state.position });
    }
  }

  function updatePosition(e) {
    const rect = wrapper.getBoundingClientRect();
    const clientX = e.touches ? e.touches[0].clientX : e.clientX;
    let pct = (clientX - rect.left) / rect.width;
    pct = Math.max(0, Math.min(1, pct));

    // Update state
    App.state.position = pct * App.state.duration;

    // Update fill
    if (fill) fill.style.width = `${pct * 100}%`;

    // Update time display
    document.getElementById('time-current').textContent = App.formatTime(App.state.position);

    // Update mini player progress
    document.getElementById('mini-player').style.setProperty('--mini-progress', `${pct * 100}%`);

    // Keep the slider ARIA contract in sync while scrubbing
    if (wrapper) {
      wrapper.setAttribute('aria-valuenow', String(Math.round(App.state.position)));
      wrapper.setAttribute('aria-valuetext', App.formatTime(App.state.position));
    }
  }

  // Arrow key seek (±5 seconds)
  document.addEventListener('keydown', (e) => {
    if (e.target.tagName === 'INPUT') return;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      if (App.state.duration > 0) {
        const delta = e.key === 'ArrowRight' ? 5 : -5;
        App.state.position = Math.max(0, Math.min(App.state.duration, App.state.position + delta));
        App.updateProgress();
        Bridge.invoke('seek', { positionSecs: App.state.position });
      }
    }
  });

  document.addEventListener('DOMContentLoaded', init);

  return {
    // Playback events check this so PositionUpdate doesn't fight the thumb
    // mid-drag. Function form — callers do typeof Seek.isDragging === 'function'.
    isDragging: () => isDragging,
  };
})();
