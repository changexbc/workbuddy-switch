// Interface-level flow for imported custom agent hooks, run against a real Node
// collector and a real store in a temporary home. Also asserts the settings page
// keeps the manual import entry hidden until the built-in adapter release.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { preview } from 'vite';
import { chromium } from 'playwright';
import { createCollector } from '../collector/lib/collector.js';
import { startServer } from '../collector/server.js';
import { defaultSettings } from '../src/settings-config.js';

const home = await fs.mkdtemp(path.join(os.tmpdir(), 'qa-custom-hooks-'));
await fs.mkdir(path.join(home, '.agent-studio'), {recursive: true});
const settings = defaultSettings();
for (const source of Object.values(settings.sources)) source.enabled = false;
await fs.writeFile(path.join(home, '.agent-studio/settings.json'), JSON.stringify(settings));
const template = {
  schemaVersion: 1, id: 'custom-test-agent', name: 'Custom Test Agent', transport: 'hook',
  mapping: {event: '/event_name', sessionId: '/session_id', roundId: '/round_id', timestamp: '/ts'},
  events: {prompt_submitted: {action: 'start'}, response_finished: {action: 'finish', status: 'done'}, response_failed: {action: 'finish', status: 'error'}},
};
const storeFile = path.join(home, '.agent-studio/custom-integrations.json');

const collector = createCollector({home});
const runtime = await startServer({port: 0, collector, staticRoot: path.join(home, 'no-static-files')});
const api = `http://127.0.0.1:${runtime.server.address().port}`;
const post = (route, body) => fetch(`${api}${route}`, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)});
const get = async route => { const response = await fetch(`${api}${route}`); assert.equal(response.status, 200); return response.json(); };

try {
  // The settings page must not expose the manual import entry in this release.
  const server = await preview({preview: {host: '127.0.0.1', port: 0, strictPort: true}});
  const browser = await chromium.launch({headless: true});
  const errors = [];
  try {
    const page = await browser.newPage({reducedMotion: 'reduce', viewport: {width: 480, height: 900}});
    page.on('pageerror', error => errors.push(error.message));
    // The page runs under Vite, but every API call goes to the real collector.
    await page.route('**/api/**', async route => {
      const request = route.request();
      const target = new URL(request.url());
      const response = await fetch(`${api}${target.pathname}${target.search}`, {
        method: request.method(),
        headers: request.headers()['content-type'] ? {'Content-Type': request.headers()['content-type']} : {},
        body: request.postData() ?? undefined,
      });
      await route.fulfill({status: response.status, contentType: 'application/json', body: await response.text()});
    });
    await page.goto(`${server.resolvedUrls.local[0]}desktop-settings.html`);
    await page.waitForFunction(() => document.querySelector('fieldset')?.disabled === false, null, {timeout: 15000});
    assert.equal(await page.locator('.custom-integration-manager').count(), 0, 'the manual custom entry stays hidden');
    assert.doesNotMatch(await page.locator('body').innerText(), /自定义 Agent/);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    server.httpServer.close();
  }

  // An invalid template reports the offending field instead of importing.
  let response = await post('/api/custom-integrations', {action: 'import', template: {schemaVersion: 2}});
  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /\/schemaVersion/);
  await assert.rejects(fs.readFile(storeFile, 'utf8'), /ENOENT/, 'a failed import must not leave a half-written store');

  // The preview maps a sample payload without creating a session.
  response = await post('/api/custom-integrations/preview', {template, payload: {event_name: 'prompt_submitted', session_id: 'preview', round_id: 'r1', ts: 1700000000000}});
  assert.equal(response.status, 200);
  const previewed = await response.json();
  assert.equal(previewed.ok, true);
  assert.equal(previewed.action, 'start');
  assert.equal((await get('/api/state')).sessions.length, 0, 'preview never creates a session');

  // Import persists; the status reports the command and no received events yet.
  response = await post('/api/custom-integrations', {action: 'import', template});
  assert.equal(response.status, 200);
  let state = await response.json();
  assert.equal(state.templates.length, 1);
  assert.equal(state.templates[0].id, 'custom-test-agent');
  assert.match(state.templates[0].command, /custom-hook --home .* --integration custom-test-agent/);
  assert.equal(state.templates[0].lastReceivedAt, null, 'an import alone proves no event arrived');
  assert.equal(state.templates[0].lastMappedAt, null);
  assert.equal((await get('/api/custom-integrations')).templates.length, 1);
  const fresh = createCollector({home});
  assert.equal((await fresh.customIntegrationsGet()).templates.length, 1, 'a fresh collector re-reads the persisted store');

  // A duplicate id is refused and the first template stays.
  response = await post('/api/custom-integrations', {action: 'import', template});
  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /请先删除/);

  // Real events through the development receive route.
  const send = payload => post('/api/custom-hook', {integration: 'custom-test-agent', payload}).then(r => r.json());
  assert.equal((await send({event_name: 'prompt_submitted', session_id: 's1', round_id: 'r1', ts: 1700000000000})).outcome, 'accepted');
  state = await get('/api/custom-integrations');
  assert.ok(state.templates[0].lastReceivedAt, 'the status records the received event');
  assert.ok(state.templates[0].lastMappedAt);
  assert.equal((await send({event_name: 'unmapped_event', session_id: 's1', ts: 1700000001000})).reason, 'unknown_event');
  state = await get('/api/custom-integrations');
  assert.ok(state.diagnostics.some(item => item.event === 'unmapped_event' && item.outcome === 'ignored' && item.reason === 'unknown_event'), 'diagnostics record the ignored event');

  // Disable rejects new events and clears the running state.
  response = await post('/api/custom-integrations', {action: 'disable', id: 'custom-test-agent'});
  assert.equal(response.status, 200);
  assert.equal((await send({event_name: 'prompt_submitted', session_id: 's2', round_id: 'r2', ts: 1700000002000})).reason, 'integration_disabled');
  assert.equal((await get('/api/state')).sessions.length, 0, 'disabling clears the running state of that source');

  // Remove deletes the template from the store on disk.
  response = await post('/api/custom-integrations', {action: 'remove', id: 'custom-test-agent'});
  assert.equal(response.status, 200);
  const stored = JSON.parse(await fs.readFile(storeFile, 'utf8'));
  assert.deepEqual(Object.keys(stored.templates), []);
  console.log('PASS: entry hidden, import failure, preview without a session, persisted reload, duplicate refusal, live events, diagnostics, disable and remove');
} finally {
  await runtime.close();
  await fs.rm(home, {recursive: true, force: true});
}
