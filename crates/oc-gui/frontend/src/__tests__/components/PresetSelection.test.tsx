import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { pluginPanelCacheSize } from "../../stores/pluginParamsCache";
import type { PresetDef } from "../../types/project";

function importedPreset(name: string): PresetDef {
  return {
    name,
    source: "vst3",
    plugin: "vst3:halcyon_2",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params: [["serum_file", { String: `/tmp/${name}.SerumPreset` }]],
    effects: [],
  };
}

const IMPORTED_PRESETS = ["Preset A", "Preset B", "Preset C", "Preset D", "Preset E"];

const PARAMS = [
  { id: 1, name: "Cutoff", module: "Oscillators/Osc 1", min: 0, max: 1, default: 0.5, current: 0.25, display: "0.2500", flags: 0 },
  { id: 2, name: "Level", module: "Oscillators/Osc 2", min: 0, max: 1, default: 0.5, current: 0.5, display: "0.5000", flags: 0 },
];

type LoadResolver = (params: typeof PARAMS) => void;
let pendingLoads: Map<string, LoadResolver>;

const loadCalls = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "load_plugin_for_preset");
const loadCountFor = (name: string) =>
  loadCalls().filter(([, args]) => (args as { presetName?: string } | undefined)?.presetName === name).length;

function resolveLoad(name: string, params = PARAMS) {
  const resolver = pendingLoads.get(name);
  if (!resolver) throw new Error(`no pending load for ${name}`);
  pendingLoads.delete(name);
  resolver(params);
}

beforeEach(async () => {
  pendingLoads = new Map();
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string, args?: { presetName?: string }) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "load_plugin_for_preset") {
      return new Promise<typeof PARAMS>((resolve) => {
        pendingLoads.set(args?.presetName ?? "", resolve);
      });
    }
    if (cmd === "plugin_has_gui") return true;
    if (cmd === "plugin_editor_status") return null;
    return makeSnapshot({
      presets: IMPORTED_PRESETS.map(importedPreset),
      tracks: [{ ...makeTrack("Preset", 0), preset: "Preset A" }],
    });
  });
  await store.initProject();
});

describe("preset detail parameter loading", () => {
  it("loads once when the panel opens, with a spinner in place of the table", async () => {
    const { getByText, getByRole, queryByText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Preset A"));

    await waitFor(() => expect(loadCalls().length).toBe(1));
    expect(loadCalls()[0][1]).toEqual({ presetName: "Preset A" });
    expect(getByRole("status").textContent).toContain("Loading Preset A…");
    expect(document.querySelector(".preset-params-spinner")).toBeTruthy();
    expect(queryByText(/^Parameters \(/)).toBeNull();

    resolveLoad("Preset A");
    await waitFor(() => expect(getByText("Parameters (2)")).toBeTruthy());

    // Expanding a module re-renders the pane but must not load again.
    fireEvent.click(getByText("Osc 1"));
    fireEvent.click(getByText("Osc 1"));
    expect(loadCalls().length).toBe(1);
  });

  it("reuses the cached parameters when the panel opens again", async () => {
    const first = render(() => <PresetEditor />);
    fireEvent.click(first.getByText("Preset A"));
    await waitFor(() => expect(loadCalls().length).toBe(1));
    resolveLoad("Preset A");
    await waitFor(() => expect(first.getByText("Parameters (2)")).toBeTruthy());
    expect(pluginPanelCacheSize()).toBe(1);
    first.unmount();

    const second = render(() => <PresetEditor />);
    fireEvent.click(second.getByText("Preset A"));

    await waitFor(() => expect(second.getByText("Parameters (2)")).toBeTruthy());
    expect(loadCountFor("Preset A")).toBe(1);
    expect(second.queryByRole("status")).toBeNull();
  });

  it("never instantiates from the row selection alone", async () => {
    const { getByText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Preset A"));
    fireEvent.click(getByText("Preset B"));
    // The click handlers only move the selection; nothing is instantiated yet.
    expect(loadCalls().length).toBe(0);

    // Only the preset the pane settled on loads.
    await waitFor(() => expect(loadCalls().length).toBe(1));
    expect(loadCalls()[0][1]).toEqual({ presetName: "Preset B" });
  });

  it("never instantiates from the audition selection alone", async () => {
    // The Space bar auditions store.selectedPresetName; naming a preset there is
    // not the same as the detail pane showing it.
    store.setSelectedPresetName("Preset A");
    const { queryByText, queryByRole } = render(() => <PresetEditor />);
    await waitFor(() => expect(queryByText("Preset A")).toBeTruthy());

    expect(loadCalls().length).toBe(0);
    expect(queryByRole("status")).toBeNull();
    expect(queryByText(/^Parameters \(/)).toBeNull();
  });

  it("shows a load failure inline and keeps the panel usable", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "load_plugin_for_preset") {
        throw new Error("VST3 plugin 'vst3:halcyon_2' not found on system");
      }
      if (cmd === "get_plugin_params") return PARAMS;
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "plugin_editor_status") return null;
      return makeSnapshot({ presets: IMPORTED_PRESETS.map(importedPreset) });
    });
    await store.initProject();

    const { getByText, getByLabelText, queryByText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Preset A"));

    await waitFor(() => expect(getByText(/not found on system/)).toBeTruthy());
    expect(queryByText(/^Parameters \(/)).toBeNull();

    // The reload control retries the load and recovers the panel.
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "load_plugin_for_preset") return PARAMS;
      return makeSnapshot({ presets: IMPORTED_PRESETS.map(importedPreset) });
    });
    fireEvent.click(getByLabelText("Reload parameters"));
    await waitFor(() => expect(getByText("Parameters (2)")).toBeTruthy());
    expect(queryByText(/not found on system/)).toBeNull();
  });

  it("keeps the Open editor button visible but disabled when the editor is blocked", async () => {
    const reason = "The plugin editor crashed; edit its parameters in the inspector instead.";
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "load_plugin_for_preset") return PARAMS;
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "plugin_editor_status") return reason;
      return makeSnapshot({
        presets: IMPORTED_PRESETS.map(importedPreset),
        tracks: [{ ...makeTrack("Preset", 0), preset: "Preset A" }],
      });
    });

    const { getByText, getByRole, findByText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Preset A"));

    const button = await findByText("Open editor");
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(getByRole("button", { name: "Open editor" })).toBeTruthy();
    expect(getByText(reason)).toBeTruthy();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "open_plugin_editor")).toBe(false);
  });

  it("drops a parameter response after the selection moved to another preset", async () => {
    const { getByText, queryByText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Preset A"));
    await waitFor(() => expect(loadCountFor("Preset A")).toBe(1));
    fireEvent.click(getByText("Preset B"));
    expect(document.querySelector(".preset-detail-name")?.textContent).toBe("Preset B");
    await waitFor(() => expect(loadCountFor("Preset B")).toBe(1));

    resolveLoad("Preset A");
    await waitFor(() => expect(queryByText(/^Parameters \(/)).toBeNull());

    resolveLoad("Preset B");
    await waitFor(() => expect(getByText("Parameters (2)")).toBeTruthy());
    expect(document.querySelector(".preset-detail-name")?.textContent).toBe("Preset B");
  });

  it("no longer offers the manual load action or its hint", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "load_plugin_for_preset") return PARAMS;
      if (cmd === "plugin_has_gui") return false;
      return makeSnapshot({ presets: IMPORTED_PRESETS.map(importedPreset) });
    });

    const { getByText, getByLabelText, queryByText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Preset A"));
    await waitFor(() => expect(getByText("Parameters (2)")).toBeTruthy());

    expect(queryByText("Load plugin parameters")).toBeNull();
    expect(queryByText("Loaded on demand — selecting a preset never starts the plugin.")).toBeNull();
    expect(getByLabelText("Reload parameters")).toBeTruthy();
  });
});

describe("preset list menu", () => {
  it("renames a preset from its own menu", async () => {
    invokeMock.mockClear();
    const snapshot = makeSnapshot();
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "rename_preset") {
        return makeSnapshot({
          presets: snapshot.project.presets.map((preset) =>
            preset.name === args?.name ? { ...preset, name: String(args?.newName) } : preset),
        });
      }
      return snapshot;
    });
    await store.initProject();
    const { container } = render(() => <><PresetEditor /><ContextMenuLayer /></>);
    const row = [...container.querySelectorAll<HTMLElement>(".preset-list-item")]
      .find((item) => item.textContent?.includes("Piano 1"))!;

    fireEvent.contextMenu(row, { clientX: 40, clientY: 60 });
    const rename = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((item) => item.textContent === "Rename preset");
    expect(rename).toBeTruthy();
    fireEvent.click(rename!);

    // The prompt opens with the current name selected for replacement.
    const input = document.querySelector<HTMLInputElement>(".text-prompt input")!;
    expect(input.value).toBe("Piano 1");
    fireEvent.input(input, { target: { value: "Grand" } });
    fireEvent.submit(input.closest("form")!);

    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "rename_preset")).toBe(true);
    });
    expect(invokeMock).toHaveBeenCalledWith("rename_preset", { name: "Piano 1", newName: "Grand" });
  });
});

describe("pattern list menu", () => {
  it("renames a pattern from its row menu", async () => {
    invokeMock.mockClear();
    const { makePattern } = await import("../setup");
    const snapshot = makeSnapshot({ patterns: [{ ...makePattern("hook"), hits: [] }] });
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "rename_pattern") {
        return makeSnapshot({
          patterns: snapshot.project.patterns.map((pattern) =>
            pattern.name === args?.oldName ? { ...pattern, name: String(args?.newName) } : pattern),
        });
      }
      return snapshot;
    });
    await store.initProject();
    const { PatternPanel } = await import("../../components/PatternPanel");
    const { ContextMenuLayer } = await import("../../components/ContextMenu");
    const { container } = render(() => <><PatternPanel /><ContextMenuLayer /></>);
    const row = [...container.querySelectorAll<HTMLElement>(".pattern-item")][0];

    fireEvent.contextMenu(row, { clientX: 30, clientY: 40 });
    const rename = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((item) => item.textContent === "Rename pattern");
    expect(rename).toBeTruthy();
    fireEvent.click(rename!);

    const input = document.querySelector<HTMLInputElement>(".text-prompt input")!;
    fireEvent.input(input, { target: { value: "hook_b" } });
    fireEvent.submit(input.closest("form")!);

    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "rename_pattern")).toBe(true);
    });
  });
});
