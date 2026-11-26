import type { Track } from "../types/project";

/** A human label never changes the identifier used by routes and mutations. */
export function trackDisplayName(track: Pick<Track, "name" | "mix">): string {
  return track.mix.overrides?.display_name?.trim() || track.name;
}
