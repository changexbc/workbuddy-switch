import { DATA_SERIES_NEUTRAL_COLOR, dataSeriesColor } from "./data-series-colors";
import type { TokenStatsAccount, TokenStatsGroup, TokenStatsTotals } from "./types";

export type TokenTrendPoint = TokenStatsGroup & {
  date: string;
  accountTokens?: Record<string, number>;
};

export interface AccountTokenSeries {
  key: string;
  label: string;
  color: string;
  total: number;
}

export const tokenTotal = (value: TokenStatsTotals) => value.input + value.output + value.cacheWrite;

/** Prefix real IDs so the unassigned option can never collide with a stored account. */
export const tokenAccountKey = (id: string | null): string => id === null ? "unattributed" : `account:${id}`;

/**
 * 具名账号按出现顺序取全站共享色板（与积分页的模型堆叠一致）；未归属没有
 * 稳定身份，固定用中性灰，既不占用色板位置，也不会被误认为某个账号。
 */
export function tokenAccountColor(id: string | null, index: number): string {
  return id === null ? DATA_SERIES_NEUTRAL_COLOR : dataSeriesColor(index);
}

/** Account stacks and their legend totals share the exact same date/model filter. */
export function buildAccountTokenTrend(
  accounts: TokenStatsAccount[], model: string, start: string, end: string,
): { points: TokenTrendPoint[]; series: AccountTokenSeries[] } {
  const daily = new Map<string, TokenTrendPoint>();
  const series: AccountTokenSeries[] = [];
  let colorIndex = 0;
  accounts.forEach((account, index) => {
    const key = `account_${index}`;
    const rows = (model === "all" ? account.daily : account.dailyByModel[model] ?? [])
      .filter((point) => point.key >= start && point.key <= end);
    let total = 0;
    for (const row of rows) {
      const point = daily.get(row.key) ?? {
        key: row.key, date: row.key, total: 0, input: 0, output: 0, cacheRead: 0,
        cacheWrite: 0, uncachedInput: 0, records: 0, cacheHitRate: null, accountTokens: {},
      };
      for (const field of ["input", "output", "cacheRead", "cacheWrite", "uncachedInput", "records"] as const) {
        point[field] += row[field];
      }
      point.total += tokenTotal(row);
      point.cacheHitRate = point.input > 0 ? point.cacheRead / point.input : null;
      point.accountTokens![key] = (point.accountTokens![key] ?? 0) + tokenTotal(row);
      total += tokenTotal(row);
      daily.set(row.key, point);
    }
    if (rows.length > 0) {
      series.push({ key, label: account.accountName || account.accountId || "未归属", color: tokenAccountColor(account.accountId, colorIndex), total });
    }
    // 即使账号在当前范围没有数据也占用色位，切换范围 / 模型时颜色不会整体漂移。
    if (account.accountId !== null) colorIndex += 1;
  });
  return { points: [...daily.values()].sort((a, b) => a.key.localeCompare(b.key)), series };
}
