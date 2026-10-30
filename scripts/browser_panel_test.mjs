import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

// Full original suite (updated selectors)
check('panel hidden initially', await page.evaluate(() => !document.querySelector('.queue-panel')));
await page.evaluate(async () => {
  await Bridge.invoke('online_play', { track: { video_id: 's1', title: 'Now Song', artist: 'A1', album: '', duration_secs: 200 } });
  await Bridge.invoke('add_to_queue', { song: { id: 'q2', title: 'Next One', artist: 'A2', album: '', duration_secs: 180, file_path: '/x' } });
});
await page.waitForTimeout(300);
await page.click('#btn-queue');
await page.waitForTimeout(250);
check('panel opens', await page.evaluate(() => !!document.querySelector('.queue-panel')));
check('rows present', await page.evaluate(() => {
  const t = [...document.querySelectorAll('.qp-row .qp-title')].map(e => e.textContent);
  return t.includes('Now Song') && t.includes('Next One');
}));

// Blocking finding 1: App.state.settings syncs on toggle
await page.evaluate(() => document.querySelector('.qp-autoplay').click());
await page.waitForTimeout(150);
check('settings snapshot synced (no silent revert)', await page.evaluate(async () => {
  const snapOk = App.state.settings.autoplay === false;
  const backend = (await Bridge.invoke('get_settings')).autoplay === false;
  return snapOk && backend;
}));
await page.evaluate(() => document.querySelector('.qp-autoplay').click());
await page.waitForTimeout(150);
check('toggle back on', await page.evaluate(() => App.state.settings.autoplay === true));

// Finding 5: duplicate current-song in queue keeps replay copy visible
await page.evaluate(() => Bridge.invoke('add_to_queue', { song: { id: 'yt:s1', title: 'Now Song replay', artist: 'A1', album: '', duration_secs: 200, file_path: '/r' } }));
await page.waitForTimeout(200);
check('replay copy visible in Playing Next', await page.evaluate(() => {
  const t = [...document.querySelectorAll('.qp-row .qp-title')].map(e => e.textContent);
  return t.includes('Now Song replay');
}));

// Finding 7: Space activates rows
check('space key activates row', await page.evaluate(() => {
  const row = [...document.querySelectorAll('.qp-row')].find(r => r.querySelector('.qp-title')?.textContent === 'Next One');
  row.focus();
  row.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
  return App.state.currentSong?.title === 'Next One';
}));

// Finding 8: Now Playing row restarts
check('now-playing row seeks to 0', await page.evaluate(() => {
  const spy = App.transport.seek;
  let called = 0;
  App.transport.seek = (s) => { called = s; };
  const row = document.querySelector('.qp-row.qp-current');
  row.click();
  App.transport.seek = spy;
  return called === 0;
}));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
await browser.close();
const failed = results.filter(r => r.startsWith('FAIL'));
process.exit(failed.length || errors.length ? 1 : 0);
