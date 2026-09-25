/**
 * Fictional snapshots that drive the embedded Agent Companion rail on the
 * public demo page.
 *
 * The rail ships a web demo build whose only data source is `postMessage`
 * (upstream `src/demo/protocol.ts`): the page publishes whole snapshots and the
 * frame answers with the interactions it cannot carry out on its own. This
 * module owns the wb-switch side of that contract — the wire types are
 * duplicated here on purpose, because `vendor/` is a pinned source snapshot and
 * never a dependency of this frontend.
 *
 * Every value below is invented for the demo. Nothing reads a real session,
 * a real project or the visitor's storage.
 */

export const COMPANION_DEMO_CHANNEL = "agent-companion-demo";

type CompanionDemoStatus = "running" | "wait" | "done" | "error";
type CompanionDemoEventKind = "wait" | "done" | "error";

interface CompanionDemoStep {
  id: string;
  ts: number;
  label: string;
}

interface CompanionDemoQuestionOption {
  label: string;
  description: string;
}

interface CompanionDemoQuestion {
  text: string;
  header: string;
  options: CompanionDemoQuestionOption[];
}

interface CompanionDemoRequest {
  id: string;
  tool: string;
  text: string;
  questions: CompanionDemoQuestion[];
  ts: number;
}

interface CompanionDemoSession {
  id: string;
  source: string;
  sessionId: string;
  cwd: string;
  title: string;
  roundId: string;
  startedAt: number;
  updatedAt: number;
  status: CompanionDemoStatus;
  steps: CompanionDemoStep[];
  pending: CompanionDemoRequest[];
  tokens: number | null;
  endedAt: number | null;
  project: string;
  stale: boolean;
  elapsed: number;
  progress: null;
}

interface CompanionDemoEvent {
  id: string;
  sessionId: string;
  roundId: string;
  kind: CompanionDemoEventKind;
  ts: number;
  historical: boolean;
  title: string;
}

export interface CompanionDemoSnapshot {
  version: 1;
  ready: boolean;
  ts: number;
  sources: Record<string, { state: "ok"; detail: string; checkedAt: number }>;
  sessions: CompanionDemoSession[];
  events: CompanionDemoEvent[];
}

/** The fictional cast, in rail order: the first row is the one that changes state. */
const COMPANION_DEMO_CAST = [
  {
    id: "codex:demo-rail-story",
    source: "codex",
    sessionId: "demo-rail-story",
    cwd: "/Users/demo/workspace/agent-companion",
    title: "生成发布说明",
    project: "agent-companion",
    tokens: 48210,
  },
  {
    id: "workbuddy:demo-release-notes",
    source: "workbuddy",
    sessionId: "demo-release-notes",
    cwd: "/Users/demo/workspace/2026-09-25-10-12-04",
    title: "汇总本周的发布说明",
    project: "2026-09-25-10-12-04",
    tokens: 15230,
  },
] as const;

/** The session that asks for confirmation and later finishes. */
const COMPANION_DEMO_STORY_ID = COMPANION_DEMO_CAST[0].id;

const COMPANION_DEMO_STEPS: CompanionDemoStep[] = [
  { id: "demo-step-read", ts: 0, label: "读取悬浮栏的会话模型" },
  { id: "demo-step-write", ts: 0, label: "更新演示场景的数据" },
];

const COMPANION_DEMO_REQUEST: CompanionDemoRequest = {
  id: "demo-question-1",
  tool: "codex",
  text: "是否现在发布 0.2.0？",
  questions: [
    {
      text: "是否现在发布 0.2.0？",
      header: "发布确认",
      options: [
        { label: "现在发布", description: "生成安装包并发布" },
        { label: "稍后发布", description: "先补充演示截图" },
      ],
    },
  ],
  ts: 0,
};

interface CompanionDemoFrame {
  status: CompanionDemoStatus;
  pending?: CompanionDemoRequest[];
  endedAt?: number;
}

/**
 * One broadcast round, in order. The rail retains a completion only for a
 * session it already showed, so the first frame of every round is `running`;
 * the round then walks to the confirmation, the completion and the failure
 * before a fresh round id restarts it.
 */
export const COMPANION_DEMO_ROUND: { phase: CompanionDemoStatus; holdMs: number }[] = [
  { phase: "running", holdMs: 4000 },
  { phase: "wait", holdMs: 4500 },
  { phase: "done", holdMs: 4000 },
  { phase: "error", holdMs: 4000 },
];

function runningFrames(): Record<string, CompanionDemoFrame> {
  const frames: Record<string, CompanionDemoFrame> = {};
  for (const member of COMPANION_DEMO_CAST) frames[member.id] = { status: "running" };
  return frames;
}

function framesForPhase(phase: CompanionDemoStatus, now: number): Record<string, CompanionDemoFrame> {
  const frames = runningFrames();
  if (phase === "running") return frames;
  const secondId = COMPANION_DEMO_CAST[1].id;
  if (phase === "wait") {
    frames[COMPANION_DEMO_STORY_ID] = { status: "wait", pending: [{ ...COMPANION_DEMO_REQUEST, ts: now }] };
    return frames;
  }
  // `done` finishes the story session; `error` keeps it finished and fails the
  // second one, which is how the rail ends up showing both terminal states.
  frames[COMPANION_DEMO_STORY_ID] = { status: "done", endedAt: now };
  if (phase === "error") frames[secondId] = { status: "error", endedAt: now };
  return frames;
}

function demoSession(
  member: (typeof COMPANION_DEMO_CAST)[number],
  roundId: string,
  now: number,
  frame: CompanionDemoFrame,
): CompanionDemoSession {
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
    steps: COMPANION_DEMO_STEPS.map((step) => ({ ...step, ts: startedAt + 60_000 })),
    pending: frame.pending ?? [],
    tokens: member.tokens,
    endedAt,
    project: member.project,
    stale: false,
    elapsed: Math.max(0, ((endedAt ?? now) - startedAt) / 1000),
    progress: null,
  };
}

/** Events use the producers' `[sessionId, roundId, kind, key]` identifier. */
function demoEvents(roundId: string, now: number, frames: Record<string, CompanionDemoFrame>): CompanionDemoEvent[] {
  const list: CompanionDemoEvent[] = [];
  for (const member of COMPANION_DEMO_CAST) {
    const frame = frames[member.id];
    if (!frame) continue;
    const keys = frame.status === "wait"
      ? (frame.pending ?? []).map((request) => ({ kind: "wait" as const, key: request.id }))
      : frame.status === "done" || frame.status === "error"
        ? [{ kind: frame.status, key: roundId }]
        : [];
    for (const { kind, key } of keys) {
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

export function buildCompanionDemoSnapshot(phase: CompanionDemoStatus, roundId: string, now = Date.now()): CompanionDemoSnapshot {
  const frames = framesForPhase(phase, now);
  const sources: CompanionDemoSnapshot["sources"] = {};
  for (const member of COMPANION_DEMO_CAST) sources[member.source] = { state: "ok", detail: "演示数据", checkedAt: now };
  return {
    version: 1,
    ready: true,
    ts: now,
    sources,
    sessions: COMPANION_DEMO_CAST.map((member) => demoSession(member, roundId, now, frames[member.id])),
    events: demoEvents(roundId, now, frames),
  };
}

/** Publishes one snapshot to the embedded rail. Same-origin is part of the contract. */
export function postCompanionDemoSnapshot(frame: HTMLIFrameElement | null, snapshot: CompanionDemoSnapshot): void {
  const target = frame?.contentWindow;
  if (!target) return;
  target.postMessage({ channel: COMPANION_DEMO_CHANNEL, type: "snapshot", snapshot }, window.location.origin);
}
