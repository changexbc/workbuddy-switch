// WorkBuddy 档位（国内版 / 国际版）的前端单一事实来源：
// 展示名、缺省判定、以及「哪些入口属于哪个档位」的判定都收敛在这里，
// 避免各页面各写一份字符串。

import type { WbVariant } from "./types";

/** 缺省档位：后端不传 / 账号无字段时一律按国内版处理（零回归）。 */
export const DEFAULT_VARIANT: WbVariant = "cn";

/** 宽松解析后端可能缺省或未知的档位取值。 */
export function normalizeVariant(value: unknown): WbVariant {
  return value === "ai" ? "ai" : DEFAULT_VARIANT;
}

/** 档位对应的中文档位名（切换控件、空状态、能力提示用）。 */
export function variantLabel(variant: WbVariant): string {
  return variant === "ai" ? "国际版" : "国内版";
}

/** 档位对应的客户端名称（对话框、状态卡、账号页 tooltip 用）。 */
export function variantAppName(variant: WbVariant): string {
  return variant === "ai" ? "WorkBuddy 国际版" : "WorkBuddy";
}

/** CodeBuddy IDE 的档位展示名（账号页档位标记 tooltip 用），与 WorkBuddy 命名保持一致。 */
export function variantCodebuddyIdeName(variant: WbVariant): string {
  return variant === "ai" ? "CodeBuddy IDE 国际版" : "CodeBuddy IDE";
}

/**
 * 档位对应的客户端下载域名（空状态提示用）。
 *
 * 只给域名字样、不做超链接（design D9）。国内版空状态不含域名，取值仅为让调用方
 * 不必再写档位判断。
 */
export function variantDownloadDomain(variant: WbVariant): string {
  return variant === "ai" ? "workbuddy.ai" : "codebuddy.cn";
}

/** 账号自身档位；账号缺省字段时按国内版处理。 */
export function accountVariant(account: { variant?: WbVariant } | null | undefined): WbVariant {
  return normalizeVariant(account?.variant);
}

/** 成长中心（派猫猫旅行）仅国内版开放；国际版不请求、不展示。 */
export function variantSupportsTravel(variant: WbVariant): boolean {
  return variant !== "ai";
}

/** 自动签到仅国内版开放；国际版签到接口未开放（后端已按 inactive 归类），不展示入口。 */
export function variantSupportsCheckin(variant: WbVariant): boolean {
  return variant !== "ai";
}

/**
 * 每日活跃会话仅国际版开放：走网页通道的 agent 会话，跑完一轮即计分。
 * 国内版走签到，没有这条链路。
 */
export function variantSupportsActivity(variant: WbVariant): boolean {
  return variant === "ai";
}

/** 国际版 Tab 切 CodeBuddy.app；国内版 Tab 仍切 CodeBuddy CN。 */
export function variantUsesIntlCodebuddyIde(variant: WbVariant): boolean {
  return variant === "ai";
}

/** 是否为国际版（档位标记选国际版图标 + INTL 角标时用）。 */
export function variantIsIntl(variant: WbVariant): boolean {
  return variant === "ai";
}
