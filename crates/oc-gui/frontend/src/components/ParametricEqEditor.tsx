/**
 * Parametric EQ curve editor: summed response, draggable nodes, band list, one undo per gesture.
 */
import { For, Show, createEffect, createMemo, createSignal, createUniqueId, onCleanup } from "solid-js";
import type { Effect } from "../types/project";
import type { DeviceDef, DeviceParamDef } from "../types/devices";
import { findParam } from "../devices/catalog";
import { withParameterTarget, type ParameterRequestOwner } from "../stores/parameterAdmission";
import { clampParam, readEffectParam, toParamString } from "../devices/params";
import {
  CHANNEL_VIEWS,
  SPECTRUM_BANDS,
  type ChannelTarget,
  type ChannelView,
  type CurvePoint,
  type ParametricBand,
  type ParametricShape,
  audibleBands,
  bandCurve,
  parseChannel,
  parseShape,
  responseCurve,
  shapeUsesGain,
  spectrumBandCenter,
  spectrumDisplayDb,
} from "../devices/parametricEq";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { meterSnapshot, selectNodeMeter } from "../stores/meters";
import { acquireSpectrum } from "../stores/spectrum";

const CURVE_SAMPLE_RATE = 48000;
const VIEW_WIDTH = 460;
const VIEW_HEIGHT = 260;
const PLOT = { left: 44, top: 14, width: 404, height: 206 };
const DB_MIN = -24;
const DB_MAX = 24;
const DB_GRID = [-12, -6, 0, 6, 12];
const FREQ_GRID = [100, 1000, 10000];
const BAND_COLORS = ["#4fc3f7", "#ffb74d", "#ce93d8", "#81c784", "#e57373", "#ffd54f"];

interface BandSlot {
  id: string;
  cut: boolean;
}

interface Draft {
  id: string;
  mode: "position" | "range";
  freq: number;
  gainDb: number;
  q: number;
  rangeDb: number;
}

interface Readout {
  x: number;
  y: number;
  text: string;
}

export function ParametricEqEditor(props: {
  target: string;
  index: number;
  effect: Effect;
  device: DeviceDef;
}) {
  let disposed = false;
  let cancelDrag: (() => void) | null = null;
  function captureTarget(): ParameterRequestOwner {
    const owner = store.parameterRequestOwner();
    const target = props.target;
    const index = props.index;
    const device = props.device.id;
    const effect = JSON.stringify(props.effect.effect_type);
    return withParameterTarget(owner, () => !disposed && props.target === target && props.index === index
      && props.device.id === device && JSON.stringify(props.effect.effect_type) === effect);
  }
  const controlOwners = new WeakMap<Element, ParameterRequestOwner>();
  const captureControl = (event: Event & { currentTarget: Element }) => {
    controlOwners.set(event.currentTarget, captureTarget());
  };
  const captureFirstInput = (event: Event & { currentTarget: Element }) => {
    if (!controlOwners.has(event.currentTarget)) captureControl(event);
  };
  const controlOwner = (element: Element) => {
    const owner = controlOwners.get(element) ?? captureTarget();
    controlOwners.delete(element);
    return owner;
  };
  const controlCommits = new WeakMap<Element, object>();
  function commitControl(
    element: HTMLInputElement | HTMLSelectElement,
    write: (owner: ParameterRequestOwner) => Promise<void>,
    restore: () => void,
  ) {
    const owner = controlOwner(element);
    const marker = {};
    controlCommits.set(element, marker);
    const restoreCurrent = () => {
      if (controlCommits.get(element) === marker && !controlOwners.has(element)) restore();
    };
    if (!owner()) { restoreCurrent(); return; }
    void write(owner).finally(restoreCurrent);
  }
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [hoveredId, setHoveredId] = createSignal<string | null>(null);
  const [wheelValues, setWheelValues] = createSignal<Record<string, number>>({});
  const [view, setView] = createSignal<ChannelView>("all");
  const [spectrumOn, setSpectrumOn] = createSignal(false);
  let releaseSpectrum: (() => void) | null = null;
  const clipId = createUniqueId();
  const wheelTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const wheelOwners = new Map<string, ParameterRequestOwner>();
  onCleanup(() => {
    disposed = true;
    cancelDrag?.();
    for (const timer of wheelTimers.values()) clearTimeout(timer);
    wheelTimers.clear();
    wheelOwners.clear();
    releaseSpectrum?.();
    releaseSpectrum = null;
  });

  const def = (id: string) => findParam(props.device, id);
  const read = (parameter: DeviceParamDef | undefined, fallback: number) =>
    parameter ? readEffectParam(props.effect, props.device.id, parameter) : fallback;

  const slots = createMemo<BandSlot[]>(() => {
    const result: BandSlot[] = [];
    for (let index = 1; index <= 6; index += 1) {
      if (def(`band${index}_freq`)) result.push({ id: `band${index}`, cut: false });
    }
    for (const cut of ["low_cut", "high_cut"]) {
      if (def(`${cut}_freq`)) result.push({ id: cut, cut: true });
    }
    return result;
  });

  const bands = createMemo<ParametricBand[]>(() =>
    slots().map((slot) => {
      const typeDef = def(`${slot.id}_type`);
      const typeIndex = Math.round(read(typeDef, 0));
      const label = typeDef?.options?.[typeIndex] ?? (slot.cut ? cutLabel(slot.id) : "Bell");
      const channelDef = def(`${slot.id}_channel`);
      const pendingQ = wheelValues()[slot.id];
      const band: ParametricBand = {
        id: slot.id,
        cut: slot.cut,
        shape: parseShape(label) ?? (slot.cut ? (slot.id as ParametricShape) : "bell"),
        freq: read(def(`${slot.id}_freq`), 1000),
        q: pendingQ ?? read(def(`${slot.id}_q`), 0.707),
        gainDb: read(def(`${slot.id}_gain`), 0),
        enabled: read(def(`${slot.id}_enabled`), 0) >= 0.5,
        solo: read(def(`${slot.id}_solo`), 0) >= 0.5,
        dynamic: read(def(`${slot.id}_dynamic`), 0) >= 0.5,
        rangeDb: read(def(`${slot.id}_range`), 0),
        target: parseChannel(
          channelDef?.options?.[Math.round(read(channelDef, 0))] ?? "Stereo",
        ) ?? "stereo",
      };
      const current = draft();
      return current?.id === slot.id
        ? { ...band, freq: current.freq, gainDb: current.gainDb, q: current.q, rangeDb: current.rangeDb }
        : band;
    }),
  );

  const selected = createMemo(() => bands().find((band) => band.id === selectedId()) ?? null);
  const soloing = createMemo(() => bands().some((band) => band.enabled && band.solo));

  const curve = createMemo(() => responseCurve(bands(), 256, CURVE_SAMPLE_RATE, view(), false));
  const rangeCurve = createMemo(() => {
    if (!bands().some((band) => band.enabled && band.dynamic)) return null;
    return responseCurve(bands(), 256, CURVE_SAMPLE_RATE, view(), true);
  });
  const traces = createMemo(() =>
    audibleBands(bands(), view()).map((band) => ({
      id: band.id,
      color: colorOf(band.id),
      path: toPath(bandCurve(band, 140, CURVE_SAMPLE_RATE)),
    })),
  );

  const nodeMeter = createMemo(() => selectNodeMeter(meterSnapshot(), props.target));
  const effectBandGains = createMemo<number[]>(() =>
    (nodeMeter()?.effect_bands?.[props.index] ?? []).map((value) =>
      Number.isFinite(value) ? value : 0,
    ),
  );

  function bandOffsetDb(id: string): number {
    const match = /^band(\d+)$/.exec(id);
    if (!match) return 0;
    return effectBandGains()[Number(match[1]) - 1] ?? 0;
  }

  function bandUsesSidechain(id: string): boolean {
    return read(def(`${id}_sidechain`), 0) >= 0.5;
  }

  const currentBands = createMemo<ParametricBand[]>(() =>
    bands().map((band) => {
      const offset = band.dynamic && band.enabled ? bandOffsetDb(band.id) : 0;
      return offset === 0 ? band : { ...band, gainDb: band.gainDb + offset };
    }),
  );
  const currentCurve = createMemo(() =>
    responseCurve(currentBands(), 256, CURVE_SAMPLE_RATE, view(), false),
  );
  const hasGainReduction = createMemo(() =>
    bands().some((band) => band.dynamic && band.enabled && Math.abs(bandOffsetDb(band.id)) > 0.05),
  );

  const spectrumPath = createMemo(() => {
    if (!spectrumOn()) return null;
    const levels = meterSnapshot()?.spectrum?.bands;
    if (!levels || levels.length === 0) return null;
    const points = levels
      .slice(0, SPECTRUM_BANDS)
      .map((db, index) => {
        const x = freqToX(spectrumBandCenter(index, SPECTRUM_BANDS)).toFixed(2);
        const y = dbToY(spectrumDisplayDb(db)).toFixed(2);
        return `${x},${y}`;
      })
      .join(" L");
    return `M${points} L${PLOT.left + PLOT.width},${PLOT.top + PLOT.height} L${PLOT.left},${PLOT.top + PLOT.height} Z`;
  });

  const sidechainSource = () => props.effect.sidechain_source ?? "";
  // Same rule as the validator's SidechainKeyBusOnTrack: a track chain reads a
  // track only; a bus or the master also reads buses.
  const sidechainOptions = createMemo(() => {
    const trackOwned = props.target.startsWith("track:");
    const ownTrack = trackOwned
      ? store.project.tracks[Number(props.target.slice("track:".length))]?.name
      : undefined;
    const ownBus = props.target.startsWith("bus:")
      ? props.target.slice("bus:".length)
      : undefined;
    return {
      tracks: store.project.tracks
        .map((track) => track.name)
        .filter((name) => name !== ownTrack),
      buses: trackOwned
        ? []
        : store.project.buses
            .map((bus) => bus.name)
            .filter((name) => name !== ownBus),
    };
  });
  const sidechainActive = createMemo(() =>
    bands().some(
      (band) => band.dynamic && band.enabled && bandUsesSidechain(band.id),
    ),
  );

  function toggleSpectrum() {
    if (spectrumOn()) {
      releaseSpectrum?.();
      releaseSpectrum = null;
      setSpectrumOn(false);
      return;
    }
    releaseSpectrum = acquireSpectrum(props.target);
    setSpectrumOn(true);
  }

  const readout = createMemo<Readout | null>(() => {
    const current = draft();
    if (!current) return null;
    const band = bands().find((entry) => entry.id === current.id);
    if (!band) return null;
    const x = clamp(freqToX(band.freq), PLOT.left + 34, PLOT.left + PLOT.width - 34);
    const y = clamp(
      dbToY(band.cut ? 0 : band.gainDb) - 26,
      PLOT.top + 14,
      PLOT.top + PLOT.height - 12,
    );
    const text = current.mode === "range"
      ? `range ${formatDb(band.rangeDb)}`
      : `${formatHz(band.freq)} · ${formatDb(band.gainDb)}`;
    return { x, y, text };
  });

  function bindSelectValue(value: () => string) {
    return (element: HTMLSelectElement) => {
      createEffect(() => {
        element.value = value();
      });
    };
  }

  function colorOf(id: string): string {
    const index = slots().findIndex((slot) => slot.id === id);
    return BAND_COLORS[(index < 0 ? 0 : index) % BAND_COLORS.length];
  }

  async function commit(id: string, values: [string, string][], owner = captureTarget()) {
    if (!owner()) return;
    const params: [string, string][] = [];
    for (const [field, raw] of values) {
      const parameter = def(id ? `${id}_${field}` : field);
      if (!parameter) continue;
      const number = clampParam(parameter, Number(raw));
      params.push([parameter.id, toParamString(parameter, number)]);
    }
    if (params.length > 0) await store.setEffectParams(props.target, props.index, params, owner);
  }

  function commitNumber(id: string, field: string, value: number, owner = captureTarget()) {
    return commit(id, [[field, String(value)]], owner);
  }

  function commitToggle(id: string, field: string, on: boolean, also?: [string, boolean][], owner = captureTarget()) {
    const values: [string, string][] = [[field, on ? "1" : "0"]];
    for (const [name, state] of also ?? []) values.push([name, state ? "1" : "0"]);
    return commit(id, values, owner);
  }

  function commitShape(id: string, value: string, owner = captureTarget()) {
    const typeDef = def(`${id}_type`);
    if (value === "dynamic") {
      const bell = (typeDef?.options ?? []).findIndex((option) => parseShape(option) === "bell");
      return commit(id, [
        ["type", String(bell < 0 ? 0 : bell)],
        ["dynamic", "1"],
      ], owner);
    }
    const values: [string, string][] = [["type", value]];
    if (def(`${id}_dynamic`)) values.push(["dynamic", "0"]);
    return commit(id, values, owner);
  }

  function addBand() {
    const next = slots().find(
      (slot) => !slot.cut && def(`${slot.id}_enabled`) && read(def(`${slot.id}_enabled`), 0) < 0.5,
    );
    if (!next) return;
    setSelectedId(next.id);
    void commit(next.id, [
      ["enabled", "1"],
      ["solo", "0"],
    ]);
  }

  function removeBand(band: ParametricBand) {
    if (selectedId() === band.id) setSelectedId(null);
    void commit(band.id, [
      ["enabled", "0"],
      ["solo", "0"],
    ]);
  }

  function resetBand(band: ParametricBand) {
    const values: [string, string][] = [];
    for (const field of ["freq", "gain", "q", ...(band.dynamic ? ["range"] : [])]) {
      const parameter = def(`${band.id}_${field}`);
      if (parameter) values.push([field, String(parameter.default)]);
    }
    setDraft(null);
    setWheelValues((previous) => {
      const next = { ...previous };
      delete next[band.id];
      return next;
    });
    void commit(band.id, values);
  }

  function beginDrag(band: ParametricBand, event: MouseEvent) {
    if (event.button !== 0) return;
    const owner = captureTarget();
    flushWheel(band.id);
    cancelDrag?.();
    event.preventDefault();
    setSelectedId(band.id);
    const svg = (event.currentTarget as SVGGElement).ownerSVGElement;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const scaleX = VIEW_WIDTH / rect.width;
    const scaleY = VIEW_HEIGHT / rect.height;
    const startX = event.clientX;
    const startY = event.clientY;
    const start = { ...band };
    const rangeMode = event.altKey && band.dynamic && !band.cut;
    const startFrequencyX = freqToX(start.freq);
    const startGainY = dbToY(start.gainDb);
    let moved = false;
    let ended = false;
    let value: Draft | null = null;

    const move = (moveEvent: MouseEvent) => {
      if (ended || !owner()) { cancel(); return; }
      moved = true;
      const dx = (moveEvent.clientX - startX) * scaleX;
      const dy = (moveEvent.clientY - startY) * scaleY;
      if (rangeMode) {
        value = {
          id: start.id,
          mode: "range",
          freq: start.freq,
          gainDb: start.gainDb,
          q: start.q,
          rangeDb: roundTo(clamp(start.rangeDb - dy * 0.25, -24, 24), 0.1),
        };
        setDraft(value);
        return;
      }
      const freq = quantizeFrequency(
        xToFreq(clamp(startFrequencyX + dx, PLOT.left, PLOT.left + PLOT.width)),
      );
      const gainDb = start.cut
        ? start.gainDb
        : roundTo(yToDb(clamp(startGainY + dy, PLOT.top, PLOT.top + PLOT.height)), 0.1);
      value = { id: start.id, mode: "position", freq, gainDb, q: start.q, rangeDb: start.rangeDb };
      setDraft(value);
    };

    const stop = () => {
      ended = true;
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      window.removeEventListener("blur", cancel);
      if (cancelDrag === cancel) cancelDrag = null;
    };
    const cancel = () => { if (ended) return; stop(); setDraft(null); };
    const up = () => {
      if (ended) return;
      stop();
      setDraft(null);
      if (!moved || !value || !owner()) return;
      if (value.mode === "range") {
        void commit(start.id, [["range", String(value.rangeDb)]], owner);
      } else if (start.cut) {
        void commit(start.id, [["freq", String(value.freq)]], owner);
      } else {
        void commit(start.id, [
          ["freq", String(value.freq)],
          ["gain", String(value.gainDb)],
        ], owner);
      }
    };

    cancelDrag = cancel;
    move(event);
    if (ended) return;
    moved = false;
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
    window.addEventListener("blur", cancel);
  }

  function onWheel(band: ParametricBand, event: WheelEvent) {
    if (!wheelOwners.has(band.id)) wheelOwners.set(band.id, captureTarget());
    event.preventDefault();
    setSelectedId(band.id);
    const qDef = def(`${band.id}_q`);
    const factor = Math.exp(-event.deltaY * 0.0015);
    const current = wheelValues()[band.id] ?? band.q;
    const q = roundTo(
      clamp(current * factor, qDef?.min ?? 0.1, qDef?.max ?? 20),
      0.01,
    );
    setWheelValues((previous) => ({ ...previous, [band.id]: q }));
    const existing = wheelTimers.get(band.id);
    if (existing) clearTimeout(existing);
    wheelTimers.set(
      band.id,
      setTimeout(() => {
        wheelTimers.delete(band.id);
        flushWheel(band.id);
      }, 300),
    );
  }

  function flushWheel(id: string) {
    const timer = wheelTimers.get(id);
    if (timer) {
      clearTimeout(timer);
      wheelTimers.delete(id);
    }
    const owner = wheelOwners.get(id);
    wheelOwners.delete(id);
    const value = wheelValues()[id];
    if (value === undefined) return;
    setWheelValues((previous) => {
      const next = { ...previous };
      delete next[id];
      return next;
    });
    if (owner) void commit(id, [["q", String(value)]], owner);
  }

  function onNodeKeyDown(band: ParametricBand, event: KeyboardEvent) {
    const step = event.shiftKey ? 10 : 1;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      const factor = event.key === "ArrowLeft" ? 1 / (1 + step / 100) : 1 + step / 100;
      commitNumber(band.id, "freq", quantizeFrequency(band.freq * factor));
    } else if (!band.cut && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      commitNumber(
        band.id,
        "gain",
        roundTo(band.gainDb + (event.key === "ArrowUp" ? step : -step) * 0.5, 0.1),
      );
    }
  }

  const bypassOn = () => props.effect.bypass === true;
  const canAdd = () =>
    slots().some(
      (slot) => !slot.cut && def(`${slot.id}_enabled`) && read(def(`${slot.id}_enabled`), 0) < 0.5,
    );

  return (
    <div class="peq-editor" data-parametric-eq>
      <div class="device-section-title peq-title">
        <span>{i18n.t("device.peq.response")}</span>
        <span class="peq-title-actions">
          <button
            type="button"
            class="peq-chip"
            classList={{ on: spectrumOn() }}
            data-peq-spectrum
            aria-pressed={spectrumOn()}
            title={i18n.t("device.peq.spectrumHint")}
            onClick={toggleSpectrum}
          >
            {i18n.t("device.peq.spectrum")}
          </button>
          <Show when={def("auto_gain")}>
            <button
              type="button"
              class="peq-chip"
              classList={{ on: read(def("auto_gain"), 0) >= 0.5 }}
              data-peq-auto-gain
              aria-pressed={read(def("auto_gain"), 0) >= 0.5}
              onClick={() => commitToggle("", "auto_gain", read(def("auto_gain"), 0) < 0.5)}
            >
              {i18n.t("device.peq.autoGain")}
            </button>
          </Show>
          <button
            type="button"
            class="peq-chip"
            classList={{ on: bypassOn() }}
            data-peq-bypass
            aria-pressed={bypassOn()}
            onClick={() => void store.setEffectBypass(props.target, props.index, !bypassOn())}
          >
            {i18n.t("device.peq.bypass")}
          </button>
          <button type="button" class="peq-chip" data-peq-add disabled={!canAdd()} onClick={addBand}>
            {i18n.t("device.peq.addBand")}
          </button>
        </span>
      </div>

      <div class="peq-view-row" role="group" aria-label={i18n.t("device.peq.view")}>
        <For each={CHANNEL_VIEWS}>
          {(entry) => (
            <button
              type="button"
              class="peq-chip"
              classList={{ on: view() === entry }}
              data-peq-view={entry}
              aria-pressed={view() === entry}
              onClick={() => setView(entry)}
            >
              {entry === "all" ? i18n.t("device.peq.all") : channelLetter(entry)}
            </button>
          )}
        </For>
      </div>

      <div class="peq-sidechain-row" role="group" aria-label={i18n.t("device.peq.sidechain")}>
        <span class="peq-field-name">{i18n.t("device.peq.sidechain")}</span>
        <select
          class="peq-sidechain"
          data-peq-sidechain
          aria-label={i18n.t("device.peq.sidechain")}
          ref={bindSelectValue(() => sidechainSource())}
          onChange={(event) =>
            void store.setEffectSidechain(
              props.target,
              props.index,
              event.currentTarget.value || null,
            )
          }
        >
          <option value="">{i18n.t("device.peq.sidechainOff")}</option>
          <Show when={sidechainOptions().tracks.length > 0}>
            <optgroup label="Tracks">
              <For each={sidechainOptions().tracks}>
                {(name) => <option value={name}>{name}</option>}
              </For>
            </optgroup>
          </Show>
          <Show when={sidechainOptions().buses.length > 0}>
            <optgroup label="Buses">
              <For each={sidechainOptions().buses}>
                {(name) => <option value={name}>{name}</option>}
              </For>
            </optgroup>
          </Show>
        </select>
        <Show when={sidechainSource()}>
          <span
            class="peq-sc-badge"
            classList={{ on: sidechainActive() }}
            data-peq-sidechain-badge
            title={i18n.t("device.peq.sidechainActive", { source: sidechainSource() })}
          >
            SC
          </span>
        </Show>
      </div>

      <svg
        class="peq-canvas"
        viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`}
        role="img"
        aria-label={i18n.t("device.peq.response")}
      >
        <defs>
          <clipPath id={clipId}>
            <rect x={PLOT.left} y={PLOT.top} width={PLOT.width} height={PLOT.height} />
          </clipPath>
        </defs>

        <rect class="peq-plot" x={PLOT.left} y={PLOT.top} width={PLOT.width} height={PLOT.height} />
        <Show when={spectrumPath()}>
          {(path) => <path class="peq-spectrum" d={path()} />}
        </Show>
        <For each={DB_GRID}>
          {(db) => (
            <>
              <line
                class="peq-grid"
                classList={{ zero: db === 0 }}
                x1={PLOT.left}
                x2={PLOT.left + PLOT.width}
                y1={dbToY(db)}
                y2={dbToY(db)}
              />
              <text class="peq-axis-label" x={PLOT.left - 6} y={dbToY(db) + 3} text-anchor="end">
                {db > 0 ? `+${db}` : String(db)}
              </text>
            </>
          )}
        </For>
        <For each={FREQ_GRID}>
          {(freq) => (
            <>
              <line
                class="peq-grid"
                x1={freqToX(freq)}
                x2={freqToX(freq)}
                y1={PLOT.top}
                y2={PLOT.top + PLOT.height}
              />
              <text
                class="peq-axis-label"
                x={freqToX(freq)}
                y={PLOT.top + PLOT.height + 13}
                text-anchor="middle"
              >
                {formatHz(freq)}
              </text>
            </>
          )}
        </For>
        <text class="peq-axis-label" x={PLOT.left} y={PLOT.top + PLOT.height + 13} text-anchor="start">
          20 Hz
        </text>
        <text
          class="peq-axis-label"
          x={PLOT.left + PLOT.width}
          y={PLOT.top + PLOT.height + 13}
          text-anchor="end"
        >
          20 kHz
        </text>
        <text class="peq-axis-label" x={PLOT.left - 6} y={PLOT.top - 4} text-anchor="end">
          dB
        </text>

        <g clip-path={`url(#${clipId})`}>
          <For each={traces()}>
            {(trace) => <path class="peq-trace" d={trace.path} style={{ stroke: trace.color }} />}
          </For>
          <path class="peq-curve" d={toPath(curve())} />
          <Show when={rangeCurve()}>
            {(path) => <path class="peq-curve-range" d={toPath(path())} />}
          </Show>
          <Show when={hasGainReduction()}>
            <path class="peq-curve-gr" d={areaPath(curve(), currentCurve())} />
            <path class="peq-curve-current" d={toPath(currentCurve())} />
          </Show>
        </g>

        <Show when={readout()}>
          {(state) => (
            <g class="peq-readout" pointer-events="none">
              <rect
                x={state().x - state().text.length * 3.3 - 7}
                y={state().y - 17}
                width={state().text.length * 6.6 + 14}
                height="23"
                rx="3"
              />
              <text x={state().x} y={state().y - 2} text-anchor="middle">
                {state().text}
              </text>
            </g>
          )}
        </Show>

        <For each={bands()}>
          {(band) => {
            const x = () => freqToX(band.freq);
            const y = () => dbToY(band.cut ? 0 : band.gainDb);
            return (
              <g
                class="peq-node"
                classList={{
                  selected: selectedId() === band.id,
                  hovered: hoveredId() === band.id,
                  off: !band.enabled,
                  muted: soloing() && !band.solo,
                  cut: band.cut,
                  dynamic: band.dynamic,
                }}
                data-peq-node={band.id}
                role="slider"
                tabIndex={0}
                aria-label={band.id}
                aria-valuetext={`${formatHz(band.freq)} ${formatDb(band.gainDb)}`}
                onMouseEnter={() => setHoveredId(band.id)}
                onMouseLeave={() => setHoveredId(null)}
                onMouseDown={(event) => beginDrag(band, event)}
                onClick={() => setSelectedId(band.id)}
                onDblClick={() => resetBand(band)}
                onWheel={(event) => onWheel(band, event)}
                onFocus={() => setSelectedId(band.id)}
                onKeyDown={(event) => onNodeKeyDown(band, event)}
              >
                <Show when={!band.cut && Math.abs(band.gainDb) > 0.05}>
                  <line
                    class="peq-node-stem"
                    x1={x()}
                    x2={x()}
                    y1={dbToY(0)}
                    y2={y()}
                    style={{ stroke: colorOf(band.id) }}
                  />
                </Show>
                <circle class="peq-node-halo" cx={x()} cy={y()} r="12" />
                <circle
                  class="peq-node-dot"
                  cx={x()}
                  cy={y()}
                  r="7"
                  style={{ stroke: colorOf(band.id) }}
                />
                <Show when={band.dynamic && band.enabled}>
                  <circle
                    class="peq-node-dyn"
                    cx={x()}
                    cy={y()}
                    r="10"
                    style={{ stroke: colorOf(band.id) }}
                  />
                </Show>
              </g>
            );
          }}
        </For>
      </svg>

      <div class="peq-hint">{i18n.t("device.peq.hint")}</div>

      <div class="peq-bands" role="list">
        <For each={bands()}>
          {(band) => {
            const typeDef = () => def(`${band.id}_type`);
            const typeValue = () => {
              const index = Math.round(read(typeDef(), 0));
              const dynamic = read(def(`${band.id}_dynamic`), 0) >= 0.5;
              return dynamic && parseShape(typeDef()?.options?.[index] ?? "Bell") === "bell" ? "dynamic" : String(index);
            };
            const channelDef = () => def(`${band.id}_channel`);
            return (
              <div
                class="peq-band"
                classList={{ selected: selectedId() === band.id, off: !band.enabled }}
                data-peq-band={band.id}
                role="listitem"
                onClick={() => setSelectedId(band.id)}
              >
                <button
                  type="button"
                  class="peq-enable"
                  classList={{ on: band.enabled }}
                  role="switch"
                  aria-checked={band.enabled}
                  aria-label={i18n.t("device.peq.enable")}
                  data-band-enable={band.id}
                  onClick={(event) => {
                    event.stopPropagation();
                    commitToggle(
                      band.id,
                      "enabled",
                      !band.enabled,
                      band.enabled ? [["solo", false]] : undefined,
                    );
                  }}
                />
                <span class="peq-swatch" style={{ background: colorOf(band.id) }} />
                <Show
                  when={typeDef()}
                  fallback={<span class="peq-band-name">{band.cut ? cutLabel(band.id) : "Bell"}</span>}
                >
                  <select
                    class="peq-type"
                    data-band-type={band.id}
                    aria-label={i18n.t("device.peq.type")}
                    ref={bindSelectValue(typeValue)}
                    onClick={(event) => event.stopPropagation()}
                    onPointerDown={captureControl}
                    onFocus={captureControl}
                    onKeyDown={captureFirstInput}
                    onInput={captureFirstInput}
                    onChange={(event) => {
                      const select = event.currentTarget;
                      const next = select.value;
                      commitControl(select, owner => commitShape(band.id, next, owner), () => { select.value = typeValue(); });
                    }}
                  >
                    <For each={typeDef()!.options ?? []}>
                      {(option, index) => <option value={String(index())}>{option}</option>}
                    </For>
                    <Show when={def(`${band.id}_dynamic`)}>
                      <option value="dynamic">{i18n.t("device.peq.dynamic")}</option>
                    </Show>
                  </select>
                </Show>
                <span class="peq-band-value" data-band-freq={band.id}>
                  {formatHz(band.freq)}
                </span>
                <Show when={!band.cut && shapeUsesGain(band.shape)}>
                  <span class="peq-band-value" data-band-gain={band.id}>
                    {formatDb(band.gainDb)}
                  </span>
                </Show>
                <Show when={band.dynamic && def(`${band.id}_range`)}>
                  <span class="peq-band-value range" data-band-range={band.id}>
                    {`${band.rangeDb > 0 ? "+" : ""}${band.rangeDb.toFixed(1)}`}
                  </span>
                </Show>
                <Show when={band.dynamic && bandUsesSidechain(band.id)}>
                  <span class="peq-band-sc" data-band-sc={band.id} title={i18n.t("device.peq.bandSidechain")}>
                    SC
                  </span>
                </Show>
                <Show when={band.dynamic && Math.abs(bandOffsetDb(band.id)) > 0.05}>
                  <span class="peq-band-value gr" data-band-gr={band.id}>
                    {formatDb(bandOffsetDb(band.id))}
                  </span>
                </Show>
                <Show when={def(`${band.id}_solo`)}>
                  <button
                    type="button"
                    class="peq-icon"
                    classList={{ on: band.solo }}
                    data-band-solo={band.id}
                    aria-pressed={band.solo}
                    aria-label={i18n.t("device.peq.solo")}
                    onClick={(event) => {
                      event.stopPropagation();
                      commitToggle(
                        band.id,
                        "solo",
                        !band.solo,
                        !band.solo ? [["enabled", true]] : undefined,
                      );
                    }}
                  >
                    S
                  </button>
                </Show>
                <Show when={channelDef()}>
                  <select
                    class="peq-channel"
                    data-band-channel={band.id}
                    aria-label={i18n.t("device.peq.channel")}
                    ref={bindSelectValue(() =>
                      String(
                        (channelDef()?.options ?? []).findIndex(
                          (option) => parseChannel(option) === band.target,
                        ),
                      ),
                    )}
                    onClick={(event) => event.stopPropagation()}
                    onPointerDown={captureControl}
                    onFocus={captureControl}
                    onKeyDown={captureFirstInput}
                    onInput={captureFirstInput}
                    onChange={(event) => {
                      const select = event.currentTarget;
                      const next = Number(select.value);
                      commitControl(select, owner => commitNumber(band.id, "channel", next, owner),
                        () => { select.value = String(Math.round(read(channelDef(), 0))); });
                    }}
                  >
                    <For each={channelDef()?.options ?? []}>
                      {(option, index) => (
                        <option value={String(index())}>
                          {channelLetter(parseChannel(option) ?? "stereo")}
                        </option>
                      )}
                    </For>
                  </select>
                </Show>
                <Show when={!band.cut}>
                  <button
                    type="button"
                    class="peq-icon remove"
                    data-band-remove={band.id}
                    aria-label={i18n.t("device.peq.removeBand")}
                    onClick={(event) => {
                      event.stopPropagation();
                      removeBand(band);
                    }}
                  >
                    ×
                  </button>
                </Show>
              </div>
            );
          }}
        </For>
      </div>

      <Show when={selected()}>
        {(band) => (
          <div class="peq-inspector" data-peq-inspector={band().id}>
            <div class="peq-inspector-head">
              <span class="peq-swatch" style={{ background: colorOf(band().id) }} />
              <span>{band().cut ? cutLabel(band().id) : shapeLabel(band())}</span>
              <button
                type="button"
                class="peq-icon"
                data-peq-reset={band().id}
                aria-label={i18n.t("device.peq.reset")}
                onClick={() => resetBand(band())}
              >
                ↺
              </button>
            </div>
            <div class="peq-fields">
              {numberField(band(), "freq", 1)}
              {numberField(band(), "gain", 0.1)}
              {numberField(band(), "q", 0.01)}
              <Show when={band().dynamic}>
                {numberField(band(), "threshold", 0.1)}
                {numberField(band(), "range", 0.1)}
                {numberField(band(), "attack", 0.1)}
                {numberField(band(), "release", 1)}
                <Show when={def(`${band().id}_sidechain`)}>
                  <label class="peq-field" title={i18n.t("device.peq.bandSidechain")}>
                    <span class="peq-field-name">{i18n.t("device.peq.bandSidechain")}</span>
                    <input
                      type="checkbox"
                      class="peq-check"
                      data-peq-band-sidechain={band().id}
                      checked={bandUsesSidechain(band().id)}
                      onPointerDown={captureControl}
                      onFocus={captureControl}
                      onInput={captureFirstInput}
                      onChange={(event) => {
                        const input = event.currentTarget;
                        const id = band().id;
                        const next = input.checked;
                        commitControl(input, owner => commitToggle(id, "sidechain", next, undefined, owner),
                          () => { input.checked = bandUsesSidechain(id); });
                      }}
                    />
                  </label>
                </Show>
              </Show>
            </div>
          </div>
        )}
      </Show>
    </div>
  );

  function numberField(band: ParametricBand, suffix: string, step: number) {
    const parameter = () => def(`${band.id}_${suffix}`);
    return (
      <Show when={parameter()}>
        {(entry) => (
          <label class="peq-field">
            <span class="peq-field-name">{entry().name}</span>
            <input
              type="number"
              class="peq-input"
              data-peq-field={`${band.id}_${suffix}`}
              min={entry().min}
              max={entry().max}
              step={step}
              value={fieldValue(band, suffix)}
              onPointerDown={captureControl}
              onFocus={captureControl}
              onKeyDown={captureFirstInput}
              onInput={captureFirstInput}
              onChange={(event) => {
                const input = event.currentTarget;
                const next = Number(input.value);
                commitControl(input, owner => commitNumber(band.id, suffix, next, owner), () => {
                  input.value = formatFieldNumber(read(parameter(), parameter()?.default ?? 0), suffix);
                });
              }}
            />
            <span class="peq-field-unit">{entry().unit === "none" ? "" : entry().unit}</span>
          </label>
        )}
      </Show>
    );
  }

  function fieldValue(band: ParametricBand, suffix: string): string {
    const parameter = def(`${band.id}_${suffix}`);
    const value = suffix === "freq"
      ? band.freq
      : suffix === "q"
        ? band.q
        : suffix === "gain"
          ? band.gainDb
          : suffix === "range"
            ? band.rangeDb
            : read(parameter, 0);
    return formatFieldNumber(value, suffix);
  }

  function formatFieldNumber(value: number, suffix: string): string {
    const decimals = suffix === "attack" || suffix === "release" ? 2 : 3;
    return String(Number(value.toFixed(decimals)));
  }

  function shapeLabel(band: ParametricBand): string {
    const typeDef = def(`${band.id}_type`);
    if (!typeDef) return "Bell";
    return typeDef.options?.[Math.round(read(typeDef, 0))] ?? "Bell";
  }
}

function cutLabel(id: string): string {
  return id === "low_cut" ? "Low Cut" : "High Cut";
}

function channelLetter(target: ChannelTarget): string {
  return target === "stereo"
    ? "St"
    : target === "mid"
      ? "M"
      : target === "side"
        ? "S"
        : target === "left"
          ? "L"
          : "R";
}

function freqToX(freq: number): number {
  const t = Math.log(clamp(freq, 20, 20000) / 20) / Math.log(1000);
  return PLOT.left + t * PLOT.width;
}

function xToFreq(x: number): number {
  const t = clamp((x - PLOT.left) / PLOT.width, 0, 1);
  return 20 * Math.pow(1000, t);
}

function dbToY(db: number): number {
  return PLOT.top + (1 - (clamp(db, DB_MIN, DB_MAX) - DB_MIN) / (DB_MAX - DB_MIN)) * PLOT.height;
}

function yToDb(y: number): number {
  const t = clamp((y - PLOT.top) / PLOT.height, 0, 1);
  return DB_MIN + (1 - t) * (DB_MAX - DB_MIN);
}

function areaPath(upper: CurvePoint[], lower: CurvePoint[]): string {
  const forward = upper
    .map((point) => `${freqToX(point.freq).toFixed(2)},${dbToY(point.db).toFixed(2)}`)
    .join(" L");
  const backward = [...lower]
    .reverse()
    .map((point) => `${freqToX(point.freq).toFixed(2)},${dbToY(point.db).toFixed(2)}`)
    .join(" L");
  return `M${forward} L${backward} Z`;
}

function toPath(points: CurvePoint[]): string {
  return points
    .map((point, index) => {
      const x = freqToX(point.freq).toFixed(2);
      const y = dbToY(point.db).toFixed(2);
      return `${index === 0 ? "M" : "L"}${x},${y}`;
    })
    .join(" ");
}

function quantizeFrequency(freq: number): number {
  if (freq >= 1000) return Math.round(freq / 5) * 5;
  if (freq >= 100) return Math.round(freq);
  return roundTo(freq, 1);
}

function formatHz(freq: number): string {
  if (freq >= 1000) return `${Number((freq / 1000).toFixed(2))} kHz`;
  return `${Number(freq.toFixed(1))} Hz`;
}

function formatDb(db: number): string {
  const sign = db > 0 ? "+" : "";
  return `${sign}${db.toFixed(1)} dB`;
}

function roundTo(value: number, step: number): number {
  return Math.round(value / step) * step;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}
