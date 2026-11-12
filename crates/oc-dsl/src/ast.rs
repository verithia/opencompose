//! Abstract Syntax Tree for OpenCompose DSL
//!
//! These types represent the complete structure of an OpenCompose project.
//! They are the canonical in-memory representation that all tools operate on:
//! - Parser produces AST from text
//! - Serializer produces text from AST
//! - Validator checks AST for semantic correctness
//! - Interpreter transforms AST into engine commands
//! - GUI reads/mutates AST for display/editing

use serde::{Deserialize, Serialize};

// ============================================================================
// Top-Level Project
// ============================================================================

/// A complete OpenCompose project
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub name: Option<String>,
    pub ocp_version: Option<String>,
    pub properties: ProjectProperties,
    pub presets: Vec<PresetDef>,
    pub tracks: Vec<Track>,
    /// Named top-level patterns (reusable via clips)
    pub patterns: Vec<PatternDef>,
    pub buses: Vec<Bus>,
    pub master: Option<Master>,
    pub sections: Vec<Section>,
    /// Named points on the timeline. Annotations only: markers never change
    /// what renders, and a marker may sit past the last audio.
    pub markers: Vec<Marker>,
    /// Clip groups. Editing-only: a group binds the move and the selection of
    /// its member clips and never changes what renders.
    pub groups: Vec<ClipGroup>,
}

/// A named set of clips that moves and selects as one unit.
///
/// Membership is by [`Clip::id`], so a member keeps its place when the clip
/// moves on the timeline. A group holds at least two members: pruning a member
/// below that count removes the group instead of leaving a stub.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ClipGroup {
    /// Stable identifier written as `@group "id"`.
    pub id: String,
    /// Clip identifiers; every one resolves to a clip in the project.
    pub members: Vec<String>,
}

/// Smallest member count a group may hold.
pub const MIN_GROUP_MEMBERS: usize = 2;

/// Grid the project `swing` percentage moves.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SwingSubdivision {
    /// Off-beat eighths, the historical behaviour.
    #[default]
    Eighth,
    /// Every second sixteenth.
    Sixteenth,
}

impl SwingSubdivision {
    fn is_eighth(&self) -> bool {
        *self == Self::Eighth
    }
}

/// Global project properties
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "ProjectPropertiesWire", into = "ProjectPropertiesWire")]
pub struct ProjectProperties {
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub key: Option<KeySignature>,
    pub swing: Option<f64>, // percentage 0.0-100.0
    /// Grid `swing` applies to; absent means off-beat eighths.
    pub swing_subdivision: SwingSubdivision,
    /// Solo propagation over the bus graph. Missing keeps the historical closure.
    pub solo_mode: SoloMode,
    pub tempo_map: Vec<TempoPoint>,
    pub picture_clock: Option<crate::timecode::PictureClock>,
    pub time_sig_map: Vec<TimeSigPoint>,
    /// Key signature changes at specific positions
    pub key_map: Vec<KeyPoint>,
    /// Stored per-note tuning decisions. Analysis never lives here.
    pub vocal_tunes: Vec<VocalTune>,
    /// Harmony voices derived from tuned clips.
    pub vocal_harmonies: Vec<VocalHarmony>,
    /// Optional MPE zones. Missing keeps the project on the plain MIDI path.
    pub mpe: Option<MpeConfig>,
    /// Export settings. Missing keeps the historical 16-bit undithered output.
    pub export: Option<ExportSettings>,
    /// Live playback resampling step. Missing keeps the engine's preview default.
    pub live_resample: Option<u8>,
    /// Audio-derived control signals other parameters can read.
    pub control_sources: Vec<ControlSource>,
    /// One-to-many control links learned from a CC, macro or lane.
    pub control_links: Vec<ControlLink>,
    /// Normalized automation table the stored synth lanes were written
    /// against. Missing is the original table (version 1); version 2 is the
    /// widened table where every engine enum spans the whole 0..1 domain.
    /// The engine itself always exposes the current table, so only the
    /// interpreter needs this; macros and agent writes get the full domain.
    pub automation_normalization: Option<u8>,
}

/// The table version new lanes are written against.
pub const WIDE_AUTOMATION_NORMALIZATION: u8 = 2;

impl ProjectProperties {
    /// Version of the normalized automation table stored lanes use.
    pub fn automation_normalization_version(&self) -> u8 {
        self.automation_normalization.unwrap_or(1)
    }

    /// Resolve the stored table, refusing unsupported versions before evaluation.
    pub fn checked_automation_normalization(
        &self,
    ) -> Result<
        crate::automation_normalization::AutomationNormalization,
        crate::automation_normalization::NormalizationVersionError,
    > {
        crate::automation_normalization::AutomationNormalization::from_optional(
            self.automation_normalization,
        )
    }

    /// Whether stored lanes use the supported widened table.
    /// Unsupported versions require the checked accessor at input boundaries.
    pub fn uses_wide_automation_normalization(&self) -> bool {
        self.checked_automation_normalization()
            == Ok(crate::automation_normalization::AutomationNormalization::Wide)
    }
}

/// Exported WAV sample format.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum SampleFormat {
    /// Signed 16-bit PCM.
    #[default]
    I16,
    /// Signed 24-bit PCM.
    I24,
    /// IEEE 32-bit float, bit-identical to the internal mix buffer.
    F32,
}

impl SampleFormat {
    /// Every format, in flag order.
    pub const ALL: [SampleFormat; 3] = [SampleFormat::I16, SampleFormat::I24, SampleFormat::F32];

    /// Canonical spelling used by the DSL and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            SampleFormat::I16 => "i16",
            SampleFormat::I24 => "i24",
            SampleFormat::F32 => "f32",
        }
    }

    /// Bits per sample the format writes.
    pub fn bits_per_sample(self) -> u16 {
        match self {
            SampleFormat::I16 => 16,
            SampleFormat::I24 => 24,
            SampleFormat::F32 => 32,
        }
    }

    /// True when the format writes integer samples, and so can be dithered.
    pub fn is_integer(self) -> bool {
        !matches!(self, SampleFormat::F32)
    }
}

impl std::fmt::Display for SampleFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for SampleFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        SampleFormat::ALL
            .into_iter()
            .find(|format| format.as_str() == value)
            .ok_or_else(|| format!("unknown sample format '{value}': expected i16, i24 or f32"))
    }
}

/// How a solo propagates over the bus graph.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum SoloMode {
    /// Keep the sources that feed a soloed node, not its destinations.
    Upstream,
    /// Keep both neighbourhoods: the historical graph closure.
    #[default]
    Both,
}

impl SoloMode {
    /// Every solo mode.
    pub const ALL: [SoloMode; 2] = [SoloMode::Upstream, SoloMode::Both];

    /// Canonical DSL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            SoloMode::Upstream => "upstream",
            SoloMode::Both => "both",
        }
    }

    /// True when a solo keeps the whole graph neighbourhood.
    pub fn is_both(&self) -> bool {
        *self == SoloMode::Both
    }
}

impl std::fmt::Display for SoloMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for SoloMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        SoloMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| format!("unknown solo mode '{value}': expected upstream or both"))
    }
}

/// Audio container an export writes. A project states the container instead of
/// inferring it from the output extension.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ExportContainer {
    /// RIFF/WAVE.
    #[default]
    Wav,
    /// Native FLAC, lossless.
    Flac,
    /// MPEG Layer III, lossy. A build without the `mp3` feature still parses
    /// it and refuses it at export with a named reason.
    Mp3,
}

impl ExportContainer {
    /// Every container the DSL accepts, in flag order.
    pub const ALL: [ExportContainer; 3] = [
        ExportContainer::Wav,
        ExportContainer::Flac,
        ExportContainer::Mp3,
    ];

    /// Canonical spelling used by the DSL and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            ExportContainer::Wav => "wav",
            ExportContainer::Flac => "flac",
            ExportContainer::Mp3 => "mp3",
        }
    }

    /// File extension this container writes.
    pub fn extension(self) -> &'static str {
        self.as_str()
    }
}

impl std::fmt::Display for ExportContainer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExportContainer {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ExportContainer::ALL
            .into_iter()
            .find(|container| container.as_str() == value)
            .ok_or_else(|| {
                format!(
                    "unknown export container '{value}': this build writes {}",
                    ExportContainer::ALL
                        .iter()
                        .map(|container| container.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// WAV container options: sample depth and dither.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct WavSettings {
    /// Sample depth written to the file.
    #[serde(default)]
    pub depth: SampleFormat,
    /// TPDF dither before integer quantisation; refused for `f32`.
    #[serde(default)]
    pub dither: bool,
}

/// Fastest FLAC compression level.
pub const MIN_FLAC_LEVEL: u8 = 0;
/// Smallest FLAC compression level.
pub const MAX_FLAC_LEVEL: u8 = 8;
/// Compression level a project uses when it states none.
pub const DEFAULT_FLAC_LEVEL: u8 = 5;

/// FLAC container options: sample depth and compression level.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct FlacSettings {
    /// Sample depth written to the file. FLAC stores integers, so `f32` is not
    /// a depth it can write.
    #[serde(default)]
    pub depth: SampleFormat,
    /// Compression level, 0 (fastest) to 8 (smallest).
    #[serde(default = "default_flac_level")]
    pub level: u8,
}

/// Serde default for [`FlacSettings::level`].
fn default_flac_level() -> u8 {
    DEFAULT_FLAC_LEVEL
}

impl Default for FlacSettings {
    fn default() -> Self {
        Self {
            depth: SampleFormat::I16,
            level: DEFAULT_FLAC_LEVEL,
        }
    }
}

/// Lowest CBR bitrate any MPEG Layer III version offers, in kbps.
pub const MIN_MP3_BITRATE_KBPS: u32 = 8;
/// Highest CBR bitrate MPEG-1 Layer III offers, in kbps.
pub const MAX_MP3_BITRATE_KBPS: u32 = 320;
/// CBR bitrate an MP3 export uses when nothing says otherwise.
pub const DEFAULT_MP3_BITRATE_KBPS: u32 = 192;
/// Best VBR quality index, the equivalent of the reference encoder's `-q:a 0`.
pub const MIN_MP3_VBR_QUALITY: u8 = 0;
/// Smallest VBR quality index, the equivalent of `-q:a 9`.
pub const MAX_MP3_VBR_QUALITY: u8 = 9;

/// MP3 container options: a CBR bitrate, or a VBR quality index.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Mp3Settings {
    /// CBR bitrate in kbps. A VBR encode chooses its own average, so this value
    /// does not apply when `vbr_quality` is set.
    #[serde(default = "default_mp3_bitrate")]
    pub bitrate: u32,
    /// VBR quality index, 0 (best) to 9 (smallest). Missing is a CBR encode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vbr_quality: Option<u8>,
}

/// Serde default for [`Mp3Settings::bitrate`].
fn default_mp3_bitrate() -> u32 {
    DEFAULT_MP3_BITRATE_KBPS
}

impl Default for Mp3Settings {
    fn default() -> Self {
        Self {
            bitrate: DEFAULT_MP3_BITRATE_KBPS,
            vbr_quality: None,
        }
    }
}

/// Export settings carried by the project header.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ExportSettings {
    /// Container to write. Missing means WAV.
    #[serde(default)]
    pub container: ExportContainer,
    /// Options for the WAV container.
    #[serde(default)]
    pub wav: WavSettings,
    /// Options for the FLAC container.
    #[serde(default)]
    pub flac: FlacSettings,
    /// Options for the MP3 container.
    #[serde(default)]
    pub mp3: Mp3Settings,
    /// Sample rate the project exports at, in hertz. Missing leaves the choice
    /// to the caller: the CLI flag, the GUI dialog or the open device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Render resampling step (0-7). Missing keeps the engine's render default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resample: Option<u8>,
}

/// MPE member-channel range, stored as 1-based MIDI channel numbers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct MpeMemberRange {
    pub first: u8,
    pub last: u8,
}

/// MPE zones recorded by a project. Both zones are optional.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct MpeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower: Option<MpeMemberRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper: Option<MpeMemberRange>,
    /// Pitch-bend range in semitones; MPE defaults to 48.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_range: Option<f64>,
}

impl MpeConfig {
    /// True when either MPE zone is configured.
    pub fn enabled(&self) -> bool {
        self.lower.is_some() || self.upper.is_some()
    }

    /// The configured bend range in semitones, clamped to the supported 0..96.
    pub fn bend_range_semitones(&self) -> f64 {
        self.bend_range.unwrap_or(48.0).clamp(0.0, 96.0)
    }

    /// Member channel for a note ordinal, round-robin across the active zone.
    pub fn channel_for_note(&self, note_ordinal: usize) -> Option<u8> {
        let range = self.lower.or(self.upper)?;
        let first = range.first.clamp(1, 16);
        let last = range.last.clamp(1, 16);
        let low = first.min(last);
        let span = first.max(last) - low + 1;
        Some(low - 1 + (note_ordinal % span as usize) as u8)
    }
}

/// A key signature change at a specific position
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct KeyPoint {
    pub position: BarBeat,
    pub key: KeySignature,
}

/// A tempo change at a specific position
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TempoPoint {
    pub position: BarBeat,
    /// Nonnegative quarter-note residual within the anchor subdivision.
    #[serde(default)]
    pub offset_quarters: f64,
    pub bpm: f64,
}

/// A time signature change at a specific position
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TimeSigPoint {
    pub position: BarBeat,
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for ProjectProperties {
    fn default() -> Self {
        Self {
            tempo: 120.0,
            time_signature: TimeSignature {
                numerator: 4,
                denominator: 4,
            },
            key: None,
            swing: None,
            swing_subdivision: SwingSubdivision::Eighth,
            solo_mode: SoloMode::Both,
            tempo_map: vec![],
            picture_clock: None,
            time_sig_map: vec![],
            key_map: vec![],
            vocal_tunes: vec![],
            vocal_harmonies: vec![],
            mpe: None,
            export: None,
            live_resample: None,
            control_sources: vec![],
            control_links: vec![],
            automation_normalization: None,
        }
    }
}

// ============================================================================
// Preset Definition
// ============================================================================

/// A user-defined sound preset — reusable across tracks.
///
/// ```ocp
/// @preset warm_pad
///   source: clap
///   plugin: "com.surge-synth-team.surge-xt"
///   patch: "Pads/Warm Strings"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PresetDef {
    pub name: String,
    /// Source type: "soundfont", "clap", "vst3", "builtin", "sample"
    pub source: Option<String>,
    /// Plugin ID (for clap/vst3/builtin)
    pub plugin: Option<String>,
    /// GM program number (for soundfont)
    pub program: Option<u8>,
    /// Patch/preset name within the plugin
    pub patch: Option<String>,
    /// Sample file path (for source: sample)
    pub file: Option<String>,
    /// Root note for sample playback
    pub root_note: Option<Pitch>,
    /// Parameters (key=value pairs)
    pub params: Vec<(String, ParamValue)>,
    /// Effects chain applied to this preset
    pub effects: Vec<Effect>,
    /// Inherit from another preset (overrides are applied on top)
    pub extends: Option<String>,
}

// ============================================================================
// Track
// ============================================================================

/// A named top-level pattern — reusable musical content with its own instrument.
/// Placed on tracks via `Clip` references.
///
/// ```ocp
/// @pattern bass_line | instrument: bass/electric_bass, preset: punchy_bass
///   events:
///     Bb1 q mf
///     @2 Db2 q mf
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PatternDef {
    pub name: String,
    pub instrument: InstrumentPath,
    pub preset: Option<String>,
    pub engine: Option<String>,
    /// Semitones by which pitched `events` move before the instrument plays
    /// them (e.g. -2 for Bb trumpet). Accepts -127..=127; a wider value is
    /// refused because it is indistinguishable after the MIDI key range clamps.
    /// `hits` address samples by name and are never transposed.
    pub transpose: Option<i8>,
    /// Per-pattern swing percentage (0-100), overrides project swing
    pub swing: Option<f64>,
    /// Declared length in bars. `None` derives the length from the written
    /// content: the end of the last bar that content occupies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<f64>,
    pub effects: Vec<Effect>,
    pub events: Vec<NoteEvent>,
    /// Sustain-pedal marks that apply to this pattern's `events:`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pedals: Vec<PedalEvent>,
    /// Hairpins that apply to this pattern's `events:`. A placed pattern expands
    /// each mark on its hosting track, like a phrase mark.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hairpins: Vec<HairpinMark>,
    pub hits: Vec<HitEvent>,
    pub color: Option<String>,
}

impl PatternDef {
    /// Quarter notes the written `events` and `hits` span. Glissandi and
    /// articulation tails can reach further at render time, so this is the
    /// bound a slip offset may move through, not a rendered length.
    pub fn content_quarters(&self) -> f64 {
        let notes = self
            .events
            .iter()
            .map(|event| event.start_beat + event.duration.to_beats());
        let hits = self.hits.iter().map(HitEvent::written_quarters);
        notes.chain(hits).fold(0.0_f64, f64::max)
    }

    /// Quarter notes a placement may use: the declared `bars:` when written,
    /// else the end of the last bar the written content occupies. A pattern
    /// with no content still occupies one bar, so its first note has room.
    pub fn length_quarters(&self) -> f64 {
        match self.bars {
            Some(bars) => (bars * 4.0).max(0.0),
            None => {
                let bars = (self.content_quarters() / 4.0 - 1e-9).ceil().max(1.0);
                bars * 4.0
            }
        }
    }
}

/// A clip places a pattern on a track at a specific time position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Clip {
    /// References a PatternDef.name
    pub pattern: String,
    pub time_range: TimeRange,
    /// Quarter notes hidden at the start of the pattern. Slip moves this
    /// without moving the clip; absent means zero.
    #[serde(default, skip_serializing_if = "zero_f64")]
    pub offset_quarters: f64,
    /// Quarter notes of pattern content played before the clip's head repeats;
    /// zero is a single pass. The loop starts at `offset_quarters`.
    #[serde(default, skip_serializing_if = "zero_f64")]
    pub loop_quarters: f64,
    /// Stable identifier a [`ClipGroup`] names this clip by. Absent until the
    /// clip joins a group; moving the clip keeps the identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Time-stretch algorithm selected for an audio clip.
///
/// `Musical` is the default and renders through the phase vocoder. `Off`
/// disables processing even when a ratio is set, which keeps an A/B path.
#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum StretchMode {
    Off,
    Percussive,
    Varispeed,
    #[default]
    Musical,
}

impl StretchMode {
    /// Canonical DSL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Percussive => "percussive",
            Self::Varispeed => "varispeed",
            Self::Musical => "musical",
        }
    }
}

fn default_stretch_ratio() -> f64 {
    1.0
}

/// Largest transposition either way, in semitones.
pub const MAX_PITCH_SEMITONES: f64 = 24.0;

/// One point of a clip's optional gain envelope. `position` is normalised to
/// the clip length (0 = head, 1 = tail); `gain_db` is relative to the clip's
/// static `gain_db` (0 dB leaves it unchanged).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GainEnvelopePoint {
    pub position: f64,
    pub gain_db: f64,
}

/// An anchor from source-media seconds to processed-media seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AudioWarpMarker {
    pub source_seconds: f64,
    pub target_seconds: f64,
}

/// Fine placement relative to the clip's musical start and end anchors.
/// Source-media trimming remains independent in `AudioClip::offset_seconds`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AudioClipPlacement {
    #[serde(default)]
    pub start_offset_seconds: f64,
    #[serde(default)]
    pub end_offset_seconds: f64,
}

impl AudioClipPlacement {
    /// Whether both offsets are zero.
    pub fn is_zero(&self) -> bool {
        self.start_offset_seconds == 0.0 && self.end_offset_seconds == 0.0
    }
}

/// Decoded audio media placed on a track at an absolute time range.
/// See `docs/audio-clip.md` for the `@audio_clip` block syntax.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AudioClip {
    pub name: String,
    /// Destination track name; the clip inherits its mix, sends and effects.
    pub track: String,
    /// Media path relative to the project file.
    pub file: String,
    pub time_range: TimeRange,
    #[serde(default, skip_serializing_if = "AudioClipPlacement::is_zero")]
    pub placement: AudioClipPlacement,
    /// Seconds trimmed from the start of the stretched media.
    #[serde(default)]
    pub offset_seconds: f64,
    /// Clip gain in dB, matching `mix: vol`.
    #[serde(default)]
    pub gain_db: f64,
    /// Linear fade lengths in seconds; zero disables the fade.
    #[serde(default)]
    pub fade_in_seconds: f64,
    #[serde(default)]
    pub fade_out_seconds: f64,
    /// Reverse the clip's polarity at mix time.
    #[serde(default)]
    pub invert: bool,
    /// Equal-power crossfade length at the clip head and tail; zero disables.
    #[serde(default)]
    pub crossfade_seconds: f64,
    /// Time-stretch factor: output duration is media duration × `stretch`.
    #[serde(default = "default_stretch_ratio")]
    pub stretch: f64,
    #[serde(default)]
    pub stretch_mode: StretchMode,
    /// Play the original media while retaining the configured processing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypass_processing: bool,
    /// Derive `stretch` from the clip range and media length instead.
    #[serde(default)]
    pub tempo_follow: bool,
    /// Transposition in semitones, fractional allowed; zero leaves the media alone.
    ///
    /// Implemented as a playback-speed resample composed with the stretch, so the
    /// clip keeps its stretched duration and the formants move with the pitch.
    #[serde(default)]
    pub pitch: f64,
    /// Formant displacement in semitones, independent of clip pitch.
    #[serde(default, skip_serializing_if = "zero_f64")]
    pub formant: f64,
    /// Compensate the formant displacement introduced by clip pitch.
    #[serde(default, skip_serializing_if = "is_false")]
    pub preserve_formants: bool,
    /// Ordered anchors after the implicit origin at (0, 0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warp_markers: Vec<AudioWarpMarker>,
    /// Seconds of media played before the clip's head repeats; zero is a single
    /// pass. The loop starts at `offset_seconds` and is shorter than the clip.
    #[serde(default, skip_serializing_if = "zero_f64")]
    pub loop_seconds: f64,
    /// Identifier shared by the passes of one loop-record session. Absent on a
    /// clip that was not recorded as a take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_group: Option<String>,
    /// One-based pass number inside [`Self::take_group`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<u32>,
    /// A muted clip keeps its media and position but contributes nothing to the
    /// mix; the engine skips it exactly like a clip that is not there.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Optional point envelope on top of `gain_db`; empty means none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gain_envelope: Vec<GainEnvelopePoint>,
}

impl AudioClip {
    /// Whether pitch, formant and warp settings leave varispeed controls free.
    pub fn varispeed_controls_compatible(&self) -> bool {
        self.pitch == 0.0
            && self.formant == 0.0
            && !self.preserve_formants
            && self.warp_markers.is_empty()
    }
}

// ============================================================================
// Clip pool
// ============================================================================

/// Owner of an automation lane.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AutomationOwner {
    /// Track by its index in `project.tracks`.
    Track(usize),
    /// Bus by name, because a bus has no stable index in the document.
    Bus(String),
    /// The master section.
    Master,
}

impl AutomationOwner {
    /// Stable text form, also the first half of an automation clip key.
    pub fn to_text(&self) -> String {
        match self {
            Self::Track(index) => format!("track:{index}"),
            Self::Bus(name) => format!("bus:{name}"),
            Self::Master => "master".to_string(),
        }
    }
}

impl std::fmt::Display for AutomationOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// One payload in a track's clip pool.
///
/// The three kinds keep the fields they always had: a [`Clip`] owns its span,
/// an [`AudioClip`] owns its span, and an [`AutomationLane`] owns an optional
/// span (`None` drives the whole timeline).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipPayload {
    /// A pattern placed on the track.
    Pattern(Clip),
    /// A decoded media file placed on the track.
    Audio(AudioClip),
    /// An automation lane owned by the track.
    Automation(AutomationLane),
}

impl ClipPayload {
    /// Kind rank used to keep a pool grouped: pattern, then audio, then
    /// automation.
    fn rank(&self) -> u8 {
        match self {
            Self::Pattern(_) => 0,
            Self::Audio(_) => 1,
            Self::Automation(_) => 2,
        }
    }
}

/// One clip in a track pool with its position among clips of the same kind.
///
/// `order` counts pattern clips per track, automation lanes per owner, and
/// audio clips across the whole project; it is assigned when the document is
/// read and never appears in JSON. Pattern clips and automation lanes of one
/// track therefore keep their authored sequence, and audio clips keep the
/// project-wide `@audio_clip` order they were written in.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineClip {
    /// Position among clips of the same kind, see [`TimelineClip`].
    pub order: usize,
    /// The stored object.
    pub payload: ClipPayload,
}

impl TimelineClip {
    /// A pattern clip at `order` among the track's pattern clips.
    pub fn pattern(order: usize, clip: Clip) -> Self {
        Self {
            order,
            payload: ClipPayload::Pattern(clip),
        }
    }

    /// An audio clip at global audio `order`.
    pub fn audio(order: usize, clip: AudioClip) -> Self {
        Self {
            order,
            payload: ClipPayload::Audio(clip),
        }
    }

    /// An automation lane at `order` among its owner's lanes.
    pub fn automation(order: usize, lane: AutomationLane) -> Self {
        Self {
            order,
            payload: ClipPayload::Automation(lane),
        }
    }

    /// The payload as a pattern clip.
    pub fn as_pattern(&self) -> Option<&Clip> {
        match &self.payload {
            ClipPayload::Pattern(clip) => Some(clip),
            _ => None,
        }
    }

    /// The payload as a mutable pattern clip.
    pub fn as_pattern_mut(&mut self) -> Option<&mut Clip> {
        match &mut self.payload {
            ClipPayload::Pattern(clip) => Some(clip),
            _ => None,
        }
    }

    /// The payload as an audio clip.
    pub fn as_audio(&self) -> Option<&AudioClip> {
        match &self.payload {
            ClipPayload::Audio(clip) => Some(clip),
            _ => None,
        }
    }

    /// The payload as a mutable audio clip.
    pub fn as_audio_mut(&mut self) -> Option<&mut AudioClip> {
        match &mut self.payload {
            ClipPayload::Audio(clip) => Some(clip),
            _ => None,
        }
    }

    /// The payload as an automation lane.
    pub fn as_automation(&self) -> Option<&AutomationLane> {
        match &self.payload {
            ClipPayload::Automation(lane) => Some(lane),
            _ => None,
        }
    }

    /// The payload as a mutable automation lane.
    pub fn as_automation_mut(&mut self) -> Option<&mut AutomationLane> {
        match &mut self.payload {
            ClipPayload::Automation(lane) => Some(lane),
            _ => None,
        }
    }
}

/// Every clip on one track, grouped by kind and ordered within each kind.
///
/// The pool is the track's only clip storage. Its elements stay sorted by
/// (kind, `order`), so filtering a kind yields the authored sequence and two
/// pools built along different paths compare equal. `Deref` exposes the slice,
/// which makes iteration, indexing and length work like the old `Vec<Clip>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipPool(Vec<TimelineClip>);

impl ClipPool {
    /// An empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one clip at its sorted position.
    pub fn push(&mut self, clip: TimelineClip) {
        let key = (clip.payload.rank(), clip.order);
        let index = self
            .0
            .partition_point(|existing| (existing.payload.rank(), existing.order) <= key);
        self.0.insert(index, clip);
    }

    /// The number of clips of every kind.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the track holds no clip at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate every clip in canonical order.
    pub fn iter(&self) -> std::slice::Iter<'_, TimelineClip> {
        self.0.iter()
    }

    /// Iterate every clip mutably.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, TimelineClip> {
        self.0.iter_mut()
    }

    /// Pattern clips in authored order.
    pub fn pattern_clips(&self) -> impl Iterator<Item = &Clip> {
        self.0.iter().filter_map(TimelineClip::as_pattern)
    }

    /// Pattern clips in authored order, mutably.
    pub fn pattern_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.0.iter_mut().filter_map(TimelineClip::as_pattern_mut)
    }

    /// Audio clips in authored order.
    pub fn audio_clips(&self) -> impl Iterator<Item = &AudioClip> {
        self.0.iter().filter_map(TimelineClip::as_audio)
    }

    /// Audio clips in authored order, mutably.
    pub fn audio_clips_mut(&mut self) -> impl Iterator<Item = &mut AudioClip> {
        self.0.iter_mut().filter_map(TimelineClip::as_audio_mut)
    }

    /// Automation lanes in authored order.
    pub fn automation_lanes(&self) -> impl Iterator<Item = &AutomationLane> {
        self.0.iter().filter_map(TimelineClip::as_automation)
    }

    /// Automation lanes in authored order, mutably.
    pub fn automation_lanes_mut(&mut self) -> impl Iterator<Item = &mut AutomationLane> {
        self.0
            .iter_mut()
            .filter_map(TimelineClip::as_automation_mut)
    }

    /// Number of pattern clips.
    pub fn pattern_count(&self) -> usize {
        self.pattern_clips().count()
    }

    /// Number of audio clips.
    pub fn audio_count(&self) -> usize {
        self.audio_clips().count()
    }

    /// Number of automation lanes.
    pub fn automation_count(&self) -> usize {
        self.automation_lanes().count()
    }

    /// Append a pattern clip after the existing ones.
    pub fn push_pattern(&mut self, clip: Clip) {
        let order = self.next_order(0);
        self.push(TimelineClip::pattern(order, clip));
    }

    /// Append an automation lane after the existing ones.
    pub fn push_automation(&mut self, lane: AutomationLane) {
        let order = self.next_order(2);
        self.push(TimelineClip::automation(order, lane));
    }

    /// The next free order among one kind.
    fn next_order(&self, rank: u8) -> usize {
        self.0
            .iter()
            .filter(|clip| clip.payload.rank() == rank)
            .map(|clip| clip.order)
            .max()
            .map_or(0, |order| order + 1)
    }

    /// Replace every pattern clip, numbering them in the given order.
    pub fn set_pattern_clips(&mut self, clips: impl IntoIterator<Item = Clip>) {
        self.replace_kind(0, TimelineClip::pattern, clips);
    }

    /// Replace every automation lane, numbering them in the given order.
    pub fn set_automation_lanes(&mut self, lanes: impl IntoIterator<Item = AutomationLane>) {
        self.replace_kind(2, TimelineClip::automation, lanes);
    }

    /// Replace every audio clip, numbering them from zero.
    pub fn set_audio_clips(&mut self, clips: impl IntoIterator<Item = AudioClip>) {
        self.replace_kind(1, TimelineClip::audio, clips);
    }

    /// Add an audio clip at an explicit global order.
    pub fn push_audio_at(&mut self, order: usize, clip: AudioClip) {
        self.push(TimelineClip::audio(order, clip));
    }

    /// Remove every clip.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Remove one clip by its index in the pool.
    pub fn remove(&mut self, index: usize) -> Option<TimelineClip> {
        (index < self.0.len()).then(|| self.0.remove(index))
    }

    fn replace_kind<T>(
        &mut self,
        rank: u8,
        make: impl Fn(usize, T) -> TimelineClip,
        items: impl IntoIterator<Item = T>,
    ) {
        let kept: Vec<TimelineClip> = std::mem::take(&mut self.0)
            .into_iter()
            .filter(|clip| clip.payload.rank() != rank)
            .collect();
        self.0 = kept;
        for (index, item) in items.into_iter().enumerate() {
            self.push(make(index, item));
        }
    }
}

impl<'a> IntoIterator for &'a ClipPool {
    type Item = &'a TimelineClip;
    type IntoIter = std::slice::Iter<'a, TimelineClip>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut ClipPool {
    type Item = &'a mut TimelineClip;
    type IntoIter = std::slice::IterMut<'a, TimelineClip>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl std::ops::Deref for ClipPool {
    type Target = [TimelineClip];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ClipPool {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Stored tuning decisions for one audio clip.
///
/// The block holds only what the human decided; detected notes, f0 and note
/// boundaries live in the derived `.ocn` analysis. `at` is the one stable
/// anchor and is the detected onset time it was authored against.
/// See `docs/vocal-tune.md` for the block syntax and the resolution rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct VocalTune {
    pub name: String,
    /// Tuned clip; exactly one of `clip` and `track` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Scale used by degree targets; `None` inherits the project key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<KeySignature>,
    /// How notes without a stored target get one. See `docs/vocal-tune.md`.
    #[serde(default)]
    pub snap: VocalSnapPolicy,
    /// Clip whose pitch contour the automatic reference policy follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Largest scale-snap correction, in cents; a wider move is never proposed.
    pub snap_cap_cents: f64,
    /// Ambiguity window for the contour tie-break, in cents.
    pub snap_window_cents: f64,
    /// Largest note move or length change the block accepts, in seconds.
    pub max_move_seconds: f64,
    /// Default correction for notes that do not carry their own, in percent.
    pub amount: f64,
    /// Default glide time in seconds.
    pub retune_seconds: f64,
    /// Reserved for formant shifting, in cents; zero leaves the envelope alone.
    pub formant_cents: f64,
    pub vibrato: VocalVibratoMode,
    /// Sparse overrides, ordered by `at`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<VocalTuneNote>,
}

impl Default for VocalTune {
    fn default() -> Self {
        Self {
            name: String::new(),
            clip: None,
            track: None,
            scale: None,
            snap: VocalSnapPolicy::Auto,
            reference: None,
            snap_cap_cents: 100.0,
            snap_window_cents: 35.0,
            max_move_seconds: 0.120,
            amount: 100.0,
            retune_seconds: 0.025,
            formant_cents: 0.0,
            vibrato: VocalVibratoMode::Preserve,
            notes: Vec::new(),
        }
    }
}

/// Vibrato policy for `@vocal_tune`.
///
/// `preserve` keeps the performer's vibrato, `flatten` holds the note at its
/// median pitch, and `scale` keeps half its depth. The bake measures the depth
/// from the analysis frames; see `docs/vocal-tune.md`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum VocalVibratoMode {
    Preserve,
    Flatten,
    Scale,
}

impl VocalVibratoMode {
    /// Canonical DSL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Flatten => "flatten",
            Self::Scale => "scale",
        }
    }
}

/// Where a note without a stored target gets its target from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum VocalSnapPolicy {
    /// The reference when the block names one, the scale otherwise.
    #[default]
    Auto,
    /// The scale, `key_map` or project key at the note's position.
    Scale,
    /// The reference clip where a note corresponds, the scale elsewhere.
    Reference,
    /// No automatic target: only stored `tgt=` decisions move a note.
    Off,
}

impl VocalSnapPolicy {
    /// Canonical DSL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scale => "scale",
            Self::Reference => "reference",
            Self::Off => "off",
        }
    }

    /// The policy a block actually runs: `auto` picks by `reference`.
    pub fn resolved(self, has_reference: bool) -> Self {
        match self {
            Self::Auto => {
                if has_reference {
                    Self::Reference
                } else {
                    Self::Scale
                }
            }
            policy => policy,
        }
    }
}

/// Where one note should land. Relative variants measure from the detected pitch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum VocalTarget {
    /// Absolute MIDI note number, for example `A3`.
    Pitch(u8),
    /// Absolute pitch in cents from MIDI 0 (C-1 = 0, so A4 = 6900).
    Cents(f64),
    /// Semitone offset from the detected pitch, for example `+2st`.
    Semitones(f64),
    /// Cent offset from the detected pitch, for example `-25ct`.
    OffsetCents(f64),
    /// One-based scale degree in the tonic's octave, for example `5@4`.
    Degree { degree: u8, octave: i32 },
}

/// One stored per-note override. Times are clip-local seconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct VocalTuneNote {
    /// Stable anchor: the detected onset this override was authored against.
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<VocalTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retune_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formant_cents: Option<f64>,
    /// Move this note's onset by seconds relative to the detected onset;
    /// positive is later. Bounded by the block `max_move`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<f64>,
    /// Length multiplier for the detected note's span; 1.0 keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// Vibrato policy for this note; `None` inherits the block default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibrato: Option<VocalVibratoMode>,
    /// Leave this note alone, including the block defaults.
    #[serde(default)]
    pub bypass: bool,
}

/// One harmony voice derived from a tuned audio clip.
///
/// The voice is a second part on the clip's own track: the clip's baked window
/// shifted by `interval` scale steps. See `docs/vocal-tune.md` for the interval
/// rule, the chromatic fallback and the range limit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct VocalHarmony {
    pub name: String,
    /// Source clip; exactly one of `clip` and `track` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Diatonic steps above the lead note; without a scale the value counts
    /// chromatic semitones instead.
    pub interval: i32,
    /// Voice level in dB relative to the lead's RMS over the same notes.
    pub level_db: f64,
    /// Stereo balance, `-1.0` (left) to `1.0` (right).
    pub pan: f64,
}

impl Default for VocalHarmony {
    fn default() -> Self {
        Self {
            name: String::new(),
            clip: None,
            track: None,
            interval: 2,
            level_db: -6.0,
            pan: 0.0,
        }
    }
}

/// A track is a single instrument/voice with its content and mix settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "TrackWire", into = "TrackWire")]
pub struct Track {
    pub name: String,
    pub instrument: InstrumentPath,
    pub preset: Option<String>, // Reference to a @preset name
    pub engine: Option<String>, // Optional engine override
    pub mix: MixState,
    pub effects: Vec<Effect>,
    pub phrases: Vec<Phrase>,
    pub patterns: Vec<Pattern>,
    /// Inline patterns (new unified model — replaces phrases + patterns above)
    pub inline_patterns: Vec<PatternDef>,
    /// Every clip on this track: pattern placements, audio clips and
    /// automation lanes.
    pub clips: ClipPool,
    /// Post-insert gain clips on this track, in the same project coordinates and
    /// linear-factor units as a bus or the master. A hairpin writes one here.
    pub gain_automation: Vec<GainAutomation>,
    pub engine_params: Option<Vec<(String, ParamValue)>>,
    pub performance: Option<crate::performance::PerformanceTimeline>,
    /// MIDI channel for note-derived events; None keeps the legacy track channel.
    pub channel: Option<u8>,
    /// Output port of the hosted instrument this track takes; None is port 0.
    pub output_port: Option<u16>,
    /// Stereo pair inside that port when the port is wider than stereo; None is
    /// the port's default pair.
    pub output_pair: Option<u16>,
    /// Device-input routes owned by this track's effect chain.
    pub input_routes: Vec<InputRoute>,
}

impl Track {
    /// Pattern clips in authored order.
    pub fn pattern_clips(&self) -> impl Iterator<Item = &Clip> {
        self.clips.pattern_clips()
    }

    /// Pattern clips in authored order, mutably.
    pub fn pattern_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.clips.pattern_clips_mut()
    }

    /// The pattern clip at `index` among this track's pattern clips.
    pub fn pattern_clip(&self, index: usize) -> Option<&Clip> {
        self.clips.pattern_clips().nth(index)
    }

    /// The pattern clip at `index`, mutably.
    pub fn pattern_clip_mut(&mut self, index: usize) -> Option<&mut Clip> {
        self.clips.pattern_clips_mut().nth(index)
    }

    /// Number of pattern clips.
    pub fn pattern_clip_count(&self) -> usize {
        self.clips.pattern_count()
    }

    /// Append a pattern clip after the existing pattern clips.
    pub fn push_pattern_clip(&mut self, clip: Clip) {
        self.clips.push_pattern(clip);
    }

    /// Remove the pattern clip at `index`.
    pub fn remove_pattern_clip(&mut self, index: usize) -> Option<Clip> {
        let position = self
            .clips
            .iter()
            .enumerate()
            .filter(|(_, clip)| clip.as_pattern().is_some())
            .nth(index)
            .map(|(position, _)| position)?;
        self.clips
            .remove(position)
            .and_then(|clip| match clip.payload {
                ClipPayload::Pattern(clip) => Some(clip),
                _ => None,
            })
    }

    /// Replace every pattern clip, numbering them in the given order.
    pub fn set_pattern_clips(&mut self, clips: impl IntoIterator<Item = Clip>) {
        self.clips.set_pattern_clips(clips);
    }

    /// Audio clips on this track in authored order.
    pub fn audio_clips(&self) -> impl Iterator<Item = &AudioClip> {
        self.clips.audio_clips()
    }

    /// Audio clips on this track in authored order, mutably.
    pub fn audio_clips_mut(&mut self) -> impl Iterator<Item = &mut AudioClip> {
        self.clips.audio_clips_mut()
    }

    /// Automation lanes owned by this track, in authored order.
    pub fn automation(&self) -> impl Iterator<Item = &AutomationLane> {
        self.clips.automation_lanes()
    }

    /// Automation lanes owned by this track, mutably.
    pub fn automation_mut(&mut self) -> impl Iterator<Item = &mut AutomationLane> {
        self.clips.automation_lanes_mut()
    }

    /// The automation lane at `index` among this track's lanes.
    pub fn automation_lane(&self, index: usize) -> Option<&AutomationLane> {
        self.clips.automation_lanes().nth(index)
    }

    /// The automation lane at `index`, mutably.
    pub fn automation_lane_mut(&mut self, index: usize) -> Option<&mut AutomationLane> {
        self.clips.automation_lanes_mut().nth(index)
    }

    /// Number of automation lanes.
    pub fn automation_count(&self) -> usize {
        self.clips.automation_count()
    }

    /// Append an automation lane after the existing lanes.
    pub fn push_automation(&mut self, lane: AutomationLane) {
        self.clips.push_automation(lane);
    }

    /// Replace every automation lane, numbering them in the given order.
    pub fn set_automation_lanes(&mut self, lanes: impl IntoIterator<Item = AutomationLane>) {
        self.clips.set_automation_lanes(lanes);
    }
}

/// Hierarchical instrument identifier: category/name
/// e.g. "piano/acoustic_grand", "strings/violin", "synth/lead_saw"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InstrumentPath {
    pub category: String,
    pub name: String,
}

impl InstrumentPath {
    /// Build an instrument path from category and name.
    pub fn new(category: &str, name: &str) -> Self {
        Self {
            category: category.to_string(),
            name: name.to_string(),
        }
    }

    /// Full path representation: "category/name"
    pub fn as_path(&self) -> String {
        format!("{}/{}", self.category, self.name)
    }
}

// ============================================================================
// Mix
// ============================================================================

/// Signal a device input route carries.
///
/// The default is `Audio`, so a route written before kinds existed stays an
/// audio route. `Control` carries the normalized control signal a
/// `@control_source` produces; it is never mixed into the audio sum.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    /// Audio from a track's post-insert tap into a device's key input.
    #[default]
    Audio,
    /// A `@control_source` reading into a device's control input.
    Control,
}

impl RouteKind {
    /// DSL spelling of the kind; the audio default is written without a word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Control => "control",
        }
    }
}

/// One route into a device input, owned by the channel whose chain holds it.
///
/// The target is an effect address (`compression`, `fx2`), resolved with the
/// same rule automation lanes use. `source` is a track name for an audio route
/// and a `@control_source` name for a control route. A control route never
/// contributes audio: the port it lands on is declared `sums: false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InputRoute {
    #[serde(default)]
    pub kind: RouteKind,
    pub source: String,
    pub target: String,
}

/// Mix state for a track or bus
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct MixState {
    /// Exclusive main output; None retains the legacy master route.
    #[serde(default)]
    pub output: Option<String>,
    pub volume_db: f64, // in dB, 0.0 = unity
    pub pan: f64,       // -1.0 (left) to 1.0 (right)
    /// Use stereo balance even when the two channels carry identical samples.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stereo_balance: bool,
    pub sends: Vec<Send>,
    pub mute: bool,
    pub solo: bool,
    /// Channel-input polarity inversion; applied post-insert, pre-fader.
    #[serde(default)]
    pub polarity_inverted: bool,
    /// Presentation and solo behaviour overrides; never changes the audio.
    #[serde(default)]
    pub overrides: ChannelOverrides,
    /// Latency-compensation policy for this channel's routes.
    #[serde(default, skip_serializing_if = "LatencyCompensation::is_on")]
    pub latency_compensation: LatencyCompensation,
    /// Extra output latency in samples, added to the plugin-reported latency.
    #[serde(default, skip_serializing_if = "latency_offset_is_zero")]
    pub latency_offset: i32,
    /// Post-fader gain ride driven by an audio-derived control source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_gain: Option<ControlGain>,
    /// Keep this channel's hosted plugins processing while it is silent.
    #[serde(
        default = "smart_disable_default",
        skip_serializing_if = "smart_disable_is_on"
    )]
    pub smart_disable: bool,
}

impl Default for MixState {
    fn default() -> Self {
        Self {
            output: None,
            volume_db: 0.0,
            pan: 0.0,
            stereo_balance: false,
            sends: Vec::new(),
            mute: false,
            solo: false,
            polarity_inverted: false,
            overrides: ChannelOverrides::default(),
            latency_compensation: LatencyCompensation::default(),
            latency_offset: 0,
            control_gain: None,
            smart_disable: true,
        }
    }
}

/// Serde helper: an absent smart-disable switch keeps the gate enabled.
fn smart_disable_default() -> bool {
    true
}

/// Serde helper: only the opt-out needs a JSON field.
fn smart_disable_is_on(enabled: &bool) -> bool {
    *enabled
}

/// Serde helper: an absent manual latency offset is zero.
fn latency_offset_is_zero(offset: &i32) -> bool {
    *offset == 0
}

/// Serde helper: an absent number is zero, so old projects keep their text.
fn zero_f64(value: &f64) -> bool {
    *value == 0.0
}

/// Per-channel latency-compensation policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LatencyCompensation {
    /// Delay every route to its destination's accumulated latency (default).
    #[default]
    On,
    /// Leave this channel's output undelayed.
    Off,
    /// Leave it undelayed while the pass is a recording pass.
    WhenRecording,
}

impl LatencyCompensation {
    /// True for the legacy default, so serialized projects do not change.
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On)
    }

    /// True when this policy drops the compensation for a pass.
    pub fn bypasses(&self, recording: bool) -> bool {
        match self {
            Self::On => false,
            Self::Off => true,
            Self::WhenRecording => recording,
        }
    }

    /// DSL spelling of this policy.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::WhenRecording => "when_recording",
        }
    }
}

/// Per-channel presentation and solo behaviour overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ChannelOverrides {
    /// Mixer/timeline label; the track name remains the routing identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Free-form CSS colour used by the mixer channel strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Keep this channel audible while another channel is soloed.
    #[serde(default)]
    pub solo_safe: bool,
}

impl ChannelOverrides {
    /// True when every override is at its legacy default.
    pub fn is_default(&self) -> bool {
        self.display_name.is_none() && self.color.is_none() && !self.solo_safe
    }
}

/// An aux send to a bus
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Send {
    pub target: String, // Bus name
    pub amount: f64,    // Linear send gain, 0.0 - 4.0 (+12 dB)
    /// Plugin auxiliary output feeding this send; None uses the track output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SendSource>,
    /// Tap before the channel fader and pan; false taps after both.
    #[serde(default)]
    pub pre_fader: bool,
    /// Balance at the bus input, -1.0 (left) to 1.0 (right); 0.0 is unity.
    #[serde(default)]
    pub pan: f64,
}

/// A plugin output bus named as `<owner>.outN` on the hosting track.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SendSource {
    /// Track or preset name that hosts the instrument.
    pub owner: String,
    /// 1-based output bus; out1 is the main track output and cannot be a send.
    pub output: u8,
}

impl SendSource {
    /// Zero-based stereo output pair used by the host buffer.
    pub fn pair(&self) -> usize {
        usize::from(self.output.saturating_sub(1))
    }
}

// ============================================================================
// Effects
// ============================================================================

/// An audio effect with typed parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Effect {
    pub effect_type: EffectType,
    pub params: Vec<(String, ParamValue)>,
    /// Takes the effect out of the chain: no processing and no latency.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypass: bool,
    /// Optional sidechain input source (track or bus name)
    pub sidechain_source: Option<String>,
}

/// Keeps `bypass: false` out of serialized projects.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Known effect types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum EffectType {
    Gain,
    Eq,
    /// Six-band parametric EQ with per-band dynamics and low/high cuts.
    DynamicEq,
    Compression,
    /// Split/full-band sibilance reduction with an HP/LP detector band.
    DeEsser,
    /// Complementary voice/breath split with a periodicity detector.
    BreathSplit,
    /// Low-band burst detector driving a dynamic low shelf.
    Plosive,
    /// Two-to-four band compressor fed by an LR4 crossover.
    Multiband,
    /// Control-rate K-weighted loudness rider with a silence gate.
    Leveler,
    /// Shunt-cell optical compressor with a two-state release.
    Opto,
    /// Feedback FET compressor with a bias-set ratio and a fast peak detector.
    Fet,
    /// Asymmetric saturation with a flux-driven transformer core.
    Iron,
    /// Coupled passive EQ: a shared low corner, high band, tilt and air.
    PassiveEq,
    /// Two to four short modulated delay voices with detune and a focus high-pass.
    Doubler,
    /// Synthesised air band from existing 5-8 kHz content.
    Exciter,
    /// Reverb return delayed, widened and ducked by the dry vocal envelope.
    DuckedSend,
    /// Many-band spectral dynamics: prominence over a local envelope.
    Resonance,
    /// Scale-snapped pitch correction with PSOLA resynthesis.
    Tuner,
    Reverb,
    ReverbFdn,
    Delay,
    Distortion,
    Chorus,
    Flanger,
    Phaser,
    Limiter,
    Gate,
    Saturator,
    StereoWidth,
    /// External plugin effect (CLAP/VST2/VST3). Holds the plugin ID.
    Plugin(String),
}

// ============================================================================
// Phrase (pitched instruments)
// ============================================================================

/// A phrase is a time-located sequence of notes for pitched instruments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Phrase {
    pub name: String,
    pub time_range: Option<TimeRange>,
    pub events: Vec<NoteEvent>,
    /// Sustain-pedal marks; a pedal spans notes, so it is not a note modifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pedals: Vec<PedalEvent>,
    /// Hairpins; a mark spans notes, so it is not a note modifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hairpins: Vec<HairpinMark>,
    /// MIDI channel override; None inherits the track channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
}

/// A hairpin at a beat position inside its owning phrase.
///
/// `@2.0 cresc` swells from the dynamic in force at the mark to the next
/// written dynamic, or to the phrase end when no later dynamic is written. A
/// hairpin expands to a gain ramp on the owning track, so it is not a note
/// modifier and takes no time in the sequential line model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HairpinMark {
    /// Beat position from the start of the owning phrase.
    pub beat: f64,
    pub kind: HairpinKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Crescendo or diminuendo.
pub enum HairpinKind {
    Crescendo,
    Diminuendo,
}

impl HairpinKind {
    /// DSL keyword for the hairpin.
    pub fn keyword(self) -> &'static str {
        match self {
            HairpinKind::Crescendo => "cresc",
            HairpinKind::Diminuendo => "dim",
        }
    }

    /// Parse a hairpin keyword.
    pub fn from_keyword(word: &str) -> Option<Self> {
        match word {
            "cresc" => Some(HairpinKind::Crescendo),
            "dim" => Some(HairpinKind::Diminuendo),
            _ => None,
        }
    }
}

/// A sustain-pedal mark at a beat position inside its owning container.
///
/// `@2.0 pedal down` holds every note whose note-off lands while the pedal is
/// down; `@4.0 pedal up` lets those notes release. The position is always
/// written, so a pedal mark never takes part in the sequential line model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PedalEvent {
    /// Beat position from the start of the owning phrase or pattern.
    pub beat: f64,
    /// `true` for `pedal down`, `false` for `pedal up`.
    pub down: bool,
}

/// A time range in bar:beat:subdivision format
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TimeRange {
    pub start: BarBeat,
    pub end: BarBeat,
}

/// Position in bar:beat:subdivision (all 1-indexed)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct BarBeat {
    pub bar: u32,
    pub beat: u32,
    pub subdivision: u32, // 0 = on the beat
}

impl BarBeat {
    /// A one-indexed bar/beat with a 0..3 subdivision.
    pub fn new(bar: u32, beat: u32, subdivision: u32) -> Self {
        Self {
            bar,
            beat,
            subdivision,
        }
    }
}

// ============================================================================
// Note Event
// ============================================================================

/// A single note with all its properties.
///
/// Notes have an absolute `start_beat` position within the phrase (0-indexed from
/// phrase start). This allows arbitrary placement, overlapping notes (chords),
/// and proper piano-roll editing.
///
/// When parsing sequential DSL (notes listed without explicit positions), the
/// parser auto-calculates start_beat from cumulative durations. The serializer
/// can output sequential format when notes are contiguous.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NoteEvent {
    /// Absolute beat position within the phrase (0.0 = phrase start).
    pub start_beat: f64,
    /// Whether the position was explicitly specified via @beat syntax.
    pub explicit_position: bool,
    pub pitch: Pitch,
    pub duration: Duration,
    pub dynamics: Option<Dynamics>,
    /// Exact MIDI velocity (1-127), overrides dynamics if set
    pub velocity: Option<u8>,
    pub articulation: Option<Articulation>,
    pub vibrato: Option<VibratoParams>,
    pub expression: Option<Vec<ExpressionCurve>>,
    pub pitch_bend: Option<f64>,
    /// Muted note — not played but still occupies time
    pub muted: bool,
    /// Grace note — ornamental, consumes negligible time
    pub grace: bool,
    /// Text annotation (lyrics, articulation text)
    pub text: Option<String>,
    pub modifiers: Vec<(String, ParamValue)>,
}

/// Scientific pitch: note name + accidental + octave
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Pitch {
    pub note: NoteName,
    pub accidental: Accidental,
    pub octave: i8,
}

impl Pitch {
    /// Convert a pitch in the unsigned note-number domain.
    /// Musical spellings such as A9 can exceed MIDI's 0..127 domain; use
    /// `checked_to_midi` at MIDI and other seven-bit input/output boundaries.
    pub fn to_midi(&self) -> u8 {
        self.semitone_number() as u8
    }

    /// Convert only pitches representable by the seven-bit MIDI note domain.
    pub fn checked_to_midi(&self) -> Option<u8> {
        let midi = self.semitone_number();
        (0..=127).contains(&midi).then_some(midi as u8)
    }

    /// Construct a MIDI pitch, preserving octave -1 and using the application's
    /// canonical spelling (C#, Eb, F#, Ab and Bb). Authored spellings are not
    /// rewritten by this conversion. Values above 127 are rejected.
    pub fn from_midi(midi: u8) -> Option<Self> {
        if midi > 127 {
            return None;
        }
        const NAMES: [(NoteName, Accidental); 12] = [
            (NoteName::C, Accidental::Natural),
            (NoteName::C, Accidental::Sharp),
            (NoteName::D, Accidental::Natural),
            (NoteName::E, Accidental::Flat),
            (NoteName::E, Accidental::Natural),
            (NoteName::F, Accidental::Natural),
            (NoteName::F, Accidental::Sharp),
            (NoteName::G, Accidental::Natural),
            (NoteName::A, Accidental::Flat),
            (NoteName::A, Accidental::Natural),
            (NoteName::B, Accidental::Flat),
            (NoteName::B, Accidental::Natural),
        ];
        let (note, accidental) = NAMES[usize::from(midi % 12)];
        Some(Self {
            note,
            accidental,
            octave: (i16::from(midi) / 12 - 1) as i8,
        })
    }

    /// Signed equal-tempered semitone number, with C-1 equal to zero.
    /// Unlike a MIDI key, this can represent accidentals below C-1.
    pub fn semitone_number(&self) -> i16 {
        let base = match self.note {
            NoteName::C => 0,
            NoteName::D => 2,
            NoteName::E => 4,
            NoteName::F => 5,
            NoteName::G => 7,
            NoteName::A => 9,
            NoteName::B => 11,
        };
        let accidental_offset = match self.accidental {
            Accidental::Natural => 0,
            Accidental::Sharp => 1,
            Accidental::DoubleSharp => 2,
            Accidental::Flat => -1,
            Accidental::DoubleFlat => -2,
        };
        (i16::from(self.octave) + 1) * 12 + base + accidental_offset
    }

    /// Convert to frequency in Hz (A4 = 440Hz)
    pub fn to_freq(&self) -> f64 {
        440.0 * 2.0_f64.powf((f64::from(self.semitone_number()) - 69.0) / 12.0)
    }

    /// Read written pitch text (`C4`, `F#3`, `Bb5`). `None` for anything else,
    /// including octaves outside -1..9, which no note line can spell.
    /// This parses musical notation; `checked_to_midi` separately checks MIDI range.
    pub fn from_text(text: &str) -> Option<Pitch> {
        let mut chars = text.chars();
        let note = match chars.next()? {
            'C' => NoteName::C,
            'D' => NoteName::D,
            'E' => NoteName::E,
            'F' => NoteName::F,
            'G' => NoteName::G,
            'A' => NoteName::A,
            'B' => NoteName::B,
            _ => return None,
        };
        let rest: String = chars.collect();
        let (accidental, digits) = if let Some(digits) = rest.strip_prefix("##") {
            (Accidental::DoubleSharp, digits)
        } else if let Some(digits) = rest.strip_prefix('#') {
            (Accidental::Sharp, digits)
        } else if let Some(digits) = rest.strip_prefix("bb") {
            (Accidental::DoubleFlat, digits)
        } else if let Some(digits) = rest.strip_prefix('b') {
            (Accidental::Flat, digits)
        } else {
            (Accidental::Natural, rest.as_str())
        };
        if digits != "-1" && (digits.len() != 1 || !digits.as_bytes()[0].is_ascii_digit()) {
            return None;
        }
        let octave = digits.parse::<i8>().ok()?;
        Some(Pitch {
            note,
            accidental,
            octave,
        })
    }

    /// The canonical spelling (`C4`, `F#3`, `Bb5`).
    pub fn to_text(&self) -> String {
        let note = match self.note {
            NoteName::C => "C",
            NoteName::D => "D",
            NoteName::E => "E",
            NoteName::F => "F",
            NoteName::G => "G",
            NoteName::A => "A",
            NoteName::B => "B",
        };
        let accidental = match self.accidental {
            Accidental::Natural => "",
            Accidental::Sharp => "#",
            Accidental::DoubleSharp => "##",
            Accidental::Flat => "b",
            Accidental::DoubleFlat => "bb",
        };
        format!("{note}{accidental}{}", self.octave)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Note letter without an accidental.
pub enum NoteName {
    C,
    D,
    E,
    F,
    G,
    A,
    B,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Accidental applied to a note letter.
pub enum Accidental {
    Natural,
    Sharp,
    DoubleSharp,
    Flat,
    DoubleFlat,
}

// ============================================================================
// Duration
// ============================================================================

/// Musical duration with optional dot, tuplet, and tie modifiers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Duration {
    pub base: BaseDuration,
    pub dots: u8, // 0, 1 (dotted), or 2 (double-dotted)
    pub triplet: bool,
    /// Arbitrary tuplet: `None` = no tuplet, `Some((n, m))` = n notes in time of m
    /// e.g., `Some((5, 4))` = 5:4 quintuplet, `None` and `triplet: true` = 3:2
    pub tuplet: Option<(u8, u8)>,
    /// Tie chain — tied note extensions (e.g., q~q~h = quarter tied to quarter tied to half)
    pub ties: Vec<BaseDuration>,
}

impl Duration {
    /// Convert to number of quarter-note beats
    pub fn to_beats(&self) -> f64 {
        let base_beats = match self.base {
            BaseDuration::Whole => 4.0,
            BaseDuration::Half => 2.0,
            BaseDuration::Quarter => 1.0,
            BaseDuration::Eighth => 0.5,
            BaseDuration::Sixteenth => 0.25,
            BaseDuration::ThirtySecond => 0.125,
            BaseDuration::SixtyFourth => 0.0625,
        };

        let mut beats = base_beats;

        // Apply dots
        match self.dots {
            1 => beats *= 1.5,
            2 => beats *= 1.75,
            _ => {}
        }

        // Apply tuplet
        if let Some((n, m)) = self.tuplet {
            beats = beats * m as f64 / n as f64;
        } else if self.triplet {
            beats *= 2.0 / 3.0;
        }

        // Apply ties
        for tie_base in &self.ties {
            let tie_beats = match tie_base {
                BaseDuration::Whole => 4.0,
                BaseDuration::Half => 2.0,
                BaseDuration::Quarter => 1.0,
                BaseDuration::Eighth => 0.5,
                BaseDuration::Sixteenth => 0.25,
                BaseDuration::ThirtySecond => 0.125,
                BaseDuration::SixtyFourth => 0.0625,
            };
            beats += tie_beats;
        }

        beats
    }

    /// Convert to seconds at given BPM
    pub fn to_seconds(&self, bpm: f64) -> f64 {
        self.to_beats() * 60.0 / bpm
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Notated note value before dots, tuplets or ties.
pub enum BaseDuration {
    Whole,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    SixtyFourth,
}

// ============================================================================
// Dynamics
// ============================================================================

/// Standard musical dynamics
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, PartialOrd)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Dynamics {
    PPP,
    PP,
    P,
    MP,
    MF,
    F,
    FF,
    FFF,
}

impl Dynamics {
    /// Resolve a standard lowercase DSL dynamics token.
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "ppp" => Self::PPP,
            "pp" => Self::PP,
            "p" => Self::P,
            "mp" => Self::MP,
            "mf" => Self::MF,
            "f" => Self::F,
            "ff" => Self::FF,
            "fff" => Self::FFF,
            _ => return None,
        })
    }

    /// Convert to MIDI velocity (1-127)
    pub fn to_velocity(&self) -> u8 {
        match self {
            Dynamics::PPP => 16,
            Dynamics::PP => 33,
            Dynamics::P => 49,
            Dynamics::MP => 64,
            Dynamics::MF => 80,
            Dynamics::F => 96,
            Dynamics::FF => 112,
            Dynamics::FFF => 127,
        }
    }

    /// Convert to normalized amplitude (0.0 - 1.0)
    pub fn to_amplitude(&self) -> f64 {
        self.to_velocity() as f64 / 127.0
    }
}

// ============================================================================
// Articulation
// ============================================================================

/// All supported articulations across instrument families
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Articulation {
    // Universal
    Legato,
    Staccato,
    Staccatissimo,
    Tenuto,
    Marcato,
    Accent,
    Sustained,
    Detached,

    // Strings
    Pizzicato,
    Arco,
    Tremolo,
    Trill,
    ColLegno,
    Harmonics,
    SulTasto,
    SulPonticello,

    // Brass / Woodwind
    Muted,
    Open,
    FlutterTongue,
    Sforzando,

    // Guitar
    PalmMute,
    HammerOn,
    PullOff,
    Slide,
    Bend,

    // Drums
    GhostNote,
    Flam,
    Roll,
    RimShot,
}

impl Articulation {
    /// Every articulation, in declaration order. Editors list this set.
    pub const ALL: [Articulation; 29] = [
        Articulation::Legato,
        Articulation::Staccato,
        Articulation::Staccatissimo,
        Articulation::Tenuto,
        Articulation::Marcato,
        Articulation::Accent,
        Articulation::Sustained,
        Articulation::Detached,
        Articulation::Pizzicato,
        Articulation::Arco,
        Articulation::Tremolo,
        Articulation::Trill,
        Articulation::ColLegno,
        Articulation::Harmonics,
        Articulation::SulTasto,
        Articulation::SulPonticello,
        Articulation::Muted,
        Articulation::Open,
        Articulation::FlutterTongue,
        Articulation::Sforzando,
        Articulation::PalmMute,
        Articulation::HammerOn,
        Articulation::PullOff,
        Articulation::Slide,
        Articulation::Bend,
        Articulation::GhostNote,
        Articulation::Flam,
        Articulation::Roll,
        Articulation::RimShot,
    ];

    /// DSL token this articulation is written as.
    pub fn as_token(self) -> &'static str {
        match self {
            Articulation::Legato => "legato",
            Articulation::Staccato => "staccato",
            Articulation::Staccatissimo => "staccatissimo",
            Articulation::Tenuto => "tenuto",
            Articulation::Marcato => "marcato",
            Articulation::Accent => "accent",
            Articulation::Sustained => "sustained",
            Articulation::Detached => "detached",
            Articulation::Pizzicato => "pizzicato",
            Articulation::Arco => "arco",
            Articulation::Tremolo => "tremolo",
            Articulation::Trill => "trill",
            Articulation::ColLegno => "col_legno",
            Articulation::Harmonics => "harmonics",
            Articulation::SulTasto => "sul_tasto",
            Articulation::SulPonticello => "sul_ponticello",
            Articulation::Muted => "muted",
            Articulation::Open => "open",
            Articulation::FlutterTongue => "flutter_tongue",
            Articulation::Sforzando => "sforzando",
            Articulation::PalmMute => "palm_mute",
            Articulation::HammerOn => "hammer_on",
            Articulation::PullOff => "pull_off",
            Articulation::Slide => "slide",
            Articulation::Bend => "bend",
            Articulation::GhostNote => "ghost_note",
            Articulation::Flam => "flam",
            Articulation::Roll => "roll",
            Articulation::RimShot => "rim_shot",
        }
    }

    /// Parse a DSL token. `None` means the token names no articulation.
    pub fn from_token(token: &str) -> Option<Articulation> {
        Articulation::ALL
            .into_iter()
            .find(|articulation| articulation.as_token() == token)
    }
}

// ============================================================================
// Expression & Vibrato
// ============================================================================

/// Vibrato parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct VibratoParams {
    pub rate_hz: Option<f64>,     // oscillation speed in Hz
    pub depth_cents: Option<f64>, // pitch deviation in cents
    pub delay_s: Option<f64>,     // onset delay in seconds
}

/// An expression curve (parameter change over the note's duration)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ExpressionCurve {
    pub param: String, // e.g. "dynamics", "brightness", "breathiness"
    pub start: ParamValue,
    pub end: Option<ParamValue>, // if None, constant at start value
}

// ============================================================================
// Pattern (drums / percussion)
// ============================================================================

/// A rhythmic pattern for drums/percussion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Pattern {
    pub name: String,
    pub time_range: Option<TimeRange>,
    pub repeat: u32, // number of repetitions
    pub hits: Vec<HitEvent>,
}

/// A single drum hit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HitEvent {
    pub drum: String, // e.g. "kick", "snare", "hihat"
    pub position: BeatPosition,
    pub dynamics: Option<Dynamics>,
    /// Exact MIDI velocity (1-127), overrides dynamics if set
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    pub articulation: Option<DrumArticulation>,
    pub modifiers: Vec<(String, ParamValue)>,
}

impl HitEvent {
    /// Quarter-note anchor representable by the beat and sixteenth subdivision.
    pub fn anchor_quarters(&self) -> f64 {
        f64::from(self.position.beat.saturating_sub(1))
            + f64::from(self.position.subdivision) * 0.25
    }

    /// Authored score position, before swing and expressive microtiming.
    pub fn written_quarters(&self) -> f64 {
        let offset = self.modifiers.iter().find_map(|(key, value)| match value {
            ParamValue::Number(offset) if key == "grid_offset_beats" && offset.is_finite() => {
                Some(*offset)
            }
            _ => None,
        });
        self.anchor_quarters() + offset.unwrap_or(0.0)
    }
}

/// Position within a bar: beat:subdivision
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct BeatPosition {
    pub beat: u32,        // 1-indexed
    pub subdivision: u32, // 0 = on the beat
}

/// Drum-specific articulations
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum DrumArticulation {
    Closed,
    Open,
    HalfOpen,
    GhostNote,
    Flam,
    Roll,
    RimShot,
    CrossStick,
}

// ============================================================================
// Control sources
// ============================================================================

/// A named control signal derived from one track's audio.
///
/// The DSL only configures the engine's follower; the follower itself lives in
/// `oc-engine` and is the single implementation of the control law.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ControlSource {
    pub name: String,
    /// Track whose post-insert, pre-fader audio feeds the detector.
    pub source_track: String,
    /// One-pole attack time constant in seconds.
    pub attack: f64,
    /// One-pole release time constant in seconds.
    pub release: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lfo: Option<ControlLfo>,
    #[serde(default = "default_lfo_depth")]
    pub lfo_depth: f64,
    /// Expression over the shaped envelope `x`; `None` keeps `x`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(default = "default_control_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default)]
    pub curve: ControlCurve,
}

fn default_lfo_depth() -> f64 {
    1.0
}

fn default_control_scale() -> f64 {
    1.0
}

/// A free-running bipolar modulator added to the shaped envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ControlLfo {
    pub shape: ControlLfoShape,
    /// Cycles per second.
    pub rate_hz: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Waveform of a control LFO.
pub enum ControlLfoShape {
    Sine,
    Triangle,
    Square,
    Saw,
}

/// Shaping applied to the envelope before the formula.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ControlCurve {
    #[default]
    Linear,
    Squared,
    Sqrt,
    Log,
}

/// A track's gain ride, evaluated per sample from a control source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ControlGain {
    pub source: String,
    /// Gain change at full control, in dB. Negative ducks, positive boosts.
    pub depth_db: f64,
}

// ============================================================================
// Control links
// ============================================================================

/// One control fanning out to many parameter targets.
///
/// The control is a MIDI CC, a synth macro, or an automation lane; every target
/// carries its own mapping formula and its own smoothing time, so a single
/// gesture can move several parameters with different responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "ControlLinkWire")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ControlLink {
    pub name: String,
    pub source: ControlLinkSource,
    /// Never empty after validation; a legacy single-target link loads as one row.
    pub targets: Vec<ControlLinkTarget>,
}

/// The wire form keeps the old `target` field working next to `targets`.
#[derive(Deserialize)]
struct ControlLinkWire {
    name: String,
    source: ControlLinkSource,
    #[serde(default)]
    targets: Vec<ControlLinkTarget>,
    #[serde(default)]
    target: Option<ControlLinkTarget>,
}

impl From<ControlLinkWire> for ControlLink {
    fn from(wire: ControlLinkWire) -> Self {
        let mut targets = wire.targets;
        if let Some(target) = wire.target {
            targets.push(target);
        }
        Self {
            name: wire.name,
            source: wire.source,
            targets,
        }
    }
}

/// Where a link's control value comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ControlLinkSource {
    /// A 7-bit MIDI controller; `channel` is 1-based and defaults to 1.
    MidiCc {
        #[serde(default = "default_midi_channel")]
        channel: u8,
        controller: u8,
    },
    /// One of the eight OpenSynth macros, 1-based.
    Macro { index: u8 },
    /// An automation lane on a track; its normalized value is the control.
    Lane { track: String, param: String },
}

fn default_midi_channel() -> u8 {
    1
}

impl ControlLinkSource {
    /// True when the source emits raw 7-bit MIDI values instead of 0..1.
    pub fn is_midi(&self) -> bool {
        matches!(self, ControlLinkSource::MidiCc { .. })
    }
}

/// How one target maps the control value to a normalized parameter value.
///
/// Every mapping is a documented curve over a fixed input domain, so a target's
/// response does not depend on which control feeds it. The formula says whether
/// it reads the control's normalized 0..1 position or its raw 0..127 value.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum LinkFormula {
    /// `y = x` over the normalized 0..1 position.
    #[default]
    Linear,
    /// `y = 1 - x` over the normalized 0..1 position.
    Inverted,
    /// `y = x^2`, an ease-in that leaves the low end fine-grained.
    Exponential,
    /// `y = sqrt(x)`, an ease-out that leaves the high end fine-grained.
    Logarithmic,
    /// `y = raw / 127`, the scaler from a 0..127 controller to 0..1.
    MidiUnit,
    /// Triangle over 0..127 with the top at 64: `raw/64` below the detent,
    /// `(127 - raw)/63` above it. Both ends map to 0.
    MidiCentered,
}

/// Which value a formula consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkFormulaInput {
    /// The control's normalized 0..1 position.
    Normalized,
    /// The controller's raw 0..127 value.
    Midi7Bit,
}

impl LinkFormula {
    /// Every formula, in menu order.
    pub const ALL: [LinkFormula; 6] = [
        LinkFormula::Linear,
        LinkFormula::Inverted,
        LinkFormula::Exponential,
        LinkFormula::Logarithmic,
        LinkFormula::MidiUnit,
        LinkFormula::MidiCentered,
    ];

    /// Canonical DSL spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkFormula::Linear => "linear",
            LinkFormula::Inverted => "inverted",
            LinkFormula::Exponential => "exponential",
            LinkFormula::Logarithmic => "logarithmic",
            LinkFormula::MidiUnit => "midi_unit",
            LinkFormula::MidiCentered => "midi_centered",
        }
    }

    /// Whether this formula reads the normalized 0..1 position or the raw 7-bit
    /// controller value.
    pub fn input(self) -> LinkFormulaInput {
        match self {
            LinkFormula::MidiUnit | LinkFormula::MidiCentered => LinkFormulaInput::Midi7Bit,
            _ => LinkFormulaInput::Normalized,
        }
    }

    /// Apply the mapping; the result is clamped to the normalized 0..1 domain.
    pub fn apply(self, value: f64) -> f64 {
        if !value.is_finite() {
            return 0.0;
        }
        let normalized = value.clamp(0.0, 1.0);
        let midi = value.clamp(0.0, 127.0);
        match self {
            LinkFormula::Linear => normalized,
            LinkFormula::Inverted => 1.0 - normalized,
            LinkFormula::Exponential => normalized * normalized,
            LinkFormula::Logarithmic => normalized.sqrt(),
            LinkFormula::MidiUnit => midi / 127.0,
            LinkFormula::MidiCentered => {
                if midi <= 64.0 {
                    (midi / 64.0).clamp(0.0, 1.0)
                } else {
                    ((127.0 - midi) / 63.0).clamp(0.0, 1.0)
                }
            }
        }
    }
}

impl std::fmt::Display for LinkFormula {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for LinkFormula {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        LinkFormula::ALL
            .into_iter()
            .find(|formula| formula.as_str() == value)
            .ok_or_else(|| format!("unknown link formula '{value}'"))
    }
}

/// One row of a fan-out: where the value goes and how it is shaped on the way.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ControlLinkTarget {
    /// Track that owns the parameter.
    pub track: String,
    /// Parameter key: an engine parameter or a hosted plugin parameter name.
    pub param: String,
    #[serde(default)]
    pub formula: LinkFormula,
    /// One-pole smoothing time in milliseconds; 0 disables smoothing.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub smoothing_ms: u32,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

impl ControlLink {
    /// The row for a target, if this link already drives it.
    pub fn target(&self, track: &str, param: &str) -> Option<&ControlLinkTarget> {
        self.targets
            .iter()
            .find(|target| target.track == track && target.param == param)
    }

    /// Learn one target: exactly one row per `(track, param)`. Learning a
    /// target that is already bound keeps its formula and smoothing.
    pub fn learn_target(&mut self, target: ControlLinkTarget) -> bool {
        if self.target(&target.track, &target.param).is_some() {
            return false;
        }
        self.targets.push(target);
        true
    }
}

// ============================================================================
// Automation
// ============================================================================

/// An automation lane for a track parameter
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AutomationLane {
    pub param: String, // parameter name
    /// Span of a clip; `None` drives the whole timeline from its first point on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    /// A muted lane writes nothing, so the target keeps its declared value.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// 0 keeps the legacy quarter-beat grid; otherwise subdivisions are ticks per quarter.
    #[serde(default)]
    pub ppq: u32,
    /// One-pole time constant in milliseconds; 0 keeps the raw curve values.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub smoothing_ms: f64,
    /// Envelope anchors, or one cycle of a repeating waveform.
    #[serde(default, skip_serializing_if = "LaneMode::is_envelope")]
    pub mode: LaneMode,
    /// Waveform cycle controls; only read in [`LaneMode::Lfo`].
    #[serde(default, skip_serializing_if = "LaneLfo::is_default")]
    pub lfo: LaneLfo,
    pub points: Vec<AutomationPoint>,
}

/// How a lane turns its points into a value.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum LaneMode {
    /// Points are absolute anchors; the value ramps between them and holds after the last.
    #[default]
    Envelope,
    /// Points are the shape of one cycle, repeated at the lane's speed.
    Lfo,
}

impl LaneMode {
    /// DSL keyword for the lane mode.
    pub fn keyword(self) -> &'static str {
        match self {
            LaneMode::Envelope => "envelope",
            LaneMode::Lfo => "lfo",
        }
    }

    /// Parse a lane-mode keyword.
    pub fn from_keyword(word: &str) -> Option<Self> {
        match word {
            "envelope" => Some(LaneMode::Envelope),
            "lfo" => Some(LaneMode::Lfo),
            _ => None,
        }
    }

    /// Whether the lane is a one-shot envelope.
    pub fn is_envelope(&self) -> bool {
        *self == LaneMode::Envelope
    }
}

/// Cycle rate of an LFO lane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum LfoSpeed {
    /// Cycles per second; a tempo change does not move this rate.
    Hz { hz: f64 },
    /// One cycle per note value: `1/4` is a quarter note, so a tempo change moves the period.
    Note { numerator: u32, denominator: u32 },
}

impl LfoSpeed {
    /// Cycle length in quarter notes when the speed follows the tempo.
    pub fn cycle_quarters(self) -> Option<f64> {
        match self {
            LfoSpeed::Note {
                numerator,
                denominator,
            } if denominator > 0 && numerator > 0 => {
                Some(f64::from(numerator) * 4.0 / f64::from(denominator))
            }
            _ => None,
        }
    }
}

/// One cycle of a lane's waveform, used only in [`LaneMode::Lfo`].
///
/// The point sequence is the shape of a single cycle: a point's relative
/// musical spacing is its phase, from zero at the first point to one at the
/// last. Its device value is the height at that phase. `speed` sets the cycle
/// length; the other controls warp the shape without moving its anchors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LaneLfo {
    #[serde(default = "default_lfo_speed")]
    pub speed: LfoSpeed,
    /// Cycles of the shape per speed cycle.
    #[serde(default = "default_lfo_mult")]
    pub mult: f64,
    /// Fraction of the cycle the shape's first half occupies; 0.5 is neutral.
    #[serde(default = "default_lfo_pulse_width")]
    pub pulse_width: f64,
    /// Phase bend in -1..=1: negative moves the shape earlier, positive later.
    #[serde(default)]
    pub skew: f64,
    /// Eases every segment's phase towards a smoothstep, in 0..=1.
    #[serde(default)]
    pub tension: f64,
    /// Depth around the shape's midpoint; 1 keeps the authored values.
    #[serde(default = "default_lfo_amount")]
    pub amount: f64,
}

/// Largest LFO rate multiplier.
pub const MAX_LFO_MULT: f64 = 64.0;
/// Largest LFO depth.
pub const MAX_LFO_AMOUNT: f64 = 4.0;
/// Slowest free-running LFO rate.
pub const MIN_LFO_SPEED_HZ: f64 = 0.001;

fn default_lfo_speed() -> LfoSpeed {
    LfoSpeed::Hz { hz: 1.0 }
}

fn default_lfo_mult() -> f64 {
    1.0
}

fn default_lfo_pulse_width() -> f64 {
    0.5
}

fn default_lfo_amount() -> f64 {
    1.0
}

impl Default for LaneLfo {
    fn default() -> Self {
        Self {
            speed: default_lfo_speed(),
            mult: default_lfo_mult(),
            pulse_width: default_lfo_pulse_width(),
            skew: 0.0,
            tension: 0.0,
            amount: default_lfo_amount(),
        }
    }
}

impl LaneLfo {
    /// Whether every LFO field is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A single automation point
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AutomationPoint {
    pub position: BarBeat,
    pub value: ParamValue,
    /// Shape of the segment from this point to the next one.
    #[serde(default, skip_serializing_if = "Curve::is_linear")]
    pub curve: Curve,
    /// Blend of `smooth` towards a full smoothstep, in 0..1; only with `smooth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tension: Option<f64>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// Upper bound for a lane's smoothing time; longer than any musical gesture.
pub const MAX_SMOOTHING_MS: f64 = 60_000.0;

/// Shape between one automation point and the next.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Curve {
    /// Straight line from this value to the next.
    #[default]
    Linear,
    /// Hold this value until the next point.
    Stairs,
    /// Half cosine: starts and ends with zero slope.
    HalfSine,
    /// Smoothstep from this value to the next, blended by `tension`.
    Smooth,
}

impl Curve {
    /// Whether the curve is linear.
    pub fn is_linear(&self) -> bool {
        *self == Curve::Linear
    }

    /// Keyword used in the DSL and by the lane editor.
    pub fn keyword(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::Stairs => "stairs",
            Curve::HalfSine => "half_sine",
            Curve::Smooth => "smooth",
        }
    }

    /// Parse a curve keyword.
    pub fn from_keyword(word: &str) -> Option<Self> {
        match word {
            "linear" => Some(Curve::Linear),
            "stairs" => Some(Curve::Stairs),
            "half_sine" | "half-sine" => Some(Curve::HalfSine),
            "smooth" => Some(Curve::Smooth),
            _ => None,
        }
    }
}

// ============================================================================
// Bus & Master
// ============================================================================

/// A post-insert gain clip with absolute project coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GainAutomation {
    pub time_range: TimeRange,
    pub interpolation: GainInterpolation,
    /// Subdivision units per beat within this clip only; legacy OCP uses four.
    #[serde(default = "default_gain_ppq")]
    pub ppq: u32,
    /// Absolute project positions; values are linear gain factors.
    pub points: Vec<AutomationPoint>,
}

fn default_gain_ppq() -> u32 {
    4
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// How gain automation interpolates between points.
pub enum GainInterpolation {
    Step,
    Linear,
}

/// A bus groups multiple tracks for shared processing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Bus {
    pub name: String,
    /// Outgoing bus/master targets. Empty means master for compatibility.
    pub routes: Vec<String>,
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub gain_automation: Vec<GainAutomation>,
    /// Effect-parameter lanes; `<effect>.<parameter>` names the target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationLane>,
    /// Post-effects return fader; 0.0 is unity.
    #[serde(default)]
    pub volume_db: f64,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Device-input routes owned by this bus's effect chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_routes: Vec<InputRoute>,
}

/// Master output configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Master {
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub gain_automation: Vec<GainAutomation>,
    /// Effect-parameter lanes; `<effect>.<parameter>` names the target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationLane>,
    pub loudness_target: Option<f64>, // target LUFS
    /// Post-effects master fader; 0.0 is unity. No master pan: the graph pan
    /// law is source-width dependent, not a stereo balance control.
    #[serde(default)]
    pub volume_db: f64,
    /// Device-input routes owned by the master effect chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_routes: Vec<InputRoute>,
}

// ============================================================================
// Section (arrangement structure)
// ============================================================================

/// A named section of the arrangement (intro, verse, chorus, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Section {
    pub name: String,
    pub time_range: TimeRange,
    pub repeat: Option<u32>,
    pub label: Option<String>,
    /// Technique in force for note onsets inside this section. A note's own
    /// articulation wins over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<Articulation>,
}

/// A named point on the timeline, written as `@marker "name" at 9:1`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Marker {
    pub name: String,
    pub position: BarBeat,
}

// ============================================================================
// Common Types
// ============================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Notated meter as numerator over denominator.
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Key root, accidental and mode.
pub struct KeySignature {
    pub root: NoteName,
    pub accidental: Accidental,
    pub mode: KeyMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Major or minor key.
pub enum KeyMode {
    Major,
    Minor,
}

/// A generic parameter value (used throughout the DSL)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ParamValue {
    Number(f64),
    Db(f64),
    Percentage(f64),
    Frequency(f64), // Hz
    Cents(f64),     // cents
    Time(f64),      // seconds
    String(String),
    Bool(bool),
    /// Bounded numeric list, used by drawn LFO shapes.
    Array(Vec<f64>),
}

impl ParamValue {
    /// Get the numeric value regardless of unit
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ParamValue::Number(v) => Some(*v),
            ParamValue::Db(v) => Some(*v),
            ParamValue::Percentage(v) => Some(*v),
            ParamValue::Frequency(v) => Some(*v),
            ParamValue::Cents(v) => Some(*v),
            ParamValue::Time(v) => Some(*v),
            _ => None,
        }
    }
}
/// Explicit performance gate in quarter notes; legacy duration metadata is inert.
pub fn performance_gate_quarters(
    modifiers: &[(String, ParamValue)],
) -> Result<Option<f64>, String> {
    let mut values = modifiers.iter().filter(|(key, _)| key == "gate_quarters");
    let Some((_, value)) = values.next() else {
        return Ok(None);
    };
    match value {
        ParamValue::Number(gate) if values.next().is_none() && gate.is_finite() && *gate > 0.0 => {
            Ok(Some(*gate))
        }
        _ => Err("gate_quarters must be one finite positive number".into()),
    }
}

// ============================================================================
// JSON compatibility layer
//
// The document shape is unchanged: a track still writes `clips` (pattern
// placements only) and `automation`, and the project still writes
// `properties.audio_clips`. Audio clips live in the track pool in memory, so
// the project converts them back on the way out and distributes them by track
// name on the way in.
// ============================================================================

/// JSON shape of a track. Keep the fields and their `serde` attributes in sync
/// with [`Track`]; only `clips` and `automation` differ, because the pool is
/// not a JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(rename = "Track"))]
pub struct TrackWire {
    pub name: String,
    pub instrument: InstrumentPath,
    pub preset: Option<String>,
    pub engine: Option<String>,
    pub mix: MixState,
    pub effects: Vec<Effect>,
    pub phrases: Vec<Phrase>,
    pub patterns: Vec<Pattern>,
    pub inline_patterns: Vec<PatternDef>,
    pub clips: Vec<Clip>,
    pub automation: Vec<AutomationLane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gain_automation: Vec<GainAutomation>,
    pub engine_params: Option<Vec<(String, ParamValue)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<crate::performance::PerformanceTimeline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_pair: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_routes: Vec<InputRoute>,
}

impl From<&Track> for TrackWire {
    fn from(track: &Track) -> Self {
        Self {
            name: track.name.clone(),
            instrument: track.instrument.clone(),
            preset: track.preset.clone(),
            engine: track.engine.clone(),
            mix: track.mix.clone(),
            effects: track.effects.clone(),
            phrases: track.phrases.clone(),
            patterns: track.patterns.clone(),
            inline_patterns: track.inline_patterns.clone(),
            clips: track.pattern_clips().cloned().collect(),
            automation: track.automation().cloned().collect(),
            gain_automation: track.gain_automation.clone(),
            engine_params: track.engine_params.clone(),
            performance: track.performance.clone(),
            channel: track.channel,
            output_port: track.output_port,
            output_pair: track.output_pair,
            input_routes: track.input_routes.clone(),
        }
    }
}

impl From<Track> for TrackWire {
    fn from(track: Track) -> Self {
        Self::from(&track)
    }
}

impl From<TrackWire> for Track {
    fn from(wire: TrackWire) -> Self {
        let mut clips = ClipPool::new();
        clips.set_pattern_clips(wire.clips);
        clips.set_automation_lanes(wire.automation);
        Self {
            name: wire.name,
            instrument: wire.instrument,
            preset: wire.preset,
            engine: wire.engine,
            mix: wire.mix,
            effects: wire.effects,
            phrases: wire.phrases,
            patterns: wire.patterns,
            inline_patterns: wire.inline_patterns,
            clips,
            gain_automation: wire.gain_automation,
            engine_params: wire.engine_params,
            performance: wire.performance,
            channel: wire.channel,
            output_port: wire.output_port,
            output_pair: wire.output_pair,
            input_routes: wire.input_routes,
        }
    }
}

/// JSON shape of [`ProjectProperties`]. Keep the fields and their `serde`
/// attributes in sync; `audio_clips` is filled from the track pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(rename = "ProjectProperties"))]
pub struct ProjectPropertiesWire {
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub key: Option<KeySignature>,
    pub swing: Option<f64>,
    #[serde(default, skip_serializing_if = "SwingSubdivision::is_eighth")]
    pub swing_subdivision: SwingSubdivision,
    #[serde(default, skip_serializing_if = "SoloMode::is_both")]
    pub solo_mode: SoloMode,
    pub tempo_map: Vec<TempoPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_clock: Option<crate::timecode::PictureClock>,
    pub time_sig_map: Vec<TimeSigPoint>,
    pub key_map: Vec<KeyPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_clips: Vec<AudioClip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vocal_tunes: Vec<VocalTune>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vocal_harmonies: Vec<VocalHarmony>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpe: Option<MpeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_resample: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_sources: Vec<ControlSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_links: Vec<ControlLink>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::automation_normalization::deserialize_optional"
    )]
    pub automation_normalization: Option<u8>,
}

impl ProjectPropertiesWire {
    /// The wire shape of `properties` with the audio clips gathered from tracks.
    pub fn from_properties(properties: &ProjectProperties, audio_clips: Vec<AudioClip>) -> Self {
        Self {
            tempo: properties.tempo,
            time_signature: properties.time_signature,
            key: properties.key,
            swing: properties.swing,
            swing_subdivision: properties.swing_subdivision,
            solo_mode: properties.solo_mode,
            tempo_map: properties.tempo_map.clone(),
            picture_clock: properties.picture_clock.clone(),
            time_sig_map: properties.time_sig_map.clone(),
            key_map: properties.key_map.clone(),
            audio_clips,
            vocal_tunes: properties.vocal_tunes.clone(),
            vocal_harmonies: properties.vocal_harmonies.clone(),
            mpe: properties.mpe,
            export: properties.export,
            live_resample: properties.live_resample,
            control_sources: properties.control_sources.clone(),
            control_links: properties.control_links.clone(),
            automation_normalization: properties.automation_normalization,
        }
    }

    /// Split into the in-memory properties and the audio clips a project
    /// distributes into its tracks.
    pub fn into_parts(self) -> (ProjectProperties, Vec<AudioClip>) {
        let properties = ProjectProperties {
            tempo: self.tempo,
            time_signature: self.time_signature,
            key: self.key,
            swing: self.swing,
            swing_subdivision: self.swing_subdivision,
            solo_mode: self.solo_mode,
            tempo_map: self.tempo_map,
            picture_clock: self.picture_clock,
            time_sig_map: self.time_sig_map,
            key_map: self.key_map,
            vocal_tunes: self.vocal_tunes,
            vocal_harmonies: self.vocal_harmonies,
            mpe: self.mpe,
            export: self.export,
            live_resample: self.live_resample,
            control_sources: self.control_sources,
            control_links: self.control_links,
            automation_normalization: self.automation_normalization,
        };
        (properties, self.audio_clips)
    }
}

impl From<ProjectPropertiesWire> for ProjectProperties {
    fn from(wire: ProjectPropertiesWire) -> Self {
        wire.into_parts().0
    }
}

impl From<ProjectProperties> for ProjectPropertiesWire {
    fn from(properties: ProjectProperties) -> Self {
        Self::from_properties(&properties, Vec::new())
    }
}

/// JSON shape of [`Project`]. Keep the fields and their `serde` attributes in
/// sync with [`Project`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(rename = "Project"))]
pub struct ProjectWire {
    pub name: Option<String>,
    pub ocp_version: Option<String>,
    pub properties: ProjectPropertiesWire,
    pub presets: Vec<PresetDef>,
    pub tracks: Vec<TrackWire>,
    pub patterns: Vec<PatternDef>,
    pub buses: Vec<Bus>,
    pub master: Option<Master>,
    pub sections: Vec<Section>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ClipGroup>,
}

impl ProjectWire {
    fn from_project(project: &Project) -> Self {
        let audio_clips = project
            .audio_clips()
            .into_iter()
            .cloned()
            .collect::<Vec<AudioClip>>();
        Self {
            name: project.name.clone(),
            ocp_version: project.ocp_version.clone(),
            properties: ProjectPropertiesWire::from_properties(&project.properties, audio_clips),
            presets: project.presets.clone(),
            tracks: project.tracks.iter().map(TrackWire::from).collect(),
            patterns: project.patterns.clone(),
            buses: project.buses.clone(),
            master: project.master.clone(),
            sections: project.sections.clone(),
            markers: project.markers.clone(),
            groups: project.groups.clone(),
        }
    }
}

impl Serialize for Project {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ProjectWire::from_project(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Project {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProjectWire::deserialize(deserializer)?;
        let (properties, audio_clips) = wire.properties.into_parts();
        let mut project = Project {
            name: wire.name,
            ocp_version: wire.ocp_version,
            properties,
            presets: wire.presets,
            tracks: wire.tracks.into_iter().map(Track::from).collect(),
            patterns: wire.patterns,
            buses: wire.buses,
            master: wire.master,
            sections: wire.sections,
            markers: wire.markers,
            groups: wire.groups,
        };
        project
            .set_audio_clips(audio_clips)
            .map_err(serde::de::Error::custom)?;
        Ok(project)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for Track {
    fn schema_name() -> String {
        "Track".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        TrackWire::json_schema(generator)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for ProjectProperties {
    fn schema_name() -> String {
        "ProjectProperties".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        ProjectPropertiesWire::json_schema(generator)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for Project {
    fn schema_name() -> String {
        "Project".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        ProjectWire::json_schema(generator)
    }
}

impl Project {
    /// Automation lanes owned by `owner`, in pool order.
    pub fn automation(&self, owner: &AutomationOwner) -> Vec<&AutomationLane> {
        match owner {
            AutomationOwner::Track(index) => self
                .tracks
                .get(*index)
                .map(|track| track.automation().collect())
                .unwrap_or_default(),
            AutomationOwner::Bus(name) => self
                .buses
                .iter()
                .find(|bus| &bus.name == name)
                .map(|bus| bus.automation.iter().collect())
                .unwrap_or_default(),
            AutomationOwner::Master => self
                .master
                .as_ref()
                .map(|master| master.automation.iter().collect())
                .unwrap_or_default(),
        }
    }

    /// Automation lanes owned by `owner`, mutably and in pool order.
    pub fn automation_mut(&mut self, owner: &AutomationOwner) -> Vec<&mut AutomationLane> {
        match owner {
            AutomationOwner::Track(index) => self
                .tracks
                .get_mut(*index)
                .map(|track| track.automation_mut().collect())
                .unwrap_or_default(),
            AutomationOwner::Bus(name) => self
                .buses
                .iter_mut()
                .find(|bus| &bus.name == name)
                .map(|bus| bus.automation.iter_mut().collect())
                .unwrap_or_default(),
            AutomationOwner::Master => self
                .master
                .as_mut()
                .map(|master| master.automation.iter_mut().collect())
                .unwrap_or_default(),
        }
    }

    /// Number of automation lanes owned by `owner`.
    pub fn automation_count(&self, owner: &AutomationOwner) -> usize {
        self.automation(owner).len()
    }

    /// The automation lane at `index` among the owner's lanes.
    pub fn automation_lane(
        &self,
        owner: &AutomationOwner,
        index: usize,
    ) -> Option<&AutomationLane> {
        match owner {
            AutomationOwner::Track(track) => self.tracks.get(*track)?.automation_lane(index),
            AutomationOwner::Bus(name) => self
                .buses
                .iter()
                .find(|bus| &bus.name == name)?
                .automation
                .get(index),
            AutomationOwner::Master => self.master.as_ref()?.automation.get(index),
        }
    }

    /// The automation lane at `index`, mutably.
    pub fn automation_lane_mut(
        &mut self,
        owner: &AutomationOwner,
        index: usize,
    ) -> Option<&mut AutomationLane> {
        match owner {
            AutomationOwner::Track(track) => {
                self.tracks.get_mut(*track)?.automation_lane_mut(index)
            }
            AutomationOwner::Bus(name) => self
                .buses
                .iter_mut()
                .find(|bus| &bus.name == name)?
                .automation
                .get_mut(index),
            AutomationOwner::Master => self.master.as_mut()?.automation.get_mut(index),
        }
    }

    /// Number of audio clips in the project.
    pub fn audio_clip_count(&self) -> usize {
        self.tracks
            .iter()
            .map(|track| track.clips.audio_count())
            .sum()
    }

    /// Global order and pool location of every audio clip, document-ordered.
    fn audio_locations(&self) -> Vec<(usize, usize, usize)> {
        let mut locations = Vec::with_capacity(self.audio_clip_count());
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (pool_index, clip) in track.clips.iter().enumerate() {
                if matches!(clip.payload, ClipPayload::Audio(_)) {
                    locations.push((clip.order, track_index, pool_index));
                }
            }
        }
        locations.sort_by_key(|(order, _, _)| *order);
        locations
    }

    /// Audio clips in document order: the project-wide `@audio_clip` sequence.
    pub fn audio_clips(&self) -> Vec<&AudioClip> {
        self.audio_locations()
            .into_iter()
            .filter_map(|(_, track, pool)| self.tracks.get(track)?.clips.get(pool)?.as_audio())
            .collect()
    }

    /// Every audio clip with its global order, in document order.
    pub fn ordered_audio_clips(&self) -> Vec<(usize, &AudioClip)> {
        self.audio_locations()
            .into_iter()
            .filter_map(|(order, track, pool)| {
                Some((order, self.tracks.get(track)?.clips.get(pool)?.as_audio()?))
            })
            .collect()
    }

    /// The audio clip at global `index`.
    pub fn audio_clip(&self, index: usize) -> Option<&AudioClip> {
        self.audio_clips().get(index).copied()
    }

    /// The audio clip at global `index`, mutably.
    pub fn audio_clip_mut(&mut self, index: usize) -> Option<&mut AudioClip> {
        let (_, track, pool) = *self.audio_locations().get(index)?;
        self.tracks
            .get_mut(track)?
            .clips
            .get_mut(pool)?
            .as_audio_mut()
    }

    /// Every audio clip, in track order and mutably.
    pub fn audio_clips_mut(&mut self) -> impl Iterator<Item = &mut AudioClip> {
        self.tracks
            .iter_mut()
            .flat_map(|track| track.audio_clips_mut())
    }

    /// The next free global audio order.
    pub fn next_audio_order(&self) -> usize {
        self.audio_locations()
            .last()
            .map_or(0, |(order, _, _)| order + 1)
    }

    /// Place one audio clip on the track its `track:` field names.
    pub fn add_audio_clip(&mut self, clip: AudioClip) -> Result<(), String> {
        let order = self.next_audio_order();
        self.place_audio_clip(order, clip)
    }

    /// Insert one audio clip at global `index`, shifting the rest.
    pub fn insert_audio_clip(&mut self, index: usize, clip: AudioClip) -> Result<(), String> {
        let mut clips = self.audio_clips().into_iter().cloned().collect::<Vec<_>>();
        let index = index.min(clips.len());
        clips.insert(index, clip);
        self.set_audio_clips(clips)
    }

    /// Replace every audio clip, numbering them in the given order.
    pub fn set_audio_clips(
        &mut self,
        clips: impl IntoIterator<Item = AudioClip>,
    ) -> Result<(), String> {
        let ordered = clips.into_iter().enumerate().collect::<Vec<_>>();
        self.set_ordered_audio_clips(ordered)
    }

    /// Replace every audio clip with explicit global orders.
    pub fn set_ordered_audio_clips(
        &mut self,
        clips: impl IntoIterator<Item = (usize, AudioClip)>,
    ) -> Result<(), String> {
        for track in &mut self.tracks {
            track.clips.set_audio_clips(std::iter::empty());
        }
        for (order, clip) in clips {
            self.place_audio_clip(order, clip)?;
        }
        Ok(())
    }

    /// Remove every audio clip from the project.
    pub fn clear_audio_clips(&mut self) {
        for track in &mut self.tracks {
            track.clips.set_audio_clips(std::iter::empty());
        }
    }

    /// Remove the audio clip at global `index`.
    pub fn remove_audio_clip(&mut self, index: usize) -> Option<AudioClip> {
        let (_, track, pool) = *self.audio_locations().get(index)?;
        self.tracks
            .get_mut(track)?
            .clips
            .remove(pool)
            .and_then(|clip| match clip.payload {
                ClipPayload::Audio(clip) => Some(clip),
                _ => None,
            })
    }

    fn place_audio_clip(&mut self, order: usize, clip: AudioClip) -> Result<(), String> {
        let track = self
            .tracks
            .iter_mut()
            .find(|track| track.name == clip.track)
            .ok_or_else(|| {
                format!(
                    "Unknown audio clip track reference: '{}' (in audio clip '{}')",
                    clip.track, clip.name
                )
            })?;
        track.clips.push_audio_at(order, clip);
        Ok(())
    }
}
