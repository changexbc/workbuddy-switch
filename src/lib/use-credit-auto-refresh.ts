import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";
import { useAccountsStore } from "@/stores/accounts";

/** Keep credits fresh while the main window is visible; pause when it is hidden. */
export const CREDIT_REFRESH_INTERVAL_MS = 30 * 60 * 1000;

function currentAccountIds(): string[] {
  return useAccountsStore.getState().accounts.map((account) => account.id);
}

function refreshOnShow() {
  const ids = currentAccountIds();
  if (ids.length === 0) return;
  const last = useAccountsStore.getState().lastCreditRefreshAt;
  if (last === 0) {
    void useAccountsStore.getState().ensureCredits(ids);
    return;
  }
  if (Date.now() - last >= CREDIT_REFRESH_INTERVAL_MS) {
    void useAccountsStore.getState().refreshCredits(ids, { silent: true });
  }
}

export function useCreditAutoRefresh() {
  const visibleRef = useRef(document.visibilityState !== "hidden");
  const timerRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    function stopTimer() {
      if (timerRef.current !== undefined) {
        window.clearInterval(timerRef.current);
        timerRef.current = undefined;
      }
    }

    function startTimer() {
      stopTimer();
      if (!visibleRef.current) return;
      timerRef.current = window.setInterval(() => {
        if (!visibleRef.current) return;
        const ids = currentAccountIds();
        if (ids.length === 0) return;
        void useAccountsStore.getState().refreshCredits(ids, { silent: true });
      }, CREDIT_REFRESH_INTERVAL_MS);
    }

    function setVisible(next: boolean) {
      visibleRef.current = next;
      if (next) {
        refreshOnShow();
        startTimer();
      } else {
        stopTimer();
      }
    }

    visibleRef.current = document.visibilityState !== "hidden";
    if (visibleRef.current) {
      refreshOnShow();
      startTimer();
    }

    const onVisibility = () => {
      setVisible(document.visibilityState !== "hidden");
    };
    document.addEventListener("visibilitychange", onVisibility);

    let unlisten: (() => void) | undefined;
    if (!api.isWebui()) {
      void listen<boolean>("main-window-visible", (event) => {
        setVisible(event.payload);
      }).then((fn) => {
        unlisten = fn;
      });
    }

    return () => {
      stopTimer();
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
    };
  }, []);
}
