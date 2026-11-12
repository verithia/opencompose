import { describe, expect, it } from "vitest";
import { makeAudioClip, makeSnapshot } from "../setup";
import { audioClipBounds } from "../../utils/audioClipPlacement";
import { createProjectClock } from "../../utils/projectClock";

describe("audio clip placement bounds", () => {
  it("defaults old documents to their musical endpoints", () => {
    const clock = createProjectClock(makeSnapshot().project.properties);
    const clip = makeAudioClip();
    const bounds = audioClipBounds(clock, clip);
    expect(bounds.startQuarter).toBe(0);
    expect(bounds.endQuarter).toBe(8);
    expect(bounds.durationSeconds).toBe(clock.secondsAt(8));
  });

  it("supports long exact offsets and equal musical anchors", () => {
    const clock = createProjectClock({ ...makeSnapshot().project.properties, tempo: 120 });
    const clip = makeAudioClip({
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 1, beat: 1, subdivision: 0 } },
      placement: { start_offset_seconds: 3600.001, end_offset_seconds: 3600.126 },
    });
    const bounds = audioClipBounds(clock, clip);
    expect(bounds.startSeconds).toBe(3600.001);
    expect(bounds.durationSeconds).toBeCloseTo(0.125);
    expect(bounds.startQuarter).toBeCloseTo(7200.002);
    expect(bounds.endQuarter - bounds.startQuarter).toBeCloseTo(0.25);
  });

  it("converts each offset in seconds across a tempo change without slipping source audio", () => {
    const properties = { ...makeSnapshot().project.properties, tempo: 120,
      tempo_map: [{ position: { bar: 2, beat: 1, subdivision: 0 }, bpm: 60 }],
    };
    const clock = createProjectClock(properties);
    const clip = makeAudioClip({ offset_seconds: 0.7,
      placement: { start_offset_seconds: 0.125, end_offset_seconds: -0.25 },
    });
    const bounds = audioClipBounds(clock, clip);
    expect(bounds.startSeconds).toBe(0.125);
    expect(bounds.endSeconds).toBe(5.75);
    expect(bounds.startQuarter).toBe(0.25);
    expect(bounds.endQuarter).toBe(7.75);
    expect(bounds.durationSeconds).toBe(5.625);
    expect(clip.offset_seconds).toBe(0.7);
  });
});
