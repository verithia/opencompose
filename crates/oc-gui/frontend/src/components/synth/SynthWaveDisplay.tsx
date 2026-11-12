import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { i18n } from "../../stores/i18n";
import { synthWaveformPreview, type WaveformPreview } from "../../stores/api";
export type { WaveformPreview };
import type { ParamValue, PresetDef } from "../../types/project";
import "./SynthWaveDisplay.css";

/** Samples the backend renders; ~2.8 cycles of C3 at 48 kHz. */
export const PREVIEW_SAMPLES = 1024;
/** One request per settled edit burst, not one per slider event. */
export const PREVIEW_DEBOUNCE_MS = 70;
const MAX_TRACES = 8;
const MAX_POINTS = 256;
const TAU = Math.PI * 2;

function paramText(value: ParamValue): string {
  if ("Number" in value) return String(value.Number);
  if ("Db" in value) return String(value.Db);
  if ("Percentage" in value) return String(value.Percentage);
  if ("Frequency" in value) return String(value.Frequency);
  if ("Cents" in value) return String(value.Cents);
  if ("Time" in value) return String(value.Time);
  if ("String" in value) return value.String;
  if ("Bool" in value) return value.Bool ? "1" : "0";
  if ("Array" in value) return `[${value.Array.join(",")}]`;
  return "";
}

/** Every stored patch value with live slider drafts layered on top. */
export function previewParams(
  preset: PresetDef,
  drafts: Record<string, number>,
): [string, string | null][] {
  const merged = new Map<string, string | null>();
  for (const [key, value] of preset.params) merged.set(key, paramText(value));
  for (const [key, value] of Object.entries(drafts)) merged.set(key, String(value));
  return [...merged];
}

/** Static shape used before the first reply and when the command fails. */
function staticSample(waveform: number, phase: number): number {
  if (waveform === 0) return Math.sin(phase * TAU);
  if (waveform === 1) return phase * 2 - 1;
  if (waveform === 2) return phase < 0.5 ? 1 : -1;
  if (waveform === 3) return 1 - 4 * Math.abs(phase - 0.5);
  return (
    (Math.sin(phase * TAU) + 0.42 * Math.sin(phase * TAU * 3) + 0.2 * Math.sin(phase * TAU * 5))
    / 1.62
  );
}

function staticPoints(waveform: number): string {
  const points: string[] = [];
  for (let x = 0; x <= 100; x += 2) {
    points.push(`${x},${(25 - staticSample(waveform, x / 100) * 17).toFixed(2)}`);
  }
  return points.join(" ");
}

/** Map samples onto the 100 x 50 view box, normalised by the returned peak. */
export function tracePoints(samples: number[], peak: number): string {
  const count = samples.length;
  if (count === 0) return "";
  const scale = 17 / Math.max(Math.abs(peak), 1e-6);
  const stride = Math.max(1, Math.ceil(count / MAX_POINTS));
  const points: string[] = [];
  for (let index = 0; index < count; index += stride) {
    const x = (index / Math.max(1, count - 1)) * 100;
    points.push(`${x.toFixed(2)},${(25 - samples[index] * scale).toFixed(2)}`);
  }
  if ((count - 1) % stride !== 0) {
    points.push(`100,${(25 - samples[count - 1] * scale).toFixed(2)}`);
  }
  return points.join(" ");
}

function validPreview(value: unknown): value is WaveformPreview {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<WaveformPreview>;
  return (
    Array.isArray(candidate.mono)
    && candidate.mono.length > 0
    && candidate.mono.every((sample) => typeof sample === "number" && Number.isFinite(sample))
    && Array.isArray(candidate.traces)
    && typeof candidate.voices === "number"
    && typeof candidate.peak === "number"
    && typeof candidate.rms === "number"
  );
}

export function SynthWaveDisplay(props: {
  preset: PresetDef;
  drafts: Record<string, number>;
  oscillator: 0 | 1 | 2;
  waveform: number;
  label: string;
}) {
  const [preview, setPreview] = createSignal<WaveformPreview | null>(null);
  const [failed, setFailed] = createSignal(false);
  let timer: number | undefined;
  let disposed = false;
  let running = false;
  let pending = false;
  let generation = 0;
  const key = createMemo(() => JSON.stringify(previewParams(props.preset, props.drafts)));

  const request = async () => {
    if (disposed) return;
    const id = ++generation;
    try {
      const result = await synthWaveformPreview(
        previewParams(props.preset, props.drafts),
        props.oscillator,
        PREVIEW_SAMPLES,
      );
      if (disposed || id !== generation) return;
      if (!validPreview(result)) {
        setFailed(true);
        return;
      }
      setPreview(result);
      setFailed(false);
    } catch {
      // A failed preview must not take the editor down; keep the static shape.
      if (!disposed && id === generation) setFailed(true);
    }
  };

  const run = async () => {
    if (disposed) return;
    if (running) {
      pending = true;
      return;
    }
    running = true;
    do {
      pending = false;
      await request();
    } while (pending && !disposed);
    running = false;
  };

  createEffect(() => {
    key();
    if (disposed) return;
    if (timer !== undefined) window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      timer = undefined;
      void run();
    }, PREVIEW_DEBOUNCE_MS);
  });

  onCleanup(() => {
    // The IPC call cannot be aborted; suppress its result instead.
    disposed = true;
    if (timer !== undefined) window.clearTimeout(timer);
  });

  const live = () => {
    const current = preview();
    return current && !failed() ? current : null;
  };
  const points = createMemo(() => {
    const current = live();
    return current ? tracePoints(current.mono, current.peak) : staticPoints(props.waveform);
  });
  const voices = createMemo(() => {
    const current = live();
    if (!current || current.voices <= 1) return [];
    return current.traces.slice(0, MAX_TRACES).map((trace) => tracePoints(trace, current.peak));
  });
  const readout = createMemo(() => {
    const current = live();
    if (!current) return "";
    const count = current.more_voices ? `${current.voices}v+` : `${current.voices}v`;
    return `pk ${current.peak.toFixed(2)} rms ${current.rms.toFixed(2)} · ${count}`;
  });

  return (
    <svg
      class="synth-wave-display"
      viewBox="0 0 100 50"
      role="img"
      aria-label={i18n.t("synth.waveformLabel", { name: props.label })}
      data-fallback={live() ? "false" : "true"}
    >
      <path class="synth-graph-grid" d="M0 25H100 M25 0V50 M50 0V50 M75 0V50" />
      <Show when={voices().length > 0}>
        <For each={voices()}>
          {(trace) => <polyline class="synth-wave-voice" points={trace} />}
        </For>
      </Show>
      <polyline class="synth-graph-line" points={points()} />
      <Show when={readout()}>
        <text x="2" y="6" class="synth-wave-readout">{readout()}</text>
      </Show>
      <Show when={failed()}>
        <text x="98" y="47" text-anchor="end" class="synth-wave-fallback">static</text>
      </Show>
    </svg>
  );
}
