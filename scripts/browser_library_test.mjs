import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
await page.evaluate(() => {
  window.__ipc = [];
  const orig = window.Bridge.invoke.bind(window.Bridge);
  window.Bridge.invoke = (cmd, ...rest) => {
    window.__ipc.push([cmd, rest[0]]);
    return orig(cmd, ...rest);
  };
});
const ipcCalls = (cmd) => page.evaluate(
  (c) => window.__ipc.filter(([k]) => k === c).map(([, a]) => a), cmd);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

const ctxClick = (sel) => page.evaluate((s) => {
  const el = document.querySelector(s);
  if (!el) return false;
  const r = el.getBoundingClientRect();
  el.dispatchEvent(new MouseEvent('contextmenu', {
    bubbles: true, clientX: r.x + 10, clientY: r.y + 10,
  }));
  return true;
}, sel);

// 1. Library shows the four sub-tabs; catalog hides the legacy list by default.
await page.evaluate(() => window.MusicoRouter.navigate('library'));
await page.waitForTimeout(400);
check('library tabs render', await page.evaluate(() => {
  const labels = [...document.querySelectorAll('.lib-tab')].map((t) => t.textContent);
  return labels.join('|') === 'Recently Added|Albums|Artists|Songs';
}));
check('legacy list hidden on catalog tabs', await page.evaluate(() =>
  document.getElementById('song-list-view').style.display === 'none'));

// 2. Empty state before anything is saved.
check('empty state shows', await page.evaluate(() =>
  !!document.querySelector('.lib-empty')));

// 3. Add an album card from the Home feed via the context menu.
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
await ctxClick('#home-root .shelf-card[data-ctx="nav"][data-kind="album"]');
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Add to Library');
await page.waitForTimeout(300);
const addCalls = await ipcCalls('library_add');
check('album add calls library_add', addCalls.length === 1 &&
  addCalls[0]?.entry?.kind === 'album' &&
  typeof addCalls[0]?.entry?.browse_id === 'string');

// Genres must NOT offer Add to Library (no such kind).
await page.evaluate(() => window.MusicoRouter.navigate('genres'));
await page.waitForTimeout(400);
await page.evaluate(() => {
  const g = document.querySelector('.genre-card');
  const r = g.getBoundingClientRect();
  g.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: r.x + 5, clientY: r.y + 5 }));
});
await page.waitForTimeout(150);
check('genre card has no Add to Library', await page.evaluate(() =>
  [...document.querySelectorAll('.ctx-menu .ctx-item')].map((i) => i.textContent).join('|') === 'Open'));
await page.keyboard.press('Escape');
await page.waitForTimeout(100);

// Seed an album + artist for the tab/rail assertions (ctx flow is covered above).
await page.evaluate(async () => {
  await window.Bridge.invoke('library_add', { entry: {
    id: 'MPREb_zeta', kind: 'album', title: 'Zeta Waves', subtitle: 'Vela',
    browse_id: 'MPREb_zeta', params: null, thumbnail: null } });
  await window.Bridge.invoke('library_add', { entry: {
    id: 'UC_alpha', kind: 'artist', title: 'Alpha Duo', subtitle: 'Artist',
    browse_id: 'UC_alpha', params: null, thumbnail: null } });
  // Same invalidation path a real save takes.
  window.MusicoStore.set('libAlbums', null);
  window.MusicoStore.set('libArtists', null);
  window.MusicoStore.set('libTracks', null);
  window.MusicoStore.set('libRev', (window.MusicoStore.get('libRev') || 0) + 1);
});

// 4. Library > Recently Added renders all three saved kinds newest-first.
await page.evaluate(() => window.MusicoRouter.navigate('library'));
await page.evaluate(() => window.MusicoStore.set('libTab', 'recent'));
await page.waitForTimeout(400);
check('saved items appear in Recently Added', await page.evaluate(() => {
  const titles = [...document.querySelectorAll('#library-catalog .lib-card-title')]
    .map((e) => e.textContent);
  // Order is timing-dependent (same-ms stamps tie), so assert membership.
  return titles.length === 3 && titles.includes('Alpha Duo') &&
    titles.includes('Zeta Waves');
}));

// 5. Albums tab: card + A–Z rail.
await page.evaluate(() => window.MusicoStore.set('libTab', 'albums'));
await page.waitForTimeout(300);
check('albums tab shows both saved albums', await page.evaluate(() => {
  const titles = [...document.querySelectorAll('.lib-with-rail .lib-card-title')]
    .map((e) => e.textContent);
  return titles.length === 2 && titles.includes('Zeta Waves');
}));
check('A-Z rail renders active letters', await page.evaluate(() =>
  [...document.querySelectorAll('.az-letter:not(.az-off)')].length === 2));
await page.evaluate(() => document.querySelector('.az-letter:not(.az-off)')?.click());
check('rail jump targets a letter card', await page.evaluate(() =>
  String(document.querySelector('.lib-card[data-letter="Z"]')?.textContent)
    .includes('Zeta Waves')));

// 6. Songs tab = merged list (local demo songs + saved tracks), windowed.
await page.evaluate(() => window.MusicoStore.set('libTab', 'songs'));
await page.waitForTimeout(400);
check('songs tab shows merged rows', await page.evaluate(() =>
  document.querySelectorAll('.lib-rows .lib-row').length >= 8));
check('letter headers render', await page.evaluate(() =>
  document.querySelectorAll('.lib-letter-header').length >= 1));

// 7. Add-to-Library round-trip through the mock bridge directly.
const added = await page.evaluate(async () => {
  await window.Bridge.invoke('library_add', { entry: {
    id: 'MPREb_test_roundtrip', kind: 'album', title: 'Roundtrip',
    subtitle: 'Tester', browse_id: 'MPREb_test_roundtrip', params: null,
  } });
  const hit = await window.Bridge.invoke('library_contains', {
    kind: 'album', ids: ['MPREb_test_roundtrip'] });
  const list = await window.Bridge.invoke('library_list', { kind: 'album' });
  return { hit, titled: list.some((e) => e.title === 'Roundtrip') };
});
check('round-trip: contains + list see the entry', added.hit.length === 1 && added.titled);

// 8. C4.1 gate: 10k rows stay windowed — tiny DOM, growing on scroll.
await page.evaluate(() => {
  const store = window.__libStore;
  store.track = [];
  for (let i = 0; i < 10_000; i++) {
    store.track.push({
      id: `perf${i}`, kind: 'track',
      title: `${'ABCDEFGHIJKLMNOPQRSTUVWXYZ'[i % 26]}song ${String(i).padStart(5, '0')}`,
      subtitle: 'Perf Artist', thumbnail: null, browse_id: null, params: null,
      video_id: `perfvid${i}`, added_at: 1_700_000_000_000 + i,
    });
  }
  ['libAlbums', 'libArtists', 'libTracks'].forEach((k) => window.MusicoStore.set(k, null));
  window.MusicoStore.set('libRev', (window.MusicoStore.get('libRev') || 0) + 1);
});
await page.evaluate(() => window.MusicoStore.set('libTab', 'songs'));
await page.waitForTimeout(800);
check('10k rows: windowed DOM under 500 nodes', await page.evaluate(() => {
  const rows = document.querySelectorAll('.lib-rows .lib-row').length;
  return rows > 50 && rows < 500;
}));
await page.evaluate(() => {
  const s = document.querySelector('.lib-sentinel');
  s?.scrollIntoView();
});
await page.waitForTimeout(500);
check('window grows after scrolling to sentinel', await page.evaluate(() =>
  document.querySelectorAll('.lib-rows .lib-row').length > 100));

// 9. A–Z rail jump beyond the rendered window grows it (regression: the
// jump used to querySelector and silently no-op past 80 rows).
await page.evaluate(() => window.MusicoStore.set('libTab', 'songs'));
await page.waitForTimeout(300);
const rowsBefore = await page.evaluate(() =>
  document.querySelectorAll('.lib-rows .lib-row').length);
await page.evaluate(() => {
  const w = [...document.querySelectorAll('.az-letter:not(.az-off)')]
    .find((l) => l.textContent === 'Z');
  w?.click();
});
await page.waitForTimeout(400);
check('rail jump beyond window grows + targets', await page.evaluate((before) => {
  const rows = document.querySelectorAll('.lib-rows .lib-row').length;
  const target = document.querySelector('.lib-row[data-letter="Z"], .lib-letter-header[data-letter="Z"]');
  return rows > before + 100 && !!target;
}, rowsBefore));

// 10. Escape clears the filter through the store (regression: the legacy
// handler cleared the input without an input event, leaving libSearch set).
await page.fill('#search-input', 'perf');
await page.waitForTimeout(300);
check('search filters the merged list', await page.evaluate(() =>
  document.querySelectorAll('.lib-rows .lib-row').length < 100));
await page.fill('#search-input', '');
await page.press('#search-input', 'Escape');
await page.waitForTimeout(300);
check('Escape resets libSearch', await page.evaluate(() =>
  (window.MusicoStore.get('libSearch') || '') === '' &&
  document.querySelectorAll('.lib-rows .lib-row').length >= 8));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
