/**
 * Device preset row: the factory/user picker, loading, saving, deleting, the
 * "(edited)" marker, and every disabled reason and failure line.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  chooseDropdown,
  expectInvoked,
  getLastCallArgs,
  invokeMock,
  makeEffect,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { DevicePanel } from "../../components/DevicePanel";
import { FALLBACK_DEVICES } from "../../devices/catalog";
import { i18n } from "../../stores/i18n";
import type { DeviceDef, DevicePreset, DevicePresetListing } from "../../types/devices";
import type { ParamValue, ProjectSnapshot } from "../../types/project";

/**
 * The FDN reverb's four aliased controls and the canonical controls they read:
 * a preset save must keep the canonical side only.
 */
const reverbFdnDevice: DeviceDef = {
  id: "reverb-fdn",
  name: "FDN Reverb",
  category: "time",
  params: [
    { id: "decay", name: "Decay", unit: "ms", min: 200, max: 10000, default: 2500, curve: "log" },
    { id: "space", name: "Space", unit: "%", min: 0, max: 100, default: 50 },
    { id: "brightness", name: "Brightness", unit: "%", min: 0, max: 100, default: 50 },
    { id: "damping", name: "Damping", unit: "%", min: 0, max: 100, default: 30 },
    { id: "character", name: "Character", unit: "%", min: 0, max: 100, default: 30 },
    { id: "modulation", name: "Modulation", unit: "%", min: 0, max: 100, default: 0 },
    { id: "distance", name: "Distance", unit: "%", min: 0, max: 100, default: 50 },
    { id: "early_late", name: "Early / Late", unit: "%", min: 0, max: 100, default: 0 },
    { id: "mix", name: "Mix", unit: "%", min: 0, max: 100, default: 30 },
  ],
};

const REVERB_DERIVED = ["decay", "damping", "modulation", "early_late"];

const eqDevice = FALLBACK_DEVICES.find((device) => device.id === "eq")!;

const FACTORY: DevicePreset = {
  id: "factory.eq.clear-presence",
  device: "eq",
  name: "Clear Presence",
  category: "eq",
  intent: "Lifts the presence band of a lead voice.",
  params: { mid_gain: 2.5 },
  source: "factory",
  shadows: false,
  deletable: false,
};

const USER: DevicePreset = {
  id: "user.eq.mine",
  device: "eq",
  name: "Mine",
  category: "eq",
  params: { mid_gain: -3 },
  source: "user",
  shadows: false,
  deletable: true,
};

/** A user preset that replaced the factory row with the same id. */
const SHADOWED: DevicePreset = {
  ...FACTORY,
  name: "Clear Mine",
  params: { mid_gain: 1 },
  source: "user",
  shadows: true,
  deletable: true,
};

let eqParams: [string, ParamValue][] = [];
let listing: DevicePresetListing = { presets: [], warnings: [], replaced: false };

function paramValue(value: number | string | boolean): ParamValue {
  if (typeof value === "boolean") return { Bool: value };
  if (typeof value === "number") return { Number: value };
  return { String: value };
}

function parseParamValue(text: string): ParamValue {
  if (text.endsWith("Hz")) return { Frequency: Number.parseFloat(text) };
  if (text.endsWith("dB")) return { Db: Number.parseFloat(text) };
  if (text.endsWith("%")) return { Percentage: Number.parseFloat(text) };
  if (text.endsWith("ms")) return { Time: Number.parseFloat(text) / 1000 };
  if (text === "true") return { Bool: true };
  if (text === "false") return { Bool: false };
  const numeric = Number.parseFloat(text);
  return Number.isFinite(numeric) ? { Number: numeric } : { String: text };
}

function currentSnapshot(): ProjectSnapshot {
  return makeSnapshot({
    tracks: [{ ...makeTrack("Piano", 0), effects: [makeEffect("Eq", eqParams)] }],
  });
}

function presets() {
  return { presets: listing.presets, warnings: listing.warnings, replaced: false };
}

const METERS = {
  available: true,
  playing: false,
  master: { gain_reduction_db: 0, effects: [] },
  tracks: [],
  buses: [],
};

async function invoke(command: string, args?: unknown) {
  if (command === "devices_catalog") return { devices: FALLBACK_DEVICES };
  if (command === "get_transport_state") return TRANSPORT_STOPPED();
  if (command === "get_undo_state") {
    return {
      can_undo: false, can_redo: false,
      undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
    };
  }
  if (command === "get_project") return currentSnapshot();
  if (command === "meter_snapshot") return METERS;
  if (command === "device_presets") return presets();
  if (command === "apply_device_preset") {
    const { id } = args as { id: string };
    const preset = listing.presets.find((entry) => entry.id === id);
    if (!preset) throw "No device preset has that id";
    eqParams = Object.entries(preset.params).map(([name, value]) => [name, paramValue(value)]);
    return currentSnapshot();
  }
  if (command === "save_device_preset") {
    const { name, params } = args as { name: string; params: Record<string, unknown> };
    const saved: DevicePreset = {
      id: `user.eq.${name.toLowerCase()}`,
      device: "eq",
      name,
      category: "eq",
      params: params as DevicePreset["params"],
      source: "user",
      shadows: false,
      deletable: true,
    };
    listing = { ...listing, presets: [...listing.presets, saved] };
    return { ...presets(), replaced: false };
  }
  if (command === "delete_device_preset") {
    const { id } = args as { id: string };
    listing = { ...listing, presets: listing.presets.filter((entry) => entry.id !== id) };
    return presets();
  }
  if (command === "begin_parameter_gesture") return `effect-owner-${invokeMock.mock.calls.length}`;
  if (command === "end_owned_undo_gesture") return true;
  if (command === "set_effect_param_owned") {
    const { paramName, paramValue: text } = args as {
      paramName: string;
      paramValue: string;
    };
    eqParams = [...eqParams.filter(([name]) => name !== paramName), [paramName, parseParamValue(text)]];
    return currentSnapshot();
  }
  if (command === "set_effect_params_owned") {
    const { params } = args as { params: [string, string][] };
    for (const [paramName, text] of params) {
      eqParams = [...eqParams.filter(([name]) => name !== paramName), [paramName, parseParamValue(text)]];
    }
    return currentSnapshot();
  }
  return currentSnapshot();
}

function Harness() {
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

function ReverbHarness() {
  return (
    <DevicePanel
      target="track:0"
      index={0}
      count={1}
      effect={makeEffect("ReverbFdn", [["space", { Number: 30 }]])}
      device={reverbFdnDevice}
      onClose={() => {}}
    />
  );
}

function picker(container: HTMLElement): HTMLButtonElement {
  const element = container.querySelector<HTMLButtonElement>("[data-preset-picker]");
  if (!element) throw new Error("the preset picker is not rendered");
  return element;
}

/** Wait until the library has been read; the picker renders empty before that. */
async function ready(container: HTMLElement) {
  await vi.waitFor(() => {
    expect(container.querySelector("[data-device-presets]")?.getAttribute("data-preset-loading"))
      .toBe("false");
  });
}

function options(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".oc-dropdown-option")];
}

function labels(): (string | null | undefined)[] {
  return options().map((element) => element.querySelector(".oc-dropdown-option-label")?.textContent);
}

function notes(): (string | null | undefined)[] {
  return options().map((element) => element.querySelector(".oc-dropdown-option-note")?.textContent);
}

beforeEach(async () => {
  i18n.setLocale("en");
  invokeMock.mockReset();
  eqParams = [];
  listing = { presets: [FACTORY, USER], warnings: [], replaced: false };
  invokeMock.mockImplementation(invoke);
  await store.initProject();
});

describe("device presets", () => {
  it("lists factory and user presets and marks the source of each", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);

    fireEvent.click(picker(container));

    expect(labels()).toEqual([
      i18n.t("device.presets.choose"),
      FACTORY.name,
      USER.name,
    ]);
    expect(notes().slice(1)).toEqual([
      i18n.t("device.presets.sourceFactory"),
      i18n.t("device.presets.sourceUser"),
    ]);
  });

  it("shows the authored intent under the picker, and nothing without one", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);
    expect(container.querySelector("[data-preset-intent]")).toBeNull();

    chooseDropdown(picker(container), FACTORY.name);
    await vi.waitFor(() => {
      expect(container.querySelector("[data-preset-intent]")?.textContent).toBe(FACTORY.intent);
    });

    // The user preset carries no intent, so the line disappears with it.
    chooseDropdown(picker(container), USER.name);
    await vi.waitFor(() => {
      expect(picker(container).textContent).toContain(USER.name);
    });
    expect(container.querySelector("[data-preset-intent]")).toBeNull();
  });

  it("marks a user preset that replaces a factory preset", async () => {
    listing = { presets: [SHADOWED, USER], warnings: [], replaced: false };
    const { container } = render(() => <Harness />);
    await ready(container);

    fireEvent.click(picker(container));

    expect(labels()).toContain(SHADOWED.name);
    expect(notes()).toContain(i18n.t("device.presets.shadowsFactory"));
  });

  it("applies the chosen preset in one command and shows it selected", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);

    chooseDropdown(picker(container), USER.name);

    await vi.waitFor(() => {
      expect(picker(container).textContent).toContain(USER.name);
    });
    expectInvoked("apply_device_preset", {
      id: USER.id,
      target: "track:0",
      effectIndex: 0,
      device: "eq",
    });
    expect(store.project.tracks[0].effects[0].params).toContainEqual([
      "mid_gain",
      { Number: -3 },
    ]);
  });

  it("marks the loaded preset edited after a control changes", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);
    chooseDropdown(picker(container), USER.name);
    await vi.waitFor(() => expect(picker(container).textContent).toContain(USER.name));
    expect(picker(container).textContent).not.toContain("(edited)");

    const knob = container.querySelector('[data-param-id="mid_gain"]');
    expect(knob).toBeTruthy();
    fireEvent.keyDown(knob!, { key: "ArrowUp" });

    await vi.waitFor(() => {
      expect(picker(container).textContent).toContain(i18n.t("device.presets.edited", { name: USER.name }));
    });
  });

  it("validates the name in the save dialog and saves every control", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);

    fireEvent.click(container.querySelector("[data-preset-save]")!);
    const dialog = document.querySelector(".text-prompt");
    expect(dialog).toBeTruthy();
    const confirm = dialog!.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    expect(confirm.disabled).toBe(true);

    const input = dialog!.querySelector<HTMLInputElement>("input")!;
    fireEvent.input(input, { target: { value: "Warm" } });
    expect(confirm.disabled).toBe(false);
    fireEvent.click(confirm);

    await vi.waitFor(() => expectInvoked("save_device_preset", {
      device: "eq",
      name: "Warm",
      replace: false,
    }));
    const args = getLastCallArgs("save_device_preset") as {
      params: Record<string, number | string | boolean>;
    };
    expect(Object.keys(args.params).sort()).toEqual(
      eqDevice.params.map((param) => param.id).sort(),
    );
    expect(args.params.mid_gain).toBe(0);
  });

  it("saves one side of every reverb alias pair", async () => {
    const { container } = render(() => <ReverbHarness />);
    await ready(container);

    fireEvent.click(container.querySelector("[data-preset-save]")!);
    const dialog = document.querySelector(".text-prompt");
    const input = dialog!.querySelector<HTMLInputElement>("input")!;
    fireEvent.input(input, { target: { value: "Reverb Warm" } });
    fireEvent.click(dialog!.querySelector<HTMLButtonElement>('button[type="submit"]')!);

    await vi.waitFor(() => expectInvoked("save_device_preset", {
      device: "reverb-fdn",
      name: "Reverb Warm",
      replace: false,
    }));
    const args = getLastCallArgs("save_device_preset") as {
      params: Record<string, number | string | boolean>;
    };
    const expected = reverbFdnDevice.params
      .map((param) => param.id)
      .filter((id) => !REVERB_DERIVED.includes(id))
      .sort();
    expect(Object.keys(args.params).sort()).toEqual(expected);
    for (const alias of REVERB_DERIVED) expect(args.params).not.toHaveProperty(alias);
    // The canonical control still carries the value the user set.
    expect(args.params.space).toBe(30);
  });

  it("deletes a user preset after a confirmation", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);
    chooseDropdown(picker(container), USER.name);
    await vi.waitFor(() => expect(picker(container).textContent).toContain(USER.name));

    const remove = container.querySelector<HTMLButtonElement>("[data-preset-delete]")!;
    expect(remove.disabled).toBe(false);
    fireEvent.click(remove);

    const dialog = document.querySelector('[role="alertdialog"]');
    expect(dialog).toBeTruthy();
    expect(dialog!.getAttribute("aria-label")).toContain(USER.name);

    fireEvent.click(document.querySelector("[data-preset-confirm]")!);

    await vi.waitFor(() => expectInvoked("delete_device_preset", { id: USER.id }));
    await vi.waitFor(() => {
      expect(picker(container).textContent).toContain(i18n.t("device.presets.choose"));
    });
    expect(container.querySelector<HTMLButtonElement>("[data-preset-delete]")!.disabled).toBe(true);
  });

  it("refuses to delete a factory preset and says why", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);
    chooseDropdown(picker(container), FACTORY.name);
    await vi.waitFor(() => expect(picker(container).textContent).toContain(FACTORY.name));

    const remove = container.querySelector<HTMLButtonElement>("[data-preset-delete]")!;
    expect(remove.disabled).toBe(true);
    expect(remove.title).toBe(i18n.t("device.presets.factoryLocked"));
  });

  it("disables Delete until a preset is chosen", async () => {
    const { container } = render(() => <Harness />);
    await ready(container);

    const remove = container.querySelector<HTMLButtonElement>("[data-preset-delete]")!;
    expect(remove.disabled).toBe(true);
    expect(remove.title).toBe(i18n.t("device.presets.chooseFirst"));
  });

  it("shows a pending state and disables every control while applying", async () => {
    let release: (() => void) | null = null;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "apply_device_preset") {
        await new Promise<void>((resolve) => { release = () => resolve(); });
      }
      return invoke(command, args);
    });
    const { container } = render(() => <Harness />);
    await ready(container);

    fireEvent.click(picker(container));
    const option = options().find(
      (element) => element.querySelector(".oc-dropdown-option-label")?.textContent === USER.name,
    )!;
    fireEvent.click(option);

    await vi.waitFor(() => {
      expect(container.querySelector("[data-preset-pending]")?.textContent)
        .toBe(i18n.t("device.presets.applying"));
    });
    expect(picker(container).disabled).toBe(true);
    expect(container.querySelector<HTMLButtonElement>("[data-preset-save]")!.disabled).toBe(true);
    expect(container.querySelector<HTMLButtonElement>("[data-preset-delete]")!.disabled).toBe(true);
    expect(picker(container).title).toBe(i18n.t("device.presets.busy"));

    release!();
    await vi.waitFor(() => expect(container.querySelector("[data-preset-pending]")).toBeNull());
  });

  it("renders the refusal text and keeps the previous selection", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "apply_device_preset") {
        throw "'Mine' is a preset for Parametric EQ and cannot be applied to Compression";
      }
      return invoke(command, args);
    });
    const { container } = render(() => <Harness />);
    await ready(container);

    chooseDropdown(picker(container), USER.name);

    await vi.waitFor(() => {
      expect(container.querySelector("[data-preset-error]")?.textContent)
        .toBe(i18n.t("device.presets.failed", {
          message: "'Mine' is a preset for Parametric EQ and cannot be applied to Compression",
        }));
    });
    expect(picker(container).textContent).toContain(i18n.t("device.presets.choose"));
  });

  it("reports a listing it cannot read instead of crashing", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "device_presets") return makeSnapshot();
      return invoke(command, args);
    });
    const { container } = render(() => <Harness />);

    await vi.waitFor(() => {
      expect(container.querySelector("[data-preset-error]")?.textContent)
        .toBe(i18n.t("device.presets.failed", { message: i18n.t("device.presets.unreadable") }));
    });
    expect(container.querySelector("[data-preset-empty]")).toBeTruthy();
    expect(container.querySelector<HTMLButtonElement>("[data-preset-save]")!.disabled).toBe(false);
  });

  it("shows a quiet empty line for a device with no presets and keeps Save working", async () => {
    listing = {
      presets: [{ ...FACTORY, device: "compression", id: "factory.compression.bus", name: "Bus Glue" }],
      warnings: [],
      replaced: false,
    };
    const { container } = render(() => <Harness />);

    await vi.waitFor(() => {
      expect(container.querySelector("[data-preset-empty]")?.textContent)
        .toBe(i18n.t("device.presets.empty"));
    });
    expect(container.querySelector("[data-preset-picker]")).toBeNull();
    expect(container.querySelector<HTMLButtonElement>("[data-preset-save]")!.disabled).toBe(false);
  });
});
