import test from 'node:test';
import assert from 'node:assert/strict';
import { automaticReminderItems, questionKey } from '../src/monitor/reminders.js';
import { permissionReminderKey } from '../src/monitor/permission-check.js';
import { sessionPresentation } from '../src/monitor/presentation.js';

// Only the fields the rule reads; a full RailItem would obscure what matters.
const item = over => ({
  id: 'codex:1',
  offline: false,
  identity: {id: 'x', name: 'x', avatar: null, slot: 0, number: 1},
  session: {id: 'codex:1', source: 'codex', sessionId: '1', status: 'wait', title: '任务', roundId: 'r1', updatedAt: 1, steps: [], pending: [{id: 'q1', text: '甲'}]},
  ...over,
});

test('waiting and completed sessions on a connected, healthy source raise a card', () => {
  const waiting = item();
  assert.deepEqual(automaticReminderItems([waiting], 'connected', new Map()).map(row => row.id), ['codex:1']);
  const completed = item({session: {...waiting.session, status: 'done', pending: []}});
  assert.deepEqual(automaticReminderItems([completed], 'connected', new Map()).map(row => row.id), ['codex:1']);
  assert.deepEqual(automaticReminderItems([item({session: {...waiting.session, status: 'running'}})], 'connected', new Map()), []);
  assert.deepEqual(automaticReminderItems([waiting], 'connecting', new Map()), []);
  assert.deepEqual(automaticReminderItems([waiting], 'offline', new Map()), []);
  assert.deepEqual(automaticReminderItems([item({offline: true})], 'connected', new Map()), []);
  assert.deepEqual(automaticReminderItems([completed], 'offline', new Map()), []);
});

test('a failed session raises a card on a connected, healthy source and nothing else does', () => {
  const failed = item({session: {...item().session, status: 'error', pending: []}});
  assert.deepEqual(automaticReminderItems([failed], 'connected', new Map()).map(row => row.id), ['codex:1']);
  assert.deepEqual(automaticReminderItems([failed], 'connecting', new Map()), []);
  assert.deepEqual(automaticReminderItems([failed], 'offline', new Map()), []);
  assert.deepEqual(automaticReminderItems([item({session: failed.session, offline: true})], 'connected', new Map()), []);
  assert.deepEqual(automaticReminderItems([item({session: {...failed.session, status: 'aborted'}})], 'connected', new Map()), [],
    'a round the user stopped themselves stays silent');
});

test('a muted question stays muted for that round and that payload only', () => {
  const waiting = item();
  const muted = new Map([['codex:1', questionKey(waiting)]]);
  assert.deepEqual(automaticReminderItems([waiting], 'connected', muted), [], 'the same question in the same round stays muted');

  const nextRound = item({session: {...waiting.session, roundId: 'r2'}});
  assert.deepEqual(automaticReminderItems([nextRound], 'connected', muted).map(row => row.id), ['codex:1'], 'a new round restores it');
  assert.equal(muted.size, 0, 'and the stale entry is dropped');

  muted.set('codex:1', questionKey(waiting));
  const newQuestion = item({session: {...waiting.session, pending: [{id: 'q2', text: '乙'}]}});
  assert.deepEqual(automaticReminderItems([newQuestion], 'connected', muted).map(row => row.id), ['codex:1'], 'a different question in the same round restores it');
});

test('a mute expires when its session stops waiting or leaves', () => {
  const waiting = item();
  const muted = new Map([['codex:1', questionKey(waiting)]]);
  automaticReminderItems([item({session: {...waiting.session, status: 'running', pending: []}})], 'connected', muted);
  assert.equal(muted.size, 0, 'a resumed session clears its mute');

  muted.set('codex:1', questionKey(waiting));
  automaticReminderItems([], 'connected', muted);
  assert.equal(muted.size, 0, 'a session that left the rail clears its mute');
});

test('questionKey separates rounds and payloads but not unrelated changes', () => {
  const waiting = item();
  assert.equal(questionKey(waiting), questionKey({...waiting, offline: true}), 'connection state is not part of the key');
  assert.notEqual(questionKey(waiting), questionKey({...waiting, session: {...waiting.session, roundId: 'r2'}}));
  assert.notEqual(questionKey(waiting), questionKey({...waiting, session: {...waiting.session, pending: [{id: 'q2', text: '乙'}]}}));
});

test('a Codex permission check raises a cautious reminder only after 90 seconds', () => {
  const now = 100_000;
  const running = item({session: {...item().session, status: 'running', pending: [], permissionChecks: [{id: 'perm:a', ts: now - 89_999}]}});
  const reminders = session => automaticReminderItems([item({session})], 'connected', new Map(), now);
  assert.deepEqual(reminders(running.session), []);
  assert.deepEqual(reminders({...running.session, permissionChecks: [{id: 'perm:a', ts: now - 90_000}]}).map(row => row.id), ['codex:1']);
  assert.deepEqual(reminders({...running.session, permissionChecks: []}), [], 'resolution withdraws the reminder');
  assert.deepEqual(reminders({...running.session, status: 'done'}).map(row => row.id), ['codex:1'], 'completion follows the existing done reminder');
  assert.deepEqual(reminders({...running.session, stale: true}), [], 'stale sessions do not raise a new reminder');
  assert.deepEqual(automaticReminderItems([running], 'offline', new Map(), now + 1), []);
});

test('slow permission reminder can be muted for one request and uses cautious wording', () => {
  const now = Date.now();
  const running = item({session: {...item().session, status: 'running', pending: [], permissionChecks: [{id: 'perm:a', ts: now - 90_001}]}});
  const muted = new Map([['codex:1', permissionReminderKey(running.session)]]);
  const reminders = session => automaticReminderItems([item({session})], 'connected', muted, now);
  assert.equal(sessionPresentation(running.session).statusLabel, '权限请求未完成');
  assert.equal(sessionPresentation(running.session).question, '请查看 Codex，可能仍在自动审批');
  assert.deepEqual(reminders(running.session), []);
  assert.deepEqual(reminders({...running.session, permissionChecks: [{id: 'perm:b', ts: now - 90_001}]}).map(row => row.id), ['codex:1']);
  assert.deepEqual(reminders({...running.session, permissionChecks: []}), []);
});
