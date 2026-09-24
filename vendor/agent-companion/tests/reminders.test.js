import test from 'node:test';
import assert from 'node:assert/strict';
import { automaticReminderItems, questionKey } from '../src/monitor/reminders.js';

// Only the fields the rule reads; a full RailItem would obscure what matters.
const item = over => ({
  id: 'codex:1',
  offline: false,
  identity: {id: 'x', name: 'x', avatar: null, slot: 0, number: 1},
  session: {id: 'codex:1', source: 'codex', sessionId: '1', status: 'wait', title: '任务', roundId: 'r1', updatedAt: 1, steps: [], pending: [{id: 'q1', text: '甲'}]},
  ...over,
});

test('only a waiting session on a connected, healthy source raises a card', () => {
  const waiting = item();
  assert.deepEqual(automaticReminderItems([waiting], 'connected', new Map()).map(row => row.id), ['codex:1']);
  // Completed sessions stay in the rail with a checkmark; only questions interrupt.
  assert.deepEqual(automaticReminderItems([item({session: {...waiting.session, status: 'done'}})], 'connected', new Map()), []);
  assert.deepEqual(automaticReminderItems([waiting], 'connecting', new Map()), []);
  assert.deepEqual(automaticReminderItems([waiting], 'offline', new Map()), []);
  assert.deepEqual(automaticReminderItems([item({offline: true})], 'connected', new Map()), []);
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
