import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

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

// Navigate to Settings.
await page.evaluate(() => window.MusicoRouter.navigate('settings'));
await page.waitForTimeout(400);

// 1. The Downloads & Storage card renders with live usage from the mock.
check('storage card renders', await page.evaluate(() =>
  !!document.getElementById('storage-cap-slider') &&
  !!document.getElementById('btn-clear-downloads')));
check('usage readout shows mock values', await page.evaluate(() =>
  document.getElementById('storage-usage')?.textContent.includes('300 MB of 1024 MB used')));

// 2. Slider init + change persists the cap and reports post-prune usage.
check('slider initialized to cap', await page.evaluate(() =>
  document.getElementById('storage-cap-slider').value === '1024'));
await page.evaluate(() => {
  const sl = document.getElementById('storage-cap-slider');
  sl.value = '2048';
  sl.dispatchEvent(new Event('input', { bubbles: true }));
  sl.dispatchEvent(new Event('change', { bubbles: true }));
});
await page.waitForTimeout(300);
const capCalls = await ipcCalls('set_online_cache_cap');
check('slider change calls set_online_cache_cap', capCalls.length === 1 &&
  capCalls[0]?.mb === 2048);
check('slider label follows selection', await page.evaluate(() =>
  document.getElementById('storage-cap-value')?.textContent === '2048 MB'));
check('cap persisted into settings snapshot', await page.evaluate(() =>
  window.App?.state?.settings?.online_cache_mb === 2048));

// 3. Clear downloads: dialog confirm → online_clear_cache → usage refresh.
await page.evaluate(() => { window.__confirm = true; window.confirm = () => window.__confirm; });
await page.evaluate(() => document.getElementById('btn-clear-downloads').click());
await page.waitForTimeout(300);
check('clear calls online_clear_cache', (await ipcCalls('online_clear_cache')).length === 1);

// 4. Declining the dialog does nothing.
await page.evaluate(() => { window.__confirm = false; });
await page.evaluate(() => document.getElementById('btn-clear-downloads').click());
await page.waitForTimeout(200);
check('declined clear makes no second call', (await ipcCalls('online_clear_cache')).length === 1);

// 5. Download action from the context menu marks the row, then done.
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
await page.evaluate(() => {
  const row = document.querySelector('[data-video-id]');
  const r = row.getBoundingClientRect();
  row.dispatchEvent(new MouseEvent('contextmenu', {
    bubbles: true, clientX: r.x + 20, clientY: r.y + 10,
  }));
});
await page.waitForTimeout(150);
await page.click('.ctx-menu .ctx-item >> text=Download');
await page.waitForTimeout(300);
check('downloaded row gets dl-done badge', await page.evaluate(() =>
  !!document.querySelector('.dl-done')));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
