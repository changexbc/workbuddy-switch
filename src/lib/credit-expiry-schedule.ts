import type { CreditResource } from "./types";

/**
 * 每日到期积分日程。
 *
 * 资源包各自带到期时间，「剩余积分」会随到期瞬间整体失效，因此按自然日归集比
 * 汇总成一个月度总额更有决策价值：用户能看到「哪一天会掉多少积分」。
 */
export interface CreditExpiryDay {
  /** 本地日期 YYYY-MM-DD */
  date: string;
  /** 当天到期的剩余积分合计（保留原始小数，展示层再格式化） */
  credits: number;
  /** 当天到期的资源包个数 */
  packages: number;
}

export interface CreditExpirySchedule {
  /** 从本地今天起连续 rangeDays 天，没有到期的日期补 0，保证图表 x 轴连续 */
  days: CreditExpiryDay[];
  /** 窗口内到期积分合计 */
  total: number;
  /** 窗口内到期资源包个数 */
  packageCount: number;
  /** 全部未来资源包中最早的到期时间（可超出窗口）；没有则 null */
  nextExpireAt: number | null;
}

/** 本地日历日 key。不能用 toISOString，否则 UTC 与本地时区不一致时会整体偏移一天。 */
function localDateKey(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** 本地当天 00:00 的时间戳；窗口按自然日对齐，而不是按「现在 + N × 24h」。 */
function startOfLocalDay(ts: number): number {
  const date = new Date(ts);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/**
 * 按到期日构建未来 rangeDays 天的积分到期日程。
 *
 * 只统计仍可能损失的资源包：剩余量 > 0、有到期时间、尚未到期（expireAt >= now）、
 * 且未被后端标记为已到期。纯函数，无副作用，`now` 可注入便于测试。
 */
export function buildCreditExpirySchedule(
  resources: readonly CreditResource[],
  rangeDays: number,
  now: number = Date.now(),
): CreditExpirySchedule {
  const normalizedNow = Number.isFinite(now) ? now : Date.now();
  const windowDays = Number.isFinite(rangeDays) ? Math.max(0, Math.floor(rangeDays)) : 0;
  const windowStart = startOfLocalDay(normalizedNow);

  // 逐日 key 用 setDate 递增，避免夏令时下「24h 定长」导致的日期漂移
  const days: CreditExpiryDay[] = [];
  const dayIndexByKey = new Map<string, number>();
  for (let offset = 0; offset < windowDays; offset += 1) {
    const date = new Date(windowStart);
    date.setDate(date.getDate() + offset);
    const key = localDateKey(date);
    dayIndexByKey.set(key, days.length);
    days.push({ date: key, credits: 0, packages: 0 });
  }

  let total = 0;
  let packageCount = 0;
  let nextExpireAt: number | null = null;

  for (const resource of resources) {
    const remaining = resource?.remaining;
    const expireAt = resource?.expireAt;
    if (typeof remaining !== "number" || !Number.isFinite(remaining) || remaining <= 0) continue;
    if (typeof expireAt !== "number" || !Number.isFinite(expireAt)) continue;
    if (resource.expired === true) continue;
    if (expireAt < normalizedNow) continue;

    // 最近到期不受窗口限制，这样窗口内无到期时仍能提示下一次到期时间
    if (nextExpireAt === null || expireAt < nextExpireAt) nextExpireAt = expireAt;

    const index = dayIndexByKey.get(localDateKey(new Date(expireAt)));
    if (index === undefined) continue;

    const day = days[index];
    day.credits += remaining;
    day.packages += 1;
    total += remaining;
    packageCount += 1;
  }

  return { days, total, packageCount, nextExpireAt };
}
