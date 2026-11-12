import type { BarBeat, ProjectProperties, TimeSignature } from "../types/project";

/** A clock built from one document revision's properties. */
export type ProjectClock = ReturnType<typeof createProjectClock>;

/** Clocks built since the last reset; the timeline layout benchmark reads it. */
let builds = 0;

/** How many clocks `createProjectClock` has built since `resetClockBuilds`. */
export function clockBuildCount(): number { return builds; }

export function resetClockBuilds(): void { builds = 0; }

/** Clocks cached by `properties` identity. A snapshot replaces that object, so
 * the identity is the document revision. */
const clocks = new WeakMap<ProjectProperties, ProjectClock>();

/** The last segment whose key is at or before `target`; the lists are ascending. */
function lastAtOrBefore<T>(list: readonly T[], key: (item: T) => number, target: number): T {
  for (let i = list.length - 1; i >= 0; i--) {
    if (key(list[i]) <= target) return list[i];
  }
  throw new Error("Invalid clock position");
}

/** Musical-axis companion to Rust ProjectClock. Coordinates are quarter notes;
 * BarBeat beats use the meter denominator. Audio sample quantization stays Rust-owned. */
export function createProjectClock(properties: ProjectProperties) {
  const validMeter = (m: TimeSignature) => Number.isInteger(m.numerator) && m.numerator >= 1 && m.numerator <= 32 && [1,2,4,8,16,32,64].includes(m.denominator);
  const validTempo = (v: number) => Number.isFinite(v) && v >= 1 && v <= 400;
  if (!validMeter(properties.time_signature) || !validTempo(properties.tempo)) throw new Error("Invalid project clock");
  const meters = [{ bar: 1, quarter: 0, meter: properties.time_signature }];
  let previousBar = 0;
  for (const point of properties.time_sig_map ?? []) {
    const bar = point.position.bar;
    if (!Number.isInteger(bar) || bar < 1 || bar <= previousBar || point.position.beat !== 1 || point.position.subdivision !== 0 || !validMeter(point)) throw new Error("Invalid meter map");
    const last = meters[meters.length - 1];
    const segment = { bar, quarter: last.quarter + (bar-last.bar)*last.meter.numerator*4/last.meter.denominator, meter: point };
    if (bar === 1) meters[0] = segment; else meters.push(segment);
    previousBar = bar;
  }
  const quarterAt = (p: BarBeat): number => {
    if (!Number.isInteger(p.bar) || p.bar < 1) throw new Error("Invalid bar");
    const m = lastAtOrBefore(meters, (m) => m.bar, p.bar);
    if (!Number.isInteger(p.beat) || p.beat < 1 || p.beat > m.meter.numerator || !Number.isInteger(p.subdivision) || p.subdivision < 0 || p.subdivision >= 4) throw new Error("Invalid beat/subdivision");
    return m.quarter + ((p.bar-m.bar)*m.meter.numerator + p.beat-1 + p.subdivision/4)*4/m.meter.denominator;
  };
  const tempos = [{ quarter: 0, seconds: 0, bpm: properties.tempo }];
  let previousQuarter = -1;
  for (const point of properties.tempo_map ?? []) {
    const offset = point.offset_quarters ?? 0;
    const meter = lastAtOrBefore(meters, (m) => m.bar, point.position.bar);
    if (!Number.isFinite(offset) || offset < 0 || offset >= 1/meter.meter.denominator) throw new Error("Invalid tempo residual");
    const quarter = quarterAt(point.position) + offset;
    if (quarter <= previousQuarter || !validTempo(point.bpm)) throw new Error("Invalid tempo map");
    const last = tempos[tempos.length-1];
    const segment = { quarter, seconds: last.seconds+(quarter-last.quarter)*60/last.bpm, bpm: point.bpm };
    if (quarter === 0) tempos[0] = segment; else tempos.push(segment);
    previousQuarter = quarter;
  }
  const nonnegative = (v: number) => { if (!Number.isFinite(v) || v < 0) throw new Error("Invalid clock position"); };
  const secondsAt = (quarter: number): number => {
    nonnegative(quarter);
    const t = lastAtOrBefore(tempos, (t) => t.quarter, quarter);
    return t.seconds+(quarter-t.quarter)*60/t.bpm;
  };
  const quarterAtSeconds = (seconds: number): number => {
    nonnegative(seconds);
    const t = lastAtOrBefore(tempos, (t) => t.seconds, seconds);
    return t.quarter+(seconds-t.seconds)*t.bpm/60;
  };
  const barBeatAt = (quarter: number): BarBeat => {
    nonnegative(quarter);
    const m = lastAtOrBefore(meters, (m) => m.quarter, quarter);
    const ticks = Math.floor((quarter-m.quarter)*m.meter.denominator + 1e-9);
    const ticksPerBar = m.meter.numerator*4;
    return { bar: m.bar+Math.floor(ticks/ticksPerBar), beat: Math.floor((ticks%ticksPerBar)/4)+1, subdivision: ticks%4 };
  };
  const meterAtBar = (bar: number): TimeSignature => {
    if (!Number.isInteger(bar) || bar < 1) throw new Error("Invalid bar");
    const meter = lastAtOrBefore(meters, (m) => m.bar, bar).meter;
    return { numerator: meter.numerator, denominator: meter.denominator };
  };
  const barStartQuarter = (bar: number): number => quarterAt({ bar, beat: 1, subdivision: 0 });
  const tempoAt = (quarter: number): number => {
    nonnegative(quarter);
    return lastAtOrBefore(tempos, (t) => t.quarter, quarter).bpm;
  };
  builds += 1;
  return { quarterAt, barBeatAt, secondsAt, quarterAtSeconds, meterAtBar, barStartQuarter, tempoAt };
}

/** The clock for one document revision. Callers share it until the store
 * replaces `properties`, so a paint builds no clock of its own. */
export function projectClock(properties: ProjectProperties): ProjectClock {
  const cached = clocks.get(properties);
  if (cached) return cached;
  const clock = createProjectClock(properties);
  clocks.set(properties, clock);
  return clock;
}
