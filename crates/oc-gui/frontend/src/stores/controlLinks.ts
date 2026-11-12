/**
 * Control-link editor state: project fan-outs, the learn arm, and the
 * last-tweaked parameter the shortcut remembers.
 *
 * The project owns the records; this module only shapes the commands and the
 * transient UI state, so the undo entry always comes from the backend.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

import type {
  ControlLink,
  ControlLinkSource,
  ControlLinkTarget,
  LinkFormula,
  ProjectSnapshot,
} from "../types/project";
import type { MidiActivityEvent } from "./midiLive";
import { store } from "./project";

/** Which control's rows a learn arm will fill. */
export interface LearnRequest {
  track: string;
  param: string;
  formula: LinkFormula;
  smoothing_ms: number;
}

/** The parameter the user moved last; the one-click bind source. */
export interface TweakedParam {
  track: string;
  trackIndex: number;
  param: string;
  label: string;
}

const [lastTweakedSignal, setLastTweaked] = createSignal<TweakedParam | null>(null);
const [learn, setLearn] = createSignal<LearnRequest | null>(null);

/** Remember a parameter touch so "bind last tweaked" has a target. */
export function rememberTweaked(entry: TweakedParam): void {
  setLastTweaked(entry);
}

/** The parameter the user moved last, if any. */
export function lastTweaked(): TweakedParam | null {
  return lastTweakedSignal();
}

export function beginLearn(request: LearnRequest): void {
  setLearn(request);
}

export function cancelLearn(): void {
  setLearn(null);
}

/** The control list, reactive through the project store. */
export function controlLinks(): ControlLink[] {
  return store.project.properties.control_links ?? [];
}

/** MIDI sources are the only ones that can be learned from a live CC. */
export function isMidiSource(source: ControlLinkSource): source is Extract<ControlLinkSource, { kind: "midi_cc" }> {
  return source.kind === "midi_cc";
}

/** Default formula: raw CC rows start at the 0-127 scaler; macros/lanes at linear. */
export function defaultFormula(source: ControlLinkSource): LinkFormula {
  return isMidiSource(source) ? "midi_unit" : "linear";
}

export async function addControlTarget(
  source: ControlLinkSource,
  target: ControlLinkTarget,
): Promise<void> {
  const snapshot = await invoke<ProjectSnapshot>("add_control_link_target", { source, target });
  store.applyAiSnapshot(snapshot);
}

export async function removeControlTarget(
  source: ControlLinkSource,
  track: string,
  param: string,
): Promise<void> {
  const snapshot = await invoke<ProjectSnapshot>("remove_control_link_target", {
    source,
    track,
    param,
  });
  store.applyAiSnapshot(snapshot);
}

export async function updateControlTarget(
  source: ControlLinkSource,
  target: ControlLinkTarget,
  formula: LinkFormula,
  smoothing_ms: number,
): Promise<void> {
  const snapshot = await invoke<ProjectSnapshot>("update_control_link_target", {
    source,
    track: target.track,
    param: target.param,
    formula,
    smoothingMs: Math.max(0, Math.round(smoothing_ms)),
  });
  store.applyAiSnapshot(snapshot);
}

/**
 * Bind the remembered parameter to `source` in one action.
 * Returns false when nothing has been touched yet.
 */
export async function bindLastTweaked(source: ControlLinkSource): Promise<boolean> {
  const last = lastTweakedSignal();
  if (!last) return false;
  await addControlTarget(source, {
    track: last.track,
    param: last.param,
    formula: defaultFormula(source),
    smoothing_ms: 0,
  });
  return true;
}

/** Arm MIDI learn for the remembered parameter; the next CC creates the link. */
export function learnLastTweaked(): boolean {
  const last = lastTweakedSignal();
  if (!last) return false;
  setLearn({
    track: last.track,
    param: last.param,
    formula: "midi_unit",
    smoothing_ms: 0,
  });
  return true;
}

let handledEvent = "";

function eventKey(event: MidiActivityEvent, request: LearnRequest): string {
  return `${event.millis}:${event.channel}:${event.controller}:${request.track}:${request.param}`;
}

export function learning(): LearnRequest | null {
  return learn();
}

/**
 * Apply one live MIDI event to the armed learn request. A CC writes exactly one
 * record for the learned target and then disarms, so a held controller cannot
 * append rows.
 */
export async function learnFromMidiActivity(event: MidiActivityEvent): Promise<boolean> {
  const request = learn();
  if (!request) return false;
  if (event.kind !== "control_change" || event.controller === null) return false;
  const key = eventKey(event, request);
  if (key === handledEvent) return false;
  handledEvent = key;
  const source: ControlLinkSource = {
    kind: "midi_cc",
    channel: event.channel,
    controller: event.controller,
  };
  try {
    await addControlTarget(source, {
      track: request.track,
      param: request.param,
      formula: request.formula,
      smoothing_ms: request.smoothing_ms,
    });
    setLearn(null);
    return true;
  } catch (error) {
    handledEvent = "";
    throw error;
  }
}

/** Test seam: clear module-level memory between cases. */
export function resetControlLinkState(): void {
  setLastTweaked(null);
  setLearn(null);
  handledEvent = "";
}
