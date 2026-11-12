/**
 * The marks a score writes over notes and the roll otherwise swallows.
 *
 * A hairpin is not a per-note value: it expands to a track gain ramp that moves
 * between the dynamic in force at the mark and the next written dynamic (or the
 * phrase end). This module resolves that span and the level it reaches, so the
 * roll can draw the wedge and the level a note inside it actually plays at.
 *
 * Mirrors `oc_dsl::dynamics` (span resolution, the pp/f fallbacks, the linear
 * decibel ramp); keep the two in step.
 */
import {
  dynamicsToVelocity,
  durationToBeats,
  type HairpinMark,
  type NoteEvent,
  type PedalMark,
} from "../types/project";

/** Velocity of `pp`: the start of a `cresc` with no dynamic before it. */
const PP_VELOCITY = 33;
/** Velocity of `f`: the target of a `cresc` with no dynamic after it. */
const F_VELOCITY = 96;

/** A hairpin resolved to the span and levels its ramp moves between. */
export interface ScoreHairpin extends HairpinMark {
  /** Absolute beat the wedge ends on: the next written dynamic, else the content end. */
  endBeat: number;
  /** Velocity in force at the mark. */
  startVelocity: number;
  /** Velocity the wedge swells to. */
  targetVelocity: number;
}

/** One pedal hold, from the `down` mark to the `up` mark that lifts it. */
export interface PedalSpan {
  start: number;
  end: number;
}

function isRest(event: NoteEvent): boolean {
  return (event.modifiers ?? []).some(([key]) => key === "rest");
}

/** The velocity a note writes, from its exact value or its dynamic token. */
function writtenVelocity(event: NoteEvent): number | null {
  if (event.velocity !== null && event.velocity !== undefined) return event.velocity;
  return event.dynamics ? dynamicsToVelocity(event.dynamics) : null;
}

/** Absolute end of a phrase's content: its written range, else its last note end. */
export function phraseEndBeat(
  events: readonly NoteEvent[],
  rangeEnd: number | null,
  phraseStart: number,
): number {
  if (rangeEnd !== null) return rangeEnd;
  return events.reduce(
    (end, event) => Math.max(end, phraseStart + event.start_beat + durationToBeats(event.duration)),
    phraseStart,
  );
}

/**
 * Resolve a phrase's hairpins into the spans the engine's ramp uses.
 *
 * `events` carry phrase-local starts; the result is in absolute beats. A mark
 * the engine would refuse (no note after it, no level change) is left out
 * rather than drawn as a wedge that cannot sound.
 */
export function phraseHairpins(
  hairpins: readonly HairpinMark[],
  events: readonly NoteEvent[],
  phraseStart: number,
  phraseEnd: number,
): ScoreHairpin[] {
  if (hairpins.length === 0) return [];
  const sounding = events.filter((event) => !isRest(event) && !event.muted);
  const spans: ScoreHairpin[] = [];
  for (const mark of [...hairpins].sort((a, b) => a.beat - b.beat)) {
    const after = sounding.filter((event) => event.start_beat > mark.beat + 1e-9);
    if (after.length === 0) continue;
    const next = after
      .filter((event) => writtenVelocity(event) !== null)
      .reduce<NoteEvent | null>(
        (best, event) => (best === null || event.start_beat < best.start_beat ? event : best),
        null,
      );
    const end = next ? next.start_beat : phraseEnd - phraseStart;
    if (end <= mark.beat + 1e-9) continue;
    const inForce = sounding
      .filter((event) => event.start_beat <= mark.beat + 1e-9 && writtenVelocity(event) !== null)
      .reduce<NoteEvent | null>(
        (last, event) =>
          last === null || event.start_beat >= last.start_beat ? event : last,
        null,
      );
    const [fallbackStart, fallbackTarget] =
      mark.kind === "crescendo" ? [PP_VELOCITY, F_VELOCITY] : [F_VELOCITY, PP_VELOCITY];
    const startVelocity = inForce ? writtenVelocity(inForce)! : fallbackStart;
    const targetVelocity = next ? writtenVelocity(next)! : fallbackTarget;
    if (startVelocity === targetVelocity) continue;
    spans.push({
      beat: phraseStart + mark.beat,
      kind: mark.kind,
      endBeat: phraseStart + end,
      startVelocity,
      targetVelocity,
    });
  }
  return spans;
}

/**
 * Resolve a pattern's own hairpins into display spans.
 *
 * A pattern's marks are already in its local beats, so `phraseHairpins` runs
 * with a zero start. A placement on a track expands the same marks against the
 * placement's offset instead.
 */
export function patternHairpins(
  hairpins: readonly HairpinMark[],
  events: readonly NoteEvent[],
  contentEnd: number,
): ScoreHairpin[] {
  return phraseHairpins(hairpins, events, 0, contentEnd);
}

/** The hairpin whose span covers `beat`, or null when none does. */
export function hairpinAt(spans: readonly ScoreHairpin[], beat: number): ScoreHairpin | null {
  return spans.find((span) => beat >= span.beat - 1e-9 && beat < span.endBeat - 1e-9) ?? null;
}

/**
 * The level a hairpin's ramp has reached at `beat`, given the level the note
 * starts from.
 *
 * The engine's ramp is linear in decibels, so the gain is
 * `(target / start) ^ fraction` in amplitude; a velocity-equivalent level is
 * therefore geometric between the two, scaled by whatever the note itself
 * writes. This is the ramp the engine uses, not a curve of the roll's own.
 */
export function rampedVelocity(span: ScoreHairpin, beat: number, base: number): number {
  const length = span.endBeat - span.beat;
  if (length <= 0) return clampVelocity(base);
  const fraction = Math.min(1, Math.max(0, (beat - span.beat) / length));
  const ratio = span.targetVelocity / span.startVelocity;
  return clampVelocity(base * Math.pow(ratio, fraction));
}

function clampVelocity(velocity: number): number {
  return Math.min(127, Math.max(1, Math.round(velocity)));
}

/**
 * Pair `pedal down` with `pedal up` into holds. An up mark with no down mark is
 * ignored, and a down mark nothing lifts holds until `endBeat`.
 */
export function pedalSpans(pedals: readonly PedalMark[], endBeat: number): PedalSpan[] {
  const spans: PedalSpan[] = [];
  let open: number | null = null;
  for (const mark of [...pedals].sort((a, b) => a.beat - b.beat)) {
    if (mark.down) {
      if (open === null) open = mark.beat;
      continue;
    }
    if (open !== null && mark.beat > open) {
      spans.push({ start: open, end: mark.beat });
    }
    open = null;
  }
  if (open !== null) {
    spans.push({ start: open, end: Math.max(endBeat, open) });
  }
  return spans;
}
