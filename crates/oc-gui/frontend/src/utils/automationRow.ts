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
    .map((point) => pointValue(point.value))
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
): { x: number; y: number } {
  return {
    x: quarterAt(point.position) * pxPerBeat,
    y: valueToY(pointValue(point.value) ?? domain.min, domain, height),
  };
}

/**
 * Segment shape at `position` in 0..1, as playback applies it: a stairs
 * segment holds its value, a half sine is a half-sine ease, and a smooth
 * segment blends towards a smoothstep by `tension` (1 by default).
 */
function segmentShape(curve: AutomationCurve, tension: number, position: number): number {
  switch (curve) {
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
}

/** The lane's points as drawn vertices, ordered by x. */
function laneVertices(
  lane: AutomationLane,
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
        y: valueToY(value ?? domain.min, domain, height),
        value,
        curve: point.curve ?? "linear",
        tension: point.tension ?? 1,
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
  lane: AutomationLane,
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
    if (point.curve === "linear" || next.x <= point.x) continue;
    if (point.curve === "stairs") {
      // Hold the value across the segment, then jump at its end.
      push(next.x, point.y);
      continue;
    }
    for (let sample = 1; sample < CURVE_SEGMENT_SAMPLES; sample += 1) {
      const progress = sample / CURVE_SEGMENT_SAMPLES;
      const value =
        point.value +
        (next.value - point.value) * segmentShape(point.curve, point.tension, progress);
      push(point.x + (next.x - point.x) * progress, valueToY(value, domain, height));
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
