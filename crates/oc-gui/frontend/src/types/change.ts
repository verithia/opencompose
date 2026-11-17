/**
 * Typed facts about a project edit, as the backend describes it.
 *
 * The wire carries parameters, never prose: names, numbers and DSL identifiers
 * are what a localized template needs. The formatter in `stores/changeText`
 * owns the wording.
 */
import type { TimeSignature } from "./project";
import { isRecord } from "../utils/isRecord";

/** A span on the project's quarter-note beat axis. */
export interface BeatSpan {
  start: number;
  end: number;
}

/** The owner of an effect chain or an automation lane. */
export type Owner =
  | { kind: "track"; name: string }
  | { kind: "bus"; name: string }
  | { kind: "master" };

/** A key root and mode, spelled as the DSL writes them. */
export interface KeyToken {
  root: string;
  mode: "major" | "minor";
}

/** Per-channel latency compensation policy. */
export type LatencyCompensation = "on" | "off" | "when_recording";

/** One mix parameter that changed. Scalar parameters carry their endpoints. */
export type MixParam =
  | { kind: "volume"; from: number; to: number }
  | { kind: "pan"; from: number; to: number }
  | { kind: "stereo_balance"; from: boolean; to: boolean }
  | { kind: "mute"; from: boolean; to: boolean }
  | { kind: "solo"; from: boolean; to: boolean }
  | { kind: "polarity_inverted"; from: boolean; to: boolean }
  | { kind: "output"; from: string | null; to: string | null }
  | { kind: "sends" }
  | { kind: "overrides" }
  | { kind: "latency_compensation"; from: LatencyCompensation; to: LatencyCompensation }
  | { kind: "latency_offset"; from: number; to: number }
  | { kind: "control_gain" }
  | { kind: "smart_disable"; from: boolean; to: boolean }
  | { kind: "routes" };

export type Change =
  | { kind: "project_renamed"; from: string | null; to: string | null }
  | { kind: "project_version"; from: string | null; to: string | null }
  | { kind: "tempo"; from: number; to: number }
  | { kind: "time_signature"; from: TimeSignature; to: TimeSignature }
  | { kind: "key"; from: KeyToken | null; to: KeyToken | null }
  | { kind: "swing"; from: number | null; to: number | null }
  | { kind: "tempo_map" }
  | { kind: "time_signature_map" }
  | { kind: "key_map" }
  | { kind: "export_settings" }
  | { kind: "control_links" }
  | { kind: "project_settings" }
  | {
    kind: "track_added" | "track_removed" | "track_instrument" | "track_performance"
      | "track_routing" | "track_changed";
    track: string;
  }
  | { kind: "track_renamed"; from: string; to: string }
  | { kind: "tracks_reordered" }
  | { kind: "track_mix"; track: string; params: MixParam[] }
  | { kind: "effect_added" | "effect_removed" | "effect_changed"; owner: Owner; effect: string }
  | { kind: "effect_bypassed"; owner: Owner; effect: string; bypassed: boolean }
  | { kind: "effects_reordered"; owner: Owner }
  | {
    kind: "notes_edited";
    track: string;
    span: BeatSpan | null;
    added: number;
    removed: number;
    changed: number;
  }
  | { kind: "clips_edited"; track: string; span: BeatSpan | null }
  | { kind: "automation_edited"; owner: Owner; span: BeatSpan | null }
  | { kind: "preset_added" | "preset_removed" | "preset_changed"; preset: string }
  | { kind: "preset_renamed"; from: string; to: string }
  | { kind: "pattern_added" | "pattern_removed"; pattern: string }
  | { kind: "pattern_renamed"; from: string; to: string }
  | { kind: "pattern_changed"; pattern: string; span: BeatSpan | null }
  | { kind: "bus_added" | "bus_removed" | "bus_changed"; bus: string }
  | { kind: "bus_renamed"; from: string; to: string }
  | { kind: "bus_mix"; bus: string; params: MixParam[] }
  | { kind: "master_changed" }
  | { kind: "presets_reordered" }
  | { kind: "patterns_reordered" }
  | { kind: "buses_reordered" }
  | { kind: "sections_edited"; span: BeatSpan | null }
  | { kind: "markers_edited"; added: number; removed: number; changed: number }
  | { kind: "groups_edited" }
  | { kind: "audio_clips_edited"; track: string | null; span: BeatSpan | null };

/** A bounded list of edit facts, plus how many the backend left out. */
export interface ChangeSet {
  changes: Change[];
  more: number;
}

/** Why a history step exists. */
export type EditCause = "edit";

/** The largest fact list a wire change set may carry before the guard cuts it. */
export const MAX_WIRE_CHANGE_FACTS = 64;

/** The largest dropped-fact count a wire change set may report. */
export const MAX_WIRE_MORE = 0xffff_ffff;

/** A change set with at least one fact to name. */
export function hasChangeFacts(set: ChangeSet): boolean {
  return set.changes.length > 0 || set.more > 0;
}

/**
 * A new change set from an untrusted value, or `null` when it is not one.
 * Unknown fact kinds stay in the list so the formatter can draw its generic
 * text for them; the list and the dropped count are bounded so a malformed
 * reply cannot make the UI work for an unbounded time.
 */
export function parseChangeSet(value: unknown): ChangeSet | null {
  if (!isRecord(value) || !Array.isArray(value.changes)) return null;
  const changes = value.changes
    .filter((change) => isRecord(change) && typeof change.kind === "string")
    .slice(0, MAX_WIRE_CHANGE_FACTS) as Change[];
  const more = typeof value.more === "number" && Number.isFinite(value.more)
    ? Math.min(MAX_WIRE_MORE, Math.max(0, Math.floor(value.more)))
    : 0;
  return { changes, more };
}
