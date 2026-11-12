import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  chooseDropdown,
  expectInvoked,
  invokeMock,
  makeEffect,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { EffectsRack } from "../../components/EffectsRack";
import { DevicePanel } from "../../components/DevicePanel";
import { FALLBACK_DEVICES } from "../../devices/catalog";
import { formatParamValue, toParamString } from "../../devices/params";
import type { Effect, EffectType, ParamValue, ProjectSnapshot } from "../../types/project";
import type { DeviceDef, DeviceParamDef } from "../../types/devices";

const eqDevice = FALLBACK_DEVICES.find((device) => device.id === "eq")!;
const compressionDevice = FALLBACK_DEVICES.find((device) => device.id === "compression")!;
const limiterDevice = FALLBACK_DEVICES.find((device) => device.id === "limiter")!;

// Device catalog entries the frontend learned late; ids match the engine registry.
const LATE_DEVICES = [
  { id: "breath-split", name: "Breath Split", category: "dynamics", params: [] },
  { id: "reverb-fdn", name: "FDN Reverb", category: "time", params: [] },
  { id: "resonance", name: "Resonance Suppressor", category: "dynamics", params: [] },
  { id: "tuner", name: "Tuner", category: "utility", params: [] },
] as const satisfies readonly DeviceDef[];

const LATE_EFFECT_TYPES = {
  "breath-split": "BreathSplit",
  "reverb-fdn": "ReverbFdn",
  resonance: "Resonance",
  tuner: "Tuner",
} as const satisfies Record<string, EffectType>;

const PLUG_SLOT_COLORS = ["#9c27b0", "rgb(156,39,176)"];

function slotColors(container: HTMLElement): string[] {
  return Array.from(container.querySelectorAll(".effect-slot-color"))
    .map((element) => (element as HTMLElement).style.background.replace(/\s/g, "").toLowerCase());
}

const BAND_FREQS = [100, 250, 500, 1000, 2000, 8000];

function continuousParam(
  id: string, name: string, unit: DeviceParamDef["unit"],
  min: number, max: number, value: number, curve: "linear" | "log",
): DeviceParamDef {
  return { id, name, unit, min, max, default: value, curve, steps: null };
}

function boolParam(id: string, name: string): DeviceParamDef {
  return {
    id, name, unit: "none", min: 0, max: 1, default: 0,
    curve: "linear", steps: 2, kind: "bool",
  };
}

function enumParam(id: string, name: string, options: string[]): DeviceParamDef {
  return {
    id, name, unit: "none", min: 0, max: options.length - 1, default: 0,
    curve: "linear", steps: options.length, kind: "enum", options,
  };
}

function bandParams(index: number, freq: number): DeviceParamDef[] {
  const prefix = `band${index}`;
  const label = `Band ${index}`;
  return [
    boolParam(`${prefix}_enabled`, `${label} Enabled`),
    boolParam(`${prefix}_solo`, `${label} Solo`),
    enumParam(`${prefix}_channel`, `${label} Channel`, ["Stereo", "Mid", "Side", "Left", "Right"]),
    enumParam(`${prefix}_type`, `${label} Type`,
      ["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut", "Notch", "Band Pass"]),
    continuousParam(`${prefix}_freq`, `${label} Frequency`, "Hz", 20, 20000, freq, "log"),
    continuousParam(`${prefix}_q`, `${label} Q`, "none", 0.1, 20, 1, "log"),
    continuousParam(`${prefix}_gain`, `${label} Gain`, "dB", -24, 24, 0, "linear"),
    boolParam(`${prefix}_dynamic`, `${label} Dynamic`),
    continuousParam(`${prefix}_threshold`, `${label} Threshold`, "dB", -60, 0, -30, "linear"),
    continuousParam(`${prefix}_range`, `${label} Range`, "dB", -24, 24, -6, "linear"),
    continuousParam(`${prefix}_attack`, `${label} Attack`, "ms", 0.1, 200, 10, "log"),
    continuousParam(`${prefix}_release`, `${label} Release`, "ms", 5, 2000, 100, "log"),
  ];
}

const dynamicEqDevice: DeviceDef = {
  id: "dynamic-eq",
  name: "Dynamic EQ",
  category: "eq",
  params: [
    boolParam("bypass", "Bypass"),
    ...BAND_FREQS.flatMap((freq, index) => bandParams(index + 1, freq)),
    boolParam("low_cut_enabled", "Low Cut Enabled"),
    continuousParam("low_cut_freq", "Low Cut Frequency", "Hz", 20, 20000, 20, "log"),
    continuousParam("low_cut_q", "Low Cut Q", "none", 0.1, 20, 0.707, "log"),
    boolParam("high_cut_enabled", "High Cut Enabled"),
    continuousParam("high_cut_freq", "High Cut Frequency", "Hz", 20, 20000, 20000, "log"),
    continuousParam("high_cut_q", "High Cut Q", "none", 0.1, 20, 0.707, "log"),
  ],
};

function dynamicHarness(params: [string, ParamValue][] = []) {
  return (
    <DevicePanel
      target="track:0"
      index={0}
      count={1}
      effect={makeEffect("DynamicEq", params)}
      device={dynamicEqDevice}
      onClose={() => {}}
    />
  );
}

function meterSnapshot() {
  return {
    available: true,
    playing: true,
    master: { gain_reduction_db: 0, effects: [] },
    tracks: [{
      index: 0,
      gain_reduction_db: 7.5,
      input_peak_db: -12,
      output_peak_db: -18,
      effects: [7.5],
    }],
    buses: [],
  };
}

let eqParams: Record<string, ParamValue> = {};
let effectCount = 1;

function currentSnapshot(): ProjectSnapshot {
  const effects: Effect[] = [];
  for (let index = 0; index < effectCount; index += 1) {
    effects.push(makeEffect("Eq", index === 0 ? Object.entries(eqParams) : []));
  }
  return makeSnapshot({ tracks: [{ ...makeTrack("Piano", 0), effects }] });
}

function parseParamValue(value: string): ParamValue {
  if (value.endsWith("Hz")) return { Frequency: Number.parseFloat(value) };
  if (value.endsWith("dB")) return { Db: Number.parseFloat(value) };
  if (value.endsWith("%")) return { Percentage: Number.parseFloat(value) };
  if (value.endsWith("ms")) return { Time: Number.parseFloat(value) / 1000 };
  if (value === "true") return { Bool: true };
  if (value === "false") return { Bool: false };
  const numeric = Number.parseFloat(value);
  return Number.isFinite(numeric) ? { Number: numeric } : { String: value };
}

function setParamCalls(): { paramName: string; paramValue: string }[] {
  const single = invokeMock.mock.calls
    .filter(([command]) => command === "set_effect_param_owned")
    .map(([, args]) => args as { paramName: string; paramValue: string });
  const batch = invokeMock.mock.calls
    .filter(([command]) => command === "set_effect_params_owned")
    .flatMap(([, args]) => {
      const { params } = args as { params: [string, string][] };
      return (params ?? []).map(([paramName, paramValue]) => ({ paramName, paramValue }));
    });
  return [...single, ...batch];
}

function PanelHarness() {
  return (
    <DevicePanel
      target="track:0"
      index={0}
      count={1}
      effect={store.project.tracks[0].effects[0]}
      device={eqDevice}
      onClose={() => {}}
    />
  );
}

function RackHarness() {
  return (
    <EffectsRack
      target="track:0"
      effects={store.project.tracks[0].effects}
      label="Effects"
    />
  );
}

const SCANNED_PLUGINS = [{
  id: "com.example.verb",
  name: "Example Verb",
  vendor: "Acme Audio",
  format: "Vst3",
  category: "Effect",
  path: "/plugins/Example Verb.vst3",
  num_params: 0,
  version: null,
  architectures: [],
  state_abi: null,
}];

async function defaultInvoke(command: string, args?: unknown) {
  if (command === "begin_guarded_effect_param_touch") return null;
  if (command === "devices_catalog") return { devices: FALLBACK_DEVICES };
  if (command === "scan_plugins") return SCANNED_PLUGINS;
  if (command === "get_transport_state") return TRANSPORT_STOPPED();
  if (command === "get_undo_state") {
    return {
      can_undo: false, can_redo: false,
      undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
    };
  }
  if (command === "begin_parameter_gesture") return `effect-owner-${invokeMock.mock.calls.length}`;
  if (command === "end_owned_undo_gesture") return true;
  if (command === "get_project") return currentSnapshot();
  if (command === "add_effect") {
    effectCount = 1;
    eqParams = {};
    return currentSnapshot();
  }
  if (command === "remove_effect") {
    effectCount = 0;
    eqParams = {};
    return currentSnapshot();
  }
  if (command === "set_effect_param_owned") {
    const { effectIndex, paramName, paramValue } = args as {
      effectIndex: number;
      paramName: string;
      paramValue: string;
    };
    if (effectIndex === 0) eqParams[paramName] = parseParamValue(paramValue);
    return currentSnapshot();
  }
  if (command === "set_effect_params_owned") {
    const { effectIndex, params } = args as {
      effectIndex: number;
      params: [string, string][];
    };
    if (effectIndex === 0) {
      for (const [paramName, paramValue] of params) {
        eqParams[paramName] = parseParamValue(paramValue);
      }
    }
    return currentSnapshot();
  }
  if (command === "meter_snapshot") return meterSnapshot();
  return currentSnapshot();
}

beforeEach(async () => {
  invokeMock.mockReset();
  eqParams = {};
  effectCount = 1;
  invokeMock.mockImplementation(defaultInvoke);
  await store.initProject();
});

describe("DevicePicker", () => {
  it("renders built-in devices grouped by category and filters the list", () => {
    const { container, getByLabelText } = render(() => (
      <EffectsRack target="track:0" effects={[]} label="Effects" />
    ));
    fireEvent.click(getByLabelText("Add an effect"));

    expect(container.querySelector("[data-device-picker]")).toBeTruthy();
    for (const category of ["EQ", "Dynamics", "Time", "Color", "Utility"]) {
      expect(container.textContent).toContain(category);
    }
    expect(container.textContent).toContain("Compressor");
    expect(container.textContent).toContain("Built-in devices first");

    fireEvent.input(getByLabelText("Search devices"), { target: { value: "rev" } });

    const filteredIds = Array.from(container.querySelectorAll<HTMLElement>("[data-device-id]"))
      .map((device) => device.dataset.deviceId).sort();
    expect(filteredIds).toEqual(["reverb", "reverb-fdn"]);
    expect(container.querySelector('[data-device-id="eq"]')).toBeNull();
  });

  it("lists a scanned plugin effect in its own group after the built-ins", async () => {
    const { container, getByLabelText } = render(() => (
      <EffectsRack target="track:0" effects={[]} label="Effects" />
    ));
    fireEvent.click(getByLabelText("Add an effect"));

    await vi.waitFor(() => {
      expect(container.querySelector('[data-plugin-id="com.example.verb"]')).toBeTruthy();
    });
    const titles = [...container.querySelectorAll(".device-picker-group-title")]
      .map((title) => title.textContent);
    expect(titles.at(-1)).toBe("Plugins");
    expect(container.querySelector('[data-plugin-id="com.example.verb"]')?.textContent)
      .toContain("Acme Audio · VST3");

    fireEvent.input(getByLabelText("Search devices"), { target: { value: "acme" } });

    expect(container.querySelectorAll("[data-device-id]")).toHaveLength(0);
    fireEvent.click(container.querySelector('[data-plugin-id="com.example.verb"]')!);
    await vi.waitFor(() => {
      expectInvoked("add_effect", {
        target: "track:0",
        effectType: "plugin:com.example.verb",
      });
    });
  });

  it("adds a searched plugin effect with Enter", async () => {
    const { getByLabelText } = render(() => (
      <EffectsRack target="track:0" effects={[]} label="Effects" />
    ));
    fireEvent.click(getByLabelText("Add an effect"));
    const search = getByLabelText("Search devices");
    fireEvent.input(search, { target: { value: "example verb" } });

    await vi.waitFor(() => {
      expect(document.querySelector('[data-plugin-id="com.example.verb"]')).toBeTruthy();
    });
    fireEvent.keyDown(search, { key: "Enter" });

    await vi.waitFor(() => {
      expectInvoked("add_effect", {
        target: "track:0",
        effectType: "plugin:com.example.verb",
      });
    });
  });

  it("switches to the real devices_catalog response when it is available", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "devices_catalog") {
        return {
          devices: [{
            id: "eq",
            name: "Studio EQ",
            category: "eq",
            params: [],
          }],
        };
      }
      return defaultInvoke(command, args);
    });

    const { container, getByLabelText } = render(() => (
      <EffectsRack target="track:0" effects={[]} label="Effects" />
    ));
    fireEvent.click(getByLabelText("Add an effect"));

    await vi.waitFor(() => {
      expect(container.querySelector('[data-device-id="eq"]')?.textContent).toContain("Studio EQ");
    });
  });

  it("moves the active option with arrow keys and adds it with Enter", async () => {
    const { getByLabelText } = render(() => (
      <EffectsRack target="track:0" effects={[]} label="Effects" />
    ));
    fireEvent.click(getByLabelText("Add an effect"));
    const search = getByLabelText("Search devices");

    fireEvent.keyDown(search, { key: "ArrowDown" });
    fireEvent.keyDown(search, { key: "Enter" });

    await vi.waitFor(() => {
      expectInvoked("add_effect", { target: "track:0", effectType: "passive-eq" });
    });
  });
});

describe("EffectsRack device panel", () => {
  it("adds the selected device and opens its panel", async () => {
    effectCount = 0;
    await store.initProject();
    const { container, getByLabelText } = render(() => <RackHarness />);

    fireEvent.click(getByLabelText("Add an effect"));
    fireEvent.click(container.querySelector('[data-device-id="eq"]')!);

    await vi.waitFor(() => {
      expect(container.querySelector('[data-device-panel][data-device-id="eq"]')).toBeTruthy();
    });
    expectInvoked("add_effect", { target: "track:0", effectType: "eq" });
  });

  it("draws the device glyph in every rack slot", () => {
    const effects: Effect[] = [
      makeEffect("Opto"),
      makeEffect("PassiveEq"),
      makeEffect("DuckedSend"),
      { effect_type: { Plugin: "Acme.Verb" }, params: [] },
    ];
    const { container } = render(() => <EffectsRack target="track:0" effects={effects} label="FX" />);
    const icons = Array.from(container.querySelectorAll(".effect-slot .effect-icon"));
    expect(icons.map((icon) => icon.getAttribute("data-effect")))
      .toEqual(["Opto", "PassiveEq", "DuckedSend", "Plugin"]);
  });

  it("gives the late families their own rack glyph and colour", () => {
    const effects = LATE_DEVICES.map((device) => makeEffect(LATE_EFFECT_TYPES[device.id]));
    const { container } = render(() => <EffectsRack target="track:0" effects={effects} label="FX" />);
    const icons = Array.from(container.querySelectorAll(".effect-slot .effect-icon"));
    expect(icons.map((icon) => icon.getAttribute("data-effect")))
      .toEqual(["BreathSplit", "ReverbFdn", "Resonance", "Tuner"]);
    expect(slotColors(container)).toHaveLength(LATE_DEVICES.length);
    expect(new Set(slotColors(container)).size).toBe(LATE_DEVICES.length);
    for (const colour of slotColors(container)) expect(PLUG_SLOT_COLORS).not.toContain(colour);
  });

  it("adds each late family from the picker and shows the right glyph", async () => {
    let effects: Effect[] = [];
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "devices_catalog") return { devices: LATE_DEVICES };
      if (command === "add_effect") {
        const { effectType } = args as { effectType: keyof typeof LATE_EFFECT_TYPES };
        effects = [...effects, makeEffect(LATE_EFFECT_TYPES[effectType])];
        return makeSnapshot({ tracks: [{ ...makeTrack("Piano", 0), effects }] });
      }
      return defaultInvoke(command, args);
    });
    const { container, getByLabelText } = render(() => <RackHarness />);

    for (const device of LATE_DEVICES) {
      fireEvent.click(getByLabelText("Add an effect"));
      await vi.waitFor(() => {
        expect(container.querySelector(`[data-device-picker] [data-device-id="${device.id}"]`)).toBeTruthy();
      });
      fireEvent.click(container.querySelector(`[data-device-picker] [data-device-id="${device.id}"]`)!);
      await vi.waitFor(() => {
        expect(container.querySelectorAll(".effect-slot .effect-icon")).toHaveLength(effects.length);
      });
    }

    expectInvoked("add_effect", { target: "track:0", effectType: "tuner" });
    const icons = Array.from(container.querySelectorAll(".effect-slot .effect-icon"));
    expect(icons.map((icon) => icon.getAttribute("data-effect")))
      .toEqual(["BreathSplit", "ReverbFdn", "Resonance", "Tuner"]);
    for (const colour of slotColors(container)) expect(PLUG_SLOT_COLORS).not.toContain(colour);
  });
});

describe("DevicePanel", () => {
  it("renders enum parameters as a themed listbox trigger", async () => {
    const device: DeviceDef = {
      id: "select-device",
      name: "Select Device",
      category: "utility",
      params: [enumParam("mode", "Mode", ["Soft", "Hard"])],
    };
    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Eq", [["mode", { Number: 1 }]])}
        device={device}
        onClose={() => {}}
      />
    ));
    const trigger = container.querySelector<HTMLElement>('[data-param-id="mode"][data-control-kind="select"]')!;
    expect(trigger.tagName).toBe("BUTTON");
    expect(trigger.getAttribute("aria-haspopup")).toBe("listbox");
    expect(trigger.textContent).toContain("Hard");

    chooseDropdown(trigger, "Soft");
    await vi.waitFor(() => {
      expect(setParamCalls().some((call) => call.paramName === "mode" && call.paramValue === "soft")).toBe(true);
    });
  });

  it("formats schema values and clamps keyboard adjustments", async () => {
    const { container } = render(() => <PanelHarness />);

    expect(container.querySelector('[data-readout-param="low_freq"]')?.textContent).toBe("200 Hz");
    expect(container.querySelector('[data-readout-param="low_gain"]')?.textContent).toBe("0.0 dB");

    const lowFreq = container.querySelector('[data-param-id="low_freq"]') as HTMLInputElement;
    expect(lowFreq.dataset.controlKind).toBe("slider");
    fireEvent.change(lowFreq, { target: { value: "1" } });

    await vi.waitFor(() => {
      expect(container.querySelector('[data-readout-param="low_freq"]')?.textContent).toBe("20 kHz");
    });
    expect(setParamCalls().find((call) => call.paramName === "low_freq")?.paramValue).toBe("20000Hz");

    const gainKnob = container.querySelector('[data-param-id="low_gain"]') as HTMLElement;
    const gainDef = eqDevice.params.find((param) => param.id === "low_gain")!;
    expect(gainKnob.dataset.controlKind).toBe("knob");

    fireEvent.keyDown(gainKnob, { key: "End" });
    await vi.waitFor(() => {
      expect(container.querySelector('[data-readout-param="low_gain"]')?.textContent)
        .toBe(formatParamValue(gainDef, gainDef.max));
      expect(setParamCalls().filter((call) => call.paramName === "low_gain").at(-1)?.paramValue)
        .toBe(toParamString(gainDef, gainDef.max));
    });

    fireEvent.keyDown(gainKnob, { key: "Home" });
    await vi.waitFor(() => {
      expect(container.querySelector('[data-readout-param="low_gain"]')?.textContent)
        .toBe(formatParamValue(gainDef, gainDef.min));
      expect(setParamCalls().filter((call) => call.paramName === "low_gain").at(-1)?.paramValue)
        .toBe(toParamString(gainDef, gainDef.min));
    });
  });

  it("reflects an external param change from the shared store", async () => {
    const { container } = render(() => <PanelHarness />);
    expect(container.querySelector('[data-readout-param="mid_gain"]')?.textContent).toBe("0.0 dB");

    store.applyAiSnapshot(makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        effects: [makeEffect("Eq", [["mid_gain", { Db: 6 }]])],
      }],
    }));

    await vi.waitFor(() => {
      expect(container.querySelector('[data-readout-param="mid_gain"]')?.textContent).toBe("+6.0 dB");
    });
    expect(setParamCalls()).toHaveLength(0);
  });

  it("hands an automated parameter to the held hand and back on release", async () => {
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return "live-touch-token";
      if (command === "update_owned_effect_param_touch" || command === "end_owned_effect_param_touch") {
        return true;
      }
      return defaultInvoke(command, args);
    });
    const { container } = render(() => <PanelHarness />);
    const knob = container.querySelector('[data-param-id="low_gain"]') as HTMLElement;

    fireEvent.mouseDown(knob, { clientX: 0, clientY: 100 });
    await vi.waitFor(() => {
      expect(knob.classList.contains("touch-override")).toBe(true);
    });
    expect(
      invokeMock.mock.calls.some(([command]) => command === "begin_guarded_effect_param_touch"),
    ).toBe(true);

    fireEvent.mouseMove(document, { clientX: 0, clientY: 80 });
    await vi.waitFor(() => {
      expect(
        invokeMock.mock.calls.some(([command]) => command === "update_owned_effect_param_touch"),
      ).toBe(true);
    });
    expect(setParamCalls()).toHaveLength(0);

    fireEvent.mouseUp(document);
    await vi.waitFor(() => {
      expect(
        invokeMock.mock.calls.some(([command]) => command === "end_owned_effect_param_touch"),
      ).toBe(true);
      expect(knob.classList.contains("touch-override")).toBe(false);
    });
    expect(setParamCalls()).toHaveLength(0);
  });

  it("takes over an automated slider drag without a project edit", async () => {
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return "live-touch-token";
      if (command === "update_owned_effect_param_touch" || command === "end_owned_effect_param_touch") {
        return true;
      }
      return defaultInvoke(command, args);
    });
    const { container } = render(() => <PanelHarness />);
    const slider = container.querySelector('[data-param-id="low_freq"]') as HTMLInputElement;

    fireEvent.pointerDown(slider);
    fireEvent.input(slider, { target: { value: "0.8" } });
    await vi.waitFor(() => {
      expect(
        invokeMock.mock.calls.some(([command]) => command === "update_owned_effect_param_touch"),
      ).toBe(true);
      expect(slider.classList.contains("touch-override")).toBe(true);
    });
    fireEvent.pointerUp(slider);
    fireEvent.change(slider, { target: { value: "0.8" } });

    await vi.waitFor(() => {
      expect(
        invokeMock.mock.calls.some(([command]) => command === "end_owned_effect_param_touch"),
      ).toBe(true);
      expect(slider.classList.contains("touch-override")).toBe(false);
    });
    expect(setParamCalls()).toHaveLength(0);
  });

  it("keeps the ordinary edit when no lane owns the parameter", async () => {
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "begin_guarded_effect_param_touch") return null;
      return defaultInvoke(command, args);
    });
    const { container } = render(() => <PanelHarness />);
    const knob = container.querySelector('[data-param-id="low_gain"]') as HTMLElement;

    fireEvent.mouseDown(knob, { clientX: 0, clientY: 100 });
    fireEvent.mouseMove(document, { clientX: 0, clientY: 80 });
    fireEvent.mouseUp(document);

    await vi.waitFor(() => {
      expect(setParamCalls().some((call) => call.paramName === "low_gain")).toBe(true);
    });
    expect(knob.classList.contains("touch-override")).toBe(false);
    expect(
      invokeMock.mock.calls.some(([command]) => command === "update_owned_effect_param_touch"),
    ).toBe(false);
  });

  it("renders live gain reduction and peak readouts for dynamics devices", async () => {
    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Compression", [])}
        device={compressionDevice}
        onClose={() => {}}
      />
    ));

    await vi.waitFor(() => {
      const meter = container.querySelector('[data-meter="gain-reduction"]');
      expect(meter?.getAttribute("data-meter-available")).toBe("true");
      expect(container.querySelector("[data-meter-gr]")?.textContent).toBe("7.5 dB");
    });
    expect(container.querySelector(".device-meter-fill")).toBeTruthy();
    expect(container.querySelector("[data-meter-history]")).toBeNull();
    expect(container.textContent).not.toContain("Metering not wired");
    expect(container.textContent).toContain("In -12.0 dB");
    expect(container.textContent).toContain("Out -18.0 dB");
  });

  it("feeds a bounded gain-reduction history strip from the meter", async () => {
    let reductionDb = 7.5;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "meter_snapshot") {
        return {
          available: true,
          playing: true,
          master: { gain_reduction_db: 0, effects: [] },
          tracks: [{
            index: 0,
            gain_reduction_db: reductionDb,
            input_peak_db: -12,
            output_peak_db: -18,
            effects: [reductionDb],
          }],
          buses: [],
        };
      }
      return defaultInvoke(command, args);
    });

    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Limiter", [])}
        device={limiterDevice}
        onClose={() => {}}
      />
    ));

    const history = () => container.querySelector("[data-meter-history]");
    expect(history()).toBeTruthy();
    expect(history()?.getAttribute("data-history-samples")).toBe("0");
    expect(container.querySelector("[data-history-window]")?.textContent).toBe("Last 6 s");
    expect(container.querySelector("[data-history-peak]")?.textContent).toContain(
      "No gain reduction yet",
    );

    reductionDb = 9;
    await vi.waitFor(() => {
      const bars = container.querySelectorAll("[data-history-bar]");
      expect(bars.length).toBeGreaterThan(0);
      expect(Number(history()?.getAttribute("data-history-samples"))).toBeLessThanOrEqual(60);
      expect((bars[bars.length - 1] as HTMLElement).style.height).toBe("75%");
    });
    expect(container.querySelector("[data-history-peak]")?.textContent).toBe("Peak 9.0 dB");

    reductionDb = 0;
    await vi.waitFor(() => {
      const bars = container.querySelectorAll("[data-history-bar]");
      expect((bars[bars.length - 1] as HTMLElement).style.height).toBe("0%");
    });
  });

  it("does not fill the history while the transport is stopped", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "meter_snapshot") {
        return {
          available: true,
          playing: false,
          master: { gain_reduction_db: 0, effects: [] },
          tracks: [{ index: 0, gain_reduction_db: 4, effects: [4] }],
          buses: [],
        };
      }
      return defaultInvoke(command, args);
    });

    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Limiter", [])}
        device={limiterDevice}
        onClose={() => {}}
      />
    ));

    await vi.waitFor(() => {
      const polls = invokeMock.mock.calls.filter(([command]) => command === "meter_snapshot");
      expect(polls.length).toBeGreaterThanOrEqual(2);
    });
    expect(
      container.querySelector("[data-meter-history]")?.getAttribute("data-history-samples"),
    ).toBe("0");
    expect(container.querySelector("[data-history-peak]")?.textContent).toContain(
      "No gain reduction yet",
    );
    expect(container.textContent).toContain("Idle");
  });

  it("renders the FET bias and ballistics and its meter", async () => {
    const fetDevice = FALLBACK_DEVICES.find((device) => device.id === "fet")!;
    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Fet", [
          ["bias", { Percentage: 75 }],
          ["attack", { Time: 0.0003 }],
          ["release", { Time: 0.12 }],
        ])}
        device={fetDevice}
        onClose={() => {}}
      />
    ));

    await vi.waitFor(() => {
      const meter = container.querySelector('[data-meter="gain-reduction"]');
      expect(meter?.getAttribute("data-meter-available")).toBe("true");
    });
    expect(container.querySelector('[data-readout-param="bias"]')?.textContent).toBe("75%");
    expect(container.querySelector('[data-readout-param="attack"]')?.textContent).toBe("0.3 ms");
    expect(container.querySelector('[data-readout-param="release"]')?.textContent).toBe("120 ms");
    expect(container.querySelector('[data-param-id="drive"]')).toBeTruthy();
    expect(container.querySelector('[data-param-id="output"]')).toBeTruthy();
  });

  it("renders the parametric EQ and its band controls for the dynamic EQ", async () => {
    const { container } = render(() => dynamicHarness());

    expect(container.querySelector("[data-parametric-eq]")).toBeTruthy();
    expect(container.querySelector('[data-param-id="band1_enabled"]')).toBeNull();
    expect(container.querySelectorAll("[data-peq-node]")).toHaveLength(8);
    expect(container.querySelectorAll("[data-peq-band]")).toHaveLength(8);

    const enable = container.querySelector('[data-band-enable="band1"]') as HTMLElement;
    expect(enable.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(enable);
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band1_enabled" && call.paramValue === "true",
      )).toBe(true);
    });

    const select = container.querySelector('[data-band-type="band1"]') as HTMLSelectElement;
    expect(select.value).toBe("0");
    fireEvent.change(select, { target: { value: "3" } });
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band1_type" && call.paramValue === "low_cut",
      )).toBe(true);
    });
  });

  it("commits one batch entry per drag gesture", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
      ["band1_freq", { Frequency: 1000 }],
      ["band1_gain", { Db: 3 }],
    ]));
    const svg = container.querySelector(".peq-canvas") as SVGSVGElement;
    svg.getBoundingClientRect = () => ({
      x: 0, y: 0, left: 0, top: 0, right: 620, bottom: 300, width: 620, height: 300,
      toJSON: () => ({}),
    } as DOMRect);
    const node = container.querySelector('[data-peq-node="band1"]') as HTMLElement;

    fireEvent.mouseDown(node, { clientX: 300, clientY: 150 });
    for (let step = 1; step <= 12; step += 1) {
      fireEvent.mouseMove(document, { clientX: 300 + step * 4, clientY: 150 - step * 2 });
    }
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_params_owned")).toHaveLength(0);
    expect(container.querySelector(".peq-readout")).toBeTruthy();
    fireEvent.mouseUp(document);

    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([command]) => command === "set_effect_params_owned");
      expect(calls).toHaveLength(1);
      const params = (calls[0][1] as { params: [string, string][] }).params;
      expect(params.map(([name]) => name)).toEqual(["band1_freq", "band1_gain"]);
      expect(params[0][1]).toMatch(/Hz$/);
      expect(params[1][1]).toMatch(/dB$/);
    });
    expect(container.querySelector(".peq-readout")).toBeNull();
  });

  it("changes Q with the wheel and coalesces a burst into one edit", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
      ["band1_q", { Number: 1 }],
    ]));
    const node = container.querySelector('[data-peq-node="band1"]') as HTMLElement;
    fireEvent.wheel(node, { deltaY: -100 });
    fireEvent.wheel(node, { deltaY: -100 });
    fireEvent.wheel(node, { deltaY: -100 });
    expect(setParamCalls().some((call) => call.paramName === "band1_q")).toBe(false);

    await vi.waitFor(() => {
      const calls = setParamCalls().filter((call) => call.paramName === "band1_q");
      expect(calls).toHaveLength(1);
      expect(Number.parseFloat(calls[0].paramValue)).toBeGreaterThan(1);
    }, { timeout: 1500 });
  });

  it("adjusts a dynamic band range with Alt-drag", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
      ["band1_dynamic", { Bool: true }],
      ["band1_freq", { Frequency: 1000 }],
      ["band1_gain", { Db: 0 }],
      ["band1_range", { Db: -6 }],
    ]));
    const svg = container.querySelector(".peq-canvas") as SVGSVGElement;
    svg.getBoundingClientRect = () => ({
      x: 0, y: 0, left: 0, top: 0, right: 620, bottom: 300, width: 620, height: 300,
      toJSON: () => ({}),
    } as DOMRect);
    const node = container.querySelector('[data-peq-node="band1"]') as HTMLElement;

    fireEvent.mouseDown(node, { clientX: 300, clientY: 150, altKey: true });
    fireEvent.mouseMove(document, { clientX: 300, clientY: 190, altKey: true });
    fireEvent.mouseUp(document);

    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([command]) => command === "set_effect_params_owned");
      expect(calls).toHaveLength(1);
      const params = (calls[0][1] as { params: [string, string][] }).params;
      expect(params.map(([name]) => name)).toEqual(["band1_range"]);
      expect(Number.parseFloat(params[0][1])).toBeLessThan(-6);
    });
  });

  it("resets a band on double-click with one batch", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
      ["band1_freq", { Frequency: 2000 }],
      ["band1_gain", { Db: 6 }],
      ["band1_q", { Number: 2 }],
    ]));
    const node = container.querySelector('[data-peq-node="band1"]') as HTMLElement;
    fireEvent.doubleClick(node);

    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([command]) => command === "set_effect_params_owned");
      expect(calls).toHaveLength(1);
      const params = (calls[0][1] as { params: [string, string][] }).params;
      expect(params).toEqual([
        ["band1_freq", "100Hz"],
        ["band1_gain", "0dB"],
        ["band1_q", "1"],
      ]);
    });
  });

  it("adds and removes bands through the enable flags", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
    ]));
    const add = container.querySelector("[data-peq-add]") as HTMLElement;
    fireEvent.click(add);
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band2_enabled" && call.paramValue === "true",
      )).toBe(true);
    });

    const remove = container.querySelector('[data-band-remove="band1"]') as HTMLElement;
    fireEvent.click(remove);
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band1_enabled" && call.paramValue === "false",
      )).toBe(true);
    });
  });

  it("offers band-pass and dynamic types plus channel targets", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
      ["band1_type", { Number: 5 }],
      ["band1_channel", { String: "side" }],
    ]));
    const type = container.querySelector('[data-band-type="band1"]') as HTMLSelectElement;
    expect([...type.options].map((option) => option.textContent)).toContain("Band Pass");
    expect([...type.options].map((option) => option.value)).toContain("dynamic");
    expect(type.value).toBe("5");

    fireEvent.change(type, { target: { value: "dynamic" } });
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band1_dynamic" && call.paramValue === "true",
      )).toBe(true);
    });

    const channel = container.querySelector('[data-band-channel="band1"]') as HTMLSelectElement;
    expect(channel.value).toBe("2");
    fireEvent.change(channel, { target: { value: "1" } });
    await vi.waitFor(() => {
      expect(setParamCalls().some(
        (call) => call.paramName === "band1_channel" && call.paramValue === "mid",
      )).toBe(true);
    });
  });

  it("shows hover and selection states on band nodes", async () => {
    const { container } = render(() => dynamicHarness([
      ["band1_enabled", { Bool: true }],
    ]));
    const node = container.querySelector('[data-peq-node="band1"]') as HTMLElement;
    fireEvent.mouseEnter(node);
    expect(node.classList.contains("hovered")).toBe(true);
    fireEvent.mouseLeave(node);
    expect(node.classList.contains("hovered")).toBe(false);
    fireEvent.click(node);
    expect(node.classList.contains("selected")).toBe(true);
  });
});

describe("EqCurveEditor", () => {
  it("drags a band handle and writes the matching freq and gain params", async () => {
    const { container } = render(() => <PanelHarness />);
    const svg = container.querySelector(".eq-curve") as SVGSVGElement;
    svg.getBoundingClientRect = () => ({
      x: 0, y: 0, left: 0, top: 0, right: 280, bottom: 110, width: 280, height: 110,
      toJSON: () => ({}),
    } as DOMRect);
    const handle = container.querySelector('[data-eq-handle="low"]')!;

    fireEvent.mouseDown(handle, { clientX: 40, clientY: 40 });
    fireEvent.mouseMove(document, { clientX: 100, clientY: 55 });
    fireEvent.mouseUp(document);

    await vi.waitFor(() => {
      const calls = setParamCalls();
      expect(calls.some((call) => call.paramName === "low_freq" && call.paramValue.endsWith("Hz"))).toBe(true);
      expect(calls.some((call) => call.paramName === "low_gain" && call.paramValue === "0dB")).toBe(true);
    });
    await vi.waitFor(() => {
      expect(container.querySelector('[data-readout-param="low_freq"]')?.textContent).not.toBe("200 Hz");
    });
  });

  it("disables a band by writing 0 dB and restores it on re-enable", async () => {
    const { container } = render(() => <PanelHarness />);
    const toggle = container.querySelector('[data-band-toggle="low"]') as HTMLElement;

    fireEvent.click(toggle);
    await vi.waitFor(() => {
      expect(setParamCalls().some((call) => call.paramName === "low_gain" && call.paramValue === "0dB")).toBe(true);
      expect(toggle.getAttribute("aria-pressed")).toBe("false");
    });

    fireEvent.click(toggle);
    await vi.waitFor(() => {
      expect(setParamCalls().some((call) => call.paramName === "low_gain" && call.paramValue === "3dB")).toBe(true);
      expect(toggle.getAttribute("aria-pressed")).toBe("true");
    });
  });

  it("commits one wet/dry edit per drag, on release", async () => {
    const device = FALLBACK_DEVICES.find((entry) => entry.id === "reverb")!;
    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={makeEffect("Reverb", [["mix", { Number: 30 }]])}
        device={device}
        onClose={() => {}}
      />
    ));
    const slider = container.querySelector<HTMLInputElement>(".device-viz-mix-slider")!;
    expect(slider.value).toBe("30");

    // A drag streams inputs; none of them may write the document.
    for (const value of ["31", "35", "40", "48"]) {
      fireEvent.input(slider, { target: { value } });
    }
    expect(slider.value).toBe("48");
    expect(
      setParamCalls().filter((call) => call.paramName === "mix"),
    ).toHaveLength(0);

    // The release is the one undoable edit.
    fireEvent.change(slider, { target: { value: "48" } });
    await vi.waitFor(() => {
      expect(
        setParamCalls().filter((call) => call.paramName === "mix"),
      ).toHaveLength(1);
    });
  });
});

describe("PassiveEqCurve", () => {
  it("draws the combined response with one marker per corner", () => {
    const device = FALLBACK_DEVICES.find((entry) => entry.id === "passive-eq")!;
    const effect = makeEffect("PassiveEq", [
      ["low_freq", { Frequency: 100 }],
      ["low_boost", { Db: 5 }],
      ["low_cut", { Db: 5 }],
    ]);
    const { container } = render(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={effect}
        device={device}
        onClose={() => {}}
      />
    ));
    expect(container.querySelector("[data-passive-eq-curve]")).toBeTruthy();
    const path = container.querySelector(".eq-curve-line") as SVGPathElement;
    expect(path.getAttribute("d")).toContain("M");
    expect(path.getAttribute("d")!.length).toBeGreaterThan(100);
    expect(container.querySelectorAll("[data-passive-marker]").length).toBe(4);
  });
});
