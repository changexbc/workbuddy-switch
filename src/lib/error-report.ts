import { toast } from "sonner";

import * as api from "./api";
import type { ErrorLogKind } from "./types";

declare global {
  interface Window {
    /**
     * 首屏挂载标记：`src/main.tsx` 在 render 之后置位；`index.html` 的静态兜底脚本
     * 据此判断入口 JS 是否已接管（10 秒仍未置位 → 展示「应用启动失败」提示）。
     */
    __WB_MOUNTED__?: boolean;
  }
}

/** 同一 message + source 在 30 秒内只上报 / 提示一次，避免崩溃循环把日志与提示刷屏。 */
const DEDUPE_WINDOW_MS = 30_000;
/** toast 描述里展示的错误摘要上限；完整内容进错误日志与错误页。 */
const TOAST_SUMMARY_MAX = 120;
/** 去重表上限：超出后顺带清理过期项，避免长时间运行无限增长。 */
const DEDUPE_MAX_ENTRIES = 50;

/**
 * 桌面端才会把错误写入 `~/.wb-switch/error.log`。
 * webui / 演示模式没有落盘通道。
 */
export function canPersistErrorLog(): boolean {
  return !api.isDemoMode() && !api.isWebui();
}

const lastReportedAt = new Map<string, number>();
let installed = false;

/** 是否应当处理这次错误；同一个 key 在 30 秒窗口内只放行一次。 */
function shouldReport(key: string, now: number): boolean {
  const last = lastReportedAt.get(key);
  if (last !== undefined && now - last < DEDUPE_WINDOW_MS) return false;
  lastReportedAt.set(key, now);
  if (lastReportedAt.size > DEDUPE_MAX_ENTRIES) {
    for (const [entry, at] of lastReportedAt) {
      if (now - at >= DEDUPE_WINDOW_MS) lastReportedAt.delete(entry);
    }
  }
  return true;
}

/** 尽力把任意抛出值转成一行可读摘要（Promise 拒绝的 reason 可能是任意值）。 */
function describe(value: unknown): string {
  if (value instanceof Error) return value.message || value.name || "未知错误";
  if (typeof value === "string") return value.trim() || "未知错误";
  if (value === null || value === undefined) return "未知错误";
  const text = safeJson(value);
  return text && text !== "{}" ? text : String(value);
}

/** 序列化失败（循环引用等）返回空串，绝不因为记日志本身再抛一次。 */
function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "";
  } catch {
    return "";
  }
}

function stackOf(value: unknown): string {
  return value instanceof Error ? (value.stack ?? "") : "";
}

/**
 * 上报一条前端错误：写本地错误日志（桌面端落盘 `~/.wb-switch/error.log`）+ 一条不打断
 * 操作的简短 toast。
 *
 * - 去重键是 `kind | source | message`：同一处相同错误 30 秒内只处理一次。
 * - 不吞错：浏览器对未捕获错误的默认控制台输出保持原样，这里额外打一行带来源的摘要，
 *   便于在 webview 控制台里定位。
 * - 落盘失败 / webui 无通道都静默忽略，上报本身绝不影响主流程。
 */
export function reportError(
  kind: ErrorLogKind,
  message: string,
  options: { detail?: string; source?: string } = {},
): void {
  const text = message.trim() || "未知错误";
  const source = options.source?.trim() || "unknown";
  const detail = options.detail ?? "";
  const now = Date.now();
  if (!shouldReport(`${kind}|${source}|${text}`, now)) return;

  console.error(`[wb-switch] ${kind} (${source}): ${text}`, detail);
  void api.logError(kind, text, detail).catch(() => {});
  // 桌面端文案与设计一致；没有落盘通道时不说「已记录」。
  toast.error(canPersistErrorLog() ? "出现一个错误，已记录" : "出现一个错误", {
    description: text.length > TOAST_SUMMARY_MAX ? `${text.slice(0, TOAST_SUMMARY_MAX)}…` : text,
  });
}

/**
 * 安装全局错误捕获：未捕获异常（`error`）与未处理的 Promise 拒绝（`unhandledrejection`）。
 *
 * 只处理「脚本错误」：资源加载失败（`<img>` / `<script>` 等）不带 message，交给浏览器
 * 默认行为，不计入错误日志。重复调用无副作用（入口只装一次）。
 */
export function installGlobalErrorHandlers(): void {
  if (installed) return;
  installed = true;

  window.addEventListener("error", (event) => {
    if (!event.message) return;
    const where = event.filename ? `${event.filename}:${event.lineno}:${event.colno}` : "";
    reportError("frontend_unhandled", event.message, {
      detail: [stackOf(event.error), where ? `来源: ${where}` : ""].filter(Boolean).join("\n"),
      source: where || "window.error",
    });
  });

  window.addEventListener("unhandledrejection", (event) => {
    reportError("frontend_unhandled", describe(event.reason), {
      detail: stackOf(event.reason) || safeJson(event.reason),
      source: "unhandledrejection",
    });
  });
}
