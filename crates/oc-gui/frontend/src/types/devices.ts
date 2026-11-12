import type { UnitId, UnitDefinition } from "../data/units.gen";

/**
 * Built-in device metadata contract shared with the `devices_catalog` command.
 * Keep this in sync with the engine metadata registry.
 */

export type DeviceCategory = "eq" | "dynamics" | "time" | "color" | "utility";

export type DeviceParamUnit = UnitId;

export type DeviceParamCurve = "linear" | "log";

/** Control rendering kind; absent means `continuous` for older catalogs. */
export type DeviceParamKind = "continuous" | "bool" | "enum";

export interface DeviceParamDef {
  id: string;
  name: string;
  unit: DeviceParamUnit;
  min: number;
  max: number;
  default: number;
  curve?: DeviceParamCurve;
  /** Parameter help provided by the canonical device schema. */
  description?: string;
  /** Number of discrete positions; null/absent means continuous. */
  steps?: number | null;
  kind?: DeviceParamKind;
  /** Enum labels indexed by value; empty for other kinds. */
  options?: string[];
}

/** What a declared device port carries. */
export type DevicePortKind = "audio" | "control";

/** One declared device port; `sums: false` is silent by construction. */
export interface DevicePortDef {
  id: string;
  name: string;
  kind: DevicePortKind;
  sums: boolean;
}

export interface DeviceDef {
  id: string;
  name: string;
  category: DeviceCategory;
  params: DeviceParamDef[];
  /** Declared inputs; missing/empty keeps the historical one audio input. */
  ports?: DevicePortDef[];
  /** Optional extension; the base contract does not require it. */
  description?: string;
}

export interface DevicesCatalog {
  /** Missing on legacy catalog v1 responses. */
  version?: number;
  units?: UnitDefinition[];
  devices: DeviceDef[];
}

/** Values in the device's declared unit: switches are bools, enums are names. */
export type DevicePresetParams = Record<string, number | string | boolean>;

/** One device preset, merged from the shipped file and the user library. */
export interface DevicePreset {
  id: string;
  /** The catalog device id this preset belongs to. */
  device: string;
  name: string;
  category: string;
  /** One authored sentence saying what the preset is for; shown as written. */
  intent?: string;
  params: DevicePresetParams;
  source: "factory" | "user";
  /** True when a user preset replaced a factory preset with the same id. */
  shadows: boolean;
  /** False for factory rows; Delete is disabled with a reason. */
  deletable: boolean;
}

export interface DevicePresetListing {
  presets: DevicePreset[];
  /** One line per file the loader refused; the rest of the library still lists. */
  warnings: string[];
  /** Set by a save: whether an entry with that name was replaced. */
  replaced: boolean;
}
