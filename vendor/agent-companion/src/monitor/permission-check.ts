import type { Session } from '../types/snapshot.js';

export const PERMISSION_CHECK_REMINDER_MS = 90_000;

// This is a slow-request reminder, not evidence that Codex is waiting for a person.
export function prolongedPermissionCheck(session: Session, now = Date.now()): boolean {
  return session.source === 'codex' && session.status === 'running' && !session.stale
    && !!session.permissionChecks?.some(check =>
      Number.isFinite(check.ts) && check.ts <= now
      && now - check.ts >= PERMISSION_CHECK_REMINDER_MS);
}

export function permissionReminderKey(session: Session): string {
  return JSON.stringify([session.roundId, session.permissionChecks || []]);
}
