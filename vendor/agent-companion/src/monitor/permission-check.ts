import type { Session } from '../types/snapshot.js';

export const PERMISSION_CHECK_REMINDER_MS = 45_000;

// Sources whose client never reports a real「需要你允许」hook, so the only signal left
// is「工具开始后多久没有任何反馈」. Codex 原生会发，CodeBuddy IDE / VS Code 插件那条
// 链路实测是死代码（`notifyPermissionPrompt` 没有调用点）。
const SLOW_CHECK_SOURCES = new Set(['codex', 'codebuddy-ide']);

// This is a slow-request reminder, not proof that a person is being asked.
export function prolongedPermissionCheck(session: Session, now = Date.now()): boolean {
  return SLOW_CHECK_SOURCES.has(session.source) && session.status === 'running' && !session.stale
    && !!session.permissionChecks?.some(check =>
      Number.isFinite(check.ts) && check.ts <= now
      && now - check.ts >= PERMISSION_CHECK_REMINDER_MS);
}

export function permissionReminderKey(session: Session): string {
  return JSON.stringify([session.roundId, session.permissionChecks || []]);
}
