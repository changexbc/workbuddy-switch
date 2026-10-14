import { useCallback, useEffect, useState } from "react";
import { displayName } from "@/lib/account-display";
import type { AccountMeta, CreditExpiry } from "@/lib/types";

/**
 * 账号列表排序方式。
 *
 * 语义：
 * - `creditPriority`（默认）：沿用积分紧迫优先 —— 临期账号排最前，其次已过期，
 *   再次正常，查询失败的排最后。积分尚未全部加载完成时不重排（按入库顺序）。
 * - `lastUsed` / `remaining`：按对应值降序，缺值（从未切换 / 积分未加载）排最后。
 * - `createdAt`：新添加的在前。
 * - `name`：按卡片显示名升序。
 *
 * 持久化在 localStorage（与 `wb-switch.compact` / `wb-switch.tools.<id>` 同风格），
 * 键名 `wb-switch.accounts.sort`，缺省或异常值按 `creditPriority` 处理。
 */
export type AccountSortKey =
  | "creditPriority"
  | "lastUsed"
  | "remaining"
  | "createdAt"
  | "name";

export const ACCOUNT_SORT_OPTIONS: Array<{ value: AccountSortKey; label: string }> = [
  { value: "creditPriority", label: "积分紧迫优先" },
  { value: "lastUsed", label: "最后使用时间" },
  { value: "remaining", label: "剩余积分（多→少）" },
  { value: "createdAt", label: "添加时间（新→旧）" },
  { value: "name", label: "名称 A→Z" },
];

const SORT_STORAGE_KEY = "wb-switch.accounts.sort";
/** 同页面内的变更通知（`storage` 事件只在其它标签页触发）。 */
const ACCOUNT_SORT_CHANGED_EVENT = "wb-switch:account-sort-changed";

const SORT_VALUES: AccountSortKey[] = ACCOUNT_SORT_OPTIONS.map((option) => option.value);

export function getAccountSortKey(): AccountSortKey {
  try {
    const stored = localStorage.getItem(SORT_STORAGE_KEY);
    return stored && SORT_VALUES.includes(stored as AccountSortKey)
      ? (stored as AccountSortKey)
      : "creditPriority";
  } catch {
    return "creditPriority";
  }
}

export function setAccountSortKey(key: AccountSortKey): void {
  try {
    localStorage.setItem(SORT_STORAGE_KEY, key);
  } catch {
    // 存储不可用（隐私模式等）：不调 broadcast，否则订阅方会读回旧值把本次选择冲掉。
    return;
  }
  window.dispatchEvent(new Event(ACCOUNT_SORT_CHANGED_EVENT));
}

export function useAccountSortKey(): [AccountSortKey, (key: AccountSortKey) => void] {
  const [sortKey, setLocalKey] = useState<AccountSortKey>(getAccountSortKey);
  useEffect(() => {
    const sync = () => setLocalKey(getAccountSortKey());
    window.addEventListener("storage", sync);
    window.addEventListener(ACCOUNT_SORT_CHANGED_EVENT, sync);
    return () => {
      window.removeEventListener("storage", sync);
      window.removeEventListener(ACCOUNT_SORT_CHANGED_EVENT, sync);
    };
  }, []);
  // 先更新本地 state 再落盘：存储不可用（隐私模式）时列表照样按选择重排。
  const update = useCallback((key: AccountSortKey) => {
    setLocalKey(key);
    setAccountSortKey(key);
  }, []);
  return [sortKey, update];
}

export function expiringSoonAmount(credit?: CreditExpiry): number {
  return credit?.ok ? credit.expiringSoonRemaining ?? 0 : 0;
}

export function hasExpiringSoonCredits(credit?: CreditExpiry): boolean {
  return credit?.ok === true && expiringSoonAmount(credit) > 0;
}

export function soonestRelevantExpiry(credit?: CreditExpiry): number {
  const soonestExpiringCredit = (credit?.resources ?? [])
    .filter((resource) => resource.remaining > 0 && resource.expiringSoon && resource.expireAt != null)
    .map((resource) => resource.expireAt as number)
    .reduce((soonest, expireAt) => Math.min(soonest, expireAt), Number.POSITIVE_INFINITY);
  return Number.isFinite(soonestExpiringCredit)
    ? soonestExpiringCredit
    : credit?.soonestExpireAt ?? Number.POSITIVE_INFINITY;
}

export function creditPriorityRank(credit?: CreditExpiry): number {
  if (!credit?.ok) return 3;
  if (hasExpiringSoonCredits(credit)) return 0;
  if (credit.expired) return 1;
  return 2;
}

/** 剩余积分总量：查询失败或未返回数字（企业版不限量）视为无值，排最后。 */
function remainingCredits(credit?: CreditExpiry): number | null {
  if (!credit?.ok) return null;
  const value = credit.totalRemaining;
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

interface OrderInput {
  accounts: AccountMeta[];
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
  sortKey: AccountSortKey;
}

/**
 * 按排序方式返回账号列表。
 *
 * 所有排序都以入参顺序作末位 tiebreaker，保证同值账号不会来回跳动。
 */
export function orderAccounts({
  accounts,
  creditMap,
  creditLoadingMap,
  sortKey,
}: OrderInput): AccountMeta[] {
  const indexed = accounts.map((account, index) => ({ account, index }));

  if (sortKey === "creditPriority") {
    // 积分没齐时不重排：否则每加载完一个账号，列表就抖一次。
    const ready =
      accounts.length > 0 &&
      accounts.every((account) => Boolean(creditMap[account.id]) && !creditLoadingMap[account.id]);
    if (!ready) return accounts;
    return indexed
      .sort((left, right) => {
        const leftCredit = creditMap[left.account.id];
        const rightCredit = creditMap[right.account.id];
        const rankDifference = creditPriorityRank(leftCredit) - creditPriorityRank(rightCredit);
        if (rankDifference !== 0) return rankDifference;

        const leftExpiry = soonestRelevantExpiry(leftCredit);
        const rightExpiry = soonestRelevantExpiry(rightCredit);
        if (leftExpiry !== rightExpiry) return leftExpiry - rightExpiry;

        const amountDifference = expiringSoonAmount(rightCredit) - expiringSoonAmount(leftCredit);
        if (amountDifference !== 0) return amountDifference;
        return left.index - right.index;
      })
      .map(({ account }) => account);
  }

  if (sortKey === "name") {
    return indexed
      .sort(
        (left, right) =>
          displayName(left.account).localeCompare(displayName(right.account), "zh-Hans-CN") ||
          left.index - right.index,
      )
      .map(({ account }) => account);
  }

  // 其余维度都是「值大者在前，无值者垫底」。
  const valueOf = (account: AccountMeta): number | null => {
    if (sortKey === "lastUsed") return account.lastUsedAt ?? null;
    if (sortKey === "createdAt") return account.createdAt ?? null;
    return remainingCredits(creditMap[account.id]);
  };
  return indexed
    .sort((left, right) => {
      const leftValue = valueOf(left.account);
      const rightValue = valueOf(right.account);
      if (leftValue === rightValue) return left.index - right.index;
      if (leftValue === null) return 1;
      if (rightValue === null) return -1;
      return rightValue - leftValue;
    })
    .map(({ account }) => account);
}

/** 最近一次被切换的账号 id；没有任何切换记录时返回 undefined。 */
export function mostRecentUsedAccountId(accounts: AccountMeta[]): string | undefined {
  let bestId: string | undefined;
  let bestAt = 0;
  for (const account of accounts) {
    const at = account.lastUsedAt ?? 0;
    if (at > bestAt) {
      bestAt = at;
      bestId = account.id;
    }
  }
  return bestId;
}

/** 「最后使用」来源端标识 → 界面短名（取值见 Rust `account::USED_CLIENT_*`）。 */
const USED_CLIENT_LABELS: Record<string, string> = {
  workbuddy: "WorkBuddy",
  codebuddyCli: "CodeBuddy CLI",
  codebuddyIde: "CodeBuddy IDE",
  vscodeExt: "VS Code 插件",
  jetbrains: "JetBrains 插件",
};

export function usedClientLabel(client?: string | null): string | null {
  if (!client) return null;
  return USED_CLIENT_LABELS[client] ?? client;
}

/** 相对时间：分钟 → 小时 → 天，超过 30 天给日期。 */
export function formatRelativeTime(ts: number, now: number): string {
  const elapsed = now - ts;
  if (elapsed < 60_000) return "刚刚";
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days <= 30) return `${days} 天前`;
  const date = new Date(ts);
  return `${String(date.getMonth() + 1).padStart(2, "0")}/${String(date.getDate()).padStart(2, "0")}`;
}
