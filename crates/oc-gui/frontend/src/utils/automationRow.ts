/**
 * Geometry and labels of one automation lane row in the timeline.
 *
 * A row's x axis is the same beat→pixel mapping the clips use, so a point sits
 * over the audio it drives. Its y axis is the range the lane's target accepts,
 * so a point can be authored anywhere the target can hold, and the row inverts
 * that scale for a point added or dragged from a pointer.
 */
import type {
  AutomationCurve,
  AutomationLane,
  AutomationPoint,
  AutomationValueMapping,
  BarBeat,
  Effect,
  ParamValue,
} from "../types/project";
import {
  automationLabel,
  automationLaneKey,
  automationTarget,
  effectLaneTarget,
} from "./automationTarget";
import { devicesCatalog, findDevice, findParam } from "../devices/catalog";
import { effectDeviceId, effectLaneName } from "../devices/effectIds";

/** Height one lane row occupies under its track lane, in px. */
export const AUTOMATION_ROW_HEIGHT = 38;

/** Samples a curved segment takes; a linear segment needs only its endpoints. */
const CURVE_SEGMENT_SAMPLES = 16;

/** Vertical value range a row draws into. */
export interface LaneDomain {
  min: number;
  max: number;
}

/** What kind of target a lane name addresses. */
export type AutomationTargetKind = "effect" | "patch" | "control";

/** Numeric height of a point, or null when it carries no number. */
export function pointValue(value: ParamValue): number | null {
  const [unit, amount] = Object.entries(value)[0] ?? [];
  if (unit === undefined || typeof amount !== "number" || !Number.isFinite(amount)) return null;
  return amount;
}

/** Effect lanes name an effect (`fx2` or a device id) and a parameter. */
export function automationTargetKind(lane: AutomationLane): AutomationTargetKind {
  if (automationLaneKey(lane.param) !== null) return "patch";
  return lane.param.includes(".") ? "effect" : "control";
}

/**
 * The name a row shows: a patch lane is decoded back to its parameter, and
 * every other lane keeps its written form.
 */
export function automationLaneLabel(lane: AutomationLane): string {
  const key = automationLaneKey(lane.param);
  return key === null ? lane.param : automationLabel(key);
}

/**
 * The range the mixer's own `volume` and `pan` control lanes author in: the
 * MIDI volume domain, where 0 dB is the middle of the usable band and +6 dB is
 * its top, and hard left..hard right.
 */
const VOLUME_DOMAIN: LaneDomain = { min: -60, max: 6 };
const PAN_DOMAIN: LaneDomain = { min: -1, max: 1 };

/** The chain entry a lane target names, by `fx<N>` position or by type. */
function effectAt(effects: Effect[], target: string): Effect | null {
  const position = /^fx(\d+)$/.exec(target);
  if (position) return effects[Number(position[1]) - 1] ?? null;
  return effects.find((effect) => effectLaneName(effect.effect_type) === target) ?? null;
}

/**
 * The value range the lane's target accepts, in the lane's own unit.
 *
 * A `pluginparam_` lane reads the patch table's physical domain; an effect lane
 * reads the device schema of the chain entry it names; `volume` and `pan` read
 * the mixer's own ranges. A target that cannot be resolved — an unloaded
 * device, a hosted plugin, any other control — returns null so the row keeps a
 * scale that fits the points it has.
 */
export function laneTargetDomain(
  lane: AutomationLane,
  effects?: Effect[],
): LaneDomain | null {
  const key = automationLaneKey(lane.param);
  if (key !== null) {
    const target = automationTarget(key);
    return target.available ? { min: target.domain[0], max: target.domain[1] } : null;
  }
  const parts = effectLaneTarget(lane.param);
  if (!parts) {
    const name = lane.param.trim().toLowerCase();
    if (name === "volume" || name === "vol") return VOLUME_DOMAIN;
    return name === "pan" ? PAN_DOMAIN : null;
  }
  if (!effects) return null;
  const effect = effectAt(effects, parts.target);
  if (!effect) return null;
  const id = effectDeviceId(effect.effect_type);
  const device = id === null ? undefined : findDevice(devicesCatalog(), id);
  const param = device ? findParam(device, parts.parameter) : undefined;
  return param && param.max > param.min ? { min: param.min, max: param.max } : null;
}

/**
 * The lane's vertical range: the target's own range when it is known, else the
 * values the lane carries, padded so the outermost points are not glued to the
 * row's edges and a flat lane has a usable scale.
 */
export function laneDomain(lane: AutomationLane, effects?: Effect[]): LaneDomain {
  const target = laneTargetDomain(lane, effects);
  if (target) return target;
  const values = lane.points
    .map((point) => {
      const value = pointValue(point.value);
      return value === null ? null : mappedAutomationValue(value, lane.value_mapping);
    })
    .filter((value): value is number => value !== null);
  if (values.length === 0) return { min: 0, max: 1 };
  const low = Math.min(...values);
  const high = Math.max(...values);
  if (high - low < 1e-9) {
    const pad = Math.max(1e-3, Math.abs(high) * 0.1);
    return { min: low - pad, max: high + pad };
  }
  const pad = (high - low) * 0.08;
  return { min: low - pad, max: high + pad };
}

/** y of a value inside a row; the row's top is the domain's maximum. */
export function valueToY(value: number, domain: LaneDomain, height: number): number {
  const span = domain.max - domain.min;
  const ratio = span > 0 ? (value - domain.min) / span : 0.5;
  return height - Math.min(1, Math.max(0, ratio)) * height;
}

/** Inverse of `valueToY`, for a point added or dragged from a pointer. */
export function yToValue(y: number, domain: LaneDomain, height: number): number {
  const ratio = 1 - Math.min(1, Math.max(0, y / height));
  return domain.min + ratio * (domain.max - domain.min);
}

/** One point's pixel position inside a row. */
export function pointXY(
  point: AutomationPoint,
  quarterAt: (position: BarBeat) => number,
  pxPerBeat: number,
  domain: LaneDomain,
  height: number,
  mapping?: AutomationValueMapping,
): { x: number; y: number } {
  return {
    x: quarterAt(point.position) * pxPerBeat,
    y: valueToY(mappedAutomationValue(pointValue(point.value) ?? domain.min, mapping), domain, height),
  };
}

/** The mathematical coefficient belongs to the authored segment, not its units. */
export function exponentialShape(position: number, curvature: number): number {
  if (curvature === 0 || position === 0 || position === 1) return position;
  // Relative factors preserve the result when the exponent product underflows.
  if (Math.abs(curvature) < Number.MIN_VALUE / Number.EPSILON) {
    const argument = curvature * position;
    const relative = argument === 0 ? 1 : Math.expm1(argument) / argument;
    return position * relative * (curvature / Math.expm1(curvature));
  }
  return Math.expm1(curvature * position) / Math.expm1(curvature);
}

/** A lane transfers the interpolated recipient-domain value, not its anchors. */
export function mappedAutomationValue(value: number, mapping?: AutomationValueMapping): number {
  if (!mapping) return value;
  const input = mapping.input_quantization_steps === undefined ? value : Math.floor(value * mapping.input_quantization_steps) / mapping.input_quantization_steps;
  const shaped = mapping.curve === "exponential" ? exponentialShape(input, mapping.curvature ?? 0) : input;
  return (mapping.offset ?? 0) + (mapping.scale ?? 1) * shaped;
}

/** Invert a displayed mapped height before writing the authored anchor. */
export function unmappedAutomationValue(value: number, mapping?: AutomationValueMapping): number | null {
  if (!mapping) return value;
  if (mapping.input_quantization_steps !== undefined) return null;
  const scale = mapping.scale ?? 1;
  if (scale === 0) return null;
  const shaped = (value - (mapping.offset ?? 0)) / scale;
  const curvature = mapping.curvature ?? 0;
  let result = shaped;
  if (mapping.curve === "exponential" && curvature !== 0 && shaped !== 0 && shaped !== 1) {
    const denominator = Math.expm1(curvature);
    // The inverse uses log1p-relative factors for the same subnormal domain.
    if (Math.abs(curvature) < Number.MIN_VALUE / Number.EPSILON) {
      const argument = shaped * denominator;
      const relative = argument === 0 ? 1 : Math.log1p(argument) / argument;
      result = shaped * relative * (denominator / curvature);
    } else {
      result = Math.log1p(shaped * denominator) / curvature;
    }
  }
  return Number.isFinite(result) ? result : null;
}

/**
 * Segment shape at `position` in 0..1, as playback applies it: a stairs
 * segment holds its value, a half sine is a half-sine ease, and a smooth
 * segment blends towards a smoothstep by `tension` (1 by default).
 */
function segmentShape(curve: AutomationCurve, tension: number, position: number, curvature: number): number {
  switch (curve) {
    case "exponential":
      return exponentialShape(position, curvature);
    case "stairs":
      return 0;
    case "half_sine":
      return 0.5 - 0.5 * Math.cos(Math.PI * position);
    case "smooth": {
      const smoothstep = position * position * (3 - 2 * position);
      return position + (smoothstep - position) * tension;
    }
    default:
      return position;
  }
}

/** One drawn vertex: its pixel position and the curve leaving it. */
interface LaneVertex {
  x: number;
  y: number;
  /** Numeric height, or null when the point carries no number. */
  value: number | null;
  curve: AutomationCurve;
  tension: number;
  curvature: number;
}

/** The lane's points as drawn vertices, ordered by x. */
function laneVertices(
  lane: Pick<AutomationLane, "points" | "value_mapping">,
  quarterAt: (position: BarBeat) => number,
  pxPerBeat: number,
  domain: LaneDomain,
  height: number,
): LaneVertex[] {
  return lane.points
    .map((point) => {
      const value = pointValue(point.value);
      return {
        x: quarterAt(point.position) * pxPerBeat,
        y: valueToY(mappedAutomationValue(value ?? domain.min, lane.value_mapping), domain, height),
        value,
        curve: point.curve ?? "linear",
        tension: point.tension ?? 1,
        curvature: point.curvature ?? 0,
      };
    })
    .sort((a, b) => a.x - b.x);
}

/**
 * The lane's polyline as SVG points, sampled from each segment's curve so the
 * drawing agrees with playback. Vertices are ordered by x: a point dragged past
 * its neighbour keeps its index but is drawn where it sits, so the shape never
 * folds back on itself.
 */
export function lanePolyline(
  lane: Pick<AutomationLane, "points" | "value_mapping">,
  quarterAt: (position: BarBeat) => number,
  pxPerBeat: number,
  domain: LaneDomain,
  height: number,
): string {
  const vertices = laneVertices(lane, quarterAt, pxPerBeat, domain, height);
  const drawn: string[] = [];
  const push = (x: number, y: number) => drawn.push(`${x.toFixed(2)},${y.toFixed(2)}`);
  for (const [index, point] of vertices.entries()) {
    push(point.x, point.y);
    const next = vertices[index + 1];
    // A missing neighbour or a non-numeric height leaves a straight segment.
    if (!next || point.value === null || next.value === null) continue;
    if (next.x <= point.x) continue;
    if (point.curve === "linear" && lane.value_mapping?.curve !== "exponential" && lane.value_mapping?.input_quantization_steps === undefined) continue;
    if (point.curve === "stairs") {
      // Hold the value across the segment, then jump at its end.
      push(next.x, point.y);
      continue;
    }
    const quantization = lane.value_mapping?.input_quantization_steps;
    if (quantization !== undefined && next.value !== point.value) {
      const first = Math.floor(Math.min(point.value, next.value) * quantization) + 1;
      const last = Math.floor(Math.max(point.value, next.value) * quantization);
      // Draw resolvable quantizer transitions without persisting sampled knots.
      if (last - first < 512) {
        const thresholds = Array.from({length: Math.max(0, last - first + 1)}, (_,i)=>first+i);
        if (next.value < point.value) thresholds.reverse();
        const mapping = {...lane.value_mapping!, input_quantization_steps: undefined};
        const descending = next.value < point.value;
        if (descending && Number.isInteger(point.value * quantization)) {
          push(point.x, valueToY(mappedAutomationValue((point.value * quantization - 1) / quantization, mapping), domain, height));
        }
        for (const threshold of thresholds) {
          const ratio = (threshold / quantization - point.value) / (next.value - point.value);
          if (!(ratio > 0 && ratio < 1)) continue;
          let lo = 0, hi = 1;
          for (let iteration = 0; iteration < 40; iteration++) {
            const middle = (lo + hi) * .5;
            if (segmentShape(point.curve, point.tension, middle, point.curvature) < ratio) lo = middle;
            else hi = middle;
          }
          const x = point.x + (next.x - point.x) * (lo + hi) * .5;
          const before = (threshold - (next.value > point.value ? 1 : 0)) / quantization;
          const after = (threshold - (next.value > point.value ? 0 : 1)) / quantization;
          push(x, valueToY(mappedAutomationValue(before, mapping), domain, height));
          push(x, valueToY(mappedAutomationValue(after, mapping), domain, height));
        }
        const endBucket = Math.floor(next.value * quantization) - (!descending && Number.isInteger(next.value * quantization) ? 1 : 0);
        push(next.x, valueToY(mappedAutomationValue(endBucket / quantization, mapping), domain, height));
        continue;
      }
    }
    for (let sample = 1; sample < CURVE_SEGMENT_SAMPLES; sample += 1) {
      const progress = sample / CURVE_SEGMENT_SAMPLES;
      const value =
        point.value +
        (next.value - point.value) * segmentShape(point.curve, point.tension, progress, point.curvature);
      push(point.x + (next.x - point.x) * progress, valueToY(mappedAutomationValue(value, lane.value_mapping), domain, height));
    }
  }
  return drawn.join(" ");
}

/** The clip block of a lane, or null for a whole-timeline lane. */
export function laneSpan(
  lane: AutomationLane,
  quarterAt: (position: BarBeat) => number,
  pxPerBeat: number,
): { left: number; width: number } | null {
  if (!lane.time_range) return null;
  const left = quarterAt(lane.time_range.start) * pxPerBeat;
  const right = quarterAt(lane.time_range.end) * pxPerBeat;
  return { left, width: Math.max(right - left, 2) };
}

/** Keep handles on the authored points and reserve screen space between them. */
export function automationHandleSubset<T extends { at: { x: number; y: number }; index: number }>(
  points: readonly T[], selected?: number | null,
): T[] {
  const spacing = 12;
  const sorted = [...points].sort((a, b) => a.at.x - b.at.x);
  const buckets = new Map<number, T[]>();
  const handles: T[] = [];
  const add = (point: T | undefined) => {
    if (!point) return;
    const { x, y } = point.at;
    const cell = Math.floor(x / spacing);
    for (let column = cell - 1; column <= cell + 1; column += 1) {
      if (buckets.get(column)?.some(({ at }) => Math.hypot(at.x - x, at.y - y) < spacing)) return;
    }
    const bucket = buckets.get(cell) ?? [];
    bucket.push(point);
    buckets.set(cell, bucket);
    handles.push(point);
  };
  if (selected != null) add(sorted.find((point) => point.index === selected));
  add(sorted[0]);
  add(sorted.at(-1));
  sorted.forEach(add);
  return handles.sort((a, b) => a.index - b.index);
}
