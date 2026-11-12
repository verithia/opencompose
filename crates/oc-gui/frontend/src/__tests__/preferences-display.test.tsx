/**
 * Display page: theme family and variant with the preview cards, the interface
 * scale with its live value and reset, and the language.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { DisplayPage } from "../components/preferences/DisplayPage";
import { themeStore } from "../stores/theme";
import { i18n } from "../stores/i18n";
import { uiScaleStore } from "../stores/uiScale";

beforeEach(() => {
  i18n.setLocale("en");
  themeStore.setTheme("default", "dark");
  uiScaleStore.reset();
});

afterEach(() => cleanup());

describe("the Display page", () => {
  it("offers the theme family and its variants with preview cards", () => {
    const { container } = render(() => <DisplayPage />);

    const family = container.querySelector(".theme-family-select") as HTMLSelectElement;
    expect(family.value).toBe("default");
    expect(Array.from(family.options).map((option) => option.value)).toContain("classic");

    expect(container.querySelectorAll(".theme-preview-card")).toHaveLength(2);
    const variant = container.querySelector(".theme-variant-select") as HTMLSelectElement;
    expect(variant.value).toBe("dark");

    fireEvent.change(family, { target: { value: "classic" } });
    expect(themeStore.family().id).toBe("classic");
    expect(container.querySelectorAll(".theme-preview-card").length).toBeGreaterThan(2);
  });

  it("localizes theme names and preview labels without changing IDs or CSS", () => {
    const { container, getByRole } = render(() => <DisplayPage />);
    const family = container.querySelector(".theme-family-select") as HTMLSelectElement;
    const variant = container.querySelector(".theme-variant-select") as HTMLSelectElement;
    i18n.setLocale("zh-Hans");
    expect(family.selectedOptions[0].textContent).toBe("默认");
    expect(variant.selectedOptions[0].textContent).toBe("深色");
    expect(getByRole("button", { name: "深色" })).toBeTruthy();
    expect(themeStore.id()).toBe("default/dark");
    expect(themeStore.cssTheme()).toBe("graphite");
    fireEvent.change(family, { target: { value: "classic" } });
    expect(family.selectedOptions[0].textContent).toBe("经典");
    expect(variant.selectedOptions[0].textContent).toBe("午夜");
    expect(themeStore.id()).toBe("classic/midnight");
    expect(themeStore.cssTheme()).toBe("midnight");
  });

  it("can return explicit theme and language overrides to system defaults", () => {
    const { container } = render(() => <DisplayPage />);
    const family = container.querySelector(".theme-family-select") as HTMLSelectElement;
    const language = container.querySelector(".locale-select") as HTMLSelectElement;
    fireEvent.change(family, { target: { value: "system" } });
    fireEvent.change(language, { target: { value: "system" } });
    expect(themeStore.followsSystem()).toBe(true);
    expect(i18n.followsSystemLocale()).toBe(true);
    expect(family.value).toBe("system");
    expect(language.value).toBe("system");
  });

  it("shows the scale as a live percentage and resets it", () => {
    const { container } = render(() => <DisplayPage />);

    const scale = container.querySelector(".preferences-scale") as HTMLInputElement;
    expect((container.querySelector(".preferences-scale-number") as HTMLInputElement).value).toBe("100");

    fireEvent.input(scale, { target: { value: "1.5" } });
    expect(uiScaleStore.scale()).toBe(1.5);
    expect((container.querySelector(".preferences-scale-number") as HTMLInputElement).value).toBe("150");

    fireEvent.click(container.querySelector(".preferences-scale-reset")!);
    expect(uiScaleStore.scale()).toBe(1);
    expect((container.querySelector(".preferences-scale-number") as HTMLInputElement).value).toBe("100");
  });

  it("applies an exact percentage only after confirmation and syncs the slider", () => {
    const { getByRole, container } = render(() => <DisplayPage />);
    const number = getByRole("spinbutton", { name: "Interface scale (%)" }) as HTMLInputElement;
    const apply = getByRole("button", { name: "Apply" }) as HTMLButtonElement;
    for (const value of ["70", "150"]) {
      const previous = uiScaleStore.scale();
      fireEvent.input(number, { target: { value } });
      expect(uiScaleStore.scale()).toBe(previous);
      expect(apply.disabled).toBe(false);
      fireEvent.click(apply);
      expect(uiScaleStore.scale()).toBe(Number(value) / 100);
      expect((container.querySelector(".preferences-scale") as HTMLInputElement).value).toBe(String(Number(value) / 100));
      expect(apply.disabled).toBe(true);
    }
  });

  it.each(["", "60", "160", "75", "100.5"])("rejects invalid percentage %s without changing zoom", value => {
    const { getByRole } = render(() => <DisplayPage />);
    const number = getByRole("spinbutton", { name: "Interface scale (%)" });
    fireEvent.input(number, { target: { value } });
    expect(number.getAttribute("aria-invalid")).toBe("true");
    expect((getByRole("button", { name: "Apply" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(number, { key: "Enter" });
    expect(uiScaleStore.scale()).toBe(1);
  });

  it("supports Enter, Escape and reset without retaining an invalid draft", () => {
    const { getByRole } = render(() => <DisplayPage />);
    const number = getByRole("spinbutton", { name: "Interface scale (%)" }) as HTMLInputElement;
    fireEvent.input(number, { target: { value: "150" } });
    fireEvent.keyDown(number, { key: "Enter" });
    expect(uiScaleStore.scale()).toBe(1.5);
    fireEvent.input(number, { target: { value: "70" } });
    fireEvent.keyDown(number, { key: "Escape" });
    expect(number.value).toBe("150");
    expect(uiScaleStore.scale()).toBe(1.5);
    fireEvent.click(getByRole("button", { name: "Reset" }));
    expect(number.value).toBe("100");
    fireEvent.input(number, { target: { value: "" } });
    fireEvent.click(getByRole("button", { name: "Reset" }));
    expect(number.value).toBe("100");
  });

  it.each(["en", "zh-Hans", "zh-Hant", "ja", "es"] as const)("localizes the exact scale controls in %s", locale => {
    i18n.setLocale(locale);
    const { getByRole } = render(() => <DisplayPage />);
    expect(getByRole("spinbutton", { name: i18n.t("preferences.display.scalePercent") })).toBeTruthy();
    expect(getByRole("button", { name: i18n.t("preferences.display.scaleApply") })).toBeTruthy();
  });

  it("changes the language without closing the page", () => {
    const { container } = render(() => <DisplayPage />);
    const locale = container.querySelector(".locale-select") as HTMLSelectElement;
    expect(locale.value).toBe("en");

    fireEvent.change(locale, { target: { value: "zh-Hans" } });

    expect(i18n.locale()).toBe("zh-Hans");
    expect(container.querySelector(".preferences-page, .theme-preview-grid")).toBeTruthy();
    i18n.setLocale("en");
  });
});
