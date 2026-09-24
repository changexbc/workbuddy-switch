// Cross-checks the Rust core and the Node collector against the shared fixture
// tests/fixtures/custom-hooks.json. Both sides normalize the fixture the same
// way; anything that disagrees fails here instead of silently drifting.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { CUSTOM_LIMITS, createCustomEngine, customSource, validateTemplate } from '../collector/lib/custom.js';
import { Hub } from '../collector/lib/hub.js';

const fixture = JSON.parse(readFileSync(new URL('../tests/fixtures/custom-hooks.json', import.meta.url), 'utf8'));
const NOW = 1_700_000_000_000;

// Mirrors crates/agent-studio-core/src/lib.rs `text`.
const text = value => value === undefined || value === null ? '' : typeof value === 'string' ? value : JSON.stringify(value);
// Mirrors the Rust replay's `normalize_round`.
const normalizeRound = round => /^custom:[0-9]+$/.test(round) ? 'custom:assigned' : round;
const patch = (value, steps) => {
  const target = structuredClone(value);
  for (const step of steps) {
    const tokens = step.pointer.split('/').slice(1);
    let cursor = target;
    for (const token of tokens.slice(0, -1)) cursor = Array.isArray(cursor) ? cursor[Number(token)] : cursor[token];
    const last = tokens.at(-1);
    if (step.op === 'set') cursor[last] = structuredClone(step.value);
    else delete cursor[last];
  }
  return target;
};

function normalizeEvent(event) {
  const out = { type: event.type };
  for (const key of ['roundId', 'callId', 'status', 'endedBy', 'text', 'tool', 'source', 'sourceLabel', 'title', 'cwd', 'sessionId']) {
    if (event[key] === undefined || event[key] === null) continue;
    out[key] = key === 'roundId' ? normalizeRound(text(event[key])) : event[key];
  }
  return out;
}
function normalizeSnapshot(hub) {
  const snapshot = hub.snapshot();
  const sessions = snapshot.sessions.map(session => ({
    id: session.id, sessionId: session.sessionId, source: session.source,
    sourceLabel: session.sourceLabel ?? null, status: session.status,
    roundId: normalizeRound(text(session.roundId)), title: session.title, cwd: session.cwd,
    project: session.project, pending: session.pending.map(pending => pending.id),
    ended: session.endedAt !== null && session.endedAt !== undefined, endedBy: session.endedBy ?? null,
    stale: session.stale,
  }));
  const events = snapshot.events.map(event => ({ kind: event.kind, sessionId: event.sessionId, roundId: normalizeRound(text(event.roundId)) }));
  return { sessions, events };
}

const data = fixture;
const templates = data.templates;
const invalid = data.invalid.map(entry => {
  const result = validateTemplate(patch(data.base, entry.mutate));
  return { name: entry.name, path: result.ok ? null : result.path };
});
const cases = data.cases.map(entry => {
  const engine = createCustomEngine();
  let outcome = null, last = null;
  for (const step of entry.payloads) {
    const template = validateTemplate(templates[step.template]).template;
    outcome = engine.apply(template, step.payload, NOW);
    last = { id: templates[step.template].id, session: text(step.payload?.session_id) };
  }
  const raw = engine.sessionState(customSource(last.id), last.session);
  const state = raw ? { roundId: normalizeRound(raw.roundId), active: raw.active, pending: raw.pending } : null;
  return {
    name: entry.name, outcome: outcome.outcome, action: outcome.action ?? null, reason: outcome.reason,
    path: outcome.path, events: outcome.events.map(normalizeEvent), state,
  };
});
const sequences = data.sequences.map(entry => {
  const engine = createCustomEngine(), hub = new Hub();
  // Both sides read the wall clock for staleness; the fixture timestamps are far
  // enough from it that the result is deterministic.
  hub.startedAt = 1; hub.ready = true;
  const diagnostics = [];
  for (const step of entry.payloads) {
    const template = validateTemplate(templates[step.template]).template;
    const outcome = engine.apply(template, step.payload, NOW);
    diagnostics.push({ outcome: outcome.outcome, reason: outcome.reason });
    for (const event of outcome.events) hub.ingest(event);
  }
  return { name: entry.name, diagnostics, ...normalizeSnapshot(hub) };
});

const node = { limits: CUSTOM_LIMITS, invalid, cases, sequences };
const rust = JSON.parse(execFileSync('cargo', ['run', '--quiet', '-p', 'agent-studio-core', '--example', 'custom_replay'], {
  input: JSON.stringify(fixture), encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
}));
assert.deepEqual(node, rust);
console.log(`Rust/Node custom-hook parity passed: ${invalid.length} invalid templates, ${cases.length} payload cases, ${sequences.length} sequences`);
