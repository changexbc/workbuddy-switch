/**
 * Browser QA for the public web demo.
 *
 * Serves the demo build (`npm run build:demo`) from the repository's GitHub
 * Pages subpath, then drives the real page: the root entry, the embedded rail,
 * scenario switches, dragging, keyboard focus and the desktop-only feedback.
 * Records every request so the isolation claims can be asserted instead of
 * assumed, and writes screenshots plus a JSON report to `artifacts/demo/`.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');

const ROOT = path.resolve(import.meta.dirname, '..');
const BUILD = path.join(ROOT, 'dist-demo');
const BASE = '/agent-companion';
const PORT = 4197;
const ORIGIN = `http://127.0.0.1:${PORT}`;
const OUT = 'artifacts/demo';
const MIME = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.png': 'image/png', '.ico': 'image/x-icon', '.svg': 'image/svg+xml', '.json': 'application/json'};

// The subpath is part of what is under test, so the server mounts the build at
// /agent-companion/ exactly like GitHub Pages does.
const server = http.createServer(async (request, response) => {
  const pathname = new URL(request.url, ORIGIN).pathname;
  const relative = decodeURIComponent(pathname.slice(BASE.length + 1)) || 'index.html';
  const file = path.resolve(BUILD, relative);
  if (!pathname.startsWith(`${BASE}/`) || !file.startsWith(BUILD)) {
    response.writeHead(404, {'content-type': 'text/plain; charset=utf-8'});
    response.end('not found');
    return;
  }
  try {
    const body = await fs.readFile(file);
    response.writeHead(200, {'content-type': MIME[path.extname(file)] || 'application/octet-stream'});
    response.end(body);
  } catch {
    response.writeHead(404, {'content-type': 'text/plain; charset=utf-8'});
    response.end('not found');
  }
});
await new Promise(resolve => server.listen(PORT, '127.0.0.1', resolve));

const errors = [], failed = [], requests = [], checks = [];
const browser = await chromium.launch({headless: true});
await fs.mkdir(OUT, {recursive: true});

function watch(page) {
  page.on('pageerror', error => errors.push(`pageerror: ${error.message}`));
  page.on('console', message => { if (message.type() === 'error') errors.push(`console: ${message.text()}`); });
  page.on('request', request => requests.push(request.url()));
  page.on('requestfailed', request => { if (!request.url().endsWith('favicon.ico')) failed.push(`${request.url()} ${request.failure()?.errorText}`); });
  page.on('response', response => { if (response.status() >= 400 && !response.url().endsWith('favicon.ico')) failed.push(`${response.status()} ${response.url()}`); });
}

async function openDemo(context) {
  const page = await context.newPage();
  watch(page);
  // AC1: the Pages root is what a visitor opens; it forwards to the demo entry.
  await page.goto(`${ORIGIN}${BASE}/`);
  await page.waitForURL(/\/agent-companion\/demo\.html$/);
  await page.locator('#demo-loading').waitFor({state: 'hidden'});
  await page.locator('.demo-intro').waitFor({state: 'detached'});
  const rail = await (await page.waitForSelector('#demo-frame')).contentFrame();
  await rail.waitForSelector('.desktop-avatar');
  await page.waitForFunction(() => document.querySelector('#demo-status')?.textContent?.includes('工作中'));
  return {page, rail};
}

/** Rail surfaces measured inside the frame, where the rail's own coordinates live. */
async function frameSurfaces(rail, selectors) {
  return rail.evaluate(list => list.map(selector => {
    const box = document.querySelector(selector)?.getBoundingClientRect();
    return box && {selector, left: box.left, top: box.top, right: box.right, bottom: box.bottom, width: innerWidth, height: innerHeight};
  }).filter(Boolean), selectors);
}

try {
  // Cold starts: hold the page module and frame stylesheet independently.
  // Neither case may expose unstyled controls or an unfinished rail.
  for (const blockedAsset of [/\/assets\/demo-[^/]+\.js$/, /\/assets\/desktop-[^/]+\.css$/]) {
    const slowContext = await browser.newContext({viewport: {width: 1440, height: 900}, reducedMotion: 'reduce'});
    const slowPage = await slowContext.newPage();
    let release, reached;
    const gate = new Promise(resolve => { release = resolve; });
    const intercepted = new Promise(resolve => { reached = resolve; });
    await slowPage.route(blockedAsset, async route => { reached(); await gate; await route.continue(); });
    try {
      await slowPage.goto(`${ORIGIN}${BASE}/demo.html`, {waitUntil: 'commit'});
      await intercepted;
      assert.ok(await slowPage.locator('#demo-loading').isVisible(), 'inline loading renders before the delayed asset');
      assert.equal(await slowPage.locator('.demo-shell').evaluate(node => getComputedStyle(node).visibility), 'hidden');
      assert.equal(await slowPage.locator('.demo-shell').evaluate(node => node.inert), true);
      release();
      await slowPage.locator('#demo-loading').waitFor({state: 'hidden'});
      assert.equal(await slowPage.locator('.demo-shell').evaluate(node => node.inert), false);
      const readyFrame = await (await slowPage.locator('#demo-frame').elementHandle()).contentFrame();
      assert.equal(await readyFrame.locator('.desktop-avatar[data-status=running]:visible').count(), 2);
      assert.equal(await readyFrame.locator('.desktop-avatar[data-status=wait]:visible').count(), 0);
      assert.equal(await readyFrame.locator('.desktop-strip').evaluate(node => getComputedStyle(node).position), 'absolute');
    } finally { release(); await slowContext.close(); }
  }
  checks.push('cold startup hides the scene until delayed page JS and rail CSS are ready');

  // A module failure still leaves a usable retry without depending on that module.
  const failureContext = await browser.newContext();
  const failurePage = await failureContext.newPage();
  await failurePage.clock.install();
  await failurePage.route(/\/assets\/demo-[^/]+\.js$/, route => route.abort());
  await failurePage.goto(`${ORIGIN}${BASE}/demo.html`);
  await failurePage.screenshot({path: `${OUT}/loading.png`});
  await failurePage.clock.fastForward(16000);
  assert.ok(await failurePage.locator('#demo-loading-retry').isVisible());
  assert.equal(await failurePage.locator('.demo-shell').evaluate(node => node.inert), true);
  await failureContext.close();
  checks.push('failed application load exposes an independent retry link');

  // The actual kitten opening finishes before the automatic scenario sequence.
  const introContext = await browser.newContext({viewport: {width: 1440, height: 900}, reducedMotion: 'no-preference'});
  const introPage = await introContext.newPage();
  watch(introPage);
  await introPage.goto(`${ORIGIN}${BASE}/demo.html`);
  await introPage.locator('#demo-loading').waitFor({state: 'hidden'});
  await introPage.locator('.demo-intro[data-phase=running]').waitFor();
  await introPage.waitForFunction(() => Number(document.querySelector('.demo-intro')?.getAttribute('data-elapsed')) > 1.2);
  assert.equal(await introPage.locator('#demo-frame').evaluate(node => node.style.visibility), 'hidden');
  const openingFrame = await (await introPage.locator('#demo-frame').elementHandle()).contentFrame();
  const finalHeight = await openingFrame.locator('.desktop-strip').evaluate(node => node.getBoundingClientRect().height);
  const drawnHeight = await introPage.locator('.demo-intro-anchor').evaluate(node => node.getBoundingClientRect().height);
  assert.ok(finalHeight > 140 && Math.abs(drawnHeight - finalHeight) < 1, 'kitten draws the final two-row rail, not its initial empty height');
  assert.equal(await introPage.locator('.demo-brand').count(), 0);
  assert.equal(await introPage.locator('[data-reveal].is-revealed').count(), 0, 'tasks have not advanced during the kitten opening');
  assert.equal(await introPage.locator('[data-client=workbuddy] .story-choice').isVisible(), false);
  const stream = introPage.locator('.story-stream i').first();
  const beforeStream = await stream.evaluate(node => getComputedStyle(node).clipPath);
  await introPage.waitForTimeout(1200);
  assert.notEqual(await stream.evaluate(node => getComputedStyle(node).clipPath), beforeStream, 'records animate while the kitten is playing');
  assert.equal(await introPage.locator('.story-stream:visible').count(), 2);
  await introPage.screenshot({path: `${OUT}/opening-kitten.png`});
  await introPage.locator('.demo-intro').waitFor({state: 'detached'});
  assert.equal(await introPage.locator('#demo-frame').evaluate(node => node.style.visibility), '');
  assert.equal(await introPage.locator('[data-reveal].is-revealed').count(), 0, 'the story leaves a breathing pause after the opening');
  checks.push('kitten opening finishes before either task progresses or asks a question');
  // Measure actual rendered movement with motion enabled, not just a class or
  // transition declaration. Both windows must travel through intermediate boxes.
  const motion = await introPage.evaluate(async () => {
    const windows = [...document.querySelectorAll('[data-client]')];
    const boxes = () => windows.map(node => {
      const rect = node.getBoundingClientRect();
      return {x: rect.x, y: rect.y, width: rect.width, height: rect.height};
    });
    const animations = () => windows.flatMap(node => node.getAnimations());
    const nextFrame = () => new Promise((resolve, reject) => {
      const timer = setTimeout(() => { cancelAnimationFrame(frame); reject(new Error('Client motion frame timed out')); }, 2000);
      const frame = requestAnimationFrame(() => { clearTimeout(timer); resolve(); });
    });
    const waitForIntermediateFrame = async animation => {
      const deadline = performance.now() + 2000;
      while (true) {
        if (!animation || animation.playState === 'idle' || animation.playState === 'finished') {
          throw new Error('Client motion ended before an intermediate frame was sampled');
        }
        if (Number(animation.currentTime ?? 0) >= 80) return;
        if (performance.now() >= deadline) throw new Error('Client motion did not advance');
        await nextFrame();
      }
    };
    const start = boxes();
    document.querySelector('[data-activate-client=codex]').click();
    const started = animations();
    document.querySelector('[data-activate-client=codex]').click();
    const sameSelectionKeptAnimation = started.every((animation, i) => animation === animations()[i]);
    const immediate = boxes();
    await waitForIntermediateFrame(started[0]);
    const middle = boxes();
    await Promise.all(started.map(animation => animation.finished));
    await nextFrame();
    const end = boxes();
    document.querySelector('[data-activate-client=workbuddy]').click();
    const reversing = animations();
    await waitForIntermediateFrame(reversing[0]);
    const beforeReversal = boxes();
    document.querySelector('[data-activate-client=codex]').click();
    const afterReversal = boxes();
    await Promise.all(animations().map(animation => animation.finished));
    await nextFrame();
    return {start, immediate, middle, end, beforeReversal, afterReversal, settled: boxes(),
      sameSelectionKeptAnimation, animationCount: started.length, remaining: animations().length,
      active: document.querySelector('.demo-scene').dataset.activeClient,
      frameTransform: getComputedStyle(document.querySelector('#demo-window')).transform};
  });
  assert.equal(motion.animationCount, 2, 'both clients animate');
  assert.ok(motion.sameSelectionKeptAnimation, 'same client selection does not restart motion');
  for (let i = 0; i < 2; i++) {
    for (const key of ['x', 'y', 'width', 'height']) {
      assert.ok(Math.abs(motion.start[i][key] - motion.immediate[i][key]) < 1, 'switch begins at the painted geometry');
      const a = motion.start[i][key], b = motion.end[i][key], mid = motion.middle[i][key];
      assert.ok(mid > Math.min(a, b) + .2 && mid < Math.max(a, b) - .2, `${key} visibly interpolates between positions`);
      assert.ok(Math.abs(motion.beforeReversal[i][key] - motion.afterReversal[i][key]) < 1, 'rapid reversal has no position jump');
      assert.ok(Math.abs(motion.settled[i][key] - motion.end[i][key]) < 1, 'rapid reversal settles at the final selection');
    }
  }
  assert.equal(motion.active, 'codex');
  assert.equal(motion.remaining, 0, 'completed swaps leave no animations behind');
  assert.equal(motion.frameTransform, 'none', 'client animation never transforms the rail viewport');
  checks.push('client swaps interpolate without jumps and release animations');
  await introPage.evaluate(() => document.querySelector('[data-activate-client=codex]').click());
  await introPage.setViewportSize({width: 1400, height: 900});
  await introPage.waitForFunction(() => [...document.querySelectorAll('[data-client]')].every(node => node.getAnimations().length === 0));
  assert.equal(await introPage.locator('.demo-scene').getAttribute('data-active-client'), 'codex', 'resize retains the chosen client');
  await introPage.setViewportSize({width: 1440, height: 900});
  await introPage.evaluate(() => document.querySelector('[data-activate-client=workbuddy]').click());
  await introPage.emulateMedia({reducedMotion: 'reduce'});
  await introPage.waitForFunction(() => [...document.querySelectorAll('[data-client]')].every(node => node.getAnimations().length === 0));
  const reducedSwitch = await introPage.evaluate(() => {
    document.querySelector('[data-activate-client=codex]').click();
    return {active: document.querySelector('.demo-scene').dataset.activeClient,
      animations: [...document.querySelectorAll('[data-client]')].flatMap(node => node.getAnimations()).length};
  });
  assert.deepEqual(reducedSwitch, {active: 'codex', animations: 0}, 'reduced motion switches immediately');
  await introPage.emulateMedia({reducedMotion: 'no-preference'});
  checks.push('resize and live reduced-motion changes settle client motion without stale effects');
  await introPage.clock.install();
  const autoFrame = await (await introPage.locator('#demo-frame').elementHandle()).contentFrame();
  await introContext.close();
  const context = await browser.newContext({viewport: {width: 1440, height: 900}, reducedMotion: 'reduce'});
  const {page, rail} = await openDemo(context);
  assert.equal(await page.locator('[data-scenario]').count(), 0);
  assert.equal(await rail.locator('.desktop-avatar').count(), 2);
  assert.equal(await page.locator('.demo-scene').getAttribute('data-active-client'), 'workbuddy');
  const storyStarted = Date.now();
  const separated = async page => page.locator('[data-client]').evaluateAll(nodes => {
    const [a, b] = nodes.map(node => node.getBoundingClientRect());
    const overlap = Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left)) * Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top));
    return overlap > 0 && overlap / Math.min(a.width * a.height, b.width * b.height) < .45;
  });
  assert.ok(await separated(page), 'desktop windows retain a shallow overlap');
  await page.locator('[data-client=workbuddy][data-story-status=wait]').waitFor();
  assert.ok(Date.now() - storyStarted < 3300, 'WorkBuddy asks within three seconds of the opening');
  await rail.locator('.desktop-avatar[data-session-id^="workbuddy:"][data-status=wait]').waitFor();
  await page.locator('[data-client=codex][data-story-status=wait]').waitFor();
  await rail.locator('.desktop-avatar[data-session-id^="codex:"][data-status=wait]').waitFor();
  await rail.locator('.desktop-automatic-card:visible').filter({has: rail.locator('.desktop-provider[aria-label="Codex"]')}).waitFor();
  await rail.locator('.desktop-avatar[data-session-id^="workbuddy:"]').click();
  assert.equal(await page.locator('.demo-scene').getAttribute('data-active-client'), 'workbuddy');
  assert.ok(await page.locator('[data-client=workbuddy] .story-choice').isVisible());
  await page.screenshot({path: `${OUT}/wide-waiting.png`, fullPage: true});
  // Install the clock only after real first-paint and intro, then advance a long
  // interval: waiting must not be another automatic rotation step.
  await page.clock.install();
  await page.clock.fastForward(60000);
  assert.equal(await page.locator('[data-client=workbuddy]').getAttribute('data-story-status'), 'wait');
  await page.clock.resume();
  assert.equal(await page.locator('[data-client=codex]').getAttribute('data-story-status'), 'wait');
  await rail.locator('.desktop-avatar[data-session-id^="codex:"]').click();
  await page.waitForFunction(() => document.querySelector('.demo-scene').dataset.activeClient === 'codex');
  await page.locator('[data-client=codex] [data-choice="2"]').click();
  assert.equal(await page.locator('[data-client=workbuddy]').getAttribute('data-story-status'), 'wait');
  await page.locator('[data-client=codex][data-story-status=done]').waitFor();
  await rail.locator('.desktop-avatar[data-session-id^="codex:"][data-status=done]').waitFor();
  checks.push('both clients wait for separate answers; Codex succeeds only after its choice');
  for (const choice of [1, 2, 3, 4]) {
    if (choice > 1) {
      await page.locator('[data-action=reset]').click();
      await page.locator('[data-client=workbuddy][data-story-status=wait]').waitFor();
    }
    await page.locator('[data-activate-client=workbuddy]').click();
    const button = page.locator(`[data-client=workbuddy] [data-choice="${choice}"]`);
    if (choice === 2) { await button.focus(); await page.keyboard.press('Enter'); }
    else await button.click();
    assert.equal(await page.locator('[data-client=workbuddy] .story-choice').isVisible(), false);
    assert.equal(await page.locator('[data-client=workbuddy]').getAttribute('data-story-status'), 'running');
    assert.equal(await page.locator('[data-client=workbuddy] .story-selection').textContent(), `✓ ${choice}`);
    await page.locator('[data-client=workbuddy][data-story-status=error]').waitFor();
    await rail.locator('.desktop-avatar[data-session-id^="workbuddy:"][data-status=error]').waitFor();
    assert.ok(await page.locator('[data-client=workbuddy] .story-result').isVisible());
    if (choice === 1) await page.screenshot({path: `${OUT}/wide-error.png`, fullPage: true});
  }
  checks.push('all four vertical choices work by pointer or keyboard and end in a WorkBuddy error');
  await page.locator('[data-action=reset]').click();
  await page.locator('[data-client=workbuddy][data-story-status=wait]').waitFor();
  await page.locator('[data-activate-client=workbuddy]').click();
  await page.locator('[data-client=workbuddy] [data-choice="1"]').click();
  await page.locator('[data-action=reset]').click();
  await page.locator('[data-client=workbuddy][data-story-status=wait]').waitFor();
  assert.equal(await page.locator('[data-client=workbuddy] .story-selection').isVisible(), false);
  await page.waitForTimeout(3200);
  assert.equal(await page.locator('[data-client=workbuddy]').getAttribute('data-story-status'), 'wait');
  checks.push('replay cancels resumed completion timers and clears the choice');
  assert.deepEqual(await rail.evaluate(() => ({local: localStorage.length, session: sessionStorage.length})), {local: 0, session: 0});
  for (const viewport of [{width: 390, height: 844}, {width: 320, height: 740}, {width: 1024, height: 640}]) {
    const responsiveContext = await browser.newContext({viewport, reducedMotion: 'reduce'});
    const responsive = await openDemo(responsiveContext);
    await responsive.page.locator('[data-client=workbuddy][data-story-status=wait]').waitFor();
    assert.ok(await responsive.rail.locator('.desktop-list').evaluate(list => {
      const bounds = list.getBoundingClientRect();
      return [...list.querySelectorAll('.desktop-avatar')].every(avatar => {
        const rect = avatar.getBoundingClientRect();
        return rect.top >= bounds.top && rect.bottom <= bounds.bottom;
      });
    }), 'both avatars fit without scrolling or clipping');
    await responsive.page.locator('[data-activate-client=workbuddy]').click();
    assert.ok(await separated(responsive.page), 'responsive stack exposes most of each window');
    await responsive.page.locator('[data-client=workbuddy] .story-choice').scrollIntoViewIfNeeded();
    const optionBoxes = await responsive.page.locator('[data-client=workbuddy] [data-choice]').evaluateAll(nodes => nodes.map(node => {
      const r = node.getBoundingClientRect();
      const parent = node.closest('.client-conversation').getBoundingClientRect();
      const center = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
      return {x: r.x, y: r.y, bottom: r.bottom, inside: r.top >= parent.top && r.bottom <= parent.bottom, clickable: node.contains(center)};
    }));
    assert.equal(optionBoxes.length, 4);
    optionBoxes.forEach((box, i) => {
      assert.ok(box.inside && box.clickable, 'every option stays visible and unobstructed');
      if (i) { assert.ok(box.y >= optionBoxes[i - 1].bottom); assert.equal(box.x, optionBoxes[0].x); }
    });
    await responsive.page.screenshot({path: `${OUT}/responsive-${viewport.width}.png`, fullPage: true});
    await responsive.page.locator('[data-client=workbuddy] [data-choice="3"]').click();
    assert.equal(await responsive.page.locator('[data-client=workbuddy]').getAttribute('data-story-status'), 'running');
    assert.ok(await responsive.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await responsive.rail.locator('.desktop-avatar[data-session-id^="codex:"]').click();
    await responsive.page.waitForFunction(() => document.querySelector('.demo-scene').dataset.activeClient === 'codex');
    assert.equal(await responsive.page.locator('.demo-scene').getAttribute('data-active-client'), 'codex');
    await responsiveContext.close();
  }
  checks.push('390px, 320px and 1024px preserve real choice clicks and avatar activation');

  for (const client of ['codex', 'workbuddy']) {
    const other = client === 'codex' ? 'workbuddy' : 'codex';
    await page.locator(`[data-activate-client=${other}]`).click();
    await rail.locator(`.desktop-avatar[data-session-id^="${client}:"]`).hover();
    const label = client === 'codex' ? 'Codex' : 'WorkBuddy';
    const preview = rail.locator('.desktop-card:not(.desktop-departing):visible').filter({has: rail.locator(`.desktop-provider[aria-label="${label}"]`)}).locator('.desktop-preview');
    await preview.click();
    await page.waitForFunction(client => document.querySelector('.demo-scene').dataset.activeClient === client, client);
    assert.equal(await page.locator('.demo-scene').getAttribute('data-active-client'), client);
    assert.ok(!(await page.locator('#demo-feedback').textContent()).includes('被拦截'));
  }
  checks.push('both conversation previews activate their fictional client without desktop-only feedback');

  // AC4: nothing the demo did left the page, and nothing failed.
  const forbidden = requests.filter(url => /\/events(?:[?#]|$)/.test(url) || /\/api\//.test(url)
    || /^(?:codex|codeg|workbuddy|workbuddy-ai|codebuddycn?):\/\//.test(url));
  assert.deepEqual(forbidden, [], 'the demo never opens a stream, the local service or an app protocol');
  const external = requests.filter(url => !url.startsWith(`${ORIGIN}${BASE}/`) && !url.startsWith('data:') && !url.startsWith('blob:'));
  assert.deepEqual(external, [], 'the demo only loads its own files');
  assert.deepEqual(failed, []);
  assert.deepEqual(errors, []);
  checks.push('no monitor stream, no local request, no protocol, no external file, no JS error');

  await fs.writeFile(`${OUT}/report.json`, JSON.stringify({passed: true, checks, requests: requests.length, errors, failed}, null, 2));
  console.log(`PASS: web demo entry, scenarios, drag, keyboard and isolation (${checks.length} checks)`);
  await context.close();
} catch (error) {
  await fs.writeFile(`${OUT}/report.json`, JSON.stringify({passed: false, checks, error: String(error), errors, failed}, null, 2));
  throw error;
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}
