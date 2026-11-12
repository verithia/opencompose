import { For, Show, createSignal, createMemo, createEffect, on, onCleanup } from "solid-js";
import { store } from "../stores/project";
import { ownedParameterSurface } from "../stores/ownedParameterGesture";
import type { AutomationWriteSurface } from "../stores/automationWrite";
import { panelStore } from "../stores/panels";
import { presetCategoryLabel, presetProgramLabel } from "../stores/presetNames";
import type { PresetCopyKey } from "../stores/locales/presetCopy";
import { i18n } from "../stores/i18n";
import { TextPromptDialog } from "./TextPromptDialog";
import { openContextMenu } from "./ContextMenu";
import * as api from "../stores/api";
import type { PluginParamInfo } from "../stores/api";
import { PluginParamPanel } from "./PluginParamPanel";
import type { ParamValue, PresetDef } from "../types/project";
import { EffectsRack } from "./EffectsRack";
import { AUDITION_KEYS, OpenSynthEditor } from "./OpenSynthEditor";
import type { FactoryPatchInfo } from "./FactoryPatchBrowser";
import { Dropdown } from "./Dropdown";
import { log } from "../stores/log";
import {
  readCachedPluginPanel,
  writeCachedPluginPanel,
  type CachedPluginPanel,
} from "../stores/pluginParamsCache";
import { PresetPicker, type SoundSource } from "./PresetPicker";
import { useTooltip } from "./Tooltip";
import { resolvePresetForDisplay } from "../utils/presetInheritance";

/** Human name of the instrument a preset belongs to, e.g. a VST3 name or `OpenSynth`. */
export function instrumentLabel(preset: Pick<PresetDef, "source" | "plugin" | "program">): string {
  const plugin = preset.plugin ?? "";
  const id = plugin.includes(":") ? plugin.slice(plugin.indexOf(":") + 1) : plugin;
  const pretty = (value: string) =>
    value
      .split(/[_\-.]+/)
      .filter(Boolean)
      .map((word) => (word.length <= 3 && /^[a-z]+$/.test(word) && word !== "os" ? word.toUpperCase() : word[0].toUpperCase() + word.slice(1)))
      .join(" ");
  if (plugin.startsWith("builtin.")) {
    const name = id.replace(/^builtin[._]/, "");
    const known: Record<string, string> = {
      open_synth: "OpenSynth",
      drumkit: i18n.t("preset.drumkit"),
    };
    return known[name] ?? pretty(name);
  }
  if (["vst3", "vst2", "clap", "lv2"].includes(preset.source ?? "")) return id ? pretty(id) : (preset.source ?? "").toUpperCase();
  if (preset.source === "soundfont") return preset.program != null ? `SoundFont #${preset.program}` : "SoundFont";
  if (preset.source === "sample") return i18n.t("preset.sample");
  if (preset.source === "au") return "Audio Unit";
  return preset.source === "builtin" ? i18n.t("preset.builtin") : preset.source ?? i18n.t("preset.notSet");
}

/** GM program names for the detail panel GM grid */
const GM_PROGRAMS: { program: number; category: string }[] = [
  { program: 0, category: "Piano" },
  { program: 1, category: "Piano" },
  { program: 4, category: "Piano" },
  { program: 5, category: "Piano" },
  { program: 6, category: "Piano" },
  { program: 16, category: "Organ" },
  { program: 19, category: "Organ" },
  { program: 24, category: "Guitar" },
  { program: 25, category: "Guitar" },
  { program: 27, category: "Guitar" },
  { program: 29, category: "Guitar" },
  { program: 30, category: "Guitar" },
  { program: 32, category: "Bass" },
  { program: 33, category: "Bass" },
  { program: 34, category: "Bass" },
  { program: 35, category: "Bass" },
  { program: 36, category: "Bass" },
  { program: 38, category: "Bass" },
  { program: 40, category: "Strings" },
  { program: 42, category: "Strings" },
  { program: 44, category: "Strings" },
  { program: 48, category: "Strings" },
  { program: 56, category: "Brass" },
  { program: 57, category: "Brass" },
  { program: 60, category: "Brass" },
  { program: 61, category: "Brass" },
  { program: 64, category: "Woodwinds" },
  { program: 65, category: "Woodwinds" },
  { program: 71, category: "Woodwinds" },
  { program: 73, category: "Woodwinds" },
  { program: 80, category: "Synth" },
  { program: 81, category: "Synth" },
  { program: 88, category: "Pad" },
  { program: 89, category: "Pad" },
  { program: 91, category: "Pad" },
];

/** Translate known source kinds; format identifiers are protocol names. */
function sourceLabel(source: PresetDef["source"]): string {
  if (!source) return i18n.t("preset.notSet");
  if (source === "builtin") return i18n.t("preset.builtin");
  if (source === "sample") return i18n.t("preset.sample");
  if (source === "soundfont") return "SoundFont";
  return source.toUpperCase();
}

/** Store library rows in the shape the OpenSynth browser already renders. */
function libraryPatchInfos(): FactoryPatchInfo[] {
  return store.libraryPatches().map((patch) => ({
    name: patch.name,
    category: patch.category,
    tags: patch.tags,
    tier: "user" as const,
    params: patch.params,
    effects: patch.effects,
  }));
}

export function PresetEditor() {
  const [picking, setPicking] = createSignal(false);
  /** The preset being renamed; the webview has no window.prompt. */
  const [renamePreset, setRenamePreset] = createSignal<string | null>(null);
  const [selectedPreset, setSelectedPresetLocal] = createSignal<string | null>(null);

  // Keep store in sync so Space bar can preview the selected preset
  const setSelectedPreset = (name: string | null) => {
    setSelectedPresetLocal(name);
    store.setSelectedPresetName(name);
    panelStore.setActiveRegion("bottom");
  };

  const presets = () => store.project.presets;
  const auditioning = () => {
    const state = store.audition();
    return state?.status === "loading" ? state.preset : null;
  };
  /** The preview for this preset is still preparing and can be stopped. */
  const loadingPreview = (name: string) => auditioning() === name;
  const selected = createMemo(() => {
    const name = selectedPreset();
    return name ? presets().find((p) => p.name === name) ?? null : null;
  });

  const presetUsage = createMemo(() => {
    const usage: Record<string, string[]> = {};
    for (const p of presets()) usage[p.name] = [];
    for (const t of store.project.tracks) {
      if (t.preset && usage[t.preset]) usage[t.preset].push(t.name);
    }
    return usage;
  });

  const handlePickerResult = (source: SoundSource, autoName: string) => {
    const opts: { program?: number; plugin?: string } = {};
    if (source.kind === "soundfont") {
      opts.program = source.program;
    } else if (["clap", "vst3", "vst2", "lv2", "builtin"].includes(source.kind)) {
      opts.plugin = source.pluginId;
    }
    store.addPreset(autoName, source.kind, opts);
    setPicking(false);
    setSelectedPreset(autoName);
  };

  const handleAddClick = () => {
    setPicking(true);
    setSelectedPreset(null);
  };

  return (
    <div class="preset-editor">
      {/* Left: preset list */}
      <div class="preset-list-pane">
        <div class="preset-list-header">
          <span class="preset-list-title">{i18n.t("panel.presets")}</span>
          <Show when={auditioning()}>
            {(name) => <span class="preset-audition-status">{i18n.t("preset.loading", { name: name() })}</span>}
          </Show>
          <button class="effects-rack-add-btn" aria-label={i18n.t("plugin.addPreset")} onClick={handleAddClick} {...useTooltip(() => i18n.t("plugin.addPreset"))}>+</button>
        </div>

        <div class="preset-list-items">
          <For each={presets()}>
            {(preset) => {
              const usage = () => presetUsage()[preset.name] ?? [];
              return (
                <div
                  class="preset-list-item"
                  classList={{ selected: selectedPreset() === preset.name && !picking() }}
                  onClick={() => { setSelectedPreset(preset.name); setPicking(false); }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    setSelectedPreset(preset.name);
                    openContextMenu({
                      x: event.clientX,
                      y: event.clientY,
                      items: [
                        { type: "heading", label: preset.name },
                        {
                          label: i18n.t("preset.rename"),
                          onSelect: () => { setRenamePreset(preset.name); },
                        },
                        {
                          label: i18n.t("preset.duplicate"),
                          onSelect: () => void store.duplicatePreset(preset.name, `${preset.name} copy`, false),
                        },
                        { type: "separator" },
                        {
                          label: i18n.t("preset.delete"),
                          danger: true,
                          // Any use blocks a bare delete: the operator has to
                          // clear the reference instead of losing it silently.
                          disabled: usage().length > 0,
                          hint: usage().length > 0
                            ? i18n.t("preset.deleteUsage", { tracks: usage().join(", ") })
                            : undefined,
                          onSelect: () => void store.removePreset(preset.name),
                        },
                      ],
                    });
                  }}
                  draggable={true}
                  onDragStart={(e) => {
                    e.dataTransfer?.setData("application/x-preset", preset.name);
                    e.dataTransfer!.effectAllowed = "copyMove";
                  }}
                >
                  <div style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                    <button
                      class="preset-preview-btn"
                      classList={{ loading: auditioning() === preset.name }}
                      aria-label={i18n.t(loadingPreview(preset.name) ? "tooltip.previewCancel" : store.auditionAction(`preset:${preset.name}`) === "pause" ? "tooltip.previewPause" : store.auditionAction(`preset:${preset.name}`) === "resume" ? "tooltip.previewResume" : "tooltip.preview")}
                      onClick={(e) => {
                        e.stopPropagation();
                        setSelectedPreset(preset.name);
                        // While the preview is still preparing, the same press
                        // stops it: a stuck load must have a way out.
                        if (loadingPreview(preset.name)) void store.stopAudition();
                        else void store.previewPreset(preset.name);
                      }}
                      {...useTooltip(() => i18n.t(loadingPreview(preset.name) ? "tooltip.previewCancel" : store.auditionAction(`preset:${preset.name}`) === "pause" ? "tooltip.previewPause" : store.auditionAction(`preset:${preset.name}`) === "resume" ? "tooltip.previewResume" : "tooltip.preview"))}
                    >{loadingPreview(preset.name) ? "×" : store.auditionAction(`preset:${preset.name}`) === "pause" ? "Ⅱ" : "▶"}</button>
                    <div class="preset-list-item-name">{preset.name}</div>
                    <button
                      class="preset-add-track-btn"
                      aria-label={i18n.t("preset.addTrack")}
                      onClick={async (e) => {
                        e.stopPropagation();
                        const trackName = preset.name.replace(/\s+\d+$/, "");
                        // Choose instrument based on preset source type
                        const isDrums = preset.source === "soundfont" && preset.program === 0;
                        const instCat = isDrums ? "drums" : preset.source === "clap" ? "synth" : "keys";
                        const instName = isDrums ? "acoustic_kit" : preset.source === "clap" ? `clap_${preset.name}` : "piano";
                        // The preset rides along with the track: one edit, one
                        // graph rebuild instead of two.
                        await store.addTrack(trackName, instCat, instName, preset.name);
                      }}
                      {...useTooltip(() => i18n.t("preset.addTrack"))}
                    >+↗</button>
                  </div>
                  <div class="preset-list-item-info">
                    {instrumentLabel(resolvePresetForDisplay(preset, presets()) ?? preset)}
                    {usage().length > 0 && <span class="preset-usage-badge">{usage().length}</span>}
                  </div>
                </div>
              );
            }}
          </For>

          <Show when={presets().length === 0 && !picking()}>
            <div style={{ padding: "16px", "text-align": "center", color: "var(--text-muted)", "font-size": "var(--font-size-sm)" }}>
              {i18n.t("preset.empty")}
            </div>
          </Show>
        </div>
      </div>

      {/* Right: picker grid OR detail */}
      <div class="preset-detail-pane">
        <Show when={picking()}>
          <PresetPicker
            onPick={handlePickerResult}
            onCancel={() => setPicking(false)}
            existingNames={presets().map((p) => p.name)}
          />
        </Show>
        <Show when={!picking() && selected()}>
          {(preset) => <PresetDetail preset={preset()} onSelect={setSelectedPreset} />}
        </Show>
        <Show when={!picking() && !selected()}>
          <div class="empty-state" style={{ height: "100%" }}>
            <div class="empty-state-text">{i18n.t("preset.select")}</div>
          </div>
        </Show>
      </div>

      <Show when={renamePreset()}>
        {(name) => (
          <TextPromptDialog
            title={i18n.t("preset.renameTitle")}
            initial={name()}
            onCancel={() => setRenamePreset(null)}
            onConfirm={(next) => {
              const from = name();
              setRenamePreset(null);
              void store.renamePreset(from, next);
            }}
          />
        )}
      </Show>
    </div>
  );
}

/** Group params by their module path (e.g. "Oscillators/Osc 1") */
function groupParamsByModule(params: PluginParamInfo[]): Map<string, PluginParamInfo[]> {
  const groups = new Map<string, PluginParamInfo[]>();
  for (const p of params) {
    // Use module path, or "General" if empty
    const key = p.module || "General";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(p);
  }
  return groups;
}

/** Short display name from a module path: "Oscillators/Osc 1" -> "Osc 1" */
function moduleShortName(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

/** Top-level category from module path: "Oscillators/Osc 1" -> "Oscillators" */
function moduleCategory(path: string): string {
  return path.split("/")[0] || "General";
}

function paramValueText(value: ParamValue): string {
  if ("Number" in value) return String(value.Number);
  if ("Db" in value) return `${value.Db}dB`;
  if ("Percentage" in value) return `${value.Percentage}%`;
  if ("Frequency" in value) return `${value.Frequency}Hz`;
  if ("Cents" in value) return `${value.Cents}ct`;
  if ("Time" in value) return `${value.Time}s`;
  if ("String" in value) return value.String;
  if ("Bool" in value) return String(value.Bool);
  return "";
}

function getPresetParam(preset: PresetDef, name: string): string {
  const found = preset.params.find(([key]) => key === name);
  return found ? paramValueText(found[1]) : "";
}

/** Defer until the pane has painted; jsdom has no animation frame. */
function nextFrame(callback: () => void): number {
  if (typeof requestAnimationFrame === "function") return requestAnimationFrame(callback);
  return setTimeout(callback, 0);
}

/** Wait for one painted frame; the spinner must be on screen before the
 * main-thread plugin instantiation blocks the event loop. */
function paintedFrame(): Promise<void> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      resolve();
    };
    if (typeof requestAnimationFrame !== "function") { finish(); return; }
    // A suspended webview never fires a frame; a bounded wait keeps the load
    // moving instead of hanging on a paint that will not come.
    setTimeout(finish, 60);
    requestAnimationFrame(() => requestAnimationFrame(finish));
  });
}

function cancelFrame(id: number): void {
  if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(id);
  else clearTimeout(id);
}

/** Dry audition of a hosted VST3 preset through the editor's own instance. */
function HostedAudition(props: { preset: string }) {
  const [active, setActive] = createSignal<Set<number>>(new Set());
  const start = (note: number) => {
    if (active().has(note)) return;
    setActive((current) => new Set(current).add(note));
    void store.openSynthNoteOn(props.preset, note);
  };
  const stop = (note: number) => {
    if (!active().has(note)) return;
    setActive((current) => {
      const next = new Set(current);
      next.delete(note);
      return next;
    });
    void store.openSynthNoteOff(props.preset, note);
  };
  const release = () => {
    for (const note of [...active()]) stop(note);
  };
  window.addEventListener("blur", release);
  onCleanup(() => {
    window.removeEventListener("blur", release);
    release();
  });
  return (
    <section class="synth-audition synth-audition-footer" aria-label={i18n.t("preset.dryPreview")}>
      <header>
        <span>{i18n.t("preset.play")}</span>
        <small {...useTooltip(() => i18n.t("preset.dryPreview"))}>{i18n.t("preset.dryPreview")}</small>
      </header>
      <div class="synth-keyboard">
        <For each={AUDITION_KEYS}>{([note, label]) => (
          <button
            classList={{ black: label.includes("#"), active: active().has(note) }}
            aria-label={i18n.t("preset.previewNote", { note: label })}
            aria-pressed={active().has(note)}
            {...useTooltip(() => i18n.t("preset.previewNote", { note: label }))}
            onPointerDown={(event) => {
              event.preventDefault();
              event.currentTarget.setPointerCapture?.(event.pointerId);
              start(note);
            }}
            onPointerUp={(event) => {
              stop(note);
              event.currentTarget.releasePointerCapture?.(event.pointerId);
            }}
            onPointerCancel={() => stop(note)}
            onLostPointerCapture={() => stop(note)}
            onBlur={() => stop(note)}
            onKeyDown={(event) => {
              if ((event.key === " " || event.key === "Enter") && !event.repeat) {
                event.preventDefault();
                start(note);
              }
            }}
            onKeyUp={(event) => {
              if (event.key === " " || event.key === "Enter") {
                event.preventDefault();
                stop(note);
              }
            }}
          ><span>{label}</span></button>
        )}</For>
      </div>
    </section>
  );
}

function PresetDetail(props: { preset: PresetDef; onSelect?: (name: string | null) => void }) {
  const p = () => props.preset;
  let disposed = false;
  const effective = createMemo(() => resolvePresetForDisplay(p(), store.project.presets));
  const keyboardCalibration = createMemo(() => {
    const preset = effective();
    return preset?.plugin === "builtin.open_synth" ? JSON.stringify(preset) : null;
  });
  createEffect(on(keyboardCalibration, (key) => {
    if (key === null) return;
    const name = p().name;
    const timer = setTimeout(() => {
      void api.warmOpenSynthKeyboard(name).catch(() => {});
    }, 180);
    onCleanup(() => clearTimeout(timer));
  }));
  const auditioning = () => {
    const state = store.audition();
    return state?.status === "loading" ? state.preset : null;
  };
  const [pluginParams, setPluginParams] = createSignal<PluginParamInfo[]>([]);
  /** The GM program being prepared; the grid stays disabled until it lands. */
  const [programPending, setProgramPending] = createSignal<number | null>(null);
  const pickProgram = async (program: number) => {
    if (programPending() !== null) return;
    setProgramPending(program);
    try {
      // The resync reopens the device and prepares the bank, so it can take
      // seconds; the click must say so instead of looking inert.
      await store.setPresetProgram(p().name, program);
    } finally {
      setProgramPending(null);
    }
  };
  let inventoryGeneration = 0;
  let pluginLoadSequence = 0;
  const [loadInFlight, setLoadInFlight] = createSignal<string | null>(null);
  const [pluginLoaded, setPluginLoaded] = createSignal(false);
  const [pluginError, setPluginError] = createSignal<string | null>(null);
  const [hasGui, setHasGui] = createSignal(false);
  const [editorBlocked, setEditorBlocked] = createSignal<string | null>(null);
  const [guiOpen, setGuiOpen] = createSignal(false);
  /** The editor runs in a helper process and can take seconds to answer. */
  const [guiOpening, setGuiOpening] = createSignal(false);
  const [expandedModules, setExpandedModules] = createSignal<Set<string>>(new Set());
  const [customParamName, setCustomParamName] = createSignal("");
  const [customParamValue, setCustomParamValue] = createSignal("");
  const [ccNumber, setCcNumber] = createSignal("64");
  const [ccParam, setCcParam] = createSignal("");
  const [expressionControl, setExpressionControl] = createSignal("pressure");
  const [expressionType, setExpressionType] = createSignal("pressure");
  const [cc14Control, setCc14Control] = createSignal("dynamics");
  const [cc14Number, setCc14Number] = createSignal("1");
  const [drumName, setDrumName] = createSignal("hihat_closed");
  const [drumNote, setDrumNote] = createSignal("");
  const [drumChoke, setDrumChoke] = createSignal("");
  const [noiseControl, setNoiseControl] = createSignal("sustain");
  const [noiseOnNote, setNoiseOnNote] = createSignal("");
  const [noiseOffNote, setNoiseOffNote] = createSignal("");
  const [noiseVelocity, setNoiseVelocity] = createSignal("32");
  const [noiseDurationMs, setNoiseDurationMs] = createSignal("50");
  const [noiseThreshold, setNoiseThreshold] = createSignal("0.5");
  const [releaseArticulation, setReleaseArticulation] = createSignal("legato");
  const [releaseNote, setReleaseNote] = createSignal("");
  const [releaseVelocity, setReleaseVelocity] = createSignal("12");
  const [releaseDurationMs, setReleaseDurationMs] = createSignal("30");
  const [releaseSustainAware, setReleaseSustainAware] = createSignal(false);
  const [microtuningNote, setMicrotuningNote] = createSignal("C4");
  const [microtuningCents, setMicrotuningCents] = createSignal("0");
  const [scalaFile, setScalaFile] = createSignal("");
  const [scalaRootNote, setScalaRootNote] = createSignal("C4");
  const [profileIssue, setProfileError] = createSignal<string | (() => string) | null>(null);
  const profileError = () => {
    const issue = profileIssue();
    return typeof issue === "function" ? issue() : issue ? i18n.t("preset.updateFailed", { error: issue }) : null;
  };

  const gmName = createMemo(() => {
    const program = p().program;
    if (program == null) return null;
    if (program === 0 && p().source === "soundfont") {
      // Check if this preset is used by any drum track/pattern
      const isDrums = store.project.tracks.some(t => t.preset === p().name && t.instrument.category === "drums")
        || store.project.patterns.some(pat => pat.preset === p().name && pat.instrument.category === "drums");
      if (isDrums) return i18n.t("preset.standardDrums");
    }
    return presetProgramLabel(program);
  });

  const usedBy = createMemo(() =>
    store.project.tracks.filter((t) => t.preset === p().name).map((t) => t.name)
  );

  const isExternalPlugin = () => ["clap", "vst3", "vst2"].includes(p().source ?? "") && !p().plugin?.startsWith("builtin.");
  /** Formats the generated parameter panel serves: they have no in-app editor. */
  const showsGeneratedPanel = () => ["vst2", "lv2", "au"].includes(p().source ?? "");
  /** Every hosted format carries an opaque plugin state; a built-in engine
   * (open_synth etc.) has params, not a state blob. */
  const isHostedPlugin = () => ["clap", "vst3", "vst2", "au", "lv2"].includes(p().source ?? "");

  // Grouped params by top-level category, then by module
  const groupedParams = createMemo(() => groupParamsByModule(pluginParams()));
  const categories = createMemo(() => {
    const cats = new Map<string, string[]>();
    for (const key of groupedParams().keys()) {
      const cat = moduleCategory(key);
      if (!cats.has(cat)) cats.set(cat, []);
      cats.get(cat)!.push(key);
    }
    return cats;
  });

  const pluginLoading = () => loadInFlight() === p().name;

  const applyCachedPanel = (panel: CachedPluginPanel) => {
    inventoryGeneration += 1;
    if (!panel.ok) {
      setPluginError(panel.error);
      return;
    }
    setPluginParams(panel.params);
    setPluginLoaded(true);
    setHasGui(panel.hasGui);
    setEditorBlocked(panel.editorBlocked);
  };

  /** Refresh the cached panel; drop it when the preset's state moved on. */
  const cachePanel = () => {
    if (!pluginLoaded()) return;
    if (!readCachedPluginPanel(p())) return;
    writeCachedPluginPanel(p(), {
      ok: true,
      params: pluginParams(),
      hasGui: hasGui(),
      editorBlocked: editorBlocked(),
    });
  };

  const loadPlugin = async (force = false): Promise<boolean> => {
    const name = p().name;
    if (loadInFlight() === name) return false;
    if (!force) {
      const cached = readCachedPluginPanel(p());
      if (cached) {
        applyCachedPanel(cached);
        return cached.ok;
      }
    }
    const generation = ++inventoryGeneration;
    const request = ++pluginLoadSequence;
    const ownsDocument = store.parameterRequestOwner();
    const ownsInventory = () => !disposed && ownsDocument() && p().name === name
      && inventoryGeneration === generation;
    setLoadInFlight(name);
    setPluginError(null);
    try {
      // Instantiating the module runs on the main thread and freezes this
      // webview for its load time. Let the frame with the spinner land first,
      // so the click never looks inert.
      await paintedFrame();
      const params = await api.loadPluginForPreset(name);
      if (!ownsInventory()) return false;
      // The editor status keys off the loaded instance, so read it before caching.
      let gui = false;
      let blocked: string | null = null;
      try {
        gui = await api.pluginHasGui(name);
        blocked = gui ? await api.pluginEditorStatus(name) : null;
      } catch (_) {}
      if (!ownsInventory()) return false;
      const current = store.project.presets.find((item) => item.name === name);
      if (current) {
        writeCachedPluginPanel(current, { ok: true, params, hasGui: gui, editorBlocked: blocked });
      }
      if (p().name !== name) return true;
      setPluginParams(params);
      setPluginLoaded(true);
      setHasGui(gui);
      setEditorBlocked(typeof blocked === "string" ? blocked : null);
      log.info("plugin", `Loaded ${params.length} params for preset '${name}'`);
      return true;
    } catch (e) {
      const message = String(e);
      if (!ownsInventory()) return false;
      const current = store.project.presets.find((item) => item.name === name);
      if (current) writeCachedPluginPanel(current, { ok: false, error: message });
      if (p().name !== name) return false;
      setPluginError(message);
      log.error("plugin", `Failed to load plugin for '${name}': ${e}`);
      return false;
    } finally {
      if (pluginLoadSequence === request && loadInFlight() === name) setLoadInFlight(null);
    }
  };

  // Reset per-preset plugin state; the pane loads once it is on screen, so a
  // row click never waits for the plugin and rapid scrubbing loads only the last.
  let autoLoadFrame: number | null = null;
  let autoLoadTimeout: ReturnType<typeof setTimeout> | null = null;
  const cancelAutoLoad = () => {
    if (autoLoadFrame !== null) cancelFrame(autoLoadFrame);
    if (autoLoadTimeout !== null) clearTimeout(autoLoadTimeout);
    autoLoadFrame = null;
    autoLoadTimeout = null;
  };

  createEffect(on(
    // A preset keeps its name when its instrument changes; the plugin load
    // must follow the source as well as the name.
    () => `${p().name}\u0000${p().source ?? ""}\u0000${p().plugin ?? ""}`,
    () => {
      cancelAutoLoad();
      inventoryGeneration += 1;
      pluginLoadSequence += 1;
      setLoadInFlight(null);
      setPluginParams([]);
      setPluginLoaded(false);
      setPluginError(null);
      setHasGui(false);
      setEditorBlocked(null);
      setGuiOpen(false);
      setExpandedModules(new Set<string>());
      if (!isExternalPlugin()) return;
      const cached = readCachedPluginPanel(p());
      if (cached) {
        applyCachedPanel(cached);
        return;
      }
      const startLoad = () => {
        cancelAutoLoad();
        void loadPlugin();
      };
      autoLoadFrame = nextFrame(startLoad);
      // Native webviews can suspend animation frames while panes/windows are hidden.
      autoLoadTimeout = setTimeout(startLoad, 250);
    },
  ));

  const handleOpenGui = async () => {
    log.info("plugin", `Opening GUI for '${p().name}'...`);
    setGuiOpening(true);
    try {
      const result = await api.openPluginGui(p().name);
      setGuiOpen(true);
      log.info("plugin", `GUI opened for '${p().name}' (${result[0]}x${result[1]})`);
    } catch (e) {
      setPluginError(String(e));
      log.error("plugin", `Failed to open GUI for '${p().name}': ${e}`);
    } finally {
      setGuiOpening(false);
    }
  };

  const handleCloseGui = async () => {
    log.info("plugin", `Closing GUI for '${p().name}'...`);
    try {
      await api.closePluginGui(p().name);
    } catch (_) {}
    setGuiOpen(false);
  };

  // Poll for GUI close (native X button) and for the notifications the plugin
  // reports. The parameter list is re-read only when one of them says a value
  // or the inventory moved: enumerating every parameter four times a second
  // stuttered the plugin's own window and every other command.
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  createEffect(() => {
    if (guiOpen()) {
      // Start polling at 4Hz
      pollTimer = setInterval(async () => {
        const name = p().name;
        const generation = inventoryGeneration;
        const ownsDocument = store.parameterRequestOwner();
        const ownsPoll = () => !disposed && ownsDocument() && p().name === name && inventoryGeneration === generation;
        try {
          // Check if GUI was closed externally
          const closed = await api.pollPluginGuiClosed(name);
          if (!ownsPoll()) return;
          if (closed) {
            setGuiOpen(false);
            log.info("plugin", `GUI closed externally for '${p().name}'`);
          }
          // A helper that crashed reports a sentence here; keep the inspector usable.
          const failure = await api.pluginEditorStatus(name);
          if (!ownsPoll()) return;
          if (failure) {
            setEditorBlocked(failure);
            setGuiOpen(false);
            setPluginError(failure);
            log.error("plugin", `Editor failed for '${p().name}': ${failure}`);
          }
          if (!guiOpen()) return;
          // Refresh params only when the plugin reported a change (a knob move,
          // a program change, a reload), not on a fixed timer.
          const report = await api.pollPluginHostEvents(name);
          if (!ownsPoll()) return;
          if (report.params_changed) {
            const refresh = ++inventoryGeneration;
            const params = await api.getPluginParams(name);
            if (disposed || !ownsDocument() || p().name !== name || inventoryGeneration !== refresh) return;
            setPluginParams(params);
            cachePanel();
          }
        } catch (_) {}
      }, 250);
    } else {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    }
  });

  onCleanup(() => {
    cancelAutoLoad();
    if (pollTimer) clearInterval(pollTimer);
    disposed = true;
    for (const session of activeParamGestures) void endParamGesture(session);
  });

  const toggleModule = (mod: string) => {
    const s = new Set(expandedModules());
    if (s.has(mod)) s.delete(mod); else s.add(mod);
    setExpandedModules(s);
  };

  const setParam = async (name: string, value: string, token?: api.UndoGestureToken) => {
    const generation = store.documentIdentity().generation;
    setProfileError(null);
    const error = await store.setPresetParam(p().name, name, value.trim() === "" ? null : value, token);
    if (store.documentIdentity().generation !== generation) return error;
    if (error) setProfileError(error);
    else cachePanel();
    return error;
  };

  const setParams = async (params: [string, string | null][], replaceSynth = false, token?: api.UndoGestureToken) => {
    const generation = store.documentIdentity().generation;
    setProfileError(null);
    const error = await store.setPresetParams(p().name, params, replaceSynth, token);
    if (store.documentIdentity().generation !== generation) return error;
    if (error) setProfileError(error);
    else cachePanel();
    return error;
  };

  interface DirectParameterGesture {
    surface: AutomationWriteSurface;
    updates: Promise<void>;
    closed: boolean;
    acknowledged: boolean;
    owns(): boolean;
    removeListeners(): void;
    last?: { param: PluginParamInfo; value: number };
  }
  const [directDrafts, setDirectDrafts] = createSignal<Record<string, { session: DirectParameterGesture; value: number }>>({});
  const directDraft = (param: PluginParamInfo) => {
    const draft = directDrafts()[String(param.id)];
    return draft?.session.owns() ? draft.value : null;
  };
  const activeParamGestures = new Set<DirectParameterGesture>();
  let directClosing: Promise<void> = Promise.resolve();

  const automationTrackIndex = () => {
    const selected = store.selectedTrackIndex();
    if (selected !== null && store.project.tracks[selected]?.preset === p().name) return selected;
    return store.project.tracks.findIndex((track) => track.preset === p().name);
  };

  const automationPosition = () => {
    const numerator = Math.max(1, store.project.properties.time_signature.numerator);
    const absolute = Math.max(0, store.transport().position_beats);
    const barOffset = Math.floor(absolute / numerator);
    const beatOffset = absolute - barOffset * numerator;
    const beatWhole = Math.floor(beatOffset);
    return {
      bar: barOffset + 1,
      beat: beatWhole + 1,
      subdivision: Math.min(3, Math.round((beatOffset - beatWhole) * 4)),
    };
  };

  const beginParamGesture = (param: PluginParamInfo): DirectParameterGesture | null => {
    const trackIndex = automationTrackIndex();
    if (trackIndex < 0) return null;
    const presetName = p().name;
    const ownsDocument = store.parameterRequestOwner();
    const inventory = inventoryGeneration;
    const owns = () => !disposed && ownsDocument() && p().name === presetName
      && inventoryGeneration === inventory;
    const surface = ownedParameterSurface({
      key: `plugin\u0000${presetName}\u0000${param.name}`, label: param.name,
      value: () => param.current, position: automationPosition,
      isCurrent: owns,
      expectedStamp: ownsDocument.expectedStamp,
      begin: async (stamp) => {
        try {
          return await api.beginGuardedPluginParamGesture(presetName, trackIndex, param.name, stamp);
        } catch (error) {
          // The parameter table came from the out-of-process probe, so no live
          // instance exists yet: the first edit loads it, then retries.
          if (!String(error).includes("not loaded")) throw error;
          await api.ensurePluginInstance(presetName);
          return api.beginGuardedPluginParamGesture(presetName, trackIndex, param.name, stamp);
        }
      },
      point: (token, position, value) => api.updateOwnedPluginParamGesture(token, presetName, param.name, value, position),
      end: token => api.endOwnedPluginParamGesture(token, presetName, param.name),
      apply: result => { session.acknowledged = true; store.applyAiSnapshot(result.snapshot); },
    });
    const session: DirectParameterGesture = {
      surface, updates: directClosing.then(() => surface.open()), closed: false, acknowledged: false,
      owns, removeListeners: () => {},
    };
    // Keep the refusal for the closing path while avoiding an unhandled open.
    void session.updates.catch(() => {});
    activeParamGestures.add(session);
    return session;
  };

  const updateParamGesture = (session: DirectParameterGesture, param: PluginParamInfo, value: number) => {
    if (session.closed || !session.owns()) return;
    session.last = { param, value };
    setDirectDrafts(drafts => ({ ...drafts, [String(param.id)]: { session, value } }));
    const position = automationPosition();
    session.updates = session.updates.then(() => session.surface.point(position, value));
    void session.updates.catch(() => {});
  };

  const endParamGesture = (session: DirectParameterGesture): Promise<void> => {
    if (session.closed) return directClosing;
    session.closed = true;
    session.removeListeners();
    activeParamGestures.delete(session);
    const closing = (async () => {
      try { await session.updates; }
      catch (error) { if (session.owns()) setProfileError(String(error)); }
      finally {
        try { await session.surface.close(); }
        catch (error) { if (session.owns()) setProfileError(String(error)); }
      }
      const last = session.last;
      if (session.acknowledged && session.owns() && last) {
        setPluginParams(params => params.map(item => item.id === last.param.id
          ? { ...item, current: last.value, display: last.value.toFixed(4) } : item));
        cachePanel();
      }
      if (last) setDirectDrafts(drafts => {
        if (drafts[String(last.param.id)]?.session !== session) return drafts;
        const next = { ...drafts }; delete next[String(last.param.id)]; return next;
      });
    })();
    directClosing = closing;
    return closing;
  };

  const setValidationError = (message: () => string) => {
    setProfileError(() => message);
    return false;
  };

  const midiNoteNumber = (value: string): number | null => {
    const trimmed = value.trim();
    if (/^\d+$/.test(trimmed)) return Number(trimmed);
    const match = /^([A-Ga-g])(#|b)?(-?\d+)$/.exec(trimmed);
    if (!match) return null;
    const base: Record<string, number> = { c: 0, d: 2, e: 4, f: 5, g: 7, a: 9, b: 11 };
    const accidental = match[2] === "#" ? 1 : match[2] === "b" ? -1 : 0;
    return (Number(match[3]) + 1) * 12 + base[match[1].toLowerCase()] + accidental;
  };

  const validNoteName = (value: string) => {
    const note = midiNoteNumber(value);
    return note !== null && note >= 0 && note <= 127;
  };

  const validateRange = (label: PresetCopyKey, value: string, min: number, max: number) => {
    const trimmed = value.trim();
    const n = Number(trimmed);
    return Number.isInteger(n) && n >= min && n <= max
      ? true
      : setValidationError(() => i18n.t("preset.rangeError", { label: i18n.t(label), min, max }));
  };

  const validateNumber = (label: PresetCopyKey, value: string) => (
    Number.isFinite(Number(value)) ? true : setValidationError(() => i18n.t("preset.numberError", { label: i18n.t(label) }))
  );

  const validateNote = (label: PresetCopyKey, value: string) => (
    value.trim() === "" || validNoteName(value.trim()) ? true : setValidationError(() => i18n.t("preset.noteError", { label: i18n.t(label) }))
  );

  const editParam = async (oldName: string, nextName: string, nextValue: string) => {
    const name = nextName.trim();
    if (!name) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.paramName") }));
      return;
    }
    if (name !== oldName) {
      await setParams([[oldName, null], [name, nextValue]]);
      return;
    }
    await setParam(name, nextValue);
  };

  const addCcMapping = () => {
    if (!validateRange("preset.ccNumber", ccNumber(), 0, 127)) return;
    if (!ccParam().trim()) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.pluginParameter") }));
      return;
    }
    void setParam(`cc_${Number.parseInt(ccNumber(), 10)}_param`, ccParam());
  };

  const addExpressionMapping = () => {
    const control = expressionControl().trim();
    if (!control) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.expressionControl") }));
      return;
    }
    void setParam(`control_${control}_note_expression`, expressionType());
  };

  const addCc14Mapping = () => {
    const control = cc14Control().trim();
    if (!control) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.cc14Control") }));
      return;
    }
    if (!validateRange("preset.cc14Number", cc14Number(), 0, 31)) return;
    void setParam(`control_${control}_cc14`, String(Number.parseInt(cc14Number(), 10)));
  };

  const addDrumMapping = async () => {
    const drum = drumName().trim();
    if (!drum) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.drumName") }));
      return;
    }
    if (drumNote().trim() && !validateNote("preset.drumNote", drumNote())) return;
    await setParams([
      ...(drumNote().trim() ? [[`drum_${drum}_note`, drumNote()] as [string, string]] : []),
      ...(drumChoke().trim() ? [[`drum_${drum}_choke_group`, drumChoke()] as [string, string]] : []),
    ]);
  };

  const addNoiseMapping = async () => {
    const control = noiseControl().trim();
    if (!control) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.noiseControl") }));
      return;
    }
    if (!validateNote("preset.noiseOn", noiseOnNote())) return;
    if (!validateNote("preset.noiseOff", noiseOffNote())) return;
    if (noiseVelocity().trim() && !validateRange("preset.noiseVelocity", noiseVelocity(), 0, 127)) return;
    if (noiseDurationMs().trim() && !validateNumber("preset.noiseDuration", noiseDurationMs())) return;
    if (noiseThreshold().trim() && !validateNumber("preset.noiseThreshold", noiseThreshold())) return;
    await setParams([
      ...(noiseOnNote().trim() ? [[`control_${control}_on_note`, noiseOnNote()] as [string, string]] : []),
      ...(noiseOffNote().trim() ? [[`control_${control}_off_note`, noiseOffNote()] as [string, string]] : []),
      ...(noiseVelocity().trim() ? [[`control_${control}_noise_velocity`, noiseVelocity()] as [string, string]] : []),
      ...(noiseDurationMs().trim() ? [[`control_${control}_noise_duration_ms`, noiseDurationMs()] as [string, string]] : []),
      ...(noiseThreshold().trim() ? [[`control_${control}_noise_threshold`, noiseThreshold()] as [string, string]] : []),
    ]);
  };

  const addReleaseTrigger = async () => {
    const articulation = releaseArticulation().trim();
    if (!articulation) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.releaseArticulation") }));
      return;
    }
    if (!validateNote("preset.releaseNote", releaseNote())) return;
    if (releaseVelocity().trim() && !validateRange("preset.releaseVelocity", releaseVelocity(), 0, 127)) return;
    if (releaseDurationMs().trim() && !validateNumber("preset.releaseDuration", releaseDurationMs())) return;
    await setParams([
      ...(releaseNote().trim() ? [[`articulation_${articulation}_release_note`, releaseNote()] as [string, string]] : []),
      ...(releaseVelocity().trim() ? [[`articulation_${articulation}_release_velocity`, releaseVelocity()] as [string, string]] : []),
      ...(releaseDurationMs().trim() ? [[`articulation_${articulation}_release_duration_ms`, releaseDurationMs()] as [string, string]] : []),
      [`articulation_${articulation}_release_sustain_aware`, String(releaseSustainAware())],
    ]);
  };

  const addMicrotuning = () => {
    const note = microtuningNote().trim();
    if (!note) {
      setValidationError(() => i18n.t("preset.requiredError", { label: i18n.t("preset.microtuningNote") }));
      return;
    }
    if (!validateNote("preset.microtuningNote", note)) return;
    if (!validateNumber("preset.microtuningCents", microtuningCents())) return;
    void setParam(`microtuning_${note}_cents`, microtuningCents());
  };

  const addScalaTuning = async () => {
    if (scalaFile().trim() && !scalaFile().trim().toLowerCase().endsWith(".scl")) {
      setValidationError(() => i18n.t("preset.scalaError"));
      return;
    }
    if (!validateNote("preset.scalaRoot", scalaRootNote())) return;
    await setParams([
      ...(scalaFile().trim() ? [["scala_file", scalaFile()] as [string, string]] : []),
      ...(scalaRootNote().trim() ? [["scala_root_note", scalaRootNote()] as [string, string]] : []),
    ]);
  };

  const browsePresetFile = async (paramName: string, extensions: string[], setValue?: (path: string) => void) => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [{ name: i18n.t("preset.profileFile"), extensions }],
    });
    if (!path || Array.isArray(path)) return;
    setValue?.(path);
    void setParam(paramName, path);
  };

  const saveClapStateFile = async () => {
    setProfileError(null);
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      filters: [{ name: i18n.t("preset.stateFile", { format: "CLAP" }), extensions: ["clapstate", "bin"] }],
    });
    if (!path) return;
    await store.savePluginStateFile(p().name, path);
  };

  const saveClapStateEmbedded = async () => {
    setProfileError(null);
    await store.savePluginStateEmbedded(p().name);
  };

  const saveVst2StateFile = async () => {
    setProfileError(null);
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      filters: [{ name: i18n.t("preset.stateFile", { format: "VST2" }), extensions: ["vst2state", "bin"] }],
    });
    if (!path) return;
    await store.savePluginStateFile(p().name, path);
  };

  const saveVst2StateEmbedded = async () => {
    setProfileError(null);
    await store.savePluginStateEmbedded(p().name);
  };

  const saveVst3StateFile = async () => {
    setProfileError(null);
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      filters: [{ name: i18n.t("preset.stateFile", { format: "VST3" }), extensions: ["vstpreset", "bin"] }],
    });
    if (!path) return;
    await store.savePluginStateFile(p().name, path);
  };

  const saveVst3StateEmbedded = async () => {
    setProfileError(null);
    await store.savePluginStateEmbedded(p().name);
  };

  /** Capture a hosted format's state; the command infers the format. */
  const saveHostedStateFile = async (label: string, extensions: string[]) => {
    setProfileError(null);
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      filters: [{ name: i18n.t("preset.stateFile", { format: label }), extensions }],
    });
    if (!path) return;
    await store.savePluginStateFile(p().name, path);
  };

  const saveHostedStateEmbedded = async () => {
    setProfileError(null);
    await store.savePluginStateEmbedded(p().name);
  };

  const importFfpPreset = async () => {
    const name = p().name;
    const ownsDocument = store.parameterRequestOwner();
    const originalInventory = inventoryGeneration;
    const ownsSource = () => !disposed && ownsDocument() && p().name === name;
    setProfileError(null);
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [{ name: i18n.t("preset.ffpPreset"), extensions: ["ffp"] }],
    });
    if (!path || Array.isArray(path) || !ownsSource() || inventoryGeneration !== originalInventory) return;
    const refresh = ++inventoryGeneration;
    const report = await store.importFfpPreset(name, path);
    if (!report || !ownsSource() || inventoryGeneration !== refresh) return;
    const params = await api.getPluginParams(name);
    if (!ownsSource() || inventoryGeneration !== refresh) return;
    setPluginParams(params);
    cachePanel();
    const summary = () => i18n.t("preset.imported", { mapped: report.mapped, parsed: report.parsed });
    if (report.unmapped.length > 0) {
      setProfileError(() => () => i18n.t("preset.unmapped", { summary: summary(), names: report.unmapped.join(", ") }));
    } else {
      log.info("plugin", summary());
    }
  };

  return (
    <div class="preset-detail">
      <aside class="preset-identity">
      <div class="preset-detail-header">
        <span class="preset-detail-name">{p().name}</span>
        <div style={{ display: "flex", gap: "4px" }}>
          <button
            class="preset-preview-btn"
            classList={{ loading: auditioning() === p().name }}
            aria-label={i18n.t(store.auditionAction(`preset:${p().name}`) === "pause" ? "tooltip.previewPause" : store.auditionAction(`preset:${p().name}`) === "resume" ? "tooltip.previewResume" : "tooltip.preview")}
            style={{ width: "24px", height: "24px", "font-size": "14px" }}
            onClick={() => {
              store.setSelectedPresetName(p().name);
              panelStore.setActiveRegion("bottom");
              void store.previewPreset(p().name);
            }}
            {...useTooltip(() => i18n.t(store.auditionAction(`preset:${p().name}`) === "pause" ? "tooltip.previewPause" : store.auditionAction(`preset:${p().name}`) === "resume" ? "tooltip.previewResume" : "tooltip.preview"))}
          >{store.auditionAction(`preset:${p().name}`) === "pause" ? "Ⅱ" : "▶"}</button>
          <button
            class="toolbar-btn preset-delete-button"
            aria-label={i18n.t("preset.delete")}
            onClick={() => store.removePreset(p().name)}
            {...useTooltip(() => i18n.t("preset.delete"))}
          >{i18n.t("preset.deleteShort")}</button>
        </div>
      </div>

      <details class="preset-metadata">
      <summary>{i18n.t("preset.metadata")}</summary>
      <div class="preset-detail-section">
        <div class="preset-detail-label">{i18n.t("preset.source")}</div>
        <div class="preset-detail-value">{sourceLabel((effective() ?? p()).source)}</div>
      </div>

      <Show when={(effective() ?? p()).plugin}>
        <div class="preset-detail-section">
          <div class="preset-detail-label">{i18n.t("preset.plugin")}</div>
          <div class="preset-detail-value">{(effective() ?? p()).plugin}</div>
        </div>
      </Show>
      </details>
      </aside>
      <div class="preset-workspace">
      {/* SoundFont GM picker */}
      <Show when={p().source === "soundfont"}>
        <div class="preset-detail-section">
          <div class="preset-detail-label">{i18n.t("preset.gmProgram")}</div>
          <div class="preset-detail-value">{gmName() ?? i18n.t("preset.notSet")} <Show when={p().program != null}>({p().program})</Show></div>
          <div class="preset-program-grid">
            <For each={GM_PROGRAMS}>
              {(gm) => (
                <button
                  class="preset-program-btn"
                  classList={{
                    active: p().program === gm.program,
                    loading: programPending() === gm.program,
                  }}
                  disabled={programPending() !== null}
                  aria-busy={programPending() === gm.program}
                  {...useTooltip(() => `${presetProgramLabel(gm.program)} (${presetCategoryLabel(gm.category)})`)}
                  onClick={() => void pickProgram(gm.program)}
                >
                  <span class="preset-program-btn-name">{presetProgramLabel(gm.program)}</span>
                </button>
              )}
            </For>
          </div>
          <Show when={programPending() !== null}>
            <div class="preset-program-pending" role="status" aria-live="polite">
              <span class="preset-params-spinner" aria-hidden="true" />
              <span>{i18n.t("preset.programPreparing", { name: presetProgramLabel(programPending()!) })}</span>
            </div>
          </Show>
        </div>
      </Show>

      {/* Generated panel for formats without an editor */}
      <Show when={showsGeneratedPanel()}>
        <div class="preset-detail-section">
          <PluginParamPanel preset={p().name} />
        </div>
      </Show>

      {/* CLAP plugin section */}
      <Show when={isExternalPlugin()}>
        <div class="preset-detail-section">
          <div class="preset-detail-label">{i18n.t("preset.formatPlugin", { format: p().source?.toUpperCase() ?? "" })}</div>
          <div class="preset-detail-value">{p().plugin ?? i18n.t("preset.notSet")}</div>
          <div class="clap-btn-row">
            <Show when={hasGui()}>
              <button
                class="clap-gui-btn"
                classList={{ loading: guiOpening() }}
                disabled={editorBlocked() !== null || guiOpening()}
                aria-busy={guiOpening()}
                {...useTooltip(
                  () => editorBlocked() ?? i18n.t(guiOpen() ? "preset.closeEditor" : "preset.openEditor"),
                )}
                onClick={guiOpen() ? handleCloseGui : handleOpenGui}
              >
                {i18n.t(guiOpening() ? "preset.openingEditor" : guiOpen() ? "preset.closeEditor" : "preset.openEditor")}
              </button>
            </Show>
              <button
                class="preset-reload-params"
                aria-label={i18n.t("preset.reloadParams")}
                disabled={pluginLoading()}
                {...useTooltip(() => i18n.t("preset.reloadParams"))}
                onClick={() => { cancelAutoLoad(); void loadPlugin(true); }}
              >
                {pluginLoaded() || pluginError() ? "\u21BB" : i18n.t("preset.reloadParams")}
              </button>
          </div>
          <Show when={editorBlocked()}>
            <div style={{ color: "var(--stop-red)", "font-size": "var(--font-size-xs)", "margin-top": "4px" }}>
              {editorBlocked()}
            </div>
          </Show>
          <Show when={pluginError()}>
            <div style={{ color: "var(--stop-red)", "font-size": "var(--font-size-xs)", "margin-top": "4px" }}>
              {i18n.t("preset.pluginFailed", { error: pluginError()! })}
            </div>
          </Show>
        </div>

        {/* The plugin reads its state on the first open; the table replaces the spinner. */}
        <Show when={pluginLoading()}>
          <div class="preset-detail-section" role="status">
            <div class="preset-detail-label">{i18n.t("preset.parameters")}</div>
            <div class="preset-params-loading">
              <span class="preset-params-spinner" aria-hidden="true" />
              {/* Name what is loading: a bare spinner says nothing while a
                  large instrument takes seconds to instantiate. */}
              <span>{i18n.t("preset.loadingParamsNamed", { name: p().name })}</span>
            </div>
          </div>
        </Show>

        {/* Plugin parameters — grouped by module (CLAP/VST3 keep their own list) */}
        <Show when={!pluginLoading() && pluginParams().length > 0 && !showsGeneratedPanel()}>
          <div class="preset-detail-section">
            <div class="preset-detail-label">{i18n.t("preset.parameterCount", { count: pluginParams().length })}</div>
            <div class="plugin-params-grouped">
              <For each={[...categories().entries()]}>
                {([catName, modulePaths]) => (
                  <div class="plugin-param-category">
                    <div class="plugin-param-category-name">{catName === "General" ? i18n.t("plugin.panel.general") : catName}</div>
                    <For each={modulePaths}>
                      {(modPath) => {
                        const modParams = () => groupedParams().get(modPath) ?? [];
                        const shortName = moduleShortName(modPath);
                        const isExpanded = () => expandedModules().has(modPath);
                        return (
                          <div class="plugin-param-module">
                            <div
                              class="plugin-param-module-header"
                              onClick={() => toggleModule(modPath)}
                            >
                              <span class="plugin-param-module-arrow">{isExpanded() ? "\u25BE" : "\u25B8"}</span>
                              <span class="plugin-param-module-name">{shortName === "General" ? i18n.t("plugin.panel.general") : shortName}</span>
                              <span class="plugin-param-module-count">{modParams().length}</span>
                            </div>
                            <Show when={isExpanded()}>
                              <div class="plugin-param-module-params">
                                <For each={modParams()}>
                                  {(param) => {
                                    let interaction: DirectParameterGesture | null = null;
                                    let pointerSequence = false;
                                    const close = (owner: DirectParameterGesture | null) => {
                                      if (!owner) return;
                                      if (interaction === owner) interaction = null;
                                      void endParamGesture(owner);
                                    };
                                    const start = (event: PointerEvent) => {
                                      if ((event.button ?? 0) !== 0) return;
                                      close(interaction);
                                      pointerSequence = true;
                                      const owner = beginParamGesture(param);
                                      interaction = owner;
                                      if (!owner) return;
                                      const release = (done: PointerEvent) => {
                                        if ((done.pointerId ?? 0) === (event.pointerId ?? 0)) close(owner);
                                      };
                                      const blur = () => close(owner);
                                      owner.removeListeners = () => {
                                        window.removeEventListener("pointerup", release, true);
                                        window.removeEventListener("pointercancel", release, true);
                                        window.removeEventListener("blur", blur);
                                      };
                                      window.addEventListener("pointerup", release, true);
                                      window.addEventListener("pointercancel", release, true);
                                      window.addEventListener("blur", blur);
                                    };
                                    onCleanup(() => close(interaction));
                                    return (
                                      <div class="plugin-param-row">
                                        <span class="plugin-param-name" {...useTooltip(() => i18n.t("preset.parameterId", { id: param.id }))}>{param.name}</span>
                                        <input
                                          class="plugin-param-slider-track"
                                          aria-label={i18n.t("preset.automate", { name: param.name })}
                                          type="range"
                                          min={param.min}
                                          max={param.max}
                                          step={param.flags & 1 ? 1 : Math.max((param.max - param.min) / 1000, 0.000001)}
                                          value={directDraft(param) ?? param.current}
                                          style={{ "accent-color": "var(--accent)" }}
                                          onPointerDown={start}
                                          onKeyDown={() => { pointerSequence = false; }}
                                          onInput={(event) => {
                                            if (!interaction && !pointerSequence) interaction = beginParamGesture(param);
                                            if (interaction) updateParamGesture(interaction, param, Number(event.currentTarget.value));
                                          }}
                                          onChange={() => close(interaction)}
                                          onBlur={() => close(interaction)}
                                        />
                                        <span class="plugin-param-display">{directDraft(param)?.toFixed(4) ?? param.display}</span>
                                      </div>
                                    );
                                  }}
                                </For>
                              </div>
                            </Show>
                          </div>
                        );
                      }}
                    </For>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>
      </Show>


      <Show when={effective()?.plugin === "builtin.open_synth" && p().name && `${store.documentIdentity().generation}\u0000${p().name}`} keyed>
        {(owner) => {
          const name = owner.slice(owner.indexOf("\u0000") + 1);
          return <OpenSynthEditor
          preset={effective() ?? p()}
          authoredEffects={p().effects}
          setParam={setParam}
          setParams={setParams}
          macroLearn={(macroIndex, control, token) => store.macroLearn(name, macroIndex, control, token)}
          loadPatch={(target, label, params, effects, token) => store.loadSynthPatch(target, label, params, effects, token)}
          noteOn={(note) => store.openSynthNoteOn(name, note)}
          noteOff={(note) => store.openSynthNoteOff(name, note)}
          auditionPatch={(label, params, effects) => store.auditionFactoryPatch(label, params, effects)}
          stopAudition={() => store.stopAudition()}
          auditioning={() => (store.auditionActive() ? store.audition()?.preset ?? null : null)}
          auditionStatus={() => store.audition()?.status ?? null}
          auditionPaused={() => store.audition()?.paused === true}
          comparePatch={(a, b) => store.compareFactoryPatches(a, b)}
          switchAb={() => store.switchAb()}
          abPair={store.abPair}
          abBusy={store.abBusy}
          abError={store.abError}
          clearAbPair={store.clearAbPair}
          libraryPatches={libraryPatchInfos}
          libraryWarnings={() => store.libraryWarnings()}
          refreshLibrary={() => store.refreshLibrary()}
          patchActions={{
            rename: async (newName) => {
              const error = await store.renamePreset(name, newName);
              if (!error) props.onSelect?.(newName);
              return error;
            },
            duplicate: async (newName) => {
              const error = await store.duplicatePreset(name, newName, true);
              if (!error) props.onSelect?.(newName);
              return error;
            },
            saveAs: async (newName) => {
              const error = await store.duplicatePreset(name, newName, true);
              if (!error) props.onSelect?.(newName);
              return error;
            },
            saveToLibrary: (libraryName, category) =>
              store.savePatchToLibrary(name, libraryName, category, true),
            randomize: (token) => store.randomizePreset(name, token),
          }}
        />;
        }}
      </Show>
      <Show when={guiOpen() && p().source === "vst3"}>
        <HostedAudition preset={p().name} />
      </Show>

      <Show when={p().file}>
        <div class="preset-detail-section">
          <div class="preset-detail-label">{i18n.t("preset.sampleFile")}</div>
          <div class="preset-detail-value">{p().file}</div>
        </div>
      </Show>

      <details class="preset-advanced">
      <summary>{i18n.t("preset.advanced")}</summary>
      <div class="preset-detail-section preset-profile-grid">
        <div class="preset-detail-value" style={{ "margin-bottom": "8px" }}>
          {i18n.t("preset.advancedHint")}
        </div>
        <Show when={profileError()}>
          <div role="alert" class="selectable" style={{ color: "var(--stop-red)", "font-size": "var(--font-size-xs)", "margin-bottom": "8px" }}>
            {profileError()}
          </div>
        </Show>

        <Show when={isHostedPlugin()}>
        <details class="profile-group" open>
          <summary>{i18n.t("preset.pluginState")}</summary>
          <div class="profile-field">
            <Show when={["vst3", "vst2", "clap", "au", "lv2"].includes(p().source ?? "")}>
              <label class="profile-label">{i18n.t("preset.pluginStateFile")}</label>
            </Show>
            <Show when={p().source === "vst3"}>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto auto auto auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.stateFile", { format: "VST3" })}
                  class="text-input"
                  value={getPresetParam(p(), "vst3_state_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("vst3_state_file", e.currentTarget.value)}
                />
                <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("vst3_state_file", ["vstpreset", "bin"])}>{i18n.t("preset.browse")}</button>
                <button aria-label={i18n.t("preset.saveState", { format: "VST3" })} class="toolbar-btn profile-action" onClick={() => void saveVst3StateFile()}>{i18n.t("dialog.save")}</button>
                <button aria-label={i18n.t("preset.embedState", { format: "VST3" })} class="toolbar-btn profile-action" onClick={() => void saveVst3StateEmbedded()}>{i18n.t("preset.embed")}</button>
                <button aria-label={i18n.t("preset.importFfpLabel")} class="toolbar-btn profile-action" onClick={() => void importFfpPreset()}>{i18n.t("preset.importFfp")}</button>
              </div>
              <Show when={getPresetParam(p(), "vst3_state_hex")}>
                <div class="preset-detail-value" style={{ "font-size": "var(--font-size-xs)" }}>
                  {i18n.t("preset.embeddedState", { format: "VST3" })}
                </div>
              </Show>
              <label class="profile-label">{i18n.t("preset.presetFile")}</label>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.presetFile")}
                  class="text-input"
                  value={getPresetParam(p(), "serum_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("serum_file", e.currentTarget.value)}
                />
                <button
                  class="toolbar-btn"
                  style={{ width: "auto", padding: "0 8px" }}
                  onClick={() => void browsePresetFile("serum_file", ["SerumPreset"])}
                >{i18n.t("preset.browse")}</button>
              </div>
            </Show>
            <Show when={p().source === "vst2"}>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto auto auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.stateFile", { format: "VST2" })}
                  class="text-input"
                  value={getPresetParam(p(), "vst2_state_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("vst2_state_file", e.currentTarget.value)}
                />
                <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("vst2_state_file", ["vst2state", "bin"])}>{i18n.t("preset.browse")}</button>
                <button aria-label={i18n.t("preset.saveState", { format: "VST2" })} class="toolbar-btn profile-action" onClick={() => void saveVst2StateFile()}>{i18n.t("dialog.save")}</button>
                <button aria-label={i18n.t("preset.embedState", { format: "VST2" })} class="toolbar-btn profile-action" onClick={() => void saveVst2StateEmbedded()}>{i18n.t("preset.embed")}</button>
              </div>
              <Show when={getPresetParam(p(), "vst2_state_hex")}>
                <div class="preset-detail-value" style={{ "font-size": "var(--font-size-xs)" }}>
                  {i18n.t("preset.embeddedState", { format: "VST2" })}
                </div>
              </Show>
            </Show>
            <Show when={p().source === "clap"}>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto auto auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.stateFile", { format: "CLAP" })}
                  class="text-input"
                  value={getPresetParam(p(), "clap_state_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("clap_state_file", e.currentTarget.value)}
                />
                <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("clap_state_file", ["clapstate", "bin"])}>{i18n.t("preset.browse")}</button>
                <button aria-label={i18n.t("preset.saveState", { format: "CLAP" })} class="toolbar-btn profile-action" onClick={() => void saveClapStateFile()}>{i18n.t("dialog.save")}</button>
                <button aria-label={i18n.t("preset.embedState", { format: "CLAP" })} class="toolbar-btn profile-action" onClick={() => void saveClapStateEmbedded()}>{i18n.t("preset.embed")}</button>
              </div>
              <Show when={getPresetParam(p(), "clap_state_hex")}>
                <div class="preset-detail-value" style={{ "font-size": "var(--font-size-xs)" }}>
                  {i18n.t("preset.embeddedState", { format: "CLAP" })}
                </div>
              </Show>
            </Show>
            <Show when={p().source === "au"}>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto auto auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.stateFile", { format: "AU" })}
                  class="text-input"
                  value={getPresetParam(p(), "au_state_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("au_state_file", e.currentTarget.value)}
                />
                <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("au_state_file", ["aupreset", "bin"])}>{i18n.t("preset.browse")}</button>
                <button aria-label={i18n.t("preset.saveState", { format: "AU" })} class="toolbar-btn profile-action" onClick={() => void saveHostedStateFile("AU", ["aupreset", "bin"])}>{i18n.t("dialog.save")}</button>
                <button aria-label={i18n.t("preset.embedState", { format: "AU" })} class="toolbar-btn profile-action" onClick={() => void saveHostedStateEmbedded()}>{i18n.t("preset.embed")}</button>
              </div>
              <Show when={getPresetParam(p(), "au_state_hex")}>
                <div class="preset-detail-value" style={{ "font-size": "var(--font-size-xs)" }}>
                  {i18n.t("preset.embeddedState", { format: "AU" })}
                </div>
              </Show>
            </Show>
            <Show when={p().source === "lv2"}>
              <div class="profile-row" style={{ "grid-template-columns": "1fr auto auto auto", gap: "6px" }}>
                <input
                  aria-label={i18n.t("preset.stateFile", { format: "LV2" })}
                  class="text-input"
                  value={getPresetParam(p(), "lv2_state_file")}
                  placeholder={i18n.t("preset.chooseFile")}
                  onChange={(e) => void setParam("lv2_state_file", e.currentTarget.value)}
                />
                <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("lv2_state_file", ["lv2state", "bin"])}>{i18n.t("preset.browse")}</button>
                <button aria-label={i18n.t("preset.saveState", { format: "LV2" })} class="toolbar-btn profile-action" onClick={() => void saveHostedStateFile("LV2", ["lv2state", "bin"])}>{i18n.t("dialog.save")}</button>
                <button aria-label={i18n.t("preset.embedState", { format: "LV2" })} class="toolbar-btn profile-action" onClick={() => void saveHostedStateEmbedded()}>{i18n.t("preset.embed")}</button>
              </div>
              <Show when={getPresetParam(p(), "lv2_state_hex")}>
                <div class="preset-detail-value" style={{ "font-size": "var(--font-size-xs)" }}>
                  {i18n.t("preset.embeddedState", { format: "LV2" })}
                </div>
              </Show>
            </Show>
          </div>
        </details>
        </Show>

        <details class="profile-group" open>
          <summary>{i18n.t("preset.controllerMapping")}</summary>
        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.ccMapping")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "64px 1fr auto", gap: "6px" }}>
            <input
              aria-label={i18n.t("preset.ccNumber")}
              class="text-input"
              value={ccNumber()}
              onInput={(e) => setCcNumber(e.currentTarget.value)}
            />
            <input
              aria-label={i18n.t("preset.pluginParameter")}
              class="text-input"
              value={ccParam()}
              placeholder={i18n.t("preset.pluginParameter")}
              onInput={(e) => setCcParam(e.currentTarget.value)}
            />
            <button aria-label={i18n.t("preset.mapCc")} class="toolbar-btn profile-action" onClick={addCcMapping}>{i18n.t("preset.map")}</button>
          </div>
          <Show when={pluginParams().length > 0}>
            <select
              aria-label={i18n.t("preset.parameterPicker")}
              class="text-input"
              value=""
              onChange={(e) => setCcParam(e.currentTarget.value)}
            >
              <option value="" disabled>{i18n.t("preset.pickParameter")}</option>
              <For each={pluginParams()}>
                {(param) => <option value={param.name}>{param.name}</option>}
              </For>
            </select>
          </Show>
        </div>

        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.noteExpressionMapping")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 1fr auto", gap: "6px" }}>
            <input
              aria-label={i18n.t("preset.expressionControl")}
              class="text-input"
              value={expressionControl()}
              placeholder="pressure"
              onInput={(e) => setExpressionControl(e.currentTarget.value)}
            />
            <Dropdown
              label={i18n.t("preset.expressionType")}
              hideLabel
              class="preset-expression-type"
              value={expressionType()}
              options={["pressure", "expression", "brightness", "tuning", "volume", "pan", "vibrato"]
                .map((type) => ({ value: type, label: i18n.t(`preset.expression.${type}` as PresetCopyKey) }))}
              onSelect={(next) => setExpressionType(next)}
            />
            <button aria-label={i18n.t("preset.mapExpression")} class="toolbar-btn profile-action" onClick={addExpressionMapping}>{i18n.t("preset.map")}</button>
          </div>
        </div>

        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.cc14Mapping")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 96px auto", gap: "6px" }}>
            <input
              aria-label={i18n.t("preset.cc14Control")}
              class="text-input"
              value={cc14Control()}
              placeholder="dynamics"
              onInput={(e) => setCc14Control(e.currentTarget.value)}
            />
            <input
              aria-label={i18n.t("preset.cc14Number")}
              class="text-input"
              value={cc14Number()}
              placeholder="1"
              onInput={(e) => setCc14Number(e.currentTarget.value)}
            />
            <button aria-label={i18n.t("preset.mapCc14")} class="toolbar-btn profile-action" onClick={addCc14Mapping}>{i18n.t("preset.map")}</button>
          </div>
        </div>
        </details>

        <details class="profile-group" open>
          <summary>{i18n.t("preset.drumsNoises")}</summary>
        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.drumChoke")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 80px 96px auto", gap: "6px" }}>
            <input aria-label={i18n.t("preset.drumName")} class="text-input" value={drumName()} onInput={(e) => setDrumName(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.drumNote")} class="text-input" value={drumNote()} placeholder="42" onInput={(e) => setDrumNote(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.chokeGroup")} class="text-input" value={drumChoke()} placeholder="hihat" onInput={(e) => setDrumChoke(e.currentTarget.value)} />
            <button aria-label={i18n.t("preset.mapDrum")} class="toolbar-btn profile-action" onClick={addDrumMapping}>{i18n.t("preset.map")}</button>
          </div>
        </div>

        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.noiseTrigger")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 72px 72px", gap: "6px" }}>
            <input aria-label={i18n.t("preset.noiseControl")} class="text-input" value={noiseControl()} onInput={(e) => setNoiseControl(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.noiseOn")} class="text-input" value={noiseOnNote()} placeholder="24" onInput={(e) => setNoiseOnNote(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.noiseOff")} class="text-input" value={noiseOffNote()} placeholder="25" onInput={(e) => setNoiseOffNote(e.currentTarget.value)} />
          </div>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 1fr 1fr auto", gap: "6px" }}>
            <input aria-label={i18n.t("preset.noiseVelocity")} class="text-input" value={noiseVelocity()} onInput={(e) => setNoiseVelocity(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.noiseDurationMs")} class="text-input" value={noiseDurationMs()} onInput={(e) => setNoiseDurationMs(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.noiseThreshold")} class="text-input" value={noiseThreshold()} onInput={(e) => setNoiseThreshold(e.currentTarget.value)} />
            <button aria-label={i18n.t("preset.mapNoise")} class="toolbar-btn profile-action" onClick={addNoiseMapping}>{i18n.t("preset.map")}</button>
          </div>
        </div>

        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.releaseTrigger")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 72px 72px 72px", gap: "6px" }}>
            <input aria-label={i18n.t("preset.releaseArticulation")} class="text-input" value={releaseArticulation()} onInput={(e) => setReleaseArticulation(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.releaseNote")} class="text-input" value={releaseNote()} placeholder="D0" onInput={(e) => setReleaseNote(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.releaseVelocity")} class="text-input" value={releaseVelocity()} onInput={(e) => setReleaseVelocity(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.releaseDurationMs")} class="text-input" value={releaseDurationMs()} onInput={(e) => setReleaseDurationMs(e.currentTarget.value)} />
          </div>
          <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
            <label class="profile-label" style={{ display: "flex", "align-items": "center", gap: "6px" }}>
              <input
                aria-label={i18n.t("preset.sustainAware")}
                type="checkbox"
                checked={releaseSustainAware()}
                onChange={(e) => setReleaseSustainAware(e.currentTarget.checked)}
              />
              {i18n.t("preset.sustainRelease")}
            </label>
            <button aria-label={i18n.t("preset.mapRelease")} class="toolbar-btn profile-action" onClick={addReleaseTrigger}>{i18n.t("preset.map")}</button>
          </div>
        </div>
        </details>

        <details class="profile-group" open>
          <summary>{i18n.t("preset.tuning")}</summary>
        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.microtuningOffset")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 1fr auto", gap: "6px" }}>
            <input
              aria-label={i18n.t("preset.microtuningNote")}
              class="text-input"
              value={microtuningNote()}
              placeholder={i18n.t("preset.noteExample")}
              onInput={(e) => setMicrotuningNote(e.currentTarget.value)}
            />
            <input
              aria-label={i18n.t("preset.microtuningCents")}
              class="text-input"
              value={microtuningCents()}
              placeholder="-12.5"
              onInput={(e) => setMicrotuningCents(e.currentTarget.value)}
            />
            <button aria-label={i18n.t("preset.mapMicrotuning")} class="toolbar-btn profile-action" onClick={addMicrotuning}>{i18n.t("preset.map")}</button>
          </div>
        </div>

        <div class="profile-field">
          <label class="profile-label">{i18n.t("preset.scalaFile")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 96px auto auto", gap: "6px" }}>
            <input
              aria-label={i18n.t("preset.scalaFile")}
              class="text-input"
              value={scalaFile()}
              placeholder={i18n.t("preset.chooseFile")}
              onInput={(e) => setScalaFile(e.currentTarget.value)}
            />
            <input
              aria-label={i18n.t("preset.scalaRoot")}
              class="text-input"
              value={scalaRootNote()}
              placeholder="C4"
              onInput={(e) => setScalaRootNote(e.currentTarget.value)}
            />
            <button class="toolbar-btn profile-action" onClick={() => browsePresetFile("scala_file", ["scl"], setScalaFile)}>{i18n.t("preset.browse")}</button>
            <button aria-label={i18n.t("preset.mapScala")} class="toolbar-btn profile-action" onClick={addScalaTuning}>{i18n.t("preset.map")}</button>
          </div>
        </div>
        </details>

        <details class="profile-group preset-custom-params">
          <summary>{i18n.t("preset.customParameters")}</summary>
        <div class="profile-field profile-field-first">
          <label class="profile-label">{i18n.t("preset.customParameter")}</label>
          <div class="profile-row" style={{ "grid-template-columns": "1fr 1fr auto", gap: "6px" }}>
            <input aria-label={i18n.t("preset.customName")} class="text-input" value={customParamName()} placeholder={i18n.t("preset.paramName")} onInput={(e) => setCustomParamName(e.currentTarget.value)} />
            <input aria-label={i18n.t("preset.customValue")} class="text-input" value={customParamValue()} placeholder={i18n.t("preset.value")} onInput={(e) => setCustomParamValue(e.currentTarget.value)} />
            <button aria-label={i18n.t("preset.setCustom")} class="toolbar-btn profile-action" onClick={() => customParamName().trim() && void setParam(customParamName(), customParamValue())}>{i18n.t("preset.set")}</button>
          </div>
        </div>

        <Show when={p().params.length > 0}>
          <div style={{ display: "grid", gap: "4px", "margin-top": "10px" }}>
            <For each={p().params}>
              {([key, value]) => (
                <div class="profile-row" style={{ "grid-template-columns": "1fr 1fr auto auto", gap: "6px", "align-items": "center" }}>
                  <input aria-label={i18n.t("preset.parameterName", { name: key })} class="text-input" value={key} {...useTooltip(() => key)} />
                  <input aria-label={i18n.t("preset.parameterValue", { name: key })} class="text-input" value={paramValueText(value)} {...useTooltip(() => paramValueText(value))} />
                  <button
                    aria-label={i18n.t("preset.applyParameter", { name: key })}
                    class="toolbar-btn"
                    style={{ width: "auto", padding: "0 8px" }}
                    onClick={(e) => {
                      const inputs = e.currentTarget.parentElement?.querySelectorAll("input") ?? [];
                      void editParam(
                        key,
                        (inputs[0] as HTMLInputElement | undefined)?.value ?? key,
                        (inputs[1] as HTMLInputElement | undefined)?.value ?? "",
                      );
                    }}
                  >{i18n.t("preset.apply")}</button>
                  <button class="toolbar-btn profile-action" onClick={() => void setParam(key, "")}>{i18n.t("preset.remove")}</button>
                </div>
              )}
            </For>
          </div>
        </Show>
        </details>
      </div>

      {/* Effects chain */}
      </details>
      <EffectsRack target={`preset:${p().name}`} effects={p().effects} label={i18n.t("preset.effectChain")} />

      {/* Usage */}
      <div class="preset-detail-section">
        <div class="preset-detail-label">{i18n.t("preset.usedBy")}</div>
        <Show when={usedBy().length > 0} fallback={
          <div class="preset-detail-value" style={{ "font-style": "italic" }}>{i18n.t("preset.unused")}</div>
        }>
          <For each={usedBy()}>
            {(trackName) => <div class="preset-detail-value">{trackName}</div>}
          </For>
        </Show>
      </div>
      </div>
    </div>
  );
}
