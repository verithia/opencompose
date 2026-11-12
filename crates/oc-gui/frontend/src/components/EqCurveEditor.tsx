/**
 * EQ response editor. Drags write the same effect params as the generic sliders;
 * per-band off writes 0 dB because the engine EQ has no bypass parameter.
 */
import { For, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { Effect } from "../types/project";
import type { DeviceDef, DeviceParamDef } from "../types/devices";
import { eqCurve, type EqBandId, type EqBandValue } from "../devices/eqResponse";
import { findParam } from "../devices/catalog";
import { readEffectParam, toParamString } from "../devices/params";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { withParameterTarget, type ParameterRequestOwner } from "../stores/parameterAdmission";

const VIEW_WIDTH = 280;
const VIEW_HEIGHT = 110;
const PLOT = { left: 8, top: 10, width: 264, height: 90 };
const FREQ_MIN = 20;
const FREQ_MAX = 20000;
const DB_MIN = -18;
const DB_MAX = 18;

type TranslationKey = Parameters<typeof i18n.t>[0];

interface BandSpec {
  id: EqBandId;
  freqId: string;
  gainId: string;
  qId: string | null;
  labelKey: TranslationKey;
}

const BANDS: BandSpec[] = [
  { id: "low", freqId: "low_freq", gainId: "low_gain", qId: null, labelKey: "device.band.low" },
  { id: "mid", freqId: "mid_freq", gainId: "mid_gain", qId: "q", labelKey: "device.band.mid" },
  { id: "high", freqId: "high_freq", gainId: "high_gain", qId: null, labelKey: "device.band.high" },
];

interface ActiveBand {
  spec: BandSpec;
  freqDef: DeviceParamDef;
  gainDef: DeviceParamDef;
  qDef: DeviceParamDef | null;
}

interface BandDraft {
  band: EqBandId;
  freq: number;
  gain: number;
}

export function EqCurveEditor(props: {
  target: string;
  index: number;
  effect: Effect;
  device: DeviceDef;
}) {
  let disposed = false;
  let cancelDrag: (() => void) | null = null;
  onCleanup(() => { disposed = true; cancelDrag?.(); });
  function captureTarget(): ParameterRequestOwner {
    const owner = store.parameterRequestOwner();
    const target = props.target;
    const index = props.index;
    const device = props.device.id;
    const effect = JSON.stringify(props.effect.effect_type);
    return withParameterTarget(owner, () => !disposed && props.target === target && props.index === index
      && props.device.id === device && JSON.stringify(props.effect.effect_type) === effect);
  }
  const [draft, setDraft] = createSignal<BandDraft | null>(null);
  const [disabled, setDisabled] = createSignal<Record<EqBandId, boolean>>({
    low: false,
    mid: false,
    high: false,
  });
  const [lastGain, setLastGain] = createSignal<Record<EqBandId, number>>({
    low: 3,
    mid: 3,
    high: 3,
  });

  const disabledBand = (band: EqBandId) => disabled()[band];

  const activeBands = createMemo<ActiveBand[]>(() =>
    BANDS.flatMap((spec) => {
      const freqDef = findParam(props.device, spec.freqId);
      const gainDef = findParam(props.device, spec.gainId);
      if (!freqDef || !gainDef) return [];
      return [{ spec, freqDef, gainDef, qDef: spec.qId ? findParam(props.device, spec.qId) ?? null : null }];
    }),
  );

  const bands = createMemo<EqBandValue[]>(() =>
    activeBands().map((band) => {
      const current = draft();
      return {
        id: band.spec.id,
        freq: current?.band === band.spec.id
          ? current.freq
          : readEffectParam(props.effect, props.device.id, band.freqDef),
        gain: current?.band === band.spec.id
          ? current.gain
          : readEffectParam(props.effect, props.device.id, band.gainDef),
        q: band.qDef ? readEffectParam(props.effect, props.device.id, band.qDef) : 1,
      };
    }),
  );

  const curvePath = createMemo(() =>
    eqCurve(bands(), 180)
      .map((point, index) =>
        `${index === 0 ? "M" : "L"}${freqToX(point.freq).toFixed(2)},${dbToY(clampDb(point.db)).toFixed(2)}`,
      )
      .join(" "),
  );

  createEffect(() => {
    for (const band of activeBands()) {
      if (!disabled()[band.spec.id]) continue;
      const gain = readEffectParam(props.effect, props.device.id, band.gainDef);
      if (Math.abs(gain) > 0.001) {
        setDisabled((previous) => ({ ...previous, [band.spec.id]: false }));
      }
    }
  });

  async function commitBand(band: ActiveBand, freq: number, gain: number, owner = captureTarget()) {
    if (!owner()) return;
    await store.setEffectParams(props.target, props.index, [
      [band.freqDef.id, toParamString(band.freqDef, freq)],
      [band.gainDef.id, toParamString(band.gainDef, gain)],
    ], owner);
  }

  async function resetBand(band: ActiveBand) {
    const owner = captureTarget();
    setDraft(null);
    setDisabled(previous => ({ ...previous, [band.spec.id]: false }));
    await commitBand(band, band.freqDef.default, band.gainDef.default, owner);
  }

  async function toggleBand(band: ActiveBand) {
    const owner = captureTarget();
    const id = band.spec.id;
    if (disabled()[id]) {
      const restore = lastGain()[id] || Math.max(band.gainDef.default, 3);
      await store.setEffectParams(props.target, props.index,
        [[band.gainDef.id, toParamString(band.gainDef, restore)]], owner);
      if (owner()) setDisabled(previous => ({ ...previous, [id]: false }));
      return;
    }
    const current = readEffectParam(props.effect, props.device.id, band.gainDef);
    if (Math.abs(current) > 0.001) setLastGain(previous => ({ ...previous, [id]: current }));
    await store.setEffectParams(props.target, props.index,
      [[band.gainDef.id, toParamString(band.gainDef, 0)]], owner);
    if (owner() && Math.abs(readEffectParam(props.effect, props.device.id, band.gainDef)) < 0.001) {
      setDisabled(previous => ({ ...previous, [id]: true }));
    }
  }

  function beginDrag(band: ActiveBand, event: MouseEvent) {
    if (event.button !== 0 || disabled()[band.spec.id]) return;
    const owner = captureTarget();
    cancelDrag?.();
    event.preventDefault();
    const svg = (event.currentTarget as SVGGElement).ownerSVGElement;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    let ended = false;
    let value: BandDraft | null = null;
    const stop = () => {
      ended = true;
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      window.removeEventListener("blur", cancel);
      if (cancelDrag === cancel) cancelDrag = null;
    };
    const cancel = () => { if (ended) return; stop(); setDraft(null); };
    const move = (moveEvent: MouseEvent) => {
      if (ended || !owner()) { cancel(); return; }
      const x = clamp(((moveEvent.clientX - rect.left) / rect.width) * VIEW_WIDTH, PLOT.left, PLOT.left + PLOT.width);
      const y = clamp(((moveEvent.clientY - rect.top) / rect.height) * VIEW_HEIGHT, PLOT.top, PLOT.top + PLOT.height);
      value = { band: band.spec.id, freq: xToFreq(x), gain: yToDb(y) };
      setDraft(value);
    };
    const up = () => {
      if (ended) return;
      stop();
      setDraft(null);
      if (value && owner()) void commitBand(band, value.freq, value.gain, owner);
    };
    cancelDrag = cancel;
    move(event);
    if (ended) return;
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
    window.addEventListener("blur", cancel);
  }

  const bandValue = (band: ActiveBand) => bands().find((value) => value.id === band.spec.id)!;

  return (
    <div class="eq-editor" data-eq-editor>
      <div class="device-section-title">{i18n.t("device.eq.response")}</div>
      <svg class="eq-curve" viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`} role="img"
        aria-label={i18n.t("device.eq.response")}>
        <For each={[-18, -9, 0, 9, 18]}>
          {(db) => (
            <line class="eq-grid-line" classList={{ zero: db === 0 }}
              x1={PLOT.left} x2={PLOT.left + PLOT.width} y1={dbToY(db)} y2={dbToY(db)} />
          )}
        </For>
        <For each={[100, 1000, 10000]}>
          {(freq) => (
            <line class="eq-grid-line" x1={freqToX(freq)} x2={freqToX(freq)}
              y1={PLOT.top} y2={PLOT.top + PLOT.height} />
          )}
        </For>
        <path class="eq-curve-line" d={curvePath()} />
        <For each={activeBands()}>
          {(band) => (
            <g
              class={`eq-handle eq-handle-${band.spec.id}`}
              classList={{ off: disabledBand(band.spec.id) }}
              data-eq-handle={band.spec.id}
              onMouseDown={(event) => beginDrag(band, event)}
              onDblClick={() => void resetBand(band)}
            >
              <circle cx={freqToX(bandValue(band).freq)} cy={dbToY(bandValue(band).gain)} r="6" />
            </g>
          )}
        </For>
        <text class="eq-axis-label" x={PLOT.left} y={VIEW_HEIGHT - 1}>20 Hz</text>
        <text class="eq-axis-label" x={PLOT.left + PLOT.width} y={VIEW_HEIGHT - 1} text-anchor="end">20 kHz</text>
      </svg>
      <div class="eq-band-row">
        <For each={activeBands()}>
          {(band) => (
            <button
              class="eq-band-toggle"
              classList={{ off: disabledBand(band.spec.id) }}
              data-band-toggle={band.spec.id}
              aria-pressed={!disabledBand(band.spec.id)}
              aria-label={i18n.t("device.band.toggle", { band: i18n.t(band.spec.labelKey) })}
              onClick={() => void toggleBand(band)}
            >
              <span class={`eq-band-swatch eq-band-swatch-${band.spec.id}`} />
              {i18n.t(band.spec.labelKey)}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}

function freqToX(freq: number): number {
  return PLOT.left + (Math.log(freq / FREQ_MIN) / Math.log(FREQ_MAX / FREQ_MIN)) * PLOT.width;
}

function xToFreq(x: number): number {
  const t = clamp((x - PLOT.left) / PLOT.width, 0, 1);
  return FREQ_MIN * Math.pow(FREQ_MAX / FREQ_MIN, t);
}

function dbToY(db: number): number {
  return PLOT.top + (1 - (clampDb(db) - DB_MIN) / (DB_MAX - DB_MIN)) * PLOT.height;
}

function yToDb(y: number): number {
  const t = clamp((y - PLOT.top) / PLOT.height, 0, 1);
  return DB_MIN + (1 - t) * (DB_MAX - DB_MIN);
}

function clampDb(db: number): number {
  return Math.min(DB_MAX, Math.max(DB_MIN, db));
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
