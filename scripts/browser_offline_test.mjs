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

// 1. The Airplane card renders; default state is online.
await page.evaluate(() => window.MusicoRouter.navigate('settings'));
await page.waitForTimeout(400);
check('airplane toggle renders online', await page.evaluate(() => {
  const t = document.getElementById('toggle-offline');
  return !!t && !t.classList.contains('on') &&
    document.getElementById('offline-hint').textContent.includes('Online');
}));

// 2. Flipping the toggle fast-fails online features (honest offline state).
await page.click('#toggle-offline');
await page.waitForTimeout(300);
check('set_offline(true) called', (await ipcCalls('set_offline')).some(
  (a) => a?.enabled === true));
check('hint flips to offline', await page.evaluate(() =>
  document.getElementById('offline-hint').textContent.includes('Offline')));
check('get_offline reflects the flag', await page.evaluate(() =>
  window.MusicoStore.get('offline') === true));

// 3. Offline: the home feed keeps its cache (no blank page)...
await page.evaluate(() => window.MusicoRouter.navigate('browse'));
await page.waitForTimeout(300);
check('cached home feed still renders offline', await page.evaluate(() =>
  document.querySelectorAll('#home-root .shelf-card').length >= 5));

// 4. ...but a fresh search fast-fails instead of hanging.
await page.fill('#browse-input', 'teardrop');
await page.waitForTimeout(500);
check('search fast-fails offline', await page.evaluate(() => {
  const status = document.getElementById('browse-status')?.textContent || '';
  return status.toLowerCase().includes('offline') || status.toLowerCase().includes('fail');
}));

// 5. Downloads requested offline are queued (command still succeeds).
await page.evaluate(() => window.Bridge.invoke('online_download_batch', {
  label: 'Offline Album', tracks: [{ video_id: 'x1', title: 'X', artist: 'A' }],
}));
await page.waitForTimeout(200);
check('batch request succeeds while offline', (await ipcCalls('online_download_batch')).length === 1);

// 6. Lifting the switch flips everything back.
await page.evaluate(() => window.MusicoRouter.navigate('settings'));
await page.waitForTimeout(300);
await page.click('#toggle-offline');
await page.waitForTimeout(300);
check('set_offline(false) called on lift', (await ipcCalls('set_offline')).some(
  (a) => a?.enabled === false));
check('hint back to online', await page.evaluate(() =>
  document.getElementById('offline-hint').textContent.includes('Online')));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
const failed = results.filter((r) => r.startsWith('FAIL'));
await browser.close();
process.exit(failed.length || errors.length ? 1 : 0);
