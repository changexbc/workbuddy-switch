import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import * as api from "@/lib/api";
import { translate } from "@/lib/i18n/translate";

/** 后端 `rotate-deferred` 事件负载（与 core 的 `notify` 字段同形）。 */
type RotateDeferredNotice = { title?: string; body?: string };

/**
 * 轮换因「有 CLI 会话在运行」被推迟时的应用内提示。
 *
 * 系统通知是尽力而为：开发态下它会被登记到「终端」名下，且投递失败无法从插件拿到
 * （`show()` 恒返回 Ok）。所以窗口开着时以这条 toast 为准；webui 没有事件通道，
 * 由 `runRotate()` 的返回值承载（见 SettingsPage 的手动检查）。
 */
export function useRotateDeferredNotice() {
  useEffect(() => {
    if (api.isWebui()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<RotateDeferredNotice>("rotate-deferred", (event) => {
      const body = event.payload?.body?.trim();
      if (!body) return;
      toast.warning(translate("lib.rotate.deferredTitle"), { description: body, duration: 10_000 });
    }).then((fn) => {
      // StrictMode 开发态会「挂载 → 卸载 → 再挂载」：`listen` 的 Promise 在清理之后才
      // resolve，不补这一步就会残留一个监听，同一事件弹两条 toast。
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
