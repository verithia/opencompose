/**
 * Plugin Store catalog state and pure matching/selection helpers.
 *
 * The catalog comes from the signed index cache or the bundled fallback; the
 * UI only exposes a rate-limited refresh action.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import type {
  InstalledPluginInfo,
  PluginCatalog,
  PluginCatalogEntry,
  PluginIndexStatus,
  PluginInstall,
} from "./api";

export const PLUGIN_CATEGORIES = [
  "synth",
  "sampler",
  "eq",
  "compressor",
  "reverb",
  "delay",
  "saturation",
  "metering",
  "drums",
  "vocal",
  "utility",
] as const;

export type PluginInstallKind =
  | "separator-runtime"
  | "page"
  | "zip-vst3"
  | "zip-vst2"
  | "zip-clap"
  | "zip-lv2"
  | "appimage"
  | "pkg"
  | "dmg"
  | "msi"
  | "exe";

const [catalog, setCatalog] = createSignal<PluginCatalog | null>(null);
const [loading, setLoading] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);
const [status, setStatus] = createSignal<PluginIndexStatus | null>(null);
const [refreshing, setRefreshing] = createSignal(false);
const [refreshError, setRefreshError] = createSignal<string | null>(null);

async function load(force = false) {
  if (loading()) return;
  if (!force && catalog()) {
    void loadStatus();
    return;
  }
  setLoading(true);
  setError(null);
  try {
    const [next, nextStatus] = await Promise.all([
      api.pluginCatalog(),
      api.pluginIndexStatus().catch(() => null),
    ]);
    setCatalog(next);
    if (nextStatus) setStatus(nextStatus);
  } catch (cause) {
    setError(String(cause));
  } finally {
    setLoading(false);
  }
}

async function loadStatus() {
  try {
    setStatus(await api.pluginIndexStatus());
  } catch (_) {
    // Status is advisory; a failed status call must not hide the catalog.
  }
}

/** Tauri serializes index errors as `{stage, message}`; keep the stage visible. */
function formatError(cause: unknown): string {
  if (cause && typeof cause === "object" && "stage" in cause && "message" in cause) {
    const { stage, message } = cause as { stage: unknown; message: unknown };
    return `${String(stage)}: ${String(message)}`;
  }
  return String(cause);
}

/** Manual refresh; the backend enforces the 5-minute floor. */
async function refresh(force = true) {
  if (refreshing()) return null;
  setRefreshing(true);
  setRefreshError(null);
  try {
    const outcome = await api.pluginIndexRefresh(force);
    // A backend that answers nothing leaves the painted catalog as it is.
    if (!outcome) return null;
    setStatus(outcome.status);
    if (outcome.changed) setCatalog(await api.pluginCatalog());
    return outcome;
  } catch (cause) {
    setRefreshError(formatError(cause));
    await loadStatus();
    return null;
  } finally {
    setRefreshing(false);
  }
}

/**
 * The browser opened: paint the cached catalog, then check the index once.
 *
 * This is the only automatic trigger. Nothing fetches before the browser is
 * opened, and a check inside the freshness interval answers from the cache
 * without touching the network. A failure is reported in the panel, not logged.
 */
async function open() {
  await load();
  await refresh(false);
}

function reset() {
  setCatalog(null);
  setError(null);
  setLoading(false);
  setStatus(null);
  setRefreshing(false);
  setRefreshError(null);
}

function openPage(entry: PluginCatalogEntry) {
  if (!entry.homepage) throw new Error("Plugin has no homepage");
  return api.pluginStoreOpen(entry.homepage);
}

function openDownload(url: string) {
  return api.pluginStoreOpen(url);
}

/** Open the vendor's own page for a page-only entry. */
function openManualPage(entry: PluginCatalogEntry) {
  return api.pluginManualPage(entry.id);
}

/** Re-scan the plugin folders and report whether the entry landed. */
function checkInstalled(id: string) {
  return api.pluginInstalledCheck(id);
}

function install(entry: PluginCatalogEntry, overwrite = false) {
  return api.pluginStoreInstall(entry.id, overwrite);
}

export const pluginStore = {
  catalog,
  loading,
  error,
  status,
  refreshing,
  refreshError,
  load,
  loadStatus,
  open,
  refresh,
  reset,
  openPage,
  openDownload,
  openManualPage,
  checkInstalled,
  install,
};

/** Normalize vendor names and bundle names for installed detection. */
export function normalizePluginName(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, "");
}

/** Match by bundle name plus aliases; hand-installed bundles still count. */
export function isEntryInstalled(
  entry: PluginCatalogEntry,
  installed: InstalledPluginInfo[],
): boolean {
  // The backend walks the same folders with the same matcher, so its answer
  // wins; the name comparison is only a fallback for an older response.
  if (entry.installed !== undefined) return entry.installed;
  const installedNames = new Set(installed.map((plugin) => normalizePluginName(plugin.name)));
  const candidates = [entry.name, ...(entry.aliases ?? [])];
  return candidates.some((name) => installedNames.has(normalizePluginName(name)));
}

export function entrySupportsPlatform(entry: PluginCatalogEntry, platform: string): boolean {
  return entry.installs.some((install) => install.platform === platform);
}

export function entryCategory(entry: PluginCatalogEntry): string {
  return (
    entry.tags.find((tag) => (PLUGIN_CATEGORIES as readonly string[]).includes(tag)) ?? "utility"
  );
}

export function entryLicense(entry: PluginCatalogEntry): string {
  if (entry.license) return entry.license;
  const tag = entry.tags.find((value) =>
    ["freeware", "freemium", "open-source"].includes(value),
  );
  if (tag === "freeware") return "free";
  return tag ?? "free";
}

export function entryFormats(entry: PluginCatalogEntry, platform: string): string[] {
  const formats = new Set<string>();
  for (const install of entry.installs) {
    if (install.platform === platform) formats.add(install.format);
  }
  return [...formats];
}

function installRank(install: PluginInstall): number {
  const sourceRank = install.kind === "vendor_page" ? 10 : 0;
  const formatRank = { vst3: 0, clap: 1, vst2: 2, lv2: 3, au: 4, standalone: 5 }[
    install.format
  ] ?? 6;
  return sourceRank + formatRank;
}

/** Best install for this host: direct build first, then preferred format. */
export function hostInstall(
  entry: PluginCatalogEntry,
  platform: string,
): PluginInstall | undefined {
  return entry.installs
    .filter((install) => install.platform === platform)
    .sort((left, right) => installRank(left) - installRank(right))[0];
}

function inferArchive(url: string): PluginInstall["archive"] {
  const lower = url.toLowerCase();
  if (lower.includes(".tar.gz")) return "tar-gz";
  if (lower.endsWith(".zip")) return "zip";
  if (lower.endsWith(".dmg")) return "dmg";
  if (lower.endsWith(".pkg")) return "pkg";
  if (lower.endsWith(".exe") || lower.endsWith(".msi") || lower.endsWith(".appimage")) {
    return "installer";
  }
  return undefined;
}

export function installKind(install: PluginInstall): PluginInstallKind | undefined {
  if (install.kind === "vendor_page") return "page";
  if (install.runtime && install.format === "standalone") return "separator-runtime";
  const archive = install.archive ?? inferArchive(install.url);
  if (archive === "zip") {
    if (install.format === "vst3") return "zip-vst3";
    if (install.format === "vst2") return "zip-vst2";
    if (install.format === "clap") return "zip-clap";
    if (install.format === "lv2") return "zip-lv2";
    return undefined;
  }

  if (archive === "tar-gz") {
    if (install.format === "vst3") return "zip-vst3";
    if (install.format === "vst2") return "zip-vst2";
    if (install.format === "clap") return "zip-clap";
    if (install.format === "lv2") return "zip-lv2";
    return undefined;
  }
  if (archive === "dmg") return "dmg";
  if (archive === "pkg") return "pkg";
  if (archive === "installer") {
    const lower = install.url.toLowerCase();
    if (lower.endsWith(".msi")) return "msi";
    if (lower.endsWith(".appimage")) return "appimage";
    return "exe";
  }
  return undefined;
}

export function entryInstallKind(
  entry: PluginCatalogEntry,
  platform: string,
): PluginInstallKind | undefined {
  const install = hostInstall(entry, platform);
  return install ? installKind(install) : undefined;
}

export function entryDownloadUrl(
  entry: PluginCatalogEntry,
  platform: string,
): string | undefined {
  const install = hostInstall(entry, platform);
  if (!install || install.kind === "vendor_page") return undefined;
  return install.url;
}

/** The vendor's own download page, for an entry with no installable build. */
export function entryVendorPage(
  entry: PluginCatalogEntry,
  platform: string,
): string | undefined {
  const install = hostInstall(entry, platform);
  if (!install || install.kind !== "vendor_page") return undefined;
  return install.url;
}

/** True when the only way in is the vendor's site. */
export function entryPageOnly(entry: PluginCatalogEntry, platform: string): boolean {
  return entryVendorPage(entry, platform) !== undefined;
}

export function entryInstallable(entry: PluginCatalogEntry, platform: string): boolean {
  // The backend resolver answers for this host; the local kind heuristic is
  // only a fallback for an older catalog response.
  if (entry.installable !== undefined) return entry.installable;
  const kind = entryInstallKind(entry, platform);
  return kind !== undefined && kind !== "page";
}

/** Why the Store cannot install this entry, or undefined when it can. */
export function entryRefusal(entry: PluginCatalogEntry): string | undefined {
  return entry.refusal?.message;
}

export function filterCatalog(
  entries: PluginCatalogEntry[],
  platform: string,
  query: string,
  category: string,
): PluginCatalogEntry[] {
  const needle = query.trim().toLowerCase();
  return entries.filter((entry) => {
    if (!entrySupportsPlatform(entry, platform)) return false;
    if (category !== "all" && entryCategory(entry) !== category) return false;
    if (!needle) return true;
    const haystack = [
      entry.name,
      entry.vendor,
      ...entry.tags,
      ...(entry.aliases ?? []),
      ...entryFormats(entry, platform),
    ]
      .join(" ")
      .toLowerCase();
    return haystack.includes(needle);
  });
}

/** Installers that should always be presented with an admin warning. */
export function installNeedsAdmin(entry: PluginCatalogEntry, platform: string): boolean {
  if (entry.needs_admin) return true;
  const kind = entryInstallKind(entry, platform);
  return kind === "pkg" || kind === "msi";
}
