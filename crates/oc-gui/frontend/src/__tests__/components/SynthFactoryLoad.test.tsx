import { openSynthRange, synthRangeInput } from "../helpers/synthRange";
import { createOwnedUndoFixture } from "../setup";
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openDialogMock, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import { parameterPosition } from "../../components/synth/numericRange";
import { specForConcreteKey } from "../../data/synthCatalog";
import type { ParamValue, PresetDef } from "../../types/project";

const ownedUndo = createOwnedUndoFixture();
let undoOrder: string[] | null = null;

function param(value: string): ParamValue {
  return { Number: Number(value) };
}

function snapshotWith(params: [string, ParamValue][] = []) {
  const preset: PresetDef = {
    name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
    program: null, patch: null, file: null, root_note: null, extends: null,
    params, effects: [],
  };
  return makeSnapshot({ presets: [preset], tracks: [] });
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

function mockLoad(respond?: (batch: [string, string | null][]) => Promise<unknown>) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "set_preset_params") {
      const { params } = args as { params: [string, string | null][] };
      if (respond) return respond(params);
      return { ...snapshotWith(applyBatch(params)), dirty: true };
    }
    if (cmd === "set_preset_param") {
      const { paramName, paramValue } = args as { paramName: string; paramValue: string | null };
      return { ...snapshotWith(applyBatch([[paramName, paramValue]])), dirty: true };
    }
    return snapshotWith();
  });
}

async function openEditor() {
  const view = render(() => <PresetEditor />);
  selectPreset(view.container, "OpenSynth Init");
  return view;
}

/** Open the picker from the preset field and load a row by double-clicking it. */
function loadPatch(view: Awaited<ReturnType<typeof openEditor>>, name: string) {
  const field = view.container.querySelector<HTMLButtonElement>(".synth-patch-field")!;
  fireEvent.click(field);
  fireEvent.dblClick(screen.getByText(name));
}

function loadBatch(): [string, string | null][] {
  const call = invokeMock.mock.calls.find(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned");
  expect(call).toBeTruthy();
  return call![1].params as [string, string | null][];
}

beforeEach(async () => {
  undoOrder = null;
  invokeMock.mockClear();
  openDialogMock.mockClear();
  mockLoad();
  await store.initProject();
});

describe("OpenSynth factory patch load", () => {
  it("rebinds same-name document controls and refuses a delayed old-document reply", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let current: [string, ParamValue][] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "begin_parameter_gesture") return "fixture-owner";
      if (cmd === "end_owned_undo_gesture") return true;
      if (cmd === "set_preset_param") {
        const { paramName, paramValue } = args as { paramName: string; paramValue: string };
        if (paramValue === "0.79") {
          await gate;
          return { ...snapshotWith([[paramName, param(paramValue)]]), revision: 1000000 };
        }
        current = [[paramName, param(paramValue)]];
      }
      return snapshotWith(current);
    });
    await store.initProject();
    const view = await openEditor();
    const selector = "[data-param='osc.a.level']";
    const first = openSynthRange(view.container.querySelector<HTMLElement>(selector)!);
    fireEvent.keyDown(first, { key: "ArrowRight" });
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_param" || cmd === "set_preset_param_owned")).toBe(true));
    current = [["osc.a.level", { Number: 0.33 }]];
    await store.initProject();
    const second = openSynthRange(view.container.querySelector<HTMLElement>(selector)!);
    expect(second).not.toBe(first);
    fireEvent.keyDown(second, { key: "ArrowRight" });
    await waitFor(() => expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.34 }]));
    release();
    for (let index = 0; index < 8; index += 1) await Promise.resolve();
    expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.34 }]);
  });

  it.each([
    ["set_preset_params", () => store.setPresetParams("OpenSynth Init", [["osc.a.level", "0.9"]])],
    ["set_preset_params", () => store.loadSynthPatch("OpenSynth Init", "late", [["osc.a.level", "0.9"]], [])],
    ["synth_macro_learn", () => store.macroLearn("OpenSynth Init", 1, "filter.cutoff")],
  ] as const)("refuses stale %s results across same-name document replacement", async (command, action) => {
    let finish!: (snapshot: unknown) => void;
    const gate = new Promise<unknown>((resolve) => { finish = resolve; });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === command) return gate;
      return snapshotWith([["osc.a.level", { Number: 0.33 }]]);
    });
    await store.initProject();
    const initialError = store.error();
    const pending = action();
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === `${command}_owned`)).toBe(true));
    await store.initProject();
    const old = { ...snapshotWith([["osc.a.level", { Number: 0.9 }]]), revision: 1000000 };
    finish(command === "synth_macro_learn" ? { snapshot: old, result: { status: "bound", slot: 1, source: 4, destination: 0 } } : old);
    await pending;
    expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.33 }]);
    expect(store.error()).toBe(initialError);
  });

  it("ignores an old preset error after a document with the same preset name opens", async () => {
    let fail!: (error: Error) => void;
    const gate = new Promise<unknown>((_, reject) => { fail = reject; });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "set_preset_param") return gate;
      return snapshotWith();
    });
    await store.initProject();
    const pending = store.setPresetParam("OpenSynth Init", "osc.a.level", "0.9");
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_param_owned")).toBe(true));
    await store.initProject();
    const error = store.error();
    fail(new Error("old document failure"));
    await pending;
    expect(store.error()).toBe(error);
  });

  it("serializes rapid edits through acknowledged backend undo boundaries", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const order: string[] = [];
    undoOrder = order;
    let params: [string, ParamValue][] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "set_preset_param") {
        const { paramName, paramValue } = args as { paramName: string; paramValue: string };
        order.push(paramValue);
        if (order.filter((value) => value === "begin").length === 1) await gate;
        params = [[paramName, param(paramValue)]];
        return snapshotWith(params);
      }
      return snapshotWith(params);
    });
    await store.initProject();
    const view = await openEditor();
    const input = openSynthRange(view.container.querySelector<HTMLElement>("[data-param='osc.a.level']")!);
    fireEvent.keyDown(input, { key: "ArrowRight" });
    await waitFor(() => expect(order).toEqual(["begin", "0.79"]));
    fireEvent.keyDown(input, { key: "ArrowRight" });
    await Promise.resolve();
    expect(order).toEqual(["begin", "0.79"]);
    release();
    await waitFor(() => expect(order).toEqual(["begin", "0.79", "end", "begin", "0.8", "end"]));
    expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.8 }]);
  });

  it("keeps a newer pointer draft when an older backend response refreshes the preset", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let params: [string, ParamValue][] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = ownedUndo.route(cmd, args, (phase) => { undoOrder?.push(phase); });
    if (ownership) return ownership.value;
    cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "set_preset_param") {
        const { paramName, paramValue } = args as { paramName: string; paramValue: string };
        if (paramValue === "0.79") await gate;
        params = [[paramName, param(paramValue)]];
        return snapshotWith(params);
      }
      return snapshotWith(params);
    });
    await store.initProject();
    const view = await openEditor();
    const row = view.container.querySelector<HTMLElement>("[data-param='osc.a.level']")!;
    const input = openSynthRange(row);
    fireEvent.keyDown(input, { key: "ArrowRight" });
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_param" || cmd === "set_preset_param_owned")).toBe(true));
    const position = parameterPosition(specForConcreteKey("osc.a.level")!, 0.9);
    fireEvent.input(input, { target: { value: String(position) } });
    expect(Number(row.dataset.automationValue)).toBeCloseTo(0.9);
    release();
    await waitFor(() => expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.79 }]));
    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "end_owned_undo_gesture")).toBe(true));
    expect(Number(row.dataset.automationValue)).toBeCloseTo(0.9);
    fireEvent.change(input, { target: { value: String(position) } });
    await waitFor(() => expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.9 }]));
  });

  it("keeps keyboard focus through the project store after consecutive parameter edits", async () => {
    const view = await openEditor();
    const selector = "[data-param='osc.a.level']";
    const input = openSynthRange(view.container.querySelector<HTMLElement>(selector)!);
    fireEvent.pointerDown(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    await waitFor(() => expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.79 }]));
    expect(synthRangeInput(view.container.querySelector<HTMLElement>(selector)!)).toBe(input);
    expect(input).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    await waitFor(() => expect(store.project.presets[0].params).toContainEqual(["osc.a.level", { Number: 0.8 }]));
  });

  it("sends the chosen patch once per key and re-reads the panel", async () => {
    const view = await openEditor();
    const level = view.container.querySelector("[data-param='osc.a.level'] .synth-param-value");
    expect(level?.textContent).toContain("78%");

    loadPatch(view, "Neon Bass");

    await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned")).toBe(true));
    const batch = loadBatch();
    const loadArgs = invokeMock.mock.calls.find(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned")![1] as {
      replaceSynth: boolean;
      effects: unknown[];
      patchLabel: string;
    };
    expect(loadArgs.replaceSynth).toBe(true);
    expect(loadArgs.effects).toEqual([]);
    expect(loadArgs.patchLabel).toBe("Neon Bass");
    expect(batch).toEqual(expect.arrayContaining([
      ["filter.cutoff", "740"], ["osc.a.level", "0.74"], ["voice.mono", "1"],
    ]));
    expect(new Set(batch.map(([key]) => key)).size).toBe(batch.length);
    expect(batch).toHaveLength(448);
    expect(batch.every(([, value]) => value === null || typeof value === "string")).toBe(true);
    expect(batch).toEqual(expect.arrayContaining([
      ["macro.8", "0"], ["step.16", "0"],
      ["mod.64.source", "0"], ["mod.64.destination", "0"], ["mod.64.amount", "0"],
      ["wavetable.file", null], ["open_synth_state_file", null],
    ]));
    // The chosen patch overrides the Init defaults for its own keys.
    expect(batch.find(([key]) => key === "filter.cutoff")?.[1]).toBe("740");
    expect(batch.filter(([key]) => key === "filter.cutoff")).toHaveLength(1);

    await waitFor(() => expect(level?.textContent).toContain("74%"));
  });

  it("clears drafts and shows the loaded values after a successful load", async () => {
    const view = await openEditor();
    const slider = openSynthRange(view.container.querySelector<HTMLElement>("[data-param='osc.a.level']")!);
    const level = view.container.querySelector("[data-param='osc.a.level'] .synth-param-value");
    fireEvent.input(slider, { target: { value: "0.1" } });
    expect(level?.textContent).toContain("10%");

    loadPatch(view, "Glass Pad");

    await waitFor(() => expect(level?.textContent).toContain("58%"));
    expect(store.project.presets[0].params.find(([key]) => key === "osc.a.waveform")?.[1]).toEqual(param("4"));
  });

  it("reloads the filter page and dirties the project", async () => {
    const view = await openEditor();
    fireEvent.click(view.getByRole("button", { name: "FILTER" }));
    const cutoff = view.container.querySelector("[data-param='filter.cutoff'] .synth-param-value");
    expect(cutoff?.textContent).toContain("12000.0");

    loadPatch(view, "Glass Pad");

    await waitFor(() => expect(cutoff?.textContent).toContain("4200.0"));
    expect(store.dirty()).toBe(true);
    expect(store.project.presets[0].params.length).toBeGreaterThan(300);
  });

  it("shows a failed load in the synth view and keeps the old values", async () => {
    mockLoad(async () => {
      throw new Error("Unknown OpenSynth parameter(s): osc.bogus");
    });
    await store.initProject();
    const view = await openEditor();
    const level = view.container.querySelector("[data-param='osc.a.level'] .synth-param-value");
    expect(level?.textContent).toContain("78%");

    loadPatch(view, "Neon Bass");

    const alert = await waitFor(() => {
      const node = view.container.querySelector("[role='alert']");
      expect(node?.textContent).toContain("osc.bogus");
      return node;
    });
    expect(alert).toBeTruthy();
    expect(level?.textContent).toContain("78%");
    expect(store.error()).toContain("osc.bogus");
  });

  it("disables both header controls while a load is in flight and ignores a second pick", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    mockLoad(async (batch) => {
      await gate;
      return snapshotWith(applyBatch(batch));
    });
    await store.initProject();
    const view = await openEditor();
    loadPatch(view, "Neon Bass");
    const field = () => view.container.querySelector<HTMLButtonElement>(".synth-patch-field")!;
    const loadButton = () => view.container.querySelector<HTMLButtonElement>(".synth-load-button")!;
    await waitFor(() => expect(field().hasAttribute("disabled")).toBe(true));
    fireEvent.click(loadButton());
    fireEvent.dblClick(screen.getByText("Neon Bass"));

    expect(field().hasAttribute("disabled")).toBe(true);
    expect(loadButton().hasAttribute("disabled")).toBe(true);
    expect(openDialogMock).not.toHaveBeenCalled();
    await waitFor(() => expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_preset_params" || cmd === "set_preset_params_owned")).toHaveLength(1));
    release();
    await waitFor(() => expect(loadButton().hasAttribute("disabled")).toBe(false));
  });
});
