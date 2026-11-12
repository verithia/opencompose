/**
 * Built-in device catalog: loads `devices_catalog` when the backend exposes it
 * and uses the generated Rust catalog while the backend is unavailable.
 */
import { createSignal } from "solid-js";

import * as api from "../stores/api";
import { DEVICE_CATALOG } from "../data/deviceCatalog.gen";
import { UNIT_DEFINITIONS } from "../data/units.gen";
import type {
  DeviceCategory,
  DeviceDef,
  DeviceParamDef,
  DeviceParamUnit,
  DevicePortDef,
  DevicesCatalog,
} from "../types/devices";

export const CATEGORY_ORDER: DeviceCategory[] = [
  "eq",
  "dynamics",
  "time",
  "color",
  "utility",
];

/** Same Rust-owned metadata as the live backend, available before startup. */
export const FALLBACK_DEVICES: DeviceDef[] = DEVICE_CATALOG.devices;

const UNITS: DeviceParamUnit[] = UNIT_DEFINITIONS.map((unit) => unit.id);

export function canonicalDeviceId(id: string): string {
  return id === "compressor" ? "compression" : id;
}

export function findDevice(devices: DeviceDef[], id: string): DeviceDef | undefined {
  return devices.find((device) => device.id === id) ?? FALLBACK_DEVICES.find((device) => device.id === id);
}

export function findParam(device: DeviceDef, id: string): DeviceParamDef | undefined {
  return device.params.find((entry) => entry.id === id);
}

/** The last registry a component loaded, so a pure helper can read a range. */
const [knownDevices, setKnownDevices] = createSignal<DeviceDef[] | null>(null);

/** The registry when one has loaded, else the local table. */
export function devicesCatalog(): DeviceDef[] {
  return knownDevices() ?? FALLBACK_DEVICES;
}

/** Fetch the registry; any transport or shape failure keeps the local table. */
export async function loadDevicesCatalog(): Promise<DeviceDef[]> {
  try {
    const catalog = await api.devicesCatalog();
    const devices = normalizeCatalog(catalog);
    if (devices) {
      setKnownDevices(devices);
      return devices;
    }
  } catch {
    // The command is optional until the metadata registry lands.
  }
  return FALLBACK_DEVICES;
}

function normalizeCatalog(raw: unknown): DeviceDef[] | null {
  if (!raw || typeof raw !== "object") return null;
  const devices = (raw as DevicesCatalog).devices;
  if (!Array.isArray(devices)) return null;
  const version = (raw as DevicesCatalog).version ?? 1;
  return devices
    .map((device) => normalizeDevice(device, version))
    .filter((device): device is DeviceDef => device !== null);
}

function normalizeDevice(raw: unknown, version: number): DeviceDef | null {
  if (!raw || typeof raw !== "object") return null;
  const candidate = raw as Partial<DeviceDef>;
  if (typeof candidate.id !== "string" || !candidate.id) return null;
  if (typeof candidate.name !== "string" || !candidate.name) return null;
  if (!isCategory(candidate.category)) return null;
  const canonical = version < 2
    ? FALLBACK_DEVICES.find((device) => device.id === canonicalDeviceId(candidate.id!))
    : undefined;
  const params = Array.isArray(candidate.params)
    ? candidate.params
        .map((entry) => {
          const param = normalizeParam(entry);
          if (param?.unit === "%" && canonical?.params.find((known) => known.id === param.id)?.unit === "fraction") {
            return { ...param, unit: "fraction" as const };
          }
          return param;
        })
        .filter((entry): entry is DeviceParamDef => entry !== null)
    : [];
  return {
    id: candidate.id,
    name: candidate.name,
    category: candidate.category,
    params,
    ...(Array.isArray(candidate.ports) ? { ports: candidate.ports.filter(isPort) } : {}),
    ...(typeof candidate.description === "string" ? { description: candidate.description } : {}),
  };
}

function isPort(raw: unknown): raw is DevicePortDef {
  if (!raw || typeof raw !== "object") return false;
  const port = raw as Partial<DevicePortDef>;
  return typeof port.id === "string" && typeof port.name === "string"
    && (port.kind === "audio" || port.kind === "control") && typeof port.sums === "boolean";
}

function normalizeParam(raw: unknown): DeviceParamDef | null {
  if (!raw || typeof raw !== "object") return null;
  const candidate = raw as Partial<DeviceParamDef>;
  if (typeof candidate.id !== "string" || !candidate.id) return null;
  if (typeof candidate.name !== "string" || !candidate.name) return null;
  if (!Number.isFinite(candidate.min) || !Number.isFinite(candidate.max)) return null;
  const min = candidate.min as number;
  const max = candidate.max as number;
  if (min > max) return null;
  const defaultValue = Number.isFinite(candidate.default)
    ? Math.min(max, Math.max(min, candidate.default as number))
    : min;
  const unit = UNITS.includes(candidate.unit as DeviceParamUnit)
    ? (candidate.unit as DeviceParamUnit)
    : "none";
  const steps = Number.isInteger(candidate.steps) && (candidate.steps as number) >= 2
    ? (candidate.steps as number)
    : null;
  const kind = candidate.kind === "bool"
    ? "bool"
    : candidate.kind === "enum"
      ? "enum"
      : "continuous";
  const options = kind === "enum" && Array.isArray(candidate.options)
    ? candidate.options.filter((option): option is string => typeof option === "string")
    : [];
  return {
    id: candidate.id,
    name: candidate.name,
    unit,
    min,
    max,
    default: kind === "bool" ? (defaultValue >= 0.5 ? 1 : 0) : defaultValue,
    curve: candidate.curve === "log" && min > 0 ? "log" : "linear",
    steps,
    ...(typeof candidate.description === "string" ? { description: candidate.description } : {}),
    ...(kind === "continuous" ? {} : { kind }),
    ...(options.length > 0 ? { options } : {}),
  };
}

function isCategory(value: unknown): value is DeviceCategory {
  return CATEGORY_ORDER.includes(value as DeviceCategory);
}
