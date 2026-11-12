import { captureParameterAdmission, type ParameterRequestOwner } from "./parameterAdmission";
/**
 * Global project store — SolidJS signals wrapping the project state.
 */
import { createSignal, createMemo } from "solid-js";
import { createStore } from "solid-js/store";
import type { BarBeat, ExportSettings, ProjectSnapshot, Project, ClipGroup, Track, Phrase, Pattern, TempoPoint, TimeSigPoint } from "../types/project";
import type { RenderAudioRequest, RenderAudioResult } from "../types/export";
import type { LibraryListing, LibraryPatch } from "./api";
import * as api from "./api";
import { log } from "./log";
import { invalidatePluginPanelCache } from "./pluginParamsCache";
import { createProjectClock } from "../utils/projectClock";
import { uniqueName } from "../utils/naming";
import { recoveryStore } from "./recovery";
import { i18n } from "./i18n";
import { gainEnvelopeWindow } from "./gainEnvelopeWindow";
import { createMixerActions } from "./mixerActions";
import { createNotesActions } from "./notesActions";
import { createPlaybackStore } from "./playbackStore";
import { createTransportRanges } from "./transportRanges";
import { createUndoStore } from "./undoStore";

export { onTransportSnapshot, type AbCandidateInput, type AuditionKind, type TakeSoloState } from "./playbackStore";

// ============================================================================
// State
// ============================================================================

const emptyProject: Project = {
  name: "Untitled",
  properties: {
    tempo: 120,
    time_signature: { numerator: 4, denominator: 4 },
    key: null,
    swing: null,
    tempo_map: [],
    time_sig_map: [],
    key_map: [],
  },
  presets: [],
  patterns: [],
  tracks: [],
  buses: [],
  master: null,
  sections: [],
};

const [project, setProject] = createStore<Project>(emptyProject);
const [dslSource, setDslSource] = createSignal("");
const [filePath, setFilePath] = createSignal<string | null>(null);
const [dirty, setDirty] = createSignal(false);
const [loading, setLoading] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);
const {
  undoState,
  undoHistory,
  refreshUndoState,
  refreshUndoHistory,
  undo,
  redo,
  restoreUndoLevel,
  clearUndoHistory,
} = createUndoStore(applySnapshot, setError,
  () => `${projectLoadEpoch}:${documentLoadPending ?? "ready"}:${appliedRevision}`);

const {
  loop,
  loopPending,
  loopError,
  punch,
  punchPending,
  punchError,
  restoreLoopForProject,
  rekeyStoredLoop,
  setLoopRegion,
  toggleLoop,
  clearLoopRegion,
  loopRegion,
  adoptPunch,
  punchRegion,
  setPunchRegion,
} = createTransportRanges(filePath);

const {
  audition,
  abPair,
  abBusy,
  abError,
  auditionActive,
  auditionKind,
  transport,
  scrubPosition,
  setScrubPosition,
  takeSolo,
  smoothPositionBeats,
  cursorActive,
  previewCursorBeats,
  previewTrackIndex,
  adoptTransport,
  startTransportPoll,
  play,
  pause,
  stop,
  seek,
  seekToMarker,
  togglePlayPause,
  previewDrumPad,
  previewMarketSample,
  stopMarketSample,
  marketSampleAction,
  auditionTake,
  previewPreset,
  auditionFactoryPatch,
  compareFactoryPatches,
  switchAb,
  clearAbPair,
  previewPattern,
  toggleAudition,
  auditionAction,
  playbackPlaying,
  auditionPositionSeconds,
  auditionLocalBeats,
  auditionStartBeats,
  auditionArrangementBeats,
  stopAudition,
  playDryKey,
  toggleTrackPreview,
  previewTrack,
  previewBlock,
  endPlaybackForProjectReplacement,
  resetPosition,
  adoptArrangementDuration,
} = createPlaybackStore(project, applyWithUndo, setLoading, setError);

/** User library entries, refreshed after every save and on project load. */
const [libraryPatches, setLibraryPatches] = createSignal<LibraryPatch[]>([]);
const [libraryWarnings, setLibraryWarnings] = createSignal<string[]>([]);
/** Marker edits in flight, and the sentence a refused edit shows. */
const [markerBusy, setMarkerBusy] = createSignal(false);
const [markerError, setMarkerError] = createSignal<string | null>(null);
/** Group edits in flight, and the sentence a refused edit shows. */
const [groupBusy, setGroupBusy] = createSignal(false);
const [groupError, setGroupError] = createSignal<string | null>(null);

/** Newest whole-patch load; an older reply must not restore its own patch. */
let patchLoadSeq = 0;
/** Newest track-preset change; same rule for the pattern and mixer pickers. */
let trackPresetSeq = 0;

const [selectedTrackIndex, setSelectedTrackIndex] = createSignal<number | null>(null);
const [selectedPresetNameValue, setSelectedPresetName] = createSignal<string | null>(null);
const selectedPresetName = () => {
  const name = selectedPresetNameValue();
  return name !== null && project.presets.some(preset => preset.name === name) ? name : null;
};
const [selectedPatternName, setSelectedPatternName] = createSignal<string | null>(null);
/** Timeline clip/phrase the editor was last opened from. */
export interface EditorSelection {
  trackIndex: number;
  startBeat: number;
  patternName: string | null;
}
const [editorSelection, setEditorSelection] = createSignal<EditorSelection | null>(null);
/** Lane blocks picked in the arrange view; lives here so it survives re-renders
 * and a project switch can clear it. */
export interface TimelineBlockRef {
  kind: "clip" | "phrase" | "pattern";
  trackIndex: number;
  index: number;
}
const [blockSelection, setBlockSelection] = createSignal<TimelineBlockRef[]>([]);

/** A copied block keeps its full payload, not a reference to the live block. */
export interface ClipClipboardBase {
  durationBeats: number;
  sourceTrackIndex: number;
  sourceStart: BarBeat;
  /** Playback window the copied pattern clip played, carried to the paste. */
  offsetQuarters?: number;
  loopQuarters?: number;
}
export type ClipClipboard =
  | (ClipClipboardBase & { kind: "clip"; patternName: string })
  | (ClipClipboardBase & { kind: "phrase"; phrase: Phrase })
  | (ClipClipboardBase & { kind: "pattern"; pattern: Pattern });
const [clipClipboard, setClipClipboard] = createSignal<ClipClipboard | null>(null);

/**
 * Identity of the open document.
 *
 * Local picks address clips, lanes and points by index, so a pick made in one
 * document can reach a different clip after another document is loaded. The
 * generation is the identity: it advances once per load and stays put while the
 * same document is edited or saved under another path. Every surface drops its
 * picks when the generation advances, and the path is metadata for that record,
 * never a comparison key.
 */
export interface DocumentIdentity {
  path: string | null;
  generation: number;
}
const [documentIdentity, setDocumentIdentity] = createSignal<DocumentIdentity>({
  path: null,
  generation: 0,
});
/**
 * Epoch of the load whose snapshot may replace the document; null when no load
 * is in flight. The epoch keeps a failed or superseded load from leaving the
 * flag behind for the next ordinary edit.
 */
let documentLoadPending: number | null = null;
/** Every snapshot admitted, so a save can tell whether an edit raced its write. */
let snapshotEpoch = 0;
/**
 * Revision of the snapshot on screen; -1 until the first one arrives.
 *
 * The backend bumps the revision on every edit and once more when a load
 * installs a document, so a reply that carries a lower revision describes a
 * document the store has already left behind. Replies are fire-and-forget, so
 * such a reply can arrive after a newer edit or a newer document.
 */
let appliedRevision = -1;
const [parameterTargetStamp, setParameterTargetStamp] = createSignal<import("../types/project").ParameterTargetStamp | null>(null);

/** Announce that the snapshot of `loadEpoch` replaces the whole document. */
function beginDocumentLoad(loadEpoch: number): void {
  documentLoadPending = loadEpoch;
}

/** Withdraw a load that failed or was superseded before it applied a snapshot. */
function cancelDocumentLoad(loadEpoch: number): void {
  if (documentLoadPending === loadEpoch) documentLoadPending = null;
}

/** Record the path the open document lives at; the document itself is unchanged. */
function noteDocumentPath(path: string | null): void {
  const current = documentIdentity();
  // A signal write would notify the surfaces that react to the identity and
  // make them drop their picks, so a path-only change updates the record.
  if (path !== current.path) current.path = path;
}

/** Advance the identity on a document replacement and drop every local pick. */
function noteDocument(snap: ProjectSnapshot): void {
  if (documentLoadPending === null) {
    // An edit or a save keeps the document open, so only its path can change.
    noteDocumentPath(snap.file_path);
    return;
  }
  documentLoadPending = null;
  setDocumentIdentity({
    path: snap.file_path,
    generation: documentIdentity().generation + 1,
  });
  setBlockSelection([]);
  setEditorSelection(null);
  setClipClipboard(null);
  setSelectedPresetName(null);
  // Every editor names the sound it shows through the track pick, so a
  // replacement falls back to the first track instead of to nothing.
  setSelectedTrackIndex(snap.project.tracks.length > 0 ? 0 : null);
}

const [activePanel, setActivePanel] = createSignal<
  "timeline" | "mixer" | "pianoroll" | "dsl" | "ai"
>("timeline");

// ============================================================================
// Apply snapshot from backend
// ============================================================================

/**
 * Adopt `snap` unless it describes a document the store has moved past.
 *
 * `loadEpoch` marks the snapshot of the load that owns the next document: a
 * load may install a document whose revision is lower than the one it replaced,
 * so its snapshot replaces the floor instead of being compared with it. While a
 * load is in flight every other reply describes the document being replaced and
 * is refused, whatever its revision.
 *
 * Returns whether the snapshot was adopted.
 */
function applySnapshot(snap: ProjectSnapshot, loadEpoch?: number): boolean {
  const replacing = documentLoadPending !== null;
  if (replacing && loadEpoch !== documentLoadPending) {
    log.warn("project", `Ignored snapshot revision ${snap.revision}: a document load owns the next document`);
    return false;
  }
  // A reply without a revision (an old command, a stub) cannot move the floor.
  const revision = Number.isFinite(snap.revision) ? snap.revision : appliedRevision;
  if (!replacing && revision < appliedRevision) {
    log.warn("project", `Ignored snapshot revision ${revision}; the document is at ${appliedRevision}`);
    return false;
  }
  appliedRevision = replacing ? revision : Math.max(appliedRevision, revision);
  setParameterTargetStamp(typeof snap.parameter_target_stamp === "string" && snap.parameter_target_stamp.length > 0
    ? snap.parameter_target_stamp : null);
  const clipEditor = gainEnvelopeWindow.state();
  if (clipEditor) {
    const index = clipEditor.clipIndex;
    const previous = project.properties.audio_clips?.[index];
    const next = snap.project.properties.audio_clips?.[index];
    if (replacing || previous?.name !== next?.name) gainEnvelopeWindow.close();
  }
  setProject(snap.project);
  // The store merges top-level keys, so a list the backend omits when it is
  // empty would survive from the previous project. Clear those two explicitly.
  setProject("groups", snap.project.groups ?? []);
  setProject("markers", snap.project.markers ?? []);
  const selectedPreset = selectedPresetNameValue();
  if (selectedPreset !== null && !snap.project.presets.some(preset => preset.name === selectedPreset)) {
    setSelectedPresetName(null);
  }
  setDslSource(snap.dsl_source);
  setFilePath(snap.file_path);
  setDirty(snap.dirty);
  setError(null);
  snapshotEpoch += 1;
  // A replacement, unlike an edit, invalidates every index-based local pick.
  noteDocument(snap);
  clampTrackPicks(snap.project.tracks.length);
  adoptArrangementDuration(snap.duration_seconds);
  // Update window title
  updateWindowTitle(snap.project.name, snap.file_path, snap.dirty);
  return true;
}

/** External events follow backend transaction boundaries and own their history. */
function applyExternalSnapshot(snap: ProjectSnapshot, replacesDocument = false): boolean {
  if (documentLoadPending !== null || !Number.isFinite(snap.revision) || snap.revision <= appliedRevision) return false;
  if (!replacesDocument && snap.file_path !== filePath()) return false;
  let loadEpoch: number | undefined;
  if (replacesDocument) {
    loadEpoch = ++projectLoadEpoch;
    beginDocumentLoad(loadEpoch);
    invalidatePluginPanelCache();
    resetPosition();
  }
  if (!applySnapshot(snap, loadEpoch)) return false;
  if (replacesDocument) void restoreLoopForProject(snap.file_path);
  void refreshUndoState();
  return true;
}

/** Apply a snapshot that already carries its own undo entry (a recorded take). */
function applyTakeSnapshot(snap: ProjectSnapshot): boolean {
  return applyWithUndo(snap);
}

/**
 * Keep the track pick inside the document. A snapshot can shrink the track
 * list (a removal, or an undo of an add), and an index past the end names no
 * track while every command built from it fails.
 */
function clampTrackPicks(count: number): void {
  const selected = selectedTrackIndex();
  if (selected !== null && selected >= count) {
    setSelectedTrackIndex(count > 0 ? count - 1 : null);
  }
}

function applyWithUndo(snap: ProjectSnapshot): boolean {
  // A refused snapshot changed nothing, so there is no history to repaint.
  if (!applySnapshot(snap)) return false;
  // Undo is now managed by the backend — no frontend push needed.
  // Refresh undo state for UI display.
  refreshUndoState();
  return true;
}

async function updateWindowTitle(name: string | null, path: string | null, isDirty: boolean) {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();
    const projectName = name || "Untitled";
    const dirtyMark = isDirty ? " *" : "";
    const fileName = path ? ` — ${path.split("/").pop()?.split("\\").pop()}` : "";
    await win.setTitle(`${projectName}${dirtyMark}${fileName} — OpenCompose`);
  } catch (_) {}
}

// ============================================================================
// Actions
// ============================================================================

/** The answer to the unsaved-changes prompt that precedes a destructive load. */
export type UnsavedAnswer = "save" | "discard" | "cancel";

const [unsavedPrompt, setUnsavedPrompt] = createSignal<
  ((answer: UnsavedAnswer) => void) | null
>(null);

/**
 * Ask Save / Don't Save / Cancel before a load replaces a dirty document.
 *
 * True means the caller may replace the document: Don't Save, or a Save that
 * already wrote the file. Cancel answers false and writes nothing, so the file
 * and the document stay as they were.
 */
async function confirmDiscardChanges(): Promise<boolean> {
  if (!dirty()) return true;
  const answer = await new Promise<UnsavedAnswer>((resolve) => {
    // A second request while the prompt is open answers itself Cancel.
    if (unsavedPrompt()) {
      resolve("cancel");
      return;
    }
    setUnsavedPrompt(() => resolve);
  });
  return answer !== "cancel";
}

/** One of the prompt's answers; the dialog calls this. */
function answerUnsavedPrompt(answer: UnsavedAnswer) {
  const resolve = unsavedPrompt();
  if (!resolve) return;
  setUnsavedPrompt(null);
  resolve(answer);
}

/**
 * The prompt's Save answer: write first, then let the load continue. A failed
 * or cancelled save keeps the prompt open, so the load does not happen without
 * the write the operator asked for.
 */
async function saveForUnsavedPrompt(): Promise<void> {
  let saved = false;
  if (filePath()) {
    saved = await saveFile();
  } else {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      filters: [{ name: "OpenCompose", extensions: ["ocp"] }],
    });
    if (typeof path === "string") saved = await saveFile(path);
  }
  if (saved) answerUnsavedPrompt("save");
}

async function initStartupProject() {
  await initProject();
  try {
    const path = await api.takeStartupProject();
    if (path) await openFile(path);
  } catch (e) { setError(String(e)); }
  await recoveryStore.checkOnStartup();
}

let projectLoadEpoch = 0;

/** End the previous audible owner before a destructive project replacement. */
async function prepareProjectReplacement(): Promise<number> {
  const loadEpoch = ++projectLoadEpoch;
  beginDocumentLoad(loadEpoch);
  const stopped = endPlaybackForProjectReplacement();
  if (stopped) await stopped;
  if (loadEpoch === projectLoadEpoch) resetPosition();
  return loadEpoch;
}

async function initProject() {
  const loadEpoch = projectLoadEpoch;
  beginDocumentLoad(loadEpoch);
  setLoading(true);
  invalidatePluginPanelCache();
  try {
    const snap = await api.getProject();
    if (loadEpoch !== projectLoadEpoch) return;
    applySnapshot(snap, loadEpoch);
    setEditorSelection(null);
    setBlockSelection([]);
    setClipClipboard(null);
    void restoreLoopForProject(snap.file_path);
  } catch (e) {
    // No snapshot arrived, so the previous document is still open and the next
    // edit must count as an edit.
    cancelDocumentLoad(loadEpoch);
    if (loadEpoch === projectLoadEpoch) setError(String(e));
  }
  finally { if (loadEpoch === projectLoadEpoch) setLoading(false); }
}

async function newProject(): Promise<boolean> {
  return installNewProject(() => api.newProject());
}

/**
 * Create the named project at `path` in one command.
 *
 * The name and the directory are part of the document before it is written, so
 * the workspace's New has no rename and no second save to sequence.
 */
async function newProjectAt(path: string, template?: "starter"): Promise<boolean> {
  return installNewProject(() => api.newProjectAt(path, template));
}

async function installNewProject(create: () => Promise<ProjectSnapshot>): Promise<boolean> {
  const loadEpoch = await prepareProjectReplacement();
  if (loadEpoch !== projectLoadEpoch) return false;
  setLoading(true);
  invalidatePluginPanelCache();
  try {
    const snap = await create();
    if (loadEpoch !== projectLoadEpoch) return false;
    applySnapshot(snap, loadEpoch);
    void restoreLoopForProject(snap.file_path);
    clearUndoHistory();
    setSelectedTrackIndex(null);
    setEditorSelection(null);
    setBlockSelection([]);
    setClipClipboard(null);
    // A load must offer any copy a previous session left newer than the file.
    void recoveryStore.checkAfterLoad();
    return true;
  } catch (e) {
    // No snapshot arrived, so the previous document is still open.
    cancelDocumentLoad(loadEpoch);
    if (loadEpoch === projectLoadEpoch) setError(String(e));
    return false;
  }
  finally { if (loadEpoch === projectLoadEpoch) setLoading(false); }
}

async function setProjectName(name: string): Promise<boolean> {
  try {
    applyWithUndo(await api.setProjectName(name));
    return true;
  } catch (e) { setError(String(e)); return false; }
}

async function openFile(path: string) {
  const loadEpoch = await prepareProjectReplacement();
  if (loadEpoch !== projectLoadEpoch) return;
  setLoading(true);
  invalidatePluginPanelCache();
  try {
    const started = performance.now();
    const snap = await api.openFile(path);
    const fetched = performance.now();
    if (loadEpoch !== projectLoadEpoch) return;
    applySnapshot(snap, loadEpoch);
    const applied = performance.now();
    log.info(
      "project",
      `gui: open ipc=${Math.round(fetched - started)}ms apply=${Math.round(applied - fetched)}ms`,
    );
    void restoreLoopForProject(snap.file_path);
    clearUndoHistory();
    setSelectedTrackIndex(snap.project.tracks.length > 0 ? 0 : null);
    setEditorSelection(null);
    setBlockSelection([]);
    setClipClipboard(null);
    void recoveryStore.checkAfterLoad();
  } catch (e) {
    // A refused open leaves the current document in place, so the next edit is
    // an edit and not a load.
    cancelDocumentLoad(loadEpoch);
    if (loadEpoch === projectLoadEpoch) setError(String(e));
  }
  finally { if (loadEpoch === projectLoadEpoch) setLoading(false); }
}

async function saveFile(path?: string): Promise<boolean> {
  try {
    // The save dialog can return a bare filename; always persist a reopenable .ocp.
    const target = path && !/\.ocp$/i.test(path) ? `${path}.ocp` : path;
    const epochAtStart = snapshotEpoch;
    const saved = await api.saveFile(target);
    const savedPath = saved.path;
    const previousPath = filePath();
    if (saved.snapshot && snapshotEpoch === epochAtStart) applySnapshot(saved.snapshot);
    setFilePath(savedPath);
    // A save keeps the document open, so its identity follows the path without
    // advancing the generation or dropping the picks made in it.
    noteDocumentPath(savedPath);
    rekeyStoredLoop(previousPath, savedPath);
    // The reply carries no dirty marker. An edit that landed during the write
    // adopted its own snapshot, and that snapshot owns the flag from here on.
    if (snapshotEpoch === epochAtStart) setDirty(false);
    setError(null);
    log.info("project", `Saved ${savedPath}`);
    updateWindowTitle(project.name, savedPath, dirty());
    // The save retires this project's recovery copies.
    void recoveryStore.refresh();
    return true;
  } catch (e) { setError(String(e)); return false; }
}

/**
 * The operator answered Don't Save on the quit prompt: the backend drops the
 * document without writing it, so the project file keeps its bytes. The
 * recovery copies are left alone.
 */
async function discardUnsaved(): Promise<boolean> {
  try {
    applySnapshot(await api.discardUnsaved());
    setError(null);
    log.info("project", "Unsaved changes discarded");
    return true;
  } catch (e) { setError(String(e)); return false; }
}

/**
 * Load a recovery copy through the backend's normal project-load path, so undo,
 * previews and caches behave exactly as they do for an opened file. The copy
 * stays on disk; the restored project is dirty until the user saves.
 */
async function restoreFromRecovery(path: string): Promise<boolean> {
  const loadEpoch = await prepareProjectReplacement();
  if (loadEpoch !== projectLoadEpoch) return false;
  recoveryStore.beginRestore();
  setLoading(true);
  invalidatePluginPanelCache();
  try {
    const snap = await api.restoreRecovery(path);
    if (loadEpoch !== projectLoadEpoch) return false;
    applySnapshot(snap, loadEpoch);
    void restoreLoopForProject(snap.file_path);
    clearUndoHistory();
    setSelectedTrackIndex(snap.project.tracks.length > 0 ? 0 : null);
    setEditorSelection(null);
    setBlockSelection([]);
    setClipClipboard(null);
    recoveryStore.close();
    // The restored copy is the offer that was just accepted; only a later copy
    // is offered again.
    await recoveryStore.checkAfterLoad(path);
    log.info("project", `Recovered from autosave ${path}`);
    return true;
  } catch (e) {
    // The copy was not applied, so the current document keeps its identity.
    cancelDocumentLoad(loadEpoch);
    if (loadEpoch === projectLoadEpoch) recoveryStore.setError(String(e));
    return false;
  } finally {
    recoveryStore.endRestore();
    if (loadEpoch === projectLoadEpoch) setLoading(false);
  }
}

async function updateDsl(source: string) {  try {
    const snap = await api.updateDsl(source);
    applySnapshot(snap);
    refreshUndoState();
  } catch (e) {
    setError(String(e));
    throw e;
  }
}

/**
 * Rename a pattern in the AST. The backend moves the declaration and every clip
 * that references it in one edit, then regenerates the DSL text from the AST.
 */
async function renamePattern(oldName: string, newName: string): Promise<boolean> {
  const name = newName.trim();
  if (!name) {
    setError("A pattern name must not be empty");
    return false;
  }
  if (name === oldName) return true;
  try {
    const snap = await api.renamePattern(oldName, name);
    if (!applyWithUndo(snap)) return false;
    setSelectedPatternName(name);
    log.info("patterns", `Renamed "${oldName}" -> "${name}"`);
    return true;
  } catch (e) {
    setError(String(e));
    return false;
  }
}

async function addTrack(
  name: string,
  category: string,
  instrument: string,
  preset: string | null = null,
): Promise<number> {
  try {
    const snap = await api.addTrack(name, category, instrument, preset);
    applyWithUndo(snap);
    const idx = snap.project.tracks.length - 1;
    setSelectedTrackIndex(idx);
    return idx;
  } catch (e) { setError(String(e)); return -1; }
}

/** Add a plugin preset, its track and the assignment as one edit. */
async function addInstrumentTrack(
  presetName: string,
  source: string,
  plugin: string | null,
  program: number | null,
  trackName: string,
  category: string,
  instrument: string,
): Promise<number> {
  try {
    const snap = await api.addInstrumentTrack(presetName, source, plugin, program, trackName, category, instrument);
    applyWithUndo(snap);
    const idx = snap.project.tracks.length - 1;
    setSelectedTrackIndex(idx);
    return idx;
  } catch (e) { setError(String(e)); return -1; }
}

/** Duplicate a track; the backend also forks its preset in the same undo entry. */
async function duplicateTrack(trackIndex: number): Promise<number> {
  const track = project.tracks[trackIndex];
  if (!track) return -1;
  const name = uniqueName(track.name, project.tracks.map((t) => t.name));
  try {
    const snap = await api.duplicateTrack(trackIndex, name);
    applyWithUndo(snap);
    const idx = snap.project.tracks.length - 1;
    setSelectedTrackIndex(idx);
    return idx;
  } catch (e) { setError(String(e)); return -1; }
}

/** Save this track's patch as a new preset and point the track at it. */
async function saveTrackPresetAsNew(trackIndex: number): Promise<boolean> {
  try {
    const snap = await api.saveTrackPresetAsNew(trackIndex);
    applyWithUndo(snap);
    return true;
  } catch (e) { setError(String(e)); return false; }
}

function applyMidiImportSnapshot(snapshot: ProjectSnapshot, firstNewTrack: number | null) {
  applyWithUndo(snapshot);
  setSelectedPatternName(null);
  setEditorSelection(null);
  if (firstNewTrack !== null && firstNewTrack < snapshot.project.tracks.length) {
    setSelectedTrackIndex(firstNewTrack);
  }
  log.info("midi", `Imported ${snapshot.project.tracks.length} project tracks`);
}

/**
 * Keep every index-based track pick on the track it names after a removal: a
 * pick on the removed track falls to the neighbour that takes its place, a
 * pick after it shifts down, and a pick on an earlier track is unchanged.
 */
function reindexAfterRemoval(removed: number, selected: number | null, count: number): void {
  if (selected === null) return;
  setSelectedTrackIndex(
    selected === removed
      ? (count > 0 ? Math.min(removed, count - 1) : null)
      : selected > removed
        ? selected - 1
        : selected,
  );
}

async function removeTrack(trackIndex: number) {
  // The snapshot's clamped pick is not the one this removal must preserve.
  const selected = selectedTrackIndex();
  try {
    const snap = await api.removeTrack(trackIndex);
    applyWithUndo(snap);
    const count = snap.project.tracks.length;
    reindexAfterRemoval(trackIndex, selected, count);
    // A block or editor pick on the removed track names nothing now.
    setBlockSelection((picks) => picks.flatMap((pick) => {
      if (pick.trackIndex === trackIndex) return [];
      return pick.trackIndex > trackIndex ? [{ ...pick, trackIndex: pick.trackIndex - 1 }] : [pick];
    }));
    const editor = editorSelection();
    if (editor?.trackIndex === trackIndex) setEditorSelection(null);
    else if (editor && editor.trackIndex > trackIndex) {
      setEditorSelection({ ...editor, trackIndex: editor.trackIndex - 1 });
    }
  } catch (e) { setError(String(e)); }
}

async function renameTrack(trackIndex: number, newName: string) {
  try {
    const snap = await api.renameTrack(trackIndex, newName);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function extendPhraseRange(trackIndex: number, phraseIndex: number, endBar: number, endBeat: number, endSub: number) {
  try {
    const snap = await api.extendPhraseRange(trackIndex, phraseIndex, endBar, endBeat, endSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function setTempo(tempo: number) {
  try {
    const snap = await api.setTempo(tempo);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function setTimeSignature(numerator: number, denominator: number) {
  try {
    const snap = await api.setTimeSignature(numerator, denominator);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

/** Persist a whole map as one undo entry; an empty map reverts to project BPM. */
async function setTempoMap(points: TempoPoint[]): Promise<boolean> {
  try {
    const snap = await api.setTempoMap(points);
    applyWithUndo(snap);
    return true;
  } catch (e) {
    setError(String(e));
    return false;
  }
}

async function setTimeSigMap(points: TimeSigPoint[]): Promise<boolean> {
  try {
    const snap = await api.setTimeSigMap(points);
    applyWithUndo(snap);
    return true;
  } catch (e) {
    setError(String(e));
    return false;
  }
}

// ============================================================================
// Timeline markers
// ============================================================================

function markerNames(): Set<string> {
  return new Set((project.markers ?? []).map((marker) => marker.name));
}

/** `Marker 1`, `Marker 2`, … — the next name no marker holds yet. */
function nextMarkerName(): string {
  const taken = markerNames();
  let index = 1;
  while (taken.has(`Marker ${index}`)) index++;
  return `Marker ${index}`;
}

async function addMarker(name: string, position: BarBeat): Promise<boolean> {
  setMarkerBusy(true);
  setMarkerError(null);
  try {
    applyWithUndo(await api.addMarker(name, position));
    return true;
  } catch (e) {
    setMarkerError(String(e));
    return false;
  } finally {
    setMarkerBusy(false);
  }
}

/** Add a marker at the playhead, named so the undo history stays one entry. */
async function addMarkerAtPlayhead(): Promise<boolean> {
  const clock = createProjectClock(project.properties);
  const position = clock.barBeatAt(transport().position_beats);
  return addMarker(nextMarkerName(), position);
}

async function moveMarker(name: string, position: BarBeat): Promise<boolean> {
  setMarkerBusy(true);
  setMarkerError(null);
  try {
    applyWithUndo(await api.moveMarker(name, position));
    return true;
  } catch (e) {
    setMarkerError(String(e));
    return false;
  } finally {
    setMarkerBusy(false);
  }
}

async function renameMarker(name: string, newName: string): Promise<boolean> {
  setMarkerBusy(true);
  setMarkerError(null);
  try {
    applyWithUndo(await api.renameMarker(name, newName));
    return true;
  } catch (e) {
    setMarkerError(String(e));
    return false;
  } finally {
    setMarkerBusy(false);
  }
}

async function removeMarker(name: string): Promise<boolean> {
  setMarkerBusy(true);
  setMarkerError(null);
  try {
    applyWithUndo(await api.removeMarker(name));
    return true;
  } catch (e) {
    setMarkerError(String(e));
    return false;
  } finally {
    setMarkerBusy(false);
  }
}

// ============================================================================
// Clip groups
// ============================================================================

/** The group a clip belongs to, or null. Membership is by clip identifier. */
function groupOfClip(trackIndex: number, clipIndex: number): ClipGroup | null {
  const id = project.tracks[trackIndex]?.clips[clipIndex]?.id;
  if (!id) return null;
  return (project.groups ?? []).find((group) => group.members.includes(id)) ?? null;
}

/** Every member of a group, in project order. */
function groupMembers(group: ClipGroup): { trackIndex: number; clipIndex: number }[] {
  const members: { trackIndex: number; clipIndex: number }[] = [];
  project.tracks.forEach((track, trackIndex) => {
    track.clips.forEach((clip, clipIndex) => {
      if (clip.id && group.members.includes(clip.id)) members.push({ trackIndex, clipIndex });
    });
  });
  return members;
}

async function groupClips(clips: api.ClipRefArg[]): Promise<boolean> {
  setGroupBusy(true);
  setGroupError(null);
  try {
    applyWithUndo(await api.groupClips(clips));
    return true;
  } catch (e) {
    const reason = String(e);
    setGroupError(reason);
    setError(i18n.t("clip.groupFailed", { reason }));
    return false;
  } finally {
    setGroupBusy(false);
  }
}

async function ungroupClips(clips: api.ClipRefArg[]): Promise<boolean> {
  setGroupBusy(true);
  setGroupError(null);
  try {
    applyWithUndo(await api.ungroupClips(clips));
    return true;
  } catch (e) {
    const reason = String(e);
    setGroupError(reason);
    setError(i18n.t("clip.ungroupFailed", { reason }));
    return false;
  } finally {
    setGroupBusy(false);
  }
}

/** Move a group by its anchor's delta; returns the refusal reason, or null. */
async function moveClipGroup(
  trackIndex: number,
  clipIndex: number,
  startBar: number,
  startBeat: number,
  startSub: number,
): Promise<string | null> {
  try {
    applyWithUndo(await api.moveClipGroup(trackIndex, clipIndex, startBar, startBeat, startSub));
    return null;
  } catch (e) {
    return String(e);
  }
}

async function setTrackPreset(trackIndex: number, presetName: string | null) {
  const seq = ++trackPresetSeq;
  try {
    const snap = await api.setTrackPreset(trackIndex, presetName);
    // A later change owns the project; this reply predates it.
    if (seq !== trackPresetSeq) return;
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function addPreset(
  name: string, source: string,
  opts?: { program?: number; plugin?: string; patch?: string; file?: string },
) {
  try {
    const snap = await api.addPreset(name, source, opts);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function removePreset(name: string) {
  try {
    const snap = await api.removePreset(name);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function setPresetProgram(name: string, program: number) {
  try {
    const snap = await api.setPresetProgram(name, program);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

/** Admission and completion belong to one document, including pending loads. */
function presetRequestOwner(): () => boolean {
  const generation = documentIdentity().generation;
  const epoch = projectLoadEpoch;
  return () => documentLoadPending === null && projectLoadEpoch === epoch
    && documentIdentity().generation === generation;
}

function parameterRequestOwner(): ParameterRequestOwner {
  const document = presetRequestOwner();
  const stamp = parameterTargetStamp();
  return captureParameterAdmission(stamp, () => document() && parameterTargetStamp() === stamp);
}

/** Parameter-only transactions retain the interaction's original admission. */
async function withParameterGesture<T>(owner: ParameterRequestOwner, token: api.UndoGestureToken | undefined,
  mutate: (token: api.UndoGestureToken) => Promise<T>): Promise<T> {
  if (!owner()) throw new Error(i18n.t("synth.ui.ownerChanged"));
  if (token !== undefined) return mutate(token);
  const opened = await api.beginParameterGesture("preset", owner.requireStamp());
  try {
    if (!owner()) throw new Error(i18n.t("synth.ui.ownerChanged"));
    return await mutate(opened);
  } finally { await api.endOwnedUndoGesture(opened); }
}

async function setPresetParam(name: string, paramName: string, paramValue: string | null, token?: api.UndoGestureToken): Promise<string | null> {
  const ownsRequest = parameterRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await withParameterGesture(ownsRequest, token, opened => api.setPresetParamOwned(opened, name, paramName, paramValue));
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    const message = String(e);
    setError(message);
    return message;
  }
}

async function setPresetParams(name: string, params: [string, string | null][], replaceSynth = false, token?: api.UndoGestureToken): Promise<string | null> {
  const ownsRequest = parameterRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await withParameterGesture(ownsRequest, token, opened => api.setPresetParamsOwned(opened, name, params, replaceSynth));
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Toggle one macro-learn route; the backend owns slot choice and the undo step. */
async function macroLearn(
  name: string,
  macroIndex: number,
  control: string,
  token?: api.UndoGestureToken,
): Promise<{ error: string | null; result: api.MacroLearnResult | null }> {
  const ownsRequest = parameterRequestOwner();
  if (!ownsRequest()) return { error: i18n.t("synth.ui.ownerChanged"), result: null };
  try {
    const response = await withParameterGesture(ownsRequest, token, opened => api.synthMacroLearnOwned(opened, name, macroIndex, control));
    if (!ownsRequest()) return { error: i18n.t("synth.ui.ownerChanged"), result: null };
    applyWithUndo(response.snapshot);
    return { error: null, result: response.result };
  } catch (e) {
    if (!ownsRequest()) return { error: i18n.t("synth.ui.ownerChanged"), result: null };
    const message = String(e);
    setError(message);
    return { error: message, result: null };
  }
}

function applyLibraryListing(listing: LibraryListing | null | undefined) {
  if (!listing || !Array.isArray(listing.patches)) return;
  setLibraryPatches(listing.patches);
  setLibraryWarnings(Array.isArray(listing.warnings) ? listing.warnings : []);
}

/** Re-read the user patch library; a corrupt file only shows up as a warning. */
async function refreshLibrary(): Promise<string | null> {
  try {
    applyLibraryListing(await api.listLibraryPatches());
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Rename the selected project patch; references follow in the same undo step. */
async function renamePreset(name: string, newName: string): Promise<string | null> {
  const ownsRequest = presetRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await api.renamePreset(name, newName);
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Copy a patch and point its players at the copy, so edits stay independent. */
async function duplicatePreset(
  name: string,
  newName: string,
  reassign = true,
): Promise<string | null> {
  const ownsRequest = presetRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await api.duplicatePreset(name, newName, reassign);
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Monotonic seed so each GUI press explores a new direction. */
let nextRandomizeSeed = 1;

/** Replace a patch with a safety-bounded variation in one undo step. */
async function randomizePreset(name: string, token?: api.UndoGestureToken): Promise<string | null> {
  const seed = nextRandomizeSeed;
  const ownsRequest = parameterRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await withParameterGesture(ownsRequest, token, opened => api.randomizePresetOwned(opened, name, seed));
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    nextRandomizeSeed += 1;
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    const message = String(e);
    setError(message);
    return message;
  }
}

/**
 * Replace the whole patch: parameters, effect chain, and the provenance label.
 * Loading A then B then A reproduces A's parameter set exactly.
 */
async function loadSynthPatch(
  name: string,
  patchLabel: string | null,
  params: [string, string | null][],
  effects: import("../types/project").Effect[],
  token?: api.UndoGestureToken,
): Promise<string | null> {
  const seq = ++patchLoadSeq;
  const ownsRequest = parameterRequestOwner();
  if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
  try {
    const snap = await withParameterGesture(ownsRequest, token, opened => api.setPresetParamsOwned(opened, name, params, true, effects, patchLabel));
    // A later click owns the project; this reply predates it.
    if (seq !== patchLoadSeq) return null;
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    applyWithUndo(snap);
    return null;
  } catch (e) {
    if (!ownsRequest()) return i18n.t("synth.ui.ownerChanged");
    if (seq !== patchLoadSeq) return null;
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Persist the current project patch into the user library. */
async function savePatchToLibrary(
  presetName: string,
  name: string,
  category: string | null,
  overwrite = true,
  tags: string[] = [],
): Promise<string | null> {
  try {
    applyLibraryListing(await api.savePatchToLibrary(presetName, name, category, tags, overwrite));
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

async function removeLibraryPatch(name: string): Promise<string | null> {
  try {
    applyLibraryListing(await api.removeLibraryPatch(name));
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

async function savePluginStateFile(name: string, path: string) {
  try {
    const snap = await api.savePluginStateFile(name, path);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function savePluginStateEmbedded(name: string) {
  try {
    const snap = await api.savePluginStateEmbedded(name);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function importFfpPreset(name: string, path: string) {
  try {
    const [snap, report] = await api.importFfpPreset(name, path);
    applyWithUndo(snap);
    return report;
  } catch (e) { setError(String(e)); return null; }
}

/** Connect a control input; returns the refusal text when the backend refuses. */
// Pattern property editing

/** A later pattern-preset write owns the project; an earlier reply predates it. */
let patternPresetSeq = 0;

/**
 * Give a pattern its own preset. Returns the failure text so the panel can
 * show it, and `null` when the write landed (or was superseded).
 */
async function setPatternPreset(patternName: string, presetName: string | null): Promise<string | null> {
  const seq = ++patternPresetSeq;
  try {
    const snap = await api.setPatternPreset(patternName, presetName);
    if (seq === patternPresetSeq) applyWithUndo(snap);
    return null;
  } catch (e) {
    const message = String(e);
    if (seq === patternPresetSeq) setError(message);
    return message;
  }
}

async function setPatternTranspose(patternName: string, transpose: number) {
  try {
    const snap = await api.setPatternTranspose(patternName, transpose);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function setPatternSwing(patternName: string, swing: number | null): Promise<string | null> {
  try {
    const snap = await api.setPatternSwing(patternName, swing);
    applyWithUndo(snap);
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

async function setProjectSwingSubdivision(subdivision: "8th" | "16th"): Promise<string | null> {
  try {
    const snap = await api.setProjectSwingSubdivision(subdivision);
    applyWithUndo(snap);
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

async function setPatternColor(patternName: string, color: string) {
  try {
    const snap = await api.setPatternColor(patternName, color);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

/** Declare a pattern's length; returns the failure so a panel can reset its field. */
async function setPatternBars(patternName: string, bars: number | null): Promise<string | null> {
  try {
    const snap = await api.setPatternBars(patternName, bars);
    applyWithUndo(snap);
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

/** Shorten a pattern and cut what it no longer covers, as one undo entry. */
async function trimPatternToBars(patternName: string, bars: number): Promise<string | null> {
  try {
    const snap = await api.trimPatternToBars(patternName, bars);
    applyWithUndo(snap);
    return null;
  } catch (e) {
    const message = String(e);
    setError(message);
    return message;
  }
}

async function addPattern(name: string, instrumentCategory: string, instrumentName: string, preset?: string) {
  try {
    const snap = await api.addPattern(name, instrumentCategory, instrumentName, preset);
    applyWithUndo(snap);
    setSelectedPatternName(name);
  } catch (e) { setError(String(e)); }
}

async function removePattern(patternName: string) {
  try {
    const snap = await api.removePattern(patternName);
    applyWithUndo(snap);
    if (selectedPatternName() === patternName) setSelectedPatternName(null);
  } catch (e) { setError(String(e)); }
}

async function addClip(
  trackIndex: number, patternName: string,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number,
  offsetQuarters?: number, loopQuarters?: number,
) {
  try {
    const snap = await api.addClip(
      trackIndex, patternName,
      startBar, startBeat, startSub, endBar, endBeat, endSub,
      offsetQuarters, loopQuarters,
    );
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function removeClip(trackIndex: number, clipIndex: number) {
  try {
    const snap = await api.removeClip(trackIndex, clipIndex);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

/** Delete a multi-selection in one undo step; the backend applies it atomically. */
async function removeBlocks(blocks: TimelineBlockRef[]) {
  if (blocks.length === 0) return;
  try {
    const snap = await api.removeBlocks(blocks);
    applyWithUndo(snap);
    setBlockSelection([]);
    setEditorSelection(null);
  } catch (e) { setError(String(e)); }
}

async function moveClip(trackIndex: number, clipIndex: number, startBar: number, startBeat: number, startSub: number) {
  try {
    const snap = await api.moveClip(trackIndex, clipIndex, startBar, startBeat, startSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function moveClipToTrack(
  fromTrackIndex: number, clipIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) {
  try {
    const snap = await api.moveClipToTrack(
      fromTrackIndex, clipIndex, toTrackIndex, startBar, startBeat, startSub,
    );
    applyWithUndo(snap);
    setSelectedTrackIndex(toTrackIndex);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function movePhrase(trackIndex: number, phraseIndex: number, startBar: number, startBeat: number, startSub: number) {
  try {
    const snap = await api.movePhrase(trackIndex, phraseIndex, startBar, startBeat, startSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function resizeClip(trackIndex: number, clipIndex: number, endBar: number, endBeat: number, endSub: number) {
  try {
    const snap = await api.resizeClip(trackIndex, clipIndex, endBar, endBeat, endSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function movePhraseToTrack(
  fromTrackIndex: number, phraseIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) {
  try {
    const snap = await api.movePhraseToTrack(
      fromTrackIndex, phraseIndex, toTrackIndex, startBar, startBeat, startSub,
    );
    applyWithUndo(snap);
    setSelectedTrackIndex(toTrackIndex);
  } catch (e) { setError(String(e)); }
}

async function moveInlinePattern(trackIndex: number, patternIndex: number, startBar: number, startBeat: number, startSub: number) {
  try {
    const snap = await api.moveInlinePattern(trackIndex, patternIndex, startBar, startBeat, startSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function moveInlinePatternToTrack(
  fromTrackIndex: number, patternIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) {
  try {
    const snap = await api.moveInlinePatternToTrack(
      fromTrackIndex, patternIndex, toTrackIndex, startBar, startBeat, startSub,
    );
    applyWithUndo(snap);
    setSelectedTrackIndex(toTrackIndex);
  } catch (e) { setError(String(e)); }
}

async function moveAudioClip(
  clipIndex: number, startBar: number, startBeat: number, startSub: number, track: string | null,
) {
  try {
    const snap = await api.moveAudioClip(clipIndex, startBar, startBeat, startSub, track);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function trimAudioClip(
  clipIndex: number,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number, edge?: "start" | "end",
) {
  try {
    const snap = await api.trimAudioClip(
      clipIndex, startBar, startBeat, startSub, endBar, endBeat, endSub, edge,
    );
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function setPictureClock(clock: Parameters<typeof api.setPictureClock>[0], expectedRevision: number) {
  try {
    const generation = documentIdentity().generation;
    const snapshot = await api.setPictureClock(clock, expectedRevision);
    if (documentIdentity().generation === generation) applyWithUndo(snapshot);
    return null;
  } catch (error) {
    return String(error);
  }
}

async function setAudioClipPlayback(
  clipIndex: number, playback: api.AudioClipPlayback, expectedRevision = appliedRevision,
) {
  try {
    applyWithUndo(await api.setAudioClipPlayback(clipIndex, playback, expectedRevision));
    return null;
  } catch (error) {
    return String(error);
  }
}

async function setAudioClipEnvelope(
  clipIndex: number, envelope: api.AudioClipEnvelope, expectedRevision = appliedRevision,
) {
  try {
    return applyWithUndo(await api.setAudioClipEnvelope(clipIndex, envelope, expectedRevision));
  } catch (error) {
    setError(String(error));
    return false;
  }
}

async function setAudioClipPlacement(
  index: number, placement: api.AudioClipPlacement, expectedRevision = appliedRevision,
) {
  try {
    return applyWithUndo(await api.setAudioClipPlacement(index, placement, expectedRevision));
  } catch (error) {
    setError(String(error));
    return false;
  }
}

async function setAudioClipOffset(clipIndex: number, offsetSeconds: number, expectedRevision: number) {
  try {
    applyWithUndo(await api.setAudioClipOffset(clipIndex, offsetSeconds, expectedRevision));
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function setPatternClipOffset(
  trackIndex: number, clipIndex: number, offsetQuarters: number,
) {
  try {
    applyWithUndo(await api.setPatternClipOffset(trackIndex, clipIndex, offsetQuarters));
  } catch (e) { setError(String(e)); }
}

async function setAudioClipLoop(clipIndex: number, loopSeconds: number | null, expectedRevision: number) {
  try {
    applyWithUndo(await api.setAudioClipLoop(clipIndex, loopSeconds, expectedRevision));
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function setPatternClipLoop(
  trackIndex: number, clipIndex: number, loopQuarters: number | null,
) {
  try {
    applyWithUndo(await api.setPatternClipLoop(trackIndex, clipIndex, loopQuarters));
  } catch (e) { setError(String(e)); }
}

async function splitAudioClip(clipIndex: number, atBar: number, atBeat: number, atSub: number) {
  try {
    const snap = await api.splitAudioClip(clipIndex, atBar, atBeat, atSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

/** Split a pattern clip in one edit; a refusal leaves the clip in place. */
async function splitClip(
  trackIndex: number, clipIndex: number,
  atBar: number, atBeat: number, atSub: number,
) {
  try {
    const snap = await api.splitClip(trackIndex, clipIndex, atBar, atBeat, atSub);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function removeAudioClip(clipIndex: number) {
  try {
    const snap = await api.removeAudioClip(clipIndex);
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); return String(e); }
  return null;
}

async function compAudioTakes(
  track: string,
  take: number,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number,
) {
  try {
    const snap = await api.compAudioTakes(
      track, take, startBar, startBeat, startSub, endBar, endBeat, endSub,
    );
    applyWithUndo(snap);
  } catch (e) { setError(String(e)); }
}

async function setAudioTakeMute(track: string, take: number, muted: boolean) {
  try {
    applyWithUndo(await api.setAudioTakeMute(track, take, muted));
  } catch (e) { setError(String(e)); }
}

async function openSynthNoteOn(presetName: string, note: number) {
  try {
    const preset = project.presets.find(preset => preset.name === presetName);
    if (preset?.plugin === "builtin.open_synth") await playDryKey(presetName, note);
    else await api.openSynthNoteOn(presetName, note);
  } catch (e) { setError(String(e)); }
}

async function openSynthNoteOff(presetName: string, note: number) {
  try { await api.openSynthNoteOff(presetName, note); }
  catch (e) { setError(String(e)); }
}

async function beginPluginParamGesture(presetName: string, trackIndex: number, paramName: string) {
  try {
    await api.beginPluginParamGesture(presetName, trackIndex, paramName);
  } catch (e) {
    setError(String(e));
    throw e;
  }
}

async function updatePluginParamGesture(
  presetName: string,
  paramName: string,
  value: number,
  position: { bar: number; beat: number; subdivision: number },
) {
  try {
    applySnapshot(await api.updatePluginParamGesture(presetName, paramName, value, position));
  } catch (e) {
    setError(String(e));
    throw e;
  }
}

async function endPluginParamGesture(presetName: string, paramName: string) {
  try {
    applyWithUndo(await api.endPluginParamGesture(presetName, paramName));
  } catch (e) {
    setError(String(e));
    throw e;
  }
}

/**
 * Render the mix or the stem set with the dialog's choices. The failure reaches
 * the caller, because the dialog owns the failure text and the retry.
 */
async function renderAudio(request: RenderAudioRequest): Promise<RenderAudioResult> {
  setLoading(true);
  try {
    return await api.renderAudio(request);
  } finally {
    setLoading(false);
  }
}

/** Write the dialog's choices into the project's `export:` block. */
async function setExportSettings(settings: ExportSettings) {
  applyWithUndo(await api.setExportSettings(settings));
}

// ============================================================================
// Derived state
// ============================================================================

const selectedTrack = createMemo<Track | null>(() => {
  const idx = selectedTrackIndex();
  if (idx === null) return null;
  return project.tracks[idx] ?? null;
});

const selectedPattern = createMemo(() => {
  const name = selectedPatternName();
  if (!name) return null;
  // Check top-level patterns first
  const p = project.patterns.find((pd) => pd.name === name);
  if (p) return p;
  // Also check inline_patterns on all tracks
  for (const track of project.tracks) {
    const ip = track.inline_patterns.find((pd) => pd.name === name);
    if (ip) return ip;
  }
  return null;
});

// ============================================================================
// Export
// ============================================================================

export const store = {
  // State
  project,
  dslSource,
  filePath,
  documentIdentity,
  parameterRequestOwner,
  parameterTargetStamp,
  get projectRevision() { return appliedRevision; },
  dirty,
  loading,
  audition,
  auditionActive,
  auditionKind,
  abPair,
  abBusy,
  abError,
  error,
  transport,
  smoothPositionBeats,
  previewCursorBeats,
  scrubPosition,
  setScrubPosition,
  cursorActive,
  undoState,
  undoHistory,
  selectedTrackIndex,
  selectedTrack,
  selectedPresetName,
  selectedPatternName,
  selectedPattern,
  editorSelection,
  blockSelection,
  clipClipboard,
  setClipClipboard,
  activePanel,

  // Actions
  editPerformanceEvent: async (...args: Parameters<typeof api.editPerformanceEvent>) => {
    applyWithUndo(await api.editPerformanceEvent(...args));
  },
  ...createMixerActions(project, applyWithUndo, setError, parameterRequestOwner),
  ...createNotesActions(applyWithUndo, setError),
  initProject,
  initStartupProject,
  newProject,
  newProjectAt,
  setProjectName,
  openFile,
  saveFile,
  unsavedPrompt,
  confirmDiscardChanges,
  answerUnsavedPrompt,
  saveForUnsavedPrompt,
  discardUnsaved,
  updateDsl,
  renamePattern,
  applyAiSnapshot: applyWithUndo,
  /**
   * Publish a document that already came from the backend, without touching
   * history: a preview side swap records no undo entry, and the accept command
   * has already written its own.
   */
  applyPreviewSnapshot: applySnapshot,
  applyExternalSnapshot,
  /** Repaint the history after the preview's accept wrote its one entry. */
  refreshPreviewHistory: refreshUndoState,
  /** A sample add commits its own undo entry; the panel only publishes it. */
  applySampleSnapshot: applyWithUndo,
  undo,
  redo,
  restoreUndoLevel,
  refreshUndoHistory,
  clearUndoHistory,
  addTrack,
  addInstrumentTrack,
  duplicateTrack,
  removeTrack,
  applyMidiImportSnapshot,
  renameTrack,
  extendPhraseRange,
  setTempo,
  setTempoMap,
  setPictureClock,
  setTimeSigMap,
  setTimeSignature,
  setTrackPreset,
  saveTrackPresetAsNew,
  addPreset,
  removePreset,
  setPresetProgram,
  setPresetParam,
  setPresetParams,
  macroLearn,
  renamePreset,
  duplicatePreset,
  randomizePreset,
  loadSynthPatch,
  refreshLibrary,
  savePatchToLibrary,
  removeLibraryPatch,
  libraryPatches,
  libraryWarnings,
  savePluginStateFile,
  savePluginStateEmbedded,
  importFfpPreset,
  setPatternPreset,
  setPatternTranspose,
  setPatternSwing,
  setProjectSwingSubdivision,
  setPatternColor,
  setPatternBars,
  trimPatternToBars,
  addPattern,
  removePattern,
  addClip,
  removeClip,
  splitClip,
  removeBlocks,
  moveClip,
  moveClipToTrack,
  resizeClip,
  movePhrase,
  movePhraseToTrack,
  moveInlinePattern,
  moveInlinePatternToTrack,
  moveAudioClip,
  trimAudioClip,
  setAudioClipEnvelope,
  setAudioClipPlayback,
  setAudioClipOffset,
  setAudioClipPlacement,
  setPatternClipOffset,
  setAudioClipLoop,
  setPatternClipLoop,
  splitAudioClip,
  removeAudioClip,
  compAudioTakes,
  setAudioTakeMute,
  auditionTake,
  takeSolo,
  previewPreset,
  auditionFactoryPatch,
  compareFactoryPatches,
  switchAb,
  clearAbPair,
  previewPattern,
  stopAudition,
  previewDrumPad,
  previewMarketSample,
  stopMarketSample,
  marketSampleAction,
  openSynthNoteOn,
  openSynthNoteOff,
  beginPluginParamGesture,
  updatePluginParamGesture,
  endPluginParamGesture,
  previewTrack,
  previewTrackIndex,
  toggleTrackPreview,
  toggleAudition,
  playbackPlaying,
  auditionPositionSeconds,
  auditionLocalBeats,
  auditionStartBeats,
  auditionArrangementBeats,
  auditionAction,
  previewBlock,
  play,
  pause,
  stop,
  seek,
  adoptTransport,
  startTransportPoll,
  seekToMarker,
  setLoopRegion,
  toggleLoop,
  clearLoopRegion,
  loop,
  loopRegion,
  loopPending,
  loopError,
  punch,
  punchRegion,
  punchPending,
  punchError,
  adoptPunch,
  setPunchRegion,
  addMarker,
  addMarkerAtPlayhead,
  moveMarker,
  renameMarker,
  removeMarker,
  markerBusy,
  markerError,
  groupClips,
  ungroupClips,
  groupOfClip,
  groupMembers,
  moveClipGroup,
  groupBusy,
  groupError,
  applyTakeSnapshot,
  togglePlayPause,
  renderAudio,
  setExportSettings,
  restoreFromRecovery,
  setSelectedTrackIndex,
  setSelectedPresetName,
  setSelectedPatternName,
  setEditorSelection,
  setBlockSelection,
  setActivePanel,
  setError,
};
