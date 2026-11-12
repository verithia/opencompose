import { gainEnvelopeWindow } from "../stores/gainEnvelopeWindow";
import { For, Show, createSignal, createMemo, createEffect, on, onMount, onCleanup } from "solid-js";
import { clipInfoUi, timelineInfoKey } from "../stores/clipInfo";
import { RENAME_SELECTION_EVENT } from "../stores/appEvents";
import { store, type TimelineBlockRef } from "../stores/project";
import { recordStore } from "../stores/record";
import { panelStore } from "../stores/panels";
import * as api from "../stores/api";
import { i18n } from "../stores/i18n";
import { commandBindingLabel } from "../stores/keymap";
import { audioClipBounds } from "../utils/audioClipPlacement";
import { projectClock, type ProjectClock } from "../utils/projectClock";
import { overlapsByTrack } from "../utils/timelineLayout";
import { hitTimingOffset, hitWrittenQuarters, patternHitOnsets, swingOffsetQuarters } from "../utils/drumkit";
import { barMarks } from "../utils/tempoLane";
import { TempoLane, TempoLaneHeader } from "./TempoLane";

import { instrumentDisplay, trackSoundDisplay, type AudioClip, type AutomationCurve, type AutomationLane, type BarBeat, type Clip, type HitEvent, type Marker, type NoteEvent, type Pattern, type PatternDef, type Phrase, type TimeRange, type TimeSignature, type Track } from "../types/project";
import { AudioClipBlock } from "./AudioClipBlock";
import { TakeHeaders, TakeLanes, takeGroupFor } from "./TakeLanes";
import { GRID_OPTIONS, blockPositionNote, gridDivision, gridOptionLabel, setGridDivision, snapBlockQuarter } from "../stores/grid";
import { peaksFor, peaksVersion } from "../stores/audioPeaks";
import { CLIP_GAIN_MAX_DB, CLIP_GAIN_MIN_DB, formatClipBeats, formatClipGain, formatClipSeconds } from "../utils/waveform";
import {
  durationToBeats,
  fileName,
  findPattern,
  isRestEvent,
  patternSlipLimit,
} from "../utils/clipContent";
import { sanitizeName, uniqueName } from "../utils/naming";
import { useTooltip, type TooltipContent, type TooltipRow } from "./Tooltip";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import { openTrackContextMenu } from "./trackMenu";
import { TextPromptDialog } from "./TextPromptDialog";
import { PictureTimecodeDialog } from "./PictureTimecodeDialog";
import { BounceDialog } from "./BounceDialog";
import {
  AutomationEmptyHeader,
  AutomationEmptyLane,
  AutomationLaneHeader,
  AutomationLaneRow,
  AutomationOwnerStrip,
  automationAreaHeight,
} from "./TimelineAutomation";
import { ownerAutomationLanes } from "../utils/automationTarget";
import {
  audioClipKey,
  automationClipKey,
  clipMenuItems,
  copyBlockToClipboard,
  copyClip,
  cutClip,
  deleteClip,
  duplicateClip,
  moveClip,
  pasteBlockAtPlayhead,
  resizeClip,
  setClipUiHooks,
  splitClip,
} from "../stores/clipActions";
import { patternKey } from "../stores/clipPool";
import { previewStore } from "../stores/preview";
import { beginBlockGesture, endBlockGesture, isSecondaryClick, withUndoGesture } from "../stores/gesture";
import { bindingFromEvent, registerCommand, runCommand } from "../stores/commands";
import { resolveBinding } from "../stores/keymap";
import { beginPointerDrag } from "../utils/pointerDrag";
import { cursorBeat } from "../utils/cursorPosition";

/** Every lane block that can be dragged: named clips, inline notes and drum patterns. */
type TimelineBlockKind = TimelineBlockRef["kind"];

/** Commands the timeline owns; the registry resolves a key to one of these. */
const TIMELINE_COMMAND_IDS = new Set([
  "edit.copy",
  "edit.cut",
  "edit.paste",
  "edit.delete",
  "edit.deleteBackspace",
  "edit.duplicate",
  "edit.groupClips",
  "edit.ungroupClips",
]);

/** Commands the webview also means by Mod+C / Mod+X / Mod+V on a text selection. */
const CLIPBOARD_COMMAND_IDS = new Set(["edit.copy", "edit.cut", "edit.paste"]);

/** Whether the webview holds a text selection that is not a clip pick. */
const hasTextSelection = () =>
  (document.getSelection()?.toString().trim().length ?? 0) > 0;

/// Gain step for Alt+Up/Down on a selected audio clip.
const GAIN_NUDGE_DB = 0.5;

/// Pointer travel that turns a press into a drag, shared by every ruler and
/// block gesture so a click with a shaky hand stays a click.
const DRAG_THRESHOLD = 4;

function sameBlock(a: TimelineBlockRef, b: TimelineBlockRef): boolean {
  return a.kind === b.kind && a.trackIndex === b.trackIndex && a.index === b.index;
}

/// One block a drag moves: the grabbed block alone, or every member of its group.
interface DragMember {
  track: number;
  index: number;
  startQuarter: number;
  durationQuarters: number;
}

interface ClipDragSession {
  pointerId: number;
  element: HTMLElement;
  kind: TimelineBlockKind;
  originTrack: number;
  originIndex: number;
  name: string;
  durationQuarters: number;
  /// Every block the drag moves, in project order; the anchor is included.
  members: DragMember[];
  /// Whether the grab took a grouped clip, which keeps its members on their tracks.
  grouped: boolean;
  grabOffsetQuarters: number;
  startClientX: number;
  startClientY: number;
  color: string;
  copy: boolean;
  /// Slip session: the drag moves the content under the block, not the block.
  slip: boolean;
  /// Quarter notes of the pattern's head the clip hides right now.
  originOffsetQuarters: number;
  /// Largest offset this clip accepts; zero means nothing is hidden.
  maxSlipQuarters: number;
  offsetQuarters: number;
  active: boolean;
  gesture: number;
}

interface ClipDragPreview {
  kind: TimelineBlockKind;
  originTrack: number;
  originIndex: number;
  targetTrack: number;
  left: number;
  width: number;
  start: BarBeat;
  end: BarBeat;
  name: string;
  color: string;
  copy: boolean;
  /// Quarter-note displacement every member keeps relative to its own start.
  deltaQuarters: number;
  /// Every block the drag moves; a single block is a one-entry list.
  members: DragMember[];
}

interface BlockResizeSession {
  pointerId: number;
  element: HTMLElement;
  kind: TimelineBlockKind;
  track: number;
  index: number;
  startQuarter: number;
  startClientX: number;
  active: boolean;
  gesture: number;
}

interface BlockResizePreview {
  kind: TimelineBlockKind;
  track: number;
  index: number;
  end: BarBeat;
  width: number;
}

interface MarqueeSession {
  pointerId: number;
  element: HTMLElement;
  startClientX: number;
  startClientY: number;
  additive: boolean;
  previous: TimelineBlockRef[];
  active: boolean;
  gesture: number;
}

/** Rubber band in lane-column coordinates; `left` is content x, not client x. */
interface MarqueeRect {
  left: number;
  top: number;
  width: number;
  height: number;
  /** Right-to-left drags only take blocks they fully contain. */
  enclosing: boolean;
}

/** A mouse has one button at a time, so any release ends the gesture even if the
 * browser reports another pointer id after dropping the stream. */
function ownsPointer(session: { pointerId: number }, event: PointerEvent): boolean {
  if (session.pointerId === event.pointerId) return true;
  return (event.pointerType ?? "mouse") !== "touch";
}

/** Shared empty set: a lane outside the last scan has no overlap. */
const EMPTY_OVERLAPS: ReadonlySet<number> = new Set();

function trackColor(index: number): string {
  // A colour chosen in the mixer is the track's colour everywhere: the header
  // bar, the clips it hosts and the routing graph all read it from here.
  const override = store.project.tracks[index]?.mix.overrides?.color;
  return override || `var(--track-color-${index % 8})`;
}

function isTypingTarget(el: EventTarget | null | undefined): boolean {
  const node = el as HTMLElement | null;
  if (!node?.tagName) return false;
  const tag = node.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || node.isContentEditable;
}

function barBeatToPixel(
  bar: number, beat: number, sub: number,
  pxPerBeat: number, _beatsPerBar: number,
): number {
  return projectClock(store.project.properties).quarterAt({bar, beat, subdivision:sub}) * pxPerBeat;
}

function barBeatToBeats(bb: { bar: number; beat: number; subdivision: number }, _bpb: number): number {
  return projectClock(store.project.properties).quarterAt(bb);
}

/** The furthest bar one owner's lanes reach: a span end or a point position. */
function automationExtent(lanes: AutomationLane[] | undefined, bar: number): number {
  let maxBar = bar;
  for (const lane of lanes ?? []) {
    if (lane.time_range) maxBar = Math.max(maxBar, lane.time_range.end.bar);
    for (const point of lane.points) maxBar = Math.max(maxBar, point.position.bar);
  }
  return maxBar;
}

function blockAt(
  kind: TimelineBlockKind, trackIndex: number, blockIndex: number,
): Clip | Phrase | Pattern | null {
  const track = store.project.tracks[trackIndex];
  if (!track) return null;
  if (kind === "clip") return track.clips[blockIndex] ?? null;
  if (kind === "phrase") return track.phrases[blockIndex] ?? null;
  return track.patterns[blockIndex] ?? null;
}

function blockName(kind: TimelineBlockKind, block: Clip | Phrase | Pattern): string {
  return kind === "clip" ? (block as Clip).pattern : (block as Phrase | Pattern).name;
}

/** The block's own span in the arrangement: an inline pattern extends by its repeat count. */
function blockSpan(kind: TimelineBlockKind, block: Clip | Phrase | Pattern): TimeRange | null {
  if (!block.time_range) return null;
  if (kind !== "pattern") return block.time_range;
  const start = block.time_range.start;
  return { start, end: { ...start, bar: start.bar + (block as Pattern).repeat } };
}

/** Audible events only: a rest placeholder and a muted note must not count. */
function audibleNotes(events: NoteEvent[] | undefined): NoteEvent[] {
  return (events ?? []).filter((event) => !isRestEvent(event) && !event.muted);
}

/** Why a block cannot be auditioned in place, or null when its own range sounds. */
function previewReason(kind: TimelineBlockKind, trackIndex: number, blockIndex: number): string | null {
  const track = store.project.tracks[trackIndex];
  if (!track) return i18n.t("menu.clip.previewEmpty");
  if (track.mix.mute) return i18n.t("menu.clip.previewMuted");
  const block = blockAt(kind, trackIndex, blockIndex);
  const span = block ? blockSpan(kind, block) : null;
  if (!block || !span) return i18n.t("menu.clip.previewNoLength");
  const clock = projectClock(store.project.properties);
  if (clock.quarterAt(span.end) <= clock.quarterAt(span.start)) {
    return i18n.t("menu.clip.previewNoLength");
  }
  if (kind === "phrase" && audibleNotes((block as Phrase).events).length === 0) {
    return i18n.t("menu.clip.previewEmpty");
  }
  if (kind === "pattern" && (block as Pattern).hits.length === 0) {
    return i18n.t("menu.clip.previewEmpty");
  }
  return null;
}

function clipColor(clipPattern: string, index: number): string {
  const p = findPattern(clipPattern);
  if (p?.color) return p.color;
  return trackColor(index);
}

function pitchToMidi(note: NoteEvent): number {
  const base = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 }[note.pitch.note];
  const acc = note.pitch.accidental === "Sharp" ? 1
    : note.pitch.accidental === "DoubleSharp" ? 2
    : note.pitch.accidental === "Flat" ? -1
    : note.pitch.accidental === "DoubleFlat" ? -2
    : 0;
  return (note.pitch.octave + 1) * 12 + base + acc;
}

function clipLengthBeats(range: { start: { bar: number; beat: number; subdivision: number }; end: { bar: number; beat: number; subdivision: number } }, bpb: number): number {
  return Math.max(0.25, barBeatToBeats(range.end, bpb) - barBeatToBeats(range.start, bpb));
}

/** Pitched events only: a rest placeholder must never become a block or a pitch. */
function pitchedNotes(events: NoteEvent[]): NoteEvent[] {
  return events.filter((event) => !isRestEvent(event));
}

const CLIP_TIME_EPSILON = 1e-9;

function visibleOnset(beat: number, length: number, offset: number, loop: number): boolean {
  const local = beat - offset;
  const end = loop > 0 ? Math.min(length, loop) : length;
  return local >= -CLIP_TIME_EPSILON && local < end - CLIP_TIME_EPSILON;
}

/** Sample the displayed timeline across every cycle without expanding long repeats. */
function thumbnailOnsets<T>(
  events: T[], beatOf: (event: T) => number, length: number, offset: number, loop: number, limit: number,
): { event: T; start: number }[] {
  if (length <= CLIP_TIME_EPSILON) return [];
  const visible = events
    .filter((event) => visibleOnset(beatOf(event), length, offset, loop))
    .map((event) => ({ event, start: Math.max(0, beatOf(event) - offset) }))
    .sort((a, b) => a.start - b.start);
  if (visible.length === 0) return [];

  const step = loop > 0 ? loop : length;
  const fullCycles = Math.max(0, Math.ceil((length - CLIP_TIME_EPSILON) / step) - 1);
  const finalEnd = length - fullCycles * step - CLIP_TIME_EPSILON;
  const finalCycle = visible.filter(({ start }) => start < finalEnd);
  const count = fullCycles * visible.length + finalCycle.length;
  const samples = Math.min(count, limit);
  return Array.from({ length: samples }, (_, index) => {
    const position = count <= limit ? index : Math.round(index * (count - 1) / (limit - 1));
    const cycle = Math.floor(position / visible.length);
    const { event, start } = visible[position % visible.length];
    return { event, start: start + cycle * step };
  });
}

// Thumbnail x positions are fractions of the full clip width; .clip-thumbnail keeps 0 horizontal inset.
export function noteThumbnails(events: NoteEvent[], lengthBeats: number, offsetBeats = 0, loopBeats = 0) {
  const limit = 512;
  const notes = audibleNotes(events);
  const end = offsetBeats + lengthBeats;
  const visible = loopBeats > 0
    ? thumbnailOnsets(notes, (event) => event.start_beat, lengthBeats, offsetBeats, loopBeats, limit)
    : notes.filter((event) => event.start_beat < end
      && event.start_beat + durationToBeats(event) > offsetBeats)
      .sort((a, b) => a.start_beat - b.start_beat)
      .map((event) => ({ event, start: Math.max(0, event.start_beat - offsetBeats) }));
  if (visible.length === 0 || lengthBeats <= 0) return [];
  const midis = visible.map(({ event }) => pitchToMidi(event));
  const minMidi = midis.reduce((min, midi) => Math.min(min, midi), Infinity);
  const maxMidi = midis.reduce((max, midi) => Math.max(max, midi), -Infinity);
  const span = Math.max(1, maxMidi - minMidi);
  // Keep proportional pitch positions, but fit bars inside the closest pitch gap.
  // A fixed pixel height merges adjacent chord tones in a short track lane.
  const pitches = [...new Set(midis)].sort((a, b) => a - b);
  const smallestGap = pitches.slice(1).reduce((gap, midi, index) =>
    Math.min(gap, midi - pitches[index]), span);
  const height = `${Math.min(8, (smallestGap / span) * 80 * 0.65)}%`;

  // Ordinary phrases show every note. Dense previews retain the whole time
  // span instead of truncating the tail after the first few chords.
  const sampled = visible.length <= limit ? visible
    : Array.from({ length: limit }, (_, index) => visible[Math.round(index * (visible.length - 1) / (limit - 1))]);
  return sampled.map(({ event, start }) => {
    const visibleStart = Math.max(0, offsetBeats - event.start_beat);
    const dur = Math.min(lengthBeats - start, durationToBeats(event) - visibleStart);
    const midi = pitchToMidi(event);
    const left = (start / lengthBeats) * 100;
    return {
      left: `${left}%`,
      width: `${Math.min(100 - left, Math.max(1.2, (dur / lengthBeats) * 100))}%`,
      top: `${8 + ((maxMidi - midi) / span) * 80}%`,
      height,
    };
  });
}

export function hitThumbnails(
  hits: HitEvent[],
  lengthBeats: number,
  offsetBeats = 0,
  loopBeats = 0,
  timing?: { start: number; swing?: number | null },
) {
  const repeatedLength = timing && loopBeats > 0 ? Math.min(lengthBeats, loopBeats * 1024) : lengthBeats;
  let visible = thumbnailOnsets(hits, hitWrittenQuarters, repeatedLength, offsetBeats, loopBeats, 96);
  if (timing) {
    visible = visible.map(({ event, start }) => ({
      event,
      start: Math.max(0, start + swingOffsetQuarters(timing.start + start, timing.swing) + (hitTimingOffset(event) ?? 0)),
    })).filter(({ start }) => start < lengthBeats - CLIP_TIME_EPSILON);
    // For dense clips, the sampled written positions can all fall outside the
    // playable window. Keep at least one audible marker when there is one.
    if (!visible.length) {
      for (const hit of hits) {
        const first = patternHitOnsets(hit, {
          start: timing.start, end: timing.start + lengthBeats,
          offset: offsetBeats, loop: loopBeats, swing: timing.swing,
        }).next();
        if (!first.done) {
          visible.push({ event: hit, start: first.value - timing.start });
          break;
        }
      }
    }
  }
  const drums = [...new Set(visible.map(({ event }) => event.drum))];
  return visible.map(({ event, start }) => ({
    left: `${(start / lengthBeats) * 100}%`,
    top: `${12 + (drums.indexOf(event.drum) / Math.max(1, drums.length - 1)) * 58}%`,
  }));
}

function trackTooltipContent(track: Track): TooltipContent {
  const preset = track.preset
    ? store.project.presets.find((candidate) => candidate.name === track.preset)
    : null;
  const rows: { label: string; value: string }[] = [];
  if (track.preset) {
    rows.push({
      label: i18n.t("tooltip.preset"),
      value: preset ? preset.name : `${track.preset} (${i18n.t("tooltip.notFound")})`,
    });
    if (preset?.source) rows.push({ label: i18n.t("tooltip.source"), value: preset.source });
    if (preset?.plugin) rows.push({ label: i18n.t("tooltip.plugin"), value: preset.plugin });
  } else {
    rows.push({
      label: i18n.t("tooltip.source"),
      value: trackSoundDisplay(track) || i18n.t("tooltip.noSound"),
    });
  }
  rows.push({
    label: i18n.t("tooltip.level"),
    value: `${track.mix.volume_db > 0 ? "+" : ""}${track.mix.volume_db.toFixed(1)} dB · ${Math.round(track.mix.pan * 100)}`,
  });
  rows.push({
    label: i18n.t("tooltip.phrases"),
    value: `${track.phrases.length} · ${track.phrases.reduce((sum, phrase) => sum + pitchedNotes(phrase.events).length, 0)} ${i18n.t("tooltip.notes")}`,
  });
  return { title: track.name, rows };
}

function formatBarBeat(position: BarBeat): string {
  const sub = position.subdivision ? `.${position.subdivision}` : "";
  return `${position.bar}.${position.beat}${sub}`;
}

function formatSeconds(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return `${minutes}:${rest.toFixed(3).padStart(6, "0")}`;
}

function formatCount(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(2).replace(/\.?0+$/, "");
}

function formatSampleRate(rate: number): string {
  const khz = rate / 1000;
  return `${Number.isInteger(khz) ? khz : khz.toFixed(1)} kHz`;
}

/** Meter governing the bar a block starts in; a later meter change is not split. */
function meterAtBar(bar: number): TimeSignature {
  let meter = store.project.properties.time_signature;
  for (const point of store.project.properties.time_sig_map ?? []) {
    if (point.position.bar <= bar) meter = { numerator: point.numerator, denominator: point.denominator };
  }
  return meter;
}

/** Bar range plus length in the block's meter; seconds keep the absolute position. */
function positionRows(clock: ProjectClock, range: TimeRange): TooltipRow[] {
  const startQuarter = clock.quarterAt(range.start);
  const endQuarter = clock.quarterAt(range.end);
  const meter = meterAtBar(range.start.bar);
  const quartersPerBeat = 4 / meter.denominator;
  const quartersPerBar = meter.numerator * quartersPerBeat;
  const beats = (endQuarter - startQuarter) / quartersPerBeat;
  const bars = (endQuarter - startQuarter) / quartersPerBar;
  const barUnit = bars === 1 ? i18n.t("tooltip.barUnit") : i18n.t("tooltip.barsUnit");
  const beatUnit = beats === 1 ? i18n.t("tooltip.beatUnit") : i18n.t("tooltip.beatsUnit");
  return [
    {
      label: i18n.t("tooltip.bars"),
      value: `${formatBarBeat(range.start)} → ${formatBarBeat(range.end)}`,
      mono: true,
    },
    {
      label: i18n.t("tooltip.length"),
      value: `${formatCount(bars)} ${barUnit} · ${formatCount(beats)} ${beatUnit} · ${formatSeconds(clock.secondsAt(endQuarter) - clock.secondsAt(startQuarter))}`,
      mono: true,
    },
  ];
}

function trackMixRows(track: Track): TooltipRow[] {
  const rows: TooltipRow[] = [];
  if (track.mix.volume_db !== 0) {
    rows.push({
      label: i18n.t("tooltip.gain"),
      value: `${track.mix.volume_db > 0 ? "+" : ""}${track.mix.volume_db.toFixed(1)} dB`,
      mono: true,
    });
  }
  if (track.mix.pan !== 0) {
    rows.push({
      label: i18n.t("tooltip.pan"),
      value: `${track.mix.pan < 0 ? "L" : "R"}${Math.round(Math.abs(track.mix.pan) * 100)}`,
      mono: true,
    });
  }
  return rows;
}

/** Clips play through the track channel, so the track owns the sound. */
function soundValue(track: Track): string {
  if (track.preset) {
    const preset = store.project.presets.find((candidate) => candidate.name === track.preset);
    return preset ? preset.name : `${track.preset} (${i18n.t("tooltip.notFound")})`;
  }
  return instrumentDisplay(track.instrument) || i18n.t("tooltip.noSound");
}

function blockTitle(name: string): string {
  return name || i18n.t("tooltip.unnamed");
}

function blockKind(kind: "clip" | "phrase" | "pattern" | "audio"): string {
  if (kind === "clip") return i18n.t("tooltip.kindClip");
  if (kind === "phrase") return i18n.t("tooltip.kindPhrase");
  if (kind === "pattern") return i18n.t("tooltip.kindPattern");
  return i18n.t("tooltip.kindAudio");
}

function blockContent(
  kind: "clip" | "phrase" | "pattern" | "audio",
  name: string,
  track: Track,
  rows: TooltipRow[],
): TooltipContent {
  return { title: blockTitle(name), subtitle: `${blockKind(kind)} · ${track.name}`, rows };
}

function contentRows(
  track: Track,
  counts: { notes?: number; hits?: number },
): TooltipRow[] {
  const rows: TooltipRow[] = [{ label: i18n.t("tooltip.sound"), value: soundValue(track) }];
  if (counts.notes !== undefined) {
    rows.push({ label: i18n.t("tooltip.notes"), value: String(counts.notes), mono: true });
  }
  if (counts.hits !== undefined) {
    rows.push({ label: i18n.t("tooltip.hits"), value: String(counts.hits), mono: true });
  }
  return rows;
}

/** One state row, present only when a value differs from the defaults. */
function stateRow(parts: string[]): TooltipRow[] {
  return parts.length > 0 ? [{ label: i18n.t("tooltip.state"), value: parts.join(" · ") }] : [];
}

function patternState(pattern: PatternDef | null): string[] {
  const parts: string[] = [];
  if (pattern?.transpose != null && pattern.transpose !== 0) {
    parts.push(`${i18n.t("tooltip.transpose")} ${pattern.transpose > 0 ? "+" : ""}${pattern.transpose}`);
  }
  if (pattern?.swing != null) {
    parts.push(`${i18n.t("tooltip.swing")} ${formatCount(pattern.swing)}%`);
  }
  if (pattern?.color) {
    parts.push(`${i18n.t("tooltip.colour")} ${pattern.color}`);
  }
  return parts;
}

function mixState(track: Track, selected: boolean): string[] {
  const parts: string[] = [];
  if (track.mix.mute) parts.push(i18n.t("tooltip.muted"));
  if (track.mix.solo) parts.push(i18n.t("tooltip.soloed"));
  for (const row of trackMixRows(track)) parts.push(`${row.label} ${row.value}`);
  if (selected) parts.push(i18n.t("tooltip.selected"));
  return parts;
}

/** Inline pattern blocks repeat by the bar, so the played range follows `repeat`. */
function patternPlayedRange(pattern: Pattern): TimeRange | null {
  if (!pattern.time_range) return null;
  const start = pattern.time_range.start;
  return {
    start,
    end: { bar: start.bar + Math.max(1, pattern.repeat), beat: start.beat, subdivision: start.subdivision },
  };
}

function isBlockSelected(kind: TimelineBlockKind, trackIndex: number, index: number): boolean {
  return store.blockSelection().some(
    (ref) => ref.kind === kind && ref.trackIndex === trackIndex && ref.index === index,
  );
}

function clipTooltipContent(
  track: Track,
  clip: Clip,
  clock: ProjectClock,
  selected: boolean,
  groupSize: number | null,
): TooltipContent {
  const pattern = findPattern(clip.pattern);
  const noteCount = pitchedNotes(pattern?.events ?? []).length;
  const hitCount = pattern?.hits.length ?? 0;
  const rows: TooltipRow[] = [
    // The clip menu carries actions only; what the clip is and where it sits
    // lives here.
    { label: i18n.t("clip.info.track"), value: track.name },
    ...positionRows(clock, clip.time_range),
    ...contentRows(track, {
      notes: noteCount > 0 || hitCount === 0 ? noteCount : undefined,
      hits: hitCount > 0 ? hitCount : undefined,
    }),
  ];
  if ((clip.offset_quarters ?? 0) > 0) {
    rows.push({
      label: i18n.t("tooltip.slip"),
      value: formatClipBeats(clip.offset_quarters ?? 0),
      mono: true,
    });
  }
  if (groupSize !== null) {
    rows.push({
      label: i18n.t("tooltip.group"),
      value: i18n.t("tooltip.groupMembers", { count: groupSize }),
    });
  }
  rows.push(...stateRow([...patternState(pattern), ...mixState(track, selected)]));
  rows.push({ value: i18n.t("tooltip.doubleClickEdit") });
  return blockContent("clip", clip.pattern, track, rows);
}

function phraseTooltipContent(
  track: Track,
  phrase: Phrase,
  clock: ProjectClock,
  selected: boolean,
): TooltipContent {
  const rows: TooltipRow[] = [];
  if (phrase.time_range) rows.push(...positionRows(clock, phrase.time_range));
  rows.push(...contentRows(track, { notes: pitchedNotes(phrase.events).length }));
  rows.push(...stateRow(mixState(track, selected)));
  rows.push({ value: i18n.t("tooltip.doubleClickEdit") });
  return blockContent("phrase", phrase.name, track, rows);
}

function inlinePatternTooltipContent(
  track: Track,
  pattern: Pattern,
  clock: ProjectClock,
  selected: boolean,
): TooltipContent {
  const range = patternPlayedRange(pattern);
  const rows: TooltipRow[] = range ? positionRows(clock, range) : [];
  if (pattern.repeat > 1) {
    rows.push({ label: i18n.t("tooltip.loop"), value: `${pattern.repeat}×`, mono: true });
  }
  rows.push(...contentRows(track, { hits: pattern.hits.length }));
  rows.push(...stateRow(mixState(track, selected)));
  rows.push({ value: i18n.t("tooltip.doubleClickEdit") });
  return blockContent("pattern", pattern.name, track, rows);
}

function audioClipTooltipContent(
  track: Track,
  clip: AudioClip,
  clock: ProjectClock,
  selected: boolean,
): TooltipContent {
  peaksVersion();
  const pyramid = peaksFor(clip.file);
  const stretch = clip.stretch ?? 1;
  const rows: TooltipRow[] = positionRows(clock, clip.time_range);
  const bounds = audioClipBounds(clock, clip);
  rows[1] = { label: i18n.t("tooltip.length"), value: formatSeconds(bounds.durationSeconds), mono: true };
  rows.push({ label: i18n.t("clipPlacement.startSeconds"), value: `${bounds.startSeconds.toFixed(6)} s`, mono: true });
  if (clip.offset_seconds > 0) {
    rows.push({ label: i18n.t("tooltip.offset"), value: `${clip.offset_seconds.toFixed(3)} s`, mono: true });
  }
  if (clip.bypass_processing) {
    rows.push({ label: i18n.t("clipPlayback.mode"), value: i18n.t("clipPlayback.off") });
  } else if (stretch !== 1) {
    rows.push({
      label: i18n.t("tooltip.stretch"),
      value: `${stretch.toFixed(3)} × ${clip.stretch_mode ?? "musical"}`,
      mono: true,
    });
  }
  rows.push({ label: i18n.t("tooltip.source"), value: fileName(clip.file), mono: true });
  rows.push({ label: i18n.t("tooltip.clipGain"), value: formatClipGain(clip.gain_db), mono: true });
  if (clip.fade_in_seconds > 0) {
    rows.push({ label: i18n.t("tooltip.fadeIn"), value: formatClipSeconds(clip.fade_in_seconds), mono: true });
  }
  if (clip.fade_out_seconds > 0) {
    rows.push({ label: i18n.t("tooltip.fadeOut"), value: formatClipSeconds(clip.fade_out_seconds), mono: true });
  }
  if (pyramid) {
    rows.push({ label: i18n.t("tooltip.sampleRate"), value: formatSampleRate(pyramid.sample_rate), mono: true });
  }
  rows.push({ label: i18n.t("tooltip.channels"), value: "2 ch", mono: true });
  rows.push(...stateRow(mixState(track, selected)));
  rows.push({ value: i18n.t("tooltip.doubleClickEdit") });
  return blockContent("audio", clip.name || fileName(clip.file), track, rows);
}

export function Timeline() {
  const [zoom, setZoom] = createSignal(24);
  const [trackHeight] = createSignal(64);
  const [editingTrackIndex, setEditingTrackIndex] = createSignal<number | null>(null);
  const [renameValue, setRenameValue] = createSignal("");
  /** The in-app rename prompt; the webview has no `window.prompt`. */
  const [renamePrompt, setRenamePrompt] = createSignal<{
    initial: string;
    confirm: (name: string) => void;
  } | null>(null);
  const [selectedAudioClip, setSelectedAudioClip] = createSignal<number | null>(null);
  // The tempo/meter lane starts open when a project already carries maps; it
  // opens on the first added point and folds away again when both maps clear.
  const lanePointCount = () =>
    (store.project.properties.tempo_map?.length ?? 0)
    + (store.project.properties.time_sig_map?.length ?? 0);
  const [tempoLaneOpen, setTempoLaneOpen] = createSignal(lanePointCount() > 0);
  let lastLanePointCount = lanePointCount();
  createEffect(() => {
    const count = lanePointCount();
    if (count === lastLanePointCount) return;
    if (lastLanePointCount === 0 && count > 0) setTempoLaneOpen(true);
    if (lastLanePointCount > 0 && count === 0) setTempoLaneOpen(false);
    lastLanePointCount = count;
  });
  // The store owns the selection so it survives a re-render and a project switch
  // can clear it; a single picked block also drives the clip commands.
  const selectedBlock = createMemo<TimelineBlockRef | null>(() => {
    const selection = store.blockSelection();
    return selection.length === 1 ? selection[0] : null;
  });
  const setSelectedBlock = (block: TimelineBlockRef | null) => {
    store.setBlockSelection(block ? [block] : []);
  };
  /** The selected clips, in the shape the group commands take. */
  const selectedClipRefs = () => store.blockSelection()
    .filter((ref) => ref.kind === "clip")
    .map((ref) => ({ trackIndex: ref.trackIndex, clipIndex: ref.index }));
  let timelineRef: HTMLDivElement | undefined;
  let renameInputRef: HTMLInputElement | undefined;
  let lanesRef: HTMLDivElement | undefined;
  let headersRef: HTMLDivElement | undefined;
  let rulerRef: HTMLDivElement | undefined;
  let laneColumnRef: HTMLDivElement | undefined;
  let dragSession: ClipDragSession | null = null;
  let resizeSession: BlockResizeSession | null = null;
  let marqueeSession: MarqueeSession | null = null;
  const [dragPreview, setDragPreview] = createSignal<ClipDragPreview | null>(null);
  const [resizePreview, setResizePreview] = createSignal<BlockResizePreview | null>(null);
  const [marqueeRect, setMarqueeRect] = createSignal<MarqueeRect | null>(null);
  const [draggingClip, setDraggingClip] = createSignal<{ kind: TimelineBlockKind; track: number; index: number } | null>(null);
  /// Live slip readout: the block that is slipping and the offset under it.
  const [slipPreview, setSlipPreview] = createSignal<
    { track: number; index: number; offsetQuarters: number; x: number } | null
  >(null);

  // Drop stale entries when the project changes underneath the selection (undo,
  // block removal elsewhere, project load).
  createEffect(() => {
    const selection = store.blockSelection();
    if (selection.length === 0) return;
    const valid = selection.filter((ref) => {
      const track = store.project.tracks[ref.trackIndex];
      if (!track) return false;
      const blocks = ref.kind === "clip" ? track.clips
        : ref.kind === "phrase" ? track.phrases
        : track.patterns;
      return ref.index < blocks.length;
    });
    if (valid.length === selection.length) return;
    store.setBlockSelection(valid);
    if (valid.length === 0) store.setEditorSelection(null);
  });

  const audioClips = () => store.project.properties.audio_clips ?? [];

  /** Tracks whose take rows are open; keyed by name so a track edit cannot
      move the disclosure to another track. */
  const [openTakeTracks, setOpenTakeTracks] = createSignal<ReadonlySet<string>>(new Set());
  const takeGroupAt = (trackName: string) => takeGroupFor(audioClips(), trackName);
  const takeLanesOpen = (trackName: string) => openTakeTracks().has(trackName);
  const toggleTakeLanes = (trackName: string) => {
    setOpenTakeTracks((open) => {
      const next = new Set(open);
      if (next.has(trackName)) next.delete(trackName);
      else next.add(trackName);
      return next;
    });
  };

  // Drop a stale audio selection when the clip list changes underneath it.
  createEffect(() => {
    const index = selectedAudioClip();
    if (index === null) return;
    if (index >= audioClips().length) setSelectedAudioClip(null);
  });

  /** Every clip of the group a clip belongs to; a lone clip is its own set. */
  const groupPicks = (trackIndex: number, clipIndex: number): TimelineBlockRef[] => {
    const group = store.groupOfClip(trackIndex, clipIndex);
    if (!group) return [{ kind: "clip", trackIndex, index: clipIndex }];
    return store.groupMembers(group).map((member) => ({
      kind: "clip" as const,
      trackIndex: member.trackIndex,
      index: member.clipIndex,
    }));
  };

  /**
   * Clips a clip-menu command acts on: the timeline selection when the clicked
   * clip is part of it, otherwise the clicked clip alone. Never a phrase or an
   * inline pattern, which carry no clip identifier to group by.
   */
  /** Whether the whole selection already sits in one group, so Group is a no-op. */
  const groupedTogether = (clips: api.ClipRefArg[]): boolean => {
    if (clips.length < 2) return false;
    const first = store.groupOfClip(clips[0].trackIndex, clips[0].clipIndex);
    if (!first) return false;
    return clips.every(
      (clip) => store.groupOfClip(clip.trackIndex, clip.clipIndex) === first,
    );
  };

  /** Pick one block; a modifier keeps the previous picks (shift/cmd extend). */
  const selectBlock = (
    kind: TimelineBlockKind,
    trackIndex: number,
    blockIndex: number,
    additive = false,
  ) => {
    const block = blockAt(kind, trackIndex, blockIndex);
    if (!block?.time_range) return;
    const clipPattern = kind === "clip" ? (block as Clip).pattern : null;
    clipInfoUi.inspect(timelineInfoKey(kind, trackIndex, blockIndex));
    // Clicking a grouped member selects the whole group.
    const picked: TimelineBlockRef[] = kind === "clip"
      ? groupPicks(trackIndex, blockIndex)
      : [{ kind, trackIndex, index: blockIndex }];
    const previous = additive
      ? store.blockSelection().filter((ref) => !picked.some((pick) => sameBlock(ref, pick)))
      : [];
    store.setBlockSelection([...previous, ...picked]);
    clearPicks(null);
    store.setSelectedTrackIndex(trackIndex);
    store.setSelectedPatternName(kind === "pattern" ? blockName(kind, block) : clipPattern);
    store.setEditorSelection({
      trackIndex,
      startBeat: projectClock(store.project.properties).quarterAt(block.time_range.start),
      patternName: clipPattern,
    });
    timelineRef?.focus({ preventScroll: true });
  };

  const selectClip = (trackIndex: number, clipIndex: number, additive = false) =>
    selectBlock("clip", trackIndex, clipIndex, additive);

  const selectAudioClip = (index: number) => {
    const clip = audioClips()[index];
    if (!clip) return;
    setSelectedAudioClip(index);
    clipInfoUi.inspect(audioClipKey(index));
    setSelectedBlock(null);
    clearPicks("audio");
    const trackIndex = store.project.tracks.findIndex((track) => track.name === clip.track);
    if (trackIndex >= 0) store.setSelectedTrackIndex(trackIndex);
    store.setSelectedPatternName(null);
    store.setEditorSelection(null);
    timelineRef?.focus({ preventScroll: true });
  };

  /** Choose one take over a snapped quarter range; the backend splits it. */
  const compTakeRange = (
    track: string, take: number, startQuarter: number, endQuarter: number,
  ) => {
    const clock = projectClock(store.project.properties);
    const start = clock.barBeatAt(startQuarter);
    const end = clock.barBeatAt(endQuarter);
    if (endQuarter <= startQuarter) return;
    void withUndoGesture("project", () => store.compAudioTakes(
      track, take,
      start.bar, start.beat, start.subdivision,
      end.bar, end.beat, end.subdivision,
    ));
  };

  const setTakeMute = (track: string, take: number, muted: boolean) => {
    void withUndoGesture("project", () => store.setAudioTakeMute(track, take, muted));
  };

  /** Solo is an audition: it changes the live mix, never the document. */
  const toggleTakeSolo = (track: string, take: number) => {
    const current = store.takeSolo();
    const same = current?.track === track && current.take === take;
    void store.auditionTake(same ? null : track, same ? null : take);
  };

  /** Whether any of the four picks — block, audio clip, automation point,
   *  automation block — is set. At most one of them ever is. */
  const hasPick = (): boolean =>
    store.blockSelection().length > 0
    || selectedAudioClip() !== null
    || selectedPoint() !== null
    || selectedAutomationBlock() !== null;

  /** Whether a clip command has something to act on right now: a pick, or a
   *  clipboard for Paste. */
  const timelineActsOn = (id: string): boolean =>
    id === "edit.paste" ? store.clipClipboard() !== null : hasPick();

  const handleTimelineKeyDown = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement | null;
    const active = document.activeElement as HTMLElement | null;
    if (isTypingTarget(target) || isTypingTarget(active)) return;
    // A dialog owns every shortcut while it is open.
    if (target?.closest?.('[role="dialog"]') || active?.closest?.('[role="dialog"]')) return;
    // The Piano Roll owns grid shortcuts while its grid has focus.
    if (target?.closest?.("[data-pianoroll-grid]") || active?.closest?.("[data-pianoroll-grid]")) return;
    // A child surface that declares its own keys wins before the clip commands:
    // the tempo/meter lane deletes its focused point with the same Delete.
    if (target?.closest?.("[data-own-keys]") || active?.closest?.("[data-own-keys]")) return;
    if (e.key === "Escape") {
      // A running gesture owns Escape; otherwise it drops every pick.
      if (dragSession || resizeSession || marqueeSession) return;
      if (!hasPick()) return;
      e.preventDefault();
      e.stopPropagation();
      store.setBlockSelection([]);
      clearPicks(null);
      store.setEditorSelection(null);
      return;
    }
    if (e.altKey) {
      // Alt+Up/Down nudges the selected clip's gain; Alt+0 resets it.
      const audio = selectedAudioClip();
      if (audio === null) return;
      if (e.key !== "ArrowUp" && e.key !== "ArrowDown" && e.key !== "0") return;
      const clip = audioClips()[audio];
      if (!clip) return;
      e.preventDefault();
      e.stopPropagation();
      const gain =
        e.key === "0"
          ? 0
          : Math.max(
              CLIP_GAIN_MIN_DB,
              Math.min(
                CLIP_GAIN_MAX_DB,
                clip.gain_db + (e.key === "ArrowUp" ? GAIN_NUDGE_DB : -GAIN_NUDGE_DB),
              ),
            );
      if (Math.abs(gain - clip.gain_db) < 1e-9) return;
      void withUndoGesture("project", () =>
        store.setAudioClipEnvelope(audio, { gain_db: gain }),
      );
      return;
    }
    // Clip keys go through the registry, so a rebinding reaches this surface
    // the same way it reaches the palette.
    const binding = bindingFromEvent(e);
    if (!binding) return;
    const command = resolveBinding(binding);
    if (!command || !TIMELINE_COMMAND_IDS.has(command.id)) return;
    // A press that lands outside the timeline is only the timeline's when one
    // of its clip commands has something to act on. Otherwise the focused
    // surface keeps the key: a text selection in the AI transcript copies and
    // cuts itself instead of the clip the timeline still remembers.
    const outside = !timelineRef?.contains(e.target as Node);
    if (
      outside
      && (
        !timelineActsOn(command.id)
        || (CLIPBOARD_COMMAND_IDS.has(command.id) && hasTextSelection())
      )
    ) return;
    e.preventDefault();
    e.stopPropagation();
    void runCommand(command.id);
  };

  onMount(() => {
    const el = timelineRef;
    if (!el) return;
    el.addEventListener("keydown", handleTimelineKeyDown);
    // Fallback for when focus is outside the timeline (toolbar, context menu,
    // document body). Capture phase wins over App-level shortcuts.
    const fallback = (e: KeyboardEvent) => {
      if (timelineRef?.contains(e.target as Node)) return;
      handleTimelineKeyDown(e);
    };
    document.addEventListener("keydown", fallback, true);
    // F2 renames the selected block's pattern, else the selected track.
    const renameListener = () => renameSelection();
    window.addEventListener(RENAME_SELECTION_EVENT, renameListener);
    onCleanup(() => window.removeEventListener(RENAME_SELECTION_EVENT, renameListener));
    // The lanes move under a cached measurement when the window resizes.
    window.addEventListener("resize", invalidateLaneBounds);
    setClipUiHooks({
      renamePattern: renamePatternBlock,
      forget: (clip) => {
        if (clip.kind === "audio") setSelectedAudioClip(null);
        if (clip.kind === "automation") setSelectedAutomationBlock(null);
      },
    });
    onCleanup(() => {
      el.removeEventListener("keydown", handleTimelineKeyDown);
      document.removeEventListener("keydown", fallback, true);
      window.removeEventListener("resize", invalidateLaneBounds);
      setClipUiHooks(null);
    });
  });

  const createPatternHere = async (trackIndex: number, beat: number) => {
    const clock = projectClock(store.project.properties);
    const track = store.project.tracks[trackIndex];
    if (!track) return;
    // Name the pattern after its track; never a bare "Pattern N" label and never a duplicate.
    const base = sanitizeName(track.name) || sanitizeName(track.instrument?.name ?? "");
    const patternName = uniqueName(base, store.project.patterns.map((p) => p.name));
    await store.addPattern(
      patternName,
      track.instrument?.category ?? "keys",
      track.instrument?.name ?? "piano",
      track.preset ?? undefined,
    );
    const start = clock.barBeatAt(beat);
    await store.addClip(
      trackIndex, patternName,
      start.bar, start.beat, start.subdivision,
      start.bar + 2, 1, 0,
    );
    const clipIndex = store.project.tracks[trackIndex]?.clips.length - 1;
    if (clipIndex >= 0) selectClip(trackIndex, clipIndex);
    panelStore.openDetail();
  };

  const renamePatternBlock = (oldName: string) => {
    setRenamePrompt({
      initial: oldName,
      confirm: (name) => void store.renamePattern(oldName, name),
    });
  };

  // Opening a menu never selects, seeks or drags; the heading names its target,
  // and the menu's actions address that target directly.
  const openBlockContextMenu = (
    event: MouseEvent,
    kind: TimelineBlockKind,
    trackIndex: number,
    blockIndex: number,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    const block = blockAt(kind, trackIndex, blockIndex);
    if (!block?.time_range) return;
    const span = blockSpan(kind, block);
    if (!span) return;
    let items: ContextMenuItem[];
    if (kind === "clip") {
      items = clipMenuItems(patternKey(trackIndex, blockIndex));
    } else {
      const reason = previewReason(kind, trackIndex, blockIndex);
      items = [
        { type: "heading", label: blockName(kind, block) },
        // Actions only: the read-only rows belong to the block's hover tooltip.
        {
          label: i18n.t("clip.info.inspect"),
          onSelect: () => { selectBlock(kind, trackIndex, blockIndex); panelStore.openDetail(); },
        },
        {
          label: i18n.t("menu.clip.preview"),
          disabled: reason !== null,
          hint: reason ?? undefined,
          onSelect: () => void store.previewBlock(trackIndex, span.start, span.end),
        },
        {
          label: i18n.t("menu.clip.openEditor"),
          onSelect: () => {
            selectBlock(kind, trackIndex, blockIndex);
            if (kind === "phrase") panelStore.openBottom("pianoroll");
            else panelStore.openDetail();
          },
        },
        { type: "separator" },
        {
          label: i18n.t("menu.clip.copy"),
          shortcut: "Cmd/Ctrl+C",
          onSelect: () => copyBlockToClipboard({ kind, trackIndex, index: blockIndex }),
        },
        {
          label: i18n.t("menu.clip.paste"),
          shortcut: "Cmd/Ctrl+V",
          disabled: !store.clipClipboard(),
          hint: store.clipClipboard() ? undefined : i18n.t("menu.clip.pasteHint"),
          onSelect: () => void pasteBlockAtPlayhead(trackIndex),
        },
      ];
    }
    if (items.length === 0) return;
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items,
    });
  };

  /** Open the pool menu for one key; an empty list opens nothing. */
  const openPoolClipMenu = (event: MouseEvent, key: string) => {
    event.preventDefault();
    event.stopPropagation();
    const items = clipMenuItems(key);
    if (items.length === 0) return;
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items,
    });
  };

  /** F2: rename the selected block's pattern, else the selected track. */
  const renameSelection = () => {
    const selected = store.blockSelection();
    if (selected.length === 1 && selected[0].kind === "clip") {
      const ref = selected[0];
      const clip = store.project.tracks[ref.trackIndex]?.clips[ref.index];
      if (clip) {
        renamePatternBlock(clip.pattern);
        return;
      }
    }
    const index = store.selectedTrackIndex();
    const track = index === null ? undefined : store.project.tracks[index];
    if (index !== null && track) startRename(index, track.name);
  };

  const startRename = (idx: number, name: string) => {
    setEditingTrackIndex(idx);
    setRenameValue(name);
    setTimeout(() => renameInputRef?.focus(), 0);
  };
  const finishRename = (idx: number) => {
    if (editingTrackIndex() === null) return;
    const newName = renameValue().trim();
    if (newName && newName !== store.project.tracks[idx]?.name) {
      store.renameTrack(idx, newName);
    }
    setEditingTrackIndex(null);
  };
  const cancelRename = () => {
    setEditingTrackIndex(null);
  };

  const pxPerBeat = () => zoom();
  const beatsPerBar = createMemo(() => store.project.properties.time_signature.numerator);

  const totalBars = createMemo(() => {
    const clock = projectClock(store.project.properties);
    let maxBar = 16;
    for (const section of store.project.sections)
      if (section.time_range) maxBar = Math.max(maxBar, section.time_range.end.bar);
    for (const track of store.project.tracks) {
      const last = track.performance?.events.at(-1);
      if (last) maxBar = Math.max(maxBar, projectClock(store.project.properties).barBeatAt(last.quarter).bar);
      for (const phrase of track.phrases)
        if (phrase.time_range) maxBar = Math.max(maxBar, phrase.time_range.end.bar);
      for (const pattern of track.patterns)
        if (pattern.time_range) maxBar = Math.max(maxBar, pattern.time_range.start.bar + pattern.repeat);
      for (const clip of track.clips)
        if (clip.time_range) maxBar = Math.max(maxBar, clip.time_range.end.bar);
      maxBar = automationExtent(track.automation, maxBar);
    }
    for (const bus of store.project.buses) {
      maxBar = automationExtent(bus.automation, maxBar);
    }
    maxBar = automationExtent(store.project.master?.automation, maxBar);
    for (const clip of store.project.properties.audio_clips ?? []) {
      maxBar = Math.max(maxBar, clock.barBeatAt(audioClipBounds(clock, clip).endQuarter).bar);
    }
    // A marker past the last audio still has to be on screen, so the ruler
    // reaches it; its flag is an annotation, never rendered content.
    for (const marker of store.project.markers ?? []) {
      maxBar = Math.max(maxBar, marker.position.bar);
    }
    return maxBar + 4;
  });

  const totalWidth = createMemo(() => barBeatToPixel(totalBars()+1, 1, 0, pxPerBeat(), beatsPerBar()));

  /** Which clips overlap another clip of their lane, for every track at once.
   * A document change re-scans here once; a paint only reads the sets. */
  const overlapIndex = createMemo(() =>
    overlapsByTrack(store.project.tracks, projectClock(store.project.properties)));

  // Drag feedback wins over the parked preview until the seek commits.
  const previewCursorX = createMemo(() => {
    const beat = store.previewCursorBeats();
    return beat === null ? null : beat * pxPerBeat();
  });
  /** A private clip preview plays on its own stream; its playhead rides the
   * arrangement so the block being auditioned shows a cursor too. */
  const auditionCursorX = createMemo(() => {
    // A pattern preview has no arrangement position of its own until the store
    // resolves where the pattern sits; without one the arrangement shows no
    // preview cursor at all.
    const beats = store.auditionArrangementBeats();
    return beats === null ? null : beats * pxPerBeat();
  });
  const cursorX = createMemo(() => {
    const db = store.scrubPosition();
    if (db !== null) return db * pxPerBeat();
    const preview = previewCursorX();
    if (preview !== null) return preview;
    const audition = auditionCursorX();
    if (audition !== null) return audition;
    const t = store.transport();
    // Hide cursor only if we've never played (initial state)
    if (!store.cursorActive() && !t.playing) return -1;
    // The poll only reports a position 20 times a second; while the transport
    // runs, paint the interpolated one so the head moves with the audio.
    return cursorBeat(t.playing, store.smoothPositionBeats(), t.position_beats) * pxPerBeat();
  });
  const previewing = () => store.previewCursorBeats() !== null
    || store.auditionArrangementBeats() !== null;

  // ── Markers and the session loop region ──
  const [markerDrag, setMarkerDrag] = createSignal<{ name: string; beat: number } | null>(null);
  const [markerRename, setMarkerRename] = createSignal<Marker | null>(null);
  const [markerDelete, setMarkerDelete] = createSignal<Marker | null>(null);
  const [loopDrag, setLoopDrag] = createSignal<{ start: number; end: number } | null>(null);

  const markerQuarter = (position: BarBeat) => barBeatToBeats(position, beatsPerBar());

  /** The band follows the drag while it runs, then the committed region. */
  const loopBand = createMemo(() => loopDrag() ?? store.loopRegion());

  const openMarkerMenu = (event: MouseEvent, marker: Marker) => {
    event.preventDefault();
    event.stopPropagation();
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items: [
        { type: "heading", label: marker.name },
        {
          label: i18n.t("marker.rename"),
          onSelect: () => { setMarkerRename(marker); },
        },
        {
          label: i18n.t("marker.delete"),
          danger: true,
          disabled: store.markerBusy(),
          hint: store.markerBusy() ? i18n.t("marker.saving") : undefined,
          onSelect: () => { setMarkerDelete(marker); },
        },
      ],
    });
  };

  /** Click jumps, drag moves once on release. */
  const startMarkerDrag = (event: PointerEvent, marker: Marker) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const element = event.currentTarget as HTMLElement;
    const pointerId = event.pointerId;
    try { element.setPointerCapture(pointerId); } catch { /* not supported */ }
    const startClientX = event.clientX;
    const startClientY = event.clientY;
    let moved = false;
    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      // A click that shook by a pixel or two still seeks; it must not move the
      // marker out from under the user.
      if (
        !moved &&
        Math.abs(ev.clientX - startClientX) < DRAG_THRESHOLD &&
        Math.abs(ev.clientY - startClientY) < DRAG_THRESHOLD
      ) return;
      moved = true;
      setMarkerDrag({ name: marker.name, beat: Math.max(0, beatFromClientX(ev.clientX)) });
    };
    const cleanup = () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      try { element.releasePointerCapture(pointerId); } catch { /* not supported */ }
    };
    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      cleanup();
      const preview = markerDrag();
      setMarkerDrag(null);
      if (!moved || !preview) { void store.seekToMarker(marker.position); return; }
      const clock = projectClock(store.project.properties);
      void store.moveMarker(marker.name, clock.barBeatAt(preview.beat));
    };
    const onCancel = () => { cleanup(); setMarkerDrag(null); };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
  };

  const openLoopMenu = (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const region = store.loopRegion();
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items: [
        { type: "heading", label: i18n.t("loop.heading") },
        {
          label: i18n.t("loop.toggle"),
          checked: store.loop().enabled,
          checkType: "checkbox",
          disabled: !region || store.loopPending(),
          hint: !region ? i18n.t("loop.needsRegion") : undefined,
          onSelect: () => void store.toggleLoop(),
        },
        {
          label: i18n.t("loop.clear"),
          danger: true,
          disabled: !region || store.loopPending(),
          hint: !region ? i18n.t("loop.needsRegion") : undefined,
          onSelect: () => void store.clearLoopRegion(),
        },
      ],
    });
  };

  /** Drag the strip under the ruler to draw the region; the backend refuses
   *  anything shorter than one beat and the reason is shown as a status. */
  const startLoopDrag = (event: PointerEvent) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const element = event.currentTarget as HTMLElement;
    const pointerId = event.pointerId;
    try { element.setPointerCapture(pointerId); } catch { /* not supported */ }
    const origin = beatFromClientX(event.clientX);
    setLoopDrag({ start: origin, end: origin });
    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      const beat = beatFromClientX(ev.clientX);
      setLoopDrag({ start: Math.min(origin, beat), end: Math.max(origin, beat) });
    };
    const cleanup = () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      try { element.releasePointerCapture(pointerId); } catch { /* not supported */ }
    };
    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      cleanup();
      const region = loopDrag();
      setLoopDrag(null);
      if (!region || region.end - region.start < 1e-6) return;
      void store.setLoopRegion(region.start, region.end);
    };
    const onCancel = () => { cleanup(); setLoopDrag(null); };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
  };

  const [pictureOpen, setPictureOpen] = createSignal(false);
  const [pictureRate, setPictureRate] = createSignal<api.PictureRate>("24");
  const [pictureOrigin, setPictureOrigin] = createSignal("00:00:00:00");
  createEffect(on(() => store.documentIdentity().generation, () => {
    setPictureOpen(false);
    setPictureRate("24");
    setPictureOrigin("00:00:00:00");
  }, { defer: true }));

  /** Ruler context menu: add a marker at the playhead, or manage the region. */
  const openRulerMenu = (event: MouseEvent) => {
    event.preventDefault();
    const region = store.loopRegion();
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "clip-context-menu",
      ariaLabel: i18n.t("menu.actions"),
      items: [
        {
          label: i18n.t("marker.add"),
          shortcut: "Alt+T",
          disabled: store.markerBusy(),
          hint: store.markerBusy() ? i18n.t("marker.saving") : undefined,
          onSelect: () => void store.addMarkerAtPlayhead(),
        },
        {
          label: i18n.t("picture.title"),
          onSelect: () => { setPictureOpen(true); },
        },
        {
          label: i18n.t("loop.toggle"),
          checked: store.loop().enabled,
          checkType: "checkbox",
          disabled: !region || store.loopPending(),
          hint: !region ? i18n.t("loop.needsRegion") : undefined,
          onSelect: () => void store.toggleLoop(),
        },
        {
          label: i18n.t("loop.clear"),
          danger: true,
          disabled: !region || store.loopPending(),
          hint: !region ? i18n.t("loop.needsRegion") : undefined,
          onSelect: () => void store.clearLoopRegion(),
        },
      ],
    });
  };



  const [followPlayback, setFollowPlayback] = createSignal(true);
  let wasPlaying = false;
  let autoScrollLeft: number | null = null;
  createEffect(() => {
    const playing = store.transport().playing;
    if (playing && !wasPlaying) setFollowPlayback(true);
    wasPlaying = playing;
  });
  // A fresh Play attaches; user scrolling detaches until the next Play.
  // Scrubbing always follows so the dragged playhead cannot leave the viewport.
  createEffect(() => {
    const cx = cursorX();
    const t = store.transport();
    const scrubbing = store.scrubPosition() !== null;
    if (!scrubbing && (!t.playing || !followPlayback())) return;
    if (cx < 0 || !lanesRef) return;
    // Beat 0 is offset by the sticky header column in shared content coords.
    const headerWidth = headersRef?.offsetWidth ?? 0;
    const contentX = headerWidth + cx;
    const viewLeft = lanesRef.scrollLeft;
    const viewRight = viewLeft + lanesRef.clientWidth;
    if (contentX > viewRight - 40) {
      lanesRef.scrollLeft = contentX - lanesRef.clientWidth * 0.7;
      autoScrollLeft = lanesRef.scrollLeft;
    } else if (contentX < viewLeft + headerWidth + 20) {
      lanesRef.scrollLeft = Math.max(0, contentX - headerWidth - 40);
      autoScrollLeft = lanesRef.scrollLeft;
    }
  });

  // Timeline-local commands: the zoom and follow state lives here, and the
  // split acts on the block this view has selected.
  const zoomIn = () => {
    setZoom(Math.min(120, zoom() + 4));
  };
  const zoomOut = () => {
    setZoom(Math.max(6, zoom() - 4));
  };
  const zoomToFit = () => {
    const lanes = lanesRef;
    if (!lanes) return;
    const headerWidth = headersRef?.offsetWidth ?? 0;
    const visible = Math.max(1, lanes.clientWidth - headerWidth);
    const beats = Math.max(1, totalBars() * beatsPerBar());
    setZoom(Math.max(6, Math.min(120, visible / beats)));
  };
  const toggleFollowPlayhead = () => {
    setFollowPlayback((value) => !value);
  };
  const splitSelection = () => {
    const audio = selectedAudioClip();
    if (audio !== null) {
      void splitClip(audioClipKey(audio));
      return;
    }
    const selected = selectedBlock();
    if (selected && selected.kind === "clip") {
      void splitClip(patternKey(selected.trackIndex, selected.index));
    }
  };

  const selectedClipCount = () => selectedClipRefs().length;

  // ── Automation rows ──
  // One collapsible area per track, plus synthetic rows at the end of the lane
  // list for bus and master owners, which have no track header of their own.
  const [automationOpen, setAutomationOpen] = createSignal<ReadonlySet<number>>(new Set());
  const [selectedPoint, setSelectedPoint] = createSignal<
    { target: string; laneIndex: number; pointIndex: number } | null
  >(null);
  // A grabbed automation block owns copy, duplicate, split and delete, so the
  // menu can act on it from either the row or the clip menu.
  const [selectedAutomationBlock, setSelectedAutomationBlock] = createSignal<
    { target: string; laneIndex: number } | null
  >(null);
  const [tensionPrompt, setTensionPrompt] = createSignal<
    { target: string; laneIndex: number; pointIndex: number; initial: string } | null
  >(null);

  /**
   * One pick at a time: every gesture that picks something drops the other
   * picks, so Delete can only reach what the user is pointing at. Pass the pick
   * that survives, or null when the gesture picks none of them.
   */
  const clearPicks = (keep: "audio" | "automation-point" | "automation-block" | null) => {
    if (keep !== "audio") setSelectedAudioClip(null);
    if (keep !== "automation-point") setSelectedPoint(null);
    if (keep !== "automation-block") setSelectedAutomationBlock(null);
  };

  // Picks address clips, lanes and points by index, so they belong to the
  // document that was open when they were made. A load replaces that document
  // and drops them, which keeps Delete off content the user never picked.
  createEffect(on(store.documentIdentity, () => {
    setSelectedPoint(null);
    setSelectedAutomationBlock(null);
    setSelectedAudioClip(null);
    setTensionPrompt(null);
  }, { defer: true }));

  const automationShown = (trackIndex: number) => automationOpen().has(trackIndex);

  const toggleAutomation = (trackIndex: number) => {
    setAutomationOpen((open) => {
      const next = new Set(open);
      if (next.has(trackIndex)) next.delete(trackIndex);
      else next.add(trackIndex);
      return next;
    });
  };

  /** The lanes one panel target owns; a preset chain owns none. */
  const lanesOf = (target: string): AutomationLane[] => ownerAutomationLanes(store.project, target);

  /** Bus and master lanes, drawn after the last track lane. */
  const ownerAutomationGroups = createMemo(() => {
    const groups: { target: string; owner: string; lanes: AutomationLane[] }[] = [];
    for (const bus of store.project.buses) {
      const lanes = bus.automation ?? [];
      if (lanes.length > 0) {
        groups.push({ target: `bus:${bus.name}`, owner: bus.name, lanes });
      }
    }
    const masterLanes = store.project.master?.automation ?? [];
    if (masterLanes.length > 0) {
      groups.push({
        target: "master",
        owner: i18n.t("mixer.master"),
        lanes: masterLanes,
      });
    }
    return groups;
  });

  const selectedPointIndex = (target: string, laneIndex: number): number | null => {
    const selected = selectedPoint();
    if (!selected || selected.target !== target || selected.laneIndex !== laneIndex) return null;
    return selected.pointIndex;
  };

  /** Every point edit refuses a muted lane, so nothing is dropped silently. */
  const editableLane = (target: string, laneIndex: number): AutomationLane | null => {
    const lane = lanesOf(target)[laneIndex];
    return lane && !lane.muted ? lane : null;
  };

  const addAutomationPointAt = (
    target: string,
    laneIndex: number,
    positionBeats: number,
    value: number,
  ) => {
    const lane = editableLane(target, laneIndex);
    if (!lane) return;
    setSelectedPoint(null);
    void store.addAutomationPoint(target, lane.param, laneIndex, positionBeats, value);
  };

  const moveAutomationPointAt = (
    target: string,
    laneIndex: number,
    pointIndex: number,
    positionBeats: number,
    value: number,
  ) => {
    const lane = editableLane(target, laneIndex);
    if (!lane) return;
    void store.moveAutomationPoint(target, lane.param, laneIndex, pointIndex, positionBeats, value);
  };

  // ── Automation clip blocks ──
  /** The selected clip block, once its lane still exists with a span. */
  const selectedAutomationClip = createMemo(() => {
    const pick = selectedAutomationBlock();
    if (!pick) return null;
    const lane = lanesOf(pick.target)[pick.laneIndex];
    if (!lane?.time_range) return null;
    return { target: pick.target, laneIndex: pick.laneIndex, lane };
  });

  const moveAutomationBlock = (target: string, laneIndex: number, start: BarBeat) => {
    void moveClip(automationClipKey(target, laneIndex), start);
  };

  const resizeAutomationBlock = (
    target: string,
    laneIndex: number,
    edge: "start" | "end",
    to: BarBeat,
  ) => {
    void resizeClip(automationClipKey(target, laneIndex), edge, to);
  };

  const duplicateAutomationBlock = (target: string, laneIndex: number, at: BarBeat) => {
    // A split or a copy inserts a lane, so the stored index may move.
    setSelectedAutomationBlock(null);
    void copyClip(automationClipKey(target, laneIndex), at);
  };

  const splitAutomationBlock = (target: string, laneIndex: number) => {
    setSelectedAutomationBlock(null);
    void splitClip(automationClipKey(target, laneIndex));
  };

  /** Copy a clip block to the playhead, which is the block counterpart of Copy. */
  const copyAutomationBlockToPlayhead = (target: string, laneIndex: number) => {
    duplicateAutomationBlock(
      target,
      laneIndex,
      projectClock(store.project.properties).barBeatAt(store.transport().position_beats),
    );
  };

  const deleteAutomationBlock = (target: string, laneIndex: number) => {
    setSelectedAutomationBlock(null);
    void deleteClip(automationClipKey(target, laneIndex));
  };

  const deleteAutomationPointAt = (target: string, laneIndex: number, pointIndex: number) => {
    const lane = editableLane(target, laneIndex);
    if (!lane) return;
    setSelectedPoint(null);
    // A stale selection after an undo must not delete a neighbour.
    if (pointIndex >= lane.points.length) return;
    void store.removeAutomationPoint(target, lane.param, laneIndex, pointIndex);
  };

  const setAutomationCurveAt = (
    target: string,
    laneIndex: number,
    pointIndex: number,
    curve: AutomationCurve,
    tension: number | null,
  ) => {
    const lane = editableLane(target, laneIndex);
    if (!lane) return;
    void store.setAutomationPointCurve(target, lane.param, pointIndex, curve, tension, laneIndex);
  };

  const toggleAutomationMute = (target: string, laneIndex: number) => {
    const lane = lanesOf(target)[laneIndex];
    if (!lane) return;
    void store.setAutomationLaneMute(target, lane.param, laneIndex, !lane.muted);
  };

  const openTensionPrompt = (target: string, laneIndex: number, pointIndex: number) => {
    const lane = editableLane(target, laneIndex);
    const point = lane?.points[pointIndex];
    if (!lane || !point) return;
    setTensionPrompt({
      target,
      laneIndex,
      pointIndex,
      initial: String(point.tension ?? 1),
    });
  };

  const deleteTimelineSelection = () => {
    // A selected automation point owns Delete before any clip does.
    const point = selectedPoint();
    if (point) {
      deleteAutomationPointAt(point.target, point.laneIndex, point.pointIndex);
      return true;
    }
    // The memo drops a stale pick (a lane the project no longer has, or one
    // that lost its span), so Delete cannot reach a neighbour by index.
    const clip = selectedAutomationClip();
    if (clip) {
      setSelectedAutomationBlock(null);
      void deleteClip(automationClipKey(clip.target, clip.laneIndex));
      return true;
    }
    const audio = selectedAudioClip();
    if (audio !== null) {
      setSelectedAudioClip(null);
      void deleteClip(audioClipKey(audio));
      return true;
    }
    const selection = store.blockSelection();
    if (selection.length === 0) return false;
    // One named clip keeps its own command; anything else goes out as a batch.
    if (selection.length === 1 && selection[0].kind === "clip") {
      void deleteClip(patternKey(selection[0].trackIndex, selection[0].index));
      return true;
    }
    void store.removeBlocks(selection);
    return true;
  };

  /** The gestures every lane of one owner shares. */
  const automationCallbacks = (target: string, laneIndex: number) => ({
    onSelectPoint: (pointIndex: number | null) => {
      clearPicks(pointIndex === null ? null : "automation-point");
      setSelectedPoint(pointIndex === null ? null : { target, laneIndex, pointIndex });
    },
    owner: target,
    onAddPoint: (positionBeats: number, value: number) =>
      addAutomationPointAt(target, laneIndex, positionBeats, value),
    onMovePoint: (pointIndex: number, positionBeats: number, value: number) =>
      moveAutomationPointAt(target, laneIndex, pointIndex, positionBeats, value),
    onDeletePoint: (pointIndex: number) =>
      deleteAutomationPointAt(target, laneIndex, pointIndex),
    onCurve: (pointIndex: number, curve: AutomationCurve, tension: number | null) =>
      setAutomationCurveAt(target, laneIndex, pointIndex, curve, tension),
    onTension: (pointIndex: number) => openTensionPrompt(target, laneIndex, pointIndex),
    onToggleMute: () => toggleAutomationMute(target, laneIndex),
    blockSelected:
      selectedAutomationClip()?.target === target
      && selectedAutomationClip()?.laneIndex === laneIndex,
    onSelectBlock: () => {
      // A block press takes the selection from any clip or point, so Delete
      // cannot reach one the user is no longer pointing at.
      store.setBlockSelection([]);
      clearPicks("automation-block");
      setSelectedAutomationBlock({ target, laneIndex });
    },
    onMoveBlock: (start: BarBeat) => moveAutomationBlock(target, laneIndex, start),
    onResizeBlock: (edge: "start" | "end", to: BarBeat) =>
      resizeAutomationBlock(target, laneIndex, edge, to),
    onDuplicateBlock: (at: BarBeat) => duplicateAutomationBlock(target, laneIndex, at),
    onCopyBlock: () => copyAutomationBlockToPlayhead(target, laneIndex),
    onSplitBlock: () => splitAutomationBlock(target, laneIndex),
    onDeleteBlock: () => deleteAutomationBlock(target, laneIndex),
    blockMenuItems: () => clipMenuItems(automationClipKey(target, laneIndex)),
  });

  const duplicateTimelineSelection = () => {
    const selected = selectedBlock();
    if (!selected || selected.kind !== "clip") return false;
    void duplicateClip(patternKey(selected.trackIndex, selected.index));
    return true;
  };

  const canGroupSelection = () => {
    const clips = selectedClipRefs();
    return clips.length >= 2 && !groupedTogether(clips);
  };

  onMount(() => {
    const unregister = [
      registerCommand("view.zoomIn", zoomIn),
      registerCommand("view.zoomOut", zoomOut),
      registerCommand("view.zoomToFit", zoomToFit),
      registerCommand("view.followPlayhead", toggleFollowPlayhead),
      registerCommand("edit.split", splitSelection, {
        enabled: () => selectedAudioClip() !== null || selectedBlock()?.kind === "clip",
        disabledReason: () => i18n.t("command.needsSelection"),
      }),
      registerCommand("edit.copy", () => {
        const selected = selectedBlock();
        if (!selected) return false;
        copyBlockToClipboard(selected);
        return true;
      }, {
        enabled: () => selectedBlock() !== null,
        disabledReason: () => i18n.t("command.needsSelection"),
      }),
      registerCommand("edit.cut", () => {
        const selected = selectedBlock();
        if (!selected || selected.kind !== "clip") return false;
        void cutClip(patternKey(selected.trackIndex, selected.index));
        return true;
      }, {
        enabled: () => selectedBlock()?.kind === "clip",
        disabledReason: () => i18n.t("command.needsSelection"),
      }),
      registerCommand("edit.paste", () => {
        if (!store.clipClipboard()) return false;
        void pasteBlockAtPlayhead();
        return true;
      }, {
        enabled: () => store.clipClipboard() !== null,
        disabledReason: () => i18n.t("command.needsClipboard"),
      }),
      registerCommand("edit.delete", deleteTimelineSelection),
      registerCommand("edit.deleteBackspace", deleteTimelineSelection),
      registerCommand("edit.duplicate", duplicateTimelineSelection),
      registerCommand("edit.groupClips", () => {
        void store.groupClips(selectedClipRefs());
        return true;
      }, {
        enabled: canGroupSelection,
        disabledReason: () => i18n.t("command.needsSelection"),
      }),
      registerCommand("edit.ungroupClips", () => {
        if (selectedClipCount() === 0) return false;
        void store.ungroupClips(selectedClipRefs());
        return true;
      }, {
        enabled: () => selectedClipCount() > 0,
        disabledReason: () => i18n.t("command.needsSelection"),
      }),
    ];
    onCleanup(() => {
      for (const off of unregister) off();
    });
  });

  // Convert a client x coordinate to a beat using the ruler's own rect. The
  // ruler shares the lane column's horizontal position, so this already
  // accounts for both the sticky header column and the shared scroll offset.
  const beatFromClientX = (clientX: number): number => {
    const rect = rulerRef?.getBoundingClientRect();
    if (!rect) return 0;
    return Math.max(0, (clientX - rect.left) / pxPerBeat());
  };

  const handleWheel = (e: WheelEvent) => {
    setFollowPlayback(false);
    invalidateLaneBounds();
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      setZoom(Math.round(Math.max(6, Math.min(120, zoom() * delta))));
    }
  };

  // Drag on the ruler previews the playhead, then commits a single seek on release.
  // The gesture runs muted, so it has to end even when no release arrives: the
  // shared drag helper also ends it on a cancelled pointer, a lost capture, a
  // blur, Escape and unmount. A gesture that a newer one replaced must not
  // unmute over it, so only the newest settle touches the mute.
  let stopRulerScrub: (() => void) | null = null;
  let rulerScrubEpoch = 0;
  const handleRulerPointerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    if (recordStore.recording()) {
      store.setError(i18n.t("toolbar.recordingPlaybackLocked"));
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    void api.transportMute().catch((error) => store.setError(String(error)));
    store.setScrubPosition(beatFromClientX(e.clientX));
    const epoch = ++rulerScrubEpoch;
    let lastX = e.clientX;
    stopRulerScrub = beginPointerDrag({
      element: e.currentTarget as HTMLElement,
      pointerId: e.pointerId,
      onMove: (clientX) => {
        lastX = clientX;
        store.setScrubPosition(beatFromClientX(clientX));
      },
      onEnd: (committed) => {
        stopRulerScrub = null;
        const settle = () => {
          if (epoch !== rulerScrubEpoch) return;
          store.setScrubPosition(null);
          api.transportUnmute();
        };
        // The drag position holds until the seek lands, then the transport does.
        if (committed) void store.seek(beatFromClientX(lastX)).then(settle, settle);
        else settle();
      },
    });
  };

  const handlePatternDrop = async (e: DragEvent) => {
    const patternName = e.dataTransfer?.getData("application/x-pattern");
    if (!patternName || !lanesRef) return;
    e.preventDefault();
    const dropBeat = beatFromClientX(e.clientX);
    const clock = projectClock(store.project.properties);
    const start = clock.barBeatAt(dropBeat);
    const end = clock.barBeatAt(clock.quarterAt({bar:start.bar+2,beat:1,subdivision:0}) + clock.quarterAt(start) - clock.quarterAt({bar:start.bar,beat:1,subdivision:0}));
    // Lanes are not a uniform height while automation rows are open, so ask the
    // lane under the pointer instead of dividing by the row height. A drop is
    // outside any drag, so its measurement starts from nothing.
    invalidateLaneBounds();
    const trackIndex = trackAtClientY(e.clientY);
    if (trackIndex !== null && trackIndex < store.project.tracks.length) {
      await store.addClip(trackIndex, patternName, start.bar, start.beat, start.subdivision, end.bar, end.beat, end.subdivision);
    }
  };

  // ── Clip drag ─────────────────────────────────────────────────────────────
  // A drag is a preview only; the project store is updated once on release.
  // Alt/Option copies the clip instead of moving it (same pattern, new clip).
  /** Bottom of the rows a lane owns: its automation and take rows, which are
   *  siblings after it and before the next lane (or the bus rows). */
  const laneBottom = (lane: HTMLElement, rect: DOMRect): number => {
    let bottom = rect.bottom;
    for (let node = lane.nextElementSibling; node; node = node.nextElementSibling) {
      if (node.classList.contains("track-lane")) break;
      if (node.classList.contains("automation-owner-strip")) break;
      bottom = Math.max(bottom, node.getBoundingClientRect().bottom);
    }
    return bottom;
  };

  /**
   * Lane Y ranges, measured once per gesture.
   *
   * A pointermove asks which lane the pointer is over, and reading a rect per
   * lane per move is what made dragging expensive. The ranges are measured on
   * first use and dropped whenever the lanes can move under the pointer: a
   * scroll, a zoom, a window resize, a row that opened or closed, or the start
   * of another drag.
   */
  let laneBounds: { top: number; bottom: number }[] | null = null;
  const invalidateLaneBounds = () => { laneBounds = null; };
  // Automation and take rows are siblings of the lanes, so opening one moves
  // every lane below it. Read the inputs of the layout so any change drops the
  // measured ranges.
  createEffect(() => {
    void store.project.tracks.length;
    void automationOpen();
    void tempoLaneOpen();
    void openTakeTracks();
    invalidateLaneBounds();
  });
  const measureLaneBounds = (): { top: number; bottom: number }[] => {
    const column = laneColumnRef;
    const lanes = column ? Array.from(column.querySelectorAll<HTMLElement>(".track-lane")) : [];
    laneBounds = lanes.map((lane, index) => {
      const rect = lane.getBoundingClientRect();
      const bottom = index + 1 < lanes.length
        ? lanes[index + 1].getBoundingClientRect().top
        : laneBottom(lane, rect);
      return { top: rect.top, bottom };
    });
    return laneBounds;
  };

  /** Track under the pointer, or null when the pointer is outside every lane.
   *  A lane owns the rows between it and the next one, so a drop over a take or
   *  automation row still names that track; nothing below the last track does. */
  const trackAtClientY = (clientY: number): number | null => {
    if (!laneColumnRef || store.project.tracks.length === 0) return null;
    const bounds = laneBounds ?? measureLaneBounds();
    for (let index = 0; index < bounds.length; index++) {
      if (clientY >= bounds[index].top && clientY < bounds[index].bottom) return index;
    }
    return null;
  };

  /** A group refuses a delta that would carry its earliest member before bar 1. */
  const groupFloor = (session: ClipDragSession): number => {
    if (!session.grouped) return 0;
    return Math.min(...session.members.map((member) => member.startQuarter));
  };

  const dragPreviewAt = (clientX: number, clientY: number): ClipDragPreview | null => {
    const session = dragSession;
    if (!session) return null;
    if (session.slip) return null;
    const raw = beatFromClientX(clientX) - session.grabOffsetQuarters;
    const requested = snapBlockQuarter(Math.max(0, raw), gridDivision());
    const floor = groupFloor(session);
    // The preview stops at the wall the release will refuse; nothing is silent.
    const startQuarter = Math.max(requested, floor);
    const clock = projectClock(store.project.properties);
    let start: BarBeat;
    let end: BarBeat;
    try {
      start = clock.barBeatAt(startQuarter);
      end = clock.barBeatAt(startQuarter + session.durationQuarters);
    } catch {
      return null;
    }
    // A group keeps every member on its own track; only a single block changes
    // lanes, and a pointer outside every lane names none.
    const targetTrack = session.grouped ? session.originTrack : trackAtClientY(clientY);
    if (targetTrack === null) return null;
    return {
      kind: session.kind,
      originTrack: session.originTrack,
      originIndex: session.originIndex,
      targetTrack,
      left: startQuarter * pxPerBeat(),
      width: Math.max(20, session.durationQuarters * pxPerBeat()),
      start,
      end,
      name: session.name,
      color: session.color,
      copy: session.copy,
      deltaQuarters: startQuarter - (session.members[0]?.startQuarter ?? 0),
      members: session.members,
    };
  };

  /** Displacement of a block that is following the pointer inside its own lane. */
  const followPreview = (kind: TimelineBlockKind, trackIndex: number, index: number): ClipDragPreview | null => {
    const preview = dragPreview();
    if (!preview || preview.kind !== kind) return null;
    if (preview.targetTrack !== trackIndex) return null;
    if (preview.originTrack === trackIndex && preview.originIndex === index) return preview;
    // A group member follows its own start plus the anchor's delta.
    if (!preview.members.some((member) => member.track === trackIndex && member.index === index)) {
      return null;
    }
    const member = preview.members.find(
      (candidate) => candidate.track === trackIndex && candidate.index === index,
    )!;
    const clock = projectClock(store.project.properties);
    const startQuarter = member.startQuarter + preview.deltaQuarters;
    let start = preview.start;
    let end = preview.end;
    try {
      start = clock.barBeatAt(startQuarter);
      end = clock.barBeatAt(startQuarter + member.durationQuarters);
    } catch { /* keep the anchor's range; the lane already draws the anchor */ }
    return {
      ...preview,
      left: startQuarter * pxPerBeat(),
      width: Math.max(20, member.durationQuarters * pxPerBeat()),
      start,
      end,
    };
  };

  /** Live width of a block whose right edge is being dragged. */
  const resizePreviewFor = (kind: TimelineBlockKind, trackIndex: number, index: number): BlockResizePreview | null => {
    const preview = resizePreview();
    if (!preview || preview.kind !== kind) return null;
    return preview.track === trackIndex && preview.index === index ? preview : null;
  };

  // Swallow the click a drag leaves behind so it cannot re-select or open the clip.
  // It is dispatched in the same task as pointerup, so a 0 ms timeout is enough.
  const swallowTrailingClick = () => {
    const swallow = (event: MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      document.removeEventListener("click", swallow, true);
    };
    document.addEventListener("click", swallow, true);
    setTimeout(() => document.removeEventListener("click", swallow, true), 0);
  };

  const endClipDrag = () => {
    const session = dragSession;
    if (!session) return;
    try { session.element.releasePointerCapture?.(session.pointerId); } catch { /* not supported */ }
    document.removeEventListener("pointermove", handleClipDragMove);
    document.removeEventListener("pointerup", handleClipDragUp);
    document.removeEventListener("pointercancel", handleClipDragCancel);
    document.removeEventListener("keydown", handleClipDragKey, true);
    window.removeEventListener("blur", endClipDrag);
    dragSession = null;
    endBlockGesture(session.gesture);
    setDraggingClip(null);
    setDragPreview(null);
    setSlipPreview(null);
  };

  const handleClipDragMove = (e: PointerEvent) => {
    const session = dragSession;
    if (!session || !ownsPointer(session, e)) return;
    if (!session.active) {
      const dx = Math.abs(e.clientX - session.startClientX);
      const dy = Math.abs(e.clientY - session.startClientY);
      if (dx < DRAG_THRESHOLD && dy < DRAG_THRESHOLD) return;
      session.active = true;
      setDraggingClip({ kind: session.kind, track: session.originTrack, index: session.originIndex });
    }
    e.preventDefault();
    if (session.slip) {
      // Dragging right moves the content right, revealing the hidden head.
      const delta = (e.clientX - session.startClientX) / pxPerBeat();
      const offset = Math.max(
        0,
        Math.min(session.maxSlipQuarters, session.originOffsetQuarters - delta),
      );
      session.offsetQuarters = Math.round(offset * 1000) / 1000;
      const blockLeft = session.element.getBoundingClientRect().left;
      setSlipPreview({
        track: session.originTrack,
        index: session.originIndex,
        offsetQuarters: session.offsetQuarters,
        x: Math.max(0, Math.min(
          session.durationQuarters * pxPerBeat() - 60,
          e.clientX - blockLeft,
        )),
      });
      return;
    }
    // A pointer outside every lane clears the ghost: the drop will be refused.
    setDragPreview(dragPreviewAt(e.clientX, e.clientY));
  };

  const handleClipDragUp = (e: PointerEvent) => {
    const session = dragSession;
    if (!session || !ownsPointer(session, e)) return;
    if (!session.active) {
      endClipDrag();
      return;
    }
    if (session.slip) {
      const offset = session.offsetQuarters;
      const origin = session.originOffsetQuarters;
      endClipDrag();
      if (Math.abs(offset - origin) < 1e-9) return;
      swallowTrailingClick();
      void withUndoGesture("project", () =>
        store.setPatternClipOffset(session.originTrack, session.originIndex, offset),
      );
      return;
    }
    const preview = dragPreviewAt(e.clientX, e.clientY);
    // The release asks for the position the pointer names, not the preview's
    // clamped one: the backend owns the bar-1 refusal and reports the member.
    const raw = beatFromClientX(e.clientX) - session.grabOffsetQuarters;
    const requested = snapBlockQuarter(Math.max(0, raw), gridDivision());
    const requestedStart = projectClock(store.project.properties).barBeatAt(requested);
    endClipDrag();
    if (!preview) return;
    swallowTrailingClick();
    if (session.kind === "clip") {
      const key = patternKey(session.originTrack, session.originIndex);
      if (session.grouped) {
        void withUndoGesture("project", async () => {
          const reason = await moveClip(key, requestedStart);
          if (reason) store.setError(i18n.t("clip.groupMoveRefused", { reason }));
        });
        return;
      }
      if (session.copy) {
        const targetTrack = preview.targetTrack;
        const targetCount = store.project.tracks[targetTrack]?.clips.length ?? 0;
        void withUndoGesture("project", async () => {
          // A copy the backend refuses lands nowhere, so there is nothing to select.
          const reason = await copyClip(key, { ...preview.start, trackIndex: targetTrack });
          if (!reason) {
            setSelectedBlock({ kind: "clip", trackIndex: targetTrack, index: targetCount });
          }
        });
        return;
      }
      if (preview.targetTrack !== session.originTrack) {
        const targetTrack = preview.targetTrack;
        const targetCount = store.project.tracks[targetTrack]?.clips.length ?? 0;
        void withUndoGesture("project", async () => {
          const reason = await moveClip(key, { ...preview.start, trackIndex: targetTrack });
          if (!reason) {
            setSelectedBlock({ kind: "clip", trackIndex: targetTrack, index: targetCount });
          }
        });
        return;
      }
      void withUndoGesture("project", () => moveClip(key, preview.start));
      return;
    }
    void withUndoGesture("project", async () => {
      const { start, targetTrack } = preview;
      if (session.kind === "phrase") {
        if (targetTrack !== session.originTrack) {
          await store.movePhraseToTrack(
            session.originTrack, session.originIndex, targetTrack,
            start.bar, start.beat, start.subdivision,
          );
        } else {
          await store.movePhrase(
            session.originTrack, session.originIndex,
            start.bar, start.beat, start.subdivision,
          );
        }
      } else if (targetTrack !== session.originTrack) {
        await store.moveInlinePatternToTrack(
          session.originTrack, session.originIndex, targetTrack,
          start.bar, start.beat, start.subdivision,
        );
      } else {
        await store.moveInlinePattern(
          session.originTrack, session.originIndex,
          start.bar, start.beat, start.subdivision,
        );
      }
      store.setEditorSelection({
        trackIndex: targetTrack,
        startBeat: projectClock(store.project.properties).quarterAt(start),
        patternName: null,
      });
    });
  };

  const handleClipDragCancel = (e: PointerEvent) => {
    if (dragSession && !ownsPointer(dragSession, e)) return;
    endClipDrag();
  };

  const handleClipDragKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    endClipDrag();
  };

  const beginClipDrag = (e: PointerEvent, trackIndex: number, kind: TimelineBlockKind, blockIndex: number) => {
    // Secondary presses (including macOS ctrl+click) open the menu; they never drag.
    if (e.button !== 0 || isSecondaryClick(e)) return;
    // Resize handles own their gestures; never start a move from one.
    if ((e.target as HTMLElement | null)?.closest(".phrase-resize-handle")) return;
    // A new press always owns the gesture: drop any session whose release the
    // webview never delivered, or it would silently mute every later drag.
    endClipDrag();
    endMarquee();
    // The lanes are measured once, on the first move of this drag.
    invalidateLaneBounds();
    const block = blockAt(kind, trackIndex, blockIndex);
    if (!block?.time_range) return;
    // Keep the browser from starting a native drag or text selection, which
    // would cancel the pointer stream mid-gesture.
    e.preventDefault();
    selectBlock(kind, trackIndex, blockIndex, e.shiftKey || e.metaKey || e.ctrlKey);
    const clock = projectClock(store.project.properties);
    const startQuarter = clock.quarterAt(block.time_range.start);
    const endQuarter = clock.quarterAt(block.time_range.end);
    if (!(endQuarter > startQuarter)) return;
    const name = blockName(kind, block);
    const element = e.currentTarget as HTMLElement;
    // Alt-drag copies a named clip, so slip takes Alt+Shift there. A phrase has
    // no stored content offset, so only a named clip can slip.
    const slip = kind === "clip" && e.altKey && e.shiftKey;
    const originOffsetQuarters = kind === "clip" ? ((block as Clip).offset_quarters ?? 0) : 0;
    const maxSlipQuarters = slip
      ? patternSlipLimit((block as Clip).pattern, endQuarter - startQuarter)
      : 0;
    if (slip && maxSlipQuarters <= 1e-9 && originOffsetQuarters <= 1e-9) {
      store.setError(i18n.t("clip.nothingToSlipPattern"));
      return;
    }
    // A grouped clip drags its whole set; every member keeps its own range. A
    // copy takes only the grabbed clip, so it stays a single-block drag.
    const copying = e.altKey && !e.shiftKey && kind === "clip";
    const group = kind === "clip" && !copying ? store.groupOfClip(trackIndex, blockIndex) : null;
    const members: DragMember[] = group
      ? store.groupMembers(group).map((member) => {
        const clip = store.project.tracks[member.trackIndex]?.clips[member.clipIndex];
        const start = clip ? clock.quarterAt(clip.time_range.start) : 0;
        const end = clip ? clock.quarterAt(clip.time_range.end) : 0;
        return {
          track: member.trackIndex,
          index: member.clipIndex,
          startQuarter: start,
          durationQuarters: end - start,
        };
      })
      : [{ track: trackIndex, index: blockIndex, startQuarter, durationQuarters: endQuarter - startQuarter }];
    // The grabbed clip is the delta anchor, so it leads the list.
    const anchor = members.findIndex(
      (member) => member.track === trackIndex && member.index === blockIndex,
    );
    if (anchor > 0) members.unshift(...members.splice(anchor, 1));
    const gesture = beginBlockGesture(slip ? "slip" : "move");
    dragSession = {
      pointerId: e.pointerId,
      element,
      kind,
      originTrack: trackIndex,
      originIndex: blockIndex,
      name,
      color: kind === "clip" ? clipColor(name, trackIndex) : trackColor(trackIndex),
      durationQuarters: endQuarter - startQuarter,
      members,
      grouped: group !== null && !slip,
      grabOffsetQuarters: beatFromClientX(e.clientX) - startQuarter,
      startClientX: e.clientX,
      startClientY: e.clientY,
      copy: copying,
      slip,
      originOffsetQuarters,
      maxSlipQuarters,
      offsetQuarters: originOffsetQuarters,
      active: false,
      gesture,
    };
    try { element.setPointerCapture?.(e.pointerId); } catch { /* not supported */ }
    document.addEventListener("pointermove", handleClipDragMove);
    document.addEventListener("pointerup", handleClipDragUp);
    document.addEventListener("pointercancel", handleClipDragCancel);
    document.addEventListener("keydown", handleClipDragKey, true);
    window.addEventListener("blur", endClipDrag);
  };

  // ── Block resize ──────────────────────────────────────────────────────────
  // The right edge of a clip or phrase changes its end; the start never moves.
  const endBlockResize = () => {
    const session = resizeSession;
    if (!session) return;
    try { session.element.releasePointerCapture?.(session.pointerId); } catch { /* not supported */ }
    document.removeEventListener("pointermove", handleResizeMove);
    document.removeEventListener("pointerup", handleResizeUp);
    document.removeEventListener("pointercancel", handleResizeCancel);
    document.removeEventListener("keydown", handleResizeKey, true);
    window.removeEventListener("blur", endBlockResize);
    resizeSession = null;
    endBlockGesture(session.gesture);
    setResizePreview(null);
  };

  const resizePreviewAt = (clientX: number): BlockResizePreview | null => {
    const session = resizeSession;
    if (!session) return null;
    const clock = projectClock(store.project.properties);
    const raw = snapBlockQuarter(Math.max(0, beatFromClientX(clientX)), gridDivision());
    const endQuarter = Math.max(session.startQuarter + 0.25, raw);
    try {
      return {
        kind: session.kind,
        track: session.track,
        index: session.index,
        end: clock.barBeatAt(endQuarter),
        width: Math.max(20, (endQuarter - session.startQuarter) * pxPerBeat()),
      };
    } catch {
      return null;
    }
  };

  const handleResizeMove = (e: PointerEvent) => {
    const session = resizeSession;
    if (!session || !ownsPointer(session, e)) return;
    if (!session.active) {
      if (Math.abs(e.clientX - session.startClientX) < 3) return;
      session.active = true;
    }
    e.preventDefault();
    const preview = resizePreviewAt(e.clientX);
    if (preview) setResizePreview(preview);
  };

  const handleResizeUp = (e: PointerEvent) => {
    const session = resizeSession;
    if (!session || !ownsPointer(session, e)) return;
    const preview = session.active ? resizePreviewAt(e.clientX) : null;
    endBlockResize();
    if (!preview) return;
    swallowTrailingClick();
    const { end } = preview;
    void withUndoGesture("project", async () => {
      if (session.kind === "clip") {
        await resizeClip(patternKey(session.track, session.index), "end", end);
      } else {
        await store.extendPhraseRange(session.track, session.index, end.bar, end.beat, end.subdivision);
      }
    });
  };

  const handleResizeCancel = (e: PointerEvent) => {
    if (resizeSession && !ownsPointer(resizeSession, e)) return;
    endBlockResize();
  };

  const handleResizeKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    endBlockResize();
  };

  const beginBlockResize = (e: PointerEvent, trackIndex: number, kind: TimelineBlockKind, blockIndex: number) => {
    if (e.button !== 0 || isSecondaryClick(e)) return;
    // The handle owns the gesture: no native drag, no move session.
    e.preventDefault();
    e.stopPropagation();
    endBlockResize();
    endMarquee();
    const block = blockAt(kind, trackIndex, blockIndex);
    if (!block?.time_range) return;
    const gesture = beginBlockGesture("resize");
    resizeSession = {
      pointerId: e.pointerId,
      element: e.currentTarget as HTMLElement,
      kind,
      track: trackIndex,
      index: blockIndex,
      startQuarter: projectClock(store.project.properties).quarterAt(block.time_range.start),
      startClientX: e.clientX,
      active: false,
      gesture,
    };
    const session = resizeSession;
    try { session.element.setPointerCapture?.(e.pointerId); } catch { /* not supported */ }
    document.addEventListener("pointermove", handleResizeMove);
    document.addEventListener("pointerup", handleResizeUp);
    document.addEventListener("pointercancel", handleResizeCancel);
    document.addEventListener("keydown", handleResizeKey, true);
    window.addEventListener("blur", endBlockResize);
  };

  // ── Marquee selection ─────────────────────────────────────────────────────
  // A drag on empty lane background paints a rectangle. Left-to-right takes every
  // block it touches; right-to-left only takes blocks it fully contains.
  const marqueeBox = (sx: number, sy: number, ex: number, ey: number): MarqueeRect | null => {
    const column = laneColumnRef?.getBoundingClientRect();
    if (!column) return null;
    const left = Math.max(0, Math.min(sx, ex) - column.left);
    const top = Math.max(0, Math.min(sy, ey) - column.top);
    return {
      left,
      top,
      width: Math.max(0, Math.max(sx, ex) - column.left - left),
      height: Math.max(0, Math.max(sy, ey) - column.top - top),
      enclosing: ex < sx,
    };
  };

  const marqueeHits = (box: MarqueeRect): TimelineBlockRef[] => {
    const column = laneColumnRef?.getBoundingClientRect();
    if (!column || box.width <= 0) return [];
    const clock = projectClock(store.project.properties);
    const beatLeft = box.left / pxPerBeat();
    const beatRight = (box.left + box.width) / pxPerBeat();
    const top = column.top + box.top;
    const bottom = top + box.height;
    const lanes = Array.from(laneColumnRef?.querySelectorAll<HTMLElement>(".track-lane") ?? []);
    const hits: TimelineBlockRef[] = [];
    lanes.forEach((lane, trackIndex) => {
      const rect = lane.getBoundingClientRect();
      if (rect.bottom <= top || rect.top >= bottom) return;
      const track = store.project.tracks[trackIndex];
      if (!track) return;
      const add = (kind: TimelineBlockKind, index: number, start: number, end: number) => {
        if (!(end > start)) return;
        const touched = end > beatLeft && start < beatRight;
        const contained = start >= beatLeft && end <= beatRight;
        if (box.enclosing ? contained : touched) hits.push({ kind, trackIndex, index });
      };
      const span = (range: TimeRange | null | undefined) => range
        ? [clock.quarterAt(range.start), clock.quarterAt(range.end)] as const
        : null;
      track.clips.forEach((clip, index) => {
        const range = span(clip.time_range);
        if (range) add("clip", index, range[0], range[1]);
      });
      track.phrases.forEach((phrase, index) => {
        const range = span(phrase.time_range);
        if (range) add("phrase", index, range[0], range[1]);
      });
      track.patterns.forEach((pattern, index) => {
        const range = span(patternPlayedRange(pattern));
        if (range) add("pattern", index, range[0], range[1]);
      });
    });
    return hits;
  };

  const mergeBlocks = (base: TimelineBlockRef[], added: TimelineBlockRef[]): TimelineBlockRef[] => {
    const merged = [...base];
    for (const block of added) {
      if (!merged.some((ref) => sameBlock(ref, block))) merged.push(block);
    }
    return merged;
  };

  /** Marquee membership: touching one member of a group takes the whole group. */
  const withGroups = (hits: TimelineBlockRef[]): TimelineBlockRef[] => {
    const expanded = [...hits];
    for (const hit of hits) {
      if (hit.kind !== "clip") continue;
      for (const pick of groupPicks(hit.trackIndex, hit.index)) {
        if (!expanded.some((ref) => sameBlock(ref, pick))) expanded.push(pick);
      }
    }
    return expanded;
  };

  const endMarquee = () => {
    const session = marqueeSession;
    if (!session) return;
    try { session.element.releasePointerCapture?.(session.pointerId); } catch { /* not supported */ }
    document.removeEventListener("pointermove", handleMarqueeMove);
    document.removeEventListener("pointerup", handleMarqueeUp);
    document.removeEventListener("pointercancel", handleMarqueeCancel);
    document.removeEventListener("keydown", handleMarqueeKey, true);
    window.removeEventListener("blur", endMarquee);
    marqueeSession = null;
    endBlockGesture(session.gesture);
    setMarqueeRect(null);
  };

  const handleMarqueeMove = (e: PointerEvent) => {
    const session = marqueeSession;
    if (!session || !ownsPointer(session, e)) return;
    if (!session.active) {
      const dx = Math.abs(e.clientX - session.startClientX);
      const dy = Math.abs(e.clientY - session.startClientY);
      if (dx < DRAG_THRESHOLD && dy < DRAG_THRESHOLD) return;
      session.active = true;
      if (!session.additive) store.setBlockSelection([]);
    }
    e.preventDefault();
    setMarqueeRect(marqueeBox(session.startClientX, session.startClientY, e.clientX, e.clientY));
  };

  const handleMarqueeUp = (e: PointerEvent) => {
    const session = marqueeSession;
    if (!session || !ownsPointer(session, e)) return;
    if (!session.active) {
      // A press without movement stays a plain click; the lane handles it.
      endMarquee();
      return;
    }
    const box = marqueeBox(session.startClientX, session.startClientY, e.clientX, e.clientY);
    const hits = box ? withGroups(marqueeHits(box)) : [];
    const picked = mergeBlocks(session.additive ? session.previous : [], hits);
    endMarquee();
    swallowTrailingClick();
    store.setBlockSelection(picked);
    clearPicks(null);
    store.setEditorSelection(null);
    timelineRef?.focus({ preventScroll: true });
  };

  const handleMarqueeCancel = (e: PointerEvent) => {
    if (marqueeSession && !ownsPointer(marqueeSession, e)) return;
    endMarquee();
  };

  const handleMarqueeKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    // Cancelling the band restores the selection it started from.
    const previous = marqueeSession?.previous ?? [];
    endMarquee();
    store.setBlockSelection(previous);
  };

  const beginMarquee = (e: PointerEvent) => {
    if (e.button !== 0 || isSecondaryClick(e)) return;
    // Keep WebKit from starting a native text selection on the lane surface.
    e.preventDefault();
    const target = e.target as HTMLElement | null;
    // Blocks own their press: a drag from a clip must keep moving that clip.
    if (target?.closest?.(".phrase-block, .clip-block, .phrase-resize-handle")) return;
    const column = laneColumnRef;
    if (!column) return;
    endClipDrag();
    endBlockResize();
    endMarquee();
    const additive = e.shiftKey || e.metaKey || e.ctrlKey;
    marqueeSession = {
      pointerId: e.pointerId,
      element: column,
      startClientX: e.clientX,
      startClientY: e.clientY,
      additive,
      previous: store.blockSelection(),
      active: false,
      gesture: beginBlockGesture("marquee"),
    };
    try { column.setPointerCapture?.(e.pointerId); } catch { /* not supported */ }
    document.addEventListener("pointermove", handleMarqueeMove);
    document.addEventListener("pointerup", handleMarqueeUp);
    document.addEventListener("pointercancel", handleMarqueeCancel);
    document.addEventListener("keydown", handleMarqueeKey, true);
    window.addEventListener("blur", endMarquee);
  };

  onCleanup(() => endMarquee());
  onCleanup(() => endClipDrag());
  onCleanup(() => endBlockResize());
  onCleanup(() => stopRulerScrub?.());

  return (
    <div class="timeline-panel" ref={timelineRef} tabIndex={0} onDragOver={(e) => {
      if (e.dataTransfer?.types.includes("application/x-pattern")) {
        e.preventDefault(); e.dataTransfer.dropEffect = "copy";
      }
    }} onDrop={handlePatternDrop}>
      {/* `.timeline-lanes` is the only vertical scroller: the headers and the
          lanes share its scroll offset, so they cannot drift apart. */}
      <div class="timeline-lanes" ref={lanesRef} onWheel={handleWheel}
        onScroll={() => {
          if (!lanesRef) return;
          invalidateLaneBounds();
          if (autoScrollLeft !== null && Math.abs(lanesRef.scrollLeft - autoScrollLeft) < 1) {
            autoScrollLeft = null;
            return;
          }
          autoScrollLeft = null;
          setFollowPlayback(false);
        }}>
        <div class="timeline-scroll-content">
          {/* Track headers — sticky left, scrolling vertically with the lanes */}
          <div class="track-headers" ref={headersRef}>
            <div class="timeline-ruler-spacer">
              <button
                type="button"
                class="tempo-lane-toggle"
                classList={{ active: tempoLaneOpen() }}
                aria-expanded={tempoLaneOpen()}
                aria-controls="timeline-tempo-lane"
                onClick={() => setTempoLaneOpen((open) => !open)}
                {...useTooltip(() => tempoLaneOpen() ? i18n.t("tempoLane.hide") : i18n.t("tempoLane.show"))}
              >
                <span class="tempo-lane-toggle-caret" aria-hidden="true">{tempoLaneOpen() ? "▾" : "▸"}</span>
                <span class="tempo-lane-toggle-label">{i18n.t("tempoLane.toggle")}</span>
              </button>
            </div>
            <Show when={tempoLaneOpen()}>
              <TempoLaneHeader />
            </Show>
            {/* Info bar spacer — matches section/key lane height */}
            {(store.project.sections.length > 0 || store.project.properties.key_map?.length > 0) && (
              <div class="timeline-info-spacer" style={{ height: "18px", "border-bottom": "1px solid var(--border)" }} />
            )}
            <For each={store.project.tracks}>
              {(track, i) => (
                <>
                <div
                  class="track-header"
                  classList={{ selected: store.selectedTrackIndex() === i(), "track-muted": track.mix.mute }}
                  onClick={() => {
                    // A header press picks the track, so it drops every other
                    // pick: Delete must not reach a clip the user left behind.
                    store.setSelectedTrackIndex(i());
                    store.setBlockSelection([]);
                    clearPicks(null);
                    store.setEditorSelection(null);
                  }}
                  style={{ height: `${trackHeight()}px`, "--track-color": trackColor(i()) }}
                  {...useTooltip(() => trackTooltipContent(track))}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    openTrackContextMenu({
                      x: event.clientX,
                      y: event.clientY,
                      trackIndex: i(),
                      onRename: () => startRename(i(), track.name),
                      onCreatePattern: () => void createPatternHere(i(), store.transport().position_beats),
                      automationShown: automationShown(i()),
                      onToggleAutomation: () => toggleAutomation(i()),
                      armed: recordStore.armed() === track.name,
                      armDisabledReason: recordStore.disabledReason(),
                      onToggleArm: () => void recordStore.toggleArm(track.name),
                      includeAddTrack: true,
                      includeOpenMixer: true,
                      includeMidi: true,
                    });
                  }}
                >
                  <div class="track-color-bar" style={{ background: trackColor(i()) }} />
                  <div class="track-info">
                    <Show when={editingTrackIndex() === i()}
                      fallback={
                        <div class="track-name" onDblClick={() => startRename(i(), track.name)}>
                          {track.name}
                        </div>
                      }
                    >
                      <input
                        class="track-name-input"
                        ref={(el) => { renameInputRef = el; }}
                        value={renameValue()}
                        onInput={(e) => setRenameValue(e.currentTarget.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") finishRename(i());
                          if (e.key === "Escape") cancelRename();
                        }}
                        onBlur={() => finishRename(i())}
                      />
                    </Show>
                    <div class="track-instrument">{trackSoundDisplay(track)}</div>
                  </div>
                  <div class="track-controls">
                    <button
                      class="track-btn track-preview-btn"
                      aria-label={store.previewTrackIndex() === i() ? i18n.t(store.transport().playing ? "tooltip.previewPause" : "tooltip.previewResume") : i18n.t("tooltip.preview")}
                      aria-pressed={store.previewTrackIndex() === i() && store.transport().playing}
                      onClick={(e) => { e.stopPropagation(); store.previewTrack(i()); }}
                      {...useTooltip(() => store.previewTrackIndex() === i() ? i18n.t(store.transport().playing ? "tooltip.previewPause" : "tooltip.previewResume") : i18n.t("tooltip.preview"))}
                    >{store.previewTrackIndex() === i() && store.transport().playing ? "Ⅱ" : "▸"}</button>
                    <button
                      class="track-btn"
                      classList={{ "mute-active": track.mix.mute }}
                      aria-label={track.mix.mute ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute")}
                      onClick={(e) => { e.stopPropagation(); store.toggleMute(i()); }}
                      {...useTooltip(() => track.mix.mute ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute"))}
                    >M</button>
                    <button
                      class="track-btn"
                      classList={{ "solo-active": track.mix.solo }}
                      aria-label={track.mix.solo ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo")}
                      onClick={(e) => { e.stopPropagation(); store.toggleSolo(i()); }}
                      {...useTooltip(() => track.mix.solo ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo"))}
                    >S</button>
                  </div>
                </div>
                <Show when={automationShown(i())}>
                  <div
                    class="track-automation-headers"
                    style={{ height: `${automationAreaHeight(track.automation ?? [])}px` }}
                  >
                    <Show
                      when={(track.automation ?? []).length > 0}
                      fallback={<AutomationEmptyHeader />}
                    >
                      <For each={track.automation}>
                        {(lane, li) => (
                          <AutomationLaneHeader
                            lane={lane}
                            onToggleMute={() => toggleAutomationMute(`track:${i()}`, li())}
                          />
                        )}
                      </For>
                    </Show>
                  </div>
                </Show>
                <Show when={takeGroupAt(track.name)}>
                  <TakeHeaders
                    trackName={track.name}
                    clips={audioClips()}
                    solo={store.takeSolo()}
                    open={takeLanesOpen(track.name)}
                    onToggle={toggleTakeLanes}
                    onMute={setTakeMute}
                    onSolo={toggleTakeSolo}
                  />
                </Show>
                </>
              )}
            </For>
            {/* Bus and master lanes: synthetic rows, they have no track header. */}
            <For each={ownerAutomationGroups()}>
              {(group) => (
                <>
                  <AutomationOwnerStrip label={i18n.t("timeline.automation.owner", { owner: group.owner })} />
                  <div
                    class="track-automation-headers"
                    style={{ height: `${automationAreaHeight(group.lanes)}px` }}
                  >
                    <For each={group.lanes}>
                      {(lane, li) => (
                        <AutomationLaneHeader
                          lane={lane}
                          onToggleMute={() => toggleAutomationMute(group.target, li())}
                        />
                      )}
                    </For>
                  </div>
                </>
              )}
            </For>
            {/* Add track button */}
            <div style={{ height: "24px", display: "flex", "align-items": "center", padding: "0 4px", "border-top": "1px solid var(--border)" }}
              {...useTooltip(() => i18n.t("timeline.addTrack"))}
              onClick={() => store.addTrack(
                uniqueName(`Track ${store.project.tracks.length + 1}`, store.project.tracks.map((t) => t.name)),
                "piano", "acoustic_grand",
              )}
              onDragOver={(e) => {
                if (e.dataTransfer?.types.includes("application/x-pattern")) {
                  e.preventDefault(); e.dataTransfer.dropEffect = "copy";
                }
              }}
              onDrop={async (e) => {
                const patternName = e.dataTransfer?.getData("application/x-pattern");
                if (patternName) {
                  e.preventDefault();
                  const newIdx = await store.addTrack(patternName, "piano", "acoustic_grand");
                  if (newIdx >= 0) {
                    store.addClip(newIdx, patternName, 1, 1, 0, 5, 1, 0);
                  }
                }
              }}
            >
              <button class="toolbar-btn" style={{ "font-size": "11px", width: "100%" }}>{i18n.t("timeline.addTrack")}</button>
            </div>
            <Show when={store.project.tracks.length === 0}>
              <div class="empty-state" style="height: 200px">
                <div class="empty-state-hint">
                  {i18n.t("timeline.dragPresetHint")}
                  <span class="empty-state-shortcut">
                    {i18n.t("timeline.pressPresets", {
                      key: commandBindingLabel("view.togglePresets") || "P",
                    })}
                  </span>
                </div>
              </div>
            </Show>
          </div>

          {/* Timeline lane column */}
          <div class="timeline-lane-column" ref={laneColumnRef} style={{ width: `${totalWidth()}px` }}>
            {/* Bar ruler — clickable to seek */}
            <div
              ref={rulerRef}
              class="timeline-ruler"
              style={{ width: `${totalWidth()}px`, cursor: "pointer" }}
              onPointerDown={handleRulerPointerDown}
              onContextMenu={openRulerMenu}
            >
              {/* Session loop region: a tinted band, drawn while dragging */}
              <Show when={loopBand()}>
                {(region) => (
                  <div
                    class="timeline-loop-band"
                    classList={{ "loop-armed": store.loop().enabled }}
                    data-loop-band
                    style={{
                      left: `${region().start * pxPerBeat()}px`,
                      width: `${Math.max((region().end - region().start) * pxPerBeat(), 2)}px`,
                    }}
                  />
                )}
              </Show>
              {/* Punch range: the span a take keeps notes inside. */}
              <Show when={store.punchRegion()}>
                {(region) => (
                  <div
                    class="timeline-punch-band"
                    classList={{ "punch-armed": store.punch().enabled }}
                    data-punch-band
                    style={{
                      left: `${region().start * pxPerBeat()}px`,
                      width: `${Math.max((region().end - region().start) * pxPerBeat(), 2)}px`,
                    }}
                  />
                )}
              </Show>
              <For each={barMarks(store.project.properties, totalBars())}>
                {(mark) => (
                  <span class="timeline-ruler-mark" style={{ left: `${mark.quarter * pxPerBeat()}px` }}>
                    {mark.bar}
                  </span>
                )}
              </For>
              {/* Marker flags: click jumps, drag moves once per gesture */}
              <For each={store.project.markers ?? []}>
                {(marker) => {
                  const preview = () => markerDrag()?.name === marker.name ? markerDrag()!.beat : null;
                  const left = () => (preview() ?? markerQuarter(marker.position)) * pxPerBeat();
                  return (
                    <div
                      class="timeline-marker"
                      classList={{ dragging: preview() !== null }}
                      data-marker={marker.name}
                      style={{ left: `${left()}px` }}
                      aria-label={i18n.t("marker.flagLabel", { name: marker.name })}
                      {...useTooltip(() => i18n.t("marker.flagHint", { name: marker.name }))}
                      onPointerDown={(e) => startMarkerDrag(e, marker)}
                      onDblClick={(e) => { e.preventDefault(); e.stopPropagation(); setMarkerRename(marker); }}
                      onContextMenu={(e) => openMarkerMenu(e, marker)}
                    >
                      <span class="timeline-marker-flag" />
                      <span class="timeline-marker-name">{marker.name}</span>
                    </div>
                  );
                }}
              </For>
              {/* Dragging the visible cursor explicitly takes arrangement ownership. */}
              <Show when={cursorX() >= 0}>
                <div
                  class="cursor-head"
                  classList={{ "preview-cursor-head": previewing() }}
                  style={{ left: `${cursorX() - 6}px` }}
                  onPointerDown={handleRulerPointerDown}
                />
              </Show>
              <Show when={store.markerBusy() || store.markerError() || store.loopPending() || store.loopError() || store.loop().clamped}>
                <div class="timeline-marker-status" role="status">
                  <Show when={store.markerBusy()}>
                    <span>{i18n.t("marker.saving")}</span>
                  </Show>
                  <Show when={!store.markerBusy() && store.markerError()}>
                    <span>{i18n.t("marker.failed", { message: store.markerError() ?? "" })}</span>
                  </Show>
                  <Show when={!store.markerError() && store.loopPending()}>
                    <span>{i18n.t("loop.pending")}</span>
                  </Show>
                  <Show when={!store.markerError() && !store.loopPending() && store.loopError()}>
                    <span>{i18n.t("loop.failed", { message: store.loopError() ?? "" })}</span>
                  </Show>
                  <Show when={!store.markerError() && !store.loopPending() && !store.loopError() && store.loop().clamped}>
                    <span>{i18n.t("loop.clamped")}</span>
                  </Show>
                </div>
              </Show>

              {/* Loop strip: drag here to draw the region; the ruler keeps seeking. */}
              <div
                class="timeline-loop-strip"
                data-loop-strip
                aria-label={i18n.t("loop.stripLabel")}
                {...useTooltip(() => i18n.t("loop.stripHint"))}
                onPointerDown={startLoopDrag}
                onContextMenu={openLoopMenu}
              />
            </div>

            <Show when={tempoLaneOpen()}>
              <TempoLane width={totalWidth()} pxPerBeat={pxPerBeat()} />
            </Show>

            {/* Info bar: sections + key in one compact stripe above ruler */}
            {(store.project.sections.length > 0 || (store.project.properties.key_map?.length ?? 0) > 0) && (
              <div style={{ width: `${totalWidth()}px`, height: "18px", position: "relative","border-bottom": "1px solid var(--border)", overflow: "hidden" }}>
                <For each={store.project.sections}>
                  {(section) => {
                    if (!section.time_range) return null;
                    const left = () => barBeatToPixel(section.time_range.start.bar, section.time_range.start.beat, section.time_range.start.subdivision, pxPerBeat(), beatsPerBar());
                    const right = () => barBeatToPixel(section.time_range.end.bar, section.time_range.end.beat, section.time_range.end.subdivision, pxPerBeat(), beatsPerBar());
                    const hue = (section.name.charCodeAt(0) * 137) % 360;
                    return (
                      <div style={{ position: "absolute", left: `${left()}px`, width: `${Math.max(right() - left(), 30)}px`, height: "100%", background: `hsla(${hue}, 20%, 30%, 0.15)`,"border-left": `2px solid hsla(${hue}, 30%, 50%, 0.5)`, display: "flex", "align-items": "center", "padding-left": "4px","font-size": "9px", "font-family": "var(--font-mono)", color: `hsla(${hue}, 25%, 65%, 0.8)`, overflow: "hidden" }} {...useTooltip(() => `${section.name}${section.label ? ` — ${section.label}` : ""}`)}>
                        {section.name}
                      </div>
                    );
                  }}
                </For>
                <For each={store.project.properties.key_map ?? []}>
                  {(kpt) => {
                    const acc = kpt.accidental === "Sharp" ? "#" : kpt.accidental === "Flat" ? "b" : "";
                    return (
                      <div style={{ position: "absolute", left: `${barBeatToPixel(kpt.position.bar, kpt.position.beat, kpt.position.subdivision, pxPerBeat(), beatsPerBar())}px`, top: "2px","font-size": "9px", color: "var(--text-muted)", background: "rgba(120,180,255,0.15)", padding: "0 3px", "border-radius": "2px" }}>
                        {kpt.root}{acc}
                      </div>
                    );
                  }}
                </For>
              </div>
            )}

            {/* Track lanes */}
            <For each={store.project.tracks}>
              {(track, i) => {
                const overlaps = () => overlapIndex()[i()] ?? EMPTY_OVERLAPS;
                const dragGhost = createMemo(() => {
                  const preview = dragPreview();
                  // A block renders its own displacement inside its lane; the
                  // ghost stands in only in a lane the block cannot reach.
                  return preview && preview.targetTrack === i() && preview.originTrack !== i() ? preview : null;
                });
                return (
                <>
                <div
                  class="track-lane"
                  classList={{
                    "clip-drop-target": dragPreview()?.targetTrack === i(),
                    "track-muted": track.mix.mute,
                    "track-lane-alt": i() % 2 === 1,
                    // The preview marks the subtrees the proposal rewrites.
                    "preview-changed": previewStore.changesTrack(track.name),
                  }}
                  style={{ width: `${totalWidth()}px`, height: `${trackHeight()}px` }}
                  onPointerDown={beginMarquee}
                  onClick={(e) => {
                    const target = e.target as HTMLElement;
                    if (target.closest(".phrase-block") || target.closest(".clip-block")) return;
                    timelineRef?.focus({ preventScroll: true });
                    store.setSelectedTrackIndex(i());
                    setSelectedBlock(null);
                    clearPicks(null);
                    store.setEditorSelection(null);
                  }}
                  onContextMenu={(e) => {
                    if (track.performance) { e.preventDefault(); return; }
                    // Only show context menu on empty lane area (not on clips)
                    const target = e.target as HTMLElement;
                    if (target.closest(".phrase-block") || target.closest(".clip-block")) return;
                    e.preventDefault();
                    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
                    const beat = Math.max(0, (e.clientX - rect.left) / pxPerBeat());
                    openTrackContextMenu({
                      x: e.clientX,
                      y: e.clientY,
                      trackIndex: i(),
                      onRename: () => startRename(i(), track.name),
                      onCreatePattern: () => void createPatternHere(i(), beat),
                      automationShown: automationShown(i()),
                      onToggleAutomation: () => toggleAutomation(i()),
                      includeAddTrack: true,
                      includeOpenMixer: true,
                      includeMidi: true,
                    });
                  }}
                >
                  <Show when={track.performance?.events.length}>
                    <button class="clip-block performance-block"
                      aria-label={i18n.t("timeline.performanceBlockLabel", { name: track.name })}
                      {...useTooltip(() => i18n.t("timeline.performanceBlockHint"))}
                      style={{ left: `${(track.performance?.events[0]?.quarter ?? 0) * pxPerBeat()}px`,
                        width: `${Math.max(80, ((track.performance?.events.at(-1)?.quarter ?? 0) - (track.performance?.events[0]?.quarter ?? 0)) * pxPerBeat())}px`,
                        top: "8px", height: "48px", "border-left": `4px solid ${trackColor(i())}` }}
                      onClick={(e) => { e.stopPropagation(); setSelectedBlock(null); store.setSelectedTrackIndex(i()); store.setEditorSelection(null); panelStore.openBottom("pianoroll"); }}>
                      {i18n.t("timeline.performanceBlock", { count: track.performance?.events.length ?? 0 })}
                    </button>
                  </Show>
                  {/* Clips (named pattern references) */}
                  <For each={track.clips}>
                    {(clip, ci) => {
                      const range = clip.time_range;
                      if (!range) return null;
                      const left = () => barBeatToPixel(range.start.bar, range.start.beat, range.start.subdivision, pxPerBeat(), beatsPerBar());
                      const right = () => barBeatToPixel(range.end.bar, range.end.beat, range.end.subdivision, pxPerBeat(), beatsPerBar());
                      const width = () => Math.max(right() - left(), 20);
                      const isOverlap = overlaps().has(ci());
                      const color = clipColor(clip.pattern, i());
                      const patInfo = findPattern(clip.pattern);
                      const clipBeats = () => clipLengthBeats(range, beatsPerBar());
                      const slip = () => {
                        const preview = slipPreview();
                        return preview && preview.track === i() && preview.index === ci()
                          ? preview
                          : null;
                      };
                      const offsetBeats = () => slip()?.offsetQuarters ?? (clip.offset_quarters ?? 0);
                      // Repeat seams at every loop period inside the block.
                      const loopBeats = () => clip.loop_quarters ?? 0;
                      const loopSeams = () => {
                        const step = loopBeats();
                        if (!(step > 0)) return [];
                        const seams: number[] = [];
                        for (let at = step; at < clipBeats() - 1e-9 && seams.length < 256; at += step) {
                          seams.push((at / clipBeats()) * 100);
                        }
                        return seams;
                      };
                      const notes = () => noteThumbnails(patInfo?.events ?? [], clipBeats(), offsetBeats(), loopBeats());
                      const hits = createMemo(() => {
                        if (!patInfo?.hits.length) return [];
                        const clock = projectClock(store.project.properties);
                        const start = clock.quarterAt(range.start);
                        return hitThumbnails(patInfo.hits, clock.quarterAt(range.end) - start, offsetBeats(), loopBeats(), {
                          start, swing: patInfo.swing ?? store.project.properties.swing,
                        });
                      });
                      const isSelected = () => isBlockSelected("clip", i(), ci());
                      const group = () => store.groupOfClip(i(), ci());
                      const follow = () => followPreview("clip", i(), ci());
                      const resize = () => resizePreviewFor("clip", i(), ci());
                      return (
                        <div
                          class="phrase-block clip-block"
                          classList={{
                            "clip-overlap": isOverlap,
                            selected: isSelected(),
                            "clip-empty": notes().length === 0 && hits().length === 0,
                            dragging: draggingClip()?.kind === "clip" && draggingClip()?.track === i() && draggingClip()?.index === ci(),
                            "drag-follow": follow() !== null,
                            resizing: resize() !== null,
                            "preview-changed": previewStore.changesClip(track.name, ci()),
                          }}
                          style={`left: ${follow()?.left ?? left()}px; width: ${resize()?.width ?? follow()?.width ?? width()}px; --clip-accent: ${color};`}
                          {...useTooltip(() => clipTooltipContent(
                            track, clip, projectClock(store.project.properties), isSelected(), group()?.members.length ?? null,
                          ))}                          onClick={(e) => {
                            e.stopPropagation();
                            selectClip(i(), ci(), e.shiftKey || e.metaKey || e.ctrlKey);
                            // Double-click edits the notes; the inspector is a
                            // separate action, not what a clip opens to.
                            if (e.detail === 2) {
                              panelStore.openBottom("pianoroll");
                            }
                          }}
                          onContextMenu={(e) => openBlockContextMenu(e, "clip", i(), ci())}
                          onPointerDown={(e) => beginClipDrag(e, i(), "clip", ci())}
                        >
                          <span class="clip-accent-flag" aria-hidden="true" />
                          <div class="clip-thumbnail" aria-hidden="true">
                            <For each={notes()}>
                              {(note) => <span class="clip-note-thumb" style={{ left: note.left, width: note.width, top: note.top, height: note.height }} />}
                            </For>
                            <For each={hits()}>
                              {(hit) => <span class="clip-hit-thumb" style={{ left: hit.left, top: hit.top }} />}
                            </For>
                            <For each={loopSeams()}>
                              {(left) => (
                                <span class="clip-loop-seam" style={{ left: `${left}%` }} aria-hidden="true" />
                              )}
                            </For>
                          </div>
                          <Show when={group()}>
                            <span class="clip-group-badge" aria-hidden="true">◈</span>
                          </Show>
                          <div class="clip-label-chip">
                            <span class="phrase-block-name">{clip.pattern}</span>
                            <Show when={patInfo}>
                              <span class="clip-meta">
                                {loopBeats() > 0
                                  ? i18n.t("clip.loopChip", { value: formatClipBeats(loopBeats()) }) + " "
                                  : ""}
                                {pitchedNotes(patInfo!.events).length}n {patInfo!.hits.length}h
                              </span>
                            </Show>
                          </div>
                          <div
                            class="phrase-resize-handle right"
                            onPointerDown={(e) => beginBlockResize(e, i(), "clip", ci())}
                          />
                          <Show when={slip()}>
                            {(preview) => (
                              <div class="clip-slip-readout" style={{ left: `${preview().x}px` }}>
                                <span>
                                  {i18n.t("clip.slipReadout", {
                                    value: formatClipBeats(preview().offsetQuarters),
                                  })}
                                </span>
                              </div>
                            )}
                          </Show>
                        </div>
                      );
                    }}
                  </For>

                  {/* Audio clips carrying a decoded waveform */}
                  <For each={audioClips()}>
                    {(clip, ci) => {
                      if (clip.track !== track.name) return null;
                      return (
                        <AudioClipBlock
                          clip={clip}
                          index={ci()}
                          color={trackColor(i())}
                          height={trackHeight() - 10}
                          pxPerBeat={pxPerBeat()}
                          selected={selectedAudioClip() === ci()}
                          tooltipContent={() => audioClipTooltipContent(track, clip, projectClock(store.project.properties), selectedAudioClip() === ci())}
                          beatFromClientX={beatFromClientX}
                          trackAtClientY={(clientY) => {
                            const laneIndex = trackAtClientY(clientY);
                            return laneIndex === null
                              ? null
                              : (store.project.tracks[laneIndex]?.name ?? null);
                          }}
                          onSelect={selectAudioClip}
                          onEditPlayback={(index) => gainEnvelopeWindow.open(index, "playback")}
                          onStretch={(index, end, revision) => {
                            void store.setAudioClipPlayback(index, { stretch_to: end }, revision).then((error) => {
                              if (error) store.setError(error);
                            });
                          }}
                          onWarp={(index, markers, revision) => {
                            void store.setAudioClipPlayback(index, { warp_markers: markers }, revision).then((error) => {
                              if (error) store.setError(error);
                            });
                          }}
                          onMove={(index, targetTrack, start) => {
                            void withUndoGesture("project", () =>
                              moveClip(audioClipKey(index), { ...start, trackName: targetTrack }),
                            );
                          }}
                          onTrim={(index, start, end, edge) => {
                            void withUndoGesture("project", () =>
                              resizeClip(audioClipKey(index), edge, edge === "start" ? start : end),
                            );
                          }}
                          onEnvelope={(index, envelope, revision) => {
                            void withUndoGesture("project", () =>
                              store.setAudioClipEnvelope(index, envelope, revision),
                            );
                          }}
                          onSlip={(index, offsetSeconds, revision) => {
                            void withUndoGesture("project", () =>
                              store.setAudioClipOffset(index, offsetSeconds, revision),
                            );
                          }}
                          onLoop={(index, loopSeconds, revision) => {
                            void withUndoGesture("project", () =>
                              store.setAudioClipLoop(index, loopSeconds, revision),
                            );
                          }}
                          onError={(message) => store.setError(message)}
                          onContextMenu={(event) => openPoolClipMenu(event, audioClipKey(ci()))}
                        />
                      );
                    }}
                  </For>

                  {/* Drag preview — a single ghost in the lane under the pointer */}
                  <Show when={dragGhost()}>
                    {(preview) => (
                      <div
                        class="phrase-block clip-block clip-drag-ghost"
                        classList={{ "clip-drag-copy": preview().copy }}
                        style={`left: ${preview().left}px; width: ${preview().width}px; --clip-accent: ${preview().color};`}
                        aria-hidden="true"
                      >
                        <div class="clip-label-chip">
                          <span class="phrase-block-name">{preview().name}</span>
                          <span class="clip-meta">{preview().copy ? "copy" : ""}</span>
                        </div>
                      </div>
                    )}
                  </Show>

                  {/* Inline phrases */}
                  <For each={track.phrases}>
                    {(phrase, pi) => {
                      const range = phrase.time_range;
                      if (!range) return null;
                      const left = () => barBeatToPixel(range.start.bar, range.start.beat, range.start.subdivision, pxPerBeat(), beatsPerBar());
                      const right = () => barBeatToPixel(range.end.bar, range.end.beat, range.end.subdivision, pxPerBeat(), beatsPerBar());
                      const width = () => Math.max(right() - left(), 20);
                      const phraseBeats = () => clipLengthBeats(range, beatsPerBar());
                      const notes = () => noteThumbnails(phrase.events ?? [], phraseBeats());
                      const follow = () => followPreview("phrase", i(), pi());
                      const resize = () => resizePreviewFor("phrase", i(), pi());
                      return (
                        <div
                          class="phrase-block phrase-note-block"
                          classList={{
                            "clip-empty": notes().length === 0,
                            selected: isBlockSelected("phrase", i(), pi()),
                            dragging: draggingClip()?.kind === "phrase" && draggingClip()?.track === i() && draggingClip()?.index === pi(),
                            "drag-follow": follow() !== null,
                            resizing: resize() !== null,
                          }}
                          style={`left: ${follow()?.left ?? left()}px; width: ${resize()?.width ?? follow()?.width ?? width()}px; --clip-accent: ${trackColor(i())};`}
                          {...useTooltip(() => phraseTooltipContent(track, phrase, projectClock(store.project.properties), isBlockSelected("phrase", i(), pi())))}                          onClick={(e) => {
                            e.stopPropagation();
                            selectBlock("phrase", i(), pi(), e.shiftKey || e.metaKey || e.ctrlKey);
                            if (e.detail === 2) {
                              // Phrases are inline notes (not named patterns) — open Piano Roll
                              panelStore.openBottom("pianoroll");
                            }
                          }}
                          onContextMenu={(e) => openBlockContextMenu(e, "phrase", i(), pi())}
                          onPointerDown={(e) => beginClipDrag(e, i(), "phrase", pi())}
                        >
                          <span class="clip-accent-flag" aria-hidden="true" />
                          <div class="clip-thumbnail" aria-hidden="true">
                            <For each={notes()}>
                              {(note) => <span class="clip-note-thumb" style={{ left: note.left, width: note.width, top: note.top, height: note.height }} />}
                            </For>
                          </div>
                          <div class="clip-label-chip">
                            <span class="phrase-block-name">{phrase.name}</span>
                            <span class="clip-meta">{pitchedNotes(phrase.events).length}n</span>
                          </div>
                          <div
                            class="phrase-resize-handle right"
                            onPointerDown={(e) => beginBlockResize(e, i(), "phrase", pi())}
                          />
                        </div>
                      );
                    }}
                  </For>

                  {/* Inline patterns */}
                  <For each={track.patterns}>
                    {(pattern, pi) => {
                      const range = pattern.time_range;
                      if (!range) return null;
                      const left = () => barBeatToPixel(range.start.bar, range.start.beat, range.start.subdivision, pxPerBeat(), beatsPerBar());
                      const repeats = () => Math.max(1, pattern.repeat);
                      const width = () => Math.max(repeats() * beatsPerBar() * pxPerBeat(), 20);
                      const hits = () => {
                        const barHits = hitThumbnails(pattern.hits, beatsPerBar());
                        return Array.from({ length: repeats() }, (_, repeat) =>
                          barHits.map((hit) => ({ ...hit, left: `${(repeat * 100 + parseFloat(hit.left)) / repeats()}%` })),
                        ).flat();
                      };
                      const follow = () => followPreview("pattern", i(), pi());
                      return (
                        <div
                          class="phrase-block pattern-block"
                          classList={{
                            selected: isBlockSelected("pattern", i(), pi()),
                            dragging: draggingClip()?.kind === "pattern" && draggingClip()?.track === i() && draggingClip()?.index === pi(),
                            "drag-follow": follow() !== null,
                          }}
                          style={{ left: `${follow()?.left ?? left()}px`, width: `${follow()?.width ?? width()}px`, "--clip-accent": trackColor(i()) }}
                          {...useTooltip(() => inlinePatternTooltipContent(track, pattern, projectClock(store.project.properties), isBlockSelected("pattern", i(), pi())))}                          onClick={(e) => {
                            e.stopPropagation();
                            selectBlock("pattern", i(), pi(), e.shiftKey || e.metaKey || e.ctrlKey);
                            if (e.detail === 2) {
                              panelStore.openBottom("pianoroll");
                            }
                          }}
                          onContextMenu={(e) => openBlockContextMenu(e, "pattern", i(), pi())}
                          onPointerDown={(e) => beginClipDrag(e, i(), "pattern", pi())}
                        >
                          <div class="clip-thumbnail" aria-hidden="true">
                            <For each={hits()}>
                              {(hit) => <span class="clip-hit-thumb" style={{ left: hit.left, top: hit.top }} />}
                            </For>
                          </div>
                          <div class="clip-label-chip">
                            <span class="phrase-block-name">{pattern.name} x{pattern.repeat}</span>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </div>
                <Show when={automationShown(i())}>
                  <div
                    class="track-automation-lanes"
                    style={{ height: `${automationAreaHeight(track.automation ?? [])}px` }}
                  >
                    <Show
                      when={(track.automation ?? []).length > 0}
                      fallback={<AutomationEmptyLane />}
                    >
                      <For each={track.automation}>
                        {(lane, li) => (
                          <AutomationLaneRow
                            lane={lane}
                            laneIndex={li()}
                            pxPerBeat={pxPerBeat()}
                            width={totalWidth()}
                            selectedPoint={selectedPointIndex(`track:${i()}`, li())}
                            {...automationCallbacks(`track:${i()}`, li())}
                          />
                        )}
                      </For>
                    </Show>
                  </div>
                </Show>
                <Show when={takeGroupAt(track.name)}>
                  <TakeLanes
                    trackName={track.name}
                    clips={audioClips()}
                    pxPerBeat={pxPerBeat()}
                    width={totalWidth()}
                    open={takeLanesOpen(track.name)}
                    onComp={compTakeRange}
                  />
                </Show>
                </>
              )}}
            </For>

            {/* Bus and master lanes: synthetic rows at the end of the lane list. */}
            <For each={ownerAutomationGroups()}>
              {(group) => (
                <>
                  <AutomationOwnerStrip label={i18n.t("timeline.automation.owner", { owner: group.owner })} />
                  <div
                    class="track-automation-lanes"
                    style={{ height: `${automationAreaHeight(group.lanes)}px` }}
                  >
                    <For each={group.lanes}>
                      {(lane, li) => (
                        <AutomationLaneRow
                          lane={lane}
                          laneIndex={li()}
                          pxPerBeat={pxPerBeat()}
                          width={totalWidth()}
                          selectedPoint={selectedPointIndex(group.target, li())}
                          {...automationCallbacks(group.target, li())}
                        />
                      )}
                    </For>
                  </div>
                </>
              )}
            </For>

            {/* Rubber-band rectangle; painted above the blocks and inert */}
            <Show when={marqueeRect()}>
              {(box) => (
                <div
                  class="timeline-marquee"
                  classList={{ enclosing: box().enclosing }}
                  style={{
                    left: `${box().left}px`,
                    top: `${box().top}px`,
                    width: `${box().width}px`,
                    height: `${box().height}px`,
                  }}
                  aria-hidden="true"
                />
              )}
            </Show>

            {/* Playback cursor line — preview mode paints the preview position */}
            <Show when={cursorX() >= 0}>
              <div
                class="playback-cursor"
                classList={{ "preview-cursor": previewing() }}
                style={{ left: `${cursorX()}px` }}
              />
            </Show>
          </div>
        </div>
      </div>

      {/* Zoom pill — floats at the bottom centre, same language as the grid pill. */}
      <div class="timeline-zoom-center">
        <button
          class="zoom-step"
          aria-label={i18n.t("timeline.zoomOut")}
          onClick={() => setZoom(Math.max(6, zoom() - 4))}
        >−</button>
        <span class="zoom-label">{i18n.t("timeline.pxPerBeat", { px: Math.round(zoom()) })}</span>
        <button
          class="zoom-step"
          aria-label={i18n.t("timeline.zoomIn")}
          onClick={() => setZoom(Math.min(120, zoom() + 4))}
        >+</button>
      </div>

      {/* The grid step is exact; a block position is written on a 16th. State the
          floor the timeline applies instead of coarsening the drag silently. */}
      <Show when={blockPositionNote(gridDivision())}>
        {(note) => <div class="timeline-grid-floor" role="note">{note()}</div>}
      </Show>

      {/* Grid pill — bottom right. */}
      <div class="timeline-zoom-bar">
        <Show when={store.groupBusy()}>
          <span class="timeline-grid-busy" role="status">{i18n.t("clip.groupPending")}</span>
        </Show>
        <label class="timeline-grid-label" {...useTooltip(() => i18n.t("timeline.gridHint"))}>
          <span class="zoom-label">{i18n.t("menu.grid.grid")}</span>
          <select
            class="timeline-grid-select"
            value={gridDivision()}
            onChange={(e) => setGridDivision(parseInt(e.currentTarget.value))}
          >
            <For each={GRID_OPTIONS}>
              {(option) => <option value={option.value}>{gridOptionLabel(option)}</option>}
            </For>
          </select>
        </label>
      </div>

      <Show when={renamePrompt()}>
        {(request) => (
          <TextPromptDialog
            title={i18n.t("menu.renamePrompt")}
            initial={request().initial}
            onCancel={() => setRenamePrompt(null)}
            onConfirm={(name) => {
              request().confirm(name);
              setRenamePrompt(null);
            }}
          />
        )}
      </Show>

      {/* Point tension: the value that rounds a smooth segment, 0..1. */}
      <Show when={tensionPrompt()}>
        {(request) => (
          <TextPromptDialog
            title={i18n.t("timeline.automation.tensionTitle")}
            initial={request().initial}
            onCancel={() => setTensionPrompt(null)}
            onConfirm={(raw) => {
              const target = request();
              setTensionPrompt(null);
              const tension = Number(raw);
              if (!Number.isFinite(tension)) return;
              setAutomationCurveAt(
                target.target,
                target.laneIndex,
                target.pointIndex,
                "smooth",
                tension,
              );
            }}
          />
        )}
      </Show>

      <Show when={pictureOpen()}>
        <PictureTimecodeDialog
          rate={store.project.properties.picture_clock?.rate ?? pictureRate()}
          origin={store.project.properties.picture_clock?.origin ?? pictureOrigin()}
          onClock={(rate, origin) => { setPictureRate(rate); setPictureOrigin(origin); }}
          onClose={() => setPictureOpen(false)}
        />
      </Show>

      {/* Marker rename: the webview has no prompt, so the app owns the dialog. */}
      <Show when={markerRename()}>
        {(marker) => (
          <TextPromptDialog
            title={i18n.t("marker.renameTitle")}
            initial={marker().name}
            onCancel={() => setMarkerRename(null)}
            onConfirm={(name) => {
              const target = marker();
              setMarkerRename(null);
              void store.renameMarker(target.name, name);
            }}
          />
        )}
      </Show>

      {/* Marker delete: a confirmation, because the undo entry is not obvious. */}
      <Show when={markerDelete()}>
        {(marker) => (
          <div
            class="text-prompt-overlay"
            onPointerDown={(event) => {
              if (event.target === event.currentTarget) setMarkerDelete(null);
            }}
          >
            <div
              class="text-prompt"
              role="alertdialog"
              aria-modal="true"
              aria-label={i18n.t("marker.deleteTitle")}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setMarkerDelete(null);
                }
              }}
            >
              <div class="text-prompt-label">
                <span>{i18n.t("marker.deleteTitle")}</span>
                <p>{i18n.t("marker.deleteBody", { name: marker().name })}</p>
              </div>
              <div class="text-prompt-actions">
                <button
                  type="button"
                  class="text-prompt-btn"
                  onClick={() => setMarkerDelete(null)}
                >{i18n.t("dialog.cancel")}</button>
                <button
                  type="button"
                  class="text-prompt-btn primary"
                  onClick={() => {
                    const target = marker();
                    setMarkerDelete(null);
                    void store.removeMarker(target.name);
                  }}
                >{i18n.t("marker.delete")}</button>
              </div>
            </div>
          </div>
        )}
      </Show>
      <BounceDialog />
    </div>
  );
}
