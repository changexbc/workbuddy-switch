import { useEffect, useState } from "react";

/**
 * 「支持工具」开关：控制各客户端入口是否在界面上出现。
 *
 * 语义（与设置页文案一致）：
 * - 关闭某工具 = 该工具的入口（账号卡片按钮、页顶状态徽标）不渲染，相关状态
 *   轮询也跳过；**不影响账号库与其它工具**，重新打开即恢复。
 * - 默认值：现有的四个端默认开启；JetBrains 端默认关闭（作者要求的灰度策略）。
 *
 * 持久化在 localStorage（与 `wb-switch.compact` / `wb-switch.theme` 同风格），
 * 键名 `wb-switch.tools.<id>`，值 `"1"` / `"0"`；缺省时取 `defaultEnabled`。
 */
export type ToolId = "workbuddy" | "codebuddyIde" | "codebuddyCli" | "vscodeExt" | "jetbrains";

export interface ToolDef {
  id: ToolId;
  /** 设置页开关标题。 */
  label: string;
  /** 设置页说明文案。 */
  description: string;
  /** 未显式配置时的默认状态。 */
  defaultEnabled: boolean;
}

export const SUPPORTED_TOOLS: ToolDef[] = [
  {
    id: "workbuddy",
    label: "WorkBuddy",
    description: "WorkBuddy 桌面客户端账号切换与会话复制",
    defaultEnabled: true,
  },
  {
    id: "codebuddyIde",
    label: "CodeBuddy IDE",
    description: "CodeBuddy IDE 桌面客户端（国内版 / 国际版）账号切换",
    defaultEnabled: true,
  },
  {
    id: "codebuddyCli",
    label: "CodeBuddy CLI",
    description: "CodeBuddy CLI 默认账号与自动轮换",
    defaultEnabled: true,
  },
  {
    id: "vscodeExt",
    label: "VS Code CodeBuddy 插件",
    description: "VS Code 内 CodeBuddy 插件账号切换与会话复制",
    defaultEnabled: true,
  },
  {
    id: "jetbrains",
    label: "JetBrains IDE 插件（IDEA / PyCharm）",
    description: "IntelliJ IDEA / PyCharm 内 CodeBuddy 插件账号切换",
    defaultEnabled: false,
  },
];

function storageKey(id: ToolId): string {
  return `wb-switch.tools.${id}`;
}

/** 同页面内的变更通知（`storage` 事件只在其它标签页触发）。 */
const TOOLS_CHANGED_EVENT = "wb-switch:tools-changed";

export function isToolEnabled(id: ToolId): boolean {
  const def = SUPPORTED_TOOLS.find((t) => t.id === id);
  const fallback = def?.defaultEnabled ?? true;
  try {
    const stored = localStorage.getItem(storageKey(id));
    if (stored === null) return fallback;
    return stored === "1";
  } catch {
    return fallback;
  }
}

export function setToolEnabled(id: ToolId, enabled: boolean): void {
  try {
    localStorage.setItem(storageKey(id), enabled ? "1" : "0");
  } catch {
    /* 隐私模式等场景下写入失败：开关本次会话内仍可见（由调用方 state 驱动） */
  }
  window.dispatchEvent(new Event(TOOLS_CHANGED_EVENT));
}

function readAll(): Record<ToolId, boolean> {
  return SUPPORTED_TOOLS.reduce(
    (acc, tool) => {
      acc[tool.id] = isToolEnabled(tool.id);
      return acc;
    },
    {} as Record<ToolId, boolean>,
  );
}

/** 订阅「支持工具」开关；设置页改动后（同一标签页）其余页面立即同步。 */
export function useSupportedTools(): Record<ToolId, boolean> {
  const [state, setState] = useState<Record<ToolId, boolean>>(readAll);
  useEffect(() => {
    const sync = () => setState(readAll());
    window.addEventListener("storage", sync);
    window.addEventListener(TOOLS_CHANGED_EVENT, sync);
    return () => {
      window.removeEventListener("storage", sync);
      window.removeEventListener(TOOLS_CHANGED_EVENT, sync);
    };
  }, []);
  return state;
}
