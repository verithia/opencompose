import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import type { PresetDef } from "../../types/project";

const SYNTH: PresetDef = {
  name: "My Pad",
  source: "builtin",
  plugin: "builtin.open_synth",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  params: [["osc.a.level", { Number: 0.5 }]],
  effects: [],
};

type Usage = { favorite: boolean; last_used_at: number | null };

/** Factory usage the backend would return; the store is mutated by a toggle. */
let preferences: Record<string, Usage>;
/** The single user library row, mutated by a toggle like the file would be. */
let userPatch: {
  name: string;
  category: string;
  tags: string[];
  patch: string | null;
  params: unknown[];
  effects: unknown[];
  favorite: boolean;
  last_used_at: number | null;
};
let favoriteCalls: [string, string, boolean][];

function snapshot() {
  return makeSnapshot({ presets: [SYNTH], tracks: [] });
}

function rowNames(): string[] {
  return [...document.querySelectorAll(".factory-patch-row .factory-patch-name")]
    .map((node) => node.textContent ?? "");
}

function row(name: string): HTMLElement {
  for (const element of document.querySelectorAll<HTMLElement>(".factory-patch-row")) {
    if (element.querySelector(".factory-patch-name")?.textContent === name) return element;
  }
  throw new Error(`no browser row for ${name}`);
}

function star(name: string): HTMLButtonElement {
  return row(name).querySelector(".factory-patch-favorite") as HTMLButtonElement;
}

function search(): HTMLInputElement {
  return screen.getByLabelText("Search presets") as HTMLInputElement;
}

async function openEditor() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "My Pad");
  return view;
}

function openBrowser() {
  fireEvent.click(screen.getByLabelText("Preset browser"));
}

async function openBrowserWithRows() {
  openBrowser();
  await waitFor(() => expect(rowNames().length).toBeGreaterThan(1));
}

async function closeBrowser() {
  fireEvent.keyDown(search(), { key: "Escape" });
  await waitFor(() => expect(screen.queryByLabelText("Search presets")).toBeNull());
}

beforeEach(async () => {
  invokeMock.mockClear();
  favoriteCalls = [];
  // Glass Pad is favourite; the user patch is merely frequently used.
  preferences = {
    "factory:Glass Pad": { favorite: true, last_used_at: 100 },
  };
  userPatch = {
    name: "User Bass",
    category: "Bass",
    tags: ["user", "warm"],
    patch: "Neon Bass",
    params: [["filter.cutoff", { Number: 740 }]],
    effects: [],
    favorite: false,
    last_used_at: 200,
  };
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "list_patch_preferences") return { version: 1, patches: preferences };
    if (cmd === "list_library_patches") {
      return { patches: [{ ...userPatch }], warnings: [], replaced: false };
    }
    if (cmd === "set_patch_favorite") {
      const { name, tier, favorite } = args as {
        name: string;
        tier: string;
        favorite: boolean;
      };
      favoriteCalls.push([name, tier, favorite]);
      if (tier === "user") {
        userPatch.favorite = favorite;
      } else {
        const key = `factory:${name}`;
        preferences[key] = { favorite, last_used_at: preferences[key]?.last_used_at ?? null };
      }
      return true;
    }
    return snapshot();
  });
  await store.initProject();
  await store.stopAudition();
});

afterEach(cleanup);

describe("preset favourites and recent order", () => {
  it("renders a favourites section first, then the most recently used row", async () => {
    await openEditor();
    await openBrowserWithRows();

    const sections = [...document.querySelectorAll(".factory-patch-group-section")]
      .map((node) => node.textContent);
    expect(sections[0]).toBe("Favourites");

    const names = rowNames();
    // Favourites by name, then the rest by last-used; unused rows keep bank order.
    expect(names.slice(0, 2)).toEqual(["Glass Pad", "User Bass"]);
    expect(star("Glass Pad").getAttribute("aria-pressed")).toBe("true");
    expect(star("User Bass").getAttribute("aria-pressed")).toBe("false");
    const glass = names.indexOf("Glass Pad");
    const user = names.indexOf("User Bass");
    const init = names.indexOf("Init");
    expect(glass).toBe(0);
    expect(user).toBe(1);
    expect(init).toBeGreaterThan(user);
  });

  it("toggles a factory star through the command and keeps the order stable", async () => {
    await openEditor();
    await openBrowserWithRows();

    fireEvent.click(star("Init"));
    await waitFor(() => expect(favoriteCalls).toEqual([["Init", "factory", true]]));
    await waitFor(() => expect(star("Init").getAttribute("aria-pressed")).toBe("true"));

    const reordered = rowNames();
    expect(reordered.slice(0, 2)).toEqual(["Glass Pad", "Init"]);

    // Clearing a search is a re-render; the sorted rows must not drift.
    fireEvent.input(search(), { target: { value: "pad" } });
    await waitFor(() => expect(rowNames().length).toBeLessThan(reordered.length));
    fireEvent.input(search(), { target: { value: "" } });
    await waitFor(() => expect(rowNames()).toEqual(reordered));
  });

  it("persists a factory favourite when the browser reopens", async () => {
    await openEditor();
    await openBrowserWithRows();

    fireEvent.click(star("Init"));
    await waitFor(() => expect(favoriteCalls).toEqual([["Init", "factory", true]]));
    await closeBrowser();

    await openBrowserWithRows();
    await waitFor(() => expect(rowNames().slice(0, 2)).toEqual(["Glass Pad", "Init"]));
    expect(star("Init").getAttribute("aria-pressed")).toBe("true");
  });

  it("toggles a user star with the user tier and keeps it in the library", async () => {
    await openEditor();
    await openBrowserWithRows();

    fireEvent.click(star("User Bass"));
    await waitFor(() => expect(favoriteCalls).toEqual([["User Bass", "user", true]]));
    expect(star("User Bass").getAttribute("aria-pressed")).toBe("true");
    await closeBrowser();

    await openBrowserWithRows();
    await waitFor(() => expect(star("User Bass").getAttribute("aria-pressed")).toBe("true"));
    expect(favoriteCalls).toHaveLength(1);
  });

  it("still matches tags in the search box and through a tag chip", async () => {
    await openEditor();
    await openBrowserWithRows();

    fireEvent.input(search(), { target: { value: "warm" } });
    await waitFor(() => expect(rowNames()).toEqual(["User Bass"]));

    fireEvent.input(search(), { target: { value: "" } });
    await waitFor(() => expect(rowNames().length).toBeGreaterThan(1));

    const chip = row("User Bass").querySelector(".factory-patch-tag") as HTMLButtonElement;
    expect(chip.textContent).toBe("warm");
    fireEvent.click(chip);
    await waitFor(() => expect(search().value).toBe("warm"));
    await waitFor(() => expect(rowNames()).toEqual(["User Bass"]));
  });
});
