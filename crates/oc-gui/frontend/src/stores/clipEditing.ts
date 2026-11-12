/**
 * Audio-clip editing, take selection, bounce and consolidate commands.
 *
 * The wire types live here; `api.ts` re-exports them for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ProjectSnapshot } from "../types/project";

// ============================================================================
// Bounce / consolidate
// ============================================================================

/** Render one track's post-FX signal into `media/` and place it as a clip. */
import type { AudioClipEnvelope, AudioClipPlacement, AudioClipPlayback, BounceMode, BounceResult, BounceTail, ConsolidateResult } from "../types/clipEditing";
export type { AudioClipEnvelope, AudioClipPlacement, AudioClipPlayback, BounceMode, BounceResult, BounceTail, ConsolidateResult } from "../types/clipEditing";

export const bounceTrackToAudio = (
  track: string,
  from: string,
  to: string,
  mode: BounceMode,
  tail: BounceTail,
) => invoke<BounceResult>("bounce_track_to_audio", { track, from, to, mode, tail });

/** Render the selected audio clips of one track into one clip. */
export const consolidateSelection = (track: string, clipIndices: number[]) =>
  invoke<ConsolidateResult>("consolidate_selection", { track, clipIndices });

/** Choose one take over a range: split at the edges, mute the other takes. */
export const compAudioTakes = (
  track: string,
  take: number,
  startBar: number, startBeat: number, startSub: number,
  endBar: number, endBeat: number, endSub: number,
) => invoke<ProjectSnapshot>("comp_audio_takes", {
  track, take, startBar, startBeat, startSub, endBar, endBeat, endSub,
});

/** Mute or unmute every segment of one take; one history entry. */
export const setAudioTakeMute = (track: string, take: number, muted: boolean) =>
  invoke<ProjectSnapshot>("set_audio_take_mute", { track, take, muted });

/** Audition one take through the live render thread; never writes the DSL. */
export const setTakeSolo = (track: string | null, take: number | null) =>
  invoke<{ track: string | null; take: number | null }>("set_take_solo", { track, take });


export const setAudioClipPlayback = (
  clipIndex: number, playback: AudioClipPlayback, expectedRevision: number,
) => invoke<ProjectSnapshot>("set_audio_clip_playback", { clipIndex, playback, expectedRevision });

export const setAudioClipEnvelope = (clipIndex: number, envelope: AudioClipEnvelope, expectedRevision: number) =>
  invoke<ProjectSnapshot>("set_audio_clip_envelope", {
    clipIndex,
    expectedRevision,
    gainDb: envelope.gain_db,
    fadeInSeconds: envelope.fade_in_seconds,
    fadeOutSeconds: envelope.fade_out_seconds,
    crossfadeSeconds: envelope.crossfade_seconds,
    gainEnvelope: envelope.gain_envelope,
  });

export const setAudioClipPlacement = (index: number, placement: AudioClipPlacement, expectedRevision: number) =>
  invoke<ProjectSnapshot>("audio_clip_set_placement", {
    clipIndex: index, startSeconds: placement.start_seconds, nudgeMs: placement.nudge_ms, expectedRevision,
  });

/** Hide `offsetSeconds` of an audio clip's media head; the clip does not move. */
export const setAudioClipOffset = (clipIndex: number, offsetSeconds: number, expectedRevision: number) =>
  invoke<ProjectSnapshot>("set_audio_clip_offset", { clipIndex, offsetSeconds, expectedRevision });

/** Hide `offsetQuarters` of a pattern clip's content head; the clip stays. */
export const setPatternClipOffset = (
  trackIndex: number, clipIndex: number, offsetQuarters: number,
) => invoke<ProjectSnapshot>("set_pattern_clip_offset", {
  trackIndex, clipIndex, offsetQuarters,
});

/** Repeat `loopSeconds` of an audio clip; `null` restores a single pass. */
export const setAudioClipLoop = (clipIndex: number, loopSeconds: number | null, expectedRevision: number) =>
  invoke<ProjectSnapshot>("set_audio_clip_loop", { clipIndex, loopSeconds, expectedRevision });

/** Repeat `loopQuarters` of a pattern clip; `null` restores a single pass. */
export const setPatternClipLoop = (
  trackIndex: number, clipIndex: number, loopQuarters: number | null,
) => invoke<ProjectSnapshot>("set_pattern_clip_loop", {
  trackIndex, clipIndex, loopQuarters,
});
