import { afterEach, expect, it, vi } from "vitest";
import { localeFromLanguages, systemLocale } from "../stores/systemLocale";
const storageDescriptor = Object.getOwnPropertyDescriptor(window, "localStorage");
afterEach(() => {
  vi.unstubAllGlobals(); vi.restoreAllMocks();
  if (storageDescriptor) Object.defineProperty(window, "localStorage", storageDescriptor);
  else Reflect.deleteProperty(window, "localStorage");
});
it.each([
  [["zh-Hant"], "zh-Hant"], [["zh-Hans-TW"], "zh-Hans"], [["ZH_hk"], "zh-Hant"],
  [["zh-MO"], "zh-Hant"], [["zh-CN"], "zh-Hans"], [["ja-JP"], "ja"], [["es-MX"], "es"],
  [["fr-FR", "ja-JP", "en-US"], "ja"], [["en-GB", "zh-Hant"], "en"],
  [["de-DE", "fr-FR"], "en"], [[], "en"],
] as const)("negotiates %j as %s", (languages, expected) => expect(localeFromLanguages(languages)).toBe(expected));
it("uses navigator.language when the platform has no language list", () => {
  vi.stubGlobal("navigator", { language: "es-AR", languages: [] });
  expect(systemLocale()).toBe("es");
});
it("keeps OS detection working when storage is denied", async () => {
  vi.stubGlobal("navigator", { language: "en-US", languages: ["fr-FR", "zh-Hant-HK"] });
  Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: () => { throw new Error("denied"); } } });
  vi.resetModules();
  const { i18n } = await import("../stores/i18n");
  expect(i18n.locale()).toBe("zh-Hant");
  expect(i18n.followsSystemLocale()).toBe(true);
});
it("preserves an explicit language over the OS preference", async () => {
  vi.stubGlobal("navigator", { language: "ja-JP", languages: ["ja-JP"] });
  Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: () => "es" } });
  vi.resetModules();
  const { i18n } = await import("../stores/i18n");
  expect(i18n.locale()).toBe("es");
  expect(i18n.followsSystemLocale()).toBe(false);
});
it("remembers system mode without repeating first-run language setup", async () => {
  vi.stubGlobal("navigator", { language: "ja-JP", languages: ["ja-JP"] });
  Object.defineProperty(window, "localStorage", { configurable: true, value: { getItem: () => "system" } });
  vi.resetModules();
  const { i18n } = await import("../stores/i18n");
  expect(i18n.locale()).toBe("ja");
  expect(i18n.hasStoredLocale()).toBe(true);
  expect(i18n.followsSystemLocale()).toBe(true);
});
it("follows OS language changes only until an explicit override", async () => {
  const os = { language: "en-US", languages: ["en-US"] };
  vi.stubGlobal("navigator", os);
  const data = new Map<string, string>();
  Object.defineProperty(window, "localStorage", { configurable: true, value: {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
  } });
  vi.resetModules();
  const { i18n } = await import("../stores/i18n");
  i18n.initLocale();
  os.languages = ["ja-JP"];
  window.dispatchEvent(new Event("languagechange"));
  expect(i18n.locale()).toBe("ja");
  i18n.setLocale("es");
  os.languages = ["zh-Hant"];
  window.dispatchEvent(new Event("languagechange"));
  expect(i18n.locale()).toBe("es");
  i18n.useSystemLocale();
  expect(i18n.locale()).toBe("zh-Hant");
  expect(data.get("opencompose.locale")).toBe("system");
});
it("keeps startup usable when the storage property itself is denied", async () => {
  vi.stubGlobal("navigator", { language: "ja-JP", languages: ["ja-JP"] });
  Object.defineProperty(window, "localStorage", { configurable: true, get() { throw new Error("Storage access denied"); } });
  vi.resetModules();
  const { i18n } = await import("../stores/i18n");
  expect(i18n.locale()).toBe("ja");
  expect(i18n.hasStoredLocale()).toBe(true);
});
