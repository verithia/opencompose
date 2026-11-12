/**
 * Automation lane reads and mutations.
 *
 * `api.ts` re-exports this module so the panel keeps its existing imports.
 */
import { invoke } from "@tauri-apps/api/core";
import type { AutomationMode, LaneLfo, ProjectSnapshot } from "../types/project";

/** The value a lane drives into one parameter, or why nothing drives it. */
export type AutomationReadout =
  | {
      status: "driven";
      /** Parameter-domain value, or the normalized position for a patch lane. */
      value: number;
      domain: "normalized" | "device";
      /** The lane's one-pole time constant; 0 keeps the raw curve values. */
      smoothing_ms: number;
    }
  | { status: "not_driven"; reason: string };

/**
 * Ask what drives one parameter at a transport position.
 *
 * `target` is a panel target (`track:N`, `bus:name`, `master`) and
 * `positionBeats` is the audible position in quarter notes. `laneIndex` keys
 * the lane when several lanes share the parameter.
 */
export const automationValueAt = (
  target: string,
  laneParam: string,
  positionBeats: number,
  laneIndex: number | null = null,
) =>
  invoke<AutomationReadout>("automation_value_at", {
    target,
    laneParam,
    laneIndex,
    positionBeats,
  });

export const setAutomationSmoothing = (
  target: string,
  laneParam: string,
  smoothingMs: number,
  laneIndex: number | null = null,
) =>
  invoke<ProjectSnapshot>("set_automation_smoothing", {
    target,
    laneParam,
    laneIndex,
    smoothingMs,
  });

export const setAutomationPointCurve = (
  target: string,
  laneParam: string,
  pointIndex: number,
  curve: string,
  tension: number | null,
  laneIndex: number | null = null,
) =>
  invoke<ProjectSnapshot>("set_automation_point_curve", {
    target,
    laneParam,
    laneIndex,
    pointIndex,
    curve,
    tension,
  });

/** Changes only the lane's mode; the cycle controls are kept as they were. */
export const setAutomationMode = (
  target: string,
  laneParam: string,
  mode: AutomationMode,
  laneIndex: number | null = null,
) => invoke<ProjectSnapshot>("set_automation_mode", { target, laneParam, laneIndex, mode });

export const setAutomationLfo = (
  target: string,
  laneParam: string,
  lfo: LaneLfo,
  laneIndex: number | null = null,
) => invoke<ProjectSnapshot>("set_automation_lfo", { target, laneParam, laneIndex, lfo });

/** Add one point at a quarter-note position. */
export const addAutomationPoint = (
  target: string,
  laneParam: string,
  laneIndex: number | null,
  positionBeats: number,
  value: number,
) =>
  invoke<ProjectSnapshot>("add_automation_point", {
    target,
    laneParam,
    laneIndex,
    positionBeats,
    value,
  });

/** Remove one point from a lane. */
export const removeAutomationPoint = (
  target: string,
  laneParam: string,
  laneIndex: number | null,
  pointIndex: number,
) =>
  invoke<ProjectSnapshot>("remove_automation_point", {
    target,
    laneParam,
    laneIndex,
    pointIndex,
  });

/** Move one point in both axes; the index stays the grabbed point. */
export const moveAutomationPoint = (
  target: string,
  laneParam: string,
  laneIndex: number | null,
  pointIndex: number,
  positionBeats: number,
  value: number,
) =>
  invoke<ProjectSnapshot>("move_automation_point", {
    target,
    laneParam,
    laneIndex,
    pointIndex,
    positionBeats,
    value,
  });

/** Mute or unmute one lane. */
export const setAutomationLaneMute = (
  target: string,
  laneParam: string,
  laneIndex: number | null,
  muted: boolean,
) =>
  invoke<ProjectSnapshot>("set_automation_lane_mute", {
    target,
    laneParam,
    laneIndex,
    muted,
  });

/**
 * Block gestures on an automation clip. `owner` is the panel target that owns
 * the lane; `laneIndex` keys it when several clips share one parameter.
 */
export const moveAutomationClip = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
  toBar: number,
  toBeat: number,
  toSubdivision: number,
) =>
  invoke<ProjectSnapshot>("move_automation_clip", {
    owner,
    laneParam,
    laneIndex,
    toBar,
    toBeat,
    toSubdivision,
  });

/** Move one edge of an automation clip; `edge` is `start` or `end`. */
export const resizeAutomationClip = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
  edge: "start" | "end",
  toBar: number,
  toBeat: number,
  toSubdivision: number,
) =>
  invoke<ProjectSnapshot>("resize_automation_clip", {
    owner,
    laneParam,
    laneIndex,
    edge,
    toBar,
    toBeat,
    toSubdivision,
  });

/** Copy a clip to the first free position at or after the asked one. */
export const duplicateAutomationClip = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
  atBar: number,
  atBeat: number,
  atSubdivision: number,
) =>
  invoke<ProjectSnapshot>("duplicate_automation_clip", {
    owner,
    laneParam,
    laneIndex,
    atBar,
    atBeat,
    atSubdivision,
  });

/** Split a clip at a position strictly inside its span. */
export const splitAutomationClip = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
  atBar: number,
  atBeat: number,
  atSubdivision: number,
) =>
  invoke<ProjectSnapshot>("split_automation_clip", {
    owner,
    laneParam,
    laneIndex,
    atBar,
    atBeat,
    atSubdivision,
  });

/** Remove an automation clip; a whole-timeline lane is refused. */
export const deleteAutomationClip = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
) =>
  invoke<ProjectSnapshot>("delete_automation_clip", {
    owner,
    laneParam,
    laneIndex,
  });

/** Remove one automation lane, whole-timeline or clip. */
export const removeAutomationLane = (
  owner: string,
  laneParam: string,
  laneIndex: number | null,
) =>
  invoke<ProjectSnapshot>("remove_automation_lane", {
    owner,
    laneParam,
    laneIndex,
  });
