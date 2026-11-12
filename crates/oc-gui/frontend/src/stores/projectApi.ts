/**
 * Undo history, project open/save, edit preview and DSL commands.
 *
 * `api.ts` re-exports this module; shared project and transport types come from
 * their owners.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ParameterTargetStamp, Project, ProjectSnapshot, RecoveryEntry } from "../types/project";
import type { TransportSnapshot } from "../types/transport";
import { startNativeAudio } from "./webAudioFocus";

import type { UndoGestureToken, UndoHistoryEntry, UndoScope, UndoState } from "../types/undo";
export type { UndoHistoryEntry, UndoScope, UndoState } from "../types/undo";

// ============================================================================
// Undo / Redo (backend-managed patch-based stack)
// ============================================================================

export const undoProject = () => invoke<ProjectSnapshot>("undo");
export const redoProject = () => invoke<ProjectSnapshot>("redo");
export const getUndoState = () => invoke<UndoState>("get_undo_state");
export const getUndoHistory = () => invoke<UndoHistoryEntry[]>("get_undo_history");
export const restoreUndoLevel = (level: number) =>
  invoke<ProjectSnapshot>("restore_undo_level", { level });
export const beginUndoGesture = (scope: UndoScope) =>
  invoke<void>("begin_undo_gesture", { scope });
export const endUndoGesture = () => invoke<void>("end_undo_gesture");
export type { UndoGestureToken } from "../types/undo";
export const beginGuardedSynthParamGesture = (presetName: string, trackIndex: number, paramKey: string, expectedStamp: ParameterTargetStamp) =>
  invoke<UndoGestureToken>("begin_guarded_synth_param_gesture", { presetName, trackIndex, paramKey, expectedStamp });
export const beginParameterGesture = (scope: UndoScope, expectedStamp: ParameterTargetStamp) =>
  invoke<UndoGestureToken>("begin_parameter_gesture", { scope, expectedStamp });
export const beginOwnedUndoGesture = (scope: UndoScope) =>
  invoke<UndoGestureToken>("begin_owned_undo_gesture", { scope });
export const endOwnedUndoGesture = (token: UndoGestureToken) =>
  invoke<boolean>("end_owned_undo_gesture", { token });

// ============================================================================
// Project / DSL
// ============================================================================

export const newProject = () => invoke<ProjectSnapshot>("new_project");
/** Create, name and write a project in one command. */
export const newProjectAt = (path: string, template?: "starter") =>
  invoke<ProjectSnapshot>("new_project_at", template ? { path, template } : { path });

// ============================================================================
// Edit preview
// ============================================================================

import type { PreviewSnapshot, PreviewState } from "../types/preview";
export type {
  PreviewClipRef,
  PreviewEffectRef,
  PreviewHighlight,
  PreviewNoteSpan,
  PreviewSnapshot,
  PreviewState,
} from "../types/preview";

export const previewBegin = (newDsl: string, targetPath: string, expectedDsl: string) =>
  invoke<PreviewSnapshot>("preview_begin", { newDsl, targetPath, expectedDsl });
export const previewShow = (side: "base" | "proposal") =>
  invoke<PreviewSnapshot>("preview_show", { side });
export const previewAccept = () => invoke<ProjectSnapshot>("preview_accept");
export const previewCancel = () => invoke<ProjectSnapshot>("preview_cancel");
export const previewState = () => invoke<PreviewState | null>("preview_state");
export const openFile = (path: string) => invoke<ProjectSnapshot>("open_file", { path });
export const saveFile = (path?: string) => invoke<{ path: string; snapshot: ProjectSnapshot | null }>("save_file", { path: path ?? null });
/** The operator answered Don't Save: the document must not reach the file. */
export const discardUnsaved = () => invoke<ProjectSnapshot>("discard_unsaved");
/** Check whether quitting needs a save decision or an active take. */
export const prepareQuit = () => invoke<{
  dirty: boolean;
  recording: boolean;
}>("prepare_quit");
export const stopRecording = () => invoke<TransportSnapshot>("stop_recording");
/** Leave the application after the quit prompt was answered. */
export const quitNow = () => invoke<void>("quit_now");
export const setTimeSignature = (numerator: number, denominator: number) =>
  invoke<ProjectSnapshot>("set_time_signature", { numerator, denominator });
export const getProject = () => invoke<ProjectSnapshot>("get_project");
export const takeStartupProject = () => invoke<string | null>("take_startup_project");
export const listRecoveries = () => invoke<RecoveryEntry[]>("list_recoveries");
export const restoreRecovery = (path: string) =>
  invoke<ProjectSnapshot>("restore_recovery", { path });
export const discardRecovery = (path: string) =>
  invoke<void>("discard_recovery", { path });
export const getDslSource = () => invoke<string>("get_dsl_source");
export const parseDsl = (source: string) => invoke<Project>("parse_dsl", { source });
export const updateDsl = (source: string) => invoke<ProjectSnapshot>("update_dsl", { source });
export const previewDrumPad = (presetName: string, note: number, velocity: number | undefined, startPaused: boolean) =>
  startNativeAudio(() => invoke<void>("preview_drum_pad", { presetName, note, velocity: velocity ?? null, startPaused }));
