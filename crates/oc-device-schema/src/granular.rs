//! Physical domains for file-backed grain clouds.
use serde::{Deserialize, Serialize};

use crate::{ParamCurve, ParamSchema, ParamUnit, param};

pub const ROOT_NOTE: ParamSchema = param(
    "granular.root_note",
    "Root note",
    ParamUnit::None,
    0.0,
    127.0,
    60.0,
    ParamCurve::Linear,
);
pub const DENSITY: ParamSchema = param(
    "granular.density",
    "Grains per second",
    ParamUnit::Hz,
    0.0,
    4096.0,
    20.0,
    ParamCurve::Linear,
);
pub const LENGTH: ParamSchema = param(
    "granular.grain_length",
    "Grain length",
    ParamUnit::Seconds,
    0.002,
    10.0,
    0.1,
    ParamCurve::Log,
);
pub const POSITION: ParamSchema = param(
    "granular.position",
    "Source position",
    ParamUnit::Seconds,
    0.0,
    86_400.0,
    0.0,
    ParamCurve::Linear,
);
pub const SCAN_RATE: ParamSchema = param(
    "granular.scan_rate",
    "Source durations per second",
    ParamUnit::None,
    -4096.0,
    4096.0,
    0.0,
    ParamCurve::Linear,
);
pub const UNIT: ParamSchema = param(
    "granular.unit",
    "Grain fraction",
    ParamUnit::None,
    0.0,
    1.0,
    0.0,
    ParamCurve::Linear,
);
pub const WINDOW_PARAM: ParamSchema = param(
    "granular.window_param",
    "Window shape",
    ParamUnit::None,
    0.0,
    1.0,
    0.5,
    ParamCurve::Linear,
);
pub const WINDOW_SKEW: ParamSchema = param(
    "granular.window_skew",
    "Window skew",
    ParamUnit::None,
    -1.0,
    1.0,
    0.0,
    ParamCurve::Linear,
);
pub const RANDOM_PITCH: ParamSchema = param(
    "granular.random_pitch",
    "Pitch jitter",
    ParamUnit::Semitones,
    0.0,
    48.0,
    0.0,
    ParamCurve::Linear,
);

/// Check without clamping an authored value into a different program.
pub fn contains(domain: &ParamSchema, value: f64) -> bool {
    value.is_finite() && (domain.min..=domain.max).contains(&value)
}

/// Signed pan jitter reverses the deterministic random spread.
pub const RANDOM_PAN: ParamSchema = param(
    "granular.random_pan",
    "Pan jitter",
    ParamUnit::None,
    -1.0,
    1.0,
    0.0,
    ParamCurve::Linear,
);

/// Whether a grain retains the source channels or uses the mono source mix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceChannelMode {
    #[default]
    Mono,
    Stereo,
}

impl SourceChannelMode {
    /// Omit the default so documents without this option keep their meaning.
    pub fn is_mono(&self) -> bool {
        matches!(self, Self::Mono)
    }
}

/// Whether note pitch is sampled at grain onset or followed during its lifetime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PitchMode {
    #[default]
    Spawn,
    Follow,
}

impl PitchMode {
    /// Keep documents without an explicit mode on onset-sampled pitch.
    pub fn is_spawn(&self) -> bool {
        matches!(self, Self::Spawn)
    }
}

/// Source interpolation and anti-aliasing for transposed grains.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    #[default]
    Linear,
    Bandlimited,
}

impl ReadMode {
    /// Preserve the default interpolation of existing patches.
    pub fn is_linear(&self) -> bool {
        matches!(self, Self::Linear)
    }
}

/// Linear attack and release as fractions of the grain duration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrainEnvelope {
    pub attack: f64,
    pub release: f64,
    /// Optional attack fraction for the grain that starts the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_attack: Option<f64>,
}

impl GrainEnvelope {
    /// Each attack and the common release share one grain duration.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !contains(&UNIT, self.release) {
            return Err("granular envelope release must be a finite fraction in 0..=1");
        }
        for attack in [self.attack, self.first_attack.unwrap_or(self.attack)] {
            if !contains(&UNIT, attack) || attack + self.release > 1.0 {
                return Err("granular envelope attack and release must fit within the grain");
            }
        }
        Ok(())
    }
}
