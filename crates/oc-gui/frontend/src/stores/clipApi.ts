/**
 * Pattern/audio clip placement, grouping, peaks and source previews.
 *
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ProjectSnapshot } from "../types/project";

// ============================================================================
// Clip management
// ============================================================================

/**
 * Place a pattern clip, optionally carrying a playback window so a copy or a
 * split sounds like the clip it came from.
 */
export const addClip = (
  trackIndex: number, patternName: string,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number,
  offsetQuarters?: number, loopQuarters?: number,
) => invoke<ProjectSnapshot>("add_clip", {
  trackIndex, patternName, startBar, startBeat, startSub, endBar, endBeat, endSub,
  offsetQuarters: offsetQuarters ?? null, loopQuarters: loopQuarters ?? null,
});

export const removeClip = (trackIndex: number, clipIndex: number) =>
  invoke<ProjectSnapshot>("remove_clip", { trackIndex, clipIndex });

/** Split a pattern clip in one edit that keeps its slip and loop. */
export const splitClip = (
  trackIndex: number, clipIndex: number,
  atBar: number, atBeat: number, atSub: number,
) => invoke<ProjectSnapshot>("split_clip", { trackIndex, clipIndex, atBar, atBeat, atSub });

/** One lane block of a timeline multi-selection. */
export interface BlockSelectionArg {
  kind: "clip" | "phrase" | "pattern";
  trackIndex: number;
  index: number;
}

/** Remove every selected lane block in a single undo step. */
export const removeBlocks = (blocks: BlockSelectionArg[]) =>
  invoke<ProjectSnapshot>("remove_blocks", { blocks });

export const moveClip = (trackIndex: number, clipIndex: number, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("move_clip", { trackIndex, clipIndex, startBar, startBeat, startSub });

/** Move every member of a clip's group by that clip's own delta. */
export const moveClipGroup = (trackIndex: number, clipIndex: number, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("move_clip_group", { trackIndex, clipIndex, startBar, startBeat, startSub });

/** One named clip addressed inside its track's clip list. */
export interface ClipRefArg {
  trackIndex: number;
  clipIndex: number;
}

export const groupClips = (clips: ClipRefArg[]) =>
  invoke<ProjectSnapshot>("group_clips", { clips });

export const ungroupClips = (clips: ClipRefArg[]) =>
  invoke<ProjectSnapshot>("ungroup_clips", { clips });

export const moveClipToTrack = (
  fromTrackIndex: number, clipIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) => invoke<ProjectSnapshot>("move_clip_to_track", {
  fromTrackIndex, clipIndex, toTrackIndex, startBar, startBeat, startSub,
});

export const resizeClip = (trackIndex: number, clipIndex: number, endBar: number, endBeat: number, endSub: number) =>
  invoke<ProjectSnapshot>("resize_clip", { trackIndex, clipIndex, endBar, endBeat, endSub });

// ============================================================================
// Audio clips
// ============================================================================

/** Base64 of the peak pyramid payload; decoded off the UI thread in Rust. */
export const audioClipPeaks = (file: string) =>
  invoke<string>("audio_clip_peaks", { file });

export interface AudioClipTransients {
  source_duration_seconds: number;
  analyzed_end_seconds: number;
  resolution_seconds: number;
  transients: { source_seconds: number; strength: number }[];
}

export interface PreviewAudio {
  mime: string;
  base64: string;
}

export const importAlignedStems = (paths: string[], expectedRevision: number) =>
  invoke<{
    snapshot: ProjectSnapshot;
    stem_count: number;
    timestamped_stem_count: number;
    midi_track_count: number;
    midi: { warnings: string[] } | null;
  }>("import_aligned_stems", { paths, expectedRevision });

export const previewAudioFile = (path: string, startSeconds: number, durationSeconds?: number) =>
  invoke<PreviewAudio & { start_s: number; end_s: number; clip_duration_s: number; truncated: boolean }>(
    "audio_file_preview", { path, startSeconds, durationSeconds },
  );

export const previewAudioClipSource = (clipIndex: number, sourceStartSeconds: number, durationSeconds: number) =>
  invoke<{ audio: PreviewAudio }>("preview_audio_clip_source", { clipIndex, sourceStartSeconds, durationSeconds });

export const previewSavedAudition = (path: string) =>
  invoke<PreviewAudio>("preview_saved_audition", { path });

export const openSavedAuditionRender = (path: string) =>
  invoke<string>("open_saved_audition_render", { path });

export const audioClipTransients = (
  clipIndex: number, sourceStartSeconds: number, durationSeconds: number, limit: number,
) => invoke<AudioClipTransients>("audio_clip_transients", {
  clipIndex, sourceStartSeconds, durationSeconds, limit,
});

export const moveAudioClip = (
  clipIndex: number, startBar: number, startBeat: number, startSub: number, track: string | null,
) => invoke<ProjectSnapshot>("move_audio_clip", {
  clipIndex, startBar, startBeat, startSub, track,
});

export const trimAudioClip = (
  clipIndex: number,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number, edge?: "start" | "end",
) => invoke<ProjectSnapshot>("trim_audio_clip", {
  clipIndex, startBar, startBeat, startSub, endBar, endBeat, endSub, edge,
});

export const splitAudioClip = (clipIndex: number, atBar: number, atBeat: number, atSub: number) =>
  invoke<ProjectSnapshot>("split_audio_clip", { clipIndex, atBar, atBeat, atSub });

export const removeAudioClip = (clipIndex: number) =>
  invoke<ProjectSnapshot>("remove_audio_clip", { clipIndex });
