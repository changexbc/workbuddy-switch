/**
 * Standalone updater bridge.
 *
 * The update service belongs to the standalone host (`src-tauri/update_service.rs`),
 * so these commands are invoked without the `plugin:agent-studio|` prefix and are
 * only reachable when the standalone desktop window is loaded directly. Embedded
 * hosts and the browser keep their own application updates: every entry point here
 * reports unsupported instead of calling into an unrelated host updater.
 *
 * Named imports stay side-effect free so plain Node (tests, collector) can import
 * this module without a Tauri runtime.
 */
import { isStandaloneDesktopWindow } from './host.js';
import { isUpdateConfig, isUpdateSnapshot, type UpdateConfig, type UpdateSnapshot } from '../types/update.js';

export const updateProxyInvalidMessage = '更新代理地址无效，请填写 HTTP 或 HTTPS 地址，例如 http://127.0.0.1:7897';

/** Every update control is hidden unless this standalone host owns the commands. */
export const updatesSupported = () => isStandaloneDesktopWindow();

async function invokeUpdate(command: string, args?: Record<string, unknown>): Promise<unknown> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke(command, args);
}

export async function loadUpdateConfig(): Promise<UpdateConfig> {
  const value = await invokeUpdate('update_config_get');
  if (!isUpdateConfig(value)) throw new Error('更新配置读取失败');
  return value;
}

/** Saves the proxy; an empty value clears the explicit proxy. Native validation is authoritative. */
export async function saveUpdateProxy(proxy: string): Promise<UpdateConfig> {
  const value = await invokeUpdate('update_config_set', { proxy });
  if (!isUpdateConfig(value)) throw new Error('更新配置保存失败');
  return value;
}

export async function loadUpdateState(): Promise<UpdateSnapshot> {
  const value = await invokeUpdate('update_state');
  if (!isUpdateSnapshot(value)) throw new Error('更新状态读取失败');
  return value;
}

/** Manual check; rejects with the native failure message on error. */
export async function checkForUpdates(): Promise<UpdateSnapshot> {
  const value = await invokeUpdate('update_check');
  if (!isUpdateSnapshot(value)) throw new Error('更新状态读取失败');
  return value;
}

export async function downloadUpdate(): Promise<void> {
  await invokeUpdate('update_download');
}

/** Installs the downloaded package and restarts. The window reloads with the new version. */
export async function installUpdate(): Promise<void> {
  await invokeUpdate('update_install');
}

export async function openReleasePage(): Promise<void> {
  await invokeUpdate('update_open_release');
}

/**
 * Subscribes to the native snapshot: live `update-state` events plus one initial
 * read. The read covers transitions emitted before this subscription, and an event
 * that arrives first wins over that initial read.
 */
export function watchUpdateState(callback: (snapshot: UpdateSnapshot) => void): () => void {
  if (!updatesSupported()) return () => {};
  let disposed = false;
  let release: (() => void) | undefined;
  let received = false;
  void (async () => {
    const { listen } = await import('@tauri-apps/api/event');
    const unlisten = await listen<unknown>('update-state', event => {
      if (disposed || !isUpdateSnapshot(event.payload)) return;
      received = true;
      callback(event.payload);
    });
    if (disposed) {
      unlisten();
      return;
    }
    release = unlisten;
    const initial = await loadUpdateState();
    if (!disposed && !received) callback(initial);
  })().catch(() => {});
  return () => {
    disposed = true;
    release?.();
  };
}

/**
 * Client-side proxy check for immediate feedback. Command validation stays
 * authoritative; an empty value clears the proxy.
 */
export function validateProxyInput(value: string): {ok: true; value: string} | {ok: false; message: string} {
  const trimmed = value.trim();
  if (!trimmed) return {ok: true, value: ''};
  try {
    const parsed = new URL(trimmed);
    if (!['http:', 'https:'].includes(parsed.protocol) || !parsed.hostname) throw new Error('unsupported proxy');
  } catch {
    return {ok: false, message: updateProxyInvalidMessage};
  }
  return {ok: true, value: trimmed};
}

export type UpdateAction = 'check' | 'download' | 'restart';

/** Primary action for both the tray item and the settings button. */
export function updateAction(snapshot: UpdateSnapshot): UpdateAction {
  if (snapshot.phase === 'available') return 'download';
  if (snapshot.phase === 'readyToRestart') return 'restart';
  if (snapshot.phase === 'error' && snapshot.latest) return 'download';
  return 'check';
}

export function updateStatusText(snapshot: UpdateSnapshot, currentVersion: string): string {
  const latest = snapshot.latest ?? '';
  switch (snapshot.phase) {
    case 'checking':
      return '正在检查更新…';
    case 'upToDate':
      return `已是最新版本 v${currentVersion}`;
    case 'available': {
      // 复查失败时 native 快照保留目标版本并写入失败文案，两个信息都要显示。
      const found = `发现新版本 v${latest}（当前 v${currentVersion}）`;
      return snapshot.message ? `${found}；${snapshot.message}` : found;
    }
    case 'downloading':
      return snapshot.percent === null ? `正在下载 v${latest}…` : `正在下载 v${latest} ${snapshot.percent}%`;
    case 'readyToRestart':
      return `v${latest} 已下载，重启后完成安装`;
    case 'error':
      return snapshot.message ?? '更新失败，请重试';
    default:
      return '尚未检查更新';
  }
}
