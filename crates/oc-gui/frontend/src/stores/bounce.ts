/**
 * The bounce/consolidate prompt: which edit is open, the range it starts with,
 * and the report line the status bar keeps until the next edit.
 *
 * The backend renders and edits in one command, so this store only owns the
 * dialog, the pending state and the failure. The project snapshot the command
 * returns already carries its undo entry, and the project store applies it.
 */
import { createSignal } from "solid-js";
import type { BarBeat, NoteEvent, Project, ProjectProperties } from "../types/project";
import { durationToBeats } from "../types/project";
import { createProjectClock } from "../utils/projectClock";
import * as api from "./api";
import type { BounceMode, BounceTail } from "./api";
import { store } from "./project";

export type { BounceMode, BounceTail };

/** One open prompt: a bounce of a track, or a consolidate of its audio clips. */
export interface BounceRequest {
  kind: "bounce" | "consolidate";
  track: string;
  /** Audio clips a consolidate replaces; empty for a bounce. */
  clipIndices: number[];
  from: string;
  to: string;
  mode: BounceMode;
  /** What a bounce does with sound that outlasts the range end. */
  tail: BounceTail;
}

const [request, setRequest] = createSignal<BounceRequest | null>(null);
const [pending, setPending] = createSignal(false);
const [failure, setFailure] = createSignal<string | null>(null);
const [report, setReport] = createSignal<string | null>(null);

/** `2:1` for a beat on the beat, `2:1:2` inside it. */
export function formatBarBeat(position: BarBeat): string {
  return position.subdivision === 0
    ? `${position.bar}:${position.beat}`
    : `${position.bar}:${position.beat}:${position.subdivision}`;
}

/** Parse a `bar:beat[:subdivision]` field; null when it is not one. */
export function parseBarBeat(text: string): BarBeat | null {
  const parts = text.trim().split(":");
  if (parts.length === 0 || parts.length > 3) return null;
  const numbers = [1, 1, 0];
  for (let index = 0; index < parts.length; index += 1) {
    const value = Number(parts[index].trim());
    if (!Number.isInteger(value)) return null;
    numbers[index] = value;
  }
  const [bar, beat, subdivision] = numbers;
  if (bar < 1 || beat < 1 || subdivision < 0 || subdivision > 3) return null;
  return { bar, beat, subdivision };
}

/** Why the two fields are not a usable span, as an i18n key, or null. */
export function rangeFailureKey(
  properties: ProjectProperties,
  from: string,
  to: string,
): "bounce.rangeSyntax" | "bounce.rangeOrder" | null {
  const start = parseBarBeat(from);
  const end = parseBarBeat(to);
  if (!start || !end) return "bounce.rangeSyntax";
  try {
    const clock = createProjectClock(properties);
    if (clock.quarterAt(end) <= clock.quarterAt(start)) return "bounce.rangeOrder";
  } catch {
    return "bounce.rangeSyntax";
  }
  return null;
}

/**
 * Bar range a track's content covers, or the first bar when it has none.
 *
 * The prompt starts here so the common case is one click: bouncing a track
 * usually means bouncing everything it plays.
 */
export function trackContentBounds(project: Project, trackName: string): { from: string; to: string } {
  const clock = createProjectClock(project.properties);
  const track = project.tracks.find((candidate) => candidate.name === trackName);
  let start = Number.POSITIVE_INFINITY;
  let end = Number.NEGATIVE_INFINITY;
  const consider = (from: BarBeat, to: BarBeat) => {
    start = Math.min(start, clock.quarterAt(from));
    end = Math.max(end, clock.quarterAt(to));
  };
  for (const clip of track?.clips ?? []) {
    consider(clip.time_range.start, clip.time_range.end);
  }
  for (const phrase of track?.phrases ?? []) {
    if (phrase.time_range) consider(phrase.time_range.start, phrase.time_range.end);
  }
  for (const pattern of track?.patterns ?? []) {
    if (!pattern.time_range) continue;
    const end = { ...pattern.time_range.start, bar: pattern.time_range.start.bar + pattern.repeat };
    consider(pattern.time_range.start, end);
  }
  for (const clip of project.properties.audio_clips ?? []) {
    if (clip.track !== trackName) continue;
    consider(clip.time_range.start, clip.time_range.end);
  }
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
    const meter = clock.meterAtBar(1);
    const bars = (4 / meter.denominator) * meter.numerator;
    return { from: "1:1", to: formatBarBeat(clock.barBeatAt(bars)) };
  }
  return { from: formatBarBeat(clock.barBeatAt(start)), to: formatBarBeat(clock.barBeatAt(end)) };
}

/** Quarter notes one block's written notes span; 0 when it has none. */
function noteSpanQuarters(events: NoteEvent[]): number {
  let span = 0;
  for (const note of events) {
    span = Math.max(span, note.start_beat + durationToBeats(note.duration));
  }
  return span;
}

/**
 * Quarter position where the track's notes stop sounding.
 *
 * Notes, not blocks: the last note may hold past its phrase, and its release
 * outlasts both. Audio clips do not release, so they are not counted here.
 */
export function trackSoundingEnd(project: Project, trackName: string): number {
  const clock = createProjectClock(project.properties);
  const track = project.tracks.find((candidate) => candidate.name === trackName);
  let end = 0;
  for (const phrase of track?.phrases ?? []) {
    if (!phrase.time_range || phrase.events.length === 0) continue;
    end = Math.max(end, clock.quarterAt(phrase.time_range.start) + noteSpanQuarters(phrase.events));
  }
  for (const clip of track?.clips ?? []) {
    const pattern = project.patterns.find((candidate) => candidate.name === clip.pattern);
    if (!pattern || pattern.events.length === 0) continue;
    end = Math.max(end, clock.quarterAt(clip.time_range.start) + noteSpanQuarters(pattern.events));
  }
  for (const event of track?.performance?.events ?? []) {
    end = Math.max(end, event.quarter);
  }
  return end;
}

/**
 * True when the range end falls before the track's last note has finished, so
 * the bounce would cut its release.
 */
export function rangeCutsNote(project: Project, trackName: string, to: string): boolean {
  const position = parseBarBeat(to);
  if (!position) return false;
  try {
    const clock = createProjectClock(project.properties);
    return clock.quarterAt(position) < trackSoundingEnd(project, trackName) - 1e-9;
  } catch {
    return false;
  }
}

/** Open the bounce prompt for one track. */
export function openBounce(track: string, mode: BounceMode = "new_track"): void {
  const bounds = trackContentBounds(store.project, track);
  setFailure(null);
  setRequest({
    kind: "bounce",
    track,
    clipIndices: [],
    from: bounds.from,
    to: bounds.to,
    mode,
    tail: "include_release",
  });
}

/** Open the consolidate prompt for a selection of audio clips on one track. */
export function openConsolidate(track: string, clipIndices: number[]): void {
  const clips = (store.project.properties.audio_clips ?? []).filter((_, index) =>
    clipIndices.includes(index),
  );
  let start = Number.POSITIVE_INFINITY;
  let end = Number.NEGATIVE_INFINITY;
  const clock = createProjectClock(store.project.properties);
  for (const clip of clips) {
    start = Math.min(start, clock.quarterAt(clip.time_range.start));
    end = Math.max(end, clock.quarterAt(clip.time_range.end));
  }
  const range =
    Number.isFinite(start) && Number.isFinite(end)
      ? { from: formatBarBeat(clock.barBeatAt(start)), to: formatBarBeat(clock.barBeatAt(end)) }
      : { from: "1:1", to: "2:1" };
  setFailure(null);
  setRequest({
    kind: "consolidate",
    track,
    clipIndices: [...clipIndices],
    from: range.from,
    to: range.to,
    mode: "in_place",
    tail: "include_release",
  });
}

export function setRange(from: string, to: string): void {
  const current = request();
  if (!current) return;
  setRequest({ ...current, from, to });
}

export function setMode(mode: BounceMode): void {
  const current = request();
  if (!current) return;
  setRequest({ ...current, mode });
}

export function setTail(tail: BounceTail): void {
  const current = request();
  if (!current) return;
  setRequest({ ...current, tail });
}

/** Abandon the prompt; the last report stays until the next edit. */
export function close(): void {
  setRequest(null);
  setFailure(null);
}

/** Render and publish the open request. */
export async function confirm(): Promise<void> {
  const current = request();
  if (!current || pending()) return;
  setPending(true);
  setFailure(null);
  try {
    if (current.kind === "bounce") {
      const result = await api.bounceTrackToAudio(
        current.track,
        current.from,
        current.to,
        current.mode,
        current.tail,
      );
      store.applyAiSnapshot(result.project);
      setReport(result.report);
    } else {
      const result = await api.consolidateSelection(current.track, current.clipIndices);
      store.applyAiSnapshot(result.project);
      setReport(result.report);
    }
    setRequest(null);
  } catch (error) {
    setFailure(String(error));
  } finally {
    setPending(false);
  }
}

export const bounceStore = {
  request,
  pending,
  failure,
  report,
  openBounce,
  openConsolidate,
  setRange,
  setMode,
  setTail,
  close,
  confirm,
};
