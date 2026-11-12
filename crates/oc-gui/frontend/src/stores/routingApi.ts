/**
 * Mixer routing: sends, track outputs, control routes and channel overrides.
 *
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ProjectSnapshot } from "../types/project";

// ============================================================================
// Send routing
// ============================================================================

export const addSend = (
  trackIndex: number,
  busName: string,
  amount: number,
  preFader: boolean,
  sourceOwner: string | null = null,
  sourceOutput: number | null = null,
  pan: number | null = null,
) => invoke<ProjectSnapshot>("add_send", {
  trackIndex, busName, amount, preFader, sourceOwner, sourceOutput, pan,
});

export const setSend = (
  trackIndex: number,
  busName: string,
  amount: number | null,
  preFader: boolean | null,
  pan: number | null = null,
  sourceOwner: string | null = null,
  sourceOutput: number | null = null,
) => invoke<ProjectSnapshot>("set_send", {
  trackIndex, busName, amount, preFader, pan, sourceOwner, sourceOutput,
});

export const setTrackOutput = (trackIndex: number, output: string) =>
  invoke<ProjectSnapshot>("set_track_output", { trackIndex, output });

/** Route a track to one declared output port of its instrument (0-based). */
export const setTrackOutputPort = (trackIndex: number, port: number) =>
  invoke<ProjectSnapshot>("set_track_output_port", { trackIndex, port });

/** Connect a control source to a device control input in one mixer node. */
export const addControlRoute = (owner: string, target: string, source: string) =>
  invoke<ProjectSnapshot>("add_control_route", { owner, target, source });

/** Disconnect one device control input in a mixer node. */
export const removeControlRoute = (owner: string, target: string) =>
  invoke<ProjectSnapshot>("remove_control_route", { owner, target });

/** Move an existing send to another bus; level, tap and pan are untouched. */
export const setSendTarget = (
  trackIndex: number,
  fromBus: string,
  toBus: string,
  sourceOwner: string | null = null,
  sourceOutput: number | null = null,
) => invoke<ProjectSnapshot>("set_send_target", {
  trackIndex, fromBus, toBus, sourceOwner, sourceOutput,
});

export const removeSend = (
  trackIndex: number,
  busName: string,
  sourceOwner: string | null = null,
  sourceOutput: number | null = null,
) => invoke<ProjectSnapshot>("remove_send", {
  trackIndex, busName, sourceOwner, sourceOutput,
});

export const setTrackChannelOverrides = (
  trackIndex: number,
  displayName: string | null,
  color: string | null,
  soloSafe: boolean | null,
  polarityInverted: boolean | null,
) => invoke<ProjectSnapshot>("set_track_channel_overrides", {
  trackIndex, displayName, color, soloSafe, polarityInverted,
});
