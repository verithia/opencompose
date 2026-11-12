/**
 * Phrase, note and pattern editing commands.
 *
 * `api.ts` re-exports this module for existing callers.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  NoteEvent,
  NoteInsert,
  NoteMove,
  NoteRef,
  NoteScope,
  NoteVelocity,
  Pattern,
  Phrase,
  ProjectSnapshot,
} from "../types/project";

// ============================================================================
// Note editing
// ============================================================================

export const addNote = (
  trackIndex: number, phraseIndex: number,
  noteName: string, accidental: string, octave: number,
  baseDuration: string, dynamics?: string, startBeat?: number, durationBeats?: number,
  note?: NoteEvent,
) => invoke<ProjectSnapshot>("add_note", {
  trackIndex, phraseIndex, noteName, accidental, octave,
  baseDuration, dynamics: dynamics ?? null, startBeat: startBeat ?? null, durationBeats,
  note: note ?? null,
});

export const removeNote = (trackIndex: number, phraseIndex: number, noteIndex: number) =>
  invoke<ProjectSnapshot>("remove_note", { trackIndex, phraseIndex, noteIndex });

export const addPhrase = (trackIndex: number, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("add_phrase", { trackIndex, startBar, startBeat, startSub });

export const insertPhrase = (trackIndex: number, phrase: Phrase, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("insert_phrase", { trackIndex, phrase, startBar, startBeat, startSub });

export const insertInlinePattern = (trackIndex: number, pattern: Pattern, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("insert_inline_pattern", { trackIndex, pattern, startBar, startBeat, startSub });

export const extendPhraseRange = (trackIndex: number, phraseIndex: number, endBar: number, endBeat: number, endSub: number) =>
  invoke<ProjectSnapshot>("extend_phrase_range", { trackIndex, phraseIndex, endBar, endBeat, endSub });

export const movePhrase = (trackIndex: number, phraseIndex: number, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("move_phrase", { trackIndex, phraseIndex, startBar, startBeat, startSub });

export const movePhraseToTrack = (
  fromTrackIndex: number, phraseIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) => invoke<ProjectSnapshot>("move_phrase_to_track", {
  fromTrackIndex, phraseIndex, toTrackIndex, startBar, startBeat, startSub,
});

export const moveInlinePattern = (trackIndex: number, patternIndex: number, startBar: number, startBeat: number, startSub: number) =>
  invoke<ProjectSnapshot>("move_inline_pattern", { trackIndex, patternIndex, startBar, startBeat, startSub });

export const moveInlinePatternToTrack = (
  fromTrackIndex: number, patternIndex: number, toTrackIndex: number,
  startBar: number, startBeat: number, startSub: number,
) => invoke<ProjectSnapshot>("move_inline_pattern_to_track", {
  fromTrackIndex, patternIndex, toTrackIndex, startBar, startBeat, startSub,
});

// Note editing (move, resize, properties)
export const moveNote = (trackIndex: number, phraseIndex: number, noteIndex: number, newStartBeat: number, newPitch?: {note: string; accidental: string; octave: number}) =>
  invoke<ProjectSnapshot>("move_note", { trackIndex, phraseIndex, noteIndex, newStartBeat, newPitch });

export const setNoteDuration = (trackIndex: number, phraseIndex: number, noteIndex: number, baseDuration: string, dots: number, triplet: boolean, durationBeats?: number) =>
  invoke<ProjectSnapshot>("set_note_duration", { trackIndex, phraseIndex, noteIndex, baseDuration, dots, triplet, durationBeats });

export const setNoteVelocity = (trackIndex: number, phraseIndex: number, noteIndex: number, velocity: number) =>
  invoke<ProjectSnapshot>("set_note_velocity", { trackIndex, phraseIndex, noteIndex, velocity });

export const clearNoteVelocity = (trackIndex: number, phraseIndex: number, noteIndex: number) =>
  invoke<ProjectSnapshot>("clear_note_velocity", { trackIndex, phraseIndex, noteIndex });

export const setNoteDynamics = (trackIndex: number, phraseIndex: number, noteIndex: number, dynamics: string | null) =>
  invoke<ProjectSnapshot>("set_note_dynamics", { trackIndex, phraseIndex, noteIndex, dynamics });

export const setNoteArticulation = (trackIndex: number, phraseIndex: number, noteIndex: number, articulation: string | null) =>
  invoke<ProjectSnapshot>("set_note_articulation", { trackIndex, phraseIndex, noteIndex, articulation });

export const setNoteMuted = (trackIndex: number, phraseIndex: number, noteIndex: number, muted: boolean) =>
  invoke<ProjectSnapshot>("set_note_muted", { trackIndex, phraseIndex, noteIndex, muted });

export const setNoteGrace = (trackIndex: number, phraseIndex: number, noteIndex: number, grace: boolean) =>
  invoke<ProjectSnapshot>("set_note_grace", { trackIndex, phraseIndex, noteIndex, grace });

// Batch note tools
export const addNotes = (scope: NoteScope, notes: NoteInsert[]) =>
  invoke<ProjectSnapshot>("add_notes", { scope, notes });

export const duplicateNotes = (scope: NoteScope, notes: NoteRef[], offsetBeats: number) =>
  invoke<ProjectSnapshot>("duplicate_notes", { scope, notes, offsetBeats });

export const moveNotes = (scope: NoteScope, moves: NoteMove[]) =>
  invoke<ProjectSnapshot>("move_notes", { scope, moves });

export const removeNotes = (scope: NoteScope, notes: NoteRef[]) =>
  invoke<ProjectSnapshot>("remove_notes", { scope, notes });

export const setNoteVelocities = (scope: NoteScope, values: NoteVelocity[]) =>
  invoke<ProjectSnapshot>("set_note_velocities", { scope, values });

export const quantiseNotes = (scope: NoteScope, notes: NoteRef[], gridQuarters: number, strengthPercent: number) =>
  invoke<ProjectSnapshot>("quantise_notes", { scope, notes, gridQuarters, strengthPercent });

export const humaniseNotes = (scope: NoteScope, notes: NoteRef[], timingTicks: number, velocitySpread: number, seed: number) =>
  invoke<ProjectSnapshot>("humanise_notes", { scope, notes, timingTicks, velocitySpread, seed });

export const legatoNotes = (scope: NoteScope, notes: NoteRef[], amountBeats: number | null) =>
  invoke<ProjectSnapshot>("legato_notes", { scope, notes, amountBeats });

export const rampNoteVelocity = (scope: NoteScope, notes: NoteRef[]) =>
  invoke<ProjectSnapshot>("ramp_note_velocity", { scope, notes });

export const scaleNoteVelocity = (scope: NoteScope, notes: NoteRef[], percent: number) =>
  invoke<ProjectSnapshot>("scale_note_velocity", { scope, notes, percent });

export const compressNoteVelocity = (scope: NoteScope, notes: NoteRef[], target: number) =>
  invoke<ProjectSnapshot>("compress_note_velocity", { scope, notes, target });

// Pattern note editing
export const addPatternNote = (patternName: string, noteName: string, accidental: string, octave: number, baseDuration: string, startBeat: number, durationBeats?: number, note?: NoteEvent) =>
  invoke<ProjectSnapshot>("add_pattern_note", { patternName, noteName, accidental, octave, baseDuration, startBeat, durationBeats, note: note ?? null });

export const removePatternNote = (patternName: string, noteIndex: number) =>
  invoke<ProjectSnapshot>("remove_pattern_note", { patternName, noteIndex });

export const movePatternNote = (patternName: string, noteIndex: number, newStartBeat: number, newPitch?: {note: string; accidental: string; octave: number}) =>
  invoke<ProjectSnapshot>("move_pattern_note", { patternName, noteIndex, newStartBeat, newPitch });

export const setPatternNoteVelocity = (patternName: string, noteIndex: number, velocity: number) =>
  invoke<ProjectSnapshot>("set_pattern_note_velocity", { patternName, noteIndex, velocity });

export const clearPatternNoteVelocity = (patternName: string, noteIndex: number) =>
  invoke<ProjectSnapshot>("clear_pattern_note_velocity", { patternName, noteIndex });

export const setPatternHitVelocity = (patternName: string, hitIndex: number, velocity: number) =>
  invoke<ProjectSnapshot>("set_pattern_hit_velocity", { patternName, hitIndex, velocity });

export const clearPatternHitVelocity = (patternName: string, hitIndex: number) =>
  invoke<ProjectSnapshot>("clear_pattern_hit_velocity", { patternName, hitIndex });

export const setPatternNoteDynamics = (patternName: string, noteIndex: number, dynamics: string | null) =>
  invoke<ProjectSnapshot>("set_pattern_note_dynamics", { patternName, noteIndex, dynamics });

export const setPatternNoteArticulation = (patternName: string, noteIndex: number, articulation: string | null) =>
  invoke<ProjectSnapshot>("set_pattern_note_articulation", { patternName, noteIndex, articulation });

export const setPatternNoteMuted = (patternName: string, noteIndex: number, muted: boolean) =>
  invoke<ProjectSnapshot>("set_pattern_note_muted", { patternName, noteIndex, muted });

export const setPatternNoteGrace = (patternName: string, noteIndex: number, grace: boolean) =>
  invoke<ProjectSnapshot>("set_pattern_note_grace", { patternName, noteIndex, grace });

export const setPatternNoteDuration = (patternName: string, noteIndex: number, baseDuration: string, dots: number, triplet: boolean, durationBeats?: number) =>
  invoke<ProjectSnapshot>("set_pattern_note_duration", { patternName, noteIndex, baseDuration, dots, triplet, durationBeats });

// Pattern management
export const addPattern = (name: string, instrumentCategory: string, instrumentName: string, preset?: string) =>
  invoke<ProjectSnapshot>("add_pattern", { name, instrumentCategory, instrumentName, preset: preset ?? null });

export const removePattern = (patternName: string) =>
  invoke<ProjectSnapshot>("remove_pattern", { patternName });

/** Rename the pattern in the AST; its clips follow in the same edit. */
export const renamePattern = (patternName: string, newName: string) =>
  invoke<ProjectSnapshot>("rename_pattern", { patternName, newName });

// Pattern property editing
export const setPatternTranspose = (patternName: string, transpose: number) =>
  invoke<ProjectSnapshot>("set_pattern_transpose", { patternName, transpose });

export const setPatternSwing = (patternName: string, swing: number | null) =>
  invoke<ProjectSnapshot>("set_pattern_swing", { patternName, swing });

/** Grid the project swing percentage moves: off-beat eighths or sixteenths. */
export const setProjectSwingSubdivision = (subdivision: "8th" | "16th") =>
  invoke<ProjectSnapshot>("set_project_swing_subdivision", { subdivision });

export const setPatternColor = (patternName: string, color: string) =>
  invoke<ProjectSnapshot>("set_pattern_color", { patternName, color });

export const setPatternBars = (patternName: string, bars: number | null) =>
  invoke<ProjectSnapshot>("set_pattern_bars", { patternName, bars });

export const trimPatternToBars = (patternName: string, bars: number) =>
  invoke<ProjectSnapshot>("trim_pattern_to_bars", { patternName, bars });

// Pattern hit editing
/** A hit's values beside its `beat:subdivision` position. */
export interface HitExtras {
  /** Expressive microtiming applied after the written grid and swing. */
  offsetBeats?: number | null;
  /** Signed quarter-note distance from the sixteenth anchor to the grid onset. */
  gridOffsetBeats?: number | null;
  /** Note the hit plays, for a hit on a pad the kit does not name. */
  midiNote?: number | null;
}

export const addPatternHit = (
  patternName: string,
  drum: string,
  beat: number,
  subdivision: number,
  dynamics?: string,
  extras: HitExtras = {},
) =>
  invoke<ProjectSnapshot>("add_pattern_hit", {
    patternName,
    drum,
    beat,
    subdivision,
    dynamics: dynamics ?? null,
    offsetBeats: extras.offsetBeats ?? null,
    gridOffsetBeats: extras.gridOffsetBeats ?? null,
    midiNote: extras.midiNote ?? null,
  });

export const removePatternHit = (patternName: string, hitIndex: number) =>
  invoke<ProjectSnapshot>("remove_pattern_hit", { patternName, hitIndex });

export const movePatternHit = (patternName: string, hitIndex: number, beat: number, subdivision: number) =>
  invoke<ProjectSnapshot>("move_pattern_hit", { patternName, hitIndex, beat, subdivision });
