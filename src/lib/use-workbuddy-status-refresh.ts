import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import * as api from "@/lib/api";
import { useAccountsStore } from "@/stores/accounts";

export const WORKBUDDY_STATUS_REFRESH_INTERVAL_MS = 60 * 1000;

/** Refresh WorkBuddy status only while the main window is visible and focused. */
export function useWorkbuddyStatusRefresh() {
  const activeRef = useRef(false);
  const timerRef = useRef<number | undefined>(undefined);
  const abortControllerRef = useRef<AbortController | undefined>(undefined);

  useEffect(() => {
    let disposed = false;
    let documentVisible = document.visibilityState !== "hidden";
    const webui = api.isWebui();
    // Tauri emits the startup visibility decision before the WebView may have
    // installed this listener. Keep desktop inactive until isVisible() supplies
    // the authoritative initial value; later transitions arrive via the event.
    let mainWindowVisible = webui;
    let windowFocused = document.hasFocus();

    function stopTimer() {
      if (timerRef.current !== undefined) {
        window.clearInterval(timerRef.current);
        timerRef.current = undefined;
      }
    }

    function refreshStatus() {
      if (disposed || !activeRef.current) return;
      void useAccountsStore.getState().refreshStatus(abortControllerRef.current?.signal);
    }

    function startTimer() {
      stopTimer();
      timerRef.current = window.setInterval(refreshStatus, WORKBUDDY_STATUS_REFRESH_INTERVAL_MS);
    }

    function syncActiveState() {
      const nextActive = documentVisible && mainWindowVisible && windowFocused;
      if (nextActive === activeRef.current) return;

      activeRef.current = nextActive;
      if (nextActive) {
        abortControllerRef.current = new AbortController();
        refreshStatus();
        startTimer();
      } else {
        abortControllerRef.current?.abort();
        abortControllerRef.current = undefined;
        stopTimer();
      }
    }

    const onVisibilityChange = () => {
      documentVisible = document.visibilityState !== "hidden";
      syncActiveState();
    };
    const onFocus = () => {
      windowFocused = true;
      syncActiveState();
    };
    const onBlur = () => {
      windowFocused = false;
      syncActiveState();
    };

    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    syncActiveState();

    let unlisten: (() => void) | undefined;
    if (!webui) {
      void (async () => {
        try {
          const fn = await listen<boolean>("main-window-visible", (event) => {
            mainWindowVisible = event.payload;
            syncActiveState();
          });
          if (disposed) {
            fn();
            return;
          }
          unlisten = fn;

          try {
            mainWindowVisible = await getCurrentWindow().isVisible();
          } catch {
            // Preserve the previous DOM-based behavior if the native query is
            // unavailable; focus is still required before polling can start.
            mainWindowVisible = documentVisible;
          }
          if (disposed) return;
          syncActiveState();
        } catch {
          if (!disposed) {
            mainWindowVisible = documentVisible;
            syncActiveState();
          }
        }
      })();
    }

    return () => {
      disposed = true;
      activeRef.current = false;
      abortControllerRef.current?.abort();
      abortControllerRef.current = undefined;
      stopTimer();
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      unlisten?.();
    };
  }, []);
}
