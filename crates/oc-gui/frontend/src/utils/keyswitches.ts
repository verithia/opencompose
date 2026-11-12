/**
 * Which key the engine presses for a note's articulation.
 *
 * The keyswitch is derived, never written: an instrument profile resolves
 * `articulation_<token>_keyswitch` (or the round-robin list) from the owning
 * track's `engine_params` layered over its preset's `params`. This module reads
 * the same two param lists the renderer reads and answers with the key the
 * engine will send, so the roll can show it before playback.
 *
 * Mirrors `oc_engine::render_control` (key normalization, note parsing, the
 * round-robin preference); keep the two in step.
 */
import { articulationToken, type Articulation, type ParamValue } from "../types/project";

/** A key the engine presses before the note: MIDI number plus its rolled name. */
export type Keyswitch = { note: number; label: string };

const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** Rolled name of a MIDI note, middle C = C4 = 60 (the roll's own spelling). */
export function midiNoteName(note: number): string {
  const pitch = NOTE_NAMES[((note % 12) + 12) % 12];
  return `${pitch}${Math.floor(note / 12) - 1}`;
}

/** Alphanumerics only, lower case — the engine's key normalization. */
function normalizeKey(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]/g, "");
}

/** MIDI note from `C0`, `13` or `{Number: 13}`; null when the value is unreadable. */
function midiNote(value: ParamValue): number | null {
  if ("Number" in value) return Number.isFinite(value.Number) ? clampNote(value.Number) : null;
  if ("String" in value) return midiNoteFromName(value.String);
  return null;
}

function clampNote(note: number): number | null {
  const rounded = Math.round(note);
  return rounded >= 0 && rounded <= 127 ? rounded : null;
}

/** `C0`, `C#0`, `Db-1` or a plain number; null when the text is not a note. */
function midiNoteFromName(text: string): number | null {
  const value = text.trim();
  if (/^\d+$/.test(value)) return clampNote(Number(value));

  const match = /^([A-Ga-g])([#b]?)(-?\d+)$/.exec(value);
  if (!match) return null;
  const [, letter, accidental, octave] = match;
  const base: Record<string, number> = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };
  const offset = accidental === "#" ? 1 : accidental === "b" ? -1 : 0;
  const note = (Number(octave) + 1) * 12 + base[letter.toUpperCase()] + offset;
  return note >= 0 && note <= 127 ? note : null;
}

/** A round-robin list (`C0 C#0 D0`); null when any entry is not a note. */
function midiNoteList(value: ParamValue): number[] | null {
  if (!("String" in value)) {
    const single = midiNote(value);
    return single === null ? null : [single];
  }
  const notes = value.String
    .split(/[,; ]+/)
    .filter((part) => part.trim() !== "")
    .map(midiNoteFromName);
  if (notes.some((note) => note === null)) return null;
  return notes.length > 0 ? (notes as number[]) : null;
}

type ArticulationProfile = { keyswitch: number | null; roundRobin: number[] };

/**
 * Read one articulation's profile out of a param list, later entries winning the
 * same field (a track's `engine_params` are appended after the preset's, so they
 * override it).
 */
function profileFor(articulation: Articulation, params: readonly [string, ParamValue][]): ArticulationProfile {
  const token = normalizeKey(articulationToken(articulation));
  const profile: ArticulationProfile = { keyswitch: null, roundRobin: [] };
  for (const [name, value] of params) {
    const key = normalizeKey(name);
    if (key === `articulation${token}roundrobinkeyswitches`) {
      const notes = midiNoteList(value);
      if (notes) profile.roundRobin = notes;
      continue;
    }
    if (key === `articulation${token}keyswitch` || key === `keyswitch${token}`) {
      const note = midiNote(value);
      if (note !== null) profile.keyswitch = note;
    }
  }
  return profile;
}

/**
 * The key the engine presses for `articulation`, or null when the preset
 * declares none for that token.
 *
 * Track params win over preset params: they are the same two lists the renderer
 * layers, in the same order. A round-robin list wins over a single keyswitch
 * because `keyswitch_for_articulation_occurrence` prefers it; the first entry is
 * the one this note's onsets start on.
 */
export function keyswitchFor(
  articulation: Articulation | null | undefined,
  trackEngineParams?: readonly [string, ParamValue][] | null,
  presetParams?: readonly [string, ParamValue][] | null,
): Keyswitch | null {
  if (!articulation) return null;
  if (!articulationToken(articulation)) return null;
  const params = [...(presetParams ?? []), ...(trackEngineParams ?? [])];
  if (params.length === 0) return null;
  const profile = profileFor(articulation, params);
  const note = profile.roundRobin.length > 0 ? profile.roundRobin[0] : profile.keyswitch;
  return note === null || note === undefined ? null : { note, label: midiNoteName(note) };
}
