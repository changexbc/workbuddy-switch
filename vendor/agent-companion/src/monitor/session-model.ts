import { visibleSession } from './session-visibility.js';
import { ACTIVE } from './model.js';
import type { ConnectionState, Session, SessionStatus, Snapshot } from '../types/snapshot.js';

export interface PortraitBase { id: string; name: string; avatar: null }
export interface RailIdentity extends PortraitBase { slot: number; number: number }

/** Only the two calls the portrait cache needs; tests pass a Map-backed fake. */
export interface PortraitStorage {
  getItem(key: string): string | null | undefined;
  setItem(key: string, value: string): void;
}

interface RailRow {
  identity: RailIdentity;
  session: Session;
  offline: boolean;
  expiresAt?: number | null;
  openedUntil?: number | null;
  hostGoneUntil?: number | null;
}

export interface RailItem extends RailRow { id: string }

export interface RailModelOptions {
  now?: () => number;
  storage?: PortraitStorage | null;
  holdMs?: number;
}

export const PORTRAITS: PortraitBase[] = [
  ['red-panda', '小熊猫'], ['elephant', '大象'], ['penguin', '企鹅'],
  ['pig', '小猪'], ['dragon', '幼龙'], ['lion', '狮子'],
  ['koala', '考拉'], ['owl', '猫头鹰'], ['robot', '小机器人'],
  ['deer', '小鹿'], ['hedgehog', '刺猬'], ['giraffe', '长颈鹿'], ['tiger', '老虎'],
].map(([id, name]) => ({ id, name, avatar: null }));
export const PORTRAIT_STORAGE_KEY = 'astra.desktop.portraits.v2';
const TERMINAL = new Set<SessionStatus>(['done', 'error', 'aborted']);
export const OPENED_HOLD_MS = 10_000;
export const FINISHED_HOLD_MS = 60 * 60_000;
export const HOST_EXIT_GRACE_MS = 15_000;
export const OVERFLOW_LIMIT = 8;
export const OVERFLOW_HOLD_MS = 10_000;

export function createRailModel({ now = Date.now, storage, holdMs = FINISHED_HOLD_MS }: RailModelOptions = {}) {
  const rows = new Map<string, RailRow>(), portraits = new Map<string, number>(), dismissed = new Set<string>();
  let snapshot: Snapshot | null = null, connection: ConnectionState = 'connecting';
  let overflow: { id: string; roundId: string; until: number } | null = null;
  try {
    const saved: unknown = JSON.parse(storage?.getItem(PORTRAIT_STORAGE_KEY) || '[]');
    if (Array.isArray(saved)) for (const entry of (saved as unknown[]).slice(-256)) {
      const [id, index] = Array.isArray(entry) ? entry as unknown[] : [];
      if (typeof id === 'string' && typeof index === 'number' && Number.isInteger(index) && index >= 0 && index < 1_000_000) portraits.set(id, index);
    }
  } catch { /* An unavailable or old cache does not stop monitoring. */ }
  function portrait(id: string): RailIdentity {
    const occupied = new Set([...rows.values()].map(row => row.identity.slot));
    if (!portraits.has(id) || occupied.has(portraits.get(id)!)) {
      const counts = PORTRAITS.map((_, index) => [...occupied].filter(slot => slot % PORTRAITS.length === index).length);
      let slot = counts.indexOf(Math.min(...counts));
      while (occupied.has(slot)) slot += PORTRAITS.length;
      portraits.set(id, slot);
      while (portraits.size > Math.max(256, rows.size + 1)) {
        const old = [...portraits.keys()].find(key => !rows.has(key) && key !== id);
        if (!old) break;
        portraits.delete(old);
      }
      try { storage?.setItem(PORTRAIT_STORAGE_KEY, JSON.stringify([...portraits].slice(-256))); } catch {}
    }
    const slot = portraits.get(id)!, base = PORTRAITS[slot % PORTRAITS.length];
    const number = Math.floor(slot / PORTRAITS.length) + 1;
    return { ...base, slot, number, name: number > 1 ? `${base.name} ${number}` : base.name };
  }
  function reconcile() {
    if (!snapshot) return;
    const online = connection === 'connected', sessions = new Map(snapshot.sessions.filter(visibleSession).map(s => [s.id, s]));
    for (const [id, row] of rows) {
      const next = sessions.get(id);
      const source = next?.source || row.session.source;
      const state = snapshot.sources?.[source]?.state;
      const healthy = online && (state === 'ok' || state === 'partial');
      if (state === 'disabled' || (!next && healthy)) { rows.delete(id); continue; }
      // A host-process exit is an observed fact, not a timeout guess: show the
      // result briefly, then retire the row. This must run before the health
      // short-circuit below or an unhealthy source would freeze the row.
      if (online && state === 'exited') {
        if (next) row.session = next;
        row.offline = true;
        row.hostGoneUntil ??= now() + HOST_EXIT_GRACE_MS;
        row.expiresAt = row.hostGoneUntil;
        if (!next || row.hostGoneUntil <= now()) rows.delete(id);
        continue;
      }
      row.hostGoneUntil = null;
      if (next && (!TERMINAL.has(next.status) || row.session.roundId !== next.roundId)) row.openedUntil = null;
      if (next) row.session = next;
      row.offline = !healthy;
      if (!next || !healthy) continue;
      if (TERMINAL.has(next.status)) {
        if (!row.openedUntil && next.viewedRoundId != null && next.viewedRoundId === next.roundId) {
          dismissed.add(id); rows.delete(id); continue;
        }
        const ended = next.endedAt ?? next.updatedAt ?? now();
        // A host exit keeps its short grace even if the app relaunches before it
        // elapses; a later round is what restores the normal one-hour hold.
        const expiresAt = row.openedUntil || (next.endedBy === 'host' ? row.hostGoneUntil ?? ended + HOST_EXIT_GRACE_MS : ended + holdMs);
        row.expiresAt = expiresAt;
        if (dismissed.has(id) || expiresAt <= now()) rows.delete(id);
      } else row.expiresAt = null;
    }
    for (const session of sessions.values()) {
      const state = snapshot.sources?.[session.source]?.state;
      const healthy = state === 'ok' || state === 'partial';
      if (ACTIVE.has(session.status) && online && healthy) {
        dismissed.delete(session.id);
        if (!rows.has(session.id)) { const identity = portrait(session.id); rows.set(session.id, { identity, session, offline: false }); }
      }
    }
    for (const id of dismissed) if (!sessions.has(id)) dismissed.delete(id);
    // Retire one oldest successful completion at a time. A new turn or a
    // recovered source must earn a fresh grace period, never inherit a timer.
    const oldest = [...rows.entries()]
      .filter(([, row]) => !row.offline && row.session.status === 'done')
      .sort((a, b) => (a[1].session.endedAt ?? a[1].session.updatedAt ?? 0)
        - (b[1].session.endedAt ?? b[1].session.updatedAt ?? 0))[0];
    if (rows.size <= OVERFLOW_LIMIT || !oldest) overflow = null;
    else {
      const [id, row] = oldest;
      if (overflow?.id !== id || overflow.roundId !== row.session.roundId) {
        overflow = { id, roundId: row.session.roundId, until: now() + OVERFLOW_HOLD_MS };
      }
      const due = overflow;
      if (due.until <= now()) {
        dismissed.add(id); rows.delete(id); overflow = null;
        reconcile();
      }
    }
  }
  return {
    accept(value: Snapshot) { snapshot = value; reconcile(); },
    connect(value: ConnectionState) { connection = value; reconcile(); },
    retainOpened(id: string, roundId: string) {
      const row = rows.get(id);
      if (!row || !TERMINAL.has(row.session.status) || row.session.roundId !== roundId) return;
      row.openedUntil ??= now() + OPENED_HOLD_MS;
      row.expiresAt = row.openedUntil;
    },
    cancelOpened(id: string, roundId: string) {
      const row = rows.get(id);
      if (row?.session.roundId === roundId) { row.openedUntil = null; reconcile(); }
    },
    dismiss(id: string, roundId?: string) {
      const session = rows.get(id)?.session;
      if (session && TERMINAL.has(session.status) && (roundId === undefined || session.roundId === roundId)) {
        dismissed.add(id); rows.delete(id); reconcile();
      }
    },
    refresh: reconcile,
    get items(): RailItem[] { return [...rows].map(([id, row]) => ({ id, ...row })); },
    get nextExpiry(): number | null {
      const times: number[] = [];
      for (const row of rows.values()) if (row.expiresAt && (!row.offline || row.hostGoneUntil)) times.push(row.expiresAt);
      if (overflow) times.push(overflow.until);
      return times.length ? Math.min(...times) : null;
    },
    get snapshot() { return snapshot; },
    get connection() { return connection; },
  };
}
