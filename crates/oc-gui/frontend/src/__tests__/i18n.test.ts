import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { paramLabel } from "../data/synthCatalog";
import { esCompletion } from "../stores/locales/esCompletion";
import { jaCompletionAi } from "../stores/locales/jaCompletionAi";
import { jaCompletionCore } from "../stores/locales/jaCompletionCore";
import { i18n, type I18nKey } from "../stores/i18n";

const CJK = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/;

beforeEach(() => i18n.setLocale("en"));
afterEach(() => i18n.setLocale("en"));

describe("i18n dictionaries", () => {
  it("keeps every English value in English", () => {
    for (const key of i18n.keys()) {
      const text = i18n.t(key);
      expect(text.trim(), key).not.toBe("");
      expect(text, key).not.toBe(key);
      expect(CJK.test(text), `${key}: ${text}`).toBe(false);
    }
  });

  it("translates every English key into simplified and traditional Chinese", () => {
    const missingHans: string[] = [];
    const missingHant: string[] = [];
    for (const key of i18n.keys()) {
      if (!i18n.has("zh-Hans", key)) missingHans.push(key);
      if (!i18n.has("zh-Hant", key)) missingHant.push(key);
    }
    expect(missingHans, "missing zh-Hans").toEqual([]);
    expect(missingHant, "missing zh-Hant").toEqual([]);
  });

  it("covers every Japanese interface key", () => {
    expect(i18n.keys().filter(key => !i18n.has("ja", key))).toEqual([]);
  });

  it.each([
    ["Japanese", { ...jaCompletionAi, ...jaCompletionCore }],
    ["Spanish", esCompletion],
  ] as const)("preserves interpolation in the %s completion", (_, translations) => {
    const placeholderNames = (value: string) => [...value.matchAll(/\{([\w.]+)\}/g)].map(match => match[1]).sort();
    const keys = new Set<string>(i18n.keys());
    for (const [key, translation] of Object.entries(translations)) {
      expect(keys.has(key), key).toBe(true);
      expect(placeholderNames(translation), key).toEqual(placeholderNames(i18n.t(key as I18nKey)));
    }
  });

  it("never renders an empty string in any locale", () => {
    for (const locale of i18n.locales) {
      i18n.setLocale(locale);
      for (const key of i18n.keys()) {
        expect(i18n.t(key).trim(), `${locale} ${key}`).not.toBe("");
      }
    }
  });

  it("covers every Spanish label and interpolates localized paths", () => {
    i18n.setLocale("es");
    expect(i18n.keys().filter(key => !i18n.has("es", key))).toEqual([]);
    const message = i18n.t("midi.export.saved", { path: "/tmp/take.mid" });
    expect(message).toContain("/tmp/take.mid");
    expect(message).not.toContain("Written:");
  });

  it("keeps English catalog labels ASCII", () => {
    for (const key of ["osc.a.wavetable_position", "osc.a.warp_var", "filter.cutoff", "lfo.sync_div"]) {
      const label = paramLabel(key);
      expect(label, key).not.toBe(key);
      expect(label, key).toMatch(/^[\x20-\x7e]+$/);
    }
  });
});
