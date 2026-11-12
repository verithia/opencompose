//! Native polyphonic synthesizer used by the built-in instrument backend.
//!
//! The parameter names in [`SynthPatch`] are a versioned project-file ABI. Keep
//! existing names and meanings stable when adding future modulation features.
//!
//! Threading: [`OpenSynth`] is single-threaded; track parallelism lives in the
//! renderer, not in the audio callback.

use crate::midi_event::{MidiEvent, MidiMessage};
use oc_device_schema::synth::{
    AMPLITUDE_FRACTION, BIPOLAR_MODULATION, FILTER_CUTOFF_HZ, FILTER_RUNTIME_BOUNDS, LFO_RATE_HZ,
    STEP_RATE_HZ, TUNING_FINE, TUNING_REFERENCE_HZ, TUNING_TRANSPOSE, UNIT_FRACTION,
};
use oc_dsl::ast::ParamValue;
use oc_dsp::kernel::{HALF_BAND_CENTER, half_band_kernel, kaiser, sinc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::f64::consts::TAU;
use std::sync::Arc;
use std::sync::OnceLock;

/// Built-in OpenSynth plugin id.
pub const PLUGIN_ID: &str = "builtin.open_synth";
/// Parameter that carries the engine's per-patch loudness trim, in dB.
pub const LOUDNESS_TRIM_PARAM: &str = "output.loudness_trim_db";
/// Allocation-free arpeggiator lanes.
pub mod arpeggiator;
/// Portable patch documents.
pub mod document;
/// Filter identifier mapping.
pub mod filter_id;
/// Granular source oscillator.
pub mod granular;
/// Deterministic noise helpers.
pub mod random;
/// Spectral source oscillator.
pub mod spectral;
pub use oc_synth_fx as fx;
pub use oc_synth_fx::{SynthFx, SynthFxType};
#[cfg(test)]
mod filter_response;
#[cfg(test)]
mod levels;
mod preview;
mod sub_osc;
mod sub_shapes;
#[cfg(test)]
mod warp_attribution;
#[cfg(test)]
mod warp_laws;
mod warp_mod;
pub use preview::{
    DEFAULT_PREVIEW_SAMPLES, MAX_PREVIEW_SAMPLES, PREVIEW_NOTE, PREVIEW_PREROLL,
    PREVIEW_SAMPLE_RATE, WaveformPreview, WaveformPreviewer, waveform_preview,
};
pub use sub_osc::SubShape;
const MAX_VOICES: usize = 32;
/// Most per-voice traces one preview returns; the rest are summed only.
const MAX_PREVIEW_TRACES: usize = 8;
const MPE_UNASSIGNED: u8 = u8::MAX;
const MPE_LOWER_ZONE: usize = 0;
const MPE_UPPER_ZONE: usize = 1;
const MAX_UNISON_VOICES: usize = 16;
const PHASE_B: usize = MAX_UNISON_VOICES;
const PHASE_C: usize = PHASE_B + 1;
const PHASE_SUB: usize = PHASE_C + 1;
const PHASE_COUNT: usize = PHASE_SUB + 1;
/// Unison pan centre compensation: keeps the summed level flat across width.
const UNISON_PAN_BASIS: f64 = 0.415_891_568_768_275_8;
pub const MAX_MOD_ROUTES: usize = 64;
/// Maximum number of FX-parameter modulation routes in a patch. FX routes are
/// evaluated at control rate, so this only bounds patch size, not audio cost.
const MAX_FX_MOD_ROUTES: usize = 64;
/// FX modulation is control-rate: parameters refresh every 32 samples. This
/// keeps a 48 kHz signal's FX sweep at ~1.5 kHz while avoiding per-sample
/// parameter lookups and leaving the voice loop untouched.
const FX_MOD_CONTROL_INTERVAL: u32 = 32;
/// Fraction of a ladder's oscillation feedback used at full resonance. A
/// four-pole cascade oscillates at a loop gain of 4, so 0.875 of it leaves the
/// loop strictly decaying and caps the peak near +18 dB.
const LADDER_FEEDBACK_MARGIN: f64 = 0.875;
/// Resolution of the warp transfer tables. 4096 points keeps interpolation
/// error far below the noise floor while staying cache-resident.
/// Shared sine table used by oscillators, the sub oscillator, ring warp and LFOs.
const SINE_TABLE_SIZE: usize = 8192;
/// Resolution of the exponential envelope-shape tables.
const ENV_TABLE_SIZE: usize = 1024;
/// Points in the shared user LFO curve; fixed so the audio path never allocates.
pub const LFO_CURVE_POINTS: usize = 256;
/// Capacity of an LFO shape; a shape stores up to this many breakpoints.
pub const LFO_SHAPE_POINTS: usize = 480;
/// Map steps per LFO cycle: 32 keeps the interpolated contour smooth at 100 Hz.
pub const LFO_CHAOS_STEPS: f64 = 32.0;
/// Upper bound on map iterations consumed by one advance call (audio-thread guard).
const LFO_CHAOS_MAX_STEPS: usize = 4096;
/// Sync divisions in beats per cycle: 14 octaves of straight/dotted/triplet,
/// plus one step; entry 22 is one quarter note.
pub const LFO_SYNC_DIVISIONS: [f64; 43] = sync_division_table();
/// A zero-length attack still ramps for this long, so notes start from silence.
const MIN_ENVELOPE_ATTACK_SECONDS: f64 = 0.001;
/// Longest one-pole time constant for a per-LFO `Smooth` setting, in seconds.
const MAX_LFO_SMOOTH_SECONDS: f64 = 0.5;
/// Longest one-pole rise/fall time for a modulation route's slew, in seconds.
const MAX_ROUTE_SLEW_SECONDS: f64 = oc_device_schema::synth::MOD_ROUTE_SLEW.max;
/// Key whose decay is the preset's `amp.decay`; damping is measured from here.
const MIDDLE_C: f64 = 60.0;
/// Holds shorter than this get the full short-note release shortening.
const SHORT_NOTE_SECONDS: f64 = 0.4;
/// `amp.damping` at 1 shortens the release by half this much per octave up.
const RELEASE_KEYTRACK: f64 = 0.6;
/// `amp.damping` at 1 halves the release of a very short note.
const RELEASE_SHORT_DEPTH: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Waveform {
    Sine,
    Saw,
    Square,
    Triangle,
    Wavetable,
    // Appended after the original 0..=4 mapping; do not renumber.
    Granular,
    Spectral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMode {
    LowPass,
    BandPass,
    HighPass,
    Notch,
}

/// Voice filter topology. `Svf` is the legacy 2-pole state-variable filter;
/// `Ladder1`/`Ladder3`/`Ladder` are 1/3/4-pole TPT ladders; `DirtyLadder`
/// adds per-stage saturation. Discriminants are the serialized ABI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterModel {
    #[default]
    Svf = 0,
    /// 4-pole TPT ladder. The imported `LadderMg/Acid/EMS` modes; also the placeholder for
    /// `MgL*` until the Moog model exists.
    Ladder = 1,
    DirtyLadder = 2,
    /// Legacy 3-pole ladder; the importer does not produce it.
    Ladder3 = 3,
    /// Legacy 1-pole ladder; the importer does not produce it.
    Ladder1 = 4,
    /// First-order tap of the SVF class (imported `L6`/`H6`, 6 dB/oct).
    Svf1 = 5,
    /// Two-pole resonant core plus a first-order section (18 dB/oct).
    Svf3 = 6,
    /// Two-pole resonant core plus a two-pole Butterworth section (24 dB/oct).
    Svf4 = 7,
    /// Four-pole ladder tapped after 1, 2, 3 or 4 stages (6/12/18/24 dB/oct).
    Moog6 = 8,
    Moog12 = 9,
    Moog18 = 10,
    Moog24 = 11,
}

/// How the two filters are connected. `Serial` is Osc -> Filter 1 -> Filter 2;
/// `Parallel` feeds both filters from the oscillator mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterRouting {
    Serial,
    Parallel,
}

/// Where one source enters the dual-filter section when per-source routing is
/// active. `Bypass` skips both filters and sums into the voice output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFilterTarget {
    #[default]
    Filter1,
    Filter2,
    Both,
    Bypass,
}

/// Per-source filter routing. `balance` is the filter 2 share for `Both`
/// (0.0 = filter 1, 1.0 = filter 2); it is ignored by the other targets.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFilterRoute {
    #[serde(default)]
    pub target: SourceFilterTarget,
    #[serde(default)]
    pub balance: f64,
}

impl Default for SourceFilterRoute {
    fn default() -> Self {
        Self {
            target: SourceFilterTarget::Filter1,
            balance: 0.0,
        }
    }
}

/// What produces an LFO's cycle value. `Curve` and `Path` read the shape
/// breakpoints; the rest are generators.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum LfoShapeType {
    /// Drawn breakpoints with a per-segment bend, the default.
    #[default]
    Curve = 0,
    /// A smooth path through the breakpoints, cyclic Catmull-Rom.
    Path = 1,
    /// Our first bounded chaotic map.
    ChaosA = 2,
    /// Our second bounded chaotic map.
    ChaosB = 3,
    /// One random value held per cycle.
    RandomHold = 4,
}

impl LfoShapeType {
    pub fn from_value(value: f64) -> Self {
        match value.round() as i32 {
            1 => Self::Path,
            2 => Self::ChaosA,
            3 => Self::ChaosB,
            4 => Self::RandomHold,
            _ => Self::Curve,
        }
    }
}

/// Retired five-waveform selector, still parsed from old documents. It now
/// only seeds the shape, so the engine has a single curve evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyLfoWaveform {
    Sine,
    Triangle,
    Saw,
    Square,
    SampleAndHold,
    Curve,
    Smooth,
    Stepped,
}

impl LegacyLfoWaveform {
    fn from_value(value: f64) -> Self {
        match value.round() as i32 {
            1 => Self::Triangle,
            2 => Self::Saw,
            3 => Self::Square,
            4 => Self::SampleAndHold,
            5 => Self::Curve,
            6 => Self::Smooth,
            7 => Self::Stepped,
            _ => Self::Sine,
        }
    }
}

/// Up to [`LFO_SHAPE_POINTS`] breakpoints, sorted by `x`, plus the bend of the
/// segment that starts at each breakpoint (0.5 = straight).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoShape {
    pub x: [f32; LFO_SHAPE_POINTS],
    pub y: [f32; LFO_SHAPE_POINTS],
    pub curve: [f32; LFO_SHAPE_POINTS],
    /// Active breakpoints, at least 2.
    pub points: usize,
}

impl Default for LfoShape {
    fn default() -> Self {
        Self::triangle()
    }
}

impl LfoShape {
    /// The canonical default: a unipolar triangle that is bit-compatible with
    /// the retired triangle waveform after the source conversion.
    pub const fn triangle() -> Self {
        let mut x = [0.0f32; LFO_SHAPE_POINTS];
        let mut y = [0.0f32; LFO_SHAPE_POINTS];
        let curve = [0.5f32; LFO_SHAPE_POINTS];
        x[1] = 0.5;
        x[2] = 1.0;
        y[1] = 1.0;
        Self {
            x,
            y,
            curve,
            points: 3,
        }
    }

    pub fn is_triangle(&self) -> bool {
        self.points == 3
            && self.x[..3] == [0.0, 0.5, 1.0]
            && self.y[..3] == [0.0, 1.0, 0.0]
            && self.curve[..3] == [0.5, 0.5, 0.5]
    }

    /// Sort the active breakpoints by `x`; evaluation assumes ascending order.
    fn sort_points(&mut self) {
        let points = self.points.clamp(2, LFO_SHAPE_POINTS);
        for index in 1..points {
            let mut position = index;
            while position > 0 && self.x[position - 1] > self.x[position] {
                self.x.swap(position - 1, position);
                self.y.swap(position - 1, position);
                self.curve.swap(position - 1, position);
                position -= 1;
            }
        }
        self.points = points;
    }

    /// Build a shape from breakpoint lists; a short bend list uses 0.5.
    pub fn from_points(x: &[f64], y: &[f64], curve: &[f64]) -> Self {
        let mut shape = Self::triangle();
        let mut order: Vec<usize> = (0..x.len().min(y.len()).min(LFO_SHAPE_POINTS)).collect();
        order.sort_by(|a, b| x[*a].total_cmp(&x[*b]));
        shape.points = order.len().clamp(2, LFO_SHAPE_POINTS);
        for (target, source) in order.iter().take(shape.points).enumerate() {
            shape.x[target] = x[*source].clamp(0.0, 1.0) as f32;
            shape.y[target] = y[*source].clamp(0.0, 1.0) as f32;
            shape.curve[target] = curve.get(*source).copied().unwrap_or(0.5).clamp(0.0, 1.0) as f32;
        }
        if shape.points == 2 {
            // A two-breakpoint shape must span the cycle or its wrap is empty.
            shape.x[1] = 1.0;
        }
        shape
    }

    /// Built-in sine shape for old documents that name the retired waveform.
    pub fn sine() -> Self {
        let x: Vec<f64> = (0..LFO_SHAPE_POINTS)
            .map(|index| index as f64 / LFO_SHAPE_POINTS as f64)
            .collect();
        let y: Vec<f64> = x
            .iter()
            .map(|phase| 0.5 + 0.5 * (std::f64::consts::TAU * phase).sin())
            .collect();
        Self::from_points(&x, &y, &[0.5; LFO_SHAPE_POINTS])
    }

    pub fn saw() -> Self {
        Self::from_points(&[0.0, 1.0], &[0.0, 1.0], &[0.5, 0.5])
    }

    pub fn square() -> Self {
        Self::from_points(&[0.0, 0.5, 0.5, 1.0], &[1.0, 1.0, 0.0, 0.0], &[0.5; 4])
    }
}

fn is_default_shape(shape: &Arc<LfoShape>) -> bool {
    shape.is_triangle()
}

mod lfo_shape_array {
    use super::LFO_SHAPE_POINTS;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        values: &[f32; LFO_SHAPE_POINTS],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        values.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[f32; LFO_SHAPE_POINTS], D::Error> {
        let values = Vec::<f32>::deserialize(deserializer)?;
        if values.len() != LFO_SHAPE_POINTS {
            return Err(D::Error::custom(format!(
                "lfo shape needs exactly {LFO_SHAPE_POINTS} values"
            )));
        }
        Ok(std::array::from_fn(|index| values[index].clamp(0.0, 1.0)))
    }
}

impl Serialize for LfoShape {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("LfoShape", 4)?;
        state.serialize_field("x", &ShapePoints(&self.x))?;
        state.serialize_field("y", &ShapePoints(&self.y))?;
        state.serialize_field("curve", &ShapePoints(&self.curve))?;
        state.serialize_field("points", &self.points)?;
        state.end()
    }
}

struct ShapePoints<'a>(&'a [f32; LFO_SHAPE_POINTS]);

impl Serialize for ShapePoints<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        lfo_shape_array::serialize(self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for LfoShape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(with = "lfo_shape_array")]
            x: [f32; LFO_SHAPE_POINTS],
            #[serde(with = "lfo_shape_array")]
            y: [f32; LFO_SHAPE_POINTS],
            #[serde(with = "lfo_shape_array")]
            curve: [f32; LFO_SHAPE_POINTS],
            points: usize,
        }
        let raw = Raw::deserialize(deserializer)?;
        let points = raw.points.clamp(2, LFO_SHAPE_POINTS);
        Ok(Self {
            x: raw.x,
            y: raw.y,
            curve: raw.curve,
            points,
        })
    }
}

/// Per-oscillator wavetable warp. Discriminants are the imported warp enum's
/// numeric ids (0..=69), so the numeric parameter ABI is the format's own id
/// space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum WarpMode {
    #[default]
    Off = 0,
    Sync = 1,
    #[serde(alias = "Bend")]
    BendPos = 2,
    BendNeg = 3,
    BendPosNeg = 4,
    Pwm = 5,
    #[serde(alias = "Asym")]
    AsymPos = 6,
    AsymNeg = 7,
    AsymPosNeg = 8,
    Flip = 9,
    Dlm = 10,
    Remap1 = 11,
    Remap2 = 12,
    Remap3 = 13,
    Remap4 = 14,
    Quantize = 15,
    EvenOdd = 16,
    /// Selectable but not implemented: the coefficient law is unresolved.
    FilterLpf = 17,
    /// Selectable but not implemented: see [`WarpMode::FilterLpf`].
    FilterHpf = 18,
    DistTube = 19,
    DistSoftClip = 20,
    DistHardClip = 21,
    DistDiode1 = 22,
    DistDiode2 = 23,
    DistLinFold = 24,
    DistSinFold = 25,
    DistZeroSquare = 26,
    DistAsym = 27,
    DistRectify = 28,
    DistSineShaper = 29,
    DistStompBox = 30,
    DistTapeSat = 31,
    DistSoftSat = 32,
    // Class 0: selectable, no readout effect.
    #[serde(alias = "Fm")]
    FmOsc = 33,
    FmOsc2 = 34,
    FmNoise = 35,
    FmSub = 36,
    FmFilt1 = 37,
    FmFilt2 = 38,
    FmxOsc = 39,
    FmxOsc2 = 40,
    FmxNoise = 41,
    FmxSub = 42,
    FmxFilt1 = 43,
    FmxFilt2 = 44,
    FmpOsc = 45,
    FmpOsc2 = 46,
    FmpNoise = 47,
    FmpSub = 48,
    FmpFilt1 = 49,
    FmpFilt2 = 50,
    #[serde(alias = "Pd")]
    PdOsc = 51,
    PdOsc2 = 52,
    PdNoise = 53,
    PdSub = 54,
    PdFilt1 = 55,
    PdFilt2 = 56,
    SelfPd = 57,
    #[serde(alias = "Am")]
    AmOsc = 58,
    AmOsc2 = 59,
    AmNoise = 60,
    AmSub = 61,
    AmFilt1 = 62,
    AmFilt2 = 63,
    #[serde(alias = "Ring")]
    RmOsc = 64,
    RmOsc2 = 65,
    RmNoise = 66,
    RmSub = 67,
    RmFilt1 = 68,
    RmFilt2 = 69,
}

impl WarpMode {
    const ALL: [WarpMode; 70] = [
        WarpMode::Off,
        WarpMode::Sync,
        WarpMode::BendPos,
        WarpMode::BendNeg,
        WarpMode::BendPosNeg,
        WarpMode::Pwm,
        WarpMode::AsymPos,
        WarpMode::AsymNeg,
        WarpMode::AsymPosNeg,
        WarpMode::Flip,
        WarpMode::Dlm,
        WarpMode::Remap1,
        WarpMode::Remap2,
        WarpMode::Remap3,
        WarpMode::Remap4,
        WarpMode::Quantize,
        WarpMode::EvenOdd,
        WarpMode::FilterLpf,
        WarpMode::FilterHpf,
        WarpMode::DistTube,
        WarpMode::DistSoftClip,
        WarpMode::DistHardClip,
        WarpMode::DistDiode1,
        WarpMode::DistDiode2,
        WarpMode::DistLinFold,
        WarpMode::DistSinFold,
        WarpMode::DistZeroSquare,
        WarpMode::DistAsym,
        WarpMode::DistRectify,
        WarpMode::DistSineShaper,
        WarpMode::DistStompBox,
        WarpMode::DistTapeSat,
        WarpMode::DistSoftSat,
        WarpMode::FmOsc,
        WarpMode::FmOsc2,
        WarpMode::FmNoise,
        WarpMode::FmSub,
        WarpMode::FmFilt1,
        WarpMode::FmFilt2,
        WarpMode::FmxOsc,
        WarpMode::FmxOsc2,
        WarpMode::FmxNoise,
        WarpMode::FmxSub,
        WarpMode::FmxFilt1,
        WarpMode::FmxFilt2,
        WarpMode::FmpOsc,
        WarpMode::FmpOsc2,
        WarpMode::FmpNoise,
        WarpMode::FmpSub,
        WarpMode::FmpFilt1,
        WarpMode::FmpFilt2,
        WarpMode::PdOsc,
        WarpMode::PdOsc2,
        WarpMode::PdNoise,
        WarpMode::PdSub,
        WarpMode::PdFilt1,
        WarpMode::PdFilt2,
        WarpMode::SelfPd,
        WarpMode::AmOsc,
        WarpMode::AmOsc2,
        WarpMode::AmNoise,
        WarpMode::AmSub,
        WarpMode::AmFilt1,
        WarpMode::AmFilt2,
        WarpMode::RmOsc,
        WarpMode::RmOsc2,
        WarpMode::RmNoise,
        WarpMode::RmSub,
        WarpMode::RmFilt1,
        WarpMode::RmFilt2,
    ];

    fn from_value(value: f64) -> Self {
        let index = value.round();
        if (0.0..70.0).contains(&index) {
            Self::ALL[index as usize]
        } else {
            Self::Off
        }
    }

    fn index(self) -> u8 {
        self as u8
    }

    /// Phase-map class: the map overwrites the phase.
    fn is_phase_warp(self) -> bool {
        matches!(self.index(), 2..=15)
    }

    fn is_dist(self) -> bool {
        matches!(self.index(), 19..=32)
    }

    /// The shaper id per distortion mode.
    fn dist_id(self) -> usize {
        const IDS: [usize; 14] = [0, 1, 2, 3, 4, 5, 6, 7, 9, 10, 13, 14, 15, 17];
        IDS[self.index().saturating_sub(19) as usize]
    }

    /// Modes that leave the readout untouched (class 0 and the
    /// unimplemented warp-stage filters). Test oracle for the stage flags.
    #[cfg(test)]
    fn is_bypass(self) -> bool {
        matches!(self.index(), 0 | 17 | 18 | 33..=56 | 58..=65)
    }

    /// `kRM_NOISE/SUB/FILT1/FILT2` are a flat `1 - amount` gain.
    fn is_rm_gain(self) -> bool {
        matches!(self.index(), 66..=69)
    }
}

impl FilterModel {
    /// Numeric ABI: 0 = SVF (legacy), 1 = 4-pole ladder, 2 = dirty ladder,
    /// 3/4 = legacy ladders, 5 = 1-pole SVF tap, 6/7 = 3/4-pole SVF
    /// cascades, 8-11 = ladder taps at 6/12/18/24 dB/oct.
    fn from_value(value: f64) -> Self {
        match value.round() as i32 {
            1 => Self::Ladder,
            2 => Self::DirtyLadder,
            3 => Self::Ladder3,
            4 => Self::Ladder1,
            5 => Self::Svf1,
            6 => Self::Svf3,
            7 => Self::Svf4,
            8 => Self::Moog6,
            9 => Self::Moog12,
            10 => Self::Moog18,
            11 => Self::Moog24,
            _ => Self::Svf,
        }
    }

    /// Ladder stage count; the SVF is always two poles.
    fn poles(self) -> usize {
        match self {
            Self::Ladder1 | Self::Svf1 | Self::Moog6 => 1,
            Self::Ladder3 | Self::Svf3 | Self::Moog18 => 3,
            Self::Ladder | Self::DirtyLadder | Self::Svf4 | Self::Moog24 => 4,
            Self::Svf | Self::Moog12 => 2,
        }
    }

    /// Feedback at resonance 1.0, scaled from the four-pole oscillation
    /// threshold `2^(poles/2)` with the stability margin. Lower pole counts
    /// sit further below their own threshold, so they resonate less.
    fn ladder_max_feedback(self) -> f64 {
        2f64.powf(self.poles() as f64 / 2.0) * LADDER_FEEDBACK_MARGIN
    }

    /// Models with a resonant two-pole core, which is what the passband trim
    /// normalises. The first-order tap has no resonance and is left alone.
    fn has_resonance(self) -> bool {
        matches!(self, Self::Svf | Self::Svf3 | Self::Svf4)
    }

    /// Ladder taps with their own feedback law.
    fn is_moog(self) -> bool {
        matches!(
            self,
            Self::Moog6 | Self::Moog12 | Self::Moog18 | Self::Moog24
        )
    }
}

/// Loop feedback at the knob position. Four times resonance is the oscillation
/// threshold; the margin keeps even the full-resonance loop strictly decaying.
#[inline]
fn moog_feedback(resonance: f64) -> f64 {
    4.0 * resonance.clamp(0.0, 1.0) * MOOG_FEEDBACK_MARGIN
}

/// Passband trim for the state-variable family: removes the shelf that grows
/// below the corner with resonance, normalised to the resonance-free setting.
#[inline]
fn svf_input_gain(resonance: f64) -> f64 {
    1.0 / (1.0 + 0.20 * resonance.clamp(0.0, 1.0).powf(0.25))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModSource {
    Lfo,
    FilterEnvelope,
    Velocity,
    Macro1,
    Macro2,
    Macro3,
    Macro4,
    Keytrack,
    Aftertouch,
    ModWheel,
    Step,
    // Appended after the original 0..=10 mapping; do not renumber.
    Lfo2,
    Lfo3,
    Lfo4,
    Lfo5,
    Lfo6,
    Lfo7,
    Lfo8,
    Lfo9,
    Lfo10,
    Env3,
    Env4,
    Macro5,
    Macro6,
    Macro7,
    Macro8,
    /// Source ID 2, Env 1: the amplitude envelope. Appended after the
    /// original table so existing `mod.*.source` numbers keep their meaning.
    AmpEnvelope,
    /// Normalized per-voice channel pitch bend, bipolar.
    PitchBend,
    /// Per-voice CC74 timbre/slide, centred on 0.5.
    Timbre,
}

/// Physical order of [`ModSource`]; also the `mod.*.source` ABI.
const MOD_SOURCE_TABLE: [ModSource; 29] = [
    ModSource::Lfo,
    ModSource::FilterEnvelope,
    ModSource::Velocity,
    ModSource::Macro1,
    ModSource::Macro2,
    ModSource::Macro3,
    ModSource::Macro4,
    ModSource::Keytrack,
    ModSource::Aftertouch,
    ModSource::ModWheel,
    ModSource::Step,
    ModSource::Lfo2,
    ModSource::Lfo3,
    ModSource::Lfo4,
    ModSource::Lfo5,
    ModSource::Lfo6,
    ModSource::Lfo7,
    ModSource::Lfo8,
    ModSource::Lfo9,
    ModSource::Lfo10,
    ModSource::Env3,
    ModSource::Env4,
    ModSource::Macro5,
    ModSource::Macro6,
    ModSource::Macro7,
    ModSource::Macro8,
    ModSource::AmpEnvelope,
    ModSource::PitchBend,
    ModSource::Timbre,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModDestination {
    FilterCutoff,
    Pitch,
    OscillatorMix,
    Resonance,
    StereoWidth,
    PulseWidthA,
    RingModLevel,
    WavetablePositionA,
    OscillatorBTuning,
    FilterDrive,
    // Appended after the original 0..=9 mapping; do not renumber.
    OscillatorCLevel,
    WavetablePositionC,
    Filter2Cutoff,
    Filter2Resonance,
    Filter2Drive,
    OscillatorALevel,
    NoiseLevel,
    SubLevel,
    FilterWet,
    Filter2Wet,
    /// Scales the voice output.
    VoiceAmp,
    // Appended after the original 0..=15 mapping; do not renumber.
    /// Primary warp amount for oscillator A/B/C.
    WarpAmountA,
    WarpAmountB,
    WarpAmountC,
    /// Secondary warp amount for oscillator A/B/C.
    Warp2AmountA,
    Warp2AmountB,
    Warp2AmountC,
    /// Wavetable position for oscillator B.
    WavetablePositionB,
    // Source-oscillator controls, appended after the wavetable destinations.
    SourceDensityA,
    SourceDensityB,
    SourceDensityC,
    SourcePositionA,
    SourcePositionB,
    SourcePositionC,
    SourceMorphA,
    SourceMorphB,
    SourceMorphC,
    SourceBendA,
    SourceBendB,
    SourceBendC,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModRoute {
    pub source: ModSource,
    pub destination: ModDestination,
    pub amount: f64,
    /// Re-centres a unipolar source on zero
    /// (`2v - 1`) so the route swings around the destination's current value.
    /// Sources that are already bipolar are left unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bipolar: bool,
    /// The route's primary value is
    /// multiplied by this source, so a static macro acts as a depth gate and a
    /// dynamic source as per-voice depth. `None` means unity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auxiliary: Option<ModSource>,
    /// Negates the auxiliary source.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auxiliary_inverted: bool,
    /// Shape exponent -1..1 applied to the route signal (0 is identity).
    #[serde(
        default = "default_mod_route_curve",
        skip_serializing_if = "is_zero_f64"
    )]
    pub curve: f64,
    /// One-pole rise time in seconds (0 bypasses the slew).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub slew_up: f64,
    /// One-pole fall time in seconds (0 bypasses the slew).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub slew_down: f64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

impl Default for ModRoute {
    fn default() -> Self {
        Self {
            source: mod_source_from_value(schema_discrete_default("mod.1.source")),
            destination: mod_destination_from_value(schema_discrete_default("mod.1.destination")),
            amount: schema_dsp_default("mod.1.amount"),
            bipolar: schema_discrete_default("mod.1.bipolar") >= 0.5,
            auxiliary: None,
            auxiliary_inverted: schema_discrete_default("mod.1.aux_invert") >= 0.5,
            curve: schema_dsp_default("mod.1.curve"),
            slew_up: schema_dsp_default("mod.1.slew_up"),
            slew_down: schema_dsp_default("mod.1.slew_down"),
        }
    }
}

/// Native FX parameter that a [`FxModRoute`] can modulate.
///
/// This is deliberately a small fixed palette rather than a mirror of every
/// `SynthFx` parameter: the rack's destination semantics are global (one chain
/// after the voice sum), so voice-domain `ModDestination` variants cannot
/// describe them. The numeric order is part of the `fxmod.N.parameter` patch
/// ABI; append new variants at the end and do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FxParam {
    /// Wet/dry mix (`mix`), 0..1.
    #[default]
    Wet,
    /// Feedback, 0..1 (the DSP clamps per effect).
    Feedback,
    /// Modulation depth, 0..1.
    Depth,
    /// EQ low-band gain in dB (`low`).
    Gain1,
    /// EQ high-band gain in dB (`high`).
    Gain2,
    /// Waveshaper drive, 0..1.
    Drive,
    /// EQ low-band frequency in Hz (`low_freq`), modulated in octaves.
    Freq1,
    /// EQ high-band frequency in Hz (`high_freq`), modulated in octaves.
    Freq2,
    /// Post-mix module output trim (`level_out`), 0..1 in the imported encoding.
    LevelOut,
    /// Distortion tone-filter bandwidth in octaves (`bw`); 0 is the one-pole tone.
    Bandwidth,
    // The remaining knob families, appended after the original 0..=9
    // mapping so stored `fxmod` indices do not move.
    /// Module cutoff (`cutoff`), swept in octaves.
    Cutoff,
    /// Shaper/filter tone position (`tone`), 0..1.
    Tone,
    /// LFO rate (`rate`) in Hz.
    Rate,
    /// Bode shift, a percentage of its range (`shift`).
    Shift,
    /// Bode shift range in Hz (`range`).
    Range,
    /// Utils stereo width (`width`), 0..800.
    Width,
    /// Utils high-pass corner (`hpf`), swept in octaves.
    Hpf,
    /// Utils low-pass corner (`lpf`), swept in octaves.
    Lpf,
    /// Utils balance (`balance`), -100..100.
    Balance,
    /// EQ band 1 resonance/Q (`reso1`), 0..100.
    Reso1,
    /// EQ band 2 resonance/Q (`reso2`), 0..100.
    Reso2,
    /// Delay time in seconds (`time`).
    Time,
    /// Compressor/multiband global make-up in dB (`makeup`).
    Makeup,
    /// Voice/FX filter resonance (`resonance`), 0..1.
    Resonance,
    /// Bode output mix between the down and up sidebands (`output_mix`).
    OutputMix,
    /// Bode stereo spread of the two sidebands (`output_width`).
    OutputWidth,
    /// Bode feedback delay time in seconds (`delay`).
    DelayTime,
}

impl FxParam {
    /// Physical order; also the numeric `fxmod.N.parameter` ABI.
    pub const ALL: [FxParam; 27] = [
        FxParam::Wet,
        FxParam::Feedback,
        FxParam::Depth,
        FxParam::Gain1,
        FxParam::Gain2,
        FxParam::Drive,
        FxParam::Freq1,
        FxParam::Freq2,
        FxParam::LevelOut,
        FxParam::Bandwidth,
        FxParam::Cutoff,
        FxParam::Tone,
        FxParam::Rate,
        FxParam::Shift,
        FxParam::Range,
        FxParam::Width,
        FxParam::Hpf,
        FxParam::Lpf,
        FxParam::Balance,
        FxParam::Reso1,
        FxParam::Reso2,
        FxParam::Time,
        FxParam::Makeup,
        FxParam::Resonance,
        FxParam::OutputMix,
        FxParam::OutputWidth,
        FxParam::DelayTime,
    ];

    /// Native `SynthFx` parameter name. This is a lookup key, not the patch
    /// ABI (which is the numeric variant).
    pub fn native_name(self) -> &'static str {
        match self {
            FxParam::Wet => "mix",
            FxParam::Feedback => "feedback",
            FxParam::Depth => "depth",
            FxParam::Gain1 => "low",
            FxParam::Gain2 => "high",
            FxParam::Drive => "drive",
            FxParam::Freq1 => "low_freq",
            FxParam::Freq2 => "high_freq",
            FxParam::LevelOut => "level_out",
            FxParam::Bandwidth => "bw",
            FxParam::Cutoff => "cutoff",
            FxParam::Tone => "tone",
            FxParam::Rate => "rate",
            FxParam::Shift => "shift",
            FxParam::Range => "range",
            FxParam::Width => "width",
            FxParam::Hpf => "hpf",
            FxParam::Lpf => "lpf",
            FxParam::Balance => "balance",
            FxParam::Reso1 => "reso1",
            FxParam::Reso2 => "reso2",
            FxParam::Time => "time",
            FxParam::Makeup => "makeup",
            FxParam::Resonance => "resonance",
            FxParam::OutputMix => "output_mix",
            FxParam::OutputWidth => "output_width",
            FxParam::DelayTime => "delay",
        }
    }

    /// Frequency controls are swept exponentially; the others linearly.
    fn is_logarithmic(self) -> bool {
        matches!(
            self,
            FxParam::Freq1 | FxParam::Freq2 | FxParam::Cutoff | FxParam::Hpf | FxParam::Lpf
        )
    }

    /// Full-scale change for a unit amount.
    fn modulation_scale(self) -> f64 {
        match self {
            FxParam::Gain1 | FxParam::Gain2 => 24.0,
            FxParam::Bandwidth => 8.0,
            FxParam::Shift | FxParam::Balance | FxParam::Reso1 | FxParam::Reso2 => 100.0,
            FxParam::Range => 5_000.0,
            FxParam::Width => 800.0,
            FxParam::Time => 1.2,
            FxParam::Makeup => 24.0,
            FxParam::Resonance => 1.0,
            FxParam::OutputMix => 100.0,
            FxParam::OutputWidth => 100.0,
            FxParam::DelayTime => 1.2,
            _ => 1.0,
        }
    }

    fn range(self) -> (f64, f64) {
        match self {
            FxParam::Wet | FxParam::Depth | FxParam::Drive | FxParam::LevelOut => (0.0, 1.0),
            FxParam::Feedback => (0.0, 0.95),
            FxParam::Gain1 | FxParam::Gain2 => (-24.0, 24.0),
            FxParam::Freq1 | FxParam::Freq2 => (20.0, 20_000.0),
            FxParam::Bandwidth => (0.0, 8.0),
            FxParam::Cutoff | FxParam::Hpf | FxParam::Lpf => (20.0, 20_000.0),
            FxParam::Tone => (0.0, 1.0),
            FxParam::Rate => (0.01, 20.0),
            FxParam::Shift | FxParam::Balance => (-100.0, 100.0),
            FxParam::Range => (0.1, 5_000.0),
            FxParam::Width => (0.0, 800.0),
            FxParam::Reso1 | FxParam::Reso2 => (0.0, 100.0),
            FxParam::Time => (0.005, 1.2),
            FxParam::Makeup => (-24.0, 24.0),
            FxParam::Resonance => (0.0, 0.99),
            FxParam::OutputMix => (-100.0, 100.0),
            FxParam::OutputWidth => (0.0, 200.0),
            FxParam::DelayTime => (0.0, 1.2),
        }
    }

    /// Apply a unit modulation amount to a base value. Linear parameters add
    /// `amount * scale`; frequency parameters sweep `amount * 4` octaves.
    fn apply(self, base: f64, modulation: f64) -> f64 {
        let value = if self.is_logarithmic() {
            base * 2f64.powf(modulation * 4.0)
        } else {
            base + modulation * self.modulation_scale()
        };
        let (lo, hi) = self.range();
        if value.is_finite() {
            value.clamp(lo, hi)
        } else {
            base.clamp(lo, hi)
        }
    }
}

fn fx_param_from_value(value: f64) -> FxParam {
    let index = value.round().max(0.0) as usize;
    FxParam::ALL.get(index).copied().unwrap_or_default()
}

/// One FX-parameter modulation route. `fx_index` is zero-based into
/// `SynthPatch::fx`. The rack is post-voice-sum, so voice-domain sources
/// (velocity, envelopes, keytrack) use a representative voice; see
/// `OpenSynth::fx_modulation_sources`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxModRoute {
    pub source: ModSource,
    pub fx_index: usize,
    pub parameter: FxParam,
    /// -1.0..=1.0 fraction of the destination's modulation range.
    pub amount: f64,
    /// Re-centres a unipolar source on zero (2s-1).
    /// Sources that are already bipolar are left unchanged.
    #[serde(default)]
    pub bipolar: bool,
}

impl Default for FxModRoute {
    fn default() -> Self {
        Self {
            source: mod_source_from_value(schema_discrete_default("fxmod.1.source")),
            fx_index: schema_discrete_default("fxmod.1.fx_index") as usize,
            parameter: fx_param_from_value(schema_discrete_default("fxmod.1.parameter")),
            amount: schema_dsp_default("fxmod.1.amount"),
            bipolar: schema_discrete_default("fxmod.1.bipolar") >= 0.5,
        }
    }
}

/// Per-LFO settings for LFO 2..LFO 10. LFO 1 keeps the original flat
/// `lfo_rate`/`lfo_shape`/... fields so old patches and automation keep working.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LfoSettings {
    pub rate: f64,
    /// Retired selector; parsed from old documents and folded into the shape.
    #[serde(rename = "waveform", default, skip_serializing_if = "Option::is_none")]
    pub legacy_waveform: Option<LegacyLfoWaveform>,
    #[serde(default)]
    pub shape_type: LfoShapeType,
    #[serde(default, skip_serializing_if = "is_default_shape")]
    pub shape: Arc<LfoShape>,
    pub sync: bool,
    pub retrigger: bool,
    /// Retired six-step sync selector, kept for old documents.
    pub division: usize,
    /// Index into [`LFO_SYNC_DIVISIONS`]; `None` uses the legacy division.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_div: Option<usize>,
    /// One-pole lag on the shaped output, 0 (bypass) .. 1 (0.5 s time constant).
    #[serde(default = "default_extra_lfo_smooth")]
    pub smooth: f64,
    /// Fade-in time after `delay`, in seconds.
    #[serde(default)]
    pub rise: f64,
    /// Silent lead-in before `rise` starts, in seconds.
    #[serde(default)]
    pub delay: f64,
    /// Phase skew -1..1; the cycle rate is unchanged.
    #[serde(default = "default_extra_lfo_bend")]
    pub bend: f64,
    /// Start phase in degrees; only a retriggered LFO applies it.
    #[serde(default = "default_extra_lfo_phase")]
    pub phase: f64,
    /// Free-running rate multiplied by ten.
    #[serde(default)]
    pub rate_x10: bool,
}

impl Default for LfoSettings {
    fn default() -> Self {
        Self {
            rate: LFO_RATE_HZ.default,
            legacy_waveform: None,
            shape_type: LfoShapeType::from_value(schema_discrete_default("lfo2.shape")),
            shape: Arc::new(LfoShape::triangle()),
            sync: schema_discrete_default("lfo2.sync") >= 0.5,
            retrigger: schema_discrete_default("lfo2.retrigger") >= 0.5,
            division: schema_discrete_default("lfo2.division") as usize,
            sync_div: None,
            smooth: schema_dsp_default("lfo2.smooth"),
            rise: schema_dsp_default("lfo2.rise"),
            delay: schema_dsp_default("lfo2.delay"),
            bend: schema_dsp_default("lfo2.bend"),
            phase: schema_dsp_default("lfo2.phase"),
            rate_x10: schema_discrete_default("lfo2.rate_x10") >= 0.5,
        }
    }
}

/// Legacy documents always stored 16 matrix slots. Keep that default so old
/// JSON loads with the same shape; writing `mod.17..64` extends the vector.
fn default_mod_routes() -> Vec<ModRoute> {
    vec![ModRoute::default(); 16]
}

fn default_velocity_to_amp() -> f64 {
    schema_dsp_default("velocity.to_amp")
}

fn default_amp_damping() -> f64 {
    schema_dsp_default("amp.damping")
}

fn default_oscillator_c_level() -> f64 {
    schema_dsp_default("osc.c.level")
}

fn default_loudness_gain() -> f64 {
    1.0
}

fn default_oscillator_c_waveform() -> Waveform {
    Waveform::Sine
}

fn default_unison_one() -> usize {
    1
}

fn default_lfo_curve() -> [f32; LFO_CURVE_POINTS] {
    std::array::from_fn(|index| {
        let phase = index as f64 / LFO_CURVE_POINTS as f64;
        (1.0 - 4.0 * (phase - 0.5).abs()) as f32
    })
}

/// Serialize the fixed curve as a plain array; a patch must carry every point.
mod lfo_curve_points {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{LFO_CURVE_POINTS, default_lfo_curve};

    pub fn serialize<S: Serializer>(
        points: &[f32; LFO_CURVE_POINTS],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        points.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[f32; LFO_CURVE_POINTS], D::Error> {
        let values = Vec::<f32>::deserialize(deserializer)?;
        if values.len() != LFO_CURVE_POINTS {
            return Err(D::Error::custom(format!(
                "lfo_curve needs exactly {LFO_CURVE_POINTS} points"
            )));
        }
        let mut points = default_lfo_curve();
        for (target, value) in points.iter_mut().zip(values) {
            *target = if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        Ok(points)
    }
}

fn default_sub_semitones() -> f64 {
    -12.0
}

fn default_sub_waveform() -> SubShape {
    SubShape::Sine
}

fn default_keytrack_origin() -> f64 {
    60.0
}

fn default_pulse_width() -> f64 {
    schema_dsp_default("osc.c.pulse_width")
}

fn default_filter2_mode() -> FilterMode {
    filter_mode_from_value(schema_discrete_default("filter2.mode"))
}

fn default_filter_routing() -> FilterRouting {
    if schema_discrete_default("filter.routing") >= 0.5 {
        FilterRouting::Parallel
    } else {
        FilterRouting::Serial
    }
}

fn default_filter2_cutoff() -> f64 {
    FILTER_CUTOFF_HZ.default
}

fn default_filter2_resonance() -> f64 {
    0.15
}

fn default_env_attack() -> f64 {
    schema_dsp_default("env3.attack")
}

fn default_env_decay() -> f64 {
    schema_dsp_default("env3.decay")
}

fn default_filter_wet() -> f64 {
    1.0
}

fn default_env_sustain() -> f64 {
    schema_dsp_default("env3.sustain")
}

fn default_env_release() -> f64 {
    schema_dsp_default("env3.release")
}

/// Inclusive MPE member-channel range, in 1-based MIDI channel numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MpeMemberRange {
    pub first: u8,
    pub last: u8,
}

impl MpeMemberRange {
    pub const fn new(first: u8, last: u8) -> Self {
        Self { first, last }
    }

    /// Ordered 0-based channel bounds used by the voice layer.
    fn bounds(self) -> (u8, u8) {
        let first = self.first.clamp(1, 16) - 1;
        let last = self.last.clamp(1, 16) - 1;
        (first.min(last), first.max(last))
    }
}

/// MPE zone configuration. A missing zone is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MpeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower: Option<MpeMemberRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper: Option<MpeMemberRange>,
    /// Pitch-bend range in semitones; MPE defaults to +/-48.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_range: Option<f64>,
}

impl MpeConfig {
    pub fn enabled(&self) -> bool {
        self.lower.is_some() || self.upper.is_some()
    }

    pub fn bend_range_semitones(&self) -> f64 {
        self.bend_range.unwrap_or(48.0).clamp(0.0, 96.0)
    }

    pub fn lower_or_default() -> Self {
        Self {
            lower: Some(MpeMemberRange::new(2, 16)),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthPatch {
    pub oscillator_a_waveform: Waveform,
    pub oscillator_a_level: f64,
    pub oscillator_a_pulse_width: f64,
    pub oscillator_a_wavetable_position: f64,
    // Warp defaults keep patches saved before wavetable warp loadable.
    #[serde(default)]
    pub oscillator_a_warp_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_a_warp_amount: f64,
    /// Controls the sync edge window or the warp filter shape, depending on the mode.
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_a_warp_var: f64,
    #[serde(default)]
    pub oscillator_a_warp2_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_a_warp2_amount: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_a_warp2_var: f64,
    /// -1.0 (left) ..= 1.0 (right). Zero keeps the unpanned legacy signal.
    #[serde(default)]
    pub oscillator_a_pan: f64,
    /// Oscillator A transpose in semitones (octave + pitch + fine).
    #[serde(default)]
    pub oscillator_a_semitones: f64,
    /// Per-oscillator unison stacks. A keeps the legacy global fields above.
    #[serde(default = "default_unison_one")]
    pub oscillator_b_unison: usize,
    #[serde(default)]
    pub oscillator_b_detune_cents: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_b_stereo_width: f64,
    pub oscillator_b_waveform: Waveform,
    pub oscillator_b_level: f64,
    pub oscillator_b_semitones: f64,
    pub oscillator_b_pulse_width: f64,
    pub oscillator_b_wavetable_position: f64,
    #[serde(default)]
    pub oscillator_b_warp_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_b_warp_amount: f64,
    /// Controls the sync edge window or the warp filter shape, depending on the mode.
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_b_warp_var: f64,
    #[serde(default)]
    pub oscillator_b_warp2_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_b_warp2_amount: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_b_warp2_var: f64,
    #[serde(default)]
    pub oscillator_b_pan: f64,
    // Oscillator C defaults keep patches saved before it existed loadable.
    #[serde(default = "default_oscillator_c_waveform")]
    pub oscillator_c_waveform: Waveform,
    #[serde(default = "default_oscillator_c_level")]
    pub oscillator_c_level: f64,
    #[serde(default)]
    pub oscillator_c_semitones: f64,
    #[serde(default = "default_pulse_width")]
    pub oscillator_c_pulse_width: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_wavetable_position: f64,
    #[serde(default)]
    pub oscillator_c_warp_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_warp_amount: f64,
    /// Controls the sync edge window or the warp filter shape, depending on the mode.
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_warp_var: f64,
    #[serde(default)]
    pub oscillator_c_warp2_mode: WarpMode,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_warp2_amount: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_warp2_var: f64,
    #[serde(default)]
    pub oscillator_c_pan: f64,
    #[serde(default = "default_unison_one")]
    pub oscillator_c_unison: usize,
    #[serde(default)]
    pub oscillator_c_detune_cents: f64,
    #[serde(default = "default_oscillator_fraction")]
    pub oscillator_c_stereo_width: f64,
    pub ring_mod_level: f64,
    pub sub_level: f64,
    /// Sub oscillator transpose in semitones, with a default of -12.
    #[serde(default = "default_sub_semitones")]
    pub sub_semitones: f64,
    /// Sub oscillator fine tuning in cents, clamped to +/-100.
    #[serde(default)]
    pub sub_fine_cents: f64,
    /// Sub oscillator pan in -1..1 (0 is centre).
    #[serde(default)]
    pub sub_pan: f64,
    /// Sub oscillator shape, with a sine as the default.
    #[serde(default = "default_sub_waveform")]
    pub sub_waveform: SubShape,
    /// Sub oscillator start phase as a fraction of a cycle.
    #[serde(default)]
    pub sub_initial_phase: f64,
    /// Free-run the sub oscillator from the patch phase accumulator.
    #[serde(default)]
    pub sub_contiguous_phase: bool,
    pub noise_level: f64,
    pub gain: f64,
    /// Linear gain the engine's per-patch loudness law applies after `gain`.
    ///
    /// Runtime only: a document carries the level the operator wrote, and a
    /// serialized patch never carries this field. One is unity.
    #[serde(default = "default_loudness_gain", skip_serializing)]
    pub loudness_gain: f64,
    /// Linear velocity-to-amplitude depth: 0 keeps the level constant, 1
    /// follows `velocity / 127`. Imported presets store 0; velocity is a matrix
    /// source there, not an implicit gain stage.
    #[serde(default = "default_velocity_to_amp")]
    pub velocity_to_amp: f64,
    #[serde(default)]
    pub arpeggiator: Option<arpeggiator::ArpSettings>,
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
    pub amp_curve: f64,
    /// How far note damping follows the played key: 0 gives every note the same
    /// decay and release; 1 halves the decay per octave above C4 and damps notes
    /// held under 0.4 s faster. Applied per note at note-on and note-off, so a
    /// live change reaches the next note, not the sounding one.
    #[serde(default = "default_amp_damping")]
    pub amp_damping: f64,
    pub detune_cents: f64,
    pub unison_voices: usize,
    pub stereo_width: f64,
    pub filter_cutoff: f64,
    pub filter_resonance: f64,
    pub filter_drive: f64,
    pub filter_mode: FilterMode,
    #[serde(default)]
    pub filter_model: FilterModel,
    /// Wet/dry mix of the filter section. The preset stores 0..100; 1.0 is fully wet.
    #[serde(default = "default_filter_wet")]
    pub filter_wet: f64,
    pub filter_keytrack: f64,
    /// MIDI note whose pitch is the un-keytracked cutoff. The preset's keytrack
    /// origin is C1 (32); 60 preserves the legacy engine behavior.
    #[serde(default = "default_keytrack_origin")]
    pub filter_keytrack_origin: f64,
    pub filter_envelope_amount: f64,
    pub filter_attack: f64,
    pub filter_decay: f64,
    pub filter_sustain: f64,
    pub filter_release: f64,
    pub filter_curve: f64,
    // Envelopes 3 and 4 are free-running ADSR sources for the modulation
    // matrix. Their defaults follow Env 1 so routing one is immediately useful.
    #[serde(default = "default_env_attack")]
    pub env3_attack: f64,
    #[serde(default = "default_env_decay")]
    pub env3_decay: f64,
    #[serde(default = "default_env_sustain")]
    pub env3_sustain: f64,
    #[serde(default = "default_env_release")]
    pub env3_release: f64,
    #[serde(default = "default_env_curve")]
    pub env3_curve: f64,
    #[serde(default = "default_env_attack")]
    pub env4_attack: f64,
    #[serde(default = "default_env_decay")]
    pub env4_decay: f64,
    #[serde(default = "default_env_sustain")]
    pub env4_sustain: f64,
    #[serde(default = "default_env_release")]
    pub env4_release: f64,
    #[serde(default = "default_env_curve")]
    pub env4_curve: f64,
    // Second filter; defaults are neutral and inert so old patches are unchanged.
    #[serde(default)]
    pub filter2_enabled: bool,
    #[serde(default = "default_filter2_cutoff")]
    pub filter2_cutoff: f64,
    #[serde(default = "default_filter2_resonance")]
    pub filter2_resonance: f64,
    #[serde(default)]
    pub filter2_drive: f64,
    #[serde(default = "default_filter2_mode")]
    pub filter2_mode: FilterMode,
    #[serde(default)]
    pub filter2_model: FilterModel,
    #[serde(default = "default_filter_wet")]
    pub filter2_wet: f64,
    #[serde(default = "default_filter_routing")]
    pub filter_routing: FilterRouting,
    /// Per-source filter targets (A, B, C, noise, sub). `None` keeps the
    /// legacy `filter_routing` path; imported presets fill it only when they
    /// carry source routing slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_filter_routes: Option<[SourceFilterRoute; 5]>,
    pub lfo_rate: f64,
    pub lfo_pitch_cents: f64,
    /// Retired selector; parsed from old documents and folded into the shape.
    #[serde(
        rename = "lfo_waveform",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_lfo_waveform: Option<LegacyLfoWaveform>,
    /// Retired shared curve, kept so `lfo.curve.N` and old JSON still load.
    #[serde(default = "default_lfo_curve", with = "lfo_curve_points")]
    pub lfo_curve: [f32; LFO_CURVE_POINTS],
    #[serde(default)]
    pub lfo_shape_type: LfoShapeType,
    #[serde(default, skip_serializing_if = "is_default_shape")]
    pub lfo_shape: Arc<LfoShape>,
    pub lfo_retrigger: bool,
    pub lfo_sync: bool,
    pub lfo_division: usize,
    /// Index into [`LFO_SYNC_DIVISIONS`]; `None` uses the legacy division.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lfo_sync_div: Option<usize>,
    /// LFO 1 shaping, mirroring the per-LFO [`LfoSettings`] fields.
    #[serde(default = "default_lfo_smooth")]
    pub lfo_smooth: f64,
    #[serde(default)]
    pub lfo_rise: f64,
    #[serde(default)]
    pub lfo_delay: f64,
    #[serde(default = "default_lfo_bend")]
    pub lfo_bend: f64,
    #[serde(default = "default_lfo_phase")]
    pub lfo_phase: f64,
    #[serde(default)]
    pub lfo_rate_x10: bool,
    // LFO 2..LFO 10; LFO 1 keeps the flat fields above for ABI stability.
    #[serde(default)]
    pub extra_lfos: [LfoSettings; 9],
    pub step_rate: f64,
    pub step_sync: bool,
    pub step_division: usize,
    pub step_smoothing: f64,
    pub step_values: [f64; 16],
    pub monophonic: bool,
    pub polyphony: usize,
    pub legato: bool,
    pub glide_time: f64,
    pub oscillator_quality: usize,
    pub oscillator_start_phase: f64,
    pub oscillator_random_phase: f64,
    pub tuning_reference_hz: f64,
    pub tuning_transpose: f64,
    pub tuning_fine_cents: f64,
    pub custom_wavetable: Option<CustomWavetable>,
    // Optional per-oscillator tables; `None` falls back to `custom_wavetable`.
    #[serde(default)]
    pub oscillator_b_wavetable: Option<CustomWavetable>,
    #[serde(default)]
    pub oscillator_c_wavetable: Option<CustomWavetable>,
    pub macros: [f64; 4],
    #[serde(default)]
    pub extra_macros: [f64; 4],
    // Older documents stored 16 routes; the default keeps them loadable.
    #[serde(default = "default_mod_routes")]
    pub mod_routes: Vec<ModRoute>,
    /// Synth-level FX rack. Missing in older documents, so it defaults empty.
    #[serde(default)]
    pub fx: Vec<SynthFx>,
    /// Sample/multisample layers imported from a preset (or built directly).
    /// Decoded by `OpenSynth` at construction, never in the audio path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler: Option<crate::sampler::SamplerPatch>,
    /// Post-voice FX parameter modulation. Missing in older documents.
    #[serde(default)]
    pub fx_mod_routes: Vec<FxModRoute>,
    /// MPE zones. Missing keeps the legacy single-channel controller path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpe: Option<MpeConfig>,
    /// Per-oscillator granular sources; `None` means the slot is not granular.
    #[serde(default, skip_serializing_if = "source_slots_empty")]
    pub granular: [Option<granular::GranularPatch>; 3],
    /// Per-oscillator spectral sources; `None` means the slot is not spectral.
    #[serde(default, skip_serializing_if = "source_slots_empty")]
    pub spectral: [Option<spectral::SpectralPatch>; 3],
}

impl Default for SynthPatch {
    fn default() -> Self {
        Self {
            oscillator_a_waveform: waveform_from_value(schema_discrete_default("osc.a.waveform")),
            oscillator_a_level: schema_dsp_default("osc.a.level"),
            oscillator_a_pulse_width: schema_dsp_default("osc.a.pulse_width"),
            oscillator_a_wavetable_position: schema_dsp_default("osc.a.wavetable_position"),
            oscillator_a_warp_mode: WarpMode::from_value(schema_discrete_default(
                "osc.a.warp_mode",
            )),
            oscillator_a_warp_amount: schema_dsp_default("osc.a.warp_amount"),
            oscillator_a_warp_var: schema_dsp_default("osc.a.warp_var"),
            oscillator_a_warp2_mode: WarpMode::from_value(schema_discrete_default(
                "osc.a.warp2_mode",
            )),
            oscillator_a_warp2_amount: schema_dsp_default("osc.a.warp2_amount"),
            oscillator_a_warp2_var: schema_dsp_default("osc.a.warp2_var"),
            oscillator_a_pan: schema_dsp_default("osc.a.pan"),
            oscillator_a_semitones: 0.0,
            oscillator_b_unison: schema_discrete_default("osc.b.unison") as usize,
            oscillator_b_detune_cents: schema_dsp_default("osc.b.detune"),
            oscillator_b_stereo_width: schema_dsp_default("osc.b.width"),
            oscillator_b_waveform: waveform_from_value(schema_discrete_default("osc.b.waveform")),
            oscillator_b_level: schema_dsp_default("osc.b.level"),
            oscillator_b_semitones: schema_dsp_default("osc.b.semitones"),
            oscillator_b_pulse_width: schema_dsp_default("osc.b.pulse_width"),
            oscillator_b_wavetable_position: schema_dsp_default("osc.b.wavetable_position"),
            oscillator_b_warp_mode: WarpMode::from_value(schema_discrete_default(
                "osc.b.warp_mode",
            )),
            oscillator_b_warp_amount: schema_dsp_default("osc.b.warp_amount"),
            oscillator_b_warp_var: schema_dsp_default("osc.b.warp_var"),
            oscillator_b_warp2_mode: WarpMode::from_value(schema_discrete_default(
                "osc.b.warp2_mode",
            )),
            oscillator_b_warp2_amount: schema_dsp_default("osc.b.warp2_amount"),
            oscillator_b_warp2_var: schema_dsp_default("osc.b.warp2_var"),
            oscillator_b_pan: schema_dsp_default("osc.b.pan"),
            oscillator_c_waveform: waveform_from_value(schema_discrete_default("osc.c.waveform")),
            oscillator_c_level: schema_dsp_default("osc.c.level"),
            oscillator_c_semitones: schema_dsp_default("osc.c.semitones"),
            oscillator_c_pulse_width: schema_dsp_default("osc.c.pulse_width"),
            oscillator_c_wavetable_position: schema_dsp_default("osc.c.wavetable_position"),
            oscillator_c_warp_mode: WarpMode::from_value(schema_discrete_default(
                "osc.c.warp_mode",
            )),
            oscillator_c_warp_amount: schema_dsp_default("osc.c.warp_amount"),
            oscillator_c_warp_var: schema_dsp_default("osc.c.warp_var"),
            oscillator_c_warp2_mode: WarpMode::from_value(schema_discrete_default(
                "osc.c.warp2_mode",
            )),
            oscillator_c_warp2_amount: schema_dsp_default("osc.c.warp2_amount"),
            oscillator_c_warp2_var: schema_dsp_default("osc.c.warp2_var"),
            oscillator_c_pan: schema_dsp_default("osc.c.pan"),
            oscillator_c_unison: schema_discrete_default("osc.c.unison") as usize,
            oscillator_c_detune_cents: schema_dsp_default("osc.c.detune"),
            oscillator_c_stereo_width: schema_dsp_default("osc.c.width"),
            ring_mod_level: schema_dsp_default("ring.level"),
            sub_level: schema_dsp_default("sub.level"),
            sub_semitones: -12.0,
            sub_fine_cents: 0.0,
            sub_pan: 0.0,
            sub_waveform: SubShape::from_value(schema_discrete_default("sub.waveform")),
            sub_initial_phase: 0.0,
            sub_contiguous_phase: schema_discrete_default("sub.contiguous_phase") >= 0.5,
            noise_level: schema_dsp_default("noise.level"),
            gain: schema_dsp_default("output.gain"),
            loudness_gain: 1.0,
            velocity_to_amp: schema_dsp_default("velocity.to_amp"),
            arpeggiator: None,
            attack: schema_dsp_default("amp.attack"),
            decay: schema_dsp_default("amp.decay"),
            sustain: schema_dsp_default("amp.sustain"),
            release: schema_dsp_default("amp.release"),
            amp_curve: schema_dsp_default("amp.curve"),
            amp_damping: schema_dsp_default("amp.damping"),
            detune_cents: schema_dsp_default("unison.detune"),
            unison_voices: schema_discrete_default("unison.voices") as usize,
            stereo_width: schema_dsp_default("osc.a.width"),
            filter_cutoff: FILTER_CUTOFF_HZ.default,
            filter_resonance: 0.15,
            filter_drive: 0.0,
            filter_mode: filter_mode_from_value(schema_discrete_default("filter.mode")),
            filter_model: FilterModel::from_value(schema_discrete_default("filter.model")),
            filter_wet: 1.0,
            filter_keytrack: 0.25,
            filter_keytrack_origin: 60.0,
            filter_envelope_amount: schema_dsp_default("filter.env.amount"),
            filter_attack: schema_dsp_default("filter_env.attack"),
            filter_decay: schema_dsp_default("filter_env.decay"),
            filter_sustain: schema_dsp_default("filter_env.sustain"),
            filter_release: schema_dsp_default("filter_env.release"),
            filter_curve: schema_dsp_default("filter_env.curve"),
            env3_attack: schema_dsp_default("env3.attack"),
            env3_decay: schema_dsp_default("env3.decay"),
            env3_sustain: schema_dsp_default("env3.sustain"),
            env3_release: schema_dsp_default("env3.release"),
            env3_curve: schema_dsp_default("env3.curve"),
            env4_attack: schema_dsp_default("env4.attack"),
            env4_decay: schema_dsp_default("env4.decay"),
            env4_sustain: schema_dsp_default("env4.sustain"),
            env4_release: schema_dsp_default("env4.release"),
            env4_curve: schema_dsp_default("env4.curve"),
            filter2_enabled: schema_discrete_default("filter2.enabled") >= 0.5,
            filter2_cutoff: FILTER_CUTOFF_HZ.default,
            filter2_resonance: 0.15,
            filter2_drive: 0.0,
            filter2_mode: filter_mode_from_value(schema_discrete_default("filter2.mode")),
            filter2_model: FilterModel::from_value(schema_discrete_default("filter2.model")),
            filter2_wet: 1.0,
            filter_routing: default_filter_routing(),
            source_filter_routes: None,
            lfo_rate: LFO_RATE_HZ.default,
            lfo_pitch_cents: 0.0,
            legacy_lfo_waveform: None,
            lfo_retrigger: schema_discrete_default("lfo.retrigger") >= 0.5,
            lfo_sync: schema_discrete_default("lfo.sync") >= 0.5,
            lfo_division: schema_discrete_default("lfo.division") as usize,
            lfo_sync_div: None,
            lfo_curve: default_lfo_curve(),
            lfo_shape_type: LfoShapeType::from_value(schema_discrete_default("lfo.shape")),
            lfo_shape: Arc::new(LfoShape::triangle()),
            lfo_smooth: schema_dsp_default("lfo.smooth"),
            lfo_rise: schema_dsp_default("lfo.rise"),
            lfo_delay: schema_dsp_default("lfo.delay"),
            lfo_bend: schema_dsp_default("lfo.bend"),
            lfo_phase: schema_dsp_default("lfo.phase"),
            lfo_rate_x10: schema_discrete_default("lfo.rate_x10") >= 0.5,
            extra_lfos: std::array::from_fn(|_| LfoSettings::default()),
            step_rate: STEP_RATE_HZ.default,
            step_sync: schema_discrete_default("step.sync") >= 0.5,
            step_division: schema_discrete_default("step.division") as usize,
            step_smoothing: schema_dsp_default("step.smoothing"),
            step_values: [schema_dsp_default("step.1"); 16],
            monophonic: schema_discrete_default("voice.mono") >= 0.5,
            polyphony: schema_discrete_default("voice.polyphony") as usize,
            legato: schema_discrete_default("voice.legato") >= 0.5,
            glide_time: schema_dsp_default("voice.glide"),
            oscillator_quality: schema_discrete_default("quality.oversampling") as usize,
            oscillator_start_phase: 0.0,
            oscillator_random_phase: 0.0,
            tuning_reference_hz: TUNING_REFERENCE_HZ.default,
            tuning_transpose: TUNING_TRANSPOSE.default,
            tuning_fine_cents: TUNING_FINE.default,
            custom_wavetable: None,
            oscillator_b_wavetable: None,
            oscillator_c_wavetable: None,
            macros: [0.0; 4],
            extra_macros: [0.0; 4],
            mod_routes: default_mod_routes(),
            fx: Vec::new(),
            sampler: None,
            fx_mod_routes: Vec::new(),
            mpe: None,
            granular: [None, None, None],
            spectral: [None, None, None],
        }
    }
}

fn source_slots_empty<T>(slots: &[Option<T>; 3]) -> bool {
    slots.iter().all(Option::is_none)
}

impl SynthPatch {
    /// Algorithmic latency of this patch's enabled FX rack, in samples. The
    /// offline renderer reports it as the track's source latency so the mixer's
    /// PDC aligns other tracks to the delay insert.
    pub fn fx_latency_samples(&self, sample_rate: u32) -> usize {
        fx::fx_rack_latency(&self.fx, f64::from(sample_rate))
    }

    fn waveforms(&self) -> [Waveform; 3] {
        [
            self.oscillator_a_waveform,
            self.oscillator_b_waveform,
            self.oscillator_c_waveform,
        ]
    }

    pub fn apply_params(&mut self, params: Option<&Vec<(String, ParamValue)>>) {
        let Some(params) = params else { return };
        if let Some((_, ParamValue::String(path))) =
            params.iter().find(|(name, _)| name == "wavetable.file")
        {
            match CustomWavetable::load(path) {
                Ok(table) => self.custom_wavetable = Some(table),
                Err(error) => {
                    log::warn!("OpenSynth could not load wavetable '{}': {}", path, error)
                }
            }
        }
        self.apply_fx_params(params);
        self.apply_lfo_shape_params(params);
        for (name, value) in params {
            let Some(value) = param_as_f64(value) else {
                continue;
            };
            self.set_param(name, value);
        }
        // The engine's loudness law rides in the parameter list, so every path
        // that builds a patch applies the same trim.
        if let Some((_, value)) = params
            .iter()
            .rev()
            .find(|(name, _)| name == LOUDNESS_TRIM_PARAM)
            && let Some(db) = param_as_f64(value)
            && db.is_finite()
        {
            self.loudness_gain = 10.0_f64.powf(db / 20.0);
        }
        self.normalize_shapes();
    }

    /// Shape point arrays arrive as `ParamValue::Array`, not as one number.
    fn apply_lfo_shape_params(&mut self, params: &[(String, ParamValue)]) {
        for (name, value) in params {
            let Some((index, field)) = lfo_shape_field(name) else {
                continue;
            };
            let ParamValue::Array(values) = value else {
                continue;
            };
            let shape = if index == 0 {
                Arc::make_mut(&mut self.lfo_shape)
            } else {
                Arc::make_mut(&mut self.extra_lfos[index - 1].shape)
            };
            let target = match field {
                "x" => &mut shape.x,
                "y" => &mut shape.y,
                _ => &mut shape.curve,
            };
            let count = values.len().min(LFO_SHAPE_POINTS);
            for (slot, value) in target.iter_mut().zip(values.iter().take(count)) {
                *slot = value.clamp(0.0, 1.0) as f32;
            }
            shape.points = shape.points.max(count).clamp(2, LFO_SHAPE_POINTS);
        }
    }

    /// Fold a retired waveform selector into the shape. Old documents keep
    /// their intent while the engine only reads shapes.
    fn apply_legacy_waveform(&mut self, index: usize, waveform: LegacyLfoWaveform) {
        let rebuild_curve = index == 0
            && matches!(
                waveform,
                LegacyLfoWaveform::Curve | LegacyLfoWaveform::Smooth | LegacyLfoWaveform::Stepped
            );
        let (new_shape, new_type) = match waveform {
            LegacyLfoWaveform::Sine => (LfoShape::sine(), LfoShapeType::Curve),
            LegacyLfoWaveform::Triangle => (LfoShape::triangle(), LfoShapeType::Curve),
            LegacyLfoWaveform::Saw => (LfoShape::saw(), LfoShapeType::Curve),
            LegacyLfoWaveform::Square => (LfoShape::square(), LfoShapeType::Curve),
            LegacyLfoWaveform::SampleAndHold => (LfoShape::triangle(), LfoShapeType::RandomHold),
            LegacyLfoWaveform::Curve | LegacyLfoWaveform::Smooth | LegacyLfoWaveform::Stepped => {
                (LfoShape::triangle(), LfoShapeType::Curve)
            }
        };
        if index == 0 {
            self.lfo_shape = Arc::new(new_shape);
            self.lfo_shape_type = new_type;
        } else {
            self.extra_lfos[index - 1].shape = Arc::new(new_shape);
            self.extra_lfos[index - 1].shape_type = new_type;
        }
        if rebuild_curve {
            self.rebuild_legacy_curve();
        }
    }

    /// Build LFO 1's shape from the retired 256-point shared curve.
    fn rebuild_legacy_curve(&mut self) {
        let x: Vec<f64> = (0..LFO_CURVE_POINTS)
            .map(|index| index as f64 / LFO_CURVE_POINTS as f64)
            .collect();
        let y: Vec<f64> = self
            .lfo_curve
            .iter()
            .map(|value| 0.5 * (*value as f64 + 1.0))
            .collect();
        self.lfo_shape = Arc::new(LfoShape::from_points(&x, &y, &[0.5; LFO_CURVE_POINTS]));
        self.lfo_shape_type = LfoShapeType::Curve;
    }

    /// Old JSON stored an LFO waveform enum and a shared curve; fold both in.
    pub fn normalize_legacy_lfo(&mut self) {
        if let Some(waveform) = self.legacy_lfo_waveform.take() {
            self.apply_legacy_waveform(0, waveform);
        }
        for index in 0..9 {
            if let Some(waveform) = self.extra_lfos[index].legacy_waveform.take() {
                self.apply_legacy_waveform(index + 1, waveform);
            }
        }
        self.normalize_shapes();
    }

    /// Keep every active shape sorted; array params may arrive in any order.
    fn normalize_shapes(&mut self) {
        Arc::make_mut(&mut self.lfo_shape).sort_points();
        for settings in &mut self.extra_lfos {
            Arc::make_mut(&mut settings.shape).sort_points();
        }
    }

    /// Rebuild the rack from `fx.N.type`/`fx.N.enabled`/`fx.N.<param>` project
    /// parameters. Missing indexes are removed, so deleting the keys deletes
    /// the module. Called only on load/edit paths.
    fn apply_fx_params(&mut self, params: &[(String, ParamValue)]) {
        let mut present = false;
        let declared_count = params
            .iter()
            .find(|(name, _)| name == "fx.count")
            .and_then(|(_, value)| param_as_f64(value))
            .map(|value| (value.round().max(0.0) as usize).min(fx::MAX_SYNTH_FX));
        let mut types: [Option<SynthFxType>; fx::MAX_SYNTH_FX] = [None; fx::MAX_SYNTH_FX];
        let mut enabled: [Option<bool>; fx::MAX_SYNTH_FX] = [None; fx::MAX_SYNTH_FX];
        let mut values: [Vec<(String, f64)>; fx::MAX_SYNTH_FX] =
            std::array::from_fn(|_| Vec::new());
        for (name, value) in params {
            let Some((slot, field)) = parse_fx_param_name(name) else {
                continue;
            };
            // `fx.N.*` is 1-based like `mod.N.*`; index N-1 into the fixed rack.
            let Some(index) = slot.checked_sub(1) else {
                continue;
            };
            if index >= fx::MAX_SYNTH_FX {
                continue;
            }
            present = true;
            match field {
                "type" => types[index] = fx_type_from_param(value),
                "enabled" => enabled[index] = param_as_f64(value).map(|value| value >= 0.5),
                _ => {
                    if let Some(value) = param_as_f64(value) {
                        values[index].push((field.to_string(), value));
                    }
                }
            }
        }
        if !present && declared_count.is_none() {
            return;
        }
        let count = declared_count.unwrap_or(fx::MAX_SYNTH_FX);
        let mut chain = Vec::new();
        for index in 0..count.min(fx::MAX_SYNTH_FX) {
            let Some(fx_type) = types[index] else {
                continue;
            };
            let mut module = SynthFx::new(fx_type);
            module.enabled = enabled[index].unwrap_or(true);
            for (name, value) in std::mem::take(&mut values[index]) {
                if module.param(&name).is_some() {
                    module.set_param(&name, value);
                } else {
                    module.params.push((name, value));
                }
            }
            chain.push(module);
        }
        self.fx = chain;
    }

    fn set_fx_param(&mut self, slot: usize, field: &str, value: f64) {
        let Some(index) = slot.checked_sub(1) else {
            return;
        };
        if index >= fx::MAX_SYNTH_FX {
            return;
        }
        let Some(module) = self.fx.get_mut(index) else {
            return;
        };
        match field {
            "type" => {
                let Some(fx_type) = SynthFxType::from_index(value) else {
                    return;
                };
                if module.fx_type == fx_type {
                    return;
                }
                let defaults = fx_type.default_params();
                module.fx_type = fx_type;
                // Reuse the existing parameter storage where possible so a live
                // type change does not allocate in the audio callback.
                for (slot, (name, default)) in defaults.iter().enumerate() {
                    if let Some(entry) = module.params.get_mut(slot) {
                        entry.0.clear();
                        entry.0.push_str(name);
                        entry.1 = *default;
                    } else {
                        module.params.push(((*name).to_string(), *default));
                    }
                }
                module.params.truncate(defaults.len());
            }
            "enabled" => module.enabled = value >= 0.5,
            _ => module.set_param(field, value),
        }
    }

    /// Returns whether `name` is a parameter this patch understands; unknown
    /// keys are ignored so callers can report them instead of failing silently.
    pub fn set_param(&mut self, name: &str, value: f64) -> bool {
        // Invalid automation must not poison persistent patch state or DSP caches.
        if !value.is_finite() {
            return true;
        }
        if let Some(index) = lfo_curve_index(name) {
            self.lfo_curve[index] = value.clamp(-1.0, 1.0) as f32;
            self.rebuild_legacy_curve();
            return true;
        }
        if name == "fx.count" {
            let count = (value.round().max(0.0) as usize).min(fx::MAX_SYNTH_FX);
            self.fx.truncate(count);
            return true;
        }
        if let Some((index, field)) = parse_fx_param_name(name) {
            self.set_fx_param(index, field, value);
            return true;
        }
        match name {
            "synth_version" => {}
            "osc.waveform" | "waveform" | "osc.a.waveform" => {
                self.oscillator_a_waveform = waveform_from_value(value)
            }
            "osc.a.level" => self.oscillator_a_level = schema_dsp_clamp(name, value),
            "osc.a.pulse_width" => self.oscillator_a_pulse_width = schema_dsp_clamp(name, value),
            "osc.a.wavetable_position" => {
                self.oscillator_a_wavetable_position = schema_dsp_clamp(name, value)
            }
            "osc.a.warp_mode" => self.oscillator_a_warp_mode = WarpMode::from_value(value),
            "osc.a.warp_amount" => self.oscillator_a_warp_amount = schema_dsp_clamp(name, value),
            "osc.a.warp_var" => self.oscillator_a_warp_var = schema_dsp_clamp(name, value),
            "osc.a.warp2_mode" => self.oscillator_a_warp2_mode = WarpMode::from_value(value),
            "osc.a.warp2_amount" => self.oscillator_a_warp2_amount = schema_dsp_clamp(name, value),
            "osc.a.warp2_var" => self.oscillator_a_warp2_var = schema_dsp_clamp(name, value),
            "osc.a.pan" => self.oscillator_a_pan = schema_dsp_clamp(name, value),
            "osc.a.semitones" => self.oscillator_a_semitones = value.clamp(-48.0, 48.0),
            "osc.b.waveform" => self.oscillator_b_waveform = waveform_from_value(value),
            "osc.b.level" => self.oscillator_b_level = schema_dsp_clamp(name, value),
            "osc.b.semitones" => self.oscillator_b_semitones = schema_dsp_clamp(name, value),
            "osc.b.pulse_width" => self.oscillator_b_pulse_width = schema_dsp_clamp(name, value),
            "osc.b.wavetable_position" => {
                self.oscillator_b_wavetable_position = schema_dsp_clamp(name, value)
            }
            "osc.b.warp_mode" => self.oscillator_b_warp_mode = WarpMode::from_value(value),
            "osc.b.warp_amount" => self.oscillator_b_warp_amount = schema_dsp_clamp(name, value),
            "osc.b.warp_var" => self.oscillator_b_warp_var = schema_dsp_clamp(name, value),
            "osc.b.warp2_mode" => self.oscillator_b_warp2_mode = WarpMode::from_value(value),
            "osc.b.warp2_amount" => self.oscillator_b_warp2_amount = schema_dsp_clamp(name, value),
            "osc.b.warp2_var" => self.oscillator_b_warp2_var = schema_dsp_clamp(name, value),
            "osc.b.pan" => self.oscillator_b_pan = schema_dsp_clamp(name, value),
            "osc.a.unison" => self.unison_voices = schema_discrete_index(name, value),
            "osc.b.unison" => self.oscillator_b_unison = schema_discrete_index(name, value),
            "osc.a.detune" => self.detune_cents = schema_dsp_clamp(name, value),
            "osc.b.detune" => self.oscillator_b_detune_cents = schema_dsp_clamp(name, value),
            "osc.a.width" => self.stereo_width = schema_dsp_clamp(name, value),
            "osc.b.width" => self.oscillator_b_stereo_width = schema_dsp_clamp(name, value),
            "osc.c.waveform" => self.oscillator_c_waveform = waveform_from_value(value),
            "osc.c.level" => self.oscillator_c_level = schema_dsp_clamp(name, value),
            "osc.c.semitones" => self.oscillator_c_semitones = schema_dsp_clamp(name, value),
            "osc.c.pulse_width" => self.oscillator_c_pulse_width = schema_dsp_clamp(name, value),
            "osc.c.wavetable_position" => {
                self.oscillator_c_wavetable_position = schema_dsp_clamp(name, value)
            }
            "osc.c.warp_mode" => self.oscillator_c_warp_mode = WarpMode::from_value(value),
            "osc.c.warp_amount" => self.oscillator_c_warp_amount = schema_dsp_clamp(name, value),
            "osc.c.warp_var" => self.oscillator_c_warp_var = schema_dsp_clamp(name, value),
            "osc.c.warp2_mode" => self.oscillator_c_warp2_mode = WarpMode::from_value(value),
            "osc.c.warp2_amount" => self.oscillator_c_warp2_amount = schema_dsp_clamp(name, value),
            "osc.c.warp2_var" => self.oscillator_c_warp2_var = schema_dsp_clamp(name, value),
            "osc.c.pan" => self.oscillator_c_pan = schema_dsp_clamp(name, value),
            "osc.c.unison" => self.oscillator_c_unison = schema_discrete_index(name, value),
            "osc.c.detune" => self.oscillator_c_detune_cents = schema_dsp_clamp(name, value),
            "osc.c.width" => self.oscillator_c_stereo_width = schema_dsp_clamp(name, value),
            "ring.level" => self.ring_mod_level = schema_dsp_clamp(name, value),
            "sub.level" => self.sub_level = schema_dsp_clamp(name, value),
            "sub.semitones" => self.sub_semitones = value.clamp(-48.0, 48.0),
            "sub.fine" | "sub.fine_cents" => self.sub_fine_cents = value.clamp(-100.0, 100.0),
            "sub.pan" => self.sub_pan = value.clamp(-1.0, 1.0),
            "sub.waveform" | "sub.shape" => self.sub_waveform = SubShape::from_value(value),
            "sub.initial_phase" => self.sub_initial_phase = value.clamp(0.0, 1.0),
            "sub.contiguous_phase" => self.sub_contiguous_phase = value >= 0.5,
            "noise.level" => self.noise_level = schema_dsp_clamp(name, value),
            "output.gain" | "gain" => self.gain = schema_dsp_clamp(name, value),
            "amp.attack" | "attack" => self.attack = schema_dsp_clamp(name, value),
            "amp.decay" | "decay" => self.decay = schema_dsp_clamp(name, value),
            "amp.sustain" | "sustain" => self.sustain = schema_dsp_clamp(name, value),
            "amp.release" | "release" => self.release = schema_dsp_clamp(name, value),
            "amp.curve" => self.amp_curve = schema_dsp_clamp(name, value),
            "amp.damping" => self.amp_damping = schema_dsp_clamp(name, value),
            "velocity.to_amp" => self.velocity_to_amp = schema_dsp_clamp(name, value),
            "unison.detune" | "detune" => self.detune_cents = schema_dsp_clamp(name, value),
            "unison.voices" => self.unison_voices = schema_discrete_index(name, value),
            "unison.width" | "stereo_width" => self.stereo_width = schema_dsp_clamp(name, value),
            "filter.cutoff" | "cutoff" => self.filter_cutoff = schema_dsp_clamp(name, value),
            "filter.resonance" | "resonance" => self.filter_resonance = value.clamp(0.0, 1.0),
            "filter.drive" => self.filter_drive = value.clamp(0.0, 1.0),
            "filter.wet" => self.filter_wet = value.clamp(0.0, 1.0),
            "filter.mode" => self.filter_mode = filter_mode_from_value(value),
            "filter.model" => self.filter_model = FilterModel::from_value(value),
            "filter.keytrack" => self.filter_keytrack = value.clamp(0.0, 1.0),
            "filter.keytrack.origin" => self.filter_keytrack_origin = value.clamp(0.0, 127.0),
            "filter.env.amount" => self.filter_envelope_amount = schema_dsp_clamp(name, value),
            "filter_env.attack" => self.filter_attack = schema_dsp_clamp(name, value),
            "filter_env.decay" => self.filter_decay = schema_dsp_clamp(name, value),
            "filter_env.sustain" => self.filter_sustain = schema_dsp_clamp(name, value),
            "filter_env.release" => self.filter_release = schema_dsp_clamp(name, value),
            "filter_env.curve" => self.filter_curve = schema_dsp_clamp(name, value),
            "env3.attack" => self.env3_attack = schema_dsp_clamp(name, value),
            "env3.decay" => self.env3_decay = schema_dsp_clamp(name, value),
            "env3.sustain" => self.env3_sustain = schema_dsp_clamp(name, value),
            "env3.release" => self.env3_release = schema_dsp_clamp(name, value),
            "env3.curve" => self.env3_curve = schema_dsp_clamp(name, value),
            "env4.attack" => self.env4_attack = schema_dsp_clamp(name, value),
            "env4.decay" => self.env4_decay = schema_dsp_clamp(name, value),
            "env4.sustain" => self.env4_sustain = schema_dsp_clamp(name, value),
            "env4.release" => self.env4_release = schema_dsp_clamp(name, value),
            "env4.curve" => self.env4_curve = schema_dsp_clamp(name, value),
            "filter2.enabled" => self.filter2_enabled = value >= 0.5,
            "filter2.cutoff" => self.filter2_cutoff = schema_dsp_clamp(name, value),
            "filter2.resonance" => self.filter2_resonance = value.clamp(0.0, 1.0),
            "filter2.drive" => self.filter2_drive = value.clamp(0.0, 1.0),
            "filter2.wet" => self.filter2_wet = value.clamp(0.0, 1.0),
            "filter2.mode" => self.filter2_mode = filter_mode_from_value(value),
            "filter2.model" => self.filter2_model = FilterModel::from_value(value),
            "filter.routing" => {
                self.filter_routing = match value.round() as i32 {
                    1 => FilterRouting::Parallel,
                    _ => FilterRouting::Serial,
                }
            }
            "lfo.rate" => self.lfo_rate = schema_dsp_clamp(name, value),
            "lfo.pitch" => self.lfo_pitch_cents = value.clamp(0.0, 200.0),
            "lfo.waveform" => self.apply_legacy_waveform(0, LegacyLfoWaveform::from_value(value)),
            "lfo.shape" => self.lfo_shape_type = LfoShapeType::from_value(value),
            "lfo.shape.points" => {
                Arc::make_mut(&mut self.lfo_shape).points = schema_discrete_index(name, value)
            }
            "lfo.retrigger" => self.lfo_retrigger = value >= 0.5,
            "lfo.sync" => self.lfo_sync = value >= 0.5,
            "lfo.division" => self.lfo_division = schema_discrete_index(name, value),
            "lfo.sync_div" => self.lfo_sync_div = Some(schema_discrete_index(name, value)),
            "lfo.smooth" => self.lfo_smooth = schema_dsp_clamp(name, value),
            "lfo.rise" => self.lfo_rise = schema_dsp_clamp(name, value),
            "lfo.delay" => self.lfo_delay = schema_dsp_clamp(name, value),
            "lfo.bend" => self.lfo_bend = schema_dsp_clamp(name, value),
            "lfo.phase" => self.lfo_phase = schema_dsp_clamp(name, value),
            "lfo.rate_x10" => self.lfo_rate_x10 = value >= 0.5,
            "step.rate" => self.step_rate = schema_dsp_clamp(name, value),
            "step.sync" => self.step_sync = value >= 0.5,
            "step.division" => self.step_division = schema_discrete_index(name, value),
            "step.smoothing" => self.step_smoothing = schema_dsp_clamp(name, value),
            "voice.mono" => self.monophonic = value >= 0.5,
            "voice.polyphony" => self.polyphony = schema_discrete_index(name, value),
            "voice.legato" => self.legato = value >= 0.5,
            "voice.glide" => self.glide_time = schema_dsp_clamp(name, value),
            "quality.oversampling" => self.oscillator_quality = schema_discrete_index(name, value),
            "osc.start_phase" => self.oscillator_start_phase = value.clamp(0.0, 1.0),
            "osc.random_phase" => self.oscillator_random_phase = value.clamp(0.0, 1.0),
            "tuning.reference_hz" => {
                self.tuning_reference_hz = TUNING_REFERENCE_HZ
                    .clamp(value)
                    .expect("valid tuning domain and finite value")
            }
            "tuning.transpose" => {
                self.tuning_transpose = TUNING_TRANSPOSE
                    .clamp(value)
                    .expect("valid tuning domain and finite value")
            }
            "tuning.fine" => {
                self.tuning_fine_cents = TUNING_FINE
                    .clamp(value)
                    .expect("valid tuning domain and finite value")
            }
            "mpe.enabled" => {
                if value >= 0.5 {
                    if self.mpe.is_none_or(|config| !config.enabled()) {
                        let bend_range = self.mpe.and_then(|config| config.bend_range);
                        self.mpe = Some(MpeConfig {
                            bend_range,
                            ..MpeConfig::lower_or_default()
                        });
                    }
                } else {
                    self.mpe = None;
                }
            }
            "mpe.bend_range" => {
                let config = self.mpe.get_or_insert_with(MpeConfig::default);
                config.bend_range = Some(value.clamp(0.0, 96.0));
            }
            "mpe_first_channel" | "mpe.lower_first" => {
                let config = self.mpe.get_or_insert_with(MpeConfig::default);
                let first = value.round().clamp(2.0, 16.0) as u8;
                config.lower.get_or_insert(MpeMemberRange::new(2, 16)).first = first;
            }
            "mpe_last_channel" | "mpe.lower_last" => {
                let config = self.mpe.get_or_insert_with(MpeConfig::default);
                let last = value.round().clamp(2.0, 16.0) as u8;
                config.lower.get_or_insert(MpeMemberRange::new(2, 16)).last = last;
            }
            "mpe.upper_first" => {
                let config = self.mpe.get_or_insert_with(MpeConfig::default);
                let first = value.round().clamp(1.0, 15.0) as u8;
                config.upper.get_or_insert(MpeMemberRange::new(2, 15)).first = first;
            }
            "mpe.upper_last" => {
                let config = self.mpe.get_or_insert_with(MpeConfig::default);
                let last = value.round().clamp(1.0, 15.0) as u8;
                config.upper.get_or_insert(MpeMemberRange::new(2, 15)).last = last;
            }
            _ => {
                if let Some(index) = indexed_param(name, "macro.", 8) {
                    if index < 4 {
                        self.macros[index] = value.clamp(0.0, 1.0);
                    } else {
                        self.extra_macros[index - 4] = value.clamp(0.0, 1.0);
                    }
                } else if let Some(index) = indexed_param(name, "step.", 16) {
                    self.step_values[index] = schema_dsp_clamp(name, value);
                } else if let Some((index, field)) = extra_lfo_param(name) {
                    if field == "waveform" {
                        self.apply_legacy_waveform(index + 1, LegacyLfoWaveform::from_value(value));
                        return true;
                    }
                    let settings = &mut self.extra_lfos[index];
                    match field {
                        "rate" => settings.rate = schema_dsp_clamp(name, value),
                        "shape" => settings.shape_type = LfoShapeType::from_value(value),
                        "shape.points" => {
                            Arc::make_mut(&mut settings.shape).points =
                                schema_discrete_index(name, value)
                        }
                        "sync" => settings.sync = value >= 0.5,
                        "retrigger" => settings.retrigger = value >= 0.5,
                        "division" => settings.division = schema_discrete_index(name, value),
                        "sync_div" => settings.sync_div = Some(schema_discrete_index(name, value)),
                        "smooth" => settings.smooth = schema_dsp_clamp(name, value),
                        "rise" => settings.rise = schema_dsp_clamp(name, value),
                        "delay" => settings.delay = schema_dsp_clamp(name, value),
                        "bend" => settings.bend = schema_dsp_clamp(name, value),
                        "phase" => settings.phase = schema_dsp_clamp(name, value),
                        "rate_x10" => settings.rate_x10 = value >= 0.5,
                        _ => return false,
                    }
                } else if let Some((index, field)) = fx_mod_route_param(name) {
                    if index >= self.fx_mod_routes.len() {
                        self.fx_mod_routes.resize(index + 1, FxModRoute::default());
                    }
                    match field {
                        "source" => self.fx_mod_routes[index].source = mod_source_from_value(value),
                        "fx_index" => {
                            self.fx_mod_routes[index].fx_index = value.round().max(0.0) as usize;
                        }
                        "parameter" => {
                            self.fx_mod_routes[index].parameter = fx_param_from_value(value)
                        }
                        "amount" => {
                            self.fx_mod_routes[index].amount = schema_dsp_clamp(name, value)
                        }
                        "bipolar" => self.fx_mod_routes[index].bipolar = value >= 0.5,
                        _ => return false,
                    }
                } else if let Some((index, field)) = route_param(name) {
                    // Writing any high slot materializes the whole 64-slot matrix.
                    if index >= self.mod_routes.len() {
                        self.mod_routes.resize(MAX_MOD_ROUTES, ModRoute::default());
                    }
                    match field {
                        "source" => self.mod_routes[index].source = mod_source_from_value(value),
                        "destination" => {
                            self.mod_routes[index].destination = mod_destination_from_value(value)
                        }
                        "amount" => self.mod_routes[index].amount = schema_dsp_clamp(name, value),
                        "curve" => self.mod_routes[index].curve = schema_dsp_clamp(name, value),
                        "slew_up" => self.mod_routes[index].slew_up = schema_dsp_clamp(name, value),
                        "slew_down" => {
                            self.mod_routes[index].slew_down = schema_dsp_clamp(name, value)
                        }
                        "bipolar" => self.mod_routes[index].bipolar = value >= 0.5,
                        // 0 disables the auxiliary source; 1..=29 are source ids.
                        "aux" => {
                            let aux = value.round() as i32;
                            self.mod_routes[index].auxiliary = if aux <= 0 {
                                None
                            } else {
                                Some(mod_source_from_value(f64::from(aux) - 1.0))
                            };
                        }
                        "aux_invert" => {
                            self.mod_routes[index].auxiliary_inverted = value >= 0.5;
                        }
                        _ => return false,
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    /// Apply host automation, whose parameter payload is normalized to 0.0-1.0.
    pub fn set_normalized_param(&mut self, name: &str, normalized: f64) {
        if !normalized.is_finite() {
            return;
        }
        self.set_param(name, normalized_param_to_physical(name, normalized));
    }
}

/// The discrete controls explicitly mapped by the version-2 automation ABI.
/// Other catalog controls retain their historical unit-fraction fallback until
/// a separately versioned automation migration defines their DSP law.
fn discrete_automation_domain(name: &str) -> Option<oc_device_schema::synth::SynthNumericDomain> {
    let supported = match name {
        "osc.waveform"
        | "waveform"
        | "osc.a.waveform"
        | "osc.b.waveform"
        | "osc.c.waveform"
        | "lfo.waveform"
        | "osc.a.warp_mode"
        | "osc.b.warp_mode"
        | "osc.c.warp_mode"
        | "osc.a.warp2_mode"
        | "osc.b.warp2_mode"
        | "osc.c.warp2_mode"
        | "filter.mode"
        | "filter2.mode"
        | "filter.model"
        | "filter2.model"
        | "quality.oversampling"
        | "unison.voices"
        | "osc.a.unison"
        | "osc.b.unison"
        | "osc.c.unison"
        | "lfo.division"
        | "step.division"
        | "step.sync"
        | "sub.waveform"
        | "sub.shape"
        | "lfo.shape"
        | "lfo.sync_div"
        | "voice.polyphony" => true,
        name if name.starts_with("mod.") => matches!(
            name.rsplit('.').next(),
            Some("source" | "destination" | "aux" | "bipolar" | "aux_invert")
        ),
        name if name.starts_with("fxmod.") => matches!(
            name.rsplit('.').next(),
            Some("source" | "parameter" | "fx_index")
        ),
        name if parse_fx_param_name(name).is_some_and(|(_, field)| field == "type") => true,
        name if extra_lfo_param(name).is_some_and(|(_, field)| {
            matches!(
                field,
                "shape" | "sync_div" | "waveform" | "rate_x10" | "division"
            )
        }) =>
        {
            true
        }
        _ => false,
    };
    supported
        .then(|| oc_device_schema::synth::numeric_domain(name))
        .flatten()
}

fn filter_mode_from_value(value: f64) -> FilterMode {
    match value.round() as i32 {
        1 => FilterMode::BandPass,
        2 => FilterMode::HighPass,
        3 => FilterMode::Notch,
        _ => FilterMode::LowPass,
    }
}

fn default_oscillator_fraction() -> f64 {
    UNIT_FRACTION.default
}

fn default_lfo_phase() -> f64 {
    schema_dsp_default("lfo.phase")
}

fn default_lfo_bend() -> f64 {
    schema_dsp_default("lfo.bend")
}

fn default_lfo_smooth() -> f64 {
    schema_dsp_default("lfo.smooth")
}

fn default_extra_lfo_phase() -> f64 {
    schema_dsp_default("lfo2.phase")
}

fn default_extra_lfo_bend() -> f64 {
    schema_dsp_default("lfo2.bend")
}

fn default_extra_lfo_smooth() -> f64 {
    schema_dsp_default("lfo2.smooth")
}

fn default_mod_route_curve() -> f64 {
    schema_dsp_default("mod.1.curve")
}
fn default_env_curve() -> f64 {
    schema_dsp_default("env3.curve")
}

fn schema_dsp_default(name: &str) -> f64 {
    oc_device_schema::synth::dsp_normalization(name)
        .expect("catalogued DSP control")
        .domain
        .default
}

fn schema_dsp_clamp(name: &str, value: f64) -> f64 {
    oc_device_schema::synth::dsp_normalization(name)
        .expect("catalogued DSP control")
        .domain
        .clamp(value)
        .expect("finite DSP parameter")
}

fn schema_discrete_default(name: &str) -> f64 {
    oc_device_schema::synth::numeric_domain(name)
        .expect("catalogued discrete control")
        .default
}

fn schema_discrete_index(name: &str, value: f64) -> usize {
    oc_device_schema::synth::numeric_domain(name)
        .expect("catalogued discrete control")
        .clamp(value.round())
        .expect("finite discrete parameter") as usize
}

/// Physical value a normalized host-automation position selects for `name`.
pub fn normalized_param_to_physical(name: &str, normalized: f64) -> f64 {
    if !normalized.is_finite() {
        return 0.0;
    }
    let value = normalized.clamp(0.0, 1.0);
    if let Some(domain) = discrete_automation_domain(name) {
        return domain
            .denormalize_linear(value)
            .expect("valid discrete domain");
    }
    if let Some(mapping) = oc_device_schema::synth::dsp_normalization(name) {
        return mapping
            .denormalize(value)
            .expect("valid DSP normalization law");
    }
    match name {
        "lfo.pitch" => value * 200.0,
        name if name.starts_with("lfo.curve.") => value * 2.0 - 1.0,
        "tuning.reference_hz" => TUNING_REFERENCE_HZ
            .denormalize_linear(value)
            .expect("valid tuning domain"),
        "tuning.transpose" => TUNING_TRANSPOSE
            .denormalize_linear(value)
            .expect("valid tuning domain"),
        "tuning.fine" => TUNING_FINE
            .denormalize_linear(value)
            .expect("valid tuning domain"),
        _ => value,
    }
}

/// Whether a patch parameter takes continuous values, so a lane can ramp it.
pub fn patch_param_is_continuous(name: &str) -> bool {
    use oc_device_schema::synth::{SynthValueKind, find_family_field, find_param};
    find_param(name)
        .map(|param| param.kind)
        .or_else(|| find_family_field(name).map(|(_, field)| field.kind))
        .is_some_and(|kind| kind == SynthValueKind::Continuous)
}

/// Normalized host-automation position that selects `physical` for `name`.
///
/// Inverse of [`normalized_param_to_physical`]; `None` when the parameter has no
/// normalized mapping for that physical value.
pub fn physical_param_to_normalized(name: &str, physical: f64) -> Option<f64> {
    if !physical.is_finite() {
        return None;
    }
    if let Some(domain) = discrete_automation_domain(name) {
        return domain.normalize_linear(physical);
    }
    if let Some(mapping) = oc_device_schema::synth::dsp_normalization(name) {
        return mapping.normalize(physical);
    }
    let normalized = match name {
        "lfo.pitch" => physical / 200.0,
        name if name.starts_with("lfo.curve.") => (physical + 1.0) / 2.0,
        "tuning.reference_hz" => TUNING_REFERENCE_HZ.normalize_linear(physical)?,
        "tuning.transpose" => TUNING_TRANSPOSE.normalize_linear(physical)?,
        "tuning.fine" => TUNING_FINE.normalize_linear(physical)?,
        _ if (0.0..=1.0).contains(&physical) => physical,
        _ => return None,
    };
    normalized.is_finite().then(|| normalized.clamp(0.0, 1.0))
}

/// Re-express a lane value written against the original normalized table in
/// the current (widened) table.
///
/// The old table mapped `v` onto `v * old_max` and the current one onto
/// `v * new_max`, so `v * old_max / new_max` preserves the physical value the
/// old patch used, including the rounding to a discrete id. A few keys had no
/// normalized arm and were applied as physical values; those map to the
/// physical value the old patch actually produced. `None` means this key's
/// table did not move, so the value is already current.
///
/// Documents without `automation_normalization: 2` run through this once per
/// lane point, in the interpreter. The engine API itself only knows the
/// current table, so macros and normalized agent writes always get the full
/// domain.
pub fn recalibrate_legacy_normalized_param(name: &str, normalized: f64) -> Option<f64> {
    if !normalized.is_finite() {
        return None;
    }
    let value = normalized.clamp(0.0, 1.0);
    let recalibrated = match name {
        "osc.waveform" | "waveform" | "osc.a.waveform" | "osc.b.waveform" | "osc.c.waveform"
        | "lfo.waveform" => value * 4.0 / 6.0,
        "filter.model" | "filter2.model" => value * 2.0 / 11.0,
        name if name.starts_with("mod.") && name.ends_with(".source") => value * 25.0 / 28.0,
        name if name.starts_with("mod.") && name.ends_with(".destination") => value * 14.0 / 34.0,
        name if name.starts_with("fxmod.") && name.ends_with(".source") => value * 25.0 / 28.0,
        // These four families had no normalized arm: the old path passed the
        // lane value straight to `set_param`. Reproduce that physical value.
        "sub.waveform" | "sub.shape" => value / 5.0,
        "osc.a.detune" | "osc.b.detune" | "osc.c.detune" => value / 100.0,
        "osc.b.unison" | "osc.c.unison" => 0.0,
        name if extra_lfo_param(name).is_some_and(|(_, field)| field == "waveform") => {
            value * 4.0 / 6.0
        }
        name if parse_fx_param_name(name).is_some_and(|(_, field)| field == "type") => {
            value / (SynthFxType::ALL.len() - 1) as f64
        }
        _ => return None,
    };
    Some(recalibrated)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomWavetable {
    samples: Arc<Vec<f64>>,
    frame_size: usize,
    frame_count: usize,
    /// Imported tables use the 16-tap reader and its start-phase alignment.
    #[serde(default, alias = "serum_readout")] // allow-re-name: patch compat
    imported_readout: bool,
    #[serde(skip)]
    mips: Arc<OnceLock<MipChain>>,
}

#[derive(Debug)]
struct MipChain {
    /// Level `l` holds `frame_count` blocks of `frame_size >> l` samples.
    levels: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct MipState {
    level: usize,
    previous: usize,
    blend: u8,
    /// Cutoff bin the default-bank readout just used.
    bin: usize,
    /// Cutoff bin the fade out of is reading. The level and the corner are one
    /// readout choice, so a change to either fades the whole choice.
    previous_bin: usize,
    /// Target level and cutoff bin, with the pitch step they were taken at. A
    /// voice whose pitch does not move keeps them, which holds `log2` and the
    /// bin split out of the sample loop.
    target: usize,
    dt: f64,
}

const MIP_LEVEL_LIMIT: usize = 12;
const MIP_CROSSFADE_SAMPLES: u8 = 64;
const POLYPHASE_ROWS: usize = 512;
const POLYPHASE_TAPS: usize = 16;
const POLYPHASE_CENTER: usize = 7;
/// Measured imported readout delay at C1..C5, in output samples.
const IMPORTED_READOUT_DELAY: f64 = 10.75;

/// Default-bank readout splits one level of oversampling into this many bins. A
/// bin's corner is the audio Nyquist at the bin's centre, so it stays within
/// +-4 % of it: never far enough below to read dark, never far enough above to
/// fold a harmonic of consequence.
const READOUT_BINS: usize = 8;
/// Row resolution of the default-bank bank; 32 rows sit 90 dB below the fold.
const READOUT_ROWS: usize = 32;
/// Tap tiers, shortest first. The fold content keeps its amplitude as the table
/// halves every level, so the short tables pay for the longer kernel.
const READOUT_SHORT_TAPS: usize = 16;
const READOUT_MID_TAPS: usize = 32;
const READOUT_LONG_TAPS: usize = 48;
const READOUT_TAPS: [usize; 3] = [READOUT_SHORT_TAPS, READOUT_MID_TAPS, READOUT_LONG_TAPS];
/// Tap tier of each mip level.
const READOUT_TIER: [usize; MIP_LEVEL_LIMIT] = [0, 0, 0, 0, 0, 1, 2, 2, 2, 2, 2, 2];
const READOUT_BETA: f64 = 4.0;

/// Fractional-delay bank for one tap count and cutoff, `READOUT_ROWS + 1` rows.
/// The extra row is the delay one sample later, so the last fraction step keeps
/// interpolating between neighbours instead of wrapping back to delay zero.
struct ReadoutTable {
    taps: usize,
    center: usize,
    rows: Box<[f64]>,
}

impl ReadoutTable {
    fn new(taps: usize, cutoff: f64) -> Self {
        let center = taps / 2 - 1;
        let mut rows = vec![0.0; (READOUT_ROWS + 1) * taps];
        for row in 0..=READOUT_ROWS {
            let delay = center as f64 + row as f64 / READOUT_ROWS as f64;
            let coefficients = &mut rows[row * taps..(row + 1) * taps];
            let mut sum = 0.0;
            for (index, coefficient) in coefficients.iter_mut().enumerate() {
                *coefficient = 2.0 * cutoff * sinc(2.0 * cutoff * (index as f64 - delay));
                *coefficient *= kaiser(index, taps, READOUT_BETA);
                sum += *coefficient;
            }
            for coefficient in coefficients.iter_mut() {
                *coefficient /= sum;
            }
        }
        Self {
            taps,
            center,
            rows: rows.into_boxed_slice(),
        }
    }

    #[inline(always)]
    fn read(&self, data: &[f64], level_len: usize, phase: f64) -> f64 {
        self.read_frames(data, None, level_len, phase, 0.0)
    }

    /// Read one position against the position frame(s) it spans. `second` is the
    /// next frame when the table position sits between two of them; both frames
    /// read the same fractional delay, so the blended coefficient row is built
    /// once here instead of once per frame.
    #[inline(always)]
    fn read_frames(
        &self,
        first: &[f64],
        second: Option<&[f64]>,
        level_len: usize,
        phase: f64,
        frame_mix: f64,
    ) -> f64 {
        // One instantiation per tap tier: the window size is a constant, so the
        // coefficient blend and the dot product both unroll.
        match self.taps {
            READOUT_SHORT_TAPS => self
                .read_frames_taps::<READOUT_SHORT_TAPS>(first, second, level_len, phase, frame_mix),
            READOUT_MID_TAPS => self
                .read_frames_taps::<READOUT_MID_TAPS>(first, second, level_len, phase, frame_mix),
            _ => self
                .read_frames_taps::<READOUT_LONG_TAPS>(first, second, level_len, phase, frame_mix),
        }
    }

    #[inline(always)]
    fn read_frames_taps<const TAPS: usize>(
        &self,
        first: &[f64],
        second: Option<&[f64]>,
        level_len: usize,
        phase: f64,
        frame_mix: f64,
    ) -> f64 {
        debug_assert_eq!(self.taps, TAPS);
        let phase_position = wrap_unit(phase) * level_len as f64;
        // Non-negative, so truncating the cast equals `floor` without its cost.
        let mut first_sample = phase_position as usize;
        if first_sample >= level_len {
            first_sample -= level_len;
        }
        let fraction = phase_position - first_sample as f64;
        let row = fraction * READOUT_ROWS as f64;
        let row_index = row as usize;
        let row_mix = row - row_index as f64;
        let first_row = &self.rows[row_index * TAPS..][..TAPS];
        let second_row = &self.rows[(row_index + 1) * TAPS..][..TAPS];
        let mut coefficients = [0.0; TAPS];
        for (index, coefficient) in coefficients.iter_mut().enumerate() {
            *coefficient = first_row[index] + (second_row[index] - first_row[index]) * row_mix;
        }
        let start = first_sample as isize - self.center as isize;
        let current = dot_window(&coefficients, start, level_len, first);
        match second {
            Some(second) => {
                let next = dot_window(&coefficients, start, level_len, second);
                current + (next - current) * frame_mix
            }
            None => current,
        }
    }
}

/// Dot a blended coefficient row against one block, wrapping the tap window.
#[inline(always)]
fn dot_window<const TAPS: usize>(
    coefficients: &[f64; TAPS],
    start: isize,
    level_len: usize,
    data: &[f64],
) -> f64 {
    if start >= 0 && start as usize + TAPS <= level_len {
        let data = &data[start as usize..start as usize + TAPS];
        let mut sum = [0.0; 4];
        let mut index = 0;
        while index + 4 <= TAPS {
            sum[0] += coefficients[index] * data[index];
            sum[1] += coefficients[index + 1] * data[index + 1];
            sum[2] += coefficients[index + 2] * data[index + 2];
            sum[3] += coefficients[index + 3] * data[index + 3];
            index += 4;
        }
        while index < TAPS {
            sum[index & 3] += coefficients[index] * data[index];
            index += 1;
        }
        return (sum[0] + sum[1]) + (sum[2] + sum[3]);
    }
    let mut sum = [0.0; 4];
    let len = level_len as isize;
    // The window is [start, start + TAPS): one wrap covers it when a level is at
    // least one window wide, narrower levels walk the modulo.
    if level_len >= TAPS {
        let mut offset = 0;
        let mut start = start;
        if start < 0 {
            let run = (-start) as usize;
            let from = (len + start) as usize;
            accumulate(&mut sum, &coefficients[..run], &data[from..from + run]);
            offset = run;
            start = 0;
        }
        let remaining = TAPS - offset;
        if start + remaining as isize > len {
            let head = (len - start) as usize;
            let from = start as usize;
            accumulate(
                &mut sum,
                &coefficients[offset..offset + head],
                &data[from..from + head],
            );
            accumulate(
                &mut sum,
                &coefficients[offset + head..],
                &data[..remaining - head],
            );
        } else {
            let from = start as usize;
            accumulate(
                &mut sum,
                &coefficients[offset..],
                &data[from..from + remaining],
            );
        }
    } else {
        let mut sample = start.rem_euclid(len) as usize;
        for (index, &coefficient) in coefficients.iter().enumerate() {
            sum[index & 3] += coefficient * data[sample];
            sample += 1;
            if sample == level_len {
                sample = 0;
            }
        }
    }
    (sum[0] + sum[1]) + (sum[2] + sum[3])
}

/// Four independent accumulator chains: a single chain would stall on the
/// floating-point add latency for every tap.
#[inline(always)]
fn accumulate(sum: &mut [f64; 4], coefficients: &[f64], data: &[f64]) {
    let mut index = 0;
    while index + 4 <= coefficients.len() {
        sum[0] += coefficients[index] * data[index];
        sum[1] += coefficients[index + 1] * data[index + 1];
        sum[2] += coefficients[index + 2] * data[index + 2];
        sum[3] += coefficients[index + 3] * data[index + 3];
        index += 4;
    }
    while index < coefficients.len() {
        sum[index & 3] += coefficients[index] * data[index];
        index += 1;
    }
}

fn readout_tables() -> &'static [[ReadoutTable; READOUT_BINS]; READOUT_TAPS.len()] {
    static TABLES: OnceLock<[[ReadoutTable; READOUT_BINS]; READOUT_TAPS.len()]> = OnceLock::new();
    TABLES.get_or_init(|| {
        std::array::from_fn(|tier| {
            let taps = READOUT_TAPS[tier];
            std::array::from_fn(|bin| {
                let cutoff = 0.5 / 2f64.powf((bin as f64 + 0.5) / READOUT_BINS as f64);
                ReadoutTable::new(taps, cutoff)
            })
        })
    })
}

/// Cutoff bin for a level position (`log2` of table samples per output sample).
#[inline(always)]
fn readout_bin(level_position: f64) -> usize {
    let fraction = level_position - level_position.floor();
    ((fraction * READOUT_BINS as f64) as usize).min(READOUT_BINS - 1)
}

#[inline(always)]
fn readout_table(level: usize, bin: usize) -> &'static ReadoutTable {
    &readout_tables()[READOUT_TIER[level.min(MIP_LEVEL_LIMIT - 1)]][bin]
}

/// 16-tap fractional-delay bank, 512 rows with linear interpolation between rows.
/// -6 dB at 0.73 of the table Nyquist (measured first cut); Kaiser beta 3 keeps the transition monotone.
fn polyphase_kernel() -> &'static [[f64; POLYPHASE_TAPS]; POLYPHASE_ROWS] {
    static KERNEL: OnceLock<[[f64; POLYPHASE_TAPS]; POLYPHASE_ROWS]> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let cutoff = 0.365;
        let beta = 3.0;
        let mut rows = [[0.0; POLYPHASE_TAPS]; POLYPHASE_ROWS];
        for (row, coefficients) in rows.iter_mut().enumerate() {
            let delay = POLYPHASE_CENTER as f64 + row as f64 / POLYPHASE_ROWS as f64;
            let mut sum = 0.0;
            for (index, coefficient) in coefficients.iter_mut().enumerate() {
                *coefficient = 2.0 * cutoff * sinc(2.0 * cutoff * (index as f64 - delay));
                *coefficient *= kaiser(index, POLYPHASE_TAPS, beta);
                sum += *coefficient;
            }
            for coefficient in coefficients.iter_mut() {
                *coefficient /= sum;
            }
        }
        rows
    })
}

/// One half-band decimation step for a single periodic block.
fn decimate_half_band(previous: &[f64], previous_len: usize) -> Vec<f64> {
    let out_len = previous_len / 2;
    let kernel = half_band_kernel();
    let mut data = vec![0.0; out_len];
    for (sample, value) in data.iter_mut().enumerate() {
        let center = 2 * sample + HALF_BAND_CENTER;
        let mut sum = 0.0;
        for (tap_index, &tap) in kernel.iter().enumerate() {
            let index =
                (center as isize - tap_index as isize).rem_euclid(previous_len as isize) as usize;
            sum += tap * previous[index];
        }
        *value = sum;
    }
    data
}

/// 16-tap polyphase read of one periodic block at `phase` in `[0, 1)`.
fn read_polyphase(data: &[f64], level_len: usize, phase: f64) -> f64 {
    let phase_position = wrap_unit(phase) * level_len as f64;
    // Non-negative, so truncating the cast equals `floor` without its cost.
    let mut first_sample = phase_position as usize;
    if first_sample >= level_len {
        first_sample -= level_len;
    }
    let fraction = phase_position - first_sample as f64;
    let row = fraction * POLYPHASE_ROWS as f64;
    let row_index = row as usize;
    let row_mix = row - row_index as f64;
    let first_row = &polyphase_kernel()[row_index & (POLYPHASE_ROWS - 1)];
    let second_row = &polyphase_kernel()[(row_index + 1) & (POLYPHASE_ROWS - 1)];
    let mut sum = 0.0;
    // The window is [-CENTER, level_len + TAPS - CENTER): one wrap covers it when
    // a level is at least one window wide, narrower levels keep the general modulo.
    if level_len >= POLYPHASE_TAPS {
        let base = first_sample as isize - POLYPHASE_CENTER as isize;
        for index in 0..POLYPHASE_TAPS {
            let coefficient = first_row[index] + (second_row[index] - first_row[index]) * row_mix;
            let mut sample = base + index as isize;
            if sample < 0 {
                sample += level_len as isize;
            } else if sample >= level_len as isize {
                sample -= level_len as isize;
            }
            sum += coefficient * data[sample as usize];
        }
    } else {
        for index in 0..POLYPHASE_TAPS {
            let coefficient = first_row[index] + (second_row[index] - first_row[index]) * row_mix;
            let sample = ((first_sample + index) as isize - POLYPHASE_CENTER as isize)
                .rem_euclid(level_len as isize) as usize;
            sum += coefficient * data[sample];
        }
    }
    sum
}

impl MipChain {
    /// Build the full pyramid from one periodic table by repeated decimation.
    fn single_table(table: Vec<f64>) -> Self {
        let mut levels = vec![table];
        for _ in 1..MIP_LEVEL_LIMIT {
            let previous = levels.last().expect("the base level always exists");
            if previous.len() < 2 {
                break;
            }
            let next = decimate_half_band(previous, previous.len());
            levels.push(next);
        }
        Self { levels }
    }
}

impl CustomWavetable {
    /// Build a table from frame-major samples and prepare its mips up front.
    fn from_frames(samples: Vec<f64>, frame_size: usize, imported_readout: bool) -> Self {
        let frame_count = samples.len() / frame_size;
        let table = Self {
            samples: Arc::new(samples),
            frame_size,
            frame_count,
            imported_readout,
            mips: Default::default(),
        };
        table.prepare_mips();
        table
    }

    fn prepare_mips(&self) -> &MipChain {
        self.mips.get_or_init(|| {
            let max_level = if self.frame_size.is_power_of_two() {
                (self.frame_size.trailing_zeros() as usize).min(MIP_LEVEL_LIMIT - 1)
            } else {
                0
            };
            let mut levels = Vec::with_capacity(max_level + 1);
            levels.push(self.samples.as_ref().clone());
            for level in 1..=max_level {
                let previous_len = self.frame_size >> (level - 1);
                let out_len = previous_len / 2;
                if out_len == 0 {
                    break;
                }
                let mut data = vec![0.0; self.frame_count * out_len];
                for frame in 0..self.frame_count {
                    let previous =
                        &levels[level - 1][frame * previous_len..(frame + 1) * previous_len];
                    let decimated = decimate_half_band(previous, previous_len);
                    data[frame * out_len..(frame + 1) * out_len].copy_from_slice(&decimated);
                }
                levels.push(data);
            }
            MipChain { levels }
        })
    }

    /// `log2` of table samples per output sample; NaN and non-positive rates
    /// report the base level.
    fn level_position(&self, output_dt: f64) -> f64 {
        if output_dt.is_nan() || output_dt <= 0.0 {
            return 0.0;
        }
        (self.frame_size as f64 * output_dt).log2()
    }

    #[cfg(test)]
    fn level_for(&self, output_dt: f64) -> usize {
        let mips = self
            .mips
            .get()
            .expect("imported mip levels must be prepared before audio");
        let max_level = mips.levels.len().saturating_sub(1);
        let level = self.level_position(output_dt).floor() as i32;
        level.clamp(0, max_level as i32) as usize
    }

    fn sample_mip(
        &self,
        mip: &mut MipState,
        phase: f64,
        position: f64,
        output_dt: f64,
        phase_delay: f64,
    ) -> f64 {
        let mips = self
            .mips
            .get()
            .expect("imported mip levels must be prepared before audio");
        // The level position is a function of the pitch step alone, so a voice
        // whose pitch does not move keeps the last one instead of paying the
        // `log2` every sample.
        if mip.dt != output_dt {
            // A voice takes its first corner without a fade; later corner moves
            // fade like level moves (a named table has one corner, so none).
            let fresh = mip.dt == 0.0;
            mip.dt = output_dt;
            let level_position = self.level_position(output_dt);
            let max_level = mips.levels.len().saturating_sub(1) as i32;
            mip.target = (level_position.floor() as i32).clamp(0, max_level) as usize;
            // A named table reads one kernel at every level, so it has no
            // corner to choose and no corner to fade.
            let bin = if self.imported_readout {
                0
            } else {
                readout_bin(level_position)
            };
            if mip.target != mip.level || (!fresh && bin != mip.bin) {
                mip.previous = mip.level;
                mip.previous_bin = if fresh { bin } else { mip.bin };
                mip.level = mip.target;
                mip.bin = bin;
                mip.blend = MIP_CROSSFADE_SAMPLES;
            } else {
                mip.bin = bin;
            }
        }
        let phase = if self.imported_readout {
            phase - phase_delay
        } else {
            phase
        };
        let current = self.sample_level(mips, mip.level, phase, position, mip.bin);
        if mip.blend == 0 {
            return current;
        }
        let old = self.sample_level(mips, mip.previous, phase, position, mip.previous_bin);
        let weight = f64::from(mip.blend) / f64::from(MIP_CROSSFADE_SAMPLES);
        mip.blend -= 1;
        current + (old - current) * weight
    }

    fn sample_level(
        &self,
        mips: &MipChain,
        level: usize,
        phase: f64,
        position: f64,
        bin: usize,
    ) -> f64 {
        let level = level.min(mips.levels.len() - 1);
        let level_len = self.frame_size >> level;
        let frame_position =
            position.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max) * (self.frame_count - 1) as f64;
        let first_frame = frame_position as usize;
        let second_frame = (first_frame + 1).min(self.frame_count - 1);
        let frame_mix = frame_position - first_frame as f64;
        let data = &mips.levels[level];
        let first_block = &data[first_frame * level_len..];
        let second_block = if frame_mix == 0.0 {
            None
        } else {
            Some(&data[second_frame * level_len..])
        };
        if self.imported_readout {
            let first = read_polyphase(first_block, level_len, phase);
            return match second_block {
                Some(second) => {
                    let second = read_polyphase(second, level_len, phase);
                    first + (second - first) * frame_mix
                }
                None => first,
            };
        }
        // Both position frames read the same fractional delay, so the blended
        // coefficient row is built once here instead of once per frame.
        readout_table(level, bin).read_frames(
            first_block,
            second_block,
            level_len,
            phase,
            frame_mix,
        )
    }

    pub fn load(path: &str) -> Result<Self, String> {
        Self::load_with_readout(path, false)
    }

    /// Load an imported wavetable with the aligned 16-tap readout.
    pub fn load_imported(path: &str) -> Result<Self, String> {
        Self::load_with_readout(path, true)
    }

    fn load_with_readout(path: &str, imported_readout: bool) -> Result<Self, String> {
        let mut reader = oc_audio::wav::open_reader(std::path::Path::new(path))
            .map_err(|error| format!("failed to open WAV: {error}"))?;
        let spec = reader.spec();
        let channels = spec.channels.max(1) as usize;
        let mut samples = Vec::new();
        match spec.sample_format {
            hound::SampleFormat::Float => {
                for (index, sample) in reader.samples::<f32>().enumerate() {
                    let sample =
                        sample.map_err(|error| format!("invalid float sample: {error}"))?;
                    // Validate all channels, including those not selected for the mono table.
                    // Infinite input can otherwise normalize into NaNs and enter the voice DSP.
                    if !sample.is_finite() {
                        return Err(format!(
                            "non-finite wavetable sample at interleaved index {index}"
                        ));
                    }
                    if index % channels == 0 {
                        if samples.len() >= MAX_CUSTOM_WAVETABLE_SAMPLES {
                            return Err("wavetable exceeds 256 frames of 2048 samples".to_string());
                        }
                        samples.push(sample as f64);
                    }
                }
            }
            hound::SampleFormat::Int => {
                let scale = ((1_i64 << spec.bits_per_sample.saturating_sub(1)) - 1).max(1) as f64;
                for (index, sample) in reader.samples::<i32>().enumerate() {
                    let sample =
                        sample.map_err(|error| format!("invalid integer sample: {error}"))?;
                    if index % channels == 0 {
                        if samples.len() >= MAX_CUSTOM_WAVETABLE_SAMPLES {
                            return Err("wavetable exceeds 256 frames of 2048 samples".to_string());
                        }
                        samples.push(sample as f64 / scale);
                    }
                }
            }
        }
        if samples.len() < 32 {
            return Err("wavetable needs at least 32 mono samples".to_string());
        }
        let (frame_size, frame_count) =
            if samples.len() >= WAVETABLE_SIZE && samples.len() % WAVETABLE_SIZE == 0 {
                (WAVETABLE_SIZE, samples.len() / WAVETABLE_SIZE)
            } else {
                (samples.len(), 1)
            };
        // Peak-normalise and clamp only: a frame's mean is part of its waveform.
        let peak = samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        if peak <= f64::EPSILON {
            return Err("wavetable contains only silence".to_string());
        }
        for sample in &mut samples {
            *sample = (*sample / peak).clamp(-1.0, 1.0);
        }
        Ok(Self {
            samples: Arc::new(samples),
            frame_size,
            frame_count,
            imported_readout,
            mips: Default::default(),
        })
    }

    #[cfg(test)]
    fn sample(&self, phase: f64, position: f64) -> f64 {
        self.sample_from(&self.samples, phase, position)
    }

    #[cfg(test)]
    fn sample_from(&self, samples: &[f64], phase: f64, position: f64) -> f64 {
        let frame_position =
            position.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max) * (self.frame_count - 1) as f64;
        let first_frame = frame_position.floor() as usize;
        let second_frame = (first_frame + 1).min(self.frame_count - 1);
        let frame_mix = frame_position.fract();
        let phase_position = phase.rem_euclid(1.0) * self.frame_size as f64;
        let first_sample = phase_position.floor() as usize % self.frame_size;
        let second_sample = (first_sample + 1) % self.frame_size;
        let sample_mix = phase_position.fract();
        let at = |frame: usize, sample: usize| samples[frame * self.frame_size + sample];
        let interpolate = |frame: usize| {
            at(frame, first_sample)
                + (at(frame, second_sample) - at(frame, first_sample)) * sample_mix
        };
        let a = interpolate(first_frame);
        a + (interpolate(second_frame) - a) * frame_mix
    }
}

fn waveform_from_value(value: f64) -> Waveform {
    match value.round() as i32 {
        0 => Waveform::Sine,
        2 => Waveform::Square,
        3 => Waveform::Triangle,
        4 => Waveform::Wavetable,
        5 => Waveform::Granular,
        6 => Waveform::Spectral,
        _ => Waveform::Saw,
    }
}

fn lfo_curve_index(name: &str) -> Option<usize> {
    let index: usize = name.strip_prefix("lfo.curve.")?.parse().ok()?;
    (index < LFO_CURVE_POINTS).then_some(index)
}

/// `lfo.shape.x` / `lfoN.shape.x` -> (LFO index, array field).
fn lfo_shape_field(name: &str) -> Option<(usize, &str)> {
    let (index, field) = if let Some(rest) = name.strip_prefix("lfo.") {
        (0, rest.strip_prefix("shape.")?)
    } else {
        let rest = name.strip_prefix("lfo")?;
        let (slot, rest) = rest.split_once('.')?;
        let one_based: usize = slot.parse().ok()?;
        if !(2..=10).contains(&one_based) {
            return None;
        }
        (one_based - 1, rest.strip_prefix("shape.")?)
    };
    matches!(field, "x" | "y" | "curve").then_some((index, field))
}

/// LFO path fields hold arrays and are applied by the patch document loader.
pub fn is_array_param(name: &str) -> bool {
    lfo_shape_field(name).is_some()
}

/// Whether a source is already centred. Imported LFOs are unipolar, so the
/// `bipolar` flag re-centres them; only the step sequencer and keytrack are centred.
fn mod_source_is_bipolar(source: ModSource) -> bool {
    matches!(
        source,
        ModSource::Step | ModSource::Keytrack | ModSource::PitchBend | ModSource::Timbre
    )
}

/// Numeric `mod.*.source` id of a source: the inverse of `mod_source_from_value`.
pub fn mod_source_value(source: ModSource) -> u8 {
    MOD_SOURCE_TABLE
        .iter()
        .position(|entry| *entry == source)
        .unwrap_or(0) as u8
}

/// Modulation source for macro 1..=8, indexed from zero.
pub fn macro_mod_source(index: usize) -> Option<ModSource> {
    Some(match index {
        0 => ModSource::Macro1,
        1 => ModSource::Macro2,
        2 => ModSource::Macro3,
        3 => ModSource::Macro4,
        4 => ModSource::Macro5,
        5 => ModSource::Macro6,
        6 => ModSource::Macro7,
        7 => ModSource::Macro8,
        _ => return None,
    })
}

fn mod_source_from_value(value: f64) -> ModSource {
    match value.round() as i32 {
        1 => ModSource::FilterEnvelope,
        2 => ModSource::Velocity,
        3 => ModSource::Macro1,
        4 => ModSource::Macro2,
        5 => ModSource::Macro3,
        6 => ModSource::Macro4,
        7 => ModSource::Keytrack,
        8 => ModSource::Aftertouch,
        9 => ModSource::ModWheel,
        10 => ModSource::Step,
        11 => ModSource::Lfo2,
        12 => ModSource::Lfo3,
        13 => ModSource::Lfo4,
        14 => ModSource::Lfo5,
        15 => ModSource::Lfo6,
        16 => ModSource::Lfo7,
        17 => ModSource::Lfo8,
        18 => ModSource::Lfo9,
        19 => ModSource::Lfo10,
        20 => ModSource::Env3,
        21 => ModSource::Env4,
        22 => ModSource::Macro5,
        23 => ModSource::Macro6,
        24 => ModSource::Macro7,
        25 => ModSource::Macro8,
        26 => ModSource::AmpEnvelope,
        27 => ModSource::PitchBend,
        28 => ModSource::Timbre,
        _ => ModSource::Lfo,
    }
}

/// Project-parameter id of a destination. `None` for the destinations the
/// numeric ABI has no id for yet (see the unmapped entries in the catalog).
fn mod_destination_value(destination: ModDestination) -> Option<u8> {
    let value = match destination {
        ModDestination::FilterCutoff => 0,
        ModDestination::Pitch => 1,
        ModDestination::OscillatorMix => 2,
        ModDestination::Resonance => 3,
        ModDestination::StereoWidth => 4,
        ModDestination::PulseWidthA => 5,
        ModDestination::RingModLevel => 6,
        ModDestination::WavetablePositionA => 7,
        ModDestination::OscillatorBTuning => 8,
        ModDestination::FilterDrive => 9,
        ModDestination::OscillatorCLevel => 10,
        ModDestination::WavetablePositionC => 11,
        ModDestination::Filter2Cutoff => 12,
        ModDestination::Filter2Resonance => 13,
        ModDestination::Filter2Drive => 14,
        ModDestination::VoiceAmp => 15,
        ModDestination::WarpAmountA => 16,
        ModDestination::WarpAmountB => 17,
        ModDestination::WarpAmountC => 18,
        ModDestination::Warp2AmountA => 19,
        ModDestination::Warp2AmountB => 20,
        ModDestination::Warp2AmountC => 21,
        ModDestination::WavetablePositionB => 22,
        ModDestination::SourceDensityA => 23,
        ModDestination::SourceDensityB => 24,
        ModDestination::SourceDensityC => 25,
        ModDestination::SourcePositionA => 26,
        ModDestination::SourcePositionB => 27,
        ModDestination::SourcePositionC => 28,
        ModDestination::SourceMorphA => 29,
        ModDestination::SourceMorphB => 30,
        ModDestination::SourceMorphC => 31,
        ModDestination::SourceBendA => 32,
        ModDestination::SourceBendB => 33,
        ModDestination::SourceBendC => 34,
        ModDestination::OscillatorALevel
        | ModDestination::NoiseLevel
        | ModDestination::SubLevel
        | ModDestination::FilterWet
        | ModDestination::Filter2Wet => return None,
    };
    Some(value)
}

/// Destination for a numeric `mod.N.destination` id; unknown ids fall back to
/// the filter cutoff, matching the project ABI's original behaviour.
pub fn mod_destination_from_value(value: f64) -> ModDestination {
    match value.round() as i32 {
        1 => ModDestination::Pitch,
        2 => ModDestination::OscillatorMix,
        3 => ModDestination::Resonance,
        4 => ModDestination::StereoWidth,
        5 => ModDestination::PulseWidthA,
        6 => ModDestination::RingModLevel,
        7 => ModDestination::WavetablePositionA,
        8 => ModDestination::OscillatorBTuning,
        9 => ModDestination::FilterDrive,
        10 => ModDestination::OscillatorCLevel,
        11 => ModDestination::WavetablePositionC,
        12 => ModDestination::Filter2Cutoff,
        13 => ModDestination::Filter2Resonance,
        14 => ModDestination::Filter2Drive,
        15 => ModDestination::VoiceAmp,
        16 => ModDestination::WarpAmountA,
        22 => ModDestination::WavetablePositionB,
        17 => ModDestination::WarpAmountB,
        18 => ModDestination::WarpAmountC,
        19 => ModDestination::Warp2AmountA,
        20 => ModDestination::Warp2AmountB,
        21 => ModDestination::Warp2AmountC,
        // Source-oscillator controls, appended after the wavetable range.
        23 => ModDestination::SourceDensityA,
        24 => ModDestination::SourceDensityB,
        25 => ModDestination::SourceDensityC,
        26 => ModDestination::SourcePositionA,
        27 => ModDestination::SourcePositionB,
        28 => ModDestination::SourcePositionC,
        29 => ModDestination::SourceMorphA,
        30 => ModDestination::SourceMorphB,
        31 => ModDestination::SourceMorphC,
        32 => ModDestination::SourceBendA,
        33 => ModDestination::SourceBendB,
        34 => ModDestination::SourceBendC,
        _ => ModDestination::FilterCutoff,
    }
}

fn indexed_param(name: &str, prefix: &str, count: usize) -> Option<usize> {
    let one_based: usize = name.strip_prefix(prefix)?.parse().ok()?;
    (1..=count).contains(&one_based).then_some(one_based - 1)
}

/// Parse `lfoN.field` for LFO 2..LFO 10 into `extra_lfos` index 0..8.
/// `lfo.rate` (LFO 1) deliberately does not match because its index is empty.
fn extra_lfo_param(name: &str) -> Option<(usize, &str)> {
    let suffix = name.strip_prefix("lfo")?;
    let (index, field) = suffix.split_once('.')?;
    let one_based: usize = index.parse().ok()?;
    (2..=10)
        .contains(&one_based)
        .then_some((one_based - 2, field))
}

fn route_param(name: &str) -> Option<(usize, &str)> {
    let suffix = name.strip_prefix("mod.")?;
    let (index, field) = suffix.split_once('.')?;
    let one_based: usize = index.parse().ok()?;
    (1..=MAX_MOD_ROUTES)
        .contains(&one_based)
        .then_some((one_based - 1, field))
}

/// Parse `fxmod.N.field` for the FX modulation matrix. `N` is 1-based to match
/// `mod.N`/`fx.N`; `fx_index` itself stays zero-based inside the patch.
fn fx_mod_route_param(name: &str) -> Option<(usize, &str)> {
    let suffix = name.strip_prefix("fxmod.")?;
    let (index, field) = suffix.split_once('.')?;
    let one_based: usize = index.parse().ok()?;
    (1..=MAX_FX_MOD_ROUTES)
        .contains(&one_based)
        .then_some((one_based - 1, field))
}

fn is_envelope_duration_param(name: &str) -> bool {
    matches!(
        name,
        "attack"
            | "decay"
            | "release"
            | "amp.attack"
            | "amp.decay"
            | "amp.release"
            | "filter_env.attack"
            | "filter_env.decay"
            | "filter_env.release"
            | "env3.attack"
            | "env3.decay"
            | "env3.release"
            | "env4.attack"
            | "env4.decay"
            | "env4.release"
    )
}

#[derive(Debug, Clone)]
struct Voice {
    channel: u8,
    note: u8,
    pitch_note: f64,
    key_down: bool,
    velocity: f64,
    /// Unison phases 0..6 belong to oscillator A, 7 to B, 8 to the sub
    /// oscillator, and 9 to C (C follows B's single-phase path).
    phases: [f64; PHASE_COUNT],
    /// Extra B/C unison phases; slot 0 lives in `phases[PHASE_B]`/`phases[PHASE_C]`.
    phases_b: [f64; MAX_UNISON_VOICES - 1],
    phases_c: [f64; MAX_UNISON_VOICES - 1],
    /// sync window phase per oscillator and unison voice, advanced by the
    /// unwarped increment; a wrap resets the oscillator phase.
    warp_window: [[f64; MAX_UNISON_VOICES]; 3],
    /// Per-lane mip level and 64-sample crossfade state.
    mip_a: [MipState; MAX_UNISON_VOICES],
    mip_b: [MipState; MAX_UNISON_VOICES],
    mip_c: [MipState; MAX_UNISON_VOICES],
    mip_sub: sub_osc::SubMipState,
    amplitude_level: f64,
    amplitude_stage: EnvStage,
    amplitude_phase: f64,
    amplitude_release_start: f64,
    /// Per-note multipliers on the patch's decay and release times.
    amplitude_decay_scale: f64,
    amplitude_release_scale: f64,
    /// Render frame the note started on, for the short-note release decision.
    note_on_frame: u64,
    filter_level: f64,
    filter_stage: EnvStage,
    filter_phase: f64,
    filter_release_start: f64,
    /// Free-running ADSR sources 3 and 4; they only feed the mod matrix.
    env3_level: f64,
    env3_stage: EnvStage,
    env3_phase: f64,
    env3_release_start: f64,
    env4_level: f64,
    env4_stage: EnvStage,
    env4_phase: f64,
    env4_release_start: f64,
    /// Filter 1 stereo state. Kept separate from filter 2's state.
    filter: [StateVariableFilter; 2],
    /// Filter 2 stereo state (left/right).
    filter2: [StateVariableFilter; 2],
    /// Ladder states stay allocated so switching models never resets the SVF.
    filter_ladder: [LadderFilter; 2],
    filter2_ladder: [LadderFilter; 2],
    filter_moog: [MoogLadder; 2],
    filter2_moog: [MoogLadder; 2],
    noise_state: u64,
    /// Two cascaded one-pole high-passes remove the noise oscillator's rumble.
    noise_hp: [f64; 2],
    /// Coefficients for both filters; refreshed at control rate.
    filter_coefficients: FilterCoefficients,
    filter2_coefficients: FilterCoefficients,
    filter_coefficient_age: u8,
    filter_coefficients_valid: bool,
    filter_sample_rate: u32,
    pressure: f64,
    /// Channel pressure snapshot; poly pressure stays in `pressure`.
    channel_pressure: f64,
    /// Normalized channel pitch bend in [-1, 1].
    pitch_bend: f64,
    /// CC74 timbre/slide in [0, 1].
    timbre: f64,
    expression_tuning: f64,
    expression_brightness: f64,
    frequency_hz: f64,
    frequency_valid: bool,
    /// One-pole state per compiled modulation route, in route order.
    route_slew: [f64; MAX_MOD_ROUTES],
    /// Source engine per oscillator slot. Boxed so wavetable voices stay small;
    /// the engine is built while a voice sits in the idle set and only re-seeded
    /// at note-on.
    source_voices: [Option<Box<SourceVoice>>; 3],
    age: u64,
}

#[derive(Debug, Clone)]
// The spectral variant's FFT buffers are large; boxing keeps `Voice` small and
// lets the idle set own the allocations instead of note-on.
#[allow(clippy::large_enum_variant)]
enum SourceVoice {
    Granular(granular::GranularVoice),
    Spectral(spectral::SpectralVoice),
}

/// Deterministic per-note seed for a granular source engine.
fn source_seed(patch: &SynthPatch, waveform: Waveform, age: u64, slot: usize, note: u8) -> u64 {
    let patch_seed = match waveform {
        Waveform::Granular => patch.granular[slot]
            .as_ref()
            .map_or(0, |settings| settings.seed),
        Waveform::Spectral => patch.spectral[slot]
            .as_ref()
            .map_or(0, |settings| settings.seed),
        _ => 0,
    };
    patch_seed ^ age.rotate_left(17) ^ ((slot as u64 + 1) << 48) ^ ((note as u64) << 8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvStage {
    Attack,
    Decay,
    Sustain,
    Release,
}

impl Voice {
    fn new(channel: u8, note: u8, velocity: u8, age: u64) -> Self {
        let voice = Self {
            channel,
            note,
            pitch_note: note as f64,
            key_down: true,
            velocity: velocity as f64 / 127.0,
            phases: [0.0; PHASE_COUNT],
            phases_b: [0.0; MAX_UNISON_VOICES - 1],
            phases_c: [0.0; MAX_UNISON_VOICES - 1],
            warp_window: [[0.0; MAX_UNISON_VOICES]; 3],
            mip_a: [MipState::default(); MAX_UNISON_VOICES],
            mip_b: [MipState::default(); MAX_UNISON_VOICES],
            mip_c: [MipState::default(); MAX_UNISON_VOICES],
            mip_sub: sub_osc::SubMipState::default(),
            amplitude_level: 0.0,
            amplitude_stage: EnvStage::Attack,
            amplitude_phase: 0.0,
            amplitude_release_start: 0.0,
            amplitude_decay_scale: 1.0,
            amplitude_release_scale: 1.0,
            note_on_frame: 0,
            filter_level: 0.0,
            filter_stage: EnvStage::Attack,
            filter_phase: 0.0,
            filter_release_start: 0.0,
            env3_level: 0.0,
            env3_stage: EnvStage::Attack,
            env3_phase: 0.0,
            env3_release_start: 0.0,
            env4_level: 0.0,
            env4_stage: EnvStage::Attack,
            env4_phase: 0.0,
            env4_release_start: 0.0,
            filter: [
                StateVariableFilter::default(),
                StateVariableFilter::default(),
            ],
            filter2: [
                StateVariableFilter::default(),
                StateVariableFilter::default(),
            ],
            filter_ladder: [LadderFilter::default(), LadderFilter::default()],
            filter2_ladder: [LadderFilter::default(), LadderFilter::default()],
            filter_moog: [MoogLadder::default(), MoogLadder::default()],
            filter2_moog: [MoogLadder::default(), MoogLadder::default()],
            noise_state: 0x9e37_79b9_7f4a_7c15
                ^ ((channel as u64) << 40)
                ^ ((note as u64) << 24)
                ^ age,
            noise_hp: [0.0; 2],
            filter_coefficients: FilterCoefficients::default(),
            filter2_coefficients: FilterCoefficients::default(),
            filter_coefficient_age: 0,
            filter_coefficients_valid: false,
            filter_sample_rate: 0,
            pressure: 0.0,
            channel_pressure: 0.0,
            pitch_bend: 0.0,
            timbre: 0.5,
            expression_tuning: 0.0,
            expression_brightness: 0.5,
            frequency_hz: 0.0,
            frequency_valid: false,
            route_slew: [0.0; MAX_MOD_ROUTES],
            source_voices: [None, None, None],
            age,
        };
        // Unison layout depends on the patch, so `OpenSynth` fills the phases
        // after construction.
        voice
    }

    /// Build the source engine a slot needs. Called outside the render loop.
    fn prepare_source(&mut self, patch: &SynthPatch, slot: usize) {
        let waveform = patch.waveforms()[slot];
        let keep = matches!(
            (waveform, self.source_voices[slot].as_deref()),
            (Waveform::Granular, Some(SourceVoice::Granular(_)))
                | (Waveform::Spectral, Some(SourceVoice::Spectral(_)))
        );
        if keep {
            return;
        }
        let seed = source_seed(patch, waveform, self.age, slot, self.note);
        self.source_voices[slot] = match waveform {
            Waveform::Granular => Some(Box::new(SourceVoice::Granular(
                granular::GranularVoice::new(seed),
            ))),
            Waveform::Spectral => Some(Box::new(SourceVoice::Spectral(
                spectral::SpectralVoice::new(),
            ))),
            _ => None,
        };
    }

    fn prepare_sources(&mut self, patch: &SynthPatch) {
        for slot in 0..3 {
            self.prepare_source(patch, slot);
        }
    }

    /// Re-seed the existing source engines for a new note without allocating.
    fn reset_sources(&mut self, patch: &SynthPatch) {
        for slot in 0..3 {
            self.reset_source(patch, slot);
        }
    }

    fn reset_source(&mut self, patch: &SynthPatch, slot: usize) {
        let waveform = patch.waveforms()[slot];
        let keep = matches!(
            (waveform, self.source_voices[slot].as_deref()),
            (Waveform::Granular, Some(SourceVoice::Granular(_)))
                | (Waveform::Spectral, Some(SourceVoice::Spectral(_)))
        );
        if !keep {
            self.prepare_source(patch, slot);
            return;
        }
        let seed = source_seed(patch, waveform, self.age, slot, self.note);
        match self.source_voices[slot].as_deref_mut() {
            Some(SourceVoice::Granular(engine)) => engine.reset(seed),
            Some(SourceVoice::Spectral(engine)) => engine.reset(),
            None => {}
        }
    }

    /// Shorten this note's decay with the key it starts on.
    fn apply_damping(&mut self, damping: f64) {
        self.amplitude_decay_scale = if damping > 0.0 {
            2f64.powf(-damping * (self.note as f64 - MIDDLE_C) / 12.0)
        } else {
            1.0
        };
    }

    /// Shorten the release with the key and with a short hold, then start it.
    fn release_with(&mut self, damping: f64, now_frame: u64, sample_rate: f64) {
        self.amplitude_release_scale = if damping > 0.0 {
            let octaves = (self.note as f64 - MIDDLE_C) / 12.0;
            let held = now_frame.saturating_sub(self.note_on_frame) as f64 / sample_rate.max(1.0);
            let short = (held / SHORT_NOTE_SECONDS).clamp(0.0, 1.0);
            let pitch = 2f64
                .powf(-RELEASE_KEYTRACK * damping * octaves)
                .clamp(0.25, 2.0);
            pitch * (1.0 - RELEASE_SHORT_DEPTH * damping * (1.0 - short))
        } else {
            1.0
        };
        self.release();
    }

    fn release(&mut self) {
        self.amplitude_stage = EnvStage::Release;
        self.amplitude_phase = 0.0;
        self.amplitude_release_start = self.amplitude_level;
        self.filter_stage = EnvStage::Release;
        self.filter_phase = 0.0;
        self.filter_release_start = self.filter_level;
        self.env3_stage = EnvStage::Release;
        self.env3_phase = 0.0;
        self.env3_release_start = self.env3_level;
        self.env4_stage = EnvStage::Release;
        self.env4_phase = 0.0;
        self.env4_release_start = self.env4_level;
    }

    fn tick_envelopes(&mut self, steps: &EnvelopeSteps, curves: &[EnvelopeCurve; 4]) -> bool {
        // A scale of 1 leaves the step bit-identical, so an undamped patch
        // renders exactly as it did before `amp.damping` existed. The scales are
        // time multipliers, so they divide the phase advance.
        let amplitude = EnvelopeStageSteps {
            decay: steps.amplitude.decay / self.amplitude_decay_scale,
            release: steps.amplitude.release / self.amplitude_release_scale,
            ..steps.amplitude
        };
        let active = tick_envelope(
            &mut self.amplitude_stage,
            &mut self.amplitude_level,
            &mut self.amplitude_phase,
            self.amplitude_release_start,
            amplitude,
            &curves[0],
        );
        tick_envelope(
            &mut self.filter_stage,
            &mut self.filter_level,
            &mut self.filter_phase,
            self.filter_release_start,
            steps.filter,
            &curves[1],
        );
        tick_envelope(
            &mut self.env3_stage,
            &mut self.env3_level,
            &mut self.env3_phase,
            self.env3_release_start,
            steps.env3,
            &curves[2],
        );
        tick_envelope(
            &mut self.env4_stage,
            &mut self.env4_level,
            &mut self.env4_phase,
            self.env4_release_start,
            steps.env4,
            &curves[3],
        );
        active
    }

    fn next_noise(&mut self) -> f64 {
        self.noise_state ^= self.noise_state << 13;
        self.noise_state ^= self.noise_state >> 7;
        self.noise_state ^= self.noise_state << 17;
        self.noise_state as f64 / u64::MAX as f64 * 2.0 - 1.0
    }
}

/// Advance per sample for one envelope's three timed stages.
#[derive(Debug, Clone, Copy, Default)]
struct EnvelopeStageSteps {
    attack: f64,
    /// A floored attack uses a straight ramp so its first sample stays quiet.
    attack_linear: bool,
    decay: f64,
    release: f64,
    sustain: f64,
}

impl EnvelopeStageSteps {
    /// Zero-length stages complete in one sample and burn no division.
    fn new(attack: f64, decay: f64, sustain: f64, release: f64, sample_rate: f64) -> Self {
        Self::with_min_attack(attack, decay, sustain, release, sample_rate, 0.0)
    }

    /// Attack durations below `min_attack` become a straight ramp of that length.
    fn with_min_attack(
        attack: f64,
        decay: f64,
        sustain: f64,
        release: f64,
        sample_rate: f64,
        min_attack: f64,
    ) -> Self {
        let step = |duration: f64| {
            if duration <= 0.0 {
                1.0
            } else {
                1.0 / (duration * sample_rate)
            }
        };
        Self {
            attack: step(attack.max(min_attack)),
            attack_linear: attack < min_attack,
            decay: step(decay),
            release: step(release),
            sustain,
        }
    }
}

/// Phase advances for amplitude, filter, Env3 and Env4.
#[derive(Debug, Clone, Copy, Default)]
struct EnvelopeSteps {
    amplitude: EnvelopeStageSteps,
    filter: EnvelopeStageSteps,
    env3: EnvelopeStageSteps,
    env4: EnvelopeStageSteps,
}

fn tick_envelope(
    stage: &mut EnvStage,
    level: &mut f64,
    phase: &mut f64,
    release_start: f64,
    steps: EnvelopeStageSteps,
    curve: &EnvelopeCurve,
) -> bool {
    match stage {
        EnvStage::Attack => {
            *phase = (*phase + steps.attack).min(1.0);
            *level = if steps.attack_linear {
                *phase
            } else {
                1.0 - curve.sample(*phase)
            };
            if *phase >= 1.0 {
                *level = 1.0;
                *stage = EnvStage::Decay;
                *phase = 0.0;
            }
        }
        EnvStage::Decay => {
            *phase = (*phase + steps.decay).min(1.0);
            *level = steps.sustain + (1.0 - steps.sustain) * curve.sample(*phase);
            if *phase >= 1.0 {
                *level = steps.sustain;
                *stage = EnvStage::Sustain;
            }
        }
        EnvStage::Sustain => *level = steps.sustain,
        EnvStage::Release => {
            *phase = (*phase + steps.release).min(1.0);
            *level = release_start * curve.sample(*phase);
            if *phase >= 1.0 {
                *level = 0.0;
                return false;
            }
        }
    }
    true
}

/// Native-only events. Tokens increase in note-on playback order and are never
/// reused, so late tail events cannot address a replacement voice.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeVoiceEvent {
    On {
        token: u64,
        channel: u8,
        note: u8,
        velocity: u8,
    },
    Off {
        token: u64,
    },
    Pitch {
        token: u64,
        semitones: f64,
    },
}

/// Precomputed envelope shape: 1 at the stage start, 0 at the end.
#[derive(Debug, Clone, Default)]
struct EnvelopeCurve {
    values: Vec<f64>,
}

/// The envelope curve and the eased value clamp at `1 - 18014/2^53` and `1e-12`.
const CURVE_CLAMP: f64 = 1.0 - 18_014.0 / 9_007_199_254_740_992.0;

impl EnvelopeCurve {
    fn rebuild(&mut self, curve: f64) {
        let c = (curve.clamp(BIPOLAR_MODULATION.min, BIPOLAR_MODULATION.max) + 1.0) * 0.5;
        let q = 1.0 - c;
        let linear = (q - 0.5).abs() < 2e-6;
        let ratio = if linear {
            1.0
        } else {
            let q = q.clamp(1e-12, CURVE_CLAMP);
            let x = if q <= 0.5 {
                4.0 * q * q * q
            } else {
                1.0 - 4.0 * (1.0 - q) * (1.0 - q) * (1.0 - q)
            };
            let x = x.clamp(1e-12, CURVE_CLAMP);
            (x / (1.0 - x)).powi(2)
        };
        self.values.resize(ENV_TABLE_SIZE + 1, 0.0);
        if !linear && ratio == 0.0 {
            self.values.fill(0.0);
            self.values[0] = 1.0;
            return;
        }
        for (index, value) in self.values.iter_mut().enumerate() {
            let u = index as f64 / ENV_TABLE_SIZE as f64;
            *value = if linear {
                1.0 - u
            } else {
                let l = ratio.ln();
                if l > 0.0 {
                    ((u - 1.0) * l).exp_m1() / (-l).exp_m1()
                } else {
                    1.0 - (u * l).exp_m1() / l.exp_m1()
                }
            };
        }
        self.values[0] = 1.0;
        self.values[ENV_TABLE_SIZE] = 0.0;
    }

    #[inline]
    fn sample(&self, phase: f64) -> f64 {
        let position = phase.clamp(0.0, 1.0) * ENV_TABLE_SIZE as f64;
        let index = (position as usize).min(ENV_TABLE_SIZE - 1);
        let mix = position - index as f64;
        self.values[index] + (self.values[index + 1] - self.values[index]) * mix
    }
}

/// The stack sends every non-centre unison voice to one side; width is the pan
/// amount, so full width hard-pans every voice and odd counts keep a centre.
/// Centre compensation keeps `table * level * master` at unity.
fn sub_pan_gains(pan: f64) -> (f64, f64) {
    let p = (pan.clamp(-1.0, 1.0) + 1.0) * 0.5;
    let comp = UNISON_PAN_BASIS * p * (1.0 - p);
    let centre = 0.5 + UNISON_PAN_BASIS * 0.25;
    (((1.0 - p) + comp) / centre, (p + comp) / centre)
}

/// Cubic pan spread: the halfway setting reaches about 0.59 of hard pan.
/// The curve passes through zero and one, preserving mono at zero width.
fn unison_pan_spread(width: f64) -> f64 {
    let width = width.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
    width * (1.245 - 0.245 * width * width)
}

/// Side of one voice, positive = right. Voices alternate sides along the detune
/// ladder instead of grouping by detune sign, and each pair further in swaps
/// sides again, so the pair straddling the centre always opposes. Grouping by
/// detune sign left both channels correlated; this alternation is what makes
/// the stack wide. An odd count keeps its centre voice in the middle.
fn unison_pan_side(position: f64, count: usize) -> f64 {
    if position == 0.0 {
        return 0.0;
    }
    let step = 2.0 / (count.max(2) - 1) as f64;
    let rings = ((1.0 - position.abs()) / step).round();
    let parity = if (rings as i64) % 2 == 0 { 1.0 } else { -1.0 };
    -position.signum() * parity
}

fn unison_pan(position: f64, count: usize, width: f64) -> f64 {
    unison_pan_side(position, count) * unison_pan_spread(width)
}

/// Quadratic width lift of panned voices relative to the centre voice:
/// `1 + 0.258 w^2`, approximately 2 dB at full width.
const UNISON_PAN_WIDTH_LIFT: f64 = 0.258;

/// L/R gains of one unison voice. The balance keeps `L + R` fixed inside the
/// panned field; the width gain on panned voices is what lifts the outer
/// voices as the width rises. The centre voice is untouched.
fn unison_pan_gains(pan: f64, width: f64) -> (f64, f64) {
    let width = width.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
    let lift = if pan == 0.0 {
        1.0
    } else {
        1.0 + UNISON_PAN_WIDTH_LIFT * width * width
    };
    (lift * (1.0 - pan), lift * (1.0 + pan))
}

fn unison_layout(
    count: usize,
    detune_cents: f64,
) -> ([f64; MAX_UNISON_VOICES], [f64; MAX_UNISON_VOICES], f64) {
    let mut positions = [0.0; MAX_UNISON_VOICES];
    let mut ratios = [1.0; MAX_UNISON_VOICES];
    for oscillator in 0..count {
        let position = if count == 1 {
            0.0
        } else {
            oscillator as f64 / (count - 1) as f64 * 2.0 - 1.0
        };
        positions[oscillator] = position;
        ratios[oscillator] = 2f64.powf(position * detune_cents / 1200.0);
    }
    // The per-voice gain is the measured level table, not a count formula.
    (positions, ratios, unison_voice_gain(count, detune_cents))
}

/// When the C layer sits exactly two octaves above the sub, its fundamental is
/// the sub's fourth harmonic. A shared start phase puts the pair in anti-phase
/// and creates a notch. A small advance in the C start phase reduces the
/// cancellation. See `docs/synth-character.md`.
const SUB_4X_C_PHASE_STEP: f64 = 22.0 / 360.0;

/// The C layer's start-phase step when it is exactly four times the sub. The
/// step only exists for that ratio; every other layout keeps the shared phase.
fn sub_c_phase_offset(patch: &SynthPatch) -> f64 {
    let two_octaves = (patch.oscillator_c_semitones - patch.sub_semitones - 24.0).abs() < 1e-6;
    if two_octaves && patch.sub_level > 0.0 {
        SUB_4X_C_PHASE_STEP
    } else {
        0.0
    }
}

/// Start phase of one voice in a unison stack. The voice index and the
/// oscillator slot give a fixed, dense arrangement; see `UNISON_PHASE_SEED`.
fn unison_voice_phase(seed: u64, slot: u32, index: usize) -> f64 {
    let mut state = seed
        ^ (u64::from(slot) + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (index as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state ^= state >> 30;
    state = state.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94d0_49bb_1331_11eb);
    state ^= state >> 31;
    (state >> 11) as f64 / (1u64 << 53) as f64
}

/// Fixed seed for the voice-to-voice phase arrangement. Excluding note,
/// velocity and voice age gives the same phase image in a solo and in a mix.
/// At full width a two-voice layer has balanced, partially correlated channels.
const UNISON_PHASE_SEED: u64 = 230606;

/// Each detuned unison voice starts at its own fixed phase; the random-phase
/// amount adds further per-voice jitter. A layer at zero detune keeps every
/// voice on the shared start phase: there is no drift to separate them, so an
/// independent start would only cancel the stack instead of widening it.
/// `c_phase_offset` rotates the C layer only, for the 4x sub alignment above.
#[allow(clippy::too_many_arguments)]
fn lay_out_voice_phases(
    voice: &mut Voice,
    start: f64,
    jitter: f64,
    sub_phase: f64,
    c_phase_offset: f64,
    independent: [bool; 3],
    a_count: usize,
    b_count: usize,
    c_count: usize,
) {
    // Never the voice counter, the channel, the note or the velocity: all of
    // them make a source depend on the mix, and the phase image note-specific.
    let seed = UNISON_PHASE_SEED;
    let mut state = seed;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as f64 / u64::MAX as f64
    };
    let mut spread = |slot: u32, index: usize, count: usize| {
        let own = if independent[slot as usize] && count > 1 {
            unison_voice_phase(seed, slot, index)
        } else {
            0.0
        };
        (start + own + jitter * next()).fract()
    };
    let a_count = a_count.clamp(1, MAX_UNISON_VOICES);
    for index in 0..a_count {
        voice.phases[index] = spread(0, index, a_count);
        voice.warp_window[0][index] = voice.phases[index];
    }
    let b_count = b_count.clamp(1, MAX_UNISON_VOICES);
    for index in 0..b_count {
        let phase = spread(1, index, b_count);
        voice.warp_window[1][index] = phase;
        if index == 0 {
            voice.phases[PHASE_B] = phase;
        } else {
            voice.phases_b[index - 1] = phase;
        }
    }
    let c_count = c_count.clamp(1, MAX_UNISON_VOICES);
    for index in 0..c_count {
        let phase = (spread(2, index, c_count) + c_phase_offset).fract();
        voice.warp_window[2][index] = phase;
        if index == 0 {
            voice.phases[PHASE_C] = phase;
        } else {
            voice.phases_c[index - 1] = phase;
        }
    }
    voice.phases[PHASE_SUB] = sub_phase;
}

/// The centre voice, or the inner pair of an even stack above two voices, sits
/// 2.45 dB below the outer voices at the same count.
const UNISON_CENTRE_WEIGHT: f64 = 0.754;

/// Voice counts the level table is measured at.
const UNISON_LEVEL_COUNTS: [f64; 8] = [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0];

/// Detune spreads, in cents, the level table is measured at.
const UNISON_LEVEL_DETUNES: [f64; 4] = [0.0, 6.0, 12.0, 22.0];

/// Steady-state level of an N-voice stack, as the per-voice amplitude factor
/// that puts the stack on one voice's RMS.
///
/// Measured, not derived: whether a stack sums as amplitude or as power (12 dB
/// apart at 16 voices) depends on how its voices beat, and no formula covers
/// the partial coherence between the two. Each cell was measured with one
/// oscillator at C5 (MIDI 72, 48 kHz) at `osc.a.level` 1.0, the filter
/// bypassed, the stereo width at 0 and the patch trim pinned to 0 dB, over the
/// steady RMS from one second after the note-on. It is a level law, not a
/// character curve: it decides how loud a stack is and nothing else.
///
/// Rows follow [`UNISON_LEVEL_COUNTS`], columns [`UNISON_LEVEL_DETUNES`]. The
/// centre-voice weight is not in the table; it still applies on top.
const UNISON_LEVEL_TABLE: [[f64; 4]; 8] = [
    // N       0¢        6¢       12¢       22¢
    [1.000000, 1.000000, 1.000000, 1.000000], // 1
    [0.500000, 0.706864, 0.706253, 0.706568], // 2
    [0.362977, 0.625662, 0.625012, 0.624059], // 3
    [0.285062, 0.568773, 0.567374, 0.565365], // 4
    [0.181488, 0.445131, 0.442960, 0.442166], // 6
    [0.133171, 0.380771, 0.376797, 0.373766], // 8
    [0.086860, 0.299746, 0.302085, 0.295017], // 12
    [0.064473, 0.249347, 0.250496, 0.263425], // 16
];

/// Bracket `value` on a sorted table axis: the two indices and the linear
/// position between them. Values outside the axis clamp to its ends.
fn table_bracket(axis: &[f64], value: f64) -> (usize, usize, f64) {
    let last = axis.len() - 1;
    if value <= axis[0] {
        return (0, 0, 0.0);
    }
    if value >= axis[last] {
        return (last, last, 0.0);
    }
    let upper = axis.partition_point(|entry| *entry < value);
    let lower = upper - 1;
    (
        lower,
        upper,
        (value - axis[lower]) / (axis[upper] - axis[lower]),
    )
}

/// Per-voice gain of an N-voice stack.
///
/// The stack carries one voice's level whatever its count: the table is the
/// measured scale, linear between detune columns and between count rows, which
/// is enough for a coarse law. Cheap and allocation-free, and the result is
/// cached per patch by `refresh_unison_cache`.
fn unison_voice_gain(count: usize, detune_cents: f64) -> f64 {
    let count = count.clamp(1, MAX_UNISON_VOICES) as f64;
    let (row, next_row, row_position) = table_bracket(&UNISON_LEVEL_COUNTS, count);
    let (column, next_column, column_position) =
        table_bracket(&UNISON_LEVEL_DETUNES, detune_cents.abs());
    let cell = |row: usize, column: usize| UNISON_LEVEL_TABLE[row][column];
    let row_gain = |row: usize| {
        let low = cell(row, column);
        low + (cell(row, next_column) - low) * column_position
    };
    let low = row_gain(row);
    low + (row_gain(next_row) - low) * row_position
}

/// Relative gain of voice `index` within a stack of `count`; one and two
/// voices have no centre to pull down.
fn unison_voice_weight(count: usize, index: usize) -> f64 {
    if count <= 2 || index >= count {
        return 1.0;
    }
    let middle = if count % 2 == 1 {
        index == count / 2
    } else {
        index == count / 2 - 1 || index == count / 2
    };
    if middle { UNISON_CENTRE_WEIGHT } else { 1.0 }
}

fn unison_voice_weights(count: usize) -> [f64; MAX_UNISON_VOICES] {
    let count = count.clamp(1, MAX_UNISON_VOICES);
    std::array::from_fn(|index| unison_voice_weight(count, index))
}

/// Per-LFO audio state: shaped output, one-pole memories, fade-in age and the
/// chaotic generator's two most recent map values.
#[derive(Debug, Clone, Copy, Default)]
struct LfoRuntime {
    output: f64,
    /// One-pole state for the per-LFO `Smooth` parameter.
    smooth_state: f64,
    /// Seconds since the LFO phase was last reset.
    age: f64,
    smooth_coeff: f64,
    /// Three-state map memory for the chaotic generators.
    chaos: [f64; 3],
    /// Map values at the current and next iteration, for interpolation.
    chaos_curr: f64,
    chaos_next: f64,
    /// Fraction of the next map step, in `[0, 1)`.
    chaos_frac: f64,
}

impl LfoRuntime {
    /// Advance the map for one output sample at `rate * LFO_CHAOS_STEPS` per second.
    fn advance_chaos(&mut self, shape_type: LfoShapeType, rate: f64, inv_sample_rate: f64) {
        if !matches!(shape_type, LfoShapeType::ChaosA | LfoShapeType::ChaosB) {
            return;
        }
        self.chaos_frac += rate * LFO_CHAOS_STEPS * inv_sample_rate;
        let steps = self.chaos_frac.floor().max(0.0) as usize;
        self.chaos_frac -= steps as f64;
        for _ in 0..steps.min(LFO_CHAOS_MAX_STEPS) {
            self.chaos_curr = self.chaos_next;
            self.chaos_next = chaos_step(shape_type, &mut self.chaos);
        }
    }

    /// Advance the map across a skipped span; the caller supplies whole frames.
    fn advance_chaos_frames(
        &mut self,
        shape_type: LfoShapeType,
        rate: f64,
        frames: usize,
        sample_rate: u32,
    ) {
        if !matches!(shape_type, LfoShapeType::ChaosA | LfoShapeType::ChaosB) {
            return;
        }
        self.chaos_frac += rate * LFO_CHAOS_STEPS * frames as f64 / sample_rate.max(1) as f64;
        let steps = self.chaos_frac.floor().max(0.0) as usize;
        self.chaos_frac -= steps as f64;
        for _ in 0..steps.min(LFO_CHAOS_MAX_STEPS) {
            self.chaos_curr = self.chaos_next;
            self.chaos_next = chaos_step(shape_type, &mut self.chaos);
        }
    }

    /// Linear interpolation between the two most recent map values.
    fn chaos_output(&self) -> f64 {
        (self.chaos_curr + (self.chaos_next - self.chaos_curr) * self.chaos_frac).clamp(0.0, 1.0)
    }
}

pub struct OpenSynth {
    arpeggiators: Option<[arpeggiator::Arpeggiator; 16]>,
    arp_sample_rate: u32,
    patch: SynthPatch,
    voices: Vec<Voice>,
    /// Idle voices with their source engines already built. Note-on takes one
    /// and resets it; a finished voice returns here instead of freeing its
    /// engines, so the callback never allocates a spectral voice.
    spare_voices: Vec<Voice>,
    event_voices: Vec<(u64, u64)>,
    last_event_token: Option<u64>,
    age: u64,
    pitch_bend: [f64; 16],
    /// Current pitch-bend range in semitones (2 for legacy, 48 for MPE).
    mpe_bend_range: f64,
    /// Note-on channel assigned per zone and key; 255 means unassigned.
    mpe_zone_notes: [[u8; 128]; 2],
    /// Round-robin cursor per zone.
    mpe_next: [u8; 2],
    sustain: [bool; 16],
    lfo_phase: f64,
    lfo_phase_accumulator: u64,
    extra_lfo_phases: [f64; 9],
    extra_lfo_phase_accumulators: [u64; 9],
    /// Per-LFO shaped output and one-pole state, refreshed once per sample.
    lfo_runtime: [LfoRuntime; 10],
    step_phase: f64,
    step_phase_accumulator: u64,
    tempo: f64,
    held_notes: Vec<(u8, u8, u8)>,
    channel_pressure: [f64; 16],
    timbre: [f64; 16],
    mod_wheel: [f64; 16],
    lfo_hold_value: f64,
    lfo_random_state: u64,
    extra_lfo_hold_values: [f64; 9],
    extra_lfo_random_states: [u64; 9],
    active_mod_routes: [usize; MAX_MOD_ROUTES],
    active_mod_route_count: usize,
    /// Deduplicated routes used by the voice loop.
    compiled_mod_routes: [CompiledModRoute; MAX_MOD_ROUTES],
    compiled_mod_route_count: usize,
    /// Consecutive runs of identical routes; duplicates are evaluated once and
    /// added once per member, keeping the accumulation order unchanged.
    route_groups: [RouteGroup; MAX_MOD_ROUTES],
    route_group_count: usize,
    /// Patch-rate warp parameters; the voice loop reuses them unless a route
    /// can move a warp amount.
    warp_a_base: WarpParams,
    warp_b_base: WarpParams,
    warp_c_base: WarpParams,
    warp_a_modulated: bool,
    warp_b_modulated: bool,
    warp_c_modulated: bool,
    /// FX modulation routes, evaluated at [`FX_MOD_CONTROL_INTERVAL`] samples.
    compiled_fx_mod_routes: [CompiledFxModRoute; MAX_FX_MOD_ROUTES],
    compiled_fx_mod_route_count: usize,
    fx_mod_source_mask: u32,
    fx_mod_countdown: u32,
    /// Bit i is set when any active route uses `MOD_SOURCE_TABLE[i]`.
    active_source_mask: u32,
    /// Amplitude, filter, Env3 and Env4 curves.
    envelope_curves: [EnvelopeCurve; 4],
    /// Precomputed envelope phase advances for `envelope_steps_sample_rate`.
    envelope_steps: EnvelopeSteps,
    envelope_steps_sample_rate: u32,
    /// Sample-rate derived constants for [`OpenSynth::next_stereo`].
    render_sample_rate: u32,
    inv_sample_rate: f64,
    noise_hp_coeff: f64,
    /// Frames rendered so far; note-on and note-off stamp it for damping.
    rendered_frames: u64,
    /// LFO 1, LFO 2..10 and step accumulator advances.
    modulator_increments: [u64; 11],
    modulator_increments_sample_rate: u32,
    pitch_modulated: bool,
    filter_modulated: bool,
    oscillator_b_tuning_modulated: bool,
    oscillator_a_ratio: f64,
    oscillator_b_ratio: f64,
    sub_ratio: f64,
    /// Free-running sub phase; `sub_contiguous_phase` notes start from it.
    sub_free_phase: f64,
    last_sub_increment: f64,
    oscillator_c_ratio: f64,
    unison_positions: [f64; MAX_UNISON_VOICES],
    unison_ratios: [f64; MAX_UNISON_VOICES],
    unison_voice_count: usize,
    /// Count-law gain shared by the stack's outer voices.
    unison_scale: f64,
    /// Per-voice share of the count law; the centre voice is lighter.
    unison_weights: [f64; MAX_UNISON_VOICES],
    unison_b_positions: [f64; MAX_UNISON_VOICES],
    unison_b_ratios: [f64; MAX_UNISON_VOICES],
    unison_b_count: usize,
    unison_b_scale: f64,
    unison_b_weights: [f64; MAX_UNISON_VOICES],
    unison_c_positions: [f64; MAX_UNISON_VOICES],
    unison_c_ratios: [f64; MAX_UNISON_VOICES],
    unison_c_count: usize,
    unison_c_scale: f64,
    unison_c_weights: [f64; MAX_UNISON_VOICES],
    fx_chain: fx::SynthFxChain,
    /// Optional sample layer played alongside the synth voices. `None` when
    /// the patch has no sampler regions.
    sample_player: Option<crate::sampler::Sampler>,
    /// Per-owner sample output for the current frame. Reserved at construction
    /// so the audio callback never grows it.
    sample_scratch: Vec<crate::sampler::OwnedSample>,
    /// Decoded source samples for the three granular oscillator slots.
    granular_sources: [Option<Arc<granular::SourceSample>>; 3],
    /// Load-time spectral analyses for the three spectral oscillator slots.
    spectral_sources: [Option<Arc<spectral::SpectralSource>>; 3],
    /// One inverse plan shared by every spectral voice.
    spectral_ifft: Option<Arc<dyn rustfft::Fft<f64>>>,
    /// Offline preview only: per-unison contributions for one oscillator slot.
    preview_capture: Option<UnisonCapture>,
}

/// Records one oscillator slot's unison voices while the offline preview
/// renders. `None` on the audio path, so the extra branch in the voice loop
/// is a predictable not-taken check.
#[derive(Debug, Default)]
pub(crate) struct UnisonCapture {
    slot: u8,
    frames: usize,
    voices: usize,
    traces: Vec<Vec<f32>>,
    more_voices: bool,
    resets: usize,
}

impl UnisonCapture {
    #[inline]
    fn record(&mut self, voice: usize, sample: f64) {
        if let Some(trace) = self.traces.get_mut(voice)
            && trace.len() < self.frames
        {
            trace.push(sample as f32);
        }
    }

    #[inline]
    fn record_reset(&mut self) {
        self.resets += 1;
    }
}

#[derive(Debug, Clone, Default)]
struct StateVariableFilter {
    integrator_one: f64,
    integrator_two: f64,
    first_order: f64,
    /// Fixed Butterworth section used by the 24 dB/oct cascade.
    skirt_one: f64,
    skirt_two: f64,
}

impl StateVariableFilter {
    #[cfg(test)]
    fn lowpass(&mut self, input: f64, cutoff: f64, resonance: f64, sample_rate: f64) -> f64 {
        let cutoff = cutoff.clamp(
            FILTER_CUTOFF_HZ.min,
            sample_rate * FILTER_RUNTIME_BOUNDS.max_sample_rate_ratio,
        );
        let g = (std::f64::consts::PI * cutoff / sample_rate).sin();
        let damping = svf_damping(resonance, g).max(SVF_MIN_DAMPING);
        self.lowpass_with_coefficients(input, g, damping)
    }

    #[cfg(test)]
    fn lowpass_with_coefficients(&mut self, input: f64, g: f64, damping: f64) -> f64 {
        self.process_with_coefficients(input, g, damping).0
    }

    /// Same transfer function as `process_with_coefficients`, reusing the
    /// cached denominator reciprocal and only forming the requested output.
    fn process_mode_cached(
        &mut self,
        input: f64,
        g: f64,
        damping: f64,
        recip: f64,
        mode: FilterMode,
    ) -> f64 {
        let high = input - self.integrator_two;
        let band = (self.integrator_one + g * high) * recip;
        let low = self.integrator_two + g * band;
        self.integrator_one = 2.0 * band - self.integrator_one;
        self.integrator_two = 2.0 * low - self.integrator_two;
        match mode {
            FilterMode::LowPass => low,
            FilterMode::BandPass => band,
            FilterMode::HighPass => high - damping * band - g * band,
            FilterMode::Notch => low + high - damping * band - g * band,
        }
    }

    /// Resonant core plus one fixed stage. The fixed stage is unity at DC and
    /// -3 dB at the corner, so the passband and corner survive the cascade.
    // DSP hot path; a parameter struct would be rebuilt per call.
    #[allow(clippy::too_many_arguments)]
    fn process_cascade(
        &mut self,
        input: f64,
        g: f64,
        damping: f64,
        recip: f64,
        skirt_recip: f64,
        mode: FilterMode,
        extra_poles: usize,
    ) -> f64 {
        let core = self.process_mode_cached(input, g, damping, recip, mode);
        if extra_poles == 1 {
            self.process_first_order(core, g, mode)
        } else {
            self.process_skirt(core, g, skirt_recip, mode)
        }
    }

    /// Fixed maximally flat two-pole section at the same cutoff.
    fn process_skirt(&mut self, input: f64, g: f64, recip: f64, mode: FilterMode) -> f64 {
        let high = input - self.skirt_two;
        let band = (self.skirt_one + g * high) * recip;
        let low = self.skirt_two + g * band;
        self.skirt_one = 2.0 * band - self.skirt_one;
        self.skirt_two = 2.0 * low - self.skirt_two;
        match mode {
            FilterMode::LowPass => low,
            FilterMode::BandPass => band,
            FilterMode::HighPass => high - BUTTERWORTH_DAMPING * band - g * band,
            FilterMode::Notch => low + high - BUTTERWORTH_DAMPING * band - g * band,
        }
    }

    /// First-order tap of the same cutoff; the 6 dB modes have no
    /// resonance (measured flat at res 0.7 and 0.98).
    fn process_first_order(&mut self, input: f64, g: f64, mode: FilterMode) -> f64 {
        let v = (input - self.first_order) * g / (1.0 + g);
        let low = v + self.first_order;
        self.first_order = low + v;
        match mode {
            FilterMode::HighPass => input - low,
            _ => low,
        }
    }

    #[cfg(test)]
    fn process_with_coefficients(&mut self, input: f64, g: f64, damping: f64) -> (f64, f64, f64) {
        let high = input - self.integrator_two;
        let band = (self.integrator_one + g * high) / (1.0 + g * (g + damping));
        let low = self.integrator_two + g * band;
        self.integrator_one = 2.0 * band - self.integrator_one;
        self.integrator_two = 2.0 * low - self.integrator_two;
        (low, band, high - damping * band - g * band)
    }
}

/// Coefficients shared by one voice's filter, refreshed at control rate.
#[derive(Debug, Clone, Copy)]
struct FilterCoefficients {
    /// State-variable prewarp; the SVF uses a sine, the ladders a tangent.
    g: f64,
    damping: f64,
    recip: f64,
    /// Reciprocal of the fixed extra section's denominator (24 dB/oct only).
    skirt_recip: f64,
    ladder_g: f64,
    ladder_k: f64,
    /// Moog one-pole coefficient `G = f2/(1+f2)` and its fixed zero `0.29G`.
    moog_g: f64,
    moog_k: f64,
    moog_feedback: f64,
    input_gain: f64,
}

impl Default for FilterCoefficients {
    fn default() -> Self {
        Self {
            g: 0.0,
            damping: 1.0,
            recip: 1.0,
            skirt_recip: 1.0,
            ladder_g: 0.0,
            ladder_k: 0.0,
            moog_g: 0.0,
            moog_k: 0.0,
            moog_feedback: 0.0,
            input_gain: 1.0,
        }
    }
}

impl FilterCoefficients {
    fn new(model: FilterModel, cutoff: f64, resonance: f64, sample_rate: f64) -> Self {
        let angle = std::f64::consts::PI * cutoff / sample_rate;
        let g = angle.sin();
        // The damping law reaches zero at full resonance; a linear loop needs
        // a positive floor to decay, so the gain is capped at `MAX_RESONANT_GAIN`.
        let damping = svf_damping(resonance, g).max(SVF_MIN_DAMPING);
        let tan_g = angle.tan();
        // The Moog stage is `G = f2/(1+f2)` with `f2` the tangent prewarp; the
        // fixed zero is `0.29G`, so `k = 0.29G` and the pole is `1-1.29G`.
        let moog_g = tan_g / (1.0 + tan_g);
        let moog_k = MOOG_STAGE_ZERO * moog_g;
        let moog_feedback = moog_feedback(resonance);
        let input_gain = if model.is_moog() {
            // The ladder loses 1/(1+feedback) of its passband to the loop;
            // scale the input back so resonance changes the peak, not the level.
            1.0 + moog_feedback
        } else if model.has_resonance() {
            svf_input_gain(resonance)
        } else {
            1.0
        };
        Self {
            g,
            damping,
            recip: 1.0 / (1.0 + g * (g + damping)),
            skirt_recip: 1.0 / (1.0 + g * (g + BUTTERWORTH_DAMPING)),
            ladder_g: tan_g / (1.0 + tan_g),
            ladder_k: resonance * model.ladder_max_feedback(),
            moog_g,
            moog_k,
            moog_feedback,
            input_gain,
        }
    }
}

/// TPT ladder with 1, 3 or 4 stages. The linear feedback is solved zero-delay;
/// the input saturator keeps the loop bounded at maximum resonance.
#[derive(Debug, Clone, Copy, Default)]
struct LadderFilter {
    stage: [f64; 4],
}

impl LadderFilter {
    /// `poles` selects the slope: 1 = 6 dB/oct, 3 = 18 dB/oct, 4 = 24 dB/oct.
    #[inline]
    fn process(&mut self, input: f64, g: f64, feedback: f64, dirty: bool, poles: usize) -> f64 {
        let u = if poles == 4 {
            let g2 = g * g;
            let state_sum = (1.0 - g)
                * (g2 * g * self.stage[0] + g2 * self.stage[1] + g * self.stage[2] + self.stage[3]);
            (input - feedback * state_sum) / (1.0 + feedback * g2 * g2)
        } else {
            let mut weight = 1.0;
            let mut state_sum = 0.0;
            for stage in self.stage.iter().take(poles).rev() {
                state_sum += weight * stage;
                weight *= g;
            }
            let state_sum = state_sum * (1.0 - g);
            (input - feedback * state_sum) / (1.0 + feedback * weight)
        };
        // The dirty path saturates at unity small-signal gain: the drive is
        // the shape, not extra loop gain, so the feedback margin still holds.
        let mut y = if dirty {
            fast_tanh(u * 1.8) * (1.0 / 1.8)
        } else {
            u
        };
        for stage in self.stage.iter_mut().take(poles) {
            let x = if dirty {
                fast_tanh(y * 1.25) * (1.0 / 1.25)
            } else {
                y
            };
            let v = (x - *stage) * g;
            y = v + *stage;
            *stage = y + v;
        }
        y
    }
}

/// Four one-pole stages with a fixed zero at z = -0.29 and a one-sub-step
/// delayed feedback loop, run twice per sample and decimated 1/4, 1/2, 1/4.
#[derive(Debug, Clone, Copy, Default)]
struct MoogLadder {
    /// Stage output and stage input, from the previous sub-step.
    y: [f64; 4],
    x: [f64; 4],
    /// Tap of the previous output sample, for the decimator.
    last_tap: f64,
}

impl MoogLadder {
    /// One oversampled step; `feedback` is the resonance term at the loop
    /// input and `tap` selects the output stage (1..=4).
    #[inline]
    fn substep(&mut self, input: f64, g: f64, k: f64, feedback: f64, tap: usize) -> f64 {
        let decay = 1.0 - g - k;
        let mut stage_input = (input - feedback * self.y[3]).clamp(-5.0 / 3.0, 5.0 / 3.0);
        for stage in 0..4 {
            let output = g * stage_input + k * self.x[stage] + decay * self.y[stage];
            self.x[stage] = stage_input;
            self.y[stage] = output;
            stage_input = output;
        }
        self.y[tap - 1]
    }

    /// Two sub-steps per input sample with the halfband decimator.
    #[inline]
    fn process(&mut self, input: f64, g: f64, k: f64, feedback: f64, tap: usize) -> f64 {
        let half = self.substep(input, g, k, feedback, tap);
        let now = self.substep(input, g, k, feedback, tap);
        let output = 0.25 * self.last_tap + 0.5 * half + 0.25 * now;
        self.last_tap = now;
        output
    }
}

/// Odd rational tanh approximation, clamped outside +/-3 so the ladder loop
/// cannot grow without bound.
#[inline]
fn fast_tanh(value: f64) -> f64 {
    if value <= -3.0 {
        -1.0
    } else if value >= 3.0 {
        1.0
    } else {
        let square = value * value;
        value * (27.0 + square) / (27.0 + 9.0 * square)
    }
}

/// Ceiling on the resonant gain of the state-variable family (+6 dB). A
/// two-pole peak adds `1 / damping` at the corner, so flooring the damping at
/// `1 / MAX_RESONANT_GAIN` bounds the peak without touching the passband. This
/// is the resonance normalisation: the stage may consume the instrument's
/// headroom, never more. See `docs/synth-levels.md`.
const MAX_RESONANT_GAIN: f64 = 2.0;

/// Smallest applied damping, i.e. the linear peak ceiling.
const SVF_MIN_DAMPING: f64 = 1.0 / MAX_RESONANT_GAIN;

/// Corner of the fixed extra stage in the 18/24 dB cascades.
const BUTTERWORTH_DAMPING: f64 = std::f64::consts::SQRT_2;

/// Fixed per-stage zero of the Moog model.
const MOOG_STAGE_ZERO: f64 = 0.29;

/// Fraction of the four-times-resonance oscillation threshold we allow; the
/// same margin as the TPT ladder, so the full-resonance loop strictly decays.
const MOOG_FEEDBACK_MARGIN: f64 = 0.875;

/// State-variable damping (reciprocal Q): `min(2 - 2*res^0.25, 1/g - g)`,
/// clamped to [0, 2], where `g` is the sine prewarp of the corner.
#[inline]
fn svf_damping(resonance: f64, g: f64) -> f64 {
    let resonance = resonance.clamp(0.0, 1.0);
    (2.0 - 2.0 * resonance.powf(0.25))
        .min(1.0 / g - g)
        .clamp(0.0, 2.0)
}

impl OpenSynth {
    pub fn new(patch: SynthPatch) -> Self {
        Self::new_with_tempo(patch, 120.0)
    }

    pub fn new_with_tempo(patch: SynthPatch, tempo: f64) -> Self {
        Self::new_with_fx_chain(patch, tempo, fx::SynthFxChain::new())
    }

    /// Build with a caller-supplied FX chain so an offline preview can reuse
    /// the rack's buffers instead of allocating them per render.
    fn new_with_fx_chain(patch: SynthPatch, tempo: f64, fx_chain: fx::SynthFxChain) -> Self {
        let mut patch = patch;
        patch.normalize_legacy_lfo();
        // Prewarm immutable tables before audio processing so switching to the
        // wavetable oscillator through automation never initializes on the sample path.
        let _ = default_shapes_table();
        if let Some(table) = &patch.custom_wavetable {
            table.prepare_mips();
        }
        for table in [
            patch.oscillator_b_wavetable.as_ref(),
            patch.oscillator_c_wavetable.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            table.prepare_mips();
        }
        // The sub shares the readout kernel and builds its mip pyramids up front.
        sub_osc::prepare();
        let sample_player = patch.sampler.as_ref().and_then(|sampler_patch| {
            match crate::sampler::Sampler::from_patch(sampler_patch) {
                Ok(player) => Some(player),
                Err(error) => {
                    log::warn!("OpenSynth sample layer disabled: {error}");
                    None
                }
            }
        });
        let mut granular_sources: [Option<Arc<granular::SourceSample>>; 3] = [None, None, None];
        for (slot, slot_source) in granular_sources.iter_mut().enumerate() {
            if patch.waveforms()[slot] != Waveform::Granular {
                continue;
            }
            let Some(settings) = patch.granular[slot].as_ref() else {
                continue;
            };
            if settings.sample.as_os_str().is_empty() {
                continue;
            }
            match granular::SourceSample::load(&settings.sample) {
                Ok(source) => *slot_source = Some(Arc::new(source)),
                Err(error) => {
                    log::warn!("OpenSynth granular source disabled: {error}");
                }
            }
        }
        let mut spectral_sources: [Option<Arc<spectral::SpectralSource>>; 3] = [None, None, None];
        for (slot, slot_source) in spectral_sources.iter_mut().enumerate() {
            if patch.waveforms()[slot] != Waveform::Spectral {
                continue;
            }
            let Some(settings) = patch.spectral[slot].as_ref() else {
                continue;
            };
            if settings.sample.as_os_str().is_empty() {
                continue;
            }
            match spectral::SpectralSource::load(&settings.sample) {
                Ok(source) => *slot_source = Some(Arc::new(source)),
                Err(error) => {
                    log::warn!("OpenSynth spectral source disabled: {error}");
                }
            }
        }
        let spectral_ifft = if spectral_sources.iter().any(Option::is_some) {
            let mut planner = rustfft::FftPlanner::<f64>::new();
            let plan = planner.plan_fft_inverse(spectral::FFT_SIZE);
            if plan.get_inplace_scratch_len() > spectral::SpectralVoice::scratch_capacity() {
                log::warn!("OpenSynth spectral source disabled: inverse FFT scratch too large");
                None
            } else {
                Some(plan)
            }
        } else {
            None
        };
        let mut synth = Self {
            arpeggiators: patch.arpeggiator.map(|settings| {
                std::array::from_fn(|_| {
                    arpeggiator::Arpeggiator::new(settings, tempo.clamp(1.0, 400.0), 48000)
                        .expect("validated arpeggiator patch")
                })
            }),
            arp_sample_rate: 48000,
            patch,
            voices: Vec::with_capacity(MAX_VOICES),
            spare_voices: Vec::with_capacity(MAX_VOICES),
            event_voices: Vec::with_capacity(MAX_VOICES),
            last_event_token: None,
            age: 0,
            pitch_bend: [0.0; 16],
            mpe_bend_range: 2.0,
            mpe_zone_notes: [[MPE_UNASSIGNED; 128]; 2],
            mpe_next: [0; 2],
            sustain: [false; 16],
            lfo_phase: 0.0,
            lfo_phase_accumulator: 0,
            extra_lfo_phases: [0.0; 9],
            extra_lfo_phase_accumulators: [0; 9],
            lfo_runtime: [LfoRuntime::default(); 10],
            step_phase: 0.0,
            step_phase_accumulator: 0,
            tempo: tempo.clamp(1.0, 400.0),
            held_notes: Vec::with_capacity(16 * 128),
            channel_pressure: [0.0; 16],
            timbre: [0.5; 16],
            mod_wheel: [0.0; 16],
            lfo_hold_value: 0.0,
            lfo_random_state: 0xd1b5_4a32_d192_ed03,
            extra_lfo_hold_values: [0.0; 9],
            extra_lfo_random_states: std::array::from_fn(|index| {
                0xd1b5_4a32_d192_ed03_u64
                    .wrapping_add((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15))
            }),
            active_mod_routes: [0; MAX_MOD_ROUTES],
            active_mod_route_count: 0,
            compiled_mod_routes: std::array::from_fn(|_| CompiledModRoute {
                source: 0,
                auxiliary: None,
                auxiliary_inverted: false,
                bipolar: false,
                destination: ModDestination::FilterCutoff,
                amount: 0.0,
                curve: 0.0,
                slew_up_seconds: 0.0,
                slew_down_seconds: 0.0,
                slew_up: 1.0,
                slew_down: 1.0,
            }),
            compiled_mod_route_count: 0,
            route_groups: [RouteGroup::default(); MAX_MOD_ROUTES],
            route_group_count: 0,
            warp_a_base: WarpParams::new(WarpMode::Off, 0.0, 0.0, WarpMode::Off, 0.0, 0.0),
            warp_b_base: WarpParams::new(WarpMode::Off, 0.0, 0.0, WarpMode::Off, 0.0, 0.0),
            warp_c_base: WarpParams::new(WarpMode::Off, 0.0, 0.0, WarpMode::Off, 0.0, 0.0),
            warp_a_modulated: false,
            warp_b_modulated: false,
            warp_c_modulated: false,
            compiled_fx_mod_routes: std::array::from_fn(|_| CompiledFxModRoute {
                source: 0,
                fx_index: 0,
                parameter: FxParam::Wet,
                amount: 0.0,
                bipolar: false,
            }),
            compiled_fx_mod_route_count: 0,
            fx_mod_source_mask: 0,
            fx_mod_countdown: 0,
            active_source_mask: 0,
            envelope_curves: std::array::from_fn(|_| EnvelopeCurve::default()),
            envelope_steps: EnvelopeSteps::default(),
            envelope_steps_sample_rate: 0,
            render_sample_rate: 0,
            inv_sample_rate: 0.0,
            noise_hp_coeff: 0.0,
            rendered_frames: 0,
            modulator_increments: [0; 11],
            modulator_increments_sample_rate: 0,
            pitch_modulated: false,
            filter_modulated: false,
            oscillator_b_tuning_modulated: false,
            oscillator_a_ratio: 1.0,
            oscillator_b_ratio: 1.0,
            sub_ratio: 0.5,
            sub_free_phase: 0.0,
            last_sub_increment: 0.0,
            oscillator_c_ratio: 1.0,
            unison_positions: [0.0; MAX_UNISON_VOICES],
            unison_ratios: [1.0; MAX_UNISON_VOICES],
            unison_voice_count: 1,
            unison_scale: 1.0,
            unison_weights: [1.0; MAX_UNISON_VOICES],
            unison_b_positions: [0.0; MAX_UNISON_VOICES],
            unison_b_ratios: [1.0; MAX_UNISON_VOICES],
            unison_b_count: 1,
            unison_b_scale: 1.0,
            unison_b_weights: [1.0; MAX_UNISON_VOICES],
            unison_c_positions: [0.0; MAX_UNISON_VOICES],
            unison_c_ratios: [1.0; MAX_UNISON_VOICES],
            unison_c_count: 1,
            unison_c_scale: 1.0,
            unison_c_weights: [1.0; MAX_UNISON_VOICES],
            fx_chain,
            sample_player,
            sample_scratch: Vec::with_capacity(crate::sampler::MAX_VOICES),
            granular_sources,
            spectral_sources,
            spectral_ifft,
            preview_capture: None,
        };
        synth.refresh_active_mod_routes();
        synth.rebuild_envelope_curves();
        synth.refresh_oscillator_a_ratio();
        synth.refresh_oscillator_b_ratio();
        synth.refresh_sub_ratio();
        synth.refresh_oscillator_c_ratio();
        synth.refresh_unison_cache();
        // Reserve the rack and its parameter buffers once, outside the audio
        // callback, so live `fx.*` edits cannot grow the patch allocation.
        synth.patch.fx.reserve(fx::MAX_SYNTH_FX);
        synth.fx_chain.sync_from_patch(&synth.patch.fx);
        synth.refresh_fx_mod_routes();
        synth.refresh_mpe_config();
        for index in 0..10 {
            synth.reset_lfo_runtime(index);
        }
        // Source engines are large; build the idle set here so a note-on only
        // re-seeds an existing engine.
        synth.prepare_spare_voices();
        synth
    }

    /// Consume the synth and hand its FX rack back for reuse.
    fn into_fx_chain(self) -> fx::SynthFxChain {
        self.fx_chain
    }

    /// Change musical rate without resetting oscillator/modulator phase or voices.
    pub fn set_tempo(&mut self, tempo: f64, sample_rate: u32) -> Result<(), String> {
        if !tempo.is_finite() || !(1.0..=400.0).contains(&tempo) || sample_rate == 0 {
            return Err("invalid synth tempo/sample rate".into());
        }
        self.modulator_increments_sample_rate = 0;
        if let Some(arps) = &mut self.arpeggiators {
            for arp in arps {
                arp.set_clock(tempo, sample_rate);
            }
            self.arp_sample_rate = sample_rate;
        }
        self.tempo = tempo;
        Ok(())
    }

    /// Build the idle set for the current patch. Call outside the callback.
    fn prepare_spare_voices(&mut self) {
        while self.spare_voices.len() < MAX_VOICES {
            let mut voice = Voice::new(0, 0, 0, 0);
            voice.prepare_sources(&self.patch);
            self.spare_voices.push(voice);
        }
    }

    /// Return a voice that no longer sounds to the idle set, keeping its source
    /// engines for the next note-on.
    fn recycle_voice(&mut self, voice: Voice) {
        // A full set means the voice came from a fallback allocation; dropping
        // it keeps the idle set bounded.
        if self.spare_voices.len() < MAX_VOICES {
            self.spare_voices.push(voice);
        }
    }

    /// Take an idle voice and reset it for a new note. `self.age` must already
    /// hold the voice identity.
    fn take_voice(&mut self, channel: u8, note: u8, velocity: u8) -> Voice {
        let mut voice = match self.spare_voices.pop() {
            Some(voice) => voice,
            None => {
                // Unreachable while the idle set is filled at construction and
                // refilled on every voice removal; keep a working fallback.
                let mut voice = Voice::new(channel, note, velocity, self.age);
                voice.prepare_sources(&self.patch);
                voice
            }
        };
        let sources = [
            voice.source_voices[0].take(),
            voice.source_voices[1].take(),
            voice.source_voices[2].take(),
        ];
        voice = Voice::new(channel, note, velocity, self.age);
        voice.source_voices = sources;
        voice.reset_sources(&self.patch);
        voice
    }

    /// Drop the voices that fail `keep`, returning them to the idle set.
    fn retain_voices(&mut self, mut keep: impl FnMut(&Voice) -> bool) {
        let mut index = 0;
        while index < self.voices.len() {
            if keep(&self.voices[index]) {
                index += 1;
            } else {
                let voice = self.voices.remove(index);
                self.recycle_voice(voice);
            }
        }
    }

    /// Rebuild the source engines after a waveform change. Control path only.
    fn refresh_source_voices(&mut self) {
        for voice in &mut self.voices {
            voice.prepare_sources(&self.patch);
        }
        for voice in &mut self.spare_voices {
            voice.prepare_sources(&self.patch);
        }
    }

    pub fn note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        if let Some(arps) = &mut self.arpeggiators {
            arps[channel.min(15) as usize].note_on(note, velocity);
            return;
        }
        if velocity == 0 {
            self.note_off(channel, note);
            return;
        }
        let channel = self.mpe_note_on_channel(channel, note);
        self.voice_note_on(channel, note, velocity);
    }

    /// Shared native event dispatch for offline export and bounded GUI playback.
    pub fn send_midi(&mut self, event: &MidiMessage) -> Result<(), String> {
        match event {
            MidiMessage::NativeVoice(event) => {
                self.send_native_voice(event)?;
            }
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => self.note_on(*channel, *note, *velocity),
            MidiMessage::NoteOff { channel, note } => self.note_off(*channel, *note),
            MidiMessage::PitchBend { channel, value } => self.set_pitch_bend(*channel, *value),
            MidiMessage::ChannelPressure { channel, pressure } => {
                self.set_channel_pressure(*channel, *pressure)
            }
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => self.set_poly_pressure(*channel, *note, *pressure),
            MidiMessage::NoteExpression {
                channel,
                note,
                expression,
                value,
            } => self.set_note_expression(*channel, *note, expression, *value),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => match controller {
                1 => self.set_mod_wheel(*channel, *value),
                64 => self.set_sustain(*channel, *value >= 64),
                74 => self.set_timbre(*channel, *value),
                120 => self.all_sound_off(*channel),
                121 => self.reset_controllers(*channel),
                123 => self.all_notes_off(*channel),
                _ => {}
            },
            MidiMessage::PluginParam { name, value } => self.set_normalized_param(name, *value),
            _ => {}
        }
        Ok(())
    }

    pub fn send_native_voice(&mut self, event: &NativeVoiceEvent) -> Result<bool, String> {
        self.event_voices
            .retain(|(_, id)| self.voices.iter().any(|v| v.age == *id));
        match *event {
            NativeVoiceEvent::On {
                token,
                channel,
                note,
                velocity,
            } => {
                if self
                    .last_event_token
                    .is_some_and(|previous| token <= previous)
                {
                    return Err("native voice tokens must increase without reuse".into());
                }
                let id = self.note_on_identified(channel, note, velocity)?;
                self.event_voices
                    .retain(|(_, old)| self.voices.iter().any(|v| v.age == *old));
                self.event_voices.push((token, id));
                self.last_event_token = Some(token);
                Ok(true)
            }
            NativeVoiceEvent::Off { token } => Ok(self
                .event_voices
                .iter()
                .find(|(t, _)| *t == token)
                .map(|(_, id)| *id)
                .is_some_and(|id| self.note_off_identified(id))),
            NativeVoiceEvent::Pitch { token, semitones } => {
                if !semitones.is_finite()
                    || semitones.abs() > crate::note_pitch_curve::MAX_SEMITONES
                {
                    return Err("native voice pitch outside range".into());
                }
                match self
                    .event_voices
                    .iter()
                    .find(|(t, _)| *t == token)
                    .map(|(_, id)| *id)
                {
                    Some(id) => self.set_identified_pitch(id, semitones),
                    None => Ok(false),
                }
            }
        }
    }

    fn voice_note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        self.voice_note_on_inner(channel, note, velocity, false);
    }

    /// Allocate an identity for polyphonic native expression, including tails.
    /// Identity is per synth instance; stolen/finished voices are never retargeted.
    pub fn note_on_identified(
        &mut self,
        channel: u8,
        note: u8,
        velocity: u8,
    ) -> Result<u64, String> {
        if self.arpeggiators.is_some() || self.patch.monophonic {
            return Err("identified notes require polyphonic non-arpeggiated source".into());
        }
        if channel > 15 || note > 127 || velocity == 0 || velocity > 127 || self.age == u64::MAX {
            return Err("invalid identified note or exhausted voice identity".into());
        }
        self.voice_note_on_inner(channel, note, velocity, true);
        Ok(self.age)
    }

    pub fn note_off_identified(&mut self, id: u64) -> bool {
        let damping = self.patch.amp_damping;
        let (now_frame, sample_rate) = (self.rendered_frames, self.envelope_steps_sample_rate);
        let Some(voice) = self.voices.iter_mut().find(|v| v.age == id) else {
            return false;
        };
        if !voice.key_down {
            return true;
        }
        let (channel, note) = (voice.channel, voice.note);
        voice.key_down = false;
        if !self.sustain[channel as usize] {
            voice.release_with(damping, now_frame, sample_rate as f64);
        }
        if let Some(player) = &mut self.sample_player {
            player.note_off(channel, note);
        }
        if !self
            .voices
            .iter()
            .any(|v| v.channel == channel && v.note == note && v.key_down)
        {
            self.held_notes
                .retain(|&(ch, key, _)| ch != channel || key != note);
        }
        true
    }

    /// Curve changes apply to the identified voice even during release.
    pub fn set_identified_pitch(&mut self, id: u64, semitones: f64) -> Result<bool, String> {
        if !semitones.is_finite() || semitones.abs() > crate::note_pitch_curve::MAX_SEMITONES {
            return Err("identified pitch outside native range".into());
        }
        let Some(voice) = self.voices.iter_mut().find(|v| v.age == id) else {
            return Ok(false);
        };
        voice.expression_tuning = semitones;
        voice.frequency_valid = false;
        Ok(true)
    }

    fn voice_note_on_inner(
        &mut self,
        channel: u8,
        note: u8,
        velocity: u8,
        preserve_same_pitch: bool,
    ) {
        if velocity == 0 {
            self.note_off(channel, note);
            return;
        }
        let start_phase = self.patch.oscillator_start_phase;
        let random_phase = self.patch.oscillator_random_phase.clamp(0.0, 1.0);
        let a_count = self.unison_voice_count.max(1);
        let b_count = self.unison_b_count.max(1);
        let c_count = self.unison_c_count.max(1);
        let sub_phase = if self.patch.sub_contiguous_phase {
            self.sub_free_phase
        } else {
            self.patch.sub_initial_phase.fract()
        };
        let c_phase_offset = sub_c_phase_offset(&self.patch);
        // A layer at zero detune has no drift to separate its voices, so it
        // stays phase-coherent; only a detuned layer gets independent starts.
        let independent = [
            self.patch.detune_cents != 0.0,
            self.patch.oscillator_b_detune_cents != 0.0,
            self.patch.oscillator_c_detune_cents != 0.0,
        ];
        // The voice created below reuses this id, so the sampler can attach its
        // regions to the same filter and amp envelope.
        let owner = self.age.wrapping_add(1);
        if let Some(player) = &mut self.sample_player {
            player.note_on_owned(channel, note, velocity, owner);
        }
        if self.patch.lfo_retrigger {
            self.lfo_phase_accumulator = phase_accumulator(self.lfo_start_phase(0) / 360.0);
            self.lfo_phase = phase_from_accumulator(self.lfo_phase_accumulator);
            self.reset_lfo_runtime(0);
        }
        for index in 0..9 {
            let retrigger = self.patch.extra_lfos[index].retrigger;
            if retrigger {
                self.extra_lfo_phase_accumulators[index] =
                    phase_accumulator(self.lfo_start_phase(index + 1) / 360.0);
                self.extra_lfo_phases[index] =
                    phase_from_accumulator(self.extra_lfo_phase_accumulators[index]);
                self.reset_lfo_runtime(index + 1);
            }
        }
        self.held_notes
            .retain(|&(held_channel, held_note, _)| held_channel != channel || held_note != note);
        self.held_notes.push((channel, note, velocity));
        if self.patch.monophonic {
            self.age = self.age.wrapping_add(1);
            let channel_index = channel.min(15) as usize;
            let expression = (
                self.pitch_bend[channel_index],
                self.channel_pressure[channel_index],
                self.timbre[channel_index],
            );
            if let Some(voice) = self.voices.first_mut() {
                let previous_pitch = voice.pitch_note;
                if self.patch.legato {
                    voice.channel = channel;
                    voice.note = note;
                    voice.velocity = velocity as f64 / 127.0;
                    voice.key_down = true;
                    voice.age = self.age;
                    voice.frequency_valid = false;
                    voice.filter_coefficients_valid = false;
                    voice.apply_damping(self.patch.amp_damping);
                } else {
                    // Keep the prebuilt engines; `Voice::new` resets every
                    // scalar field, and the engines are re-seeded for this note.
                    let sources = [
                        voice.source_voices[0].take(),
                        voice.source_voices[1].take(),
                        voice.source_voices[2].take(),
                    ];
                    *voice = Voice::new(channel, note, velocity, self.age);
                    voice.source_voices = sources;
                    lay_out_voice_phases(
                        voice,
                        start_phase,
                        random_phase,
                        sub_phase,
                        c_phase_offset,
                        independent,
                        a_count,
                        b_count,
                        c_count,
                    );
                    voice.reset_sources(&self.patch);
                    voice.apply_damping(self.patch.amp_damping);
                    voice.note_on_frame = self.rendered_frames;
                }
                voice.pitch_bend = expression.0;
                voice.channel_pressure = expression.1;
                voice.timbre = expression.2;
                voice.pitch_note = if self.patch.glide_time > 0.0 {
                    previous_pitch
                } else {
                    note as f64
                };
            } else {
                let mut voice = self.take_voice(channel, note, velocity);
                lay_out_voice_phases(
                    &mut voice,
                    start_phase,
                    random_phase,
                    sub_phase,
                    c_phase_offset,
                    independent,
                    a_count,
                    b_count,
                    c_count,
                );
                voice.apply_damping(self.patch.amp_damping);
                voice.note_on_frame = self.rendered_frames;
                voice.pitch_bend = expression.0;
                voice.channel_pressure = expression.1;
                voice.timbre = expression.2;
                self.voices.push(voice);
            }
            while self.voices.len() > 1 {
                let voice = self.voices.pop().expect("voice present");
                self.recycle_voice(voice);
            }
            return;
        }
        self.retain_voices(|voice| {
            preserve_same_pitch || voice.channel != channel || voice.note != note
        });
        let voice_limit = self.patch.polyphony.clamp(1, MAX_VOICES);
        if self.voices.len() >= voice_limit {
            let index = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, voice)| (voice.amplitude_stage != EnvStage::Release, voice.age))
                .map(|(index, _)| index)
                .unwrap_or(0);
            let voice = self.voices.remove(index);
            self.recycle_voice(voice);
        }
        self.age = self.age.wrapping_add(1);
        let mut voice = self.take_voice(channel, note, velocity);
        lay_out_voice_phases(
            &mut voice,
            start_phase,
            random_phase,
            sub_phase,
            c_phase_offset,
            independent,
            a_count,
            b_count,
            c_count,
        );
        voice.apply_damping(self.patch.amp_damping);
        voice.note_on_frame = self.rendered_frames;
        let channel_index = channel.min(15) as usize;
        voice.pitch_bend = self.pitch_bend[channel_index];
        voice.channel_pressure = self.channel_pressure[channel_index];
        voice.timbre = self.timbre[channel_index];
        self.voices.push(voice);
    }

    pub fn note_off(&mut self, channel: u8, note: u8) {
        if let Some(arps) = &mut self.arpeggiators {
            arps[channel.min(15) as usize].note_off(note);
            return;
        }
        if let Some((zone, range)) = self.mpe_zone_for_master(channel) {
            let member = self.mpe_zone_notes[zone][(note & 0x7f) as usize];
            if member != MPE_UNASSIGNED {
                self.mpe_zone_notes[zone][(note & 0x7f) as usize] = MPE_UNASSIGNED;
                self.voice_note_off(member, note);
            } else {
                let (low, high) = range.bounds();
                for member in low..=high {
                    self.voice_note_off(member, note);
                }
            }
            return;
        }
        for zone in 0..2 {
            if self.mpe_zone_notes[zone][(note & 0x7f) as usize] == channel {
                self.mpe_zone_notes[zone][(note & 0x7f) as usize] = MPE_UNASSIGNED;
            }
        }
        self.voice_note_off(channel, note);
    }

    fn mpe_config(&self) -> Option<MpeConfig> {
        self.patch.mpe.filter(MpeConfig::enabled)
    }

    /// A note-on on a zone master channel allocates a member channel.
    fn mpe_zone_for_master(&self, channel: u8) -> Option<(usize, MpeMemberRange)> {
        let config = self.mpe_config()?;
        match channel.min(15) {
            0 => config.lower.map(|range| (MPE_LOWER_ZONE, range)),
            15 => config.upper.map(|range| (MPE_UPPER_ZONE, range)),
            _ => None,
        }
    }

    fn mpe_allocate_member(&mut self, zone: usize, range: MpeMemberRange) -> u8 {
        let (low, high) = range.bounds();
        let span = high - low + 1;
        let start = self.mpe_next[zone] % span;
        for offset in 0..span {
            let member = low + (start + offset) % span;
            if !self
                .voices
                .iter()
                .any(|voice| voice.channel == member && voice.key_down)
            {
                self.mpe_next[zone] = (start + offset + 1) % span;
                return member;
            }
        }
        let member = low + start;
        self.mpe_next[zone] = (start + 1) % span;
        member
    }

    fn mpe_note_on_channel(&mut self, channel: u8, note: u8) -> u8 {
        let Some((zone, range)) = self.mpe_zone_for_master(channel) else {
            return channel.min(15);
        };
        let note_index = (note & 0x7f) as usize;
        if self.mpe_zone_notes[zone][note_index] != MPE_UNASSIGNED {
            return self.mpe_zone_notes[zone][note_index];
        }
        let member = self.mpe_allocate_member(zone, range);
        self.mpe_zone_notes[zone][note_index] = member;
        member
    }

    /// Apply live zone configuration without rebuilding the instrument.
    pub fn set_mpe_config(&mut self, config: Option<MpeConfig>) {
        self.patch.mpe = config;
        self.mpe_zone_notes = [[MPE_UNASSIGNED; 128]; 2];
        self.mpe_next = [0; 2];
        self.refresh_mpe_config();
    }

    fn refresh_mpe_config(&mut self) {
        self.mpe_bend_range = self
            .patch
            .mpe
            .map_or(2.0, |config| config.bend_range_semitones());
    }

    fn voice_note_off(&mut self, channel: u8, note: u8) {
        if let Some(player) = &mut self.sample_player {
            player.note_off(channel, note);
        }
        self.held_notes
            .retain(|&(held_channel, held_note, _)| held_channel != channel || held_note != note);
        let damping = self.patch.amp_damping;
        let (now_frame, sample_rate) = (self.rendered_frames, self.envelope_steps_sample_rate);
        if self.patch.monophonic {
            let replacement = self.held_notes.last().copied();
            if let Some(voice) = self.voices.first_mut()
                && voice.channel == channel
                && voice.note == note
            {
                if let Some((next_channel, next_note, next_velocity)) = replacement {
                    voice.channel = next_channel;
                    voice.note = next_note;
                    voice.velocity = next_velocity as f64 / 127.0;
                    voice.key_down = true;
                    voice.frequency_valid = false;
                    voice.filter_coefficients_valid = false;
                    voice.apply_damping(damping);
                } else {
                    voice.key_down = false;
                    if !self.sustain[channel.min(15) as usize] {
                        voice.release_with(damping, now_frame, sample_rate as f64);
                    }
                }
            }
            return;
        }
        let sustain = self.sustain[channel.min(15) as usize];
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel && voice.note == note)
        {
            voice.key_down = false;
            if !sustain {
                voice.release_with(damping, now_frame, sample_rate as f64);
            }
        }
    }

    pub fn set_pitch_bend(&mut self, channel: u8, value: i16) {
        if let Some(player) = &mut self.sample_player {
            player.set_pitch_bend(channel, value);
        }
        let channel = channel.min(15);
        let normalized = (value as f64 / 8192.0).clamp(-1.0, 1.0);
        self.pitch_bend[channel as usize] = normalized;
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.pitch_bend = normalized;
            voice.frequency_valid = false;
        }
    }

    pub fn set_timbre(&mut self, channel: u8, value: u8) {
        let channel = channel.min(15);
        let timbre = value.min(127) as f64 / 127.0;
        self.timbre[channel as usize] = timbre;
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.timbre = timbre;
        }
    }

    pub fn set_sustain(&mut self, channel: u8, enabled: bool) {
        if let Some((_, range)) = self.mpe_zone_for_master(channel) {
            let (low, high) = range.bounds();
            for member in low..=high {
                self.set_sustain(member, enabled);
            }
            return;
        }
        if let Some(arps) = &mut self.arpeggiators {
            arps[channel.min(15) as usize].set_sustain(enabled);
            return;
        }
        if let Some(player) = &mut self.sample_player {
            player.set_sustain(channel, enabled);
        }
        let channel_index = channel.min(15) as usize;
        let was_enabled = self.sustain[channel_index];
        self.sustain[channel_index] = enabled;
        if was_enabled && !enabled {
            let damping = self.patch.amp_damping;
            let (now_frame, sample_rate) =
                (self.rendered_frames, self.envelope_steps_sample_rate as f64);
            for voice in self
                .voices
                .iter_mut()
                .filter(|voice| voice.channel == channel && !voice.key_down)
            {
                voice.release_with(damping, now_frame, sample_rate);
            }
        }
    }

    /// Return to the state a freshly constructed instrument has, keeping the
    /// patch and its prepared tables.
    ///
    /// A seek that rewinds past audio this instrument already rendered rebuilds
    /// its state instead of the lane, so no wavetable or sample is read again.
    pub fn reset(&mut self) {
        // Both vectors reserve MAX_VOICES during preparation. Move ownership
        // back to the idle pool instead of destroying source engines.
        while let Some(voice) = self.voices.pop() {
            self.spare_voices.push(voice);
        }
        self.event_voices.clear();
        self.last_event_token = None;
        self.age = 0;
        self.held_notes.clear();
        self.pitch_bend.fill(0.0);
        self.mpe_zone_notes.fill([MPE_UNASSIGNED; 128]);
        self.mpe_next.fill(0);
        self.refresh_mpe_config();
        self.sustain.fill(false);
        self.channel_pressure.fill(0.0);
        self.timbre.fill(0.5);
        self.mod_wheel.fill(0.0);
        self.lfo_phase = 0.0;
        self.lfo_phase_accumulator = 0;
        self.extra_lfo_phases.fill(0.0);
        self.extra_lfo_phase_accumulators.fill(0);
        self.step_phase = 0.0;
        self.step_phase_accumulator = 0;
        self.lfo_hold_value = 0.0;
        self.lfo_random_state = 0xd1b5_4a32_d192_ed03;
        self.extra_lfo_hold_values.fill(0.0);
        self.extra_lfo_random_states = std::array::from_fn(|index| {
            0xd1b5_4a32_d192_ed03_u64
                .wrapping_add((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15))
        });
        self.lfo_runtime.fill(LfoRuntime::default());
        for index in 0..10 {
            self.reset_lfo_runtime(index);
        }
        self.fx_mod_countdown = 0;
        self.fx_chain.reset();
        self.rendered_frames = 0;
        self.sub_free_phase = 0.0;
        self.last_sub_increment = 0.0;
        // Derived caches contain no history, but invalidate the rate tags so
        // the next render refreshes exactly as on a fresh instance.
        self.envelope_steps = EnvelopeSteps::default();
        self.envelope_steps_sample_rate = 0;
        self.render_sample_rate = 0;
        self.inv_sample_rate = 0.0;
        self.noise_hp_coeff = 0.0;
        self.modulator_increments_sample_rate = 0;
        self.modulator_increments.fill(0);
        if let Some(player) = &mut self.sample_player {
            player.reset();
        }
        self.sample_scratch.clear();
        if let Some(arps) = &mut self.arpeggiators {
            for arp in arps {
                arp.reset();
                arp.set_clock(self.tempo, 48000);
            }
        }
        self.arp_sample_rate = 48000;
        if let Some(capture) = &mut self.preview_capture {
            for trace in &mut capture.traces {
                trace.clear();
            }
            capture.more_voices = false;
            capture.resets = 0;
        }
    }

    pub fn all_notes_off(&mut self, channel: u8) {
        let channel = channel.min(15);
        if let Some((_, range)) = self.mpe_zone_for_master(channel) {
            let (low, high) = range.bounds();
            for member in low..=high {
                self.all_notes_off(member);
            }
            return;
        }
        if let Some(arps) = &mut self.arpeggiators {
            arps[channel as usize].reset();
        }
        if let Some(player) = &mut self.sample_player {
            player.all_notes_off(channel);
        }
        self.held_notes
            .retain(|&(held_channel, _, _)| held_channel != channel);
        let sustain = self.sustain[channel as usize];
        let damping = self.patch.amp_damping;
        let (now_frame, sample_rate) =
            (self.rendered_frames, self.envelope_steps_sample_rate as f64);
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.key_down = false;
            if !sustain {
                voice.release_with(damping, now_frame, sample_rate);
            }
        }
    }

    pub fn all_sound_off(&mut self, channel: u8) {
        let channel = channel.min(15);
        if let Some((_, range)) = self.mpe_zone_for_master(channel) {
            let (low, high) = range.bounds();
            for member in low..=high {
                self.all_sound_off(member);
            }
            return;
        }
        if let Some(arps) = &mut self.arpeggiators {
            arps[channel as usize].reset();
        }
        if let Some(player) = &mut self.sample_player {
            player.all_sound_off(channel);
        }
        self.held_notes
            .retain(|&(held_channel, _, _)| held_channel != channel);
        self.retain_voices(|voice| voice.channel != channel);
        self.sustain[channel as usize] = false;
        for zone in 0..2 {
            for note in 0..128 {
                if self.mpe_zone_notes[zone][note] == channel {
                    self.mpe_zone_notes[zone][note] = MPE_UNASSIGNED;
                }
            }
        }
    }

    /// Latency the FX rack adds to the voice sum, in samples. The engine does
    /// not delay-compensate synth sources, so a convolution insert shifts the
    /// instrument by this amount.
    pub fn fx_latency_samples(&self) -> usize {
        self.fx_chain.latency_samples()
    }

    pub fn reset_controllers(&mut self, channel: u8) {
        let channel = channel.min(15);
        if let Some((_, range)) = self.mpe_zone_for_master(channel) {
            let (low, high) = range.bounds();
            for member in low..=high {
                self.reset_controllers(member);
            }
            return;
        }
        self.pitch_bend[channel as usize] = 0.0;
        self.channel_pressure[channel as usize] = 0.0;
        self.timbre[channel as usize] = 0.5;
        self.mod_wheel[channel as usize] = 0.0;
        self.set_sustain(channel, false);
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.pressure = 0.0;
            voice.channel_pressure = 0.0;
            voice.pitch_bend = 0.0;
            voice.timbre = 0.5;
            voice.expression_tuning = 0.0;
            voice.expression_brightness = 0.5;
            voice.frequency_valid = false;
            voice.filter_coefficients_valid = false;
        }
    }

    pub fn set_channel_pressure(&mut self, channel: u8, pressure: u8) {
        let channel = channel.min(15);
        let pressure = pressure.min(127) as f64 / 127.0;
        self.channel_pressure[channel as usize] = pressure;
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.channel_pressure = pressure;
        }
        self.invalidate_channel_filter_coefficients(channel);
    }

    pub fn set_mod_wheel(&mut self, channel: u8, value: u8) {
        self.mod_wheel[channel.min(15) as usize] = value as f64 / 127.0;
    }

    pub fn set_poly_pressure(&mut self, channel: u8, note: u8, pressure: u8) {
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel && voice.note == note)
        {
            voice.pressure = pressure as f64 / 127.0;
            voice.filter_coefficients_valid = false;
        }
    }

    pub fn set_note_expression(&mut self, channel: u8, note: u8, expression: &str, value: f64) {
        for voice in self.voices.iter_mut().filter(|voice| {
            voice.channel == channel
                && voice.note == note
                && (expression != "pitch_curve" || voice.key_down)
        }) {
            match expression {
                "pitch_curve"
                    // Parser validates this range; direct callers must not inject
                    // nonfinite values or silently clamp a different melody.
                    if value.is_finite() && value.abs() <= crate::note_pitch_curve::MAX_SEMITONES => {
                        voice.expression_tuning = value;
                        voice.frequency_valid = false;
                    }
                "tuning" | "pitch" => {
                    voice.expression_tuning = value.clamp(-4.0, 4.0);
                    voice.frequency_valid = false;
                }
                "brightness" | "timbre" => {
                    let value = value.clamp(0.0, 1.0);
                    voice.expression_brightness = value;
                    voice.timbre = value;
                    voice.filter_coefficients_valid = false;
                }
                "pressure" | "expression" => voice.pressure = value.clamp(0.0, 1.0),
                _ => {}
            }
        }
    }

    /// Install a measured loudness correction on the patch master.
    pub fn set_loudness_trim_db(&mut self, db: f64) {
        if db.is_finite() {
            self.patch.loudness_gain = 10.0_f64.powf(db / 20.0);
        }
    }

    pub fn set_param(&mut self, name: &str, value: f64) {
        self.patch.set_param(name, value);
        if name.starts_with("mpe") {
            self.refresh_mpe_config();
        }
        if name.starts_with("fx.") {
            self.fx_chain.sync_from_patch(&self.patch.fx);
            self.refresh_fx_mod_routes();
        } else if name.starts_with("fxmod.") {
            self.refresh_fx_mod_routes();
        }
        if name.starts_with("mod.") {
            self.refresh_active_mod_routes();
        }
        if matches!(
            name,
            "unison.detune"
                | "detune"
                | "osc.a.detune"
                | "unison.voices"
                | "osc.a.unison"
                | "osc.a.width"
                | "osc.b.unison"
                | "osc.b.detune"
                | "osc.b.width"
                | "osc.c.unison"
                | "osc.c.detune"
                | "osc.c.width"
        ) {
            self.refresh_unison_cache();
        }
        if name == "osc.a.semitones" {
            self.refresh_oscillator_a_ratio();
        }
        if name == "osc.b.semitones" {
            self.refresh_oscillator_b_ratio();
        }
        if name == "osc.c.semitones" {
            self.refresh_oscillator_c_ratio();
        }
        if name == "sub.semitones" || name == "sub.fine" || name == "sub.fine_cents" {
            self.refresh_sub_ratio();
        }
        if name == "waveform" || name.ends_with(".waveform") {
            self.refresh_source_voices();
        }
        if is_envelope_duration_param(name) {
            self.invalidate_envelope_steps();
        }
        self.modulator_increments_sample_rate = 0;
        if matches!(
            name,
            "amp.curve" | "filter_env.curve" | "env3.curve" | "env4.curve"
        ) {
            self.rebuild_envelope_curves();
        }
        if name == "lfo.pitch" {
            self.refresh_pitch_modulation_state();
        }
        if matches!(name, "voice.polyphony" | "voice.mono") {
            self.enforce_voice_limit();
        }
        if matches!(
            name,
            "tuning.reference_hz" | "tuning.transpose" | "tuning.fine"
        ) {
            self.invalidate_voice_frequencies();
        }
        if matches!(
            name,
            "filter.cutoff"
                | "cutoff"
                | "filter.resonance"
                | "resonance"
                | "filter.keytrack"
                | "filter.keytrack.origin"
                | "filter2.cutoff"
                | "filter2.resonance"
        ) {
            self.invalidate_filter_coefficients();
        }
        if name == "filter.env.amount" {
            self.refresh_filter_modulation_state();
        }
        self.refresh_warp_cache();
    }

    pub fn set_normalized_param(&mut self, name: &str, value: f64) {
        self.patch.set_normalized_param(name, value);
        if name.starts_with("mod.") {
            self.refresh_active_mod_routes();
        }
        if matches!(
            name,
            "unison.detune"
                | "detune"
                | "osc.a.detune"
                | "unison.voices"
                | "osc.a.unison"
                | "osc.a.width"
                | "osc.b.unison"
                | "osc.b.detune"
                | "osc.b.width"
                | "osc.c.unison"
                | "osc.c.detune"
                | "osc.c.width"
        ) {
            self.refresh_unison_cache();
        }
        if name == "osc.a.semitones" {
            self.refresh_oscillator_a_ratio();
        }
        if name == "osc.b.semitones" {
            self.refresh_oscillator_b_ratio();
        }
        if name == "osc.c.semitones" {
            self.refresh_oscillator_c_ratio();
        }
        if name == "sub.semitones" || name == "sub.fine" || name == "sub.fine_cents" {
            self.refresh_sub_ratio();
        }
        if name == "waveform" || name.ends_with(".waveform") {
            self.refresh_source_voices();
        }
        if is_envelope_duration_param(name) {
            self.invalidate_envelope_steps();
        }
        self.modulator_increments_sample_rate = 0;
        if matches!(
            name,
            "amp.curve" | "filter_env.curve" | "env3.curve" | "env4.curve"
        ) {
            self.rebuild_envelope_curves();
        }
        if name == "lfo.pitch" {
            self.refresh_pitch_modulation_state();
        }
        if matches!(name, "voice.polyphony" | "voice.mono") {
            self.enforce_voice_limit();
        }
        if matches!(
            name,
            "tuning.reference_hz" | "tuning.transpose" | "tuning.fine"
        ) {
            self.invalidate_voice_frequencies();
        }
        if matches!(
            name,
            "filter.cutoff"
                | "cutoff"
                | "filter.resonance"
                | "resonance"
                | "filter.keytrack"
                | "filter.keytrack.origin"
                | "filter2.cutoff"
                | "filter2.resonance"
        ) {
            self.invalidate_filter_coefficients();
        }
        if name == "filter.env.amount" {
            self.refresh_filter_modulation_state();
        }
        self.refresh_warp_cache();
    }

    pub fn is_idle(&self) -> bool {
        self.voices.is_empty()
            && !self
                .arpeggiators
                .as_ref()
                .is_some_and(|arps| arps.iter().any(arpeggiator::Arpeggiator::has_notes))
            && !self.fx_chain.tail_active()
            && !self
                .sample_player
                .as_ref()
                .is_some_and(crate::sampler::Sampler::is_active)
    }

    /// Start recording one oscillator slot's per-unison contributions for the
    /// next `frames` samples. Offline preview only; allocates.
    pub(crate) fn start_unison_capture(&mut self, slot: usize, frames: usize) {
        let slot = slot.min(2);
        let voices = match slot {
            0 => self.unison_voice_count,
            1 => self.unison_b_count,
            _ => self.unison_c_count,
        }
        .clamp(1, MAX_UNISON_VOICES);
        let traced = voices.min(MAX_PREVIEW_TRACES);
        self.preview_capture = Some(UnisonCapture {
            slot: slot as u8,
            frames,
            voices,
            traces: (0..traced).map(|_| Vec::with_capacity(frames)).collect(),
            more_voices: voices > traced,
            resets: 0,
        });
    }

    /// Stop recording and return the captured traces.
    pub(crate) fn take_unison_capture(&mut self) -> Option<UnisonCapture> {
        self.preview_capture.take()
    }

    /// Render an interleaved stereo block without allocation. Entirely idle
    /// spans advance global modulators in O(number of wraps), not O(frames).
    pub fn render_interleaved(&mut self, output: &mut [f32], sample_rate: u32) {
        let frames = output.len() / 2;
        if self.is_idle() {
            output.fill(0.0);
            self.advance_idle(frames, sample_rate);
            return;
        }
        let mut frame_index = 0;
        while frame_index < frames {
            if self.is_idle() {
                output[frame_index * 2..frames * 2].fill(0.0);
                self.advance_idle(frames - frame_index, sample_rate);
                break;
            }
            let [left, right] = self.next_stereo(sample_rate);
            output[frame_index * 2] = left;
            output[frame_index * 2 + 1] = right;
            frame_index += 1;
        }
        if !output.len().is_multiple_of(2) {
            output[output.len() - 1] = 0.0;
        }
    }

    /// Rebuild the cached warp parameters. A route that can move a warp amount
    /// disables the cache for that oscillator, because the amount comes from
    /// the voice loop instead of the patch.
    fn refresh_warp_cache(&mut self) {
        let (mut a, mut b, mut c) = (false, false, false);
        for route in &self.compiled_mod_routes[..self.compiled_mod_route_count] {
            match route.destination {
                ModDestination::WarpAmountA | ModDestination::Warp2AmountA => a = true,
                ModDestination::WarpAmountB | ModDestination::Warp2AmountB => b = true,
                ModDestination::WarpAmountC | ModDestination::Warp2AmountC => c = true,
                _ => {}
            }
        }
        self.warp_a_modulated = a;
        self.warp_b_modulated = b;
        self.warp_c_modulated = c;
        let patch = &self.patch;
        self.warp_a_base = WarpParams::new(
            patch.oscillator_a_warp_mode,
            patch.oscillator_a_warp_amount,
            patch.oscillator_a_warp_var,
            patch.oscillator_a_warp2_mode,
            patch.oscillator_a_warp2_amount,
            patch.oscillator_a_warp2_var,
        );
        self.warp_b_base = WarpParams::new(
            patch.oscillator_b_warp_mode,
            patch.oscillator_b_warp_amount,
            patch.oscillator_b_warp_var,
            patch.oscillator_b_warp2_mode,
            patch.oscillator_b_warp2_amount,
            patch.oscillator_b_warp2_var,
        );
        self.warp_c_base = WarpParams::new(
            patch.oscillator_c_warp_mode,
            patch.oscillator_c_warp_amount,
            patch.oscillator_c_warp_var,
            patch.oscillator_c_warp2_mode,
            patch.oscillator_c_warp2_amount,
            patch.oscillator_c_warp2_var,
        );
    }

    fn refresh_active_mod_routes(&mut self) {
        self.active_mod_route_count = 0;
        self.active_source_mask = 0;
        self.oscillator_b_tuning_modulated = false;
        self.compiled_mod_route_count = 0;
        for (index, route) in self
            .patch
            .mod_routes
            .iter()
            .take(MAX_MOD_ROUTES)
            .enumerate()
        {
            if route.amount == 0.0 {
                continue;
            }
            // Pre-scale units so the voice loop is a plain multiply-add.
            let scale = match route.destination {
                ModDestination::FilterCutoff | ModDestination::Filter2Cutoff => 11.4,
                ModDestination::Pitch => 12.0,
                ModDestination::PulseWidthA => 0.45,
                ModDestination::OscillatorBTuning => 24.0,
                ModDestination::SourceDensityA
                | ModDestination::SourceDensityB
                | ModDestination::SourceDensityC => 512.0,
                ModDestination::SourcePositionA
                | ModDestination::SourcePositionB
                | ModDestination::SourcePositionC => 2.0,
                ModDestination::SourceBendA
                | ModDestination::SourceBendB
                | ModDestination::SourceBendC => 24.0,
                _ => 1.0,
            };
            self.active_mod_routes[self.active_mod_route_count] = index;
            self.active_mod_route_count += 1;
            // Keep one compiled record per route instead of summing by
            // `(source, destination)`: two routes that share a source and
            // destination may still differ in polarity, auxiliary scaling or
            // inversion, so they cannot be merged.
            self.compiled_mod_routes[self.compiled_mod_route_count] = CompiledModRoute {
                source: route.source as u8,
                auxiliary: route.auxiliary.map(|source| source as u8),
                auxiliary_inverted: route.auxiliary.is_some() && route.auxiliary_inverted,
                bipolar: route.bipolar,
                destination: route.destination,
                amount: route.amount * scale,
                curve: route
                    .curve
                    .clamp(BIPOLAR_MODULATION.min, BIPOLAR_MODULATION.max),
                slew_up_seconds: route.slew_up.clamp(0.0, MAX_ROUTE_SLEW_SECONDS),
                slew_down_seconds: route.slew_down.clamp(0.0, MAX_ROUTE_SLEW_SECONDS),
                slew_up: 1.0,
                slew_down: 1.0,
            };
            self.compiled_mod_route_count += 1;
            self.active_source_mask |= 1 << (route.source as usize);
            if let Some(auxiliary) = route.auxiliary {
                self.active_source_mask |= 1 << (auxiliary as usize);
            }
            self.oscillator_b_tuning_modulated |=
                route.destination == ModDestination::OscillatorBTuning;
        }
        self.route_group_count = 0;
        let mut first = 0;
        while first < self.compiled_mod_route_count {
            let mut end = first + 1;
            if self.compiled_mod_routes[first].immediate_slew() {
                while end < self.compiled_mod_route_count
                    && self.compiled_mod_routes[first].same_route(&self.compiled_mod_routes[end])
                {
                    end += 1;
                }
            }
            self.route_groups[self.route_group_count] = RouteGroup {
                first: first as u8,
                len: (end - first) as u8,
            };
            self.route_group_count += 1;
            first = end;
        }
        self.refresh_warp_cache();
        self.refresh_pitch_modulation_state();
        self.refresh_filter_modulation_state();
    }

    /// Recompile the FX modulation routes. Called only from load/edit paths;
    /// the audio thread never allocates or walks the patch vectors for this.
    fn refresh_fx_mod_routes(&mut self) {
        self.compiled_fx_mod_route_count = 0;
        self.fx_mod_source_mask = 0;
        self.fx_mod_countdown = 0;
        for route_index in 0..self.patch.fx_mod_routes.len().min(MAX_FX_MOD_ROUTES) {
            let route = self.patch.fx_mod_routes[route_index];
            if !route.amount.is_finite() || route.amount == 0.0 {
                continue;
            }
            if route.fx_index >= fx::MAX_SYNTH_FX {
                continue;
            }
            // Drop routes whose destination module no longer exposes the
            // native parameter (for example after a live `fx.N.type` change).
            let Some(module) = self.patch.fx.get(route.fx_index) else {
                continue;
            };
            if !module
                .params
                .iter()
                .any(|(name, _)| name == route.parameter.native_name())
            {
                continue;
            }
            self.compiled_fx_mod_routes[self.compiled_fx_mod_route_count] = CompiledFxModRoute {
                source: route.source as u8,
                fx_index: route.fx_index as u8,
                parameter: route.parameter,
                amount: route.amount,
                bipolar: route.bipolar,
            };
            self.compiled_fx_mod_route_count += 1;
            self.fx_mod_source_mask |= 1 << (route.source as u32);
        }
        // Clear any values left by routes that were just removed.
        self.fx_chain.reset_modulation();
    }

    /// Sample every modulation source once for the global FX rack. The rack is
    /// post-voice-sum, so per-voice sources are approximated from the newest
    /// active voice (velocity, keytrack) or the maximum across voices
    /// (envelopes, pressure). Macros/LFOs/step/mod-wheel are exact globals.
    fn fx_modulation_sources(&self) -> [f64; MOD_SOURCE_TABLE.len()] {
        let mut values = [0.0; MOD_SOURCE_TABLE.len()];
        values[ModSource::Lfo as usize] = self.lfo_runtime[0].output;
        for index in 0..9 {
            values[ModSource::Lfo2 as usize + index] = self.lfo_runtime[index + 1].output;
        }
        values[ModSource::Step as usize] = step_value_at(
            &self.patch.step_values,
            self.step_phase,
            self.patch.step_smoothing,
        );
        values[ModSource::Macro1 as usize] = self.patch.macros[0];
        values[ModSource::Macro2 as usize] = self.patch.macros[1];
        values[ModSource::Macro3 as usize] = self.patch.macros[2];
        values[ModSource::Macro4 as usize] = self.patch.macros[3];
        values[ModSource::Macro5 as usize] = self.patch.extra_macros[0];
        values[ModSource::Macro6 as usize] = self.patch.extra_macros[1];
        values[ModSource::Macro7 as usize] = self.patch.extra_macros[2];
        values[ModSource::Macro8 as usize] = self.patch.extra_macros[3];
        values[ModSource::ModWheel as usize] = self.mod_wheel.iter().copied().fold(0.0, f64::max);
        let mut newest: Option<&Voice> = None;
        let mut aftertouch = self.channel_pressure.iter().copied().fold(0.0, f64::max);
        for voice in &self.voices {
            values[ModSource::FilterEnvelope as usize] =
                values[ModSource::FilterEnvelope as usize].max(voice.filter_level);
            values[ModSource::Env3 as usize] =
                values[ModSource::Env3 as usize].max(voice.env3_level);
            values[ModSource::Env4 as usize] =
                values[ModSource::Env4 as usize].max(voice.env4_level);
            aftertouch = aftertouch.max(voice.pressure.max(voice.channel_pressure));
            if newest.is_none_or(|current| voice.age > current.age) {
                newest = Some(voice);
            }
        }
        if let Some(voice) = newest {
            values[ModSource::Velocity as usize] = voice.velocity;
            values[ModSource::Keytrack as usize] = (voice.note as f64 - 60.0) / 60.0;
            values[ModSource::PitchBend as usize] = voice.pitch_bend;
            values[ModSource::Timbre as usize] = voice.timbre * 2.0 - 1.0;
        }
        values[ModSource::Aftertouch as usize] = aftertouch;
        values
    }

    /// Apply all compiled FX routes to the rack. Called at control rate, not per
    /// sample. `reset_modulation` restores the imported base values first, so
    /// multiple routes to one destination sum without accumulating over time.
    fn apply_fx_modulation(&mut self) {
        if self.compiled_fx_mod_route_count == 0 {
            return;
        }
        self.fx_chain.reset_modulation();
        let sources = self.fx_modulation_sources();
        for route_index in 0..self.compiled_fx_mod_route_count {
            let route = self.compiled_fx_mod_routes[route_index];
            let source_enum = MOD_SOURCE_TABLE[route.source as usize];
            let mut source = sources[route.source as usize];
            if route.bipolar && !mod_source_is_bipolar(source_enum) {
                source = source * 2.0 - 1.0;
            }
            let name = route.parameter.native_name();
            let Some(base) = self.fx_chain.module_value(route.fx_index as usize, name) else {
                continue;
            };
            self.fx_chain.set_module_param(
                route.fx_index as usize,
                name,
                route.parameter.apply(base, source * route.amount),
            );
        }
    }

    fn rebuild_envelope_curves(&mut self) {
        self.envelope_curves[0].rebuild(self.patch.amp_curve);
        self.envelope_curves[1].rebuild(self.patch.filter_curve);
        self.envelope_curves[2].rebuild(self.patch.env3_curve);
        self.envelope_curves[3].rebuild(self.patch.env4_curve);
    }

    fn rebuild_envelope_steps(&mut self, sample_rate: u32) {
        let sr = sample_rate as f64;
        self.envelope_steps = EnvelopeSteps {
            amplitude: EnvelopeStageSteps::with_min_attack(
                self.patch.attack,
                self.patch.decay,
                self.patch.sustain,
                self.patch.release,
                sr,
                MIN_ENVELOPE_ATTACK_SECONDS,
            ),
            filter: EnvelopeStageSteps::new(
                self.patch.filter_attack,
                self.patch.filter_decay,
                self.patch.filter_sustain,
                self.patch.filter_release,
                sr,
            ),
            env3: EnvelopeStageSteps::new(
                self.patch.env3_attack,
                self.patch.env3_decay,
                self.patch.env3_sustain,
                self.patch.env3_release,
                sr,
            ),
            env4: EnvelopeStageSteps::new(
                self.patch.env4_attack,
                self.patch.env4_decay,
                self.patch.env4_sustain,
                self.patch.env4_release,
                sr,
            ),
        };
        self.envelope_steps_sample_rate = sample_rate;
    }

    fn invalidate_envelope_steps(&mut self) {
        self.envelope_steps_sample_rate = 0;
    }

    fn rebuild_modulator_increments(&mut self, sample_rate: u32) {
        let inv_sr = 1.0 / sample_rate as f64;
        self.modulator_increments[0] = phase_increment_scaled(self.lfo_rate(), inv_sr);
        for index in 0..9 {
            self.modulator_increments[index + 1] =
                phase_increment_scaled(self.extra_lfo_rate(index), inv_sr);
        }
        self.modulator_increments[10] = phase_increment_scaled(self.step_rate() / 16.0, inv_sr);
        let sr = sample_rate as f64;
        for route in &mut self.compiled_mod_routes[..self.compiled_mod_route_count] {
            route.slew_up = slew_coefficient(route.slew_up_seconds, sr);
            route.slew_down = slew_coefficient(route.slew_down_seconds, sr);
        }
        self.rebuild_lfo_runtime(sample_rate);
        self.modulator_increments_sample_rate = sample_rate;
    }

    /// One-pole coefficients for each LFO; called outside the loop.
    fn rebuild_lfo_runtime(&mut self, sample_rate: u32) {
        let sr = sample_rate.max(1) as f64;
        for index in 0..10 {
            let smooth = if index == 0 {
                self.patch.lfo_smooth
            } else {
                self.patch.extra_lfos[index - 1].smooth
            };
            let runtime = &mut self.lfo_runtime[index];
            runtime.smooth_coeff = smoothing_coefficient(smooth, MAX_LFO_SMOOTH_SECONDS, sr);
        }
    }

    fn refresh_pitch_modulation_state(&mut self) {
        self.pitch_modulated = self.patch.lfo_pitch_cents != 0.0
            || self.active_mod_routes[..self.active_mod_route_count]
                .iter()
                .any(|index| self.patch.mod_routes[*index].destination == ModDestination::Pitch);
    }

    fn invalidate_voice_frequencies(&mut self) {
        for voice in &mut self.voices {
            voice.frequency_valid = false;
        }
    }

    fn refresh_filter_modulation_state(&mut self) {
        self.filter_modulated = self.patch.filter_envelope_amount != 0.0
            || self.active_mod_routes[..self.active_mod_route_count]
                .iter()
                .any(|index| {
                    matches!(
                        self.patch.mod_routes[*index].destination,
                        ModDestination::FilterCutoff
                            | ModDestination::Resonance
                            | ModDestination::Filter2Cutoff
                            | ModDestination::Filter2Resonance
                    )
                });
        self.invalidate_filter_coefficients();
    }

    fn invalidate_filter_coefficients(&mut self) {
        for voice in &mut self.voices {
            voice.filter_coefficients_valid = false;
            voice.filter_coefficient_age = 0;
        }
    }

    fn invalidate_channel_filter_coefficients(&mut self, channel: u8) {
        let channel = channel.min(15);
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel)
        {
            voice.filter_coefficients_valid = false;
            voice.filter_coefficient_age = 0;
        }
    }

    /// Release sample voices whose synth voice ended or was stolen.
    fn prune_sample_owners(&mut self) {
        let mut live = [0u64; MAX_VOICES];
        let mut count = 0;
        for voice in &self.voices {
            if count < live.len() {
                live[count] = voice.age;
                count += 1;
            }
        }
        if let Some(player) = &mut self.sample_player {
            player.retain_owners(&live[..count]);
        }
    }

    fn enforce_voice_limit(&mut self) {
        let limit = if self.patch.monophonic {
            1
        } else {
            self.patch.polyphony.clamp(1, MAX_VOICES)
        };
        while self.voices.len() > limit {
            let index = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, voice)| (voice.amplitude_stage != EnvStage::Release, voice.age))
                .map(|(index, _)| index)
                .unwrap_or(0);
            let voice = self.voices.remove(index);
            self.recycle_voice(voice);
        }
    }

    fn refresh_sub_ratio(&mut self) {
        let semitones =
            self.patch.sub_semitones + self.patch.sub_fine_cents.clamp(-100.0, 100.0) / 100.0;
        self.sub_ratio = 2f64.powf(semitones / 12.0);
    }

    fn refresh_oscillator_a_ratio(&mut self) {
        self.oscillator_a_ratio = 2f64.powf(self.patch.oscillator_a_semitones / 12.0);
    }

    fn refresh_oscillator_b_ratio(&mut self) {
        self.oscillator_b_ratio = 2f64.powf(self.patch.oscillator_b_semitones / 12.0);
    }

    fn refresh_oscillator_c_ratio(&mut self) {
        self.oscillator_c_ratio = 2f64.powf(self.patch.oscillator_c_semitones / 12.0);
    }

    fn refresh_unison_cache(&mut self) {
        let (positions, ratios, scale) = unison_layout(
            self.patch.unison_voices.clamp(1, MAX_UNISON_VOICES),
            self.patch.detune_cents,
        );
        self.unison_voice_count = self.patch.unison_voices.clamp(1, MAX_UNISON_VOICES);
        self.unison_positions = positions;
        self.unison_ratios = ratios;
        self.unison_scale = scale;
        self.unison_weights = unison_voice_weights(self.unison_voice_count);
        let (positions, ratios, scale) = unison_layout(
            self.patch.oscillator_b_unison.clamp(1, MAX_UNISON_VOICES),
            self.patch.oscillator_b_detune_cents,
        );
        self.unison_b_count = self.patch.oscillator_b_unison.clamp(1, MAX_UNISON_VOICES);
        self.unison_b_positions = positions;
        self.unison_b_ratios = ratios;
        self.unison_b_scale = scale;
        self.unison_b_weights = unison_voice_weights(self.unison_b_count);
        let (positions, ratios, scale) = unison_layout(
            self.patch.oscillator_c_unison.clamp(1, MAX_UNISON_VOICES),
            self.patch.oscillator_c_detune_cents,
        );
        self.unison_c_count = self.patch.oscillator_c_unison.clamp(1, MAX_UNISON_VOICES);
        self.unison_c_positions = positions;
        self.unison_c_ratios = ratios;
        self.unison_c_scale = scale;
        self.unison_c_weights = unison_voice_weights(self.unison_c_count);
    }

    fn advance_idle(&mut self, frames: usize, sample_rate: u32) {
        if frames == 0 {
            return;
        }
        let lfo_increment = phase_increment(self.lfo_rate(), sample_rate);
        let total_lfo = self.lfo_phase_accumulator as u128 + lfo_increment as u128 * frames as u128;
        let mut wraps = [0usize; 10];
        wraps[0] = (total_lfo >> 64) as usize;
        for _ in 0..wraps[0] {
            self.advance_lfo_random();
        }
        self.lfo_phase_accumulator = total_lfo as u64;
        self.lfo_phase = phase_from_accumulator(self.lfo_phase_accumulator);
        let extra_rates: [f64; 9] = std::array::from_fn(|index| self.extra_lfo_rate(index));
        for index in 0..9 {
            let increment = phase_increment(extra_rates[index], sample_rate);
            let total = self.extra_lfo_phase_accumulators[index] as u128
                + increment as u128 * frames as u128;
            wraps[index + 1] = (total >> 64) as usize;
            for _ in 0..wraps[index + 1] {
                self.advance_extra_lfo_random(index);
            }
            self.extra_lfo_phase_accumulators[index] = total as u64;
            self.extra_lfo_phases[index] =
                phase_from_accumulator(self.extra_lfo_phase_accumulators[index]);
        }
        let step_increment = phase_increment(self.step_rate() / 16.0, sample_rate);
        let total_step =
            self.step_phase_accumulator as u128 + step_increment as u128 * frames as u128;
        self.step_phase_accumulator = total_step as u64;
        self.step_phase = phase_from_accumulator(self.step_phase_accumulator);
        let sub_phase = self.sub_free_phase + self.last_sub_increment * frames as f64;
        self.sub_free_phase = sub_phase.fract();
        let age_step = frames as f64 / sample_rate.max(1) as f64;
        for runtime in &mut self.lfo_runtime {
            runtime.age += age_step;
        }
        // Keep the chaotic maps and the LFO phases on the same timeline.
        for index in 0..10 {
            let shape_type = self.lfo_shape_type(index);
            let rate = if index == 0 {
                self.lfo_rate()
            } else {
                self.extra_lfo_rate(index - 1)
            };
            self.lfo_runtime[index].advance_chaos_frames(shape_type, rate, frames, sample_rate);
        }
    }

    fn lfo_rate(&self) -> f64 {
        if self.patch.lfo_sync {
            synced_lfo_rate(self.tempo, self.patch.lfo_division, self.patch.lfo_sync_div)
        } else {
            free_lfo_rate(self.patch.lfo_rate, self.patch.lfo_rate_x10)
        }
    }

    fn extra_lfo_rate(&self, index: usize) -> f64 {
        let settings = &self.patch.extra_lfos[index];
        if settings.sync {
            synced_lfo_rate(self.tempo, settings.division, settings.sync_div)
        } else {
            free_lfo_rate(settings.rate, settings.rate_x10)
        }
    }

    fn lfo_shape_type(&self, index: usize) -> LfoShapeType {
        if index == 0 {
            self.patch.lfo_shape_type
        } else {
            self.patch.extra_lfos[index - 1].shape_type
        }
    }

    fn lfo_start_phase(&self, index: usize) -> f64 {
        if index == 0 {
            self.patch.lfo_phase
        } else {
            self.patch.extra_lfos[index - 1].phase
        }
    }

    fn step_rate(&self) -> f64 {
        if self.patch.step_sync {
            const BEATS_PER_STEP: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
            self.tempo / 60.0 / BEATS_PER_STEP[self.patch.step_division.min(5)]
        } else {
            self.patch.step_rate
        }
    }

    fn advance_lfo_random(&mut self) {
        self.lfo_random_state ^= self.lfo_random_state << 13;
        self.lfo_random_state ^= self.lfo_random_state >> 7;
        self.lfo_random_state ^= self.lfo_random_state << 17;
        self.lfo_hold_value = self.lfo_random_state as f64 / u64::MAX as f64;
    }

    fn advance_extra_lfo_random(&mut self, index: usize) {
        let mut state = self.extra_lfo_random_states[index];
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.extra_lfo_random_states[index] = state;
        self.extra_lfo_hold_values[index] = state as f64 / u64::MAX as f64;
    }

    /// Restart one LFO's fade-in, one-pole memories and generator after a phase reset.
    fn reset_lfo_runtime(&mut self, index: usize) {
        let shape_type = self.lfo_shape_type(index);
        let seed = lfo_chaos_seed(index);
        if index == 0 {
            if shape_type == LfoShapeType::RandomHold {
                self.lfo_random_state = lfo_random_seed(0);
                self.lfo_hold_value = 0.5;
            }
        } else if shape_type == LfoShapeType::RandomHold {
            self.extra_lfo_random_states[index - 1] = lfo_random_seed(index);
            self.extra_lfo_hold_values[index - 1] = 0.5;
        }
        let runtime = &mut self.lfo_runtime[index];
        runtime.age = 0.0;
        runtime.output = 0.5;
        runtime.smooth_state = 0.5;
        runtime.chaos = seed;
        runtime.chaos_curr = chaos_step(shape_type, &mut runtime.chaos);
        runtime.chaos_next = chaos_step(shape_type, &mut runtime.chaos);
        runtime.chaos_frac = 0.0;
    }

    /// Shape every LFO for the current sample; the voice loop only reads outputs.
    fn update_lfo_runtime(&mut self, inv_sample_rate: f64) {
        for index in 0..10 {
            let (shape_type, phase, hold, rise, delay, bend, rate) = if index == 0 {
                (
                    self.patch.lfo_shape_type,
                    self.lfo_phase,
                    self.lfo_hold_value,
                    self.patch.lfo_rise,
                    self.patch.lfo_delay,
                    self.patch.lfo_bend,
                    self.lfo_rate(),
                )
            } else {
                let settings = &self.patch.extra_lfos[index - 1];
                (
                    settings.shape_type,
                    self.extra_lfo_phases[index - 1],
                    self.extra_lfo_hold_values[index - 1],
                    settings.rise,
                    settings.delay,
                    settings.bend,
                    self.extra_lfo_rate(index - 1),
                )
            };
            let shape = if index == 0 {
                &self.patch.lfo_shape
            } else {
                &self.patch.extra_lfos[index - 1].shape
            };
            let runtime = &mut self.lfo_runtime[index];
            runtime.age += inv_sample_rate;
            let phase = warp_lfo_phase(phase, bend);
            let shaped = match shape_type {
                LfoShapeType::Curve => curve_shape_value(shape, phase),
                LfoShapeType::Path => path_shape_value(shape, phase),
                LfoShapeType::ChaosA | LfoShapeType::ChaosB => {
                    runtime.advance_chaos(shape_type, rate, inv_sample_rate);
                    runtime.chaos_output()
                }
                LfoShapeType::RandomHold => hold.clamp(0.0, 1.0),
            };
            // Fade around the midpoint, so a starting LFO sits at neutral 0.5.
            let fade = lfo_fade(runtime.age, delay, rise);
            let faded = if fade >= 1.0 {
                shaped
            } else if fade <= 0.0 {
                0.5
            } else {
                0.5 + (shaped - 0.5) * fade
            };
            if runtime.smooth_coeff >= 1.0 {
                runtime.smooth_state = faded;
                runtime.output = faded;
            } else {
                runtime.smooth_state += (faded - runtime.smooth_state) * runtime.smooth_coeff;
                runtime.output = runtime.smooth_state;
            }
        }
    }

    pub fn next_stereo(&mut self, sample_rate: u32) -> [f32; 2] {
        self.rendered_frames = self.rendered_frames.wrapping_add(1);
        if self.arpeggiators.is_some() {
            if self.arp_sample_rate != sample_rate {
                for arp in self.arpeggiators.as_mut().unwrap() {
                    arp.set_clock(self.tempo, sample_rate);
                }
                self.arp_sample_rate = sample_rate;
            }
            for channel in 0..16 {
                let events = self.arpeggiators.as_mut().unwrap()[channel].tick();
                for event in events.into_iter().flatten() {
                    match event {
                        arpeggiator::ArpEvent::On(note, velocity) => {
                            self.voice_note_on(channel as u8, note, velocity)
                        }
                        arpeggiator::ArpEvent::Off(note) => {
                            self.voice_note_off(channel as u8, note)
                        }
                    }
                }
            }
        }
        let sr = sample_rate as f64;
        if self.envelope_steps_sample_rate != sample_rate {
            self.rebuild_envelope_steps(sample_rate);
        }
        if self.render_sample_rate != sample_rate {
            self.render_sample_rate = sample_rate;
            self.inv_sample_rate = 1.0 / sr;
            self.noise_hp_coeff = 1.0 - (-std::f64::consts::TAU * NOISE_HP_HZ / sr).exp();
        }
        let inv_sr = self.inv_sample_rate;
        let noise_hp_coeff = self.noise_hp_coeff;
        if self.modulator_increments_sample_rate != sample_rate {
            self.rebuild_modulator_increments(sample_rate);
        }
        self.update_lfo_runtime(inv_sr);
        // FX parameters are global and evaluated at control rate. The rack
        // itself still runs per sample; only its parameter values step.
        if self.compiled_fx_mod_route_count > 0 && self.fx_chain.is_active() {
            if self.fx_mod_countdown == 0 {
                self.apply_fx_modulation();
                self.fx_mod_countdown = FX_MOD_CONTROL_INTERVAL - 1;
            } else {
                self.fx_mod_countdown -= 1;
            }
        }
        let mut left = 0.0;
        let mut right = 0.0;
        let lfo = self.lfo_runtime[0].output;
        let mut lfo_values = [0.0; 10];
        // LFOs are unipolar 0..1 like macros; a route's `bipolar` flag
        // re-centres them. Direct pitch vibrato uses the centred value.
        lfo_values[0] = lfo;
        for index in 0..9 {
            lfo_values[index + 1] = self.lfo_runtime[index + 1].output;
        }
        let lfo_pitch = (2.0 * lfo - 1.0) * self.patch.lfo_pitch_cents / 100.0;
        let step = if self.active_source_mask & (1 << 10) != 0 {
            step_value_at(
                &self.patch.step_values,
                self.step_phase,
                self.patch.step_smoothing,
            )
        } else {
            0.0
        };
        // Sample voices are grouped by owning synth voice so each group is
        // filtered and enveloped once, below.
        let mut sample_owner_count = 0;
        if let Some(player) = &mut self.sample_player {
            sample_owner_count = player.next_owned_stereo(sample_rate, &mut self.sample_scratch);
        }
        for voice in &mut self.voices {
            if !voice.tick_envelopes(&self.envelope_steps, &self.envelope_curves) {
                continue;
            }
            let envelopes = [
                voice.amplitude_level,
                voice.filter_level,
                voice.env3_level,
                voice.env4_level,
            ];
            let bend = voice.pitch_bend * self.mpe_bend_range;
            let expressive_pressure = voice.pressure.max(voice.channel_pressure);
            let mut cutoff_octaves = self.patch.filter_envelope_amount * voice.filter_level
                + (voice.expression_brightness - 0.5) * 4.0
                + expressive_pressure * 1.5;
            let mut pitch_semitones = bend + lfo_pitch + voice.expression_tuning;
            // Level slots are gains, so a level route adds in the amplitude
            // domain: L^2 restores the gain the patch stores.
            let mut oscillator_mix = self
                .patch
                .oscillator_b_level
                .max(AMPLITUDE_FRACTION.min)
                .sqrt();
            let mut resonance = self.patch.filter_resonance;
            let mut stereo_width = self.patch.stereo_width;
            let mut pulse_width_a = self.patch.oscillator_a_pulse_width;
            let mut ring_mod_level = self.patch.ring_mod_level;
            let mut wavetable_position_a = self.patch.oscillator_a_wavetable_position;
            let mut oscillator_b_tuning = self.patch.oscillator_b_semitones;
            let mut filter_drive = self.patch.filter_drive;
            let mut oscillator_c_level = self
                .patch
                .oscillator_c_level
                .max(AMPLITUDE_FRACTION.min)
                .sqrt();
            let mut wavetable_position_b = self.patch.oscillator_b_wavetable_position;
            let mut wavetable_position_c = self.patch.oscillator_c_wavetable_position;
            let mut filter2_cutoff_octaves = 0.0;
            let mut filter2_resonance = self.patch.filter2_resonance;
            let mut filter2_drive = self.patch.filter2_drive;
            let mut oscillator_a_level = self
                .patch
                .oscillator_a_level
                .max(AMPLITUDE_FRACTION.min)
                .sqrt();
            let mut noise_level = self.patch.noise_level.max(AMPLITUDE_FRACTION.min).sqrt();
            // The slot parameter is sqrt-normalised: L^2 restores the linear knob.
            let mut sub_level = self.patch.sub_level.max(AMPLITUDE_FRACTION.min).sqrt();
            let mut filter_wet = self.patch.filter_wet;
            let mut filter2_wet = self.patch.filter2_wet;
            let mut warp_a = self.patch.oscillator_a_warp_amount;
            let mut warp2_a = self.patch.oscillator_a_warp2_amount;
            let mut warp_b = self.patch.oscillator_b_warp_amount;
            let mut warp2_b = self.patch.oscillator_b_warp2_amount;
            let mut warp_c = self.patch.oscillator_c_warp_amount;
            let mut warp2_c = self.patch.oscillator_c_warp2_amount;
            // Source-oscillator offsets, indexed by oscillator A/B/C.
            let mut source_density = [0.0f64; 3];
            let mut source_position = [0.0f64; 3];
            let mut source_morph = [0.0f64; 3];
            let mut source_bend = [0.0f64; 3];
            let mut voice_amp = 1.0;
            // Evaluate each source used by the active matrix once per voice.
            // Worst-case 64 routes often share a handful of sources, so this
            // turns the route loop into an array index instead of 64 matches.
            let mut source_values = [0.0f64; MOD_SOURCE_TABLE.len()];
            let mut source_mask = self.active_source_mask;
            while source_mask != 0 {
                let index = source_mask.trailing_zeros() as usize;
                source_values[index] = modulation_source(
                    MOD_SOURCE_TABLE[index],
                    &lfo_values,
                    &envelopes,
                    voice,
                    &self.patch.macros,
                    &self.patch.extra_macros,
                    expressive_pressure,
                    self.mod_wheel[voice.channel.min(15) as usize],
                    step,
                );
                source_mask &= source_mask - 1;
            }
            for group in &self.route_groups[..self.route_group_count] {
                let route = &self.compiled_mod_routes[group.first as usize];
                // `amount` already includes the destination's unit scale.
                let value = route.value(&source_values);
                let shaped = shape_mod_value(value, route.curve);
                let value = if route.immediate_slew() {
                    // Every member still records the target, exactly like the
                    // per-route path; a later slew edit must start from it.
                    for offset in 0..group.len as usize {
                        voice.route_slew[group.first as usize + offset] = shaped;
                    }
                    shaped
                } else {
                    let state = &mut voice.route_slew[group.first as usize];
                    *state = slew_step(*state, shaped, route.slew_up, route.slew_down);
                    *state
                };
                for _ in 0..group.len {
                    match route.destination {
                        ModDestination::FilterCutoff => cutoff_octaves += value,
                        ModDestination::Pitch => pitch_semitones += value,
                        ModDestination::OscillatorMix => oscillator_mix += value,
                        ModDestination::Resonance => resonance += value,
                        ModDestination::StereoWidth => stereo_width += value,
                        ModDestination::PulseWidthA => pulse_width_a += value,
                        ModDestination::RingModLevel => ring_mod_level += value,
                        ModDestination::WavetablePositionA => wavetable_position_a += value,
                        ModDestination::OscillatorBTuning => oscillator_b_tuning += value,
                        ModDestination::FilterDrive => filter_drive += value,
                        ModDestination::OscillatorCLevel => oscillator_c_level += value,
                        ModDestination::WavetablePositionB => wavetable_position_b += value,
                        ModDestination::WavetablePositionC => wavetable_position_c += value,
                        ModDestination::Filter2Cutoff => filter2_cutoff_octaves += value,
                        ModDestination::Filter2Resonance => filter2_resonance += value,
                        ModDestination::Filter2Drive => filter2_drive += value,
                        ModDestination::OscillatorALevel => oscillator_a_level += value,
                        ModDestination::NoiseLevel => noise_level += value,
                        ModDestination::SubLevel => sub_level += value,
                        ModDestination::FilterWet => filter_wet += value,
                        ModDestination::Filter2Wet => filter2_wet += value,
                        // VoiceAmp's centre is 0.5; a bipolar route spans
                        // half the range, a unipolar route the full range.
                        ModDestination::VoiceAmp => {
                            voice_amp += if route.bipolar { value } else { value * 2.0 }
                        }
                        ModDestination::WarpAmountA => warp_a += value,
                        ModDestination::WarpAmountB => warp_b += value,
                        ModDestination::WarpAmountC => warp_c += value,
                        ModDestination::Warp2AmountA => warp2_a += value,
                        ModDestination::Warp2AmountB => warp2_b += value,
                        ModDestination::Warp2AmountC => warp2_c += value,
                        ModDestination::SourceDensityA => source_density[0] += value,
                        ModDestination::SourceDensityB => source_density[1] += value,
                        ModDestination::SourceDensityC => source_density[2] += value,
                        ModDestination::SourcePositionA => source_position[0] += value,
                        ModDestination::SourcePositionB => source_position[1] += value,
                        ModDestination::SourcePositionC => source_position[2] += value,
                        ModDestination::SourceMorphA => source_morph[0] += value,
                        ModDestination::SourceMorphB => source_morph[1] += value,
                        ModDestination::SourceMorphC => source_morph[2] += value,
                        ModDestination::SourceBendA => source_bend[0] += value,
                        ModDestination::SourceBendB => source_bend[1] += value,
                        ModDestination::SourceBendC => source_bend[2] += value,
                    }
                }
            }
            // +6 dB: a `VoiceAmp` route is a gain stage, and one route may
            // not carry a macro past the instrument headroom on its own.
            voice_amp = voice_amp.clamp(0.0, MAX_VOICE_AMP);
            oscillator_mix = oscillator_mix.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            resonance = resonance.clamp(0.0, 1.0);
            stereo_width = stereo_width.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            pulse_width_a = pulse_width_a.clamp(0.05, 0.95);
            ring_mod_level = ring_mod_level.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            wavetable_position_a = wavetable_position_a.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            oscillator_b_tuning = oscillator_b_tuning.clamp(-48.0, 48.0);
            filter_drive = filter_drive.clamp(0.0, 1.0);
            oscillator_c_level =
                oscillator_c_level.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            wavetable_position_b = wavetable_position_b.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            wavetable_position_c = wavetable_position_c.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            filter2_resonance = filter2_resonance.clamp(0.0, 1.0);
            filter2_drive = filter2_drive.clamp(0.0, 1.0);
            oscillator_a_level =
                oscillator_a_level.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            noise_level = noise_level.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            sub_level = sub_level.clamp(AMPLITUDE_FRACTION.min, AMPLITUDE_FRACTION.max);
            filter_wet = filter_wet.clamp(0.0, 1.0);
            filter2_wet = filter2_wet.clamp(0.0, 1.0);
            warp_a = warp_a.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            warp2_a = warp2_a.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            warp_b = warp_b.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            warp2_b = warp2_b.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            warp_c = warp_c.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            warp2_c = warp2_c.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            let warp_a_params = if self.warp_a_modulated {
                WarpParams::new(
                    self.patch.oscillator_a_warp_mode,
                    warp_a,
                    self.patch.oscillator_a_warp_var,
                    self.patch.oscillator_a_warp2_mode,
                    warp2_a,
                    self.patch.oscillator_a_warp2_var,
                )
            } else {
                self.warp_a_base
            };
            let warp_b_params = if self.warp_b_modulated {
                WarpParams::new(
                    self.patch.oscillator_b_warp_mode,
                    warp_b,
                    self.patch.oscillator_b_warp_var,
                    self.patch.oscillator_b_warp2_mode,
                    warp2_b,
                    self.patch.oscillator_b_warp2_var,
                )
            } else {
                self.warp_b_base
            };
            let warp_c_params = if self.warp_c_modulated {
                WarpParams::new(
                    self.patch.oscillator_c_warp_mode,
                    warp_c,
                    self.patch.oscillator_c_warp_var,
                    self.patch.oscillator_c_warp2_mode,
                    warp2_c,
                    self.patch.oscillator_c_warp2_var,
                )
            } else {
                self.warp_c_base
            };
            let target_note = voice.note as f64;
            if self.patch.glide_time <= 0.0 {
                voice.pitch_note = target_note;
            } else if voice.pitch_note != target_note {
                let smoothing = 1.0 - (-1.0 / (self.patch.glide_time * sr)).exp();
                voice.pitch_note += (target_note - voice.pitch_note) * smoothing;
            }
            let global_tuning = self.patch.tuning_transpose + self.patch.tuning_fine_cents / 100.0;
            let base = if self.pitch_modulated || voice.pitch_note != target_note {
                voice.frequency_hz = self.patch.tuning_reference_hz
                    * 2f64.powf((voice.pitch_note - 69.0 + pitch_semitones + global_tuning) / 12.0);
                voice.frequency_hz
            } else {
                if !voice.frequency_valid {
                    voice.frequency_hz = self.patch.tuning_reference_hz
                        * 2f64.powf(
                            (target_note - 69.0 + bend + voice.expression_tuning + global_tuning)
                                / 12.0,
                        );
                    voice.frequency_valid = true;
                }
                voice.frequency_hz
            };
            let oscillator_b_ratio = if self.oscillator_b_tuning_modulated {
                2f64.powf(oscillator_b_tuning / 12.0)
            } else {
                self.oscillator_b_ratio
            };
            let increment_b = base * oscillator_b_ratio * inv_sr;
            let increment_sub = base * self.sub_ratio * inv_sr;
            self.last_sub_increment = increment_sub;
            // Oscillator C follows B's single-phase path (no per-oscillator
            // unison stack), but still uses the quality oversampling factor.
            let increment_c = base * self.oscillator_c_ratio * inv_sr;
            // Source oscillators bypass the wavetable path and render through
            // their own engine. Only audible slots are evaluated.
            let waveforms = self.patch.waveforms();
            let mut source_out = [[0.0f64; 2]; 3];
            let mut source_mono = [0.0f64; 3];
            let mut source_active = [false; 3];
            for slot in 0..3 {
                let audible = match slot {
                    0 => oscillator_a_level > 0.0 || ring_mod_level > 0.0,
                    1 => oscillator_mix > 0.0 || ring_mod_level > 0.0,
                    _ => oscillator_c_level > 0.0,
                };
                if !audible {
                    continue;
                }
                let root_note = match waveforms[slot] {
                    Waveform::Granular => self.patch.granular[slot].as_ref().map(|s| s.root_note),
                    Waveform::Spectral => self.patch.spectral[slot].as_ref().map(|s| s.root_note),
                    _ => None,
                };
                let Some(root_note) = root_note else {
                    continue;
                };
                let root = self.patch.tuning_reference_hz * 2f64.powf((root_note - 69.0) / 12.0);
                match waveforms[slot] {
                    Waveform::Granular => {
                        let Some(source) = self.granular_sources[slot].as_ref() else {
                            continue;
                        };
                        let Some(settings) = self.patch.granular[slot].as_ref() else {
                            continue;
                        };
                        let pitch_ratio = if settings.pitch_track {
                            base / root.max(1.0e-6)
                        } else {
                            1.0
                        };
                        let Some(SourceVoice::Granular(engine)) =
                            voice.source_voices[slot].as_deref_mut()
                        else {
                            continue;
                        };
                        let (left, right) = engine.render(
                            source,
                            settings,
                            pitch_ratio,
                            source_density[slot],
                            source_position[slot],
                            sr,
                        );
                        source_out[slot] = [left, right];
                        source_mono[slot] = 0.5 * (left + right);
                        source_active[slot] = true;
                    }
                    Waveform::Spectral => {
                        let Some(source) = self.spectral_sources[slot].as_ref() else {
                            continue;
                        };
                        let Some(settings) = self.patch.spectral[slot].as_ref() else {
                            continue;
                        };
                        let Some(SourceVoice::Spectral(engine)) =
                            voice.source_voices[slot].as_deref_mut()
                        else {
                            continue;
                        };
                        let Some(ifft) = self.spectral_ifft.as_deref() else {
                            continue;
                        };
                        let pitch_ratio = if settings.pitch_track {
                            base / root.max(1.0e-6)
                        } else {
                            1.0
                        };
                        let value = engine.render(
                            source,
                            settings,
                            pitch_ratio,
                            source_morph[slot],
                            source_bend[slot],
                            sr,
                            ifft,
                        );
                        source_out[slot] = [value, value];
                        source_mono[slot] = value;
                        source_active[slot] = true;
                    }
                    _ => {}
                }
            }
            // B and C fall back to A's table so patches written before
            // per-oscillator tables keep their original timbre.
            let wavetable_a = self.patch.custom_wavetable.as_ref();
            let wavetable_b = self.patch.oscillator_b_wavetable.as_ref().or(wavetable_a);
            let wavetable_c = self.patch.oscillator_c_wavetable.as_ref().or(wavetable_a);
            let pan_a = self.patch.oscillator_a_pan.clamp(-1.0, 1.0);
            let pan_b = self.patch.oscillator_b_pan.clamp(-1.0, 1.0);
            let pan_c = self.patch.oscillator_c_pan.clamp(-1.0, 1.0);
            let unison_voices = self.unison_voice_count;
            let unison_scale = self.unison_scale;
            let mut unison_left = 0.0;
            let mut unison_right = 0.0;
            let mut unison_mono = 0.0;
            let oversampling = oscillator_oversampling(
                self.patch.oscillator_a_waveform,
                self.patch.oscillator_quality,
            );
            let inv_oversampling = 1.0 / oversampling as f64;
            for oscillator in 0..unison_voices {
                let position = self.unison_positions[oscillator];
                let detuned =
                    base * self.oscillator_a_ratio * self.unison_ratios[oscillator] * inv_sr;
                // Every sync slot scales this shared increment.
                let increment = (detuned * warp_a_params.sync_factor()).min(1.0);
                let step = increment * inv_oversampling;
                let (window, restarted) = advance_sync_window(
                    warp_a_params,
                    detuned,
                    &mut voice.warp_window[0][oscillator],
                    &mut voice.phases[oscillator],
                );
                let mut sample = 0.0;
                for _ in 0..oversampling {
                    let phase = voice.phases[oscillator];
                    sample += oscillator_sample_with_warp(
                        self.patch.oscillator_a_waveform,
                        phase,
                        step,
                        increment,
                        pulse_width_a,
                        wavetable_position_a,
                        wavetable_a,
                        warp_a_params,
                        window,
                        &mut voice.mip_a[oscillator],
                    );
                    let mut next = phase + step;
                    if next >= 1.0 {
                        next -= 1.0;
                    }
                    voice.phases[oscillator] = next;
                }
                sample *= inv_oversampling;
                sample *= self.unison_weights[oscillator];
                if let Some(capture) = self.preview_capture.as_mut()
                    && capture.slot == 0
                {
                    capture.record(oscillator, sample * unison_scale);
                    if restarted {
                        capture.record_reset();
                    }
                }
                let pan = unison_pan(position, unison_voices, stereo_width);
                let (left_gain, right_gain) = unison_pan_gains(pan, stereo_width);
                unison_left += sample * left_gain;
                unison_right += sample * right_gain;
                unison_mono += sample;
            }
            unison_left *= unison_scale;
            unison_right *= unison_scale;
            unison_mono *= unison_scale;
            if source_active[0] {
                unison_left = source_out[0][0];
                unison_right = source_out[0][1];
                unison_mono = source_mono[0];
            }
            let b_width = self
                .patch
                .oscillator_b_stereo_width
                .clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            let c_width = self
                .patch
                .oscillator_c_stereo_width
                .clamp(UNIT_FRACTION.min, UNIT_FRACTION.max);
            let b_count = self.unison_b_count;
            let c_count = self.unison_c_count;
            let mut sample_b = 0.0;
            let mut b_left = 0.0;
            let mut b_right = 0.0;
            let mut sample_sub = 0.0;
            let oscillator_b_active = oscillator_mix > 0.0 || ring_mod_level > 0.0;
            let sub_active = sub_level > 0.0;
            if oscillator_b_active {
                let oversampling = oscillator_oversampling(
                    self.patch.oscillator_b_waveform,
                    self.patch.oscillator_quality,
                );
                let inv_oversampling = 1.0 / oversampling as f64;
                for unison in 0..b_count {
                    let position = self.unison_b_positions[unison];
                    let detuned = base * oscillator_b_ratio * self.unison_b_ratios[unison] * inv_sr;
                    let increment = (detuned * warp_b_params.sync_factor()).min(1.0);
                    let step = increment * inv_oversampling;
                    let (window, restarted) = advance_sync_window(
                        warp_b_params,
                        detuned,
                        &mut voice.warp_window[1][unison],
                        if unison == 0 {
                            &mut voice.phases[PHASE_B]
                        } else {
                            &mut voice.phases_b[unison - 1]
                        },
                    );
                    let mut sample = 0.0;
                    for _ in 0..oversampling {
                        let phase = if unison == 0 {
                            voice.phases[PHASE_B]
                        } else {
                            voice.phases_b[unison - 1]
                        };
                        sample += oscillator_sample_with_warp(
                            self.patch.oscillator_b_waveform,
                            phase,
                            step,
                            increment,
                            self.patch.oscillator_b_pulse_width,
                            wavetable_position_b,
                            wavetable_b,
                            warp_b_params,
                            window,
                            &mut voice.mip_b[unison],
                        );
                        let mut next = phase + step;
                        if next >= 1.0 {
                            next -= 1.0;
                        }
                        if unison == 0 {
                            voice.phases[PHASE_B] = next;
                        } else {
                            voice.phases_b[unison - 1] = next;
                        }
                    }
                    sample *= inv_oversampling;
                    sample *= self.unison_b_weights[unison];
                    if let Some(capture) = self.preview_capture.as_mut()
                        && capture.slot == 1
                    {
                        capture.record(unison, sample * self.unison_b_scale);
                        if restarted {
                            capture.record_reset();
                        }
                    }
                    let pan = unison_pan(position, b_count, b_width);
                    let (left_gain, right_gain) = unison_pan_gains(pan, b_width);
                    b_left += sample * left_gain;
                    b_right += sample * right_gain;
                    sample_b += sample;
                }
                b_left *= self.unison_b_scale;
                b_right *= self.unison_b_scale;
                sample_b *= self.unison_b_scale;
            } else {
                voice.phases[PHASE_B] = (voice.phases[PHASE_B] + increment_b).fract();
                for unison in 1..b_count {
                    let step = base * oscillator_b_ratio * self.unison_b_ratios[unison] * inv_sr;
                    voice.phases_b[unison - 1] = (voice.phases_b[unison - 1] + step).fract();
                }
            }
            if source_active[1] {
                b_left = source_out[1][0];
                b_right = source_out[1][1];
                sample_b = source_mono[1];
            }
            if sub_active {
                // The table is already band-limited, so the sub is read once
                // per output sample instead of being oversampled.
                sample_sub = sub_osc::sub_sample(
                    self.patch.sub_waveform,
                    &mut voice.mip_sub,
                    voice.phases[PHASE_SUB],
                    increment_sub,
                );
            }
            voice.phases[PHASE_SUB] = (voice.phases[PHASE_SUB] + increment_sub).fract();
            let mut c_left = 0.0;
            let mut c_right = 0.0;
            if oscillator_c_level > 0.0 {
                let oversampling = oscillator_oversampling(
                    self.patch.oscillator_c_waveform,
                    self.patch.oscillator_quality,
                );
                let inv_oversampling = 1.0 / oversampling as f64;
                for unison in 0..c_count {
                    let position = self.unison_c_positions[unison];
                    let detuned =
                        base * self.oscillator_c_ratio * self.unison_c_ratios[unison] * inv_sr;
                    let increment = (detuned * warp_c_params.sync_factor()).min(1.0);
                    let step = increment * inv_oversampling;
                    let (window, restarted) = advance_sync_window(
                        warp_c_params,
                        detuned,
                        &mut voice.warp_window[2][unison],
                        if unison == 0 {
                            &mut voice.phases[PHASE_C]
                        } else {
                            &mut voice.phases_c[unison - 1]
                        },
                    );
                    let mut sample = 0.0;
                    for _ in 0..oversampling {
                        let phase = if unison == 0 {
                            voice.phases[PHASE_C]
                        } else {
                            voice.phases_c[unison - 1]
                        };
                        sample += oscillator_sample_with_warp(
                            self.patch.oscillator_c_waveform,
                            phase,
                            step,
                            increment,
                            self.patch.oscillator_c_pulse_width,
                            wavetable_position_c,
                            wavetable_c,
                            warp_c_params,
                            window,
                            &mut voice.mip_c[unison],
                        );
                        let mut next = phase + step;
                        if next >= 1.0 {
                            next -= 1.0;
                        }
                        if unison == 0 {
                            voice.phases[PHASE_C] = next;
                        } else {
                            voice.phases_c[unison - 1] = next;
                        }
                    }
                    sample *= inv_oversampling;
                    sample *= self.unison_c_weights[unison];
                    if let Some(capture) = self.preview_capture.as_mut()
                        && capture.slot == 2
                    {
                        capture.record(unison, sample * self.unison_c_scale);
                        if restarted {
                            capture.record_reset();
                        }
                    }
                    let pan = unison_pan(position, c_count, c_width);
                    let (left_gain, right_gain) = unison_pan_gains(pan, c_width);
                    c_left += sample * left_gain;
                    c_right += sample * right_gain;
                }
                c_left *= self.unison_c_scale;
                c_right *= self.unison_c_scale;
            } else {
                voice.phases[PHASE_C] = (voice.phases[PHASE_C] + increment_c).fract();
                for unison in 1..c_count {
                    let step =
                        base * self.oscillator_c_ratio * self.unison_c_ratios[unison] * inv_sr;
                    voice.phases_c[unison - 1] = (voice.phases_c[unison - 1] + step).fract();
                }
            }
            if source_active[2] {
                c_left = source_out[2][0];
                c_right = source_out[2][1];
            }
            let noise_raw = voice.next_noise();
            voice.noise_hp[0] += noise_hp_coeff * (noise_raw - voice.noise_hp[0]);
            voice.noise_hp[1] += noise_hp_coeff * (voice.noise_hp[0] - voice.noise_hp[1]);
            let noise = (noise_raw - voice.noise_hp[1]) * NOISE_RMS_SCALE;
            let ring = unison_mono * sample_b;
            // Balance-style pan: panning attenuates the far channel and leaves
            // the centre exactly at unity, so pan=0 is bit-identical to the
            // pre-pan mix. Multipliers are applied before the original sum
            // order to keep floating-point grouping unchanged.
            let b_gain = oscillator_mix * oscillator_mix;
            let signal_b_left = b_left * b_gain * (1.0 - pan_b.max(0.0));
            let signal_b_right = b_right * b_gain * (1.0 + pan_b.min(0.0));
            let sub_gain = sub_level * sub_level;
            let (sub_left_gain, sub_right_gain) = sub_pan_gains(self.patch.sub_pan);
            let signal_sub_left = sample_sub * sub_gain * sub_left_gain;
            let signal_sub_right = sample_sub * sub_gain * sub_right_gain;
            let c_gain = oscillator_c_level * oscillator_c_level;
            let signal_c_left = c_left * c_gain * (1.0 - pan_c.max(0.0));
            let signal_c_right = c_right * c_gain * (1.0 + pan_c.min(0.0));
            let signal_noise = noise * (noise_level * noise_level);
            let signal_ring = ring * ring_mod_level;
            // The sampler already applies the SFZ velocity response, so divide
            // it back out here and let the voice amplitude apply it once.
            let velocity_gain =
                1.0 - self.patch.velocity_to_amp + self.patch.velocity_to_amp * voice.velocity;
            let sample = if sample_owner_count > 0 {
                self.sample_scratch
                    .iter()
                    .find(|sample| sample.owner == voice.age)
                    .map(|sample| [sample.left / velocity_gain, sample.right / velocity_gain])
            } else {
                None
            };
            let keytrack_octaves = (voice.note as f64 - self.patch.filter_keytrack_origin) / 12.0
                * self.patch.filter_keytrack;
            // Both filters share the coefficient refresh cadence: envelope and
            // mod-route updates drive both cutoffs at the same rate.
            let recompute_coefficients = if self.filter_modulated {
                if voice.filter_coefficient_age == 0 {
                    // Cutoff is control-rate; 32 samples avoids per-sample tan/pow.
                    voice.filter_coefficient_age = 31;
                    true
                } else {
                    voice.filter_coefficient_age -= 1;
                    false
                }
            } else {
                !voice.filter_coefficients_valid || voice.filter_sample_rate != sample_rate
            };
            if recompute_coefficients {
                let octaves = cutoff_octaves + keytrack_octaves;
                let cutoff =
                    FILTER_RUNTIME_BOUNDS.clamp(self.patch.filter_cutoff * 2f64.powf(octaves), sr);
                voice.filter_coefficients =
                    FilterCoefficients::new(self.patch.filter_model, cutoff, resonance, sr);
                // Filter 2 reuses filter 1's keytrack and envelope shape but
                // adds its own matrix cutoff offset and keeps its own base
                // cutoff and resonance.
                let cutoff2 = FILTER_RUNTIME_BOUNDS.clamp(
                    self.patch.filter2_cutoff * 2f64.powf(octaves + filter2_cutoff_octaves),
                    sr,
                );
                voice.filter2_coefficients = FilterCoefficients::new(
                    self.patch.filter2_model,
                    cutoff2,
                    filter2_resonance,
                    sr,
                );
                voice.filter_coefficients_valid = true;
                voice.filter_sample_rate = sample_rate;
            }
            let a_gain = oscillator_a_level * oscillator_a_level;
            let source_a = [
                unison_left * a_gain * (1.0 - pan_a.max(0.0)),
                unison_right * a_gain * (1.0 + pan_a.min(0.0)),
            ];
            let mut voice_left;
            let mut voice_right;
            if let Some(routes) = self.patch.source_filter_routes.as_ref() {
                // One shared router for every source; ring mod belongs to A.
                let mut filter1 = [0.0; 2];
                let mut filter2 = [0.0; 2];
                let mut dry = [0.0; 2];
                add_routed_source(routes[0], source_a, &mut filter1, &mut filter2, &mut dry);
                // Osc 1's slot: the sampler layer is that oscillator's sample
                // source, so it takes the same route as the wavetable content.
                if let Some(sample) = sample {
                    add_routed_source(routes[0], sample, &mut filter1, &mut filter2, &mut dry);
                }
                add_routed_source(
                    routes[1],
                    [signal_b_left, signal_b_right],
                    &mut filter1,
                    &mut filter2,
                    &mut dry,
                );
                add_routed_source(
                    routes[4],
                    [signal_sub_left, signal_sub_right],
                    &mut filter1,
                    &mut filter2,
                    &mut dry,
                );
                add_routed_source(
                    routes[2],
                    [signal_c_left, signal_c_right],
                    &mut filter1,
                    &mut filter2,
                    &mut dry,
                );
                add_routed_source(
                    routes[3],
                    [signal_noise, signal_noise],
                    &mut filter1,
                    &mut filter2,
                    &mut dry,
                );
                add_routed_source(
                    routes[0],
                    [signal_ring, signal_ring],
                    &mut filter1,
                    &mut filter2,
                    &mut dry,
                );
                // A disabled second filter folds its send into filter 1 so no
                // source is silently dropped.
                if !self.patch.filter2_enabled {
                    filter1[0] += filter2[0];
                    filter1[1] += filter2[1];
                    filter2 = [0.0; 2];
                }
                let filter1_input = filter1;
                let filter1_left = process_voice_filter(
                    self.patch.filter_model,
                    &mut voice.filter[0],
                    &mut voice.filter_ladder[0],
                    &mut voice.filter_moog[0],
                    filter1_input[0],
                    filter_drive,
                    voice.filter_coefficients,
                    self.patch.filter_mode,
                );
                let filter1_right = process_voice_filter(
                    self.patch.filter_model,
                    &mut voice.filter[1],
                    &mut voice.filter_ladder[1],
                    &mut voice.filter_moog[1],
                    filter1_input[1],
                    filter_drive,
                    voice.filter_coefficients,
                    self.patch.filter_mode,
                );
                let mut out_left =
                    filter1_input[0] + (filter1_left - filter1_input[0]) * filter_wet;
                let mut out_right =
                    filter1_input[1] + (filter1_right - filter1_input[1]) * filter_wet;
                if self.patch.filter2_enabled {
                    let serial = self.patch.filter_routing == FilterRouting::Serial;
                    let filter2_input = if serial {
                        [filter2[0] + out_left, filter2[1] + out_right]
                    } else {
                        filter2
                    };
                    let filter2_left = process_voice_filter(
                        self.patch.filter2_model,
                        &mut voice.filter2[0],
                        &mut voice.filter2_ladder[0],
                        &mut voice.filter2_moog[0],
                        filter2_input[0],
                        filter2_drive,
                        voice.filter2_coefficients,
                        self.patch.filter2_mode,
                    );
                    let filter2_right = process_voice_filter(
                        self.patch.filter2_model,
                        &mut voice.filter2[1],
                        &mut voice.filter2_ladder[1],
                        &mut voice.filter2_moog[1],
                        filter2_input[1],
                        filter2_drive,
                        voice.filter2_coefficients,
                        self.patch.filter2_mode,
                    );
                    let filter2_left =
                        filter2_input[0] + (filter2_left - filter2_input[0]) * filter2_wet;
                    let filter2_right =
                        filter2_input[1] + (filter2_right - filter2_input[1]) * filter2_wet;
                    if serial {
                        out_left = filter2_left;
                        out_right = filter2_right;
                    } else {
                        out_left += filter2_left;
                        out_right += filter2_right;
                    }
                }
                voice_left = out_left + dry[0];
                voice_right = out_right + dry[1];
            } else {
                // Legacy global routing: all sources share the same filter path.
                let mono_left =
                    signal_b_left + signal_sub_left + signal_c_left + signal_noise + signal_ring;
                let mono_right =
                    signal_b_right + signal_sub_right + signal_c_right + signal_noise + signal_ring;
                voice_left = source_a[0] + mono_left;
                voice_right = source_a[1] + mono_right;
                if let Some(sample) = sample {
                    voice_left += sample[0];
                    voice_right += sample[1];
                }
                let filter1_input = [voice_left, voice_right];
                let mut filtered_left = process_voice_filter(
                    self.patch.filter_model,
                    &mut voice.filter[0],
                    &mut voice.filter_ladder[0],
                    &mut voice.filter_moog[0],
                    filter1_input[0],
                    filter_drive,
                    voice.filter_coefficients,
                    self.patch.filter_mode,
                );
                let mut filtered_right = process_voice_filter(
                    self.patch.filter_model,
                    &mut voice.filter[1],
                    &mut voice.filter_ladder[1],
                    &mut voice.filter_moog[1],
                    filter1_input[1],
                    filter_drive,
                    voice.filter_coefficients,
                    self.patch.filter_mode,
                );
                filtered_left = filter1_input[0] + (filtered_left - filter1_input[0]) * filter_wet;
                filtered_right =
                    filter1_input[1] + (filtered_right - filter1_input[1]) * filter_wet;
                if self.patch.filter2_enabled {
                    // Serial feeds F1's output into F2; parallel feeds both
                    // filters from the oscillator mix and averages them.
                    let (filter2_input_left, filter2_input_right) = match self.patch.filter_routing
                    {
                        FilterRouting::Serial => (filtered_left, filtered_right),
                        FilterRouting::Parallel => (voice_left, voice_right),
                    };
                    let filter2_left = process_voice_filter(
                        self.patch.filter2_model,
                        &mut voice.filter2[0],
                        &mut voice.filter2_ladder[0],
                        &mut voice.filter2_moog[0],
                        filter2_input_left,
                        filter2_drive,
                        voice.filter2_coefficients,
                        self.patch.filter2_mode,
                    );
                    let filter2_right = process_voice_filter(
                        self.patch.filter2_model,
                        &mut voice.filter2[1],
                        &mut voice.filter2_ladder[1],
                        &mut voice.filter2_moog[1],
                        filter2_input_right,
                        filter2_drive,
                        voice.filter2_coefficients,
                        self.patch.filter2_mode,
                    );
                    let filter2_left =
                        filter2_input_left + (filter2_left - filter2_input_left) * filter2_wet;
                    let filter2_right =
                        filter2_input_right + (filter2_right - filter2_input_right) * filter2_wet;
                    match self.patch.filter_routing {
                        FilterRouting::Serial => {
                            filtered_left = filter2_left;
                            filtered_right = filter2_right;
                        }
                        FilterRouting::Parallel => {
                            filtered_left = (filtered_left + filter2_left) * 0.5;
                            filtered_right = (filtered_right + filter2_right) * 0.5;
                        }
                    }
                }
                voice_left = filtered_left;
                voice_right = filtered_right;
            }
            let amplitude = velocity_gain * voice_amp * voice.amplitude_level;
            left += voice_left * amplitude;
            right += voice_right * amplitude;
        }
        let (next_lfo_phase, wrapped) = self
            .lfo_phase_accumulator
            .overflowing_add(self.modulator_increments[0]);
        if wrapped {
            self.advance_lfo_random();
        }
        self.lfo_phase_accumulator = next_lfo_phase;
        self.lfo_phase = phase_from_accumulator(next_lfo_phase);
        for index in 0..9 {
            let (next_phase, wrapped) = self.extra_lfo_phase_accumulators[index]
                .overflowing_add(self.modulator_increments[index + 1]);
            if wrapped {
                self.advance_extra_lfo_random(index);
            }
            self.extra_lfo_phase_accumulators[index] = next_phase;
            self.extra_lfo_phases[index] = phase_from_accumulator(next_phase);
        }
        self.step_phase_accumulator = self
            .step_phase_accumulator
            .wrapping_add(self.modulator_increments[10]);
        self.step_phase = phase_from_accumulator(self.step_phase_accumulator);
        self.sub_free_phase = (self.sub_free_phase + self.last_sub_increment).fract();
        // A late-jump attack can sit at exactly 0 for many samples; only a
        // finished release ends the voice.
        self.retain_voices(|voice| {
            voice.amplitude_stage != EnvStage::Release || voice.amplitude_level > 0.0
        });
        if sample_owner_count > 0 {
            self.prune_sample_owners();
        }
        if self.fx_chain.is_active() {
            [left, right] = self.fx_chain.process(left, right, sr);
        }
        // The patch master is the last user stage, after the FX rack. The fixed
        // output safety clip stays at the voice output and sees the same level;
        // the engine's per-patch loudness trim is part of that master.
        let master = self.patch.gain * self.patch.loudness_gain;
        [
            soft_clip(left * master) as f32,
            soft_clip(right * master) as f32,
        ]
    }
}

/// Shared 8192-point sine table; linear interpolation keeps error below 1e-7.
fn sine_table() -> &'static [f32; SINE_TABLE_SIZE + 1] {
    static TABLE: OnceLock<[f32; SINE_TABLE_SIZE + 1]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|index| (TAU * index as f64 / SINE_TABLE_SIZE as f64).sin() as f32)
    })
}

#[inline]
fn sine_lookup_unit(phase: f64) -> f64 {
    if !phase.is_finite() {
        return 0.0;
    }
    let position = phase * SINE_TABLE_SIZE as f64;
    let index = position as usize;
    let mix = position - index as f64;
    let table = sine_table();
    (table[index] + (table[index + 1] - table[index]) * mix as f32) as f64
}

/// Locate the segment of a cyclic shape that contains `phase`. Returns the
/// breakpoint indices and the local position in `[0, 1]`.
#[inline]
fn shape_segment(shape: &LfoShape, phase: f64) -> (usize, usize, f64) {
    let points = shape.points.clamp(2, LFO_SHAPE_POINTS);
    let phase = phase.rem_euclid(1.0);
    let first = shape.x[0] as f64;
    let last = shape.x[points - 1] as f64;
    if phase >= last || phase < first {
        // Wrap from the last breakpoint to the first one a cycle later.
        let base = if phase >= last { last } else { last - 1.0 };
        let span = first + 1.0 - last;
        let u = if span <= 0.0 {
            0.0
        } else {
            (phase - base) / span
        };
        return (points - 1, 0, u.clamp(0.0, 1.0));
    }
    let mut low = 0;
    let mut high = points - 1;
    while high - low > 1 {
        let middle = (low + high) / 2;
        if shape.x[middle] as f64 <= phase {
            low = middle;
        } else {
            high = middle;
        }
    }
    let span = shape.x[high] as f64 - shape.x[low] as f64;
    let u = if span <= 0.0 {
        0.0
    } else {
        ((phase - shape.x[low] as f64) / span).clamp(0.0, 1.0)
    };
    (low, high, u)
}

/// Bent interpolation for the drawn curve: 0.5 is straight, 0.25 and 0.75
/// bend the segment toward the two endpoints.
#[inline]
fn curve_shape_value(shape: &LfoShape, phase: f64) -> f64 {
    let (low, high, u) = shape_segment(shape, phase);
    let bend = shape.curve[low].clamp(0.0, 1.0) as f64;
    let u = if bend == 0.5 {
        u
    } else {
        u.powf(4f64.powf(2.0 * bend - 1.0))
    };
    let y0 = shape.y[low] as f64;
    let y1 = shape.y[high] as f64;
    (y0 + (y1 - y0) * u).clamp(0.0, 1.0)
}

/// Cyclic Catmull-Rom through the active breakpoints; smoother than `Curve`.
#[inline]
fn path_shape_value(shape: &LfoShape, phase: f64) -> f64 {
    let points = shape.points.clamp(2, LFO_SHAPE_POINTS);
    let (low, high, u) = shape_segment(shape, phase);
    if points < 4 {
        let y0 = shape.y[low] as f64;
        let y1 = shape.y[high] as f64;
        return (y0 + (y1 - y0) * u).clamp(0.0, 1.0);
    }
    let previous = shape.y[(low + points - 1) % points] as f64;
    let first = shape.y[low] as f64;
    let second = shape.y[high] as f64;
    let next = shape.y[(high + 1) % points] as f64;
    let u2 = u * u;
    let u3 = u2 * u;
    let value = 0.5
        * (2.0 * first
            + (second - previous) * u
            + (2.0 * previous - 5.0 * first + 4.0 * second - next) * u2
            + (3.0 * first - previous - 3.0 * second + next) * u3);
    value.clamp(0.0, 1.0)
}

/// One iteration of our bounded chaotic maps. Both keep their state in `[0, 1]`
/// by construction, so the output is always a valid unipolar modulation source.
fn chaos_step(kind: LfoShapeType, state: &mut [f64; 3]) -> f64 {
    match kind {
        LfoShapeType::ChaosA => {
            // 3.99 is chaotic above 3.57 and still maps [0,1] into itself.
            let logistic = 3.99 * state[0] * (1.0 - state[0]);
            let first = logistic.clamp(0.0, 1.0);
            // Convex echo keeps a slow second scale without leaving the bound.
            let second = (0.86 * state[1] + 0.14 * first).clamp(0.0, 1.0);
            state[0] = first;
            state[1] = second;
            0.65 * first + 0.35 * second
        }
        LfoShapeType::ChaosB => {
            // The tent fold maps [0,1] onto itself, so the mix stays bounded.
            // Unequal weights keep the states from synchronising and collapsing.
            let folded = 1.0 - (2.0 * (0.65 * state[0] + 0.35 * state[1]) - 1.0).abs();
            let follow = 0.5 * state[1] + 0.5 * folded;
            state[0] = folded;
            state[1] = follow;
            folded
        }
        _ => 0.5,
    }
}

/// Incommensurate starting points keep each LFO's attractor out of phase.
fn lfo_chaos_seed(index: usize) -> [f64; 3] {
    let index = index as f64;
    [
        0.11 + 0.07 * index,
        0.37 + 0.03 * index,
        0.53 + 0.05 * index,
    ]
}

/// One fixed seed per LFO keeps sample-and-hold reproducible across renders.
fn lfo_random_seed(index: usize) -> u64 {
    0xd1b5_4a32_d192_ed03_u64.wrapping_add((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// Skew the phase without changing the rate: exponent 0.25..4 follows `bend`.
#[inline]
fn warp_lfo_phase(phase: f64, bend: f64) -> f64 {
    if bend == 0.0 {
        return phase;
    }
    phase.powf(4f64.powf(bend.clamp(-1.0, 1.0)))
}

/// Fade-in gain: silent for `delay` seconds, then linear over `rise` seconds.
fn lfo_fade(age: f64, delay: f64, rise: f64) -> f64 {
    if age < delay {
        return 0.0;
    }
    if rise <= 0.0 {
        return 1.0;
    }
    ((age - delay) / rise).clamp(0.0, 1.0)
}

/// One-pole coefficient for a 0..1 amount and a maximum time constant.
fn smoothing_coefficient(amount: f64, max_seconds: f64, sample_rate: f64) -> f64 {
    if amount <= 0.0 {
        return 1.0;
    }
    let tau = amount.clamp(0.0, 1.0).powi(2) * max_seconds;
    1.0 - (-1.0 / (tau * sample_rate)).exp()
}

fn slew_coefficient(seconds: f64, sample_rate: f64) -> f64 {
    if seconds <= 0.0 {
        return 1.0;
    }
    1.0 - (-1.0 / (seconds * sample_rate)).exp()
}

/// Odd exponent shaper: 0 is identity, -1 softens and +1 sharpens the signal.
fn shape_mod_value(value: f64, curve: f64) -> f64 {
    if curve == 0.0 || value == 0.0 {
        return value;
    }
    value.signum()
        * value
            .abs()
            .powf(4f64.powf(curve.clamp(BIPOLAR_MODULATION.min, BIPOLAR_MODULATION.max)))
}

/// Move one step toward `target`; separate rise/fall coefficients never overshoot.
#[inline]
fn slew_step(state: f64, target: f64, up: f64, down: f64) -> f64 {
    let coefficient = if target > state { up } else { down };
    if coefficient >= 1.0 {
        return target;
    }
    state + (target - state) * coefficient
}

/// Effective free-running rate: the knob is real Hz and `x10` multiplies it.
fn free_lfo_rate(rate: f64, x10: bool) -> f64 {
    rate.clamp(LFO_RATE_HZ.min, LFO_RATE_HZ.max) * if x10 { 10.0 } else { 1.0 }
}

/// Synced rate from the 43-step ladder, or the retired six-step selector.
fn synced_lfo_rate(tempo: f64, division: usize, sync_div: Option<usize>) -> f64 {
    let beats = match sync_div {
        Some(index) => LFO_SYNC_DIVISIONS[index.min(LFO_SYNC_DIVISIONS.len() - 1)],
        None => {
            const LEGACY_BEATS_PER_CYCLE: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
            LEGACY_BEATS_PER_CYCLE[division.min(5)]
        }
    };
    tempo / 60.0 / beats
}

/// 43 sync divisions in beats per cycle, descending. Entry 22 is one beat.
const fn sync_division_table() -> [f64; 43] {
    let mut table = [0.0f64; 43];
    let mut base = 128.0;
    let mut group = 0;
    while group < 14 {
        table[group * 3] = base * 1.5;
        table[group * 3 + 1] = base;
        table[group * 3 + 2] = base * (2.0 / 3.0);
        base *= 0.5;
        group += 1;
    }
    table[42] = 0.0078125;
    let mut index = 1;
    while index < 43 {
        let value = table[index];
        let mut position = index;
        while position > 0 && table[position - 1] < value {
            table[position] = table[position - 1];
            position -= 1;
        }
        table[position] = value;
        index += 1;
    }
    table
}

/// Pick one modulation source. `lfos` is `[LFO1..LFO10]` and `envelopes` is
/// `[amp/env1, filter/env2, env3, env4]`, all sampled at the current frame.
// DSP hot path; a parameter struct would be rebuilt per call.
#[allow(clippy::too_many_arguments)]
fn modulation_source(
    source: ModSource,
    lfos: &[f64; 10],
    envelopes: &[f64; 4],
    voice: &Voice,
    macros: &[f64; 4],
    extra_macros: &[f64; 4],
    expressive_pressure: f64,
    mod_wheel: f64,
    step: f64,
) -> f64 {
    match source {
        ModSource::Lfo => lfos[0],
        ModSource::FilterEnvelope => envelopes[1],
        ModSource::Velocity => voice.velocity,
        ModSource::Macro1 => macros[0],
        ModSource::Macro2 => macros[1],
        ModSource::Macro3 => macros[2],
        ModSource::Macro4 => macros[3],
        ModSource::Macro5 => extra_macros[0],
        ModSource::Macro6 => extra_macros[1],
        ModSource::Macro7 => extra_macros[2],
        ModSource::Macro8 => extra_macros[3],
        ModSource::Keytrack => (voice.note as f64 - 60.0) / 60.0,
        ModSource::Aftertouch => expressive_pressure,
        ModSource::ModWheel => mod_wheel,
        ModSource::Step => step,
        ModSource::Lfo2 => lfos[1],
        ModSource::Lfo3 => lfos[2],
        ModSource::Lfo4 => lfos[3],
        ModSource::Lfo5 => lfos[4],
        ModSource::Lfo6 => lfos[5],
        ModSource::Lfo7 => lfos[6],
        ModSource::Lfo8 => lfos[7],
        ModSource::Lfo9 => lfos[8],
        ModSource::Lfo10 => lfos[9],
        ModSource::Env3 => envelopes[2],
        ModSource::Env4 => envelopes[3],
        ModSource::AmpEnvelope => envelopes[0],
        ModSource::PitchBend => voice.pitch_bend,
        ModSource::Timbre => voice.timbre * 2.0 - 1.0,
    }
}

fn step_value_at(values: &[f64; 16], phase: f64, smoothing: f64) -> f64 {
    let position = phase.fract() * values.len() as f64;
    let index = position.floor() as usize % values.len();
    let next = (index + 1) % values.len();
    let fraction = position.fract();
    let curved = fraction * fraction * (3.0 - 2.0 * fraction);
    let mix = curved * smoothing.clamp(0.0, 1.0);
    values[index] + (values[next] - values[index]) * mix
}

const PHASE_SCALE: f64 = 18_446_744_073_709_551_616.0;

fn phase_increment(cycles_per_second: f64, sample_rate: u32) -> u64 {
    (cycles_per_second.max(0.0) / sample_rate.max(1) as f64 * PHASE_SCALE)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

/// Same accumulator step as `phase_increment`, using a precomputed reciprocal.
#[inline]
fn phase_increment_scaled(cycles_per_second: f64, inv_sample_rate: f64) -> u64 {
    (cycles_per_second.max(0.0) * inv_sample_rate * PHASE_SCALE)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

fn phase_from_accumulator(accumulator: u64) -> f64 {
    accumulator as f64 / PHASE_SCALE
}

/// Accumulator value for a start phase in `[0, 1)`.
fn phase_accumulator(phase: f64) -> u64 {
    (phase.rem_euclid(1.0) * PHASE_SCALE)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

pub fn render_track(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    params: Option<&Vec<(String, ParamValue)>>,
) -> Vec<f32> {
    render_track_with_tempo(events, total_samples, sample_rate, params, 120.0)
}

pub fn render_track_with_tempo(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    params: Option<&Vec<(String, ParamValue)>>,
    tempo: f64,
) -> Vec<f32> {
    let mut patch = SynthPatch::default();
    patch.apply_params(params);
    render_track_with_patch(events, total_samples, sample_rate, patch, tempo)
}

pub fn render_track_with_patch(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    patch: SynthPatch,
    tempo: f64,
) -> Vec<f32> {
    render_track_with_patch_and_clock(events, total_samples, sample_rate, patch, tempo, None)
}

pub fn render_track_with_patch_and_clock(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    patch: SynthPatch,
    tempo: f64,
    clock: Option<&oc_dsl::timing::ProjectClock>,
) -> Vec<f32> {
    try_render_track_with_patch_and_clock(events, total_samples, sample_rate, patch, tempo, clock)
        .expect("native render requires validated events")
}

pub fn try_render_track_with_patch_and_clock(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    patch: SynthPatch,
    tempo: f64,
    clock: Option<&oc_dsl::timing::ProjectClock>,
) -> Result<Vec<f32>, String> {
    try_render_track_with_patch_and_clock_cancellable(
        events,
        total_samples,
        sample_rate,
        patch,
        tempo,
        clock,
        None,
    )
}

/// Render a track and poll `cancel` between blocks.
///
/// A render with no listener passes `None`, which takes the same path as
/// [`try_render_track_with_patch_and_clock`] with one predictable branch.
pub fn try_render_track_with_patch_and_clock_cancellable(
    events: &[&MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    patch: SynthPatch,
    tempo: f64,
    clock: Option<&oc_dsl::timing::ProjectClock>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<f32>, String> {
    if sample_rate == 0 || !tempo.is_finite() || !(1.0..=400.0).contains(&tempo) {
        return Err("invalid native render clock".into());
    }
    if total_samples > sample_rate as usize * 600 || total_samples.checked_mul(2).is_none() {
        return Err("native render exceeds ten-minute allocation limit".into());
    }
    if events
        .windows(2)
        .any(|pair| pair[0].time_samples > pair[1].time_samples)
    {
        return Err("native events must be in timestamp order".into());
    }
    let mut synth = OpenSynth::new_with_tempo(patch, tempo);
    let mut output = vec![0.0; total_samples * 2];
    let mut event_index = 0;
    let mut frame = 0;
    let mut clock_boundary = 0;
    while frame < total_samples {
        // A relaxed load per sample costs less than one voice, and keeps the
        // stop latency inside the caller's block bound even when the synth
        // jumps over idle spans.
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err("render cancelled".into());
        }
        if frame == clock_boundary {
            clock_boundary = total_samples;
            if let Some(clock) = clock {
                let position = clock.position_at_sample(frame as u64, sample_rate)?;
                synth.set_tempo(position.bpm, sample_rate)?;
                clock_boundary = clock
                    .next_change_sample(frame as u64, sample_rate)?
                    .map(|sample| sample.min(total_samples as u64) as usize)
                    .unwrap_or(total_samples);
            }
        }
        if synth.is_idle() {
            let next_event_frame = events
                .get(event_index)
                .map(|event| event.time_samples as usize)
                .unwrap_or(total_samples)
                .min(total_samples)
                .min(clock_boundary);
            if next_event_frame > frame {
                synth.advance_idle(next_event_frame - frame, sample_rate);
                frame = next_event_frame;
                continue;
            }
        }
        while event_index < events.len() && events[event_index].time_samples as usize <= frame {
            synth.send_midi(&events[event_index].event)?;
            event_index += 1;
        }
        let [left, right] = synth.next_stereo(sample_rate);
        output[frame * 2] = left;
        output[frame * 2 + 1] = right;
        frame += 1;
    }
    Ok(output)
}

#[cfg(test)]
fn oscillator_sample(waveform: Waveform, phase: f64, dt: f64) -> f64 {
    let mut mip = MipState::default();
    oscillator_sample_with_pulse_width(
        waveform,
        phase,
        dt,
        dt,
        0.5,
        0.0,
        None,
        &mut mip,
        IMPORTED_READOUT_DELAY * dt,
    )
}

/// Table reads are band-limited by their mip chain, so they run once per output
/// sample; analytic waveforms keep the quality oversampling.
#[inline]
fn oscillator_oversampling(waveform: Waveform, quality: usize) -> usize {
    if matches!(waveform, Waveform::Wavetable) {
        1
    } else {
        [1, 2, 4][quality.min(2)]
    }
}

#[inline(always)]
// DSP hot path; a parameter struct would be rebuilt per call.
#[allow(clippy::too_many_arguments)]
fn oscillator_sample_with_pulse_width(
    waveform: Waveform,
    phase: f64,
    dt: f64,
    output_dt: f64,
    pulse_width: f64,
    wavetable_position: f64,
    custom_wavetable: Option<&CustomWavetable>,
    mip: &mut MipState,
    phase_delay: f64,
) -> f64 {
    match waveform {
        Waveform::Sine => sine_lookup_unit(phase),
        Waveform::Saw => 2.0 * phase - 1.0 - poly_blep(phase, dt),
        Waveform::Square => {
            let pulse_width = pulse_width.clamp(0.05, 0.95);
            let raw = if phase < pulse_width { 1.0 } else { -1.0 };
            let mut falling_edge = phase + 1.0 - pulse_width;
            if falling_edge >= 1.0 {
                falling_edge -= 1.0;
            }
            raw + poly_blep(phase, dt) - poly_blep(falling_edge, dt)
        }
        Waveform::Triangle => {
            let raw = 1.0 - 4.0 * (phase - 0.5).abs();
            // Integrate the two-point BLEP residual at the two slope changes.
            // The slopes jump by +8/-8 per cycle, hence 4 times the residual
            // normalized to a slope jump of two. Use output bandwidth even
            // when oscillator substeps are box-averaged.
            raw + 4.0
                * (poly_blamp(phase, output_dt) - poly_blamp((phase + 0.5).fract(), output_dt))
        }
        Waveform::Wavetable => match custom_wavetable {
            Some(table) => table.sample_mip(mip, phase, wavetable_position, output_dt, phase_delay),
            None => {
                default_shapes_table().sample_mip(mip, phase, wavetable_position, output_dt, 0.0)
            }
        },
        // Source oscillators render through their own engine, never this path.
        Waveform::Granular | Waveform::Spectral => 0.0,
    }
}

/// One active modulation route. Routes are not merged, because polarity,
/// auxiliary scaling and inversion are per-route.
#[derive(Debug, Clone, Copy)]
struct CompiledModRoute {
    source: u8,
    /// Auxiliary source index (`source[1]`); `None` is unity.
    auxiliary: Option<u8>,
    auxiliary_inverted: bool,
    bipolar: bool,
    destination: ModDestination,
    /// Amount already scaled into destination units (semitones, octaves, ...).
    amount: f64,
    /// Route shape exponent -1..1, applied before the slew.
    curve: f64,
    /// One-pole times in seconds; the audio path uses the precomputed coefficients.
    slew_up_seconds: f64,
    slew_down_seconds: f64,
    /// Coefficients for the current sample rate.
    slew_up: f64,
    slew_down: f64,
}

/// One FX-parameter route, flattened for the control-rate update loop.
#[derive(Debug, Clone, Copy)]
struct CompiledFxModRoute {
    source: u8,
    fx_index: u8,
    parameter: FxParam,
    amount: f64,
    bipolar: bool,
}

/// A consecutive run of routes that produce the same value every sample.
#[derive(Debug, Clone, Copy, Default)]
struct RouteGroup {
    first: u8,
    len: u8,
}

impl CompiledModRoute {
    /// True when the route reaches its target in one step, so its slew state is
    /// never read back and adjacent identical routes stay identical.
    #[inline]
    fn immediate_slew(&self) -> bool {
        self.slew_up_seconds <= 0.0 && self.slew_down_seconds <= 0.0
    }

    /// Field-wise identity used to coalesce adjacent routes. The derived slew
    /// coefficients are excluded because they follow the seconds fields.
    fn same_route(&self, other: &Self) -> bool {
        self.source == other.source
            && self.auxiliary == other.auxiliary
            && self.auxiliary_inverted == other.auxiliary_inverted
            && self.bipolar == other.bipolar
            && self.destination == other.destination
            && self.amount == other.amount
            && self.curve == other.curve
            && self.slew_up_seconds == other.slew_up_seconds
            && self.slew_down_seconds == other.slew_down_seconds
    }

    /// Evaluate the route for one voice. `sources` holds one value per
    /// [`MOD_SOURCE_TABLE`] entry.
    #[inline]
    fn value(&self, sources: &[f64; MOD_SOURCE_TABLE.len()]) -> f64 {
        let source = MOD_SOURCE_TABLE[self.source as usize];
        let mut value = sources[self.source as usize];
        // Bipolar mode centres a unipolar source on zero. Sources that
        // already swing around zero are left as they are.
        if self.bipolar && !source_is_bipolar(source) {
            value = value * 2.0 - 1.0;
        }
        if let Some(auxiliary) = self.auxiliary {
            let mut auxiliary = sources[auxiliary as usize];
            if self.auxiliary_inverted {
                auxiliary = -auxiliary;
            }
            value *= auxiliary;
        }
        value * self.amount
    }
}

/// Natural polarity of the engine's modulation sources. LFOs, envelopes,
/// velocity, macros, aftertouch and mod wheel are unipolar; the step sequencer
/// and the note-relative sources are centred.
fn source_is_bipolar(source: ModSource) -> bool {
    matches!(
        source,
        ModSource::Step | ModSource::Keytrack | ModSource::PitchBend | ModSource::Timbre
    )
}

/// One warp slot with the knob already mapped through its amount curve.
#[derive(Debug, Clone, Copy)]
struct WarpSlot {
    mode: WarpMode,
    amount: f64,
    var: f64,
    /// Stage bits resolved once, so the sample loop never classifies the mode.
    flags: u8,
}

const WARP_PHASE: u8 = 1 << 0;
const WARP_FLIP: u8 = 1 << 1;
const WARP_SELFPD: u8 = 1 << 2;
const WARP_EVENODD: u8 = 1 << 3;
const WARP_DIST: u8 = 1 << 4;
const WARP_SYNC: u8 = 1 << 5;
const WARP_RM_GAIN: u8 = 1 << 6;

impl WarpSlot {
    fn new(mode: WarpMode, amount: f64, var: f64) -> Self {
        let mut flags = 0;
        if mode.is_phase_warp() {
            flags |= WARP_PHASE;
        }
        if mode == WarpMode::Flip {
            flags |= WARP_FLIP;
        }
        if mode == WarpMode::SelfPd {
            flags |= WARP_SELFPD;
        }
        if mode == WarpMode::EvenOdd {
            flags |= WARP_EVENODD;
        }
        if mode.is_dist() {
            flags |= WARP_DIST;
        }
        if mode == WarpMode::Sync {
            flags |= WARP_SYNC;
        }
        if mode.is_rm_gain() {
            flags |= WARP_RM_GAIN;
        }
        Self {
            mode,
            amount: warp_mod::amount_curve(mode, amount),
            var: if var.is_finite() {
                var.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max)
            } else {
                0.0
            },
            flags,
        }
    }

    #[inline]
    fn has(self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// Both warp slots of one oscillator, flattened for the audio callback.
#[derive(Debug, Clone, Copy)]
struct WarpParams {
    slot1: WarpSlot,
    slot2: WarpSlot,
}

impl WarpParams {
    fn new(
        mode1: WarpMode,
        amount1: f64,
        var1: f64,
        mode2: WarpMode,
        amount2: f64,
        var2: f64,
    ) -> Self {
        Self {
            slot1: WarpSlot::new(mode1, amount1, var1),
            slot2: WarpSlot::new(mode2, amount2, var2),
        }
    }

    fn bypass(self) -> bool {
        // Every mode outside the warp-stage sets is a readout no-op, so an
        // empty flag union is exactly the old `is_bypass()` pair.
        (self.slot1.flags | self.slot2.flags) == 0
    }

    /// Only sync scales the shared increment, and both slots
    /// multiply into it.
    fn sync_factor(self) -> f64 {
        [self.slot1, self.slot2]
            .into_iter()
            .filter(|slot| slot.has(WARP_SYNC))
            .map(|slot| warp_mod::sync_factor(slot.amount))
            .product()
    }

    fn has_sync(self) -> bool {
        (self.slot1.flags | self.slot2.flags) & WARP_SYNC != 0
    }
}

/// Advance the sync window by the unwarped increment and restart the
/// oscillator when the window wraps. The flag reports that restart.
fn advance_sync_window(
    warp: WarpParams,
    increment: f64,
    window: &mut f64,
    phase: &mut f64,
) -> (f64, bool) {
    if !warp.has_sync() {
        return (*window, false);
    }
    let next = *window + increment.max(0.0);
    if next >= 1.0 {
        *window = next.rem_euclid(1.0);
        *phase = *window;
        (*window, true)
    } else {
        *window = next;
        (*window, false)
    }
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn read_warped(
    waveform: Waveform,
    phase: f64,
    dt: f64,
    output_dt: f64,
    pulse_width: f64,
    wavetable_position: f64,
    custom_wavetable: Option<&CustomWavetable>,
    mip: &mut MipState,
    phase_delay: f64,
) -> f64 {
    oscillator_sample_with_pulse_width(
        waveform,
        phase,
        dt,
        output_dt,
        pulse_width,
        wavetable_position,
        custom_wavetable,
        mip,
        phase_delay,
    )
}

/// Exact `rem_euclid(1.0)` with a fast path for values already in `[0, 1)`.
#[inline(always)]
fn wrap_unit(value: f64) -> f64 {
    if (0.0..1.0).contains(&value) {
        value
    } else {
        value.rem_euclid(1.0)
    }
}

/// Warps slot 2 first, then slot 1; `window` is the sync window phase and a
/// scratch mip copy keeps extra readouts out of the crossfade bookkeeping.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn oscillator_sample_with_warp(
    waveform: Waveform,
    phase: f64,
    dt: f64,
    output_dt: f64,
    pulse_width: f64,
    wavetable_position: f64,
    custom_wavetable: Option<&CustomWavetable>,
    warp: WarpParams,
    window: f64,
    mip: &mut MipState,
) -> f64 {
    if warp.bypass() {
        return read_warped(
            waveform,
            phase,
            dt,
            output_dt,
            pulse_width,
            wavetable_position,
            custom_wavetable,
            mip,
            IMPORTED_READOUT_DELAY * output_dt,
        );
    }
    // Callers pass a phase already in [0, 1); avoid an `fmod` per sample.
    debug_assert!((0.0..=1.0).contains(&phase));
    let phase = if phase.is_finite() { phase } else { 0.0 };
    // The readout kernel's group delay is a time shift: apply it before the
    // phase map so a folded map is not wrapped past the table edge.
    let readout_delay = match custom_wavetable {
        Some(table) if table.imported_readout => IMPORTED_READOUT_DELAY * output_dt,
        _ => 0.0,
    };
    let mut warped = wrap_unit(phase - readout_delay);
    let mut slope = 1.0;
    let mut gain = 1.0;
    for slot in [warp.slot2, warp.slot1] {
        if slot.has(WARP_PHASE) {
            let (next, local) = warp_mod::phase_warp(slot.mode, slot.amount, warped);
            warped = next;
            slope *= local;
        }
        if slot.has(WARP_FLIP) {
            gain *= warp_mod::flip_sign(warped, slot.amount);
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_SELFPD) {
            let mut scratch = *mip;
            let source = read_warped(
                waveform,
                warped,
                dt,
                output_dt,
                pulse_width,
                wavetable_position,
                custom_wavetable,
                &mut scratch,
                0.0,
            );
            warped = wrap_unit(warped + slot.amount * source);
        }
    }
    // Warped rates drive our (deliberate) band-limit selection; a hold or a
    // staircase has no local slope, so it keeps the unwarped rate.
    let slope = if slope.is_finite() && slope > 0.0 {
        slope
    } else {
        1.0
    };
    let read_dt = output_dt * slope;
    let mut output = read_warped(
        waveform,
        warped,
        dt * slope,
        read_dt,
        pulse_width,
        wavetable_position,
        custom_wavetable,
        mip,
        0.0,
    );
    // A PWM map splices a ramp of the default bank onto a hold at the frame's
    // cycle boundary; the read cannot band-limit the two corners that makes.
    if waveform == Waveform::Wavetable
        && custom_wavetable.is_none()
        && let Some(amount) = pwm_slot_amount(warp)
    {
        let boundary_slope = default_boundary_slope(mip.level, mip.bin, wavetable_position);
        output += pwm_corner_correction(phase, dt, amount, boundary_slope);
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_EVENODD) {
            let mut scratch = *mip;
            let second = read_warped(
                waveform,
                wrap_unit(warped + 0.5),
                dt,
                read_dt,
                pulse_width,
                wavetable_position,
                custom_wavetable,
                &mut scratch,
                0.0,
            );
            output += (second + second) * (slot.amount - 0.5);
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_DIST) {
            output = warp_mod::shaper(output, slot.amount, slot.mode.dist_id());
        }
    }
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_SYNC) {
            gain *= warp_mod::sync_window(window, slot.var);
        }
        if slot.has(WARP_RM_GAIN) {
            gain *= 1.0 - slot.amount;
        }
    }
    output * gain
}

fn poly_blamp(phase: f64, dt: f64) -> f64 {
    let dt = dt.abs().min(0.5);
    if dt == 0.0 {
        return 0.0;
    }
    let distance = phase.min(1.0 - phase);
    if distance < dt {
        let x = 1.0 - distance / dt;
        dt * x * x * x / 3.0
    } else {
        0.0
    }
}

/// The one phase-warp slot when it is a PWM map below the identity amount.
/// Chained phase maps and `amount == 1` (a full-width ramp) have no corners of
/// their own, so they keep the plain readout.
fn pwm_slot_amount(warp: WarpParams) -> Option<f64> {
    let mut amount = None;
    for slot in [warp.slot1, warp.slot2] {
        if slot.has(WARP_PHASE) {
            if slot.mode != WarpMode::Pwm || amount.is_some() {
                return None;
            }
            amount = Some(slot.amount);
        }
    }
    amount.filter(|amount| *amount < 1.0)
}

/// Band-limit the corners the PWM map makes in the default bank. The map runs a
/// ramp of the frame out at `1 / amount` and then holds, so both corners are
/// slope jumps of `+/- boundary_slope / amount`; neither is a step, because the
/// readout is a continuous periodic function of table phase. `boundary_slope` is
/// the readout's own slope across the frame's cycle boundary, so the residual is
/// in signal units whatever frame, position and mip level are playing.
#[inline(always)]
fn pwm_corner_correction(phase: f64, dt: f64, amount: f64, boundary_slope: f64) -> f64 {
    let slope = boundary_slope / (2.0 * amount);
    let at_amount = if phase < amount {
        phase - amount + 1.0
    } else {
        phase - amount
    };
    slope * (poly_blamp(phase, dt) - poly_blamp(at_amount, dt))
}

const WAVETABLE_SIZE: usize = 2_048;
const MAX_CUSTOM_WAVETABLE_SAMPLES: usize = WAVETABLE_SIZE * 256;
/// Frames in the built-in table a wavetable oscillator plays when the patch
/// names no table: saw, sine, triangle, double saw, square, then four pulses.
const DEFAULT_SHAPE_COUNT: usize = 9;

/// The default table is a basic-shapes bank, not a single frame: a tableless
/// slot still has to respond to the position control. Frames are sampled from
/// their analytic shape and normalised once; the mip chain band-limits them.
fn default_shapes_table() -> &'static CustomWavetable {
    static TABLE: OnceLock<CustomWavetable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut samples = Vec::with_capacity(DEFAULT_SHAPE_COUNT * WAVETABLE_SIZE);
        for shape in 0..DEFAULT_SHAPE_COUNT {
            let mut frame: Vec<f64> = (0..WAVETABLE_SIZE)
                .map(|sample| default_shape_sample(shape, sample as f64 / WAVETABLE_SIZE as f64))
                .collect();
            let peak = frame
                .iter()
                .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
            if peak > f64::EPSILON {
                for sample in &mut frame {
                    *sample /= peak;
                }
            }
            samples.extend(frame);
        }
        CustomWavetable::from_frames(samples, WAVETABLE_SIZE, false)
    })
}

/// Slope of the default bank's readout across the cycle boundary, per mip level,
/// cutoff bin and frame, in signal units per table cycle. The PWM map splices the
/// ramp to the hold there, so the corner residual needs the readout's slope, not
/// the raw frame's. One bank, built once with the table.
fn default_boundary_slopes()
-> &'static [[[f64; DEFAULT_SHAPE_COUNT]; READOUT_BINS]; MIP_LEVEL_LIMIT] {
    static SLOPES: OnceLock<[[[f64; DEFAULT_SHAPE_COUNT]; READOUT_BINS]; MIP_LEVEL_LIMIT]> =
        OnceLock::new();
    SLOPES.get_or_init(|| {
        let mut slopes = [[[0.0; DEFAULT_SHAPE_COUNT]; READOUT_BINS]; MIP_LEVEL_LIMIT];
        for (level, data) in default_shapes_table()
            .prepare_mips()
            .levels
            .iter()
            .enumerate()
        {
            let level_len = WAVETABLE_SIZE >> level;
            let epsilon = 0.5 / level_len as f64;
            for (bin, row) in slopes[level].iter_mut().enumerate() {
                let table = readout_table(level, bin);
                for (frame, slope) in row.iter_mut().enumerate() {
                    let block = &data[frame * level_len..];
                    let after = table.read(block, level_len, epsilon);
                    let before = table.read(block, level_len, 1.0 - epsilon);
                    *slope = (after - before) / (2.0 * epsilon);
                }
            }
        }
        slopes
    })
}

/// The default bank's boundary slope at the level and cutoff bin the readout just
/// used, blended across the two frames the position sits between.
fn default_boundary_slope(level: usize, bin: usize, position: f64) -> f64 {
    let slopes = default_boundary_slopes();
    let frame =
        position.clamp(UNIT_FRACTION.min, UNIT_FRACTION.max) * (DEFAULT_SHAPE_COUNT - 1) as f64;
    let first = (frame.floor() as usize).min(DEFAULT_SHAPE_COUNT - 1);
    let second = (first + 1).min(DEFAULT_SHAPE_COUNT - 1);
    let mix = frame.fract();
    let row = &slopes[level.min(MIP_LEVEL_LIMIT - 1)][bin.min(READOUT_BINS - 1)];
    row[first] + (row[second] - row[first]) * mix
}

/// One cycle of a default frame. Shapes run from the full-band saw through the
/// narrowest pulse so the position control has a continuous timbre path.
fn default_shape_sample(shape: usize, phase: f64) -> f64 {
    match shape {
        0 => 2.0 * ((phase + 0.5) % 1.0) - 1.0,
        1 => (TAU * phase).sin(),
        2 => 1.0 - 4.0 * (((phase + 0.25) % 1.0) - 0.5).abs(),
        3 => {
            if phase < 0.5 {
                1.0 - 2.0 * phase
            } else {
                2.0 * phase - 2.0
            }
        }
        4 => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        _ => {
            let duty = 0.5_f64.powi(shape as i32 - 3);
            if phase >= 0.5 - duty && phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

fn poly_blep(t: f64, dt: f64) -> f64 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// Peak ceiling of the instrument output (-6.2 dBFS) and the level below which
/// it passes the signal untouched. Every gain-raising stage is bounded so this
/// master stage only ever sees the summed worst case; a preview therefore
/// never has to render and measure the figure to stay under the headroom. The
/// transparent knee covers every pinned render. See `docs/synth-levels.md`.
const OUTPUT_CEILING: f64 = 0.49;
const OUTPUT_CEILING_KNEE: f64 = 0.9 * OUTPUT_CEILING;

fn soft_clip(value: f64) -> f64 {
    let magnitude = value.abs();
    if magnitude <= OUTPUT_CEILING_KNEE {
        return value;
    }
    let span = OUTPUT_CEILING - OUTPUT_CEILING_KNEE;
    let shaped = OUTPUT_CEILING - span * (-(magnitude - OUTPUT_CEILING_KNEE) / span).exp();
    shaped.copysign(value)
}

/// The noise oscillator is a bright source; its low end is removed so the
/// noise layer does not dominate the sub/bass bands.
const NOISE_HP_HZ: f64 = 500.0;

/// The BrightWhite sample is a little quieter than the engine's uniform
/// generator; matching its RMS keeps the noise layer at the authored level.
const NOISE_RMS_SCALE: f64 = 0.860;

/// Largest voice-amplitude multiplier a `VoiceAmp` route may reach (+6 dB).
const MAX_VOICE_AMP: f64 = 2.0;

/// Shared filter input stage: gain `(1 + drive)^4`, hard clamp at `+/-1.5`,
/// then `x - (4/27)x^3`; drive 0 stays transparent for unity gain. The cubic
/// clamp is the stage's normalisation: the input gain reaches +24 dB, but the
/// output peak cannot pass `saturate(1.5) = 1.0`, and the master ceiling bounds
/// what is left. See `docs/synth-levels.md`.
#[inline]
fn drive_gain(drive: f64) -> f64 {
    (1.0 + drive.clamp(0.0, 1.0)).powi(4)
}

/// The saturator itself; shared by the drive stage and the Moog input.
#[inline]
fn saturate(value: f64) -> f64 {
    let x = value.clamp(-1.5, 1.5);
    x - (4.0 / 27.0) * x * x * x
}

fn drive_signal(value: f64, drive: f64) -> f64 {
    if drive <= f64::EPSILON {
        return value;
    }
    saturate(value * drive_gain(drive))
}

/// One source's stereo contribution to the dual-filter inputs, or to the dry
/// voice output when the route bypasses both filters.
#[inline]
fn add_routed_source(
    route: SourceFilterRoute,
    source: [f64; 2],
    filter1: &mut [f64; 2],
    filter2: &mut [f64; 2],
    dry: &mut [f64; 2],
) {
    match route.target {
        SourceFilterTarget::Filter1 => {
            filter1[0] += source[0];
            filter1[1] += source[1];
        }
        SourceFilterTarget::Filter2 => {
            filter2[0] += source[0];
            filter2[1] += source[1];
        }
        SourceFilterTarget::Both => {
            let balance = route.balance.clamp(0.0, 1.0);
            let to_filter1 = 1.0 - balance;
            filter1[0] += source[0] * to_filter1;
            filter1[1] += source[1] * to_filter1;
            filter2[0] += source[0] * balance;
            filter2[1] += source[1] * balance;
        }
        SourceFilterTarget::Bypass => {
            dry[0] += source[0];
            dry[1] += source[1];
        }
    }
}

/// Run one channel through the selected voice filter model with cached
/// coefficients; shared by the legacy mix and the per-source path.
#[inline]
// DSP hot path; a parameter struct would be rebuilt per call.
#[allow(clippy::too_many_arguments)]
fn process_voice_filter(
    model: FilterModel,
    svf: &mut StateVariableFilter,
    ladder: &mut LadderFilter,
    moog: &mut MoogLadder,
    input: f64,
    drive: f64,
    coefficients: FilterCoefficients,
    mode: FilterMode,
) -> f64 {
    // The drive stage precedes every model. The Moog saturator is always on;
    // the other families keep drive 0 transparent.
    let input = if model.is_moog() {
        saturate(input * drive_gain(drive)) * coefficients.input_gain
    } else {
        drive_signal(input, drive) * coefficients.input_gain
    };
    match model {
        FilterModel::Svf => svf.process_mode_cached(
            input,
            coefficients.g,
            coefficients.damping,
            coefficients.recip,
            mode,
        ),
        FilterModel::Svf1 => svf.process_first_order(input, coefficients.g, mode),
        FilterModel::Svf3 | FilterModel::Svf4 => svf.process_cascade(
            input,
            coefficients.g,
            coefficients.damping,
            coefficients.recip,
            coefficients.skirt_recip,
            mode,
            model.poles() - 2,
        ),
        FilterModel::Ladder
        | FilterModel::Ladder3
        | FilterModel::Ladder1
        | FilterModel::DirtyLadder => ladder.process(
            input,
            coefficients.ladder_g,
            coefficients.ladder_k,
            model == FilterModel::DirtyLadder,
            model.poles(),
        ),
        FilterModel::Moog6 | FilterModel::Moog12 | FilterModel::Moog18 | FilterModel::Moog24 => {
            moog.process(
                input,
                coefficients.moog_g,
                coefficients.moog_k,
                coefficients.moog_feedback,
                model.poles(),
            )
        }
    }
}

fn param_as_f64(value: &ParamValue) -> Option<f64> {
    match value {
        // A switch is a 0/1 parameter: without this arm `lfo.sync: true` was
        // silently dropped while `lfo.sync: 1` worked.
        ParamValue::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        ParamValue::String(text) => text.parse().ok(),
        other => other.as_f64(),
    }
}

/// Split `fx.<index>.<field>` (the field may itself contain dots).
fn parse_fx_param_name(name: &str) -> Option<(usize, &str)> {
    let rest = name.strip_prefix("fx.")?;
    let (index, field) = rest.split_once('.')?;
    if field.is_empty() {
        return None;
    }
    Some((index.parse().ok()?, field))
}

fn fx_type_from_param(value: &ParamValue) -> Option<SynthFxType> {
    match value {
        ParamValue::String(text) => SynthFxType::from_label(text)
            .or_else(|| text.parse().ok().and_then(SynthFxType::from_index)),
        _ => param_as_f64(value).and_then(SynthFxType::from_index),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod unit_fraction_tests;
