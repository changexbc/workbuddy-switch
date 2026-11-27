export const creditsFormatter = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 });

export function formatCredits(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return creditsFormatter.format(value);
}
