/**
 * PresetPicker — macOS Launchpad-style grid for choosing a new preset.
 *
 * Flow:
 * 1. Shows category grid (Piano, Guitar, Bass, Synth, etc.)
 *    + "Recent" row at top with recently-used sources
 * 2. Click category → shows instrument grid (GM programs + CLAP plugins)
 * 3. Click instrument → creates preset with auto-name → done
 */
import { For, Show, createSignal, createMemo, onMount } from "solid-js";
import { GM_PROGRAMS } from "../data/gmPrograms.gen";
import { i18n } from "../stores/i18n";
import { presetCategoryLabel, presetProgramLabel } from "../stores/presetNames";
import * as api from "../stores/api";
import type { PluginInfo } from "../stores/api";
import { useTooltip } from "./Tooltip";

// ============================================================================
// Category/instrument data
// ============================================================================

/** A pickable sound source — either a GM program or a plugin */
export interface SoundSource {
  id: string;            // unique key: "gm:0" or "clap:com.surge-synth-team.surge-xt"
  name: string;          // stable picker name used by recent items and callbacks
  category: string;      // Piano, Guitar, Bass, Synth, etc.
  kind: "soundfont" | "clap" | "vst3" | "vst2" | "lv2" | "builtin";
  program?: number;      // GM program number (for soundfont)
  pluginId?: string;     // CLAP plugin ID
  pluginVendor?: string; // CLAP vendor name
  icon: string;          // emoji/icon for the tile
}

/** Category metadata — display order + icons */
const CATEGORIES: { id: string; icon: string }[] = [
  { id: "Piano",       icon: "\u{1F3B9}" },
  { id: "Guitar",      icon: "\u{1F3B8}" },
  { id: "Bass",        icon: "\u{1F3B8}" },
  { id: "Strings",     icon: "\u{1F3BB}" },
  { id: "Brass",       icon: "\u{1F3BA}" },
  { id: "Woodwinds",   icon: "\u{1F3B7}" },
  { id: "Organ",       icon: "\u{1F3B9}" },
  { id: "Synth",       icon: "\u{1F50A}" },
  { id: "Pad",         icon: "\u{2728}"  },
  { id: "Ensemble",    icon: "\u{1F3B6}" },
  { id: "Chromatic",   icon: "\u{1F514}" },
  { id: "SynthFX",    icon: "\u{1F30A}" },
  { id: "World",       icon: "\u{1F3B6}" },
  { id: "Percussion",  icon: "\u{1F941}" },
  { id: "SFX",         icon: "\u{1F4A5}" },
  { id: "Plugin",     icon: "\u{1F50C}" },
];

/** Program identities and names come from the Rust-owned GM authority. */
const GM_FULL: SoundSource[] = GM_PROGRAMS.map((program) => ({
  id: `gm:${program.program}`,
  name: program.pickerName,
  category: program.category,
  kind: "soundfont",
  program: program.program,
  icon: CATEGORIES.find((category) => category.id === program.category)!.icon,
}));

// ============================================================================
// Recent presets — persisted in localStorage
// ============================================================================

const sourceLabel = (source: SoundSource): string =>
  source.kind === "soundfont" && source.program !== undefined ? presetProgramLabel(source.program) : source.name;

const RECENT_KEY = "opencompose_recent_presets";
const MAX_RECENT = 12;

function getRecent(): SoundSource[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    if (!raw) return [];
    return JSON.parse(raw) as SoundSource[];
  } catch { return []; }
}

function pushRecent(source: SoundSource) {
  try {
    const list = getRecent().filter((s) => s.id !== source.id);
    list.unshift(source);
    if (list.length > MAX_RECENT) list.length = MAX_RECENT;
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch (_) {}
}

// ============================================================================
// Component
// ============================================================================

interface PresetPickerProps {
  onPick: (source: SoundSource, autoName: string) => void;
  onCancel: () => void;
  existingNames: string[];
}

export function PresetPicker(props: PresetPickerProps) {
  const [selectedCategory, setSelectedCategory] = createSignal<string | null>(null);
  const [clapPlugins, setClapPlugins] = createSignal<PluginInfo[]>([]);
  const [search, setSearch] = createSignal("");
  const [recent, setRecent] = createSignal<SoundSource[]>(getRecent());
  const [scanning, setScanning] = createSignal(false);
  const [scanError, setScanError] = createSignal<string | null>(null);

  const doScan = async (rescan = false) => {
    if (scanning()) return;
    setScanning(true);
    setScanError(null);
    try {
      const plugins = await api.scanPlugins(rescan);
      setClapPlugins(plugins.filter((p) => p.category === "Instrument"));
    } catch (e) { setScanError(String(e)); }
    setScanning(false);
  };

  onMount(() => {
    doScan();
  });

  /** All sound sources = GM + scanned plugins (CLAP preferred over VST3) */
  const allSources = createMemo((): SoundSource[] => {
    const sources = [...GM_FULL];
    const seen = new Set<string>(); // deduplicate by name
    // Sort: CLAP first, then Internal, then VST3 — so CLAP takes priority
    const sorted = [...clapPlugins()].sort((a, b) => {
      const order: Record<string, number> = { Clap: 0, Internal: 1, Vst3: 2, Vst2: 3, Lv2: 4 };
      return (order[a.format] ?? 9) - (order[b.format] ?? 9);
    });
    for (const p of sorted) {
      if (seen.has(p.name)) continue; // skip duplicate (VST3 version of a CLAP plugin)
      seen.add(p.name);
      const isBuiltin = p.format === "Internal";
      const isVst3 = p.format === "Vst3";
      const isVst2 = p.format === "Vst2";
      const isLv2 = p.format === "Lv2";
      sources.push({
        id: isBuiltin ? `builtin:${p.id}` : isVst3 ? `vst3:${p.id}` : isVst2 ? `vst2:${p.id}` : isLv2 ? `lv2:${p.id}` : `clap:${p.id}`,
        name: p.name,
        category: "Plugin",
        kind: isBuiltin ? "builtin" : isVst3 ? "vst3" : isVst2 ? "vst2" : isLv2 ? "lv2" : "clap",
        pluginId: p.id,
        pluginVendor: p.vendor,
        icon: isBuiltin ? "\u{1F527}" : "\u{1F50C}",
      });
    }
    return sources;
  });

  /** Sources filtered to current category */
  const categoryItems = createMemo(() => {
    const cat = selectedCategory();
    if (!cat) return [];
    return allSources().filter((s) => s.category === cat);
  });

  /** Category tiles with count */
  const categoryTiles = createMemo(() =>
    CATEGORIES.map((cat) => ({
      ...cat,
      count: allSources().filter((s) => s.category === cat.id).length,
    })).filter((c) => c.count > 0)
  );

  /** Search results */
  const searchResults = createMemo(() => {
    const q = search().toLowerCase().trim();
    if (!q) return [];
    return allSources().filter((s) =>
      sourceLabel(s).toLowerCase().includes(q) ||
      presetCategoryLabel(s.category).toLowerCase().includes(q) ||
      presetCategoryLabel(s.category, "en").toLowerCase().includes(q) ||
      (s.kind === "soundfont" && s.program !== undefined && presetProgramLabel(s.program, "en").toLowerCase().includes(q)) ||
      s.id.toLowerCase().includes(q) ||
      s.name.toLowerCase().includes(q) ||
      s.category.toLowerCase().includes(q) ||
      (s.pluginVendor?.toLowerCase().includes(q) ?? false)
    ).slice(0, 30);
  });

  /** Auto-generate preset name: "Piano 1", "Piano 2", etc */
  const autoName = (source: SoundSource): string => {
    const base = source.kind === "soundfont" ? source.category : source.name;
    let n = 1;
    while (props.existingNames.includes(`${base} ${n}`)) n++;
    return `${base} ${n}`;
  };

  const handlePick = (source: SoundSource) => {
    pushRecent(source);
    setRecent(getRecent());
    const name = autoName(source);
    props.onPick(source, name);
  };

  const handleBack = () => {
    if (selectedCategory()) {
      setSelectedCategory(null);
    } else {
      props.onCancel();
    }
  };

  return (
    <div class="preset-picker">
      {/* Header with search */}
      <div class="preset-picker-header">
        <Show when={selectedCategory()}>
          <button type="button" class="preset-picker-back" aria-label={i18n.t("preset.back")} onClick={handleBack}>&larr;</button>
        </Show>
        <span class="preset-picker-title">
          {selectedCategory() ? presetCategoryLabel(selectedCategory()!) : i18n.t("preset.newPreset")}
        </span>
        <button
          type="button"
          class="preset-picker-back"
          aria-label={i18n.t("preset.rescan")}
          style={{ "font-size": "10px", "margin-right": "4px", width: "24px", height: "24px" }}
          onClick={() => void doScan(true)}
          disabled={scanning()}
          {...useTooltip(() => i18n.t("preset.rescan"))}
        >{scanning() ? "\u23F3" : "\u21BB"}</button>
        <input
          class="preset-picker-search"
          aria-label={i18n.t("preset.search")}
          placeholder={i18n.t("preset.search")}
          value={search()}
          onInput={(e) => setSearch(e.currentTarget.value)}
        />
      </div>

      {/* Search results overlay */}
      <Show when={scanError()}>
        {(error) => <div role="alert" class="preset-picker-scan-error selectable">{i18n.t("plugin.scanFailed", { error: error() })}</div>}
      </Show>
      <Show when={search().trim()}>
        <div class="preset-picker-grid">
          <For each={searchResults()}>
            {(s) => (
              <button type="button" class="preset-picker-item" onClick={() => handlePick(s)}>
                <span class="preset-picker-item-icon">{s.icon}</span>
                <span class="preset-picker-item-name">{sourceLabel(s)}</span>
                <span class="preset-picker-item-meta">
                  {s.kind === "soundfont" ? presetCategoryLabel(s.category) : s.pluginVendor ?? presetCategoryLabel(s.category)}
                </span>
              </button>
            )}
          </For>
          <Show when={searchResults().length === 0}>
            <div class="preset-picker-empty">{i18n.t("preset.noMatches")}</div>
          </Show>
        </div>
      </Show>

      {/* Category grid (layer 1) */}
      <Show when={!selectedCategory() && !search().trim()}>
        {/* Recent row */}
        <Show when={recent().length > 0}>
          <div class="preset-picker-section-label">{i18n.t("preset.recent")}</div>
          <div class="preset-picker-grid compact">
            <For each={recent()}>
              {(s) => (
                <button type="button" class="preset-picker-item compact" onClick={() => handlePick(s)}>
                  <span class="preset-picker-item-icon">{s.icon}</span>
                  <span class="preset-picker-item-name">{sourceLabel(s)}</span>
                </button>
              )}
            </For>
          </div>
        </Show>

        <div class="preset-picker-section-label">{i18n.t("preset.categories")}</div>
        <div class="preset-picker-categories">
          <For each={categoryTiles()}>
            {(cat) => (
              <button type="button" class="preset-picker-category" onClick={() => setSelectedCategory(cat.id)}>
                <span class="preset-picker-category-icon">{cat.icon}</span>
                <span class="preset-picker-category-name">{presetCategoryLabel(cat.id)}</span>
                <span class="preset-picker-category-count">{cat.count}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      {/* Instrument grid (layer 2) */}
      <Show when={selectedCategory() && !search().trim()}>
        <div class="preset-picker-grid">
          <For each={categoryItems()}>
            {(s) => (
              <button type="button" class="preset-picker-item" onClick={() => handlePick(s)}>
                <span class="preset-picker-item-icon">{s.icon}</span>
                <span class="preset-picker-item-name">{sourceLabel(s)}</span>
                <span class="preset-picker-item-meta">
                  {s.kind === "soundfont" && s.program !== undefined ? i18n.t("preset.programNumber", { number: s.program }) : s.pluginVendor ?? presetCategoryLabel(s.category)}
                </span>
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
