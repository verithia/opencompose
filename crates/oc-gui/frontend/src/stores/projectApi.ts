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

import type {
  HistoryClearOutcome, HistoryInfo, HistoryPage, HistoryStep, UndoGestureToken, UndoScope, UndoState,
} from "../types/undo";
export type {
  HistoryAuthor, HistoryBrief, HistoryClearOutcome, HistoryFailure, HistoryInfo, HistoryPage, HistoryRefusal,
  HistoryStep,
  UndoHistoryEntry, UndoScope, UndoState,
} from "../types/undo";

// ============================================================================
// Edit history (backend-managed, kept in the app data folder)
// ============================================================================

export const historyUndo = () => invoke<HistoryStep<ProjectSnapshot>>("history_undo");
export const historyRedo = () => invoke<HistoryStep<ProjectSnapshot>>("history_redo");
/** Move the cursor so `level` entries are applied; 0 is before the oldest kept one. */
export const historyJump = (level: number) =>
  invoke<HistoryStep<ProjectSnapshot>>("history_jump", { level });
/** Up to `limit` (at most 64) entries older than level `before`, newest first. */
export const historyPage = (before?: number, limit?: number) =>
  invoke<HistoryPage>("history_page", { before: before ?? null, limit: limit ?? null });
export const historyInfo = () => invoke<HistoryInfo>("history_info");
export const historyClearOlder = (keep: number) =>
  invoke<HistoryClearOutcome>("history_clear_older", { keep });
export const getUndoState = () => invoke<UndoState>("get_undo_state");
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
/** Give the project another file; its history and backup move with it. */
export const moveProject = (to: string) => invoke<ProjectSnapshot>("move_project", { to });
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
