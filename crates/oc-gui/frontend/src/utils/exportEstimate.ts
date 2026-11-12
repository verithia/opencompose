/**
 * What an export will produce, before it runs: file size, length in bars and
 * H:M:S, and the file names a stem set will write.
 *
 * The numbers here are estimates the dialog shows; the backend still decides
 * the real size. WAV, FLAC and MP3 are writable; the MP3 estimate is the CBR
 * size at the chosen bitrate, so a VBR encode lands near the number rather than
 * on it.
 */

import type { ExportDepth } from "../types/project";
import { DEFAULT_FLAC_LEVEL } from "../types/export";

export type ExportContainerId = "wav" | "flac" | "mp3";

/** Bytes one sample of this depth occupies in a WAV file. */
function bytesPerSample(depth: ExportDepth): number {
  if (depth === "i16") return 2;
  if (depth === "i24") return 3;
  return 4;
}

/**
 * Fraction of the same-depth PCM file a FLAC stream takes at one level.
 *
 * The points are measured on the 20-track reference project at 48 kHz. A
 * 24-bit mix carries eight more bits of low-level detail than a 16-bit one, so
 * it compresses less; both curves flatten once LPC is enabled at level 4. The
 * dialog labels the result as an estimate, not a promise.
 */
const FLAC_RATIOS: Record<"i16" | "i24", [number, number][]> = {
  i16: [
    [0, 0.513],
    [4, 0.489],
    [5, 0.488],
    [8, 0.4865],
  ],
  i24: [
    [0, 0.743],
    [5, 0.702],
    [8, 0.694],
  ],
};

function flacRatio(level: number, depth: ExportDepth): number {
  const points = FLAC_RATIOS[depth === "i24" ? "i24" : "i16"];
  const clamped = Math.min(8, Math.max(0, Math.round(level)));
  for (let index = 1; index < points.length; index += 1) {
    const [upperLevel, upperRatio] = points[index];
    const [lowerLevel, lowerRatio] = points[index - 1];
    if (clamped <= upperLevel) {
      const span = upperLevel - lowerLevel;
      const t = span === 0 ? 0 : (clamped - lowerLevel) / span;
      return lowerRatio + (upperRatio - lowerRatio) * t;
    }
  }
  return points[points.length - 1][1];
}

export interface EstimateInput {
  container: ExportContainerId;
  seconds: number;
  sampleRate: number;
  depth: ExportDepth;
  channels?: number;
  /** FLAC only; missing means the dialog's default level. */
  flacLevel?: number;
  /** MP3 only: the CBR bitrate the estimate is computed from. */
  bitrateKbps?: number;
}

/** Approximate size of the file an export would write, in bytes. */
export function estimateExportBytes(input: EstimateInput): number | null {
  if (!Number.isFinite(input.seconds) || input.seconds <= 0) return null;
  if (!Number.isFinite(input.sampleRate) || input.sampleRate <= 0) return null;
  const channels = input.channels ?? 2;
  const frames = Math.ceil(input.seconds * input.sampleRate);
  const wavBytes = 44 + frames * channels * bytesPerSample(input.depth);
  switch (input.container) {
    case "wav":
      return wavBytes;
    // Lossless compression over the same integer grid; the level trades bytes
    // for search time.
    case "flac":
      return Math.round(wavBytes * flacRatio(input.flacLevel ?? DEFAULT_FLAC_LEVEL, input.depth));
    case "mp3":
      return Math.round(((input.bitrateKbps ?? 192) * 1000 * input.seconds) / 8) + 1024;
  }
}

/** "3:42" under an hour, "1:02:03" above it. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0:00";
  const total = Math.round(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = total % 60;
  const pad = (value: number) => value.toString().padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(rest)}` : `${minutes}:${pad(rest)}`;
}

/** A rate in kHz without losing 22.05 or 11.025. */
function formatRateKhz(sampleRate: number): string {
  const khz = sampleRate / 1000;
  return Number.isInteger(khz) ? `${khz}` : `${Number(khz.toFixed(3))}`;
}

/** "24-bit / 44.1 kHz" for the summary line. */
export function formatSampleDescription(depth: ExportDepth, sampleRate: number): string {
  const bits = depth === "i16" ? 16 : depth === "i24" ? 24 : 32;
  const depthLabel = depth === "f32" ? "32-bit float" : `${bits}-bit`;
  return `${depthLabel} / ${formatRateKhz(sampleRate)} kHz`;
}

/**
 * "192 kbps CBR / 44.1 kHz" for the summary line, or "VBR q2 / 44.1 kHz" when
 * the export asks for a quality index instead of a bitrate.
 */
export function formatMp3Description(
  bitrateKbps: number,
  sampleRate: number,
  vbrQuality: number | null,
): string {
  const control = vbrQuality === null ? `${bitrateKbps} kbps CBR` : `VBR q${vbrQuality}`;
  return `${control} / ${formatRateKhz(sampleRate)} kHz`;
}

/** "42 MB", "3.1 MB", "800 KB". */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(bytes >= 10_000_000 ? 0 : 1)} MB`;
  if (bytes >= 1000) return `${Math.round(bytes / 1000)} KB`;
  return `${bytes} B`;
}

/** The song name the stem set is built from: lower case, punctuation folded. */
export function stemSlug(value: string, fallback: string): string {
  let out = "";
  for (const character of value) {
    if (/[\p{L}\p{N}]/u.test(character)) out += character.toLowerCase();
    else if ((character === "-" || character === "_") && out.length > 0) out += character;
    else if (out.length > 0 && !out.endsWith("-")) out += "-";
  }
  const trimmed = out.replace(/^-+|-+$/g, "");
  return trimmed.length > 0 ? trimmed : fallback;
}

export interface StemNameSource {
  song: string;
  tracks: string[];
  buses: string[];
}

/**
 * Base names a stem set writes, mirroring `write_stems_export`: the mix first,
 * then one file per track and one per bus.
 */
export function stemFileNames(source: StemNameSource, extension = "wav"): string[] {
  const song = stemSlug(source.song, "song");
  const used = new Set<string>([`${song}-mix`]);
  const names = [`${song}-mix.${extension}`];
  const push = (base: string) => {
    let name = base;
    let suffix = 2;
    while (used.has(name)) {
      name = `${base}-${suffix}`;
      suffix += 1;
    }
    used.add(name);
    names.push(`${name}.${extension}`);
  };
  for (const track of source.tracks) push(`${song}-${stemSlug(track, "track")}`);
  for (const bus of source.buses) push(`${song}-bus-${stemSlug(bus, "bus")}`);
  return names;
}
