/**
 * Standalone updater contract.
 *
 * Mirrors the commands and the `update-state` event owned by
 * `src-tauri/src/update_service.rs`. These commands belong to the standalone
 * binary, not to the shared `agent-studio` plugin, so they are invoked without
 * the `plugin:` prefix and only exist in the standalone desktop host.
 *
 * Native state is the single source of truth: the settings section and the tray
 * menu both read this snapshot, and nothing here re-derives progress or phases.
 */

export type UpdatePhase =
  | 'idle'
  | 'checking'
  | 'upToDate'
  | 'available'
  | 'downloading'
  | 'readyToRestart'
  | 'error';

const phases: readonly UpdatePhase[] = ['idle', 'checking', 'upToDate', 'available', 'downloading', 'readyToRestart', 'error'];

export interface UpdateSnapshot {
  phase: UpdatePhase;
  /** Target version without the `v` prefix; null while unknown or up to date. */
  latest: string | null;
  /** Download progress 0-100; null when the total size is unknown. */
  percent: number | null;
  message: string | null;
  /** Unix seconds of the last completed check. */
  checkedAt: number | null;
}

export interface UpdateConfig {
  proxy: string;
  currentVersion: string;
  releaseUrl: string;
}

export const idleUpdateSnapshot = (): UpdateSnapshot => ({phase: 'idle', latest: null, percent: null, message: null, checkedAt: null});

const isNullableString = (value: unknown) => value === null || typeof value === 'string';
const isNullablePercent = (value: unknown) => value === null || (typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 100);

/** Decodes one `update-state` payload or `update_state` result. */
export function isUpdateSnapshot(value: unknown): value is UpdateSnapshot {
  if (!value || typeof value !== 'object') return false;
  const candidate = value as Record<string, unknown>;
  return (
    phases.includes(candidate.phase as UpdatePhase) &&
    isNullableString(candidate.latest) &&
    isNullablePercent(candidate.percent) &&
    isNullableString(candidate.message) &&
    (candidate.checkedAt === null || typeof candidate.checkedAt === 'number')
  );
}

export function isUpdateConfig(value: unknown): value is UpdateConfig {
  if (!value || typeof value !== 'object') return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.proxy === 'string' && typeof candidate.currentVersion === 'string' && typeof candidate.releaseUrl === 'string';
}
