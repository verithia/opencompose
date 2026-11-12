/**
 * Export dialog plumbing shared by the toolbar, the File menu and Cmd/Ctrl+E:
 * the open request, the defaults a project declares, and the last-used choices
 * that survive a restart.
 *
 * The project file is the source of truth. `localStorage` only fills options
 * the project leaves out, so it can never silently override a saved project.
 */

import { createSignal } from "solid-js";
import type { ExportDepth, ExportSettings, FlacDepth, Project } from "../types/project";
import type { ExportScope, WritableContainer } from "../types/export";
import {
  DEFAULT_FLAC_LEVEL,
  DEFAULT_MP3_BITRATE,
  MP3_BITRATES,
  MP3_LOW_RATE_BITRATES,
  MP3_VBR_QUALITIES,
  mp3BitrateFor,
  mp3BitratesFor,
} from "../types/export";

export interface ExportDefaults {
  container: WritableContainer;
  depth: ExportDepth;
  dither: boolean;
  flacLevel: number;
  /** MP3 CBR bitrate in kbps. */
  mp3Bitrate: number;
  /** MP3 VBR quality index; null is a CBR encode. */
  mp3Vbr: number | null;
  sampleRate: number;
  resample: number | null;
  /**
   * The machine's last normalisation choice, when it stored one.
   *
   * It only fills a project that declares no `loudness:` target; a declared
   * target always starts enabled and can be turned off for one export.
   */
  normalise?: boolean;
}

export const LAST_USED_EXPORT_KEY = "opencompose.export.last";

const DEPTHS: ExportDepth[] = ["i16", "i24", "f32"];

function isDepth(value: unknown): value is ExportDepth {
  return typeof value === "string" && (DEPTHS as string[]).includes(value);
}

function isWritableContainer(value: unknown): value is WritableContainer {
  return value === "wav" || value === "flac" || value === "mp3";
}

function isMp3Bitrate(value: unknown): value is number {
  return (
    typeof value === "number" &&
    ((MP3_BITRATES as readonly number[]).includes(value) ||
      (MP3_LOW_RATE_BITRATES as readonly number[]).includes(value))
  );
}

function isMp3Vbr(value: unknown): value is number {
  return typeof value === "number" && (MP3_VBR_QUALITIES as readonly number[]).includes(value);
}

function isFlacLevel(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 8;
}

function isRate(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 8000 && value <= 192000;
}

/** The last export choices used on this device, or null when none were stored. */
export function readLastUsedExport(): Partial<ExportDefaults> | null {
  try {
    const raw = window.localStorage.getItem(LAST_USED_EXPORT_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return null;
    const record = parsed as Record<string, unknown>;
    const stored: Partial<ExportDefaults> = {};
    if (isWritableContainer(record.container)) stored.container = record.container;
    if (isDepth(record.depth)) stored.depth = record.depth;
    if (typeof record.dither === "boolean") stored.dither = record.dither;
    if (isFlacLevel(record.flacLevel)) stored.flacLevel = record.flacLevel;
    if (isMp3Bitrate(record.mp3Bitrate)) stored.mp3Bitrate = record.mp3Bitrate;
    if (isMp3Vbr(record.mp3Vbr)) stored.mp3Vbr = record.mp3Vbr;
    if (isRate(record.sampleRate)) stored.sampleRate = record.sampleRate;
    if (typeof record.resample === "number" && record.resample >= 0 && record.resample <= 7) {
      stored.resample = record.resample;
    }
    if (typeof record.normalise === "boolean") stored.normalise = record.normalise;
    return stored;
  } catch {
    return null;
  }
}

/** Remember the choices for the next dialog on this device. */
export function saveLastUsedExport(defaults: ExportDefaults): void {
  try {
    window.localStorage.setItem(LAST_USED_EXPORT_KEY, JSON.stringify(defaults));
  } catch {
    // A full or blocked store must not fail an export.
  }
}

/**
 * Merge stored export choices, keeping keys this build does not read: a bundle
 * import writes one setting without dropping the others.
 */
export function mergeLastUsedExport(patch: Partial<ExportDefaults>): void {
  try {
    const raw = window.localStorage.getItem(LAST_USED_EXPORT_KEY);
    let stored: Record<string, unknown> = {};
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
        stored = parsed as Record<string, unknown>;
      }
    }
    window.localStorage.setItem(
      LAST_USED_EXPORT_KEY,
      JSON.stringify({ ...stored, ...patch }),
    );
  } catch {
    // A full or blocked store must not fail an import.
  }
}

/** The project's `export:` block, if it declares one. */
export function projectExportSettings(project: Project): ExportSettings | null {
  return project.properties.export ?? null;
}

/**
 * Defaults for a new dialog: the project block, then the last-used choices,
 * then WAV / 16-bit / no dither / 44100.
 */
export function exportDefaults(project: Project): ExportDefaults {
  const declared = projectExportSettings(project);
  const last = readLastUsedExport();
  const container = declared?.container ?? last?.container ?? "wav";
  // A FLAC project declares its own depth; a WAV project keeps the WAV block.
  const depth =
    container === "flac"
      ? declared?.flac?.depth ?? last?.depth ?? "i16"
      : declared?.wav?.depth ?? last?.depth ?? "i16";
  const dither = declared?.wav?.dither ?? last?.dither ?? false;
  const flacLevel = declared?.flac?.level ?? last?.flacLevel ?? DEFAULT_FLAC_LEVEL;
  const mp3Bitrate = declared?.mp3?.bitrate ?? last?.mp3Bitrate ?? DEFAULT_MP3_BITRATE;
  const mp3Vbr = declared?.mp3?.vbr_quality ?? last?.mp3Vbr ?? null;
  const sampleRate = declared?.sample_rate ?? last?.sampleRate ?? 44100;
  const resample = declared?.resample ?? last?.resample ?? null;
  const resolvedDepth = isDepth(depth) ? depth : "i16";
  // Mutable choices are resolved once here; MP3 has no depth and no dither.
  const mp3 = container === "mp3";
  const resolvedRate = isRate(sampleRate) ? sampleRate : 44100;
  // A rate the container cannot write is kept so the dialog can refuse it; only
  // an unwritable bitrate is replaced, because no rate can rescue it.
  const bitrate = isMp3Bitrate(mp3Bitrate) ? mp3Bitrate : DEFAULT_MP3_BITRATE;
  return {
    container,
    // FLAC has no float depth; a stale or hand-written f32 falls back to 24-bit.
    depth: mp3 ? "i16" : container === "flac" && resolvedDepth === "f32" ? "i24" : resolvedDepth,
    // Dither cannot act on float output, and MP3 has no dither step at all.
    dither: container === "wav" && depth !== "f32" && dither,
    flacLevel: isFlacLevel(flacLevel) ? flacLevel : DEFAULT_FLAC_LEVEL,
    mp3Bitrate:
      mp3 && !mp3BitratesFor(resolvedRate).includes(bitrate)
        ? mp3BitrateFor(resolvedRate)
        : bitrate,
    mp3Vbr: isMp3Vbr(mp3Vbr) ? mp3Vbr : null,
    sampleRate: resolvedRate,
    resample: typeof resample === "number" && resample >= 0 && resample <= 7 ? resample : null,
    // A declared target always wins: a remembered "off" is a default for
    // projects that declare nothing, never an override of the project.
    normalise: project.master?.loudness_target != null ? true : last?.normalise,
  };
}

/** The value to write into the project's `export:` block for a dialog state. */
export function exportSettingsFor(defaults: ExportDefaults): ExportSettings {
  const flac = defaults.container === "flac";
  const mp3 = defaults.container === "mp3";
  // FLAC stores integers, so a stored f32 choice falls back to 24-bit.
  const depth = flac && defaults.depth === "f32" ? "i24" : defaults.depth;
  const settings: ExportSettings = {
    container: defaults.container,
    wav: { depth: mp3 ? "i16" : depth, dither: flac || mp3 ? false : defaults.dither },
    sample_rate: defaults.sampleRate,
    resample: defaults.resample,
  };
  if (flac) {
    settings.flac = { depth: depth as FlacDepth, level: defaults.flacLevel };
  }
  // The block only appears when it says something the defaults do not, so a
  // WAV or FLAC project keeps its historical spelling.
  if (mp3 && (defaults.mp3Bitrate !== DEFAULT_MP3_BITRATE || defaults.mp3Vbr !== null)) {
    settings.mp3 = {
      bitrate: defaults.mp3Bitrate,
      ...(defaults.mp3Vbr === null ? {} : { vbr_quality: defaults.mp3Vbr }),
    };
  }
  return settings;
}

// ── Dialog request ─────────────────────────────────────────────────────────

export interface ExportAudioRequest {
  scope: ExportScope;
}

const [dialogRequest, setDialogRequest] = createSignal<ExportAudioRequest | null>(null);

/** Open the one export dialog; `scope` presets the segmented control. */
export function openExportAudioDialog(scope: ExportScope = "mix"): void {
  setDialogRequest({ scope });
}

// The stem folder is machine-specific, so it stays out of the project file and
// only lives for this session.
let stemFolder: string | null = null;

export const exportAudioStore = {
  dialogRequest,
  close: () => setDialogRequest(null),
  stemFolder: () => stemFolder,
  setStemFolder: (folder: string | null) => {
    stemFolder = folder;
  },
};
