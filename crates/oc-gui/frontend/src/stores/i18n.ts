import { en, type I18nKey } from "./locales/en";
import { es } from "./locales/es";
import { ja } from "./locales/ja";
import { zhHans } from "./locales/zhHans";
import { zhHant } from "./locales/zhHant";
import { createSignal } from "solid-js";
import { systemLocale } from "./systemLocale";

export const LOCALES = ["en", "zh-Hans", "zh-Hant", "ja", "es"] as const;
export type LocaleName = (typeof LOCALES)[number];

const LOCALE_STORAGE_KEY = "opencompose.locale";

export const LOCALE_LABELS: Record<LocaleName, string> = {
  en: "English",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  ja: "日本語",
  es: "Español",
};

export type { I18nKey } from "./locales/en";

const dictionaries: Record<LocaleName, Record<I18nKey, string>> = {
  en,
  "zh-Hans": zhHans,
  "zh-Hant": zhHant,
  ja,
  es,
};

function isLocaleName(value: string | null): value is LocaleName {
  return LOCALES.includes(value as LocaleName);
}

function readStoredLocale(): LocaleName | null {
  try {
    if (typeof window === "undefined") return null;
    const stored = window.localStorage.getItem(LOCALE_STORAGE_KEY);
    return isLocaleName(stored) ? stored : null;
  } catch { return null; }
}

const storedLocale = readStoredLocale();
const [locale, setLocaleSignal] = createSignal<LocaleName>(storedLocale ?? systemLocale());
const [followsSystemLocale, setFollowsSystemLocale] = createSignal(storedLocale === null);

function format(key: I18nKey, values?: Record<string, string | number>): string {
  // English is the source of truth; an empty translation falls back with it.
  let text = dictionaries[locale()][key] || en[key];
  if (!values) return text;
  for (const [name, value] of Object.entries(values)) {
    text = text.replaceAll(`{${name}}`, String(value));
  }
  return text;
}

function applyLocale(value: LocaleName) {
  document.documentElement.lang = value;
}

function setLocale(value: LocaleName) {
  setFollowsSystemLocale(false);
  setLocaleSignal(value);
  applyLocale(value);
  try {
    window.localStorage.setItem(LOCALE_STORAGE_KEY, value);
  } catch (_) {}
}

function useSystemLocale() {
  setFollowsSystemLocale(true);
  const value = systemLocale();
  setLocaleSignal(value);
  applyLocale(value);
  try { window.localStorage.setItem(LOCALE_STORAGE_KEY, "system"); } catch {}
}

let observingSystemLocale = false;
function initLocale() {
  applyLocale(locale());
  if (observingSystemLocale || typeof window === "undefined") return;
  window.addEventListener("languagechange", () => {
    if (!followsSystemLocale()) return;
    const value = systemLocale();
    setLocaleSignal(value);
    applyLocale(value);
  });
  observingSystemLocale = true;
}

/** Whether the user has already picked a language on this device. */
function hasStoredLocale(): boolean {
  // No storage means no way to remember the answer, so do not ask: the prompt
  // is a first-run convenience, not a gate.
  if (typeof window === "undefined") return true;
  try {
    if (!window.localStorage) return true;
    const value = window.localStorage.getItem(LOCALE_STORAGE_KEY);
    return value === "system" || isLocaleName(value);
  } catch (_) {
    return true;
  }
}

export const i18n = {
  locale,
  locales: LOCALES,
  labels: LOCALE_LABELS,
  t: format,
  /** Every key in the English dictionary; English is the source of truth. */
  keys: () => Object.keys(en) as I18nKey[],
  /** Whether a locale carries its own translation (tests use this for coverage). */
  has: (value: LocaleName, key: I18nKey) => key in dictionaries[value],
  setLocale,
  followsSystemLocale,
  useSystemLocale,
  initLocale,
  hasStoredLocale,
};
