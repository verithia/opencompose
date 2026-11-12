/** Startup appearance choices: OS defaults until the user chooses an override. */
import { For } from "solid-js";
import { i18n, type LocaleName } from "../stores/i18n";
import { systemLocale } from "../stores/systemLocale";
import { themeFamilyLabel, themeStore, themeVariantLabel } from "../stores/theme";
import "./StartupAppearance.css";

export function StartupAppearance() {
  const customTheme = () => !["default/light", "default/dark"].includes(themeStore.id());
  return (
    <fieldset class="startup-appearance">
      <legend>{i18n.t("startup.appearance")}</legend>
      <p>{i18n.t("startup.appearanceHint")}</p>
      <div class="startup-appearance-fields">
        <label>
          <span>{i18n.t("toolbar.theme")}</span>
          <select aria-label={i18n.t("toolbar.theme")}
            value={themeStore.followsSystem() ? "system" : themeStore.id()}
            onChange={(event) => event.currentTarget.value === "system"
              ? themeStore.useSystemTheme() : themeStore.setTheme(event.currentTarget.value)}>
            <option value="system">{i18n.t("startup.system")}</option>
            <option value="default/light">{i18n.t("startup.light")}</option>
            <option value="default/dark">{i18n.t("startup.dark")}</option>
            <For each={customTheme() ? [themeStore.id()] : []}>
              {id => <option value={id}>{themeFamilyLabel(themeStore.family())} · {themeVariantLabel(themeStore.variant())}</option>}
            </For>
          </select>
        </label>
        <label>
          <span>{i18n.t("toolbar.language")}</span>
          <select aria-label={i18n.t("toolbar.language")}
            value={i18n.followsSystemLocale() ? "system" : i18n.locale()}
            onChange={(event) => event.currentTarget.value === "system"
              ? i18n.useSystemLocale() : i18n.setLocale(event.currentTarget.value as LocaleName)}>
            <option value="system">{i18n.t("startup.system")} · {i18n.labels[systemLocale()]}</option>
            <For each={i18n.locales}>{locale => <option value={locale}>{i18n.labels[locale]}</option>}</For>
          </select>
        </label>
      </div>
    </fieldset>
  );
}
