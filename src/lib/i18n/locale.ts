export type Locale = "en" | "ko";

export const DEFAULT_LOCALE: Locale = "en";

const LOCALE_STORAGE_KEY = "wb-switch.locale";
const LOCALE_CHANGED_EVENT = "wb-switch:locale-changed";

export const LOCALE_OPTIONS: { value: Locale; label: string }[] = [
  { value: "en", label: "English" },
  { value: "ko", label: "한국어" },
];

export function isLocale(value: unknown): value is Locale {
  return value === "en" || value === "ko";
}

export function getLocale(): Locale {
  try {
    const stored = localStorage.getItem(LOCALE_STORAGE_KEY);
    if (isLocale(stored)) return stored;
  } catch {
    // Storage unavailable: the default locale still renders.
  }
  return DEFAULT_LOCALE;
}

export function setLocalePreference(locale: Locale): void {
  try {
    localStorage.setItem(LOCALE_STORAGE_KEY, locale);
  } catch {
    // Storage unavailable: the caller's state still drives this session.
  }
  window.dispatchEvent(new Event(LOCALE_CHANGED_EVENT));
}

export function subscribeLocale(listener: () => void): () => void {
  window.addEventListener(LOCALE_CHANGED_EVENT, listener);
  window.addEventListener("storage", listener);
  return () => {
    window.removeEventListener(LOCALE_CHANGED_EVENT, listener);
    window.removeEventListener("storage", listener);
  };
}
