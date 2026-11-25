//! Shared controls for the mixer delay.

use crate::{
    ParamCurve::*,
    ParamSchema,
    ParamUnit::{Hz, Ms, None as Unitless, Percent},
    bool_param, enum_param, param, with_aliases, with_description,
};

/// The signal topology used by the first echo and its feedback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DelayRouting {
    #[default]
    Stereo,
    Mono,
    Swapped,
    PingPong,
}

impl DelayRouting {
    const OPTIONS: &'static [&'static str] = &["Stereo", "Mono", "Swapped", "Ping Pong"];

    pub fn code(self) -> f64 {
        self as u8 as f64
    }

    pub fn from_code(value: f64) -> Option<Self> {
        match value {
            0.0 => Some(Self::Stereo),
            1.0 => Some(Self::Mono),
            2.0 => Some(Self::Swapped),
            3.0 => Some(Self::PingPong),
            _ => None,
        }
    }
}

/// Filter applied to the signal returning to the delay line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FeedbackFilter {
    #[default]
    Off,
    LowPass,
    HighPass,
    BandPass,
}

impl FeedbackFilter {
    const OPTIONS: &'static [&'static str] = &["Off", "Low Pass", "High Pass", "Band Pass"];

    pub fn code(self) -> f64 {
        self as u8 as f64
    }

    pub fn from_code(value: f64) -> Option<Self> {
        match value {
            0.0 => Some(Self::Off),
            1.0 => Some(Self::LowPass),
            2.0 => Some(Self::HighPass),
            3.0 => Some(Self::BandPass),
            _ => None,
        }
    }
}

pub const MAX_TIME_MS: f64 = 2000.0;
pub const MAX_GAIN_PERCENT: f64 = 200.0;
pub const MIN_QUARTERS: f64 = 1.0 / 32.0;
pub const MAX_QUARTERS: f64 = 4.0;
pub const MIN_CUTOFF_HZ: f64 = 20.0;
pub const MAX_CUTOFF_HZ: f64 = 20_000.0;
pub const DEFAULT_CUTOFF_HZ: f64 = 10_000.0;

pub const PARAMS: &[ParamSchema] = &[
    with_aliases(
        param("time_l", "Left Time", Ms, 1.0, MAX_TIME_MS, 375.0, Log),
        &["time"],
    ),
    param("time_r", "Right Time", Ms, 1.0, MAX_TIME_MS, 250.0, Log),
    param("feedback", "Feedback", Percent, 0.0, 95.0, 35.0, Linear),
    param("mix", "Mix", Percent, 0.0, 100.0, 30.0, Linear),
    with_description(
        bool_param("interpolate", "Interpolate Time", false),
        "Interpolate fractional read positions when automating delay time.",
    ),
    enum_param(
        "routing",
        "Routing",
        DelayRouting::OPTIONS,
        DelayRouting::Stereo as usize,
    ),
    with_description(
        bool_param("tempo_sync", "Tempo Sync", false),
        "Use quarter-note lengths at the current tempo, capped at two seconds per tap.",
    ),
    param(
        "quarters_l",
        "Left Quarter Notes",
        Unitless,
        MIN_QUARTERS,
        MAX_QUARTERS,
        0.75,
        Log,
    ),
    param(
        "quarters_r",
        "Right Quarter Notes",
        Unitless,
        MIN_QUARTERS,
        MAX_QUARTERS,
        0.5,
        Log,
    ),
    with_description(
        param(
            "dry_gain",
            "Dry Gain",
            Percent,
            0.0,
            MAX_GAIN_PERCENT,
            100.0,
            Linear,
        ),
        "Scale the dry signal after the Mix control.",
    ),
    with_description(
        param(
            "wet_gain",
            "Wet Gain",
            Percent,
            0.0,
            MAX_GAIN_PERCENT,
            100.0,
            Linear,
        ),
        "Scale the delayed signal after the Mix control.",
    ),
    enum_param(
        "feedback_filter",
        "Feedback Filter",
        FeedbackFilter::OPTIONS,
        FeedbackFilter::Off as usize,
    ),
    param(
        "feedback_cutoff",
        "Feedback Cutoff",
        Hz,
        MIN_CUTOFF_HZ,
        MAX_CUTOFF_HZ,
        DEFAULT_CUTOFF_HZ,
        Log,
    ),
];
