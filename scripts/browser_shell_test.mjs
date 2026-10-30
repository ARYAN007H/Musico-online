import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

// Original 15 assertions (condensed re-run)
check('sidebar rendered', await page.evaluate(() => document.querySelectorAll('#sidebar-root .nav-item').length >= 6));
check('default route #/browse', await page.evaluate(() => location.hash === '#/browse'));
await page.click('#sidebar-root .nav-item[data-view="library"]');
await page.waitForTimeout(150);
check('library nav', await page.evaluate(() => location.hash === '#/library'));
await page.click('#sidebar-root .nav-item[data-view="made"]');
await page.waitForTimeout(150);
await page.click('#nav-back');
await page.waitForTimeout(150);
check('back works', await page.evaluate(() => location.hash === '#/library'));
await page.click('#nav-forward');
await page.waitForTimeout(150);
check('forward works', await page.evaluate(() => location.hash === '#/made'));

// Review fix 1, superseded by C4.1c: the merged Songs list replaces the
// legacy list/grid toggle — verify it stays hidden and navigation works.
await page.click('#sidebar-root .nav-item[data-view="library"]');
await page.waitForTimeout(150);
await page.evaluate(() => window.MusicoStore.set('libTab', 'songs'));
await page.waitForTimeout(200);
check('library view stays active', await page.evaluate(() =>
  document.getElementById('view-library').classList.contains('active')));
check('legacy list/grid toggle stays hidden', await page.evaluate(() =>
  document.querySelector('#view-library .view-toggle').style.display === 'none'));
check('merged songs rows render', await page.evaluate(() =>
  document.querySelectorAll('.lib-rows .lib-row').length >= 8));

// Review fix 2: LCD tracks PositionUpdate + Seeked
await page.evaluate(async () => {
  await Bridge.invoke('online_play', { track: { video_id: 't1', title: 'T', artist: 'A', album: '', duration_secs: 200 } });
});
await page.waitForTimeout(200);
await page.evaluate(() => App.handlePlaybackEvent({ type: 'PositionUpdate', position_secs: 42 }));
await page.waitForTimeout(150);
check('LCD position updates (0:42)', await page.evaluate(() =>
  [...document.querySelectorAll('.lcd-time')][0].textContent === '0:42'));
check('LCD seek fill reflects position', await page.evaluate(() =>
  Number(document.querySelector('.lcd-seek').value) > 100));
await page.evaluate(() => App.handlePlaybackEvent({ type: 'Seeked', position_secs: 120 }));
await page.waitForTimeout(150);
check('LCD tracks Seeked (2:00)', await page.evaluate(() =>
  [...document.querySelectorAll('.lcd-time')][0].textContent === '2:00'));

// Review fix 3: seek sends camelCase
await page.evaluate(() => {
  const orig = Bridge.invoke;
  window.__seekArgs = null;
  Bridge.invoke = (cmd, args) => { if (cmd === 'seek') window.__seekArgs = args; return orig(cmd, args); };
});
await page.evaluate(() => App.transport.seek(30));
await page.waitForTimeout(100);
check('seek sends positionSecs (camelCase)', await page.evaluate(() =>
  window.__seekArgs && window.__seekArgs.positionSecs === 30 && !('position_secs' in window.__seekArgs)));

// Review fix 4: single module instance
check('store module not double-evaluated', await page.evaluate(() => {
  const entries = performance.getEntriesByType('resource').filter(r => r.name.includes('core/store.js'));
  return entries.length === 1;
}));

// Regression: play/pause + search + fullscreen
const before = await page.evaluate(() => !!document.querySelector('.lcd-play svg polygon'));
await page.click('.lcd-play');
await page.waitForTimeout(200);
const after = await page.evaluate(() => !!document.querySelector('.lcd-play svg polygon'));
check('LCD play/pause toggles', before !== after);
// Search moved to its own view (AM layout) — open it, then search.
await page.click('#sidebar-root .nav-item[data-view="search"]');
await page.waitForTimeout(150);
await page.fill('#browse-input', 'teardrop');
await page.waitForTimeout(600);
check('browse search works', await page.evaluate(() =>
  document.querySelectorAll('#browse-results .browse-row').length > 0));
await page.evaluate(() => App.navigateTo('now-playing'));
await page.click('#btn-fullscreen');
check('fullscreen vinyl works', await page.evaluate(() =>
  document.body.classList.contains('player-fullscreen')));
await page.keyboard.press('Escape');
await page.setViewportSize({ width: 700, height: 800 });
await page.waitForTimeout(200);
check('topbar hidden compact', await page.evaluate(() =>
  getComputedStyle(document.getElementById('topbar-root')).display === 'none'));
check('transport pill renders', await page.evaluate(() =>
  !!document.querySelector('#transport-root .transport-bar')));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
await browser.close();
const failed = results.filter(r => r.startsWith('FAIL'));
process.exit(failed.length || errors.length ? 1 : 0);
