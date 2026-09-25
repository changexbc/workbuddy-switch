import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { buildStorySnapshot, buildScenarioSteps, DEMO_CAST, DEMO_SCENARIO_NAMES, DEMO_STEP_GAP_MS, DEMO_STORY_ID, demoClientForSession } from '../src/demo/scenarios.ts';
import { DEMO_CHANNEL, isDemoFrameMessage, isDemoPageMessage } from '../src/demo/protocol.ts';
import { createRailModel } from '../src/desktop/rail-model.js';
import { automaticReminderItems, questionKey } from '../src/monitor/reminders.js';

const NOW = 1_700_000_000_000;
const EVENT_KINDS = ['wait', 'done', 'error'];

test('every demo scenario step is a complete, fictional snapshot', () => {
  for (const name of DEMO_SCENARIO_NAMES) {
    const steps = buildScenarioSteps(name, NOW);
    assert.ok(steps.length >= 1, `${name} plays at least one step`);
    assert.equal(steps[0].delayMs, 0, `${name} shows the running frame first`);
    for (const {snapshot} of steps) {
      assert.equal(snapshot.version, 1);
      assert.equal(snapshot.ready, true);
      assert.ok(Number.isFinite(snapshot.ts));
      assert.equal(snapshot.sessions.length, name === 'idle' ? 0 : DEMO_CAST.length);
      assert.equal(new Set(snapshot.sessions.map(s => s.id)).size, snapshot.sessions.length);
      for (const session of snapshot.sessions) {
        assert.ok(session.id.startsWith(`${session.source}:`), session.id);
        assert.equal(snapshot.sources[session.source]?.state, 'ok', `${session.source} must look healthy`);
        assert.equal(session.stale, false, session.id);
        assert.equal(session.progress, null, session.id);
        assert.ok(Array.isArray(session.pending) && Array.isArray(session.steps), session.id);
        // A viewed round suppresses its own reminder, so the demo must never set it.
        assert.notEqual(session.viewedRoundId, session.roundId, session.id);
        if (session.status === 'wait') assert.ok(session.pending.length > 0, `${session.id} asks something`);
        if (session.status === 'done') assert.ok(Number.isFinite(session.endedAt), session.id);
        if (session.status === 'running') assert.equal(session.pending.length, 0, session.id);
      }
      for (const event of snapshot.events) {
        const [sessionId, roundId, kind, key] = JSON.parse(event.id);
        assert.equal(event.sessionId, sessionId);
        assert.equal(event.roundId, roundId);
        assert.equal(event.kind, kind);
        assert.ok(EVENT_KINDS.includes(kind), event.id);
        assert.equal(event.historical, false, event.id);
        assert.notEqual(key, undefined, event.id);
      }
    }
  }
});

test('the demo cast covers multiple sources and every key status', () => {
  const sources = new Set(), statuses = new Set();
  for (const name of DEMO_SCENARIO_NAMES) {
    for (const {snapshot} of buildScenarioSteps(name, NOW)) {
      for (const session of snapshot.sessions) { sources.add(session.source); statuses.add(session.status); }
    }
  }
  assert.deepEqual([...statuses].sort(), ['done', 'error', 'running', 'wait']);
  assert.deepEqual([...sources].sort(), ['codex', 'workbuddy']);
  assert.equal(DEMO_CAST.length, 2);
  assert.equal(DEMO_CAST.filter(member => member.source === 'codex').length, 1);
  assert.equal(DEMO_CAST.filter(member => member.source === 'workbuddy').length, 1);
  assert.equal(new Set(DEMO_CAST.map(member => member.id)).size, DEMO_CAST.length);
});

test('the rail model turns the scenario timeline into avatars and reminders', () => {
  const model = createRailModel({storage: null, now: () => NOW});
  model.connect('connected');

  for (const step of buildScenarioSteps('working', NOW)) model.accept(step.snapshot);
  assert.equal(model.items.length, DEMO_CAST.length);
  assert.deepEqual(model.items.map(item => item.id), DEMO_CAST.map(member => member.id), 'rail order follows the cast');
  assert.deepEqual(automaticReminderItems(model.items, model.connection, new Map()), [], 'working sessions remind nobody');

  for (const step of buildScenarioSteps('waiting', NOW + DEMO_STEP_GAP_MS)) model.accept(step.snapshot);
  let reminders = automaticReminderItems(model.items, model.connection, new Map());
  assert.deepEqual(reminders.map(item => item.id), [DEMO_STORY_ID], 'waiting asks for confirmation');
  assert.equal(reminders[0].session.status, 'wait');
  assert.ok(reminders[0].session.pending[0].questions[0].text.length > 0);

  for (const step of buildScenarioSteps('done', NOW + 2 * DEMO_STEP_GAP_MS)) model.accept(step.snapshot);
  assert.equal(model.items.length, DEMO_CAST.length, 'a completion is retained, not dropped');
  reminders = automaticReminderItems(model.items, model.connection, new Map());
  assert.deepEqual(reminders.map(item => item.id), [DEMO_STORY_ID], 'the completion reminds');
  assert.equal(reminders[0].session.status, 'done');
});

test('the failed scenario raises a reminder, and closing it ends that round', () => {
  const model = createRailModel({storage: null, now: () => NOW});
  model.connect('connected');
  for (const step of buildScenarioSteps('working', NOW)) model.accept(step.snapshot);
  const steps = buildScenarioSteps('error', NOW + DEMO_STEP_GAP_MS);
  for (const step of steps) model.accept(step.snapshot);

  assert.equal(model.items.length, DEMO_CAST.length, 'a failure is retained, not dropped');
  const reminders = automaticReminderItems(model.items, model.connection, new Map());
  assert.deepEqual(reminders.map(item => item.id), [DEMO_STORY_ID], 'the failure asks for attention by itself');
  assert.equal(reminders[0].session.status, 'error');

  const roundId = reminders[0].session.roundId;
  model.dismiss(DEMO_STORY_ID, roundId);
  model.accept(steps.at(-1).snapshot);
  assert.ok(!model.items.some(item => item.id === DEMO_STORY_ID), 'the closed failure does not return for the same round');
  assert.deepEqual(automaticReminderItems(model.items, model.connection, new Map()), [], 'and it raises no reminder again');
});

test('replaying a scenario is a new round, so a dismissed reminder returns', () => {
  const first = buildScenarioSteps('waiting', NOW);
  const second = buildScenarioSteps('waiting', NOW + 1000);
  const roundOf = steps => steps.at(-1).snapshot.sessions.find(session => session.id === DEMO_STORY_ID).roundId;
  assert.notEqual(roundOf(first), roundOf(second), 'each play is its own round');

  const model = createRailModel({storage: null, now: () => NOW + 1000});
  model.connect('connected');
  for (const step of first) model.accept(step.snapshot);
  const muted = new Map([[DEMO_STORY_ID, questionKey(model.items.find(item => item.id === DEMO_STORY_ID))]]);
  assert.deepEqual(automaticReminderItems(model.items, 'connected', muted), [], 'the dismissed question stays closed for its round');

  for (const step of second) model.accept(step.snapshot);
  assert.deepEqual(automaticReminderItems(model.items, 'connected', muted).map(item => item.id), [DEMO_STORY_ID],
    'the replayed round is a new question, so the reminder is back');
});

test('the demo protocol only accepts its own channel and shapes', () => {
  const snapshot = buildScenarioSteps('working', NOW)[0].snapshot;
  assert.ok(isDemoPageMessage({channel: DEMO_CHANNEL, type: 'snapshot', snapshot}));
  assert.ok(!isDemoPageMessage({channel: 'agent-companion', type: 'snapshot', snapshot}));
  assert.ok(!isDemoPageMessage({channel: DEMO_CHANNEL, type: 'snapshot', snapshot: {version: 2, sessions: [], events: []}}));
  assert.ok(!isDemoPageMessage({channel: DEMO_CHANNEL, type: 'snapshot'}));

  for (const client of ['codex', 'workbuddy']) {
    assert.ok(isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'activate-client', client}));
  }
  for (const client of [undefined, null, '', 'terminal', 'codeg', {}, 1]) {
    assert.equal(isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'activate-client', client}), false);
  }
  assert.equal(isDemoFrameMessage({channel: 'other', type: 'activate-client', client: 'codex'}), false);
  assert.ok(isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'ready'}));
  assert.ok(isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'drag', phase: 'move', dx: 4, dy: -2}));
  assert.ok(!isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'drag', phase: 'move', dx: Number.NaN, dy: 0}));
  assert.ok(isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'blocked', action: 'open-session', text: '桌面版可打开原会话'}));
  assert.ok(!isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'blocked', action: 'delete-everything', text: 'x'}));
  assert.ok(!isDemoFrameMessage({channel: DEMO_CHANNEL, type: 'shutdown'}));
});

test('only known fictional session identities map to a demo client', () => {
  for (const member of DEMO_CAST) assert.equal(demoClientForSession(member.id), member.source);
  for (const id of [undefined, '', 'codex:unknown', 'workbuddy:unknown', 'codeg:demo-docs']) {
    assert.equal(demoClientForSession(id), undefined);
  }
});

test('the demo sources never reach a host, a protocol or the visitor storage', async () => {
  const directory = path.join(import.meta.dirname, '..', 'src', 'demo');
  const files = (await fs.readdir(directory)).filter(name => name.endsWith('.ts')).map(name => path.join(directory, name));
  files.push(path.join(import.meta.dirname, '..', 'demo.html'));
  const forbidden = [
    {pattern: /new\s+EventSource|createSSETransport/, why: 'the demo must not open a monitor stream'},
    {pattern: /desktopCommand|@tauri-apps|__TAURI_INTERNALS__/, why: 'the demo must not call the desktop host'},
    {pattern: /['"`]\/api\//, why: 'the demo must not request the local service'},
    {pattern: /(?:codex|codeg|workbuddy|codebuddycn?|workbuddy-ai):\/\//, why: 'the demo must not open app protocols'},
    {pattern: /\bfetch\s*\(|XMLHttpRequest|sendBeacon/, why: 'the demo must not send any request'},
    {pattern: /from\s+['"][^'"]*session-link/, why: 'the demo must not import the session-link opener'},
    {pattern: /from\s+['"][^'"]*desktop\/host/, why: 'the demo must not import the native host bridge'},
    {pattern: /localStorage|sessionStorage|indexedDB/, why: 'the demo must not touch the visitor storage'},
    {pattern: /window\.open\s*\(/, why: 'the demo must not open another page'},
  ];
  for (const file of files) {
    const source = await fs.readFile(file, 'utf8');
    for (const {pattern, why} of forbidden) {
      assert.ok(!pattern.test(source), `${path.relative(process.cwd(), file)}: ${why}`);
    }
  }
});


test('both story questions remain independent until each client is answered', () => {
  const state = {roundId: 'two-questions', codex: 'wait', workbuddy: 'wait', codexStep: 2, workbuddyStep: 1, choice: null, codexChoice: null};
  const waiting = buildStorySnapshot(state, NOW);
  assert.deepEqual(waiting.sessions.map(session => session.status), ['wait', 'wait']);
  assert.equal(new Set(waiting.sessions.map(session => session.pending[0].id)).size, 2);
  for (const session of waiting.sessions) assert.equal(session.pending[0].questions[0].options.length, 4);
  const answered = buildStorySnapshot({...state, codex: 'done', codexChoice: 2, codexEndedAt: NOW}, NOW);
  assert.equal(answered.sessions[0].pending.length, 0);
  assert.equal(answered.sessions[1].status, 'wait');
  assert.deepEqual(answered.sessions[1].pending, waiting.sessions[1].pending);
});
