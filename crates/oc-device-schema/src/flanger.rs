//! Working ranges for the modulated delay stack.

use crate::{ParamCurve::*, ParamSchema, ParamUnit::*, param, stepped_param, with_description};

pub const MAX_VOICES: usize = 8;
pub const MIN_DELAY_MS: f64 = 0.5;
pub const MAX_DELAY_MS: f64 = 40.0;
pub const MAX_GAIN_PERCENT: f64 = 200.0;

pub const PARAMS: &[ParamSchema] = &[
    param("rate", "Rate", Hz, 0.05, 10.0, 0.5, Log),
    param("depth", "Depth", Ms, 0.1, 20.0, 3.0, Log),
    param("feedback", "Feedback", Percent, 0.0, 95.0, 50.0, Linear),
    param("mix", "Mix", Percent, 0.0, 100.0, 50.0, Linear),
    with_description(
        stepped_param(
            "voices",
            "Voices",
            None,
            1.0,
            MAX_VOICES as f64,
            1.0,
            MAX_VOICES as u32,
            Linear,
        ),
        "Number of swept taps; their wet contributions are averaged.",
    ),
    with_description(
        param(
            "delay",
            "Delay",
            Ms,
            MIN_DELAY_MS,
            MAX_DELAY_MS,
            MIN_DELAY_MS,
            Log,
        ),
        "Delay at the bottom of the sweep, before per-voice spread.",
    ),
    with_description(
        param("spread", "Spread", Percent, 0.0, 100.0, 0.0, Linear),
        "Separates the voices in delay, depth and modulation rate.",
    ),
    with_description(
        param(
            "stereo_cross",
            "Stereo Cross",
            Percent,
            -100.0,
            100.0,
            0.0,
            Linear,
        ),
        "Blend the delayed signal with the opposite channel. Negative values invert the crossed signal.",
    ),
    with_description(
        param(
            "dry_gain",
            "Dry Gain",
            Percent,
            -MAX_GAIN_PERCENT,
            MAX_GAIN_PERCENT,
            100.0,
            Linear,
        ),
        "Signed level trim on the unprocessed branch after applying Mix.",
    ),
    with_description(
        param(
            "wet_gain",
            "Wet Gain",
            Percent,
            -MAX_GAIN_PERCENT,
            MAX_GAIN_PERCENT,
            100.0,
            Linear,
        ),
        "Signed level trim on the delayed branch after applying Mix.",
    ),
];
