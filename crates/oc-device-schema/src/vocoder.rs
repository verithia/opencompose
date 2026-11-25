//! Editable controls for the stereo filter-bank vocoder.

use crate::{
    ParamCurve::{Linear, Log},
    ParamSchema,
    ParamUnit::{Db, Ms, None, Percent},
    param, stepped_param,
};

pub const BANDS: ParamSchema = stepped_param("bands", "Bands", None, 12.0, 48.0, 24.0, 37, Linear);
pub const ATTACK: ParamSchema = param("attack", "Attack", Ms, 0.5, 100.0, 5.0, Log);
pub const RELEASE: ParamSchema = param("release", "Release", Ms, 5.0, 500.0, 50.0, Log);
pub const OUTPUT_GAIN: ParamSchema =
    param("output_gain", "Output Gain", Db, -24.0, 24.0, 0.0, Linear);
pub const MIX: ParamSchema = param("mix", "Mix", Percent, 0.0, 100.0, 100.0, Linear);
pub const PARAMS: &[ParamSchema] = &[BANDS, ATTACK, RELEASE, OUTPUT_GAIN, MIX];
