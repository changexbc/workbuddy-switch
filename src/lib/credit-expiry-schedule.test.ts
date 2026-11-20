import { describe, expect, it } from "vitest";

import { buildCreditExpirySchedule, type CreditExpirySchedule } from "./credit-expiry-schedule";
import type { CreditResource } from "./types";

/** 资源包工厂：只写关心的字段，其余填合法默认值，避免用例被无关字段噪声淹没。 */
function resource(overrides: Partial<CreditResource> = {}): CreditResource {
  return {
    packageCode: "TCACA_code_009_0XmEQc2xOf",
    packageName: "购买积分",
    total: 100,
    remaining: 40,
    used: 60,
    status: 1,
    expireAt: null,
    expired: false,
    expiringSoon: false,
    ...overrides,
  };
}

/**
 * 本地时间构造。全部断言都基于本地日历日，禁止用 `toISOString()`，
 * 否则在非 UTC 时区或夏令时下会整体偏移一天，测试就变成依赖运行机时区了。
 */
function localTs(
  year: number,
  month: number,
  day: number,
  hour = 0,
  minute = 0,
  second = 0,
  ms = 0,
): number {
  return new Date(year, month - 1, day, hour, minute, second, ms).getTime();
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

function localKey(year: number, month: number, day: number): string {
  return `${year}-${pad(month)}-${pad(day)}`;
}

/** 本地日历日 ±N 天后的 key（用 setDate 递增，与实现口径一致）。 */
function shiftedKey(baseTs: number, offset: number): string {
  const date = new Date(baseTs);
  date.setDate(date.getDate() + offset);
  return localKey(date.getFullYear(), date.getMonth() + 1, date.getDate());
}

function findDay(schedule: CreditExpirySchedule, date: string) {
  return schedule.days.find((point) => point.date === date);
}

// 全部用例共用的「现在」：2024-05-10 10:30 本地时间
const NOW = localTs(2024, 5, 10, 10, 30);
const TODAY = localKey(2024, 5, 10);

describe("buildCreditExpirySchedule 按到期日归集", () => {
  it("同一天多个资源包求和：40.5 + 10 = 50.5，包数 2，不四舍五入", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 40.5, expireAt: localTs(2024, 5, 12, 9, 0) }),
        resource({ remaining: 10, expireAt: localTs(2024, 5, 12, 20, 0) }),
      ],
      30,
      NOW,
    );

    const day = findDay(schedule, localKey(2024, 5, 12));
    expect(day).toEqual({ date: localKey(2024, 5, 12), credits: 50.5, packages: 2 });
    expect(schedule.total).toBe(50.5);
    expect(schedule.packageCount).toBe(2);
  });

  it("跨账号/跨资源包合并：只按日期归集，不区分商品码与包名", () => {
    const expireAt = localTs(2024, 5, 15, 12, 0);
    const schedule = buildCreditExpirySchedule(
      [
        resource({ packageCode: "TCACA_code_003_FAnt7lcmRT", packageName: "个人标准版", remaining: 1.5, expireAt }),
        resource({ packageCode: "TCACA_code_007_nzdH5h4Nl0", packageName: "平台奖励积分", remaining: 2.25, expireAt }),
        resource({ packageCode: null, packageName: null, remaining: 6, expireAt: localTs(2024, 5, 15, 23, 59) }),
      ],
      30,
      NOW,
    );

    expect(findDay(schedule, localKey(2024, 5, 15))).toEqual({
      date: localKey(2024, 5, 15),
      credits: 9.75,
      packages: 3,
    });
    expect(schedule.days.filter((point) => point.credits > 0)).toHaveLength(1);
    expect(schedule.packageCount).toBe(3);
  });

  it("不同到期日分别落到各自的日期，互不串味", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 5, expireAt: localTs(2024, 5, 11, 1, 0) }),
        resource({ remaining: 7, expireAt: localTs(2024, 5, 11, 23, 0) }),
        resource({ remaining: 3, expireAt: localTs(2024, 5, 20, 8, 0) }),
      ],
      30,
      NOW,
    );

    expect(findDay(schedule, TODAY)?.credits).toBe(0);
    expect(findDay(schedule, localKey(2024, 5, 11))?.credits).toBe(12);
    expect(findDay(schedule, localKey(2024, 5, 20))?.credits).toBe(3);
    expect(schedule.total).toBe(15);
    expect(schedule.packageCount).toBe(3);
  });
});

describe("buildCreditExpirySchedule 过滤规则", () => {
  it("remaining 为 0 或负数不计入，也不参与 nextExpireAt", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 0, expireAt: localTs(2024, 5, 11, 9, 0) }),
        resource({ remaining: -25, expireAt: localTs(2024, 5, 11, 9, 0) }),
        resource({ remaining: 12, expireAt: localTs(2024, 5, 13, 9, 0) }),
      ],
      30,
      NOW,
    );

    expect(findDay(schedule, localKey(2024, 5, 11))).toEqual({
      date: localKey(2024, 5, 11),
      credits: 0,
      packages: 0,
    });
    expect(schedule.total).toBe(12);
    expect(schedule.packageCount).toBe(1);
    expect(schedule.nextExpireAt).toBe(localTs(2024, 5, 13, 9, 0));
  });

  it("expired=true 的资源包即使未来到期也不计入", () => {
    const schedule = buildCreditExpirySchedule(
      [resource({ remaining: 33, expireAt: localTs(2024, 5, 14, 9, 0), expired: true })],
      30,
      NOW,
    );

    expect(schedule.days.every((point) => point.credits === 0 && point.packages === 0)).toBe(true);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBeNull();
  });

  it("expireAt 为 null 或非有限值不计入", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 10, expireAt: null }),
        resource({ remaining: 11, expireAt: Number.NaN }),
        resource({ remaining: 12, expireAt: Number.POSITIVE_INFINITY }),
      ],
      30,
      NOW,
    );

    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBeNull();
  });

  it("已过期（expireAt < now）不计入，且不污染 nextExpireAt", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 99, expireAt: NOW - 1 }),
        resource({ remaining: 99, expireAt: localTs(2024, 5, 9, 23, 59, 59) }),
        resource({ remaining: 4, expireAt: localTs(2024, 5, 18, 9, 0) }),
      ],
      30,
      NOW,
    );

    expect(schedule.total).toBe(4);
    expect(schedule.packageCount).toBe(1);
    expect(schedule.nextExpireAt).toBe(localTs(2024, 5, 18, 9, 0));
  });

  it("remaining 为 NaN 不计入", () => {
    const schedule = buildCreditExpirySchedule(
      [resource({ remaining: Number.NaN, expireAt: localTs(2024, 5, 12, 9, 0) })],
      30,
      NOW,
    );

    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBeNull();
  });

  it("全部资源包被过滤时 total=0、packageCount=0、nextExpireAt=null", () => {
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 0, expireAt: localTs(2024, 5, 12, 9, 0) }),
        resource({ remaining: -1, expireAt: localTs(2024, 5, 12, 9, 0) }),
        resource({ remaining: 5, expireAt: null }),
        resource({ remaining: 6, expireAt: localTs(2024, 5, 12, 9, 0), expired: true }),
        resource({ remaining: 7, expireAt: NOW - 1000 }),
      ],
      7,
      NOW,
    );

    expect(schedule.days).toHaveLength(7);
    expect(schedule.days.every((point) => point.credits === 0 && point.packages === 0)).toBe(true);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBeNull();
  });
});

describe("buildCreditExpirySchedule 窗口零填充", () => {
  it("rangeDays=7 返回从本地今天起的 7 个连续日期，无到期日为 0", () => {
    const schedule = buildCreditExpirySchedule(
      [resource({ remaining: 8, expireAt: localTs(2024, 5, 12, 9, 0) })],
      7,
      NOW,
    );

    expect(schedule.days).toHaveLength(7);
    expect(schedule.days[0].date).toBe(TODAY);
    expect(schedule.days.map((point) => point.date)).toEqual(
      Array.from({ length: 7 }, (_, offset) => shiftedKey(NOW, offset)),
    );
    expect(schedule.days.map((point) => point.credits)).toEqual([0, 0, 8, 0, 0, 0, 0]);
    expect(schedule.days.map((point) => point.packages)).toEqual([0, 0, 1, 0, 0, 0, 0]);
  });

  it("窗口外的到期包不计入 total，但作为未来最早到期被 nextExpireAt 取到", () => {
    const outsideA = localTs(2024, 5, 20, 9, 0);
    const outsideB = localTs(2024, 5, 25, 9, 0);
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 50, expireAt: outsideA }),
        resource({ remaining: 60, expireAt: outsideB }),
      ],
      7,
      NOW,
    );

    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBe(outsideA);
  });

  it("窗口内也有到期时，nextExpireAt 取最早的那一天（窗口内优先）", () => {
    const inWindow = localTs(2024, 5, 14, 9, 0);
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 50, expireAt: localTs(2024, 5, 20, 9, 0) }),
        resource({ remaining: 20, expireAt: inWindow }),
      ],
      7,
      NOW,
    );

    expect(schedule.total).toBe(20);
    expect(schedule.nextExpireAt).toBe(inWindow);
  });
});

describe("buildCreditExpirySchedule 边界", () => {
  it("expireAt === now 视为未到期，归入今天", () => {
    const schedule = buildCreditExpirySchedule([resource({ remaining: 9, expireAt: NOW })], 7, NOW);

    expect(findDay(schedule, TODAY)).toEqual({ date: TODAY, credits: 9, packages: 1 });
    expect(schedule.total).toBe(9);
    expect(schedule.nextExpireAt).toBe(NOW);
  });

  it("窗口最后一天 23:59:59 计入，次日 00:00:00 不计入", () => {
    const lastDay = shiftedKey(NOW, 2);
    const schedule = buildCreditExpirySchedule(
      [
        resource({ remaining: 10, expireAt: localTs(2024, 5, 12, 23, 59, 59) }),
        resource({ remaining: 20, expireAt: localTs(2024, 5, 13, 0, 0, 0) }),
      ],
      3,
      NOW,
    );

    expect(lastDay).toBe(localKey(2024, 5, 12));
    expect(findDay(schedule, lastDay)?.credits).toBe(10);
    expect(schedule.days.map((point) => point.date)).toEqual([
      TODAY,
      localKey(2024, 5, 11),
      localKey(2024, 5, 12),
    ]);
    expect(schedule.total).toBe(10);
    expect(schedule.packageCount).toBe(1);
    // nextExpireAt 取全部未来资源包里最早的一个：23:59:59 早于次日 00:00:00
    expect(schedule.nextExpireAt).toBe(localTs(2024, 5, 12, 23, 59, 59));
  });

  it("rangeDays=0 时 days 为空、total=0，但 nextExpireAt 仍正确", () => {
    const next = localTs(2024, 6, 1, 9, 0);
    const schedule = buildCreditExpirySchedule([resource({ remaining: 30, expireAt: next })], 0, NOW);

    expect(schedule.days).toEqual([]);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBe(next);
  });

  it("rangeDays 为负数时按 0 处理", () => {
    const schedule = buildCreditExpirySchedule([resource({ remaining: 5, expireAt: localTs(2024, 5, 12) })], -7, NOW);

    expect(schedule.days).toEqual([]);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBe(localTs(2024, 5, 12));
  });

  it("rangeDays 为小数时向下取整", () => {
    const schedule = buildCreditExpirySchedule([], 7.9, NOW);

    expect(schedule.days).toHaveLength(7);
    expect(schedule.days[schedule.days.length - 1].date).toBe(shiftedKey(NOW, 6));
  });

  it("rangeDays 为 NaN 时按 0 处理，不抛异常", () => {
    const schedule = buildCreditExpirySchedule([resource({ remaining: 5, expireAt: localTs(2024, 5, 12) })], Number.NaN, NOW);

    expect(schedule.days).toEqual([]);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBe(localTs(2024, 5, 12));
  });

  it("resources 为空数组时返回零填充窗口", () => {
    const schedule = buildCreditExpirySchedule([], 30, NOW);

    expect(schedule.days).toHaveLength(30);
    expect(schedule.days.every((point) => point.credits === 0 && point.packages === 0)).toBe(true);
    expect(schedule.total).toBe(0);
    expect(schedule.packageCount).toBe(0);
    expect(schedule.nextExpireAt).toBeNull();
  });
});

describe("buildCreditExpirySchedule 跨月与跨日连续", () => {
  it("跨月末时日期连续、无重复无跳过（1 月 31 日起 5 天）", () => {
    const now = localTs(2024, 1, 31, 8, 0);
    const schedule = buildCreditExpirySchedule(
      [resource({ remaining: 6, expireAt: localTs(2024, 2, 2, 9, 0) })],
      5,
      now,
    );

    expect(schedule.days.map((point) => point.date)).toEqual([
      localKey(2024, 1, 31),
      localKey(2024, 2, 1),
      localKey(2024, 2, 2),
      localKey(2024, 2, 3),
      localKey(2024, 2, 4),
    ]);
    expect(new Set(schedule.days.map((point) => point.date)).size).toBe(5);
    expect(findDay(schedule, localKey(2024, 2, 2))?.credits).toBe(6);
  });

  it("闰年 2 月 29 日不跳过（2 月 28 日起 3 天）", () => {
    const now = localTs(2024, 2, 28, 8, 0);
    const schedule = buildCreditExpirySchedule([], 3, now);

    expect(schedule.days.map((point) => point.date)).toEqual([
      localKey(2024, 2, 28),
      localKey(2024, 2, 29),
      localKey(2024, 3, 1),
    ]);
  });

  it("每个日期都是可回读的本地日历日 key（不受 UTC 偏移影响）", () => {
    const schedule = buildCreditExpirySchedule([], 30, NOW);

    for (const point of schedule.days) {
      expect(point.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      const [year, month, day] = point.date.split("-").map(Number);
      const parsed = new Date(year, month - 1, day);
      expect(parsed.getFullYear()).toBe(year);
      expect(parsed.getMonth() + 1).toBe(month);
      expect(parsed.getDate()).toBe(day);
      // 本地零点回读同一天，说明 key 是按本地日历日切分的
      expect(parsed.getHours()).toBe(0);
    }
  });

  it("到期时间按本地日历日归属：23:30 到期不落到次日", () => {
    const schedule = buildCreditExpirySchedule(
      [resource({ remaining: 7, expireAt: localTs(2024, 5, 11, 23, 30) })],
      7,
      NOW,
    );

    expect(findDay(schedule, localKey(2024, 5, 11))?.credits).toBe(7);
    expect(findDay(schedule, localKey(2024, 5, 12))?.credits).toBe(0);
  });
});

describe("buildCreditExpirySchedule 无关字段与确定性", () => {
  it("used/total/status/expiringSoon 等无关字段不影响聚合", () => {
    const expireAt = localTs(2024, 5, 16, 9, 0);
    const variants: CreditResource[] = [
      resource({ remaining: 15, expireAt, used: 85, total: 100, status: 1, expiringSoon: false }),
      resource({
        remaining: 15,
        expireAt,
        used: 0,
        total: 15,
        status: null,
        expiringSoon: true,
        packageName: "另一个名字",
        packageCode: null,
      }),
    ];

    const [first, second] = variants.map((item) => buildCreditExpirySchedule([item], 30, NOW));

    expect(first.days).toEqual(second.days);
    expect(first.total).toBe(15);
    expect(second.total).toBe(15);
    expect(first.packageCount).toBe(1);
    expect(first.nextExpireAt).toBe(expireAt);
    expect(second.nextExpireAt).toBe(expireAt);
  });

  it("额外后端字段（如 updatedAt）不影响聚合结果", () => {
    const expireAt = localTs(2024, 5, 17, 9, 0);
    const withExtras = {
      ...resource({ remaining: 21, expireAt }),
      updatedAt: 1_700_000_000_000,
      accountId: "acct-1",
    } as CreditResource;

    const schedule = buildCreditExpirySchedule([withExtras], 30, NOW);

    expect(schedule.total).toBe(21);
    expect(schedule.packageCount).toBe(1);
    expect(schedule.nextExpireAt).toBe(expireAt);
  });

  it("now 决定窗口起点：窗口外不计入、窗口内计入，nextExpireAt 始终指向该包", () => {
    const expireAt = localTs(2024, 5, 20, 9, 0);
    // 窗口 05-10 ~ 05-16：到期日 05-20 在窗口外
    const early = buildCreditExpirySchedule([resource({ remaining: 4, expireAt })], 7, localTs(2024, 5, 10));
    // 窗口 05-18 ~ 05-24：同一个到期日落到窗口内
    const late = buildCreditExpirySchedule([resource({ remaining: 4, expireAt })], 7, localTs(2024, 5, 18));

    expect(early.days[0].date).toBe(localKey(2024, 5, 10));
    expect(late.days[0].date).toBe(localKey(2024, 5, 18));
    expect(early.total).toBe(0);
    expect(late.total).toBe(4);
    expect(early.nextExpireAt).toBe(expireAt);
    expect(late.nextExpireAt).toBe(expireAt);

    const insideWindow = buildCreditExpirySchedule(
      [resource({ remaining: 4, expireAt })],
      7,
      localTs(2024, 5, 19),
    );
    expect(insideWindow.total).toBe(4);
    expect(findDay(insideWindow, localKey(2024, 5, 20))?.credits).toBe(4);
  });

  it("同一入参多次调用结果稳定（纯函数、无副作用）", () => {
    const resources = [
      resource({ remaining: 2.5, expireAt: localTs(2024, 5, 11, 1, 0) }),
      resource({ remaining: 3.5, expireAt: localTs(2024, 5, 11, 2, 0) }),
    ];

    const first = buildCreditExpirySchedule(resources, 30, NOW);
    const second = buildCreditExpirySchedule(resources, 30, NOW);

    expect(second).toEqual(first);
    // 不修改入参
    expect(resources[0].remaining).toBe(2.5);
    expect(resources[1].expireAt).toBe(localTs(2024, 5, 11, 2, 0));
  });
});
