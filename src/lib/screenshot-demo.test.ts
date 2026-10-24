import { describe, expect, it } from "vitest";

import { screenshotDemoResponse } from "./screenshot-demo";
import type { AccountMeta, CreditExpiry, CreditStatistics, TokenStatistics } from "./types";

/**
 * 企业版 demo 数据（issue #167）：
 * - C 为企业版有限配额、D 为企业版不限量，卡片数据与后端企业形态一致；
 * - 个人版账号 mock 不回归；
 * - 统计页 mock 覆盖全部演示账号（`usageSeeds` / `creditPackages` 与 `accounts` 同索引，
 *   新增账号后索引错位会在这里直接抛错）。
 */

function creditExpiry(accountId: string): CreditExpiry {
  return screenshotDemoResponse("get_credit_expiry", { accountId }) as CreditExpiry;
}

describe("企业版 demo 数据", () => {
  it("demo-account-c：企业版有限配额（单条「企业配额」）", () => {
    const credit = creditExpiry("demo-account-c");
    expect(credit.ok).toBe(true);
    expect(credit.unlimited).toBe(false);
    expect(credit.totalRemaining).toBe(1500);
    expect(credit.resources).toHaveLength(1);
    expect(credit.resources?.[0].packageName).toBe("企业配额");
  });

  it("demo-account-d：企业版不限量（数字字段为 null、无资源）", () => {
    const credit = creditExpiry("demo-account-d");
    expect(credit.ok).toBe(true);
    expect(credit.unlimited).toBe(true);
    expect(credit.totalRemaining).toBeNull();
    expect(credit.totalCapacity).toBeNull();
    expect(credit.resources).toEqual([]);
  });

  it("个人版账号 mock 不回归（不携带 unlimited 标记）", () => {
    const credit = creditExpiry("demo-account-a");
    expect(credit.unlimited).toBeUndefined();
    expect(credit.resources).toHaveLength(1);
  });

  it("统计页 mock 覆盖全部演示账号", () => {
    const stats = screenshotDemoResponse("get_credit_statistics") as CreditStatistics;
    expect(stats.accounts).toHaveLength(6);
    const enterprise = stats.accounts.find((row) => row.accountId === "demo-account-d");
    expect(enterprise).toBeDefined();
    expect(enterprise?.currentRemaining).toBeNull();
  });

  it("账号列表对企业版下发 isEnterprise，个人版不下发", () => {
    const payload = screenshotDemoResponse("get_accounts") as { accounts: AccountMeta[] };
    expect(payload.accounts).toHaveLength(6);
    expect(payload.accounts.find((row) => row.id === "demo-account-c")?.isEnterprise).toBe(true);
    expect(payload.accounts.find((row) => row.id === "demo-account-d")?.isEnterprise).toBe(true);
    expect(payload.accounts.find((row) => row.id === "demo-account-a")?.isEnterprise).toBeUndefined();
  });
});

describe("账号 Token 演示数据", () => {
  it("四个来源均提供独立账号、未归属和逐模型汇总，数字与原统计守恒", () => {
    const stats = screenshotDemoResponse("get_token_statistics") as TokenStatistics;
    expect(stats.sources).toHaveLength(4);
    for (const source of stats.sources) {
      expect(source.accounts).toHaveLength(3);
      expect(source.accounts?.filter((account) => account.accountId === null)).toHaveLength(1);
      for (const field of ["total", "input", "output", "cacheRead", "cacheWrite", "uncachedInput", "records"] as const) {
        expect(source.accounts!.reduce((sum, account) => sum + account.summary[field], 0)).toBe(source.summary[field]);
        for (const [model, points] of Object.entries(source.dailyByModel!)) {
          for (const point of points) {
            const total = source.accounts!.reduce((sum, account) => sum + (account.dailyByModel[model].find((row) => row.key === point.key)?.[field] ?? 0), 0);
            expect(total).toBe(point[field]);
          }
        }
      }
    }
  });
});
