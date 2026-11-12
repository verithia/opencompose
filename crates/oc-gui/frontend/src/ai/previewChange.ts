/**
 * What a proposal's diff does, as one plain line for the preview bar.
 *
 * The diff is the only source: the model's prose never reaches this module. A
 * declaration line names its subject (`@track`, `@phrase`, `@pattern`), so the
 * classifier reads the changed lines and their enclosing declaration instead of
 * counting them. A diff with nothing to decompose is reported as a wholesale
 * replacement rather than guessed at.
 */
import { i18n } from "../stores/i18n";
import type { DiffResult } from "./transcript";

/** The kinds of change the line can name, most significant first. */
const ORDER = [
  "trackAdd",
  "trackRemove",
  "patternAdd",
  "patternRemove",
  "clipRemove",
  "clipAdd",
  "clipMove",
  "notes",
  "structure",
  "settings",
] as const;

type ChangeKind = (typeof ORDER)[number];

interface ChangePart {
  kind: ChangeKind;
  count: number;
  /** The declaration's name, when the kind names one. */
  name?: string;
  /** The track a clip belongs to. */
  track?: string;
}

/** One line names at most this many kinds; a project edit rarely spans more. */
const MAX_PARTS = 3;

type PreviewChange =
  | { kind: "wholesale" }
  | { kind: "parts"; parts: ChangePart[] }
  | null;

const DECLARATION = /^(\s*)@([A-Za-z_][A-Za-z0-9_]*)(.*)$/;
const RANGE = /\[([^\]]*)\]/;
const INLINE_TRACK = /track:\s*(?:"([^"]+)"|([^,\s|]+))/;
const WHOLE_DOCUMENT = /^@(?:ocp|project)\b/m;

/** The first token after a keyword, quoted names included. */
function declarationName(rest: string): string {
  const quoted = /^\s*"([^"]+)"/.exec(rest);
  if (quoted) return quoted[1];
  return /^\s*([^\s|,\[]+)/.exec(rest)?.[1] ?? "";
}

function clipTrack(rest: string, enclosing: string): string {
  const inline = INLINE_TRACK.exec(rest);
  return inline ? (inline[1] ?? inline[2]) : enclosing;
}

function changed(line: { kind: string }): boolean {
  return line.kind === "Added" || line.kind === "Removed";
}

/**
 * The change a diff carries, or `null` when it carries none. A diff with no
 * hunks, or one that shares no line with the base document at the whole
 * document's level, cannot be decomposed into named changes.
 */
function previewChange(diff: DiffResult | null | undefined): PreviewChange {
  if (!diff) return null;
  const hunks = Array.isArray(diff.hunks) ? diff.hunks : [];
  const lines = hunks.flatMap((hunk) => (Array.isArray(hunk?.lines) ? hunk.lines : []));
  if (!lines.length) return { kind: "wholesale" };
  if (!lines.some(changed)) return null;
  if (
    !lines.some((line) => !changed(line))
    && WHOLE_DOCUMENT.test(diff.old_text ?? "")
    && WHOLE_DOCUMENT.test(diff.new_text ?? "")
  ) {
    return { kind: "wholesale" };
  }

  const tracks: Record<"Added" | "Removed", string[]> = { Added: [], Removed: [] };
  const patterns: Record<"Added" | "Removed", string[]> = { Added: [], Removed: [] };
  const clips: Record<"Added" | "Removed", { name: string; track: string; range: string }[]> = {
    Added: [],
    Removed: [],
  };
  // The enclosing declarations, so a body line is read as notes or settings.
  const enclosing: { indent: number; keyword: string; name: string; replaced: boolean }[] = [];
  let notes = 0;
  let settings = 0;
  let structure = 0;
  const noteNames = new Set<string>();
  /** The innermost declaration a line at `indent` sits inside. */
  const scopeOf = (indent: number) =>
    enclosing.findLast((entry) => entry.indent < indent) ?? enclosing[enclosing.length - 1];

  for (const line of lines) {
    const text = typeof line?.text === "string" ? line.text : "";
    const trimmed = text.trim();
    const side = line.kind === "Added" ? "Added" : line.kind === "Removed" ? "Removed" : null;
    if (!trimmed || trimmed.startsWith("//")) continue;
    const declaration = DECLARATION.exec(text);
    if (!declaration) {
      // A body line belongs to whatever declares it; a body under a declaration
      // the diff adds or removes wholesale is part of that change, not a
      // separate setting.
      if (!side) continue;
      const scope = scopeOf(text.length - text.trimStart().length);
      if (scope?.replaced) continue;
      // A body line is a note only inside a phrase or pattern body.
      if (scope?.keyword === "phrase" || scope?.keyword === "pattern") {
        notes += 1;
        if (scope.name) noteNames.add(scope.name);
      } else {
        settings += 1;
      }
      continue;
    }
    const [, spacing, keyword, rest] = declaration;
    const indent = spacing.length;
    const name = declarationName(rest);
    while (enclosing.length && enclosing[enclosing.length - 1].indent >= indent) enclosing.pop();
    const track = enclosing.findLast((entry) => entry.keyword === "track")?.name ?? "";
    enclosing.push({ indent, keyword, name, replaced: side !== null });
    if (!side) continue;
    if (keyword === "track") {
      if (name) tracks[side].push(name);
      else structure += 1;
    } else if (keyword === "phrase" || keyword === "audio_clip" || (keyword === "pattern" && indent > 0)) {
      if (!name) {
        structure += 1;
        continue;
      }
      clips[side].push({
        name,
        track: clipTrack(rest, track),
        range: RANGE.exec(rest)?.[1]?.trim() ?? "",
      });
    } else if (keyword === "pattern") {
      if (name) patterns[side].push(name);
      else structure += 1;
    } else if (keyword === "project" || keyword === "ocp") {
      settings += 1;
    } else {
      structure += 1;
    }
  }

  // A track or pattern named on both sides kept its identity and changed in
  // place, so it is neither added nor removed.
  const inPlace = (added: string[], removed: string[]) => {
    const remaining = new Map<string, number>();
    for (const name of removed) remaining.set(name, (remaining.get(name) ?? 0) + 1);
    const addedOnly: string[] = [];
    for (const name of added) {
      const count = remaining.get(name) ?? 0;
      if (count > 0) {
        remaining.set(name, count - 1);
        settings += 1;
      } else {
        addedOnly.push(name);
      }
    }
    const removedOnly: string[] = [];
    for (const [name, count] of remaining) {
      for (let index = 0; index < count; index++) removedOnly.push(name);
    }
    return { added: addedOnly, removed: removedOnly };
  };
  const changedTracks = inPlace(tracks.Added, tracks.Removed);
  const changedPatterns = inPlace(patterns.Added, patterns.Removed);

  // A clip that left one range and arrived at another has moved; a clip whose
  // header changed without changing its range only had its settings changed.
  const removedClips = new Map<string, { name: string; track: string; range: string }[]>();
  for (const clip of clips.Removed) {
    if (!clip.track) {
      structure += 1;
      continue;
    }
    const key = `${clip.track}\u0000${clip.name}`;
    removedClips.set(key, [...(removedClips.get(key) ?? []), clip]);
  }
  const addedByTrack = new Map<string, number>();
  const movedByTrack = new Map<string, number>();
  for (const clip of clips.Added) {
    if (!clip.track) {
      structure += 1;
      continue;
    }
    const match = removedClips.get(`${clip.track}\u0000${clip.name}`)?.pop();
    if (!match) {
      addedByTrack.set(clip.track, (addedByTrack.get(clip.track) ?? 0) + 1);
    } else if (match.range !== clip.range) {
      movedByTrack.set(clip.track, (movedByTrack.get(clip.track) ?? 0) + 1);
    } else {
      settings += 1;
    }
  }
  const removedByTrack = new Map<string, number>();
  for (const bucket of removedClips.values()) {
    for (const clip of bucket) {
      removedByTrack.set(clip.track, (removedByTrack.get(clip.track) ?? 0) + 1);
    }
  }

  const parts: ChangePart[] = [];
  const named = (kind: ChangeKind, names: string[]) => {
    if (names.length) parts.push({ kind, count: names.length, name: names[0] });
  };
  named("trackAdd", changedTracks.added);
  named("trackRemove", changedTracks.removed);
  named("patternAdd", changedPatterns.added);
  named("patternRemove", changedPatterns.removed);
  for (const [name, count] of removedByTrack) parts.push({ kind: "clipRemove", count, track: name });
  for (const [name, count] of addedByTrack) parts.push({ kind: "clipAdd", count, track: name });
  for (const [name, count] of movedByTrack) parts.push({ kind: "clipMove", count, track: name });
  // Body lines under a declaration the diff adds or removes are part of that
  // change; only the notes of a declaration that stayed put get a clause.
  const noteScopes = [...noteNames].filter((name) => name);
  if (notes) {
    parts.push(noteScopes.length === 1
      ? { kind: "notes", count: notes, name: noteScopes[0] }
      : { kind: "notes", count: notes });
  }
  if (structure) parts.push({ kind: "structure", count: structure });
  if (settings) parts.push({ kind: "settings", count: settings });

  if (!parts.length) return null;
  parts.sort((left, right) => ORDER.indexOf(left.kind) - ORDER.indexOf(right.kind));
  return { kind: "parts", parts: parts.slice(0, MAX_PARTS) };
}

/** The line the bar prints, or `null` when the diff has nothing to name. */
export function previewChangeLine(diff: DiffResult | null | undefined): string | null {
  const change = previewChange(diff);
  if (!change) return null;
  if (change.kind === "wholesale") return i18n.t("ai.preview.change.wholesale");
  const clauses = change.parts.map(partText);
  if (clauses.length === 1) return i18n.t("ai.preview.change.one", { change: clauses[0] });
  if (clauses.length === 2) {
    return i18n.t("ai.preview.change.pair", { first: clauses[0], second: lowerFirst(clauses[1]) });
  }
  return i18n.t("ai.preview.change.three", {
    first: clauses[0],
    second: lowerFirst(clauses[1]),
    third: lowerFirst(clauses[2]),
  });
}

function partText(part: ChangePart): string {
  const many = part.count > 1;
  switch (part.kind) {
    case "trackAdd":
      return many
        ? i18n.t("ai.preview.change.trackAddMany", { count: part.count })
        : i18n.t("ai.preview.change.trackAdd", { name: part.name ?? "" });
    case "trackRemove":
      return many
        ? i18n.t("ai.preview.change.trackRemoveMany", { count: part.count })
        : i18n.t("ai.preview.change.trackRemove", { name: part.name ?? "" });
    case "patternAdd":
      return many
        ? i18n.t("ai.preview.change.patternAddMany", { count: part.count })
        : i18n.t("ai.preview.change.patternAdd", { name: part.name ?? "" });
    case "patternRemove":
      return many
        ? i18n.t("ai.preview.change.patternRemoveMany", { count: part.count })
        : i18n.t("ai.preview.change.patternRemove", { name: part.name ?? "" });
    case "clipAdd":
      return many
        ? i18n.t("ai.preview.change.clipAddMany", { count: part.count, track: part.track ?? "" })
        : i18n.t("ai.preview.change.clipAdd", { track: part.track ?? "" });
    case "clipRemove":
      return many
        ? i18n.t("ai.preview.change.clipRemoveMany", { count: part.count, track: part.track ?? "" })
        : i18n.t("ai.preview.change.clipRemove", { track: part.track ?? "" });
    case "clipMove":
      return many
        ? i18n.t("ai.preview.change.clipMoveMany", { count: part.count, track: part.track ?? "" })
        : i18n.t("ai.preview.change.clipMove", { track: part.track ?? "" });
    case "notes":
      return part.name
        ? i18n.t("ai.preview.change.notesOf", { name: part.name })
        : i18n.t("ai.preview.change.notes");
    case "structure":
      return i18n.t("ai.preview.change.structure");
    case "settings":
      return i18n.t("ai.preview.change.settings");
  }
}

/** The opening word of a second clause: "Adds" reads as "adds" after "and". */
function lowerFirst(text: string): string {
  return text.charAt(0).toLocaleLowerCase() + text.slice(1);
}
