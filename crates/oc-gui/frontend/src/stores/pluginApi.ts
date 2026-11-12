/**
 * Plugin discovery, installation, panels, preset state and host events.
 *
 * `api.ts` re-exports this module; shared gesture and project types are
 * imported as types from their owners.
 */
import { invoke } from "@tauri-apps/api/core";
import type { BarBeat, ParameterTargetStamp, ProjectSnapshot } from "../types/project";
import type { OwnedParameterGestureEnd, UndoGestureToken } from "./api";
import { startNativeAudio } from "./webAudioFocus";

export interface PluginInfo {
  id: string;
  name: string;
  num_params?: number;
  vendor: string;
  format: string;
  path: string;
  category: string;
  version: string | null;
  architectures: string[];
  state_abi: string | null;
}

export const scanPlugins = (rescan = false) =>
  invoke<PluginInfo[]>("scan_plugins", { rescan });

export interface SamplerAssetInfo {
  asset_id: string;
  entrypoint: string;
  source_url: string;
  license: string;
  attribution: string;
  file_count: number;
  manifest_path: string;
}

export const listSamplerAssets = () => invoke<SamplerAssetInfo[]>("list_sampler_assets");

export type PluginInstallSource = "direct" | "github_release" | "vendor_page";
export type PluginArchive = "zip" | "dmg" | "pkg" | "tar-gz" | "installer";
export type PluginFormat = "vst3" | "vst2" | "clap" | "au" | "lv2" | "standalone";

export interface SeparatorRuntimeDescriptor {
  provider: string;
  layout_version: number;
  launcher: string;
  required_files: { path: string; sha256: string }[];
}

export interface PluginInstall {
  runtime?: SeparatorRuntimeDescriptor;
  kind: PluginInstallSource;
  platform: string;
  format: PluginFormat;
  archive?: PluginArchive;
  url: string;
  sha256?: string;
  bytes?: number;
}

export interface PluginCatalogEntry {
  id: string;
  name: string;
  vendor: string;
  version?: string;
  description?: string;
  homepage?: string;
  license?: string;
  tags: string[];
  installs: PluginInstall[];
  aliases?: string[];
  known_issues?: string[];
  needs_admin?: boolean;
  /** Computed by the backend resolver for this host. */
  installable?: boolean;
  /** Whether a scanned bundle already satisfies the entry, per the backend. */
  installed?: boolean;
  install_mode?: "extract" | "installer";
  refusal?: PluginCatalogRefusal | null;
}

/** What a fresh walk of the plugin folders found for one catalog entry. */
export interface PluginInstalledCheck {
  id: string;
  name: string;
  installed: boolean;
  /** The bundle that satisfied the entry, when one did. */
  matched?: string | null;
  /** Bundles the walk saw, so an empty scan is visible. */
  scanned: number;
}

/** Why the backend cannot install an entry on this host. */
export interface PluginCatalogRefusal {
  code: string;
  message: string;
  url?: string | null;
}

export interface InstalledPluginInfo {
  name: string;
  format: string;
  path: string;
}

export type PluginCatalogSource = "bundled" | "cache" | "remote";

export interface PluginCatalog {
  platform: string;
  entries: PluginCatalogEntry[];
  installed: InstalledPluginInfo[];
  source?: PluginCatalogSource;
  generated_at?: string | null;
  fetched_at?: number | null;
  stale_days?: number | null;
  last_error?: string | null;
}

export interface PluginIndexStatus {
  trust?: "unconfigured" | "configured";
  source: PluginCatalogSource;
  generated_at?: string | null;
  fetched_at?: number | null;
  stale_days?: number | null;
  stale_warning: boolean;
  last_error?: string | null;
  catalog_version?: string | null;
  min_app_version?: string | null;
  next_refresh_in_seconds: number;
}

export interface PluginIndexRefreshOutcome {
  performed: boolean;
  changed: boolean;
  status: PluginIndexStatus;
}

export interface PluginInstallOutcome {
  status: "installed" | "needs_confirmation" | "opened_installer";
  path: string;
  message: string;
}

export interface PackLicense {
  name: string;
  url: string;
  text: string;
}

export interface SoundfontItem {
  id: string;
  name: string;
  version: string;
  description: string;
  bytes: number;
  license: PackLicense;
  installed: boolean;
  installed_version?: string | null;
  installed_bytes?: number | null;
  path?: string | null;
  /** `index` when the signed catalog offers it, `local` when only the disk has it. */
  source: "index" | "local";
}

export interface SoundfontListing {
  dir: string;
  items: SoundfontItem[];
}

export interface InstalledSoundfont {
  id: string;
  name: string;
  version: string;
  bytes: number;
  sha256: string;
  license: PackLicense;
  path: string;
  installed_at: number;
}

export interface SoundfontProgress {
  id: string;
  received: number;
  total: number;
}

export const soundfontsList = () => invoke<SoundfontListing>("soundfonts_list");
export const soundfontInstall = (id: string) =>
  invoke<InstalledSoundfont>("soundfont_install", { id });
export const soundfontsRemove = (id: string) =>
  invoke<InstalledSoundfont>("soundfonts_remove", { id });

export const pluginCatalog = () => invoke<PluginCatalog>("plugin_catalog");
export const pluginStoreOpen = (url: string) => invoke<void>("plugin_store_open", { url });
export const pluginStoreInstall = (id: string, overwrite = false) =>
  invoke<PluginInstallOutcome>("plugin_store_install", { id, overwrite });
/** Open the vendor page a page-only entry links; returns the address opened. */
export const pluginManualPage = (id: string) => invoke<string>("plugin_manual_page", { id });
/** Re-scan the plugin folders for one entry; downloads nothing. */
export const pluginInstalledCheck = (id: string) =>
  invoke<PluginInstalledCheck>("plugin_installed_check", { id });
export const pluginIndexStatus = () => invoke<PluginIndexStatus>("plugin_index_status");
export const pluginIndexRefresh = (force = false) =>
  invoke<PluginIndexRefreshOutcome>("plugin_index_refresh", { force });

export interface CompatibilityIssue {
  kind: string;
  subject: string;
  message: string;
}

export interface ProjectCompatibilityReport {
  compatible: boolean;
  installed_plugin_count: number;
  resolved_plugins: {
    required_id: string;
    installed_id: string;
    name: string;
    format: string;
    path: string;
    version: string | null;
    architectures: string[];
    state_abi: string | null;
  }[];
  issues: CompatibilityIssue[];
}

export const projectCompatibilityReport = () =>
  invoke<ProjectCompatibilityReport>("project_compatibility_report");

export interface PluginParamInfo {
  id: number;
  name: string;
  module: string;
  min: number;
  max: number;
  default: number;
  current: number;
  display: string;
  flags: number;
}

/** One parameter of the generated panel, in the plugin's own units. */
export interface PluginPanelParam {
  index: number;
  name: string;
  /** Unit label the plugin reports; empty when it reports none. */
  unit: string;
  min: number;
  max: number;
  current: number;
  /** The plugin's own formatted value at read time; empty when it has none. */
  display: string;
  /** `current` on the automation lane's 0..1 domain. */
  normalized: number;
  /** The plugin's own default; null when the format reports none. */
  default: number | null;
  /** Step count for a switch/integer/enum control. */
  steps: number | null;
  automatable: boolean;
  /** A bypass switch that is currently on. */
  by_passing: boolean;
  group: string | null;
}

/** Why a preset has no generated panel; never an empty parameter list. */
export type PluginPanelIssue =
  | { kind: "format_has_no_parameters"; format: string }
  | { kind: "editor_format"; format: string }
  | { kind: "plugin_reports_no_parameters" }
  | { kind: "query_failed"; message: string };

export type PluginPanelSurface =
  | { kind: "parameters"; parameters: PluginPanelParam[] }
  | { kind: "unavailable"; issue: PluginPanelIssue };

export interface PluginParameterPanel {
  preset: string;
  format: string;
  surface: PluginPanelSurface;
}

export const pluginParameterPanel = (presetName: string) =>
  invoke<PluginParameterPanel>("plugin_parameter_panel", { presetName });

export const loadPluginForPreset = (presetName: string) =>
  invoke<PluginParamInfo[]>("load_plugin_for_preset", { presetName });

export const getPluginParams = (presetName: string) =>
  invoke<PluginParamInfo[]>("get_plugin_params", { presetName });

export interface FfpParamMismatch {
  name: string;
  expected_plain: number;
  readback_plain: number | null;
}

export interface FfpImportReport {
  signature: string;
  version: number;
  format: string;
  preset_name: string | null;
  parsed: number;
  mapped: number;
  applied: number;
  unmapped: string[];
  rejected: string[];
  mismatched: FfpParamMismatch[];
}

export const importFfpPreset = (presetName: string, path: string) =>
  invoke<[ProjectSnapshot, FfpImportReport]>("import_ffp_preset", { presetName, path });

export const beginPluginParamGesture = (
  presetName: string,
  trackIndex: number,
  paramName: string,
) => invoke<void>("begin_plugin_param_gesture", { presetName, trackIndex, paramName });

export const updatePluginParamGesture = (
  presetName: string,
  paramName: string,
  value: number,
  position: { bar: number; beat: number; subdivision: number },
) => invoke<ProjectSnapshot>("update_plugin_param_gesture", {
  presetName,
  paramName,
  value,
  position,
});

export const endPluginParamGesture = (presetName: string, paramName: string) =>
  invoke<ProjectSnapshot>("end_plugin_param_gesture", { presetName, paramName });

export const beginGuardedPluginParamGesture = (presetName: string, trackIndex: number, paramName: string, expectedStamp: ParameterTargetStamp) =>
  invoke<UndoGestureToken>("begin_guarded_plugin_param_gesture", { presetName, trackIndex, paramName, expectedStamp });
export const beginOwnedPluginParamGesture = (presetName: string, trackIndex: number, paramName: string) =>
  invoke<UndoGestureToken>("begin_owned_plugin_param_gesture", { presetName, trackIndex, paramName });

export const updateOwnedPluginParamGesture = (token: UndoGestureToken, presetName: string, paramName: string, value: number, position: BarBeat) =>
  invoke<ProjectSnapshot>("update_owned_plugin_param_gesture", { presetName, paramName, value, position, token });

export const endOwnedPluginParamGesture = (token: UndoGestureToken, presetName: string, paramName: string) =>
  invoke<OwnedParameterGestureEnd>("end_owned_plugin_param_gesture", { presetName, paramName, token });

export const savePluginStateFile = (presetName: string, path: string) =>
  invoke<ProjectSnapshot>("save_plugin_state_file", { presetName, path });

export const savePluginStateEmbedded = (presetName: string) =>
  invoke<ProjectSnapshot>("save_plugin_state_embedded", { presetName });

export const openPluginGui = (presetName: string) =>
  startNativeAudio(() => invoke<[number, number]>("open_plugin_gui", { presetName }));

export const closePluginGui = (presetName: string) =>
  invoke<void>("close_plugin_gui", { presetName });

/// Null when the native editor can be hosted; a reason otherwise.
export const pluginEditorStatus = (presetName: string) =>
  invoke<string | null>("plugin_editor_status", { presetName });

export const pluginHasGui = (presetName: string) =>
  invoke<boolean>("plugin_has_gui", { presetName });

export const pollPluginGuiClosed = (presetName: string) =>
  invoke<boolean>("poll_plugin_gui_closed", { presetName });

/// Host-level notifications a loaded plugin reported since the last poll.
///
/// `params_changed` says a value or the parameter inventory moved, which is the
/// only reason to re-read the parameter list.
export interface PluginHostEvents {
  events: string[];
  dirty: boolean;
  open_editor: string | null;
  params_changed: boolean;
}

export const pollPluginHostEvents = (presetName: string) =>
  invoke<PluginHostEvents>("poll_plugin_host_events", { presetName });

/** Select a plugin bundle or sample file in the platform's file manager. */
export const revealPath = (path: string) => invoke<void>("reveal_path", { path });

/** Ask a running pack or plugin install to stop at its next checkpoint. */
export const cancelInstall = (id: string) => invoke<void>("install_cancel", { id });

/** Create the live instance a probe skipped, before the first edit. */
export const ensurePluginInstance = (presetName: string) =>
  invoke<void>("ensure_plugin_instance", { presetName });
