//! Bounded per-note control curves shared by score adapters and renderers.
use serde::{Deserialize, Serialize};

/// Maximum authored points in a note-relative pitch curve.
pub const MAX_PITCH_CURVE_POINTS: usize = 1024;

/// How a pitch value changes between authored points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PitchCurveMode {
    #[default]
    Linear,
    /// Hold the left point until the next point's exact score time.
    Step,
}

impl PitchCurveMode {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Step => "step",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Linear, Self::Step]
            .into_iter()
            .find(|mode| mode.token() == token)
    }
}
