/**
 * Clip content geometry shared by the timeline and the clip actions.
 *
 * Everything here reads the project or the peak pyramid and writes nothing, so
 * a menu, a drag and a dispatch all agree on how long a clip is and why a loop
 * cannot start.
 */
import { hitWrittenQuarters } from "./drumkit";
import { store } from "../stores/project";
import { peaksFor } from "../stores/audioPeaks";
import { i18n } from "../stores/i18n";
import type { AudioClip, NoteEvent, PatternDef } from "../types/project";

/** Shortest clip loop the timeline offers, matching the backend's refusals. */
export const MIN_LOOP_SECONDS = 0.1;
export const MIN_LOOP_QUARTERS = 0.25;

export function findPattern(name: string): PatternDef | null {
  return store.project.patterns.find((pattern) => pattern.name === name) ?? null;
}

export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

/** True for a rest placeholder, which carries no pitch and sounds nothing. */
export function isRestEvent(event: NoteEvent): boolean {
  return event.modifiers?.some(([key]) => key === "rest") ?? false;
}

/** A note's length in quarter notes, including dots and triplets. */
export function durationToBeats(note: NoteEvent): number {
  const base = note.duration.base;
  const baseBeats = base === "Whole" ? 4
    : base === "Half" ? 2
    : base === "Quarter" ? 1
    : base === "Eighth" ? 0.5
    : base === "Sixteenth" ? 0.25
    : base === "ThirtySecond" ? 0.125
    : 0.0625;
  let beats = baseBeats;
  let add = baseBeats / 2;
  for (let i = 0; i < note.duration.dots; i++) {
    beats += add;
    add /= 2;
  }
  if (note.duration.triplet) beats *= 2 / 3;
  return Math.max(beats, 0.125);
}

/** Quarter notes the pattern's written events and hits span. */
export function patternContentQuarters(pattern: PatternDef | null): number {
  if (!pattern) return 0;
  const notes = pattern.events.map((event) => event.start_beat + durationToBeats(event));
  const hits = pattern.hits.map(hitWrittenQuarters);
  return [...notes, ...hits].reduce((max, value) => Math.max(max, value), 0);
}

/** Bars a pattern occupies: its declared `bars:`, else the last bar its content touches. */
export function patternLengthBars(pattern: PatternDef | null): number {
  if (!pattern) return 0;
  if (pattern.bars != null && Number.isFinite(pattern.bars) && pattern.bars > 0) {
    return pattern.bars;
  }
  return Math.max(1, Math.ceil(patternContentQuarters(pattern) / 4 - 1e-9));
}

/**
 * Quarter notes a placement may use. A pattern with no content still occupies
 * one bar, so its first note has room.
 */
export function patternLengthQuarters(pattern: PatternDef | null): number {
  return patternLengthBars(pattern) * 4;
}

/** Quarter notes of pattern content a clip of `rangeQuarters` does not show. */
export function patternSlipLimit(patternName: string, rangeQuarters: number): number {
  return Math.max(0, patternContentQuarters(findPattern(patternName)) - rangeQuarters);
}

/** Stretched media length in seconds, read from the clip's peak pyramid. */
export function clipContentSeconds(clip: AudioClip): number {
  const pyramid = peaksFor(clip.file);
  if (!pyramid) return 0;
  const media = pyramid.frames / pyramid.sample_rate;
  return clip.stretch_mode === "off" ? media : media * (clip.stretch ?? 1);
}

/** Why an audio clip cannot loop right now, or null when it can. */
export function audioClipLoopReason(clip: AudioClip, clipSeconds: number): string | null {
  const content = clipContentSeconds(clip);
  if (content <= 0) return i18n.t("clip.loopNeedsMedia");
  if (content < MIN_LOOP_SECONDS) return i18n.t("clip.loopMediaTooShort");
  if (clipSeconds < MIN_LOOP_SECONDS) return i18n.t("clip.loopNeedsLength");
  const headroom = Math.min(
    Math.max(0, content - (clip.offset_seconds ?? 0)),
    clipSeconds,
  );
  return headroom < MIN_LOOP_SECONDS ? i18n.t("clip.loopNeedsContent") : null;
}

/** Natural loop length for an audio clip: the media left, but at most half the clip. */
export function audioClipLoopDefault(clip: AudioClip, clipSeconds: number): number {
  const headroom = Math.min(
    Math.max(0, clipContentSeconds(clip) - (clip.offset_seconds ?? 0)),
    clipSeconds,
  );
  return Math.min(headroom, Math.max(MIN_LOOP_SECONDS, clipSeconds / 2));
}

export function roundLoopSeconds(seconds: number): number {
  return Math.round(seconds * 1000) / 1000;
}
