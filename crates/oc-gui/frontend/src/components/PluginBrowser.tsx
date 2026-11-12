import { createSignal, createMemo, onCleanup, onMount, For, Show, type JSX } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { store } from "../stores/project";
import * as api from "../stores/api";
import type { PluginInfo, SoundfontItem } from "../stores/api";
import { i18n } from "../stores/i18n";
import { useTooltip } from "./Tooltip";
import { openContextMenu } from "./ContextMenu";
import { uniqueName } from "../utils/naming";
import { pluginFormatLabel } from "../devices/labels";
import { KeyboardIcon, PlugIcon, SliderIcon, WaveIcon } from "./icons";

interface RowMessage {
  text: string;
  error: boolean;
}

/** One decimal, so a 49 MB pack does not read as "50". */
function megabytes(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1);
}

/** Grey rows that stand in for the inventory until the scan answers. */
const SKELETON_ROWS = [0, 1, 2, 3, 4, 5];

export function PluginBrowser() {
  const [plugins, setPlugins] = createSignal<PluginInfo[]>([]);
  const [scanning, setScanning] = createSignal(false);
  const [filter, setFilter] = createSignal("");
  const [formatFilter, setFormatFilter] = createSignal<string>("all");
  const [categoryFilter, setCategoryFilter] = createSignal<string>("all");
  const [compatibility, setCompatibility] = createSignal<api.ProjectCompatibilityReport | null>(null);
  const [samplerAssets, setSamplerAssets] = createSignal<api.SamplerAssetInfo[]>([]);
  const [soundfonts, setSoundfonts] = createSignal<api.SoundfontListing | null>(null);
  const [soundfontError, setSoundfontError] = createSignal<string | null>(null);
  const [soundfontBusy, setSoundfontBusy] = createSignal<string | null>(null);
  const [soundfontProgress, setSoundfontProgress] = createSignal<api.SoundfontProgress | null>(null);
  const [soundfontMessages, setSoundfontMessages] = createSignal<Record<string, RowMessage>>({});
  const [openLicense, setOpenLicense] = createSignal<string | null>(null);
  const [scanError, setScanError] = createSignal<string | null>(null);
  const [addingTrack, setAddingTrack] = createSignal(false);
  const [addingEffect, setAddingEffect] = createSignal<string | null>(null);
  const [effectError, setEffectError] = createSignal<string | null>(null);

  // Auto-load on first mount. Every call below is asynchronous, so the tab
  // paints its skeleton before any inventory, compatibility, or pack lookup
  // answers.
  onMount(() => {
    if (plugins().length === 0) void handleScan();
    refreshCompatibility();
    api.listSamplerAssets().then(setSamplerAssets).catch((e) => console.error("Sampler asset scan failed:", e));
    void refreshSoundfonts();
    // A listener that answers after unmount is unlistened at once instead of
    // being stored where no cleanup will ever reach it.
    let disposed = false;
    const stops: (() => void)[] = [];
    const keep = (unlisten: () => void) => {
      if (disposed) unlisten();
      else stops.push(unlisten);
    };
    void listen<api.SoundfontProgress>("soundfont_progress", (event) => {
      setSoundfontProgress(event.payload);
    }).then(keep);
    // A background refresh replaces the inventory after this tab painted; show
    // the new list without a spinning button.
    void listen<number>("plugin_inventory_updated", () => {
      void handleScan(false, true);
      refreshCompatibility();
    }).then(keep);
    onCleanup(() => {
      disposed = true;
      for (const stop of stops.splice(0)) stop();
    });
  });

  const refreshSoundfonts = async () => {
    try {
      setSoundfonts(await api.soundfontsList());
      setSoundfontError(null);
    } catch (cause) {
      setSoundfontError(String(cause));
    }
  };

  const setSoundfontMessage = (id: string, text: string, error = false) => {
    setSoundfontMessages((previous) => ({ ...previous, [id]: { text, error } }));
  };

  const installSoundfont = async (item: SoundfontItem) => {
    setSoundfontBusy(item.id);
    setSoundfontProgress(null);
    setSoundfontMessage(item.id, "");
    try {
      await api.soundfontInstall(item.id);
      await refreshSoundfonts();
    } catch (cause) {
      setSoundfontMessage(
        item.id,
        i18n.t("soundfont.installFailed", { error: String(cause) }),
        true,
      );
    } finally {
      setSoundfontBusy(null);
      setSoundfontProgress(null);
    }
  };

  /** Stop a running pack download where it stands. */
  const cancelSoundfont = async (id: string) => {
    try {
      await api.cancelInstall(id);
    } catch (error) {
      setSoundfontMessages((current) => ({
        ...current,
        [id]: { text: String(error), error: true },
      }));
    }
  };

  const removeSoundfont = async (item: SoundfontItem) => {
    setSoundfontBusy(item.id);
    setSoundfontMessage(item.id, "");
    try {
      await api.soundfontsRemove(item.id);
      await refreshSoundfonts();
    } catch (cause) {
      setSoundfontMessage(
        item.id,
        i18n.t("soundfont.removeFailed", { error: String(cause) }),
        true,
      );
    } finally {
      setSoundfontBusy(null);
    }
  };

  /** `12.3 / 49.0 MB` while the total is known, `12.3 MB` when it is not. */
  const progressText = (progress: api.SoundfontProgress): string =>
    progress.total > 0
      ? i18n.t("soundfont.downloading", {
          received: megabytes(progress.received),
          total: megabytes(progress.total),
        })
      : i18n.t("soundfont.downloadingBytes", { received: megabytes(progress.received) });

  const refreshCompatibility = async () => {
    try {
      setCompatibility(await api.projectCompatibilityReport());
    } catch (e) {
      console.error("Project compatibility audit failed:", e);
    }
  };

  const filtered = createMemo(() => {
    const q = filter().toLowerCase();
    const fmt = formatFilter();
    const cat = categoryFilter();
    return plugins().filter((p) => {
      if (fmt !== "all" && p.format !== fmt) return false;
      if (cat !== "all" && p.category !== cat) return false;
      if (q && !p.name.toLowerCase().includes(q) && !p.vendor.toLowerCase().includes(q)) return false;
      return true;
    });
  });

  /** `rescan` walks every bundle; `quiet` hides the spinner for a background
   *  refresh that is already reported as scanning by the backend. */
  let scanInFlight = false;
  const handleScan = async (rescan = false, quiet = false) => {
    if (scanInFlight) return;
    scanInFlight = true;
    if (!quiet) setScanning(true);
    setScanError(null);
    try {
      const result = await api.scanPlugins(rescan);
      setPlugins(result);
    } catch (e) {
      // A silent failure leaves an empty list looking like "no plug-ins".
      setScanError(String(e));
    } finally {
      scanInFlight = false;
      if (!quiet) setScanning(false);
    }
  };

  const categoryIcon = (cat: string): JSX.Element => {
    switch (cat) {
      case "Instrument": return <KeyboardIcon />;
      case "Effect": return <SliderIcon />;
      case "Analyzer": return <WaveIcon />;
      default: return <PlugIcon />;
    }
  };

  /** Copy a bundle path to the clipboard; the row's other identity is the id. */
  const copyPath = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
    } catch (error) {
      setScanError(String(error));
    }
  };

  const revealPluginPath = async (path: string) => {
    try {
      await api.revealPath(path);
    } catch (error) {
      setScanError(String(error));
    }
  };

  /** Add a track using this plugin as its instrument */
  const handleAddTrack = async (plugin: PluginInfo) => {
    if (plugin.category !== "Instrument" || addingTrack()) return;
    const source = ({ Vst3: "vst3", Vst2: "vst2", Clap: "clap", Au: "au", Lv2: "lv2", Internal: "builtin" } as Record<string, string>)[plugin.format];
    if (!source) return;
    setAddingTrack(true);
    try {
      const name = uniqueName(plugin.name, store.project.presets.map((preset) => preset.name));
      const trackName = uniqueName(plugin.name, store.project.tracks.map((track) => track.name));
      // One edit, one graph rebuild: the preset, its track and the assignment
      // land together, so the instrument reloads once instead of three times.
      await store.addInstrumentTrack(name, source, plugin.id, null, trackName, "keys", "piano");
    } finally { setAddingTrack(false); }
  };

  /** Insert this effect into the selected track's FX chain. */
  const handleAddEffect = async (plugin: PluginInfo) => {
    const index = store.selectedTrackIndex();
    if (plugin.category !== "Effect" || index === null || addingEffect()) return;
    setAddingEffect(plugin.id);
    setEffectError(null);
    try {
      const error = await store.addEffect(`track:${index}`, `plugin:${plugin.id}`);
      if (error) setEffectError(i18n.t("browse.effects.failed", { name: plugin.name, error }));
    } finally { setAddingEffect(null); }
  };

  const addSamplerPreset = async (asset: api.SamplerAssetInfo) => {
    const stableName = asset.asset_id.replace(/[^a-zA-Z0-9_]+/g, "_");
    await store.addPreset(stableName, "sample", {
      plugin: "sfizz",
      file: asset.entrypoint,
    });
  };

  return (
    <div class="plugin-browser">
      <div class="plugin-browser-header">
        <button
          class="toolbar-btn"
          classList={{ active: scanning() }}
          disabled={scanning()}
          onClick={() => handleScan(true)}
          style={{ width: "auto", padding: "0 8px", "font-size": "var(--font-size-sm)" }}
        >
          {scanning() ? i18n.t("plugin.scanningShort") : i18n.t("plugin.rescan")}
        </button>
        <span style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)" }}>
          {i18n.t("plugin.count", { count: plugins().length })}
        </span>
        <button class="toolbar-btn" aria-label={i18n.t("plugin.refreshReport")} onClick={refreshCompatibility}>{i18n.t("plugin.checkProject")}</button>
      </div>

      <Show when={scanError()}>
        <div class="plugin-scan-error selectable" role="alert">
          {i18n.t("plugin.scanFailed", { error: scanError() ?? "" })}
        </div>
      </Show>

      <Show when={effectError()}>
        <div class="plugin-scan-error selectable" role="alert">{effectError()}</div>
      </Show>

      <Show when={compatibility()}>
        {(report) => (
          <div class="project-compatibility selectable" style={{ padding: "8px", "border-bottom": "1px solid var(--border)" }}>
            <div style={{ color: report().compatible ? "var(--success, #65b96e)" : "var(--warning, #e2a842)", "font-size": "var(--font-size-sm)" }}>
              {report().compatible
                ? i18n.t("plugin.compatibilityOk")
                : i18n.t("plugin.compatibilityIssues", { count: report().issues.length })}
            </div>
            <For each={report().issues}>
              {(issue) => <div class="compatibility-issue" style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)", "margin-top": "3px" }}><strong>{issue.subject}:</strong> {issue.message}</div>}
            </For>
            <Show when={report().resolved_plugins.length > 0}>
              <details style={{ "margin-top": "5px", "font-size": "var(--font-size-xs)" }}>
                <summary>{i18n.t("plugin.resolvedExternal", { count: report().resolved_plugins.length })}</summary>
                <For each={report().resolved_plugins}>
                  {(plugin) => <div {...useTooltip(() => plugin.path)} style={{ color: "var(--text-muted)", "margin-top": "2px" }}>{plugin.name} · {plugin.format}</div>}
                </For>
              </details>
            </Show>
          </div>
        )}
      </Show>

      <div class="plugin-browser-filters">
        <input
          type="text"
          placeholder={i18n.t("plugin.searchPlaceholder")}
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
          class="plugin-search-input"
        />
        <select value={formatFilter()} onChange={(e) => setFormatFilter(e.currentTarget.value)} class="plugin-format-select">
          <option value="all">{i18n.t("midi.import.allChannels")}</option>
          <option value="Internal">{i18n.t("plugin.filterBuiltIn")}</option>
          <option value="Clap">CLAP</option>
          <option value="Vst3">VST3</option>
          <option value="Vst2">VST2</option>
          <option value="Au">AU</option>
          <option value="Lv2">LV2</option>
        </select>
        <select value={categoryFilter()} onChange={(e) => setCategoryFilter(e.currentTarget.value)} class="plugin-format-select">
          <option value="all">{i18n.t("plugin.filterAllTypes")}</option>
          <option value="Instrument">{i18n.t("soundfont.group")}</option>
          <option value="Effect">{i18n.t("plugin.filterEffects")}</option>
        </select>
      </div>

      <div class="plugin-list">
        <Show when={(soundfonts()?.items?.length ?? 0) > 0}>
          <div class="soundfont-group">
            <span>{i18n.t("soundfont.group")}</span>
            <span class="soundfont-group-count">
              {i18n.t("soundfont.groupCount", {
                count: soundfonts()?.items?.length ?? 0,
              })}
            </span>
          </div>
          <For each={soundfonts()?.items ?? []}>
            {(item) => {
              const message = () => soundfontMessages()[item.id];
              const busy = () => soundfontBusy() === item.id;
              const progress = () =>
                busy() && soundfontProgress()?.id === item.id ? soundfontProgress() : null;
              return (
                <>
                  <div class="soundfont-row" data-soundfont-id={item.id}>
                    <span class="soundfont-row-name selectable" {...useTooltip(() => item.description)}>
                      {item.name}
                    </span>
                    <span class="soundfont-row-size">{megabytes(item.bytes)} MB</span>
                    <button
                      class="soundfont-license"
                      classList={{ open: openLicense() === item.id }}
                      aria-expanded={openLicense() === item.id}
                      {...useTooltip(() => i18n.t("soundfont.licenseHint"))}
                      onClick={() =>
                        setOpenLicense((current) => (current === item.id ? null : item.id))
                      }
                    >
                      {item.license.name}
                    </button>
                    <Show
                      when={busy()}
                      fallback={
                        <Show
                          when={item.installed}
                          fallback={
                            <button
                              class="soundfont-action"
                              disabled={soundfontBusy() !== null}
                              aria-label={i18n.t("soundfont.install")}
                              onClick={() => void installSoundfont(item)}
                            >
                              {i18n.t("soundfont.install")}
                            </button>
                          }
                        >
                          <span class="soundfont-badge">{i18n.t("soundfont.installed")}</span>
                          <button
                            class="soundfont-action"
                            disabled={soundfontBusy() !== null}
                            aria-label={i18n.t("soundfont.remove")}
                            onClick={() => void removeSoundfont(item)}
                          >
                            {i18n.t("soundfont.remove")}
                          </button>
                        </Show>
                      }
                    >
                      <span class="soundfont-row-progress">{i18n.t("soundfont.installing")}</span>
                      <button
                        class="soundfont-action"
                        aria-label={i18n.t("store.installCancel", { name: item.name })}
                        onClick={() => void cancelSoundfont(item.id)}
                      >{i18n.t("store.installCancelShort")}</button>
                    </Show>
                  </div>
                  <Show when={progress()}>
                    {(current) => (
                      <div class="soundfont-row-progress-line" role="status">
                        {progressText(current())}
                      </div>
                    )}
                  </Show>
                  <Show when={openLicense() === item.id}>
                    <div class="soundfont-license-text selectable">
                      {item.license.text}
                    </div>
                  </Show>
                  <Show when={message()?.text}>
                    <div
                      class="soundfont-row-message selectable"
                      classList={{ error: message()?.error }}
                      role={message()?.error ? "alert" : "status"}
                    >
                      {message()?.text}
                    </div>
                  </Show>
                </>
              );
            }}
          </For>
        </Show>

        <Show when={soundfontError()}>
          <div class="soundfont-row-message error selectable" role="alert">
            {i18n.t("soundfont.listFailed", { error: soundfontError() ?? "" })}
          </div>
        </Show>

        <Show when={samplerAssets().length > 0}>
          <div style={{ padding: "7px 8px", "font-size": "var(--font-size-xs)", color: "var(--text-muted)", "border-bottom": "1px solid var(--border)" }}>
            {i18n.t("plugin.samplerAssets")}
          </div>
          <For each={samplerAssets()}>
            {(asset) => (
              <div class="plugin-item" {...useTooltip(() => `${asset.attribution} · ${asset.license}`)}>
                <span class="plugin-item-icon"><KeyboardIcon /></span>
                <div class="plugin-item-info">
                  <div class="plugin-item-name">{asset.asset_id}</div>
                  <div class="plugin-item-vendor">{asset.license} · {i18n.t("plugin.fileCount", { count: asset.file_count })}</div>
                </div>
                <button class="toolbar-btn" aria-label={i18n.t("plugin.addPresetLabel", { asset: asset.asset_id })} onClick={() => void addSamplerPreset(asset)}>{i18n.t("plugin.addPreset")}</button>
              </div>
            )}
          </For>
        </Show>
        <Show when={scanning()}>
          <div class="plugin-scan-progress" role="status" aria-busy="true">
            <div class="plugin-scan-progress-text">{i18n.t("plugin.scanningDirs")}</div>
            <Show when={plugins().length === 0}>
              <For each={SKELETON_ROWS}>
                {() => (
                  <div class="plugin-item plugin-item-skeleton" aria-hidden="true">
                    <span class="skeleton-line skeleton-line-title" />
                    <span class="skeleton-line skeleton-line-meta" />
                  </div>
                )}
              </For>
            </Show>
          </div>
        </Show>

        <For each={filtered()}>
          {(plugin) => (
            <div
              class="plugin-item"
              role={plugin.category === "Instrument" ? "button" : "group"}
              tabIndex={plugin.category === "Instrument" ? 0 : undefined}
              aria-label={plugin.category === "Instrument"
                ? i18n.t("plugin.addTrackFrom", { name: plugin.name })
                : i18n.t("plugin.effectRow", { name: plugin.name })}
              {...useTooltip(() => plugin.path || plugin.id)}
              aria-disabled={plugin.category === "Instrument" ? addingTrack() : undefined}
              onDblClick={() => void handleAddTrack(plugin)}
              onContextMenu={(event) => {
                event.preventDefault();
                openContextMenu({
                  x: event.clientX,
                  y: event.clientY,
                  items: [
                    { type: "heading", label: plugin.name },
                    ...(plugin.category === "Instrument"
                      ? [{
                        label: i18n.t("browse.addTrack"),
                        onSelect: () => void handleAddTrack(plugin),
                      }]
                      : []),
                    { type: "separator" },
                    {
                      label: i18n.t("browse.copyPath"),
                      disabled: !plugin.path,
                      onSelect: () => void copyPath(plugin.path),
                    },
                    {
                      label: i18n.t("browse.reveal"),
                      disabled: !plugin.path,
                      onSelect: () => void revealPluginPath(plugin.path),
                    },
                  ],
                });
              }}
              onKeyDown={(event) => {
                if (plugin.category !== "Instrument") return;
                if (event.key !== "Enter" && event.key !== " ") return;
                event.preventDefault();
                if (!event.repeat) void handleAddTrack(plugin);
              }}
            >
              <span class="plugin-item-icon">{categoryIcon(plugin.category)}</span>
              <div class="plugin-item-info">
                <div class="plugin-item-name">{plugin.name}</div>
                <div class="plugin-item-vendor">{plugin.vendor || "OpenCompose"}</div>
              </div>
              <span class="plugin-item-format">{pluginFormatLabel(plugin.format)}</span>
              <Show when={plugin.category === "Effect"}>
                <button
                  class="toolbar-btn plugin-item-add"
                  style={{ width: "auto", padding: "0 8px", "font-size": "var(--font-size-sm)" }}
                  disabled={store.selectedTrackIndex() === null || addingEffect() !== null}
                  aria-label={store.selectedTrackIndex() === null
                    ? i18n.t("plugin.addEffectNoTrack", { name: plugin.name })
                    : i18n.t("plugin.addEffectToTrack", { name: plugin.name })}
                  {...useTooltip(() => store.selectedTrackIndex() === null
                    ? i18n.t("plugin.addEffectNoTrack", { name: plugin.name })
                    : i18n.t("plugin.addEffectToTrack", { name: plugin.name }))}
                  onClick={() => void handleAddEffect(plugin)}
                >{i18n.t("plugin.addEffect")}</button>
              </Show>
            </div>
          )}
        </For>

        <Show when={filtered().length === 0 && !scanning()}>
          <div style={{ padding: "16px", "text-align": "center", color: "var(--text-muted)", "font-size": "var(--font-size-sm)" }}>
            {i18n.t("plugin.noMatch")}
          </div>
        </Show>
      </div>
    </div>
  );
}
