/**
 * Localized text for typed edit facts.
 *
 * A change carries parameters; this module owns the sentence, the separator and
 * the bar conversion. Spans are quarter-note beats, so they become bars with the
 * meter of the project the caller passes.
 */
import { BUILTIN_EFFECT_TYPES, effectLabel } from "../devices/effectNames";
import { effectLaneName } from "../devices/effectIds";
import type {
  BeatSpan, Change, ChangeSet, KeyToken, MixParam, Owner,
} from "../types/change";
import type { EffectType, ProjectProperties } from "../types/project";
import { projectClock } from "../utils/projectClock";
import { barBeatText, humanRange } from "../utils/rangeText";
import { i18n, type I18nKey } from "./i18n";

/** How many facts a summary names before the remainder becomes a count. */
export const DEFAULT_CHANGE_FACTS = 3;

export interface ChangeSetOptions {
  /** The largest number of facts to name; the rest become "more". */
  max?: number;
  /** The project's meter, so a beat span becomes a bar range. */
  properties?: ProjectProperties;
}

const DEFAULT_PROPERTIES: ProjectProperties = {
  tempo: 120,
  time_signature: { numerator: 4, denominator: 4 },
  key: null,
  swing: null,
  tempo_map: [],
  time_sig_map: [],
  key_map: [],
};

const MODE_KEYS: Record<KeyToken["mode"], I18nKey> = {
  major: "change.mode.major",
  minor: "change.mode.minor",
};

const MIX_KEYS: Record<MixParam["kind"], I18nKey> = {
  volume: "change.mix.volume",
  pan: "change.mix.pan",
  stereo_balance: "change.mix.stereo_balance",
  mute: "change.mix.mute",
  solo: "change.mix.solo",
  polarity_inverted: "change.mix.polarity_inverted",
  output: "change.mix.output",
  sends: "change.mix.sends",
  overrides: "change.mix.overrides",
  latency_compensation: "change.mix.latency_compensation",
  latency_offset: "change.mix.latency_offset",
  control_gain: "change.mix.control_gain",
  smart_disable: "change.mix.smart_disable",
  routes: "change.mix.routes",
};

/** The built-in effect a DSL token names, for its display label. */
const TOKEN_EFFECTS = new Map<string, EffectType>();
for (const type of BUILTIN_EFFECT_TYPES) {
  const token = effectLaneName(type);
  if (token) TOKEN_EFFECTS.set(token, type);
}

function numberText(value: unknown, digits = 3): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "?";
  return new Intl.NumberFormat(i18n.locale(), { maximumFractionDigits: digits }).format(value);
}

function nameText(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function keyText(token: KeyToken | null | undefined): string {
  if (!token || typeof token.root !== "string") return "";
  const mode = token.mode === "minor" ? MODE_KEYS.minor : MODE_KEYS.major;
  return i18n.t("change.keyValue", { root: token.root, mode: i18n.t(mode) });
}

function timeSignatureText(value: unknown): string {
  const signature = value as { numerator?: unknown; denominator?: unknown } | null;
  if (!signature || typeof signature.numerator !== "number" || typeof signature.denominator !== "number") return "";
  return i18n.t("change.timeSignatureValue", {
    numerator: numberText(signature.numerator, 0),
    denominator: numberText(signature.denominator, 0),
  });
}

function ownerText(owner: Owner | null | undefined): string {
  if (!owner || typeof owner !== "object") return i18n.t("change.ownerMaster");
  switch (owner.kind) {
    case "track": return i18n.t("change.ownerTrack", { name: nameText(owner.name) });
    case "bus": return i18n.t("change.ownerBus", { name: nameText(owner.name) });
    case "master": return i18n.t("change.ownerMaster");
    default: return i18n.t("change.ownerMaster");
  }
}

/** An effect token is a DSL identifier or `plugin:<id>`; unknown tokens stay raw. */
export function effectTokenText(token: string): string {
  if (token.startsWith("plugin:")) return token.slice("plugin:".length) || token;
  const type = TOKEN_EFFECTS.get(token);
  return type ? effectLabel(type) : token;
}

function mixText(params: unknown): string {
  const list = Array.isArray(params) ? params : [];
  const names = list.map((param) => {
    const kind = param && typeof param === "object" ? (param as { kind?: unknown }).kind : undefined;
    return typeof kind === "string" && kind in MIX_KEYS
      ? i18n.t(MIX_KEYS[kind as MixParam["kind"]])
      : i18n.t("change.mix.other");
  });
  return names.join(i18n.t("change.separator"));
}

function spanText(span: BeatSpan | null | undefined, options: ChangeSetOptions): string {
  if (!span || typeof span.start !== "number" || typeof span.end !== "number") return "";
  if (!Number.isFinite(span.start) || !Number.isFinite(span.end) || span.start < 0 || span.end < 0) return "";
  try {
    const clock = projectClock(options.properties ?? DEFAULT_PROPERTIES);
    return humanRange(barBeatText(clock.barBeatAt(span.start)), barBeatText(clock.barBeatAt(span.end)));
  } catch {
    return "";
  }
}

function rangeSuffix(span: BeatSpan | null | undefined, options: ChangeSetOptions): string {
  const range = spanText(span, options);
  return range ? i18n.t("change.inRange", { range }) : "";
}

function renameText(
  from: unknown,
  to: unknown,
  keys: { changed: I18nKey; named: I18nKey; cleared: I18nKey },
): string {
  const before = typeof from === "string" ? from : null;
  const after = typeof to === "string" ? to : null;
  if (before === null && after === null) return i18n.t("change.edit");
  if (before === null) return i18n.t(keys.named, { to: after ?? "" });
  if (after === null) return i18n.t(keys.cleared, { from: before });
  return i18n.t(keys.changed, { from: before, to: after });
}

function keyChangeText(from: unknown, to: unknown): string {
  const before = keyText(from as KeyToken | null);
  const after = keyText(to as KeyToken | null);
  if (!before && !after) return i18n.t("change.edit");
  if (!before) return i18n.t("change.keySet", { to: after });
  if (!after) return i18n.t("change.keyCleared", { from: before });
  return i18n.t("change.key", { from: before, to: after });
}

function swingChangeText(from: unknown, to: unknown): string {
  const before = typeof from === "number" && Number.isFinite(from) ? numberText(from, 2) : "";
  const after = typeof to === "number" && Number.isFinite(to) ? numberText(to, 2) : "";
  if (!before && !after) return i18n.t("change.edit");
  if (!before) return i18n.t("change.swingSet", { to: after });
  if (!after) return i18n.t("change.swingCleared", { from: before });
  return i18n.t("change.swing", { from: before, to: after });
}

/** One fact as a sentence in the app's language. Unknown kinds read as "Edit". */
export function changeText(change: Change, options: ChangeSetOptions = {}): string {
  if (!change || typeof change !== "object") return i18n.t("change.edit");
  const value = change as Change & { kind?: string };
  try {
    switch (value.kind) {
      case "project_renamed":
        return renameText(value.from, value.to, {
          changed: "change.projectRenamed",
          named: "change.projectNamed",
          cleared: "change.projectNameCleared",
        });
      case "project_version":
        return renameText(value.from, value.to, {
          changed: "change.projectVersion",
          named: "change.projectVersionSet",
          cleared: "change.projectVersionCleared",
        });
      case "tempo":
        return i18n.t("change.tempo", { from: numberText(value.from), to: numberText(value.to) });
      case "time_signature":
        return i18n.t("change.timeSignature", {
          from: timeSignatureText(value.from), to: timeSignatureText(value.to),
        });
      case "key":
        return keyChangeText(value.from, value.to);
      case "swing":
        return swingChangeText(value.from, value.to);
      case "tempo_map": return i18n.t("change.tempoMap");
      case "time_signature_map": return i18n.t("change.timeSignatureMap");
      case "key_map": return i18n.t("change.keyMap");
      case "export_settings": return i18n.t("change.exportSettings");
      case "control_links": return i18n.t("change.controlLinks");
      case "project_settings": return i18n.t("change.projectSettings");
      case "track_added": return i18n.t("change.trackAdded", { track: nameText(value.track) });
      case "track_removed": return i18n.t("change.trackRemoved", { track: nameText(value.track) });
      case "track_renamed":
        return i18n.t("change.trackRenamed", { from: nameText(value.from), to: nameText(value.to) });
      case "tracks_reordered": return i18n.t("change.tracksReordered");
      case "track_instrument": return i18n.t("change.trackInstrument", { track: nameText(value.track) });
      case "track_performance": return i18n.t("change.trackPerformance", { track: nameText(value.track) });
      case "track_routing": return i18n.t("change.trackRouting", { track: nameText(value.track) });
      case "track_changed": return i18n.t("change.trackChanged", { track: nameText(value.track) });
      case "track_mix":
        return i18n.t("change.trackMix", { track: nameText(value.track), params: mixText(value.params) });
      case "effect_added":
        return i18n.t("change.effectAdded", {
          effect: effectTokenText(nameText(value.effect)), owner: ownerText(value.owner),
        });
      case "effect_removed":
        return i18n.t("change.effectRemoved", {
          effect: effectTokenText(nameText(value.effect)), owner: ownerText(value.owner),
        });
      case "effect_changed":
        return i18n.t("change.effectChanged", {
          effect: effectTokenText(nameText(value.effect)), owner: ownerText(value.owner),
        });
      case "effect_bypassed":
        return i18n.t(value.bypassed ? "change.effectBypassed" : "change.effectEnabled", {
          effect: effectTokenText(nameText(value.effect)), owner: ownerText(value.owner),
        });
      case "effects_reordered":
        return i18n.t("change.effectsReordered", { owner: ownerText(value.owner) });
      case "notes_edited":
        return i18n.t("change.notesEdited", {
          track: nameText(value.track),
          added: numberText(value.added, 0),
          removed: numberText(value.removed, 0),
          changed: numberText(value.changed, 0),
          range: rangeSuffix(value.span, options),
        });
      case "clips_edited":
        return i18n.t("change.clipsEdited", {
          track: nameText(value.track), range: rangeSuffix(value.span, options),
        });
      case "automation_edited":
        return i18n.t("change.automationEdited", {
          owner: ownerText(value.owner), range: rangeSuffix(value.span, options),
        });
      case "preset_added": return i18n.t("change.presetAdded", { preset: nameText(value.preset) });
      case "preset_removed": return i18n.t("change.presetRemoved", { preset: nameText(value.preset) });
      case "preset_renamed":
        return i18n.t("change.presetRenamed", { from: nameText(value.from), to: nameText(value.to) });
      case "preset_changed": return i18n.t("change.presetChanged", { preset: nameText(value.preset) });
      case "pattern_added": return i18n.t("change.patternAdded", { pattern: nameText(value.pattern) });
      case "pattern_removed": return i18n.t("change.patternRemoved", { pattern: nameText(value.pattern) });
      case "pattern_renamed":
        return i18n.t("change.patternRenamed", { from: nameText(value.from), to: nameText(value.to) });
      case "pattern_changed":
        return i18n.t("change.patternChanged", {
          pattern: nameText(value.pattern), range: rangeSuffix(value.span, options),
        });
      case "bus_added": return i18n.t("change.busAdded", { bus: nameText(value.bus) });
      case "bus_removed": return i18n.t("change.busRemoved", { bus: nameText(value.bus) });
      case "bus_renamed":
        return i18n.t("change.busRenamed", { from: nameText(value.from), to: nameText(value.to) });
      case "bus_changed": return i18n.t("change.busChanged", { bus: nameText(value.bus) });
      case "bus_mix": return i18n.t("change.busMix", { bus: nameText(value.bus), params: mixText(value.params) });
      case "master_changed": return i18n.t("change.masterChanged");
      case "presets_reordered": return i18n.t("change.presetsReordered");
      case "patterns_reordered": return i18n.t("change.patternsReordered");
      case "buses_reordered": return i18n.t("change.busesReordered");
      case "sections_edited":
        return i18n.t("change.sectionsEdited", { range: rangeSuffix(value.span, options) });
      case "markers_edited":
        return i18n.t("change.markersEdited", {
          added: numberText(value.added, 0),
          removed: numberText(value.removed, 0),
          changed: numberText(value.changed, 0),
        });
      case "groups_edited": return i18n.t("change.groupsEdited");
      case "audio_clips_edited":
        return value.track
          ? i18n.t("change.audioClipsOnTrack", {
            track: nameText(value.track), range: rangeSuffix(value.span, options),
          })
          : i18n.t("change.audioClipsEdited", { range: rangeSuffix(value.span, options) });
      default: return i18n.t("change.edit");
    }
  } catch {
    return i18n.t("change.edit");
  }
}

/**
 * The first few facts joined for one line, plus a count for what the caller cut
 * and what the backend dropped. An empty set reads as a generic edit.
 */
export function changeSetText(set: ChangeSet, options: ChangeSetOptions = {}): string {
  const facts = Array.isArray(set?.changes) ? set.changes : [];
  const max = Math.max(1, Math.floor(options.max ?? DEFAULT_CHANGE_FACTS));
  const shown = facts.slice(0, max);
  const separator = i18n.t("change.separator");
  const extra = Math.max(0, facts.length - shown.length)
    + Math.max(0, Math.floor(Number(set?.more) || 0));
  const more = extra > 0 ? i18n.t("change.more", { count: numberText(extra, 0) }) : "";
  if (shown.length === 0) {
    const base = i18n.t("change.edit");
    return more ? `${base}${separator}${more}` : base;
  }
  const joined = shown.map((change) => changeText(change, options)).join(separator);
  return more ? `${joined}${separator}${more}` : joined;
}
