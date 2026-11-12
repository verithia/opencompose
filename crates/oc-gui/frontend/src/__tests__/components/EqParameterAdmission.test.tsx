import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { EqCurveEditor } from "../../components/EqCurveEditor";
import { ParametricEqEditor } from "../../components/ParametricEqEditor";
import { FALLBACK_DEVICES } from "../../devices/catalog";
import { store } from "../../stores/project";
import type { Effect, ParameterTargetStamp, ProjectSnapshot } from "../../types/project";
import { invokeMock, makeEffect, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

const FIRST = "eq-target-first" as ParameterTargetStamp;
const SECOND = "eq-target-second" as ParameterTargetStamp;
let current: ProjectSnapshot;
let history: number;
let pendingBegin: Promise<string> | null;

function effect(dynamic: boolean, gain: number): Effect {
  return dynamic ? makeEffect("DynamicEq", [["band1_enabled", { Bool: true }], ["band1_freq", { Frequency: 1000 }], ["band1_gain", { Db: gain }]])
    : makeEffect("Eq", [["low_freq", { Frequency: 200 }], ["low_gain", { Db: gain }]]);
}
async function load(dynamic = false) {
  current = { ...makeSnapshot({ groups: [], markers: [], tracks: [{ ...makeTrack("EQ", 0), effects: [effect(dynamic, 3), effect(dynamic, 9)] }] }), parameter_target_stamp: FIRST };
  await store.initProject();
}
function swapSameType() {
  current = { ...current, revision: current.revision + 1, parameter_target_stamp: SECOND,
    project: { ...current.project, tracks: [{ ...current.project.tracks[0], effects: [...current.project.tracks[0].effects].reverse() }] } };
  store.applyAiSnapshot(current);
}
function mount(dynamic = false) {
  const device = FALLBACK_DEVICES.find(entry => entry.id === (dynamic ? "dynamic-eq" : "eq"))!;
  return render(() => dynamic
    ? <ParametricEqEditor target="track:0" index={0} effect={store.project.tracks[0].effects[0]} device={device} />
    : <EqCurveEditor target="track:0" index={0} effect={store.project.tracks[0].effects[0]} device={device} />);
}
function plot(container: HTMLElement, dynamic: boolean) {
  const svg = container.querySelector(dynamic ? ".peq-canvas" : ".eq-curve")!;
  svg.getBoundingClientRect = () => ({ left: 0, top: 0, right: 460, bottom: 260, width: 460, height: 260, x: 0, y: 0, toJSON: () => ({}) });
  return container.querySelector(dynamic ? '[data-peq-node="band1"]' : '[data-eq-handle="low"]')!;
}
function calls(command: string) { return invokeMock.mock.calls.filter(([name]) => name === command); }

beforeEach(() => {
  history = 0; pendingBegin = null;
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "scan_plugins") return [];
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "get_project") return current;
    if (command === "get_undo_state") return { can_undo: history > 0, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    if (command === "begin_parameter_gesture") {
      if (pendingBegin) return pendingBegin;
      if (args?.expectedStamp !== current.parameter_target_stamp) throw new Error("Parameter target changed before gesture admission");
      return "eq-owner";
    }
    if (command === "end_owned_undo_gesture") return true;
    if (command === "set_effect_params_owned") {
      const updated = structuredClone(current);
      for (const [key, raw] of args?.params as [string, string][]) {
        const params = updated.project.tracks[0].effects[0].params;
        const value = raw.endsWith("Hz") ? { Frequency: Number.parseFloat(raw) } : raw.endsWith("dB") ? { Db: Number.parseFloat(raw) } : { Number: Number(raw) };
        const index = params.findIndex(([name]) => name === key);
        if (index >= 0) params[index] = [key, value]; else params.push([key, value]);
      }
      history += 1; current = { ...updated, revision: updated.revision + 1 };
      return current;
    }
    return current;
  });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("EQ parameter admission", () => {
  it.each([false, true])("refuses a same-type replacement during the %s EQ drag", async dynamic => {
    await load(dynamic); const { container } = mount(dynamic); const node = plot(container, dynamic);
    fireEvent.mouseDown(node, { button: 0, clientX: 150, clientY: 100 });
    fireEvent.mouseMove(document, { clientX: 200, clientY: 80 });
    swapSameType(); const preserved = structuredClone(current.project);
    fireEvent.mouseUp(document);
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_effect_params_owned")).toHaveLength(0);
    expect(store.project).toEqual(preserved);
  });

  it.each([false, true])("writes a normal %s EQ drag as one atomic owned batch and one history entry", async dynamic => {
    await load(dynamic); const { container } = mount(dynamic); const node = plot(container, dynamic);
    fireEvent.mouseDown(node, { button: 0, clientX: 150, clientY: 100 });
    fireEvent.mouseMove(document, { clientX: 200, clientY: 80 }); fireEvent.mouseUp(document);
    await vi.waitFor(() => expect(calls("end_owned_undo_gesture")).toHaveLength(1));
    expect(calls("begin_parameter_gesture")).toHaveLength(1);
    expect(calls("begin_parameter_gesture")[0][1]).toMatchObject({ expectedStamp: FIRST });
    expect(calls("set_effect_params_owned")).toHaveLength(1);
    expect(calls("set_effect_param_owned")).toHaveLength(0);
    expect((calls("set_effect_params_owned")[0][1] as { params: [string,string][] }).params.map(([name]) => name))
      .toEqual(dynamic ? ["band1_freq", "band1_gain"] : ["low_freq", "low_gain"]);
    expect(history).toBe(1);
  });

  it("keeps reset frequency and gain in one batch and revokes a deferred reset after replacement", async () => {
    await load(); const { container } = mount(); const node = plot(container, false);
    let finish!: (token: string) => void; pendingBegin = new Promise(resolve => { finish = resolve; });
    fireEvent.dblClick(node);
    await vi.waitFor(() => expect(calls("begin_parameter_gesture")).toHaveLength(1));
    swapSameType(); const preserved = structuredClone(current.project);
    finish("old-reset-owner");
    await vi.waitFor(() => expect(calls("end_owned_undo_gesture")).toHaveLength(1));
    expect(calls("set_effect_params_owned")).toHaveLength(0); expect(history).toBe(0);
    expect(calls("begin_parameter_gesture")[0][1]).toMatchObject({ expectedStamp: FIRST });
    expect(calls("end_owned_undo_gesture")[0][1]).toEqual({ token: "old-reset-owner" });
    expect(store.project).toEqual(preserved);
    pendingBegin = null;
    fireEvent.dblClick(container.querySelector('[data-eq-handle="low"]')!);
    await vi.waitFor(() => expect(calls("set_effect_params_owned")).toHaveLength(1));
    expect((calls("set_effect_params_owned")[0][1] as { params: unknown }).params).toEqual([["low_freq", "200Hz"], ["low_gain", "0dB"]]);
    expect(history).toBe(1);
  });

  it("retains the first Q-wheel admission until the coalesced commit", async () => {
    await load(true); const { container } = mount(true); const node = plot(container, true);
    vi.useFakeTimers();
    fireEvent.wheel(node, { deltaY: -100 });
    swapSameType();
    await vi.advanceTimersByTimeAsync(301);
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_effect_params_owned")).toHaveLength(0);
  });

  it("retains numeric-input admission from its first input instead of recapturing on change", async () => {
    await load(true); const { container } = mount(true); fireEvent.click(plot(container, true));
    const input = container.querySelector('[data-peq-field="band1_freq"]')!;
    fireEvent.input(input, { target: { value: "1200" } });
    swapSameType();
    fireEvent.change(input, { target: { value: "1200" } });
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_effect_params_owned")).toHaveLength(0);
    expect((container.querySelector('[data-peq-field="band1_freq"]') as HTMLInputElement).value).toBe("1000");
  });
});
