// End-to-end drill for docs/custom-agent-hooks.md, run against a real packaged
// runtime and a temporary home. Nothing here touches the user's own agent
// configuration: every hook file, the settings and the store live under the
// temporary home, and the third-party tools are simulated by piping payloads
// into the documented command.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { defaultSettings } from '../src/settings-config.js';

const binary = path.resolve('target/release/agent-studio-runtime');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const examples = path.resolve('docs/examples/custom-agent-hooks');
const payloadOf = name => fs.readFile(path.join(examples, 'scenarios', name), 'utf8');

assert(await fs.stat(binary).then(() => true, () => false), `missing ${binary}; run npm run desktop:prepare first`);
const home = await fs.mkdtemp(path.join(os.tmpdir(), 'custom-hooks-e2e-'));
await fs.mkdir(path.join(home, '.agent-studio'), {recursive: true});
await fs.mkdir(path.join(home, '.codex'), {recursive: true});
const settings = defaultSettings();
for (const [id, config] of Object.entries(settings.sources)) config.enabled = id === 'codex';
await fs.writeFile(path.join(home, '.agent-studio/settings.json'), JSON.stringify(settings));

let child;
async function startRuntime() {
  child = spawn(binary, ['serve'], {env: {...process.env, AGENT_STUDIO_HOME: home}, stdio: 'ignore'});
  for (let index = 0; index < 200; index++) {
    try { return JSON.parse(await fs.readFile(path.join(home, '.agent-studio/runtime-v1.json'), 'utf8')); } catch { await delay(50); }
  }
  throw Error('runtime endpoint was not created');
}
async function stopRuntime() {
  if (!child) return;
  const exited = new Promise(resolve => child.once('exit', resolve));
  child.kill();
  await exited;
  await fs.rm(path.join(home, '.agent-studio/runtime-v1.json'), {force: true});
}
let endpoint = await startRuntime();
const rpc = async (command, payload = {}) => {
  const response = await fetch(`http://127.0.0.1:${endpoint.port}/rpc`, {
    method: 'POST', headers: {Authorization: `Bearer ${endpoint.token}`}, body: JSON.stringify({command, payload}),
  });
  const body = await response.json();
  assert(!body.error, `${command}: ${body.error}`);
  return body.value;
};
const poll = () => rpc('poll', {client: 'e2e'});
const status = () => rpc('custom_integrations_get');
const hookBinary = path.join(home, '.agent-studio/bin/agent-studio-runtime-v1');
const run = (integration, input) => spawnSync(hookBinary, ['custom-hook', '--home', home, '--integration', integration], {input, encoding: 'utf8'});
const send = async (integration, name) => {
  const result = run(integration, await payloadOf(name));
  assert.equal(result.status, 0, `${name} failed: ${result.stderr}`);
  assert.equal(result.stdout, '', `${name} must leave stdout empty`);
  return result;
};
const diagnostics = async () => (await status()).diagnostics.map(item => `${item.outcome}:${item.reason}`);
const sessions = async () => (await poll()).snapshot.sessions.filter(session => session.source.startsWith('custom:'));
const sessionById = async id => (await sessions()).find(session => session.id === id);

try {
  await rpc('hello', {client: 'e2e'});

  // The delivered examples are importable as-is.
  for (const name of ['generic-agent.json', 'minimal-agent.json']) {
    const template = JSON.parse(await fs.readFile(path.join(examples, name), 'utf8'));
    await rpc('custom_integrations_set', {action: 'import', template});
  }
  let state = await status();
  assert.deepEqual(state.templates.map(item => item.id), ['example-agent', 'minimal-agent']);
  assert.equal(state.storage.ok, true);
  assert.equal(state.binaryInstalled, true, 'the runtime publishes its own hook binary');
  assert.deepEqual(state.templates[0].capabilities, ['close', 'finish:done', 'finish:error', 'resume', 'start', 'wait:input', 'wait:permission']);
  assert.equal(state.templates[0].lastReceivedAt, null, 'an imported template proves no event arrived');
  assert.equal(state.templates[0].command, `'${hookBinary}' custom-hook --home '${home}' --integration example-agent`);

  // basic-lifecycle, in the order the document lists.
  await send('example-agent', 'basic-lifecycle/01-start.json');
  let session = await sessionById('custom:example-agent:sim-basic');
  assert.equal(session.status, 'running');
  assert.equal(session.title, '把登录页按钮对齐');
  assert.equal(session.project, 'demo');
  assert.equal(session.roundId, 'turn-1');
  await send('example-agent', 'basic-lifecycle/02-wait-permission.json');
  session = await sessionById('custom:example-agent:sim-basic');
  assert.equal(session.status, 'wait');
  assert.equal(session.pending.length, 1);
  await send('example-agent', 'basic-lifecycle/03-resume.json');
  assert.equal((await sessionById('custom:example-agent:sim-basic')).status, 'running');
  await send('example-agent', 'basic-lifecycle/04-finish-done.json');
  session = await sessionById('custom:example-agent:sim-basic');
  assert.equal(session.status, 'done');
  await send('example-agent', 'basic-lifecycle/05-finish-again.json');
  assert.equal((await sessionById('custom:example-agent:sim-basic')).status, 'done', 'a finished round is not rewritten');
  assert.deepEqual((await diagnostics()).slice(-5), ['accepted:start', 'accepted:wait', 'accepted:resume', 'accepted:finish', 'ignored:round_ended']);
  // Replaying the start is a duplicate, not a new round.
  await send('example-agent', 'basic-lifecycle/01-start.json');
  assert.deepEqual((await diagnostics()).slice(-1), ['ignored:duplicate_event']);
  assert.equal((await sessionById('custom:example-agent:sim-basic')).status, 'done');

  // aborted-and-late: close aborts the running round, a late old round is dropped.
  await send('example-agent', 'aborted-and-late/01-start-round-1.json');
  await send('example-agent', 'aborted-and-late/02-close-round-1.json');
  session = await sessionById('custom:example-agent:sim-close');
  assert.equal(session.status, 'aborted');
  assert.equal(session.endedBy, 'session_closed');
  await send('example-agent', 'aborted-and-late/03-start-round-2.json');
  assert.equal((await sessionById('custom:example-agent:sim-close')).roundId, 'turn-2');
  await send('example-agent', 'aborted-and-late/04-late-finish-round-1.json');
  assert.deepEqual((await diagnostics()).slice(-1), ['ignored:late_round']);
  assert.equal((await sessionById('custom:example-agent:sim-close')).status, 'running');

  // ignored and rejected payloads never create a session.
  const before = (await sessions()).length;
  await send('example-agent', 'ignored-and-rejected/01-start-parent.json');
  await send('example-agent', 'ignored-and-rejected/02-subtask-start.json');
  assert.ok(!(await sessionById('custom:example-agent:sim-child')), 'subtasks are filtered before a session exists');
  await send('example-agent', 'ignored-and-rejected/03-unknown-event.json');
  const missing = run('example-agent', await payloadOf('ignored-and-rejected/04-missing-session.json'));
  assert.notEqual(missing.status, 0, 'a rejected payload reports failure');
  assert.equal(missing.stdout, '');
  assert.match(missing.stderr, /missing_session_id/);
  assert.deepEqual((await diagnostics()).slice(-3), ['ignored:ignored_field_present', 'ignored:unknown_event', 'rejected:missing_session_id']);
  assert.equal((await sessions()).length, before + 1, 'only the parent session was created');
  const unknown = run('missing-agent', '{}');
  assert.notEqual(unknown.status, 0);
  assert.match(unknown.stderr, /未注册|unknown_integration/);
  const broken = run('example-agent', 'not json');
  assert.notEqual(broken.status, 0);
  assert.match(broken.stderr, /JSON/);
  const oversized = run('example-agent', JSON.stringify({event_name: 'prompt_submitted', session_id: 'big', prompt: 'x'.repeat(1024 * 1024)}));
  assert.notEqual(oversized.status, 0);
  assert.match(oversized.stderr, /1 MiB/);

  // Minimal template with an assigned round, exactly as documented.
  await send('minimal-agent', 'minimal-agent/01-begin.json');
  await send('minimal-agent', 'minimal-agent/02-end.json');
  session = await sessionById('custom:minimal-agent:sim-minimal');
  assert.equal(session.status, 'done');
  assert.match(session.roundId, /^custom:\d+$/);

  // Restart recovery: the templates survive, the in-memory round does not.
  await stopRuntime();
  endpoint = await startRuntime();
  await rpc('hello', {client: 'e2e'});
  state = await status();
  assert.deepEqual(state.templates.map(item => item.id), ['example-agent', 'minimal-agent']);
  assert.equal((await sessions()).length, 0, 'unfinished sessions are not restored after a restart');
  await send('example-agent', 'basic-lifecycle/01-start.json');
  session = await sessionById('custom:example-agent:sim-basic');
  assert.equal(session.status, 'running', 'a fresh event resumes working after a restart');
  assert.equal(session.roundId, 'turn-1');
  await send('minimal-agent', 'minimal-agent/01-begin.json');
  await send('minimal-agent', 'minimal-agent/02-end.json');
  assert.equal((await sessionById('custom:minimal-agent:sim-minimal')).status, 'done');

  // Disable and remove only affect that source, and never touch the tool config.
  const codexHooks = path.join(home, '.codex/hooks.json');
  const codexBefore = await fs.readFile(codexHooks, 'utf8');
  await rpc('custom_integrations_set', {action: 'disable', id: 'example-agent'});
  assert.equal((await status()).templates.find(item => item.id === 'example-agent').enabled, false);
  assert.ok(!(await sessionById('custom:example-agent:sim-basic')), 'disabling clears the running state of that source');
  const disabled = run('example-agent', await payloadOf('basic-lifecycle/01-start.json'));
  assert.notEqual(disabled.status, 0);
  assert.match(disabled.stderr, /integration_disabled/);
  assert.equal((await status()).diagnostics.at(-1).reason, 'integration_disabled');
  assert.ok(await sessionById('custom:minimal-agent:sim-minimal'), 'the other source keeps its session');
  await rpc('custom_integrations_set', {action: 'remove', id: 'example-agent'});
  assert.deepEqual((await status()).templates.map(item => item.id), ['minimal-agent']);
  const stored = JSON.parse(await fs.readFile(path.join(home, '.agent-studio/custom-integrations.json'), 'utf8'));
  assert.deepEqual(Object.keys(stored.templates), ['minimal-agent']);
  assert.equal(await fs.readFile(codexHooks, 'utf8'), codexBefore, 'custom management never edits a third-party hook file');

  console.log('PASS: documented import, command, lifecycle, diagnostics, restart, disable and remove on a temporary home');
} finally {
  await stopRuntime();
  await fs.rm(home, {recursive: true, force: true});
}
