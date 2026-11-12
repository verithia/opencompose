/**
 * Audio export request and result, mirroring `commands::render::RenderAudio*`.
 * The dialog sends every choice explicitly, so the backend never guesses.
 */

import type { ExportDepth } from "./project";

/** Which files one export publishes. */
export type ExportScope = "mix" | "stems";

/** The checkpoints a render passes, in order. */
export type ExportPhase = "rendering" | "encoding" | "publishing" | "done" | "cancelled";

/** Containers this build can write. The picker lists the rest as disabled. */
export type WritableContainer = "wav" | "flac" | "mp3";

/** Containers the dialog offers, with the reason the build cannot write them. */
export interface ExportContainerOption {
  id: "wav" | "flac" | "mp3";
  label: string;
  writable: boolean;
}

/**
 * The format list for a build that does or does not carry the MP3 encoder.
 *
 * MP3 is never hidden: a build without the encoder lists it disabled with the
 * reason `export.audio.mp3Unavailable`, so the option cannot appear to work and
 * then fail at export.
 */
export function exportContainers(mp3: boolean): ExportContainerOption[] {
  return [
    { id: "wav", label: "WAV", writable: true },
    { id: "flac", label: "FLAC", writable: true },
    { id: "mp3", label: "MP3", writable: mp3 },
  ];
}

/** Compression level a FLAC export uses when nothing says otherwise. */
export const DEFAULT_FLAC_LEVEL = 5;

/** CBR bitrate an MP3 export uses when nothing says otherwise, in kbps. */
export const DEFAULT_MP3_BITRATE = 192;

/** CBR bitrates MPEG-1 Layer III writes, in kbps. */
export const MP3_BITRATES = [
  32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
] as const;

/** CBR bitrates MPEG-2 and MPEG-2.5 Layer III write, in kbps. */
export const MP3_LOW_RATE_BITRATES = [
  8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160,
] as const;

/** VBR quality indices, 0 best to 9 smallest. */
export const MP3_VBR_QUALITIES = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] as const;

/**
 * Sample rates the dialog offers for MP3, in hertz.
 *
 * This is the encoder's table in fallback form; the dialog replaces it with
 * `ExportCapabilities.mp3_sample_rates` when the backend answers.
 */
export const MP3_SAMPLE_RATES: readonly number[] = [
  44100, 48000, 16000, 22050, 24000, 11025, 12000,
];

/** CBR bitrates the MPEG version carrying `sampleRate` writes, in kbps. */
export function mp3BitratesFor(sampleRate: number): readonly number[] {
  return sampleRate >= 32000 ? MP3_BITRATES : MP3_LOW_RATE_BITRATES;
}

/** A bitrate this build can write at `sampleRate`, preferring 192 then 160. */
export function mp3BitrateFor(sampleRate: number): number {
  const table = mp3BitratesFor(sampleRate);
  if (table.includes(DEFAULT_MP3_BITRATE)) return DEFAULT_MP3_BITRATE;
  return table.includes(160) ? 160 : table[table.length - 1];
}

/** Sample rates the dialog offers, in hertz. */
export const EXPORT_SAMPLE_RATES = [44100, 48000, 88200, 96000] as const;

/** Render resampling steps the engine accepts, 0 fastest to 7 best. */
export const EXPORT_RESAMPLE_STEPS = [0, 1, 2, 3, 4, 5, 6, 7] as const;

export interface ExportOverrides {
  container?: WritableContainer;
  depth?: ExportDepth;
  dither?: boolean;
  /** FLAC compression level, 0 fastest to 8 smallest. */
  flac_level?: number;
  /** MP3 CBR bitrate in kbps. */
  mp3_bitrate?: number;
  /** MP3 VBR quality index, 0 best to 9 smallest; missing is CBR. */
  mp3_vbr?: number;
  sample_rate?: number;
  resample?: number | null;
  /** False renders without the master loudness target. */
  normalise?: boolean;
}

/** What this build can encode, from the `export_capabilities` command. */
export interface ExportCapabilities {
  mp3: boolean;
  /** MP3 sample rates the encoder writes, straight from its table. */
  mp3_sample_rates?: number[];
}

export interface RenderAudioRequest {
  scope: ExportScope;
  /** Target file for the mix scope. */
  output_path?: string;
  /** Publish directory for the stems scope. */
  directory?: string;
  /** Base name of the stem files; missing uses the song name. */
  stem_name?: string;
  pre_roll?: number;
  options: ExportOverrides;
}

export interface RenderAudioResult {
  scope: ExportScope;
  /** The published file, or the mix of a stem set. */
  path: string;
  files: string[];
  stems: number;
  bytes: number;
  frames: number;
  sample_rate: number;
  container: WritableContainer;
  depth: ExportDepth;
  dither: boolean;
  peak_dbfs: number;
  true_peak_dbfs: number;
  /** True when the mix exceeded full scale, whether or not the file clamped. */
  above_full_scale: boolean;
  /** True when the written file clamped the overload. */
  clipped: boolean;
}

/** One `export-progress` event. */
export interface ExportProgress {
  generation: number;
  phase: ExportPhase;
  /** The same stage as `phase`, for consumers that read a named stage. */
  stage: string;
  /** Completed fraction of the stage, 0 to 1. */
  fraction: number;
  elapsed_ms: number;
}
