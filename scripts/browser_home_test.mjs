import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(1000);
const results = [];
const check = (n, c) => results.push(`${c ? 'PASS' : 'FAIL'} ${n}`);

// 1. Home feed renders shelves from the mock
check('home feed renders shelves', await page.evaluate(() =>
  document.querySelectorAll('#home-root .shelf').length >= 2));
check('shelf titles render', await page.evaluate(() =>
  [...document.querySelectorAll('.shelf-title')].map(e => e.textContent).includes('Late Night Lounge')));
check('cards render with titles', await page.evaluate(() =>
  document.querySelectorAll('#home-root .shelf-card').length >= 8));

// 2. Song cards play; playlist cards toast honestly
const songCard = await page.evaluateHandle(() =>
  [...document.querySelectorAll('.shelf-card')].find(c => c.__itemKind === undefined));
await page.evaluate(() => {
  // find a song-kind card by clicking behavior: song cards have video_id
  const cards = [...document.querySelectorAll('#home-root .shelf-card')];
  window.__clicked = null;
  const origPlay = window.Online.playTrack;
  window.Online.playTrack = (track, row) => { window.__clicked = track; };
});
await page.evaluate(() => {
  const songCard = [...document.querySelectorAll('#home-root .shelf-card')][6]; // second shelf = songs in mock
  songCard.click();
});
await page.waitForTimeout(200);
check('song card calls playTrack', await page.evaluate(() =>
  window.__clicked && typeof window.__clicked.video_id === 'string'));

await page.evaluate(() => {
  const plCard = [...document.querySelectorAll('#home-root .shelf-card')][0]; // first shelf = playlists
  plCard.click();
});
await page.waitForTimeout(200);
check('playlist card shows honest toast', await page.evaluate(() => {
  const toast = document.getElementById('musico-toast');
  return toast && toast.textContent.includes('next release');
}));

// 3. Feed caches in store (no refetch on rerender)
check('feed cached in store', await page.evaluate(() =>
  !!window.MusicoStore.get('homeFeed')));

// 4. Regressions: search still present under the feed, LCD/panel fine
check('search still present on Home', await page.evaluate(() =>
  !!document.getElementById('browse-input')));
await page.fill('#browse-input', 'teardrop');
await page.waitForTimeout(600);
check('search works alongside feed', await page.evaluate(() =>
  document.querySelectorAll('#browse-results .browse-row').length > 0));

console.log(results.join('\n'));
console.log('JS errors:', errors.length ? errors : 'none');
await browser.close();
const failed = results.filter(r => r.startsWith('FAIL'));
process.exit(failed.length || errors.length ? 1 : 0);
