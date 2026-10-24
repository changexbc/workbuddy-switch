import { describe, expect, it } from "vitest";

import { buildAccountTokenTrend, tokenAccountColor, tokenAccountKey, tokenTotal } from "./token-account-stats";
import type { TokenStatsAccount, TokenStatsGroup } from "./types";

function point(key: string, input: number, records = 1): TokenStatsGroup {
  return { key, input, output: input / 10, cacheRead: input / 2, cacheWrite: input / 20,
    uncachedInput: input / 2, total: input * 1.15, records, cacheHitRate: 0.5 };
}

function account(id: string | null, input: number): TokenStatsAccount {
  const row = point("2026-10-24", input);
  return { accountId: id, accountName: id === null ? "未归属" : "同名账号", summary: row,
    daily: [point("2026-09-01", 10_000), row], dailyByModel: { modelA: [point(row.key, input / 2)] } };
}

describe("账号 Token 趋势", () => {
  it("按 ID 区分同名账号，未归属也计入堆叠与占比分母", () => {
    const result = buildAccountTokenTrend([account("a", 100), account("b", 200), account(null, 400)], "all", "2026-10-01", "2026-10-24");
    expect(result.series.map((series) => series.total)).toEqual([115, 230, 460]);
    expect(result.points).toHaveLength(1);
    const row = result.points[0];
    expect(row.accountTokens).toEqual({ account_0: 115, account_1: 230, account_2: 460 });
    expect(tokenTotal(row)).toBe(805);
    expect(row.records).toBe(3);
    expect(row.uncachedInput).toBe(350);
    expect(row.cacheHitRate).toBe(0.5);
    expect(result.series.reduce((sum, item) => sum + item.total / tokenTotal(row), 0)).toBe(1);
  });

  it("模型和日期同时作用于柱图、调用次数和范围图例", () => {
    const result = buildAccountTokenTrend([account("a", 100), account(null, 200)], "modelA", "2026-10-24", "2026-10-24");
    expect(result.points[0].input).toBe(150);
    expect(result.points[0].records).toBe(2);
    expect(result.series.map((item) => item.total)).toEqual([57.5, 115]);
    expect(buildAccountTokenTrend([account("a", 100)], "missing", "2026-10-01", "2026-10-24")).toEqual({ points: [], series: [] });
    expect(buildAccountTokenTrend([account("a", 100)], "all", "2026-10-25", "2026-10-25")).toEqual({ points: [], series: [] });
  });

  it("账号按共享色板顺序取色，未归属固定中性色且不占色位", () => {
    const result = buildAccountTokenTrend(
      [account("a", 100), account(null, 400), account("b", 200)],
      "all",
      "2026-10-24",
      "2026-10-24",
    );
    expect(result.series.map((item) => item.color)).toEqual([
      "var(--data-series-emerald)",
      "var(--data-series-slate)",
      "var(--data-series-teal)",
    ]);
    expect(tokenAccountColor(null, 0)).toBe("var(--data-series-slate)");
    expect(tokenAccountColor("a", 0)).toBe("var(--data-series-emerald)");
    expect(tokenAccountColor("b", 1)).toBe("var(--data-series-teal)");
  });

  it("同一账号在范围 / 模型筛选变化时颜色不变", () => {
    const accounts = [account("a", 100), account("b", 200)];
    const wide = buildAccountTokenTrend(accounts, "all", "2026-09-01", "2026-10-24");
    const narrow = buildAccountTokenTrend(accounts, "modelA", "2026-10-24", "2026-10-24");
    expect(narrow.series.map((item) => item.color)).toEqual(wide.series.map((item) => item.color));
  });

  it("零用量不会产生无效比例或丢失调用", () => {
    const a = account("a", 0);
    const alone = buildAccountTokenTrend([a], "all", "2026-10-24", "2026-10-24");
    expect(alone.points[0].total).toBe(0);
    expect(alone.points[0].records).toBe(1);
    expect(alone.points[0].cacheHitRate).toBeNull();
    expect(tokenAccountKey(null)).not.toBe(tokenAccountKey("unattributed"));
    expect(buildAccountTokenTrend([], "all", "2026-10-01", "2026-10-24")).toEqual({ points: [], series: [] });
  });
});
