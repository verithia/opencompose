import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

const STORAGE_KEY = "opencompose.theme";
const originalMatchMedia = window.matchMedia;
const storage = new Map<string, string>();

// jsdom does not expose Web Storage in this environment; the store only needs a map.
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      get length() {
        return storage.size;
      },
      key: (index: number) => Array.from(storage.keys())[index] ?? null,
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => {
        storage.set(key, String(value));
      },
      removeItem: (key: string) => {
        storage.delete(key);
      },
      clear: () => {
        storage.clear();
      },
    },
  });
}

function stubMatchMedia(dark: boolean) {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: dark && query === "(prefers-color-scheme: dark)",
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}

async function loadThemeStore() {
  vi.resetModules();
  const module = await import("../../stores/theme");
  return module.themeStore;
}

beforeEach(() => {
  window.localStorage.clear();
  delete document.documentElement.dataset.theme;
});

afterEach(() => {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: originalMatchMedia,
  });
  window.localStorage.clear();
  delete document.documentElement.dataset.theme;
  vi.restoreAllMocks();
});

describe("themeStore", () => {
  it.each([
    ["graphite", "default/dark", "graphite"],
    ["ivory", "default/light", "ivory"],
    ["midnight", "classic/midnight", "midnight"],
    ["prism", "classic/prism", "prism"],
    ["high-contrast", "classic/signal", "signal"],
  ])("migrates legacy theme %s to %s", async (legacy, expectedId, expectedCss) => {
    window.localStorage.setItem(STORAGE_KEY, legacy);
    const themeStore = await loadThemeStore();
    const [familyId, variantId] = expectedId.split("/");

    expect(themeStore.id()).toBe(expectedId);
    expect(themeStore.family().id).toBe(familyId);
    expect(themeStore.variant().id).toBe(variantId);
    expect(themeStore.cssTheme()).toBe(expectedCss);
  });

  it("persists family/variant ids and applies the CSS theme key", async () => {
    const themeStore = await loadThemeStore();

    themeStore.setTheme("catppuccin", "mocha");
    expect(themeStore.id()).toBe("catppuccin/mocha");
    expect(themeStore.cssTheme()).toBe("catppuccin-mocha");
    expect(window.localStorage.getItem(STORAGE_KEY)).toBe("catppuccin/mocha");
    expect(document.documentElement.dataset.theme).toBe("catppuccin-mocha");
  });

  it("accepts legacy flat ids through the setter", async () => {
    const themeStore = await loadThemeStore();

    themeStore.setTheme("graphite");
    expect(themeStore.id()).toBe("default/dark");

    themeStore.setTheme("prism");
    expect(themeStore.id()).toBe("classic/prism");
    expect(document.documentElement.dataset.theme).toBe("prism");
  });

  it("ignores unknown theme ids", async () => {
    stubMatchMedia(false);
    const themeStore = await loadThemeStore();

    themeStore.setTheme("not-a-theme");
    expect(themeStore.id()).toBe("default/light");
    expect(window.localStorage.getItem(STORAGE_KEY)).toBeNull();
  });

  it("exposes the three-family catalog", async () => {
    const themeStore = await loadThemeStore();

    expect(themeStore.catalog.map((family) => family.id)).toEqual(["default", "catppuccin", "classic"]);
    for (const family of themeStore.catalog) {
      expect(family.label.length).toBeGreaterThan(0);
      expect(family.variants.length).toBeGreaterThan(0);
      for (const variant of family.variants) {
        expect(variant.label.length).toBeGreaterThan(0);
        expect(variant.colors).toEqual({
          bg: expect.any(String),
          surface: expect.any(String),
          accent: expect.any(String),
          rail: expect.any(String),
        });
      }
    }
  });

  it("uses the system dark preference on first launch", async () => {
    stubMatchMedia(true);
    const themeStore = await loadThemeStore();

    expect(themeStore.id()).toBe("default/dark");
    themeStore.initTheme();
    expect(document.documentElement.dataset.theme).toBe("graphite");
  });

  it("uses the light default when the system prefers light", async () => {
    stubMatchMedia(false);
    const themeStore = await loadThemeStore();

    expect(themeStore.id()).toBe("default/light");
    themeStore.initTheme();
    expect(document.documentElement.dataset.theme).toBe("ivory");
  });

  it("falls back to light when matchMedia is unavailable", async () => {
    Object.defineProperty(window, "matchMedia", {
      configurable: true,
      writable: true,
      value: undefined,
    });
    const themeStore = await loadThemeStore();

    expect(themeStore.id()).toBe("default/light");
  });

  it("falls back to the system default for unknown stored values", async () => {
    window.localStorage.setItem(STORAGE_KEY, "legacy-unknown");
    stubMatchMedia(true);
    const themeStore = await loadThemeStore();

    expect(themeStore.id()).toBe("default/dark");
  });
});

it("preserves an explicit light choice when the OS prefers dark", async () => {
  window.localStorage.setItem(STORAGE_KEY, "default/light");
  stubMatchMedia(true);
  const themeStore = await loadThemeStore();
  expect(themeStore.id()).toBe("default/light");
  expect(themeStore.followsSystem()).toBe(false);
});

it("tracks OS changes only while following the system", async () => {
  let dark = false;
  let notify = () => {};
  Object.defineProperty(window, "matchMedia", { configurable: true, writable: true, value: () => ({
    get matches() { return dark; },
    addEventListener: (_event: string, listener: () => void) => { notify = listener; },
  }) });
  const themeStore = await loadThemeStore();
  themeStore.initTheme();
  dark = true;
  notify();
  expect(themeStore.id()).toBe("default/dark");
  themeStore.setTheme("catppuccin/mocha");
  dark = false;
  notify();
  expect(themeStore.id()).toBe("catppuccin/mocha");
  themeStore.useSystemTheme();
  expect(themeStore.id()).toBe("default/light");
  expect(themeStore.followsSystem()).toBe(true);
  expect(window.localStorage.getItem(STORAGE_KEY)).toBe("system");
});
