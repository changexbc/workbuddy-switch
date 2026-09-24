import assert from 'node:assert/strict';
import { preview } from 'vite';
import { chromium } from 'playwright';
import { defaultSettings } from '../src/settings-config.js';
const server = await preview({preview:{host:'127.0.0.1',port:0,strictPort:true}});
const browser = await chromium.launch({headless:true});
try {
  const context = await browser.newContext({viewport:{width:368,height:600},reducedMotion:'no-preference'});
  await context.route('**/api/settings',route=>route.fulfill({json:defaultSettings()}));
  await context.addInitScript(() => {
    localStorage.setItem('agent-studio.welcome.v1','1'); // Existing users must also get startup motion.
    window.EventSource=class {close(){}};
  });
  const page = await context.newPage();
  const errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  const url = `${server.resolvedUrls.local[0]}desktop.html?welcome`;
  for (let launch=0;launch<2;launch++) {
    await page.goto(url);
    await page.waitForFunction(()=>document.querySelector('#desktop-rail')?.dataset.welcome==='running');
    await page.waitForFunction(()=>document.querySelector('#desktop-rail')?.dataset.welcome==='settled',{},{timeout:12000});
    assert.equal(await page.locator('.desktop-welcome').count(),0,'completed animation releases overlay');
    await page.evaluate(()=>window.dispatchEvent(new StorageEvent('storage',{key:'astra.desktop.preferences.v1'})));
    await page.waitForTimeout(600);
    assert.equal(await page.locator('#desktop-rail').getAttribute('data-welcome'),'settled','settings refresh does not replay');
  }
  await page.emulateMedia({reducedMotion:'reduce'});
  await page.goto(url);
  await page.waitForTimeout(800);
  assert.equal(await page.locator('.desktop-welcome').count(),0,'reduced motion still respected');
  assert.deepEqual(errors,[]);
  console.log('PASS: two launches replay despite legacy seen flag; no replay on settings refresh; reduced motion respected');
} finally {await browser.close();await server.httpServer.close();}
