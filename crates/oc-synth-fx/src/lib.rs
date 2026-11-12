//! Compact synth-level FX rack.
//!
//! The rack runs after the voice sum and before the output soft clip. It holds
//! at most [`MAX_SYNTH_FX`] modules with a fixed parameter palette per type.
//! A patch stores the tree in preorder: serial nodes, and split or parallel
//! nodes whose child chains follow them. [`SynthFxChain`] resolves that tree
//! into branch spans when the patch loads, so processing never allocates.
//!
//! Every time-based module owns an independent state, up to the rack capacity.
//! Two states per type are initially prepared; additional states are allocated
//! only by topology preparation in [`SynthFxChain::sync_from_patch`]. Buffers
//! are sized for [`MAX_FX_SAMPLE_RATE`]; higher rates clamp delay times to the
//! available storage instead of allocating on the audio thread.

use oc_dsp::AudioEffect;
use oc_dsp::biquad::{BiquadCoefficients, BiquadState};
use oc_dsp::compander::{Compander, lookahead_samples};
use oc_dsp::fft::{Complex, Fft};
use oc_dsp::kernel::{HALF_BAND_CENTER, HALF_BAND_TAPS, half_band_kernel, kaiser, sinc};
use oc_dsp::multiband::MultibandCompressor;
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};

/// Largest number of modules one synth rack can hold.
pub const MAX_SYNTH_FX: usize = 16;
pub(crate) const MAX_FX_VALUE_SLOTS: usize = 40;
/// Largest sample rate the preallocated time-effect buffers are sized for.
pub const MAX_FX_SAMPLE_RATE: f64 = 96_000.0;
/// Longest delay time the preallocated lines can represent.
pub const MAX_DELAY_SECONDS: f64 = 1.2;
const INITIAL_TIME_INSTANCES: usize = 2;
const MAX_DELAY_SAMPLES: usize = 115_204; // 1.2 s at 96 kHz
/// Delay line length per channel: 2^21 samples, 43.7 s at 48 kHz.
const DELAY_LINE_SAMPLES: usize = 1 << 21;
/// Reported delay latency: 62.5 ms, rounded to whole samples.
const DELAY_LATENCY_SECONDS: f64 = 0.0625;
const DELAY_LATENCY_MAX_SAMPLES: usize = 6_000; // 62.5 ms at 96 kHz
const DELAY_INTERP_TAPS: usize = 8;
const DELAY_INTERP_PHASES: usize = 256;
const MAX_PREDELAY_SAMPLES: usize = 19_204; // 0.2 s at 96 kHz
const MAX_CHORUS_SAMPLES: usize = 5_764; // 60 ms at 96 kHz
const MAX_FLANGER_SAMPLES: usize = 1_536; // 16 ms at 96 kHz
const PHASER_STAGES: usize = 4;
const TAIL_CAP_SECONDS: f64 = 30.0;
/// Highest single-band threshold the measured import law produces, in dB.
pub const COMPRESSOR_KNEE_OFFSET_DB: f64 = 13.50;

/// One synth-level FX module type; its wire spelling is the snake_case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynthFxType {
    Drive,
    Compressor,
    Eq3,
    Delay,
    Reverb,
    Chorus,
    Distortion,
    Phaser,
    Flanger,
    Filter,
    Utils,
    Split,
    Split3,
    SplitMs,
    Bode,
    ParallelRack,
    HyperD,
    Conv,
    Multiband,
}

impl SynthFxType {
    /// Every module type in numeric selector order.
    pub const ALL: [SynthFxType; 19] = [
        SynthFxType::Drive,
        SynthFxType::Compressor,
        SynthFxType::Eq3,
        SynthFxType::Delay,
        SynthFxType::Reverb,
        SynthFxType::Chorus,
        SynthFxType::Distortion,
        SynthFxType::Phaser,
        SynthFxType::Flanger,
        SynthFxType::Filter,
        SynthFxType::Utils,
        SynthFxType::Split,
        SynthFxType::Split3,
        SynthFxType::SplitMs,
        SynthFxType::Bode,
        SynthFxType::ParallelRack,
        SynthFxType::HyperD,
        SynthFxType::Conv,
        SynthFxType::Multiband,
    ];

    /// Project-file spelling used by `fx.N.type` (also the serde form).
    pub fn label(self) -> &'static str {
        match self {
            SynthFxType::Drive => "drive",
            SynthFxType::Compressor => "compressor",
            SynthFxType::Eq3 => "eq3",
            SynthFxType::Delay => "delay",
            SynthFxType::Reverb => "reverb",
            SynthFxType::Chorus => "chorus",
            SynthFxType::Distortion => "distortion",
            SynthFxType::Phaser => "phaser",
            SynthFxType::Flanger => "flanger",
            SynthFxType::Filter => "filter",
            SynthFxType::Utils => "utils",
            SynthFxType::Split => "split",
            SynthFxType::Split3 => "split3",
            SynthFxType::SplitMs => "split_ms",
            SynthFxType::Bode => "bode",
            SynthFxType::ParallelRack => "parallel",
            SynthFxType::HyperD => "hyperd",
            SynthFxType::Conv => "conv",
            SynthFxType::Multiband => "multiband",
        }
    }

    /// Numeric `fx.N.type` encoding. Keep this order stable: it is part of the
    /// preset ABI. The imported preset has its own FX enum; `foreign_preset.rs` keeps it
    /// provisional table instead of reusing this one.
    pub fn index(self) -> usize {
        match self {
            SynthFxType::Drive => 0,
            SynthFxType::Compressor => 1,
            SynthFxType::Eq3 => 2,
            SynthFxType::Delay => 3,
            SynthFxType::Reverb => 4,
            SynthFxType::Chorus => 5,
            SynthFxType::Distortion => 6,
            SynthFxType::Phaser => 7,
            SynthFxType::Flanger => 8,
            SynthFxType::Filter => 9,
            SynthFxType::Utils => 10,
            SynthFxType::Split => 11,
            SynthFxType::Split3 => 12,
            SynthFxType::SplitMs => 13,
            SynthFxType::Bode => 14,
            SynthFxType::ParallelRack => 15,
            SynthFxType::HyperD => 16,
            SynthFxType::Conv => 17,
            SynthFxType::Multiband => 18,
        }
    }

    /// Resolve a user-facing or patch label, ignoring case and separators.
    pub fn from_label(label: &str) -> Option<Self> {
        let normalized = label
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-', '_'], "");
        match normalized.as_str() {
            "drive" | "overdrive" => Some(SynthFxType::Drive),
            "distortion" | "dist" | "saturation" | "saturator" => Some(SynthFxType::Distortion),
            "compressor" | "comp" => Some(SynthFxType::Compressor),
            "eq" | "eq3" | "equalizer" => Some(SynthFxType::Eq3),
            "delay" | "echo" => Some(SynthFxType::Delay),
            "reverb" | "verb" => Some(SynthFxType::Reverb),
            "chorus" => Some(SynthFxType::Chorus),
            "phaser" => Some(SynthFxType::Phaser),
            "flanger" => Some(SynthFxType::Flanger),
            "filter" => Some(SynthFxType::Filter),
            "utils" => Some(SynthFxType::Utils),
            "split" => Some(SynthFxType::Split),
            "split3" => Some(SynthFxType::Split3),
            "splitms" => Some(SynthFxType::SplitMs),
            "bode" => Some(SynthFxType::Bode),
            "parallel" | "parallelrack" => Some(SynthFxType::ParallelRack),
            "hyperd" | "dimension" => Some(SynthFxType::HyperD),
            "conv" | "convolution" | "ir" => Some(SynthFxType::Conv),
            "multiband" | "mbcomp" | "multibandcomp" => Some(SynthFxType::Multiband),
            _ => None,
        }
    }

    /// Resolve the numeric selector stored in a patch; non-integral values
    /// round to the nearest selector.
    pub fn from_index(index: f64) -> Option<Self> {
        match index.round() as i32 {
            0 => Some(SynthFxType::Drive),
            1 => Some(SynthFxType::Compressor),
            2 => Some(SynthFxType::Eq3),
            3 => Some(SynthFxType::Delay),
            4 => Some(SynthFxType::Reverb),
            5 => Some(SynthFxType::Chorus),
            6 => Some(SynthFxType::Distortion),
            7 => Some(SynthFxType::Phaser),
            8 => Some(SynthFxType::Flanger),
            9 => Some(SynthFxType::Filter),
            10 => Some(SynthFxType::Utils),
            11 => Some(SynthFxType::Split),
            12 => Some(SynthFxType::Split3),
            13 => Some(SynthFxType::SplitMs),
            14 => Some(SynthFxType::Bode),
            15 => Some(SynthFxType::ParallelRack),
            16 => Some(SynthFxType::HyperD),
            17 => Some(SynthFxType::Conv),
            18 => Some(SynthFxType::Multiband),
            _ => None,
        }
    }

    /// The preset parameter palette for this type.
    pub fn default_params(self) -> &'static [(&'static str, f64)] {
        match self {
            SynthFxType::Drive => &[
                ("drive", 0.35),
                ("tone", 0.55),
                ("level", 0.8),
                ("mix", 1.0),
                ("level_out", 1.0),
            ],
            SynthFxType::Compressor => &[
                ("crossover_low", 120.0),
                ("crossover_high", 2_500.0),
                ("threshold", -18.0),
                ("ratio", 4.0),
                ("threshold_below", -30.0),
                ("ratio_below", 1.0),
                ("attack", 10.0),
                ("release", 100.0),
                ("band1_gain", 0.0),
                ("band2_gain", 0.0),
                ("band3_gain", 0.0),
                ("makeup", 0.0),
                ("mix", 1.0),
                ("limiter", 1.0),
                ("level_out", 1.0),
            ],
            SynthFxType::Eq3 => &[
                ("low_freq", 250.0),
                ("high_freq", 4_000.0),
                ("low", 0.0),
                ("mid", 0.0),
                ("high", 0.0),
                // Negative means no band-type data: keep the legacy shelves.
                ("type1", -1.0),
                ("type2", -1.0),
                ("reso1", 60.0),
                ("reso2", 60.0),
                ("level_out", 1.0),
            ],
            SynthFxType::Delay => &[
                ("time", 0.25),
                ("feedback", 0.35),
                ("tone", 0.55),
                ("mix", 0.3),
                ("level_out", 1.0),
            ],
            SynthFxType::Reverb => &[
                ("size", 0.5),
                ("damp", 0.4),
                ("predelay", 0.02),
                ("width", 0.8),
                ("mix", 0.25),
                ("low", 0.5),
                ("high", 0.5),
                ("level_out", 1.0),
            ],
            SynthFxType::Chorus => &[
                ("rate", 0.6),
                ("depth", 0.35),
                ("feedback", 0.1),
                ("mix", 0.35),
                ("level_out", 1.0),
                ("delay", 7.0),
                ("delay2", 13.0),
            ],
            SynthFxType::Distortion => &[
                ("drive", 0.4),
                // A fully open tone control leaves the default distortion unfiltered.
                ("tone", 1.0),
                ("level", 0.7),
                ("mix", 1.0),
                ("level_out", 1.0),
                // Tone-filter bandwidth in octaves; 0 keeps the one-pole tone.
                ("bw", 0.0),
                // Shaper family (0 = soft clip) and cascade length 2..=16.
                ("mode", 0.0),
                ("stages", 2.0),
            ],
            SynthFxType::Phaser => &[
                ("rate", 0.5),
                ("depth", 0.7),
                ("feedback", 0.35),
                ("mix", 0.5),
                ("level_out", 1.0),
            ],
            SynthFxType::Flanger => &[
                ("rate", 0.25),
                ("depth", 0.7),
                ("feedback", 0.5),
                ("mix", 0.5),
                ("level_out", 1.0),
            ],
            SynthFxType::Filter => &[
                ("cutoff", 2_000.0),
                ("resonance", 0.2),
                ("mode", 0.0),
                ("mix", 1.0),
                ("level_out", 1.0),
            ],
            // HPF is 24 dB/oct (LR4), LPF 12 dB/oct (Butterworth); see
            // docs/fx-utils-split-bode.md. Width is a percentage, 100 = neutral.
            SynthFxType::Utils => &[
                ("hpf", 20.0),
                ("lpf", 20_000.0),
                ("lf_mono", 0.0),
                ("lf_xover", 120.0),
                ("width", 100.0),
                ("balance", 0.0),
                ("polarity_l", 0.0),
                ("polarity_r", 0.0),
                ("mix", 1.0),
                ("level_out", 1.0),
            ],
            // Counts are the number of following serial modules in each band.
            // The band gains are ours: unity keeps the split magnitude-flat.
            SynthFxType::Split => &[
                ("freq", 500.0),
                ("count1", 0.0),
                ("count2", 0.0),
                ("bypass1", 0.0),
                ("bypass2", 0.0),
                ("level_out", 1.0),
                ("gain1", 1.0),
                ("gain2", 1.0),
            ],
            SynthFxType::Split3 => &[
                ("freq", 300.0),
                ("freq2", 2_000.0),
                ("count1", 0.0),
                ("count2", 0.0),
                ("count3", 0.0),
                ("bypass1", 0.0),
                ("bypass2", 0.0),
                ("bypass3", 0.0),
                ("level_out", 1.0),
                ("gain1", 1.0),
                ("gain2", 1.0),
                ("gain3", 1.0),
            ],
            SynthFxType::SplitMs => &[
                ("count1", 0.0),
                ("count2", 0.0),
                ("bypass1", 0.0),
                ("bypass2", 0.0),
                ("level_out", 1.0),
                ("gain1", 1.0),
                ("gain2", 1.0),
            ],
            // Parallel rack: three branches fed the same input and summed with
            // their own gains. The default is a single dry branch.
            SynthFxType::ParallelRack => &[
                ("count1", 0.0),
                ("count2", 0.0),
                ("count3", 0.0),
                ("gain1", 1.0),
                ("gain2", 0.0),
                ("gain3", 0.0),
                ("level_out", 1.0),
            ],
            // `shift` is a percentage of `range`; both sidebands are summed at
            // output mix 0 and output width spreads them across the stereo field.
            SynthFxType::Bode => &[
                ("shift", 0.0),
                ("range", 22.4),
                ("feedback", 0.0),
                ("delay", 0.0),
                ("output_mix", 0.0),
                ("output_width", 100.0),
                ("mono_input", 0.0),
                ("mix", 1.0),
                ("level_out", 1.0),
                // Swap the two single-sideband outputs.
                ("swap_ab", 0.0),
            ],
            // `spread` scales the short allpass network; `feedback` is the
            // allpass coefficient (diffusion depth), not a delay loop.
            SynthFxType::HyperD => &[
                ("rate", 0.35),
                ("depth", 0.35),
                ("feedback", 0.5),
                ("spread", 1.0),
                ("mix", 0.5),
                ("level_out", 1.0),
            ],
            // `size` selects one of four generated rooms/plates; `damp` is a
            // one-pole low-pass on the wet path, `predelay` is pre-convolution.
            SynthFxType::Conv => &[
                ("size", 0.5),
                ("damp", 0.3),
                ("predelay", 0.02),
                ("width", 0.8),
                ("mix", 0.25),
                ("level_out", 1.0),
            ],
            // Three-band compander over the oc-dsp device. `range` stays 0 so a
            // freshly inserted band is neutral; each band's makeup is static.
            SynthFxType::Multiband => &[
                ("band_count", 1.0),
                ("slope", 1.0),
                ("phase_mode", 0.0),
                ("crossover_1", 125.0),
                ("crossover_2", 2_500.0),
                ("mix", 1.0),
                ("detector_link", 1.0),
                ("makeup", 0.0),
                ("level_out", 1.0),
                ("band1_enabled", 1.0),
                ("band1_threshold", 0.0),
                ("band1_threshold_below", -60.0),
                ("band1_range", 0.0),
                ("band1_ratio", 1.0),
                ("band1_ratio_below", 1.0),
                ("band1_attack", 10.0),
                ("band1_release", 100.0),
                ("band1_knee", 6.0),
                ("band1_makeup", 0.0),
                ("band2_enabled", 1.0),
                ("band2_threshold", 0.0),
                ("band2_threshold_below", -60.0),
                ("band2_range", 0.0),
                ("band2_ratio", 1.0),
                ("band2_ratio_below", 1.0),
                ("band2_attack", 10.0),
                ("band2_release", 100.0),
                ("band2_knee", 6.0),
                ("band2_makeup", 0.0),
                ("band3_enabled", 1.0),
                ("band3_threshold", 0.0),
                ("band3_threshold_below", -60.0),
                ("band3_range", 0.0),
                ("band3_ratio", 1.0),
                ("band3_ratio_below", 1.0),
                ("band3_attack", 10.0),
                ("band3_release", 100.0),
                ("band3_knee", 6.0),
                ("band3_makeup", 0.0),
            ],
        }
    }
}

fn default_enabled() -> bool {
    true
}

/// One configured module in a patch's FX tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthFx {
    pub fx_type: SynthFxType,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub params: Vec<(String, f64)>,
}

impl SynthFx {
    /// A module of this type with its default palette.
    pub fn new(fx_type: SynthFxType) -> Self {
        Self {
            fx_type,
            enabled: true,
            params: fx_type
                .default_params()
                .iter()
                .map(|(name, value)| ((*name).to_string(), *value))
                .collect(),
        }
    }

    /// The stored value of one parameter, if the module declares it.
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
    }

    /// Update an existing parameter in place. Creating new named parameters is
    /// intentionally limited to the load/import path so live automation on the
    /// audio thread never grows this vector.
    pub fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        if let Some(entry) = self.params.iter_mut().find(|(key, _)| key == name) {
            entry.1 = value;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FxModule {
    fx_type: SynthFxType,
    enabled: bool,
    values: [f64; MAX_FX_VALUE_SLOTS],
    /// Unmodulated values. `values` is reset from this on every FX-modulation
    /// control tick so a removed route leaves no residue.
    base_values: [f64; MAX_FX_VALUE_SLOTS],
    /// Occurrence index in this type's retained pool of prepared time-effect states.
    time_slot: usize,
    /// Slot of the post-mix imported output trim; [`usize::MAX`] means unity.
    level_out_slot: usize,
}

impl FxModule {
    fn from_patch(fx: &SynthFx, time_slot: usize) -> Self {
        // Start from this type's defaults so a patch that omits a key cannot
        // inherit an unrelated module's slot value.
        let mut values = [0.0; MAX_FX_VALUE_SLOTS];
        for (name, value) in fx.fx_type.default_params() {
            if let Some(slot) = fx_param_slot(fx.fx_type, name) {
                values[slot] = *value;
            }
        }
        for (name, value) in &fx.params {
            if !value.is_finite() {
                continue;
            }
            if let Some(slot) = fx_param_slot(fx.fx_type, name) {
                values[slot] = *value;
            }
        }
        Self {
            fx_type: fx.fx_type,
            enabled: fx.enabled,
            values,
            base_values: values,
            time_slot,
            level_out_slot: fx_param_slot(fx.fx_type, "level_out").unwrap_or(usize::MAX),
        }
    }
}

impl Default for FxModule {
    fn default() -> Self {
        let mut values = [0.0; MAX_FX_VALUE_SLOTS];
        for (slot, (_, value)) in SynthFxType::Drive.default_params().iter().enumerate() {
            values[slot] = *value;
        }
        Self {
            fx_type: SynthFxType::Drive,
            enabled: false,
            values,
            base_values: values,
            time_slot: 0,
            level_out_slot: usize::MAX,
        }
    }
}

fn fx_param_slot(fx_type: SynthFxType, name: &str) -> Option<usize> {
    fx_type
        .default_params()
        .iter()
        .position(|(key, _)| *key == name)
}

/// Algorithmic latency of an enabled FX rack, in samples. This is the value the
/// render graph uses for source delay compensation; it must match
/// [`SynthFxChain::latency_samples`] for the same modules.
pub fn fx_rack_latency(fx: &[SynthFx], sample_rate: f64) -> usize {
    let mut total = 0;
    for module in fx.iter().take(MAX_SYNTH_FX) {
        if !module.enabled {
            continue;
        }
        total += match module.fx_type {
            // The delay reports its fixed 62.5 ms even at wet 0, because the
            // dry path is delayed too.
            SynthFxType::Delay => delay_latency_samples(sample_rate),
            SynthFxType::Distortion => DISTORTION_LATENCY,
            SynthFxType::Compressor if module.param("limiter").unwrap_or(1.0) >= 0.5 => {
                lookahead_samples(sample_rate)
            }
            SynthFxType::Conv => CONV_LATENCY,
            SynthFxType::Multiband if module.param("phase_mode").map(f64::round) == Some(2.0) => {
                oc_dsp::crossover::LINEAR_DELAY
            }
            _ => 0,
        };
    }
    total
}

fn branch_count(module: &FxModule, band: usize) -> usize {
    let slots: &[usize] = match module.fx_type {
        SynthFxType::Split => &[SPLIT_COUNT1, SPLIT_COUNT2],
        SynthFxType::Split3 => &[SPLIT3_COUNT1, SPLIT3_COUNT2, SPLIT3_COUNT3],
        SynthFxType::SplitMs => &[SPLIT_MS_COUNT1, SPLIT_MS_COUNT2],
        SynthFxType::ParallelRack => &[PARALLEL_COUNT1, PARALLEL_COUNT2, PARALLEL_COUNT3],
        _ => return 0,
    };
    let Some(slot) = slots.get(band) else {
        return 0;
    };
    (module.values[*slot].round().max(0.0) as usize).min(MAX_SYNTH_FX)
}

#[derive(Debug, Clone, Copy, Default)]
struct DriveState {
    tone_left: f64,
    tone_right: f64,
}

impl DriveState {
    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let drive = 1.0 + values[0].clamp(0.0, 1.0) * 24.0;
        let cutoff = 250.0 * (20_000.0_f64 / 250.0).powf(values[1].clamp(0.0, 1.0));
        let coeff = (1.0 - (-TAU * cutoff / sr).exp()).clamp(0.0, 1.0);
        let level = values[2].clamp(0.0, 2.0);
        let mix = values[3].clamp(0.0, 1.0);
        let compensation = drive.sqrt().max(1.0);
        let wet_left = (left * drive).tanh() / compensation;
        let wet_right = (right * drive).tanh() / compensation;
        self.tone_left += coeff * (wet_left - self.tone_left);
        self.tone_right += coeff * (wet_right - self.tone_right);
        let wet_left = self.tone_left * level;
        let wet_right = self.tone_right * level;
        mix_stereo(left, right, wet_left, wet_right, mix)
    }
}

/// `bw` slot of the distortion palette (appended after `level_out`).
const DISTORTION_BW: usize = 5;
/// `mode` and `stages` slots of the distortion palette.
const DISTORTION_MODE: usize = 6;
const DISTORTION_STAGES: usize = 7;
/// Eight-times oversampling through three half-band stages each way adds 97
/// base-rate samples of nominal delay.
const DISTORTION_LATENCY: usize = 97;
/// Input-drive curve sampled at 10% control steps. Calibration uses a 0.165
/// reference peak; the separate output curve controls small-signal gain.
const DISTORTION_DRIVE: [f64; 11] = [
    0.5, 1.0706, 1.6409, 2.2154, 2.901, 3.8441, 5.0917, 5.6395, 6.3128, 7.7246, 26.68,
];

/// Reference peak for the drive curve; hotter input receives less drive.
const DISTORTION_REFERENCE_PEAK: f64 = 0.165;

/// Small-signal gain: 0.5 (-6 dB) at zero drive rising to 28.6 (+29.1 dB) at
/// full. The 0.9825 keeps the one-pole boost finite at the top of the knob.
fn distortion_gain(amount: f64) -> f64 {
    0.5 / (1.0 - 0.9825 * amount)
}

/// Post-shaper trim: the small-signal gain law divided by the input drive.
#[cfg(test)]
fn distortion_trim(amount: f64) -> f64 {
    distortion_gain(amount) / table_lerp(&DISTORTION_DRIVE, amount)
}

/// Linear interpolation over a uniform 0..1 table.
fn table_lerp(table: &[f64; 11], amount: f64) -> f64 {
    let position = amount.clamp(0.0, 1.0) * 10.0;
    let index = (position.floor() as usize).min(9);
    let fraction = position - index as f64;
    table[index] + (table[index + 1] - table[index]) * fraction
}

/// Two cascaded 2x interpolators built from the synth's existing 113-tap
/// half-band kernel. The same kernel decimates back to the base rate.
const HALF_BAND_HISTORY: usize = HALF_BAND_TAPS;

struct HalfBandUp {
    history: [f64; HALF_BAND_HISTORY],
    write: usize,
}

impl HalfBandUp {
    fn new() -> Self {
        Self {
            history: [0.0; HALF_BAND_HISTORY],
            write: 0,
        }
    }

    /// Push one sample at the input rate; return its two interpolated samples.
    fn push(&mut self, input: f64) -> [f64; 2] {
        self.history[self.write] = input;
        let kernel = half_band_kernel();
        let mut even = 0.0;
        let mut odd = 0.0;
        for index in 0..=HALF_BAND_CENTER {
            let sample = self.history[(self.write + HALF_BAND_HISTORY - index) % HALF_BAND_HISTORY];
            even += kernel[2 * index] * sample;
            if index < HALF_BAND_CENTER {
                odd += kernel[2 * index + 1] * sample;
            }
        }
        self.write = (self.write + 1) % HALF_BAND_HISTORY;
        [2.0 * even, 2.0 * odd]
    }
}

struct HalfBandDown {
    history: [f64; HALF_BAND_HISTORY],
    write: usize,
    next: bool,
}

impl HalfBandDown {
    fn new() -> Self {
        Self {
            history: [0.0; HALF_BAND_HISTORY],
            write: 0,
            next: false,
        }
    }

    /// Push one sample at the input rate; return every second sample.
    fn push(&mut self, input: f64) -> Option<f64> {
        self.history[self.write] = input;
        self.write = (self.write + 1) % HALF_BAND_HISTORY;
        let output = self.next.then(|| {
            let kernel = half_band_kernel();
            let mut sum = 0.0;
            for (tap, &coefficient) in kernel.iter().enumerate().take(HALF_BAND_HISTORY) {
                let index = (self.write + HALF_BAND_HISTORY - 1 - tap) % HALF_BAND_HISTORY;
                sum += coefficient * self.history[index];
            }
            sum
        });
        self.next = !self.next;
        output
    }
}

/// 8x shaper path: three half-band stages up, the nonlinearity at 8x, three
/// stages down. Fixed [`DISTORTION_LATENCY`] samples of nominal latency.
struct Oversampler8x {
    up_first: HalfBandUp,
    up_second: HalfBandUp,
    up_third: HalfBandUp,
    down_first: HalfBandDown,
    down_second: HalfBandDown,
    down_third: HalfBandDown,
}

impl Oversampler8x {
    fn new() -> Self {
        Self {
            up_first: HalfBandUp::new(),
            up_second: HalfBandUp::new(),
            up_third: HalfBandUp::new(),
            down_first: HalfBandDown::new(),
            down_second: HalfBandDown::new(),
            down_third: HalfBandDown::new(),
        }
    }

    fn reset(&mut self) {
        for state in [&mut self.up_first, &mut self.up_second, &mut self.up_third] {
            state.history.fill(0.0);
            state.write = 0;
        }
        for state in [
            &mut self.down_first,
            &mut self.down_second,
            &mut self.down_third,
        ] {
            state.history.fill(0.0);
            state.write = 0;
            state.next = false;
        }
    }

    fn process(&mut self, input: f64, mut shape: impl FnMut(f64) -> f64) -> f64 {
        let mut shaped = [0.0; 8];
        for (first, intermediate) in self.up_first.push(input).into_iter().enumerate() {
            for (second, half) in self.up_second.push(intermediate).into_iter().enumerate() {
                for (third, high) in self.up_third.push(half).into_iter().enumerate() {
                    shaped[first * 4 + second * 2 + third] = shape(high);
                }
            }
        }
        let mut output = 0.0;
        for high in shaped {
            if let Some(quarter) = self.down_first.push(high)
                && let Some(half) = self.down_second.push(quarter)
                && let Some(base) = self.down_third.push(half)
            {
                output = base;
            }
        }
        output
    }
}

/// Shaper families. The numeric value is the project-file `mode` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShaperMode {
    Soft,
    Hard,
    Fold,
    Asymmetric,
    Diode,
    Crush,
}

impl ShaperMode {
    fn from_value(value: f64) -> Self {
        match value.round() as i64 {
            1 => Self::Hard,
            2 => Self::Fold,
            3 => Self::Asymmetric,
            4 => Self::Diode,
            5 => Self::Crush,
            _ => Self::Soft,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Soft => 0,
            Self::Hard => 1,
            Self::Fold => 2,
            Self::Asymmetric => 3,
            Self::Diode => 4,
            Self::Crush => 5,
        }
    }
}

/// Asymmetric bias and the small-signal gain `1 + b*sech^2(b)` it introduces.
const ASYMMETRIC_BIAS: f64 = 0.35;
const ASYMMETRIC_GAIN: f64 = 1.310_402;
/// Diode rectified-blend boost and the mean slope `1 + boost/2`.
const DIODE_BOOST: f64 = 0.6;
const DIODE_GAIN: f64 = 1.3;
/// Quantise blend, the staircase resolution and the slope `1 + blend`.
const CRUSH_BLEND: f64 = 0.7;
const CRUSH_STEPS: f64 = 8.0;
const CRUSH_GAIN: f64 = 1.7;

/// One stage law, normalized to unit small-signal slope at zero.
fn shaper_stage(law: usize, input: f64) -> f64 {
    match law {
        0 => input.tanh(),
        1 => input.clamp(-1.0, 1.0),
        2 => input.sin(),
        3 => {
            let curved = input.tanh()
                + ASYMMETRIC_BIAS * ((input + ASYMMETRIC_BIAS).tanh() - ASYMMETRIC_BIAS.tanh());
            curved / ASYMMETRIC_GAIN
        }
        4 => (input.tanh() + DIODE_BOOST * input.tanh().max(0.0)) / DIODE_GAIN,
        _ => {
            let staircase = (1.0 + CRUSH_BLEND) * input
                - CRUSH_BLEND * (input * CRUSH_STEPS).round() / CRUSH_STEPS;
            (staircase / CRUSH_GAIN).clamp(-1.0, 1.0)
        }
    }
}

/// Cascade `stages` laws, rotating through the palette from the mode's law.
/// Stages are distinct: after the sixth the knee scale changes.
fn shaper_cascade(mode: ShaperMode, stages: usize, input: f64) -> f64 {
    let mut value = input;
    for stage in 0..stages.clamp(2, 16) {
        let law = (mode.index() + stage) % 6;
        let scale = 1.0 + 0.12 * (stage / 6) as f64;
        value = shaper_stage(law, value * scale) / scale;
    }
    value
}

/// One-pole DC blocker for the asymmetric laws; corner at 10 Hz.
#[derive(Debug, Clone, Copy, Default)]
struct DcBlocker {
    input: f64,
    output: f64,
}

impl DcBlocker {
    fn process(&mut self, input: f64, sr: f64) -> f64 {
        let pole = (-TAU * 10.0 / sr.max(8_000.0)).exp();
        let output = input - self.input + pole * self.output;
        self.input = input;
        self.output = output;
        output
    }
}

struct DistortionState {
    tone_left: f64,
    tone_right: f64,
    shaper_left: Box<Oversampler8x>,
    shaper_right: Box<Oversampler8x>,
    dc_left: DcBlocker,
    dc_right: DcBlocker,
    dry_left: [f64; DISTORTION_LATENCY],
    dry_right: [f64; DISTORTION_LATENCY],
    dry_write: usize,
    /// Program peak for the level-compensated drive, 50 ms release.
    peak: f64,
}

impl DistortionState {
    fn reset(&mut self) {
        self.tone_left = 0.0;
        self.tone_right = 0.0;
        self.shaper_left.reset();
        self.shaper_right.reset();
        self.dc_left = DcBlocker::default();
        self.dc_right = DcBlocker::default();
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
        self.dry_write = 0;
        self.peak = 0.0;
    }

    fn new() -> Self {
        Self {
            tone_left: 0.0,
            tone_right: 0.0,
            shaper_left: Box::new(Oversampler8x::new()),
            shaper_right: Box::new(Oversampler8x::new()),
            dc_left: DcBlocker::default(),
            dc_right: DcBlocker::default(),
            dry_left: [0.0; DISTORTION_LATENCY],
            dry_right: [0.0; DISTORTION_LATENCY],
            dry_write: 0,
            peak: 0.0,
        }
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let amount = values[0].clamp(0.0, 1.0);
        let linear = amount <= f64::EPSILON;
        let sample_peak = left.abs().max(right.abs());
        let rate = sr.max(1.0);
        if linear {
            self.peak = 0.0;
        } else {
            let coefficient = if sample_peak > self.peak {
                1.0 - (-1.0 / (0.001 * rate)).exp()
            } else {
                1.0 - (-1.0 / (0.05 * rate)).exp()
            };
            self.peak += coefficient * (sample_peak - self.peak);
        }
        // Keep the shaper at the measured 0.165-peak operating point so the
        // harmonic profile does not turn into peak clamping on hot material.
        let reference = self.peak.max(DISTORTION_REFERENCE_PEAK);
        let drive = if linear {
            table_lerp(&DISTORTION_DRIVE, amount)
        } else {
            table_lerp(&DISTORTION_DRIVE, amount) * (DISTORTION_REFERENCE_PEAK / reference)
        };
        let mode = ShaperMode::from_value(values[DISTORTION_MODE]);
        let stages = values[DISTORTION_STAGES].round().clamp(2.0, 16.0) as usize;
        // Drive 0 is the documented harmonic null: a linear gain through the
        // same 8x path, so the module latency does not change with the knob.
        let shaped_left = self.shaper_left.process(left * drive, |input| {
            if linear {
                input
            } else {
                shaper_cascade(mode, stages, input)
            }
        });
        let shaped_right = self.shaper_right.process(right * drive, |input| {
            if linear {
                input
            } else {
                shaper_cascade(mode, stages, input)
            }
        });
        let cutoff = 250.0 * (18_000.0_f64 / 250.0).powf(values[1].clamp(0.0, 1.0));
        let coeff = (1.0 - (-TAU * cutoff / sr).exp()).clamp(0.0, 1.0);
        self.tone_left += coeff * (shaped_left - self.tone_left);
        self.tone_right += coeff * (shaped_right - self.tone_right);
        // BW runs the shaper's tone filter from fully applied (0) to wide open
        // (8); the blend amount is ours because the reference topology is not
        // established.
        let bw = values[DISTORTION_BW].clamp(0.0, 8.0);
        let filter_amount = (1.0 - bw / 8.0).clamp(0.0, 1.0);
        let level = values[2].clamp(0.0, 2.0);
        let trim = distortion_gain(amount) / drive;
        let wet_left = self.dc_left.process(
            shaped_left + (self.tone_left - shaped_left) * filter_amount,
            sr,
        ) * level
            * trim;
        let wet_right = self.dc_right.process(
            shaped_right + (self.tone_right - shaped_right) * filter_amount,
            sr,
        ) * level
            * trim;

        let dry_left = self.dry_left[self.dry_write];
        let dry_right = self.dry_right[self.dry_write];
        self.dry_left[self.dry_write] = left;
        self.dry_right[self.dry_write] = right;
        self.dry_write = (self.dry_write + 1) % DISTORTION_LATENCY;
        mix_stereo(
            dry_left,
            dry_right,
            wet_left,
            wet_right,
            values[3].clamp(0.0, 1.0),
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PhaserState {
    x1: [[f64; PHASER_STAGES]; 2],
    y1: [[f64; PHASER_STAGES]; 2],
    feedback: [f64; 2],
    phase: f64,
}

impl PhaserState {
    fn process_channel(
        &mut self,
        channel: usize,
        input: f64,
        depth: f64,
        feedback: f64,
        lfo: f64,
        sr: f64,
    ) -> f64 {
        let frequency = (700.0 * 8.0_f64.powf(depth * lfo)).clamp(80.0, sr * 0.45);
        let tangent = (PI * frequency / sr).tan();
        let coefficient = (tangent - 1.0) / (tangent + 1.0);
        let mut signal = input + self.feedback[channel] * feedback;
        for stage in 0..PHASER_STAGES {
            let x = signal;
            let y =
                coefficient * x + self.x1[channel][stage] - coefficient * self.y1[channel][stage];
            self.x1[channel][stage] = x;
            self.y1[channel][stage] = y;
            signal = y;
        }
        self.feedback[channel] = signal;
        signal
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let rate = values[0].clamp(0.05, 8.0);
        let depth = values[1].clamp(0.0, 1.0);
        let feedback = values[2].clamp(0.0, 0.85);
        let mix = values[3].clamp(0.0, 1.0);
        self.phase = (self.phase + rate / sr).fract();
        let lfo_left = 0.5 + 0.5 * (TAU * self.phase).sin();
        let lfo_right = 0.5 + 0.5 * (TAU * (self.phase + 0.25)).sin();
        let wet_left = self.process_channel(0, left, depth, feedback, lfo_left, sr);
        let wet_right = self.process_channel(1, right, depth, feedback, lfo_right, sr);
        mix_stereo(left, right, wet_left, wet_right, mix)
    }
}

struct FlangerState {
    left: Box<[f64]>,
    right: Box<[f64]>,
    write: usize,
    phase: f64,
    staging: PowerSplit,
}

impl FlangerState {
    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
        self.phase = 0.0;
        self.staging.reset();
    }

    fn new() -> Self {
        Self {
            left: vec![0.0; MAX_FLANGER_SAMPLES].into_boxed_slice(),
            right: vec![0.0; MAX_FLANGER_SAMPLES].into_boxed_slice(),
            write: 0,
            phase: 0.0,
            staging: PowerSplit::new(48_000.0),
        }
    }

    fn read(buffer: &[f64], write: usize, delay: f64) -> f64 {
        let len = buffer.len();
        let mut position = write as f64 - delay;
        while position < 0.0 {
            position += len as f64;
        }
        let position = position % len as f64;
        let index = position.floor() as usize;
        let fraction = position - index as f64;
        let next = (index + 1) % len;
        buffer[index] + (buffer[next] - buffer[index]) * fraction
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let rate = values[0].clamp(0.05, 8.0);
        let depth = values[1].clamp(0.0, 1.0);
        let feedback = values[2].clamp(0.0, 0.85);
        let mix = values[3].clamp(0.0, 1.0);
        self.phase = (self.phase + rate / sr).fract();
        let lfo_left = 0.5 + 0.5 * (TAU * self.phase).sin();
        let lfo_right = 0.5 + 0.5 * (TAU * (self.phase + 0.25)).sin();
        let max_delay = (self.left.len() - 2) as f64;
        let delay_left = ((0.0005 + depth * 0.0045 * lfo_left) * sr).clamp(1.0, max_delay);
        let delay_right = ((0.0005 + depth * 0.0045 * lfo_right) * sr).clamp(1.0, max_delay);
        let delayed_left = Self::read(&self.left, self.write, delay_left);
        let delayed_right = Self::read(&self.right, self.write, delay_right);
        self.left[self.write] = left + delayed_left * feedback;
        self.right[self.write] = right + delayed_right * feedback;
        self.write = (self.write + 1) % self.left.len();
        self.staging.prepare(sr);
        let share = self
            .staging
            .share([left, right], [delayed_left, delayed_right], mix);
        let dry_gain = 1.0 - share;
        let wet_gain = mix;
        (
            left * dry_gain + delayed_left * wet_gain,
            right * dry_gain + delayed_right * wet_gain,
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    ic1: [f64; 2],
    ic2: [f64; 2],
}

impl FilterState {
    // DSP hot path; a parameter struct would be rebuilt per call.
    #[allow(clippy::too_many_arguments)]
    fn process_channel(
        &mut self,
        channel: usize,
        input: f64,
        a1: f64,
        a2: f64,
        a3: f64,
        k: f64,
        mode: i32,
    ) -> f64 {
        let v3 = input - self.ic2[channel];
        let v1 = a1 * self.ic1[channel] + a2 * v3;
        let v2 = self.ic2[channel] + a2 * self.ic1[channel] + a3 * v3;
        self.ic1[channel] = 2.0 * v1 - self.ic1[channel];
        self.ic2[channel] = 2.0 * v2 - self.ic2[channel];
        match mode {
            1 => input - k * v1 - v2,
            2 => v1,
            _ => v2,
        }
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let cutoff = values[0].clamp(20.0, 20_000.0).min(sr * 0.45);
        let resonance = values[1].clamp(0.0, 0.95);
        let mode = values[2].round() as i32;
        let mix = values[3].clamp(0.0, 1.0);
        let g = (PI * cutoff / sr).tan();
        let k = 2.0 - 1.9 * resonance;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let wet_left = self.process_channel(0, left, a1, a2, a3, k, mode);
        let wet_right = self.process_channel(1, right, a1, a2, a3, k, mode);
        mix_stereo(left, right, wet_left, wet_right, mix)
    }
}

/// Three-band RMS compander with an output lookahead limiter. `prepare` runs
/// on the load path; `process` never allocates, locks or spawns.
struct CompressorState {
    compander: Option<Box<Compander>>,
    cached: [f64; MAX_FX_VALUE_SLOTS],
    cached_sr: u32,
    configured: bool,
}

impl Default for CompressorState {
    fn default() -> Self {
        Self {
            compander: None,
            cached: [0.0; MAX_FX_VALUE_SLOTS],
            cached_sr: 0,
            configured: false,
        }
    }
}

impl CompressorState {
    fn prepare(&mut self, sr: u32) {
        if self.compander.is_none() || self.cached_sr != sr {
            self.cached_sr = sr;
            self.compander = Some(Box::new(Compander::new(sr)));
            self.configured = false;
        }
    }

    fn configure(&mut self, values: &[f64; MAX_FX_VALUE_SLOTS]) {
        let Some(compander) = self.compander.as_mut() else {
            return;
        };
        if self.configured && self.cached == *values {
            return;
        }
        self.cached = *values;
        self.configured = true;
        for (slot, (name, _)) in SynthFxType::Compressor.default_params().iter().enumerate() {
            if *name == "level_out" {
                continue;
            }
            compander.set_param(name, clamp_static_gain(name, values[slot]));
        }
    }

    fn process(&mut self, left: f64, right: f64, values: &[f64; MAX_FX_VALUE_SLOTS]) -> (f64, f64) {
        if !self.configured || self.cached != *values {
            self.configure(values);
        }
        let Some(compander) = self.compander.as_mut() else {
            return (left, right);
        };
        compander.process_frame(left, right)
    }

    fn latency_samples(&self) -> usize {
        self.compander
            .as_ref()
            .map_or(0, |compander| compander.latency_samples())
    }
}

/// Three-band compander from `oc-dsp`, configured from the module palette.
/// The compressor allocates its crossover state on first use; that happens on
/// the load path (`sync_from_patch`), never inside the audio callback.
struct MultibandState {
    compressor: Option<Box<MultibandCompressor>>,
    cached: [f64; MAX_FX_VALUE_SLOTS],
    cached_sr: u32,
    configured: bool,
}

impl Default for MultibandState {
    fn default() -> Self {
        Self {
            compressor: None,
            cached: [0.0; MAX_FX_VALUE_SLOTS],
            cached_sr: 0,
            configured: false,
        }
    }
}

impl MultibandState {
    fn ensure(&mut self, sr: u32) {
        if self.compressor.is_none() || self.cached_sr != sr {
            self.cached_sr = sr;
            self.compressor = Some(Box::new(MultibandCompressor::new(sr)));
            self.configured = false;
        }
    }

    /// Push the palette into the device when any value changed. The rack runs
    /// per sample, so this only does work on a control-tick change.
    fn configure(&mut self, values: &[f64; MAX_FX_VALUE_SLOTS]) {
        let Some(compressor) = self.compressor.as_mut() else {
            return;
        };
        if self.configured && self.cached == *values {
            return;
        }
        self.cached = *values;
        self.configured = true;
        for (slot, (name, _)) in SynthFxType::Multiband.default_params().iter().enumerate() {
            if *name == "level_out" || *name == "makeup" {
                continue;
            }
            compressor.set_param(name, clamp_static_gain(name, values[slot]));
        }
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let rate = sr.max(8_000.0).round() as u32;
        self.ensure(rate);
        self.configure(values);
        let Some(compressor) = self.compressor.as_mut() else {
            return (left, right);
        };
        let mut buffer = [left as f32, right as f32];
        compressor.process(&mut buffer, rate);
        // Global make-up is a linear gain; the preset stores it as an amplitude.
        let makeup = 10f64.powf(values[MB_MAKEUP].min(MAX_FX_STATIC_GAIN_DB) / 20.0);
        (f64::from(buffer[0]) * makeup, f64::from(buffer[1]) * makeup)
    }

    fn latency_samples(&self) -> usize {
        self.compressor
            .as_ref()
            .map(|compressor| compressor.latency_samples() as usize)
            .unwrap_or(0)
    }
}

const MB_MAKEUP: usize = 7;

/// Largest static gain a compressor make-up or band-gain control may add
/// (+12 dB). A make-up beyond the compression it compensates only raises the
/// module output, so the stage carries a ceiling of its own; every import this
/// bank pins sits below it. See `docs/synth-levels.md`.
const MAX_FX_STATIC_GAIN_DB: f64 = 12.0;

/// Clamp the gain-bearing compressor parameters. `makeup` is a make-up gain and
/// `band*_gain` a static band trim; every other parameter keeps its own range.
fn clamp_static_gain(name: &str, value: f64) -> f64 {
    let gain_bearing = name == "makeup" || name.ends_with("_gain") || name.ends_with("_makeup");
    if gain_bearing {
        value.clamp(-MAX_FX_STATIC_GAIN_DB, MAX_FX_STATIC_GAIN_DB)
    } else {
        value
    }
}

// Imported FXEQ band slots. Types are 0 = shelf, 1 = bell, 2 = cut and -1 means
// the patch carries no type data, which keeps the pre-import shelves.
const EQ_LOW_FREQ: usize = 0;
const EQ_HIGH_FREQ: usize = 1;
const EQ_LOW_GAIN: usize = 2;
const EQ_MID_GAIN: usize = 3;
const EQ_HIGH_GAIN: usize = 4;
const EQ_TYPE1: usize = 5;
const EQ_TYPE2: usize = 6;
const EQ_RESO1: usize = 7;
const EQ_RESO2: usize = 8;

/// Band shapes are selected by a numeric shelf, bell or cut value.
const EQ_TYPE_SHELF: i32 = 0;
const EQ_TYPE_BELL: i32 = 1;
const EQ_TYPE_CUT: i32 = 2;

/// RBJ shelf/bell amplitude term `A = 10^(dB/40)`: the section peak gain is
/// then the requested dB instead of its square.
fn eq_amplitude(gain_db: f64) -> f64 {
    10f64.powf(gain_db.clamp(-24.0, 24.0) / 40.0)
}

fn normalized_biquad(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> BiquadCoefficients {
    if !a0.is_finite() || a0.abs() < 1e-12 {
        return BiquadCoefficients::default();
    }
    BiquadCoefficients::from_raw(b0, b1, b2, a0, a1, a2)
}

/// RBJ peaking EQ. `q` is the imported resonance mapping.
fn peaking(freq: f64, gain_db: f64, q: f64, sr: f64) -> BiquadCoefficients {
    let (sin, cos) = eq_angle(freq, sr);
    let a = eq_amplitude(gain_db);
    let alpha = sin / (2.0 * q.max(1e-3));
    normalized_biquad(
        1.0 + alpha * a,
        -2.0 * cos,
        1.0 - alpha * a,
        1.0 + alpha / a,
        -2.0 * cos,
        1.0 - alpha / a,
    )
}

fn high_pass(freq: f64, q: f64, sr: f64) -> BiquadCoefficients {
    let (sin, cos) = eq_angle(freq, sr);
    let alpha = sin / (2.0 * q.max(1e-3));
    normalized_biquad(
        (1.0 + cos) * 0.5,
        -(1.0 + cos),
        (1.0 + cos) * 0.5,
        1.0 + alpha,
        -2.0 * cos,
        1.0 - alpha,
    )
}

fn low_pass(freq: f64, q: f64, sr: f64) -> BiquadCoefficients {
    let (sin, cos) = eq_angle(freq, sr);
    let alpha = sin / (2.0 * q.max(1e-3));
    normalized_biquad(
        (1.0 - cos) * 0.5,
        1.0 - cos,
        (1.0 - cos) * 0.5,
        1.0 + alpha,
        -2.0 * cos,
        1.0 - alpha,
    )
}

fn low_shelf(freq: f64, gain_db: f64, slope: f64, sr: f64) -> BiquadCoefficients {
    let (sin, cos) = eq_angle(freq, sr);
    let a = eq_amplitude(gain_db);
    let alpha = eq_shelf_alpha(a, sin, slope);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    normalized_biquad(
        a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
        2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
        a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
        (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
        -2.0 * ((a - 1.0) + (a + 1.0) * cos),
        (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
    )
}

fn high_shelf(freq: f64, gain_db: f64, slope: f64, sr: f64) -> BiquadCoefficients {
    let (sin, cos) = eq_angle(freq, sr);
    let a = eq_amplitude(gain_db);
    let alpha = eq_shelf_alpha(a, sin, slope);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
    normalized_biquad(
        a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
        a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
        (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
        2.0 * ((a - 1.0) - (a + 1.0) * cos),
        (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
    )
}

fn eq_angle(freq: f64, sr: f64) -> (f64, f64) {
    let w0 = TAU * freq.clamp(10.0, sr * 0.45) / sr;
    (w0.sin(), w0.cos())
}

/// RBJ shelf alpha for the band's slope.
fn eq_shelf_alpha(a: f64, sin: f64, slope: f64) -> f64 {
    let argument = (a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0;
    sin * 0.5 * argument.max(0.01).sqrt()
}

/// Map the resonance control (0..100) to bell or cut Q.
fn eq_q(reso: f64) -> f64 {
    let x = (reso / 100.0).clamp(0.0, 1.0);
    0.1 + 18.75 * x * x * x * x
}

/// Map the resonance control (0..100) to shelf transition steepness.
fn eq_shelf_slope(reso: f64) -> f64 {
    let x = (reso / 100.0).clamp(0.0, 1.0);
    0.37 + 3.13 * x * x * x
}

#[derive(Debug, Clone, Copy, Default)]
struct EqChannelState {
    legacy_low: f64,
    legacy_high_lowpass: f64,
    band1: BiquadState,
    band2: BiquadState,
}

#[derive(Debug, Clone, Copy)]
struct EqState {
    left: EqChannelState,
    right: EqChannelState,
    band1: BiquadCoefficients,
    band2: BiquadCoefficients,
    cached: [f64; MAX_FX_VALUE_SLOTS],
    cached_sr: f64,
    configured: bool,
}

impl Default for EqState {
    fn default() -> Self {
        Self {
            left: EqChannelState::default(),
            right: EqChannelState::default(),
            band1: BiquadCoefficients::default(),
            band2: BiquadCoefficients::default(),
            cached: [0.0; MAX_FX_VALUE_SLOTS],
            cached_sr: 0.0,
            configured: false,
        }
    }
}

impl EqState {
    fn band_coefficients(
        band: usize,
        freq: f64,
        gain_db: f64,
        kind: f64,
        reso: f64,
        sr: f64,
    ) -> BiquadCoefficients {
        match kind.round() as i32 {
            EQ_TYPE_SHELF if band == 0 => low_shelf(freq, gain_db, eq_shelf_slope(reso), sr),
            EQ_TYPE_SHELF => high_shelf(freq, gain_db, eq_shelf_slope(reso), sr),
            EQ_TYPE_BELL => peaking(freq, gain_db, eq_q(reso), sr),
            EQ_TYPE_CUT if band == 0 => high_pass(freq, eq_q(reso), sr),
            EQ_TYPE_CUT => low_pass(freq, eq_q(reso), sr),
            _ => BiquadCoefficients::default(),
        }
    }

    fn update(&mut self, values: &[f64; MAX_FX_VALUE_SLOTS], sr: f64) {
        if self.configured && self.cached_sr == sr && self.cached == *values {
            return;
        }
        self.cached = *values;
        self.cached_sr = sr;
        self.configured = true;
        self.band1 = Self::band_coefficients(
            0,
            values[EQ_LOW_FREQ],
            values[EQ_LOW_GAIN],
            values[EQ_TYPE1],
            values[EQ_RESO1],
            sr,
        );
        self.band2 = Self::band_coefficients(
            1,
            values[EQ_HIGH_FREQ],
            values[EQ_HIGH_GAIN],
            values[EQ_TYPE2],
            values[EQ_RESO2],
            sr,
        );
    }

    fn process_channel(
        state: &mut EqChannelState,
        band1: &BiquadCoefficients,
        band2: &BiquadCoefficients,
        input: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> f64 {
        let kind1 = values[EQ_TYPE1].round() as i32;
        let kind2 = values[EQ_TYPE2].round() as i32;
        if kind1 < 0 && kind2 < 0 {
            let low_freq = values[EQ_LOW_FREQ].clamp(20.0, 20_000.0);
            let high_freq = values[EQ_HIGH_FREQ].clamp(20.0, 20_000.0);
            let low_coeff = 1.0 - (-TAU * low_freq / sr).exp();
            let high_coeff = 1.0 - (-TAU * high_freq / sr).exp();
            state.legacy_low += low_coeff * (input - state.legacy_low);
            state.legacy_high_lowpass += high_coeff * (input - state.legacy_high_lowpass);
            let low = state.legacy_low;
            let high = input - state.legacy_high_lowpass;
            let mid = input - low - high;
            return low * db_to_gain(values[EQ_LOW_GAIN])
                + mid * db_to_gain(values[EQ_MID_GAIN])
                + high * db_to_gain(values[EQ_HIGH_GAIN]);
        }
        let mut signal = input;
        if (EQ_TYPE_SHELF..=EQ_TYPE_CUT).contains(&kind1) {
            signal = state.band1.process(band1, signal);
        } else {
            let low_freq = values[EQ_LOW_FREQ].clamp(20.0, 20_000.0);
            let coeff = 1.0 - (-TAU * low_freq / sr).exp();
            state.legacy_low += coeff * (signal - state.legacy_low);
            signal += (db_to_gain(values[EQ_LOW_GAIN]) - 1.0) * state.legacy_low;
        }
        if (EQ_TYPE_SHELF..=EQ_TYPE_CUT).contains(&kind2) {
            signal = state.band2.process(band2, signal);
        } else {
            let high_freq = values[EQ_HIGH_FREQ].clamp(20.0, 20_000.0);
            let coeff = 1.0 - (-TAU * high_freq / sr).exp();
            state.legacy_high_lowpass += coeff * (signal - state.legacy_high_lowpass);
            let high = signal - state.legacy_high_lowpass;
            signal += (db_to_gain(values[EQ_HIGH_GAIN]) - 1.0) * high;
        }
        signal
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        self.update(values, sr);
        (
            Self::process_channel(&mut self.left, &self.band1, &self.band2, left, values, sr),
            Self::process_channel(&mut self.right, &self.band1, &self.band2, right, values, sr),
        )
    }
}

fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db.clamp(-24.0, 24.0) / 20.0)
}

/// The FX wet control is an amplitude crossfade: both paths sum to unity,
/// so a module whose wet equals its dry stays level-neutral.
fn wet_dry_gains(mix: f64) -> (f64, f64) {
    let mix = mix.clamp(0.0, 1.0);
    (1.0 - mix, mix)
}

/// Settled staging for a decorrelated wet path: dry and wet powers add, so the
/// constant-power split `sqrt(1 - w)` / `sqrt(w)` keeps the module neutral.
#[cfg(test)]
fn wet_dry_power(mix: f64) -> (f64, f64) {
    let mix = mix.clamp(0.0, 1.0);
    ((1.0 - mix).sqrt(), mix.sqrt())
}

/// Time constant of the wet/dry power estimate, long enough that the ratio
/// follows the programme envelope rather than the waveform.
const POWER_SPLIT_SECONDS: f64 = 0.02;

/// Digital-silence power (below -200 dBFS) where the split re-arms.
const POWER_SILENCE: f64 = 1e-20;

/// Power split for a wet path that arrives later than the dry. The split is
/// primed by the wet's first arrival: until then the dry keeps its level, and
/// once the wet has carried programme the settled constant-power pair applies.
#[derive(Debug, Clone, Copy)]
struct PowerSplit {
    dry_power: f64,
    wet_power: f64,
    ratio: f64,
    rate: f64,
    coefficient: f64,
}

impl PowerSplit {
    fn reset(&mut self) {
        self.dry_power = 0.0;
        self.wet_power = 0.0;
        self.ratio = 0.0;
    }

    fn new(sr: f64) -> Self {
        let mut split = Self {
            dry_power: 0.0,
            wet_power: 0.0,
            ratio: 0.0,
            rate: 0.0,
            coefficient: 0.0,
        };
        split.prepare(sr);
        split
    }

    fn prepare(&mut self, sr: f64) {
        let rate = sr.max(8_000.0);
        if (self.rate - rate).abs() < 0.5 {
            return;
        }
        self.rate = rate;
        self.coefficient = 1.0 - (-1.0 / (POWER_SPLIT_SECONDS * rate)).exp();
    }

    /// Share of the dry that the wet compensates this frame: the primed wet
    /// power share, plus the share the control commits while the wet is still
    /// absent, so the dry vanishes at full wet and not before.
    fn share(&mut self, dry: [f64; 2], wet: [f64; 2], mix: f64) -> f64 {
        let mix = mix.clamp(0.0, 1.0);
        let dry_power = 0.5 * (dry[0] * dry[0] + dry[1] * dry[1]);
        let wet_power = 0.5 * (wet[0] * wet[0] + wet[1] * wet[1]);
        self.dry_power += self.coefficient * (dry_power - self.dry_power);
        self.wet_power += self.coefficient * (wet_power - self.wet_power);
        let measured = if self.dry_power > POWER_SILENCE {
            (self.wet_power / self.dry_power).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.ratio = self.ratio.max(measured);
        if self.dry_power < POWER_SILENCE && self.wet_power < POWER_SILENCE {
            self.ratio = 0.0;
        }
        (mix * (self.ratio + (1.0 - self.ratio) * mix * mix)).clamp(0.0, 1.0)
    }
}

fn mix_stereo(left: f64, right: f64, wet_left: f64, wet_right: f64, mix: f64) -> (f64, f64) {
    let (dry_gain, wet_gain) = wet_dry_gains(mix);
    (
        left * dry_gain + wet_left * wet_gain,
        right * dry_gain + wet_right * wet_gain,
    )
}

/// Time control maps to seconds through `t = 2^(20x - 2)`, so 0.05/0.1/0.2
/// give 0.5/1/4 s. See docs/fx-delay-distortion.md.
fn delay_time_seconds(control: f64) -> f64 {
    2f64.powf(20.0 * control.clamp(0.0, 1.0) - 2.0)
}

/// Time control in samples, clamped to the preallocated line and the latency.
fn delay_time_samples(control: f64, sr: f64) -> f64 {
    let latency = delay_latency_samples(sr) as f64;
    let maximum = (DELAY_LINE_SAMPLES - DELAY_INTERP_TAPS) as f64 - latency;
    (delay_time_seconds(control) * sr).clamp(1.0, maximum)
}

/// Reported module latency in samples: 62.5 ms, rounded and capped at the
/// preallocated dry ring.
fn delay_latency_samples(sr: f64) -> usize {
    ((DELAY_LATENCY_SECONDS * sr).round() as usize).min(DELAY_LATENCY_MAX_SAMPLES)
}

/// ZDF one-pole low-pass coefficient `k = g / (1 + g)` with
/// `g = tan(pi * cutoff / sr)`.
fn damping_coefficient(cutoff: f64, sr: f64) -> f64 {
    let g = (PI * cutoff.clamp(20.0, sr * 0.49) / sr).tan();
    g / (1.0 + g)
}

/// Trapezoidal one-pole low-pass used in the delay feedback path.
#[derive(Debug, Clone, Copy, Default)]
struct ZdfDamping {
    state: f64,
}

impl ZdfDamping {
    fn process(&mut self, input: f64, coefficient: f64) -> f64 {
        let v = (input - self.state) * coefficient;
        let output = v + self.state;
        self.state = output + v;
        output
    }
}

/// 8-tap, 256-phase windowed-sinc table. Phase 0 is the centre tap.
fn delay_interpolation_taps() -> [[f64; DELAY_INTERP_TAPS]; DELAY_INTERP_PHASES] {
    let mut table = [[0.0; DELAY_INTERP_TAPS]; DELAY_INTERP_PHASES];
    for (phase, row) in table.iter_mut().enumerate() {
        let fraction = phase as f64 / DELAY_INTERP_PHASES as f64;
        let mut sum = 0.0;
        for (tap, coefficient) in row.iter_mut().enumerate() {
            let offset = fraction - (tap as f64 - 3.0);
            *coefficient = sinc(offset) * kaiser(tap, DELAY_INTERP_TAPS, 8.0);
            sum += *coefficient;
        }
        for coefficient in row.iter_mut() {
            *coefficient /= sum;
        }
    }
    table
}

/// Fractional read of `buffer` at `position`, wrapped into the buffer.
fn sinc_read(
    buffer: &[f64],
    position: f64,
    taps: &[[f64; DELAY_INTERP_TAPS]; DELAY_INTERP_PHASES],
) -> f64 {
    let len = buffer.len();
    let wrapped = position.rem_euclid(len as f64);
    let base = wrapped.floor() as usize;
    let phase = ((wrapped - base as f64) * DELAY_INTERP_PHASES as f64).round() as usize;
    let row = &taps[phase.min(DELAY_INTERP_PHASES - 1)];
    let mut sum = 0.0;
    for (tap, coefficient) in row.iter().enumerate() {
        let index = (base + len + tap - 3) % len;
        sum += coefficient * buffer[index];
    }
    sum
}

struct DelayState {
    left: Box<[f64]>,
    right: Box<[f64]>,
    write: usize,
    dry_left: Box<[f64]>,
    dry_right: Box<[f64]>,
    dry_write: usize,
    latency: usize,
    sample_rate: f64,
    damping_left: ZdfDamping,
    damping_right: ZdfDamping,
    taps: Option<Box<[[f64; DELAY_INTERP_TAPS]; DELAY_INTERP_PHASES]>>,
    staging: PowerSplit,
}

impl DelayState {
    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
        self.dry_write = 0;
        self.damping_left = ZdfDamping::default();
        self.damping_right = ZdfDamping::default();
        self.staging.reset();
    }

    fn new() -> Self {
        Self {
            left: Vec::new().into_boxed_slice(),
            right: Vec::new().into_boxed_slice(),
            write: 0,
            dry_left: Vec::new().into_boxed_slice(),
            dry_right: Vec::new().into_boxed_slice(),
            dry_write: 0,
            latency: 0,
            sample_rate: 48_000.0,
            damping_left: ZdfDamping::default(),
            damping_right: ZdfDamping::default(),
            taps: None,
            staging: PowerSplit::new(48_000.0),
        }
    }

    /// Allocate the line and size the latency. Called from the load path; the
    /// audio path only updates the latency when the sample rate changes.
    fn prepare(&mut self, sr: f64) {
        self.latency = delay_latency_samples(sr);
        self.staging.prepare(sr);
        if self.left.is_empty() {
            self.taps = Some(Box::new(delay_interpolation_taps()));
            self.left = vec![0.0; DELAY_LINE_SAMPLES].into_boxed_slice();
            self.right = vec![0.0; DELAY_LINE_SAMPLES].into_boxed_slice();
            self.dry_left = vec![0.0; DELAY_LATENCY_MAX_SAMPLES].into_boxed_slice();
            self.dry_right = vec![0.0; DELAY_LATENCY_MAX_SAMPLES].into_boxed_slice();
        }
    }

    fn latency_samples(&self) -> usize {
        self.latency
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        if self.left.is_empty() {
            return (left, right);
        }
        if (self.sample_rate - sr).abs() > 0.5 {
            self.sample_rate = sr;
            self.latency = delay_latency_samples(sr);
        }
        let len = self.left.len();
        let delay = delay_time_samples(values[0], sr);
        let read = self.write as f64 - delay - self.latency as f64;
        let taps = self.taps.as_deref().expect("prepared delay table");
        let delayed_left = sinc_read(&self.left, read, taps);
        let delayed_right = sinc_read(&self.right, read, taps);

        let tone = values[2].clamp(0.0, 1.0);
        let cutoff = 250.0 * (18_000.0_f64 / 250.0).powf(tone);
        let coefficient = damping_coefficient(cutoff, sr);
        let damped_left = self.damping_left.process(delayed_left, coefficient);
        let damped_right = self.damping_right.process(delayed_right, coefficient);
        let feedback = values[1].clamp(0.0, 1.0);
        self.left[self.write] = left + damped_left * feedback;
        self.right[self.write] = right + damped_right * feedback;
        self.write = (self.write + 1) % len;

        let dry_len = self.dry_left.len();
        let dry_read = (self.dry_write + dry_len - self.latency) % dry_len;
        let dry_left = self.dry_left[dry_read];
        let dry_right = self.dry_right[dry_read];
        self.dry_left[self.dry_write] = left;
        self.dry_right[self.dry_write] = right;
        self.dry_write = (self.dry_write + 1) % dry_len;

        let mix = values[3].clamp(0.0, 1.0);
        if mix <= 0.0 {
            return (dry_left, dry_right);
        }
        let share = self
            .staging
            .share([dry_left, dry_right], [delayed_left, delayed_right], mix);
        let dry_gain = (1.0 - share).sqrt();
        let wet_gain = mix.sqrt();
        (
            dry_left * dry_gain + delayed_left * wet_gain,
            dry_right * dry_gain + delayed_right * wet_gain,
        )
    }
}

/// Four tanks of eight lines. The 32-line feedback matrix is
/// `(H8/√8) ⊗ (H4/2)`, a normalised Hadamard matrix over all lines.
const REVERB_TANKS: usize = 4;
const REVERB_LINES: usize = 8;
const REVERB_TOTAL_LINES: usize = REVERB_TANKS * REVERB_LINES;

/// Line lengths at 48 kHz, tank-major. Every value is prime, so no two loops
/// share a period.
const REVERB_LINE_SAMPLES_48K: [usize; REVERB_TOTAL_LINES] = [
    907, 1109, 1321, 1567, 1789, 2039, 2281, 2531, 967, 1171, 1423, 1613, 1871, 2089, 2341, 2593,
    1013, 1229, 1459, 1667, 1913, 2143, 2389, 2659, 1061, 1289, 1499, 1733, 1993, 2237, 2441, 2699,
];

/// Guard samples so a prime snapped near 96 kHz always fits its buffer.
const REVERB_LINE_GUARD: usize = 16;
/// Shelf corners for the frequency-dependent decay: low and high.
const REVERB_BAND_HZ: [f64; 2] = [250.0, 4_000.0];
/// Band RT60 ratios against the mid band; the controls trim them.
const REVERB_LOW_RATIO: f64 = 1.10;
const REVERB_HIGH_RATIO: f64 = 0.55;
/// Mid-band RT60 at size 0 and the octave span of the size control.
const REVERB_RT60_BASE: f64 = 0.585;
const REVERB_RT60_OCTAVES: f64 = 4.0;
/// Damping corner at full damping; zero is an exact bypass.
const REVERB_DAMP_HZ: f64 = 1_500.0;
const REVERB_PREDELAY_SECONDS: f64 = 0.2;
/// Wet tap gain; a steady noise at the default size reads -6 dB against dry.
const REVERB_WET_GAIN: f64 = 4.0;
/// Measured gain calibration: the tank decays 1.11x slower than the target
/// RT60 before this factor is applied.
const REVERB_DECAY_CALIBRATION: f64 = 0.90;
/// Safety cap: every band gain stays below one at every setting.
const REVERB_MAX_GAIN: f64 = 0.999;

/// Orthogonal ±1 input and output taps over the 32 lines.
const REVERB_IN_L: [f64; REVERB_TOTAL_LINES] = [
    1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0, -1.0,
    1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, 1.0, -1.0,
];
const REVERB_IN_R: [f64; REVERB_TOTAL_LINES] = [
    1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, 1.0, -1.0,
    -1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, -1.0, 1.0, 1.0,
];
const REVERB_OUT_L: [f64; REVERB_TOTAL_LINES] = [
    1.0, 1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0,
    1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0,
];
const REVERB_OUT_R: [f64; REVERB_TOTAL_LINES] = [
    -1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0,
    1.0, 1.0, 1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, 1.0,
];

fn is_prime(value: usize) -> bool {
    if value < 2 {
        return false;
    }
    if value.is_multiple_of(2) {
        return value == 2;
    }
    let mut divisor = 3;
    while divisor * divisor <= value {
        if value.is_multiple_of(divisor) {
            return false;
        }
        divisor += 2;
    }
    true
}

fn nearest_prime(value: usize) -> usize {
    if value <= 2 {
        return 2;
    }
    let mut offset = 0;
    loop {
        if offset <= value && is_prime(value - offset) {
            return value - offset;
        }
        if is_prime(value + offset) {
            return value + offset;
        }
        offset += 1;
    }
}

fn reverb_line_capacity(line: usize) -> usize {
    REVERB_LINE_SAMPLES_48K[line] * (MAX_FX_SAMPLE_RATE as usize / 48_000) + REVERB_LINE_GUARD
}

/// Mid-band RT60 in seconds; size 0.6 is the 3.1 s hall setting.
fn reverb_rt60_mid(size: f64) -> f64 {
    REVERB_RT60_BASE * 2f64.powf(REVERB_RT60_OCTAVES * size.clamp(0.0, 1.0))
}

/// Amplitude lost per line loop so a band reaches -60 dB in `rt60` seconds.
fn reverb_band_gain(rt60: f64, line_delay: f64) -> f64 {
    let decay = (rt60 * REVERB_DECAY_CALIBRATION).max(1e-3);
    (10f64.powf(-3.0 * line_delay / decay)).min(REVERB_MAX_GAIN)
}

fn reverb_tail_seconds(size: f64, predelay: f64) -> f64 {
    reverb_rt60_mid(size) * REVERB_LOW_RATIO + predelay
}

fn hadamard8(values: &mut [f64; REVERB_LINES]) {
    let mut stride = 1;
    while stride < REVERB_LINES {
        let mut base = 0;
        while base < REVERB_LINES {
            for offset in 0..stride {
                let a = values[base + offset];
                let b = values[base + offset + stride];
                values[base + offset] = a + b;
                values[base + offset + stride] = a - b;
            }
            base += stride * 2;
        }
        stride *= 2;
    }
    let scale = 1.0 / (REVERB_LINES as f64).sqrt();
    for value in values.iter_mut() {
        *value *= scale;
    }
}

fn hadamard4(values: &mut [f64; REVERB_TANKS]) {
    let a = values[0];
    let b = values[1];
    let c = values[2];
    let d = values[3];
    values[0] = (a + b + c + d) * 0.5;
    values[1] = (a - b + c - d) * 0.5;
    values[2] = (a + b - c - d) * 0.5;
    values[3] = (a - b - c + d) * 0.5;
}

/// Butterworth section used by the shelf pair that shapes the band decay.
#[derive(Clone, Copy, Default)]
struct ReverbLowpass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl ReverbLowpass {
    fn lowpass(freq: f64, sr: f64) -> Self {
        let freq = freq.min(sr * 0.45);
        let omega = TAU * freq / sr;
        let alpha = omega.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let cos = omega.cos();
        Self {
            b0: (1.0 - cos) * 0.5 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn highpass(freq: f64, sr: f64) -> Self {
        let freq = freq.min(sr * 0.45);
        let omega = TAU * freq / sr;
        let alpha = omega.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let cos = omega.cos();
        Self {
            b0: (1.0 + cos) * 0.5 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) * 0.5 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn process(&self, state: &mut [f64; 2], input: f64) -> f64 {
        let output = self.b0 * input + state[0];
        state[0] = self.b1 * input - self.a1 * output + state[1];
        state[1] = self.b2 * input - self.a2 * output;
        output
    }
}

struct ReverbState {
    lines: [Box<[f64]>; REVERB_TOTAL_LINES],
    positions: [usize; REVERB_TOTAL_LINES],
    damping: [f64; REVERB_TOTAL_LINES],
    lengths: [usize; REVERB_TOTAL_LINES],
    shelf_low: [[f64; 2]; REVERB_TOTAL_LINES],
    shelf_high: [[f64; 2]; REVERB_TOTAL_LINES],
    low_filter: ReverbLowpass,
    high_filter: ReverbLowpass,
    cached_size: f64,
    cached_low: f64,
    cached_high: f64,
    line_gain_low: [f64; REVERB_TOTAL_LINES],
    line_gain_mid: [f64; REVERB_TOTAL_LINES],
    line_gain_high: [f64; REVERB_TOTAL_LINES],
    line_shelf_low: [f64; REVERB_TOTAL_LINES],
    line_shelf_high: [f64; REVERB_TOTAL_LINES],
    predelay_left: Box<[f64]>,
    predelay_right: Box<[f64]>,
    predelay_write: usize,
    sample_rate: f64,
    staging: PowerSplit,
}

impl ReverbState {
    fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.positions.fill(0);
        self.damping.fill(0.0);
        self.shelf_low.fill([0.0; 2]);
        self.shelf_high.fill([0.0; 2]);
        self.predelay_left.fill(0.0);
        self.predelay_right.fill(0.0);
        self.predelay_write = 0;
        self.staging.reset();
    }

    fn new() -> Self {
        let mut state = Self {
            lines: std::array::from_fn(|line| {
                vec![0.0; reverb_line_capacity(line)].into_boxed_slice()
            }),
            positions: [0; REVERB_TOTAL_LINES],
            damping: [0.0; REVERB_TOTAL_LINES],
            lengths: REVERB_LINE_SAMPLES_48K,
            shelf_low: [[0.0; 2]; REVERB_TOTAL_LINES],
            shelf_high: [[0.0; 2]; REVERB_TOTAL_LINES],
            low_filter: ReverbLowpass::lowpass(REVERB_BAND_HZ[0], 48_000.0),
            high_filter: ReverbLowpass::highpass(REVERB_BAND_HZ[1], 48_000.0),
            cached_size: f64::NAN,
            cached_low: f64::NAN,
            cached_high: f64::NAN,
            line_gain_low: [0.0; REVERB_TOTAL_LINES],
            line_gain_mid: [0.0; REVERB_TOTAL_LINES],
            line_gain_high: [0.0; REVERB_TOTAL_LINES],
            line_shelf_low: [0.0; REVERB_TOTAL_LINES],
            line_shelf_high: [0.0; REVERB_TOTAL_LINES],
            predelay_left: vec![0.0; MAX_PREDELAY_SAMPLES].into_boxed_slice(),
            predelay_right: vec![0.0; MAX_PREDELAY_SAMPLES].into_boxed_slice(),
            predelay_write: 0,
            sample_rate: 48_000.0,
            staging: PowerSplit::new(48_000.0),
        };
        state.prepare(48_000.0);
        state
    }

    /// Snap every line to a prime length for the current rate and clear it if
    /// the geometry changed. Called from the load path only.
    fn prepare(&mut self, sr: f64) {
        let sr = sr.max(8_000.0);
        let scale = sr / 48_000.0;
        for (line, &samples) in REVERB_LINE_SAMPLES_48K.iter().enumerate() {
            let target = (samples as f64 * scale).round() as usize;
            let length = nearest_prime(target.max(16)).min(self.lines[line].len());
            if self.lengths[line] != length {
                self.lengths[line] = length;
                self.lines[line].fill(0.0);
                self.positions[line] = 0;
            }
        }
        if (self.sample_rate - sr).abs() > 0.5 {
            self.shelf_low = [[0.0; 2]; REVERB_TOTAL_LINES];
            self.shelf_high = [[0.0; 2]; REVERB_TOTAL_LINES];
            self.damping = [0.0; REVERB_TOTAL_LINES];
        }
        self.low_filter = ReverbLowpass::lowpass(REVERB_BAND_HZ[0], sr);
        self.high_filter = ReverbLowpass::highpass(REVERB_BAND_HZ[1], sr);
        self.cached_size = f64::NAN;
        self.sample_rate = sr;
        self.staging.prepare(sr);
    }

    #[cfg(test)]
    fn allocated_bytes(&self) -> usize {
        let samples: usize = self.lines.iter().map(|line| line.len()).sum::<usize>()
            + self.predelay_left.len()
            + self.predelay_right.len();
        samples * std::mem::size_of::<f64>()
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let size = values[0].clamp(0.0, 1.0);
        let damp = values[1].clamp(0.0, 1.0);
        let predelay = values[2].clamp(0.0, REVERB_PREDELAY_SECONDS);
        let width = values[3].clamp(0.0, 1.0);
        let mix = values[4].clamp(0.0, 1.0);
        let low_ratio = REVERB_LOW_RATIO - 0.40 + 0.80 * values[5].clamp(0.0, 1.0);
        let high_ratio = REVERB_HIGH_RATIO - 0.35 + 0.70 * values[6].clamp(0.0, 1.0);

        if self.cached_size != size
            || self.cached_low != low_ratio
            || self.cached_high != high_ratio
        {
            let mid_rt60 = reverb_rt60_mid(size);
            for line in 0..REVERB_TOTAL_LINES {
                let delay = self.lengths[line] as f64 / sr;
                self.line_gain_mid[line] = reverb_band_gain(mid_rt60, delay);
                self.line_gain_low[line] = reverb_band_gain(mid_rt60 * low_ratio, delay);
                self.line_gain_high[line] = reverb_band_gain(mid_rt60 * high_ratio, delay);
                self.line_shelf_low[line] = self.line_gain_low[line] / self.line_gain_mid[line];
                self.line_shelf_high[line] = self.line_gain_high[line] / self.line_gain_mid[line];
            }
            self.cached_size = size;
            self.cached_low = low_ratio;
            self.cached_high = high_ratio;
        }
        let damp_pole = damp * (-TAU * REVERB_DAMP_HZ / sr).exp();
        let inject = 1.0 / (REVERB_TOTAL_LINES as f64).sqrt();

        let prelen = self.predelay_left.len();
        let predelay_samples = ((predelay * sr).round() as usize).min(prelen - 2);
        self.predelay_left[self.predelay_write] = left;
        self.predelay_right[self.predelay_write] = right;
        let read = (self.predelay_write + prelen - predelay_samples) % prelen;
        let input_left = self.predelay_left[read];
        let input_right = self.predelay_right[read];
        self.predelay_write = (self.predelay_write + 1) % prelen;

        let mut taps = [[0.0f64; REVERB_LINES]; REVERB_TANKS];
        for line in 0..REVERB_TOTAL_LINES {
            taps[line / REVERB_LINES][line % REVERB_LINES] = self.lines[line][self.positions[line]];
        }

        let mut wet_left = 0.0;
        let mut wet_right = 0.0;
        for line in 0..REVERB_TOTAL_LINES {
            let value = taps[line / REVERB_LINES][line % REVERB_LINES];
            wet_left += value * REVERB_OUT_L[line];
            wet_right += value * REVERB_OUT_R[line];
        }
        let wet_scale = REVERB_WET_GAIN * inject;
        wet_left *= wet_scale;
        wet_right *= wet_scale;

        for tank in taps.iter_mut() {
            hadamard8(tank);
        }
        let [row0, row1, row2, row3] = &mut taps;
        for (((r0, r1), r2), r3) in row0
            .iter_mut()
            .zip(row1.iter_mut())
            .zip(row2.iter_mut())
            .zip(row3.iter_mut())
        {
            let mut column = [*r0, *r1, *r2, *r3];
            hadamard4(&mut column);
            *r0 = column[0];
            *r1 = column[1];
            *r2 = column[2];
            *r3 = column[3];
        }

        for (tank, tap_row) in taps.iter_mut().enumerate() {
            for (offset, &value) in tap_row.iter().enumerate() {
                let line = tank * REVERB_LINES + offset;
                let mid_gain = self.line_gain_mid[line];
                let low_shelf = self.line_shelf_low[line];
                let high_shelf = self.line_shelf_high[line];
                let low = self.low_filter.process(&mut self.shelf_low[line], value);
                let shaped = value + (low_shelf - 1.0) * low;
                let high = self.high_filter.process(&mut self.shelf_high[line], shaped);
                let shaped = shaped + (high_shelf - 1.0) * high;
                let decayed = mid_gain * shaped;
                let damped = decayed * (1.0 - damp_pole) + self.damping[line] * damp_pole;
                self.damping[line] = damped;
                let position = self.positions[line];
                let injected = input_left * REVERB_IN_L[line] + input_right * REVERB_IN_R[line];
                self.lines[line][position] = damped + injected * inject;
                self.positions[line] = if position + 1 == self.lengths[line] {
                    0
                } else {
                    position + 1
                };
            }
        }

        let mid = (wet_left + wet_right) * 0.5;
        let side = (wet_left - wet_right) * 0.5 * width;
        let share = self.staging.share([left, right], [mid, mid], mix);
        let dry_gain = (1.0 - share).sqrt();
        let wet_gain = mix.sqrt();
        (
            left * dry_gain + (mid + side) * wet_gain,
            right * dry_gain + (mid - side) * wet_gain,
        )
    }
}

/// Conv: uniform partitioned FFT convolution, hop 128, FFT 256.
/// Maximum IR is 0.4 s at 96 kHz (38 400 taps); the state is sized once.
const CONV_BLOCK: usize = 128;
const CONV_BINS: usize = CONV_BLOCK + 1;
const MAX_IR_SAMPLES: usize = 38_400;
const MAX_CONV_PARTITIONS: usize = MAX_IR_SAMPLES / CONV_BLOCK;
const CONV_LATENCY: usize = CONV_BLOCK;
const CONV_IR_PRESETS: usize = 4;

/// One synthetic room/plate. `rt60` is the -60 dB decay time; the IR is stored
/// up to `seconds = 2 * rt60`, so the truncated tail is below -120 dB.
struct ConvIrPreset {
    seconds: f64,
    rt60: f64,
    diffusion: f64,
    damping_hz: f64,
    early: usize,
    seed: u64,
}

const CONV_PRESETS: [ConvIrPreset; CONV_IR_PRESETS] = [
    ConvIrPreset {
        seconds: 0.24,
        rt60: 0.12,
        diffusion: 0.60,
        damping_hz: 7_000.0,
        early: 12,
        seed: 0x6a09_e667_f3bc_c908,
    },
    ConvIrPreset {
        seconds: 0.32,
        rt60: 0.16,
        diffusion: 0.64,
        damping_hz: 5_500.0,
        early: 14,
        seed: 0xbb67_ae85_84ca_a73b,
    },
    ConvIrPreset {
        seconds: 0.40,
        rt60: 0.20,
        diffusion: 0.66,
        damping_hz: 4_500.0,
        early: 16,
        seed: 0x3c6e_f372_fe94_f82b,
    },
    ConvIrPreset {
        seconds: 0.36,
        rt60: 0.18,
        diffusion: 0.72,
        damping_hz: 9_000.0,
        early: 10,
        seed: 0xa54f_f53a_5f1d_36f1,
    },
];

fn conv_preset_for_size(size: f64) -> usize {
    ((size.clamp(0.0, 1.0) * CONV_IR_PRESETS as f64).floor() as usize).min(CONV_IR_PRESETS - 1)
}

/// SplitMix64: fixed seeds make every generated IR reproducible.
struct IrRng(u64);

impl IrRng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn bipolar(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }
}

/// One channel of a partitioned convolver: frequency-domain delay line, IR
/// spectra, overlap-add block and the one-block output queue.
struct ConvolutionChannel {
    history: Box<[Complex]>,
    spectra: Box<[Complex]>,
    taps: Box<[f64]>,
    partitions: usize,
    ir_samples: usize,
    write_partition: usize,
    input_block: Box<[f64]>,
    fill: usize,
    output_block: Box<[f64]>,
    output_pos: usize,
    overlap: Box<[f64]>,
    spectrum: Box<[Complex]>,
    accum: Box<[Complex]>,
}

impl ConvolutionChannel {
    fn new() -> Self {
        Self {
            history: vec![Complex::default(); MAX_CONV_PARTITIONS * CONV_BINS].into_boxed_slice(),
            spectra: vec![Complex::default(); MAX_CONV_PARTITIONS * CONV_BINS].into_boxed_slice(),
            taps: vec![0.0; MAX_IR_SAMPLES].into_boxed_slice(),
            partitions: 1,
            ir_samples: CONV_BLOCK,
            write_partition: 0,
            input_block: vec![0.0; CONV_BLOCK].into_boxed_slice(),
            fill: 0,
            output_block: vec![0.0; CONV_BLOCK].into_boxed_slice(),
            output_pos: CONV_BLOCK,
            overlap: vec![0.0; CONV_BLOCK].into_boxed_slice(),
            spectrum: vec![Complex::default(); 2 * CONV_BLOCK].into_boxed_slice(),
            accum: vec![Complex::default(); 2 * CONV_BLOCK].into_boxed_slice(),
        }
    }

    /// Install the current time-domain IR and precompute its partition spectra.
    fn load(&mut self, fft: &Fft) {
        self.partitions = self.ir_samples.div_ceil(CONV_BLOCK).max(1);
        for partition in 0..self.partitions {
            let start = partition * CONV_BLOCK;
            for index in 0..2 * CONV_BLOCK {
                self.spectrum[index] = if index < CONV_BLOCK && start + index < self.ir_samples {
                    Complex::new(self.taps[start + index], 0.0)
                } else {
                    Complex::default()
                };
            }
            fft.forward(&mut self.spectrum);
            let base = partition * CONV_BINS;
            self.spectra[base..base + CONV_BINS].copy_from_slice(&self.spectrum[..CONV_BINS]);
        }
        self.history.fill(Complex::default());
        self.overlap.fill(0.0);
        self.output_block.fill(0.0);
        self.input_block.fill(0.0);
        self.fill = 0;
        self.output_pos = CONV_BLOCK;
        self.write_partition = 0;
    }

    fn reset(&mut self) {
        self.history.fill(Complex::default());
        self.spectrum.fill(Complex::default());
        self.accum.fill(Complex::default());
        self.input_block.fill(0.0);
        self.output_block.fill(0.0);
        self.overlap.fill(0.0);
        self.fill = 0;
        self.output_pos = CONV_BLOCK;
        self.write_partition = 0;
    }

    fn push(&mut self, fft: &Fft, input: f64) -> f64 {
        let output = if self.output_pos < CONV_BLOCK {
            self.output_block[self.output_pos]
        } else {
            0.0
        };
        if self.output_pos < CONV_BLOCK {
            self.output_pos += 1;
        }
        self.input_block[self.fill] = input;
        self.fill += 1;
        if self.fill == CONV_BLOCK {
            self.fill = 0;
            self.compute(fft);
        }
        output
    }

    fn compute(&mut self, fft: &Fft) {
        for (index, value) in self.spectrum.iter_mut().enumerate() {
            *value = if index < CONV_BLOCK {
                Complex::new(self.input_block[index], 0.0)
            } else {
                Complex::default()
            };
        }
        fft.forward(&mut self.spectrum);
        let base = self.write_partition * CONV_BINS;
        self.history[base..base + CONV_BINS].copy_from_slice(&self.spectrum[..CONV_BINS]);
        self.accum.fill(Complex::default());
        for partition in 0..self.partitions {
            let ir_base = partition * CONV_BINS;
            let input_base = ((self.write_partition + self.partitions - partition)
                % self.partitions)
                * CONV_BINS;
            for bin in 0..CONV_BINS {
                self.accum[bin] = self.accum[bin]
                    .plus(self.spectra[ir_base + bin].times(self.history[input_base + bin]));
            }
        }
        self.accum[0].im = 0.0;
        self.accum[CONV_BLOCK].im = 0.0;
        for bin in 1..CONV_BLOCK {
            self.accum[2 * CONV_BLOCK - bin] = self.accum[bin].conjugate();
        }
        fft.inverse(&mut self.accum);
        for index in 0..CONV_BLOCK {
            self.output_block[index] = self.accum[index].re + self.overlap[index];
            self.overlap[index] = self.accum[CONV_BLOCK + index].re;
        }
        self.write_partition = (self.write_partition + 1) % self.partitions;
        self.output_pos = 0;
    }
}

/// Conv: true-stereo partitioned convolution of two generated IR channels.
struct ConvState {
    fft: Fft,
    left: ConvolutionChannel,
    right: ConvolutionChannel,
    predelay_left: Box<[f64]>,
    predelay_right: Box<[f64]>,
    predelay_write: usize,
    dry_left: Box<[f64]>,
    dry_right: Box<[f64]>,
    dry_write: usize,
    damp_left: f64,
    damp_right: f64,
    ir_rate: f64,
    ir_preset: usize,
    diffusion_lines: Box<[f64]>,
}

impl ConvState {
    /// Allocate the buffers. The IR bank is generated later, when the chain
    /// knows which slots carry a Conv module, so a rack without one pays
    /// nothing for it.
    fn new() -> Self {
        Self {
            fft: Fft::new(2 * CONV_BLOCK),
            left: ConvolutionChannel::new(),
            right: ConvolutionChannel::new(),
            predelay_left: vec![0.0; MAX_PREDELAY_SAMPLES].into_boxed_slice(),
            predelay_right: vec![0.0; MAX_PREDELAY_SAMPLES].into_boxed_slice(),
            predelay_write: 0,
            dry_left: vec![0.0; CONV_LATENCY].into_boxed_slice(),
            dry_right: vec![0.0; CONV_LATENCY].into_boxed_slice(),
            dry_write: 0,
            damp_left: 0.0,
            damp_right: 0.0,
            ir_rate: 0.0,
            ir_preset: usize::MAX,
            diffusion_lines: vec![0.0; 3 * 320].into_boxed_slice(),
        }
    }

    fn latency_samples(&self) -> usize {
        CONV_LATENCY
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.predelay_left.fill(0.0);
        self.predelay_right.fill(0.0);
        self.predelay_write = 0;
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
        self.dry_write = 0;
        self.damp_left = 0.0;
        self.damp_right = 0.0;
        self.diffusion_lines.fill(0.0);
    }

    /// Regenerate the IR bank at a new rate or size. Bounded, allocation-free,
    /// and only runs when the cached rate/preset changes.
    fn prepare(&mut self, sr: f64, preset: usize) {
        if (self.ir_rate - sr).abs() < 0.5 && self.ir_preset == preset {
            return;
        }
        self.ir_rate = sr;
        self.ir_preset = preset;
        let seconds = CONV_PRESETS[preset].seconds;
        let samples = ((seconds * sr).round() as usize).clamp(CONV_BLOCK, MAX_IR_SAMPLES);
        self.left.ir_samples = samples;
        self.right.ir_samples = samples;
        generate_ir(
            preset,
            sr,
            0,
            &mut self.left.taps,
            &mut self.diffusion_lines,
        );
        generate_ir(
            preset,
            sr,
            1,
            &mut self.right.taps,
            &mut self.diffusion_lines,
        );
        self.left.load(&self.fft);
        self.right.load(&self.fft);
        self.reset();
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        self.prepare(sr, conv_preset_for_size(values[0]));
        let predelay = values[2].clamp(0.0, 0.2);
        let delay = ((predelay * sr).round() as usize).min(MAX_PREDELAY_SAMPLES - 2);
        let len = self.predelay_left.len();
        self.predelay_left[self.predelay_write] = left;
        self.predelay_right[self.predelay_write] = right;
        let read = (self.predelay_write + len - delay) % len;
        let pre_left = self.predelay_left[read];
        let pre_right = self.predelay_right[read];
        self.predelay_write = (self.predelay_write + 1) % len;

        let wet_left = self.left.push(&self.fft, pre_left);
        let wet_right = self.right.push(&self.fft, pre_right);
        let damping = values[1].clamp(0.0, 1.0);
        if damping > 0.0 {
            let cutoff = (20_000.0 * (2_000.0f64 / 20_000.0).powf(damping)).min(sr * 0.45);
            let coefficient = 1.0 - (-TAU * cutoff / sr).exp();
            self.damp_left += coefficient * (wet_left - self.damp_left);
            self.damp_right += coefficient * (wet_right - self.damp_right);
        } else {
            self.damp_left = wet_left;
            self.damp_right = wet_right;
        }
        let width = values[3].clamp(0.0, 2.0);
        let mid = (self.damp_left + self.damp_right) * 0.5;
        let side = (self.damp_left - self.damp_right) * 0.5 * width;

        // Both paths carry the same one-block delay, so the crossfade aligns.
        let dry_left = self.dry_left[self.dry_write];
        let dry_right = self.dry_right[self.dry_write];
        self.dry_left[self.dry_write] = left;
        self.dry_right[self.dry_write] = right;
        self.dry_write = (self.dry_write + 1) % CONV_LATENCY;

        mix_stereo(dry_left, dry_right, mid + side, mid - side, values[4])
    }

    #[cfg(test)]
    fn taps(&self, channel: usize) -> &[f64] {
        let state = if channel == 0 {
            &self.left
        } else {
            &self.right
        };
        &state.taps[..state.ir_samples]
    }
}

/// Generate one deterministic IR channel. The law is documented in
/// `docs/fx-hyperd-conv.md`; all buffers are caller-owned.
fn generate_ir(preset: usize, sr: f64, channel: usize, taps: &mut [f64], lines: &mut [f64]) {
    let spec = &CONV_PRESETS[preset];
    let samples = ((spec.seconds * sr).round() as usize).clamp(CONV_BLOCK, MAX_IR_SAMPLES);
    taps.fill(0.0);
    lines.fill(0.0);
    let tau = spec.rt60 / 6.907_755_278_982_137;
    let mut rng = IrRng(spec.seed ^ (channel as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
    for _ in 0..spec.early {
        let position = ((0.002 + rng.unit().powf(1.6) * spec.seconds * 0.35) * sr) as usize;
        let position = position.min(samples - 1);
        let decay = (-(position as f64) / (sr * tau)).exp();
        let sign = if rng.unit() < 0.5 { -1.0 } else { 1.0 };
        taps[position] += (0.35 + 0.5 * rng.unit()) * decay * sign;
    }
    let start = (0.002 * sr) as usize;
    let mut lowpass = 0.0;
    for (offset, tap) in taps[start..samples].iter_mut().enumerate() {
        let index = start + offset;
        let progress = offset as f64 / (samples - start).max(1) as f64;
        let cutoff = spec.damping_hz * 0.12f64.powf(progress);
        let coefficient = 1.0 - (-TAU * cutoff / sr).exp();
        lowpass += coefficient * (rng.bipolar() - lowpass);
        let decay = (-(index as f64) / (sr * tau)).exp();
        *tap += lowpass * decay * 0.6;
    }
    // Three Schroeder allpass stages turn the sparse onset into a diffuse field.
    for (stage, delay_ms) in [1.7, 2.3, 3.1].into_iter().enumerate() {
        let delay = ((delay_ms * 0.001 * sr) as usize).clamp(1, 320);
        let line = &mut lines[stage * 320..stage * 320 + delay];
        let mut write = 0;
        for sample in taps[..samples].iter_mut() {
            let delayed = line[write];
            let stored = *sample + spec.diffusion * delayed;
            line[write] = stored;
            *sample = -spec.diffusion * stored + delayed;
            write = (write + 1) % delay;
        }
    }
    taps[0] += 0.25;
    let fade = (0.01 * sr) as usize;
    if fade > 1 && fade < samples {
        for (index, sample) in taps[samples - fade..samples].iter_mut().enumerate() {
            let position = index as f64 / fade as f64;
            *sample *= 0.5 + 0.5 * (PI * position).cos();
        }
    }
    let energy = taps[..samples]
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>()
        .sqrt();
    if energy > 0.0 {
        let scale = 1.0 / energy;
        for sample in &mut taps[..samples] {
            *sample *= scale;
        }
    }
}

struct ChorusState {
    left: Box<[f64]>,
    right: Box<[f64]>,
    write: usize,
    phase: f64,
}

impl ChorusState {
    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
        self.phase = 0.0;
    }

    fn new() -> Self {
        Self {
            left: vec![0.0; MAX_CHORUS_SAMPLES].into_boxed_slice(),
            right: vec![0.0; MAX_CHORUS_SAMPLES].into_boxed_slice(),
            write: 0,
            phase: 0.0,
        }
    }

    fn read(buffer: &[f64], write: usize, delay: f64) -> f64 {
        let len = buffer.len();
        let mut position = write as f64 - delay;
        while position < 0.0 {
            position += len as f64;
        }
        let position = position % len as f64;
        let index = position.floor() as usize;
        let fraction = position - index as f64;
        let next = (index + 1) % len;
        buffer[index] + (buffer[next] - buffer[index]) * fraction
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let rate = values[0].clamp(0.01, 10.0);
        let depth = values[1].clamp(0.0, 1.0);
        let feedback = values[2].clamp(0.0, 0.9);
        let mix = values[3].clamp(0.0, 1.0);
        let base = values[5].clamp(0.5, 40.0) * 0.001;
        let base2 = values[6].clamp(0.5, 40.0) * 0.001;
        self.phase = (self.phase + rate / sr).fract();
        let swing = (0.001 + depth * 0.012) * sr;
        let lfo = (TAU * self.phase).sin();
        let lfo_right = (TAU * (self.phase + 0.25)).sin();
        let left_a =
            (base * sr + swing * (0.5 + 0.5 * lfo)).clamp(1.0, (self.left.len() - 2) as f64);
        let left_b =
            (base2 * sr + swing * (0.5 - 0.5 * lfo)).clamp(1.0, (self.left.len() - 2) as f64);
        let right_a =
            (base * sr + swing * (0.5 + 0.5 * lfo_right)).clamp(1.0, (self.right.len() - 2) as f64);
        let right_b = (base2 * sr + swing * (0.5 - 0.5 * lfo_right))
            .clamp(1.0, (self.right.len() - 2) as f64);
        let delayed_left = (Self::read(&self.left, self.write, left_a)
            + Self::read(&self.left, self.write, left_b))
            * std::f64::consts::FRAC_1_SQRT_2;
        let delayed_right = (Self::read(&self.right, self.write, right_a)
            + Self::read(&self.right, self.write, right_b))
            * std::f64::consts::FRAC_1_SQRT_2;
        self.left[self.write] = left + delayed_left * feedback;
        self.right[self.write] = right + delayed_right * feedback;
        self.write = (self.write + 1) % self.left.len();
        // Measured chorus staging: the dry keeps its body until the wet is
        // substantial, with a +3 dB makeup that vanishes at full wet.
        if mix <= 0.0 {
            return (left, right);
        }
        let dry_gain = std::f64::consts::SQRT_2 * (1.0 - mix * mix);
        (
            left * dry_gain + delayed_left * mix,
            right * dry_gain + delayed_right * mix,
        )
    }
}

/// HyperD: two decorrelated four-stage modulated allpass diffusers. The mono
/// sum stays the mid signal, so the side path vanishes on a mono sum.
const HYPERD_STAGES: usize = 4;
/// 10.7 ms at 96 kHz; the longest tap is 3.5 ms * 2.0 spread + modulation.
const HYPERD_BUFFER: usize = 1_024;
const HYPERD_BASE_MS: [[f64; HYPERD_STAGES]; 2] = [[0.9, 1.5, 2.2, 3.1], [1.1, 1.8, 2.6, 3.5]];
const HYPERD_MOD_SCALE: [f64; HYPERD_STAGES] = [1.0, 0.8, 0.6, 0.45];
const HYPERD_LFO_MULT: [[f64; HYPERD_STAGES]; 2] =
    [[1.0, 1.31, 1.73, 2.17], [1.17, 1.53, 1.91, 2.39]];
const HYPERD_LFO_OFFSET: [[f64; HYPERD_STAGES]; 2] =
    [[0.0, 0.17, 0.41, 0.66], [0.5, 0.71, 0.93, 0.19]];
/// Side gain after the diffusers: measured side/mid lands near -2 dB.
const HYPERD_SIDE_GAIN: f64 = 1.8;
/// The side path is kept out of the sub band so the image stays grounded.
const HYPERD_SIDE_HPF_HZ: f64 = 150.0;

struct HyperDStage {
    buffer: Box<[f64]>,
    write: usize,
}

impl HyperDStage {
    fn new() -> Self {
        Self {
            buffer: vec![0.0; HYPERD_BUFFER].into_boxed_slice(),
            write: 0,
        }
    }

    fn read(&self, delay: f64) -> f64 {
        let len = self.buffer.len();
        let mut position = self.write as f64 - delay;
        while position < 0.0 {
            position += len as f64;
        }
        let position = position % len as f64;
        let index = position.floor() as usize;
        let fraction = position - index as f64;
        let next = (index + 1) % len;
        self.buffer[index] + (self.buffer[next] - self.buffer[index]) * fraction
    }

    fn process(&mut self, input: f64, delay: f64, coefficient: f64) -> f64 {
        let delayed = self.read(delay);
        let stored = input + coefficient * delayed;
        self.buffer[self.write] = stored;
        self.write = (self.write + 1) % self.buffer.len();
        -coefficient * stored + delayed
    }
}

struct HyperDState {
    chains: [[HyperDStage; HYPERD_STAGES]; 2],
    phase: f64,
    side_lowpass: f64,
}

impl HyperDState {
    fn new() -> Self {
        Self {
            chains: std::array::from_fn(|_| std::array::from_fn(|_| HyperDStage::new())),
            phase: 0.0,
            side_lowpass: 0.0,
        }
    }

    fn reset(&mut self) {
        for chain in &mut self.chains {
            for stage in chain {
                stage.buffer.fill(0.0);
                stage.write = 0;
            }
        }
        self.phase = 0.0;
        self.side_lowpass = 0.0;
    }

    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let rate = values[0].clamp(0.01, 5.0);
        let depth = values[1].clamp(0.0, 1.0);
        let coefficient = 0.35 + values[2].clamp(0.0, 0.95) * 0.4;
        let spread = values[3].clamp(0.25, 2.0);
        let mix = values[4].clamp(0.0, 1.0);
        self.phase = (self.phase + rate / sr).fract();
        let modulation = 0.000_8 * depth;
        let max_delay = (HYPERD_BUFFER - 2) as f64;
        let mono = (left + right) * 0.5;
        let mut outputs = [0.0; 2];
        for (channel, output) in outputs.iter_mut().enumerate() {
            let mut signal = mono;
            for stage in 0..HYPERD_STAGES {
                let base = HYPERD_BASE_MS[channel][stage] * 0.001 * spread;
                let lfo = (TAU
                    * (self.phase * HYPERD_LFO_MULT[channel][stage]
                        + HYPERD_LFO_OFFSET[channel][stage]))
                    .sin();
                let delay = ((base + modulation * HYPERD_MOD_SCALE[stage] * lfo) * sr)
                    .clamp(1.0, max_delay);
                signal = self.chains[channel][stage].process(signal, delay, coefficient);
            }
            *output = signal;
        }
        let mut side = (outputs[0] - outputs[1]) * 0.5 * HYPERD_SIDE_GAIN;
        // Mono guard: remove the sub band from the side image only.
        let cutoff = HYPERD_SIDE_HPF_HZ.min(sr * 0.45);
        let lowpass = 1.0 - (-TAU * cutoff / sr).exp();
        self.side_lowpass += lowpass * (side - self.side_lowpass);
        side -= self.side_lowpass;
        let mid = (left + right) * 0.5;
        mix_stereo(left, right, mid + side, mid - side, mix)
    }
}

/// Value slots for the Utils palette.
const UTILS_HPF: usize = 0;
const UTILS_LPF: usize = 1;
const UTILS_LF_MONO: usize = 2;
const UTILS_LF_XOVER: usize = 3;
const UTILS_WIDTH: usize = 4;
const UTILS_BALANCE: usize = 5;
const UTILS_POLARITY_L: usize = 6;
const UTILS_POLARITY_R: usize = 7;
const UTILS_MIX: usize = 8;

const SPLIT_FREQ: usize = 0;
const SPLIT_COUNT1: usize = 1;
const SPLIT_COUNT2: usize = 2;
const SPLIT_BYPASS1: usize = 3;
const SPLIT_BYPASS2: usize = 4;
const SPLIT_GAIN1: usize = 6;
const SPLIT_GAIN2: usize = 7;

const SPLIT3_FREQ: usize = 0;
const SPLIT3_FREQ2: usize = 1;
const SPLIT3_COUNT1: usize = 2;
const SPLIT3_COUNT2: usize = 3;
const SPLIT3_COUNT3: usize = 4;
const SPLIT3_BYPASS1: usize = 5;
const SPLIT3_BYPASS2: usize = 6;
const SPLIT3_BYPASS3: usize = 7;
const SPLIT3_GAIN1: usize = 9;
const SPLIT3_GAIN2: usize = 10;
const SPLIT3_GAIN3: usize = 11;

const SPLIT_MS_COUNT1: usize = 0;
const SPLIT_MS_COUNT2: usize = 1;
const SPLIT_MS_BYPASS1: usize = 2;
const SPLIT_MS_BYPASS2: usize = 3;
const SPLIT_MS_GAIN1: usize = 5;
const SPLIT_MS_GAIN2: usize = 6;

const PARALLEL_COUNT1: usize = 0;
const PARALLEL_COUNT2: usize = 1;
const PARALLEL_COUNT3: usize = 2;
const PARALLEL_GAINS: [usize; 3] = [3, 4, 5];
const PARALLEL_BRANCHES: usize = 3;
const MAX_FX_BRANCHES: usize = 3;

const BODE_SHIFT: usize = 0;
const BODE_RANGE: usize = 1;
const BODE_FEEDBACK: usize = 2;
const BODE_DELAY: usize = 3;
const BODE_OUTPUT_MIX: usize = 4;
const BODE_OUTPUT_WIDTH: usize = 5;
const BODE_MONO_INPUT: usize = 6;
const BODE_MIX: usize = 7;
const BODE_SWAP_AB: usize = 9;

/// One Butterworth biquad per stage; two cascaded stages are a 4th-order
/// Linkwitz-Riley section (-6 dB at the corner, allpass sum).
const LR4_STAGE_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

fn lr4_low_pass(freq: f64, sr: f64) -> [BiquadCoefficients; 2] {
    [
        low_pass(freq, LR4_STAGE_Q, sr),
        low_pass(freq, LR4_STAGE_Q, sr),
    ]
}

fn lr4_high_pass(freq: f64, sr: f64) -> [BiquadCoefficients; 2] {
    [
        high_pass(freq, LR4_STAGE_Q, sr),
        high_pass(freq, LR4_STAGE_Q, sr),
    ]
}

fn run_biquads(
    stages: &mut [BiquadState; 2],
    coefficients: &[BiquadCoefficients; 2],
    input: f64,
) -> f64 {
    let first = stages[0].process(&coefficients[0], input);
    stages[1].process(&coefficients[1], first)
}

/// Utils: HPF (LR4), LPF (2-pole), LF mono, stereo width, balance, polarity.
#[derive(Debug, Clone, Copy, Default)]
struct UtilsState {
    hpf: [[BiquadState; 2]; 2],
    lpf: [BiquadState; 2],
    lf_low: [[BiquadState; 2]; 2],
}

impl UtilsState {
    fn process(
        &mut self,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let hpf = values[UTILS_HPF].clamp(1.0, 400.0);
        let lpf = values[UTILS_LPF].clamp(50.0, 20_000.0).min(sr * 0.45);
        let high_pass = lr4_high_pass(hpf, sr);
        let low_pass = low_pass(lpf, LR4_STAGE_Q, sr);
        let mut filtered = [0.0; 2];
        for (channel, input) in [left, right].into_iter().enumerate() {
            let mut signal = run_biquads(&mut self.hpf[channel], &high_pass, input);
            signal = self.lpf[channel].process(&low_pass, signal);
            filtered[channel] = signal;
        }
        let mut output = filtered;
        if values[UTILS_LF_MONO] >= 0.5 {
            let xover = values[UTILS_LF_XOVER].clamp(20.0, 400.0).min(sr * 0.45);
            let low_pass = lr4_low_pass(xover, sr);
            let mut lows = [0.0; 2];
            for channel in 0..2 {
                lows[channel] =
                    run_biquads(&mut self.lf_low[channel], &low_pass, filtered[channel]);
            }
            let mono_low = (lows[0] + lows[1]) * 0.5;
            output[0] = (filtered[0] - lows[0]) + mono_low;
            output[1] = (filtered[1] - lows[1]) + mono_low;
        }
        let width = (values[UTILS_WIDTH] / 100.0).clamp(0.0, 8.0);
        let mid = (output[0] + output[1]) * 0.5;
        let side = (output[0] - output[1]) * 0.5 * width;
        output[0] = mid + side;
        output[1] = mid - side;
        let balance = (values[UTILS_BALANCE] / 100.0).clamp(-1.0, 1.0);
        if balance < 0.0 {
            output[1] *= 1.0 + balance;
        } else {
            output[0] *= 1.0 - balance;
        }
        if values[UTILS_POLARITY_L] >= 0.5 {
            output[0] = -output[0];
        }
        if values[UTILS_POLARITY_R] >= 0.5 {
            output[1] = -output[1];
        }
        mix_stereo(left, right, output[0], output[1], values[UTILS_MIX])
    }
}

/// Crossover filters for one splitter. Bands are adjacent LR4 sections, so an
/// empty or bypassed splitter sums back to a magnitude-flat allpass.
#[derive(Debug, Clone, Copy, Default)]
struct CrossoverState {
    low: [[BiquadState; 2]; 2],
    high: [[BiquadState; 2]; 2],
    mid_hp: [[BiquadState; 2]; 2],
    mid_lp: [[BiquadState; 2]; 2],
}

impl CrossoverState {
    fn split_2(&mut self, left: f64, right: f64, freq: f64, sr: f64) -> ([f64; 2], [f64; 2]) {
        let low = lr4_low_pass(freq, sr);
        let high = lr4_high_pass(freq, sr);
        let mut lows = [0.0; 2];
        let mut highs = [0.0; 2];
        for (channel, input) in [left, right].into_iter().enumerate() {
            lows[channel] = run_biquads(&mut self.low[channel], &low, input);
            highs[channel] = run_biquads(&mut self.high[channel], &high, input);
        }
        (lows, highs)
    }

    fn split_3(
        &mut self,
        left: f64,
        right: f64,
        freq1: f64,
        freq2: f64,
        sr: f64,
    ) -> ([f64; 2], [f64; 2], [f64; 2]) {
        let high1 = lr4_high_pass(freq1, sr);
        let low1 = lr4_low_pass(freq1, sr);
        let high2 = lr4_high_pass(freq2, sr);
        let low2 = lr4_low_pass(freq2, sr);
        let mut lows = [0.0; 2];
        let mut mids = [0.0; 2];
        let mut highs = [0.0; 2];
        for (channel, input) in [left, right].into_iter().enumerate() {
            lows[channel] = run_biquads(&mut self.low[channel], &low1, input);
            let mut mid = run_biquads(&mut self.mid_hp[channel], &high1, input);
            mid = run_biquads(&mut self.mid_lp[channel], &low2, mid);
            mids[channel] = mid;
            highs[channel] = run_biquads(&mut self.high[channel], &high2, input);
        }
        (lows, mids, highs)
    }
}

/// Linear-phase Hilbert FIR used by the Bode frequency shifter.
const BODE_HILBERT_TAPS: usize = 63;
const BODE_HILBERT_CENTER: usize = BODE_HILBERT_TAPS / 2;

fn bode_hilbert_coefficients() -> [f64; BODE_HILBERT_TAPS] {
    let mut coefficients = [0.0; BODE_HILBERT_TAPS];
    for (index, coefficient) in coefficients.iter_mut().enumerate() {
        let offset = index as i32 - BODE_HILBERT_CENTER as i32;
        if offset == 0 || offset % 2 == 0 {
            continue;
        }
        let window = 0.54 - 0.46 * (TAU * index as f64 / (BODE_HILBERT_TAPS - 1) as f64).cos();
        *coefficient = 2.0 / (PI * offset as f64) * window;
    }
    coefficients
}

/// Feedback delay shared by the Bode instances (two preallocated slots).
struct BodeDelay {
    left: Box<[f64]>,
    right: Box<[f64]>,
    write: usize,
}

impl BodeDelay {
    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }

    fn new() -> Self {
        Self {
            left: vec![0.0; MAX_DELAY_SAMPLES].into_boxed_slice(),
            right: vec![0.0; MAX_DELAY_SAMPLES].into_boxed_slice(),
            write: 0,
        }
    }

    fn read(&self, delay: f64, sr: f64) -> (f64, f64) {
        let len = self.left.len();
        let samples = (delay.clamp(0.0, MAX_DELAY_SECONDS) * sr).round() as usize;
        let read = (self.write + len - samples.max(1).min(len - 2)) % len;
        (self.left[read], self.right[read])
    }

    fn write(&mut self, left: f64, right: f64) {
        self.left[self.write] = left;
        self.right[self.write] = right;
        self.write = (self.write + 1) % self.left.len();
    }
}

struct BodeState {
    coefficients: [f64; BODE_HILBERT_TAPS],
    history: [[f64; BODE_HILBERT_TAPS]; 2],
    write: usize,
    phase: f64,
}

impl Default for BodeState {
    fn default() -> Self {
        Self {
            coefficients: bode_hilbert_coefficients(),
            history: [[0.0; BODE_HILBERT_TAPS]; 2],
            write: 0,
            phase: 0.0,
        }
    }
}

impl BodeState {
    fn reset(&mut self) {
        self.history.fill([0.0; BODE_HILBERT_TAPS]);
        self.write = 0;
        self.phase = 0.0;
    }

    fn shift_channel(&mut self, channel: usize, input: f64) -> (f64, f64) {
        let write = self.write;
        let history = &mut self.history[channel];
        history[write] = input;
        let mut quadrature = 0.0;
        for (tap, coefficient) in self.coefficients.iter().enumerate() {
            quadrature +=
                coefficient * history[(write + BODE_HILBERT_TAPS - tap) % BODE_HILBERT_TAPS];
        }
        let delayed =
            history[(write + BODE_HILBERT_TAPS - BODE_HILBERT_CENTER) % BODE_HILBERT_TAPS];
        let (sine, cosine) = self.phase.sin_cos();
        // The two single-sideband outputs; mix 0 sums them back into ring
        // modulation, which is what the preset stores at the knob centre.
        (
            delayed * cosine + quadrature * sine,
            delayed * cosine - quadrature * sine,
        )
    }

    fn process(
        &mut self,
        delay: &mut BodeDelay,
        left: f64,
        right: f64,
        values: &[f64; MAX_FX_VALUE_SLOTS],
        sr: f64,
    ) -> (f64, f64) {
        let shift =
            (values[BODE_SHIFT] / 100.0).clamp(-1.0, 1.0) * values[BODE_RANGE].clamp(0.0, 5_000.0);
        let feedback = values[BODE_FEEDBACK].clamp(0.0, 0.95);
        let delay_time = values[BODE_DELAY].clamp(0.0, MAX_DELAY_SECONDS);
        let output_mix = (values[BODE_OUTPUT_MIX] / 100.0).clamp(-1.0, 1.0);
        let output_width = (values[BODE_OUTPUT_WIDTH] / 100.0).clamp(0.0, 2.0);
        let mix = values[BODE_MIX].clamp(0.0, 1.0);
        self.phase = (self.phase + TAU * shift / sr).rem_euclid(TAU);

        let (mut input_left, mut input_right) = (left, right);
        if values[BODE_MONO_INPUT] >= 0.5 {
            let mono = (left + right) * 0.5;
            input_left = mono;
            input_right = mono;
        }
        let (delayed_left, delayed_right) = delay.read(delay_time, sr);
        let fed_left = input_left + feedback * delayed_left;
        let fed_right = input_right + feedback * delayed_right;
        let (mut down_left, mut up_left) = self.shift_channel(0, fed_left);
        let (mut down_right, mut up_right) = self.shift_channel(1, fed_right);
        if values[BODE_SWAP_AB] >= 0.5 {
            std::mem::swap(&mut down_left, &mut up_left);
            std::mem::swap(&mut down_right, &mut up_right);
        }
        self.write = (self.write + 1) % BODE_HILBERT_TAPS;
        let dry = 1.0 - output_mix.abs();
        let mid_left = down_left * (1.0 - output_mix) * 0.5 + up_left * (1.0 + output_mix) * 0.5;
        let mid_right = down_right * (1.0 - output_mix) * 0.5 + up_right * (1.0 + output_mix) * 0.5;
        let side_left = (down_left - up_left) * 0.5 * output_width * dry;
        let side_right = (down_right - up_right) * 0.5 * output_width * dry;
        let wet_left = mid_left + side_left;
        let wet_right = mid_right - side_right;
        delay.write(wet_left, wet_right);
        mix_stereo(left, right, wet_left, wet_right, mix)
    }
}

#[derive(Clone, Copy, Default)]
struct FxBranch {
    start: usize,
    end: usize,
}

/// Cached tree layout for one rack slot. Child chains follow their node in the
/// patch, so load resolves the spans and the audio path only walks them.
#[derive(Clone, Copy, Default)]
struct FxLayout {
    end: usize,
    branches: [FxBranch; MAX_FX_BRANCHES],
}

pub struct SynthFxChain {
    modules: [FxModule; MAX_SYNTH_FX],
    count: usize,
    layout: [FxLayout; MAX_SYNTH_FX],
    drives: [DriveState; MAX_SYNTH_FX],
    distortions: [DistortionState; MAX_SYNTH_FX],
    compressors: [CompressorState; MAX_SYNTH_FX],
    equalizers: [EqState; MAX_SYNTH_FX],
    phasers: [PhaserState; MAX_SYNTH_FX],
    filters: [FilterState; MAX_SYNTH_FX],
    utils: [UtilsState; MAX_SYNTH_FX],
    crossovers: [CrossoverState; MAX_SYNTH_FX],
    bodes: [BodeState; MAX_SYNTH_FX],
    hyperds: [HyperDState; MAX_SYNTH_FX],
    multibands: [MultibandState; MAX_SYNTH_FX],
    delays: Vec<DelayState>,
    reverbs: Vec<ReverbState>,
    choruses: Vec<ChorusState>,
    flangers: Vec<FlangerState>,
    bode_delays: Vec<BodeDelay>,
    convolutions: Vec<ConvState>,
    tail_remaining: u64,
    tail_samples: u64,
    sample_rate: f64,
}

impl Default for SynthFxChain {
    fn default() -> Self {
        Self::new()
    }
}

impl SynthFxChain {
    pub fn new() -> Self {
        Self {
            modules: [FxModule::default(); MAX_SYNTH_FX],
            count: 0,
            layout: [FxLayout::default(); MAX_SYNTH_FX],
            drives: [DriveState::default(); MAX_SYNTH_FX],
            distortions: std::array::from_fn(|_| DistortionState::new()),
            compressors: std::array::from_fn(|_| CompressorState::default()),
            equalizers: [EqState::default(); MAX_SYNTH_FX],
            phasers: [PhaserState::default(); MAX_SYNTH_FX],
            filters: [FilterState::default(); MAX_SYNTH_FX],
            utils: [UtilsState::default(); MAX_SYNTH_FX],
            crossovers: [CrossoverState::default(); MAX_SYNTH_FX],
            bodes: std::array::from_fn(|_| BodeState::default()),
            hyperds: std::array::from_fn(|_| HyperDState::new()),
            multibands: std::array::from_fn(|_| MultibandState::default()),
            delays: (0..INITIAL_TIME_INSTANCES)
                .map(|_| DelayState::new())
                .collect(),
            reverbs: (0..INITIAL_TIME_INSTANCES)
                .map(|_| ReverbState::new())
                .collect(),
            choruses: (0..INITIAL_TIME_INSTANCES)
                .map(|_| ChorusState::new())
                .collect(),
            flangers: (0..INITIAL_TIME_INSTANCES)
                .map(|_| FlangerState::new())
                .collect(),
            bode_delays: (0..INITIAL_TIME_INSTANCES)
                .map(|_| BodeDelay::new())
                .collect(),
            convolutions: (0..INITIAL_TIME_INSTANCES)
                .map(|_| ConvState::new())
                .collect(),
            tail_remaining: 0,
            tail_samples: 0,
            sample_rate: 48_000.0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.count > 0
    }

    pub fn tail_active(&self) -> bool {
        self.tail_remaining > 0
    }

    /// Restore every live parameter slot from its imported base value. Called
    /// once per FX-modulation control tick before the compiled routes are
    /// applied, so multiple routes sum and removed routes leave no residue.
    /// This is a bounded copy over the fixed rack: no allocation or locks.
    pub fn reset_modulation(&mut self) {
        for module in &mut self.modules[..self.count] {
            module.values = module.base_values;
        }
    }

    /// Clear runtime histories and pending tails for a new playback origin.
    /// Restores the patch's base parameters while retaining the prepared rack,
    /// sample rate, delay capacities, convolution IRs and FFT plans. All memory
    /// is cleared in place; this operation performs no allocation or I/O.
    pub fn reset(&mut self) {
        self.reset_modulation();
        self.drives.fill(DriveState::default());
        self.phasers.fill(PhaserState::default());
        self.filters.fill(FilterState::default());
        self.utils.fill(UtilsState::default());
        self.crossovers.fill(CrossoverState::default());
        for state in &mut self.distortions {
            state.reset();
        }
        for state in &mut self.equalizers {
            state.left = EqChannelState::default();
            state.right = EqChannelState::default();
        }
        for (index, state) in self.compressors.iter_mut().enumerate() {
            if self.modules[index].fx_type == SynthFxType::Compressor {
                state.configure(&self.modules[index].values);
            }
            if let Some(compander) = state.compander.as_mut() {
                compander.reset();
            }
        }
        for (index, state) in self.multibands.iter_mut().enumerate() {
            if self.modules[index].fx_type == SynthFxType::Multiband {
                state.configure(&self.modules[index].values);
            }
            if let Some(compressor) = state.compressor.as_mut() {
                compressor.reset();
            }
        }
        for state in &mut self.bodes {
            state.reset();
        }
        for state in &mut self.hyperds {
            state.reset();
        }
        for state in &mut self.delays {
            state.reset();
        }
        for state in &mut self.reverbs {
            state.reset();
        }
        for state in &mut self.choruses {
            state.reset();
        }
        for state in &mut self.flangers {
            state.reset();
        }
        for state in &mut self.bode_delays {
            state.reset();
        }
        for state in &mut self.convolutions {
            state.reset();
        }
        self.tail_remaining = 0;
        self.refresh_tail();
    }

    /// Current live value of one native parameter. `None` when the module or
    /// parameter does not exist, which makes stale routes harmless.
    pub fn module_value(&self, index: usize, name: &str) -> Option<f64> {
        if index >= self.count {
            return None;
        }
        let module = &self.modules[index];
        let slot = fx_param_slot(module.fx_type, name)?;
        Some(module.values[slot])
    }

    /// Update one live parameter without touching its base value. The module
    /// palette is fixed, so this never grows a vector on the audio thread.
    pub fn set_module_param(&mut self, index: usize, name: &str, value: f64) {
        if !value.is_finite() || index >= self.count {
            return;
        }
        let module = &mut self.modules[index];
        let Some(slot) = fx_param_slot(module.fx_type, name) else {
            return;
        };
        module.values[slot] = value;
    }

    /// Prepare a patch's topology and copy its parameter palette. Call this on
    /// the load/edit path before publishing a rack to the audio callback. Adding
    /// modules can allocate their independent state; removed slots retain their
    /// storage for reuse. A topology change resets runtime histories, while a
    /// parameter-only update preserves them and reuses prepared storage.
    /// Live modulation uses `set_module_param` and never grows these pools.
    pub fn sync_from_patch(&mut self, fx: &[SynthFx]) {
        let count = fx.len().min(MAX_SYNTH_FX);
        let topology_changed = self.count != count
            || self.modules[..count]
                .iter()
                .zip(fx)
                .any(|(old, new)| old.fx_type != new.fx_type);
        let mut time_counts = [0usize; 6];
        // Fixed-capacity slots with a dynamic count; a five-way zip would obscure it.
        #[allow(clippy::needless_range_loop)]
        for index in 0..MAX_SYNTH_FX {
            if index < count {
                let module = &fx[index];
                let bucket = match module.fx_type {
                    SynthFxType::Delay => Some(0),
                    SynthFxType::Reverb => Some(1),
                    SynthFxType::Chorus => Some(2),
                    SynthFxType::Flanger => Some(3),
                    SynthFxType::Bode => Some(4),
                    SynthFxType::Conv => Some(5),
                    _ => None,
                };
                let time_slot = match bucket {
                    Some(bucket) => {
                        let slot = time_counts[bucket];
                        time_counts[bucket] += 1;
                        slot
                    }
                    None => 0,
                };
                if self.modules[index].fx_type != module.fx_type {
                    self.hyperds[index].reset();
                    self.multibands[index].compressor = None;
                    self.compressors[index].compander = None;
                }
                self.modules[index] = FxModule::from_patch(module, time_slot);
            } else {
                self.modules[index] = FxModule::default();
            }
        }
        // Exact per-type occurrence slots are bounded by MAX_SYNTH_FX. Grow
        // only the type that needs a new instance and retain every prior slot.
        self.delays
            .resize_with(time_counts[0].max(self.delays.len()), DelayState::new);
        self.reverbs
            .resize_with(time_counts[1].max(self.reverbs.len()), ReverbState::new);
        self.choruses
            .resize_with(time_counts[2].max(self.choruses.len()), ChorusState::new);
        self.flangers
            .resize_with(time_counts[3].max(self.flangers.len()), FlangerState::new);
        self.bode_delays
            .resize_with(time_counts[4].max(self.bode_delays.len()), BodeDelay::new);
        self.convolutions
            .resize_with(time_counts[5].max(self.convolutions.len()), ConvState::new);
        self.count = count;
        self.layout = [FxLayout::default(); MAX_SYNTH_FX];
        self.build_layout(0, count);
        self.prepare_convolutions();
        self.prepare_multibands();
        self.prepare_compressors();
        self.prepare_delays();
        self.prepare_reverbs();
        if topology_changed {
            self.reset();
        } else {
            self.refresh_tail();
        }
    }

    /// Snap the reverb tank lines to the current rate. Buffers are allocated
    /// once in [`ReverbState::new`]; this only clears and re-primes lengths.
    fn prepare_reverbs(&mut self) {
        let sr = self.sample_rate.max(8_000.0);
        for index in 0..self.count {
            let module = self.modules[index];
            if module.fx_type == SynthFxType::Reverb && module.enabled {
                self.reverbs[module.time_slot].prepare(sr);
            }
        }
    }

    /// Size the delay state's reported latency for the current rate. The delay
    /// line itself is allocated on the first Delay module and never resized.
    fn prepare_delays(&mut self) {
        let sr = self.sample_rate.max(8_000.0);
        for index in 0..self.count {
            let module = self.modules[index];
            if module.fx_type == SynthFxType::Delay && module.enabled {
                self.delays[module.time_slot].prepare(sr);
            }
        }
    }

    /// Keep each convolution state's generated IR in step with its module. It
    /// only runs when the sample rate or selected size changed.
    fn prepare_convolutions(&mut self) {
        let sr = self.sample_rate.max(8_000.0);
        for index in 0..self.count {
            let module = self.modules[index];
            if module.fx_type == SynthFxType::Conv {
                let preset = conv_preset_for_size(module.values[0]);
                self.convolutions[module.time_slot].prepare(sr, preset);
            }
        }
    }

    /// Allocate and configure each multiband device from its current palette.
    fn prepare_multibands(&mut self) {
        let sr = self.sample_rate.max(8_000.0).round() as u32;
        for index in 0..self.count {
            let module = self.modules[index];
            if module.fx_type == SynthFxType::Multiband {
                self.multibands[index].ensure(sr);
                self.multibands[index].configure(&module.values);
            }
        }
    }

    /// Allocate and configure each three-band compander on the load path.
    fn prepare_compressors(&mut self) {
        let sr = self.sample_rate.max(8_000.0).round() as u32;
        for index in 0..self.count {
            let module = self.modules[index];
            if module.fx_type == SynthFxType::Compressor {
                self.compressors[index].prepare(sr);
                self.compressors[index].configure(&module.values);
            }
        }
    }

    /// Algorithmic latency of the enabled rack modules, in samples. The
    /// engine reports it as the track's source latency.
    pub fn latency_samples(&self) -> usize {
        let mut total = 0;
        for (index, module) in self.modules[..self.count].iter().enumerate() {
            if !module.enabled {
                continue;
            }
            match module.fx_type {
                SynthFxType::Delay => {
                    total += self.delays[module.time_slot].latency_samples();
                }
                SynthFxType::Distortion => {
                    total += DISTORTION_LATENCY;
                }
                SynthFxType::Compressor => {
                    total += self.compressors[index].latency_samples();
                }
                SynthFxType::Conv => {
                    total += self.convolutions[module.time_slot].latency_samples();
                }
                SynthFxType::Multiband => {
                    total += self.multibands[index].latency_samples();
                }
                _ => {}
            }
        }
        total
    }

    /// Resolve one serial span into branch spans and return its end. Child
    /// counts are structural and read only here; gains stay live parameters.
    fn build_layout(&mut self, start: usize, end: usize) -> usize {
        let mut index = start;
        while index < end {
            let module = self.modules[index];
            let mut child = index + 1;
            let mut layout = FxLayout {
                end: child,
                branches: [FxBranch::default(); MAX_FX_BRANCHES],
            };
            let branches = match module.fx_type {
                SynthFxType::Split => 2,
                SynthFxType::Split3 => 3,
                SynthFxType::SplitMs => 2,
                SynthFxType::ParallelRack => PARALLEL_BRANCHES,
                _ => 0,
            };
            for band in 0..branches {
                let count = branch_count(&module, band);
                let child_end = self.build_layout(child, end.min(child + count));
                layout.branches[band] = FxBranch {
                    start: child,
                    end: child_end,
                };
                child = child_end;
            }
            layout.end = child;
            self.layout[index] = layout;
            index = child;
        }
        end
    }

    fn refresh_tail(&mut self) {
        let sr = self.sample_rate.max(8_000.0);
        let mut tail = 0u64;
        for module in self.modules[..self.count].iter() {
            if !module.enabled {
                continue;
            }
            let mix = match module.fx_type {
                SynthFxType::Delay => module.values[3],
                SynthFxType::Reverb => module.values[4],
                SynthFxType::Chorus => module.values[3],
                SynthFxType::Flanger => module.values[3],
                SynthFxType::Bode => module.values[BODE_MIX],
                SynthFxType::HyperD => module.values[4],
                SynthFxType::Conv => module.values[4],
                _ => 0.0,
            };
            if mix <= 0.0 {
                continue;
            }
            let seconds = match module.fx_type {
                SynthFxType::Delay => {
                    let time = delay_time_samples(module.values[0], sr) / sr;
                    let feedback = module.values[1].clamp(0.0, 1.0);
                    let repeats = (1e-4f64.ln() / feedback.max(1e-3).ln())
                        .ceil()
                        .clamp(1.0, 200.0);
                    time + time * repeats
                }
                SynthFxType::Reverb => reverb_tail_seconds(module.values[0], module.values[2]),
                SynthFxType::Chorus => 0.08,
                SynthFxType::Flanger => {
                    let depth = module.values[1].clamp(0.0, 1.0);
                    let delay = 0.0005 + depth * 0.0045;
                    let feedback = module.values[2].clamp(0.0, 0.85);
                    let repeats = (1e-4f64.ln() / feedback.max(1e-3).ln())
                        .ceil()
                        .clamp(1.0, 200.0);
                    delay + delay * repeats
                }
                SynthFxType::HyperD => 0.15,
                SynthFxType::Conv => {
                    let preset = conv_preset_for_size(module.values[0]);
                    CONV_PRESETS[preset].seconds + CONV_LATENCY as f64 / sr
                }
                SynthFxType::Bode => {
                    let delay = module.values[BODE_DELAY].clamp(0.0, MAX_DELAY_SECONDS);
                    let feedback = module.values[BODE_FEEDBACK].clamp(0.0, 0.95);
                    if feedback <= 0.0 {
                        0.0
                    } else {
                        let repeats = (1e-4f64.ln() / feedback.max(1e-3).ln())
                            .ceil()
                            .clamp(1.0, 200.0);
                        delay.max(0.01) + delay.max(0.01) * repeats
                    }
                }
                _ => 0.0,
            };
            tail = tail.max((seconds * sr).round() as u64);
        }
        self.tail_samples = tail.min((TAIL_CAP_SECONDS * sr).round() as u64);
        if self.tail_remaining > self.tail_samples {
            self.tail_remaining = self.tail_samples;
        }
    }

    /// Process one stereo sample. Returns the input untouched when the chain is
    /// empty so a patch without FX is bit-identical to the pre-rack engine.
    pub fn process(&mut self, left: f64, right: f64, sample_rate: f64) -> [f64; 2] {
        if self.count == 0 {
            return [left, right];
        }
        let sr = sample_rate.max(8_000.0);
        if (self.sample_rate - sr).abs() > 0.5 {
            self.sample_rate = sr;
            self.prepare_convolutions();
            self.prepare_multibands();
            self.prepare_compressors();
            self.prepare_delays();
            self.prepare_reverbs();
            self.refresh_tail();
        }
        if left.abs().max(right.abs()) > 1e-6 {
            self.tail_remaining = self.tail_samples;
        } else {
            self.tail_remaining = self.tail_remaining.saturating_sub(1);
        }
        let (left, right) = self.process_range(0, self.count, left, right, sr, 0);
        [left, right]
    }

    fn level_out(module: &FxModule) -> f64 {
        if module.level_out_slot < MAX_FX_VALUE_SLOTS {
            module.values[module.level_out_slot]
        } else {
            1.0
        }
    }

    /// Run one non-splitter module, including its post-mix output trim.
    fn process_module(&mut self, index: usize, left: f64, right: f64, sr: f64) -> (f64, f64) {
        let module = self.modules[index];
        let values = &module.values;
        let (next_left, next_right) = match module.fx_type {
            SynthFxType::Drive => self.drives[index].process(left, right, values, sr),
            SynthFxType::Distortion => self.distortions[index].process(left, right, values, sr),
            SynthFxType::Compressor => self.compressors[index].process(left, right, values),
            SynthFxType::Eq3 => self.equalizers[index].process(left, right, values, sr),
            SynthFxType::Phaser => self.phasers[index].process(left, right, values, sr),
            SynthFxType::Filter => self.filters[index].process(left, right, values, sr),
            SynthFxType::Utils => self.utils[index].process(left, right, values, sr),
            SynthFxType::Delay => self.delays[module.time_slot].process(left, right, values, sr),
            SynthFxType::Reverb => self.reverbs[module.time_slot].process(left, right, values, sr),
            SynthFxType::Chorus => self.choruses[module.time_slot].process(left, right, values, sr),
            SynthFxType::Flanger => {
                self.flangers[module.time_slot].process(left, right, values, sr)
            }
            SynthFxType::HyperD => self.hyperds[index].process(left, right, values, sr),
            SynthFxType::Conv => {
                self.convolutions[module.time_slot].process(left, right, values, sr)
            }
            SynthFxType::Multiband => self.multibands[index].process(left, right, values, sr),
            SynthFxType::Bode => self.bodes[index].process(
                &mut self.bode_delays[module.time_slot],
                left,
                right,
                values,
                sr,
            ),
            // Routers are consumed by `process_range`, never here.
            SynthFxType::Split
            | SynthFxType::Split3
            | SynthFxType::SplitMs
            | SynthFxType::ParallelRack => (left, right),
        };
        let level_out = Self::level_out(&module);
        (next_left * level_out, next_right * level_out)
    }

    /// Walk one serial span. Split and parallel nodes own child spans from
    /// `build_layout`.
    fn process_range(
        &mut self,
        start: usize,
        end: usize,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
    ) -> (f64, f64) {
        let mut left = left;
        let mut right = right;
        let mut index = start;
        while index < end {
            let module = self.modules[index];
            let layout = self.layout[index];
            let next = if layout.end > index {
                layout.end
            } else {
                index + 1
            };
            if !module.enabled {
                index = next;
                continue;
            }
            let (next_left, next_right) = match module.fx_type {
                SynthFxType::Split => self.process_split(index, left, right, sr, depth),
                SynthFxType::Split3 => self.process_split3(index, left, right, sr, depth),
                SynthFxType::SplitMs => self.process_split_ms(index, left, right, sr, depth),
                SynthFxType::ParallelRack => self.process_parallel(index, left, right, sr, depth),
                _ => self.process_module(index, left, right, sr),
            };
            left = next_left;
            right = next_right;
            index = next;
        }
        (left, right)
    }

    /// Run one band or parallel branch and apply its gain. Silencing a branch
    /// skips its state so a muted branch contributes exactly zero.
    // DSP hot path; a parameter struct would be rebuilt per call.
    #[allow(clippy::too_many_arguments)]
    fn process_branch(
        &mut self,
        branch: FxBranch,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
        bypass: bool,
        gain: f64,
    ) -> (f64, f64) {
        if gain == 0.0 {
            return (0.0, 0.0);
        }
        let (left, right) = if bypass || branch.start >= branch.end || depth >= MAX_SYNTH_FX {
            (left, right)
        } else {
            self.process_range(branch.start, branch.end, left, right, sr, depth + 1)
        };
        (left * gain, right * gain)
    }

    fn process_split(
        &mut self,
        index: usize,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
    ) -> (f64, f64) {
        let module = self.modules[index];
        let values = &module.values;
        let layout = self.layout[index];
        let freq = values[SPLIT_FREQ].clamp(20.0, 20_000.0).min(sr * 0.45);
        let (lows, highs) = self.crossovers[index].split_2(left, right, freq, sr);
        let low = self.process_branch(
            layout.branches[0],
            lows[0],
            lows[1],
            sr,
            depth,
            values[SPLIT_BYPASS1] >= 0.5,
            values[SPLIT_GAIN1],
        );
        let high = self.process_branch(
            layout.branches[1],
            highs[0],
            highs[1],
            sr,
            depth,
            values[SPLIT_BYPASS2] >= 0.5,
            values[SPLIT_GAIN2],
        );
        let level_out = Self::level_out(&module);
        ((low.0 + high.0) * level_out, (low.1 + high.1) * level_out)
    }

    fn process_split3(
        &mut self,
        index: usize,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
    ) -> (f64, f64) {
        let module = self.modules[index];
        let values = &module.values;
        let layout = self.layout[index];
        let freq1 = values[SPLIT3_FREQ].clamp(20.0, 20_000.0).min(sr * 0.45);
        let freq2 = values[SPLIT3_FREQ2].clamp(20.0, 20_000.0).min(sr * 0.45);
        let freq2 = if freq2 <= freq1 {
            (freq1 * 1.01).min(sr * 0.45)
        } else {
            freq2
        };
        let (lows, mids, highs) = self.crossovers[index].split_3(left, right, freq1, freq2, sr);
        let low = self.process_branch(
            layout.branches[0],
            lows[0],
            lows[1],
            sr,
            depth,
            values[SPLIT3_BYPASS1] >= 0.5,
            values[SPLIT3_GAIN1],
        );
        let mid = self.process_branch(
            layout.branches[1],
            mids[0],
            mids[1],
            sr,
            depth,
            values[SPLIT3_BYPASS2] >= 0.5,
            values[SPLIT3_GAIN2],
        );
        let high = self.process_branch(
            layout.branches[2],
            highs[0],
            highs[1],
            sr,
            depth,
            values[SPLIT3_BYPASS3] >= 0.5,
            values[SPLIT3_GAIN3],
        );
        let level_out = Self::level_out(&module);
        (
            (low.0 + mid.0 + high.0) * level_out,
            (low.1 + mid.1 + high.1) * level_out,
        )
    }

    fn process_split_ms(
        &mut self,
        index: usize,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
    ) -> (f64, f64) {
        let module = self.modules[index];
        let values = &module.values;
        let layout = self.layout[index];
        let mid = (left + right) * 0.5;
        let side = (left - right) * 0.5;
        let mid = self.process_branch(
            layout.branches[0],
            mid,
            mid,
            sr,
            depth,
            values[SPLIT_MS_BYPASS1] >= 0.5,
            values[SPLIT_MS_GAIN1],
        );
        let side = self.process_branch(
            layout.branches[1],
            side,
            side,
            sr,
            depth,
            values[SPLIT_MS_BYPASS2] >= 0.5,
            values[SPLIT_MS_GAIN2],
        );
        let level_out = Self::level_out(&module);
        ((mid.0 + side.0) * level_out, (mid.1 - side.1) * level_out)
    }

    fn process_parallel(
        &mut self,
        index: usize,
        left: f64,
        right: f64,
        sr: f64,
        depth: usize,
    ) -> (f64, f64) {
        let module = self.modules[index];
        let values = &module.values;
        let layout = self.layout[index];
        let mut out = (0.0, 0.0);
        for band in 0..PARALLEL_BRANCHES {
            // A parallel branch gain is a send level, so it scales the branch
            // input rather than trimming its processed output.
            let gain = values[PARALLEL_GAINS[band]];
            if gain == 0.0 {
                continue;
            }
            let branch = self.process_branch(
                layout.branches[band],
                left * gain,
                right * gain,
                sr,
                depth,
                false,
                1.0,
            );
            out.0 += branch.0;
            out.1 += branch.1;
        }
        let level_out = Self::level_out(&module);
        (out.0 * level_out, out.1 * level_out)
    }
}

#[cfg(test)]
mod test_alloc;
#[cfg(test)]
mod tests;
