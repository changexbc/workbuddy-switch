import { describe, expect, it } from "vitest";

import { en, ko } from "./messages";

function placeholders(template: string): string[] {
  return [...template.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
}

describe("message catalogs", () => {
  it("translates every English key into Korean", () => {
    const missing = Object.keys(en).filter((key) => !ko[key as keyof typeof ko]);
    expect(missing).toEqual([]);
  });

  it("defines no Korean key that English lacks", () => {
    const extra = Object.keys(ko).filter((key) => !(key in en));
    expect(extra).toEqual([]);
  });

  it("keeps the same placeholders in both locales", () => {
    const mismatched = Object.entries(en).filter(([key, english]) => {
      const korean = ko[key as keyof typeof ko];
      return korean !== undefined && placeholders(english).join() !== placeholders(korean).join();
    });
    expect(mismatched.map(([key]) => key)).toEqual([]);
  });
});
