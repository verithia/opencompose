/**
 * Meter polling shared by every open panel and mixer strip. One 10 Hz poll
 * feeds all of them; polling stops as soon as the last subscriber unmounts.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import type {
  ChannelMeter,
  EffectMeter,
  MasterMonitor,
  MeterSnapshot,
  MonoCheck,
  NodeMeter,
  SpectrumMeter,
} from "../types/meters";
import { MeterHistory } from "../utils/meterHistory";

const POLL_MS = 100;
const MAX_GR_DB = 100;
/** Levels outside this range are not representable on a meter, in dBFS. */
export const MIN_LEVEL_DB = -120;
export const MAX_LEVEL_DB = 12;

const [snapshot, setSnapshot] = createSignal<MeterSnapshot | null>(null);
const [masterHistory, setMasterHistory] = createSignal<(number | null)[]>([]);
const history = new MeterHistory();
let timer: ReturnType<typeof setTimeout> | null = null;
let subscribers = 0;
let inFlight = false;

export { snapshot as meterSnapshot };
export { masterHistory };

/** Fetch one meter snapshot; shape failures clear the shared signal. */
export async function pollMeters(): Promise<void> {
  if (inFlight) return;
  inFlight = true;
  try {
    const next = normalizeSnapshot(await api.meterSnapshot());
    setSnapshot(next);
    // One history sample per poll: 100 polls cover the last 10 s.
    const master = selectChannelMeter(next, "master");
    if (next?.available && next.playing) {
      history.push(master?.peakDb ?? null);
    } else {
      history.push(null);
    }
    setMasterHistory(history.values());
  } catch {
    setSnapshot(null);
    history.push(null);
    setMasterHistory(history.values());
  } finally {
    inFlight = false;
  }
}

/** Drop the drawn history; the next poll starts a fresh window. */
export function clearMeterHistory(): void {
  history.reset();
  setMasterHistory(history.values());
}

/** Start polling; the returned release function is safe to call repeatedly. */
export function startMeterPolling(): () => void {
  subscribers += 1;
  if (subscribers === 1) {
    void pollMeters();
    schedule();
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    subscribers = Math.max(0, subscribers - 1);
    if (subscribers === 0 && timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };
}

/** Clear every latched clip indicator; the engine keeps the latches otherwise. */
export async function resetMeterClip(): Promise<void> {
  await api.resetMeterClip();
  await pollMeters();
}

/** Fold the monitored master to mono, or return it to stereo. */
export async function setMonitorMono(enabled: boolean): Promise<MonoCheck> {
  const mono = await api.setMonitorMono(enabled);
  await pollMeters();
  return mono;
}

/** Resolve the meter row for one effect; falls back to its chain total. */
export function selectMeter(
  snapshot: MeterSnapshot | null,
  target: string,
  effectIndex: number,
): EffectMeter | null {
  if (!snapshot) return null;
  const node = nodeFor(snapshot, target);
  if (!node) return null;
  const effect = node.effects[effectIndex];
  const gainReductionDb = clampGr(effect ?? node.gain_reduction_db);
  return {
    available: snapshot.available,
    playing: snapshot.playing,
    gainReductionDb,
    maxGainReductionDb: clampGr(
      node.effects_max_gain_reduction_db?.[effectIndex] ?? gainReductionDb,
    ),
    ceilingMarginDb: margin(node.effects_ceiling_margin_db?.[effectIndex]),
    inputPeakDb: peak(node.input_peak_db),
    outputPeakDb: peak(node.output_peak_db),
  };
}

/** Raw node row for a panel target, including per-band effect meters. */
export function selectNodeMeter(
  snapshot: MeterSnapshot | null,
  target: string,
): NodeMeter | null {
  return snapshot ? nodeFor(snapshot, target) : null;
}

/**
 * Peak and RMS of one mixer channel. `null` means the engine could not measure
 * anything: there is no reading to fabricate, so callers show a note instead.
 */
export function selectChannelMeter(
  snapshot: MeterSnapshot | null,
  target: string,
): ChannelMeter | null {
  if (!snapshot) return null;
  const node = nodeFor(snapshot, target);
  if (!node) return null;
  return {
    available: snapshot.available,
    playing: snapshot.playing,
    peakDb: peak(node.output_peak_db),
    rmsDb: peak(node.output_rms_db),
    clipped: node.clipped === true,
    reason: snapshot.reason ?? null,
  };
}

/** Everything the master monitoring row shows, in one row. */
export function selectMasterMonitor(
  snapshot: MeterSnapshot | null,
): MasterMonitor | null {
  if (!snapshot) return null;
  const meter = selectChannelMeter(snapshot, "master");
  return {
    available: snapshot.available,
    playing: snapshot.playing,
    meter,
    lufs: level(snapshot.master.short_term_lufs),
    windowSeconds: snapshot.master.short_term_window_seconds ?? 3,
    correlation: correlation(snapshot.master.correlation),
    goniometer: goniometer(snapshot.master.goniometer),
    mono: snapshot.mono,
    monoPending: snapshot.mono.requested !== snapshot.mono.applied,
    masterChannels: snapshot.master_channels,
    reason: snapshot.reason ?? null,
  };
}

function nodeFor(snapshot: MeterSnapshot, target: string): NodeMeter | null {
  if (target === "master") return snapshot.master;
  if (target.startsWith("track:")) {
    const index = Number(target.slice("track:".length));
    return snapshot.tracks.find((track) => track.index === index) ?? null;
  }
  if (target.startsWith("bus:")) {
    const name = target.slice("bus:".length);
    return snapshot.buses.find((bus) => bus.name === name) ?? null;
  }
  return null;
}

function schedule() {
  if (timer !== null || subscribers === 0) return;
  timer = setTimeout(() => {
    timer = null;
    if (subscribers === 0) return;
    void pollMeters().finally(() => schedule());
  }, POLL_MS);
}

function clampGr(value: number | undefined): number {
  return Number.isFinite(value) ? Math.min(MAX_GR_DB, Math.max(-MAX_GR_DB, value as number)) : 0;
}

/** Clamp one dBFS reading into the representable range; `null` stays absent. */
function peak(value: number | null | undefined): number | null {
  if (value === null || value === undefined || !Number.isFinite(value)) return null;
  return Math.min(MAX_LEVEL_DB, Math.max(MIN_LEVEL_DB, value as number));
}

/** Loudness is stored in LUFS, so it is not clamped as a dBFS reading. */
function level(value: number | null | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function correlation(value: number | null | undefined): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return Math.min(1, Math.max(-1, value));
}

function goniometer(value: number[] | undefined): number[] {
  if (!Array.isArray(value)) return [];
  return value.filter((sample) => Number.isFinite(sample)).map((sample) => Math.min(16, Math.max(-16, sample)));
}

function margin(value: number | null | undefined): number | null {
  if (!Number.isFinite(value)) return null;
  return Math.min(MAX_GR_DB, Math.max(-MAX_GR_DB, value as number));
}

function normalizeSnapshot(raw: unknown): MeterSnapshot | null {
  if (!raw || typeof raw !== "object") return null;
  const candidate = raw as Partial<MeterSnapshot>;
  if (typeof candidate.available !== "boolean") return null;
  if (typeof candidate.playing !== "boolean") return null;
  if (!candidate.master || typeof candidate.master !== "object") return null;
  if (!Array.isArray(candidate.tracks) || !Array.isArray(candidate.buses)) return null;
  const spectrum = normalizeSpectrum(candidate.spectrum);
  return {
    available: candidate.available,
    reason: typeof candidate.reason === "string" ? candidate.reason : null,
    playing: candidate.playing,
    master_channels:
      typeof candidate.master_channels === "number" && Number.isFinite(candidate.master_channels)
        ? Math.max(0, Math.trunc(candidate.master_channels))
        : 0,
    mono: normalizeMono(candidate.mono),
    master: {
      ...normalizeNode(candidate.master),
      short_term_lufs: level(candidate.master.short_term_lufs),
      short_term_window_seconds:
        typeof candidate.master.short_term_window_seconds === "number" &&
        Number.isFinite(candidate.master.short_term_window_seconds)
          ? candidate.master.short_term_window_seconds
          : 3,
      correlation: correlation(candidate.master.correlation),
      goniometer: goniometer(candidate.master.goniometer),
    },
    tracks: candidate.tracks.flatMap((track) =>
      track && Number.isInteger(track.index)
        ? [{ ...normalizeNode(track), index: track.index as number }]
        : [],
    ),
    buses: candidate.buses.flatMap((bus) =>
      bus && typeof bus.name === "string"
        ? [{ ...normalizeNode(bus), name: bus.name }]
        : [],
    ),
    ...(spectrum ? { spectrum } : {}),
  };
}

function normalizeMono(raw: unknown): MonoCheck {
  const candidate = (raw ?? {}) as Partial<MonoCheck>;
  return {
    requested: candidate.requested === true,
    applied: candidate.applied === true,
  };
}

function normalizeNode(raw: NodeMeter): NodeMeter {
  const effects = Array.isArray(raw.effects)
    ? raw.effects.map((value) => clampGr(value))
    : [];
  return {
    gain_reduction_db: clampGr(raw.gain_reduction_db),
    input_peak_db: peak(raw.input_peak_db),
    output_peak_db: peak(raw.output_peak_db),
    output_rms_db: peak(raw.output_rms_db),
    clipped: raw.clipped === true,
    effect_bands: Array.isArray(raw.effect_bands)
      ? raw.effect_bands.map((bands) =>
          Array.isArray(bands) ? bands.map((value) => clampGr(value)) : [],
        )
      : [],
    effects,
    effects_max_gain_reduction_db: Array.isArray(raw.effects_max_gain_reduction_db)
      ? raw.effects_max_gain_reduction_db.map((value) => clampGr(value))
      : effects,
    effects_ceiling_margin_db: Array.isArray(raw.effects_ceiling_margin_db)
      ? raw.effects_ceiling_margin_db.map((value) => margin(value))
      : [],
  };
}

function normalizeSpectrum(raw: unknown): SpectrumMeter | null {
  if (!raw || typeof raw !== "object") return null;
  const candidate = raw as Partial<SpectrumMeter>;
  if (!Array.isArray(candidate.bands)) return null;
  return {
    bands: candidate.bands.map((value) =>
      Number.isFinite(value) ? Math.min(0, Math.max(-160, value as number)) : -160,
    ),
  };
}
