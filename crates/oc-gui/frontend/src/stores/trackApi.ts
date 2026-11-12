/**
 * Track, preset, synth-patch and library commands, plus project properties.
 *
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  Effect,
  ParamValue,
  ProjectSnapshot,
  TempoPoint,
  TimeSigPoint,
} from "../types/project";
import type { UndoGestureToken } from "./api";

// ============================================================================
// Track mutations
// ============================================================================

export const setMasterVolume = (volumeDb: number) =>
  invoke<ProjectSnapshot>("set_master_volume", { volumeDb });

export const updateTrackMix = (trackIndex: number, volumeDb?: number, pan?: number) =>
  invoke<ProjectSnapshot>("update_track_mix", {
    trackIndex, volumeDb: volumeDb ?? null, pan: pan ?? null,
  });

/**
 * Open a fader drag. The pre-gesture project stays on the backend until the
 * release commit, so the live updates in between record no undo entry.
 */
export const beginMixerTouch = (target: string) =>
  invoke<void>("begin_mixer_touch", { target });

/** Move a fader without serializing or returning the whole document. */
export const updateMixerTouch = (target: string, volumeDb?: number, pan?: number) =>
  invoke<void>("update_mixer_touch", {
    target, volumeDb: volumeDb ?? null, pan: pan ?? null,
  });

/** One destination a track's main output feeds. */
export interface DestinationLatency {
  destination: string;
  /** Latency the destination already carries when the route lands. */
  reference_samples: number;
  /** Delay the plan applies to this route after the manual offset. */
  compensation_samples: number;
  /** The readable diagnostic's delta when the destination path is later. */
  diagnostic_delta: number | null;
}

/** Three-part latency readout for one track. */
export interface TrackLatencyReadout {
  track: string;
  reported_samples: number;
  requested_offset_samples: number;
  applied_offset_samples: number;
  offset_clamped: boolean;
  effective_samples: number;
  bypassed: boolean;
  destinations: DestinationLatency[];
}

export interface TrackLatencyReadoutResponse {
  readout: TrackLatencyReadout;
  note: string | null;
}

export const trackLatencyReadout = (trackIndex: number) =>
  invoke<TrackLatencyReadoutResponse>("track_latency_readout", { trackIndex });

export const setTrackMute = (trackIndex: number, mute: boolean) =>
  invoke<ProjectSnapshot>("set_track_mute", { trackIndex, mute });

export const setTrackSolo = (trackIndex: number, solo: boolean) =>
  invoke<ProjectSnapshot>("set_track_solo", { trackIndex, solo });

export const addTrack = (
  name: string,
  instrumentCategory: string,
  instrumentName: string,
  preset: string | null = null,
) => invoke<ProjectSnapshot>("add_track", { name, instrumentCategory, instrumentName, preset });

/** Add a plugin preset and the track that plays it in one document edit. */
export const addInstrumentTrack = (
  presetName: string,
  source: string,
  plugin: string | null,
  program: number | null,
  trackName: string,
  instrumentCategory: string,
  instrumentName: string,
) =>
  invoke<ProjectSnapshot>("add_instrument_track", {
    presetName,
    source,
    plugin,
    program,
    trackName,
    instrumentCategory,
    instrumentName,
  });

/** Duplicate a track and give the copy its own preset in one undo entry. */
export const duplicateTrack = (trackIndex: number, name: string) =>
  invoke<ProjectSnapshot>("duplicate_track", { trackIndex, name });

export const removeTrack = (trackIndex: number) =>
  invoke<ProjectSnapshot>("remove_track", { trackIndex });

export const renameTrack = (trackIndex: number, newName: string) =>
  invoke<ProjectSnapshot>("rename_track", { trackIndex, newName });

export const setTrackPreset = (trackIndex: number, presetName: string | null) =>
  invoke<ProjectSnapshot>("set_track_preset", { trackIndex, presetName });

/** Give a pattern its own preset; its clips play it instead of the host track's. */
export const setPatternPreset = (patternName: string, preset: string | null) =>
  invoke<ProjectSnapshot>("set_pattern_preset", { patternName, preset });

/** Fork this track's preset; only this track moves to the copy. */
export const saveTrackPresetAsNew = (trackIndex: number) =>
  invoke<ProjectSnapshot>("save_track_preset_as_new", { trackIndex });

export const editPerformanceEvent = (
  trackIndex: number, trackName: string, eventIndex: number,
  expected: import("../types/project").PerformanceEvent,
  replacement: import("../types/project").PerformanceEvent,
) => invoke<ProjectSnapshot>("edit_performance_event", { trackIndex, trackName, eventIndex, expected, replacement });

export const addPreset = (
  name: string, source: string,
  opts?: { program?: number; plugin?: string; patch?: string; file?: string },
) =>
  invoke<ProjectSnapshot>("add_preset", {
    name, source,
    program: opts?.program ?? null,
    plugin: opts?.plugin ?? null,
    patch: opts?.patch ?? null,
    file: opts?.file ?? null,
  });

export const removePreset = (name: string) =>
  invoke<ProjectSnapshot>("remove_preset", { name });

export const setPresetProgram = (name: string, program: number) =>
  invoke<ProjectSnapshot>("set_preset_program", { name, program });

export const setPresetParam = (name: string, paramName: string, paramValue: string | null) =>
  invoke<ProjectSnapshot>("set_preset_param", { name, paramName, paramValue });

export const setPresetParams = (
  name: string,
  params: [string, string | null][],
  replaceSynth = false,
  effects?: Effect[],
  patchLabel?: string | null,
) => invoke<ProjectSnapshot>("set_preset_params", {
  name,
  params,
  ...(replaceSynth ? { replaceSynth: true } : {}),
  ...(effects ? { effects } : {}),
  ...(patchLabel != null ? { patchLabel } : {}),
});

/** Owner-scoped writes reject a gesture from another or replaced document. */
export const setPresetParamOwned = (
  token: UndoGestureToken, name: string, paramName: string, paramValue: string | null,
) => invoke<ProjectSnapshot>("set_preset_param_owned", { name, paramName, paramValue, token });

export const setPresetParamsOwned = (
  token: UndoGestureToken,
  name: string,
  params: [string, string | null][],
  replaceSynth = false,
  effects?: Effect[],
  patchLabel?: string | null,
) => invoke<ProjectSnapshot>("set_preset_params_owned", {
  name, params, token,
  ...(replaceSynth ? { replaceSynth: true } : {}),
  ...(effects ? { effects } : {}),
  ...(patchLabel != null ? { patchLabel } : {}),
});

/** One macro-learn toggle: bind, unbind, refuse or report a full matrix. */
export type MacroLearnStatus = "bound" | "unbound" | "unsupported" | "full";

export interface MacroLearnResult {
  status: MacroLearnStatus;
  /** 1-based matrix slot that changed, when one did. */
  slot: number | null;
  /** Numeric modulation destination the control drives. */
  destination: number | null;
  /** Numeric `mod.N.source` written on a bind. */
  source: number | null;
}

export interface MacroLearnResponse {
  snapshot: ProjectSnapshot;
  result: MacroLearnResult;
}


/** Response of `import_external_preset`: the file's own provenance and values. */
export interface ExternalPresetImport {
  /** File name, used as the preset's provenance label. */
  label: string;
  /** Name and author declared inside the file. */
  name: string;
  author: string;
  /** Every value the file declares, plus the `serum_file` import path. */
  params: [string, string | null][];
  /** Mapping notes for values the engine could not express. */
  warnings: string[];
}

export const importExternalPreset = (path: string) =>
  invoke<ExternalPresetImport>("import_external_preset", { path });

/** Wire shape of `synth_waveform_preview`; extra fields stay optional. */
export interface WaveformPreview {
  mono: number[];
  traces: number[][];
  more_voices: boolean;
  voices: number;
  peak: number;
  rms: number;
  samples: number;
  sample_rate: number;
  note: number;
  sync_resets: number;
}

export const synthWaveformPreview = (
  params: [string, string | null][],
  oscillator: number,
  samples: number,
) => invoke<WaveformPreview>("synth_waveform_preview", { params, oscillator, samples });

export const synthMacroLearn = (name: string, macroIndex: number, control: string) =>
  invoke<MacroLearnResponse>("synth_macro_learn", { name, macroIndex, control });

export const synthMacroLearnOwned = (token: UndoGestureToken, name: string, macroIndex: number, control: string) =>
  invoke<MacroLearnResponse>("synth_macro_learn_owned", { name, macroIndex, control, token });

export const duplicatePreset = (name: string, newName: string, reassign = true) =>
  invoke<ProjectSnapshot>("duplicate_preset", { name, newName, reassign });

export const renamePreset = (name: string, newName: string) =>
  invoke<ProjectSnapshot>("rename_preset", { name, newName });

/** Replace a preset's parameters with a seeded, safety-bounded variation. */
export const randomizePreset = (name: string, seed: number) =>
  invoke<ProjectSnapshot>("randomize_preset", { name, seed });

export const randomizePresetOwned = (token: UndoGestureToken, name: string, seed: number) =>
  invoke<ProjectSnapshot>("randomize_preset_owned", { name, seed, token });

/** One entry of the on-disk user library, as listed by the backend. */
export interface LibraryPatch {
  name: string;
  category: string;
  tags: string[];
  patch: string | null;
  params: [string, ParamValue][];
  effects: Effect[];
  /** Browsing preferences stored in the entry's own file. */
  favorite?: boolean;
  /** Unix seconds of the last whole-patch load, if it was ever loaded. */
  last_used_at?: number | null;
}

export interface LibraryListing {
  patches: LibraryPatch[];
  warnings: string[];
  replaced: boolean;
}

/** Browsing preferences for one patch, the unit the browser sorts by. */
export interface PatchUsage {
  favorite: boolean;
  last_used_at: number | null;
}

/** Factory rows have no library file, so their usage lives in this store. */
export interface PatchPreferences {
  version: number;
  /** Keyed by `factory:<name>`; anything else is ignored by the browser. */
  patches: Record<string, PatchUsage>;
}

export const listLibraryPatches = () => invoke<LibraryListing>("list_library_patches");

export const listPatchPreferences = () => invoke<PatchPreferences>("list_patch_preferences");

/** `tier` is `user` for library rows and `factory` for the built-in bank. */
export const setPatchFavorite = (name: string, tier: string, favorite: boolean) =>
  invoke<boolean>("set_patch_favorite", { name, tier, favorite });

export const savePatchToLibrary = (
  presetName: string,
  name: string,
  category: string | null,
  tags: string[],
  overwrite: boolean,
) => invoke<LibraryListing>("save_patch_to_library", { presetName, name, category, tags, overwrite });

export const exportPatchBundle = (presetName: string, category: string | null, path: string) =>
  invoke<void>("export_patch_bundle", { presetName, category, path });

export const importPatchBundle = (path: string) =>
  invoke<LibraryPatch>("import_patch_bundle", { path });

export const removeLibraryPatch = (name: string) =>
  invoke<LibraryListing>("remove_library_patch", { name });

// ============================================================================
// Project properties
// ============================================================================

export const setTempo = (tempo: number) => invoke<ProjectSnapshot>("set_tempo", { tempo });
export const setTempoMap = (points: TempoPoint[]) =>
  invoke<ProjectSnapshot>("set_tempo_map", { points });
export const setTimeSigMap = (points: TimeSigPoint[]) =>
  invoke<ProjectSnapshot>("set_time_sig_map", { points });
export const setProjectName = (name: string) => invoke<ProjectSnapshot>("set_project_name", { name });
