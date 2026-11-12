import { describe, expect, it } from "vitest";
import { makeNote } from "../setup";
import type { HitEvent } from "../../types/project";
import { hitThumbnails, noteThumbnails } from "../../components/Timeline";

describe("timeline note thumbnails", () => {
  it("keeps dense chord tones separate while preserving pitch spacing and note timing", () => {
    const chord = [
      { note: "C", accidental: "Natural", octave: 2 },
      { note: "C", accidental: "Natural", octave: 4 },
      { note: "C", accidental: "Sharp", octave: 4 },
      { note: "D", accidental: "Natural", octave: 4 },
      { note: "E", accidental: "Natural", octave: 4 },
      { note: "G", accidental: "Natural", octave: 4 },
      { note: "C", accidental: "Natural", octave: 6 },
    ] as const;
    const notes = Array.from({ length: 32 }, (_, beat) => chord.map(pitch =>
      makeNote({ pitch: { ...pitch }, start_beat: beat * 2 }))).flat();
    const thumbs = noteThumbnails(notes, 64);
    expect(thumbs).toHaveLength(224);
    const first = thumbs.slice(0, 7).sort((a, b) => parseFloat(a.top) - parseFloat(b.top));
    for (const areaHeight of [18, 30, 60]) {
      for (let i = 0; i < first.length - 1; i++) {
        const bottom = (parseFloat(first[i].top) + parseFloat(first[i].height)) * areaHeight / 100;
        expect(bottom).toBeLessThan(parseFloat(first[i + 1].top) * areaHeight / 100);
      }
    }
    // Relative semitone distance remains linear, not ordinal pitch rows.
    const c4 = parseFloat(thumbs[1].top), cs4 = parseFloat(thumbs[2].top);
    expect(c4 - cs4).toBeCloseTo(80 / 48);
    expect(parseFloat(thumbs[0].top) - c4).toBeCloseTo((c4 - cs4) * 24);
    expect(thumbs[0].left).toBe("0%");
    expect(thumbs.at(-1)?.left).toBe("96.875%");
  });

  it("shows late chords in a sixteen-bar phrase beyond the former72-note prefix", () => {
    const events = Array.from({ length: 30 }, (_, chord) =>
      Array.from({ length: 4 }, () => makeNote({ start_beat: chord * 2 }))
    ).flat();
    const thumbs = noteThumbnails(events, 64);
    expect(thumbs).toHaveLength(120);
    expect(thumbs.filter(note => parseFloat(note.left) >= 50)).toHaveLength(56);
    expect(thumbs.at(-1)?.left).toBe("90.625%");
  });
  it("bounds dense unsorted previews while retaining both ends of the clip", () => {
    const events = Array.from({ length: 2000 }, (_, index) => makeNote({ start_beat: index })).reverse();
    const thumbs = noteThumbnails(events, 2000);
    expect(thumbs).toHaveLength(512);
    expect(thumbs[0].left).toBe("0%");
    expect(parseFloat(thumbs.at(-1)!.left)).toBeCloseTo(99.95);
    expect(events[0].start_beat).toBe(1999);
  });
  it("clips overlapping sustained notes to the visible slipped interval", () => {
    const events = [
      makeNote({ start_beat: 0, duration: { ...makeNote().duration, base: "Whole" } }),
      makeNote({ start_beat: 5, duration: { ...makeNote().duration, base: "Whole" } }),
      makeNote({ start_beat: 6 }),
      makeNote({ start_beat: 3, muted: true }),
    ];
    const thumbs = noteThumbnails(events, 4, 2);
    expect(thumbs).toHaveLength(2);
    expect(thumbs[0]).toMatchObject({ left: "0%", width: "50%" });
    expect(thumbs[1]).toMatchObject({ left: "75%", width: "25%" });
  });
});


it("shows the same half-open loop window for notes and hits across a partial repeat", () => {
  const sourceBeats = [0, 1, 2, 3, 4];
  const notes = sourceBeats.map(start_beat => makeNote({ start_beat }));
  const hits: HitEvent[] = sourceBeats.map(beat => ({
    drum: "tom_low",
    position: { beat: beat + 1, subdivision: 0 },
    dynamics: null,
    articulation: null,
    modifiers: [],
  }));
  const expected = [0, 1, 2, 3, 4].map(beat => (beat / 4.5) * 100);
  for (const thumbs of [noteThumbnails(notes, 4.5, 1, 2), hitThumbnails(hits, 4.5, 1, 2)]) {
    expect(thumbs.map(thumb => parseFloat(thumb.left))).toEqual(expected);
  }
  const dense = hitThumbnails([hits[1]], 4096, 1, 0.25);
  expect(dense).toHaveLength(96);
  expect(dense[0].left).toBe("0%");
  expect(parseFloat(dense.at(-1)!.left)).toBeGreaterThan(99.99);
});

it("keeps a fine-grid hit before the edge while excluding a shifted legacy anchor", () => {
  const hit: HitEvent = {
    drum: "kick",
    position: { beat: 3, subdivision: 3 },
    dynamics: null,
    articulation: null,
    modifiers: [["grid_offset_beats", { Number: -0.125 }]],
  };
  const legacy: HitEvent = {
    ...hit,
    modifiers: [["timing_offset_beats", { Number: -0.125 }]],
  };
  const thumbs = hitThumbnails([hit, legacy], 2.75, 0, 0, { start: 0, swing: 100 });
  expect(thumbs).toHaveLength(1);
  expect(parseFloat(thumbs[0].left)).toBeCloseTo(2.625 / 2.75 * 100);
});
