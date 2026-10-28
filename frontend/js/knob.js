/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO — Volume Controller
   Slider in the mini player strip + keyboard arrows (app.js). Exposes the same
   Knob.setVolume(v, notify) API the rest of the app already calls.
   ══════════════════════════════════════════════════════════════════════════════ */

const Knob = (() => {
  const slider = document.getElementById('mini-volume');

  let lastSent = 0; // IPC throttle while the slider is being dragged

  function init() {
    if (!slider) return;
    setVolume(App.state.volume, false);

    slider.addEventListener('input', () => {
      const vol = Number(slider.value) / 100;
      // Update internal state + visuals but throttle IPC while scrubbing
      applyVolume(vol);
      const now = Date.now();
      if (now - lastSent > 100) {
        lastSent = now;
        Bridge.invoke('set_volume', { volume: vol });
      }
    });

    // Flush the final value when the drag ends
    slider.addEventListener('change', () => {
      Bridge.invoke('set_volume', { volume: Number(slider.value) / 100 });
    });
  }

  function applyVolume(vol) {
    App.state.volume = vol;
    if (slider && Number(slider.value) !== Math.round(vol * 100)) {
      slider.value = String(Math.round(vol * 100));
    }
  }

  function setVolume(vol, notify = true) {
    vol = Math.max(0, Math.min(1, vol));
    applyVolume(vol);
    if (notify) {
      Bridge.invoke('set_volume', { volume: vol });
    }
  }

  document.addEventListener('DOMContentLoaded', init);

  return { setVolume };
})();
