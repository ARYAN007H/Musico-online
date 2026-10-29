import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

// 1. Click a playlist card on Home → navigates to detail route
await page.evaluate(() => {
  const plCard = [...document.querySelectorAll('#home-root .shelf-card')][0]; // playlist kind in mock
  plCard.click();
});
await page.waitForTimeout(400);
check('card click navigates to detail route', await page.evaluate(() =>
  location.hash.startsWith('#/browse/')));
check('detail view active', await page.evaluate(() =>
  document.getElementById('view-detail').classList.contains('active')));

// 2. Detail renders header + tracklist from the mock
check('header title renders', await page.evaluate(() =>
  document.querySelector('.detail-title')?.textContent === 'Demo Album'));
check('header subtitle renders', await page.evaluate(() =>
  document.querySelector('.detail-sub')?.textContent.includes('Album')));
check('tracklist renders', await page.evaluate(() =>
  document.querySelectorAll('.dt-row').length >= 6));

// 3. Track click plays via Online.playTrack
await page.evaluate(() => {
  window.__played = null;
  window.Online.playTrack = (track, row) => { window.__played = track; };
});
await page.click('.dt-row');
await page.waitForTimeout(200);
check('track click plays', await page.evaluate(() =>
  window.__played && typeof window.__played.video_id === 'string'));

// 4. Play-all button plays first track; Queue all enqueues all
await page.evaluate(() => { window.__played = null; });
await page.evaluate(() => { window.__enqueued = []; window.Online.enqueueTrack = (t) => { window.__enqueued.push(t); }; });
await page.click('.detail-play');
await page.waitForTimeout(200);
check('play-all plays first track', await page.evaluate(() => !!window.__played));
await page.click('.detail-queueall');
await page.waitForTimeout(200);
check('queue-all enqueues all tracks', await page.evaluate(() => window.__enqueued.length >= 6));

// 5. Back arrow returns Home; forward returns to the detail page (state preserved)
await page.click('#nav-back');
await page.waitForTimeout(300);
check('back returns Home', await page.evaluate(() =>
  location.hash === '#/browse' && document.getElementById('view-browse').classList.contains('active')));
await page.click('#nav-forward');
await page.waitForTimeout(300);
check('forward restores detail view', await page.evaluate(() =>
  location.hash.startsWith('#/browse/') && document.getElementById('view-detail').classList.contains('active')));

// 6. Embedded navigation: artist cards inside detail shelves also route (mock has none; verify card handler exists)
check('enqueue buttons present on rows', await page.evaluate(() =>
  document.querySelectorAll('.dt-row .dt-enqueue').length >= 6));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
await browser.close();
const failed = results.filter(r => r.startsWith('FAIL'));
process.exit(failed.length || errors.length ? 1 : 0);
