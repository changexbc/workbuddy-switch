/**
 * The only link between the demo page and the rail frame it embeds.
 *
 * In the demo build `desktop.html` is loaded inside a bounded stage, so the two
 * documents agree on a tiny protocol instead of a shared source: the page
 * publishes whole fictional snapshots, and the frame reports the interactions
 * it cannot carry out on its own (opening a session, closing a monitor round)
 * plus the drag of its own grip. Both sides validate the channel and the shape
 * before acting, and both are only wired up in the demo build.
 */
import { isDemoClient, type DemoClient } from './scenarios.js';
import type { Snapshot } from '../types/snapshot.js';

export const DEMO_CHANNEL = 'agent-companion-demo';

/** Actions the desktop app owns and the public page therefore cannot run. */
export type DemoBlockedAction = 'open-session' | 'close-monitoring';

export interface DemoSnapshotMessage {
  channel: typeof DEMO_CHANNEL;
  type: 'snapshot';
  snapshot: Snapshot;
}

export type DemoPageMessage = DemoSnapshotMessage;

export type DemoFrameMessage =
  | {channel: typeof DEMO_CHANNEL; type: 'ready'}
  | {channel: typeof DEMO_CHANNEL; type: 'activate-client'; client: DemoClient}
  | {channel: typeof DEMO_CHANNEL; type: 'drag'; phase: 'start' | 'move' | 'end'; dx: number; dy: number}
  | {channel: typeof DEMO_CHANNEL; type: 'blocked'; action: DemoBlockedAction; text: string};

/** What a frame may report, without the channel the sender adds itself. */
export type DemoFrameReport =
  | {type: 'ready'}
  | {type: 'activate-client'; client: DemoClient}
  | {type: 'drag'; phase: 'start' | 'move' | 'end'; dx: number; dy: number}
  | {type: 'blocked'; action: DemoBlockedAction; text: string};

/** Enough of a snapshot to prove it respects the wire contract before it is used. */
function looksLikeSnapshot(value: unknown): value is Snapshot {
  if (typeof value !== 'object' || value === null) return false;
  const snapshot = value as Partial<Snapshot>;
  return snapshot.version === 1 && Array.isArray(snapshot.sessions) && Array.isArray(snapshot.events);
}

export function isDemoPageMessage(value: unknown): value is DemoPageMessage {
  if (typeof value !== 'object' || value === null) return false;
  const message = value as Partial<DemoPageMessage>;
  return message.channel === DEMO_CHANNEL && message.type === 'snapshot' && looksLikeSnapshot(message.snapshot);
}

export function isDemoFrameMessage(value: unknown): value is DemoFrameMessage {
  if (typeof value !== 'object' || value === null) return false;
  const message = value as Partial<DemoFrameMessage>;
  if (message.channel !== DEMO_CHANNEL) return false;
  if (message.type === 'ready') return true;
  if (message.type === 'activate-client') return isDemoClient(message.client);
  if (message.type === 'drag') {
    const drag = message as Partial<Extract<DemoFrameMessage, {type: 'drag'}>>;
    return (drag.phase === 'start' || drag.phase === 'move' || drag.phase === 'end')
      && Number.isFinite(drag.dx) && Number.isFinite(drag.dy);
  }
  if (message.type !== 'blocked') return false;
  const blocked = message as Partial<Extract<DemoFrameMessage, {type: 'blocked'}>>;
  return (blocked.action === 'open-session' || blocked.action === 'close-monitoring') && typeof blocked.text === 'string';
}
