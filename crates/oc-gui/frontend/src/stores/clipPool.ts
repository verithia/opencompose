/**
 * Typed, ordered view of every clip-shaped object in the project.
 *
 * The document keeps three separate lists: pattern clips on their track, audio
 * clips on `properties.audio_clips` and automation lanes on their owner. This
 * module lists them as one sequence with stable keys, in the same order the
 * Rust pool uses: for each track its pattern clips, then every audio clip,
 * then every automation lane in track order, bus order and master.
 */
import { store } from "./project";
import type { AutomationLane, BarBeat, TimeRange } from "../types/project";

export type ClipKind = "pattern" | "audio" | "automation";

/** Owner of an automation lane, in the form the backend keys it by. */
export type ClipOwner = string;

export interface ClipRef {
  /** Stable key, see [`patternKey`], [`audioKey`] and [`automationKey`]. */
  key: string;
  kind: ClipKind;
  /** Span the timeline draws; null only for a whole-timeline automation lane. */
  time_range: TimeRange | null;
  /** Pattern name, audio clip name or `<owner>/<parameter>`. */
  label: string;
  /** Track owning a pattern clip or a track automation lane. */
  trackIndex: number | null;
  /** Index inside `track.clips`, for a pattern clip. */
  clipIndex: number | null;
  /** Index inside `properties.audio_clips`, for an audio clip. */
  audioIndex: number | null;
  /** Automation owner: `track:<i>`, `bus:<name>` or `master`. */
  owner: ClipOwner | null;
  /** Index inside the owner's `automation` list, for an automation lane. */
  laneIndex: number | null;
}

export function patternKey(trackIndex: number, clipIndex: number): string {
  return `track:${trackIndex}/clip:${clipIndex}`;
}

export function audioKey(index: number): string {
  return `audio:${index}`;
}

export function automationKey(owner: ClipOwner, laneIndex: number): string {
  return `automation:${owner}/${laneIndex}`;
}

/** A whole-timeline automation lane has no span and refuses every clip gesture. */
export function isWholeTimeline(ref: ClipRef | null): boolean {
  return ref?.kind === "automation" && ref.time_range === null;
}

function automationRef(
  owner: ClipOwner,
  trackIndex: number | null,
  lane: AutomationLane,
  laneIndex: number,
): ClipRef {
  return {
    key: automationKey(owner, laneIndex),
    kind: "automation",
    time_range: lane.time_range ?? null,
    label: `${owner}/${lane.param}`,
    trackIndex,
    clipIndex: null,
    audioIndex: null,
    owner,
    laneIndex,
  };
}

/** Every clip in document order; reads the live project, so it stays reactive. */
export function clipPool(): ClipRef[] {
  const project = store.project;
  const clips: ClipRef[] = [];
  project.tracks.forEach((track, trackIndex) => {
    track.clips.forEach((clip, clipIndex) => {
      clips.push({
        key: patternKey(trackIndex, clipIndex),
        kind: "pattern",
        time_range: clip.time_range,
        label: clip.pattern,
        trackIndex,
        clipIndex,
        audioIndex: null,
        owner: null,
        laneIndex: null,
      });
    });
  });
  (project.properties.audio_clips ?? []).forEach((clip, index) => {
    clips.push({
      key: audioKey(index),
      kind: "audio",
      time_range: clip.time_range,
      label: clip.name,
      trackIndex: null,
      clipIndex: null,
      audioIndex: index,
      owner: null,
      laneIndex: null,
    });
  });
  project.tracks.forEach((track, trackIndex) => {
    (track.automation ?? []).forEach((lane, laneIndex) => {
      clips.push(automationRef(`track:${trackIndex}`, trackIndex, lane, laneIndex));
    });
  });
  (project.buses ?? []).forEach((bus) => {
    (bus.automation ?? []).forEach((lane, laneIndex) => {
      clips.push(automationRef(`bus:${bus.name}`, null, lane, laneIndex));
    });
  });
  (project.master?.automation ?? []).forEach((lane, laneIndex) => {
    clips.push(automationRef("master", null, lane, laneIndex));
  });
  return clips;
}

/** The clip a key names, or null when the document changed under it. */
export function clipAt(key: string): ClipRef | null {
  return clipPool().find((clip) => clip.key === key) ?? null;
}

/** The label of a key's clip, or null when it no longer exists. */
export function clipLabel(key: string): string | null {
  return clipAt(key)?.label ?? null;
}

/** One position in the project's own bar:beat:subdivision grammar. */
export function positionText(position: BarBeat): string {
  const subdivision = position.subdivision > 0 ? `:${position.subdivision}` : "";
  return `${position.bar}:${position.beat}${subdivision}`;
}
