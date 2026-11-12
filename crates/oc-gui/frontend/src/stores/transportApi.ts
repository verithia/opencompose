/**
 * Transport, audition, export and picture-clock commands.
 *
 * Shared result types that still live in the API barrel are imported as types;
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import { pauseWebAudio, startNativeAudio, type NativeAudioBlocker } from "./webAudioFocus";
import type { BuildSnapshot, AbSnapshot, LoopState, PunchState, Quantise, TransportSnapshot } from "../types/transport";
import type {
  BarBeat,
  Effect,
  ExportSettings,
  PictureClock,
  PictureRate,
  ProjectSnapshot,
} from "../types/project";
import type {
  ExportCapabilities,
  RenderAudioRequest,
  RenderAudioResult,
} from "../types/export";

// ============================================================================
// Audio / Transport
// ============================================================================

export const previewPreset = (presetName: string, startPaused?: boolean) => startNativeAudio(() => invoke<void>("preview_preset", { presetName, startPaused }));
export const auditionFactoryPatch = (
  params: [string, string | null][],
  label: string,
  effects?: Effect[],
  startPaused?: boolean,
) => startNativeAudio(() => invoke<void>("audition_factory_patch", {
  params,
  label,
  startPaused,
  ...(effects && effects.length ? { effects } : {}),
}));
/** Render the same segment for two candidates; B comes back aligned to A. */
export const compareFactoryPatches = (
  a: { label: string; params: [string, string | null][]; effects: Effect[] },
  b: { label: string; params: [string, string | null][]; effects: Effect[] },
  startPaused?: boolean,
) => startNativeAudio(() => invoke<AbSnapshot>("compare_factory_patches", {
  aLabel: a.label,
  aParams: a.params,
  aEffects: a.effects,
  bLabel: b.label,
  bParams: b.params,
  bEffects: b.effects,
  startPaused,
}));
export const switchAb = () => startNativeAudio(() => invoke<AbSnapshot>("switch_ab"));
export const getAbAuditionState = () => invoke<AbSnapshot | null>("get_ab_audition_state");

/** What the render thread is preparing now; null when nothing is being built. */
export const projectBuildProgress = () =>
  invoke<BuildSnapshot | null>("project_build_progress");
export const previewPattern = (patternName: string, startPaused?: boolean) => startNativeAudio(() => invoke<void>("preview_pattern", { patternName, startPaused }));
export const stopAudition = () => invoke<void>("stop_audition");
export const pauseAudition = () => invoke<TransportSnapshot>("pause_audition");
export const resumeAudition = () => startNativeAudio(() => invoke<TransportSnapshot>("resume_audition"));
// The first press can open a native device. Keep its release and any retrigger
// behind it while allowing other notes in a chord to start independently.
const pendingSynthKeys = new Map<string, Promise<unknown>>();

function sendSynthKey<T>(presetName: string, note: number, send: () => Promise<T>) {
  const key = JSON.stringify([presetName, note]);
  const pending = pendingSynthKeys.get(key);
  const request = pending ? pending.then(send, send) : send();
  pendingSynthKeys.set(key, request);
  const clear = () => {
    if (pendingSynthKeys.get(key) === request) pendingSynthKeys.delete(key);
  };
  void request.then(clear, clear);
  return request;
}

export const openSynthNoteOn = (
  presetName: string, note: number, velocity = 108,
  beforeStart?: () => Promise<boolean> | undefined,
) => startNativeAudio(() => sendSynthKey(presetName, note, () => {
  const start = () => invoke<TransportSnapshot | null>("open_synth_note_on", { presetName, note, velocity });
  const parking = beforeStart?.();
  return parking ? parking.then(ready => ready ? start() : null) : start();
}));
export const openSynthNoteOff = (presetName: string, note: number) =>
  sendSynthKey(presetName, note, () => invoke<void>("open_synth_note_off", { presetName, note }));
export const warmOpenSynthKeyboard = (presetName: string) =>
  invoke<void>("warm_open_synth_keyboard", { presetName });
export const resumeTrackPreview = (trackIndex: number) => startNativeAudio(() => invoke<TransportSnapshot>("resume_track_preview", { trackIndex }));
export const previewTrack = (trackIndex: number, startPaused?: boolean) =>
  startNativeAudio(() => invoke<TransportSnapshot>("preview_track", { trackIndex, startPaused }));
export const previewBlock = (
  trackIndex: number,
  start: BarBeat,
  end: BarBeat,
  startPaused?: boolean,
  audioClipName?: string,
) => startNativeAudio(() => invoke<string[]>("preview_block", {
  trackIndex, start, end, startPaused, audioClipName,
}));
export const renderAudio = (request: RenderAudioRequest) =>
  invoke<RenderAudioResult>("render_audio", { request });
export const cancelExport = () => invoke<void>("cancel_export");
export const exportTargetExists = (path: string) =>
  invoke<boolean>("export_target_exists", { path });
/** Containers this build can encode; the dialog disables the rest. */
export const exportCapabilities = () => invoke<ExportCapabilities>("export_capabilities");
export const revealExport = (path: string) => invoke<void>("reveal_export", { path });
export const setExportSettings = (settings: ExportSettings) =>
  invoke<ProjectSnapshot>("set_export_settings", { settings });
export const transportPlay = () => startNativeAudio(() => invoke<TransportSnapshot>("transport_play"));
export const transportPause = () => invoke<TransportSnapshot>("transport_pause");
export const transportStop = () => {
  pauseWebAudio();
  return invoke<TransportSnapshot>("transport_stop");
};
export type { PictureRate } from "../types/project";

export const setPictureClock = (clock: PictureClock | null, expectedRevision: number) =>
  invoke<ProjectSnapshot>("set_picture_clock", { clock, expectedRevision });

export interface PicturePoint {
  timecode: string;
  frame_offset: number;
  project_seconds: number;
  frame_seconds: number;
  offset_to_frame_ms: number;
  quarter: number | null;
  nearest_position: BarBeat | null;
  offset_to_grid_ms: number | null;
}

export const pictureTimecode = (
  rate: PictureRate,
  origin: string,
  points: ({ timecode: string } | { seconds: number } | { position: BarBeat })[],
) => invoke<{ points: PicturePoint[] }>("picture_timecode", { request: { rate, origin, points } });

export const transportSeek = (beat: number) => invoke<TransportSnapshot>("transport_seek", { beat });
export const transportMute = () => invoke<void>("transport_mute");
export const transportUnmute = () => invoke<void>("transport_unmute");
export const startRecording = () =>
  startNativeAudio(() => invoke<TransportSnapshot>("start_recording"));


export async function nativePlaybackBlocker(): Promise<NativeAudioBlocker | null> {
  const state = await getTransportState();
  if (state.playing || state.starting || state.audition_active
      || state.audition?.status === "loading" || state.record?.recording) return "busy";
  if (state.native_monitoring) return "monitoring";
  return state.live_synth_sounding ? "synth" : null;
}

export const getTransportState = () => invoke<TransportSnapshot>("get_transport_state");
export const setLoopRegion = (startBeats: number | null, endBeats: number | null, enabled: boolean) =>
  invoke<LoopState>("set_loop_region", { startBeats, endBeats, enabled });
export const setPunchRegion = (
  startBeats: number | null,
  endBeats: number | null,
  enabled: boolean,
) => invoke<PunchState>("set_punch_region", { startBeats, endBeats, enabled });
export const setMidiRecordSettings = (quantise: Quantise | null, overdub: boolean | null) =>
  invoke<{ target: string | null; quantise: Quantise; overdub: boolean; input: string | null }>(
    "set_midi_record_settings",
    { quantise, overdub },
  );
export const addMarker = (name: string, position: BarBeat) =>
  invoke<ProjectSnapshot>("add_marker", { name, position });
export const moveMarker = (name: string, position: BarBeat) =>
  invoke<ProjectSnapshot>("move_marker", { name, position });
export const renameMarker = (name: string, newName: string) =>
  invoke<ProjectSnapshot>("rename_marker", { name, newName });
export const removeMarker = (name: string) =>
  invoke<ProjectSnapshot>("remove_marker", { name });
