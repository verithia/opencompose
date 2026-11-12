import { For, Show, createSignal, createMemo, createEffect, onCleanup, onMount } from "solid-js";
import { store } from "../stores/project";
import { recordStore } from "../stores/record";
import { previewStore } from "../stores/preview";
import { PerformanceEditor } from "./PerformanceEditor";
import { createProjectClock } from "../utils/projectClock";
import { beginPointerDrag } from "../utils/pointerDrag";
import { patternKey } from "../stores/clipPool";
import { resizeClip } from "../stores/clipActions";
import { beginUndoGesture, endUndoGesture } from "../stores/gesture";
import { isSecondaryClick, withUndoGesture } from "../stores/gesture";
import { registerCommand } from "../stores/commands";

import { GRID_OPTIONS, gridDivision, gridOptionLabel, setGridDivision, snapQuarterToGrid } from "../stores/grid";
import { useTooltip } from "./Tooltip";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import { i18n, type I18nKey } from "../stores/i18n";
import * as api from "../stores/api";
import {
  ARTICULATIONS,
  articulationTag,
  articulationToken,
  dynamicsToVelocity,
  INJECTED_PRESET_PREFIX,
  pitchToMidi,
  pitchToString,
  durationToBeats,
  instrumentDisplay,
  type Articulation,
  type Dynamics,
  type HitEvent,
  type NoteInsert,
  type NoteMove,
  type NoteRef,
  type NoteScope,
  type ParamValue,
  type PatternDef,
  type NoteName,
  type Accidental,
  type NoteEvent,
  type TimeRange,
} from "../types/project";
import { keyswitchFor, type Keyswitch } from "../utils/keyswitches";
import { patternLengthQuarters } from "../utils/clipContent";
import {
  HIT_STEP_QUARTERS,
  drumForNote,
  hitNote,
  hitStartQuarters,
  patternHitOnsets,
  isDrumKitPreset,
  kitFromPreset,
  kitLaneRows,
  type DrumLaneRow,
  type Kit,
  type PatternHitWindow,
} from "../utils/drumkit";
import {
  hairpinAt,
  patternHairpins,
  pedalSpans,
  phraseEndBeat,
  phraseHairpins,
  rampedVelocity,
  type PedalSpan,
  type ScoreHairpin,
} from "../utils/scoreMarks";

export const NOTE_HEIGHT = 12;
export const MIN_MIDI = 24;
export const MAX_MIDI = 96;
export const NOTE_RANGE = MAX_MIDI - MIN_MIDI;
export const PIANO_ROLL_RULER_HEIGHT = 24;
export const PIANO_ROLL_KEYS_WIDTH = 48;
/** Left gutter of a drum lane: it names each pad, so it needs more room. */
export const DRUM_LANE_KEYS_WIDTH = 132;
/**
 * Height of the velocity lane pinned under the note grid. Bars are drawn inside
 * `VELOCITY_LANE_INSET` at the top and bottom so 1 and 127 stay apart.
 */
export const VELOCITY_LANE_HEIGHT = 56;
export const VELOCITY_LANE_INSET = 6;
/** Height of the articulation lane pinned under the velocity lane. */
export const ARTICULATION_LANE_HEIGHT = 24;
/** Height of the score-mark strip between the ruler and the note canvas. */
export const SCORE_STRIP_HEIGHT = 14;
/** Middle C (MIDI 60) — the pitch the Piano Roll opens centered on. */
export const PIANO_ROLL_CENTER_MIDI = 60;
/** A zero-length clip range still gets one grid step instead of a broken canvas. */
const MIN_VIEW_SPAN_BEATS = 0.25;

/** Quantise strengths the toolbar offers; 0 % is offered so the button can say so. */
const QUANTISE_STRENGTHS = [0, 25, 50, 75, 100];

/** A humanise amount: how far a note may move, in editor ticks and velocity. */
interface HumaniseAmount {
  key: string;
  timingTicks: number;
  velocitySpread: number;
  label: I18nKey;
}

const HUMANISE_AMOUNTS: HumaniseAmount[] = [
  { key: "timing-1", timingTicks: 1, velocitySpread: 0, label: "pianoroll.humaniseTimingOne" },
  { key: "timing-2", timingTicks: 2, velocitySpread: 0, label: "pianoroll.humaniseTimingTwo" },
  {
    key: "timing-1-velocity-12",
    timingTicks: 1,
    velocitySpread: 12,
    label: "pianoroll.humaniseTimingAndVelocity",
  },
];

/** A legato amount: `steps` grid steps, or null to reach the next note. */
interface LegatoAmount {
  key: string;
  steps: number | null;
  label: I18nKey;
}

const LEGATO_AMOUNTS: LegatoAmount[] = [
  { key: "next", steps: null, label: "pianoroll.legatoToNext" },
  { key: "one-step", steps: 1, label: "pianoroll.legatoOneStep" },
  { key: "two-steps", steps: 2, label: "pianoroll.legatoTwoSteps" },
];

/** Fresh humanise seed per click; the command stays deterministic for one seed. */
let humaniseSeed = 0;
const nextHumaniseSeed = () => ++humaniseSeed;

type ToolName = "quantise" | "humanise" | "legato";

const TOOL_LABELS: Record<ToolName, I18nKey> = {
  quantise: "pianoroll.quantise",
  humanise: "pianoroll.humanise",
  legato: "pianoroll.legato",
};

const TOOL_PENDING: Record<ToolName, I18nKey> = {
  quantise: "pianoroll.quantisePending",
  humanise: "pianoroll.humanisePending",
  legato: "pianoroll.legatoPending",
};

const TOOL_HINTS: Record<ToolName, I18nKey> = {
  quantise: "pianoroll.quantiseHint",
  humanise: "pianoroll.humaniseHint",
  legato: "pianoroll.legatoHint",
};

const TOOL_FAILED: Record<ToolName, I18nKey> = {
  quantise: "pianoroll.quantiseFailed",
  humanise: "pianoroll.humaniseFailed",
  legato: "pianoroll.legatoFailed",
};

/** Velocity lane gestures: ramp between the endpoints, scale, compress to a target. */
type VelocityTool = "ramp" | "scale" | "compress";

const VELOCITY_TOOL_LABELS: Record<VelocityTool, I18nKey> = {
  ramp: "pianoroll.ramp",
  scale: "pianoroll.scaleVelocity",
  compress: "pianoroll.compressVelocity",
};

const VELOCITY_TOOL_PENDING: Record<VelocityTool, I18nKey> = {
  ramp: "pianoroll.rampPending",
  scale: "pianoroll.scaleVelocityPending",
  compress: "pianoroll.compressVelocityPending",
};

const VELOCITY_TOOL_HINTS: Record<VelocityTool, I18nKey> = {
  ramp: "pianoroll.rampHint",
  scale: "pianoroll.scaleVelocityHint",
  compress: "pianoroll.compressVelocityHint",
};

const VELOCITY_TOOL_FAILED: Record<VelocityTool, I18nKey> = {
  ramp: "pianoroll.rampFailed",
  scale: "pianoroll.scaleVelocityFailed",
  compress: "pianoroll.compressVelocityFailed",
};

/** Scale percentages the lane offers; 100 % is left out because it is a no-op. */
const SCALE_PERCENTS = [50, 75, 125, 150];

/** Targets a compress moves the selection halfway toward. */
const COMPRESS_TARGETS = [40, 64, 80, 96];

/** Top of a MIDI row inside the note canvas, in pixels. */
export function midiRowTop(midi: number): number {
  return (MAX_MIDI - 1 - midi) * NOTE_HEIGHT;
}

/**
 * Scroll offset that vertically centers `midi` in the note grid.
 * The ruler is sticky at the top of the scrollport, so only the area below
 * it is usable. Results are clamped so the request stays inside the content
 * bounds (the target is then merely "as close as the bounds allow").
 */
export function scrollTopToCenterMidi(
  clientHeight: number,
  midi: number = PIANO_ROLL_CENTER_MIDI,
): number {
  const contentHeight = PIANO_ROLL_RULER_HEIGHT + NOTE_RANGE * NOTE_HEIGHT;
  const visibleHeight = clientHeight - PIANO_ROLL_RULER_HEIGHT;
  if (visibleHeight <= 0) return 0;
  const desired = midiRowTop(midi) + NOTE_HEIGHT / 2 - visibleHeight / 2;
  const maxScroll = Math.max(0, contentHeight - clientHeight);
  return Math.min(Math.max(0, desired), maxScroll);
}

/** Canvas x for an absolute beat when x = 0 is the clip's start. */
export function xForBeat(originBeat: number, pxPerBeat: number, beat: number): number {
  return (beat - originBeat) * pxPerBeat;
}

/** Absolute beat under a canvas x; never returns a beat before the clip origin. */
export function beatAtX(originBeat: number, pxPerBeat: number, x: number): number {
  return originBeat + Math.max(0, x) / pxPerBeat;
}

/** A hit position as a sixteenth anchor and an exact grid offset. */
export interface HitPlacement {
  /** One-based beat the position is written against. */
  beat: number;
  /** Sixteenth inside the beat, 0-3. */
  subdivision: number;
  /** Quarters between the written position and the beat the user aimed at. */
  offsetBeats: number;
}

/**
 * Encode a local onset as a sixteenth anchor plus a signed grid offset.
 * The offset keeps triplets and finer steps part of the written score.
 */
export function hitPlacement(localBeat: number): HitPlacement {
  const step = Math.round(localBeat / HIT_STEP_QUARTERS);
  const offsetBeats = localBeat - step * HIT_STEP_QUARTERS;
  const stepsPerBeat = Math.round(1 / HIT_STEP_QUARTERS);
  return {
    beat: Math.floor(step / stepsPerBeat) + 1,
    subdivision: ((step % stepsPerBeat) + stepsPerBeat) % stepsPerBeat,
    offsetBeats: Math.abs(offsetBeats) < 1e-9 ? 0 : offsetBeats,
  };
}

/**
 * One copied note: the whole event the AST stores, plus its start relative to
 * the copied group. The note itself is a snapshot, not a live document object.
 */
type NoteClipboardEntry = {
  offsetBeats: number;
  event: NoteEvent;
};

let noteClipboard: NoteClipboardEntry[] | null = null;

/** Drop the cross-pattern clipboard (tests start from a clean editor). */
export function clearNoteClipboard(): void {
  noteClipboard = null;
}

/** A clipboard copy must not follow later edits, so it is a deep copy. */
function cloneNote(event: NoteEvent): NoteEvent {
  return JSON.parse(JSON.stringify(event)) as NoteEvent;
}

/** Dynamics tokens in DSL spelling, loudest last. */
const DYNAMICS_TOKENS: [Dynamics, string][] = [
  ["PPP", "ppp"], ["PP", "pp"], ["P", "p"], ["MP", "mp"],
  ["MF", "mf"], ["F", "f"], ["FF", "ff"], ["FFF", "fff"],
];

function noteVelocity(event: NoteEvent): number {
  return event.velocity ?? (event.dynamics ? dynamicsToVelocity(event.dynamics) : 80);
}

/** Where a note's loudness is written: an exact velocity, a dynamics token, or neither. */
export type VelocitySource = "velocity" | "dynamics" | "default";

export function velocitySource(event: NoteEvent): VelocitySource {
  if (event.velocity !== null && event.velocity !== undefined) return "velocity";
  return event.dynamics ? "dynamics" : "default";
}

/**
 * Short phrase naming a note's loudness and where it came from, so `mf` and
 * `vel=80` never read as the same note: `mf 80`, `mf→96`, `v 96`, `80 (default)`.
 */
export function velocityText(
  velocity: number,
  source: VelocitySource,
  dynamics: Dynamics | null,
): string {
  const token = dynamics ? dynamics.toLowerCase() : "";
  switch (source) {
    case "velocity": return token ? `${token}\u2192${velocity}` : `v ${velocity}`;
    case "dynamics": return `${token} ${velocity}`;
    default: return i18n.t("pianoroll.velocityDefault", { velocity });
  }
}

type DisplayNoteEvent = NoteEvent & {
  __phraseIndex?: number;
  __noteIndex?: number;
  __phraseOffsetBeats?: number;
};

/** A hit, with the pattern and index that own it when the track view made it. */
type DisplayHitEvent = HitEvent & {
  __patternName?: string;
  __hitIndex?: number;
};

type DisplayPatternDef = PatternDef & {
  events: DisplayNoteEvent[];
  hits: DisplayHitEvent[];
  /** The owning track's engine params; a pattern has none of its own. */
  engine_params?: [string, ParamValue][] | null;
  /** Hairpin spans with the levels their ramp moves between; absent when none. */
  hairpins?: ScoreHairpin[];
  /** Pedal holds, paired down-to-up; absent when none. */
  pedalSpans?: PedalSpan[];
};

export type DisplayNoteBlock = {
  midi: number;
  startBeat: number;
  durationBeats: number;
  velocity: number;
  velocitySource: VelocitySource;
  dynamics: Dynamics | null;
  articulation: Articulation | null;
  /** Key the engine presses for `articulation`, or null when none is declared. */
  keyswitch: Keyswitch | null;
  muted: boolean;
  grace: boolean;
  /** The mark's duration ratio (`3:2`), or null when the note is not a tuplet. */
  tuplet: [number, number] | null;
  /** The note slides to the next one (`gliss` modifier). */
  gliss: boolean;
  /** The hairpin whose ramp moves this note, or null outside every span. */
  hairpin: ScoreHairpin | null;
  /**
   * Level the hairpin's ramp puts the note at, or null outside every span.
   * The bar keeps the written value; the ghost line shows what the ramp does.
   */
  ghostVelocity: number | null;
  label: string;
  noteIndex: number;
  phraseIndex: number;
  /** True when the note starts at or past the pattern's or clip's end. */
  outOfRange: boolean;
  phraseOffsetBeats: number;
};

/**
 * The notes the grid draws: inside the pitch range and inside the opened clip.
 * The velocity lane reads the same list, so it can never show a bar for a note
 * the grid hides.
 *
 * `presetParams` is the param list of the preset that owns the sound, so the
 * articulation lane can name the keyswitch the engine will send. Omit it when
 * no preset is known; the block then carries no keyswitch.
 *
 * `rows` is the drum lane's row list: it replaces the semitone range, because a
 * kit row may sit on any MIDI note and a note with no row has nowhere to draw.
 */
export function visibleNoteBlocks(
  patternDef: DisplayPatternDef,
  xForBeat: (beat: number) => number,
  viewWidth: number,
  presetParams?: [string, ParamValue][] | null,
  lengthBeats?: number,
  rows?: DrumLaneRow[] | null,
): DisplayNoteBlock[] {
  const blocks: DisplayNoteBlock[] = [];
  const hairpins = patternDef.hairpins ?? [];
  for (let ni = 0; ni < patternDef.events.length; ni++) {
    const event = patternDef.events[ni];
    const isRest = event.modifiers?.some(([key]: [string, unknown]) => key === "rest");
    if (isRest) continue;
    const midi = pitchToMidi(event.pitch);
    if (rows ? !rows.some((row) => row.note === midi) : midi < MIN_MIDI || midi >= MAX_MIDI) {
      continue; // outside the drawn pitch range
    }
    const noteX = xForBeat(event.start_beat ?? 0);
    if (noteX < 0 || noteX >= viewWidth) continue; // outside the clip
    const source = noteSource(event, ni);
    const articulation = event.articulation ?? null;
    const writtenFrom = velocitySource(event);
    const velocity = noteVelocity(event);
    const hairpin = hairpinAt(hairpins, event.start_beat ?? 0);
    // The engine replaces the level of a note that writes none with the span's
    // start level; a written level keeps its value and the track gain moves it.
    const rampBase = hairpin && writtenFrom === "default" ? hairpin.startVelocity : velocity;
    blocks.push({
      midi,
      startBeat: event.start_beat ?? 0,
      durationBeats: durationToBeats(event.duration),
      velocity,
      velocitySource: writtenFrom,
      dynamics: event.dynamics ?? null,
      articulation,
      keyswitch: keyswitchFor(articulation, patternDef.engine_params, presetParams),
      muted: event.muted ?? false,
      grace: event.grace ?? false,
      tuplet: event.duration.tuplet ?? null,
      gliss: (event.modifiers ?? []).some(([key]) => key === "gliss"),
      hairpin,
      ghostVelocity: hairpin
        ? rampedVelocity(hairpin, event.start_beat ?? 0, rampBase)
        : null,
      label: pitchToString(event.pitch),
      noteIndex: source.noteIndex,
      phraseIndex: source.phraseIndex,
      phraseOffsetBeats: source.phraseOffsetBeats,
      outOfRange:
        lengthBeats !== undefined && (event.start_beat ?? 0) >= lengthBeats - 1e-9,
    });
  }
  return blocks;
}

/** One authored drum hit shared by the grid and velocity lane. */
export type DisplayHitBlock = {
  beat: number;
  velocity: number;
  velocitySource: VelocitySource;
  dynamics: Dynamics | null;
  label: string;
  hitIndex: number;
  /** True when the selected placement plays no onset for this hit. */
  outOfRange: boolean;
};

type DisplayDrumHit = DisplayHitBlock & {
  note: number;
  patternName: string | null;
};

/** Hit loudness: an exact value, else the written dynamic, else the engine default. */
function hitVelocity(event: HitEvent): number {
  return event.velocity ?? (event.dynamics ? dynamicsToVelocity(event.dynamics) : 100);
}

function hitVelocitySource(event: HitEvent): VelocitySource {
  if (event.velocity !== null && event.velocity !== undefined) return "velocity";
  return event.dynamics ? "dynamics" : "default";
}

/** A lane bar's height in px; proportional to velocity inside the lane's inset. */
export function velocityBarHeight(velocity: number): number {
  const usable = VELOCITY_LANE_HEIGHT - VELOCITY_LANE_INSET * 2;
  return Math.max(1, Math.round((Math.min(127, Math.max(1, velocity)) / 127) * usable));
}

/**
 * The usable lane box a velocity maps against: the lane body shrunk by the
 * inset on both sides, so 1 and 127 sit on the body's inner edges.
 *
 * The pointer-to-value mapping uses this box, never a bar's own box — a bar's
 * height is the value being edited, so aiming with it rescales mid-drag.
 */
export function velocityLaneBox(body: Pick<DOMRect, "top" | "height">): { top: number; height: number } {
  // A body jsdom reports as zero-height cannot be aimed at; use the lane height.
  const height = body.height > 0 ? body.height : VELOCITY_LANE_HEIGHT;
  return {
    top: body.top + VELOCITY_LANE_INSET,
    height: Math.max(1, height - VELOCITY_LANE_INSET * 2),
  };
}

/** Velocity a pointer at `clientY` selects inside the usable lane box, bottom = 1. */
export function velocityAtClientY(clientY: number, box: Pick<DOMRect, "top" | "height">): number {
  const height = box.height > 0 ? box.height : VELOCITY_LANE_HEIGHT - VELOCITY_LANE_INSET * 2;
  const ratio = 1 - (clientY - box.top) / height;
  return Math.min(127, Math.max(1, Math.round(ratio * 127)));
}

/** End of a pattern's own content, used to close a pedal nothing lifts. */
function patternContentEnd(pattern: PatternDef): number {
  return pattern.events.reduce(
    (end, event) => Math.max(end, (event.start_beat ?? 0) + durationToBeats(event.duration)),
    0,
  );
}

function noteSource(event: DisplayNoteEvent, fallbackIndex: number) {
  return {
    phraseIndex: event.__phraseIndex ?? 0,
    noteIndex: event.__noteIndex ?? fallbackIndex,
    phraseOffsetBeats: event.__phraseOffsetBeats ?? 0,
  };
}

/**
 * A note's identity in the document: its phrase, pitch and own start beat.
 *
 * An index cannot name a note across an edit — every move re-sorts the list, so
 * index *i* names a different note afterwards. The roll keeps identities and
 * resolves them against the live list on every read instead.
 */
type NoteIdentity = {
  phraseIndex: number;
  midi: number;
  startBeat: number;
};

/** Identity of a displayed block, in the coordinates the document stores. */
function blockIdentity(
  block: Pick<DisplayNoteBlock, "phraseIndex" | "midi" | "startBeat" | "phraseOffsetBeats">,
): NoteIdentity {
  return {
    phraseIndex: block.phraseIndex,
    midi: block.midi,
    startBeat: block.startBeat - block.phraseOffsetBeats,
  };
}

/** Identity of a displayed event, in the coordinates the document stores. */
function eventIdentity(event: DisplayNoteEvent, index: number): NoteIdentity {
  return {
    phraseIndex: noteSource(event, index).phraseIndex,
    midi: pitchToMidi(event.pitch),
    startBeat: (event.start_beat ?? 0) - (event.__phraseOffsetBeats ?? 0),
  };
}

/** One key per note, so a selection never lists the same note twice. */
function identityKey(identity: NoteIdentity): string {
  return `${identity.phraseIndex}:${identity.midi}:${identity.startBeat}`;
}

/**
 * Resolve identities against the live note list, one note per identity.
 *
 * Each identity claims a distinct note, so two identical notes stay two. The
 * ones the document no longer holds are counted in `missing` and come back as
 * null: a stale selection is then a refusal, never an edit of whatever note
 * inherited that index.
 */
function resolveNoteSelection(
  events: DisplayNoteEvent[],
  identities: NoteIdentity[],
): { refs: (NoteRef | null)[]; missing: number } {
  const claimed = new Array<boolean>(events.length).fill(false);
  const refs: (NoteRef | null)[] = [];
  let missing = 0;
  for (const identity of identities) {
    let match = -1;
    for (let index = 0; index < events.length; index++) {
      if (claimed[index]) continue;
      const candidate = eventIdentity(events[index], index);
      if (
        candidate.phraseIndex === identity.phraseIndex
        && candidate.midi === identity.midi
        && Math.abs(candidate.startBeat - identity.startBeat) < 1e-6
      ) {
        match = index;
        break;
      }
    }
    if (match < 0) {
      missing++;
      refs.push(null);
      continue;
    }
    claimed[match] = true;
    const source = noteSource(events[match], match);
    refs.push({ phrase_index: source.phraseIndex, note_index: source.noteIndex });
  }
  return { refs, missing };
}

function isBlackKey(midi: number): boolean {
  return [1, 3, 6, 8, 10].includes(midi % 12);
}

function midiToNoteParts(midi: number): { name: NoteName; accidental: Accidental; octave: number } {
  const names: [NoteName, Accidental][] = [
    ["C", "Natural"], ["C", "Sharp"], ["D", "Natural"], ["D", "Sharp"],
    ["E", "Natural"], ["F", "Natural"], ["F", "Sharp"], ["G", "Natural"],
    ["G", "Sharp"], ["A", "Natural"], ["A", "Sharp"], ["B", "Natural"],
  ];
  const [name, accidental] = names[midi % 12];
  const octave = Math.floor(midi / 12) - 1;
  return { name, accidental, octave };
}

/** Map a duration in beats to the nearest named duration string for the backend. */
function beatsToDurationName(beats: number): string {
  // Standard durations: w=4, h=2, q=1, 8th=0.5, 16th=0.25, 32nd=0.125
  const base = beats > 0 ? beats : 0.25;
  if (base >= 3.0) return "w";
  if (base >= 1.5) return "h";
  if (base >= 0.75) return "q";
  if (base >= 0.375) return "8th";
  if (base >= 0.1875) return "16th";
  return "32nd";
}

/** Determine dot count for a duration. */
function beatsToDots(beats: number, durName: string): number {
  const baseBeats: Record<string, number> = { w: 4, h: 2, q: 1, "8th": 0.5, "16th": 0.25, "32nd": 0.125 };
  const b = baseBeats[durName] ?? 1;
  // Dotted = 1.5x base
  if (Math.abs(beats - b * 1.5) < 0.01) return 1;
  // Double-dotted = 1.75x base
  if (Math.abs(beats - b * 1.75) < 0.01) return 2;
  return 0;
}

/** Check if duration is a triplet (2/3 of base). */
function beatsIsTriplet(beats: number, durName: string): boolean {
  const baseBeats: Record<string, number> = { w: 4, h: 2, q: 1, "8th": 0.5, "16th": 0.25, "32nd": 0.125 };
  const b = baseBeats[durName] ?? 1;
  return Math.abs(beats - b * (2 / 3)) < 0.01;
}

export function PianoRoll() {
  return <Show when={store.selectedTrack()?.performance} fallback={<NotePianoRoll />}>
    <PerformanceEditor track={store.selectedTrack()!} trackIndex={store.selectedTrackIndex()!}
      onEdit={store.editPerformanceEvent} />
  </Show>;
}

function NotePianoRoll() {
  const [zoom, setZoom] = createSignal(40);
  const gridDiv = gridDivision;
  const setGridDiv = setGridDivision;
  const [tool, setTool] = createSignal<"select" | "draw">("draw");
  let gridRef: HTMLDivElement | undefined;
  let canvasRef: HTMLDivElement | undefined;

  // Drag state — tracks what note is being moved/resized
  const [dragState, setDragState] = createSignal<{
    noteIndex: number;
    phraseIndex: number;
    phraseOffsetBeats: number;
    mode: "move" | "resize";
    originalStartBeat: number;
    originalDurationBeats: number;
    dragStartX: number;
    dragStartY: number;
    noteMidi: number;
    shiftKey: boolean;
  } | null>(null);
  const [dragOffsetX, setDragOffsetX] = createSignal(0);
  const [dragPitch, setDragPitch] = createSignal(0);
  let drawing = false;

  // Draw preview — follows mouse in draw mode
  const [drawPreview, setDrawPreview] = createSignal<{
    midi: number; startBeat: number; durBeats: number; label: string;
  } | null>(null);

  // The selection names notes, not indices: see `NoteIdentity`.
  const [selection, setSelection] = createSignal<NoteIdentity[]>([]);
  /** The drum hit the menu and the delete key act on, by pattern and index. */
  const [selectedHit, setSelectedHit] = createSignal<{
    patternName: string;
    hitIndex: number;
  } | null>(null);
  const [marquee, setMarquee] = createSignal<{
    startX: number; startY: number; endX: number; endY: number;
  } | null>(null);

  const pxPerBeat = () => zoom();
  const track = () => store.selectedTrack();
  const trackIdx = () => store.selectedTrackIndex();
  const selPattern = () => store.selectedPattern();

  /**
   * Whether a note that starts at `startBeat` sits in a subtree the open
   * preview changed. The note keeps rendering from the document the backend
   * installed; this only adds the highlight class.
   */
  const previewChangedNote = (startBeat: number): boolean => {
    if (!previewStore.open()) return false;
    const pattern = activePattern();
    const signature = store.project.properties.time_signature;
    const beatsPerBar = Math.max(1, (signature.numerator * 4) / Math.max(1, signature.denominator));
    const bar = Math.floor(Math.max(0, startBeat) / beatsPerBar) + 1;
    if (pattern && previewStore.changesNote(pattern.name, bar)) return true;
    // The roll may show a track's own pattern, which a track rewrite covers.
    return selPattern() === null && previewStore.changesTrack(track()?.name ?? "");
  };

  const activePattern = createMemo<DisplayPatternDef | null>(() => {
    const sp = selPattern();
    if (sp) {
      // A pattern writes its own hairpins; a placement expands them on its
      // track, so a pattern opened on its own shows those marks in local beats.
      const end = patternContentEnd(sp);
      return {
        ...(sp as DisplayPatternDef),
        hairpins: patternHairpins(sp.hairpins ?? [], sp.events, end),
        pedalSpans: pedalSpans(sp.pedals ?? [], end),
      };
    }
    const t = track();
    if (!t) return null;
    const clock = createProjectClock(store.project.properties);
    const bb2b = (bb: { bar: number; beat: number; subdivision: number }) =>
      clock.quarterAt(bb);
    const events = t.phrases.flatMap((ph, phraseIndex) => {
      const off = ph.time_range ? bb2b(ph.time_range.start) : 0;
      return ph.events.map((ev, noteIndex) => ({
        ...ev,
        start_beat: (ev.start_beat ?? 0) + off,
        __phraseIndex: phraseIndex,
        __noteIndex: noteIndex,
        __phraseOffsetBeats: off,
      }));
    });
    const hits = t.patterns.flatMap(pat => {
      const start = pat.time_range ? bb2b(pat.time_range.start) : 0;
      return Array.from({ length: pat.repeat }, (_, r) =>
        pat.hits.map((h, hitIndex) => {
          const originBar = pat.time_range?.start.bar ?? 1;
          const repeatOffset = bb2b({bar:originBar+r,beat:1,subdivision:0}) - bb2b({bar:originBar,beat:1,subdivision:0});
          const absBeat = start + repeatOffset + (h.position.beat - 1) + h.position.subdivision * 0.25;
          // The hit keeps the pattern and index it came from, so a menu opened
          // on it edits that hit rather than the copy drawn here.
          return {
            ...h,
            position: { beat: absBeat + 1, subdivision: 0 },
            __patternName: pat.name,
            __hitIndex: hitIndex,
          } as DisplayHitEvent;
        })
      ).flat();
    });
    // Phrase marks resolve per phrase, then land at absolute beats, so the
    // strip and the notes it rides over share one coordinate space.
    const phraseMarks = t.phrases.map((ph) => {
      const start = ph.time_range ? bb2b(ph.time_range.start) : 0;
      const end = phraseEndBeat(
        ph.events,
        ph.time_range ? bb2b(ph.time_range.end) : null,
        start,
      );
      return {
        hairpins: phraseHairpins(ph.hairpins ?? [], ph.events, start, end),
        pedals: pedalSpans(ph.pedals ?? [], end - start)
          .map((span) => ({ start: span.start + start, end: span.end + start })),
      };
    });
    // A pattern's pedal marks ride with its hits; a repeated pattern draws them
    // once, at its first pass.
    const patternPedals = t.patterns.flatMap((pat) => {
      const def = store.project.patterns.find((pattern) => pattern.name === pat.name);
      if (!def?.pedals?.length) return [];
      const start = pat.time_range ? bb2b(pat.time_range.start) : 0;
      const end = phraseEndBeat(def.events, null, start);
      return pedalSpans(def.pedals, end - start)
        .map((span) => ({ start: span.start + start, end: span.end + start }));
    });
    return {
      name: t.name,
      instrument: t.instrument,
      preset: t.preset ?? null,
      engine: t.engine ?? null,
      transpose: null,
      swing: null,
      effects: [],
      events,
      hits,
      // The track view is the only place these exist: phrase marks land at
      // absolute beats here, and the track's params resolve keyswitches.
      engine_params: t.engine_params ?? null,
      hairpins: phraseMarks.flatMap((marks) => marks.hairpins),
      pedalSpans: [...phraseMarks.flatMap((marks) => marks.pedals), ...patternPedals],
      color: null,
    };
  });

  /**
   * The notes the selection still names, in selection order. An identity the
   * document no longer holds resolves to null, so a stale pick is visible as
   * such rather than silently pointing at a neighbour.
   */
  const resolvedSelection = createMemo(() => {
    const ap = activePattern();
    return ap ? resolveNoteSelection(ap.events, selection()) : { refs: [], missing: 0 };
  });

  /** Index keys of the resolved notes, the form the grid renders and reads. */
  const selectedNotes = createMemo(
    () => new Set(resolvedSelection().refs
      .filter((ref): ref is NoteRef => ref !== null)
      .map((ref) => `${ref.phrase_index}:${ref.note_index}`)),
  );

  /**
   * The selection a gesture may act on, or why it may not. A selection that no
   * longer matches the document is refused; it never falls back to whatever
   * note inherited the index.
   */
  const liveSelection = (): { refs: NoteRef[] } | { refusal: string } => {
    const { refs, missing } = resolvedSelection();
    if (missing > 0) {
      setSelection([]);
      return { refusal: i18n.t("pianoroll.selectionStale") };
    }
    return { refs: refs.filter((ref): ref is NoteRef => ref !== null) };
  };

  /**
   * The preset that owns the sound the roll is showing: the pattern's own
   * preset, else its hosting track's. The editor's injected defaults mean
   * "follow the track", so they are not a choice either.
   */
  const openPreset = createMemo(() => {
    const ap = activePattern();
    if (!ap) return null;
    const chosen = ap.preset && !ap.preset.startsWith(INJECTED_PRESET_PREFIX) ? ap.preset : null;
    const name = chosen ?? track()?.preset ?? null;
    if (!name) return null;
    return store.project.presets.find((preset) => preset.name === name) ?? null;
  });

  const presetParams = createMemo<[string, ParamValue][] | null>(
    () => openPreset()?.params ?? null,
  );

  /** The open preset read as a drum kit, or null when the sound is not a kit. */
  const openKit = createMemo<Kit | null>(() => {
    const preset = openPreset();
    return preset && isDrumKitPreset(preset) ? kitFromPreset(preset) : null;
  });

  /**
   * The drum lane's rows, or null when the roll shows a pitched instrument.
   * A kit turns the pitch axis into one row per pad plus an `Other` row for
   * every used note the kit does not declare; a plain drum track keeps the
   * General MIDI rows, because it has no pads to name.
   */
  const laneRows = createMemo<DrumLaneRow[] | null>(() => {
    const kit = openKit();
    const ap = activePattern();
    if (!kit || !ap || ap.instrument.category !== "drums") return null;
    const used = ap.hits.map((hit) => hitNote(hit, kit));
    for (const event of ap.events) {
      used.push(pitchToMidi(event.pitch));
    }
    const rows = kitLaneRows(kit, used);
    // A kit with no pads names nothing, so the General MIDI rows serve better.
    return rows.length > 0 ? rows : null;
  });

  /** Rows the note canvas holds: the lane's rows, else every semitone. */
  const rowCount = () => laneRows()?.length ?? NOTE_RANGE;

  /** Left gutter width; a drum lane names its pads. */
  const keysWidth = () => (laneRows() ? DRUM_LANE_KEYS_WIDTH : PIANO_ROLL_KEYS_WIDTH);

  /** Top of a pitch's row, or null when the open lane has no row for it. */
  const rowTopOf = (note: number): number | null => {
    const rows = laneRows();
    if (!rows) return midiRowTop(note);
    const index = rows.findIndex((row) => row.note === note);
    return index < 0 ? null : index * NOTE_HEIGHT;
  };

  /** The pitch a canvas y lands on, or null outside every row. */
  const rowNoteAt = (y: number): number | null => {
    const rows = laneRows();
    const index = Math.floor(y / NOTE_HEIGHT);
    if (rows) return index >= 0 && index < rows.length ? rows[index].note : null;
    const midi = MAX_MIDI - 1 - index;
    return midi >= MIN_MIDI && midi < MAX_MIDI ? midi : null;
  };

  const headerTitle = () => {
    const p = activePattern();
    if (p) {
      return selPattern()
        ? i18n.t("pianoroll.patternTitle", { name: p.name })
        : i18n.t("pianoroll.trackTitle", { name: p.name });
    }
    return i18n.t("panel.pianoroll");
  };

  // Offset for pattern-relative cursor: when viewing a standalone pattern,
  // subtract the clip's start beat so cursor aligns with pattern-local notes.
  const patternOffsetBeats = createMemo(() => {
    const sp = selPattern();
    if (!sp) return 0; // Track view — events already in absolute coords
    const sel = store.editorSelection();
    if (sel && sel.patternName === sp.name && sel.trackIndex === trackIdx()) return sel.startBeat;
    const bb2b = (bb: { bar: number; beat: number; subdivision: number }) =>
      createProjectClock(store.project.properties).quarterAt(bb);
    const t = track();
    // Inline pattern blocks place the pattern on the track itself.
    const placed = t?.patterns.filter(pat => pat.name === sp.name && pat.time_range) ?? [];
    if (placed.length > 0) {
      const selectedStart = sel && sel.trackIndex === trackIdx() ? sel.startBeat : null;
      const clicked = placed.find(pat => bb2b(pat.time_range!.start) === selectedStart);
      return clicked ? selectedStart! : bb2b(placed[0].time_range!.start);
    }
    // Find the first clip referencing this pattern
    for (const track of store.project.tracks) {
      for (const clip of track.clips) {
        if (clip.pattern === sp.name && clip.time_range) {
          return bb2b(clip.time_range.start);
        }
      }
    }
    return 0;
  });

  // Absolute beat at x = 0: the opened clip's start. Nothing before it exists.
  const viewOriginBeats = createMemo(() => {
    if (selPattern()) return patternOffsetBeats();
    const sel = store.editorSelection();
    if (sel && sel.trackIndex === trackIdx()) return sel.startBeat;
    const t = track();
    if (!t) return 0;
    const clock = createProjectClock(store.project.properties);
    let origin = Infinity;
    for (const ph of t.phrases) {
      if (ph.time_range) origin = Math.min(origin, clock.quarterAt(ph.time_range.start));
    }
    for (const pat of t.patterns) {
      if (pat.time_range) origin = Math.min(origin, clock.quarterAt(pat.time_range.start));
    }
    return Number.isFinite(origin) ? origin : 0;
  });

  // Track block the editor was opened from, while it still exists. It sets
  // placement bounds even when the drum editor shows later authored hits.
  const openedBlock = createMemo<{
    start: number;
    end: number;
    sourceOffset?: number;
    loop?: number;
  } | null>(() => {
    const sel = store.editorSelection();
    const ti = trackIdx();
    const t = track();
    if (!sel || ti === null || sel.trackIndex !== ti || !t) return null;
    const clock = createProjectClock(store.project.properties);
    const startOf = (range: TimeRange) => clock.quarterAt(range.start);
    const endOf = (range: TimeRange) => clock.quarterAt(range.end);
    if (sel.patternName !== null) {
      const clip = t.clips.find(
        (c) => c.pattern === sel.patternName && startOf(c.time_range) === sel.startBeat,
      );
      return clip ? {
        start: sel.startBeat,
        end: endOf(clip.time_range),
        sourceOffset: clip.offset_quarters,
        loop: clip.loop_quarters,
      } : null;
    }
    const sp = selPattern();
    if (sp) {
      const placed = t.patterns.find(
        (p) => p.name === sp.name && p.time_range && startOf(p.time_range) === sel.startBeat,
      );
      return placed?.time_range ? { start: sel.startBeat, end: endOf(placed.time_range) } : null;
    }
    const phrase = t.phrases.find(
      (ph) => ph.time_range && startOf(ph.time_range) === sel.startBeat,
    );
    return phrase?.time_range ? { start: sel.startBeat, end: endOf(phrase.time_range) } : null;
  });

  // Notes are native (pattern-local or absolute), so the view mapping shifts
  // them by the same origin.
  const originLocalBeat = () => viewOriginBeats() - patternOffsetBeats();
  const xForLocalBeat = (beat: number) =>
    xForBeat(viewOriginBeats(), pxPerBeat(), beat + patternOffsetBeats());

  const viewSpanBeats = createMemo(() => {
    const origin = viewOriginBeats();
    const block = openedBlock();
    const blockSpan = block && Math.abs(block.start - origin) < 1e-9 ? block.end - origin : null;
    let span: number;
    if (blockSpan !== null) {
      // The roll edits the pattern; a clip shorter than its pattern must not
      // hide the part the pattern owns.
      span = Math.max(blockSpan, selPattern() ? patternLengthQuarters(activePattern()) : 0);
      // A drum hit beyond the placement still belongs to the pattern and
      // must stay selectable. Keep a little editing room after that hit.
      if (selPattern()) {
        for (const hit of activePattern()?.hits ?? []) {
          const beat = hitStartQuarters(hit);
          if (beat >= blockSpan - 1e-9) span = Math.max(span, beat + 1);
        }
      }
    } else {
      const p = activePattern();
      const offset = patternOffsetBeats();
      let contentEnd = origin;
      // A declared length can reach past the written content, so the view
      // keeps room for every bar the pattern owns.
      if (p) contentEnd = Math.max(contentEnd, offset + patternLengthQuarters(p));
      for (const ev of p?.events ?? []) {
        contentEnd = Math.max(
          contentEnd,
          (ev.start_beat ?? 0) + offset + durationToBeats(ev.duration),
        );
      }
      for (const hit of p?.hits ?? []) {
        contentEnd = Math.max(
          contentEnd,
          hitStartQuarters(hit) + offset + 0.5,
        );
      }
      if (!selPattern()) {
        const clock = createProjectClock(store.project.properties);
        for (const ph of track()?.phrases ?? []) {
          if (ph.time_range) contentEnd = Math.max(contentEnd, clock.quarterAt(ph.time_range.end));
        }
        for (const pat of track()?.patterns ?? []) {
          if (pat.time_range) contentEnd = Math.max(contentEnd, clock.quarterAt(pat.time_range.end));
        }
      }
      // Unplaced patterns and whole-track views keep one bar of editing room.
      span = Math.max(16, Math.ceil(Math.max(0, contentEnd - origin) / 4) * 4 + 4);
    }
    return Math.max(span, MIN_VIEW_SPAN_BEATS);
  });

  const viewEndBeats = createMemo(() => viewOriginBeats() + viewSpanBeats());
  // End of the span placement may use: the opened clip's end, else the
  // pattern's declared or derived length. A draw past it lands on the boundary.
  const placementEndBeats = createMemo(() => {
    const block = openedBlock();
    if (block) return block.end;
    // A whole-track view has no single container to stop at; the view stays the
    // bound, as it always did.
    if (!selPattern()) return viewEndBeats();
    const p = activePattern();
    if (!p) return viewEndBeats();
    return patternOffsetBeats() + patternLengthQuarters(p);
  });
  const placementEndLocal = () => placementEndBeats() - patternOffsetBeats();
  const canvasWidth = createMemo(() => viewSpanBeats() * pxPerBeat());
  const canvasHeight = createMemo(() => rowCount() * NOTE_HEIGHT);
  const clipEndX = () => Math.min(
    canvasWidth() - 1,
    xForBeat(viewOriginBeats(), pxPerBeat(), openedBlock()?.end ?? viewEndBeats()),
  );
  const hitWindow = createMemo<PatternHitWindow | null>(() => {
    const pattern = selPattern();
    if (!pattern) return null;
    const block = openedBlock();
    return {
      start: patternOffsetBeats(),
      end: placementEndBeats(),
      offset: block?.sourceOffset ?? 0,
      loop: block?.loop ?? 0,
      swing: pattern.swing ?? store.project.properties.swing,
    };
  });
  // Draw each source hit once so editing never targets a repeated playback
  // copy. The same playback decision drives the notice, marker and lane bar.
  const authoredHits = createMemo<DisplayDrumHit[]>(() => {
    const pattern = activePattern();
    if (!pattern) return [];
    const window = hitWindow();
    const kit = openKit();
    return pattern.hits.map((hit: DisplayHitEvent, index) => ({
      beat: hitStartQuarters(hit),
      velocity: hitVelocity(hit),
      velocitySource: hitVelocitySource(hit),
      dynamics: hit.dynamics ?? null,
      label: hit.drum,
      note: hitNote(hit, kit),
      hitIndex: hit.__hitIndex ?? index,
      patternName: hit.__patternName ?? selPattern()?.name ?? null,
      outOfRange: window !== null && patternHitOnsets(hit, window).next().done === true,
    }));
  });
  const hitBlocks = createMemo(() => authoredHits().filter((hit) => {
    const x = xForLocalBeat(hit.beat);
    return x >= 0 && x < canvasWidth();
  }));
  /** Shortening or slipping a placement never discards the authored hits. */
  const outOfRange = createMemo(() => {
    if (!selPattern()) return { notes: 0, hits: 0 };
    const pattern = activePattern();
    const end = placementEndLocal();
    return {
      notes: pattern?.events.filter(
        (event) =>
          !event.modifiers?.some(([key]) => key === "rest") &&
          (event.start_beat ?? 0) >= end - 1e-9,
      ).length ?? 0,
      hits: authoredHits().filter((hit) => hit.outOfRange).length,
    };
  });
  // The lane reads the same block list the note layer draws, so a bar and its
  // note appear and disappear together.
  const laneBlocks = createMemo(() => {
    const ap = activePattern();
    return ap
      ? visibleNoteBlocks(
          ap,
          xForLocalBeat,
          canvasWidth(),
          presetParams(),
          selPattern() ? placementEndLocal() : undefined,
        )
      : [];
  });
  /** The velocity lane is an editing surface for the notes that exist; an empty
   * pattern has nothing to show there. */
  const showsVelocityLane = createMemo(() =>
    laneBlocks().length > 0 || hitBlocks().length > 0);

  /** The articulation lane exists only for notes that name an articulation. */
  const showsArticulationLane = createMemo(() =>
    laneBlocks().some((block) => block.articulation !== null));

  /** Notes the gutter box writes to; it never acts on an unselected note. */
  const selectedBlocks = createMemo(() =>
    laneBlocks().filter((block) => selectedNotes().has(`${block.phraseIndex}:${block.noteIndex}`)),
  );
  // Score marks the strip draws. They come from the same display pattern the
  // notes do, so a wedge can never outlive the phrase that carries it.
  const scoreHairpins = createMemo(() => activePattern()?.hairpins ?? []);
  const scorePedals = createMemo(() => activePattern()?.pedalSpans ?? []);
  const hasScoreMarks = () => scoreHairpins().length > 0 || scorePedals().length > 0;
  /** The strip only takes room while the roll has a mark to draw. */
  const scoreStripHeight = () => (hasScoreMarks() ? SCORE_STRIP_HEIGHT : 0);
  /** Smallest editable step, used to keep a note start strictly inside the clip. */
  const minStepBeats = () => 1 / gridDiv();

  // ── Batch note tools ──
  // Each tool writes one command for the whole gesture, so one undo entry comes
  // back. The bar states a pending label while the call runs and a failure line
  // when it returns one; silence is not a state.
  const [toolPending, setToolPending] = createSignal<ToolName | null>(null);
  const [toolFailure, setToolFailure] = createSignal<string | null>(null);
  const [quantiseStrength, setQuantiseStrength] = createSignal(100);
  const [humaniseAmount, setHumaniseAmount] = createSignal(HUMANISE_AMOUNTS[0].key);
  const [legatoAmount, setLegatoAmount] = createSignal(LEGATO_AMOUNTS[0].key);

  /** Notes a tool acts on: the selection, else every visible note. */
  const toolTargets = () => {
    const selected = selectedBlocks();
    return selected.length > 0 ? selected : laneBlocks();
  };

  /** Where the tool writes; a roll with no track and no pattern has no scope. */
  const toolScope = (): NoteScope | null => {
    const sp = selPattern();
    if (sp) return { kind: "pattern", pattern_name: sp.name };
    const ti = trackIdx();
    return ti === null ? null : { kind: "track", track_index: ti };
  };

  /** Length of the block the roll shows: its authored range, else its content. */
  const containerSpanBeats = createMemo(() => {
    const block = openedBlock();
    if (block) return block.end - block.start;
    let end = 0;
    for (const event of activePattern()?.events ?? []) {
      end = Math.max(end, (event.start_beat ?? 0) + durationToBeats(event.duration));
    }
    return end;
  });

  /** Why a tool cannot act, or null while it can. */
  const toolRefusal = (tool: ToolName): string | null => {
    const count = toolTargets().length;
    if (tool === "legato") {
      if (count < 2) return i18n.t("pianoroll.legatoNeedsTwo");
    } else if (count === 0) {
      return i18n.t(
        tool === "quantise" ? "pianoroll.quantiseNoSelection" : "pianoroll.humaniseNoSelection",
      );
    }
    if (tool === "quantise") {
      if (quantiseStrength() === 0) return i18n.t("pianoroll.quantiseZeroStrength");
      // A grid coarser than the whole block would move every note to one place.
      if (containerSpanBeats() < minStepBeats()) return i18n.t("pianoroll.gridTooCoarse");
    }
    return null;
  };

  /** The tool's own label: a pending verb, or the refusal the button states. */
  const toolLabel = (tool: ToolName): string => {
    if (toolPending() === tool) return i18n.t(TOOL_PENDING[tool]);
    if (tool === "quantise" && quantiseStrength() === 0) {
      return i18n.t("pianoroll.quantiseZeroStrength");
    }
    return i18n.t(TOOL_LABELS[tool]);
  };

  /** Hover text: the refusal while disabled, else what the tool does and on what. */
  const toolTitle = (tool: ToolName): string =>
    toolRefusal(tool) ?? i18n.t(TOOL_HINTS[tool]);

  const runTool = (
    tool: ToolName,
    write: (scope: NoteScope, notes: NoteRef[]) => Promise<string | null>,
  ) => {
    const scope = toolScope();
    const targets = toolTargets();
    if (!scope || targets.length === 0 || toolPending() !== null) return;
    setToolFailure(null);
    setToolPending(tool);
    void write(scope, targets.map((block) => ({
      phrase_index: block.phraseIndex,
      note_index: block.noteIndex,
    }))).then((message) => {
      setToolPending(null);
      if (message) {
        setToolFailure(i18n.t(TOOL_FAILED[tool], { message }));
        return;
      }
      // A tool moves the notes it acted on, so their identities no longer name
      // them; the selection is dropped rather than guessed at.
      setSelection([]);
    });
  };

  const handleQuantise = () =>
    runTool("quantise", (scope, notes) =>
      store.quantiseNotes(scope, notes, minStepBeats(), quantiseStrength()));

  const handleHumanise = () => {
    const amount =
      HUMANISE_AMOUNTS.find((option) => option.key === humaniseAmount()) ?? HUMANISE_AMOUNTS[0];
    const seed = nextHumaniseSeed();
    runTool("humanise", (scope, notes) =>
      store.humaniseNotes(scope, notes, amount.timingTicks, amount.velocitySpread, seed));
  };

  const handleLegato = () => {
    const amount = LEGATO_AMOUNTS.find((option) => option.key === legatoAmount()) ?? LEGATO_AMOUNTS[0];
    const amountBeats = amount.steps === null ? null : amount.steps * minStepBeats();
    runTool("legato", (scope, notes) => store.legatoNotes(scope, notes, amountBeats));
  };

  // ── Velocity lane tools ──
  // They act on the selection only: a ramp over every note in the roll is not a
  // gesture anyone asked for, and the lane already says what a selection is.
  const [velocityToolPending, setVelocityToolPending] = createSignal<VelocityTool | null>(null);
  const [velocityToolFailure, setVelocityToolFailure] = createSignal<string | null>(null);
  const [scalePercent, setScalePercent] = createSignal(SCALE_PERCENTS[0]);
  const [compressTarget, setCompressTarget] = createSignal(COMPRESS_TARGETS[1]);

  /** Why a velocity gesture cannot run, or null while it can. */
  const velocityRefusal = (tool: VelocityTool): string | null => {
    const count = selectedBlocks().length;
    if (tool === "ramp" && count < 2) return i18n.t("pianoroll.rampNeedsTwo");
    if (tool !== "ramp" && count === 0) {
      return i18n.t(tool === "scale" ? "pianoroll.scaleNeedsOne" : "pianoroll.compressNeedsOne");
    }
    return null;
  };

  /** The tool's own label: a pending verb, or its name. */
  const velocityToolLabel = (tool: VelocityTool): string => {
    if (velocityToolPending() === tool) return i18n.t(VELOCITY_TOOL_PENDING[tool]);
    return i18n.t(VELOCITY_TOOL_LABELS[tool]);
  };

  /** Hover text: the refusal while disabled, else what the tool does. */
  const velocityToolTitle = (tool: VelocityTool): string =>
    velocityRefusal(tool) ?? i18n.t(VELOCITY_TOOL_HINTS[tool]);

  const runVelocityTool = (
    tool: VelocityTool,
    write: (scope: NoteScope, notes: NoteRef[]) => Promise<string | null>,
  ) => {
    const scope = toolScope();
    const targets = selectedBlocks();
    if (!scope || targets.length === 0 || velocityToolPending() !== null) return;
    setVelocityToolFailure(null);
    setVelocityToolPending(tool);
    void write(scope, targets.map((block) => ({
      phrase_index: block.phraseIndex,
      note_index: block.noteIndex,
    }))).then((message) => {
      setVelocityToolPending(null);
      if (message) {
        setVelocityToolFailure(i18n.t(VELOCITY_TOOL_FAILED[tool], { message }));
      }
    });
  };

  const handleRampVelocity = () =>
    runVelocityTool("ramp", (scope, notes) => store.rampNoteVelocity(scope, notes));

  const handleScaleVelocity = () =>
    runVelocityTool("scale", (scope, notes) =>
      store.scaleNoteVelocity(scope, notes, scalePercent()));

  const handleCompressVelocity = () =>
    runVelocityTool("compress", (scope, notes) =>
      store.compressNoteVelocity(scope, notes, compressTarget()));

  // The velocity and timing tools keep their state (grid, strength, amount) in
  // this view, so their commands exist only while the roll is mounted.
  onMount(() => {
    const timing = (tool: ToolName) => ({
      enabled: () => toolRefusal(tool) === null,
      disabledReason: () => toolRefusal(tool),
    });
    const velocity = (tool: VelocityTool) => ({
      enabled: () => velocityRefusal(tool) === null,
      disabledReason: () => velocityRefusal(tool),
    });
    const unregister = [
      registerCommand("pattern.quantise", handleQuantise, timing("quantise")),
      registerCommand("pattern.humanise", handleHumanise, timing("humanise")),
      registerCommand("pattern.legato", handleLegato, timing("legato")),
      registerCommand("pattern.ramp", handleRampVelocity, velocity("ramp")),
      registerCommand("pattern.scale", handleScaleVelocity, velocity("scale")),
      registerCommand("pattern.compress", handleCompressVelocity, velocity("compress")),
    ];
    onCleanup(() => {
      for (const off of unregister) off();
    });
  });

  const clampViewBeat = (beat: number) =>
    Math.min(
      Math.max(beat, viewOriginBeats()),
      Math.max(viewOriginBeats(), viewEndBeats() - minStepBeats()),
    );
  /** Snap inside the playable placement, even when its edge is off grid. */
  const snapPlacementBeat = (beat: number) => {
    const snapped = snapQuarterToGrid(beat, gridDiv());
    const step = minStepBeats();
    const origin = viewOriginBeats();
    const end = placementEndBeats();
    if (activePattern()?.instrument.category === "drums" && selPattern()) {
      const first = Math.ceil((origin - 1e-9) / step) * step;
      const last = (Math.ceil((end - 1e-9) / step) - 1) * step;
      return last < first ? origin : Math.min(last, Math.max(first, snapped));
    }
    return Math.min(Math.max(snapped, origin), Math.max(origin, end - step));
  };
  const maxNoteStartLocal = () =>
    Math.max(originLocalBeat(), placementEndLocal() - minStepBeats());

  // Identity of the content currently shown in the roll. Re-centering only
  // happens when this changes (open / new pattern or clip), never on renders
  // caused by editing, zooming, or the user's own scrolling.
  const viewKey = createMemo(() => {
    const sel = store.editorSelection();
    const start = sel && sel.trackIndex === trackIdx() ? sel.startBeat : "";
    return `${trackIdx() ?? -1}:${selPattern()?.name ?? ""}:${start}`;
  });

  // A selection belongs to the view it was made in: another track, pattern or
  // clip holds other notes, so the old identities would name nothing there.
  createEffect(() => {
    viewKey();
    setSelection([]);
    setSelectedHit(null);
  });

  // Center C4 on mount and when the selected pattern/clip changes.
  createEffect(() => {
    viewKey();
    const el = gridRef;
    if (!el) return;
    let observing = true;
    let settleTimer: ReturnType<typeof setTimeout> | undefined;
    let resizeObserver: ResizeObserver | undefined;
    const apply = () => {
      // The drum lane's rows are its whole axis, so it opens at the top.
      if (!laneRows()) el.scrollTop = scrollTopToCenterMidi(el.clientHeight);
      // The origin is x = 0, so the roll opens at the left edge.
      queueMicrotask(() => {
        if (observing) el.scrollLeft = 0;
      });
    };
    apply();

    const stop = () => {
      observing = false;
      if (settleTimer !== undefined) clearTimeout(settleTimer);
      resizeObserver?.disconnect();
      el.removeEventListener("wheel", stop);
      el.removeEventListener("pointerdown", stop);
      el.removeEventListener("keydown", stop);
    };

    if (typeof ResizeObserver === "function") {
      // The panel animates open, so the first measurement is often mid-
      // transition. Follow the size until it settles, then stop observing so
      // the user's own scrolling is never overridden. Any user input stops it
      // immediately.
      resizeObserver = new ResizeObserver(() => {
        if (!observing) return;
        apply();
        if (settleTimer !== undefined) clearTimeout(settleTimer);
        settleTimer = setTimeout(stop, 120);
      });
      resizeObserver.observe(el);
      settleTimer = setTimeout(stop, 400);
      el.addEventListener("wheel", stop, { passive: true });
      el.addEventListener("pointerdown", stop);
      el.addEventListener("keydown", stop);
    } else if (el.clientHeight <= PIANO_ROLL_RULER_HEIGHT && typeof requestAnimationFrame === "function") {
      // No ResizeObserver: at least retry once after layout settles.
      const raf = requestAnimationFrame(apply);
      onCleanup(() => cancelAnimationFrame(raf));
    }
    onCleanup(stop);
  });

  // Zooming keeps the beat at the left edge in place instead of drifting.
  let previousPxPerBeat = zoom();
  createEffect(() => {
    const ppb = pxPerBeat();
    const el = gridRef;
    if (!el || ppb === previousPxPerBeat) return;
    const anchorBeat = el.scrollLeft / previousPxPerBeat;
    previousPxPerBeat = ppb;
    el.scrollLeft = anchorBeat * ppb;
  });

  // Local drag override for cursor (same pattern as Timeline)
  const barMarks = createMemo(() => {
    const clock = createProjectClock(store.project.properties);
    const origin = viewOriginBeats();
    const span = viewSpanBeats();
    const first = clock.barBeatAt(origin).bar;
    const last = clock.barBeatAt(origin + span).bar;
    return Array.from({length:last-first+1}, (_,i) => {
      const bar = first+i;
      return {bar,quarter:clock.quarterAt({bar,beat:1,subdivision:0})-origin};
    }).filter(mark => mark.quarter >= 0 && mark.quarter < span);
  });
  // Bars are strong, beats faint; snap subdivisions are drawn only when they
  // are wide enough to read (>= 16px), so a 1/16 grid at normal zoom doesn't
  // turn the canvas into a wall of lines. Snap still follows gridDiv().
  const gridBackground = createMemo(() => {
    const ppb = pxPerBeat();
    const subW = ppb/gridDiv();
    const layers = [
      ...barMarks().map(mark => `linear-gradient(to right, transparent ${mark.quarter*ppb}px, var(--grid-bar) ${mark.quarter*ppb}px, var(--grid-bar) ${mark.quarter*ppb+1}px, transparent ${mark.quarter*ppb+1}px)`),
      `repeating-linear-gradient(to right, var(--grid-beat) 0, var(--grid-beat) 1px, transparent 1px, transparent ${ppb}px)`,
    ];
    if (gridDiv() > 1 && subW >= 16) {
      layers.push(`repeating-linear-gradient(to right, color-mix(in srgb, var(--grid-beat) 30%, transparent) 0, color-mix(in srgb, var(--grid-beat) 30%, transparent) 1px, transparent 1px, transparent ${subW}px)`);
    }
    return layers.join(", ");
  });

  /** A private preview of this pattern, in beats from the previewed content's
   * start. A pattern preview is local; a clip preview carries its position. */
  const auditionLocalX = createMemo(() => {
    const local = store.auditionLocalBeats();
    if (local === null) return null;
    const start = store.auditionStartBeats();
    const localBeat = start === null ? local : start + local - patternOffsetBeats();
    if (localBeat < originLocalBeat() || localBeat - originLocalBeat() >= viewSpanBeats()) return null;
    return Math.min(Math.max(0, xForLocalBeat(localBeat)), canvasWidth());
  });

  // Cursor position — from transport signal, pattern-relative, or scrub override
  const cursorX = createMemo(() => {
    const db = store.scrubPosition();
    if (db !== null) {
      // Scrub positions from the timeline may lie outside this clip.
      if (db < viewOriginBeats() || db >= viewEndBeats()) return -1;
      return Math.min(Math.max(0, xForLocalBeat(db - patternOffsetBeats())), canvasWidth());
    }
    const audition = auditionLocalX();
    if (audition !== null) return audition;
    const t = store.transport();
    // Hide only if never played
    if (!store.cursorActive() && !t.playing) return -1;
    const localPos = t.position_beats - patternOffsetBeats();
    if (localPos < originLocalBeat()) return -1; // cursor before the clip origin
    // Hide cursor if it's past the visible clip area
    if (localPos - originLocalBeat() >= viewSpanBeats()) return -1;
    return xForLocalBeat(localPos);
  });

  // Auto-scroll to follow the cursor during playback and while scrubbing.
  createEffect(() => {
    const cx = cursorX();
    const t = store.transport();
    const scrubbing = store.scrubPosition() !== null;
    if (!scrubbing && !t.playing) return;
    if (cx < 0 || !gridRef) return;
    // Beat 0 sits after the sticky piano-key column in content coordinates.
    const contentX = keysWidth() + cx;
    const viewLeft = gridRef.scrollLeft;
    const viewRight = viewLeft + gridRef.clientWidth;
    if (contentX > viewRight - 60) {
      gridRef.scrollLeft = contentX - gridRef.clientWidth * 0.3;
    } else if (contentX < viewLeft + keysWidth() + 20) {
      gridRef.scrollLeft = Math.max(0, contentX - keysWidth() - 40);
    }
  });

  // Snap guide position — vertical alignment line during drag
  const snapGuideX = createMemo(() => {
    const ds = dragState();
    if (!ds) return -1;
    if (ds.mode === "move") {
      return xForLocalBeat(ds.originalStartBeat) + dragOffsetX();
    }
    return xForLocalBeat(ds.originalStartBeat + ds.originalDurationBeats) + dragOffsetX();
  });

  const handleWheel = (e: WheelEvent) => {
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      setZoom(Math.round(Math.max(10, Math.min(200, zoom() * delta))));
    }
  };

  // Absolute beat under the pointer. x = 0 is the clip origin and x = clip
  // width is its end, so pointers cannot leave the clip's own time range.
  const beatFromClientX = (clientX: number): number => {
    const el = canvasRef;
    if (!el) return viewOriginBeats();
    const rect = el.getBoundingClientRect();
    return clampViewBeat(beatAtX(viewOriginBeats(), pxPerBeat(), clientX - rect.left));
  };
  const localBeatAtClientX = (clientX: number) => beatFromClientX(clientX) - patternOffsetBeats();
  /** A pointer position as a beat, without clamping to the current view: a
   * handle that changes the view's end must be able to reach past it. */
  const rawBeatAtClientX = (clientX: number) => {
    const el = canvasRef;
    if (!el) return viewOriginBeats();
    return beatAtX(viewOriginBeats(), pxPerBeat(), clientX - el.getBoundingClientRect().left);
  };

  // Drag previews the cursor, then commits a single absolute seek on release.
  // The gesture runs muted, so it has to end even when no release arrives: the
  // shared drag helper also ends it on a cancelled pointer, a lost capture, a
  // blur, Escape and unmount. A gesture that a newer one replaced must not
  // unmute over it, so only the newest settle touches the mute.
  let stopRulerScrub: (() => void) | null = null;
  let rulerScrubEpoch = 0;
  onCleanup(() => stopRulerScrub?.());
  // A panel that closes mid-erase must end the gesture it opened.
  onCleanup(() => {
    stopErase?.();
    stopErase = null;
  });
  const handleRulerPointerDown = (e: PointerEvent) => {
    if (isSecondaryClick(e)) return;
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

  // Grid mouse move — track draw preview
  const handleGridMouseMove = (e: MouseEvent) => {
    if (drawing) return;
    if (tool() !== "draw" || dragState()) {
      if (drawPreview()) setDrawPreview(null);
      return;
    }
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const beat = snapPlacementBeat(beatFromClientX(e.clientX)) - patternOffsetBeats();
    const midi = rowNoteAt(e.clientY - rect.top);
    if (midi === null) {
      setDrawPreview(null);
      return;
    }
    const ap = activePattern();
    if (!ap) { setDrawPreview(null); return; }
    const { name, accidental, octave } = midiToNoteParts(midi);
    const durBeats = 1 / gridDiv();
    const accStr = accidental === "Sharp" ? "#" : accidental === "Flat" ? "b" : "";
    const row = laneRows()?.find((candidate) => candidate.note === midi);
    setDrawPreview({
      midi,
      startBeat: beat,
      durBeats,
      label: row ? row.label : `${name}${accStr}${octave}`,
    });
  };

  const handleGridMouseLeave = () => {
    setDrawPreview(null);
  };

  const handleGridMouseDown = (e: MouseEvent) => {
    if (tool() !== "select" || isSecondaryClick(e)) return;
    const target = e.target as HTMLElement;
    if (target.closest(".pianoroll-note") || target.closest(".pianoroll-drum-hit")) return;
    e.preventDefault();
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const startX = e.clientX - rect.left;
    const startY = e.clientY - rect.top;
    setMarquee({ startX, startY, endX: startX, endY: startY });

    const onMove = (ev: MouseEvent) => {
      const endX = ev.clientX - rect.left;
      const endY = ev.clientY - rect.top;
      setMarquee({ startX, startY, endX, endY });
    };

    const onUp = (ev: MouseEvent) => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      const m = marquee();
      setMarquee(null);
      if (!m) return;
      const dx = Math.abs(m.endX - m.startX);
      const dy = Math.abs(m.endY - m.startY);
      if (dx < 4 && dy < 4) {
        if (!ev.shiftKey) setSelection([]);
        return;
      }
      const ap = activePattern();
      if (!ap) return;
      const minX = Math.min(m.startX, m.endX);
      const maxX = Math.max(m.startX, m.endX);
      const minY = Math.min(m.startY, m.endY);
      const maxY = Math.max(m.startY, m.endY);
      const ppb = pxPerBeat();
      const selected = ev.shiftKey ? [...selection()] : [];
      const chosen = new Set(selected.map(identityKey));
      for (let ni = 0; ni < ap.events.length; ni++) {
        const ev2 = ap.events[ni];
        const noteTop = rowTopOf(pitchToMidi(ev2.pitch));
        if (noteTop === null) continue;
        const noteLeft = xForLocalBeat(ev2.start_beat ?? 0);
        const noteRight = noteLeft + durationToBeats(ev2.duration) * ppb;
        const noteBottom = noteTop + NOTE_HEIGHT;
        if (noteRight > minX && noteLeft < maxX && noteBottom > minY && noteTop < maxY) {
          const identity = eventIdentity(ev2, ni);
          const key = identityKey(identity);
          if (!chosen.has(key)) {
            chosen.add(key);
            selected.push(identity);
          }
        }
      }
      setSelection(selected);
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  /** Insert a note (or drum hit) at an absolute beat; the grid menu reuses this. */
  const addNoteAt = (midi: number, absBeat: number, durationBeats: number) => {
    const ap = activePattern();
    if (!ap) return;
    if (!laneRows() && (midi < MIN_MIDI || midi >= MAX_MIDI)) return;

    const drumPattern = ap.instrument.category === "drums" && selPattern();
    const startAbs = snapPlacementBeat(absBeat);
    const beat = startAbs - patternOffsetBeats();
    // A new note may start inside the clip, never past its end.
    const duration = Math.min(durationBeats, Math.max(minStepBeats(), viewEndBeats() - startAbs));

    const { name, accidental, octave } = midiToNoteParts(midi);
    const durName = gridDiv() >= 4 ? "16th" : gridDiv() >= 2 ? "8th" : "q";
    const accStr = accidental === "Sharp" ? "#" : accidental === "Flat" ? "b" : "";

    if (drumPattern) {
      const placement = hitPlacement(beat);
      const { drum, midiNote } = drumForNote(midi, openKit());
      store.addPatternHit(
        ap.name,
        drum,
        placement.beat,
        placement.subdivision,
        undefined,
        {
          gridOffsetBeats: placement.offsetBeats === 0 ? null : placement.offsetBeats,
          midiNote,
        },
      );
      return;
    }

    if (selPattern()) {
      store.addPatternNote(ap.name, name, accStr, octave, durName, Math.max(0, beat), duration);
      return;
    }

    const ti = trackIdx();
    if (ti === null) return;
    const t = track();
    if (!t || t.phrases.length === 0) {
      store.setError("Track has no phrases. Open the DSL editor and add: @phrase melody [1:1:0 - 8:4:0]");
      return;
    }

    const clock = createProjectClock(store.project.properties);
    const barBeatToBeatsFn = (bb: { bar: number; beat: number; subdivision: number }) => clock.quarterAt(bb);

    let phraseIdx = -1;
    for (let pi = 0; pi < t.phrases.length; pi++) {
      const pr = t.phrases[pi].time_range;
      if (!pr) { phraseIdx = pi; break; }
      const startBeat = barBeatToBeatsFn(pr.start);
      const endBeat = barBeatToBeatsFn(pr.end);
      if (beat >= startBeat && beat <= endBeat) { phraseIdx = pi; break; }
    }
    if (phraseIdx < 0) {
      let bestDist = Infinity;
      for (let pi = 0; pi < t.phrases.length; pi++) {
        const pr = t.phrases[pi].time_range;
        if (!pr) continue;
        const s = barBeatToBeatsFn(pr.start);
        const e = barBeatToBeatsFn(pr.end);
        const dist = beat < s ? s - beat : beat > e ? beat - e : 0;
        if (dist < bestDist) { bestDist = dist; phraseIdx = pi; }
      }
      if (phraseIdx < 0) {
        store.setError("No phrase exists. Click within an existing phrase range or extend one in the DSL editor.");
        return;
      }
      const end = clock.barBeatAt(beat);
      store.extendPhraseRange(ti, phraseIdx, end.bar, end.beat, end.subdivision);
    }
    const phrase = t.phrases[phraseIdx];
    const pr = phrase.time_range;
    const phraseStartBeat = pr ? barBeatToBeatsFn(pr.start) : 0;
    const noteStartBeat = beat - phraseStartBeat;
    store.addNote(ti, phraseIdx, name, accidental, octave, durName, "mf", Math.max(0, noteStartBeat), duration);
  };

  /** Live end of the block while its handle is dragged. */
  const [resizeEndPreview, setResizeEndPreview] = createSignal<number | null>(null);

  /**
   * Drag the block's end: a clip resizes, a bare pattern grows or shrinks by
   * whole bars. The roll is where the writing happens, so its end is a handle.
   */
  const beginResizeEnd = (event: PointerEvent) => {
    const block = openedBlock();
    const trackIndex = trackIdx();
    const pattern = selPattern();
    if (trackIndex === null) return;
    event.preventDefault();
    event.stopPropagation();
    const clock = createProjectClock(store.project.properties);
    const start = block?.start ?? patternOffsetBeats();
    const minEnd = start + minStepBeats();
    let lastX = event.clientX;
    let moved = false;
    const endAt = (x: number) => Math.max(minEnd, snapQuarterToGrid(rawBeatAtClientX(x), gridDiv()));
    const stop = beginPointerDrag({
      element: event.currentTarget as HTMLElement,
      pointerId: event.pointerId,
      onMove: (x) => {
        if (Math.abs(x - event.clientX) > 3) moved = true;
        if (!moved) return;
        lastX = x;
        setResizeEndPreview(endAt(x));
      },
      onEnd: () => {
        setResizeEndPreview(null);
        if (!moved) return;
        const end = endAt(lastX);
        if (block) {
          const clipIndex = (track()?.clips ?? []).findIndex((clip) =>
            Math.abs(clock.quarterAt(clip.time_range.start) - block.start) < 1e-9);
          if (clipIndex < 0) return;
          const position = clock.barBeatAt(end);
          void withUndoGesture("project", () => resizeClip(patternKey(trackIndex, clipIndex), "end", position));
          return;
        }
        if (!pattern) return;
        // A bare pattern only grows by whole bars.
        const bars = Math.max(1, Math.ceil((end - patternOffsetBeats()) / 4));
        void withUndoGesture("project", () => store.setPatternBars(pattern.name, bars));
      },
    });
    void stop;
  };

  /** Bars the open pattern owns; zero when the roll shows a track. */
  const patternBars = () => {
    const p = selPattern();
    return p ? Math.max(1, Math.round(patternLengthQuarters(p) / 4)) : 0;
  };

  /**
   * Add or remove one bar. The roll's own drawing bound is the pattern's
   * length, so this is how content grows past the current end; a clip that
   * ended exactly at the old end grows with it.
   */
  const changePatternBars = (delta: number) => {
    const p = selPattern();
    if (!p) return;
    const next = Math.max(1, patternBars() + delta);
    if (next === patternBars()) return;
    const block = openedBlock();
    const showingClip = block
      ? (track()?.clips ?? []).findIndex((clip) =>
          Math.abs(createProjectClock(store.project.properties).quarterAt(clip.time_range.start) - block.start) < 1e-9)
      : -1;
    const follows = block !== null && Math.abs(block.end - (block.start + patternBars() * 4)) < 1e-6;
    void withUndoGesture("project", async () => {
      await store.setPatternBars(p.name, next);
      if (follows && showingClip >= 0) {
        const end = block.start + next * 4;
        const beatsPerBar = 4;
        const endBar = Math.floor(end / beatsPerBar) + 1;
        const rest = end - (endBar - 1) * beatsPerBar;
        await store.resizeClip(trackIdx()!, showingClip, endBar, Math.floor(rest) + 1, 0);
      }
    });
  };

  const handleGridClick = (e: MouseEvent, durationBeats = 1 / gridDiv()) => {
    if (tool() === "select") return;
    if (tool() !== "draw") return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const midi = rowNoteAt(e.clientY - rect.top);
    if (midi === null) return;
    addNoteAt(midi, beatFromClientX(e.clientX), durationBeats);
  };

  let cancelDraw: (() => void) | undefined;
  const cancelGesture = () => {
    cancelDraw?.();
    setDragState(null);
    setDragOffsetX(0);
    setDragPitch(0);
    window.removeEventListener("mousemove", onDocMouseMove);
    window.removeEventListener("mouseup", onDocMouseUp);
  };
  const cancelKey = (e: KeyboardEvent) => { if(e.key === "Escape") cancelGesture(); };
  window.addEventListener("blur", cancelGesture);
  window.addEventListener("keydown", cancelKey);
  const startDraw = (e: MouseEvent) => {
    e.preventDefault();
    const target = e.currentTarget as HTMLElement;
    const rect = target.getBoundingClientRect();
    const startAbs = snapPlacementBeat(beatFromClientX(e.clientX));
    const start = startAbs - patternOffsetBeats();
    const maxLength = () => Math.max(minStepBeats(), viewEndBeats() - startAbs);
    const midi = rowNoteAt(e.clientY - rect.top);
    if (midi === null) return;
    const label = () => {
      const rows = laneRows();
      const row = rows?.find((candidate) => candidate.note === midi);
      if (row) return row.label;
      const parts = midiToNoteParts(midi);
      return pitchToString({ note: parts.name, accidental: parts.accidental, octave: parts.octave });
    };
    const clientX = e.clientX, clientY = e.clientY;
    let length = minStepBeats();
    drawing = true;
    const move = (ev: MouseEvent) => {
      length = Math.min(
        maxLength(),
        Math.max(minStepBeats(), snapQuarterToGrid(localBeatAtClientX(ev.clientX) - start, gridDiv())),
      );
      setDrawPreview({midi, startBeat:start, durBeats:length, label:label()});
    };
    const clean = () => {
      drawing = false;
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      window.removeEventListener("blur", clean);
      setDrawPreview(null);
      cancelDraw = undefined;
    };
    const up = (ev: MouseEvent) => {
      if (ev.button !== 0) return;
      move(ev); clean();
      handleGridClick({currentTarget:target, clientX, clientY} as unknown as MouseEvent, length);
    };
    cancelDraw = clean;
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    window.addEventListener("blur", clean);
  };
  onCleanup(() => {
    cancelGesture();
    window.removeEventListener("blur", cancelGesture);
    window.removeEventListener("keydown", cancelKey);
  });

  // ── Drag handlers (document-level for smooth mouse tracking) ──

  const onDocMouseMove = (e: MouseEvent) => {
    const ds = dragState();
    if (!ds) return;
    const ppb = pxPerBeat();
    const pixelDelta = e.clientX - ds.dragStartX;
    const beatDelta = pixelDelta / ppb;
    const snappedDelta = snapQuarterToGrid(beatDelta, gridDiv());
    const minStartDelta = Math.max(originLocalBeat(), ds.phraseOffsetBeats) - ds.originalStartBeat;
    const maxStartDelta = maxNoteStartLocal() - ds.originalStartBeat;
    const endLocal = viewEndBeats() - patternOffsetBeats();
    const maxDurationDelta = endLocal - ds.originalStartBeat - ds.originalDurationBeats;
    const delta = ds.mode === "move"
      ? Math.min(maxStartDelta, Math.max(minStartDelta, snappedDelta))
      : Math.min(maxDurationDelta, Math.max(minStepBeats() - ds.originalDurationBeats, snappedDelta));
    setDragOffsetX(delta * ppb);
    if (ds.mode !== "move") {
      setDragPitch(0);
      return;
    }
    const rows = laneRows();
    const wanted = Math.round((ds.dragStartY - e.clientY) / NOTE_HEIGHT);
    if (rows) {
      // A drum lane moves between rows, so the delta is bounded by the lane.
      const from = Math.max(0, rows.findIndex((row) => row.note === ds.noteMidi));
      setDragPitch(Math.max(-from, Math.min(rows.length - 1 - from, wanted)));
      return;
    }
    setDragPitch(Math.max(MIN_MIDI - ds.noteMidi, Math.min(MAX_MIDI - 1 - ds.noteMidi, wanted)));
  };

  const onDocMouseUp = (e: MouseEvent) => {
    if (e.button !== 0) return;
    const ds = dragState();
    if (!ds) return;
    // Release coordinates are authoritative even without a final mousemove.
    onDocMouseMove(e);
    const ppb = pxPerBeat();
    const movedSignificantly = dragOffsetX() !== 0 || dragPitch() !== 0;

    if (movedSignificantly) {
      const beatDelta = dragOffsetX() / ppb;
      // The dragged note keeps its selection; its identity follows the move so
      // a re-sorted list cannot hand the highlight to a neighbour.
      const before: NoteIdentity = {
        phraseIndex: ds.phraseIndex,
        midi: ds.noteMidi,
        startBeat: ds.originalStartBeat - ds.phraseOffsetBeats,
      };
      // A drum lane drags between rows; a pitched grid drags by semitones.
      const rows = laneRows();
      const rowIndex = rows
        ? Math.max(0, rows.findIndex((row) => row.note === ds.noteMidi)) + dragPitch()
        : 0;
      const dragged = {
        midi: rows
          ? rows[Math.max(0, Math.min(rows.length - 1, rowIndex))].note
          : ds.noteMidi + dragPitch(),
      };

      void withUndoGesture("project", async () => {
        if (ds.mode === "move") {
          const newStartBeat = Math.min(
            maxNoteStartLocal(),
            Math.max(
              Math.max(originLocalBeat(), ds.phraseOffsetBeats),
              ds.originalStartBeat + beatDelta,
            ),
          );
          if (selPattern()) {
            const p = selPattern()!;
            const pitch = midiToNoteParts(dragged.midi);
            await store.movePatternNote(p.name, ds.noteIndex, newStartBeat, {note: pitch.name, accidental: pitch.accidental, octave: pitch.octave});
            retargetSelection(before, { ...before, ...dragged, startBeat: newStartBeat });
          } else {
            const ti = trackIdx();
            if (ti !== null) {
              const localStartBeat = Math.max(0, snapQuarterToGrid(newStartBeat - ds.phraseOffsetBeats, gridDiv()));
              const pitch = midiToNoteParts(dragged.midi);
              await store.moveNote(ti, ds.phraseIndex, ds.noteIndex, localStartBeat, {note: pitch.name, accidental: pitch.accidental, octave: pitch.octave});
              retargetSelection(before, { ...before, ...dragged, startBeat: localStartBeat });
            }
          }
        } else if (ds.mode === "resize") {
          // A note may ring out, but its edited body cannot pass the clip end.
          const maxDuration = Math.max(
            minStepBeats(),
            viewEndBeats() - patternOffsetBeats() - ds.originalStartBeat,
          );
          const snapped = Math.min(
            maxDuration,
            Math.max(minStepBeats(), snapQuarterToGrid(ds.originalDurationBeats + beatDelta, gridDiv())),
          );
          const durName = beatsToDurationName(snapped);
          const dots = beatsToDots(snapped, durName);
          const triplet = beatsIsTriplet(snapped, durName);
          if (selPattern()) {
            const p = selPattern()!;
            await store.setPatternNoteDuration(p.name, ds.noteIndex, durName, dots, triplet, snapped);
          } else {
            const ti = trackIdx();
            if (ti !== null) {
              await store.setNoteDuration(ti, ds.phraseIndex, ds.noteIndex, durName, dots, triplet, snapped);
            }
          }
        }
      });
    }

    setDragState(null);
    setDragOffsetX(0);
    setDragPitch(0);
    window.removeEventListener("mousemove", onDocMouseMove);
    window.removeEventListener("mouseup", onDocMouseUp);
  };

  // Note bodies and edges are editable in both pencil and select modes.
  const onNoteDragStart = (e: MouseEvent, phraseIdx: number, noteIdx: number, startBeat: number, durationBeats: number, midi: number, phraseOffsetBeats = 0) => {
    e.stopPropagation();
    e.preventDefault();
    selectNote(
      { phraseIndex: phraseIdx, midi, startBeat: startBeat - phraseOffsetBeats },
      e.shiftKey,
    );
    setDragState({
      noteIndex: noteIdx, phraseIndex: phraseIdx, phraseOffsetBeats, mode: "move",
      originalStartBeat: startBeat, originalDurationBeats: durationBeats,
      dragStartX: e.clientX, dragStartY: e.clientY, noteMidi: midi, shiftKey: e.shiftKey,
    });
    setDragOffsetX(0);
    setDragPitch(0);
    window.addEventListener("mousemove", onDocMouseMove);
    window.addEventListener("mouseup", onDocMouseUp);
  };

  const onNoteResizeStart = (e: MouseEvent, phraseIdx: number, noteIdx: number, startBeat: number, durationBeats: number, midi: number, phraseOffsetBeats = 0) => {
    e.stopPropagation();
    e.preventDefault();
    const dragStartX = e.clientX;
    setDragState({
      noteIndex: noteIdx, phraseIndex: phraseIdx, phraseOffsetBeats, mode: "resize",
      originalStartBeat: startBeat, originalDurationBeats: durationBeats,
      dragStartX, dragStartY: e.clientY, noteMidi: midi, shiftKey: e.shiftKey,
    });
    setDragOffsetX(0);
    setDragPitch(0);
    window.addEventListener("mousemove", onDocMouseMove);
    window.addEventListener("mouseup", onDocMouseUp);
  };

  /** Move a selected note's identity with the note itself. */
  const retargetSelection = (before: NoteIdentity, after: NoteIdentity) => {
    const key = identityKey(before);
    setSelection((prev) => prev.map((entry) => (identityKey(entry) === key ? after : entry)));
  };

  const selectNote = (identity: NoteIdentity, shiftKey: boolean) => {
    const key = identityKey(identity);
    setSelectedHit(null);
    setSelection((prev) => {
      const next = shiftKey ? [...prev] : [];
      const at = next.findIndex((entry) => identityKey(entry) === key);
      if (at >= 0 && shiftKey) next.splice(at, 1);
      else if (at < 0) next.push(identity);
      return next;
    });
  };

  /** The hit the delete key acts on. A hit is one value, so it replaces a pick. */
  const selectHit = (patternName: string | null, hitIndex: number) => {
    if (!patternName) return;
    setSelection([]);
    setSelectedHit({ patternName, hitIndex });
  };

  const isHitSelected = (patternName: string | null, hitIndex: number): boolean => {
    const selected = selectedHit();
    return selected !== null
      && selected.hitIndex === hitIndex
      && selected.patternName === patternName;
  };

  // ── Right-click handling ──
  // A menu names the note, hit or position it acts on and never seeks or starts
  // a gesture. A right-click on a hit also picks it, so the delete key follows
  // the pointer.

  const noteAt = (phraseIdx: number, noteIdx: number): NoteEvent | null => {
    if (selPattern()) return selPattern()?.events[noteIdx] ?? null;
    const ti = trackIdx();
    if (ti === null) return null;
    return store.project.tracks[ti]?.phrases[phraseIdx]?.events[noteIdx] ?? null;
  };

  /** A note's start in the view's beat space: local in a pattern, absolute on a track. */
  const absoluteBeatOf = (phraseIdx: number, noteIdx: number): number => {
    const note = noteAt(phraseIdx, noteIdx);
    if (!note) return 0;
    if (selPattern()) return (note.start_beat ?? 0) + patternOffsetBeats();
    const phrase = track()?.phrases[phraseIdx];
    const clock = createProjectClock(store.project.properties);
    return (note.start_beat ?? 0) + (phrase?.time_range ? clock.quarterAt(phrase.time_range.start) : 0);
  };

  const phraseIndexAtBeat = (absBeat: number): number | null => {
    const t = track();
    if (!t) return null;
    const clock = createProjectClock(store.project.properties);
    for (let i = 0; i < t.phrases.length; i++) {
      const range = t.phrases[i].time_range;
      if (!range) return i;
      if (absBeat >= clock.quarterAt(range.start) && absBeat < clock.quarterAt(range.end)) return i;
    }
    return null;
  };

  /** The note a selection ref names in the document, not the display copy. */
  const eventAt = (ref: NoteRef): NoteEvent | null => {
    const p = selPattern();
    if (p) return p.events[ref.note_index] ?? null;
    const t = track();
    return t?.phrases[ref.phrase_index]?.events[ref.note_index] ?? null;
  };

  /** The local start a copied note's offset lands at, or null with no room. */
  const pasteStart = (phraseIdx: number, absBeat: number, offsetBeats: number): number | null => {
    if (selPattern()) {
      return Math.max(
        0,
        Math.min(viewSpanBeats() - minStepBeats(), absBeat + offsetBeats - patternOffsetBeats()),
      );
    }
    const phrase = track()?.phrases[phraseIdx];
    if (!phrase) return null;
    const clock = createProjectClock(store.project.properties);
    const phraseStart = phrase.time_range ? clock.quarterAt(phrase.time_range.start) : 0;
    const phraseEnd = phrase.time_range ? clock.quarterAt(phrase.time_range.end) - phraseStart : Infinity;
    return Math.max(0, Math.min(phraseEnd - minStepBeats(), absBeat + offsetBeats - phraseStart));
  };

  /**
   * Paste one copied note. The event payload is the whole note, so velocity,
   * dynamics, articulation, mute, grace and modifiers survive; only the start
   * here changes.
   */
  const pasteEntry = (entry: NoteClipboardEntry, phraseIdx: number, absBeat: number) => {
    const ap = activePattern();
    if (!ap) return;
    const start = pasteStart(phraseIdx, absBeat, entry.offsetBeats);
    if (start === null) return;
    const payload: NoteEvent = { ...entry.event, start_beat: start, explicit_position: true };
    const accidental = payload.pitch.accidental === "Sharp" ? "#" : payload.pitch.accidental === "Flat" ? "b" : "";
    const durationBeats = durationToBeats(payload.duration);
    if (selPattern()) {
      void store.addPatternNote(ap.name, payload.pitch.note, accidental, payload.pitch.octave, payload.duration.base, start, durationBeats, payload);
      return;
    }
    const ti = trackIdx();
    if (ti === null) return;
    void store.addNote(ti, phraseIdx, payload.pitch.note, accidental, payload.pitch.octave, payload.duration.base, payload.dynamics?.toLowerCase(), start, durationBeats, payload);
  };

  /**
   * Paste the clipboard at an absolute beat. One note keeps the single-note
   * command; a group is one batch command, so the paste is one undo entry.
   */
  const pasteEntries = (phraseIdx: number, absBeat: number) => {
    const entries = noteClipboard;
    if (!entries || entries.length === 0) return;
    if (entries.length === 1) {
      pasteEntry(entries[0], phraseIdx, absBeat);
      return;
    }
    const scope = toolScope();
    if (!scope) return;
    const inserts: NoteInsert[] = [];
    for (const entry of entries) {
      const start = pasteStart(phraseIdx, absBeat, entry.offsetBeats);
      if (start === null) return;
      inserts.push({
        phrase_index: scope.kind === "pattern" ? 0 : phraseIdx,
        note: { ...entry.event, start_beat: start, explicit_position: true },
      });
    }
    void store.addNotes(scope, inserts);
  };

  /** Copy the selection, keeping every field the AST stores. */
  const copySelectedNotes = () => {
    const live = liveSelection();
    if ("refusal" in live || live.refs.length === 0) return;
    const ap = activePattern();
    if (!ap) return;
    const picked: NoteClipboardEntry[] = [];
    for (const ref of live.refs) {
      const display = ap.events[ref.note_index];
      const source = eventAt(ref);
      if (!display || !source) return;
      picked.push({ offsetBeats: display.start_beat ?? 0, event: cloneNote(source) });
    }
    const first = Math.min(...picked.map((entry) => entry.offsetBeats));
    noteClipboard = picked.map((entry) => ({ ...entry, offsetBeats: entry.offsetBeats - first }));
  };

  const copyNoteAt = (phraseIdx: number, noteIdx: number) => {
    const note = noteAt(phraseIdx, noteIdx);
    if (!note) return;
    noteClipboard = [{ offsetBeats: 0, event: cloneNote(note) }];
  };

  const duplicateNoteAt = (phraseIdx: number, noteIdx: number) => {
    const note = noteAt(phraseIdx, noteIdx);
    if (!note) return;
    pasteEntry(
      { offsetBeats: 0, event: cloneNote(note) },
      phraseIdx,
      absoluteBeatOf(phraseIdx, noteIdx) + 1,
    );
  };

  const pasteAtPlayhead = (phraseIdx: number) => {
    if (!noteClipboard) return;
    const origin = viewOriginBeats();
    const playhead = Math.max(origin, Math.min(origin + viewSpanBeats(), store.transport().position_beats));
    pasteEntries(phraseIdx, playhead);
  };

  const removeNoteAt = (phraseIdx: number, noteIdx: number) => {
    if (selPattern()) {
      const p = selPattern();
      if (p) store.removePatternNote(p.name, noteIdx);
      return;
    }
    const ti = trackIdx();
    if (ti !== null) store.removeNote(ti, phraseIdx, noteIdx);
  };

  /**
   * Write an exact velocity; the store reports the failure message so the lane
   * can show it instead of going quiet.
   */
  const setVelocityAt = (phraseIdx: number, noteIdx: number, velocity: number): Promise<string | null> => {
    if (selPattern()) {
      const p = selPattern();
      return p ? store.setPatternNoteVelocity(p.name, noteIdx, velocity) : Promise.resolve(null);
    }
    const ti = trackIdx();
    return ti !== null ? store.setNoteVelocity(ti, phraseIdx, noteIdx, velocity) : Promise.resolve(null);
  };

  /**
   * Write one typed velocity across the whole selection. The backend resolves
   * every note before it writes, so the gesture is one undo entry and a refusal
   * leaves every note unchanged.
   */
  const setVelocitiesAt = (
    blocks: DisplayNoteBlock[],
    velocity: number,
  ): Promise<string | null> => {
    const scope = toolScope();
    if (!scope) return Promise.resolve(null);
    const live = liveSelection();
    if ("refusal" in live) return Promise.resolve(live.refusal);
    if (blocks.length === 0) return Promise.resolve(null);
    return store.setNoteVelocities(scope, blocks.map((block) => ({
      phrase_index: block.phraseIndex,
      note_index: block.noteIndex,
      velocity,
    })));
  };

  /** Drop the exact velocity so the note's written dynamic is what plays again. */
  const clearVelocityAt = (phraseIdx: number, noteIdx: number): Promise<string | null> => {
    if (selPattern()) {
      const p = selPattern();
      return p ? store.clearPatternNoteVelocity(p.name, noteIdx) : Promise.resolve(null);
    }
    const ti = trackIdx();
    return ti !== null ? store.clearNoteVelocity(ti, phraseIdx, noteIdx) : Promise.resolve(null);
  };

  /** Write a drum hit's exact velocity; only a pattern owns hits. */
  const setHitVelocityAt = (hitIndex: number, velocity: number): Promise<string | null> => {
    const p = selPattern();
    return p ? store.setPatternHitVelocity(p.name, hitIndex, velocity) : Promise.resolve(null);
  };

  /** Drop a hit's exact velocity so its written dynamic is what plays again. */
  const clearHitVelocityAt = (hitIndex: number): Promise<string | null> => {
    const p = selPattern();
    return p ? store.clearPatternHitVelocity(p.name, hitIndex) : Promise.resolve(null);
  };

  const setMutedAt = (phraseIdx: number, noteIdx: number, muted: boolean) => {
    if (selPattern()) {
      const p = selPattern();
      if (p) store.setPatternNoteMuted(p.name, noteIdx, muted);
      return;
    }
    const ti = trackIdx();
    if (ti !== null) store.setNoteMuted(ti, phraseIdx, noteIdx, muted);
  };

  const setGraceAt = (phraseIdx: number, noteIdx: number, grace: boolean) => {
    if (selPattern()) {
      const p = selPattern();
      if (p) store.setPatternNoteGrace(p.name, noteIdx, grace);
      return;
    }
    const ti = trackIdx();
    if (ti !== null) store.setNoteGrace(ti, phraseIdx, noteIdx, grace);
  };

  const setDynamicsAt = (phraseIdx: number, noteIdx: number, dynamics: string | null) => {
    if (selPattern()) {
      const p = selPattern();
      if (p) store.setPatternNoteDynamics(p.name, noteIdx, dynamics);
      return;
    }
    const ti = trackIdx();
    if (ti !== null) store.setNoteDynamics(ti, phraseIdx, noteIdx, dynamics);
  };

  const setArticulationAt = (phraseIdx: number, noteIdx: number, articulation: string | null): Promise<string | null> => {
    if (selPattern()) {
      const p = selPattern();
      return p ? store.setPatternNoteArticulation(p.name, noteIdx, articulation) : Promise.resolve(null);
    }
    const ti = trackIdx();
    return ti !== null ? store.setNoteArticulation(ti, phraseIdx, noteIdx, articulation) : Promise.resolve(null);
  };

  const VELOCITY_LABELS: [number, string][] = [
    [127, "FFF"], [112, "FF"], [96, "F"], [80, "MF"], [64, "MP"], [48, "P"], [32, "PP"], [16, "PPP"],
  ];

  /** One row per articulation the DSL knows, with the written token checked. */
  const articulationMenuItems = (
    phraseIdx: number,
    noteIdx: number,
    current: Articulation | null,
  ): ContextMenuItem[] => [
    {
      label: i18n.t("menu.note.articulationNone"),
      checked: !current,
      onSelect: () => { void setArticulationAt(phraseIdx, noteIdx, null); },
    },
    ...ARTICULATIONS.map((entry) => ({
      label: entry.token,
      checked: current === entry.value,
      onSelect: () => { void setArticulationAt(phraseIdx, noteIdx, entry.token); },
    })),
  ];

  const openNoteMenu = (e: MouseEvent, phraseIdx: number, noteIdx: number) => {
    const note = noteAt(phraseIdx, noteIdx);
    if (!note) return;
    const velocity = noteVelocity(note);
    const items: ContextMenuItem[] = [
      { type: "heading", label: `${pitchToString(note.pitch)} · ${velocityText(velocity, velocitySource(note), note.dynamics ?? null)}` },
      {
        label: i18n.t("menu.note.copy"),
        shortcut: "Cmd/Ctrl+C",
        onSelect: () => copyNoteAt(phraseIdx, noteIdx),
      },
      {
        label: i18n.t("menu.note.paste"),
        shortcut: "Cmd/Ctrl+V",
        disabled: !noteClipboard,
        onSelect: () => pasteAtPlayhead(phraseIdx),
      },
      {
        label: i18n.t("menu.note.duplicate"),
        shortcut: "Cmd/Ctrl+D",
        onSelect: () => duplicateNoteAt(phraseIdx, noteIdx),
      },
      {
        label: i18n.t("menu.note.delete"),
        shortcut: "Delete",
        danger: true,
        onSelect: () => removeNoteAt(phraseIdx, noteIdx),
      },
      { type: "separator" },
      // The 30 articulation tokens and both velocity scales fold into groups:
      // flat, an open menu would be 62 rows long.
      {
        type: "group",
        label: i18n.t("menu.note.dynamics"),
        items: DYNAMICS_TOKENS.map(([value, token]) => ({
          label: `${token} (${dynamicsToVelocity(value)})`,
          checked: note.dynamics === value,
          onSelect: () => setDynamicsAt(phraseIdx, noteIdx, token),
        })),
      },
      {
        type: "group",
        label: i18n.t("menu.note.velocity"),
        items: [
          ...VELOCITY_LABELS.map(([value, name]) => ({
            label: `${value} (${name})`,
            checked: value === velocity,
            onSelect: () => { void setVelocityAt(phraseIdx, noteIdx, value); },
          })),
          {
            label: i18n.t("menu.note.resetVelocity"),
            // The token is the fallback; an already-written note has nothing to clear.
            disabled: note.velocity === null || note.velocity === undefined,
            hint: i18n.t("pianoroll.velocityAlreadyWritten"),
            onSelect: () => { void clearVelocityAt(phraseIdx, noteIdx); },
          },
        ],
      },
      {
        type: "group",
        label: i18n.t("menu.note.articulation"),
        items: articulationMenuItems(phraseIdx, noteIdx, note.articulation ?? null),
      },
      { type: "separator" },
      {
        label: note.muted ? i18n.t("menu.note.unmute") : i18n.t("menu.note.mute"),
        onSelect: () => setMutedAt(phraseIdx, noteIdx, !note.muted),
      },
      {
        label: note.grace ? i18n.t("menu.note.ungrace") : i18n.t("menu.note.grace"),
        onSelect: () => setGraceAt(phraseIdx, noteIdx, !note.grace),
      },
    ];
    openContextMenu({
      x: e.clientX,
      y: e.clientY,
      className: "pianoroll-note-menu",
      ariaLabel: i18n.t("menu.note.actions"),
      items,
    });
  };

  const openGridMenu = (e: MouseEvent, midi: number, absBeat: number) => {
    const clock = createProjectClock(store.project.properties);
    const position = clock.barBeatAt(Math.max(0, absBeat));
    // A drum lane names its row; a pitched grid names the note.
    const row = laneRows()?.find((candidate) => candidate.note === midi);
    const parts = midiToNoteParts(midi);
    const target = row
      ? row.label
      : pitchToString({ note: parts.name, accidental: parts.accidental, octave: parts.octave });
    const items: ContextMenuItem[] = [
      { type: "heading", label: i18n.t("pianoroll.position", { target, bar: position.bar, beat: position.beat }) },
      {
        label: i18n.t("menu.grid.drawNote"),
        onSelect: () => addNoteAt(midi, absBeat, 1 / gridDiv()),
      },
      {
        label: i18n.t("menu.grid.pasteNote"),
        shortcut: "Cmd/Ctrl+V",
        disabled: !noteClipboard,
        onSelect: () => {
          if (noteClipboard) pasteEntries(phraseIndexAtBeat(absBeat) ?? 0, absBeat);
        },
      },
      { type: "separator" },
      { type: "heading", label: i18n.t("menu.grid.grid") },
      ...GRID_OPTIONS.map((option) => ({
        label: gridOptionLabel(option),
        checked: gridDivision() === option.value,
        onSelect: () => { setGridDivision(option.value); },
      })),
    ];
    openContextMenu({
      x: e.clientX,
      y: e.clientY,
      className: "pianoroll-grid-menu",
      ariaLabel: i18n.t("menu.grid.actions"),
      items,
    });
  };

  /** True while a right-button drag erases notes under the pointer. */
  const [erasing, setErasing] = createSignal(false);
  let stopErase: (() => void) | null = null;

  /**
   * Right-drag erases every note the pointer passes over, as one undo entry.
   * A press that never moves is still the note's menu, which is where the note
   * actions live.
   */
  const beginErase = (event: PointerEvent) => {
    const scope = toolScope();
    if (!scope) return;
    event.preventDefault();
    event.stopPropagation();
    const startX = event.clientX;
    const startY = event.clientY;
    const erased = new Set<string>();
    let moved = false;
    let gestureOpen = false;
    setErasing(true);
    const targetAt = (x: number, y: number) =>
      (document.elementFromPoint(x, y) as Element | null);
    const eraseAt = (x: number, y: number) => {
      const target = targetAt(x, y);
      const note = target?.closest<HTMLElement>("[data-note-key]");
      if (note) {
        const key = note.dataset.noteKey!;
        if (erased.has(key)) return;
        erased.add(key);
        void store.removeNotes(scope, [{
          phrase_index: Number(note.dataset.phraseIndex),
          note_index: Number(note.dataset.noteIndex),
        }]);
        return;
      }
      // A drum lane draws hits instead of notes; the eraser takes them too.
      const hit = target?.closest<HTMLElement>("[data-hit-key]");
      if (!hit) return;
      const key = hit.dataset.hitKey!;
      if (erased.has(key)) return;
      erased.add(key);
      const name = hit.dataset.hitPattern || selPattern()?.name || "";
      if (!name) return;
      void store.removePatternHit(name, Number(hit.dataset.hitIndex));
    };
    stopErase = beginPointerDrag({
      element: event.currentTarget as HTMLElement,
      pointerId: event.pointerId,
      onMove: (x, y) => {
        if (!moved) {
          if (Math.hypot(x - startX, y - startY) < 4) return;
          moved = true;
          gestureOpen = true;
          beginUndoGesture("project");
          // The swipe starts where the button went down, not where it happens
          // to cross the threshold.
          eraseAt(startX, startY);
        }
        eraseAt(x, y);
      },
      onEnd: () => {
        stopErase = null;
        setErasing(false);
        if (gestureOpen) endUndoGesture();
        if (moved) return;
        const target = targetAt(startX, startY);
        const note = target?.closest<HTMLElement>("[data-note-key]");
        if (note) {
          handleNoteRightClick(
            new MouseEvent("contextmenu", { clientX: startX, clientY: startY }),
            Number(note.dataset.phraseIndex),
            Number(note.dataset.noteIndex),
          );
          return;
        }
        const hit = target?.closest<HTMLElement>("[data-hit-key]");
        if (hit) {
          handleHitRightClick(
            new MouseEvent("contextmenu", { clientX: startX, clientY: startY }),
            hit.dataset.hitPattern || null,
            Number(hit.dataset.hitIndex),
          );
        }
      },
    });
  };

  const handleNoteRightClick = (e: MouseEvent, phraseIdx: number, noteIdx: number) => {
    e.preventDefault();
    e.stopPropagation();
    openNoteMenu(e, phraseIdx, noteIdx);
  };

  /**
   * The hit menu names the hit under the pointer and deletes that hit. The
   * index is the one the hit holds in its own pattern, which the grid keeps
   * even for hits the view does not draw.
   */
  const handleHitRightClick = (
    e: MouseEvent,
    patternName: string | null,
    hitIndex: number,
  ) => {
    e.preventDefault();
    e.stopPropagation();
    const name = patternName ?? selPattern()?.name ?? null;
    if (!name) return;
    const pattern = store.project.patterns.find((candidate) => candidate.name === name);
    const hit = pattern?.hits[hitIndex];
    if (!pattern || !hit) return;
    selectHit(name, hitIndex);
    openContextMenu({
      x: e.clientX,
      y: e.clientY,
      className: "pianoroll-hit-menu",
      ariaLabel: i18n.t("menu.hit.actions"),
      items: [
        { type: "heading", label: hit.drum },
        {
          label: i18n.t("menu.hit.delete"),
          danger: true,
          onSelect: () => {
            setSelectedHit(null);
            void store.removePatternHit(pattern.name, hitIndex);
          },
        },
      ],
    });
  };

  /** Delete the selection as one command, so one undo brings all of it back. */
  const handleDeleteKey = () => {
    const scope = toolScope();
    if (!scope) return;
    const live = liveSelection();
    if ("refusal" in live) {
      store.setError(live.refusal);
      return;
    }
    if (live.refs.length === 0) {
      // No note is picked: the delete key acts on the picked drum hit.
      const hit = selectedHit();
      if (hit) {
        setSelectedHit(null);
        void store.removePatternHit(hit.patternName, hit.hitIndex);
      }
      return;
    }
    void store.removeNotes(scope, live.refs).then((message) => {
      if (message) return;
      setSelection([]);
    });
  };

  /**
   * Ctrl+D / Cmd+D — duplicate the selection one beat later. One note keeps the
   * single-note path; a group is one batch command, so one undo removes it all.
   */
  const handleDuplicateSelected = () => {
    const ap = activePattern();
    const scope = toolScope();
    if (!ap || !scope) return;
    const live = liveSelection();
    if ("refusal" in live) {
      store.setError(live.refusal);
      return;
    }
    // With nothing selected the pattern view duplicates every note it shows.
    const refs = live.refs.length > 0
      ? live.refs
      : selPattern()
        ? ap.events.map((_, index) => ({ phrase_index: 0, note_index: index }))
        : [];
    if (refs.length === 0) return;
    if (refs.length === 1) {
      duplicateNoteAt(refs[0].phrase_index, refs[0].note_index);
      return;
    }
    void store.duplicateNotes(scope, refs, 1);
  };

  // Arrow keys — nudge the selection by one step, as one command.
  const handleNudgeSelected = (beatOffset: number) => {
    const ap = activePattern();
    const scope = toolScope();
    if (!ap || !scope) return;
    const live = liveSelection();
    if ("refusal" in live) {
      store.setError(live.refusal);
      return;
    }
    const selected = live.refs.length > 0;
    const moves: NoteMove[] = [];
    const identities: NoteIdentity[] = [];
    if (scope.kind === "pattern") {
      // With nothing selected the pattern view nudges every note it shows.
      const targets = selected
        ? live.refs.map((ref) => ({ ref, event: ap.events[ref.note_index] }))
        : ap.events.map((event, index) => ({
          ref: { phrase_index: 0, note_index: index },
          event,
        }));
      const maxLocal = Math.max(0, viewSpanBeats() - minStepBeats());
      for (const { ref, event } of targets) {
        if (!event) continue;
        const newBeat = Math.min(
          maxLocal,
          Math.max(0, snapQuarterToGrid((event.start_beat ?? 0) + beatOffset, gridDiv())),
        );
        moves.push({ ...ref, new_start_beat: newBeat });
        identities.push({ ...eventIdentity(event, ref.note_index), startBeat: newBeat });
      }
    } else {
      // The track view has no unselected gesture: every note in it belongs to
      // the track, so a nudge with nothing selected would move the whole song.
      if (!selected) return;
      const t = track();
      const clock = createProjectClock(store.project.properties);
      for (const ref of live.refs) {
        const phrase = t?.phrases[ref.phrase_index];
        const event = phrase?.events[ref.note_index];
        if (!event) continue;
        const range = phrase?.time_range;
        const maxLocal = range
          ? Math.max(0, clock.quarterAt(range.end) - clock.quarterAt(range.start) - minStepBeats())
          : Infinity;
        const newBeat = Math.min(
          maxLocal,
          Math.max(0, snapQuarterToGrid((event.start_beat ?? 0) + beatOffset, gridDiv())),
        );
        moves.push({ phrase_index: ref.phrase_index, note_index: ref.note_index, new_start_beat: newBeat });
        identities.push({ phraseIndex: ref.phrase_index, midi: pitchToMidi(event.pitch), startBeat: newBeat });
      }
    }
    if (moves.length === 0) return;
    void store.moveNotes(scope, moves).then((message) => {
      if (message) return;
      // The notes stay selected: their identities follow them to their new
      // starts, so a re-sorted list cannot move the highlight to a neighbour.
      if (selected) setSelection(identities);
    });
  };

  // Keydown handler for keyboard shortcuts inside the grid
  const handleGridKeyDown = (e: KeyboardEvent) => {
    const isMod = e.metaKey || e.ctrlKey;
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      handleDeleteKey();
      return;
    }
    if (isMod && e.key === "d") {
      e.preventDefault();
      e.stopPropagation();
      handleDuplicateSelected();
      return;
    }
    if (isMod && e.key === "a") {
      e.preventDefault();
      e.stopPropagation();
      const ap = activePattern();
      if (ap) {
        setSelection(ap.events.map((event, index) => eventIdentity(event, index)));
      }
      return;
    }
    if (isMod && e.key === "c") {
      e.preventDefault();
      e.stopPropagation();
      copySelectedNotes();
      return;
    }
    if (isMod && e.key === "v") {
      e.preventDefault();
      e.stopPropagation();
      const playhead = store.transport().position_beats;
      pasteAtPlayhead(phraseIndexAtBeat(playhead) ?? 0);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setSelection([]);
      return;
    }
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      e.stopPropagation();
      const step = e.shiftKey ? 1 : (1 / gridDiv());
      const dir = e.key === "ArrowRight" ? step : -step;
      handleNudgeSelected(dir);
      return;
    }
  };

  // Piano key click — preview instrument sound
  const handlePianoKeyClick = (_midi: number) => {
    const ap = activePattern();
    if (!ap) return;
    if (ap.preset) {
      store.previewPreset(ap.preset);
    } else {
      const ti = trackIdx();
      if (ti !== null) store.previewTrack(ti);
    }
  };

  return (
    <div style={{ display: "flex", "flex-direction": "column", height: "100%", background: "var(--bg-primary)" }}>
      {/* Toolbar */}
      <div class="pianoroll-toolbar">
        <span style={{ "font-size": "var(--font-size-sm)", "font-weight": "600", "margin-right": "12px" }}>
          {headerTitle()}
        </span>
        {(() => {
          const ap = activePattern();
          if (!ap) return null;
          return (
          <span style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)", "margin-right": "12px" }}>
            {instrumentDisplay(ap.instrument)}
            {ap.preset ? ` \u{2022} ${ap.preset}` : ""}
          </span>
        );})()}
        <div class="toolbar-separator" />
        {/* The kit the lane's rows come from, so the row labels have a source. */}
        <Show when={openKit()}>
          {(kit) => (
            <span
              class="pianoroll-kit-name"
              classList={{ "pianoroll-kit-name-empty": kit().pads.length === 0 }}
              data-kit-name={kit().name}
              {...useTooltip(() => i18n.t("pianoroll.kitRows", { kit: kit().name }))}
            >
              {kit().name}
            </span>
          )}
        </Show>
        <button class="toolbar-btn" classList={{ active: tool() === "select" }} aria-label={i18n.t("pianoroll.selectTool")} onClick={() => setTool("select")} {...useTooltip(() => i18n.t("pianoroll.selectToolHint"))}>
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none"><path d="M2 1l4.5 12 1.5-4.5L12.5 7 2 1z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/></svg>
        </button>
        <button class="toolbar-btn" classList={{ active: tool() === "draw" }} aria-label={i18n.t("pianoroll.drawTool")} onClick={() => setTool("draw")} {...useTooltip(() => i18n.t("pianoroll.drawToolHint"))}>
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none"><path d="M2 12l1.5-1.5m0 0L9.5 4.5l2 2-6 6-3.5 1 1.5-3.5zm0 0L5 9m4.5-4.5l1-1a1.414 1.414 0 00-2-2l-1 1 2 2z" stroke="currentColor" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round"/></svg>
        </button>
        <div class="toolbar-separator" />
        <span style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)" }}>{i18n.t("menu.grid.grid")}:</span>
        <select
          class="timeline-grid-select"
          value={gridDiv()}
          onChange={(e) => setGridDiv(parseInt(e.currentTarget.value))}
        >

          <For each={GRID_OPTIONS}>
            {(option) => <option value={option.value}>{gridOptionLabel(option)}</option>}
          </For>
        </select>
        <Show when={selPattern()}>
          <div class="toolbar-separator" />
          <span class="pianoroll-tool" data-pattern-length>
            <span class="pianoroll-tool-label">{i18n.t("pianoroll.length")}</span>
            <button
              class="toolbar-btn"
              aria-label={i18n.t("pianoroll.removeBar")}
              disabled={patternBars() <= 1}
              onClick={() => changePatternBars(-1)}
            >−</button>
            <span class="pianoroll-length-value">{i18n.t("pianoroll.bars", { count: patternBars() })}</span>
            <button
              class="toolbar-btn"
              aria-label={i18n.t("pianoroll.addBar")}
              onClick={() => changePatternBars(1)}
            >+</button>
          </span>
        </Show>
        <div class="toolbar-separator" />
        {/* Batch tools: the button states its own refusal, the select the amount. */}
        <span class="pianoroll-tool" data-note-tool="quantise" title={toolTitle("quantise")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={toolLabel("quantise")}
            aria-busy={toolPending() === "quantise"}
            disabled={toolRefusal("quantise") !== null || toolPending() !== null}
            onClick={handleQuantise}
          >{toolLabel("quantise")}</button>
          <select
            class="timeline-grid-select"
            aria-label={i18n.t("pianoroll.quantiseStrength")}
            value={quantiseStrength()}
            onChange={(event) => setQuantiseStrength(parseInt(event.currentTarget.value, 10))}
          >
            <For each={QUANTISE_STRENGTHS}>
              {(percent) => (
                <option value={percent}>{i18n.t("pianoroll.strengthPercent", { percent })}</option>
              )}
            </For>
          </select>
        </span>
        <span class="pianoroll-tool" data-note-tool="humanise" title={toolTitle("humanise")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={toolLabel("humanise")}
            aria-busy={toolPending() === "humanise"}
            disabled={toolRefusal("humanise") !== null || toolPending() !== null}
            onClick={handleHumanise}
          >{toolLabel("humanise")}</button>
          <select
            class="timeline-grid-select"
            aria-label={i18n.t("pianoroll.humaniseAmount")}
            value={humaniseAmount()}
            onChange={(event) => setHumaniseAmount(event.currentTarget.value)}
          >
            <For each={HUMANISE_AMOUNTS}>
              {(option) => <option value={option.key}>{i18n.t(option.label)}</option>}
            </For>
          </select>
        </span>
        <span class="pianoroll-tool" data-note-tool="legato" title={toolTitle("legato")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={toolLabel("legato")}
            aria-busy={toolPending() === "legato"}
            disabled={toolRefusal("legato") !== null || toolPending() !== null}
            onClick={handleLegato}
          >{toolLabel("legato")}</button>
          <select
            class="timeline-grid-select"
            aria-label={i18n.t("pianoroll.legatoAmount")}
            value={legatoAmount()}
            onChange={(event) => setLegatoAmount(event.currentTarget.value)}
          >
            <For each={LEGATO_AMOUNTS}>
              {(option) => <option value={option.key}>{i18n.t(option.label)}</option>}
            </For>
          </select>
        </span>
        <div class="toolbar-separator" />
        {/* Velocity lane gestures: they write one command for the selection and
            state their failure inside the lane, where the bars are. The label
            says they act on velocity, which the short names cannot. */}
        <span class="pianoroll-tool-label">{i18n.t("pianoroll.velocityTools")}</span>
        <span class="pianoroll-tool" data-velocity-tool="ramp" title={velocityToolTitle("ramp")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={velocityToolLabel("ramp")}
            aria-busy={velocityToolPending() === "ramp"}
            disabled={velocityRefusal("ramp") !== null || velocityToolPending() !== null}
            onClick={handleRampVelocity}
          >{velocityToolLabel("ramp")}</button>
        </span>
        <span class="pianoroll-tool" data-velocity-tool="scale" title={velocityToolTitle("scale")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={velocityToolLabel("scale")}
            aria-busy={velocityToolPending() === "scale"}
            disabled={velocityRefusal("scale") !== null || velocityToolPending() !== null}
            onClick={handleScaleVelocity}
          >{velocityToolLabel("scale")}</button>
          <select
            class="timeline-grid-select"
            aria-label={i18n.t("pianoroll.scaleAmount")}
            value={scalePercent()}
            onChange={(event) => setScalePercent(parseInt(event.currentTarget.value, 10))}
          >
            <For each={SCALE_PERCENTS}>
              {(percent) => (
                <option value={percent}>{i18n.t("pianoroll.strengthPercent", { percent })}</option>
              )}
            </For>
          </select>
        </span>
        <span class="pianoroll-tool" data-velocity-tool="compress" title={velocityToolTitle("compress")}>
          <button
            class="toolbar-btn pianoroll-tool-btn"
            aria-label={velocityToolLabel("compress")}
            aria-busy={velocityToolPending() === "compress"}
            disabled={velocityRefusal("compress") !== null || velocityToolPending() !== null}
            onClick={handleCompressVelocity}
          >{velocityToolLabel("compress")}</button>
          <select
            class="timeline-grid-select"
            aria-label={i18n.t("pianoroll.compressTarget")}
            value={compressTarget()}
            onChange={(event) => setCompressTarget(parseInt(event.currentTarget.value, 10))}
          >
            <For each={COMPRESS_TARGETS}>
              {(velocity) => (
                <option value={velocity}>
                  {i18n.t("pianoroll.targetVelocity", { velocity })}
                </option>
              )}
            </For>
          </select>
        </span>
        <div class="toolbar-separator" />
        <button class="toolbar-btn" aria-label={i18n.t("timeline.zoomOut")} onClick={() => setZoom(Math.max(10, zoom() - 6))} {...useTooltip(() => i18n.t("timeline.zoomOut"))}>−</button>
        <span class="zoom-label">{i18n.t("timeline.pxPerBeat", { px: zoom() })}</span>
        <button class="toolbar-btn" aria-label={i18n.t("timeline.zoomIn")} onClick={() => setZoom(Math.min(200, zoom() + 6))} {...useTooltip(() => i18n.t("timeline.zoomIn"))}>+</button>
      </div>

      {/* The one line the toolbar states a failed batch edit on. */}
      <Show when={toolFailure()}>
        <div class="pianoroll-toolbar-notice">
          <LaneNotice text={toolFailure()} danger />
        </div>
      </Show>

      {/* Notes and hits a shortened pattern no longer covers stay counted, so
          their silence is a warning rather than a disappearance. */}
      <Show when={outOfRange().notes + outOfRange().hits > 0}>
        <div class="pianoroll-toolbar-notice">
          <LaneNotice
            text={i18n.t("pianoroll.outOfRange", {
              notes: outOfRange().notes,
              hits: outOfRange().hits,
            })}
            danger
          />
        </div>
      </Show>

      <div style={{ display: "flex", flex: "1", overflow: "hidden" }}>
        {/* One scroll container holds the keys, the grid and the velocity lane,
            so all three share one x mapping and one scroll offset. */}
        <div
          ref={gridRef}
          data-pianoroll-grid
          class="pianoroll-scroll"
          style={{ flex: "1", overflow: "auto", position: "relative", "overscroll-behavior": "none", "overflow-anchor": "none" }}
          onWheel={handleWheel}
          onKeyDown={handleGridKeyDown}
          tabIndex={-1}
        >
          <div
            class="pianoroll-scroll-content"
            style={{
              display: "flex",
              "flex-direction": "column",
              width: `${keysWidth() + canvasWidth()}px`,
              "min-width": "100%",
              height: `${PIANO_ROLL_RULER_HEIGHT + scoreStripHeight() + canvasHeight() + VELOCITY_LANE_HEIGHT}px`,
            }}
          >
            <div class="pianoroll-rows" style={{ display: "flex", height: `${PIANO_ROLL_RULER_HEIGHT + scoreStripHeight() + canvasHeight()}px` }}>
              {/* Piano keys — sticky left: they scroll vertically with the grid
                  but never move horizontally. A drum lane labels pads instead. */}
              <div
                class="pianoroll-keys"
                classList={{ "drum-lane": laneRows() !== null }}
                style={{ position: "sticky", left: "0", "z-index": "6", width: `${keysWidth()}px` }}
              >
                {/* Covers the same height as the sticky ruler so rows stay aligned. */}
                <div
                  class="pianoroll-keys-spacer"
                  style={{
                    position: "sticky", top: "0",
                    height: `${PIANO_ROLL_RULER_HEIGHT + scoreStripHeight()}px`,
                    "flex-shrink": "0", background: "var(--bg-secondary)", "z-index": "7",
                  }}
                />
                <div style={{ position: "relative", height: `${canvasHeight()}px` }}>
                  <Show
                    when={laneRows()}
                    fallback={
                      <For each={Array.from({ length: NOTE_RANGE }, (_, i) => MAX_MIDI - 1 - i)}>
                        {(midi) => (
                          <div
                            class="piano-key"
                            classList={{ black: isBlackKey(midi), "c-note": midi % 12 === 0 }}
                            style={{ top: `${(MAX_MIDI - 1 - midi) * NOTE_HEIGHT}px`, height: `${NOTE_HEIGHT}px` }}
                            onClick={(e) => { e.stopPropagation(); handlePianoKeyClick(midi); }}
                            {...useTooltip(() => `Click to preview${midi % 12 === 0 ? ` — ${midiToNoteParts(midi).name}${midiToNoteParts(midi).octave}` : ""}`)}
                          >
                            {midi % 12 === 0 ? `C${Math.floor(midi / 12) - 1}` : ""}
                          </div>
                        )}
                      </For>
                    }
                  >
                    {(rows) => (
                      <For each={rows()}>
                        {(row, index) => (
                          <div
                            class="piano-key drum-lane-key"
                            classList={{ "kit-pad": row.pad !== null }}
                            data-row-note={String(row.note)}
                            data-row-label={row.label}
                            style={{ top: `${index() * NOTE_HEIGHT}px`, height: `${NOTE_HEIGHT}px` }}
                            onClick={(e) => { e.stopPropagation(); handlePianoKeyClick(row.note); }}
                            {...useTooltip(() => row.pad === null ? row.label : `${row.label} — note ${row.note}`)}
                          >{row.label}</div>
                        )}
                      </For>
                    )}
                  </Show>
                </div>
              </div>

              {/* Grid column — shares the keys' scroll offset by construction. */}
              <div
                class="pianoroll-grid-column"
                style={{ position: "relative", width: `${canvasWidth()}px`, "flex-shrink": "0" }}
              >
                {/* Ruler */}
                <div
                  class="pianoroll-ruler"
                  style={{ width: `${canvasWidth()}px`, cursor: "pointer", position: "sticky", top: "0", "z-index": "5" }}
                  data-ruler-start-beat={String(viewOriginBeats())}
                  data-ruler-end-beat={String(viewEndBeats())}
                  onPointerDown={handleRulerPointerDown}
                  onContextMenu={(e) => e.preventDefault()}
                >
                  <div class="pianoroll-clip-edge start" style={{ left: "0px" }} />
                  <div class="pianoroll-clip-edge end" style={{ left: `${clipEndX()}px` }} />
                  {/* The end is a handle: the roll is where the writing happens. */}
                  <div
                    class="pianoroll-clip-end-handle"
                    data-clip-end-handle
                    style={{
                      left: `${
                        (resizeEndPreview() !== null
                          ? xForBeat(viewOriginBeats(), pxPerBeat(), resizeEndPreview()!)
                          : clipEndX()) - 6
                      }px`,
                    }}
                    role="separator"
                    aria-orientation="vertical"
                    aria-label={i18n.t("pianoroll.resizeEnd")}
                    {...useTooltip(() => i18n.t("pianoroll.resizeEndHint"))}
                    onPointerDown={beginResizeEnd}
                  />
                  <For each={barMarks()}>
                    {(mark) => (
                      <span class="pianoroll-ruler-mark" style={{ left: `${mark.quarter * pxPerBeat()}px` }}>
                        {mark.bar}
                      </span>
                    )}
                  </For>
                  <Show when={cursorX() >= 0}>
                    <div
                      class="cursor-head"
                      style={{ left: `${cursorX() - 6}px` }}
                      onPointerDown={handleRulerPointerDown}
                    />
                  </Show>
                </div>

                {/* Score-mark strip: the hairpins and pedals the engine plays
                    but the notes cannot show. It takes no room when empty. */}
                <Show when={hasScoreMarks()}>
                  <ScoreMarks
                    hairpins={scoreHairpins()}
                    pedals={scorePedals()}
                    xForBeat={xForLocalBeat}
                    width={canvasWidth()}
                  />
                </Show>

                {/* Note grid */}
                <div
                  ref={canvasRef}
                  style={{
                    width: `${canvasWidth()}px`, height: `${canvasHeight()}px`,
                    position: "relative",
                    overflow: "hidden",
                    cursor: tool() === "draw"
                      ? "url('data:image/svg+xml," + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path d="M11 1.5L14.5 5 5 14.5H1.5V11z" fill="%23e2a842" stroke="white" stroke-width="1"/><path d="M9 3.5l4 4" stroke="white" stroke-width="1"/></svg>') + "') 1 15, crosshair"
                      : "default",
                  }}
                  data-note-canvas
                  data-origin-beat={String(viewOriginBeats())}
                  data-view-end-beat={String(viewEndBeats())}
                  data-view-span-beats={String(viewSpanBeats())}
                  classList={{ "pianoroll-erasing": erasing() }}
                  onPointerDown={(e) => {
                    // A secondary press starts the eraser; a press without a
                    // drag falls back to the note menu when it ends.
                    if (isSecondaryClick(e)) beginErase(e);
                  }}
                  onMouseDown={(e) => {
                    // Only the primary button edits; secondary presses open the menu.
                    if (isSecondaryClick(e)) return;
                    if (tool() === "draw" && e.button === 0) { startDraw(e); return; }
                    handleGridMouseDown(e);
                  }}
                  onMouseMove={handleGridMouseMove}
                  onMouseLeave={handleGridMouseLeave}
                  onContextMenu={(e) => {
                    // The eraser gesture owns the secondary button while it runs.
                    if (erasing()) {
                      e.preventDefault();
                      return;
                    }
                    const target = e.target as HTMLElement;
                    // Note and hit rows handle their own menu and stop propagation.
                    if (target.closest(".pianoroll-note") || target.closest(".pianoroll-drum-hit")) return;
                    e.preventDefault();
                    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
                    const note = rowNoteAt(e.clientY - rect.top);
                    if (note === null) return;
                    openGridMenu(e, note, beatFromClientX(e.clientX));
                  }}
                >
                  {/* Row backgrounds: one per lane row, else one per semitone. */}
                  <Show
                    when={laneRows()}
                    fallback={
                      <For each={Array.from({ length: NOTE_RANGE }, (_, i) => MAX_MIDI - 1 - i)}>
                        {(midi) => (
                          <div
                            class="pianoroll-row-bg"
                            style={{
                              top: `${(MAX_MIDI - 1 - midi) * NOTE_HEIGHT}px`,
                              height: `${NOTE_HEIGHT}px`,
                              background: isBlackKey(midi) ? "color-mix(in srgb, var(--text-primary) 6%, transparent)" : "transparent",
                              "border-bottom": midi % 12 === 0 ? "1px solid color-mix(in srgb, var(--grid-bar) 55%, transparent)" : undefined,
                            }}
                          />
                        )}
                      </For>
                    }
                  >
                    {(rows) => (
                      <For each={rows()}>
                        {(row, index) => (
                          <div
                            class="pianoroll-row-bg drum-lane-row"
                            classList={{ "other-row": row.pad === null }}
                            style={{
                              top: `${index() * NOTE_HEIGHT}px`,
                              height: `${NOTE_HEIGHT}px`,
                            }}
                          />
                        )}
                      </For>
                    )}
                  </Show>

                  {/* Grid lines — CSS background */}
                  <div
                    class="pianoroll-grid-bg"
                    style={{
                      width: `${canvasWidth()}px`, height: `${canvasHeight()}px`,
                      "background-image": gridBackground(),
                    }}
                  />

                  {/* Clip bounds stay fixed when the editor shows authored hits beyond them. */}
                  <div class="pianoroll-clip-edge start" style={{ left: "0px" }} />
                  <div class="pianoroll-clip-edge end" style={{ left: `${clipEndX()}px` }} />
                  {/* The pattern's own end, inside a wider editing view. */}
                  <Show when={selPattern() && !openedBlock() && placementEndLocal() < viewSpanBeats() - 1e-9}>
                    <div
                      class="pianoroll-length-edge"
                      data-length-end-beat={String(placementEndLocal())}
                      style={{ left: `${xForLocalBeat(placementEndLocal())}px` }}
                    />
                  </Show>

                  {/* Notes */}
                  {(() => {
                    const ap = activePattern();
                    if (!ap) return null;
                    return (
                    <div style={{ position: "absolute", inset: "0", "pointer-events": "auto" }}>
                      <PatternDefNotes
                        patternDef={ap}
                        presetParams={presetParams()}
                        getPxPerBeat={pxPerBeat}
                        xForBeat={xForLocalBeat}
                        viewWidth={canvasWidth()}
                        lengthBeats={selPattern() ? placementEndLocal() : undefined}
                        rows={laneRows()}
                        onDragStart={onNoteDragStart}
                        onResizeStart={onNoteResizeStart}
                        onNoteRightClick={handleNoteRightClick}
                        erasing={erasing}
                        onNoteSelect={selectNote}
                        dragState={dragState()}
                        dragOffsetX={dragOffsetX()}
                        dragPitch={dragPitch()}
                        gridDiv={gridDiv}
                        isNoteSelected={(phraseIdx, noteIdx) => selectedNotes().has(`${phraseIdx}:${noteIdx}`)}
                        previewChanged={previewChangedNote}
                        tool={tool}
                      />
                      <PatternDefHits
                        hits={hitBlocks()}
                        xForBeat={xForLocalBeat}
                        rows={laneRows()}
                        isHitSelected={isHitSelected}
                        onHitSelect={selectHit}
                        onHitRightClick={handleHitRightClick}
                        erasing={erasing}
                      />
                    </div>
                  );})()}

                  {/* Alignment snap guide */}
                  <Show when={snapGuideX() >= 0}>
                    <div
                      class="pianoroll-snap-guide"
                      style={{ left: `${snapGuideX()}px`, height: `${canvasHeight()}px` }}
                    />
                  </Show>

                  {/* Draw preview */}
                  {drawPreview() && (() => {
                    const dp = drawPreview()!;
                    return (
                      <div
                        class="pianoroll-note note-preview"
                        style={{
                          left: `${xForLocalBeat(dp.startBeat)}px`,
                          top: `${rowTopOf(dp.midi) ?? 0}px`,
                          width: `${Math.max(dp.durBeats * pxPerBeat() - 1, 10)}px`,
                          height: `${NOTE_HEIGHT - 1}px`,
                        }}
                      >
                        <span class="pianoroll-note-label">{dp.label}</span>
                      </div>
                    );
                  })()}

                  {/* Marquee selection rectangle */}
                  {marquee() && (() => {
                    const m = marquee()!;
                    const left = Math.min(m.startX, m.endX);
                    const top = Math.min(m.startY, m.endY);
                    const w = Math.abs(m.endX - m.startX);
                    const h = Math.abs(m.endY - m.startY);
                    return (
                      <div
                        class="pianoroll-marquee"
                        style={{
                          left: `${left}px`, top: `${top}px`,
                          width: `${w}px`, height: `${h}px`,
                        }}
                      />
                    );
                  })()}

                  {/* Playback cursor */}
                  <Show when={cursorX() >= 0}>
                    <div
                      class="playback-cursor"
                      classList={{ "preview-cursor": store.auditionLocalBeats() !== null }}
                      style={{ left: `${cursorX()}px` }}
                    />
                  </Show>

                  {!activePattern() && (
                    <div class="empty-state" style={{ height: "100%" }}>
                      <div class="empty-state-text">{i18n.t("pianoroll.emptyTitle")}</div>
                      <div class="empty-state-hint">{i18n.t("pianoroll.emptyHint")}</div>
                    </div>
                  )}

                </div>
              </div>
            </div>
            <Show when={showsVelocityLane()}>
            <VelocityLane
              blocks={laneBlocks()}
              hits={hitBlocks()}
              selected={selectedBlocks()}
              pxPerBeat={pxPerBeat()}
              xForBeat={xForLocalBeat}
              width={canvasWidth()}
              gutterWidth={keysWidth()}
              toolFailure={velocityToolFailure()}
              onSelect={(block, shiftKey) => selectNote(blockIdentity(block), shiftKey)}
              onSetVelocity={(block, velocity) => setVelocityAt(block.phraseIndex, block.noteIndex, velocity)}
              onSetVelocities={(blocks, velocity) => setVelocitiesAt(blocks, velocity)}
              onClearVelocity={(block) => clearVelocityAt(block.phraseIndex, block.noteIndex)}
              onHitSelect={(hit) => selectHit(selPattern()?.name ?? null, hit.hitIndex)}
              onSetHitVelocity={(hit, velocity) => setHitVelocityAt(hit.hitIndex, velocity)}
              onClearHitVelocity={(hit) => clearHitVelocityAt(hit.hitIndex)}
            />
            </Show>
            <Show when={showsArticulationLane()}>
            <ArticulationLane
              blocks={laneBlocks()}
              pxPerBeat={pxPerBeat()}
              xForBeat={xForLocalBeat}
              width={canvasWidth()}
              gutterWidth={keysWidth()}
              onSetArticulation={(block, articulation) => setArticulationAt(block.phraseIndex, block.noteIndex, articulation)}
              menuItems={(block) => articulationMenuItems(block.phraseIndex, block.noteIndex, block.articulation)}
            />
            </Show>
          </div>
        </div>
      </div>
    </div>
  );
}

// ── Note rendering sub-components ──

/** Lane line shown when a control failed or cannot act; silence is not a state. */
function LaneNotice(props: { text: string | null; danger?: boolean }) {
  return (
    <Show when={props.text}>
      <span
        class="pianoroll-lane-notice"
        classList={{ "lane-notice-danger": props.danger }}
        role={props.danger ? "alert" : "status"}
      >{props.text}</span>
    </Show>
  );
}

/**
 * The score-mark strip between the ruler and the note canvas: one wedge per
 * hairpin and one line per pedal hold.
 *
 * Display only: the DSL owns these marks, so there is no edit control, no
 * pending state and no refusal to state — the tooltip names what the engine
 * will play.
 */
function ScoreMarks(props: {
  hairpins: ScoreHairpin[];
  pedals: PedalSpan[];
  /** Maps a mark's beat to the grid column, the same mapping the notes use. */
  xForBeat: (beat: number) => number;
  /** Body width in px; a mark past it is clipped by the strip. */
  width: number;
}) {
  const left = (beat: number) => props.xForBeat(beat);
  const spanWidth = (start: number, end: number) =>
    Math.max(2, props.xForBeat(end) - props.xForBeat(start));
  const kindName = (span: ScoreHairpin) =>
    i18n.t(span.kind === "crescendo" ? "pianoroll.crescendo" : "pianoroll.diminuendo");

  return (
    <div
      class="pianoroll-score-marks"
      data-score-strip
      role="group"
      aria-label={i18n.t("pianoroll.scoreMarks")}
      style={{
        width: `${props.width}px`,
        height: `${SCORE_STRIP_HEIGHT}px`,
        position: "sticky",
        top: `${PIANO_ROLL_RULER_HEIGHT}px`,
        "z-index": "44",
      }}
    >
      <For each={props.hairpins}>
        {(span) => {
          const width = () => spanWidth(span.beat, span.endBeat);
          // A crescendo opens from a point on its left; a diminuendo closes to
          // one on its right.
          const points = () =>
            span.kind === "crescendo"
              ? `0,${SCORE_STRIP_HEIGHT / 2} ${width()},1 ${width()},${SCORE_STRIP_HEIGHT - 1}`
              : `0,1 0,${SCORE_STRIP_HEIGHT - 1} ${width()},${SCORE_STRIP_HEIGHT / 2}`;
          return (
            <svg
              class="pianoroll-hairpin"
              data-hairpin={span.kind}
              data-hairpin-start={span.beat}
              data-hairpin-end={span.endBeat}
              width={width()}
              height={SCORE_STRIP_HEIGHT}
              style={{ left: `${left(span.beat)}px` }}
              {...useTooltip(() => i18n.t("pianoroll.hairpinTooltip", {
                kind: kindName(span),
                from: span.startVelocity,
                to: span.targetVelocity,
                beats: (span.endBeat - span.beat).toFixed(2),
              }))}
            >
              <polygon points={points()} />
            </svg>
          );
        }}
      </For>
      <For each={props.pedals}>
        {(span) => (
          <div
            class="pianoroll-pedal"
            data-pedal
            data-pedal-start={span.start}
            data-pedal-end={span.end}
            style={{
              left: `${left(span.start)}px`,
              width: `${spanWidth(span.start, span.end)}px`,
            }}
            {...useTooltip(() => i18n.t("pianoroll.pedalTooltip", {
              start: span.start.toFixed(2),
              end: span.end.toFixed(2),
            }))}
          >
            <span class="pianoroll-pedal-label">{i18n.t("pianoroll.pedalDown")}</span>
            {/* The lift sign closes the hold at its right end. */}
            <span class="pianoroll-pedal-lift" aria-hidden="true">*</span>
          </div>
        )}
      </For>
    </div>
  );
}

/** Velocity guides: the values the lane's axis is labelled with. */
const VELOCITY_TICKS = [32, 64, 96, 127];
/** Bottom offset of a velocity inside the lane body, in px. */
function velocityTickBottom(velocity: number): number {
  const usable = VELOCITY_LANE_HEIGHT - VELOCITY_LANE_INSET * 2;
  return VELOCITY_LANE_INSET + (velocity / 127) * usable;
}

/**
 * Velocity lane pinned under the note grid. Bars share the grid's x mapping and
 * its scroll offset because the lane is the last row of the same scroll content;
 * a drag previews locally and commits one store command on release.
 *
 * The pointer maps against the lane body's usable box, never a bar's own box: a
 * bar's height is the value under edit, so aiming with it would rescale the axis
 * under the cursor. Ticks name the values, the gutter box types one, and
 * `Alt`+click drops the exact velocity so the written dynamic applies again.
 */
export function VelocityLane(props: {
  blocks: DisplayNoteBlock[];
  /** Drum hits drawn at their own loudness and writable by dragging. */
  hits: DisplayHitBlock[];
  /** Notes the user selected; the gutter box writes to all of them. */
  selected: DisplayNoteBlock[];
  pxPerBeat: number;
  xForBeat: (beat: number) => number;
  /** Width of the lane body in px; matches the note canvas. */
  width: number;
  /** Width of the left gutter; matches the note grid's key column. */
  gutterWidth?: number;
  /** A velocity tool's failure, stated where the bars are. */
  toolFailure: string | null;
  onSelect: (block: DisplayNoteBlock, shiftKey: boolean) => void;
  onSetVelocity: (block: DisplayNoteBlock, velocity: number) => Promise<string | null>;
  /** Write one value across the whole selection as a single undo entry. */
  onSetVelocities: (blocks: DisplayNoteBlock[], velocity: number) => Promise<string | null>;
  onClearVelocity: (block: DisplayNoteBlock) => Promise<string | null>;
  onHitSelect?: (hit: DisplayHitBlock) => void;
  onSetHitVelocity: (hit: DisplayHitBlock, velocity: number) => Promise<string | null>;
  onClearHitVelocity: (hit: DisplayHitBlock) => Promise<string | null>;
}) {
  let laneBody: HTMLDivElement | undefined;
  const [preview, setPreview] = createSignal<{ key: string; velocity: number } | null>(null);
  const [pending, setPending] = createSignal(false);
  /** Validation and write failures; the lane shows the newest one. */
  const [failure, setFailure] = createSignal<string | null>(null);
  /** The disabled reason, or nothing while the entry can act. */
  const [refusal, setRefusal] = createSignal<string | null>(null);

  const keyOf = (block: DisplayNoteBlock) => `${block.phraseIndex}:${block.noteIndex}`;
  const velocityOf = (block: DisplayNoteBlock) => {
    const current = preview();
    return current && current.key === keyOf(block) ? current.velocity : block.velocity;
  };
  const barWidth = (block: DisplayNoteBlock) =>
    Math.min(10, Math.max(3, block.durationBeats * props.pxPerBeat - 1));

  /** The usable lane box; a bar's own rect is never the axis. */
  const boxOf = () => velocityLaneBox(laneBody?.getBoundingClientRect() ?? { top: 0, height: 0 });

  const notice = () => {
    if (props.toolFailure) return { text: props.toolFailure, danger: true };
    const written = failure() ?? refusal();
    if (written) return { text: written, danger: failure() !== null };
    if (props.selected.length === 0) {
      return { text: i18n.t("pianoroll.velocityNoSelection"), danger: false };
    }
    return null;
  };

  /** One note's value; several selected notes show a blank that takes a typed one. */
  const entryValue = () => (props.selected.length === 1 ? String(props.selected[0].velocity) : "");
  const entryWritten = () => props.selected.length === 1 && props.selected[0].velocitySource !== "velocity";

  /** What the box means right now: a disabled reason, a warning, or what it does. */
  const entryTitle = () => {
    if (props.selected.length === 0) return i18n.t("pianoroll.velocityNoSelection");
    if (entryWritten()) return i18n.t("pianoroll.velocityEntryWrittenHint");
    return i18n.t("pianoroll.velocityEntryHint");
  };

  const commitTyped = async (raw: string) => {
    if (raw === "") return;
    const value = Number(raw);
    if (!Number.isInteger(value) || value < 1 || value > 127) {
      setRefusal(null);
      setFailure(i18n.t("pianoroll.velocityRange"));
      return;
    }
    if (props.selected.length === 0) return;
    setFailure(null);
    setRefusal(null);
    setPending(true);
    // Several notes go in one command, so a refusal on any of them leaves every
    // note unchanged and one undo entry covers the gesture. A single note keeps
    // its own command: it is already one entry.
    const message = props.selected.length > 1
      ? await props.onSetVelocities(props.selected, value)
      : await props.onSetVelocity(props.selected[0], value);
    setPending(false);
    if (message) setFailure(i18n.t("pianoroll.velocityWriteFailed", { message }));
  };

  /** A hit's bar height, showing the drag ghost while one is moving. */
  const hitVelocityOf = (hit: DisplayHitBlock) => {
    const current = preview();
    return current && current.key === `hit:${hit.hitIndex}` ? current.velocity : hit.velocity;
  };

  /** One drag writes one hit's exact velocity; Alt drops it back to its dynamic. */
  const startHitDrag = (event: MouseEvent, hit: DisplayHitBlock) => {
    if (isSecondaryClick(event)) return;
    event.preventDefault();
    event.stopPropagation();
    props.onHitSelect?.(hit);
    if (event.altKey) {
      if (hit.velocitySource !== "velocity") {
        setFailure(null);
        setRefusal(i18n.t("pianoroll.velocityAlreadyWritten"));
        return;
      }
      setFailure(null);
      setRefusal(null);
      setPending(true);
      void props.onClearHitVelocity(hit).then((message) => {
        setPending(false);
        if (message) setFailure(i18n.t("pianoroll.velocityClearFailed", { message }));
      });
      return;
    }
    let velocity = velocityAtClientY(event.clientY, boxOf());
    setPreview({ key: `hit:${hit.hitIndex}`, velocity });
    const onMove = (moveEvent: MouseEvent) => {
      velocity = velocityAtClientY(moveEvent.clientY, boxOf());
      setPreview({ key: `hit:${hit.hitIndex}`, velocity });
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setPreview(null);
      setPending(true);
      void props.onSetHitVelocity(hit, velocity).then((message) => {
        setPending(false);
        if (message) setFailure(i18n.t("pianoroll.velocityWriteFailed", { message }));
      });
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  const startDrag = (event: MouseEvent, block: DisplayNoteBlock) => {
    if (isSecondaryClick(event)) return;
    event.preventDefault();
    event.stopPropagation();
    props.onSelect(block, event.shiftKey);
    if (event.altKey) {
      // Alt drops the exact velocity; a note that already follows its written
      // dynamic has nothing to clear, so the lane says why nothing happened.
      if (block.velocitySource !== "velocity") {
        setFailure(null);
        setRefusal(i18n.t("pianoroll.velocityAlreadyWritten"));
        return;
      }
      setFailure(null);
      setRefusal(null);
      setPending(true);
      void props.onClearVelocity(block).then((message) => {
        setPending(false);
        if (message) setFailure(i18n.t("pianoroll.velocityClearFailed", { message }));
      });
      return;
    }
    let velocity = velocityAtClientY(event.clientY, boxOf());
    setPreview({ key: keyOf(block), velocity });
    const onMove = (moveEvent: MouseEvent) => {
      velocity = velocityAtClientY(moveEvent.clientY, boxOf());
      setPreview({ key: keyOf(block), velocity });
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setPreview(null);
      setPending(true);
      void props.onSetVelocity(block, velocity).then((message) => {
        setPending(false);
        if (message) setFailure(i18n.t("pianoroll.velocityWriteFailed", { message }));
      });
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  return (
    <div
      class="pianoroll-velocity-lane"
      role="group"
      aria-label={i18n.t("pianoroll.velocityLane")}
      style={{ height: `${VELOCITY_LANE_HEIGHT}px` }}
    >
      <div class="pianoroll-velocity-gutter" style={{ width: `${props.gutterWidth ?? PIANO_ROLL_KEYS_WIDTH}px` }}>
        <For each={VELOCITY_TICKS}>
          {(value) => (
            <span
              class="pianoroll-velocity-tick-label"
              data-velocity-tick={value}
              style={{ bottom: `${velocityTickBottom(value)}px` }}
            >{value}</span>
          )}
        </For>
        <span class="pianoroll-velocity-entry" classList={{ "lane-pending": pending() }}>
          <span class="pianoroll-velocity-gutter-label">{i18n.t("pianoroll.velocity")}</span>
          <input
            class="pianoroll-velocity-input"
            classList={{ "velocity-input-written": entryWritten() }}
            type="number"
            min="1"
            max="127"
            step="1"
            inputmode="numeric"
            aria-label={i18n.t("pianoroll.velocityEntry")}
            title={entryTitle()}
            disabled={props.selected.length === 0 || pending()}
            value={entryValue()}
            onChange={(event) => void commitTyped(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
            }}
          />
        </span>
      </div>
      <div
        ref={laneBody}
        class="pianoroll-velocity-body"
        style={{ width: `${props.width}px`, height: `${VELOCITY_LANE_HEIGHT}px` }}
      >
        <For each={VELOCITY_TICKS}>
          {(value) => (
            <div
              class="pianoroll-velocity-guide"
              data-velocity-guide={value}
              style={{ bottom: `${velocityTickBottom(value)}px` }}
            />
          )}
        </For>
        <For each={props.blocks}>
          {(block) => (
            <>
              <div
                class="pianoroll-velocity-bar"
                classList={{
                  "velocity-bar-explicit": block.velocitySource === "velocity",
                  "velocity-bar-muted": block.muted,
                  // A ramp moves the note, so its written bar is an outline and
                  // the ghost line below says what it plays at.
                  "velocity-bar-hollow": block.hairpin !== null,
                }}
                data-velocity-bar
                data-note-key={keyOf(block)}
                data-velocity-source={block.velocitySource}
                data-hairpin={block.hairpin?.kind}
                style={{
                  left: `${props.xForBeat(block.startBeat)}px`,
                  bottom: `${VELOCITY_LANE_INSET}px`,
                  width: `${barWidth(block)}px`,
                  height: `${velocityBarHeight(velocityOf(block))}px`,
                }}
                {...useTooltip(() => [
                  `${block.label} · ${velocityText(block.velocity, block.velocitySource, block.dynamics)}`,
                  block.hairpin
                    ? i18n.t("pianoroll.hairpinMoved", {
                        kind: i18n.t(block.hairpin.kind === "crescendo"
                          ? "pianoroll.crescendo"
                          : "pianoroll.diminuendo"),
                        from: block.hairpin.startVelocity,
                        to: block.hairpin.targetVelocity,
                      })
                    : "",
                ].filter(Boolean).join(" · "))}
                onMouseDown={(event) => startDrag(event, block)}
              />
              <Show when={block.ghostVelocity !== null}>
                <div
                  class="pianoroll-velocity-ghost"
                  data-velocity-ghost
                  data-ghost-velocity={block.ghostVelocity!}
                  aria-hidden="true"
                  style={{
                    left: `${props.xForBeat(block.startBeat)}px`,
                    bottom: `${velocityTickBottom(block.ghostVelocity!)}px`,
                    width: `${barWidth(block)}px`,
                  }}
                />
              </Show>
            </>
          )}
        </For>
        <For each={props.hits}>
          {(hit) => (
            <div
              class="pianoroll-velocity-bar velocity-bar-hit"
              classList={{
                "velocity-bar-explicit": hit.velocitySource === "velocity",
                "hit-out-of-range": hit.outOfRange,
              }}
              data-hit-bar
              data-hit-index={hit.hitIndex}
              data-velocity-source={hit.velocitySource}
              style={{
                left: `${props.xForBeat(hit.beat)}px`,
                bottom: `${VELOCITY_LANE_INSET}px`,
                width: `${Math.min(10, Math.max(3, props.pxPerBeat * 0.25 - 1))}px`,
                height: `${velocityBarHeight(hitVelocityOf(hit))}px`,
              }}
              {...useTooltip(() => [
                hit.label,
                velocityText(hitVelocityOf(hit), hit.velocitySource, hit.dynamics),
                hit.outOfRange ? i18n.t("pianoroll.outOfRangeNote") : "",
              ].filter(Boolean).join(" · "))}
              onMouseDown={(event) => startHitDrag(event, hit)}
            />
          )}
        </For>
        <LaneNotice text={notice()?.text ?? null} danger={notice()?.danger} />
      </div>
    </div>
  );
}

/**
 * Articulation lane pinned under the velocity lane: one chip at each note start.
 * A chip names the articulation and the key the engine will press; the chip menu
 * is the same 29-token list the note menu carries, and `Alt`+click clears.
 */
function ArticulationLane(props: {
  blocks: DisplayNoteBlock[];
  pxPerBeat: number;
  xForBeat: (beat: number) => number;
  width: number;
  /** Width of the left gutter; matches the note grid's key column. */
  gutterWidth?: number;
  onSetArticulation: (block: DisplayNoteBlock, articulation: string | null) => Promise<string | null>;
  /** The 29-token rows, shared with the note context menu. */
  menuItems: (block: DisplayNoteBlock) => ContextMenuItem[];
}) {
  const [pendingKey, setPendingKey] = createSignal<string | null>(null);
  const [failure, setFailure] = createSignal<{ text: string; danger: boolean } | null>(null);

  const keyOf = (block: DisplayNoteBlock) => `${block.phraseIndex}:${block.noteIndex}`;
  const chipWidth = (block: DisplayNoteBlock) =>
    Math.min(30, Math.max(18, block.durationBeats * props.pxPerBeat - 1));

  /** `staccato · keyswitch C0`, or the honest absence for this preset. */
  const summary = (block: DisplayNoteBlock): string => {
    const token = articulationToken(block.articulation);
    if (!token) return i18n.t("menu.note.articulationNone");
    return block.keyswitch
      ? i18n.t("pianoroll.keyswitchTooltip", { token, note: block.keyswitch.label })
      : i18n.t("pianoroll.keyswitchNone", { token });
  };

  const write = async (block: DisplayNoteBlock, articulation: string | null) => {
    setFailure(null);
    setPendingKey(keyOf(block));
    const message = await props.onSetArticulation(block, articulation);
    setPendingKey(null);
    if (message) {
      setFailure({
        text: i18n.t(articulation ? "pianoroll.articulationSetFailed" : "pianoroll.articulationClearFailed", { message }),
        danger: true,
      });
    }
  };

  const clear = async (block: DisplayNoteBlock) => {
    if (!block.articulation) {
      setFailure({ text: i18n.t("pianoroll.noArticulationToClear"), danger: false });
      return;
    }
    await write(block, null);
  };

  const openMenu = (event: MouseEvent, block: DisplayNoteBlock) => {
    openContextMenu({
      x: event.clientX,
      y: event.clientY,
      className: "pianoroll-note-menu",
      ariaLabel: i18n.t("pianoroll.articulationMenu"),
      items: [
        { type: "heading", label: summary(block) },
        ...props.menuItems(block),
      ],
    });
  };

  return (
    <div
      class="pianoroll-articulation-lane"
      role="group"
      aria-label={i18n.t("pianoroll.articulationLane")}
      aria-busy={pendingKey() !== null}
      style={{ height: `${ARTICULATION_LANE_HEIGHT}px` }}
    >
      <div class="pianoroll-articulation-gutter" style={{ width: `${props.gutterWidth ?? PIANO_ROLL_KEYS_WIDTH}px` }}>
        <span class="pianoroll-velocity-gutter-label">{i18n.t("pianoroll.articulation")}</span>
      </div>
      <div
        class="pianoroll-articulation-body"
        style={{ width: `${props.width}px`, height: `${ARTICULATION_LANE_HEIGHT}px` }}
      >
        <For each={props.blocks}>
          {(block) => (
            <button
              type="button"
              class="pianoroll-articulation-chip"
              classList={{
                "articulation-chip-empty": !block.articulation,
                "articulation-chip-muted": block.muted,
                "lane-pending": pendingKey() === keyOf(block),
              }}
              data-articulation-chip
              data-note-key={keyOf(block)}
              data-articulation={articulationToken(block.articulation) || undefined}
              data-keyswitch={block.keyswitch?.label}
              aria-label={summary(block)}
              aria-busy={pendingKey() === keyOf(block)}
              style={{
                left: `${props.xForBeat(block.startBeat)}px`,
                width: `${chipWidth(block)}px`,
              }}
              {...useTooltip(() => summary(block))}
              onClick={(event) => {
                if (event.altKey) { void clear(block); return; }
                openMenu(event, block);
              }}
            >{articulationTag(block.articulation)}</button>
          )}
        </For>
        <LaneNotice text={failure()?.text ?? null} danger={failure()?.danger} />
      </div>
    </div>
  );
}

function PatternDefNotes(props: {
  patternDef: DisplayPatternDef;
  /** Preset params the keyswitch resolver reads; null when no preset is known. */
  presetParams: [string, ParamValue][] | null;
  getPxPerBeat: () => number;
  xForBeat: (beat: number) => number;
  /** Canvas width in px; notes at or past it are outside the opened clip. */
  viewWidth: number;
  /** Pattern- or clip-local end placement may use; undefined in a track view. */
  lengthBeats?: number;
  /** The drum lane's rows; null keeps the semitone pitch rows. */
  rows: DrumLaneRow[] | null;
  onDragStart: (e: MouseEvent, phraseIdx: number, noteIdx: number, startBeat: number, durationBeats: number, midi: number, phraseOffsetBeats?: number) => void;
  onResizeStart: (e: MouseEvent, phraseIdx: number, noteIdx: number, startBeat: number, durationBeats: number, midi: number, phraseOffsetBeats?: number) => void;
  onNoteRightClick: (e: MouseEvent, phraseIdx: number, noteIdx: number) => void;
  /** A right-drag eraser owns the pointer; the note menu stays shut. */
  erasing: () => boolean;
  onNoteSelect: (identity: NoteIdentity, shiftKey: boolean) => void;
  dragState: { noteIndex: number; phraseIndex: number; mode: string; noteMidi: number } | null;
  dragOffsetX: number;
  dragPitch: number;
  gridDiv: () => number;
  isNoteSelected: (phraseIdx: number, noteIdx: number) => boolean;
  /** True when the open preview changed the subtree this note belongs to. */
  previewChanged: (startBeat: number) => boolean;
  tool: () => "select" | "draw";
}) {
  const notes = createMemo(() =>
    visibleNoteBlocks(
      props.patternDef,
      props.xForBeat,
      props.viewWidth,
      props.presetParams,
      props.lengthBeats,
      props.rows,
    ),
  );

  /** Top of a note's row; a lane has no row for a pitch it does not hold. */
  const topOf = (midi: number): number | null => {
    const rows = props.rows;
    if (!rows) return midiRowTop(midi);
    const index = rows.findIndex((row) => row.note === midi);
    return index < 0 ? null : index * NOTE_HEIGHT;
  };

  const DS = () => props.dragState;
  const DOX = () => props.dragOffsetX;
  const ppb = () => props.getPxPerBeat();

  return (
    <For each={notes()}>
      {(note) => {
        if (topOf(note.midi) === null) return null;
        const isDragging = () => DS()?.noteIndex === note.noteIndex && DS()?.phraseIndex === note.phraseIndex;
        const isMove = () => isDragging() && DS()?.mode === "move";
        const isResize = () => isDragging() && DS()?.mode === "resize";
        const isSelected = () => props.isNoteSelected(note.phraseIndex, note.noteIndex);

        const left = () => {
          const base = props.xForBeat(note.startBeat);
          return isMove() ? base + DOX() : base;
        };
        const width = () => {
          const base = Math.max(note.durationBeats * ppb() - 1, 4);
          return isResize() ? Math.max(4, base + DOX()) : base;
        };
        const top = topOf(note.midi) ?? 0;
        const velPct = note.velocity / 127;
        const tag = () => articulationTag(note.articulation);
        const tooltip = () => [
          note.label,
          velocityText(note.velocity, note.velocitySource, note.dynamics),
          `${note.durationBeats.toFixed(2)}b`,
          articulationToken(note.articulation),
          note.tuplet ? i18n.t("pianoroll.tupletTooltip", { count: note.tuplet[0], normal: note.tuplet[1] }) : "",
          note.gliss ? i18n.t("pianoroll.glissTooltip") : "",
          note.grace ? "grace" : "",
          note.muted ? "muted" : "",
          note.outOfRange ? i18n.t("pianoroll.outOfRangeNote") : "",
          isDragging() && isMove() ? `\u2192 ${((note.startBeat + DOX() / ppb())).toFixed(2)}b` : "",
        ].filter(Boolean).join(" \u00b7 ");

        return (
          <>
            {/* Ghost — original position during drag */}
            {isDragging() && (
              <div
                class="pianoroll-note note-ghost"
                style={{
                  left: `${props.xForBeat(note.startBeat)}px`, top: `${top}px`,
                  width: `${Math.max(note.durationBeats * ppb() - 1, 4)}px`,
                  height: `${NOTE_HEIGHT - 1}px`,
                }}
              />
            )}
            {/* Main note */}
            <div
              class="pianoroll-note"
              classList={{
                "note-dragging": isDragging(),
                "note-muted": note.muted,
                "note-selected": isSelected(),
                "note-velocity-explicit": note.velocitySource === "velocity",
                "note-tuplet": note.tuplet !== null,
                "note-out-of-range": note.outOfRange,
                "preview-changed": props.previewChanged(note.startBeat),
              }}
              data-note-key={`${note.phraseIndex}:${note.noteIndex}`}
              data-phrase-index={note.phraseIndex}
              data-note-index={note.noteIndex}
              data-velocity-source={note.velocitySource}
              data-articulation={articulationToken(note.articulation) || undefined}
              style={{
                left: `${left()}px`, top: `${top - (isMove() ? props.dragPitch * NOTE_HEIGHT : 0)}px`,
                width: `${width()}px`, height: `${NOTE_HEIGHT - 1}px`,
                opacity: note.muted ? 0.35 : 1,
                cursor: props.tool() === "draw" ? "pointer" : "grab",
              }}
              {...useTooltip(tooltip)}
              onClick={(e) => e.stopPropagation()}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                // The eraser gesture decides at pointer-up whether this was a
                // menu press or an erase.
                if (props.erasing()) return;
                props.onNoteRightClick(e, note.phraseIndex, note.noteIndex);
              }}
              onMouseDown={(e) => {
                if (isSecondaryClick(e)) return;
                const target = e.target as HTMLElement;
                const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
                const nearRightEdge = rect.width > 0 && e.clientX >= rect.right - 8;
                if (target.closest(".note-resize-handle") || nearRightEdge) {
                  props.onResizeStart(e, note.phraseIndex, note.noteIndex, note.startBeat, note.durationBeats, note.midi, note.phraseOffsetBeats);
                } else {
                  props.onDragStart(e, note.phraseIndex, note.noteIndex, note.startBeat, note.durationBeats, note.midi, note.phraseOffsetBeats);
                }
              }}
            >
              <span class="pianoroll-note-label">
                {width() > 20 ? note.label : ""}
              </span>
              <Show when={note.tuplet}>
                <span
                  class="pianoroll-note-tuplet"
                  data-tuplet={`${note.tuplet![0]}:${note.tuplet![1]}`}
                  {...useTooltip(() => i18n.t("pianoroll.tupletTooltip", {
                    count: note.tuplet![0],
                    normal: note.tuplet![1],
                  }))}
                >{note.tuplet![0]}</span>
              </Show>
              <Show when={note.gliss}>
                <svg
                  class="pianoroll-note-gliss"
                  data-note-gliss
                  viewBox="0 0 10 10"
                  width="10"
                  height={NOTE_HEIGHT - 1}
                  aria-hidden="true"
                >
                  <path d="M1 8.5 L9 1.5" />
                </svg>
              </Show>
              <div
                class="pianoroll-note-vel"
                classList={{ "vel-explicit": note.velocitySource === "velocity" }}
                style={{ width: `${velPct * 100}%` }}
              />
              <Show when={tag() && width() > 26}>
                <span class="pianoroll-note-tag" data-articulation-tag={tag()}>{tag()}</span>
              </Show>
              <div
                class="note-resize-handle"
                onMouseDown={(e) => {
                  if (isSecondaryClick(e)) return;
                  props.onResizeStart(e, note.phraseIndex, note.noteIndex, note.startBeat, note.durationBeats, note.midi, note.phraseOffsetBeats);
                }}
              />
            </div>
          </>
        );
      }}
    </For>
  );
}

function PatternDefHits(props: {
  hits: DisplayDrumHit[];
  xForBeat: (beat: number) => number;
  /** The drum lane's rows; null keeps the General MIDI pitch rows. */
  rows: DrumLaneRow[] | null;
  isHitSelected: (patternName: string | null, hitIndex: number) => boolean;
  onHitSelect: (patternName: string | null, hitIndex: number) => void;
  onHitRightClick: (e: MouseEvent, patternName: string | null, hitIndex: number) => void;
  /** A right-drag eraser owns the pointer; the hit menu stays shut. */
  erasing: () => boolean;
}) {
  /** Top of the row a hit lights; a row the open lane does not hold is not drawn. */
  const rowTop = (note: number): number | null => {
    const rows = props.rows;
    if (!rows) return midiRowTop(note);
    const index = rows.findIndex((row) => row.note === note);
    return index < 0 ? null : index * NOTE_HEIGHT;
  };

  return (
    <For each={props.hits}>
      {(hit) => {
        const top = () => rowTop(hit.note);
        const left = () => props.xForBeat(hit.beat);
        const size = Math.min(NOTE_HEIGHT - 2, 8);
        return (
          <Show when={top() !== null}>
            <div
              class="pianoroll-drum-hit"
              classList={{
                "hit-out-of-range": hit.outOfRange,
                "hit-selected": props.isHitSelected(hit.patternName, hit.hitIndex),
              }}
              data-hit-row-note={String(hit.note)}
              data-hit-index={hit.hitIndex}
              data-hit-pattern={hit.patternName ?? ""}
              data-hit-key={`${hit.patternName ?? ""}:${hit.hitIndex}`}
              style={{
                left: `${left() - size / 2}px`,
                top: `${(top() ?? 0) + (NOTE_HEIGHT - size) / 2}px`,
                width: `${size}px`, height: `${size}px`,
              }}
              {...useTooltip(() => [
                hit.label,
                hit.outOfRange ? i18n.t("pianoroll.outOfRangeNote") : "",
                i18n.t("menu.hit.actions"),
              ].filter(Boolean).join("\n"))}
              onMouseDown={(event) => event.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                props.onHitSelect(hit.patternName, hit.hitIndex);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                if (props.erasing()) return;
                props.onHitRightClick(e, hit.patternName, hit.hitIndex);
              }}
            />
          </Show>
        );
      }}
    </For>
  );
}
