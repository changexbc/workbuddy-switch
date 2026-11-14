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
import { CATALOGS, en, type MessageKey } from "./messages";

export type TranslateVars = Record<string, string | number>;

export type Translate = (key: MessageKey, vars?: TranslateVars) => string;

function interpolate(template: string, vars?: TranslateVars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name) =>
    Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : match,
  );
}

interface I18nValue {
  locale: Locale;
  t: Translate;
  setLocale: (locale: Locale) => void;
}

const I18nContext = createContext<I18nValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(getLocale);

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  useEffect(() => subscribeLocale(() => setLocaleState(getLocale())), []);

  const setLocale = useCallback((next: Locale) => {
    setLocalePreference(next);
    setLocaleState(next);
  }, []);

  const t = useCallback<Translate>(
    (key, vars) => interpolate(CATALOGS[locale][key] ?? en[key], vars),
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
