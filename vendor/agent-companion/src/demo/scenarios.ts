/**
 * The fictional world the public web demo replays.
 *
 * Every value here is invented for the demo page and is never read from the
 * visitor's machine: no hooks, no webhooks, no local sessions, no real titles.
 * Scenarios are published as complete `Snapshot` values so that the rail's own
 * model keeps deciding portraits, reminders, retirement and completion
 * retention — the demo never re-implements those rules.
 *
 * A scenario is a short timeline rather than a single frame. The cast first
 * appears as running and then transitions, because that is the only way the real
 * model ever notices a status change: a completion is retained only for a
 * session that was already on the rail, and the same rule is what lets a visitor
 * see "工作中" turn into "待确认" or "已完成" instead of a card appearing from
 * nowhere.
 */
import { STORY_CHOICES, STORY_QUESTION } from './story.js';
import type { PendingRequest, Session, SessionStatus, SessionStep, Snapshot, SnapshotEvent, SourceHealth } from '../types/snapshot.js';

export type DemoClient = 'codex' | 'workbuddy';

export function isDemoClient(value: unknown): value is DemoClient {
  return value === 'codex' || value === 'workbuddy';
}

export type DemoScenarioName = 'idle' | 'working' | 'waiting' | 'done' | 'error';

export interface DemoScenarioStep {
  /** How long the page waits before publishing this step, in milliseconds. */
  delayMs: number;
  snapshot: Snapshot;
}

export interface DemoCastMember {
  /** `source:sessionId`, the identity the rail model keys rows and portraits by. */
  id: string;
  source: DemoClient;
  sessionId: string;
  cwd: string;
  title: string;
  project: string;
  tokens: number;
  agentType?: string;
  hostKind?: string;
}

/**
 * The fictional cast, in rail order: the first row is the one that changes
 * state. Both GUI clients remain visible without an overflow row.
 */
export const DEMO_CAST: DemoCastMember[] = [
  {id: 'codex:demo-rail-story', source: 'codex', sessionId: 'demo-rail-story', cwd: '/Users/demo/workspace/agent-companion', title: '生成发布说明', project: 'agent-companion', tokens: 48210},
  {id: 'workbuddy:demo-release-notes', source: 'workbuddy', sessionId: 'demo-release-notes', cwd: '/Users/demo/workspace/2026-09-24-17-24-22', title: '汇总本周的发布说明', project: '2026-09-24-17-24-22', tokens: 15230},
];

/** The session that asks for confirmation and later finishes. */
export const DEMO_STORY_ID = DEMO_CAST[0].id;

export const DEMO_SCENARIO_NAMES: DemoScenarioName[] = ['idle', 'working', 'waiting', 'done', 'error'];

/** How long the running frame is shown before the scenario's change lands. */
export const DEMO_STEP_GAP_MS = 600;

const SOURCE_NAMES = [...new Set(DEMO_CAST.map(member => member.source))];

export function demoClientForSession(id: string | undefined): DemoClient | undefined {
  return DEMO_CAST.find(member => member.id === id)?.source;
}

const STEPS: SessionStep[] = [
  {id: 'demo-step-read', ts: 0, label: '读取悬浮栏的会话模型'},
  {id: 'demo-step-write', ts: 0, label: '更新演示场景的数据'},
];

const WAIT_REQUEST: PendingRequest = {
  id: 'demo-question-1',
  tool: 'codex',
  text: '是否现在发布 0.2.0？',
  questions: [{
    text: '是否现在发布 0.2.0？',
    header: '发布确认',
    options: [
      {label: '现在发布', description: '生成安装包并发布'},
      {label: '稍后发布', description: '先补充演示截图'},
    ],
  }],
  ts: 0,
};

interface DemoFrameState {
  status: SessionStatus;
  pending?: PendingRequest[];
  endedAt?: number;
}

function allRunning(): Record<string, DemoFrameState> {
  const frames: Record<string, DemoFrameState> = {};
  for (const member of DEMO_CAST) frames[member.id] = {status: 'running'};
  return frames;
}

function sources(now: number): Record<string, SourceHealth> {
  const health: Record<string, SourceHealth> = {};
  for (const source of SOURCE_NAMES) health[source] = {state: 'ok', detail: '演示数据', checkedAt: now};
  return health;
}

function session(member: DemoCastMember, roundId: string, now: number, frame: DemoFrameState): Session {
  const startedAt = now - 12 * 60_000;
  const endedAt = frame.endedAt ?? null;
  return {
    id: member.id,
    source: member.source,
    sessionId: member.sessionId,
    cwd: member.cwd,
    title: member.title,
    roundId,
    startedAt,
    updatedAt: now,
    status: frame.status,
    steps: STEPS.map(step => ({...step, ts: startedAt + 60_000})),
    pending: frame.pending ?? [],
    tokens: member.tokens,
    endedAt,
    ...(member.agentType ? {agentType: member.agentType} : {}),
    ...(member.hostKind ? {hostKind: member.hostKind} : {}),
    project: member.project,
    stale: false,
    elapsed: Math.max(0, ((endedAt ?? now) - startedAt) / 1000),
    progress: null,
  };
}

/** Events use the producers' `[sessionId, roundId, kind, key]` identifier. */
function events(roundId: string, now: number, frames: Record<string, DemoFrameState>): SnapshotEvent[] {
  const list: SnapshotEvent[] = [];
  for (const member of DEMO_CAST) {
    const frame = frames[member.id];
    if (!frame) continue;
    const keys = frame.status === 'wait'
      ? (frame.pending ?? []).map(request => ({kind: 'wait' as const, key: request.id}))
      : frame.status === 'done' || frame.status === 'error' ? [{kind: frame.status, key: roundId}] : [];
    for (const {kind, key} of keys) {
      list.push({
        id: JSON.stringify([member.id, roundId, kind, key]),
        sessionId: member.id,
        roundId,
        kind,
        ts: now,
        historical: false,
        title: member.title,
      });
    }
  }
  return list;
}

function snapshot(roundId: string, now: number, frames: Record<string, DemoFrameState>): Snapshot {
  return {
    version: 1,
    ready: true,
    ts: now,
    sources: sources(now),
    sessions: DEMO_CAST.map(member => session(member, roundId, now, frames[member.id])),
    events: events(roundId, now, frames),
  };
}

/**
 * The timeline a scenario plays, in order.
 *
 * `roundId` defaults to a value derived from the play time so that replaying a
 * scenario is a new round: a reminder the visitor dismissed stays dismissable
 * for that round only, exactly as in the desktop rail.
 */
export function buildScenarioSteps(name: DemoScenarioName, now: number, roundId = `demo-${name}-${now}`): DemoScenarioStep[] {
  const running = snapshot(roundId, now, allRunning());
  if (name === 'idle') return [{delayMs: 0, snapshot: {...running, sessions: [], events: []}}];
  if (name === 'working') return [{delayMs: 0, snapshot: running}];
  const changed = allRunning();
  if (name === 'waiting') changed[DEMO_STORY_ID] = {status: 'wait', pending: [{...WAIT_REQUEST, ts: now}]};
  else changed[DEMO_STORY_ID] = {status: name === 'error' ? 'error' : 'done', endedAt: now};
  return [
    {delayMs: 0, snapshot: running},
    {delayMs: DEMO_STEP_GAP_MS, snapshot: snapshot(roundId, now + DEMO_STEP_GAP_MS, changed)},
  ];
}

export function buildStorySnapshot(state: import('./story.js').StoryState, now: number): Snapshot {
  const frames = allRunning();
  frames[DEMO_CAST[0].id] = {status: state.codex, ...(state.codex === 'done' ? {endedAt: state.codexEndedAt ?? now} : {})};
  frames[DEMO_CAST[1].id] = {status: state.workbuddy, ...((state.workbuddy === 'done' || state.workbuddy === 'error') ? {endedAt: state.workbuddyEndedAt ?? now} : {})};
  for (const member of DEMO_CAST) {
    if (state[member.source] !== 'wait') continue;
    frames[member.id].pending = [{
      id: `demo-version-choice-${member.source}`, tool: member.source, text: STORY_QUESTION, ts: now,
      questions: [{text: STORY_QUESTION, header: '版本选择', options: STORY_CHOICES.map(label => ({label, description: label}))}],
    }];
  }
  return snapshot(state.roundId, now, frames);
}
