/**
 * Typed Tauri IPC barrel.
 *
 * Domain commands live in their own modules; this file is the stable import
 * path used by stores, components and tests.
 */
export * as aiApi from "../ai/api";

export * from "./projectApi";

export * from "./trackApi";

export * from "./noteApi";

export * from "./clipApi";

export * from "./sampleMarket";

export * from "./clipEditing";

export * from "./routingApi";

export * from "./automationApi";

export * from "./effectsApi";
export * from "./pluginApi";
export * from "./transportApi";

export * from "./appStatus";

export type {
  AbSnapshot,
  AuditionSnapshot,
  LoopState,
  PunchState,
  Quantise,
  TransportSnapshot,
} from "../types/transport";
