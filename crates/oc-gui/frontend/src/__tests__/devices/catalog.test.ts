import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "../setup";
import { FALLBACK_DEVICES, loadDevicesCatalog } from "../../devices/catalog";
import { deviceParameterName } from "../../devices/labels";
import { i18n } from "../../stores/i18n";

beforeEach(() => {
  invokeMock.mockClear();
});

describe("built-in device catalog", () => {
  it("exposes the swept delay stack with localized signed controls", () => {
    const device = FALLBACK_DEVICES.find((entry) => entry.id === "flanger")!;
    const voices = device.params.find((param) => param.id === "voices")!;
    const cross = device.params.find((param) => param.id === "stereo_cross")!;
    const wet = device.params.find((param) => param.id === "wet_gain")!;
    expect(voices).toMatchObject({ min: 1, max: 8, default: 1 });
    expect(cross).toMatchObject({ min: -100, max: 100, default: 0 });
    expect(wet).toMatchObject({ min: -200, max: 200, default: 100 });
    const prior = i18n.locale();
    try {
      i18n.setLocale("zh-Hans");
      expect(deviceParameterName(device, cross)).toBe("立体声交叉混合");
      i18n.setLocale("es");
      expect(deviceParameterName(device, wet)).toBe("Ganancia húmeda");
    } finally {
      i18n.setLocale(prior);
    }
  });

  it("adapts only known v1 fractional builtins without rescaling values", async () => {
    const mix = { id: "mix", name: "Mix", unit: "%", min: 0, max: 1, default: 0.4, curve: "linear", steps: null };
    for (const version of [1, 2]) {
      invokeMock.mockResolvedValue({ version, devices: ["compression", "compressor", "multiband", "external"].map((id) => ({ id, name: id, category: "dynamics", params: [mix] })) });
      const devices = await loadDevicesCatalog();
      for (const device of devices) {
        expect(device.params[0].default).toBe(0.4);
        expect(device.params[0].unit).toBe(version === 1 && device.id !== "external" ? "fraction" : "%");
      }
    }
  });

  it("ships complete schema ranges and declared routing ports before backend startup", () => {
    const eq = FALLBACK_DEVICES.find((device) => device.id === "eq")!;
    expect(eq.params.find((param) => param.id === "low_gain")).toMatchObject({ min: -24, max: 24, default: 0 });
    expect(eq.params.find((param) => param.id === "q")).toMatchObject({ min: 0.1, max: 20, default: 1 });
    const compression = FALLBACK_DEVICES.find((device) => device.id === "compression")!;
    expect(compression.ports).toEqual(expect.arrayContaining([
      { id: "key", name: "Key", kind: "audio", sums: false },
      { id: "control", name: "Control", kind: "control", sums: false },
    ]));
    for (const device of FALLBACK_DEVICES) {
      for (const param of device.params) {
        expect(param.default, `${device.id}.${param.id}`).toBeGreaterThanOrEqual(param.min);
        expect(param.default, `${device.id}.${param.id}`).toBeLessThanOrEqual(param.max);
      }
    }
  });

  it("keeps backend port and parameter help metadata", async () => {
    const source = FALLBACK_DEVICES.find((device) => device.id === "passive-eq")!;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "devices_catalog") return { devices: [source] };
      throw new Error(`Unexpected command ${cmd}`);
    });
    const [device] = await loadDevicesCatalog();
    expect(device.ports).toEqual(source.ports);
    expect(device.params.find((param) => param.description)).toBeDefined();
  });

  it("falls back to the local catalog when devices_catalog is unavailable", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "devices_catalog") throw new Error("unknown command devices_catalog");
      return {};
    });

    const devices = await loadDevicesCatalog();

    expect(devices).toHaveLength(FALLBACK_DEVICES.length);
    expect(devices.map((device) => device.id)).toContain("eq");
    expect(devices.map((device) => device.id)).toContain("stereo-width");
  });

  it("uses devices_catalog when the command returns a valid catalog", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "devices_catalog") {
        return {
          devices: [{
            id: "eq",
            name: "Studio EQ",
            category: "eq",
            params: [{
              id: "low_freq",
              name: "Low",
              unit: "Hz",
              min: 20,
              max: 20000,
              default: 250,
              curve: "log",
              steps: null,
            }],
          }],
        };
      }
      return {};
    });

    const devices = await loadDevicesCatalog();

    expect(devices).toHaveLength(1);
    expect(devices[0].name).toBe("Studio EQ");
    expect(devices[0].params[0].default).toBe(250);
  });

  it("normalizes malformed parameters and clamps their defaults", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "devices_catalog") {
        return {
          devices: [{
            id: "utility",
            name: "Utility",
            category: "utility",
            params: [{
              id: "amount",
              name: "Amount",
              unit: "furlongs",
              min: 0,
              max: 10,
              default: 99,
              curve: "log",
              steps: 3,
            }],
          }],
        };
      }
      return {};
    });

    const devices = await loadDevicesCatalog();

    expect(devices[0].params[0]).toMatchObject({
      unit: "none",
      default: 10,
      curve: "linear",
      steps: 3,
    });
  });

  it("keeps the local catalog when the command returns an invalid shape", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "devices_catalog") return { devices: "nope" };
      return {};
    });

    const devices = await loadDevicesCatalog();

    expect(devices).toEqual(FALLBACK_DEVICES);
  });
});


it("exposes the all-pass coefficient through shared effect identity and generic control mapping", async () => {
  const { effectDeviceId } = await import("../../devices/effectIds");
  const { normalizedToValue, valueToNormalized } = await import("../../devices/params");
  const device = FALLBACK_DEVICES.find((entry) => entry.id === effectDeviceId("AllPass"))!;
  const parameter = device.params.find((entry) => entry.id === "coefficient")!;
  expect(parameter).toMatchObject({ min: -0.999, max: 0.999, default: 0, curve: "linear" });
  expect(normalizedToValue(parameter, 0)).toBe(-0.999);
  expect(normalizedToValue(parameter, 1)).toBe(0.999);
  expect(valueToNormalized(parameter, 0)).toBe(0.5);
});

it("writes a signed frequency and independent sidebands from the mixer catalog", async () => {
  const { effectDeviceId, effectLaneName } = await import("../../devices/effectIds");
  const { toParamString, valueToNormalized } = await import("../../devices/params");
  const device = FALLBACK_DEVICES.find((entry) => entry.id === effectDeviceId("FrequencyShift"))!;
  const shift = device.params.find((entry) => entry.id === "shift")!;
  const right = device.params.find((entry) => entry.id === "sideband_right")!;
  expect(effectLaneName("FrequencyShift")).toBe("frequency_shift");
  expect(valueToNormalized(shift, 0)).toBe(0.5);
  expect(toParamString(shift, -180)).toBe("-180Hz");
  expect(toParamString(right, -100)).toBe("-100%");
});

it("exposes signed matrix gains, inter-channel offset and signal order", async () => {
  const { effectDeviceId, effectLaneName } = await import("../../devices/effectIds");
  const { toParamString } = await import("../../devices/params");
  const device = FALLBACK_DEVICES.find((entry) => entry.id === effectDeviceId("StereoMatrix"))!;
  const lookup = (id: string) => device.params.find((entry) => entry.id === id)!;
  expect(effectLaneName("StereoMatrix")).toBe("stereo_matrix");
  expect(toParamString(lookup("left_from_right"), -50)).toBe("-50%");
  expect(toParamString(lookup("offset"), -12)).toBe("-12ms");
  expect(toParamString(lookup("delay_before_matrix"), 1)).toBe("true");
});

it("offers a non-summing vocoder key and writes a whole band count", async () => {
  const { effectDeviceId } = await import("../../devices/effectIds");
  const { toParamString } = await import("../../devices/params");
  const device = FALLBACK_DEVICES.find((entry) => entry.id === effectDeviceId("Vocoder"))!;
  expect(device.ports).toEqual(expect.arrayContaining([expect.objectContaining({ id: "key", sums: false })]));
  expect(toParamString(device.params.find((entry) => entry.id === "bands")!, 31.6)).toBe("32");
});
