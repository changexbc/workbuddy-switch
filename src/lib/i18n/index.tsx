import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { getLocale, setLocalePreference, subscribeLocale, type Locale } from "./locale";
import { syncHostLocale } from "./host";
import type { MessageKey } from "./messages";
import { setActiveLocale, translateWith, type Translate, type TranslateVars } from "./translate";

export type { Translate, TranslateVars };

interface I18nValue {
  locale: Locale;
  t: Translate;
  setLocale: (locale: Locale) => void;
}

const I18nContext = createContext<I18nValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(getLocale);

  // Set during render, not in an effect: children format dates through the
  // module-level reader on the same commit that the locale changes, and an
  // effect would leave them one render behind.
  setActiveLocale(locale);

  useEffect(() => {
    document.documentElement.lang = locale;
    syncHostLocale(locale);
  }, [locale]);

  useEffect(() => subscribeLocale(() => setLocaleState(getLocale())), []);

  const setLocale = useCallback((next: Locale) => {
    setLocalePreference(next);
    setLocaleState(next);
  }, []);

  const t = useCallback<Translate>(
    (key: MessageKey, vars?: TranslateVars) => translateWith(locale, key, vars),
    [locale],
  );

  const value = useMemo(() => ({ locale, t, setLocale }), [locale, t, setLocale]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

function useI18nContext(): I18nValue {
  const value = useContext(I18nContext);
  if (!value) throw new Error("I18n hooks require an I18nProvider above them");
  return value;
}

export function useT(): Translate {
  return useI18nContext().t;
}

export function useLocale(): Locale {
  return useI18nContext().locale;
}

export function useSetLocale(): (locale: Locale) => void {
  return useI18nContext().setLocale;
}
