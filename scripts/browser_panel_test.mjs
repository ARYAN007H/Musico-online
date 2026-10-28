import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

// 1. Panel hidden initially
check('panel hidden initially', await page.evaluate(() =>
  !document.querySelector('.queue-panel')));

// 2. Queue some songs, then open panel
await page.evaluate(async () => {
  await Bridge.invoke('online_play', { track: { video_id: 's1', title: 'Now Song', artist: 'A1', album: '', duration_secs: 200 } });
  await Bridge.invoke('add_to_queue', { song: { id: 'q2', title: 'Next One', artist: 'A2', album: '', duration_secs: 180, file_path: '/x' } });
  await Bridge.invoke('add_to_queue', { song: { id: 'q3', title: 'After That', artist: 'A3', album: '', duration_secs: 150, file_path: '/y' } });
});
await page.waitForTimeout(300);
await page.click('.topbar-right .lcd-btn[title="Playing Next"]');
await page.waitForTimeout(300);
check('panel opens from LCD', await page.evaluate(() => !!document.querySelector('.queue-panel')));
check('now playing row present', await page.evaluate(() =>
  [...document.querySelectorAll('.qp-row .qp-title')].some(e => e.textContent === 'Now Song')));
check('upcoming rows present', await page.evaluate(() => {
  const t = [...document.querySelectorAll('.qp-row .qp-title')].map(e => e.textContent);
  return t.includes('Next One') && t.includes('After That');
}));
check('current song excluded from upcoming', await page.evaluate(() => {
  const rows = [...document.querySelectorAll('.qp-row.qp-current .qp-title')];
  return rows.length === 1 && rows[0].textContent === 'Now Song';
}));

// 3. History fills when a new song starts
await page.evaluate(async () => {
  await Bridge.invoke('online_play', { track: { video_id: 's2', title: 'Second Song', artist: 'B1', album: '', duration_secs: 190 } });
});
await page.waitForTimeout(300);
check('history section fills', await page.evaluate(() => {
  const sections = [...document.querySelectorAll('.qp-section')];
  return sections.some(e => e.textContent === 'Recently Played');
}) && await page.evaluate(() =>
  [...document.querySelectorAll('.qp-row .qp-title')].some(e => e.textContent === 'Now Song')));

// 4. Autoplay toggle persists via backend mock
check('autoplay toggle checked by default', await page.evaluate(() =>
  document.querySelector('.qp-autoplay input')?.checked === true));
await page.click('.qp-autoplay');
await page.waitForTimeout(200);
check('autoplay toggles off + calls backend', await page.evaluate(async () => {
  const settings = await Bridge.invoke('get_settings');
  return settings.autoplay === false;
}));
await page.click('.qp-autoplay');
await page.waitForTimeout(200);

// 5. Close via X
await page.click('.qp-header .lcd-btn[title="Close"]');
await page.waitForTimeout(200);
check('panel closes', await page.evaluate(() => !document.querySelector('.queue-panel')));

// 6. Panel reopens with state; playing a queued row works
await page.click('.topbar-right .lcd-btn[title="Playing Next"]');
await page.waitForTimeout(200);
const rowTitles = await page.evaluate(() => [...document.querySelectorAll('.qp-row .qp-title')].map(e => e.textContent));
await page.evaluate(() => {
  const rows = [...document.querySelectorAll('.qp-row')];
  const target = rows.find(r => r.querySelector('.qp-title')?.textContent === 'Next One');
  target.click();
});
await page.waitForTimeout(300);
check('clicking queued row plays it', await page.evaluate(() =>
  App.state.currentSong?.title === 'Next One'));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
await browser.close();
const failed = results.filter(r => r.startsWith('FAIL'));
process.exit(failed.length || errors.length ? 1 : 0);
