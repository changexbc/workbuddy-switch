import type { RailPreferences, SourceId } from '@/types/settings.js';

export type PreferencePatch = Partial<RailPreferences & {autostart: boolean}>;
type ListenerPatch = Partial<Record<SourceId, boolean>>;
export interface SaveState { busy: boolean; error: unknown | null; pending: boolean }

/** A single mutation lane. Failed batches remain pending; edits always win over older batches. */
export function createSettingsAutosave(options: {
  preferences: (patch: PreferencePatch) => Promise<unknown>;
  listening: (patch: ListenerPatch) => Promise<unknown>;
  logWatch: (value: boolean) => Promise<unknown>;
  changed: (state: SaveState) => void;
  listeningSaved: () => void;
}) {
  let preferences: PreferencePatch = {};
  let listening: ListenerPatch = {};
  let logWatch: boolean | undefined;
  let busy = false;
  let error: unknown | null = null;
  const pending = () => !!(Object.keys(preferences).length || Object.keys(listening).length || logWatch !== undefined);
  const publish = () => options.changed({busy, error, pending: pending()});
  async function flush() {
    if (busy || error || !pending()) return;
    busy = true;
    publish();
    try {
      while (pending()) {
        if (Object.keys(listening).length) {
          const batch = listening;
          listening = {};
          try { await options.listening(batch); }
          catch (failure) { listening = {...batch, ...listening}; throw failure; }
          options.listeningSaved();
        }
        if (logWatch !== undefined) {
          const value = logWatch;
          logWatch = undefined;
          try { await options.logWatch(value); }
          catch (failure) { logWatch = value; throw failure; }
        }
        if (Object.keys(preferences).length) {
          const batch = preferences;
          preferences = {};
          try { await options.preferences(batch); }
          catch (failure) { preferences = {...batch, ...preferences}; throw failure; }
        }
      }
    } catch (failure) { error = failure; }
    finally { busy = false; publish(); }
  }
  return {
    editPreferences(patch: PreferencePatch) { preferences = {...preferences, ...patch}; publish(); void flush(); },
    editListening(patch: ListenerPatch) { listening = {...listening, ...patch}; publish(); void flush(); },
    editLogWatch(value: boolean) { logWatch = value; publish(); void flush(); },
    retry() { error = null; void flush(); },
    acquire() { if (busy || pending()) return false; busy = true; publish(); return true; },
    release() { busy = false; publish(); void flush(); },
  };
}
