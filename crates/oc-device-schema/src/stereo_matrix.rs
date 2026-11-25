//! Physical controls for a stereo channel matrix and offset.
use crate::{ParamCurve::Linear, ParamSchema, ParamUnit::*, bool_param, param};

pub const LEFT_FROM_LEFT: ParamSchema = param(
    "left_from_left",
    "Left into left",
    Percent,
    -200.0,
    200.0,
    100.0,
    Linear,
);
pub const LEFT_FROM_RIGHT: ParamSchema = param(
    "left_from_right",
    "Right into left",
    Percent,
    -200.0,
    200.0,
    0.0,
    Linear,
);
pub const RIGHT_FROM_LEFT: ParamSchema = param(
    "right_from_left",
    "Left into right",
    Percent,
    -200.0,
    200.0,
    0.0,
    Linear,
);
pub const RIGHT_FROM_RIGHT: ParamSchema = param(
    "right_from_right",
    "Right into right",
    Percent,
    -200.0,
    200.0,
    100.0,
    Linear,
);
pub const OFFSET: ParamSchema = ParamSchema {
    description: "Negative delays the left channel. Positive delays the right channel.",
    ..param("offset", "Offset", Ms, -500.0, 500.0, 0.0, Linear)
};
pub const BEFORE: ParamSchema = bool_param("delay_before_matrix", "Delay before matrix", false);

pub const PARAMS: &[ParamSchema] = &[
    LEFT_FROM_LEFT,
    LEFT_FROM_RIGHT,
    RIGHT_FROM_LEFT,
    RIGHT_FROM_RIGHT,
    OFFSET,
    BEFORE,
];
