import { audioClipBounds } from "../utils/audioClipPlacement";
/** Read-only clip provenance and placement, shared by menus and the inspector. */
import { createSignal } from "solid-js";
import { store } from "./project";
import { clipAt, positionText, type ClipRef } from "./clipPool";
import { findPattern, isRestEvent } from "../utils/clipContent";
import { trackDisplayName } from "../utils/trackLabel";
import { createProjectClock } from "../utils/projectClock";
import { effectivePatternSound, patternSoundLabel, patternSoundOrigin, type EffectivePatternSound } from "../utils/patternSound";
import { type TimeRange, type Track } from "../types/project";
import { i18n } from "./i18n";

export interface ClipInfoRow { label: string; value: string; path?: boolean }
export interface ClipInfo { title: string; kind: ClipRef["kind"]; rows: ClipInfoRow[] }
const [inspectedKey, inspect] = createSignal<string | null>(null);
export const clipInfoUi = { inspectedKey, inspect };

/** Resolve a displayed document-relative path without claiming filesystem existence. */
export function displayMediaPath(file: string, project: string | null): string {
  if (!project || /^(?:[/\\]|[A-Za-z]:[/\\])/.test(file)) return file;
  const slash = project.includes("\\") ? "\\" : "/";
  const base = project.replace(/[/\\][^/\\]*$/, "");
  if (base === project) return file;
  const parts: string[] = [];
  for (const part of `${base}/${file}`.replace(/\\/g, "/").split("/")) {
    if (part === ".") continue;
    if (part === ".." && parts.length > 1) parts.pop();
    else parts.push(part);
  }
  return parts.join(slash);
}

type AddRow = (label: Parameters<typeof i18n.t>[0], value: unknown, path?: boolean) => void;

function addPlacement(row: AddRow, range: TimeRange | null, ppq = 0) {
  if (!range) return;
  const clock = createProjectClock(store.project.properties);
  const start = clock.quarterAt(range.start, ppq), end = clock.quarterAt(range.end, ppq);
  row("clip.info.position", `${positionText(range.start)} – ${positionText(range.end)}`);
  row("clip.info.duration", i18n.t("clip.info.durationValue", { beats: +(end - start).toFixed(3), seconds: +(clock.secondsAt(end) - clock.secondsAt(start)).toFixed(3) }));
}

function addSound(row: AddRow, sound: EffectivePatternSound) {
  const {preset, presetName, instrument, engine} = sound;
  row("tooltip.preset", presetName ? patternSoundLabel(sound) : null);
  row("clip.info.soundOrigin", patternSoundOrigin(sound));
  row("tooltip.source", preset?.source);
  row("tooltip.plugin", preset?.plugin ?? engine);
  row("clip.info.patch", preset?.patch);
  row("clip.info.program", preset?.program);
  const file = preset?.file;
  row("clip.info.file", file ? displayMediaPath(file, store.filePath()) : null, true);
  row("clip.info.instrument", `${instrument.category}/${instrument.name}`);
  return preset?.effects.length ?? 0;
}

export function timelineInfoKey(kind: "clip" | "phrase" | "pattern", trackIndex: number, index: number): string {
  return kind === "clip" ? `track:${trackIndex}/clip:${index}` : `lane:${kind}:${trackIndex}:${index}`;
}

export function clipInformation(key: string): ClipInfo | null {
  const rows: ClipInfoRow[] = [];
  const row: AddRow = (label, value, path = false) => {
    if (value !== null && value !== undefined && value !== "") rows.push({ label: i18n.t(label), value: String(value), path });
  };
  const lane = /^lane:(phrase|pattern):(\d+):(\d+)$/.exec(key);
  if (lane) {
    const track = store.project.tracks[Number(lane[2])];
    if (!track) return null;
    const phrase = lane[1] === "phrase" ? track.phrases[Number(lane[3])] : undefined;
    const drum = lane[1] === "pattern" ? track.patterns[Number(lane[3])] : undefined;
    const block = phrase ?? drum;
    if (!block) return null;
    row("clip.info.track", trackDisplayName(track));
    addPlacement(row, block.time_range);
    addSound(row, effectivePatternSound(store.project, track));
    row("clip.info.events", i18n.t("clip.info.eventValue", { notes: phrase?.events.filter(event => !isRestEvent(event)).length ?? 0, hits: drum?.hits.length ?? 0 }));
    return { title: block.name, kind: "pattern", rows };
  }
  const ref = clipAt(key);
  if (!ref) return null;
  const track = ref.kind === "audio"
    ? store.project.tracks.find(t => t.name === store.project.properties.audio_clips?.[ref.audioIndex!]?.track)
    : ref.trackIndex === null ? undefined : store.project.tracks[ref.trackIndex];
  row("clip.info.track", track ? trackDisplayName(track) : ref.owner);
  if (ref.kind !== "audio") addPlacement(row, ref.time_range, ref.ppq);
  if (ref.kind === "audio") {
    const clip = store.project.properties.audio_clips?.[ref.audioIndex!];
    if (!clip) return null;
    const bounds = audioClipBounds(createProjectClock(store.project.properties), clip);
    row("clip.info.position", `${positionText(clip.time_range.start)} – ${positionText(clip.time_range.end)}`);
    row("clipPlacement.startSeconds", `${bounds.startSeconds.toFixed(6)} s`);
    row("clip.info.duration", i18n.t("clip.info.durationValue", {
      beats: +(bounds.endQuarter - bounds.startQuarter).toFixed(3), seconds: +bounds.durationSeconds.toFixed(6),
    }));
    row("clip.info.file", displayMediaPath(clip.file, store.filePath()), true);
    row("clip.info.sourceOffset", `${clip.offset_seconds} s`);
    row("clip.info.gain", `${clip.gain_db} dB`);
    row("clip.info.fades", `${clip.fade_in_seconds} / ${clip.fade_out_seconds} s`);
    row("clip.info.stretch", `${clip.stretch ?? 1}× · ${clip.stretch_mode ?? "off"}`);
    row("clip.info.transpose", `${clip.pitch ?? 0} st`);
    if (clip.loop_seconds) row("clip.info.loop", `${clip.loop_seconds} s`);
    if (clip.take) row("clip.info.take", `${clip.take_group ?? ""} · ${clip.take}`);
  } else if (ref.kind === "pattern") {
    const clip = track?.clips[ref.clipIndex!];
    const pattern = clip ? findPattern(clip.pattern) : null;
    row("clip.info.pattern", clip?.pattern);
    const presetEffects = track ? addSound(row, effectivePatternSound(store.project, track, pattern)) : 0;
    row("clip.info.events", i18n.t("clip.info.eventValue", { notes: pattern?.events.filter(event => !isRestEvent(event)).length ?? 0, hits: pattern?.hits.length ?? 0 }));
    row("clip.info.sourceOffset", `${clip?.offset_quarters ?? 0} q`);
    if (clip?.loop_quarters) row("clip.info.loop", `${clip.loop_quarters} q`);
    row("clip.info.transpose", `${pattern?.transpose ?? 0} st`);
    row("clip.info.effects", (track?.effects.length ?? 0) + (pattern?.effects.length ?? 0) + presetEffects);
  } else {
    row("clip.info.parameter", ref.label);
  }
  return { title: ref.label, kind: ref.kind, rows };
}

export function selectedClipKey(): string | null {
  if (inspectedKey() && clipInformation(inspectedKey()!)) return inspectedKey();
  const selected = store.blockSelection()[0];
  if (selected) return timelineInfoKey(selected.kind, selected.trackIndex, selected.index);
  const index = store.selectedTrackIndex();
  const track: Track | undefined = index === null ? undefined : store.project.tracks[index];
  const clip = track?.clips.findIndex(c => c.pattern === store.selectedPatternName()) ?? -1;
  return index !== null && clip >= 0 ? `track:${index}/clip:${clip}` : null;
}
