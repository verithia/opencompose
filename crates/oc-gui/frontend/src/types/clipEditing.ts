import type { AudioWarpMarker, BarBeat, GainEnvelopePoint, ProjectSnapshot } from "./project";

/** Where a bounce puts its clip. */
export type BounceMode = "new_track" | "in_place";

/** How a bounce treats the sound that outlasts its range end. */
export type BounceTail = "include_release" | "cut_exactly";

export interface BounceResult {
  project: ProjectSnapshot;
  /** One status line naming the media, the range and the baked inserts. */
  report: string;
  clip: string;
  track: string;
  /** Track that holds the bounced clip. */
  target_track: string;
  media: string;
  path: string;
  mode: BounceMode;
  frames: number;
  sample_rate: number;
  /** Insert effects the bounce baked. */
  baked: string[];
}

export interface ConsolidateResult {
  project: ProjectSnapshot;
  report: string;
  clip: string;
  track: string;
  media: string;
  path: string;
  /** Audio clips the new clip replaced. */
  clips: number;
  frames: number;
  sample_rate: number;
}
/** The part of an audio clip's gain/fade envelope a gesture changed. */
export interface AudioClipPlayback {
  formant?: number;
  preserve_formants?: boolean;
  warp_markers?: AudioWarpMarker[];
  stretch_to?: BarBeat;
  stretch?: number;
  pitch?: number;
  stretch_mode?: "off" | "percussive" | "musical" | "varispeed";
  tempo_follow?: boolean;
  bypass_processing?: boolean;
}
export interface AudioClipEnvelope {
  gain_db?: number;
  fade_in_seconds?: number;
  fade_out_seconds?: number;
  crossfade_seconds?: number;
  gain_envelope?: GainEnvelopePoint[];
}
export interface AudioClipPlacement {
  start_seconds?: number;
  nudge_ms?: number;
}
