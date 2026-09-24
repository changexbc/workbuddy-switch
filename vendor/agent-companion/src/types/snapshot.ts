/**
 * Wire contract for the monitor snapshot.
 *
 * Owned by the producers, not by the views:
 * - `crates/agent-studio-core/src/hub.rs` (`Hub::snapshot`) — the desktop app's source.
 * - `collector/lib/hub.js` (`Hub#snapshot`) — the browser/SSE source, kept in parity
 *   by `scripts/qa-rust-parity.mjs`.
 *
 * Views must consume these shapes instead of re-casting payload fields locally.
 */

/** Status a producer can write onto a session. */
export type SessionStatus = 'unknown' | 'running' | 'wait' | 'done' | 'error' | 'aborted';

/** `idle`/`offline` never come from the wire; `sessionPresentation` derives them. */
export type PresentationStatus = SessionStatus | 'idle' | 'offline';

export type ConnectionState = 'connecting' | 'connected' | 'offline';

export type SourceState = 'ok' | 'partial' | 'disabled' | 'exited' | 'error';

export interface SessionStep {
  id: string;
  ts: number;
  label: string;
}

export interface PendingQuestionOption {
  label: string;
  description: string;
}

/** Normalized by `questionDetails()` in `collector/lib/codex.js`. */
export interface PendingQuestion {
  text: string;
  header: string;
  options: PendingQuestionOption[];
}

export interface PendingRequest {
  id: string;
  tool?: string;
  text: string;
  questions: PendingQuestion[];
  ts: number;
}

export interface Session {
  id: string;
  source: string;
  sessionId: string;
  cwd: string;
  title: string;
  roundId: string;
  startedAt: number;
  updatedAt: number;
  status: SessionStatus;
  steps: SessionStep[];
  pending: PendingRequest[];
  tokens: number | null;
  endedAt: number | null;

  // Present only once an adapter has observed them.
  folderId?: string | number;
  agentType?: string;
  /**
   * Which application the hook reported: `"vscode"` for the CodeBuddy VS Code
   * plugin, `"codebuddy-ide"` for the IDE. Absent on older sessions and on any
   * unknown host, both of which stay IDE hosts.
   */
  hostKind?: string;
  /** Display name of an imported `custom:<id>` source, taken from its template. */
  sourceLabel?: string;
  externalId?: string;
  webPort?: number;
  permissionChecks?: string[];
  /** Set when a host process exited; absent after a new round starts. */
  endedBy?: string;
  /** Written by the Codex read-state watcher, not by the hub ingest path. */
  viewedRoundId?: string | null;

  // Added by the snapshot projection.
  project: string;
  stale: boolean;
  elapsed: number;
  progress: null;
}

export type SnapshotEventKind = 'wait' | 'done' | 'error';

export interface SnapshotEvent {
  id: string;
  sessionId: string;
  roundId: string;
  kind: SnapshotEventKind;
  ts: number;
  /** True for replay before the first ready poll, or for backfilled pending requests. */
  historical: boolean;
  title: string;
}

export interface SourceHealth {
  state: SourceState;
  detail: string;
  checkedAt: number;
  /** Producers spread adapter-specific extras over these three fields. */
  [extra: string]: unknown;
}

export interface Snapshot {
  version: 1;
  ready: boolean;
  ts: number;
  sources: Record<string, SourceHealth>;
  sessions: Session[];
  events: SnapshotEvent[];
}

/** `createNotificationTracker` decorates a fresh event with the time it was first seen. */
export interface MonitorAlert extends SnapshotEvent {
  receivedAt: number;
}

/** Healthy enough to trust a session's terminal state. */
export function isHealthySource(health: SourceHealth | undefined): boolean {
  return health?.state === 'ok' || health?.state === 'partial';
}
