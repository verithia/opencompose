import { createOwnedUndoFixture } from "../setup";
import { readFileSync } from "node:fs";
import { open, save } from "@tauri-apps/plugin-dialog";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import { placePatchMenu } from "../../components/OpenSynthEditor";
import type { Effect, PresetDef } from "../../types/project";

const ownedUndo = createOwnedUndoFixture();

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

const LIBRARY_EFFECT: Effect = {
  effect_type: "Delay",
  params: [["time", { Time: 0.3 }]],
  sidechain_source: null,
};

function snapshot(presets: PresetDef[] = [SYNTH]) {
  return makeSnapshot({ presets, tracks: [] });
}

function libraryListing(overrides: Record<string, unknown> = {}) {
  return {
    patches: [{
      name: "User Bass",
      category: "Bass",
      tags: ["user", "warm"],
      patch: "Neon Bass",
      params: [["filter.cutoff", { Number: 740 }]],
      effects: [LIBRARY_EFFECT],
    }],
    warnings: [],
    replaced: false,
    ...overrides,
  };
}

type Handler = (args?: unknown) => unknown;

function mockCommands(overrides: Record<string, Handler> = {}) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    const handler = overrides[cmd];
    if (handler) return handler(args);
    if (cmd === "list_library_patches") return libraryListing();
    return snapshot();
  });
}

async function openEditor() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "My Pad");
  return view;
}

/** The header field opens the factory + user-library browser. */
function openBrowser() {
  fireEvent.click(screen.getByLabelText("Preset browser"));
}

function headerName(): string {
  return document.querySelector(".synth-patch-field .synth-patch-name")?.textContent ?? "";
}

async function openMenu(): Promise<HTMLElement> {
  fireEvent.click(screen.getByRole("button", { name: "Patch actions" }));
  return screen.findByRole("menu", { name: "Patch actions" });
}

async function confirmDialog(title: string, name: string) {
  const dialog = await screen.findByRole("dialog", { name: title });
  const input = dialog.querySelector("input")!;
  fireEvent.input(input, { target: { value: name } });
  const confirm = [...dialog.querySelectorAll("button")]
    .find((button) => button.textContent !== "Cancel")!;
  fireEvent.click(confirm);
}

afterEach(cleanup);

beforeEach(async () => {
  invokeMock.mockClear();
  mockCommands();
  await store.initProject();
  // An audition can outlive a test in the module-global store.
  await store.stopAudition();
});

describe("preset sharing", () => {
  it("exports the current patch to the chosen archive", async () => {
    vi.mocked(save).mockResolvedValueOnce("/tmp/pad");
    await openEditor();
    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Share preset…" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("export_patch_bundle", expect.objectContaining({
      presetName: "My Pad", path: "/tmp/pad.ocppatch",
    })));
  });

  it("loads an imported bundle with its effect chain", async () => {
    vi.mocked(open).mockResolvedValueOnce("/tmp/shared.ocppatch");
    mockCommands({ import_patch_bundle: () => libraryListing().patches[0] });
    await openEditor();
    fireEvent.click(screen.getByText("LOAD", { selector: "button" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("import_patch_bundle", { path: "/tmp/shared.ocppatch" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_preset_params_owned", expect.objectContaining({
      patchLabel: "User Bass", effects: [LIBRARY_EFFECT],
    })));
  });
});

describe("patch lifecycle in the project", () => {
  it("renames the project patch and the header follows", async () => {
    const renamed = { ...SYNTH, name: "Warm Pad" };
    mockCommands({
      rename_preset: (args) => {
        expect(args).toEqual({ name: "My Pad", newName: "Warm Pad" });
        return snapshot([renamed]);
      },
    });
    await store.initProject();
    await openEditor();
    expect(headerName()).toBe("My Pad");

    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename…" }));
    await confirmDialog("Rename patch", "Warm Pad");

    await waitFor(() => expect(headerName()).toBe("Warm Pad"));
    expect(store.project.presets[0].name).toBe("Warm Pad");
  });

  it("duplicates into an independent project patch and edits the copy", async () => {
    const copy = { ...SYNTH, name: "My Pad copy" };
    mockCommands({
      duplicate_preset: (args) => {
        expect(args).toEqual({ name: "My Pad", newName: "My Pad copy", reassign: true });
        return snapshot([SYNTH, copy]);
      },
    });
    await store.initProject();
    await openEditor();

    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Duplicate" }));
    await confirmDialog("Duplicate patch", "My Pad copy");

    await waitFor(() => expect(headerName()).toBe("My Pad copy"));
    // The original is still there with its own values.
    expect(store.project.presets.map((preset) => preset.name)).toEqual(["My Pad", "My Pad copy"]);
  });

  it("save as creates a new project patch with the current values", async () => {
    const copy = { ...SYNTH, name: "My Pad 2" };
    mockCommands({
      duplicate_preset: (args) => {
        expect(args).toEqual({ name: "My Pad", newName: "My Pad 2", reassign: true });
        return snapshot([SYNTH, copy]);
      },
    });
    await store.initProject();
    await openEditor();

    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Save as…" }));
    await confirmDialog("Save patch as", "My Pad 2");

    await waitFor(() => expect(headerName()).toBe("My Pad 2"));
    expect(store.project.presets[0].params).toEqual(SYNTH.params);
  });

  it("saves the current patch to the user library", async () => {
    mockCommands({
      list_library_patches: () => libraryListing({ patches: [] }),
      save_patch_to_library: (args) => {
        expect(args).toEqual({
          presetName: SYNTH.name,
          name: "My Pad",
          category: null,
          tags: [],
          overwrite: true,
        });
        return libraryListing();
      },
    });
    await store.initProject();
    await store.refreshLibrary();
    await openEditor();
    expect(store.libraryPatches()).toHaveLength(0);

    await openMenu();
    fireEvent.click(screen.getByRole("menuitem", { name: "Save to library…" }));
    await confirmDialog("Save to library", "My Pad");

    await waitFor(() => expect(store.libraryPatches()).toHaveLength(1));
  });
});

describe("user library in the OpenSynth browser", () => {
  it("lists user patches with tags and loads the stored object", async () => {
    mockCommands({
      set_preset_params: () => snapshot([{ ...SYNTH, patch: "User Bass" }]),
    });
    await store.initProject();
    const view = await openEditor();
    openBrowser();

    const row = await screen.findByText("User Bass");
    expect(row).toBeTruthy();
    expect(screen.getByText("User library")).toBeTruthy();

    fireEvent.input(screen.getByLabelText("Search presets"), { target: { value: "warm" } });
    await waitFor(() => expect(screen.getByText("User Bass")).toBeTruthy());

    fireEvent.dblClick(screen.getByText("User Bass"));
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned");
      expect(call).toBeTruthy();
      const args = call![1] as {
        params: [string, string | null][];
        replaceSynth?: boolean;
        effects?: Effect[];
        patchLabel?: string;
      };
      expect(args.replaceSynth).toBe(true);
      expect(args.patchLabel).toBe("User Bass");
      expect(args.effects).toEqual([LIBRARY_EFFECT]);
      expect(args.params).toEqual([["filter.cutoff", "740"]]);
    });
    // The header shows the project patch, with the library entry as its origin.
    await waitFor(() =>
      expect(view.container.querySelector(".synth-patch-origin")?.textContent).toBe("User Bass"));
    expect(headerName()).toBe("My Pad");
  });

  it("auditions a user patch with its own effect chain", async () => {
    mockCommands({
      audition_factory_patch: () => undefined,
      get_transport_state: () => ({
        playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0,
        audition: { preset: "User Bass", status: "ready", ready_ms: 1, instances: 0, loads: 0, error: null },
      }),
    });
    await store.initProject();
    await openEditor();
    openBrowser();
    await screen.findByText("User Bass");
    await new Promise((resolve) => setTimeout(resolve, 10));
    const userRow = [...document.querySelectorAll(".factory-patch-row")].find(
      (element) => element.querySelector(".factory-patch-name")?.textContent === "User Bass",
    )!;
    const audition = userRow.querySelector(".factory-patch-audition")!;
    fireEvent.click(audition);

    await waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "audition_factory_patch");
      expect(call).toBeTruthy();
      const args = call![1] as { params: [string, string | null][]; effects?: Effect[]; label: string };
      expect(args.label).toBe("User Bass");
      expect(args.effects).toEqual([LIBRARY_EFFECT]);
      expect(args.params).toEqual([["filter.cutoff", "740"]]);
    });
  });

  it("warns about a corrupt file without hiding the rest of the library", async () => {
    mockCommands({
      list_library_patches: () => libraryListing({
        warnings: ["Skipped /tmp/broken.ocp-preset: expected value"],
      }),
    });
    await store.initProject();
    await openEditor();
    openBrowser();

    await screen.findByText("User Bass");
    expect(document.querySelector(".synth-library-warning")?.textContent).toContain("broken.ocp-preset");
  });
});

describe("patch actions menu placement", () => {
  it("opens portalled under the trigger and closes on Escape with focus back on it", async () => {
    await openEditor();
    const trigger = screen.getByRole("button", { name: "Patch actions" });
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(trigger.parentElement?.classList.contains("synth-patch-actions")).toBe(true);

    await openMenu();
    const menu = await screen.findByRole("menu", { name: "Patch actions" });
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(trigger.getAttribute("aria-controls")).toBe(menu.id);
    // A portal: the workbench clips overflow and contains its inline size.
    expect(menu.closest(".synth-workbench")).toBeNull();
    expect(menu.closest(".synth-patch-browser")).toBeNull();
    expect(menu.parentElement?.parentElement).toBe(document.body);
    expect(menu.classList.contains("synth-patch-menu")).toBe(true);
    expect(menu.dataset.placement).toBe("below");
    expect(menu.style.left).toMatch(/^\d+px$/);
    expect(menu.style.top).toMatch(/^\d+px$/);
    expect(menu.style.visibility).toBe("visible");

    await waitFor(() => expect(document.activeElement).toBe(menu));
    expect(menu.getAttribute("aria-activedescendant")).toBe(`${menu.id}-item-0`);
    expect(screen.getAllByRole("menuitem").map((item) => item.getAttribute("data-action")))
      .toEqual(["rename", "duplicate", "save-as", "library", "share"]);

    fireEvent.keyDown(menu, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu", { name: "Patch actions" })).toBeNull());
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(trigger.hasAttribute("aria-controls")).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  it("opens from the keyboard on the trigger", async () => {
    await openEditor();
    const trigger = screen.getByRole("button", { name: "Patch actions" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const menu = await screen.findByRole("menu", { name: "Patch actions" });
    await waitFor(() => expect(document.activeElement).toBe(menu));
    fireEvent.keyDown(menu, { key: "Escape" });
    await waitFor(() => expect(document.activeElement).toBe(trigger));
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  it("moves the active item with arrows and closes on an outside pointer", async () => {
    await openEditor();
    await openMenu();
    const menu = await screen.findByRole("menu", { name: "Patch actions" });
    await waitFor(() => expect(document.activeElement).toBe(menu));
    const activeId = () => menu.getAttribute("aria-activedescendant");
    expect(activeId()).toBe(`${menu.id}-item-0`);

    fireEvent.keyDown(menu, { key: "ArrowDown" });
    await waitFor(() => expect(activeId()).toBe(`${menu.id}-item-1`));
    expect(document.getElementById(`${menu.id}-item-1`)?.classList.contains("active")).toBe(true);

    fireEvent.keyDown(menu, { key: "ArrowUp" });
    await waitFor(() => expect(activeId()).toBe(`${menu.id}-item-0`));
    fireEvent.keyDown(menu, { key: "End" });
    await waitFor(() => expect(activeId()).toBe(`${menu.id}-item-4`));
    fireEvent.keyDown(menu, { key: "Home" });
    await waitFor(() => expect(activeId()).toBe(`${menu.id}-item-0`));

    fireEvent.pointerDown(document.body);
    await waitFor(() => expect(screen.queryByRole("menu", { name: "Patch actions" })).toBeNull());
  });

  it("routes every menu item to its dialog action and returns focus to the trigger", async () => {
    await openEditor();
    const trigger = screen.getByRole("button", { name: "Patch actions" });
    const expected: [string, string][] = [
      ["Rename…", "Rename patch"],
      ["Duplicate", "Duplicate patch"],
      ["Save as…", "Save patch as"],
      ["Save to library…", "Save to library"],
    ];
    for (const [label, title] of expected) {
      await openMenu();
      fireEvent.click(await screen.findByRole("menuitem", { name: label }));
      const dialog = await screen.findByRole("dialog", { name: title });
      const cancel = [...dialog.querySelectorAll("button")]
        .find((button) => button.textContent === "Cancel")!;
      fireEvent.click(cancel);
      await waitFor(() => expect(screen.queryByRole("dialog", { name: title })).toBeNull());
      expect(document.activeElement).toBe(trigger);
    }
  });
});

describe("randomize button", () => {
  it("sits next to the patch actions and advances the seed on each press", async () => {
    const calls: { name?: string; seed?: number }[] = [];
    mockCommands({
      randomize_preset: (args) => {
        calls.push(args as { name?: string; seed?: number });
        return snapshot();
      },
    });
    await openEditor();
    const button = screen.getByRole("button", { name: "Randomize patch" });
    expect(button.parentElement?.classList.contains("synth-patch-browser")).toBe(true);
    expect(button.nextElementSibling?.classList.contains("synth-patch-actions")).toBe(true);

    fireEvent.click(button);
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0].name).toBe("My Pad");

    fireEvent.click(button);
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1].name).toBe("My Pad");
    expect(calls[1].seed).toBe((calls[0].seed ?? 0) + 1);
  });

  it("reports a backend refusal without advancing the seed", async () => {
    const seeds: number[] = [];
    mockCommands({
      randomize_preset: (args) => {
        seeds.push((args as { seed: number }).seed);
        throw new Error("Randomisation requires an OpenSynth preset");
      },
    });
    await openEditor();
    const button = screen.getByRole("button", { name: "Randomize patch" });
    fireEvent.click(button);
    await waitFor(() => expect(seeds).toHaveLength(1));
    fireEvent.click(button);
    await waitFor(() => expect(seeds).toHaveLength(2));
    expect(seeds[1]).toBe(seeds[0]);
  });
});

describe("placePatchMenu", () => {
  const viewport = { width: 1024, height: 768 };

  it("right-aligns below the trigger", () => {
    expect(placePatchMenu(
      { right: 500, top: 40, bottom: 65 },
      { width: 160, height: 140 },
      viewport,
    )).toEqual({ x: 340, y: 69, side: "below" });
  });

  it("flips above when the menu would overflow the bottom edge", () => {
    const placed = placePatchMenu(
      { right: 500, top: 700, bottom: 725 },
      { width: 160, height: 140 },
      viewport,
    );
    expect(placed.side).toBe("above");
    expect(placed.y).toBe(556);
  });

  it("stays inside a small viewport", () => {
    const placed = placePatchMenu(
      { right: 310, top: 60, bottom: 85 },
      { width: 156, height: 140 },
      { width: 320, height: 480 },
    );
    expect(placed).toEqual({ x: 154, y: 89, side: "below" });
  });
});

describe("patch menu styles", () => {
  const css = readFileSync(`${process.cwd()}/src/styles/synth.css`, "utf8");
  const rule = (selector: string) => {
    const start = css.indexOf(`${selector} {`);
    expect(start).toBeGreaterThanOrEqual(0);
    return css.slice(start, css.indexOf("}", start));
  };

  it("anchors the trigger and frees the menu from layout overflow", () => {
    expect(rule(".synth-patch-actions")).toContain("position: relative");
    const menu = rule(".synth-patch-menu");
    expect(menu).toContain("position: fixed");
    expect(menu).toContain("background: var(--bg-float)");
    expect(menu).toContain("border: 1px solid");
    expect(menu).toContain("z-index: 1100");
    expect(menu).not.toContain("margin-top");
    expect(menu).not.toContain("var(--bg-panel)");
  });

  it("fixes the sibling patch dialog the same way", () => {
    const dialog = rule(".patch-action-dialog");
    expect(dialog).toContain("top: calc(100% + 4px)");
    expect(dialog).toContain("right: 7px");
    expect(dialog).toContain("background: var(--bg-float)");
    expect(dialog).not.toContain("margin-top");
  });
});
