import test from 'node:test';
import assert from 'node:assert/strict';
import { isDesktop, onDesktopPointer, onDesktopWindowActive, subscribeDesktop } from '../src/desktop/host.ts';

// The bridge has an embedding path for exactly this: a host window that provides
// `__AGENT_STUDIO_EMBED_HOST__` makes `isDesktop()` true and routes every command
// and event through the host instead of Tauri. A fake host is therefore a mock of
// the real boundary, not a mock of our own code.
function install({holdListen = false, holdInitial = false} = {}) {
  const listeners = new Map(), calls = [], pending = [];
  let initial = null;
  const host = {
    desktop: true,
    listen: (event, handler) => {
      const register = () => {
        const list = listeners.get(event) ?? [];
        list.push(handler);
        listeners.set(event, list);
        // Tauri's unlisten is not instantaneous, and neither is this. A
        // synchronous deregistration would hide the whole reason the bridge
        // keeps a `disposed` flag: events can still arrive in that window.
        return async () => {
          await new Promise(resolve => setTimeout(resolve, 0));
          listeners.set(event, (listeners.get(event) ?? []).filter(entry => entry !== handler));
        };
      };
      if (holdListen) return new Promise(resolve => pending.push(() => resolve(register())));
      return Promise.resolve(register());
    },
    invoke: (command, args) => {
      calls.push({command, args});
      if (command.endsWith('monitor_state')) {
        if (holdInitial) return new Promise(resolve => pending.push(() => resolve(initial)));
        return Promise.resolve(initial ?? {snapshot: null, connected: true});
      }
      return Promise.resolve(null);
    },
    enableNotifications: async () => 'granted',
  };
  const bridge = {
    host,
    calls,
    count: () => [...listeners.values()].reduce((total, list) => total + list.length, 0),
    emit: (event, payload) => { for (const handler of listeners.get(event) ?? []) handler({payload}); },
    releasePending: () => { for (const resolve of pending.splice(0)) resolve(); },
    setInitial: value => { initial = value; },
  };
  globalThis.window = {parent: {__AGENT_STUDIO_EMBED_HOST__: host}};
  return bridge;
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
const restore = () => { delete globalThis.window; };

test('the rail is not a desktop client without an embedding host', t => {
  t.after(restore);
  delete globalThis.window;
  assert.equal(isDesktop(), false);
});

test('a disposed subscription releases every listener and ignores later events', async t => {
  t.after(restore);
  const bridge = install();
  const snapshots = [], states = [];
  const release = subscribeDesktop(snapshot => snapshots.push(snapshot), state => states.push(state));
  await tick();
  assert.equal(states[0], 'connecting', 'the rail announces connecting before it can know anything');
  assert.equal(bridge.count(), 2, 'one listener per channel');
  assert(bridge.calls.some(call => call.command.endsWith('monitor_state')), 'and it asks for the current state instead of waiting for one');
  bridge.emit('monitor-state', {version: 1, ts: 10, ready: true, sessions: [], events: []});
  assert.equal(snapshots.length, 1);

  release();
  const statesBefore = states.length;
  // The listeners are still registered for a tick — that window is exactly what
  // the disposal guard exists for, and asserting only after the tick would make
  // the guard look redundant.
  bridge.emit('monitor-state', {version: 1, ts: 11, ready: true, sessions: [], events: []});
  bridge.emit('monitor-connection', 'offline');
  assert.equal(snapshots.length, 1, 'a disposed subscription ignores events that arrive before its listeners are gone');
  assert.equal(states.length, statesBefore, 'and later connection changes');
  await tick();
  assert.equal(bridge.count(), 0, 'every listener is released');
});

test('a listen that resolves after dispose is released instead of leaking', async t => {
  t.after(restore);
  const bridge = install({holdListen: true});
  const release = subscribeDesktop(() => {}, () => {});
  await tick();
  assert.equal(bridge.count(), 0, 'nothing is registered while the listen is still in flight');
  release();
  bridge.releasePending();
  await tick();
  await tick();
  assert.equal(bridge.count(), 0, 'the late registration is released immediately');
});

test('an initial response older than a live event cannot overwrite it', async t => {
  t.after(restore);
  const bridge = install({holdInitial: true});
  const sessions = [];
  const release = subscribeDesktop(snapshot => sessions.push(snapshot), () => {});
  await tick();
  bridge.emit('monitor-state', {version: 1, ts: 50, ready: true, sessions: [{id: 'a'}], events: []});
  bridge.setInitial({snapshot: {version: 1, ts: 10, ready: true, sessions: [{id: 'b'}], events: []}, connected: true});
  bridge.releasePending();
  await tick();
  assert.equal(sessions.length, 1, 'the stale initial snapshot is dropped');
  assert.equal(sessions[0].sessions[0].id, 'a');
  release();
});

test('pointer and window-active listeners release, and reject malformed payloads', async t => {
  t.after(restore);
  const bridge = install();
  const points = [], actives = [];
  const releasePointer = onDesktopPointer(point => points.push(point));
  const releaseActive = onDesktopWindowActive(active => actives.push(active));
  await tick();
  assert.equal(bridge.count(), 2);

  bridge.emit('agent-studio-pointer', {x: 12, y: 34});
  bridge.emit('agent-studio-pointer', null);
  assert.deepEqual(points, [{x: 12, y: 34}, null]);

  bridge.emit('agent-studio-window-active', {active: true});
  bridge.emit('agent-studio-window-active', {active: 'yes'});
  bridge.emit('agent-studio-window-active', null);
  assert.deepEqual(actives, [true], 'only a real boolean is forwarded');

  releasePointer();
  releaseActive();
  await tick();
  assert.equal(bridge.count(), 0);
  bridge.emit('agent-studio-pointer', {x: 1, y: 1});
  assert.deepEqual(points, [{x: 12, y: 34}, null], 'released listeners stay silent');
});
