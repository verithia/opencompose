import { i18n } from "./i18n";
import type { ParameterTargetStamp } from "../types/project";
/**
 * One hosted plugin's generated parameter panel.
 *
 * The backend owns the parameter list; the edit depends on the write arm. An
 * armed drag rides the shared writer in `stores/automationWrite`, so it keeps
 * one point per grid cell and closes as one undo entry. An unarmed drag is an
 * ordinary edit: it applies the value on release and creates no lane.
 */
import { createSignal } from "solid-js";
import { ownedParameterSurface } from "./ownedParameterGesture";
import * as api from "./api";
import type { PluginPanelIssue, PluginPanelParam } from "./api";
import { store } from "./project";
import {
  automationWriteArmed,
  closeAutomationWrite,
  openAutomationWrite,
  sampleAutomationWrite,
  type AutomationWriteHandle,
  type AutomationWriteSurface,
} from "./automationWrite";
import { automationPositionAt } from "../utils/automationSampler";

export type PanelStatus = "idle" | "loading" | "ready" | "unavailable" | "error";

export interface PluginPanelState {
  preset: string;
  format: string;
  status: PanelStatus;
  params: PluginPanelParam[];
  issue: PluginPanelIssue | null;
  error: string | null;
}

const EMPTY: PluginPanelState = {
  preset: "",
  format: "",
  status: "idle",
  params: [],
  issue: null,
  error: null,
};

const [panel, setPanel] = createSignal<PluginPanelState>(EMPTY);
/** Values a drag has moved ahead of the instance's next read, keyed by name. */
const [liveValues, setLiveValues] = createSignal<
  Record<string, { normalized: number; current: number }>
>({});

/** The panel the preset detail pane is showing. */
export const pluginPanel = panel;

/** A parameter's value while a drag is in flight; the loaded value otherwise. */
export function panelValue(
  param: PluginPanelParam,
): { normalized: number; current: number; moved: boolean } {
  const live = liveValues()[param.name];
  return live
    ? { ...live, moved: true }
    : { normalized: param.normalized, current: param.current, moved: false };
}

/** Values the plugin had when the panel opened, used when it reports no default. */
let openedValues = new Map<string, number>();
let panelEpoch = 0;

/** The transport position a lane point lands on, matching the synth path. */
export function panelAutomationPosition() {
  const numerator = Math.max(1, store.project.properties.time_signature.numerator);
  const absolute = Math.max(0, store.transport().position_beats);
  const barOffset = Math.floor(absolute / numerator);
  const beatOffset = absolute - barOffset * numerator;
  const beatWhole = Math.floor(beatOffset);
  return {
    bar: barOffset + 1,
    beat: beatWhole + 1,
    subdivision: Math.min(3, Math.round((beatOffset - beatWhole) * 4)),
  };
}

/** The track a preset's automation belongs to: the selected one, else the first. */
export function panelTrackIndex(presetName: string): number {
  const selected = store.selectedTrackIndex();
  if (selected !== null && store.project.tracks[selected]?.preset === presetName) return selected;
  return store.project.tracks.findIndex((track) => track.preset === presetName);
}

/** Load the panel for one preset; safe to call again after a project change. */
export async function loadPluginPanel(presetName: string): Promise<void> {
  const epoch = ++panelEpoch;
  const ownsDocument = store.parameterRequestOwner();
  const ownsLoad = () => epoch === panelEpoch && ownsDocument();
  setPanel({ ...EMPTY, preset: presetName, status: "loading" });
  setLiveValues({});
  try {
    const result = await api.pluginParameterPanel(presetName);
    if (!ownsLoad() || panel().preset !== presetName) return;
    if (result.surface.kind === "parameters") {
      openedValues = new Map(result.surface.parameters.map((param) => [param.name, param.current]));
      setPanel({
        preset: presetName,
        format: result.format,
        status: "ready",
        params: result.surface.parameters,
        issue: null,
        error: null,
      });
      return;
    }
    setPanel({
      preset: presetName,
      format: result.format,
      status: "unavailable",
      params: [],
      issue: result.surface.issue,
      error: null,
    });
  } catch (error) {
    if (!ownsLoad() || panel().preset !== presetName) return;
    setPanel({
      ...EMPTY,
      preset: presetName,
      status: "error",
      error: String(error),
    });
  }
}

/** Value a double-click restores: the plugin's default, else the value on open. */
export function resetValue(param: PluginPanelParam): number {
  if (param.default !== null && Number.isFinite(param.default)) return param.default;
  return openedValues.get(param.name) ?? param.current;
}

function setParamValue(paramName: string, normalized: number, current: number): void {
  setLiveValues((values) => ({ ...values, [paramName]: { normalized, current } }));
}

const dragHandleBrand: unique symbol = Symbol("plugin-parameter-drag");
export interface PluginParamDragHandle { readonly [dragHandleBrand]: true }

/** One drag owns its live values and shared writer through exact identity. */
let drag: {
  handle: PluginParamDragHandle;
  writer: AutomationWriteHandle | null;
  preset: string;
  param: PluginPanelParam;
  surface: AutomationWriteSurface | null;
  owns(): boolean;
  expectedStamp: ParameterTargetStamp | null;
} | null = null;

function reportError(error: unknown): void {
  setPanel((state) => ({ ...state, error: String(error) }));
}

/** Bar numerator a sampled quarter is read against. */
function numerator(): number {
  return Math.max(1, store.project.properties.time_signature.numerator);
}

/** The write surface for one hosted parameter, or null when it has no track. */
function surfaceFor(
  presetName: string,
  param: PluginPanelParam,
): AutomationWriteSurface | null {
  const trackIndex = panelTrackIndex(presetName);
  if (trackIndex < 0) return null;
  const epoch = panelEpoch;
  const ownsDocument = store.parameterRequestOwner();
  return ownedParameterSurface({
    key: `plugin\u0000${presetName}\u0000${param.name}`,
    label: param.name,
    value: () => panelValue(param).normalized,
    position: (quarter) => automationPositionAt(quarter, numerator()),
    isCurrent: () => ownsDocument() && epoch === panelEpoch && panel().preset === presetName,
    expectedStamp: ownsDocument.expectedStamp,
    begin: stamp => api.beginGuardedPluginParamGesture(presetName, trackIndex, param.name, stamp),
    point: (token, position, normalized) => api.updateOwnedPluginParamGesture(token, presetName, param.name, normalized, position),
    end: token => api.endOwnedPluginParamGesture(token, presetName, param.name),
    apply: result => store.applyAiSnapshot(result.snapshot),
  });
}

/** Start a drag; an armed drag opens the shared write gesture. */
export function beginPluginParamDrag(presetName: string, param: PluginPanelParam): PluginParamDragHandle {
  const ownsDocument = store.parameterRequestOwner();
  if (drag) void endPluginParamDrag(drag.handle);
  const surface = automationWriteArmed() ? surfaceFor(presetName, param) : null;
  if (automationWriteArmed() && !surface) reportError(`No track uses preset '${presetName}'`);
  const writer = surface ? openAutomationWrite(surface) : null;
  const handle: PluginParamDragHandle = Object.freeze({ [dragHandleBrand]: true as const });
  const epoch = panelEpoch;
  drag = { handle, writer, expectedStamp: ownsDocument.expectedStamp, preset: presetName, param: { ...param }, surface,
    owns: () => ownsDocument() && epoch === panelEpoch && panel().preset === presetName };
  return handle;
}

/** A stale event cannot paint another drag's values or sample its writer. */
export function dragPluginParam(
  presetName: string,
  param: PluginPanelParam,
  normalized: number,
  owner?: PluginParamDragHandle,
): PluginParamDragHandle | null {
  if (owner && drag?.handle !== owner) return null;
  if (!drag || drag.preset !== presetName || drag.param.name !== param.name) {
    if (owner) return null;
    owner = beginPluginParamDrag(presetName, param);
  }
  const opened = drag;
  if (!opened || !opened.owns()) return null;
  const clamped = Math.min(1, Math.max(0, normalized));
  setParamValue(param.name, clamped, param.min + clamped * (param.max - param.min));
  if (opened.writer) sampleAutomationWrite(opened.writer, store.transport().position_beats);
  return opened.handle;
}

/** Only the captured drag may close, including when another same-key drag starts. */
export async function endPluginParamDrag(owner?: PluginParamDragHandle, canceled = false): Promise<void> {
  const opened = drag;
  if (!opened || (owner && opened.handle !== owner)) return;
  drag = null;
  if (opened.writer) {
    if (!canceled) sampleAutomationWrite(opened.writer, store.transport().position_beats);
    await closeAutomationWrite(opened.writer);
    return;
  }
  if (!opened.owns()) return;
  if (canceled) {
    setLiveValues((values) => {
      const next = { ...values };
      delete next[opened.param.name];
      return next;
    });
    return;
  }
  const current = panelValue(opened.param).current;
  let token: api.UndoGestureToken | null = null;
  try {
    if (opened.expectedStamp === null) throw new Error(i18n.t("synth.ui.ownerChanged"));
    token = await api.beginParameterGesture("preset", opened.expectedStamp);
    if (!opened.owns()) return;
    const snapshot = await api.setPresetParamOwned(token, opened.preset, opened.param.name, String(current));
    if (opened.owns()) store.applyAiSnapshot(snapshot);
  } catch (error) {
    if (opened.owns()) reportError(error);
  } finally {
    if (token) {
      try { await api.endOwnedUndoGesture(token); }
      catch (error) { if (opened.owns()) reportError(error); }
    }
  }
}

/** Write one point for a parameter at the current transport position. */
export async function automatePluginParam(
  presetName: string,
  param: PluginPanelParam,
  normalized: number,
): Promise<void> {
  const surface = surfaceFor(presetName, param);
  if (!surface) return;
  const epoch = panelEpoch;
  const ownsDocument = store.parameterRequestOwner();
  try {
    await surface.open();
    await surface.point(panelAutomationPosition(), normalized);
  } catch (error) {
    if (ownsDocument() && panelEpoch === epoch) reportError(error);
  } finally {
    try { await surface.close(); }
    catch (error) { if (ownsDocument() && panelEpoch === epoch) reportError(error); }
  }
}

/** Reset one parameter through the same gesture, so it undoes in one step. */
export async function resetPluginParam(
  presetName: string,
  param: PluginPanelParam,
): Promise<void> {
  const value = resetValue(param);
  const span = param.max - param.min;
  const normalized = span === 0 ? 0 : (value - param.min) / span;
  await automatePluginParam(presetName, param, Math.min(1, Math.max(0, normalized)));
}

/** Whether the preset's track already carries a lane for this parameter. */
export function hasPluginAutomation(presetName: string, paramName: string): boolean {
  const index = panelTrackIndex(presetName);
  if (index < 0) return false;
  const lane = `pluginparam_${Array.from(new TextEncoder().encode(paramName))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")}`;
  return (store.project.tracks[index]?.automation ?? []).some((entry) => entry.param === lane);
}
