/**
 * Piano Roll vertical positioning helpers.
 *
 * The roll must open centered on C4 (MIDI 60); these tests pin the geometry
 * math independently of the DOM.
 */
import { describe, it, expect } from "vitest";
import {
  MAX_MIDI,
  MIN_MIDI,
  NOTE_HEIGHT,
  NOTE_RANGE,
  PIANO_ROLL_CENTER_MIDI,
  PIANO_ROLL_RULER_HEIGHT,
  midiRowTop,
  scrollTopToCenterMidi,
} from "../components/PianoRoll";

describe("Piano Roll C4 centering", () => {
  it("uses MIDI 60 (C4) as the centering target", () => {
    expect(PIANO_ROLL_CENTER_MIDI).toBe(60);
    expect(midiRowTop(PIANO_ROLL_CENTER_MIDI)).toBe((MAX_MIDI - 1 - 60) * NOTE_HEIGHT);
  });

  it("centers C4 in the note area below the sticky ruler", () => {
    const clientHeight = 400;
    const visibleHeight = clientHeight - PIANO_ROLL_RULER_HEIGHT;
    const c4Center = midiRowTop(60) + NOTE_HEIGHT / 2;
    const scrollTop = scrollTopToCenterMidi(clientHeight);

    expect(scrollTop).toBeCloseTo(c4Center - visibleHeight / 2);
    expect(scrollTop + visibleHeight / 2).toBeCloseTo(c4Center);
    expect(scrollTop).toBeGreaterThan(0);
  });

  it("defaults to C4 when no pitch is supplied", () => {
    expect(scrollTopToCenterMidi(600)).toBe(scrollTopToCenterMidi(600, 60));
  });

  it("clamps at the top when the target is near the highest key", () => {
    expect(scrollTopToCenterMidi(400, MAX_MIDI)).toBe(0);
  });

  it("clamps at the bottom when the target is near the lowest key", () => {
    const contentHeight = PIANO_ROLL_RULER_HEIGHT + NOTE_RANGE * NOTE_HEIGHT;
    const maxScroll = contentHeight - 400;
    expect(maxScroll).toBeGreaterThan(0);
    expect(scrollTopToCenterMidi(400, MIN_MIDI)).toBe(maxScroll);
  });

  it("returns a safe offset before the panel has been measured", () => {
    expect(scrollTopToCenterMidi(0)).toBe(0);
    expect(scrollTopToCenterMidi(PIANO_ROLL_RULER_HEIGHT)).toBe(0);
  });
});
