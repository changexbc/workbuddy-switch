import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  formatRelativeTime,
  getAccountSortKey,
  mostRecentUsedAccountId,
  orderAccounts,
  setAccountSortKey,
  usedClientLabel,
  type AccountSortKey,
} from "./account-order";
import type { AccountMeta, CreditExpiry, CreditResource } from "./types";

function account(id: string, extra: Partial<AccountMeta> = {}): AccountMeta {
  return {
    id,
    uid: `uid-${id}`,
    email: `${id}@example.com`,
    nickname: id,
    enterpriseName: null,
    expiresAt: null,
    refreshExpiresAt: null,
    refreshedAt: null,
    createdAt: null,
    needsRelogin: false,
    needsReloginReason: null,
    ...extra,
  };
}

function credit(extra: Partial<CreditExpiry> = {}): CreditExpiry {
  return { ok: true, ...extra };
}

function resource(extra: Partial<CreditResource> = {}): CreditResource {
  return {
    packageCode: null,
    packageName: null,
    total: 100,
    remaining: 100,
    used: 0,
    status: null,
    expireAt: null,
    expired: false,
    expiringSoon: false,
    ...extra,
  };
}

describe("账号列表排序", () => {
  const accounts = [account("a"), account("b"), account("c")];

  it("积分紧迫优先：积分未齐时保持入库顺序，齐了才重排", () => {
    const partial = { a: credit({ totalRemaining: 100 }) };
    expect(
      orderAccounts({
        accounts,
        creditMap: partial,
        creditLoadingMap: {},
        sortKey: "creditPriority",
      }).map((a) => a.id),
    ).toEqual(["a", "b", "c"]);

    const full = {
      a: credit({
        expiringSoonRemaining: 50,
        resources: [resource({ remaining: 50, expiringSoon: true, expireAt: 200 })],
      }),
      b: credit({ totalRemaining: 900 }),
      c: credit({ expired: true, totalRemaining: 0 }),
    };
    expect(
      orderAccounts({ accounts, creditMap: full, creditLoadingMap: {}, sortKey: "creditPriority" }).map(
        (a) => a.id,
      ),
    ).toEqual(["a", "c", "b"]);
  });

  it("积分仍在查询中时不重排", () => {
    expect(
      orderAccounts({
        accounts,
        creditMap: { a: credit(), b: credit(), c: credit() },
        creditLoadingMap: { c: true },
        sortKey: "creditPriority",
      }).map((a) => a.id),
    ).toEqual(["a", "b", "c"]);
  });

  it("剩余积分：多的在前，查询失败与企业版不限量的垫底", () => {
    const creditMap = {
      a: credit({ totalRemaining: 10 }),
      b: credit({ totalRemaining: null, unlimited: true }),
      c: credit({ totalRemaining: 500 }),
    };
    const loading = { a: false, b: false, c: false };
    expect(
      orderAccounts({ accounts, creditMap, creditLoadingMap: loading, sortKey: "remaining" }).map(
        (a) => a.id,
      ),
    ).toEqual(["c", "a", "b"]);
  });

  it("最后使用时间：新的在前，从未切换的按入库顺序垫底", () => {
    const withUsage = [
      account("a", { lastUsedAt: 100 }),
      account("b", { lastUsedAt: null }),
      account("c", { lastUsedAt: 900 }),
    ];
    expect(
      orderAccounts({
        accounts: withUsage,
        creditMap: {},
        creditLoadingMap: {},
        sortKey: "lastUsed",
      }).map((a) => a.id),
    ).toEqual(["c", "a", "b"]);
  });

  it("添加时间：新的在前", () => {
    const byCreated = [
      account("a", { createdAt: 5 }),
      account("b", { createdAt: 50 }),
      account("c", { createdAt: null }),
    ];
    expect(
      orderAccounts({
        accounts: byCreated,
        creditMap: {},
        creditLoadingMap: {},
        sortKey: "createdAt",
      }).map((a) => a.id),
    ).toEqual(["b", "a", "c"]);
  });

  it("名称：中文按拼音在前、英文名在后，备注偏好参与取名", () => {
    const byName = [
      account("a", { nickname: "小明" }),
      account("b", { nickname: "Alice" }),
      account("c", { nickname: "阿强" }),
      account("d", { nickname: "Bob", note: "Zoe", displayField: "note" }),
    ];
    expect(
      orderAccounts({ accounts: byName, creditMap: {}, creditLoadingMap: {}, sortKey: "name" }).map(
        (a) => a.id,
      ),
    ).toEqual(["c", "a", "b", "d"]);
  });

  it("同值账号维持入库顺序，列表不会来回跳动", () => {
    const same = [account("a", { lastUsedAt: 7 }), account("b", { lastUsedAt: 7 }), account("c", { lastUsedAt: 7 })];
    expect(
      orderAccounts({ accounts: same, creditMap: {}, creditLoadingMap: {}, sortKey: "lastUsed" }).map(
        (a) => a.id,
      ),
    ).toEqual(["a", "b", "c"]);
  });

  it("最近切换标记：取最大时间戳的账号，全无记录时不标记", () => {
    expect(
      mostRecentUsedAccountId([
        account("a", { lastUsedAt: 10 }),
        account("b", { lastUsedAt: 20 }),
        account("c", {}),
      ]),
    ).toBe("b");
    expect(mostRecentUsedAccountId([account("a"), account("b")])).toBeUndefined();
  });

  it("来源端标识转短名，未知标识原样展示", () => {
    expect(usedClientLabel("workbuddy")).toBe("WorkBuddy");
    expect(usedClientLabel("vscodeExt")).toBe("VS Code 插件");
    expect(usedClientLabel(null)).toBeNull();
    expect(usedClientLabel("futureClient")).toBe("futureClient");
  });

  it("相对时间按分钟 / 小时 / 天递进，超过 30 天给日期", () => {
    const now = Date.UTC(2026, 9, 14, 12, 0, 0);
    expect(formatRelativeTime(now - 30_000, now)).toBe("刚刚");
    expect(formatRelativeTime(now - 5 * 60_000, now)).toBe("5 分钟前");
    expect(formatRelativeTime(now - 3 * 3_600_000, now)).toBe("3 小时前");
    expect(formatRelativeTime(now - 4 * 86_400_000, now)).toBe("4 天前");
    expect(formatRelativeTime(now - 90 * 86_400_000, now)).toBe("07/16");
  });
});

describe("排序偏好持久化", () => {
  const store = new Map<string, string>();
  const originalLocalStorage = globalThis.localStorage;
  const originalWindow = globalThis.window;

  beforeEach(() => {
    store.clear();
    Object.defineProperty(globalThis, "localStorage", {
      value: {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => {
          store.set(key, String(value));
        },
      },
      writable: true,
      configurable: true,
    });
    Object.defineProperty(globalThis, "window", {
      value: new EventTarget(),
      writable: true,
      configurable: true,
    });
  });

  afterEach(() => {
    Object.defineProperty(globalThis, "localStorage", {
      value: originalLocalStorage,
      writable: true,
      configurable: true,
    });
    Object.defineProperty(globalThis, "window", {
      value: originalWindow,
      writable: true,
      configurable: true,
    });
  });

  it("缺省与非法值都回落到积分紧迫优先", () => {
    expect(getAccountSortKey()).toBe("creditPriority");
    store.set("wb-switch.accounts.sort", "nonsense");
    expect(getAccountSortKey()).toBe("creditPriority");
  });

  it("写入后可读回，并广播同页面变更事件", () => {
    const handler = vi.fn();
    globalThis.window.addEventListener("wb-switch:account-sort-changed", handler);
    try {
      setAccountSortKey("lastUsed" satisfies AccountSortKey);
      expect(getAccountSortKey()).toBe("lastUsed");
      expect(handler).toHaveBeenCalledTimes(1);
    } finally {
      globalThis.window.removeEventListener("wb-switch:account-sort-changed", handler);
    }
  });

  it("存储不可用时读取回落默认值、写入不抛出", () => {
    Object.defineProperty(globalThis, "localStorage", {
      value: {
        getItem: () => {
          throw new Error("SecurityError: Access is denied");
        },
        setItem: () => {
          throw new Error("SecurityError: Access is denied");
        },
      },
      writable: true,
      configurable: true,
    });
    expect(getAccountSortKey()).toBe("creditPriority");
    expect(() => setAccountSortKey("name")).not.toThrow();
  });
});
