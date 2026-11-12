/**
 * Per-preset cache for the hosted plugin's parameter panel.
 *
 * Opening the preset detail pane must stay instant, so a hosted preset's
 * parameters are loaded once and reused until its state or the project changes.
 */
import type { PluginParamInfo } from "./api";
import type { ParamValue, PresetDef } from "../types/project";

export type CachedPluginPanel =
  | { ok: true; params: PluginParamInfo[]; hasGui: boolean; editorBlocked: string | null }
  | { ok: false; error: string };

interface CacheEntry {
  fingerprint: number;
  panel: CachedPluginPanel;
}

const cache = new Map<string, CacheEntry>();

function valueText(value: ParamValue): string {
  if ("Number" in value) return `n${value.Number}`;
  if ("Db" in value) return `d${value.Db}`;
  if ("Percentage" in value) return `p${value.Percentage}`;
  if ("Frequency" in value) return `f${value.Frequency}`;
  if ("Cents" in value) return `c${value.Cents}`;
  if ("Time" in value) return `t${value.Time}`;
  if ("String" in value) return `s${value.String}`;
  if ("Bool" in value) return `b${value.Bool}`;
  if ("Array" in value) return `a${value.Array.join(",")}`;
  return "?";
}

/** FNV-1a over the preset's identity and its parameter/state entries. */
function fingerprint(preset: PresetDef): number {
  let hash = 0x811c9dc5;
  const feed = (text: string) => {
    for (let index = 0; index < text.length; index += 1) {
      hash ^= text.charCodeAt(index);
      hash = Math.imul(hash, 0x01000193);
    }
    hash ^= 0x1f;
    hash = Math.imul(hash, 0x01000193);
  };
  feed(preset.source ?? "");
  feed(preset.plugin ?? "");
  feed(preset.file ?? "");
  feed(String(preset.program ?? ""));
  for (const [name, value] of preset.params) {
    feed(name);
    feed(valueText(value));
  }
  return hash >>> 0;
}

/** The cached panel for this preset, or null once its state has moved on. */
export function readCachedPluginPanel(preset: PresetDef): CachedPluginPanel | null {
  const entry = cache.get(preset.name);
  if (!entry) return null;
  if (entry.fingerprint !== fingerprint(preset)) {
    cache.delete(preset.name);
    return null;
  }
  return entry.panel;
}

export function writeCachedPluginPanel(preset: PresetDef, panel: CachedPluginPanel): void {
  cache.set(preset.name, { fingerprint: fingerprint(preset), panel });
}

/** Drop one preset's entry, or the whole cache when no name is given. */
export function invalidatePluginPanelCache(presetName?: string): void {
  if (presetName === undefined) cache.clear();
  else cache.delete(presetName);
}

/** Test hook: the number of presets currently cached. */
export function pluginPanelCacheSize(): number {
  return cache.size;
}
