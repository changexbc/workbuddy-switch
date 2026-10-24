/**
 * 全站图表共享的数据色板。各板块按系列顺序取色，避免单个看板另开一套色系；
 * 具体色值由浅色 / 深色主题 token 提供。
 */
export const DATA_SERIES_COLORS = [
  "var(--data-series-emerald)",
  "var(--data-series-teal)",
  "var(--data-series-violet)",
  "var(--data-series-amber)",
  "var(--data-series-rose)",
  "var(--data-series-indigo)",
  "var(--data-series-sky)",
  "var(--data-series-lime)",
  "var(--data-series-orange)",
  "var(--data-series-pink)",
  "var(--data-series-cyan)",
  "var(--data-series-slate)",
] as const;

/** 中性色（色板末位的 slate）：用于「未归属」「其他」等没有稳定身份的系列。 */
export const DATA_SERIES_NEUTRAL_COLOR = DATA_SERIES_COLORS[DATA_SERIES_COLORS.length - 1];

/** 按系列顺序取色；超出色板长度时循环，保证颜色可预期。 */
export function dataSeriesColor(index: number): string {
  return DATA_SERIES_COLORS[index % DATA_SERIES_COLORS.length];
}
