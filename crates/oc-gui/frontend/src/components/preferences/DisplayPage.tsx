/**
 * The Display page: theme family and variant with their preview cards, the
 * interface scale, and the language.
 */
import { createEffect, createSignal, For } from "solid-js";
import { i18n, type LocaleName } from "../../stores/i18n";
import { themeFamilyLabel, themeStore, themeVariantLabel } from "../../stores/theme";
import { UI_SCALE_BOUNDS, uiScaleStore } from "../../stores/uiScale";
import { useTooltip } from "../Tooltip";

export function DisplayPage() {
  const currentPercent = () => Math.round(uiScaleStore.scale() * 100);
  const [scaleDraft, setScaleDraft] = createSignal(String(currentPercent()));
  createEffect(() => setScaleDraft(String(currentPercent())));
  const validScale = () => {
    const value = Number(scaleDraft());
    return scaleDraft().trim() !== "" && Number.isFinite(value)
      && value >= UI_SCALE_BOUNDS.min * 100 && value <= UI_SCALE_BOUNDS.max * 100
      && Number.isInteger(value / (UI_SCALE_BOUNDS.step * 100));
  };
  const applyScale = () => {
    if (validScale()) uiScaleStore.set(Number(scaleDraft()) / 100);
  };
  const resetScale = () => {
    uiScaleStore.reset();
    setScaleDraft(String(currentPercent()));
  };

  return (
    <>
      <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.language"))}>
        <span>{i18n.t("toolbar.language")}</span>
        <select
          class="theme-select locale-select"
          aria-label={i18n.t("toolbar.language")}
          value={i18n.followsSystemLocale() ? "system" : i18n.locale()}
          onChange={(event) => event.currentTarget.value === "system"
            ? i18n.useSystemLocale() : i18n.setLocale(event.currentTarget.value as LocaleName)}
        >
          <option value="system">{i18n.t("startup.system")}</option>
          {i18n.locales.map((locale) => (
            <option value={locale}>{i18n.labels[locale]}</option>
          ))}
        </select>
      </label>

      <div class="preferences-divider" />
      <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.theme"))}>
        <span>{i18n.t("toolbar.theme")}</span>
        <select
          class="theme-select theme-family-select"
          aria-label={i18n.t("toolbar.theme")}
          value={themeStore.followsSystem() ? "system" : themeStore.family().id}
          onChange={(event) => {
            if (event.currentTarget.value === "system") { themeStore.useSystemTheme(); return; }
            const family = themeStore.catalog.find((entry) => entry.id === event.currentTarget.value);
            if (family) themeStore.setTheme(family.id, family.variants[0].id);
          }}
        >
          <option value="system">{i18n.t("startup.system")}</option>
          <For each={themeStore.catalog}>
            {(family) => <option value={family.id}>{themeFamilyLabel(family)}</option>}
          </For>
        </select>
      </label>

      <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.variant"))}>
        <span>{i18n.t("toolbar.variant")}</span>
        <select
          class="theme-select theme-variant-select"
          aria-label={i18n.t("toolbar.variant")}
          value={themeStore.variant().id}
          disabled={themeStore.family().variants.length <= 1}
          onChange={(event) => themeStore.setTheme(themeStore.family().id, event.currentTarget.value)}
        >
          <For each={themeStore.family().variants}>
            {(variant) => <option value={variant.id}>{themeVariantLabel(variant)}</option>}
          </For>
        </select>
      </label>

      <div class="theme-preview-grid" aria-label={i18n.t("toolbar.variant")}>
        <For each={themeStore.family().variants}>
          {(variant) => (
            <button
              class="theme-preview-card"
              classList={{ active: themeStore.variant().id === variant.id }}
              style={`--preview-bg: ${variant.colors.bg}; --preview-surface: ${variant.colors.surface}; --preview-accent: ${variant.colors.accent}; --preview-rail: ${variant.colors.rail};`}
              onClick={() => themeStore.setTheme(themeStore.family().id, variant.id)}
              {...useTooltip(() => themeVariantLabel(variant))}
              type="button"
            >
              <span class="theme-preview-mini">
                <span class="theme-preview-toolbar" />
                <span class="theme-preview-lane lane-a"><span /><span /></span>
                <span class="theme-preview-lane lane-b"><span /><span /></span>
              </span>
              <span class="theme-preview-name">{themeVariantLabel(variant)}</span>
            </button>
          )}
        </For>
      </div>

      <div class="preferences-divider" />

      <div class="preferences-field">
        <label class="preferences-field-label" for="preferences-ui-scale">
          {i18n.t("preferences.display.scale")}
        </label>
        <div class="preferences-scale-row">
          <input
            id="preferences-ui-scale"
            type="range"
            class="preferences-range preferences-scale"
            aria-label={i18n.t("preferences.display.scale")}
            min={UI_SCALE_BOUNDS.min}
            max={UI_SCALE_BOUNDS.max}
            step={UI_SCALE_BOUNDS.step}
            value={uiScaleStore.scale()}
            onInput={(event) => uiScaleStore.set(Number(event.currentTarget.value))}
          />
          <span class="preferences-scale-value">
            <input
              type="number"
              class="preferences-scale-number"
              aria-label={i18n.t("preferences.display.scalePercent")}
              min={UI_SCALE_BOUNDS.min * 100}
              max={UI_SCALE_BOUNDS.max * 100}
              step={UI_SCALE_BOUNDS.step * 100}
              value={scaleDraft()}
              aria-invalid={!validScale()}
              onInput={event => setScaleDraft(event.currentTarget.value)}
              onKeyDown={event => {
                if (event.key === "Enter") {
                  event.preventDefault(); event.stopPropagation(); applyScale();
                } else if (event.key === "Escape") {
                  event.preventDefault(); event.stopPropagation(); setScaleDraft(String(currentPercent()));
                }
              }}
            />
            <span aria-hidden="true">%</span>
          </span>
          <button
            type="button"
            class="toolbar-btn text-btn preferences-scale-apply"
            disabled={!validScale() || Number(scaleDraft()) === currentPercent()}
            onClick={applyScale}
          >{i18n.t("preferences.display.scaleApply")}</button>
          <button
            type="button"
            class="toolbar-btn text-btn preferences-scale-reset"
            onClick={resetScale}
          >{i18n.t("preferences.display.scaleReset")}</button>
        </div>
      </div>

      <div class="preferences-divider" />

    </>
  );
}
