/**
 * Compact lane editor: one row per automation lane that targets this rack's
 * owner, a mode, a smoothing time for the lane and a segment curve for each
 * point. Every lane of the owner is listed, not only effect lanes, and a lane
 * is addressed by its index so two lanes on one parameter stay distinct. In
 * LFO mode the same points are one cycle of a waveform, so the row also
 * carries its speed, cycle multiplier, pulse width, skew, tension and amount.
 * Point positions and values stay read-only here; they are authored on the
 * timeline row, so this stays a compact editor.
 */
import { For, Show, createMemo, createSignal } from "solid-js";

import type {
  AutomationCurve,
  AutomationLane,
  AutomationMode,
  LaneLfo,
  LfoSpeed,
  ParamValue,
} from "../types/project";
import { store } from "../stores/project";
import { i18n, type I18nKey } from "../stores/i18n";
import { automationLaneLabel, automationTargetKind } from "../utils/automationRow";

const CURVES: AutomationCurve[] = ["linear", "stairs", "half_sine", "smooth"];
const MAX_SMOOTHING_MS = 60_000;

const DEFAULT_LFO: Required<LaneLfo> = {
  speed: { unit: "hz", hz: 1 },
  mult: 1,
  pulse_width: 0.5,
  skew: 0,
  tension: 0,
  amount: 1,
};

/** Lanes owned by the panel target (`track:N`, `bus:name` or `master`). */
function ownerLanes(target: string): AutomationLane[] {
  const project = store.project;
  if (target === "master") {
    return project.master?.automation ?? [];
  }
  if (target.startsWith("bus:")) {
    const name = target.slice(4);
    return project.buses.find((bus) => bus.name === name)?.automation ?? [];
  }
  if (target.startsWith("track:")) {
    const index = Number(target.slice(6));
    return project.tracks[index]?.automation ?? [];
  }
  return [];
}

function formatPosition(position: { bar: number; beat: number; subdivision: number }): string {
  const subdivision = position.subdivision > 0 ? `:${position.subdivision}` : "";
  return `${position.bar}:${position.beat}${subdivision}`;
}

function formatValue(value: ParamValue): string {
  if (typeof value === "number") return String(value);
  if (typeof value === "string") return value;
  if (typeof value === "boolean") return value ? "on" : "off";
  const [unit, amount] = Object.entries(value)[0] ?? [];
  if (unit === "Array" && Array.isArray(amount)) return `[${amount.length}]`;
  if (typeof unit !== "string" || typeof amount !== "number") return "?";
  switch (unit) {
    case "Db":
      return `${amount} dB`;
    case "Percentage":
      return `${amount}%`;
    case "Frequency":
      return `${amount} Hz`;
    case "Cents":
      return `${amount} ct`;
    case "Time":
      return amount < 1 ? `${amount * 1000} ms` : `${amount} s`;
    default:
      return String(amount);
  }
}

/** Stored cycle controls with defaults for a lane that never set them. */
function laneLfo(lane: AutomationLane): Required<LaneLfo> {
  const lfo = lane.lfo ?? {};
  return {
    speed: lfo.speed ?? DEFAULT_LFO.speed,
    mult: lfo.mult ?? DEFAULT_LFO.mult,
    pulse_width: lfo.pulse_width ?? DEFAULT_LFO.pulse_width,
    skew: lfo.skew ?? DEFAULT_LFO.skew,
    tension: lfo.tension ?? DEFAULT_LFO.tension,
    amount: lfo.amount ?? DEFAULT_LFO.amount,
  };
}

function formatSpeed(speed: LfoSpeed): string {
  return speed.unit === "hz" ? String(speed.hz) : `${speed.numerator}/${speed.denominator}`;
}

/** Parse a value inside a closed range; `null` when the text is not a number. */
function numberIn(raw: string, min: number, max: number, openMin = false): number | null {
  const value = Number(raw);
  if (!Number.isFinite(value)) return null;
  if (openMin ? value <= min : value < min) return null;
  return value > max ? null : value;
}

function parseBeatSpeed(raw: string): LfoSpeed | null {
  const match = /^\s*(\d+)\s*\/\s*(\d+)\s*$/.exec(raw);
  if (!match) return null;
  const numerator = Number(match[1]);
  const denominator = Number(match[2]);
  if (numerator < 1 || numerator > 64 || denominator < 1 || denominator > 64) return null;
  return { unit: "note", numerator, denominator };
}

export function EffectAutomation(props: { target: string }) {
  const lanes = createMemo(() => ownerLanes(props.target));

  function commitSmoothing(laneParam: string, laneIndex: number, raw: string) {
    const value = Number(raw);
    if (!Number.isFinite(value) || value < 0 || value > MAX_SMOOTHING_MS) return;
    void store.setAutomationSmoothing(props.target, laneParam, value, laneIndex);
  }

  function commitCurve(
    laneParam: string,
    laneIndex: number,
    pointIndex: number,
    curve: string,
    tension: number | null,
  ) {
    void store.setAutomationPointCurve(props.target, laneParam, pointIndex, curve, tension, laneIndex);
  }

  return (
    <Show when={lanes().length > 0}>
      <div class="effect-automation" data-effect-automation>
        <div class="effect-automation-header">{i18n.t("device.automation")}</div>
        <For each={lanes()}>
          {(lane, laneIndex) => (
            <LaneRow
              lane={lane}
              laneIndex={laneIndex()}
              target={props.target}
              onCurve={commitCurve}
              onSmoothing={commitSmoothing}
            />
          )}
        </For>
      </div>
    </Show>
  );
}

function LaneRow(props: {
  lane: AutomationLane;
  laneIndex: number;
  target: string;
  onCurve: (
    laneParam: string,
    laneIndex: number,
    pointIndex: number,
    curve: string,
    tension: number | null,
  ) => void;
  onSmoothing: (laneParam: string, laneIndex: number, raw: string) => void;
}) {
  const [invalid, setInvalid] = createSignal<string | null>(null);
  const mode = () => props.lane.mode ?? "envelope";
  const isLfo = () => mode() === "lfo";
  const lfo = () => laneLfo(props.lane);
  // A waveform needs two anchors; below that the shape controls cannot move it.
  const shapeReady = () => props.lane.points.length >= 2;
  const empty = () => props.lane.points.length === 0;
  const title = () =>
    `${automationLaneLabel(props.lane)} — ${i18n.t(`timeline.automation.kind.${automationTargetKind(props.lane)}` as I18nKey)}`;

  function commit(patch: Partial<LaneLfo>) {
    void store.setAutomationLfo(props.target, props.lane.param, { ...lfo(), ...patch }, props.laneIndex);
  }

  function commitNumber(field: "mult" | "pulse_width" | "skew" | "tension" | "amount", raw: string) {
    const limits: Record<typeof field, [number, number, boolean]> = {
      mult: [0, 64, true],
      pulse_width: [0, 1, true],
      skew: [-1, 1, false],
      tension: [0, 1, false],
      amount: [0, 4, false],
    };
    const [min, max, openMin] = limits[field];
    const value = numberIn(raw, min, max, openMin);
    if (value === null) {
      setInvalid(field);
      return;
    }
    setInvalid(null);
    commit({ [field]: value });
  }

  function commitSpeedUnit(unit: string) {
    if (unit === lfo().speed.unit) return;
    setInvalid(null);
    // The two units are not convertible without the tempo, so each starts from
    // its own default rather than carrying a meaningless number across.
    commit({ speed: unit === "note" ? { unit: "note", numerator: 1, denominator: 4 } : { unit: "hz", hz: 1 } });
  }

  function commitSpeed(raw: string) {
    const speed = lfo().speed;
    if (speed.unit === "hz") {
      const hz = numberIn(raw, 0, 1000, true);
      if (hz === null) {
        setInvalid("speed");
        return;
      }
      setInvalid(null);
      commit({ speed: { unit: "hz", hz } });
      return;
    }
    const note = parseBeatSpeed(raw);
    if (!note) {
      setInvalid("speed");
      return;
    }
    setInvalid(null);
    commit({ speed: note });
  }

  return (
    <div
      class="effect-automation-lane"
      data-lane={props.lane.param}
      data-lane-index={props.laneIndex}
      data-lane-mode={mode()}
      classList={{ muted: Boolean(props.lane.muted) }}
    >
      <span class="effect-automation-name" title={title()}>
        {automationLaneLabel(props.lane)}
      </span>
      <span class="effect-automation-kind">
        {i18n.t(`timeline.automation.kind.${automationTargetKind(props.lane)}` as I18nKey)}
      </span>
      <button
        type="button"
        class="track-btn automation-mute-btn"
        classList={{ "mute-active": Boolean(props.lane.muted) }}
        aria-label={
          props.lane.muted
            ? i18n.t("timeline.automation.unmute")
            : i18n.t("timeline.automation.mute")
        }
        aria-pressed={Boolean(props.lane.muted)}
        title={
          props.lane.muted
            ? i18n.t("timeline.automation.unmute")
            : i18n.t("timeline.automation.mute")
        }
        onClick={() =>
          void store.setAutomationLaneMute(
            props.target,
            props.lane.param,
            props.laneIndex,
            !props.lane.muted,
          )
        }
      >M</button>
      <button
        type="button"
        class="track-btn effect-automation-delete"
        aria-label={i18n.t("timeline.automation.deleteLane")}
        title={i18n.t("timeline.automation.deleteLane")}
        onClick={() =>
          void store.removeAutomationLane(props.target, props.lane.param, props.laneIndex)
        }
      >×</button>
      <label class="effect-automation-field">
        <span>{i18n.t("device.automation.mode")}</span>
        <select
          class="effect-automation-mode"
          value={mode()}
          aria-label={i18n.t("device.automation.mode")}
          title={i18n.t("device.automation.mode.hint")}
          onChange={(event) => {
            const next = event.currentTarget.value as AutomationMode;
            setInvalid(null);
            void store.setAutomationMode(props.target, props.lane.param, next, props.laneIndex);
          }}
        >
          <For each={["envelope", "lfo"] as AutomationMode[]}>
            {(name) => (
              <option value={name}>{i18n.t(`device.automation.mode.${name}`)}</option>
            )}
          </For>
        </select>
      </label>
      <Show when={isLfo()}>
        <div
          class="effect-automation-lfo"
          role="group"
          aria-label={i18n.t("device.automation.lfo")}
          title={i18n.t("device.automation.lfo.hint")}
        >
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.speed")}</span>
            <input
              class="effect-automation-speed"
              type="text"
              value={formatSpeed(lfo().speed)}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.speed")}
              aria-invalid={invalid() === "speed"}
              title={i18n.t("device.automation.speed.hint")}
              onChange={(event) => commitSpeed(event.currentTarget.value)}
            />
            <select
              class="effect-automation-speed-unit"
              value={lfo().speed.unit}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.speed.unit")}
              title={i18n.t("device.automation.speed.hint")}
              onChange={(event) => commitSpeedUnit(event.currentTarget.value)}
            >
              <option value="hz">{i18n.t("device.automation.speed.hz")}</option>
              <option value="note">{i18n.t("device.automation.speed.note")}</option>
            </select>
            <Show when={invalid() === "speed"}>
              <span class="effect-automation-error" role="alert">
                {i18n.t("device.automation.speed.invalid")}
              </span>
            </Show>
          </label>
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.mult")}</span>
            <input
              class="effect-automation-mult"
              type="number"
              min="0"
              max="64"
              step="0.25"
              value={lfo().mult}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.mult")}
              aria-invalid={invalid() === "mult"}
              title={i18n.t("device.automation.mult.hint")}
              onChange={(event) => commitNumber("mult", event.currentTarget.value)}
            />
          </label>
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.pw")}</span>
            <input
              class="effect-automation-pw"
              type="number"
              min="0"
              max="1"
              step="0.01"
              value={lfo().pulse_width}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.pw")}
              aria-invalid={invalid() === "pulse_width"}
              title={i18n.t("device.automation.pw.hint")}
              onChange={(event) => commitNumber("pulse_width", event.currentTarget.value)}
            />
          </label>
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.skew")}</span>
            <input
              class="effect-automation-skew"
              type="number"
              min="-1"
              max="1"
              step="0.01"
              value={lfo().skew}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.skew")}
              aria-invalid={invalid() === "skew"}
              title={i18n.t("device.automation.skew.hint")}
              onChange={(event) => commitNumber("skew", event.currentTarget.value)}
            />
          </label>
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.tension")}</span>
            <input
              class="effect-automation-lfo-tension"
              type="number"
              min="0"
              max="1"
              step="0.01"
              value={lfo().tension}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.lfo.tension")}
              aria-invalid={invalid() === "tension"}
              title={i18n.t("device.automation.tension.hint")}
              onChange={(event) => commitNumber("tension", event.currentTarget.value)}
            />
          </label>
          <label class="effect-automation-field">
            <span>{i18n.t("device.automation.amount")}</span>
            <input
              class="effect-automation-amount"
              type="number"
              min="0"
              max="4"
              step="0.01"
              value={lfo().amount}
              disabled={!shapeReady()}
              aria-label={i18n.t("device.automation.amount")}
              aria-invalid={invalid() === "amount"}
              title={i18n.t("device.automation.amount.hint")}
              onChange={(event) => commitNumber("amount", event.currentTarget.value)}
            />
          </label>
        </div>
        <Show when={!shapeReady()}>
          <span class="effect-automation-lfo-hint" role="status">
            {empty()
              ? i18n.t("device.automation.lfo.empty")
              : i18n.t("device.automation.lfo.onePoint")}
          </span>
        </Show>
      </Show>
      <label class="effect-automation-smoothing-field">
        <span>{i18n.t("device.automation.smoothing")}</span>
        <input
          class="effect-automation-smoothing"
          type="number"
          min="0"
          max={MAX_SMOOTHING_MS}
          step="1"
          value={props.lane.smoothing_ms ?? 0}
          aria-label={i18n.t("device.automation.smoothing")}
          title={i18n.t("device.automation.smoothing.hint")}
          onChange={(event) => props.onSmoothing(props.lane.param, props.laneIndex, event.currentTarget.value)}
        />
        <span class="effect-automation-unit">ms</span>
      </label>
      <div class="effect-automation-points">
        <For each={props.lane.points}>
          {(point, index) => {
            const curve = () => point.curve ?? "linear";
            return (
              <span class="effect-automation-point">
                <span class="effect-automation-pos">{formatPosition(point.position)}</span>
                <span class="effect-automation-value">{formatValue(point.value)}</span>
                <select
                  class="effect-automation-curve"
                  value={curve()}
                  aria-label={i18n.t("device.automation.curve")}
                  title={i18n.t("device.automation.curve.hint")}
                  onChange={(event) => {
                    const next = event.currentTarget.value;
                    props.onCurve(
                      props.lane.param,
                      props.laneIndex,
                      index(),
                      next,
                      next === "smooth" ? (point.tension ?? 1) : null,
                    );
                  }}
                >
                  <For each={CURVES}>
                    {(name) => (
                      <option value={name}>
                        {i18n.t(`device.automation.curve.${name}`)}
                      </option>
                    )}
                  </For>
                </select>
                <Show when={curve() === "smooth"}>
                  <input
                    class="effect-automation-tension"
                    type="number"
                    min="0"
                    max="1"
                    step="0.05"
                    value={point.tension ?? 1}
                    aria-label={i18n.t("device.automation.tension")}
                    title={i18n.t("device.automation.tension.hint")}
                    onChange={(event) => {
                      const tension = Number(event.currentTarget.value);
                      if (!Number.isFinite(tension)) return;
                      props.onCurve(props.lane.param, props.laneIndex, index(), "smooth", tension);
                    }}
                  />
                </Show>
              </span>
            );
          }}
        </For>
      </div>
    </div>
  );
}
