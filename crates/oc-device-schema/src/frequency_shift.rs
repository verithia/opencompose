//! Physical controls for the mixer frequency shifter.
use crate::{ParamCurve::Linear, ParamSchema, ParamUnit::*, param};

pub const SHIFT: ParamSchema = param("shift", "Shift", Hz, -5000.0, 5000.0, 0.0, Linear);
pub const LEFT: ParamSchema = param(
    "sideband_left",
    "Left sideband",
    Percent,
    -100.0,
    100.0,
    100.0,
    Linear,
);
pub const RIGHT: ParamSchema = param(
    "sideband_right",
    "Right sideband",
    Percent,
    -100.0,
    100.0,
    100.0,
    Linear,
);
pub const PHASE: ParamSchema = param("phase", "Start phase", Percent, 0.0, 100.0, 0.0, Linear);
pub const STEREO_PHASE: ParamSchema = param(
    "stereo_phase",
    "Stereo phase offset",
    Percent,
    -50.0,
    50.0,
    0.0,
    Linear,
);
pub const FEEDBACK: ParamSchema = param("feedback", "Feedback", Percent, 0.0, 90.0, 0.0, Linear);
pub const MIX: ParamSchema = param("mix", "Mix", Percent, 0.0, 100.0, 100.0, Linear);
pub const WIDTH: ParamSchema = param("width", "Stereo width", Percent, 0.0, 200.0, 100.0, Linear);

pub const PARAMS: &[ParamSchema] = &[
    SHIFT,
    LEFT,
    RIGHT,
    PHASE,
    STEREO_PHASE,
    FEEDBACK,
    MIX,
    WIDTH,
];
