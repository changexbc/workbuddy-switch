import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { preview } from 'vite';
import { chromium } from 'playwright';
import { defaultSettings } from '../src/settings-config.js';

const server = await preview({preview:{host:'127.0.0.1',port:0,strictPort:true}});
const browser = await chromium.launch({headless:true});
const url = `${server.resolvedUrls.local[0]}desktop-settings.html`;
try {
  await fs.mkdir('artifacts/ui', {recursive:true});
  const staticPage = await browser.newPage({javaScriptEnabled:false, viewport:{width:480,height:700}});
  await staticPage.goto(url);
  assert(await staticPage.locator('#settings-loading').isVisible(), 'HTML paints before JavaScript');
  assert.equal(await staticPage.locator('.skeleton-card').first().evaluate(el => Math.round(el.getBoundingClientRect().height)), 117);
  await staticPage.screenshot({path:'artifacts/ui/settings-loading.png'});
  await staticPage.close();

  const page = await browser.newPage({reducedMotion:'reduce',viewport:{width:480,height:700}});
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  let releaseRead;
  let signalRead;
  let started = new Promise(resolve => { signalRead = resolve; });
  let gate = new Promise(resolve => { releaseRead = resolve; });
  let fail = true;
  await page.route('**/api/settings', async route => {
    signalRead();
    await gate;
    await route.fulfill(fail ? {status:500,json:{error:'测试读取失败'}} : {json:defaultSettings()});
  });
  await page.goto(url);
  await started;
  assert(await page.locator('#settings-loading').isVisible(), 'skeleton stays while reading');
  assert(await page.locator('#root main').isHidden(), 'placeholder values are hidden');
  assert.equal(await page.locator('.settings-skeleton').evaluate(el => getComputedStyle(el).animationName), 'none');
  releaseRead();
  await page.locator('[data-action=retry]').waitFor({state:'visible'});
  assert(await page.locator('#settings-loading').isHidden(), 'failure replaces loading');
  assert.match(await page.locator('#save-status').innerText(), /测试读取失败/);
  fail = false;
  started = new Promise(resolve => { signalRead = resolve; });
  gate = new Promise(resolve => { releaseRead = resolve; });
  await page.locator('[data-action=retry]').click();
  await started;
  assert(await page.locator('#settings-loading').isVisible(), 'retry restores skeleton');
  releaseRead();
  await page.waitForFunction(() => document.querySelector('fieldset')?.disabled === false);
  assert(await page.locator('#settings-loading').isHidden());
  assert.equal(await page.locator('[data-action=save]').count(),0);
  assert.match(await page.locator('#save-status').innerText(),/实时生效/);
  assert.deepEqual(errors, []);
  console.log('PASS: pre-JS skeleton, slow read, failure, retry, success, reduced motion');
} finally {
  await browser.close();
  await server.httpServer.close();
}
