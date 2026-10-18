import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";
import type { CreditExpiry } from "@/lib/types";
import { newerCreditResult } from "@/lib/newer-credit-result";
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
    let unlistenCache: (() => void) | undefined;
    let disposed = false;
    if (!api.isWebui()) {
      void listen<Record<string, CreditExpiry>>("credit-query-cache-updated", ({ payload }) => {
        const state = useAccountsStore.getState();
        const creditMap = { ...state.creditMap };
        const creditUpdatedAtMap = { ...state.creditUpdatedAtMap };
        for (const account of state.accounts) {
          const result = payload[account.id];
          if (!result?.ok || (result.updatedAt ?? 0) <= (creditMap[account.id]?.updatedAt ?? 0)) continue;
          creditMap[account.id] = newerCreditResult(creditMap[account.id], result);
          creditUpdatedAtMap[account.id] = result.updatedAt ?? Date.now();
        }
        useAccountsStore.setState({ creditMap, creditUpdatedAtMap });
      }).then((fn) => {
        if (disposed) fn();
        else unlistenCache = fn;
      });
      void listen<boolean>("main-window-visible", (event) => {
        setVisible(event.payload);
      }).then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      });
    }

    return () => {
      disposed = true;
      stopTimer();
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
      unlistenCache?.();
    };
  }, []);
}
