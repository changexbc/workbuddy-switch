import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/lib/i18n/translate";
import {
  DEFAULT_VARIANT,
  accountVariant,
  normalizeVariant,
  variantAppName,
  variantCodebuddyIdeName,
  variantDownloadDomain,
  variantIsIntl,
  variantLabel,
  variantSupportsCheckin,
  variantSupportsTravel,
  variantUsesIntlCodebuddyIde,
} from "./variant";

describe("normalizeVariant", () => {
  it("保留已知档位 cn 与 ai", () => {
    expect(normalizeVariant("cn")).toBe("cn");
    expect(normalizeVariant("ai")).toBe("ai");
  });

  it("未知字符串一律回落国内版", () => {
    expect(normalizeVariant("intl")).toBe("cn");
    expect(normalizeVariant("AI")).toBe("cn");
    expect(normalizeVariant("cn ")).toBe("cn");
  });

  it("缺省、空值与非法类型回落国内版", () => {
    expect(normalizeVariant(undefined)).toBe("cn");
    expect(normalizeVariant(null)).toBe("cn");
    expect(normalizeVariant("")).toBe("cn");
    expect(normalizeVariant(0)).toBe("cn");
    expect(normalizeVariant({ variant: "ai" })).toBe("cn");
  });

  it("缺省常量本身是国内版", () => {
    expect(DEFAULT_VARIANT).toBe("cn");
  });
});

describe("档位文案映射", () => {
  afterEach(() => setActiveLocale("en"));

  it("档位名跟随当前语言", () => {
    expect(variantLabel("cn")).toBe("Domestic");
    expect(variantLabel("ai")).toBe("International");
    setActiveLocale("ko");
    expect(variantLabel("cn")).toBe("국내판");
    expect(variantLabel("ai")).toBe("국제판");
  });

  it("客户端名：国际版带后缀", () => {
    expect(variantAppName("cn")).toBe("WorkBuddy");
    expect(variantAppName("ai")).toBe("WorkBuddy International");
  });

  it("CodeBuddy IDE 名：国际版带后缀", () => {
    expect(variantCodebuddyIdeName("cn")).toBe("CodeBuddy IDE");
    expect(variantCodebuddyIdeName("ai")).toBe("CodeBuddy IDE International");
  });

  it("下载域名：国内版 codebuddy.cn / 国际版 workbuddy.ai", () => {
    expect(variantDownloadDomain("cn")).toBe("codebuddy.cn");
    expect(variantDownloadDomain("ai")).toBe("workbuddy.ai");
  });
});

describe("accountVariant", () => {
  it("账号为 null / undefined / 无档位字段时按国内版", () => {
    expect(accountVariant(null)).toBe("cn");
    expect(accountVariant(undefined)).toBe("cn");
    expect(accountVariant({})).toBe("cn");
  });

  it("读取账号自身档位", () => {
    expect(accountVariant({ variant: "ai" })).toBe("ai");
    expect(accountVariant({ variant: "cn" })).toBe("cn");
  });
});

describe("档位能力开关", () => {
  it("成长中心仅国内版开放", () => {
    expect(variantSupportsTravel("cn")).toBe(true);
    expect(variantSupportsTravel("ai")).toBe(false);
  });

  it("自动签到仅国内版开放", () => {
    expect(variantSupportsCheckin("cn")).toBe(true);
    expect(variantSupportsCheckin("ai")).toBe(false);
  });

  it("只有国际版切 CodeBuddy.app", () => {
    expect(variantUsesIntlCodebuddyIde("cn")).toBe(false);
    expect(variantUsesIntlCodebuddyIde("ai")).toBe(true);
  });

  it("档位标记：只有国际版算 INTL", () => {
    expect(variantIsIntl("cn")).toBe(false);
    expect(variantIsIntl("ai")).toBe(true);
  });
});
