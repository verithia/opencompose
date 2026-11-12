/**
 * Built-in synth automation transactions.
 *
 * The backend owns the undo entry: `begin` snapshots the project, `update`
 * writes lane points and the live parameter without touching undo, and `end`
 * commits the whole gesture as one entry — the same transaction shape the
 * hosted-plugin parameter path uses.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ParameterTargetStamp, ProjectSnapshot } from "../types/project";
import { store } from "./project";
import { beginGuardedSynthParamGesture as beginGuardedSynthParamGestureRequest, type UndoGestureToken } from "./api";
import { i18n } from "./i18n";

export interface AutomationPosition {
  bar: number;
  beat: number;
  subdivision: number;
}

export const beginSynthParamGesture = (
  presetName: string,
  trackIndex: number,
  paramKey: string,
) => invoke<void>("begin_synth_param_gesture", { presetName, trackIndex, paramKey });

/** Intermediate points update the project; the snapshot is not applied here. */
export const updateSynthParamGesture = (
  presetName: string,
  paramKey: string,
  normalized: number,
  physical: number | null,
  position: AutomationPosition,
) => invoke<ProjectSnapshot>("update_synth_param_gesture", {
  presetName,
  paramKey,
  normalized,
  physical,
  position,
});

export const endSynthParamGesture = async (presetName: string, paramKey: string) => {
  store.applyAiSnapshot(await invoke<ProjectSnapshot>("end_synth_param_gesture", {
    presetName,
    paramKey,
  }));
};

/** Tokenized APIs bind every point and close to the same backend session. */
export const beginGuardedSynthParamGesture = (
  presetName: string, trackIndex: number, paramKey: string, expectedStamp: ParameterTargetStamp,
) => beginGuardedSynthParamGestureRequest(presetName, trackIndex, paramKey, expectedStamp);

export const beginOwnedSynthParamGesture = (
  presetName: string, trackIndex: number, paramKey: string,
) => invoke<UndoGestureToken>("begin_owned_synth_param_gesture", { presetName, trackIndex, paramKey });

export const updateOwnedSynthParamGesture = (
  token: UndoGestureToken, presetName: string, paramKey: string,
  normalized: number, physical: number | null, position: AutomationPosition,
) => invoke<ProjectSnapshot>("update_owned_synth_param_gesture", {
  presetName, paramKey, normalized, physical, position, token,
});

export interface OwnedSynthGestureEnd {
  closed: boolean;
  snapshot: ProjectSnapshot;
}

/** The caller owns generation-aware snapshot admission; a stale close is false. */
export const endOwnedSynthParamGesture = (
  token: UndoGestureToken, presetName: string, paramKey: string,
) => invoke<OwnedSynthGestureEnd>("end_owned_synth_param_gesture", { presetName, paramKey, token });

export const removeSynthParamAutomation = async (trackIndex: number, paramKey: string) => {
  store.applyAiSnapshot(await invoke<ProjectSnapshot>("remove_synth_param_automation", {
    trackIndex,
    paramKey,
  }));
};

/** Write a single lane point at `position` as one undoable gesture. */
export async function automateSynthParamAt(
  presetName: string,
  trackIndex: number,
  paramKey: string,
  normalized: number,
  position: AutomationPosition,
  physical: number | null = null,
): Promise<void> {
  await beginSynthParamGesture(presetName, trackIndex, paramKey);
  let failure: unknown = null;
  try {
    await updateSynthParamGesture(presetName, paramKey, normalized, physical, position);
  } catch (error) {
    failure = error;
  }
  try {
    await endSynthParamGesture(presetName, paramKey);
  } catch (error) {
    failure ??= error;
  }
  if (failure) throw failure;
}

/** Owner-scoped single point; legacy automateSynthParamAt remains available. */
export async function automateOwnedSynthParamAt(
  presetName: string,
  trackIndex: number,
  paramKey: string,
  normalized: number,
  position: AutomationPosition,
  physical: number | null = null,
): Promise<void> {
  const owner = store.parameterRequestOwner();
  const token = await beginGuardedSynthParamGesture(presetName, trackIndex, paramKey, owner.requireStamp());
  let failure: unknown = null;
  try {
    if (!owner()) {
      throw new Error(i18n.t("synth.ui.ownerChanged"));
    }
    await updateOwnedSynthParamGesture(token, presetName, paramKey, normalized, physical, position);
  } catch (error) {
    failure = error;
  }
  try {
    const result = await endOwnedSynthParamGesture(token, presetName, paramKey);
    if (result.closed && owner()) {
      store.applyAiSnapshot(result.snapshot);
    }
  } catch (error) {
    failure ??= error;
  }
  if (failure) throw failure;
}
