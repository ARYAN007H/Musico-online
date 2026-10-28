/* ══════════════════════════════════════════════════════════════════════════════
   MUSICO ONLINE — scripts/contrast_gate.mjs
   Programmatic WCAG contrast gate: text-on-surface pairs must be ≥ 4.5:1 in
   BOTH themes.  Reads real computed values from the live page across every
   palette × theme combination.  Exit 1 on any failure.
   ══════════════════════════════════════════════════════════════════════════════ */
import { chromium } from 'playwright';

const PORT = process.argv[2] || '8145';
const BASE = `http://127.0.0.1:${PORT}`;
const PALETTES = ['nebula', 'sakura', 'aurora', 'ocean', 'ember', 'mono'];
const THEMES = ['light', 'dark'];

function lum(cRaw) {
  let c = cRaw.trim();
  if (c.startsWith('rgba')) {
    // blend any alpha over mid-grey so the ratio is measurable
    const f = c.match(/[\d.]+/g).map(Number);
    const a = f[3] ?? 1;
    c = `rgb(${Math.round(f[0] * a + 128 * (1 - a))}, ${Math.round(f[1] * a + 128 * (1 - a))}, ${Math.round(f[2] * a + 128 * (1 - a))})`;
  }
  let r, g, b;
  if (c.startsWith('#')) {
    const hex = c.slice(1);
    const full = hex.length === 3
      ? hex.split('').map((ch) => ch + ch).join('')
      : hex.slice(0, 6);
    r = parseInt(full.slice(0, 2), 16);
    g = parseInt(full.slice(2, 4), 16);
    b = parseInt(full.slice(4, 6), 16);
  } else {
    [r, g, b] = c.match(/\d+/g).slice(0, 3).map(Number);
  }
  const f = (v) => {
    v /= 255;
    return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function ratio(fg, bg) {
  const [l1, l2] = [lum(fg), lum(bg)].sort((a, b) => b - a);
  return (l1 + 0.05) / (l2 + 0.05);
}

function blend(fg, bg) {
  // alpha-composite an rgba() fg over a solid bg
  const f = fg.match(/[\d.]+/g).map(Number);
  const b = bg.match(/[\d.]+/g).map(Number);
  const a = f[3] ?? 1;
  const mix = (i) => Math.round(f[i] * a + b[i] * (1 - a));
  return `rgb(${mix(0)}, ${mix(1)}, ${mix(2)})`;
}

const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
await page.goto(`${BASE}/index.html`);
await page.waitForTimeout(800);

// Probe element: text colored like each token role, on each surface tier.
const probe = `
  <div id="contrast-probe" style="position:absolute; visibility:hidden;">
    <span id="p-primary"></span><span id="p-secondary"></span>
    <span id="p-muted"></span><span id="p-accent"></span>
  </div>`;
await page.evaluate((html) => {
  document.body.insertAdjacentHTML('beforeend', html);
}, probe);

const results = [];
let failures = 0;

for (const theme of THEMES) {
  for (const palette of PALETTES) {
    await page.evaluate(([t, p]) => {
      document.documentElement.setAttribute('data-theme', t);
      document.documentElement.setAttribute('data-palette', p);
    }, [theme, palette]);
    await page.waitForTimeout(60);

    const pairs = await page.evaluate(() => {
      const cs = getComputedStyle(document.documentElement);
      const surfaces = {
        base: cs.getPropertyValue('--surface-base').trim(),
        raised: cs.getPropertyValue('--surface-raised').trim(),
      };
      const texts = {
        primary: cs.getPropertyValue('--text-primary').trim(),
        secondary: cs.getPropertyValue('--text-secondary').trim(),
        muted: cs.getPropertyValue('--text-muted').trim(),
      };
      const probeEl = (id) => {
        const el = document.getElementById(id);
        return getComputedStyle(el).color;
      };
      void probeEl; // probe spans exist for future direct-element checks
      return { surfaces, texts };
    });

    const check = (name, fgRaw, bgRaw, min = 4.5) => {
      // --text-* values may be rgba(); blend over the surface before ratio.
      const fg = fgRaw.startsWith('rgba') ? blend(fgRaw, bgRaw) : fgRaw;
      const r = ratio(fg, bgRaw);
      const ok = r >= min;
      if (!ok) failures += 1;
      results.push(`${ok ? 'PASS' : 'FAIL'} ${theme}/${palette} ${name}: ${r.toFixed(2)}:1 (min ${min})`);
    };

    for (const [tName, tVal] of Object.entries(pairs.texts)) {
      for (const [sName, sVal] of Object.entries(pairs.surfaces)) {
        check(`${tName} on ${sName}`, tVal, sVal);
      }
    }
  }
}

await browser.close();
const fails = results.filter((r) => r.startsWith('FAIL'));
const passCount = results.length - fails.length;
console.log(`\nContrast gate: ${passCount} passed, ${fails.length} failed (min 4.5:1)`);
if (fails.length) console.log(fails.join('\n'));
process.exit(fails.length ? 1 : 0);
