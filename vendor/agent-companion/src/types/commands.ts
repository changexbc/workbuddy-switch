/**
 * Desktop command and event contract.
 *
 * Mirrors the `invoke_handler` and `emit` call sites in
 * `crates/agent-studio-desktop/src/{lib,rail_settings,session,hit_test}.rs`.
 * Commands are invoked as `plugin:agent-studio|<name>`.
 */
import type { CustomAction, CustomIntegrations, CustomPreview, CustomPreviewRequest } from './custom-integrations.js';
import type { IntegrationAction, Integrations } from './integrations.js';
import type { RailPreferences, RailPreferencesState, Settings } from './settings.js';
import type { ConnectionState, Snapshot } from './snapshot.js';

export type DesktopView = 'rail' | 'settings';

export type CollectorRequestCommand =
  | 'settings_get' | 'settings_set' | 'settings_check'
  | 'integrations_get' | 'integrations_set'
  | 'custom_integrations_get' | 'custom_integrations_set' | 'custom_preview';

/**
 * A clickable region in logical window points, origin at the window's top-left.
 * `hit_test::Region` rejects non-finite values and non-positive sizes, and caps
 * the list at 512 entries.
 */
export interface HitRegion {
  x: number;
  y: number;
  width: number;
  height: number;
  /** Drives the macOS cursor: `grab` shows an open hand. */
  cursor?: 'pointer' | 'grab';
}

export interface DesktopCommandMap {
  monitor_state: {
    args: Record<string, never>;
    result: { snapshot: Snapshot | null; connected: boolean };
  };
  collector_request: {
    args: { command: CollectorRequestCommand; payload: Settings | IntegrationAction | CustomAction | CustomPreviewRequest | null };
    result: Settings | Integrations | CustomIntegrations | CustomPreview;
  };
  set_hit_regions: {
    args: { regions: HitRegion[] };
    result: null;
  };
  rail_settings_get: {
    args: Record<string, never>;
    result: RailPreferencesState;
  };
  rail_settings_set: {
    args: { preferences: RailPreferences; autostart: boolean };
    result: RailPreferencesState;
  };
  close_settings: {
    args: Record<string, never>;
    result: null;
  };
  open_view: {
    args: { view: DesktopView };
    result: null;
  };
  open_session_url: {
    args: { url: string };
    result: null;
  };
  open_integration_folder: {
    args: { source: string; location: string };
    result: null;
  };
}

export type DesktopCommand = keyof DesktopCommandMap;

/** Payload shapes as emitted by the plugin. */
export interface DesktopEventMap {
  'monitor-state': Snapshot;
  /** The plugin only ever emits `offline`; `connected`/`connecting` come from snapshots. */
  'monitor-connection': Extract<ConnectionState, 'offline'>;
  'monitor-settings': Settings;
  'agent-studio-rail-settings': RailPreferencesState;
  'agent-studio-window-active': { label: string; active: boolean };
  /** `null` when the pointer left every registered region. */
  'agent-studio-pointer': { x: number; y: number } | null;
  'agent-studio:enabled': boolean;
  'monitor-warning': string;
}

export type DesktopEvent = keyof DesktopEventMap;

/** Tauri rejects with the Rust `Err(String)` value. */
export function errorMessage(error: unknown): string {
  return typeof error === 'string' ? error : error instanceof Error ? error.message : String(error);
}
