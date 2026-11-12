/**
 * The preferences bundle: one portable JSON file carrying the settings this
 * build keeps outside a project — theme, language, interface scale, audio and
 * MIDI devices, export choices, the shared grid, the keymap, and the recent
 * directories.
 *
 * `buildPreferencesBundle` reads each store; `applyPreferences` writes one leaf
 * at a time through the store that owns it, so a value the store refuses comes
 * back as a named skipped field instead of vanishing.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { i18n, LOCALES, type LocaleName } from "./i18n";
import { themeStore } from "./theme";
import { uiScaleStore } from "./uiScale";
import { audioIoStore, type AudioIoView } from "./audioIo";
import { midiStore } from "./midiLive";
import { GRID_OPTIONS, gridStore } from "./grid";
import { workspaceStore } from "./workspace";
import { mergeLastUsedExport, readLastUsedExport, type ExportDefaults } from "./exportAudio";
import { MP3_BITRATES, MP3_VBR_QUALITIES } from "../types/export";
import type { ExportDepth } from "../types/project";

/** Bundle schema this build writes and reads. */
export const PREFERENCES_SCHEMA = "opencompose.preferences.v1";

/** Mirrors the crate version; the export command stamps the build that writes. */
export const APP_VERSION = "0.1.0";

/** The keymap store's own key; this module only copies the object. */
const KEYMAP_STORAGE_KEY = "opencompose.keymap";

const UI_SCALE_RANGE = [0.7, 1.5] as const;
const MIDI_CHANNEL_RANGE = [1, 16] as const;
const FLAC_LEVEL_RANGE = [0, 8] as const;
const MP3_BITRATE_RANGE = [32, 320] as const;
const MP3_VBR_RANGE = [0, 9] as const;

export interface ThemePreference {
  family: string;
  variant: string;
}

export interface AudioPreference {
  /** Backend-stable device key; `null` is the system default. */
  output_device: string | null;
  sample_rate: number | null;
  buffer_size: number | null;
}

export interface MidiPreference {
  /** Input port name; `null` selects no port. */
  input: string | null;
  /** 1-based channel; `null` carries no channel preference. */
  channel: number | null;
  monitor: boolean | null;
}

export interface ExportPreference {
  format: string | null;
  bit_depth: number | null;
  flac_level: number | null;
  normalise: boolean | null;
  /** MP3 CBR bitrate in kbps; null for the other containers. */
  mp3_bitrate: number | null;
  /** MP3 VBR quality index; null is a CBR encode. */
  mp3_vbr: number | null;
}

export interface GridPreference {
  subdivision: number | null;
}

export interface WorkspacePreference {
  recent_directories: string[] | null;
}

/** One bundle. Every field is optional, so a file may carry one setting or all. */
export interface PreferencesBundle {
  schema: string;
  exported_at: string | null;
  app_version: string | null;
  theme: ThemePreference | null;
  locale: string | null;
  ui_scale: number | null;
  audio: AudioPreference | null;
  midi: MidiPreference | null;
  export: ExportPreference | null;
  grid: GridPreference | null;
  /** Carried opaque; the keymap store reads it on the next start. */
  keymap: Record<string, unknown> | null;
  workspace: WorkspacePreference | null;
}

/** One leaf a reader refused, with the reason it gives. */
export interface SkippedPreference {
  field: string;
  reason: string;
}

/** What `preferences_import` answers before anything is applied. */
export interface PreferencesImportPreview {
  bundle: PreferencesBundle;
  /** Leaf names the backend accepted; the confirmation counts these. */
  applied: string[];
  skipped: SkippedPreference[];
  /** A non-empty list refuses the file. */
  errors: string[];
}

/** What applying the bundle to the stores reports. */
export interface PreferencesApplySummary {
  applied: string[];
  skipped: SkippedPreference[];
}

function depthForBits(bits: number): ExportDepth | null {
  if (bits === 16) return "i16";
  if (bits === 24) return "i24";
  if (bits === 32) return "f32";
  return null;
}

function bitsForDepth(depth: ExportDepth): number {
  if (depth === "i16") return 16;
  if (depth === "i24") return 24;
  return 32;
}

/** Whether a bundle may carry this MP3 CBR bitrate, in kbps. */
function isBundleBitrate(bitrate: unknown): bitrate is number {
  return (
    typeof bitrate === "number" &&
    Number.isInteger(bitrate) &&
    bitrate >= MP3_BITRATE_RANGE[0] &&
    bitrate <= MP3_BITRATE_RANGE[1] &&
    (MP3_BITRATES as readonly number[]).includes(bitrate)
  );
}

/** Whether a bundle may carry this MP3 VBR quality index. */
function isBundleVbr(quality: unknown): quality is number {
  return (
    typeof quality === "number" &&
    Number.isInteger(quality) &&
    quality >= MP3_VBR_RANGE[0] &&
    quality <= MP3_VBR_RANGE[1] &&
    (MP3_VBR_QUALITIES as readonly number[]).includes(quality)
  );
}

/** The CBR bitrate an MP3 record carries; null keeps the dialog's default. */
function bitrateForBundle(bitrate: number | undefined): number | null {
  return isBundleBitrate(bitrate) ? bitrate : null;
}

function readStoredKeymap(): Record<string, unknown> | null {
  try {
    const raw = window.localStorage.getItem(KEYMAP_STORAGE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/** The bundle this device would export right now. */
export function buildPreferencesBundle(): PreferencesBundle {
  const stored = readLastUsedExport();
  const selection = audioIoStore.status().selection;
  const midi = midiStore.config();
  return {
    schema: PREFERENCES_SCHEMA,
    exported_at: new Date().toISOString(),
    app_version: APP_VERSION,
    theme: { family: themeStore.family().id, variant: themeStore.variant().id },
    locale: i18n.locale(),
    ui_scale: uiScaleStore.scale(),
    audio: {
      output_device: selection.output_device,
      sample_rate: selection.sample_rate,
      buffer_size: selection.buffer_frames,
    },
    midi: {
      input: midi.input,
      // No channel preference is readable here; the field stays empty.
      channel: null,
      monitor: midi.monitor,
    },
    export: stored === null
      ? null
      : {
        format: stored.container ?? "wav",
        bit_depth: bitsForDepth(stored.depth ?? "i16"),
        flac_level: stored.flacLevel ?? 5,
        normalise: stored.normalise ?? null,
        // Both MP3 fields belong to an MP3 record only.
        mp3_bitrate: stored.container === "mp3" ? bitrateForBundle(stored.mp3Bitrate) : null,
        mp3_vbr: stored.container === "mp3" ? stored.mp3Vbr ?? null : null,
      },
    grid: { subdivision: gridStore.division() },
    keymap: readStoredKeymap(),
    workspace: { recent_directories: workspaceStore.recents().map((entry) => entry.path) },
  };
}

/** How many leaves this bundle carries; the export dialog reports it. */
export function countBundleSettings(bundle: PreferencesBundle): number {
  let count = 0;
  if (bundle.theme !== null) count += 1;
  if (bundle.locale !== null) count += 1;
  if (bundle.ui_scale !== null) count += 1;
  if (bundle.audio !== null) {
    for (const value of [
      bundle.audio.output_device,
      bundle.audio.sample_rate,
      bundle.audio.buffer_size,
    ]) {
      if (value !== null) count += 1;
    }
  }
  if (bundle.midi !== null) {
    for (const value of [bundle.midi.input, bundle.midi.channel, bundle.midi.monitor]) {
      if (value !== null) count += 1;
    }
  }
  if (bundle.export !== null) {
    for (const value of [
      bundle.export.format,
      bundle.export.bit_depth,
      bundle.export.flac_level,
      bundle.export.normalise,
      bundle.export.mp3_bitrate,
      bundle.export.mp3_vbr,
    ]) {
      if (value !== null) count += 1;
    }
  }
  if (bundle.grid?.subdivision != null) count += 1;
  if (bundle.keymap !== null) count += 1;
  if (bundle.workspace?.recent_directories != null) count += 1;
  return count;
}

/** Write the current preferences to `path`; answers the number of settings. */
export async function exportPreferencesFile(path: string): Promise<number> {
  const bundle = buildPreferencesBundle();
  await invoke("preferences_export", { path, json: JSON.stringify(bundle, null, 2) });
  return countBundleSettings(bundle);
}

/** Validate a file without applying it; a refused file answers `errors`. */
export function importPreferencesFile(path: string): Promise<PreferencesImportPreview> {
  return invoke<PreferencesImportPreview>("preferences_import", { path });
}

interface Report {
  applied: string[];
  skipped: SkippedPreference[];
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Run one leaf, turning a refusal into a skipped entry naming it. */
function attempt(report: Report, field: string, run: () => void): void {
  try {
    run();
  } catch (error) {
    report.skipped.push({ field, reason: message(error) });
  }
}

async function attemptAsync(
  report: Report,
  field: string,
  run: () => Promise<void>,
): Promise<void> {
  try {
    await run();
  } catch (error) {
    report.skipped.push({ field, reason: message(error) });
  }
}

function applyTheme(theme: ThemePreference | null, report: Report): void {
  if (theme === null) return;
  attempt(report, "theme", () => {
    const family = themeStore.catalog.find((entry) => entry.id === theme.family);
    const variant = family?.variants.find((entry) => entry.id === theme.variant);
    if (!family || !variant) {
      throw new Error(`theme '${theme.family}/${theme.variant}' is not in this build's catalog`);
    }
    themeStore.setTheme(family.id, variant.id);
    report.applied.push("theme");
  });
}

function applyLocale(locale: string | null, report: Report): void {
  if (locale === null) return;
  attempt(report, "locale", () => {
    if (!(LOCALES as readonly string[]).includes(locale)) {
      throw new Error(`locale '${locale}' is not one of ${LOCALES.join(", ")}`);
    }
    i18n.setLocale(locale as LocaleName);
    report.applied.push("locale");
  });
}

function applyUiScale(scale: number | null, report: Report): void {
  if (scale === null) return;
  attempt(report, "ui_scale", () => {
    if (scale < UI_SCALE_RANGE[0] || scale > UI_SCALE_RANGE[1]) {
      throw new Error(`ui_scale ${scale} is outside ${UI_SCALE_RANGE[0]}–${UI_SCALE_RANGE[1]}`);
    }
    uiScaleStore.set(scale);
    report.applied.push("ui_scale");
  });
}

/** The device the audio section is currently showing, or null. */
function currentDevice() {
  const status = audioIoStore.status();
  const key = status.opened?.key ?? status.selection.output_device;
  return audioIoStore.devices().find((entry) => entry.key === key) ?? null;
}

/** The sentence a period refusal carries, or null when the period was taken. */
function bufferRefusal(view: AudioIoView): string | null {
  for (const notice of view.status.notices) {
    if (notice.kind === "buffer_clamped") {
      return `the device clamped the period to ${notice.chosen} frames`;
    }
    if (notice.kind === "buffer_unsupported") {
      return `the device does not support ${notice.requested} frames`;
    }
    if (notice.kind === "buffer_rejected") {
      return `the backend rejected the period: ${notice.error}`;
    }
  }
  return null;
}

async function applyAudio(audio: AudioPreference | null, report: Report): Promise<void> {
  if (audio === null) return;
  // A snapshot first: the device list decides what this device can apply.
  await audioIoStore.readStatus();

  if (audio.output_device !== null) {
    await attemptAsync(report, "audio.output_device", async () => {
      const device = audioIoStore.devices().find((entry) => entry.key === audio.output_device);
      if (!device) throw new Error(`output device '${audio.output_device}' is not connected`);
      const view = await audioIoStore.selectOutput(device.key);
      if (!view) throw new Error(audioIoStore.error() ?? "the audio backend refused the device");
      report.applied.push("audio.output_device");
    });
  }

  const rate = audio.sample_rate;
  if (rate !== null) {
    await attemptAsync(report, "audio.sample_rate", async () => {
      const device = currentDevice();
      if (device && device.sample_rates.length > 0 && !device.sample_rates.includes(rate)) {
        throw new Error(`${rate} Hz is not offered by '${device.name}'`);
      }
      const view = await audioIoStore.selectRate(rate);
      if (!view) throw new Error(audioIoStore.error() ?? "the audio backend refused the rate");
      const substituted = view.status.notices.find((notice) => notice.kind === "rate_substituted");
      if (substituted && substituted.kind === "rate_substituted") {
        throw new Error(`the device opened ${substituted.chosen} Hz instead`);
      }
      report.applied.push("audio.sample_rate");
    });
  }

  const frames = audio.buffer_size;
  if (frames !== null) {
    await attemptAsync(report, "audio.buffer_size", async () => {
      const device = currentDevice();
      const range = device?.buffer_frames;
      if (range && (frames < range[0] || frames > range[1])) {
        throw new Error(`${frames} frames is outside this device's ${range[0]}–${range[1]}`);
      }
      const view = await audioIoStore.setBuffer(frames);
      if (!view) throw new Error(audioIoStore.error() ?? "the audio backend refused the period");
      const refusal = bufferRefusal(view);
      if (refusal) throw new Error(refusal);
      report.applied.push("audio.buffer_size");
    });
  }
}

async function applyMidi(midi: MidiPreference | null, report: Report): Promise<void> {
  if (midi === null) return;

  if (midi.input !== null) {
    await attemptAsync(report, "midi.input", async () => {
      const port = midiStore.ports().inputs.find((entry) => entry.name === midi.input);
      if (!port) throw new Error(`MIDI input '${midi.input}' is not connected`);
      const view = await midiStore.selectInput(midi.input);
      if (!view) throw new Error(midiStore.config().error ?? "the MIDI backend refused the input");
      report.applied.push("midi.input");
    });
  }

  const monitor = midi.monitor;
  if (monitor !== null) {
    await attemptAsync(report, "midi.monitor", async () => {
      if (monitor && !midiStore.config().input) {
        throw new Error("monitoring needs a MIDI input port");
      }
      const view = await midiStore.setMonitor(monitor);
      if (!view) throw new Error(midiStore.config().error ?? "the MIDI backend refused monitoring");
      report.applied.push("midi.monitor");
    });
  }

  if (midi.channel !== null) {
    const [min, max] = MIDI_CHANNEL_RANGE;
    const detail = midi.channel >= min && midi.channel <= max
      ? "this build has no MIDI channel preference to write"
      : `channel ${midi.channel} is outside ${min}–${max}`;
    report.skipped.push({ field: "midi.channel", reason: detail });
  }
}

function applyExport(record: ExportPreference | null, report: Report): void {
  if (record === null) return;
  const patch: Partial<ExportDefaults> = {};
  const fields: string[] = [];
  const mp3 = record.format === "mp3";

  if (record.format !== null) {
    if (record.format !== "wav" && record.format !== "flac" && record.format !== "mp3") {
      report.skipped.push({
        field: "export.format",
        reason: `format '${record.format}' is not wav, flac or mp3`,
      });
    } else {
      patch.container = record.format;
      fields.push("export.format");
    }
  }
  if (record.bit_depth !== null && !mp3) {
    // MP3 consumes 16-bit PCM, so its depth carries no choice.
    const depth = depthForBits(record.bit_depth);
    if (depth === null) {
      report.skipped.push({
        field: "export.bit_depth",
        reason: `${record.bit_depth}-bit is not one of 16, 24, 32`,
      });
    } else {
      patch.depth = depth;
      fields.push("export.bit_depth");
    }
  }
  if (record.flac_level !== null) {
    const [min, max] = FLAC_LEVEL_RANGE;
    if (!Number.isInteger(record.flac_level) || record.flac_level < min || record.flac_level > max) {
      report.skipped.push({
        field: "export.flac_level",
        reason: `FLAC level ${record.flac_level} is outside ${min}–${max}`,
      });
    } else {
      patch.flacLevel = record.flac_level;
      fields.push("export.flac_level");
    }
  }
  if (record.normalise !== null) {
    patch.normalise = record.normalise;
    fields.push("export.normalise");
  }
  if (record.mp3_bitrate !== null) {
    if (!isBundleBitrate(record.mp3_bitrate)) {
      report.skipped.push({
        field: "export.mp3_bitrate",
        reason:
          `MP3 bitrate ${record.mp3_bitrate} kbps is outside ${MP3_BITRATE_RANGE[0]}–${MP3_BITRATE_RANGE[1]}`,
      });
    } else {
      patch.mp3Bitrate = record.mp3_bitrate;
      fields.push("export.mp3_bitrate");
    }
  }
  if (record.mp3_vbr !== null) {
    if (!isBundleVbr(record.mp3_vbr)) {
      report.skipped.push({
        field: "export.mp3_vbr",
        reason:
          `MP3 VBR quality ${record.mp3_vbr} is outside ${MP3_VBR_RANGE[0]}–${MP3_VBR_RANGE[1]}`,
      });
    } else {
      patch.mp3Vbr = record.mp3_vbr;
      fields.push("export.mp3_vbr");
    }
  } else if (mp3) {
    // An MP3 bundle with no quality index asks for a constant bitrate.
    patch.mp3Vbr = null;
    fields.push("export.mp3_vbr");
  }

  attempt(report, "export", () => {
    mergeLastUsedExport(patch);
    report.applied.push(...fields);
  });
}

function applyGrid(grid: GridPreference | null, report: Report): void {
  const subdivision = grid?.subdivision;
  if (subdivision === null || subdivision === undefined) return;
  attempt(report, "grid.subdivision", () => {
    const option = GRID_OPTIONS.find((entry) => entry.value === subdivision);
    if (!option) {
      const values = GRID_OPTIONS.map((entry) => entry.value).join(", ");
      throw new Error(`subdivision ${subdivision} is not one of ${values}`);
    }
    gridStore.setDivision(option.value);
    report.applied.push("grid.subdivision");
  });
}

function applyKeymap(keymap: Record<string, unknown> | null, report: Report): void {
  if (keymap === null) return;
  attempt(report, "keymap", () => {
    window.localStorage.setItem(KEYMAP_STORAGE_KEY, JSON.stringify(keymap));
    report.applied.push("keymap");
  });
}

function applyWorkspace(workspace: WorkspacePreference | null, report: Report): void {
  const directories = workspace?.recent_directories;
  if (!directories || directories.length === 0) return;
  report.skipped.push({
    field: "workspace.recent_directories",
    reason:
      `this build cannot write the recent-directory list; open ${directories.length} folder(s) once to re-add them`,
  });
}

/** Apply a validated bundle through the stores, one leaf at a time. */
export async function applyPreferences(bundle: PreferencesBundle): Promise<PreferencesApplySummary> {
  const report: Report = { applied: [], skipped: [] };
  applyTheme(bundle.theme, report);
  applyLocale(bundle.locale, report);
  applyUiScale(bundle.ui_scale, report);
  await applyAudio(bundle.audio, report);
  await applyMidi(bundle.midi, report);
  applyExport(bundle.export, report);
  applyGrid(bundle.grid, report);
  applyKeymap(bundle.keymap, report);
  applyWorkspace(bundle.workspace, report);
  return report;
}

// ── Dialog requests ────────────────────────────────────────────────────────

export interface PreferencesRequest {
  kind: "export" | "import";
  path: string;
}

const [request, setRequest] = createSignal<PreferencesRequest | null>(null);
const [pickError, setPickError] = createSignal<string | null>(null);

/** Ask where to write the bundle; answers whether a path was chosen. */
export async function openPreferencesExportDialog(): Promise<boolean> {
  try {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const chosen = await save({
      title: i18n.t("preferences.exportTitle"),
      defaultPath: "opencompose-preferences.json",
      filters: [{ name: i18n.t("preferences.fileFilter"), extensions: ["json"] }],
    });
    if (typeof chosen === "string" && chosen) {
      setPickError(null);
      setRequest({ kind: "export", path: chosen });
      return true;
    }
    return false;
  } catch (error) {
    setPickError(i18n.t("preferences.pickExportFailed", { error: String(error) }));
    return false;
  }
}

/** Ask for a bundle file; answers whether a path was chosen. */
export async function openPreferencesImportDialog(): Promise<boolean> {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const chosen = await open({
      title: i18n.t("preferences.importTitle"),
      multiple: false,
      filters: [{ name: i18n.t("preferences.fileFilter"), extensions: ["json"] }],
    });
    if (typeof chosen === "string" && chosen) {
      setPickError(null);
      setRequest({ kind: "import", path: chosen });
      return true;
    }
    return false;
  } catch (error) {
    setPickError(i18n.t("preferences.pickImportFailed", { error: String(error) }));
    return false;
  }
}

export const preferencesStore = {
  request,
  pickError,
  close: () => setRequest(null),
  /** A picker failure the popover shows next to the two actions. */
  fail: (message: string) => setPickError(message),
  /** Clears the run state between tests. */
  reset: () => {
    setRequest(null);
    setPickError(null);
  },
};
