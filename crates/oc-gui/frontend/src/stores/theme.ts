import { createSignal } from "solid-js";
import { i18n, type I18nKey } from "./i18n";

export interface ThemePreviewColors {
  bg: string;
  surface: string;
  accent: string;
  rail: string;
}

export interface ThemeVariant {
  id: string;
  label: string;
  /** `data-theme` key consumed by the CSS token blocks. */
  css: string;
  colors: ThemePreviewColors;
}

export interface ThemeFamily {
  id: string;
  label: string;
  variants: readonly ThemeVariant[];
}

export const THEME_CATALOG: readonly ThemeFamily[] = [
  {
    id: "default",
    label: "Default",
    variants: [
      { id: "light", label: "Light", css: "ivory", colors: { bg: "#F3F5F8", surface: "#E1E7EF", accent: "#0284C7", rail: "#4F46E5" } },
      { id: "dark", label: "Dark", css: "graphite", colors: { bg: "#12151A", surface: "#1E232B", accent: "#38BDF8", rail: "#818CF8" } },
    ],
  },
  {
    id: "catppuccin",
    label: "Catppuccin",
    variants: [
      { id: "latte", label: "Latte", css: "catppuccin-latte", colors: { bg: "#eff1f5", surface: "#ccd0da", accent: "#8839ef", rail: "#1e66f5" } },
      { id: "frappe", label: "Frappé", css: "catppuccin-frappe", colors: { bg: "#303446", surface: "#414559", accent: "#ca9ee6", rail: "#8caaee" } },
      { id: "macchiato", label: "Macchiato", css: "catppuccin-macchiato", colors: { bg: "#24273a", surface: "#363a4f", accent: "#c6a0f6", rail: "#8aadf4" } },
      { id: "mocha", label: "Mocha", css: "catppuccin-mocha", colors: { bg: "#1e1e2e", surface: "#313244", accent: "#cba6f7", rail: "#89b4fa" } },
    ],
  },
  {
    id: "classic",
    label: "Classic",
    variants: [
      { id: "midnight", label: "Midnight", css: "midnight", colors: { bg: "#171018", surface: "#2a1d22", accent: "#e2a842", rail: "#6b8aab" } },
      { id: "signal", label: "Signal", css: "signal", colors: { bg: "#000000", surface: "#111111", accent: "#ffe600", rail: "#ffffff" } },
      { id: "linen", label: "Linen", css: "linen", colors: { bg: "#efede6", surface: "#d5cec0", accent: "#6f8f4c", rail: "#557c9e" } },
      { id: "draft", label: "Draft", css: "draft", colors: { bg: "#eef5f8", surface: "#cbd9e2", accent: "#0097b8", rail: "#3f67b5" } },
      { id: "quartz", label: "Quartz", css: "quartz", colors: { bg: "#f7f9fb", surface: "#e0e7ef", accent: "#3157d8", rail: "#0099b8" } },
      { id: "prism", label: "Prism", css: "prism", colors: { bg: "#fbfcff", surface: "#e5eaf4", accent: "#7c3aed", rail: "#06a3c8" } },
      { id: "carbon", label: "Carbon", css: "carbon", colors: { bg: "#0b0d0d", surface: "#1a1e1e", accent: "#7bd88f", rail: "#74a6d8" } },
      { id: "ember", label: "Ember", css: "ember", colors: { bg: "#180d09", surface: "#321b12", accent: "#ff9a45", rail: "#5ac7bc" } },
      { id: "vector", label: "Vector", css: "vector", colors: { bg: "#020908", surface: "#0a1b18", accent: "#74ff8a", rail: "#3de6ff" } },
    ],
  },
];

const THEME_STORAGE_KEY = "opencompose.theme";

/** Flat ids kept for projects that persisted a pre-catalog theme. */
const LEGACY_THEME_IDS: Record<string, string> = {
  graphite: "default/dark",
  ivory: "default/light",
  "high-contrast": "classic/signal",
};

interface ResolvedTheme {
  family: ThemeFamily;
  variant: ThemeVariant;
}

function findTheme(id: string): ResolvedTheme | undefined {
  const [familyId, variantId] = id.split("/");
  const family = THEME_CATALOG.find((entry) => entry.id === familyId);
  const variant = family?.variants.find((entry) => entry.id === variantId);
  return family && variant ? { family, variant } : undefined;
}

/** Accepts `family/variant` ids and legacy flat ids; returns `null` when unknown. */
function resolveThemeId(value: string | null | undefined): string | null {
  if (!value) return null;
  if (findTheme(value)) return value;

  const legacy = LEGACY_THEME_IDS[value];
  if (legacy && findTheme(legacy)) return legacy;

  for (const family of THEME_CATALOG) {
    const variant = family.variants.find((entry) => entry.id === value);
    if (variant) return `${family.id}/${variant.id}`;
  }
  return null;
}

function systemThemeId(): string {
  try {
    if (typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches) {
      return "default/dark";
    }
  } catch (_) {}
  return "default/light";
}

function readStoredTheme(): string | null {
  try {
    return typeof window === "undefined" ? null : resolveThemeId(window.localStorage.getItem(THEME_STORAGE_KEY));
  } catch { return null; }
}

const storedTheme = readStoredTheme();
const [themeId, setThemeId] = createSignal<string>(storedTheme ?? systemThemeId());
const [followsSystem, setFollowsSystem] = createSignal(storedTheme === null);

const currentTheme = (): ResolvedTheme => findTheme(themeId()) ?? findTheme("default/light")!;

function currentFamily(): ThemeFamily {
  return currentTheme().family;
}

function currentVariant(): ThemeVariant {
  return currentTheme().variant;
}

function applyTheme(id: string) {
  if (typeof document === "undefined") return;
  const theme = findTheme(id);
  if (theme) document.documentElement.dataset.theme = theme.variant.css;
}

function persistTheme(id: string) {
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, id);
  } catch (_) {}
}

function setTheme(familyOrId: string, variantId?: string) {
  const resolved = resolveThemeId(variantId ? `${familyOrId}/${variantId}` : familyOrId);
  if (!resolved) return;
  // Apply the DOM attribute before the signal: effects that read theme tokens
  // (canvas colours, for example) must see the new theme's computed values.
  setFollowsSystem(false);
  applyTheme(resolved);
  setThemeId(resolved);
  persistTheme(resolved);
}

function useSystemTheme() {
  setFollowsSystem(true);
  const id = systemThemeId();
  applyTheme(id);
  setThemeId(id);
  persistTheme("system");
}

let observingSystemTheme = false;
function initTheme() {
  applyTheme(themeId());
  if (observingSystemTheme) return;
  try {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => {
      if (!followsSystem()) return;
      const id = systemThemeId();
      applyTheme(id);
      setThemeId(id);
    };
    if (media.addEventListener) media.addEventListener("change", update);
    else media.addListener(update);
    observingSystemTheme = true;
  } catch { /* A WebView without matchMedia keeps the light fallback. */ }
}

const familyLabelKeys: Readonly<Record<string, I18nKey>> = {
  default: "theme.family.default", classic: "theme.family.classic",
};
const variantLabelKeys: Readonly<Record<string, I18nKey>> = {
  light: "startup.light", dark: "startup.dark",
  midnight: "theme.variant.midnight", signal: "theme.variant.signal", linen: "theme.variant.linen",
  draft: "theme.variant.draft", quartz: "theme.variant.quartz", prism: "theme.variant.prism",
  carbon: "theme.variant.carbon", ember: "theme.variant.ember", vector: "theme.variant.vector",
  latte: "theme.variant.latte", frappe: "theme.variant.frappe",
  macchiato: "theme.variant.macchiato", mocha: "theme.variant.mocha",
};

export function themeFamilyLabel(family: ThemeFamily): string {
  return Object.hasOwn(familyLabelKeys, family.id) ? i18n.t(familyLabelKeys[family.id]) : family.label;
}

export function themeVariantLabel(variant: ThemeVariant): string {
  return Object.hasOwn(variantLabelKeys, variant.id) ? i18n.t(variantLabelKeys[variant.id]) : variant.label;
}

export const themeStore = {
  id: themeId,
  family: currentFamily,
  variant: currentVariant,
  cssTheme: () => currentVariant().css,
  catalog: THEME_CATALOG,
  setTheme,
  followsSystem,
  useSystemTheme,
  initTheme,
};
