import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(800);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

const openGenres = () => page.evaluate(() => {
  const btn = document.querySelector('#sidebar-root .nav-item[data-view="genres"]')
    || document.querySelector('.bottom-nav-pill .nav-item[data-view="genres"]');
  btn.click();
});

// 1. No InnerTube call at boot — the grid loads only when Browse activates.
check('no genres fetch at boot', await page.evaluate(() =>
  !window.MusicoStore.get('genres')));

// 2. Activation fetches and renders the mock grid.
await openGenres();
await page.waitForTimeout(400);
check('genre grid renders cards', await page.evaluate(() =>
  document.querySelectorAll('.genre-card').length >= 6));
check('grid shows mock category names', await page.evaluate(() =>
  [...document.querySelectorAll('.genre-name')].map(e => e.textContent).includes('Chill')));

// 3. Card click routes to a browse detail page.
await page.evaluate(() => document.querySelector('.genre-card').click());
await page.waitForTimeout(400);
check('card click opens browse detail route', await page.evaluate(() =>
  location.hash.startsWith('#/browse/')));
check('published view is the detail route', await page.evaluate(() =>
  String(window.MusicoStore.get('view')).startsWith('browse/')));

// 4. Returning to Browse re-uses the store cache — no second fetch.
await page.evaluate(() => {
  let calls = 0;
  const orig = window.Bridge.invoke.bind(window.Bridge);
  window.Bridge.invoke = (cmd, ...rest) => {
    if (cmd === 'online_genres') calls++;
    return orig(cmd, ...rest);
  };
  window.__genreCalls = () => calls;
});
await page.evaluate(() => window.MusicoRouter.back());
await page.waitForTimeout(400);
check('back returns to browse grid', await page.evaluate(() =>
  document.querySelectorAll('.genre-card').length >= 6));
check('no refetch on re-activation', await page.evaluate(() =>
  window.__genreCalls() === 0));

// 5. A failed load shows Retry; Retry recovers.
await page.evaluate(() => {
  window.MusicoStore.set('genres', null);
  window.MusicoStore.set('genresError', null);
  const orig = window.Bridge.invoke.bind(window.Bridge);
  window.Bridge.invoke = (cmd, ...rest) =>
    cmd === 'online_genres' ? Promise.reject('offline') : orig(cmd, ...rest);
});
await page.evaluate(() => window.MusicoRouter.navigate('now-playing'));
await page.waitForTimeout(200);
await openGenres();
await page.waitForTimeout(400);
check('failure shows retry button', await page.evaluate(() =>
  !!document.querySelector('.home-retry')));
await page.evaluate(() => {
  const orig = window.Bridge.invoke.bind(window.Bridge);
  window.Bridge.invoke = (cmd, ...rest) =>
    cmd === 'online_genres'
      ? Promise.resolve([{ kind: 'unknown', title: 'Recovered', subtitle: '',
                           thumbnail: null, video_id: null, browse_id: 'X',
                           params: 'p', playlist_id: null }])
      : orig(cmd, ...rest);
});
await page.click('.home-retry');
await page.waitForTimeout(400);
check('retry recovers the grid', await page.evaluate(() =>
  [...document.querySelectorAll('.genre-name')].map(e => e.textContent).includes('Recovered')));

// 6. An empty-but-successful response is a failure, never a blank grid.
await page.evaluate(() => {
  window.MusicoStore.set('genres', null);
  window.MusicoStore.set('genresError', null);
  const orig = window.Bridge.invoke.bind(window.Bridge);
  window.Bridge.invoke = (cmd, ...rest) =>
    cmd === 'online_genres' ? Promise.resolve([]) : orig(cmd, ...rest);
});
await page.evaluate(() => window.MusicoRouter.navigate('now-playing'));
await page.waitForTimeout(200);
await openGenres();
await page.waitForTimeout(400);
check('empty response treated as failure with Retry', await page.evaluate(() =>
  !!document.querySelector('.home-retry')));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
