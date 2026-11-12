/**
 * Meter snapshot contract shared with the `meter_snapshot` Tauri command.
 * Levels are dBFS and `null` for silence or nodes without that tap.
 */

export interface NodeMeter {
  gain_reduction_db: number;
  /** `null` for a node without an input tap (bus/master input). */
  input_peak_db?: number | null;
  output_peak_db?: number | null;
  /** Mean level of the same tap; `null` for silence. */
  output_rms_db?: number | null;
  /** Latched until `reset_meter_clip` or the channel is gated; a sample peak reached full scale. */
  clipped: boolean;
  /** Signed band gain per effect, in dB; absent for devices without bands. */
  effect_bands?: number[][];
  effects: number[];
  /** Peak gain reduction per effect row since reset. */
  effects_max_gain_reduction_db?: number[];
  /** Per-effect ceiling headroom; `null` for devices without a ceiling. */
  effects_ceiling_margin_db?: (number | null)[];
}

export interface TrackMeter extends NodeMeter {
  index: number;
}

export interface BusMeter extends NodeMeter {
  name: string;
}

/** The master row also carries loudness, correlation and the stereo image. */
export interface MasterMeter extends NodeMeter {
  /** Short-term loudness over `short_term_window_seconds`; `null` while the
   * window fills, when it is silent, or when the transport is stopped. */
  short_term_lufs?: number | null;
  short_term_window_seconds?: number;
  /** Left/right correlation read ahead of the mono fold. */
  correlation?: number | null;
  /** Interleaved left/right pairs of the latest chunk, oldest first. */
  goniometer?: number[];
}

/** The MONO control's request and what the render thread last applied. */
export interface MonoCheck {
  requested: boolean;
  applied: boolean;
}

/** dBFS per display band, 20 Hz to 20 kHz, low to high; present while open. */
export interface SpectrumMeter {
  bands: number[];
}

export interface MeterSnapshot {
  available: boolean;
  /** Why meters are unavailable, when the engine can say. */
  reason?: string | null;
  playing: boolean;
  /** Channel count of the monitored master; `0` means no graph is loaded. */
  master_channels: number;
  mono: MonoCheck;
  master: MasterMeter;
  tracks: TrackMeter[];
  buses: BusMeter[];
  spectrum?: SpectrumMeter | null;
}

/** Resolved meter for one effect row in the device panel. */
export interface EffectMeter {
  available: boolean;
  playing: boolean;
  gainReductionDb: number;
  maxGainReductionDb: number;
  /** Positive below the ceiling, negative above it. */
  ceilingMarginDb: number | null;
  inputPeakDb: number | null;
  outputPeakDb: number | null;
}

/** Resolved levels for one mixer channel. */
export interface ChannelMeter {
  available: boolean;
  playing: boolean;
  peakDb: number | null;
  rmsDb: number | null;
  clipped: boolean;
  /** Why nothing is being measured, when the engine says. */
  reason: string | null;
}

/** Resolved master monitoring row. */
export interface MasterMonitor {
  available: boolean;
  playing: boolean;
  meter: ChannelMeter | null;
  lufs: number | null;
  windowSeconds: number;
  correlation: number | null;
  goniometer: number[];
  mono: MonoCheck;
  /** True while a mono request has not reached a rendered chunk. */
  monoPending: boolean;
  masterChannels: number;
  reason: string | null;
}
