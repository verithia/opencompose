import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "../setup";
import { FALLBACK_DEVICES, loadDevicesCatalog } from "../../devices/catalog";

beforeEach(() => {
  invokeMock.mockClear();
});

describe("built-in device catalog", () => {
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
