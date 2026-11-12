import type {
  NoteEvent, NoteInsert, NoteMove, NoteRef, NoteScope, NoteVelocity, Pattern, Phrase, ProjectSnapshot,
} from "../types/project";
import * as api from "./api";

export function createNotesActions(
  applyWithUndo: (snapshot: ProjectSnapshot) => void,
  setError: (message: string) => void,
) {
  async function addNote(
    trackIndex: number, phraseIndex: number,
    noteName: string, accidental: string, octave: number,
    baseDuration: string, dynamics?: string, startBeat?: number, durationBeats?: number,
    note?: NoteEvent,
  ) {
    try {
      const snap = await api.addNote(trackIndex, phraseIndex, noteName, accidental, octave, baseDuration, dynamics, startBeat, durationBeats, note);
      applyWithUndo(snap);
    } catch (e) {
      setError(String(e));
    }
  }

  async function removeNote(trackIndex: number, phraseIndex: number, noteIndex: number) {
    try {
      const snap = await api.removeNote(trackIndex, phraseIndex, noteIndex);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function addPhrase(trackIndex: number, startBar: number, startBeat: number, startSub: number) {
    try {
      const snap = await api.addPhrase(trackIndex, startBar, startBeat, startSub);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function insertPhrase(trackIndex: number, phrase: Phrase, startBar: number, startBeat: number, startSub: number) {
    try {
      const snap = await api.insertPhrase(trackIndex, phrase, startBar, startBeat, startSub);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function insertInlinePattern(trackIndex: number, pattern: Pattern, startBar: number, startBeat: number, startSub: number) {
    try {
      const snap = await api.insertInlinePattern(trackIndex, pattern, startBar, startBeat, startSub);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function moveNote(trackIndex: number, phraseIndex: number, noteIndex: number, newStartBeat: number, newPitch?: {note: string; accidental: string; octave: number}) {
    try {
      const snap = await api.moveNote(trackIndex, phraseIndex, noteIndex, newStartBeat, newPitch);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setNoteDuration(trackIndex: number, phraseIndex: number, noteIndex: number, baseDuration: string, dots: number, triplet: boolean, durationBeats?: number) {
    try {
      const snap = await api.setNoteDuration(trackIndex, phraseIndex, noteIndex, baseDuration, dots, triplet, durationBeats);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setNoteVelocity(trackIndex: number, phraseIndex: number, noteIndex: number, velocity: number): Promise<string | null> {
    try {
      const snap = await api.setNoteVelocity(trackIndex, phraseIndex, noteIndex, velocity);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Drop the exact velocity so the note's written dynamic is what plays again. */
  async function clearNoteVelocity(trackIndex: number, phraseIndex: number, noteIndex: number): Promise<string | null> {
    try {
      const snap = await api.clearNoteVelocity(trackIndex, phraseIndex, noteIndex);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /**
   * Move every selected note in one command. The failure message comes back to
   * the caller, which then leaves the selection where the document still has it.
   */
  async function moveNotes(scope: NoteScope, moves: NoteMove[]): Promise<string | null> {
    try {
      const snap = await api.moveNotes(scope, moves);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Remove every selected note in one command, so one undo restores all of them. */
  async function removeNotes(scope: NoteScope, notes: NoteRef[]): Promise<string | null> {
    try {
      const snap = await api.removeNotes(scope, notes);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Paste every copied note in one command, so one undo restores the list. */
  async function addNotes(scope: NoteScope, notes: NoteInsert[]): Promise<string | null> {
    try {
      const snap = await api.addNotes(scope, notes);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Duplicate the selection in one command, so one undo removes every copy. */
  async function duplicateNotes(
    scope: NoteScope,
    notes: NoteRef[],
    offsetBeats: number,
  ): Promise<string | null> {
    try {
      const snap = await api.duplicateNotes(scope, notes, offsetBeats);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Write one exact velocity across the selection as a single undo entry. */
  async function setNoteVelocities(
    scope: NoteScope,
    values: NoteVelocity[],
  ): Promise<string | null> {
    try {
      const snap = await api.setNoteVelocities(scope, values);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /**
   * Snap selected starts toward the roll's grid. The failure message comes back to
   * the caller so the roll can state it where the tool lives.
   */
  async function quantiseNotes(
    scope: NoteScope,
    notes: NoteRef[],
    gridQuarters: number,
    strengthPercent: number,
  ): Promise<string | null> {
    try {
      const snap = await api.quantiseNotes(scope, notes, gridQuarters, strengthPercent);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Nudge selected starts and velocities by a seeded per-note amount. */
  async function humaniseNotes(
    scope: NoteScope,
    notes: NoteRef[],
    timingTicks: number,
    velocitySpread: number,
    seed: number,
  ): Promise<string | null> {
    try {
      const snap = await api.humaniseNotes(scope, notes, timingTicks, velocitySpread, seed);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Extend selected notes to their next start, or by `amountBeats`. */
  async function legatoNotes(
    scope: NoteScope,
    notes: NoteRef[],
    amountBeats: number | null,
  ): Promise<string | null> {
    try {
      const snap = await api.legatoNotes(scope, notes, amountBeats);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Ramp the selection from its first velocity to its last, one undo entry. */
  async function rampNoteVelocity(scope: NoteScope, notes: NoteRef[]): Promise<string | null> {
    try {
      const snap = await api.rampNoteVelocity(scope, notes);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Scale every selected velocity by a percentage, one undo entry. */
  async function scaleNoteVelocity(
    scope: NoteScope,
    notes: NoteRef[],
    percent: number,
  ): Promise<string | null> {
    try {
      const snap = await api.scaleNoteVelocity(scope, notes, percent);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Move every selected velocity halfway toward a target, one undo entry. */
  async function compressNoteVelocity(
    scope: NoteScope,
    notes: NoteRef[],
    target: number,
  ): Promise<string | null> {
    try {
      const snap = await api.compressNoteVelocity(scope, notes, target);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  async function setNoteDynamics(trackIndex: number, phraseIndex: number, noteIndex: number, dynamics: string | null) {
    try {
      const snap = await api.setNoteDynamics(trackIndex, phraseIndex, noteIndex, dynamics);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setNoteArticulation(trackIndex: number, phraseIndex: number, noteIndex: number, articulation: string | null): Promise<string | null> {
    try {
      const snap = await api.setNoteArticulation(trackIndex, phraseIndex, noteIndex, articulation);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  async function setNoteMuted(trackIndex: number, phraseIndex: number, noteIndex: number, muted: boolean) {
    try {
      const snap = await api.setNoteMuted(trackIndex, phraseIndex, noteIndex, muted);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setNoteGrace(trackIndex: number, phraseIndex: number, noteIndex: number, grace: boolean) {
    try {
      const snap = await api.setNoteGrace(trackIndex, phraseIndex, noteIndex, grace);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  // Pattern note editing
  async function addPatternNote(
    patternName: string, noteName: string, accidental: string,
    octave: number, baseDuration: string, startBeat: number, durationBeats?: number,
    note?: NoteEvent,
  ) {
    try {
      const snap = await api.addPatternNote(patternName, noteName, accidental, octave, baseDuration, startBeat, durationBeats, note);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function removePatternNote(patternName: string, noteIndex: number) {
    try {
      const snap = await api.removePatternNote(patternName, noteIndex);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function movePatternNote(patternName: string, noteIndex: number, newStartBeat: number, newPitch?: {note: string; accidental: string; octave: number}) {
    try {
      const snap = await api.movePatternNote(patternName, noteIndex, newStartBeat, newPitch);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setPatternNoteVelocity(patternName: string, noteIndex: number, velocity: number): Promise<string | null> {
    try {
      const snap = await api.setPatternNoteVelocity(patternName, noteIndex, velocity);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Write a pattern hit's exact velocity; the drum lane owns this value. */
  async function setPatternHitVelocity(patternName: string, hitIndex: number, velocity: number): Promise<string | null> {
    try {
      const snap = await api.setPatternHitVelocity(patternName, hitIndex, velocity);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Drop a pattern hit's exact velocity so its written dynamic applies again. */
  async function clearPatternHitVelocity(patternName: string, hitIndex: number): Promise<string | null> {
    try {
      const snap = await api.clearPatternHitVelocity(patternName, hitIndex);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  /** Drop a pattern note's exact velocity so its written dynamic applies again. */
  async function clearPatternNoteVelocity(patternName: string, noteIndex: number): Promise<string | null> {
    try {
      const snap = await api.clearPatternNoteVelocity(patternName, noteIndex);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  async function setPatternNoteDynamics(patternName: string, noteIndex: number, dynamics: string | null) {
    try {
      const snap = await api.setPatternNoteDynamics(patternName, noteIndex, dynamics);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setPatternNoteArticulation(patternName: string, noteIndex: number, articulation: string | null): Promise<string | null> {
    try {
      const snap = await api.setPatternNoteArticulation(patternName, noteIndex, articulation);
      applyWithUndo(snap);
      return null;
    } catch (e) {
      const message = String(e);
      setError(message);
      return message;
    }
  }

  async function setPatternNoteMuted(patternName: string, noteIndex: number, muted: boolean) {
    try {
      const snap = await api.setPatternNoteMuted(patternName, noteIndex, muted);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setPatternNoteGrace(patternName: string, noteIndex: number, grace: boolean) {
    try {
      const snap = await api.setPatternNoteGrace(patternName, noteIndex, grace);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function setPatternNoteDuration(patternName: string, noteIndex: number, baseDuration: string, dots: number, triplet: boolean, durationBeats?: number) {
    try {
      const snap = await api.setPatternNoteDuration(patternName, noteIndex, baseDuration, dots, triplet, durationBeats);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  // Pattern hit editing
  async function addPatternHit(
    patternName: string,
    drum: string,
    beat: number,
    subdivision: number,
    dynamics?: string,
    extras: api.HitExtras = {},
  ) {
    try {
      const snap = await api.addPatternHit(patternName, drum, beat, subdivision, dynamics, extras);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function removePatternHit(patternName: string, hitIndex: number) {
    try {
      const snap = await api.removePatternHit(patternName, hitIndex);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  async function movePatternHit(patternName: string, hitIndex: number, beat: number, subdivision: number) {
    try {
      const snap = await api.movePatternHit(patternName, hitIndex, beat, subdivision);
      applyWithUndo(snap);
    } catch (e) { setError(String(e)); }
  }

  return {
    addNote,
    removeNote,
    addPhrase,
    insertPhrase,
    insertInlinePattern,
    moveNote,
    setNoteDuration,
    setNoteVelocity,
    clearNoteVelocity,
    moveNotes,
    removeNotes,
    addNotes,
    duplicateNotes,
    setNoteVelocities,
    quantiseNotes,
    humaniseNotes,
    legatoNotes,
    rampNoteVelocity,
    scaleNoteVelocity,
    compressNoteVelocity,
    setNoteDynamics,
    setNoteArticulation,
    setNoteMuted,
    setNoteGrace,
    addPatternNote,
    removePatternNote,
    movePatternNote,
    setPatternNoteVelocity,
    setPatternHitVelocity,
    clearPatternHitVelocity,
    clearPatternNoteVelocity,
    setPatternNoteDynamics,
    setPatternNoteArticulation,
    setPatternNoteMuted,
    setPatternNoteGrace,
    setPatternNoteDuration,
    addPatternHit,
    removePatternHit,
    movePatternHit,
  };
}
