import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";

type CompanionState = {
  enabled: boolean | null;
  busy: boolean;
  error: string | null;
};

let state: CompanionState = { enabled: null, busy: false, error: null };
const subscribers = new Set<() => void>();
let unlisten: (() => void) | undefined;
let generation = 0;

function publish(next: CompanionState) {
  state = next;
  for (const subscriber of subscribers) subscriber();
}

async function refresh(current = generation) {
  try {
    const enabled = await api.getCompanionEnabled();
    if (current === generation) publish({ ...state, enabled, error: null });
  } catch (error) {
    if (current === generation) publish({ ...state, error: api.asError(error) });
  }
}

function subscribe(subscriber: () => void) {
  subscribers.add(subscriber);
  if (subscribers.size === 1) {
    const current = ++generation;
    void refresh(current);
    void listen<boolean>("agent-studio:enabled", (event) => {
      if (current === generation) publish({ ...state, enabled: event.payload, error: null });
    }).then((stop) => {
      if (current === generation) unlisten = stop;
      else stop();
    }).catch((error) => {
      if (current === generation) publish({ ...state, error: api.asError(error) });
    });
  }
  return () => {
    subscribers.delete(subscriber);
    if (subscribers.size === 0) {
      generation += 1;
      unlisten?.();
      unlisten = undefined;
    }
  };
}

function getSnapshot() {
  return state;
}

/** Shared host-backed state for the sidebar and settings page. */
export function useCompanionEnabled(): CompanionState {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export async function reloadCompanionEnabled() {
  await refresh();
}

export async function changeCompanionEnabled(enabled: boolean): Promise<boolean> {
  if (state.busy || state.enabled === null) throw new Error("悬浮窗状态尚未就绪");
  publish({ ...state, busy: true, error: null });
  try {
    const confirmed = await api.setCompanionEnabled(enabled);
    publish({ enabled: confirmed, busy: false, error: null });
    return confirmed;
  } catch (error) {
    publish({ ...state, busy: false, error: api.asError(error) });
    throw error;
  }
}
