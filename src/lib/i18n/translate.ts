import { DEFAULT_LOCALE, getLocale, type Locale } from "./locale";
import { CATALOGS, en, type MessageKey } from "./messages";

export type TranslateVars = Record<string, string | number>;

export type Translate = (key: MessageKey, vars?: TranslateVars) => string;

function interpolate(template: string, vars?: TranslateVars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name) =>
    Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : match,
  );
}

let activeLocale: Locale = DEFAULT_LOCALE;

export function setActiveLocale(locale: Locale): void {
  activeLocale = locale;
}

export function getActiveLocale(): Locale {
  return activeLocale;
}

export function translateWith(locale: Locale, key: MessageKey, vars?: TranslateVars): string {
  return interpolate(CATALOGS[locale][key] ?? en[key], vars);
}

export function translate(key: MessageKey, vars?: TranslateVars): string {
  return translateWith(activeLocale, key, vars);
}

export function syncActiveLocaleFromStorage(): Locale {
  activeLocale = getLocale();
  return activeLocale;
}
