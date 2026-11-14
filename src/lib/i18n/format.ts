import { getActiveLocale } from "./translate";

const INTL_LOCALES = { en: "en-US", ko: "ko-KR" } as const;

/**
 * BCP 47 tag for date and number formatting. The interface copy comes from the
 * catalog, but `Intl` needs a tag, and a hardcoded `zh-CN` would keep Chinese
 * date order and separators in the English and Korean builds.
 */
export function intlLocale(): string {
  return INTL_LOCALES[getActiveLocale()];
}
