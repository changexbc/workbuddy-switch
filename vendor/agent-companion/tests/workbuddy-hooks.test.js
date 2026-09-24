import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Hub } from '../collector/lib/hub.js';
import {
  WorkBuddyPoller,
  mergeWorkBuddyHooks,
  installWorkBuddyHooks,
  HOOK_SCRIPT_NAME,
} from '../collector/lib/workbuddy.js';

test('WorkBuddy hook lifecycle maps ask, tools, notification and stop without file reads', async t => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'workbuddy-hook-'));
  t.after(() => fs.rm(home, { recursive: true, force: true }));
  const hub = new Hub();
  const poller = new WorkBuddyPoller(hub, { home });
  const methods = ['readFile', 'open', 'stat', 'access', 'readdir'];
  const originals = Object.fromEntries(methods.map(k => [k, fs[k]]));
  try {
    for (const k of methods) fs[k] = () => { throw Error(`Unexpected filesystem access: ${k}`); };
    let ts = Date.now();
    const hook = (event, extra = {}) => poller.ingestHook({ session_id: 'x', cwd: '/p', timestamp: ++ts, hook_event_name: event, ...extra });
    hook('UserPromptSubmit', { prompt: 'hello', agent_edition: 'international' });
    assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
    assert.equal(hub.sessions.get('workbuddy:x').title, 'hello');
    assert.equal(hub.sessions.get('workbuddy:x').agentType, 'workbuddy-ai');
    hook('PreToolUse', { tool_name: 'AskUserQuestion', tool_use_id: 'q', tool_input: { questions: [{ question: '选哪个？' }] } });
    assert.equal(hub.sessions.get('workbuddy:x').status, 'wait');
    assert.equal(hub.sessions.get('workbuddy:x').pending[0].text, '选哪个？');
    hook('PostToolUse', { tool_name: 'AskUserQuestion', tool_use_id: 'q' });
    assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
    hook('Notification');
    assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
    assert.equal(hub.sessions.get('workbuddy:x').pending.length, 0);
    hook('PermissionRequest', { tool_name: 'Read', tool_use_id: 'cred', message: 'Allow the model to access sensitive credentials?' });
    assert.equal(hub.sessions.get('workbuddy:x').status, 'wait');
    assert.equal(hub.sessions.get('workbuddy:x').pending[0].text, 'Allow the model to access sensitive credentials?');
    hook('PostToolUse', { tool_name: 'Read', tool_use_id: 'cred' });
    assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
    hook('Stop');
    assert.equal(hub.sessions.get('workbuddy:x').status, 'done');
  } finally {
    for (const k of methods) fs[k] = originals[k];
  }
  const restarted = new WorkBuddyPoller(new Hub(), { home });
  await restarted.poll();
  assert.equal(restarted.hub.sessions.size, 0);
});

test('WorkBuddy hook merge keeps foreign settings, skips PermissionRequest, and yields to native runtime', () => {
  const existing = {
    sandbox: { extraAllowWrite: ['~/tmp'] },
    hooks: {
      PermissionRequest: [{ hooks: [{ type: 'http', url: 'http://example' }] }],
      PreToolUse: [{ matcher: '', hooks: [{ type: 'command', command: 'other-tool' }] }],
    },
  };
  const merged = mergeWorkBuddyHooks(existing, `/usr/bin/python3 /tmp/${HOOK_SCRIPT_NAME}`);
  assert.deepEqual(merged.sandbox, existing.sandbox);
  assert.equal(merged.hooks.PermissionRequest[0].hooks[0].url, 'http://example');
  assert.equal(merged.hooks.PreToolUse[0].hooks[0].command, 'other-tool');
  assert.ok(merged.hooks.PreToolUse.some(group => group.hooks.some(h => h.command.includes(HOOK_SCRIPT_NAME))));
  const native = mergeWorkBuddyHooks({
    hooks: {
      PreToolUse: [{ matcher: '', hooks: [{ type: 'command', command: "agent-studio-runtime-v1 hook --home /tmp --source workbuddy" }] }],
    },
  }, `/usr/bin/python3 /tmp/${HOOK_SCRIPT_NAME}`);
  assert.ok(native.hooks.PreToolUse.some(group => group.hooks.some(h => h.command.includes('--source workbuddy'))));
  assert.equal(native.hooks.PreToolUse.some(group => group.hooks.some(h => h.command.includes(HOOK_SCRIPT_NAME))), false);
});

test('WorkBuddy hook installer writes international and domestic editions, not a binaries-only leftover', async t => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'workbuddy-install-'));
  t.after(() => fs.rm(home, { recursive: true, force: true }));
  assert.equal(await installWorkBuddyHooks(home, { monitorUrl: 'http://127.0.0.1:9' }), null);
  await fs.mkdir(path.join(home, '.workbuddy', 'binaries'), { recursive: true });
  await fs.mkdir(path.join(home, '.workbuddy-ai'));
  let installed = await installWorkBuddyHooks(home, { monitorUrl: 'http://127.0.0.1:9' });
  assert.deepEqual(installed.settingsPaths, [path.join(home, '.workbuddy-ai', 'settings.json')]);
  await fs.writeFile(path.join(home, '.workbuddy', 'settings.json'), '{}\n');
  installed = await installWorkBuddyHooks(home, { monitorUrl: 'http://127.0.0.1:9' });
  assert.deepEqual(installed.settingsPaths, [
    path.join(home, '.workbuddy-ai', 'settings.json'),
    path.join(home, '.workbuddy', 'settings.json'),
  ]);
  for (const settingsPath of installed.settingsPaths) {
    const settings = JSON.parse(await fs.readFile(settingsPath, 'utf8'));
    assert.ok(settings.hooks.UserPromptSubmit[0].hooks[0].command.includes(HOOK_SCRIPT_NAME));
    assert.ok(settings.hooks.PermissionRequest[0].hooks[0].command.includes(HOOK_SCRIPT_NAME));
    assert.equal(settings.hooks.PermissionRequest[0].hooks[0].type, 'command');
  }
  await fs.access(path.join(home, '.workbuddy-ai', 'hooks', HOOK_SCRIPT_NAME));
  await fs.access(path.join(home, '.workbuddy', 'hooks', HOOK_SCRIPT_NAME));
});

test('WorkBuddy host exit aborts the unfinished round and recovers on the next hook', async () => {
  const hub = new Hub();
  let mode = 'alive';
  const presence = { noteHook() { mode = 'alive'; }, async observe() { return mode; } };
  const poller = new WorkBuddyPoller(hub, { hostPresence: presence });
  let ts = Date.now();
  const hook = (event, extra = {}) => poller.ingestHook({ session_id: 'x', cwd: '/p', timestamp: ++ts, hook_event_name: event, ...extra });
  hook('UserPromptSubmit', { prompt: 'build' });
  await poller.poll();
  assert.equal(hub.sources.workbuddy.state, 'ok');
  hook('Stop');
  assert.equal(hub.sessions.get('workbuddy:x').status, 'done');
  hook('UserPromptSubmit', { prompt: 'more' });
  assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
  mode = 'gone';
  await poller.poll();
  assert.equal(hub.sources.workbuddy.state, 'exited');
  assert.equal(hub.sources.workbuddy.detail, 'WorkBuddy 已退出，未完成的任务已标记中止');
  assert.equal(hub.sessions.get('workbuddy:x').status, 'aborted');
  assert.equal(hub.sessions.get('workbuddy:x').endedBy, 'host');
  hook('UserPromptSubmit', { prompt: 'again' });
  await poller.poll();
  assert.equal(hub.sources.workbuddy.state, 'ok');
  assert.equal(hub.sessions.get('workbuddy:x').status, 'running');
  assert.equal(hub.sessions.get('workbuddy:x').endedBy, undefined);
});
