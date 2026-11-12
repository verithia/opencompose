/**
 * TypeScript types mirroring the Rust AST (opencompose_dsl::ast).
 * All types derive Serialize/Deserialize in Rust, so Tauri IPC
 * sends these as JSON. Keep in sync with ast.rs.
 */

// ============================================================================
// Top-Level Project
// ============================================================================

export interface Project {
  name: string | null;
  ocp_version?: string | null;
  properties: ProjectProperties;
  presets: PresetDef[];
  patterns: PatternDef[];
  tracks: Track[];
  buses: Bus[];
  master: Master | null;
  sections: Section[];
  /**
   * Named points on the timeline. Annotations only: they never change what
   * renders, and the backend omits the field when the project has none.
   */
  markers?: Marker[];
  /**
   * Clip groups. Editing-only: a group binds the move and the selection of the
   * clips it lists; the backend omits the field when the project has none.
   */
  groups?: ClipGroup[];
}

// ============================================================================
// Preset Definition
// ============================================================================

export interface PresetDef {
  name: string;
  source: string | null;
  plugin: string | null;
  program: number | null;
  patch: string | null;
  file: string | null;
  root_note: Pitch | null;
  extends: string | null;
  params: [string, ParamValue][];
  effects: Effect[];
}

export type PictureRate = "24000/1001" | "24" | "25" | "30000/1001" | "30" | "48000/1001" | "48" | "50" | "60000/1001" | "60";

export interface PictureClock {
  rate: PictureRate;
  origin: string;
}

export interface ProjectProperties {
  picture_clock?: PictureClock | null;
  tempo: number;
  time_signature: TimeSignature;
  key: KeySignature | null;
  swing: number | null;
  /** Grid `swing` moves: off-beat eighths unless `sixteenth` is set. */
  swing_subdivision?: "eighth" | "sixteenth";
  tempo_map: TempoPoint[];
  time_sig_map: TimeSigPoint[];
  key_map: KeyPoint[];
  audio_clips?: AudioClip[];
  /** One-to-many control fan-outs; missing means the project has none. */
  control_links?: ControlLink[];
  /** Audio-derived control signals a device control input can read. */
  control_sources?: ControlSource[];
  /** Export choices saved with the project; missing keeps the app defaults. */
  export?: ExportSettings | null;
}

/** Sample depth an integer export writes. */
export type ExportDepth = "i16" | "i24" | "f32";

/** Sample depth FLAC can store. */
export type FlacDepth = "i16" | "i24";

/** FLAC container options: depth and compression level. */
export interface FlacSettings {
  depth: FlacDepth;
  level: number;
}
/** MP3 container options: a CBR bitrate or a VBR quality index. */
export interface Mp3Settings {
  /** CBR bitrate in kbps; ignored by a VBR encode. */
  bitrate: number;
  /** VBR quality index, 0 best to 9 smallest; missing is CBR. */
  vbr_quality?: number | null;
}

/**
 * The project's `export:` block. The dialog writes it, the engine reads it and
 * the DSL editor shows it.
 */
export interface ExportSettings {
  container: "wav" | "flac" | "mp3";
  wav: { depth: ExportDepth; dither: boolean };
  /** Present when a FLAC choice differs from the defaults. */
  flac?: FlacSettings;
  /** Present when an MP3 choice differs from the defaults. */
  mp3?: Mp3Settings;
  /** Export rate in hertz; missing leaves the caller's choice. */
  sample_rate?: number | null;
  /** Render resampling step, 0 fastest to 7 best. */
  resample?: number | null;
}

/** A named control signal produced by one track's post-insert audio. */
export interface ControlSource {
  name: string;
  source_track: string;
  attack: number;
  release: number;
  lfo?: { shape: "Sine" | "Triangle" | "Square" | "Saw"; rate_hz: number } | null;
  lfo_depth?: number;
  formula?: string | null;
  scale?: number;
  offset?: number;
  curve?: "Linear" | "Squared" | "Sqrt" | "Log";
}

/** A named mapping applied to one fan-out row. */
export type LinkFormula =
  | "linear"
  | "inverted"
  | "exponential"
  | "logarithmic"
  | "midi_unit"
  | "midi_centered";

/** Where the control value comes from; `channel` is 1-based. */
export type ControlLinkSource =
  | { kind: "midi_cc"; channel: number; controller: number }
  | { kind: "macro"; index: number }
  | { kind: "lane"; track: string; param: string };

/** One parameter a link drives, with its own formula and smoothing. */
export interface ControlLinkTarget {
  track: string;
  param: string;
  formula: LinkFormula;
  /** One-pole smoothing time in milliseconds; 0 disables smoothing. */
  smoothing_ms: number;
}

/** One control fanning out to many targets. */
export interface ControlLink {
  name: string;
  source: ControlLinkSource;
  targets: ControlLinkTarget[];
}

/** Project-owned audio placement on a track, in the same range grammar as clips. */
/** One point of a clip's optional gain envelope. */
export interface GainEnvelopePoint {
  /** Normalised clip position; 0 is the head and 1 the tail. */
  position: number;
  /** Relative gain in dB; 0 leaves the clip gain unchanged. */
  gain_db: number;
}

export interface AudioWarpMarker {
  source_seconds: number;
  target_seconds: number;
}

export interface AudioClip {
  /** Fine endpoint placement relative to the musical anchors, in seconds. */
  placement?: { start_offset_seconds: number; end_offset_seconds: number };
  name: string;
  track: string;
  /** Media path relative to the project file. */
  file: string;
  time_range: TimeRange;
  /** Seconds trimmed from the decoded media start. */
  offset_seconds: number;
  /** Clip gain in dB. */
  gain_db: number;
  fade_in_seconds: number;
  fade_out_seconds: number;
  /** Equal-power crossfade at the clip head and tail, in seconds. */
  crossfade_seconds?: number;
  /** Optional point envelope layered on top of `gain_db`; absent means none. */
  gain_envelope?: GainEnvelopePoint[];
  /** Output duration is media duration x stretch; 1.0 bypasses the stretcher. */
  stretch?: number;
  stretch_mode?: "off" | "percussive" | "musical" | "varispeed";
  tempo_follow?: boolean;
  bypass_processing?: boolean;
  /** Semitone transposition, fractional allowed; 0 leaves the media alone. */
  pitch?: number;
  formant?: number;
  preserve_formants?: boolean;
  warp_markers?: AudioWarpMarker[];
  /** Seconds of media the clip repeats from its offset; absent is a single pass. */
  loop_seconds?: number;
  /** Passes of one loop session share this identifier; absent outside a take. */
  take_group?: string;
  /** One-based pass number inside `take_group`. */
  take?: number;
  /** A muted take keeps its media and position but contributes no audio. */
  muted?: boolean;
}

export interface TempoPoint {
  position: BarBeat;
  offset_quarters?: number;
  bpm: number;
}

export interface TimeSigPoint {
  position: BarBeat;
  numerator: number;
  denominator: number;
}

export interface KeyPoint {
  position: BarBeat;
  root: NoteName;
  accidental: Accidental;
  mode: string;
}

// ============================================================================
// Track
// ============================================================================

export interface Track {
  name: string;
  instrument: InstrumentPath;
  preset: string | null;
  engine: string | null;
  mix: MixState;
  effects: Effect[];
  phrases: Phrase[];
  patterns: Pattern[];
  inline_patterns: PatternDef[];
  clips: Clip[];
  automation: AutomationLane[];
  engine_params: [string, ParamValue][] | null;
  performance?: { events: PerformanceEvent[] } | null;
  /** Instrument output port this track takes; absent is port 0. */
  output_port?: number | null;
  /** Stereo pair inside a wider port; absent is the port's default pair. */
  output_pair?: number | null;
  /** Device-input routes owned by this track's effect chain. */
  input_routes?: InputRoute[];
}

/** What a device input route carries: audio (default) or a control signal. */
export type RouteKind = "audio" | "control";

/**
 * One route into a device input. `target` addresses an effect the way
 * automation lanes do (`compression`, `fx2`); `source` is a track name for an
 * audio route and a control-source name for a control route.
 */
export interface InputRoute {
  kind: RouteKind;
  source: string;
  target: string;
}

/** One declared output port of a preset's instrument. */
export interface PluginPortInfo {
  channels: number;
  first_pair: number;
  pairs: number;
}

export interface PerformanceEvent {
  quarter: number;
  channel: number;
  message:
    | { type: "note_on" | "note_off"; note: number; velocity: number }
    | { type: "control_change"; controller: number; value: number }
    | { type: "program_change"; program: number }
    | { type: "pitch_bend"; value: number }
    | { type: "channel_pressure"; pressure: number }
    | { type: "poly_pressure"; note: number; pressure: number };
}

export interface InstrumentPath {
  category: string;
  name: string;
}

// ============================================================================
// Mix
// ============================================================================

export interface MixState {
  output?: string | null;
  volume_db: number;
  pan: number;
  stereo_balance?: boolean;
  sends: Send[];
  mute: boolean;
  solo: boolean;
  /** Channel-input polarity inversion; post-insert, pre-fader. */
  polarity_inverted?: boolean;
  overrides?: ChannelOverrides;
  /** Extra output latency in samples, added to the plugin-reported latency. */
  latency_offset?: number;
}

export interface ChannelOverrides {
  display_name?: string | null;
  color?: string | null;
  solo_safe?: boolean;
}

export interface Send {
  target: string;
  amount: number;
  /** Plugin auxiliary output feeding this send; absent for the channel tap. */
  source?: { owner: string; output: number } | null;
  /** Tap before the channel fader and pan; false/absent taps after both. */
  pre_fader?: boolean;
  /** Balance at the bus input, -1..1; absent/0 is unity. */
  pan?: number;
}

// ============================================================================
// Effects
// ============================================================================

export interface Effect {
  effect_type: EffectType;
  params: [string, ParamValue][];
  /** Takes the effect out of the chain: no processing and no latency. */
  bypass?: boolean;
  /** Detector key source: another track or bus name, or null for self. */
  sidechain_source?: string | null;
}

export type EffectType =
  | "Eq"
  | "DynamicEq"
  | "Compression"
  | "DeEsser"
  | "BreathSplit"
  | "Multiband"
  | "Leveler"
  | "Opto"
  | "Fet"
  | "Iron"
  | "PassiveEq"
  | "Doubler"
  | "Exciter"
  | "DuckedSend"
  | "Resonance"
  | "Tuner"
  | "Reverb"
  | "ReverbFdn"
  | "Delay"
  | "Distortion"
  | "Chorus"
  | "Flanger"
  | "Phaser"
  | "Limiter"
  | "Gate"
  | "Saturator"
  | "StereoWidth"
  | "Gain"
  | { Plugin: string };

// ============================================================================
// Phrase (pitched instruments)
// ============================================================================

export interface Phrase {
  name: string;
  time_range: TimeRange | null;
  events: NoteEvent[];
  /** Sustain-pedal marks, in beats from the phrase start; absent when empty. */
  pedals?: PedalMark[];
  /** Hairpin marks, in beats from the phrase start; absent when empty. */
  hairpins?: HairpinMark[];
}

/** A sustain-pedal mark's position inside its owning phrase or pattern. */
export interface PedalMark {
  beat: number;
  down: boolean;
}

/** A hairpin's position inside its owning phrase; the kind spells the wedge. */
export interface HairpinMark {
  beat: number;
  kind: "crescendo" | "diminuendo";
}

export interface TimeRange {
  start: BarBeat;
  end: BarBeat;
}

export interface BarBeat {
  bar: number;
  beat: number;
  subdivision: number;
}

// ============================================================================
// Note Event
// ============================================================================

export interface NoteEvent {
  start_beat: number;
  explicit_position: boolean;
  pitch: Pitch;
  duration: Duration;
  dynamics: Dynamics | null;
  velocity: number | null;
  articulation: Articulation | null;
  vibrato: VibratoParams | null;
  expression: ExpressionCurve[] | null;
  pitch_bend: number | null;
  muted: boolean;
  grace: boolean;
  text: string | null;
  modifiers: [string, ParamValue][];
}

export interface Pitch {
  note: NoteName;
  accidental: Accidental;
  octave: number;
}

export type NoteName = "C" | "D" | "E" | "F" | "G" | "A" | "B";

/** Where a batch note edit acts: one track's phrases, or one named pattern. */
export type NoteScope =
  | { kind: "track"; track_index: number }
  | { kind: "pattern"; pattern_name: string };

/** One note inside a scope; a pattern scope ignores `phrase_index`. */
export interface NoteRef {
  phrase_index: number;
  note_index: number;
}

/** One note a batch move retargets; a null pitch keeps the written one. */
export interface NoteMove extends NoteRef {
  new_start_beat: number;
  new_pitch?: Pitch | null;
}

/** One copied note a batch paste writes; a pattern scope ignores `phrase_index`. */
export interface NoteInsert {
  phrase_index: number;
  note: NoteEvent;
}

/** One note a batch velocity write retargets. */
export interface NoteVelocity extends NoteRef {
  velocity: number;
}

export type Accidental =
  | "Natural"
  | "Sharp"
  | "DoubleSharp"
  | "Flat"
  | "DoubleFlat";

// ============================================================================
// Duration
// ============================================================================

export interface Duration {
  base: BaseDuration;
  dots: number;
  triplet: boolean;
  tuplet: [number, number] | null;
  ties: BaseDuration[];
}

export type BaseDuration =
  | "Whole"
  | "Half"
  | "Quarter"
  | "Eighth"
  | "Sixteenth"
  | "ThirtySecond"
  | "SixtyFourth";

// ============================================================================
// Dynamics & Articulation
// ============================================================================

export type Dynamics =
  | "PPP" | "PP" | "P" | "MP" | "MF" | "F" | "FF" | "FFF";

export type Articulation =
  | "Legato" | "Staccato" | "Staccatissimo" | "Tenuto" | "Marcato"
  | "Accent" | "Sustained" | "Detached"
  | "Pizzicato" | "Arco" | "Tremolo" | "Trill" | "ColLegno"
  | "Harmonics" | "SulTasto" | "SulPonticello"
  | "Muted" | "Open" | "FlutterTongue" | "Sforzando"
  | "PalmMute" | "HammerOn" | "PullOff" | "Slide" | "Bend"
  | "GhostNote" | "Flam" | "Roll" | "RimShot";

/**
 * Every articulation the DSL can write, in `Articulation::ALL` order, with the
 * DSL token the serializer emits and a two-letter tag the roll draws on a note.
 */
export const ARTICULATIONS: readonly { value: Articulation; token: string; tag: string }[] = [
  { value: "Legato", token: "legato", tag: "lg" },
  { value: "Staccato", token: "staccato", tag: "st" },
  { value: "Staccatissimo", token: "staccatissimo", tag: "ss" },
  { value: "Tenuto", token: "tenuto", tag: "tn" },
  { value: "Marcato", token: "marcato", tag: "mc" },
  { value: "Accent", token: "accent", tag: "ac" },
  { value: "Sustained", token: "sustained", tag: "su" },
  { value: "Detached", token: "detached", tag: "dt" },
  { value: "Pizzicato", token: "pizzicato", tag: "pz" },
  { value: "Arco", token: "arco", tag: "ar" },
  { value: "Tremolo", token: "tremolo", tag: "tm" },
  { value: "Trill", token: "trill", tag: "tr" },
  { value: "ColLegno", token: "col_legno", tag: "cl" },
  { value: "Harmonics", token: "harmonics", tag: "hm" },
  { value: "SulTasto", token: "sul_tasto", tag: "ta" },
  { value: "SulPonticello", token: "sul_ponticello", tag: "po" },
  { value: "Muted", token: "muted", tag: "mu" },
  { value: "Open", token: "open", tag: "op" },
  { value: "FlutterTongue", token: "flutter_tongue", tag: "ft" },
  { value: "Sforzando", token: "sforzando", tag: "sf" },
  { value: "PalmMute", token: "palm_mute", tag: "pm" },
  { value: "HammerOn", token: "hammer_on", tag: "ho" },
  { value: "PullOff", token: "pull_off", tag: "pl" },
  { value: "Slide", token: "slide", tag: "sl" },
  { value: "Bend", token: "bend", tag: "bd" },
  { value: "GhostNote", token: "ghost_note", tag: "gn" },
  { value: "Flam", token: "flam", tag: "fl" },
  { value: "Roll", token: "roll", tag: "rl" },
  { value: "RimShot", token: "rim_shot", tag: "rs" },
];

/** The articulation a DSL token names, if any. */
export function articulationForToken(token: string): Articulation | null {
  return ARTICULATIONS.find((entry) => entry.token === token)?.value ?? null;
}

/** The two-letter tag the roll draws for an articulation; empty when there is none. */
export function articulationTag(articulation: Articulation | null | undefined): string {
  if (!articulation) return "";
  return ARTICULATIONS.find((entry) => entry.value === articulation)?.tag ?? "";
}

/** The DSL token of an articulation; empty when there is none. */
export function articulationToken(articulation: Articulation | null | undefined): string {
  if (!articulation) return "";
  return ARTICULATIONS.find((entry) => entry.value === articulation)?.token ?? "";
}

// ============================================================================
// Expression & Vibrato
// ============================================================================

export interface VibratoParams {
  rate_hz: number | null;
  depth_cents: number | null;
  delay_s: number | null;
}

export interface ExpressionCurve {
  param: string;
  start: ParamValue;
  end: ParamValue | null;
}

// ============================================================================
// Pattern (drums)
// ============================================================================

export interface Pattern {
  name: string;
  time_range: TimeRange | null;
  repeat: number;
  hits: HitEvent[];
}

// ============================================================================
// PatternDef (new unified pattern model — combines notes + hits)
// ============================================================================

export interface PatternDef {
  name: string;
  instrument: InstrumentPath;
  preset: string | null;
  engine: string | null;
  transpose: number | null;
  swing: number | null;
  /** Declared length in bars; absent derives it from the written content. */
  bars?: number | null;
  effects: Effect[];
  events: NoteEvent[];
  /** Sustain-pedal marks that apply to `events`; absent when empty. */
  pedals?: PedalMark[];
  /** Hairpins that apply to `events`; a placement expands them on its track. */
  hairpins?: HairpinMark[];
  hits: HitEvent[];
  color: string | null;
}

/**
 * Preset namespace the editor injects for a pattern with no sound of its own;
 * it still follows its hosting track, so it is not a chosen preset.
 */
export const INJECTED_PRESET_PREFIX = "oc_default_";

export interface Clip {
  pattern: string;
  time_range: TimeRange;
  /** Quarter notes hidden at the pattern's head by a slip; absent means zero. */
  offset_quarters?: number;
  /** Quarter notes of pattern content the clip repeats; absent is a single pass. */
  loop_quarters?: number;
  /** Stable identifier a group names this clip by; present only while grouped. */
  id?: string;
}

export interface HitEvent {
  drum: string;
  position: BeatPosition;
  dynamics: Dynamics | null;
  /** Exact velocity 1-127; overrides `dynamics` and absent means none. */
  velocity?: number | null;
  articulation: DrumArticulation | null;
  modifiers: [string, ParamValue][];
}

export interface BeatPosition {
  beat: number;
  subdivision: number;
}

export type DrumArticulation =
  | "Closed" | "Open" | "HalfOpen" | "GhostNote"
  | "Flam" | "Roll" | "RimShot" | "CrossStick";

// ============================================================================
// Automation, Bus, Master, Section
// ============================================================================

/** Shape of one automation segment; `tension` only applies to `smooth`. */
export type AutomationCurve = "linear" | "stairs" | "half_sine" | "smooth";

/** Envelope anchors, or one cycle of a repeating waveform. */
export type AutomationMode = "envelope" | "lfo";

/** Absolute frequency, or a note length that follows the tempo (`1/4` = quarter note). */
export type LfoSpeed =
  | { unit: "hz"; hz: number }
  | { unit: "note"; numerator: number; denominator: number };

/** One cycle of a lane's waveform; read only in `mode: "lfo"`. */
export interface LaneLfo {
  speed?: LfoSpeed;
  /** Cycles of the shape per speed cycle. */
  mult?: number;
  /** Fraction of the cycle the shape's first half occupies; 0.5 is neutral. */
  pulse_width?: number;
  /** Phase bend in -1..1. */
  skew?: number;
  /** Eases each segment towards a smoothstep, in 0..1. */
  tension?: number;
  /** Depth around the shape's midpoint; 1 keeps the authored values. */
  amount?: number;
}

export interface AutomationLane {
  param: string;
  /** Span of a clip; absent drives the whole timeline from its first point on. */
  time_range?: TimeRange;
  /** A muted lane writes nothing, so the target keeps its declared value. */
  muted?: boolean;
  /** Ticks per quarter for tick positions; 0 keeps the quarter-beat grid. */
  ppq?: number;
  /** One-pole time constant in milliseconds; absent/0 keeps the raw values. */
  smoothing_ms?: number;
  /** Absent means envelope. */
  mode?: AutomationMode;
  lfo?: LaneLfo;
  points: AutomationPoint[];
}

export interface AutomationPoint {
  position: BarBeat;
  value: ParamValue;
  /** Shape from this point to the next; absent is linear. */
  curve?: AutomationCurve;
  /** Blend of the smooth curve in 0..1; absent means 1. */
  tension?: number | null;
}

export interface Bus {
  name: string;
  routes: string[];
  effects: Effect[];
  gain_automation?: GainAutomation[];
  automation?: AutomationLane[];
  /** Post-effects return fader in dB; absent/0 is unity. */
  volume_db?: number;
  mute?: boolean;
  solo?: boolean;
  /** Device-input routes owned by this bus's effect chain. */
  input_routes?: InputRoute[];
}

export interface GainAutomation {
  time_range: TimeRange;
  ppq?: number;
  interpolation: "Step" | "Linear";
  points: AutomationPoint[];
}

export interface Master {
  effects: Effect[];
  gain_automation?: GainAutomation[];
  automation?: AutomationLane[];
  loudness_target: number | null;
  volume_db: number;
  /** Device-input routes owned by the master effect chain. */
  input_routes?: InputRoute[];
}

/** A named point on the timeline, written as `@marker "name" at 9:1`. */
export interface Marker {
  name: string;
  position: BarBeat;
}

/** A set of clips that moves and selects as one unit, written as `@group`. */
export interface ClipGroup {
  id: string;
  /** Clip identifiers; every one resolves to a clip in the project. */
  members: string[];
}

export interface Section {
  name: string;
  time_range: TimeRange;
  repeat: number | null;
  label: string | null;
}

// ============================================================================
// Common Types
// ============================================================================

export interface TimeSignature {
  numerator: number;
  denominator: number;
}

export interface KeySignature {
  root: NoteName;
  accidental: Accidental;
  mode: "Major" | "Minor";
}

// ParamValue is a tagged enum in Rust — serde serializes as { "Number": 42 } etc.
export type ParamValue =
  | { Number: number }
  | { Db: number }
  | { Percentage: number }
  | { Frequency: number }
  | { Cents: number }
  | { Time: number }
  | { String: string }
  | { Bool: boolean }
  | { Array: number[] };

// ============================================================================
// Tauri command response types
// ============================================================================

export type ParameterTargetStamp = string & { readonly __parameterTargetStamp: unique symbol };

export interface ProjectSnapshot {
  /** Ephemeral opaque backend authority for parameter interaction admission. */
  parameter_target_stamp: ParameterTargetStamp | null;
  project: Project;
  dsl_source: string;
  file_path: string | null;
  dirty: boolean;
  track_count: number;
  /** Document revision; a reply older than the applied one is refused. */
  revision: number;
  /** Written arrangement length, known before the first playback. */
  duration_seconds: number;
  /** The recovery copy or previous version a restore applied. */
  restored_from?: string | null;
}

/** One recovery copy written by the autosave timer. */
export interface RecoveryEntry {
  /** An autosave copy, or the content the last write replaced. */
  kind: "recovery" | "backup";
  path: string;
  project_name: string;
  /** Unix milliseconds when the copy was written. */
  saved_at_ms: number;
  size: number;
  newer_than_disk: boolean;
  /** Why Restore is disabled, or null when the copy can be restored. */
  restore_disabled_reason: string | null;
}

export interface TransportState {
  playing: boolean;
  position_beats: number;
  tempo: number;
  time_signature: [number, number];
}

// ============================================================================
// Helpers
// ============================================================================

export function pitchToString(p: Pitch): string {
  const acc =
    p.accidental === "Sharp" ? "#" :
    p.accidental === "Flat" ? "b" :
    p.accidental === "DoubleSharp" ? "##" :
    p.accidental === "DoubleFlat" ? "bb" : "";
  return `${p.note}${acc}${p.octave}`;
}

export function pitchToMidi(p: Pitch): number {
  const base: Record<NoteName, number> = {
    C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11,
  };
  const accOff: Record<Accidental, number> = {
    Natural: 0, Sharp: 1, DoubleSharp: 2, Flat: -1, DoubleFlat: -2,
  };
  return (p.octave + 1) * 12 + base[p.note] + accOff[p.accidental];
}

export function durationToBeats(d: Duration): number {
  const baseBeats: Record<BaseDuration, number> = {
    Whole: 4, Half: 2, Quarter: 1, Eighth: 0.5,
    Sixteenth: 0.25, ThirtySecond: 0.125, SixtyFourth: 0.0625,
  };
  let beats = baseBeats[d.base];
  if (d.dots === 1) beats *= 1.5;
  if (d.dots === 2) beats *= 1.75;
  if (d.triplet) beats *= 2 / 3;
  if (d.tuplet) beats = beats * d.tuplet[1] / d.tuplet[0];
  if (d.ties) for (const t of d.ties) beats += baseBeats[t];
  return beats;
}

export function instrumentDisplay(inst: InstrumentPath): string {
  if (inst.category === "unknown" && inst.name === "unknown") return "";
  return `${inst.category}/${inst.name}`;
}

/** Display string for a track's sound source: preset name or instrument path */
export function trackSoundDisplay(track: Track): string {
  if (track.preset) return track.preset;
  return instrumentDisplay(track.instrument) || "No sound";
}

export function dynamicsToVelocity(d: Dynamics): number {
  const map: Record<Dynamics, number> = {
    PPP: 16, PP: 33, P: 49, MP: 64, MF: 80, F: 96, FF: 112, FFF: 127,
  };
  return map[d];
}
