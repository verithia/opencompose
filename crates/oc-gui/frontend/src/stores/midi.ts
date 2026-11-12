/**
 * MIDI import/export adapter — the only module that talks to the backend.
 *
 * Shapes mirror `opencompose_engine::midi` (docs/midi-import-export.md). When
 * the core commands change, this file is the one place the UI must follow.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ProjectSnapshot } from "../types/project";

// ============================================================================
// Import
// ============================================================================

export interface MidiIgnoredEvent {
  kind: string;
  count: number;
}

export interface MidiTrackSummary {
  source_track: number;
  source_track_name: string | null;
  target_track: number | null;
  name: string;
  channel: number | null;
  program: number | null;
  program_name: string | null;
  preset: string | null;
  instrument: string | null;
  note_count: number;
  lanes: string[];
  first_bar: number | null;
  last_bar: number | null;
  ignored: MidiIgnoredEvent[];
  dropped: boolean;
}

export interface MidiImportSummary {
  format: number;
  source_ppq: number;
  target_ppq: number;
  max_tick_error: number;
  ticks_shifted: number;
  tempo_changes: number;
  meter_changes: number;
  key_changes: number;
  sustain_expanded_notes: number;
  overlapping_notes: number;
  zero_length_notes: number;
  unterminated_notes: number;
  tracks: MidiTrackSummary[];
  warnings: string[];
}

/** 1-based channel; `null` keeps the source track's channels together. */
export type MidiTrackTarget = "NewTrack" | { ExistingTrack: number } | "Drop";

export interface MidiTrackMapping {
  source_track: number;
  channel: number | null;
  target: MidiTrackTarget;
  name: string | null;
}

export interface MidiImportOptions {
  split_channels: boolean;
  drop_empty: boolean;
  expand_sustain: boolean;
  import_clock: boolean;
  mappings: MidiTrackMapping[];
}

export type MidiImportStage = "parse" | "tracks" | "clock" | "notes" | "params" | "commit";

export interface MidiImportProgress {
  stage: MidiImportStage;
  fraction: number;
}

/** Typed backend failure: `kind` separates cancellation from an error. */
export interface MidiImportFailure {
  kind: "cancelled" | "failed";
  message?: string;
}

export function isMidiImportCancelled(error: unknown): boolean {
  return typeof error === "object" && error !== null && (error as { kind?: unknown }).kind === "cancelled";
}

export function midiImportErrorMessage(error: unknown): string {
  if (typeof error === "object" && error !== null) {
    const failure = error as MidiImportFailure;
    if (failure.kind === "cancelled") return "MIDI import cancelled";
    if (failure.kind === "failed") return failure.message ?? "MIDI import failed";
  }
  return String(error);
}

export interface MidiImportResult {
  summary: MidiImportSummary;
  snapshot: ProjectSnapshot;
}

export interface MidiImportJob {
  id: string;
  result: Promise<MidiImportResult>;
  cancel: () => Promise<void>;
}

export const DEFAULT_IMPORT_OPTIONS: MidiImportOptions = {
  split_channels: false,
  drop_empty: true,
  expand_sustain: true,
  import_clock: true,
  mappings: [],
};

export function previewMidiImport(path: string, options: MidiImportOptions): Promise<MidiImportSummary> {
  return invoke<MidiImportSummary>("midi_import_preview", { path, options });
}

/** Start an import and poll backend progress until the promise settles. */
export function startMidiImport(
  path: string,
  options: MidiImportOptions,
  onProgress: (progress: MidiImportProgress) => void,
): MidiImportJob {
  const jobId = `midi-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
  const timer = setInterval(() => {
    void invoke<MidiImportProgress | null>("midi_import_progress", { jobId })
      .then((progress) => { if (progress) onProgress(progress); })
      .catch(() => {});
  }, 120);
  const result = invoke<MidiImportResult>("midi_import_apply", {
    path, options, jobId,
  }).finally(() => clearInterval(timer));
  return {
    id: jobId,
    result,
    cancel: async () => {
      await invoke("midi_import_cancel", { jobId });
    },
  };
}

// ============================================================================
// Export
// ============================================================================

export type MidiExportScope =
  | "Project"
  | { Tracks: { start: number; end: number } }
  | { Track: number };

export interface MidiChannelAssignment {
  track: number;
  channel: number;
}

export interface MidiExportOptions {
  format: 0 | 1;
  ppq: 480 | 960;
  scope: MidiExportScope;
  channels: MidiChannelAssignment[];
}

export interface MidiExportTrackReport {
  track: number;
  name: string;
  channel: number;
  notes: number;
  widened_notes: number;
  automation_events: number;
  program: number | null;
  smf_track: number;
}

export interface MidiLossItem {
  kind: string;
  count: number;
  detail: string;
}

export interface MidiExportReport {
  format: number;
  ppq: number;
  written: MidiExportTrackReport[];
  losses: MidiLossItem[];
}

export function reportMidiExport(options: MidiExportOptions): Promise<MidiExportReport> {
  return invoke<MidiExportReport>("midi_export_report", { options });
}

export function writeMidiExport(options: MidiExportOptions, path: string): Promise<MidiExportReport> {
  return invoke<MidiExportReport>("midi_export_write", { options, path });
}
