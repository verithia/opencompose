import type { LocaleName } from "./i18n";

/** Negotiate supported UI languages in the order supplied by the OS/WebView. */
export function localeFromLanguages(languages: readonly string[]): LocaleName {
  for (const language of languages) {
    const parts = language.replaceAll("_", "-").toLowerCase().split("-");
    switch (parts[0]) {
      case "zh":
        if (parts.includes("hant")) return "zh-Hant";
        if (parts.includes("hans")) return "zh-Hans";
        return parts.some(part => ["tw", "hk", "mo"].includes(part)) ? "zh-Hant" : "zh-Hans";
      case "en": return "en";
      case "ja": return "ja";
      case "es": return "es";
    }
  }
  return "en";
}

export function systemLocale(): LocaleName {
  try {
    if (typeof navigator === "undefined") return "en";
    const languages = navigator.languages?.length ? navigator.languages : [navigator.language];
    return localeFromLanguages(languages.filter((language): language is string => typeof language === "string"));
  } catch {
    return "en";
  }
}
