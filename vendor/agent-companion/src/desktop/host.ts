/**
 * Native bridge. Kept importable from plain Node: every Tauri module is loaded
 * with a dynamic `import()` and `isDesktop()` is false without a host, so tests
 * and the collector can import this file (directly or through `session-link.js`)
 * without a bundler.
 */
import type {
  SessionMonitorCloseResult,
  CollectorRequestCommand,
  DesktopCommand,
  DesktopCommandMap,
  DesktopView,
} from '../types/commands.js';
import type { IntegrationAction } from '../types/integrations.js';
import type { RailPreferencesState, Settings } from '../types/settings.js';
import type { ConnectionState, Snapshot } from '../types/snapshot.js';

type Unlisten = () => void;
type RawEvent = { payload: unknown };
type RawListen = (event: string, handler: (event: RawEvent) => void) => Promise<Unlisten>;
type RawInvoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

/** Provided by the embedding host application when this UI runs inside another window. */
interface EmbedHost {
  desktop?: boolean;
  listen: RawListen;
  invoke: RawInvoke;
  enableNotifications: () => Promise<string>;
}

declare global {
  var __TAURI_INTERNALS__: unknown;
  interface Window {
    __AGENT_STUDIO_EMBED_HOST__?: EmbedHost;
  }
}

const embedHost = (): EmbedHost | null => {
  try {
    return window.parent !== window ? window.parent.__AGENT_STUDIO_EMBED_HOST__ ?? null : null;
  } catch {
    return null;
  }
};

/** Tauri's `listen` hands over a richer event object; callers only read `payload`. */
async function eventApi(): Promise<{ listen: RawListen }> {
  const host = embedHost();
  if (host) return { listen: host.listen };
  const { listen } = await import('@tauri-apps/api/event');
  return { listen: (event, handler) => listen(event, ({ payload }) => handler({ payload })) };
}

async function invokeApi(): Promise<{ invoke: RawInvoke }> {
  const host = embedHost();
  if (host) return { invoke: host.invoke };
  const { invoke } = await import('@tauri-apps/api/core');
  return { invoke: (command, args) => invoke(command, args) };
}

export const isDesktop = () => Boolean(globalThis.__TAURI_INTERNALS__ || embedHost()?.desktop);
export const isStandaloneDesktopWindow = () => Boolean(globalThis.__TAURI_INTERNALS__) && !embedHost();

export function disableNativeContentDrag(target: Document = document) {
  target.addEventListener('dragstart', event => event.preventDefault());
}

export function installStandaloneWindowChrome() {
  disableNativeContentDrag();
  if (!isStandaloneDesktopWindow() || document.documentElement.classList.contains('tauri-desktop')) return;
  document.documentElement.classList.add('tauri-desktop');
  const drag = document.createElement('div');
  drag.className = 'window-chrome-drag';
  drag.dataset.tauriDragRegion = '';
  drag.setAttribute('aria-hidden', 'true');
  document.body.prepend(drag);
}

/**
 * Commands with no arguments may be called without the second parameter.
 * Results are typed by `DesktopCommandMap`; Tauri's generic is a compile-time
 * claim only, not a runtime check of what Rust actually returns.
 */
type CommandArgs<K extends DesktopCommand> = DesktopCommandMap[K]['args'] extends Record<string, never>
  ? [args?: undefined]
  : [args: DesktopCommandMap[K]['args']];

export async function desktopCommand<K extends DesktopCommand>(
  command: K,
  ...rest: CommandArgs<K>
): Promise<DesktopCommandMap[K]['result']> {
  const { invoke } = await invokeApi();
  return (await invoke(`plugin:agent-studio|${command}`, rest[0] as Record<string, unknown> | undefined)) as DesktopCommandMap[K]['result'];
}

/** Ends only the selected session round. The next activity may create another row. */
export async function closeSessionMonitoring(source: string, sessionId: string, roundId: string): Promise<boolean> {
  if (!isDesktop()) throw new Error('此操作仅在桌面悬浮窗中可用');
  const result = await desktopCommand('collector_request', {
    command: 'session_monitor_close', payload: {source, sessionId, roundId},
  }) as SessionMonitorCloseResult;
  return result.closed === true;
}

const railPreferenceKey = 'astra.desktop.visible-count.v1';

function withRailPreference(value: Settings): Settings {
  try {
    const count = Number(localStorage.getItem(railPreferenceKey));
    if (value?.monitor && Number.isInteger(count) && count >= 3 && count <= 16) value.monitor.railVisibleCount = count as Settings['monitor']['railVisibleCount'];
  } catch {}
  return value;
}

function publishAppearance(value: Settings) {
  // Publish only confirmed saves; the backend remains the source of truth.
  try { localStorage.setItem('astra.desktop.appearance.v1', JSON.stringify(value)); } catch {}
  window.dispatchEvent(new CustomEvent('agent-studio-appearance', {detail: value}));
}

export function onDesktopSettingsChange(callback: (settings: Settings) => void) {
  const onLocal = (event: Event) => { const detail = (event as CustomEvent<Settings>).detail; if (detail) callback(detail); };
  const onStorage = (event: StorageEvent) => { if (event.key === 'astra.desktop.appearance.v1' && event.newValue) { try { callback(JSON.parse(event.newValue) as Settings); } catch {} } };
  window.addEventListener('agent-studio-appearance', onLocal);
  window.addEventListener('storage', onStorage);
  const cleanup = () => { window.removeEventListener('agent-studio-appearance', onLocal); window.removeEventListener('storage', onStorage); };
  if (!isDesktop()) return cleanup;
  let disposed = false, release: Unlisten | undefined;
  eventApi().then(({ listen }) => listen('monitor-settings', ({ payload }) => { if (!disposed) callback(withRailPreference(payload as Settings)); }))
    .then(value => { if (disposed) value(); else release = value; }).catch(() => {});
  return () => { disposed = true; release?.(); cleanup(); };
}

export function subscribeDesktop(onSnapshot: (snapshot: Snapshot) => void, onConnection: (state: ConnectionState) => void) {
  let disposed = false, releases: Unlisten[] = [], latest = -Infinity;
  const deliver = (snapshot: Snapshot) => { if (snapshot.ts < latest) return; latest = snapshot.ts; onSnapshot(snapshot); };
  onConnection('connecting');
  (async () => {
    const { listen } = await eventApi();
    const keep = (release: Unlisten) => { if (disposed) release(); else releases.push(release); };
    keep(await listen('monitor-state', ({ payload }) => { if (!disposed) { const snapshot = payload as Snapshot; deliver(snapshot); onConnection(snapshot.ready ? 'connected' : 'connecting'); } }));
    keep(await listen('monitor-connection', ({ payload }) => { if (!disposed) onConnection(payload as ConnectionState); }));
    const initial = await desktopCommand('monitor_state');
    if (!disposed) {
      if (initial.snapshot) deliver(initial.snapshot);
      onConnection(initial.connected ? 'connected' : 'offline');
    }
  })().catch(() => { if (!disposed) onConnection('offline'); });
  return () => { disposed = true; releases.forEach(release => release()); releases = []; };
}

/** Minimal response surface: a real `Response` in the browser, a shim over the desktop command. */
export interface HostResponse {
  ok: boolean;
  json(): Promise<unknown>;
}

export async function hostFetch(url: string, options: {method?: string; headers?: Record<string, string>; body?: string} = {}): Promise<HostResponse> {
  if (!isDesktop()) {
    const response = await fetch(url, options);
    if (url === '/api/settings' && options.method === 'PUT' && response.ok) publishAppearance(await response.clone().json() as Settings);
    return response;
  }
  let command: CollectorRequestCommand;
  if (url === '/api/settings') command = options.method === 'PUT' ? 'settings_set' : 'settings_get';
  else if (url === '/api/settings/check') command = 'settings_check';
  else if (url === '/api/integrations') command = options.method === 'POST' ? 'integrations_set' : 'integrations_get';
  else if (url === '/api/custom-integrations') command = options.method === 'POST' ? 'custom_integrations_set' : 'custom_integrations_get';
  else if (url === '/api/custom-integrations/preview') command = 'custom_preview';
  else throw new Error('Unsupported desktop request');
  try {
    const value = await desktopCommand('collector_request', { command, payload: options.body ? JSON.parse(options.body) as Settings | IntegrationAction : null });
    if (command === 'settings_set') {
      publishAppearance(value as Settings);
      const count = (JSON.parse(options.body || '{}') as Partial<Settings>).monitor?.railVisibleCount;
      if (Number.isInteger(count) && count! >= 3 && count! <= 16) {
        try { localStorage.setItem(railPreferenceKey, String(count)); } catch {}
        window.dispatchEvent(new CustomEvent('agent-studio-rail-preference', {detail: count}));
      }
    }
    return { ok: true, json: async () => url === '/api/settings' ? withRailPreference(value as Settings) : value };
  } catch (error) { return { ok: false, json: async () => ({ error: String(error) }) }; }
}

export async function enableNotifications() {
  if (embedHost()) return embedHost()!.enableNotifications();
  if (isDesktop()) {
    const { isPermissionGranted, requestPermission } = await import('@tauri-apps/plugin-notification');
    return await isPermissionGranted() ? 'granted' : requestPermission();
  }
  return globalThis.Notification ? Notification.requestPermission() : 'unsupported';
}

export async function openDesktopView(view: DesktopView) {
  if (!['rail', 'settings'].includes(view)) throw new Error('未知视图');
  if (isDesktop()) return desktopCommand('open_view', { view });
  window.open(new URL(view === 'rail' ? 'desktop.html' : 'desktop-settings.html', window.location.href), '_blank', 'noopener,noreferrer');
}

// Event-driven native hover, including while another app owns keyboard focus.
export function onDesktopPointer(callback: (point: {x: number; y: number} | null) => void) {
  if (!isDesktop()) return () => {};
  let disposed = false, release: Unlisten | undefined;
  eventApi().then(({ listen }) => listen('agent-studio-pointer', ({ payload }) => {
    if (!disposed) callback(payload as {x: number; y: number} | null);
  })).then(value => { if (disposed) value(); else release = value; }).catch(() => {});
  return () => { disposed = true; release?.(); };
}

export function onDesktopWindowActive(callback: (active: boolean) => void) {
  if (!isDesktop()) return () => {};
  let disposed = false, release: Unlisten | undefined;
  eventApi().then(({listen}) => listen('agent-studio-window-active', ({payload}) => {
    const active = (payload as {active?: unknown} | null)?.active;
    if (!disposed && typeof active === 'boolean') callback(active);
  })).then(fn => { if (disposed) fn(); else release = fn; }).catch(() => {});
  return () => { disposed = true; release?.(); };
}

export function onRailPreferences(callback: (state: RailPreferencesState) => void) {
  if (!isDesktop()) return () => {};
  let disposed = false, release: Unlisten | undefined;
  eventApi().then(({ listen }) => listen('agent-studio-rail-settings', ({ payload }) => { if (!disposed) callback(payload as RailPreferencesState); }))
    .then(fn => { if (disposed) fn(); else release = fn; }).catch(() => {});
  return () => { disposed = true; release?.(); };
}
