/**
 * Diagnostics, build identity, render performance and storage reports.
 *
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";

// ============================================================================
// Diagnostics
// ============================================================================

export interface DiagnosticLogLine {
  time: number;
  level: string;
  source: string;
  message: string;
}

export const exportDiagnostics = (dest: string, logs: DiagnosticLogLine[]) =>
  invoke<string>("export_diagnostics", { dest, logs });

export const revealDiagnostics = (path: string) =>
  invoke<void>("reveal_diagnostics", { path });

// ============================================================================
// Build identity, render performance, storage
// ============================================================================

/** The version this binary reports and the DSL version its parser accepts. */
export interface AppInfo {
  version: string;
  dsl_version: string;
  source_revision: string;
  build_profile: string;
  os: string;
  arch: string;
}

export const appInfo = () => invoke<AppInfo>("app_info");

/** One start path's press-to-first-block numbers. */
export interface FirstBlockSnapshot {
  path: string;
  seq: number;
  delivered: boolean;
  latency_ms: number | null;
  since_start_ms: number;
  block_frames: number;
  frames_computed: number;
  blocks_computed: number;
  queued_frames: number;
  splice_ms: number | null;
}

/** The render-performance setting, persisted in the private GUI settings file. */
export interface PerformanceSettings {
  /** `null` follows the machine's parallelism. */
  render_threads: number | null;
  /** Off renders every track on one worker. */
  parallel_tracks: boolean;
}

export interface PerformanceFacts {
  app_version: string;
  dsl_version: string;
  cores: number;
  render_threads: number | null;
  parallel_tracks: boolean;
  effective_workers: number;
  lookahead_frames: number;
  first_chunk_frames: number;
  block_frames: number;
  sample_rate: number;
  first_block_budget_ms: number;
  first_block: FirstBlockSnapshot | null;
}

export const performanceFacts = () => invoke<PerformanceFacts>("performance_facts");

export const setPerformanceSettings = (settings: PerformanceSettings) =>
  invoke<PerformanceFacts>("set_performance_settings", { settings });

export interface SoundfontStorage {
  dir: string;
  installed: number;
  files: number;
  bytes: number;
}

export interface FormatStorage {
  format: string;
  count: number;
  bytes: number;
}

export interface PluginStorage {
  by_format: FormatStorage[];
  folders: PluginFolderStorage[];
  bytes: number;
}

export interface SampleStorage {
  dir: string | null;
  files: number;
  bytes: number;
}

export interface SessionEntryStorage {
  id: string;
  title: string;
  bytes: number;
  updated: string;
}

export interface WorkspaceStorage {
  path: string;
  key: string;
  sessions: SessionEntryStorage[];
  bytes: number;
}

export interface AuditionDirStorage {
  dir: string;
  files: number;
  bytes: number;
}

/** The audition archive: delivery clips and the full-rate renders beside them. */
export interface AuditionStorage {
  media: AuditionDirStorage;
  renders: AuditionDirStorage;
  /** The cap both directories share. */
  budget_bytes: number;
}

export interface SessionStorage {
  root: string;
  workspaces: WorkspaceStorage[];
  audio_bytes: number;
  auditions: AuditionStorage;
  total_bytes: number;
}

export interface PluginFolderStorage {
  path: string;
  count: number;
  bytes: number;
}

export interface ClipCacheUsage {
  dir: string | null;
  cached_bytes: number;
  open_cached_bytes: number;
  live_temporary_bytes: number;
  temporary_bytes?: number;
  budget_bytes: number;
}

export interface ClipPcmStorage {
  decoded: ClipCacheUsage;
  processed: ClipCacheUsage;
}

export interface StorageReport {
  soundfonts: SoundfontStorage;
  plugins: PluginStorage;
  samples: SampleStorage;
  clip_pcm?: ClipPcmStorage;
  sessions: SessionStorage;
  preferences: string;
}

export const storageReport = () => invoke<StorageReport>("storage_report");

export const clearClipPcmCache = () => invoke<ClipPcmStorage>("clear_clip_pcm_cache");

export const clearSampleCache = () => invoke<StorageReport>("clear_sample_cache");

export const clearAuditionArchive = () =>
  invoke<StorageReport>("clear_audition_archive");

export const openStorageFolder = (path: string) =>
  invoke<void>("open_storage_folder", { path });

export const openUrl = (url: string) => invoke<void>("open_url", { url });
