/**
 * Combined passive-EQ response. Read-only: the schema sliders below own the
 * values, and this view draws the sum of the six serial stages.
 */
import { For, createMemo } from "solid-js";
import type { Effect } from "../types/project";
import type { DeviceDef } from "../types/devices";
import { findParam } from "../devices/catalog";
import { readEffectParam } from "../devices/params";
import {
  PASSIVE_EQ_DEFAULTS,
  passiveEqCurve,
  passiveEqDb,
  type PassiveEqValues,
} from "../devices/passiveEq";
import { i18n } from "../stores/i18n";

const VIEW_WIDTH = 460;
const VIEW_HEIGHT = 150;
const PLOT = { left: 8, top: 8, width: 444, height: 132 };
const FREQ_MIN = 20;
const FREQ_MAX = 20000;
const DB_MIN = -18;
const DB_MAX = 18;

export function PassiveEqCurve(props: {
  target: string;
  index: number;
  effect: Effect;
  device: DeviceDef;
}) {
  const read = (id: string, fallback: number): number => {
    const def = findParam(props.device, id);
    return def ? readEffectParam(props.effect, props.device.id, def) : fallback;
  };
  const values = createMemo<PassiveEqValues>(() => ({
    lowFreq: read("low_freq", PASSIVE_EQ_DEFAULTS.lowFreq),
    lowBoost: read("low_boost", PASSIVE_EQ_DEFAULTS.lowBoost),
    lowCut: read("low_cut", PASSIVE_EQ_DEFAULTS.lowCut),
    highFreq: read("high_freq", PASSIVE_EQ_DEFAULTS.highFreq),
    highBoost: read("high_boost", PASSIVE_EQ_DEFAULTS.highBoost),
    highQ: read("high_q", PASSIVE_EQ_DEFAULTS.highQ),
    highCutFreq: read("high_cut_freq", PASSIVE_EQ_DEFAULTS.highCutFreq),
    highCut: read("high_cut", PASSIVE_EQ_DEFAULTS.highCut),
    tilt: read("tilt", PASSIVE_EQ_DEFAULTS.tilt),
    airFreq: read("air_freq", PASSIVE_EQ_DEFAULTS.airFreq),
    air: read("air", PASSIVE_EQ_DEFAULTS.air),
  }));

  const curvePath = createMemo(() =>
    passiveEqCurve(values(), 180)
      .map(
        (point, index) =>
          `${index === 0 ? "M" : "L"}${freqToX(point.freq).toFixed(2)},${dbToY(clampDb(point.db)).toFixed(2)}`,
      )
      .join(" "),
  );

  const markers = createMemo(() => {
    const current = values();
    return [
      { id: "low", freq: current.lowFreq },
      { id: "high", freq: current.highFreq },
      { id: "high-cut", freq: current.highCutFreq },
      { id: "air", freq: current.airFreq },
    ];
  });

  return (
    <div class="eq-editor" data-passive-eq-curve>
      <div class="device-section-title">{i18n.t("device.peq.response")}</div>
      <svg
        class="eq-curve"
        viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`}
        role="img"
        aria-label={i18n.t("device.peq.response")}
      >
        <For each={[-18, -9, 0, 9, 18]}>
          {(db) => (
            <line
              class="eq-grid-line"
              classList={{ zero: db === 0 }}
              x1={PLOT.left}
              x2={PLOT.left + PLOT.width}
              y1={dbToY(db)}
              y2={dbToY(db)}
            />
          )}
        </For>
        <For each={[100, 1000, 10000]}>
          {(freq) => (
            <line
              class="eq-grid-line"
              x1={freqToX(freq)}
              x2={freqToX(freq)}
              y1={PLOT.top}
              y2={PLOT.top + PLOT.height}
            />
          )}
        </For>
        <path class="eq-curve-line" d={curvePath()} />
        <For each={markers()}>
          {(marker) => (
            <g
              class={`eq-marker eq-marker-${marker.id}`}
              data-passive-marker={marker.id}
            >
              <circle
                cx={freqToX(marker.freq)}
                cy={dbToY(clampDb(passiveEqDb(values(), marker.freq)))}
                r="5"
              />
            </g>
          )}
        </For>
        <text class="eq-axis-label" x={PLOT.left} y={VIEW_HEIGHT - 1}>
          20 Hz
        </text>
        <text
          class="eq-axis-label"
          x={PLOT.left + PLOT.width}
          y={VIEW_HEIGHT - 1}
          text-anchor="end"
        >
          20 kHz
        </text>
      </svg>
    </div>
  );
}

function freqToX(freq: number): number {
  return PLOT.left + (Math.log(freq / FREQ_MIN) / Math.log(FREQ_MAX / FREQ_MIN)) * PLOT.width;
}

function dbToY(db: number): number {
  return PLOT.top + (1 - (clampDb(db) - DB_MIN) / (DB_MAX - DB_MIN)) * PLOT.height;
}

function clampDb(db: number): number {
  return Math.min(DB_MAX, Math.max(DB_MIN, db));
}
