import type { CreditExpiry } from "@/lib/types";

/** Network responses and cross-process cache events can arrive out of order. */
export function newerCreditResult(previous: CreditExpiry | undefined, incoming: CreditExpiry): CreditExpiry {
  return (previous?.updatedAt ?? 0) > (incoming.updatedAt ?? 0) ? previous! : incoming;
}
