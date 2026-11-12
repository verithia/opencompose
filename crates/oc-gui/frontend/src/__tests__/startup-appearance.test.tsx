import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { StartupAppearance } from "../components/StartupAppearance";
import { i18n } from "../stores/i18n";
import { themeStore } from "../stores/theme";
beforeEach(() => { i18n.setLocale("en"); themeStore.setTheme("default/light"); });
afterEach(() => { cleanup(); vi.restoreAllMocks(); i18n.setLocale("en"); themeStore.setTheme("default/light"); });
it("shows existing explicit appearance and language choices", () => {
  themeStore.setTheme("catppuccin/mocha");
  render(() => <StartupAppearance />);
  expect(screen.getByRole("combobox", { name: "Theme" })).toHaveValue("catppuccin/mocha");
  expect(screen.getByRole("option", { name: "Catppuccin · Mocha" })).toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "Language" })).toHaveValue("en");
});
it("returns theme to the OS default and permits an override", () => {
  render(() => <StartupAppearance />);
  const theme = screen.getByRole("combobox", { name: "Theme" });
  fireEvent.change(theme, { target: { value: "system" } });
  expect(themeStore.followsSystem()).toBe(true);
  expect(theme).toHaveValue("system");
  fireEvent.change(theme, { target: { value: "default/dark" } });
  expect(themeStore.followsSystem()).toBe(false);
  expect(themeStore.id()).toBe("default/dark");
});
it("offers the OS language and retains a deliberate override", () => {
  vi.spyOn(navigator, "languages", "get").mockReturnValue(["ja-JP"]);
  render(() => <StartupAppearance />);
  const language = screen.getByRole("combobox", { name: "Language" });
  fireEvent.change(language, { target: { value: "system" } });
  expect(i18n.locale()).toBe("ja");
  expect(i18n.followsSystemLocale()).toBe(true);
  fireEvent.change(language, { target: { value: "es" } });
  expect(i18n.locale()).toBe("es");
  expect(i18n.followsSystemLocale()).toBe(false);
});
it.each(i18n.locales)("localizes startup appearance in %s", locale => {
  i18n.setLocale(locale);
  render(() => <StartupAppearance />);
  expect(screen.getByRole("group", { name: i18n.t("startup.appearance") })).toBeVisible();
  expect(screen.getByRole("combobox", { name: i18n.t("toolbar.theme") })).toBeVisible();
  expect(screen.getByRole("combobox", { name: i18n.t("toolbar.language") })).toBeVisible();
});
