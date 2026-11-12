import type { DeviceDef } from "../../types/devices";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { DevicePanel } from "../../components/DevicePanel";
import { FALLBACK_DEVICES } from "../../devices/catalog";
import { store } from "../../stores/project";
import { resetAutomationWriteForTests } from "../../stores/automationWrite";
import type { ParameterTargetStamp, ProjectSnapshot } from "../../types/project";
import { invokeMock, makeEffect, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

const device = FALLBACK_DEVICES.find(entry => entry.id === "reverb")!;
const firstStamp = "device-first-target" as ParameterTargetStamp;
const movedStamp = "device-moved-target" as ParameterTargetStamp;
let backend: ProjectSnapshot;

function mountedDevice(definition: DeviceDef = device) {
  return render(() => <DevicePanel target="track:0" index={0} count={2}
    effect={store.project.tracks[0].effects[0]} device={definition} onClose={() => {}} />);
}
function swapSameType() {
  backend = { ...backend, revision: backend.revision + 1, parameter_target_stamp: movedStamp,
    project: { ...backend.project, tracks: backend.project.tracks.map((track, index) => index === 0
      ? { ...track, effects: [...track.effects].reverse() } : track) } };
  store.applyAiSnapshot(backend);
}
function edits() {
  return invokeMock.mock.calls.filter(([command]) => command === "begin_parameter_gesture"
    || command === "set_effect_param_owned" || command === "set_effect_params_owned");
}

beforeEach(async () => {
  resetAutomationWriteForTests();
  backend = { ...makeSnapshot({ tracks: [{ ...makeTrack("Vocal", 0), effects: [
    makeEffect("Reverb", [["mix", { Percentage: 30 }], ["decay", { Time: 2 }]]),
    makeEffect("Reverb", [["mix", { Percentage: 80 }], ["decay", { Time: 4 }]]),
  ] }] }), revision: 1, parameter_target_stamp: firstStamp };
  invokeMock.mockImplementation(async (command: string, raw?: unknown) => {
    if (command === "begin_guarded_effect_param_touch") return null;
    if (command === "get_project") return backend;
    if (command === "device_presets") return { presets: [], warnings: [] };
    if (command === "meter_snapshot") return { available: false, playing: false, channels: [] };
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "set_loop_region") return { start_beats: null, end_beats: null, enabled: false };
    if (command === "get_undo_history") return [];
    if (command === "get_undo_state") return { can_undo: false, can_redo: false,
      undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    if (command === "begin_parameter_gesture") {
      const args = raw as { expectedStamp: string };
      if (args.expectedStamp !== backend.parameter_target_stamp) throw new Error("Parameter target changed");
      return "device-parameter-owner";
    }
    if (command === "end_owned_undo_gesture") return true;
    if (command === "set_effect_param_owned") return backend;
    throw new Error(`Unexpected device admission IPC ${command}`);
  });
  await store.initProject();
  invokeMock.mockClear();
});
afterEach(() => { cleanup(); resetAutomationWriteForTests(); });

describe("device range interaction-start admission", () => {
  it.each(["pointer", "accessibility"] as const)("keeps wet/dry %s admission across a same-type swap before release", async kind => {
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>(".device-viz-mix-slider")!;
    expect(slider).not.toBeNull();
    if (kind === "pointer") fireEvent.pointerDown(slider, { pointerId: 7, button: 0 });
    fireEvent.input(slider, { target: { value: "48" } });
    swapSameType();
    expect(view.container.querySelector(".device-viz-mix-slider")).toBe(slider);
    const effectsAfterSwap = structuredClone(backend.project.tracks[0].effects);
    if (kind === "pointer") fireEvent.pointerUp(window, { pointerId: 7, button: 0 });
    fireEvent.change(slider, { target: { value: "48" } });
    await Promise.resolve();
    expect(edits()).toEqual([]);
    expect(backend.project.tracks[0].effects).toEqual(effectsAfterSwap);
  });

  it("retains first accessibility input ownership for the native parameter slider", async () => {
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>(".device-slider")!;
    expect(slider).not.toBeNull();
    fireEvent.input(slider, { target: { value: "0.6" } });
    swapSameType();
    expect(view.container.querySelector(".device-slider")).toBe(slider);
    fireEvent.change(slider, { target: { value: "0.6" } });
    await Promise.resolve();
    expect(edits()).toEqual([]);
  });

  it("cancels wet/dry input without letting a trailing change create a new owner", async () => {
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>(".device-viz-mix-slider")!;
    fireEvent.pointerDown(slider, { pointerId: 8, button: 0 });
    fireEvent.input(slider, { target: { value: "60" } });
    fireEvent.pointerCancel(window, { pointerId: 8, button: 0 });
    fireEvent.change(slider, { target: { value: "60" } });
    await Promise.resolve();
    expect(edits()).toEqual([]);
  });

  it("closes a wet/dry keyboard step once and ignores its trailing compatibility change", async () => {
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>(".device-viz-mix-slider")!;
    fireEvent.keyDown(slider, { key: "ArrowRight" });
    fireEvent.change(slider, { target: { value: "31" } });
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned")).toHaveLength(1));
    expect(invokeMock).toHaveBeenCalledWith("begin_parameter_gesture", { scope: "preset", expectedStamp: firstStamp });
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "end_owned_undo_gesture")).toHaveLength(1));
  });
});


function discretePanel() {
  const source = FALLBACK_DEVICES.find(entry => entry.id === "dynamic-eq")!;
  const definition: DeviceDef = { ...source, id: "admission-discrete", category: "utility",
    params: source.params.filter(param => param.id === "band1_enabled" || param.id === "band1_channel") };
  backend = { ...backend, revision: backend.revision + 1,
    project: { ...backend.project, tracks: [{ ...backend.project.tracks[0], effects: [
      makeEffect("DynamicEq", [["band1_enabled", { Bool: false }], ["band1_channel", { String: "stereo" }]]),
      makeEffect("DynamicEq", [["band1_enabled", { Bool: true }], ["band1_channel", { String: "side" }]]),
    ] }] } };
  store.applyAiSnapshot(backend);
  const view = mountedDevice(definition);
  return { view,
    toggle: view.container.querySelector<HTMLButtonElement>('[data-param-id="band1_enabled"]')!,
    select: view.container.querySelector<HTMLButtonElement>('[data-param-id="band1_channel"]')!,
  };
}
function key(target: HTMLElement, type: "keydown" | "keyup", value: string) {
  const event = new KeyboardEvent(type, { key: value, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}
function optionOne() {
  return document.querySelector<HTMLButtonElement>('[role="option"][data-value="1"]')!;
}

describe("device discrete interaction-start admission", () => {
  it.each(["AX", "pointer", "Enter", "Space"] as const)("supports a normal %s toggle exactly once", async source => {
    const { toggle } = discretePanel();
    if (source === "AX") fireEvent.click(toggle, { detail: 0 });
    else if (source === "pointer") {
      fireEvent.pointerDown(toggle, { pointerId: 11, button: 0 });
      fireEvent.pointerUp(toggle, { pointerId: 11, button: 0 });
      fireEvent.click(toggle, { detail: 1 });
    } else {
      const value = source === "Space" ? " " : "Enter";
      expect(key(toggle, "keydown", value).defaultPrevented).toBe(true);
      if (source === "Space") expect(edits()).toHaveLength(0);
      expect(key(toggle, "keyup", value).defaultPrevented).toBe(true);
    }
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned")).toHaveLength(1));
    expect(invokeMock).toHaveBeenCalledWith("begin_parameter_gesture", { scope: "preset", expectedStamp: firstStamp });
    expect(invokeMock).toHaveBeenCalledWith("set_effect_param_owned", {
      token: "device-parameter-owner", target: "track:0", effectIndex: 0, paramName: "band1_enabled", paramValue: "true",
    });
  });

  it.each(["pointer", "Space"] as const)("keeps held %s toggle ownership across a same-type replacement", async source => {
    const { view, toggle } = discretePanel();
    if (source === "pointer") fireEvent.pointerDown(toggle, { pointerId: 12, button: 0 });
    else key(toggle, "keydown", " ");
    swapSameType();
    expect(view.container.querySelector('[data-param-id="band1_enabled"]')).toBe(toggle);
    if (source === "pointer") {
      fireEvent.pointerUp(toggle, { pointerId: 12, button: 0 });
      fireEvent.click(toggle, { detail: 1 });
    } else expect(key(toggle, "keyup", " ").defaultPrevented).toBe(true);
    await Promise.resolve();
    expect(edits()).toEqual([]);
  });

  it("suppresses canceled physical activation while allowing the first fresh AX click", async () => {
    const { toggle } = discretePanel();
    fireEvent.pointerDown(toggle, { pointerId: 13, button: 0 });
    swapSameType();
    fireEvent.pointerCancel(window, { pointerId: 13, button: 0 });
    fireEvent.click(toggle, { detail: 1 });
    expect(edits()).toEqual([]);
    fireEvent.click(toggle, { detail: 0 });
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned")).toHaveLength(1));
    expect(invokeMock).toHaveBeenCalledWith("begin_parameter_gesture", { scope: "preset", expectedStamp: movedStamp });
  });

  it.each(["Escape", "blur"] as const)("cancels held Space on %s without a native keyup click, then admits fresh AX", async cancel => {
    const { toggle } = discretePanel();
    expect(key(toggle, "keydown", " ").defaultPrevented).toBe(true);
    if (cancel === "Escape") key(toggle, "keydown", "Escape");
    else fireEvent.blur(toggle);
    expect(key(toggle, "keyup", " ").defaultPrevented).toBe(true);
    expect(edits()).toEqual([]);
    fireEvent.click(toggle, { detail: 0 });
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned")).toHaveLength(1));
  });

  it.each(["AX", "pointer", "keyboard"] as const)("captures enum admission on actual %s menu open before a same-type replacement", async source => {
    const { view, select } = discretePanel();
    if (source === "keyboard") key(select, "keydown", "ArrowDown");
    else fireEvent.click(select, { detail: source === "AX" ? 0 : 1 });
    const option = optionOne();
    expect(option).not.toBeNull();
    swapSameType();
    expect(view.container.querySelector('[data-param-id="band1_channel"]')).toBe(select);
    fireEvent.click(option);
    await Promise.resolve();
    expect(edits()).toEqual([]);
  });

  it("drops canceled menu selection and stale detached options without consuming a newer open", async () => {
    const { select } = discretePanel();
    fireEvent.click(select, { detail: 0 });
    const stale = optionOne();
    fireEvent.keyDown(window, { key: "Escape" });
    fireEvent.click(stale);
    expect(edits()).toEqual([]);
    swapSameType();
    fireEvent.click(select, { detail: 0 });
    const current = optionOne();
    expect(current).not.toBe(stale);
    fireEvent.click(stale);
    expect(edits()).toEqual([]);
    fireEvent.click(current);
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned")).toHaveLength(1));
    expect(invokeMock).toHaveBeenCalledWith("begin_parameter_gesture", { scope: "preset", expectedStamp: movedStamp });
  });
});


describe("live effect touch ownership", () => {
  function touchFixture() {
    const base = invokeMock.getMockImplementation()!;
    const pending: ((token: string | null) => void)[] = [];
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return new Promise<string | null>(resolve => pending.push(resolve));
      if (command === "update_owned_effect_param_touch" || command === "end_owned_effect_param_touch") return Promise.resolve(true);
      return base(command, args);
    });
    return pending;
  }
  function touchCalls(command: string) {
    return invokeMock.mock.calls.filter(([name]) => name === command).map(([, args]) => args);
  }
  function knobIn(view: ReturnType<typeof mountedDevice>) {
    return view.container.querySelector<HTMLElement>('[data-control-kind="knob"][data-param-id="mix"]')!;
  }

  it("releases a delayed old admission exactly once without clearing the newer hand indicator", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 80 });
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    expect(pending).toHaveLength(2);
    pending[1]("new-touch");
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    pending[0]("old-touch");
    await waitFor(() => expect(touchCalls("end_owned_effect_param_touch")).toEqual([
      { target: "track:0", effectIndex: 0, paramName: "mix", token: "old-touch" },
    ]));
    expect(knob).toHaveClass("touch-override");
    fireEvent.mouseMove(document, { clientY: 60 });
    await waitFor(() => expect(touchCalls("update_owned_effect_param_touch").length).toBeGreaterThan(0));
    expect(touchCalls("update_owned_effect_param_touch")).toEqual(expect.arrayContaining([
      expect.objectContaining({ token: "new-touch", target: "track:0", effectIndex: 0, paramName: "mix" }),
    ]));
    expect(touchCalls("update_owned_effect_param_touch").every(args => (args as { token: string }).token === "new-touch")).toBe(true);
    fireEvent.mouseUp(document);
    await waitFor(() => expect(touchCalls("end_owned_effect_param_touch")).toHaveLength(2));
    view.unmount();
    expect(touchCalls("end_owned_effect_param_touch")).toHaveLength(2);
    expect(edits()).toEqual([]);
  });

  it("does not retarget a delayed begin after a same-type swap or fall back to an ordinary edit", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 80 });
    expect(touchCalls("begin_guarded_effect_param_touch")).toEqual([
      expect.objectContaining({ target: "track:0", effectIndex: 0, paramName: "mix", expectedStamp: firstStamp }),
    ]);
    swapSameType();
    fireEvent.mouseUp(document);
    pending[0]("old-target-touch");
    await waitFor(() => expect(touchCalls("end_owned_effect_param_touch")).toEqual([
      { target: "track:0", effectIndex: 0, paramName: "mix", token: "old-target-touch" },
    ]));
    expect(touchCalls("update_owned_effect_param_touch")).toEqual([]);
    expect(edits()).toEqual([]);
    expect(knob).not.toHaveClass("touch-override");
  });

  it("does not open a successor after the prior close synchronously replaces its target", async () => {
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return Promise.resolve("first-touch");
      if (command === "end_owned_effect_param_touch") { swapSameType(); return Promise.resolve(true); }
      return base(command, args);
    });
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    expect(touchCalls("begin_guarded_effect_param_touch")).toEqual([
      expect.objectContaining({ expectedStamp: firstStamp }),
    ]);
    fireEvent.mouseMove(document, { clientY: 40 });
    fireEvent.mouseUp(document);
    expect(edits()).toEqual([]);
    expect(touchCalls("end_owned_effect_param_touch")).toHaveLength(1);
  });

  it("does not fall back to editing a replacement when an old begin grants no touch", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    fireEvent.mouseMove(document, { clientY: 60 });
    fireEvent.mouseUp(document);
    swapSameType();
    pending[0](null);
    await Promise.resolve();
    await Promise.resolve();
    expect(edits()).toEqual([]);
    expect(touchCalls("end_owned_effect_param_touch")).toEqual([]);
  });

  it("retires pending timers and closes late handles after unmount without reopening a hand", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>('[data-control-kind="slider"]')!;
    fireEvent.pointerDown(slider, { button: 0, pointerId: 3 });
    fireEvent.input(slider, { target: { value: "0.7" } });
    view.unmount();
    pending[0]("unmounted-touch");
    await waitFor(() => expect(touchCalls("end_owned_effect_param_touch")).toEqual([
      expect.objectContaining({ token: "unmounted-touch" }),
    ]));
    expect(touchCalls("update_owned_effect_param_touch")).toEqual([]);
    expect(edits()).toEqual([]);
  });

  it("keeps the replacement knob canonical during old mouse moves and accepts a fresh gesture", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    pending[0]("original-touch");
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    swapSameType();
    expect(knob.getAttribute("aria-valuenow")).toBe("80");
    fireEvent.mouseMove(document, { clientY: 0 });
    expect(knob.getAttribute("aria-valuenow")).toBe("80");
    expect(knob).not.toHaveClass("touch-override");
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    pending[1]("replacement-touch");
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    fireEvent.mouseMove(document, { clientY: 86 });
    expect(knob.getAttribute("aria-valuenow")).not.toBe("80");
    fireEvent.mouseUp(document);
    await waitFor(() => expect(knob.getAttribute("aria-valuenow")).toBe("80"));
    expect(edits()).toEqual([]);
  });

  it("retires stale slider input without a replacement draft and allows the next pointer intent", async () => {
    const pending = touchFixture();
    const view = mountedDevice();
    const slider = view.container.querySelector<HTMLInputElement>('[data-control-kind="slider"]')!;
    const readout = view.container.querySelector('[data-readout-param="decay"]')!;
    fireEvent.pointerDown(slider, { button: 0, pointerId: 3 });
    pending[0]("original-slider");
    await waitFor(() => expect(slider).toHaveClass("touch-override"));
    swapSameType();
    const canonical = slider.value;
    const canonicalText = readout.textContent;
    fireEvent.input(slider, { target: { value: "0.99" } });
    expect(slider.value).toBe(canonical);
    expect(readout.textContent).toBe(canonicalText);
    fireEvent.input(slider, { target: { value: "0.98" } });
    expect(slider.value).toBe(canonical);
    fireEvent.pointerDown(slider, { button: 0, pointerId: 9 });
    pending[1]("replacement-slider");
    await waitFor(() => expect(slider).toHaveClass("touch-override"));
    fireEvent.input(slider, { target: { value: "0.8" } });
    expect(Number(slider.value)).toBeCloseTo(0.8, 12);
    expect(readout.textContent).not.toBe(canonicalText);
    fireEvent.pointerUp(window, { pointerId: 9 });
    await waitFor(() => expect(slider.value).toBe(canonical));
    expect(edits()).toEqual([]);
  });

  it.each(["false", "throw"] as const)("cancels a physical drag when its live update returns %s, without a static fallback", async failure => {
    const base = invokeMock.getMockImplementation()!;
    let admissions = 0;
    let updates = 0;
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return Promise.resolve(`retire-touch-${++admissions}`);
      if (command === "update_owned_effect_param_touch") {
        updates += 1;
        if (updates === 1) return failure === "throw" ? Promise.reject(new Error("Touch retired")) : Promise.resolve(false);
        return Promise.resolve(true);
      }
      if (command === "end_owned_effect_param_touch") return Promise.resolve(true);
      return base(command, args);
    });
    const view = mountedDevice();
    const knob = knobIn(view);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    fireEvent.mouseMove(document, { clientY: 60 });
    await waitFor(() => expect(knob).not.toHaveClass("touch-override"));
    expect(knob.getAttribute("aria-valuenow")).toBe("30");
    fireEvent.mouseMove(document, { clientY: 0 });
    expect(knob.getAttribute("aria-valuenow")).toBe("30");
    fireEvent.mouseUp(document);
    expect(edits()).toEqual([]);
    expect(touchCalls("end_owned_effect_param_touch")).toEqual([
      expect.objectContaining({ token: "retire-touch-1" }),
    ]);
    fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
    await waitFor(() => expect(knob).toHaveClass("touch-override"));
    fireEvent.mouseMove(document, { clientY: 86 });
    await waitFor(() => expect(updates).toBe(2));
    expect(knob.getAttribute("aria-valuenow")).not.toBe("30");
    fireEvent.mouseUp(document);
    expect(edits()).toEqual([]);
  });

  it("drops an already scheduled hand update when its target is replaced before flush", async () => {
    vi.useFakeTimers();
    try {
      const pending = touchFixture();
      const view = mountedDevice();
      const knob = knobIn(view);
      fireEvent.mouseDown(knob, { button: 0, clientY: 100 });
      pending[0]("timed-touch");
      await Promise.resolve();
      fireEvent.mouseMove(document, { clientY: 60 });
      swapSameType();
      await vi.advanceTimersByTimeAsync(32);
      expect(touchCalls("update_owned_effect_param_touch")).toEqual([]);
      expect(touchCalls("end_owned_effect_param_touch")).toEqual([
        expect.objectContaining({ token: "timed-touch" }),
      ]);
      expect(knob).not.toHaveClass("touch-override");
      fireEvent.mouseUp(document);
      expect(edits()).toEqual([]);
    } finally { vi.useRealTimers(); }
  });
});
