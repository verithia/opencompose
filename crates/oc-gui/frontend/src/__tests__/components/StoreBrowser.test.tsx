/**
 * StoreBrowser layout and filtering tests.
 */
import { afterEach, describe, it, expect } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { StoreBrowser } from "../../components/StoreBrowser";
import { i18n } from "../../stores/i18n";

describe("StoreBrowser", () => {
  afterEach(() => i18n.setLocale("en"));

  it("renders a compact header, search, and category control", () => {
    const { container } = render(() => <StoreBrowser />);
    expect(container.querySelector(".store-header")).toBeTruthy();
    expect(container.querySelector(".store-search")).toBeTruthy();
    expect(container.querySelector(".store-category")).toBeTruthy();
    // Category selection is a select so it cannot clip in a narrow sidebar.
    expect(container.querySelectorAll(".store-filters .toolbar-btn")).toHaveLength(0);
    expect(container.querySelectorAll(".store-card")).toHaveLength(14);
  });

  it("filters sites by category", () => {
    const { container } = render(() => <StoreBrowser />);
    fireEvent.change(container.querySelector(".store-category")!, { target: { value: "samples" } });
    expect(container.querySelectorAll(".store-card")).toHaveLength(5);
  });

  it("filters sites by search text", () => {
    const { container } = render(() => <StoreBrowser />);
    fireEvent.input(container.querySelector(".store-search")!, { target: { value: "surge" } });
    expect(container.querySelectorAll(".store-card")).toHaveLength(1);
    expect(container.querySelector(".store-card")?.textContent).toContain("Surge XT");
  });

  it("shows an empty state when nothing matches", () => {
    const { container } = render(() => <StoreBrowser />);
    fireEvent.input(container.querySelector(".store-search")!, { target: { value: "no-such-site" } });
    expect(container.querySelectorAll(".store-card")).toHaveLength(0);
    expect(container.textContent).toContain("No matching sites");
  });

  it("renders card descriptions in the active locale", async () => {
    const { container } = render(() => <StoreBrowser />);
    const description = () =>
      [...container.querySelectorAll(".store-card")]
        .find((element) => element.textContent?.includes("KVR Audio"))!
        .querySelector(".store-card-desc")!.textContent;
    expect(description()).toContain("Massive plugin database");

    i18n.setLocale("zh-Hans");
    await Promise.resolve();
    expect(description()).toContain("大型插件数据库");

    // Brand names stay untranslated while the blurb follows the locale.
    expect(container.textContent).toContain("KVR Audio");
    i18n.setLocale("zh-Hant");
    await Promise.resolve();
    expect(description()).toContain("大型外掛資料庫");
  });

  it("matches a card on a query in the localized description", () => {
    i18n.setLocale("zh-Hans");
    const { container } = render(() => <StoreBrowser />);
    const search = container.querySelector<HTMLInputElement>(".store-search")!;

    fireEvent.input(search, { target: { value: "波表" } });
    expect(container.querySelectorAll(".store-card")).toHaveLength(1);
    expect(container.querySelector(".store-card")?.textContent).toContain("Vital Synth");

    // The English blurb is gone, so an English-only word stops matching.
    fireEvent.input(search, { target: { value: "wavetable" } });
    expect(container.querySelectorAll(".store-card")).toHaveLength(0);
  });

  it("shows our category glyph on every card and loads no image", () => {
    const { container } = render(() => <StoreBrowser />);
    const cards = [...container.querySelectorAll(".store-card")];
    expect(cards.length).toBeGreaterThan(0);
    // Third-party logos are not bundled, and the page makes no image request.
    expect(container.querySelector("img.store-card-icon")).toBeNull();
    for (const card of cards) {
      const glyph = card.querySelector(".store-card-icon-fallback")!;
      expect(glyph.querySelector("svg")).toBeTruthy();
      expect(glyph.getAttribute("aria-hidden")).toBe("true");
    }
    const surge = cards.find((element) => element.textContent?.includes("Surge XT"))!;
    expect(surge.querySelector(".store-card-icon-fallback")!.getAttribute("data-category")).toBe("plugins");
    const freesound = cards.find((element) => element.textContent?.includes("Freesound"))!;
    expect(freesound.querySelector(".store-card-icon-fallback")!.getAttribute("data-category")).toBe("samples");
  });
});
