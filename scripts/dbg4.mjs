import { chromium } from 'playwright';
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
page.on('pageerror', (e) => console.log('PAGEERR:', e.message));
await page.goto('http://127.0.0.1:8143/index.html');
await page.waitForTimeout(900);
await page.evaluate(async () => {
  await window.Bridge.invoke('library_add', { entry: { id: 'MPREb_zeta', kind: 'album', title: 'Zeta Waves', subtitle: 'Vela', browse_id: 'MPREb_zeta', params: null, thumbnail: null } });
  await window.Bridge.invoke('library_add', { entry: { id: 'UC_alpha', kind: 'artist', title: 'Alpha Duo', subtitle: 'Artist', browse_id: 'UC_alpha', params: null, thumbnail: null } });
  window.MusicoStore.set('libAlbums', null);
  window.MusicoStore.set('libArtists', null);
  window.MusicoStore.set('libTracks', null);
  window.MusicoStore.set('libRev', (window.MusicoStore.get('libRev') || 0) + 1);
});
await page.evaluate(() => window.MusicoRouter.navigate('library'));
await page.waitForTimeout(600);
console.log(JSON.stringify(await page.evaluate(() => ({
  active: window.MusicoStore.get('activeNav'),
  albums: window.MusicoStore.get('libAlbums')?.map((e) => e.title),
  artists: window.MusicoStore.get('libArtists')?.map((e) => e.title),
  tab: window.MusicoStore.get('libTab'),
  cards: [...document.querySelectorAll('#library-catalog .lib-card-title')].map((e) => e.textContent),
}))));
await browser.close();
