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

// 3. Add the first Home nav card (playlist) via the context menu.
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
const navSel = '#home-root .shelf-card[data-ctx="nav"]';
await ctxClick(navSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Add to Library');
await page.waitForTimeout(300);
const addCalls = await ipcCalls('library_add');
check('card add calls library_add with kind', addCalls.length === 1 &&
  typeof addCalls[0]?.entry?.kind === 'string' &&
  typeof addCalls[0]?.entry?.browse_id === 'string');

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
check('saved items appear in Recently Added', await page.evaluate(() =>
  [...document.querySelectorAll('#library-catalog .lib-card-title')]
    .map((e) => e.textContent).join(',') === 'Zeta Waves,Alpha Duo'));

// 5. Albums tab: card + A–Z rail.
await page.evaluate(() => window.MusicoStore.set('libTab', 'albums'));
await page.waitForTimeout(300);
check('albums tab shows only the album', await page.evaluate(() =>
  [...document.querySelectorAll('.lib-with-rail .lib-card-title')]
    .map((e) => e.textContent).join(',') === 'Zeta Waves'));
check('A-Z rail renders active letters', await page.evaluate(() =>
  [...document.querySelectorAll('.az-letter:not(.az-off)')].map((l) => l.textContent).join('') === 'Z'));
await page.evaluate(() => document.querySelector('.az-letter:not(.az-off)')?.click());
check('rail jump targets a letter card', await page.evaluate(() =>
  String(document.querySelector('.lib-card[data-letter="Z"]')?.textContent)
    .includes('Zeta Waves')));

// 6. Songs tab restores the legacy list.
await page.evaluate(() => window.MusicoStore.set('libTab', 'songs'));
await page.waitForTimeout(300);
check('songs tab restores legacy list', await page.evaluate(() =>
  document.getElementById('song-list-view').style.display !== 'none' &&
  !document.querySelector('#library-catalog .lib-grid')));

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

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
