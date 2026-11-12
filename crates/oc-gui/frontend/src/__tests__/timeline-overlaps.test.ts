import { beforeEach, describe, expect, it } from "vitest";
import { clockBuildCount, createProjectClock, projectClock, resetClockBuilds } from "../utils/projectClock";
import { layoutStats, overlappingClips, overlapsByTrack, resetLayoutStats } from "../utils/timelineLayout";
import type { BarBeat, ProjectProperties, TimeRange } from "../types/project";

const TRACKS = 20;
const CLIPS_PER_TRACK = 25;
const EDITS = 100;

const properties = (tempo = 120): ProjectProperties => ({
  tempo,
  time_signature: { numerator: 4, denominator: 4 },
  key: null,
  swing: null,
  key_map: [],
  tempo_map: [],
  time_sig_map: [],
});

interface LaneClip {
  time_range: TimeRange | null;
}

const beat = (bar: number): BarBeat => ({ bar, beat: 1, subdivision: 0 });

/** A two-bar clip; every fourth one starts a bar early and overlaps its neighbour. */
function clip(index: number): LaneClip {
  const bar = 1 + index * 2 + (index % 4 === 3 ? -1 : 0);
  return { time_range: { start: beat(bar), end: beat(bar + 2) } };
}

function track(clips: LaneClip[]) {
  return { clips };
}

function arrangement(): ReturnType<typeof track>[] {
  return Array.from({ length: TRACKS }, () =>
    track(Array.from({ length: CLIPS_PER_TRACK }, (_, index) => clip(index))));
}

/**
 * The previous pairwise scan: every clip pair, and a fresh clock for
 * every endpoint lookup. Used only as the benchmark's "before" side.
 */
function pairwiseOverlaps(clips: LaneClip[], props: ProjectProperties) {
  const overlaps = new Set<number>();
  let comparisons = 0;
  let builds = 0;
  // `barBeatToBeats` built a clock for every endpoint, as the removed scan did.
  const quarterOf = (position: BarBeat) => {
    builds += 1;
    return createProjectClock(props).quarterAt(position);
  };
  for (let i = 0; i < clips.length; i++) {
    const a = clips[i];
    if (!a.time_range) continue;
    const aStart = quarterOf(a.time_range.start);
    const aEnd = quarterOf(a.time_range.end);
    for (let j = i + 1; j < clips.length; j++) {
      const b = clips[j];
      if (!b.time_range) continue;
      comparisons += 1;
      if (aStart < quarterOf(b.time_range.end) && quarterOf(b.time_range.start) < aEnd) {
        overlaps.add(i);
        overlaps.add(j);
      }
    }
  }
  return { overlaps, comparisons, builds };
}

beforeEach(() => {
  resetClockBuilds();
  resetLayoutStats();
});

describe("timeline overlap layout", () => {
  it("reports the same clips as the pairwise definition in any insertion order", () => {
    const props = properties();
    const clock = projectClock(props);
    const clips: LaneClip[] = [
      { time_range: null },
      { time_range: { start: beat(1), end: beat(3) } },
      { time_range: { start: beat(2), end: beat(4) } },  // overlaps 1
      { time_range: { start: beat(4), end: beat(6) } },  // touches 2's end, no overlap
      { time_range: { start: beat(4), end: beat(9) } },  // overlaps 3
      { time_range: { start: beat(5), end: beat(5) } },  // empty span inside 4
      { time_range: { start: beat(12), end: beat(14) } },
    ];
    // A span inside an earlier one overlaps it, an empty span included.
    expect([...overlappingClips(clips, clock)].sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5]);

    const reversed = [...clips].reverse();
    expect([...overlappingClips(reversed, clock)].sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5]);

    for (const order of [clips, reversed]) {
      const expected = pairwiseOverlaps(order, props).overlaps;
      expect([...overlappingClips(order, clock)].sort((a, b) => a - b))
        .toEqual([...expected].sort((a, b) => a - b));
    }
  });

  it("keeps an empty span out of a span that starts with it", () => {
    const clock = projectClock(properties());
    const empty: LaneClip = { time_range: { start: beat(4), end: beat(4) } };
    const long: LaneClip = { time_range: { start: beat(4), end: beat(9) } };
    expect([...overlappingClips([empty, long], clock)]).toEqual([]);
    expect([...overlappingClips([long, empty], clock)]).toEqual([]);
    const earlier: LaneClip = { time_range: { start: beat(1), end: beat(9) } };
    expect([...overlappingClips([earlier, empty], clock)].sort((a, b) => a - b)).toEqual([0, 1]);
  });

  it("builds one clock per revision and reuses it", () => {
    const props = properties();
    const clock = projectClock(props);
    for (let i = 0; i < 1000; i++) expect(projectClock(props)).toBe(clock);
    expect(clockBuildCount()).toBe(1);

    // A snapshot hands the store a new properties object, so the next revision
    // builds once more; the memoized clock still matches a direct build.
    const next = properties(140);
    expect(projectClock(next)).not.toBe(clock);
    expect(clockBuildCount()).toBe(2);
    expect(projectClock(next).tempoAt(16)).toBe(140);
    expect(projectClock(next).secondsAt(8)).toBe(createProjectClock(next).secondsAt(8));
  });

  it("neither caches nor counts a clock it could not build", () => {
    const invalid = properties();
    invalid.tempo = 0;
    expect(() => projectClock(invalid)).toThrow("Invalid project clock");
    expect(() => projectClock(invalid)).toThrow("Invalid project clock");
    expect(clockBuildCount()).toBe(0);
  });

  it("scans 500 clips across 20 tracks once per edit, not per pair", () => {
    const tracks = arrangement();
    const clips = tracks.reduce((sum, lane) => sum + lane.clips.length, 0);
    expect(clips).toBe(TRACKS * CLIPS_PER_TRACK);

    // Before: one pairwise pass per lane, two clocks per compared pair.
    const before = tracks.reduce((total, lane) => {
      const scan = pairwiseOverlaps(lane.clips, properties());
      return {
        comparisons: total.comparisons + scan.comparisons,
        builds: total.builds + scan.builds,
      };
    }, { comparisons: 0, builds: 0 });
    expect(before.comparisons).toBe(TRACKS * (CLIPS_PER_TRACK * (CLIPS_PER_TRACK - 1)) / 2);
    expect(before.builds).toBe(TRACKS * (2 * CLIPS_PER_TRACK + 2 * before.comparisons / TRACKS));

    // After: one clock and one scan for the whole arrangement, per edit.
    resetClockBuilds();
    resetLayoutStats();
    const started = performance.now();
    for (let edit = 0; edit < EDITS; edit++) {
      const lane = tracks[edit % TRACKS];
      const moved = lane.clips[edit % CLIPS_PER_TRACK];
      if (moved.time_range) {
        const start = moved.time_range.start.bar + 1;
        moved.time_range = { start: beat(start), end: beat(start + 2) };
      }
      overlapsByTrack(tracks, projectClock(properties()));
    }
    const elapsed = performance.now() - started;

    expect(clockBuildCount()).toBe(EDITS);
    expect(layoutStats().scans).toBe(EDITS);
    // One clock per edit instead of one per endpoint, and a sweep that examines
    // far fewer pairs than the 6 000 a pairwise pass compares.
    expect(before.builds).toBe(13_000);
    expect(layoutStats().comparisons / EDITS).toBeLessThan(before.comparisons / 4);
    // 100 edits of 500 clips stay far below a frame's worth of work per edit.
    expect(elapsed / EDITS).toBeLessThan(5);
  });

  it("keeps one overlap set per track for the lanes to read", () => {
    const props = properties();
    const tracks = arrangement();
    const sets = overlapsByTrack(tracks, projectClock(props));
    expect(sets).toHaveLength(TRACKS);
    for (const lane of sets) expect(lane.size).toBeGreaterThan(0);
    // Overlaps are per lane: a clip overlapping nothing in its own lane stays out.
    const sparse = overlapsByTrack([track([clip(0), clip(1), clip(2), clip(4)])], projectClock(props));
    expect([...sparse[0]].sort((a, b) => a - b)).toEqual([]);
  });
});
