import type { ProjectSnapshot } from "./project";

/** Chunks of a project rebuild, as the render thread reports them. */
export interface BuildSnapshot {
  stage: "idle" | "reading" | "instruments" | "arrangement" | "effects" | "soundfont";
  /** The instrument being prepared, empty outside the per-instrument stage. */
  label: string;
  done: number;
  total: number;
}

export interface AuditionSnapshot {
  paused?: boolean;
  finished?: boolean;
  position_seconds?: number;
  preset: string | null;
  status: "idle" | "loading" | "ready" | "error";
  ready_ms: number | null;
  instances: number;
  loads: number;
  error: string | null;
}

/** A level-matched A/B pair, as measured over the fixed audition window. */
export interface AbSnapshot {
  measurement_pending?: boolean;
  status: "matched" | "unmeasured";
  /** The take currently selected on the audition stream. */
  active: "a" | "b";
  a_label: string;
  b_label: string;
  a_lufs: number | null;
  b_raw_lufs: number | null;
  b_lufs: number | null;
  raw_delta_lu: number | null;
  applied_gain_db: number | null;
  matched_delta_lu: number | null;
  /** `a`, `b`, `both` or `unstable` when the pair could not be matched. */
  unmeasured: "a" | "b" | "both" | "unstable" | null;
  window_seconds: number;
}

// TransportSnapshot matches the Rust struct
export interface TransportSnapshot {
  playing: boolean;
  position_beats: number;
  position_seconds: number;
  duration_seconds: number;
  did_render?: boolean;
  warnings?: string[];
  starting?: boolean;
  track_preview_rebuild?: boolean;
  status?: string | null;
  /** The project is still being built or pre-rendered in the background. */
  warming?: boolean;
  /** The project is fully warm: sources built and the opening chunks queued. */
  prepared?: boolean;
  /** Stage and progress of a project rebuild the render thread is running. */
  build?: BuildSnapshot | null;
  /** Current preset audition progress. */
  audition?: AuditionSnapshot | null;
  /** An audition is still sounding; the transport may be paused. */
  audition_active?: boolean;
  native_monitoring?: boolean;
  live_synth_sounding?: boolean;
  /**
   * Seconds the live monitoring path holds the reported position back by
   * (device period plus the monitored chain). The position fields already
   * include it, so the cursor must not add it again.
   */
  monitor_latency_seconds?: number;
  /** The audition click's settings, as the transport controls show them. */
  click?: {
    enabled: boolean;
    level_db: number;
    count_in: "off" | "one_bar" | "two_bars";
    count_in_available: boolean;
  };
  /** Count-in frames still to sound before the timeline starts. */
  count_in_remaining_frames?: number;
  /** The armed track, the take in progress and the last record report. */
  record?: {
    armed: string | null;
    recording: boolean;
    seconds: number;
    report: string | null;
    disabled_reason: string | null;
    count_in_remaining_frames: number;
    /** What the armed track records. */
    mode?: "audio" | "midi";
    /** The MIDI half: target pattern, quantise, overdub and the input. */
    midi?: {
      target: string | null;
      quantise: Quantise;
      overdub: boolean;
      input: string | null;
    };
    /** The capture device an audio take would use. */
    audio_device?: string | null;
  };
  /** The punch range a take keeps notes inside. */
  punch?: PunchState;
  /**
   * The project as it stands after a stop that committed a take. Present only
   * when the stop changed the project, so the editors pick up the new clip or
   * notes without a reload.
   */
  project?: ProjectSnapshot;
}

/** Input quantise for a MIDI take; ids match `midi_record::Quantise`. */
export type Quantise = "off" | "thirty_second" | "sixteenth" | "eighth" | "quarter";

/** The punch range. Session state, like the loop region. */
export interface PunchState {
  enabled: boolean;
  start_beats: number | null;
  end_beats: number | null;
}

/** The session loop region. Playback only; it is never part of the project. */
export interface LoopState {
  enabled: boolean;
  start_beats: number | null;
  end_beats: number | null;
  /**
   * The project end cuts the region short: playback wraps at the project end
   * instead of at `end_beats`, which stays the region the user drew.
   */
  clamped?: boolean;
}
