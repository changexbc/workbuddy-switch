import { describe, expect, it } from "vitest";
import { newerCreditResult } from "./newer-credit-result";

describe("credits received from concurrent GUI/CLI queries", () => {
  it("keeps a newer CLI observation when an older GUI request fails later", () => {
    const fresh = { ok: true, totalRemaining: 80, updatedAt: 200 };
    expect(newerCreditResult(fresh, { ok: false, error: "old request failed", updatedAt: 100 })).toBe(fresh);
  });
  it("accepts newer balances and initializes an empty card", () => {
    const fresh = { ok: true, totalRemaining: 50, updatedAt: 300 };
    expect(newerCreditResult({ ok: true, totalRemaining: 80, updatedAt: 200 }, fresh)).toBe(fresh);
    expect(newerCreditResult(undefined, fresh)).toBe(fresh);
  });
});
