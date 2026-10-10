import { describe, expect, it } from "vitest";

import {
  buildSelections,
  isActionable,
  isDirectlySyncable,
  primaryMode,
  shouldDefaultSelect,
  summarySentence,
} from "./session-link-shared";
import type { SessionLinkPreviewGroup, SessionSyncMode } from "@/lib/types";

/**
 * 「关联会话」共享勾选逻辑的纯函数单测（#143）。
 *
 * 只覆盖不依赖 DOM 的纯逻辑：可勾选判定、提交组装、模式优先级与摘要文案。
 */

function group(
  overrides: Partial<SessionLinkPreviewGroup> & {
    availableModes?: SessionSyncMode[];
  } = {},
): SessionLinkPreviewGroup {
  return {
    groupId: "g-1",
    title: "标题一",
    cwd: "/ws/a",
    verdict: "identical",
    extraA: 0,
    extraB: 0,
    common: 2,
    defaultChecked: false,
    availableModes: [],
    reason: "无需同步",
    recordCount: { source: 2, target: 2, baseline: 2 },
    source: null,
    target: null,
    ...overrides,
  };
}

describe("isActionable", () => {
  it("有正文模式且有凭据时可勾选", () => {
    expect(isActionable(group({ availableModes: ["fastForward"], previewToken: "t" }))).toBe(true);
  });

  it("只归档项（模式为空 + archiveAction）有凭据时可勾选", () => {
    expect(
      isActionable(group({ archiveAction: "statusOnly", previewToken: "t" })),
    ).toBe(true);
  });

  it("只归档项没有凭据时不可勾选", () => {
    expect(isActionable(group({ archiveAction: "statusOnly" }))).toBe(false);
  });

  it("既无正文模式也无 archiveAction 时不可勾选", () => {
    expect(isActionable(group({ previewToken: "t" }))).toBe(false);
  });

  it("正文模式存在但没有凭据时不可勾选", () => {
    expect(isActionable(group({ availableModes: ["overwrite"] }))).toBe(false);
  });
});

describe("buildSelections", () => {
  it("正文模式优先：不会误选 statusOnly", () => {
    const target = group({
      availableModes: ["fastForward"],
      archiveAction: "statusOnly",
      previewToken: "t",
    });
    expect(buildSelections([target], new Set(["g-1"]))).toEqual([
      { groupId: "g-1", previewToken: "t", mode: "fastForward" },
    ]);
  });

  it("只归档项被勾选时提交 statusOnly", () => {
    const target = group({ archiveAction: "statusOnly", previewToken: "t" });
    expect(buildSelections([target], new Set(["g-1"]))).toEqual([
      { groupId: "g-1", previewToken: "t", mode: "statusOnly" },
    ]);
  });

  it("未勾选则不产生提交", () => {
    const target = group({ archiveAction: "statusOnly", previewToken: "t" });
    expect(buildSelections([target], new Set())).toEqual([]);
  });

  it("不可勾选的项即使被勾也不提交", () => {
    const target = group({ availableModes: [], previewToken: "t" });
    expect(buildSelections([target], new Set(["g-1"]))).toEqual([]);
  });
});

describe("模式优先级与全选口径不变", () => {
  it("primaryMode 在模式为空时仍返回 null", () => {
    expect(primaryMode(group({ archiveAction: "statusOnly" }))).toBeNull();
    expect(primaryMode(group({ availableModes: ["overwrite"] }))).toBe("overwrite");
  });

  it("全选仍只作用于 fastForward", () => {
    expect(isDirectlySyncable(group({ availableModes: ["fastForward"], previewToken: "t" }))).toBe(
      true,
    );
    expect(
      isDirectlySyncable(group({ availableModes: [], archiveAction: "statusOnly", previewToken: "t" })),
    ).toBe(false);
    expect(isDirectlySyncable(group({ availableModes: ["overwrite"], previewToken: "t" }))).toBe(
      false,
    );
  });
});

describe("summarySentence", () => {
  it("只归档（identical）说明仅同步归档、保留正文", () => {
    expect(summarySentence(group({ verdict: "identical", archiveAction: "statusOnly" }), "账号B")).toBe(
      "仅同步归档，保留正文",
    );
  });

  it("只归档（ahead）同样说明仅同步归档，不再是「本次不同步」", () => {
    expect(summarySentence(group({ verdict: "ahead", archiveAction: "statusOnly" }), "账号B")).toBe(
      "仅同步归档，保留正文",
    );
  });

  it("快进附带归档时保留正文句并写明一并归档", () => {
    const sentence = summarySentence(
      group({ verdict: "fastForward", availableModes: ["fastForward"], archiveAction: "statusOnly" }),
      "账号B",
    );
    expect(sentence).toContain("将当前账号的新内容同步到「账号B」");
    expect(sentence).toContain("一并归档");
  });

  it("没有归档动作时文案与旧行为逐字一致", () => {
    expect(summarySentence(group({ verdict: "identical" }), "账号B")).toBe("无需同步");
    expect(summarySentence(group({ verdict: "ahead" }), "账号B")).toBe("保留目标内容，本次不同步");
    expect(
      summarySentence(group({ verdict: "fastForward", availableModes: ["fastForward"] }), "账号B"),
    ).toBe("将当前账号的新内容同步到「账号B」");
    expect(summarySentence(group({ verdict: "unknown" }), "账号B")).toBe(
      "暂时无法确认两边内容，本次不会同步",
    );
  });
});

describe("shouldDefaultSelect (#165 方案 3)", () => {
  it("偏好关闭时：纯归档项默认不勾选", () => {
    const pureArchive = group({
      archiveAction: "statusOnly",
      previewToken: "tok-1",
      availableModes: [],
      defaultChecked: false,
    });
    expect(shouldDefaultSelect(pureArchive, false)).toBe(false);
  });

  it("偏好关闭时：后端显式 defaultChecked（如快进）仍默认勾选", () => {
    const ff = group({
      availableModes: ["fastForward"],
      previewToken: "tok-1",
      defaultChecked: true,
    });
    expect(shouldDefaultSelect(ff, false)).toBe(true);
  });

  it("偏好开启时：纯归档项（无正文模式）自动预选", () => {
    const pureArchive = group({
      archiveAction: "statusOnly",
      previewToken: "tok-1",
      availableModes: [],
      defaultChecked: false,
    });
    expect(shouldDefaultSelect(pureArchive, true)).toBe(true);
  });

  it("偏好开启时：无凭据的纯归档项绝不预选（非 actionable）", () => {
    const unActionable = group({
      archiveAction: "statusOnly",
      previewToken: undefined,
      availableModes: [],
      defaultChecked: false,
    });
    expect(shouldDefaultSelect(unActionable, true)).toBe(false);
  });

  it("防覆盖铁律：Diverge 冲突项附带 statusOnly 时，绝不因偏好自动预选（必须用户手动勾选）", () => {
    const divergeGroup = group({
      verdict: "diverge",
      availableModes: ["overwrite"],
      archiveAction: "statusOnly",
      previewToken: "tok-1",
      defaultChecked: false,
    });
    // primaryMode 不为 null（为 overwrite），严禁自动勾选，避免意外触发正文覆盖
    expect(shouldDefaultSelect(divergeGroup, true)).toBe(false);
  });

  it("偏好开启时：无归档动作的普通未改动项不预选", () => {
    const unchanged = group({
      previewToken: "tok-1",
      availableModes: [],
      defaultChecked: false,
    });
    expect(shouldDefaultSelect(unchanged, true)).toBe(false);
  });

  it("偏好开启时：正文快进项沿用自身 defaultChecked", () => {
    const ffChecked = group({
      availableModes: ["fastForward"],
      previewToken: "tok-1",
      defaultChecked: true,
    });
    const ffUnchecked = group({
      availableModes: ["fastForward"],
      previewToken: "tok-1",
      defaultChecked: false,
    });
    expect(shouldDefaultSelect(ffChecked, true)).toBe(true);
    expect(shouldDefaultSelect(ffUnchecked, true)).toBe(false);
  });
});

