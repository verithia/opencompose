/**
 * Pure editing helpers for the timeline tempo/meter lane.
 *
 * Positions are quarter notes on the same checked map the engine and the
 * transport use. Every mutation returns a new map array; the project store
 * persists it as one undoable command.
 */
import type {
  BarBeat,
  ProjectProperties,
  TempoPoint,
  TimeSigPoint,
  TimeSignature,
} from "../types/project";
import { createProjectClock } from "./projectClock";
import { snapBlockQuarter } from "../stores/grid";

/** The GUI keeps the toolbar's 20 BPM floor; the engine accepts 1–400. */
export const TEMPO_MIN_BPM = 20;
export const TEMPO_MAX_BPM = 400;
export const METER_NUMERATOR_MAX = 32;
export const METER_DENOMINATORS = [1, 2, 4, 8, 16, 32] as const;
export const TEMPO_LANE_ROW_HEIGHT = 17;

export type LaneError =
  | "invalidTempo"
  | "invalidMeter"
  | "invalidPosition"
  | "duplicatePosition"
  | "outOfRange";

export type LaneEdit<T> =
  | { ok: true; points: T[]; index: number }
  | { ok: false; error: LaneError };

export interface BarMark {
  bar: number;
  quarter: number;
}

type Clock = ReturnType<typeof createProjectClock>;

export function isValidTempo(bpm: number): boolean {
  return Number.isFinite(bpm) && bpm >= TEMPO_MIN_BPM && bpm <= TEMPO_MAX_BPM;
}

export function isValidMeter(numerator: number, denominator: number): boolean {
  return Number.isInteger(numerator)
    && numerator >= 1
    && numerator <= METER_NUMERATOR_MAX
    && (METER_DENOMINATORS as readonly number[]).includes(denominator);
}

export function formatTempo(bpm: number): string {
  return Number.isInteger(bpm) ? String(bpm) : bpm.toFixed(2).replace(/\.?0+$/, "");
}

export function formatMeter(numerator: number, denominator: number): string {
  return `${numerator}/${denominator}`;
}

function safeClock(properties: ProjectProperties): Clock | null {
  try {
    return createProjectClock(properties);
  } catch {
    return null;
  }
}

/** Absolute quarter of a tempo point, including its fine residual. */
export function tempoPointQuarter(clock: Clock, point: TempoPoint): number {
  return clock.quarterAt(point.position) + (point.offset_quarters ?? 0);
}

function sameQuarter(a: number, b: number): boolean {
  return Math.abs(a - b) < 1e-9;
}

export function tempoAtQuarter(properties: ProjectProperties, quarter: number): number {
  const clock = safeClock(properties);
  return clock ? clock.tempoAt(Math.max(0, quarter)) : properties.tempo;
}

export function meterAtBar(properties: ProjectProperties, bar: number): TimeSignature {
  const clock = safeClock(properties);
  return clock ? clock.meterAtBar(bar) : properties.time_signature;
}

export function meterAtQuarter(properties: ProjectProperties, quarter: number): TimeSignature {
  const clock = safeClock(properties);
  if (!clock) return properties.time_signature;
  return clock.meterAtBar(clock.barBeatAt(Math.max(0, quarter)).bar);
}

/** Bar starts on the current meter map, for ruler labels that must relabel. */
export function barMarks(properties: ProjectProperties, count: number): BarMark[] {
  const clock = safeClock(properties);
  const marks: BarMark[] = [];
  for (let bar = 1; bar <= Math.max(1, count); bar++) {
    marks.push({ bar, quarter: clock ? clock.barStartQuarter(bar) : (bar - 1) * 4 });
  }
  return marks;
}

export function insertTempoPoint(
  properties: ProjectProperties,
  quarter: number,
  bpm: number,
  division: number,
): LaneEdit<TempoPoint> {
  if (!isValidTempo(bpm)) return { ok: false, error: "invalidTempo" };
  const clock = safeClock(properties);
  if (!clock) return { ok: false, error: "invalidPosition" };
  const target = Math.max(0, snapBlockQuarter(quarter, division));
  if (!Number.isFinite(target)) return { ok: false, error: "invalidPosition" };
  let position: BarBeat;
  try {
    position = clock.barBeatAt(target);
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  const points = properties.tempo_map ?? [];
  if (points.some((point) => sameQuarter(tempoPointQuarter(clock, point), target))) {
    return { ok: false, error: "duplicatePosition" };
  }
  const point: TempoPoint = { position, offset_quarters: 0, bpm };
  const next = [...points, point].sort(
    (a, b) => tempoPointQuarter(clock, a) - tempoPointQuarter(clock, b),
  );
  try {
    createProjectClock({ ...properties, tempo_map: next });
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  return { ok: true, points: next, index: next.indexOf(point) };
}

export function moveTempoPoint(
  properties: ProjectProperties,
  index: number,
  quarter: number,
  division: number,
): LaneEdit<TempoPoint> {
  const points = properties.tempo_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  const clock = safeClock(properties);
  if (!clock) return { ok: false, error: "invalidPosition" };
  const target = Math.max(0, snapBlockQuarter(quarter, division));
  if (!Number.isFinite(target)) return { ok: false, error: "invalidPosition" };
  let position: BarBeat;
  try {
    position = clock.barBeatAt(target);
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  const moved: TempoPoint = { ...points[index], position, offset_quarters: 0 };
  const rest = points.filter((_, at) => at !== index);
  if (rest.some((point) => sameQuarter(tempoPointQuarter(clock, point), target))) {
    return { ok: false, error: "duplicatePosition" };
  }
  const next = rest.slice();
  const at = next.findIndex((point) => tempoPointQuarter(clock, point) > target);
  if (at === -1) next.push(moved);
  else next.splice(at, 0, moved);
  try {
    createProjectClock({ ...properties, tempo_map: next });
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  return { ok: true, points: next, index: next.indexOf(moved) };
}

export function setTempoPointBpm(
  properties: ProjectProperties,
  index: number,
  bpm: number,
): LaneEdit<TempoPoint> {
  const points = properties.tempo_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  if (!isValidTempo(bpm)) return { ok: false, error: "invalidTempo" };
  const next = points.map((point, at) => (at === index ? { ...point, bpm } : point));
  return { ok: true, points: next, index };
}

export function removeTempoPoint(
  properties: ProjectProperties,
  index: number,
): LaneEdit<TempoPoint> {
  const points = properties.tempo_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  const next = points.filter((_, at) => at !== index);
  return { ok: true, points: next, index: next.length === 0 ? -1 : Math.min(index, next.length - 1) };
}

export function insertTimeSigPoint(
  properties: ProjectProperties,
  bar: number,
  numerator: number,
  denominator: number,
): LaneEdit<TimeSigPoint> {
  if (!isValidMeter(numerator, denominator)) return { ok: false, error: "invalidMeter" };
  if (!Number.isInteger(bar) || bar < 1) return { ok: false, error: "invalidPosition" };
  const points = properties.time_sig_map ?? [];
  if (points.some((point) => point.position.bar === bar)) {
    return { ok: false, error: "duplicatePosition" };
  }
  const point: TimeSigPoint = { position: { bar, beat: 1, subdivision: 0 }, numerator, denominator };
  const next = [...points, point].sort((a, b) => a.position.bar - b.position.bar);
  try {
    createProjectClock({ ...properties, time_sig_map: next });
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  return { ok: true, points: next, index: next.indexOf(point) };
}

export function moveTimeSigPoint(
  properties: ProjectProperties,
  index: number,
  quarter: number,
): LaneEdit<TimeSigPoint> {
  const points = properties.time_sig_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  const clock = safeClock(properties);
  if (!clock) return { ok: false, error: "invalidPosition" };
  if (!Number.isFinite(quarter)) return { ok: false, error: "invalidPosition" };
  let bar: number;
  try {
    bar = clock.barBeatAt(Math.max(0, quarter)).bar;
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  if (bar < 1) bar = 1;
  const rest = points.filter((_, at) => at !== index);
  if (rest.some((point) => point.position.bar === bar)) {
    return { ok: false, error: "duplicatePosition" };
  }
  const moved: TimeSigPoint = { ...points[index], position: { bar, beat: 1, subdivision: 0 } };
  const next = rest.slice();
  const at = next.findIndex((point) => point.position.bar > bar);
  if (at === -1) next.push(moved);
  else next.splice(at, 0, moved);
  try {
    createProjectClock({ ...properties, time_sig_map: next });
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  return { ok: true, points: next, index: next.indexOf(moved) };
}

export function setTimeSigPointValue(
  properties: ProjectProperties,
  index: number,
  numerator: number,
  denominator: number,
): LaneEdit<TimeSigPoint> {
  const points = properties.time_sig_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  if (!isValidMeter(numerator, denominator)) return { ok: false, error: "invalidMeter" };
  const next = points.map((point, at) =>
    at === index ? { ...point, numerator, denominator } : point,
  );
  try {
    createProjectClock({ ...properties, time_sig_map: next });
  } catch {
    return { ok: false, error: "invalidPosition" };
  }
  return { ok: true, points: next, index };
}

export function removeTimeSigPoint(
  properties: ProjectProperties,
  index: number,
): LaneEdit<TimeSigPoint> {
  const points = properties.time_sig_map ?? [];
  if (index < 0 || index >= points.length) return { ok: false, error: "outOfRange" };
  const next = points.filter((_, at) => at !== index);
  return { ok: true, points: next, index: next.length === 0 ? -1 : Math.min(index, next.length - 1) };
}
