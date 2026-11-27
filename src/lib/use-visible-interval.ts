import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";

/**
 * 仅在主窗口可见时按间隔执行回调；隐藏时暂停，恢复可见时立即执行一次。
 *
 * 可见性来源与 `use-credit-auto-refresh` 一致：浏览器 `visibilitychange` +
 * Tauri `main-window-visible`（后者覆盖 Tauri 窗口的显示/隐藏不触发前者的情形）。
 *
 * 轻量模式会销毁主窗口（前端随之卸载），本 hook 的定时器在卸载时清理；退出轻量模式后
 * 窗口重建、重新挂载时会立即执行一次，因此无需额外处理窗口销毁场景。
 *
 * `callback` 用 ref 持有：调用方每次渲染传入新的闭包（例如最新的账号列表）不会重启定时器，
 * 定时器触发时用的是最新闭包。`enabled` 变化会重启定时器并立即执行一次。
 */
export function useVisibleInterval(callback: () => void, intervalMs: number, enabled = true): void {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;

  useEffect(() => {
    let timer: number | undefined;
    const visible = () => document.visibilityState !== "hidden";
    const stop = () => {
      if (timer !== undefined) {
        window.clearInterval(timer);
        timer = undefined;
      }
    };
    const start = () => {
      stop();
      if (!enabledRef.current || !visible()) return;
      callbackRef.current();
      timer = window.setInterval(() => {
        if (enabledRef.current) callbackRef.current();
      }, intervalMs);
    };

    start();
    const onVisibility = () => {
      if (visible()) start();
      else stop();
    };
    document.addEventListener("visibilitychange", onVisibility);

    let disposed = false;
    let unlisten: (() => void) | undefined;
    if (!api.isWebui()) {
      void listen<boolean>("main-window-visible", (event) => {
        if (event.payload) start();
        else stop();
      }).then((fn) => {
        // 清理先于 resolve 时立即解绑，避免监听泄漏。
        if (disposed) fn();
        else unlisten = fn;
      });
    }

    return () => {
      disposed = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
    };
  }, [intervalMs, enabled]);
}
