import type { TimeRange } from "../types/project";
import type { ProjectClock } from "./projectClock";

/** What the last scans since `resetLayoutStats` cost. */
export interface LayoutStats {
  /** Scans started: one per document change that recomputes the overlap set. */
  scans: number;
  /** Span pairs the sweep examined. */
  comparisons: number;
}

let stats: LayoutStats = { scans: 0, comparisons: 0 };

/** Overlap work since the last reset; the timeline layout benchmark reads it. */
export function layoutStats(): LayoutStats { return { ...stats }; }

export function resetLayoutStats(): void { stats = { scans: 0, comparisons: 0 }; }

/** One clip span in quarter notes, tagged with the index it came from. */
interface Span {
  start: number;
  end: number;
  index: number;
}

function project(clips: readonly { time_range: TimeRange | null }[], clock: ProjectClock): Span[] {
  const spans: Span[] = [];
  for (let index = 0; index < clips.length; index++) {
    const range = clips[index].time_range;
    if (!range) continue;
    spans.push({ start: clock.quarterAt(range.start), end: clock.quarterAt(range.end), index });
  }
  return spans;
}

/**
 * Sort-and-sweep overlap detection. It reports the same set as the pairwise
 * definition `a.start < b.end && b.start < a.end`, in O(n log n) instead of
 * O(n²). `open` holds the spans that still reach the current one; a span that
 * has already ended never reaches a later start, so it leaves the sweep.
 */
function sweep(spans: Span[]): Set<number> {
  spans.sort((a, b) => a.start - b.start);
  const overlaps = new Set<number>();
  const open: Span[] = [];
  for (const span of spans) {
    let kept = 0;
    for (let i = 0; i < open.length; i++) {
      stats.comparisons += 1;
      const running = open[i];
      if (running.end <= span.start) continue;
      open[kept++] = running;
      // The left inequality holds for every running span; the right one fails
      // only for an empty span that starts with the running one.
      if (running.start < span.end) {
        overlaps.add(running.index);
        overlaps.add(span.index);
      }
    }
    open.length = kept;
    open.push(span);
  }
  return overlaps;
}

/** Clip indices of one lane that overlap another clip of that lane. */
export function overlappingClips(
  clips: readonly { time_range: TimeRange | null }[],
  clock: ProjectClock,
): Set<number> {
  stats.scans += 1;
  return sweep(project(clips, clock));
}

/** One overlap set per track: the whole arrangement in a single scan. */
export function overlapsByTrack(
  tracks: readonly { clips: readonly { time_range: TimeRange | null }[] }[],
  clock: ProjectClock,
): Set<number>[] {
  stats.scans += 1;
  return tracks.map((track) => sweep(project(track.clips, clock)));
}
