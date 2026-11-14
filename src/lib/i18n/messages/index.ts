import type { Locale } from "../locale";
import { accountsEn } from "./accounts.en";
import { accountsKo } from "./accounts.ko";
import { commonEn } from "./common.en";
import { commonKo } from "./common.ko";
import { dialogsEn } from "./dialogs.en";
import { dialogsKo } from "./dialogs.ko";
import { libEn } from "./lib.en";
import { libKo } from "./lib.ko";
import { navEn } from "./nav.en";
import { navKo } from "./nav.ko";
import { sessionsEn } from "./sessions.en";
import { sessionsKo } from "./sessions.ko";
import { settingsEn } from "./settings.en";
import { settingsKo } from "./settings.ko";
import { statsEn } from "./stats.en";
import { statsKo } from "./stats.ko";

export const en = {
  ...commonEn,
  ...navEn,
  ...accountsEn,
  ...settingsEn,
  ...sessionsEn,
  ...statsEn,
  ...dialogsEn,
  ...libEn,
};

export type MessageKey = keyof typeof en;

export const ko: Partial<Record<MessageKey, string>> = {
  ...commonKo,
  ...navKo,
  ...accountsKo,
  ...settingsKo,
  ...sessionsKo,
  ...statsKo,
  ...dialogsKo,
  ...libKo,
};

export const CATALOGS: Record<Locale, Partial<Record<MessageKey, string>>> = { en, ko };
