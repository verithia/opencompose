/**
 * Effect parameters, device presets, buses, meters and sidechains.
 *
 * `api.ts` re-exports this module for the existing call surface.
 */
import { invoke } from "@tauri-apps/api/core";
import type { BarBeat, ParameterTargetStamp, PluginPortInfo, ProjectSnapshot } from "../types/project";
import type { DevicesCatalog, DevicePresetListing, DevicePresetParams } from "../types/devices";
import type { MeterSnapshot, MonoCheck } from "../types/meters";
import type { UndoGestureToken } from "./api";

// ============================================================================
// Effects / Buses / Plugins
// ============================================================================

export const addEffect = (target: string, effectType: string) =>
  invoke<ProjectSnapshot>("add_effect", { target, effectType });

export const removeEffect = (target: string, effectIndex: number) =>
  invoke<ProjectSnapshot>("remove_effect", { target, effectIndex });

export const moveEffect = (target: string, effectIndex: number, newIndex: number) =>
  invoke<ProjectSnapshot>("move_effect", { target, effectIndex, newIndex });

/** Set the effect's own bypass field, which every device honours. */
export const setEffectBypass = (target: string, effectIndex: number, bypass: boolean) =>
  invoke<ProjectSnapshot>("set_effect_bypass", { target, effectIndex, bypass });

export const setEffectParam = (target: string, effectIndex: number, paramName: string, paramValue: string) =>
  invoke<ProjectSnapshot>("set_effect_param", { target, effectIndex, paramName, paramValue });

/** One mutation for a whole gesture, so undo records a single entry. */
export const setEffectParams = (target: string, effectIndex: number, params: [string, string][]) =>
  invoke<ProjectSnapshot>("set_effect_params", { target, effectIndex, params });

export const setEffectBypassOwned = (token: UndoGestureToken, target: string, effectIndex: number, bypass: boolean) =>
  invoke<ProjectSnapshot>("set_effect_bypass_owned", { target, effectIndex, bypass, token });

export const setEffectParamOwned = (token: UndoGestureToken, target: string, effectIndex: number, paramName: string, paramValue: string) =>
  invoke<ProjectSnapshot>("set_effect_param_owned", { target, effectIndex, paramName, paramValue, token });

export const setEffectParamsOwned = (token: UndoGestureToken, target: string, effectIndex: number, params: [string, string][]) =>
  invoke<ProjectSnapshot>("set_effect_params_owned", { target, effectIndex, params, token });

import type { EffectTouchToken, OwnedParameterGestureEnd } from "../types/gesture";
export type { EffectTouchToken, OwnedParameterGestureEnd } from "../types/gesture";

export const beginGuardedEffectParamWrite = (target: string, effectIndex: number, param: string, expectedStamp: ParameterTargetStamp) =>
  invoke<UndoGestureToken>("begin_guarded_effect_param_write", { target, effectIndex, param, expectedStamp });
export const beginOwnedEffectParamWrite = (target: string, effectIndex: number, param: string) =>
  invoke<UndoGestureToken>("begin_owned_effect_param_write", { target, effectIndex, param });

export const updateOwnedEffectParamWrite = (token: UndoGestureToken, target: string, effectIndex: number, param: string, position: BarBeat, value: string) =>
  invoke<ProjectSnapshot>("update_owned_effect_param_write", { target, effectIndex, param, position, value, token });

export const endOwnedEffectParamWrite = (token: UndoGestureToken, target: string, effectIndex: number, param: string) =>
  invoke<OwnedParameterGestureEnd>("end_owned_effect_param_write", { target, effectIndex, param, token });

/** The factory and user device presets, merged with their source and shadowing. */
export const devicePresets = () => invoke<DevicePresetListing>("device_presets");

/** Save one user preset; `replace` names an existing user preset to overwrite. */
export const saveDevicePreset = (
  device: string,
  name: string,
  params: DevicePresetParams,
  replace: boolean,
) => invoke<DevicePresetListing>("save_device_preset", { device, name, params, replace });

/** Delete one user preset; a factory preset is refused with a sentence. */
export const deleteDevicePreset = (id: string) =>
  invoke<DevicePresetListing>("delete_device_preset", { id });

/** Apply one preset to an effect as a single undoable edit. */
export const applyDevicePreset = (
  id: string,
  target: string,
  effectIndex: number,
  device: string,
) => invoke<ProjectSnapshot>("apply_device_preset", { id, target, effectIndex, device });


export const setEffectSidechain = (
  target: string,
  effectIndex: number,
  source: string | null,
) => invoke<ProjectSnapshot>("set_effect_sidechain", { target, effectIndex, source });

/**
 * Open one effect-parameter automation write. `param` is the device's own
 * parameter id; the lane name is `fx<N>.<param>`, created on the first point.
 */
export const beginEffectParamWrite = (
  target: string,
  effectIndex: number,
  param: string,
) => invoke<void>("begin_effect_param_write", { target, effectIndex, param });

export const updateEffectParamWrite = (
  target: string,
  effectIndex: number,
  param: string,
  position: BarBeat,
  value: string,
) =>
  invoke<ProjectSnapshot>("update_effect_param_write", {
    target,
    effectIndex,
    param,
    position,
    value,
  });

export const endEffectParamWrite = (target: string, effectIndex: number, param: string) =>
  invoke<ProjectSnapshot>("end_effect_param_write", { target, effectIndex, param });

/**
 * Take over an automated effect parameter while the hand holds it. Resolves
 * `false` when no lane owns it or the transport is not streaming.
 */


export const beginGuardedEffectParamTouch = (
  target: string, effectIndex: number, paramName: string, value: number, expectedStamp: ParameterTargetStamp,
) => invoke<EffectTouchToken | null>("begin_guarded_effect_param_touch", { target, effectIndex, paramName, value, expectedStamp });

export const updateOwnedEffectParamTouch = (
  token: EffectTouchToken, target: string, effectIndex: number, paramName: string, value: number,
) => invoke<boolean>("update_owned_effect_param_touch", { target, effectIndex, paramName, value, token });

export const endOwnedEffectParamTouch = (
  token: EffectTouchToken, target: string, effectIndex: number, paramName: string,
) => invoke<boolean>("end_owned_effect_param_touch", { target, effectIndex, paramName, token });

export const beginEffectParamTouch = (
  target: string,
  effectIndex: number,
  paramName: string,
  value: number,
) => invoke<boolean>("begin_effect_param_touch", { target, effectIndex, paramName, value });

export const updateEffectParamTouch = (
  target: string,
  effectIndex: number,
  paramName: string,
  value: number,
) => invoke<void>("update_effect_param_touch", { target, effectIndex, paramName, value });

export const endEffectParamTouch = (target: string, effectIndex: number, paramName: string) =>
  invoke<void>("end_effect_param_touch", { target, effectIndex, paramName });

export const setSpectrum = (target: string | null) =>
  invoke<void>("set_spectrum", { target });

export const devicesCatalog = () => invoke<DevicesCatalog>("devices_catalog");

export const meterSnapshot = () => invoke<MeterSnapshot>("meter_snapshot");

/** Fold the monitored master to mono, or return it to stereo. */
export const setMonitorMono = (enabled: boolean) =>
  invoke<MonoCheck>("set_monitor_mono", { enabled });

/** Clear every latched clip indicator. */
export const resetMeterClip = () => invoke<void>("reset_meter_clip");

export const addBus = (name: string, routes: string[]) =>
  invoke<ProjectSnapshot>("add_bus", { name, routes });

export const setBusRoutes = (name: string, routes: string[]) =>
  invoke<ProjectSnapshot>("set_bus_routes", { name, routes });

export const updateBusMix = (
  name: string,
  volumeDb: number | null,
  mute: boolean | null,
  solo: boolean | null,
) => invoke<ProjectSnapshot>("update_bus_mix", { name, volumeDb, mute, solo });

export const setBusMute = (name: string, mute: boolean) =>
  invoke<ProjectSnapshot>("set_bus_mute", { name, mute });

export const setBusSolo = (name: string, solo: boolean) =>
  invoke<ProjectSnapshot>("set_bus_solo", { name, solo });

export const presetOutputPairs = (presetName: string, load = true) =>
  invoke<number>("preset_output_pairs", { presetName, load });

/** Declared output ports of a preset's instrument, in declaration order. */
export const presetOutputPorts = (presetName: string, load = false) =>
  invoke<PluginPortInfo[]>("preset_output_ports", { presetName, load });

/** Delete a bus. A referenced bus is rejected unless `reroute` is true. */
export const removeBus = (name: string, reroute = false) =>
  invoke<ProjectSnapshot>("remove_bus", { name, reroute });
