import { desktopCommand, isDesktop, onRailPreferences } from './host.js';
import type { RailPreferences, RailPreferencesState } from '../types/settings.js';

const key = 'astra.desktop.preferences.v1';

export const defaultPreferences = (): RailPreferences => ({ avatarStyle: 'animal', visibleCount: 8, animation: true });

/**
 * Runtime guard for values arriving from storage, the native command or a form.
 * Callers hold typed data, but JS callers and persisted JSON do not, so the
 * checks stay.
 */
export function validatePreferences(value: RailPreferences): RailPreferences {
  if (!['animal', 'bot'].includes(value.avatarStyle) || !Number.isInteger(value.visibleCount) || value.visibleCount < 3 || value.visibleCount > 16 || typeof value.animation !== 'boolean') throw Error('悬浮窗设置无效');
  return { avatarStyle: value.avatarStyle, visibleCount: value.visibleCount, animation: value.animation };
}

const browserFallback = (preferences: RailPreferences): RailPreferencesState => ({ ...preferences, autostart: false, autostartSupported: false });

export async function loadPreferences(): Promise<RailPreferencesState> {
  if (isDesktop()) return desktopCommand('rail_settings_get');
  const stored: unknown = JSON.parse(localStorage.getItem(key) || 'null');
  return browserFallback(validatePreferences((stored || defaultPreferences()) as RailPreferences));
}

export async function savePreferences(value: RailPreferences & {autostart: boolean}): Promise<RailPreferencesState> {
  const preferences = validatePreferences(value);
  if (isDesktop()) return desktopCommand('rail_settings_set', { preferences, autostart: value.autostart });
  localStorage.setItem(key, JSON.stringify(preferences));
  return browserFallback(preferences);
}

/** Merge only edited fields into a fresh read from the shared settings host. */
export async function savePreferencePatch(patch: Partial<RailPreferences & {autostart: boolean}>): Promise<RailPreferencesState> {
  return savePreferences({...await loadPreferences(), ...patch});
}

export function watchPreferences(callback: (state: RailPreferencesState) => void) {
  const release = onRailPreferences(callback);
  const storage = (event: StorageEvent) => { if (event.key === key) loadPreferences().then(callback).catch(() => {}); };
  window.addEventListener('storage', storage);

  return () => { release(); window.removeEventListener('storage', storage); };
}
