import { createOwnedUndoFixture } from "../setup";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  invokeMock,
  makeSnapshot,
  openDialogMock,
  selectPreset,
} from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import type { ParamValue, PresetDef } from "../../types/project";

const ownedUndo = createOwnedUndoFixture();

const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => { storage.set(key, String(value)); },
      removeItem: (key: string) => { storage.delete(key); },
      clear: () => { storage.clear(); },
    },
  });
}

const PRESET: PresetDef = {
  name: "My Pad",
  source: "builtin",
  plugin: "builtin.open_synth",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  params: [["osc.a.level", { Number: 0.5 }], ["filter.cutoff", { Number: 12000 }]],
  effects: [],
};

const IMPORT = {
  label: "Warm Pad.SerumPreset",
  name: "Warm Pad",
  author: "Test",
  params: [
    ["filter.cutoff", "740"],
    ["osc.a.level", "0.74"],
    ["mod.1.amount", "0.5"],
    ["serum_file", "/Users/me/Presets/Warm Pad.SerumPreset"],
    ["wavetable.file", null],
    ["open_synth_state_file", null],
  ] as [string, string | null][],
  warnings: [],
};

function snapshotWith(presets: PresetDef[] = [PRESET]) {
  return makeSnapshot({ presets, tracks: [] });
}

function param(value: string): ParamValue {
  return { Number: Number(value) };
}

/** Mirror the Rust batch: last write wins, null removes. */
function applyBatch(batch: [string, string | null][]): [string, ParamValue][] {
  const applied: [string, ParamValue][] = [];
  for (const [key, value] of batch) {
    const index = applied.findIndex(([existing]) => existing === key);
    if (value === null) {
      if (index >= 0) applied.splice(index, 1);
    } else if (index >= 0) {
      applied[index] = [key, param(value)];
    } else {
      applied.push([key, param(value)]);
    }
  }
  return applied;
}

type Handler = (args?: unknown) => unknown;

function mockCommands(overrides: Record<string, Handler> = {}) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "list_library_patches") return { patches: [], warnings: [], replaced: false };
    const handler = overrides[cmd];
    if (handler) return handler(args);
    if (cmd === "set_preset_params") {
      const { params, patchLabel } = args as {
        params: [string, string | null][];
        patchLabel?: string | null;
      };
      return {
        ...snapshotWith([{ ...PRESET, params: applyBatch(params), patch: patchLabel ?? null }]),
        dirty: true,
      };
    }
    return snapshotWith();
  });
}

async function openEditor() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "My Pad");
  return view;
}

function importCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "import_external_preset");
}

function writeCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned");
}

function headerOrigin(): string | null {
  return document.querySelector(".synth-patch-origin")?.textContent ?? null;
}

function loadButton(view: Awaited<ReturnType<typeof openEditor>>): HTMLElement {
  return view.getByText("LOAD");
}

afterEach(cleanup);

beforeEach(async () => {
  invokeMock.mockClear();
  openDialogMock.mockReset();
  window.localStorage.clear();
  mockCommands();
  await store.initProject();
  await store.stopAudition();
});

describe("LOAD imports external files", () => {
  it("does not load a delayed external import into a newly selected preset", async () => {
    let finish!: (value: typeof IMPORT) => void;
    const gate = new Promise<typeof IMPORT>((resolve) => { finish = resolve; });
    const second = { ...PRESET, name: "Other Pad" };
    mockCommands({
      get_project: () => snapshotWith([PRESET, second]),
      import_external_preset: () => gate,
    });
    await store.initProject();
    openDialogMock.mockResolvedValue("/test/Warm Pad.SerumPreset");
    const view = await openEditor();
    fireEvent.click(loadButton(view));
    await waitFor(() => expect(importCalls()).toHaveLength(1));
    selectPreset(view.container, "Other Pad");
    finish(IMPORT);
    for (let index = 0; index < 8; index += 1) await Promise.resolve();
    expect(writeCalls()).toHaveLength(0);
    expect(store.project.presets.find((preset) => preset.name === "Other Pad")?.params).toEqual(PRESET.params);
  });

  it("opens the external file picker, not the preset browser", async () => {
    openDialogMock.mockResolvedValue(undefined);
    const view = await openEditor();

    fireEvent.click(loadButton(view));

    await waitFor(() => expect(openDialogMock).toHaveBeenCalledTimes(1));
    expect(openDialogMock.mock.calls[0][0]).toMatchObject({
      multiple: false,
      filters: [{ name: "Preset files", extensions: ["ocppatch", "SerumPreset"] }],
    });
    expect(document.querySelector(".factory-patch-layer")).toBeNull();
    expect(screen.queryByLabelText("Search presets")).toBeNull();
    // A cancelled picker imports nothing.
    expect(importCalls()).toHaveLength(0);
    expect(writeCalls()).toHaveLength(0);
  });

  it("the preset name button opens the browser without a file dialog", async () => {
    await openEditor();

    fireEvent.click(screen.getByLabelText("Preset browser"));

    await waitFor(() => expect(document.querySelector(".factory-patch-layer")).toBeTruthy());
    expect(screen.getByLabelText("Search presets")).toBeTruthy();
    expect(openDialogMock).not.toHaveBeenCalled();
  });

  it("imports the chosen file as a whole patch with its file name as provenance", async () => {
    openDialogMock.mockResolvedValue("/Users/me/Presets/Warm Pad.SerumPreset");
    mockCommands({
      import_external_preset: (args) => {
        expect(args).toEqual({ path: "/Users/me/Presets/Warm Pad.SerumPreset" });
        return IMPORT;
      },
    });
    const view = await openEditor();

    fireEvent.click(loadButton(view));

    await waitFor(() => expect(writeCalls()).toHaveLength(1));
    expect(writeCalls()[0][1]).toEqual({
      token: expect.any(String),
      name: "My Pad",
      params: IMPORT.params,
      replaceSynth: true,
      effects: [],
      patchLabel: "Warm Pad.SerumPreset",
    });
    await waitFor(() => expect(headerOrigin()).toBe("Warm Pad.SerumPreset"));
    expect(store.project.presets[0].params).toEqual(applyBatch(IMPORT.params));
    // The workbench renders the imported values, not the previous patch's.
    await waitFor(() => expect(
      view.container.querySelector("[data-param='osc.a.level'] .synth-param-value")?.textContent,
    ).toContain("74%"));
    fireEvent.click(view.getByRole("button", { name: "FILTER" }));
    await waitFor(() => expect(
      view.container.querySelector("[data-param='filter.cutoff'] .synth-param-value")?.textContent,
    ).toContain("740"));
    // The import is not an audition: nothing is rendered before it loads.
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "audition_factory_patch")).toBe(false);
  });

  it("reports a corrupt file by name and reason and changes nothing", async () => {
    openDialogMock.mockResolvedValue("/Users/me/Presets/broken.SerumPreset");
    mockCommands({
      import_external_preset: () => {
        throw "broken.SerumPreset: not a wavetable preset (missing XferJson header)";
      },
    });
    const view = await openEditor();

    fireEvent.click(loadButton(view));

    const alert = await waitFor(() => {
      const node = document.querySelector(".synth-patch-error");
      expect(node?.textContent).toContain("broken.SerumPreset");
      expect(node?.textContent).toContain("missing XferJson header");
      return node;
    });
    expect(alert?.getAttribute("role")).toBe("alert");
    expect(writeCalls()).toHaveLength(0);
    expect(store.project.presets[0].params).toEqual(PRESET.params);
    expect(store.project.presets[0].patch).toBeNull();
    expect(headerOrigin()).toBeNull();
  });

  it("reopens the picker in the last directory", async () => {
    openDialogMock
      .mockResolvedValueOnce("/Users/me/Presets/Warm Pad.SerumPreset")
      .mockResolvedValueOnce(undefined);
    mockCommands({ import_external_preset: () => IMPORT });
    const view = await openEditor();

    fireEvent.click(loadButton(view));
    await waitFor(() => expect(writeCalls()).toHaveLength(1));
    await waitFor(() => expect(view.getByText("LOAD")).toBeTruthy());

    fireEvent.click(loadButton(view));
    await waitFor(() => expect(openDialogMock).toHaveBeenCalledTimes(2));
    expect(openDialogMock.mock.calls[1][0]).toMatchObject({
      defaultPath: "/Users/me/Presets",
    });
  });
});
