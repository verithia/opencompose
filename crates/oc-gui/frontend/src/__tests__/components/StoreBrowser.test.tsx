/**
 * StoreBrowser layout and filtering tests.
 */
import { afterEach, beforeEach, describe, it, expect } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { StoreBrowser } from "../../components/StoreBrowser";
import { i18n } from "../../stores/i18n";
import { clearSiteIcons } from "../../stores/storeIcons";
import { invokeMock } from "../setup";

const SURGE_PAGE = "https://surge-synthesizer.github.io/";
const PNG_DATA = "data:image/png;base64,iVBORw0KGgo=";

/** Answers `store_site_icon` from a table keyed by site page; null elsewhere. */
function serveIcons(icons: Record<string, unknown>): void {
  invokeMock.mockImplementation(async (cmd: string, args?: { url?: string }) =>
    cmd === "store_site_icon" ? (icons[args?.url ?? ""] ?? null) : undefined);
}

function card(container: HTMLElement, name: string): Element {
  return [...container.querySelectorAll(".store-card")]
    .find((element) => element.textContent?.includes(name))!;
}

function iconRequests(): string[] {
  return invokeMock.mock.calls
    .filter(([command]) => command === "store_site_icon")
    .map(([, args]) => (args as { url: string }).url);
}

describe("StoreBrowser", () => {
  beforeEach(() => {
    clearSiteIcons();
    invokeMock.mockReset();
    serveIcons({});
  });
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

  it("shows the icon the backend returns and our glyph for every other site", async () => {
    serveIcons({ [SURGE_PAGE]: PNG_DATA });
    const { container } = render(() => <StoreBrowser />);
    const surge = card(container, "Surge XT");
    await waitFor(() => expect(surge.querySelector("img.store-card-icon")).toBeTruthy());
    const img = surge.querySelector<HTMLImageElement>("img.store-card-icon")!;
    expect(img.getAttribute("src")).toBe(PNG_DATA);
    expect(img.getAttribute("alt")).toBe("");
    expect(img.getAttribute("aria-hidden")).toBe("true");

    // Only the backend's data URL is ever an image source; nothing remote.
    const images = [...container.querySelectorAll("img")];
    expect(images).toHaveLength(1);
    const freesound = card(container, "Freesound").querySelector(".store-card-icon-fallback")!;
    expect(freesound.querySelector("svg")).toBeTruthy();
    expect(freesound.getAttribute("data-category")).toBe("samples");
    // The backend is asked with the site page, once per card.
    expect(iconRequests()).toContain(SURGE_PAGE);
    expect(iconRequests()).toHaveLength(container.querySelectorAll(".store-card").length);
  });

  it("falls back to our glyph when the icon fails to decode", async () => {
    serveIcons({ [SURGE_PAGE]: PNG_DATA });
    const { container } = render(() => <StoreBrowser />);
    const surge = card(container, "Surge XT");
    await waitFor(() => expect(surge.querySelector("img.store-card-icon")).toBeTruthy());
    surge.querySelector("img.store-card-icon")!.dispatchEvent(new Event("error"));
    await waitFor(() => expect(surge.querySelector("img.store-card-icon")).toBeNull());
    const fallback = surge.querySelector(".store-card-icon-fallback")!;
    expect(fallback.getAttribute("data-category")).toBe("plugins");
    expect(fallback.getAttribute("aria-hidden")).toBe("true");
  });

  it("never displays a reply that is not a raster data URL", async () => {
    serveIcons({
      [SURGE_PAGE]: "https://surge-synthesizer.github.io/favicon.ico",
      "https://vital.audio/": "data:image/svg+xml;base64,PHN2Zy8+",
      "https://freesound.org/": { url: PNG_DATA },
      // The last card answers validly, so the refused replies have settled
      // once its image is on screen.
      "https://patchstorage.com/": PNG_DATA,
    });
    const { container } = render(() => <StoreBrowser />);
    await waitFor(() => expect(card(container, "Patchstorage").querySelector("img")).toBeTruthy());
    expect(container.querySelectorAll("img")).toHaveLength(1);
    for (const name of ["Surge XT", "Vital Synth", "Freesound"]) {
      expect(card(container, name).querySelector(".store-card-icon-fallback svg"), name).toBeTruthy();
    }
  });

  it("asks the backend once per site across renders", async () => {
    serveIcons({ [SURGE_PAGE]: PNG_DATA });
    const first = render(() => <StoreBrowser />);
    await waitFor(() => expect(card(first.container, "Surge XT").querySelector("img")).toBeTruthy());
    first.unmount();
    const second = render(() => <StoreBrowser />);
    await waitFor(() => expect(card(second.container, "Surge XT").querySelector("img")).toBeTruthy());
    expect(iconRequests().filter((url) => url === SURGE_PAGE)).toHaveLength(1);
  });
});
