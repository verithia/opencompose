/** Short activity labels; raw arguments and results remain in the disclosure. */
import { i18n, type I18nKey } from "../stores/i18n";
import type { ToolRow } from "./transcript";
import { oneLine } from "./transcript";

export function readableName(value: string): string {
  return oneLine(value.replace(/[_-]+/g, " "), 72);
}

const ACTIVITY_KEYS: Partial<Record<string, I18nKey>> = {
  sections_list: "ai.activity.sections_list",
  sections_add: "ai.activity.sections_add",
  transaction_begin: "ai.activity.transaction_begin",
  transaction_commit: "ai.activity.transaction_commit",
  transaction_rollback: "ai.activity.transaction_rollback",
  presets_add: "ai.activity.presets_add",
  tracks_preset: "ai.activity.tracks_preset",
  audition_range: "ai.activity.audition_range",
  audition_clip_source: "ai.activity.audition_clip_source",
  audio_clip_transients: "ai.activity.audio_clip_transients",
  audio_clip_set_playback: "ai.activity.audio_clip_set_playback",
  render_range: "ai.activity.render_range",
  midi_import_handoff: "ai.activity.midi_import_handoff",
  midi_export_handoff: "ai.activity.midi_export_handoff",
  devices_catalog: "ai.activity.devices_catalog",
  presets_list: "ai.activity.presets_list",
  plugins_list: "ai.activity.plugins_list",
};

function argString(args: Record<string, unknown>, ...keys: string[]): string {
  for (const key of keys) {
    const value = args[key];
    if (typeof value === "string" && value.trim()) return oneLine(value, 64);
  }
  return "";
}

function argNumber(args: Record<string, unknown>, key: string): number | null {
  const value = args[key];
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) return Number(value);
  return null;
}

interface Position { bar: number; beat: number }

/** A `bar:beat` position; a subdivision stays in the raw bar:beat form. */
function positionOf(value: string): Position | null {
  const match = /^(\d+)(?::(\d+))?(?::(\d+))?$/.exec(value.trim());
  if (!match) return null;
  if (match[3] !== undefined && Number(match[3]) !== 0) return null;
  return { bar: Number(match[1]), beat: match[2] === undefined ? 1 : Number(match[2]) };
}

/** Two positions as bars, beats, or the raw bar:beat when neither fits. */
function humanRange(start: string, end: string): string {
  const first = positionOf(start);
  const last = positionOf(end);
  if (first && last) {
    // DSL and tool ranges end exclusively; name the last bar or beat they cover.
    if (first.beat === 1 && last.beat === 1) {
      const to = last.bar - 1;
      if (to === first.bar) return i18n.t("ai.activity.barOne", { bar: first.bar });
      if (to > first.bar) return i18n.t("ai.activity.barRange", { start: first.bar, end: to });
    }
    if (first.bar === last.bar && last.beat > first.beat) {
      const to = last.beat - 1;
      if (to === first.beat) return i18n.t("ai.activity.barBeatOne", { bar: first.bar, beat: first.beat });
      return i18n.t("ai.activity.barBeats", { bar: first.bar, from: first.beat, to });
    }
    if (first.bar !== last.bar) {
      return i18n.t("ai.activity.barBeatRange", {
        fromBar: first.bar, fromBeat: first.beat, toBar: last.bar, toBeat: last.beat,
      });
    }
  }
  return i18n.t("ai.activity.rangeSpan", { start, end });
}

/** The window a render or audition names, in bars when it uses bars. */
function rangeLabel(args: Record<string, unknown>): string {
  const startBar = argNumber(args, "start_bar");
  const endBar = argNumber(args, "end_bar");
  if (startBar !== null && endBar !== null) {
    return humanRange(`${startBar}:1`, `${endBar}:1`);
  }
  const start = args.start;
  const end = args.end;
  if ((typeof start === "number" || typeof start === "string")
    && (typeof end === "number" || typeof end === "string")) {
    const suffix = typeof start === "number" && typeof end === "number" ? "s" : "";
    return i18n.t("ai.activity.rangeSpan", { start: `${start}${suffix}`, end: `${end}${suffix}` });
  }
  return "";
}

/** A `@track` / `clip:` / `@phrase` range from a subtree replacement. */
function projectEditLabel(args: Record<string, unknown>): string {
  const dsl = typeof args.dsl === "string" ? args.dsl : "";
  const path = argString(args, "path");
  const track = /@track\s+(?:"([^"]+)"|([^\s|]+))/.exec(dsl);
  const trackPath = /^track:(.+)$/.exec(path);
  const name = track
    ? oneLine(track[1] ?? track[2], 48)
    : trackPath ? oneLine(trackPath[1], 48) : "";
  const clip = /clip:\s+(?:"([^"]+)"|([^\s\[]+))\s*\[\s*([^\]]+)\]/.exec(dsl);
  if (name && clip) {
    const [start, end] = splitRange(clip[3]);
    return i18n.t("ai.activity.editTrackClip", {
      track: name, clip: oneLine(clip[1] ?? clip[2], 48),
      range: humanRange(start, end),
    });
  }
  const phrase = /@phrase\s+(?:"([^"]+)"|([^\s\[]+))\s*\[\s*([^\]]+)\]/.exec(dsl);
  if (name && phrase) {
    const [start, end] = splitRange(phrase[3]);
    return i18n.t("ai.activity.editTrackPhrase", {
      track: name, phrase: oneLine(phrase[1] ?? phrase[2], 48),
      range: humanRange(start, end),
    });
  }
  if (name) return i18n.t("ai.activity.editTrack", { name });
  if (path) return i18n.t("ai.activity.editTrack", { name: path });
  return i18n.t("ai.activity.editProject");
}

function splitRange(value: string): [string, string] {
  const match = /^\s*(.*?)\s+-\s+(.*?)\s*$/.exec(value);
  return match ? [match[1], match[2]] : [value.trim(), ""];
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function numeric(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** The tool's result, when the caller supplied the full payload rather than its preview. */
function resultOf(result: string | undefined): Record<string, unknown> | null {
  if (!result?.trim()) return null;
  try {
    return asRecord(JSON.parse(result));
  } catch {
    return null;
  }
}

/** How many markets actually returned an item, not how many were asked. */
function sampleSites(payload: Record<string, unknown> | null): number {
  if (!payload) return 0;
  const pages = Array.isArray(payload.pages) ? payload.pages : [];
  const answering = pages.filter((page) => {
    const entry = asRecord(page);
    return entry !== null && (numeric(entry.count) ?? 0) > 0;
  }).length;
  if (answering > 0) return answering;
  const items = Array.isArray(payload.items) ? payload.items : [];
  return new Set(items.map((item) => String(asRecord(item)?.provider ?? "")).filter(Boolean)).size;
}

function sampleSearchLabel(args: Record<string, unknown>, payload: Record<string, unknown> | null): string {
  const count = numeric(payload?.count);
  if (count === null) {
    const query = argString(args, "query");
    return query
      ? i18n.t("ai.activity.searchSamples", { query })
      : i18n.t("ai.activity.sample_search");
  }
  const sites = sampleSites(payload);
  if (sites === 1) {
    return i18n.t(count === 1 ? "ai.activity.sampleResultOneSite" : "ai.activity.sampleResultsOneSite", { count });
  }
  return i18n.t(count === 1 ? "ai.activity.sampleResultOne" : "ai.activity.sampleResults", { count, sites });
}

/** The window and the number of notes a transcription reported. */
function analyzeNotesLabel(args: Record<string, unknown>, payload: Record<string, unknown> | null): string {
  const start = numeric(payload?.start_s) ?? argNumber(args, "start");
  const end = numeric(payload?.end_s) ?? argNumber(args, "end");
  const range = start !== null && end !== null
    ? i18n.t("ai.activity.rangeSpan", { start: `${start}s`, end: `${end}s` })
    : rangeLabel(args);
  const notes = payload && Array.isArray(payload.notes) ? payload.notes.length : null;
  if (notes === null || !range) return i18n.t("ai.activity.analyze_notes");
  return i18n.t(notes === 1 ? "ai.activity.analyzeNotesOne" : "ai.activity.analyzeNotes", {
    range,
    count: notes,
  });
}

/** The arrangement window a project snapshot covers, from its section list. */
function projectSectionRange(payload: Record<string, unknown> | null): string {
  const project = asRecord(payload?.project);
  const sections = project && Array.isArray(project.sections) ? project.sections : [];
  let firstBar = Infinity;
  let firstBeat = 1;
  let lastBar = -Infinity;
  let lastBeat = 1;
  for (const item of sections) {
    const section = asRecord(item);
    const time = section && asRecord(section.time_range);
    const start = time && asRecord(time.start);
    const end = time && asRecord(time.end);
    const startBar = numeric(start?.bar);
    const endBar = numeric(end?.bar);
    if (startBar === null || endBar === null) continue;
    const startBeat = numeric(start?.beat) ?? 1;
    const endBeat = numeric(end?.beat) ?? 1;
    if (startBar < firstBar || (startBar === firstBar && startBeat < firstBeat)) {
      firstBar = startBar;
      firstBeat = startBeat;
    }
    if (endBar > lastBar || (endBar === lastBar && endBeat > lastBeat)) {
      lastBar = endBar;
      lastBeat = endBeat;
    }
  }
  return Number.isFinite(firstBar) && Number.isFinite(lastBar)
    ? humanRange(`${firstBar}:${firstBeat}`, `${lastBar}:${lastBeat}`)
    : "";
}

export function activitySummary(tool: ToolRow, result?: string): string {
  let args: Record<string, unknown> = {};
  try {
    const parsed = JSON.parse(tool.args ?? "{}");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) args = parsed;
  } catch { /* Malformed arguments stay available in the expanded details. */ }
  const text = (...keys: string[]) => {
    for (const key of keys) if (typeof args[key] === "string" && String(args[key]).trim()) return readableName(String(args[key]));
    return "";
  };
  const name = (...keys: string[]) => argString(args, ...keys);
  const index = argNumber(args, "index");
  const range = rangeLabel(args);
  // The row names the skill itself; the icon says a skill was read.
  if (tool.name === "read_skill") return text("topic", "id", "skill_id", "skill") || i18n.t("ai.activity.skill");
  if (tool.name === "list_skills") return i18n.t("ai.activity.skills");
  if (["content_libraries", "soundfonts_list"].includes(tool.name)) return i18n.t("ai.activity.instruments");
  if (tool.name === "plugin_search") return i18n.t("ai.activity.search", { name: text("query") || i18n.t("ai.activity.instrumentsName") });

  // Mutations name the object they touch, so the row reads without the JSON.
  switch (tool.name) {
    case "sample_search": return sampleSearchLabel(args, resultOf(result ?? tool.result));
    case "sample_pull": return name("id")
      ? i18n.t("ai.activity.pullSample", { id: name("id") })
      : i18n.t("ai.activity.sample_pull");
    case "analyze_notes": return analyzeNotesLabel(args, resultOf(result ?? tool.result));
    case "tracks_add": return i18n.t("ai.activity.addTrack", { name: name("name") || i18n.t("ai.activity.track") });
    case "tracks_create": return i18n.t("ai.activity.createTrack", { name: name("name") || i18n.t("ai.activity.track") });
    case "tracks_remove": return i18n.t("ai.activity.removeTrack", { index: index ?? "?" });
    case "tracks_rename": return i18n.t("ai.activity.renameTrack", { index: index ?? "?", name: name("name") });
    case "tracks_duplicate": return i18n.t("ai.activity.duplicateTrack", { index: index ?? "?", name: name("name") });
    case "tracks_mix": return i18n.t("ai.activity.mixTrack", { index: index ?? "?" });
    case "tracks_mute": return i18n.t("ai.activity.muteTrack", { index: index ?? "?" });
    case "tracks_solo": return i18n.t("ai.activity.soloTrack", { index: index ?? "?" });
    case "tracks_send_add": return i18n.t("ai.activity.sendTrack", { index: index ?? "?", bus: name("bus") });
    case "tracks_preset": return i18n.t("ai.activity.setTrackPreset", { index: index ?? "?", preset: name("preset") });
    case "project_edit": return projectEditLabel(args);
    case "project_set_name": return i18n.t("ai.activity.renameProject", { name: name("name") });
    case "project_set_tempo": return i18n.t("ai.activity.setTempo", { tempo: argNumber(args, "tempo") ?? "?" });
    case "project_save": return i18n.t("ai.activity.saveProject");
    case "sections_add": return i18n.t("ai.activity.addSection", {
      name: name("name"), range: humanRange(name("start"), name("end")),
    });
    case "effects_add": return i18n.t("ai.activity.addEffect", {
      effect: name("effect_type") || i18n.t("ai.activity.effect"),
      target: name("target") || i18n.t("ai.activity.track"),
    });
    case "effects_set_bypass": {
      if (typeof args.bypass !== "boolean" || index === null) break;
      const payload = resultOf(result ?? tool.result);
      const effect = payload ? argString(payload, "effect") : "";
      return i18n.t(args.bypass ? "ai.activity.bypassEffect" : "ai.activity.enableEffect", {
        effect: effect ? readableName(effect) : i18n.t("ai.activity.effect"),
        slot: index + 1,
      });
    }
    case "buses_add": return i18n.t("ai.activity.addBus", { name: name("name") });
    case "presets_add": return i18n.t("ai.activity.addPreset", { name: name("name") });
    case "midi_import_apply": return i18n.t("ai.activity.importMidi");
    case "transport_seek": return i18n.t("ai.activity.seekBeat", { beat: argNumber(args, "beat") ?? "?" });
    case "render_range": return range ? i18n.t("ai.activity.renderRange", { range }) : i18n.t("ai.activity.render_range");
    case "audition_range": return range ? i18n.t("ai.activity.auditionRange", { range }) : i18n.t("ai.activity.audition_range");
    default: break;
  }

  const semanticKey = ACTIVITY_KEYS[tool.name];
  if (semanticKey) return i18n.t(semanticKey);
  if (tool.name === "project_get") {
    const arrangement = projectSectionRange(resultOf(result ?? tool.result));
    if (arrangement) return i18n.t("ai.activity.inspectProjectRange", { range: arrangement });
  }
  if (["project_get", "project_dsl", "project_extract", "tracks_list"].includes(tool.name)) return i18n.t("ai.activity.inspect", { name: text("path") || i18n.t("ai.activity.project") });
  return i18n.t("ai.activity.run", { name: oneLine(tool.name, 72) });
}

/** The one-line preview of a reasoning stream, with Markdown marks removed. */
export function thinkingPreview(text: string): string {
  return oneLine(
    text
      .replace(/```[\s\S]*?```/g, " ")
      .replace(/`([^`]*)`/g, "$1")
      .replace(/\*\*([^*]+)\*\*/g, "$1")
      .replace(/\*([^*]+)\*/g, "$1")
      .replace(/__([^_]+)__/g, "$1")
      .replace(/^\s{0,3}#{1,6}\s*/gm, "")
      .replace(/^\s{0,3}>\s?/gm, "")
      .replace(/^\s{0,3}[-*+]\s+/gm, "")
      .replace(/\[(.*?)\]\((.*?)\)/g, "$1"),
    88,
  );
}

/** Bounds provider text in the UI without interpreting or reconstructing it. */
export const THINKING_DISPLAY_LIMIT = 65_536;
export function appendThinking(previous: string, delta: string): string {
  const text = previous + delta;
  return text.length > THINKING_DISPLAY_LIMIT
    ? `${text.slice(0, THINKING_DISPLAY_LIMIT)}\n[${i18n.t("ai.activity.truncated")}]` : text;
}

/** Partial tool names are progress hints, never executable calls. */
export function preparingActivity(name: string): string {
  const value = name.replace(/^functions\./, "");
  const key: I18nKey = /^(edit_subtree|replace_project|dsl_|patterns_|notes_|clips_|tracks_add)/.test(value)
    ? "ai.preparing.edit"
    : /^sample_search/.test(value) ? "ai.preparing.samples"
    : /^plugin_search/.test(value) ? "ai.preparing.plugins"
    : /^(read_skill|list_skills)/.test(value) ? "ai.preparing.skills"
    : /^(audio_compare|analyze_)/.test(value) ? "ai.preparing.audio"
    : "ai.preparing.tool";
  return i18n.t(key, { tool: readableName(value) });
}

/** Recognize only a leading text-protocol call, not tool names quoted in prose. */
export function preparingToolFromText(text: string): string | null {
  return /^\s*(?:```(?:json)?\s*)?\{\s*"tool"\s*:\s*"([a-z][a-z0-9_]{0,99})"/.exec(text)?.[1] ?? null;
}
