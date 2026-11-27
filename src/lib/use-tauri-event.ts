import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

/**
 * 订阅 Tauri 事件，卸载时安全解绑。
 *
 * `listen` 返回 Promise：若清理先于它 resolve（StrictMode 的「挂载 → 卸载 → 再挂载」），
 * 必须在 resolve 时立刻解绑，否则监听泄漏、同一事件被处理多次。
 * `handler` 用 ref 持有，调用方无需 memoize，也不会因闭包变化重新订阅。
 * `enabled = false`（例如 webui 没有事件通道）时不订阅。
 */
export function useTauriEvent<T = unknown>(
  name: string,
  handler: (payload: T) => void,
  enabled = true,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<T>(name, (event) => handlerRef.current(event.payload))
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // 非 Tauri 环境下 listen 会失败：静默忽略，与原先「无监听」行为一致。
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [name, enabled]);
}
