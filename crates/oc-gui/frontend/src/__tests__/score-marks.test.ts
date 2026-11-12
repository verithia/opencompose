/**
 * Score-mark helpers: hairpin span resolution and the level its ramp reaches.
 *
 * The span rules mirror the engine's expansion, so these cases are the same
 * ones `oc-dsl` pins: the dynamic in force at the mark, the next written
 * dynamic (or the phrase end), and the pp/f fallbacks.
 */
import { describe, expect, it } from "vitest";
import { makeNote } from "./setup";
import {
  hairpinAt,
  pedalSpans,
  phraseEndBeat,
  patternHairpins,
  phraseHairpins,
  rampedVelocity,
  type ScoreHairpin,
} from "../utils/scoreMarks";
import type { NoteEvent } from "../types/project";

const phraseEnd = 8;

function events(list: NoteEvent[]): NoteEvent[] {
  return list;
}

describe("phraseHairpins", () => {
  it("swells from the dynamic in force to the next one written", () => {
    const spans = phraseHairpins(
      [{ beat: 2, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 6, dynamics: "F", velocity: null }),
      ]),
      0,
      phraseEnd,
    );

    expect(spans).toEqual([
      {
        beat: 2,
        kind: "crescendo",
        endBeat: 6,
        startVelocity: 33,
        targetVelocity: 96,
      },
    ]);
  });

  it("falls back to pp for a crescendo with nothing written before it", () => {
    const spans = phraseHairpins(
      [{ beat: 0, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: null, velocity: null }),
        makeNote({ start_beat: 4, dynamics: null, velocity: null }),
      ]),
      0,
      phraseEnd,
    );

    // Nothing writes a dynamic, so the ramp runs pp to f and stops at the end.
    expect(spans).toEqual([
      {
        beat: 0,
        kind: "crescendo",
        endBeat: phraseEnd,
        startVelocity: 33,
        targetVelocity: 96,
      },
    ]);
  });

  it("falls back to f for a diminuendo that has nothing to fall from", () => {
    const spans = phraseHairpins(
      [{ beat: 1, kind: "diminuendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: null, velocity: null }),
        makeNote({ start_beat: 1, dynamics: null, velocity: null }),
        makeNote({ start_beat: 3, dynamics: null, velocity: null }),
      ]),
      0,
      phraseEnd,
    );

    expect(spans[0].startVelocity).toBe(96);
    expect(spans[0].targetVelocity).toBe(33);
  });

  it("ends the span at the first note that writes its own velocity", () => {
    // A written velocity is what the engine swells *to*, so a note carrying one
    // ends the range instead of being moved by it.
    const spans = phraseHairpins(
      [{ beat: 0, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 2, dynamics: null, velocity: 40 }),
        makeNote({ start_beat: 4, dynamics: "F", velocity: null }),
      ]),
      0,
      phraseEnd,
    );

    expect(spans).toHaveLength(1);
    expect(spans[0].endBeat).toBe(2);
    expect(spans[0].targetVelocity).toBe(40);
  });

  it("leaves out a mark whose levels do not move", () => {
    // pp written before and pp written after: the engine refuses this project,
    // and the strip must not draw a wedge that cannot sound.
    const spans = phraseHairpins(
      [{ beat: 0, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 2, dynamics: "PP", velocity: null }),
      ]),
      0,
      phraseEnd,
    );

    expect(spans).toEqual([]);
  });

  it("places the span at absolute beats when the phrase starts later", () => {
    const spans = phraseHairpins(
      [{ beat: 2, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 4, dynamics: "F", velocity: null }),
      ]),
      16,
      32,
    );

    expect(spans[0].beat).toBe(18);
    expect(spans[0].endBeat).toBe(20);
  });

  it("draws nothing for a phrase with no marks", () => {
    expect(phraseHairpins([], events([makeNote()]), 0, phraseEnd)).toEqual([]);
  });
});

describe("phraseEndBeat", () => {
  it("takes the written range when there is one", () => {
    expect(phraseEndBeat([makeNote({ start_beat: 0 })], 16, 0)).toBe(16);
  });

  it("falls back to the last note end", () => {
    expect(phraseEndBeat([makeNote({ start_beat: 2 })], null, 4)).toBe(7);
  });
});

describe("hairpinAt", () => {
  const span: ScoreHairpin = {
    beat: 2,
    kind: "crescendo",
    endBeat: 6,
    startVelocity: 33,
    targetVelocity: 96,
  };

  it("covers its start and stops at its end", () => {
    expect(hairpinAt([span], 2)).toBe(span);
    expect(hairpinAt([span], 4)).toBe(span);
    expect(hairpinAt([span], 6)).toBeNull();
    expect(hairpinAt([span], 1.5)).toBeNull();
  });
});

describe("rampedVelocity", () => {
  const span: ScoreHairpin = {
    beat: 0,
    kind: "crescendo",
    endBeat: 6,
    startVelocity: 33,
    targetVelocity: 96,
  };

  it("is the engine's decibel ramp, not a linear velocity ladder", () => {
    // Linear in decibels means geometric in velocity: the midpoint of pp and f
    // is 56, not the arithmetic 64.5.
    expect(rampedVelocity(span, 0, 33)).toBe(33);
    expect(rampedVelocity(span, 3, 33)).toBe(56);
    expect(rampedVelocity(span, 6, 33)).toBe(96);
  });

  it("scales a written level by the same ramp", () => {
    // A note that writes 40 keeps 40 at the mark and reaches 40 * 96/33 at the end.
    expect(rampedVelocity(span, 0, 40)).toBe(40);
    expect(rampedVelocity(span, 6, 40)).toBe(116);
  });

  it("holds the endpoints outside the span", () => {
    expect(rampedVelocity(span, -2, 33)).toBe(33);
    expect(rampedVelocity(span, 12, 33)).toBe(96);
  });
});

describe("pedalSpans", () => {
  it("pairs a down with the next up", () => {
    expect(
      pedalSpans([{ beat: 1, down: true }, { beat: 3, down: false }], 8),
    ).toEqual([{ start: 1, end: 3 }]);
  });

  it("closes a hold nothing lifts at the given end", () => {
    expect(pedalSpans([{ beat: 4, down: true }], 8)).toEqual([{ start: 4, end: 8 }]);
  });

  it("ignores an up mark with no down mark", () => {
    expect(pedalSpans([{ beat: 2, down: false }], 8)).toEqual([]);
  });

  it("keeps only the first of two overlapping downs", () => {
    expect(
      pedalSpans(
        [
          { beat: 0, down: true },
          { beat: 1, down: true },
          { beat: 4, down: false },
        ],
        8,
      ),
    ).toEqual([{ start: 0, end: 4 }]);
  });
});

describe("patternHairpins", () => {
  it("resolves a pattern's own marks in its local beats", () => {
    // A pattern owns the marks; its last note end is the content end.
    const spans = patternHairpins(
      [{ beat: 2, kind: "crescendo" }],
      events([
        makeNote({ start_beat: 0, dynamics: "P", velocity: null }),
        makeNote({ start_beat: 6, dynamics: "F", velocity: null }),
      ]),
      8,
    );

    expect(spans).toEqual([
      { beat: 2, kind: "crescendo", endBeat: 6, startVelocity: 49, targetVelocity: 96 },
    ]);
  });

  it("drops a mark with no note after it", () => {
    expect(
      patternHairpins(
        [{ beat: 7, kind: "diminuendo" }],
        events([makeNote({ start_beat: 0, dynamics: "F", velocity: null })]),
        8,
      ),
    ).toEqual([]);
  });

  it("is empty for a pattern with no marks", () => {
    expect(patternHairpins([], [], 4)).toEqual([]);
  });
});
