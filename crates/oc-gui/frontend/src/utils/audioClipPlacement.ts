import type { AudioClip } from "../types/project";
import type { ProjectClock } from "./projectClock";

/** Fine placement changes timeline position without slipping the source media. */
export function audioClipBounds(clock: ProjectClock, clip: AudioClip) {
  const startSeconds = clock.secondsAt(clock.quarterAt(clip.time_range.start))
    + (clip.placement?.start_offset_seconds ?? 0);
  const endSeconds = clock.secondsAt(clock.quarterAt(clip.time_range.end))
    + (clip.placement?.end_offset_seconds ?? 0);
  return {
    startSeconds,
    endSeconds,
    durationSeconds: endSeconds - startSeconds,
    startQuarter: clock.quarterAtSeconds(startSeconds),
    endQuarter: clock.quarterAtSeconds(endSeconds),
  };
}
