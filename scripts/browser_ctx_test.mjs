import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

const ctxClick = (sel) => page.evaluate(({ sel }) => {
  const el = document.querySelector(sel);
  if (!el) return false;
  const r = el.getBoundingClientRect();
  el.dispatchEvent(new MouseEvent('contextmenu', {
    bubbles: true, cancelable: true,
    clientX: Math.round(r.x + r.width / 2), clientY: Math.round(r.y + r.height / 2),
  }));
  return true;
}, { sel });

// Spy on the bridge for the two C3.1 commands.
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

// 1. Right-click a song card opens the full song menu.
const songSel = '#home-root .shelf-card[data-ctx="song"]';
check('song cards carry context data', await page.evaluate(
  (sel) => !!document.querySelector(sel) &&
    !!document.querySelector(sel).dataset.videoId, songSel));
await ctxClick(songSel);
await page.waitForTimeout(150);
check('song menu opens with 6 items', await page.evaluate(() => {
  const items = [...document.querySelectorAll('.ctx-menu .ctx-item')];
  return items.length === 6 &&
    items.map((i) => i.textContent).join('|') ===
    'Play|Play Next|Play Last|Download|Add to Library|Start Radio';
}));

// 1b. Playlist cards get no Add to Library — the library has no playlist kind.
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
await page.evaluate((sel) => {
  const el = document.querySelector(sel);
  const r = el.getBoundingClientRect();
  el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: r.x + 5, clientY: r.y + 5 }));
}, '#home-root .shelf-card[data-ctx="nav"][data-kind="playlist"]');
await page.waitForTimeout(150);
check('playlist menu is Open-only', await page.evaluate(() =>
  [...document.querySelectorAll('.ctx-menu .ctx-item')].map((i) => i.textContent).join('|') === 'Open'));
await page.keyboard.press('Escape');

// 2. Play Next routes through online_play_next and closes the menu.
await ctxClick(songSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Play Next');
await page.waitForTimeout(200);
const pn = await ipcCalls('online_play_next');
check('Play Next calls online_play_next', pn.length === 1 &&
  typeof pn[0]?.track?.video_id === 'string');
check('menu closes after action', await page.evaluate(
  () => !document.querySelector('.ctx-menu')));

// 3. Play Last routes through the existing enqueue command.
await ctxClick(songSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Play Last');
await page.waitForTimeout(200);
check('Play Last calls online_enqueue', (await ipcCalls('online_enqueue')).length === 1);

// 4. Start Radio routes through online_start_radio.
await ctxClick(songSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Start Radio');
await page.waitForTimeout(200);
check('Start Radio calls online_start_radio', (await ipcCalls('online_start_radio')).length === 1);

// 5. Escape dismisses the menu without acting.
await ctxClick(songSel);
await page.waitForTimeout(150);
await page.keyboard.press('Escape');
await page.waitForTimeout(150);
check('Escape closes the menu', await page.evaluate(
  () => !document.querySelector('.ctx-menu')));

// 5b. Download routes through online_download and flips the row badge.
await ctxClick(songSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Download');
await page.waitForTimeout(300);
check('Download calls online_download', (await ipcCalls('online_download')).length === 1);
check('downloaded row gets dl-done badge', await page.evaluate(() =>
  !!document.querySelector('.dl-done')));

// 6. Album/artist cards get an Open-only menu that navigates.
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
const navSel = '#home-root .shelf-card[data-ctx="nav"]';
await ctxClick(navSel);
await page.waitForTimeout(150);
check('nav card menu has only Open', await page.evaluate(() => {
  const items = [...document.querySelectorAll('.ctx-menu .ctx-item')];
  return items.length === 1 && items[0].textContent === 'Open';
}));
await page.click('.ctx-menu .ctx-item');
await page.waitForTimeout(300);
check('Open navigates to a detail route', await page.evaluate(() =>
  location.hash.startsWith('#/browse/')));

// 7. Right-click on plain background opens nothing.
await page.evaluate(() => window.MusicoRouter.back());
await page.waitForTimeout(300);
await page.evaluate(() => {
  document.body.dispatchEvent(new MouseEvent('contextmenu', {
    bubbles: true, clientX: 400, clientY: 400,
  }));
});
await page.waitForTimeout(150);
check('no menu on non-context target', await page.evaluate(
  () => !document.querySelector('.ctx-menu')));

// 8. Queue rows: Remove from Queue routes through remove_from_queue.
await page.evaluate(() => window.MusicoStore.set('queuePanel', true));
await page.waitForTimeout(300);
const rowSel = '.qp-row[data-ctx="qrow"]';
check('queue rows carry context data', await page.evaluate(
  (sel) => !!document.querySelector(sel) &&
    !!document.querySelector(sel).dataset.qid, rowSel));
await ctxClick(rowSel);
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Remove from Queue');
await page.waitForTimeout(200);
check('Remove calls remove_from_queue', (await ipcCalls('remove_from_queue')).length === 1 &&
  typeof (await ipcCalls('remove_from_queue'))[0]?.songId === 'string');

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
