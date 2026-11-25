//! Editable parametric bands and dedicated cuts for Dynamic EQ.

use super::{Db, Hz, Linear, Log, Ms, ParamSchema, ParamUnit, bool_param, enum_param, param};

/// Filter shapes in `BandKind` code order.
const BAND_TYPES: [&str; 7] = [
    "Bell",
    "Low Shelf",
    "High Shelf",
    "Low Cut",
    "High Cut",
    "Notch",
    "Band Pass",
];
/// Dynamic EQ output trim range in dB.
pub const DYNAMIC_EQ_OUTPUT_GAIN_DB_RANGE: (f64, f64) = (-24.0, 24.0);
/// Each successive cut slope adds one second-order section.
pub const DYNAMIC_EQ_CUT_SLOPES: [&str; 4] = ["12 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct"];

/// Resolve a cut slope label from the catalog or its DSL spelling.
pub fn dynamic_eq_cut_slope_code(label: &str) -> Option<usize> {
    fn normalize(text: &str) -> impl Iterator<Item = u8> + '_ {
        text.bytes()
            .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'_')
            .map(|byte| byte.to_ascii_lowercase())
    }
    DYNAMIC_EQ_CUT_SLOPES
        .iter()
        .position(|option| normalize(label).eq(normalize(option)))
}

/// Channel targets in `ChannelTarget` code order.
const CHANNEL_TARGETS: [&str; 5] = ["Stereo", "Mid", "Side", "Left", "Right"];

macro_rules! define_bands {
    ($($band:literal => $frequency:literal),+ $(,)?) => {
        /// Initial center frequency of each parametric slot in Dynamic EQ.
        pub const DYNAMIC_EQ_CENTER_FREQUENCIES: [f64; [$($frequency),+].len()] = [$($frequency),+];

        pub(super) const PARAMS: &[ParamSchema] = &[
            $(
                bool_param(
                    concat!("band", stringify!($band), "_enabled"),
                    concat!("Band ", stringify!($band), " Enabled"),
                    false,
                ),
                bool_param(
                    concat!("band", stringify!($band), "_solo"),
                    concat!("Band ", stringify!($band), " Solo"),
                    false,
                ),
                enum_param(
                    concat!("band", stringify!($band), "_channel"),
                    concat!("Band ", stringify!($band), " Channel"),
                    &CHANNEL_TARGETS, 0,
                ),
                enum_param(
                    concat!("band", stringify!($band), "_type"),
                    concat!("Band ", stringify!($band), " Type"),
                    &BAND_TYPES, 0,
                ),
                enum_param(
                    concat!("band", stringify!($band), "_slope"),
                    concat!("Band ", stringify!($band), " Cut Slope"),
                    &DYNAMIC_EQ_CUT_SLOPES, 0,
                ),
                param(
                    concat!("band", stringify!($band), "_freq"),
                    concat!("Band ", stringify!($band), " Frequency"),
                    Hz,
                    20.0,
                    20_000.0,
                    $frequency,
                    Log,
                ),
                param(
                    concat!("band", stringify!($band), "_q"),
                    concat!("Band ", stringify!($band), " Q"),
                    ParamUnit::None, 0.1, 20.0, 1.0, Log,
                ),
                param(
                    concat!("band", stringify!($band), "_gain"),
                    concat!("Band ", stringify!($band), " Gain"),
                    Db, -24.0, 24.0, 0.0, Linear,
                ),
                bool_param(
                    concat!("band", stringify!($band), "_dynamic"),
                    concat!("Band ", stringify!($band), " Dynamic"),
                    false,
                ),
                bool_param(
                    concat!("band", stringify!($band), "_sidechain"),
                    concat!("Band ", stringify!($band), " Sidechain"),
                    false,
                ),
                param(
                    concat!("band", stringify!($band), "_threshold"),
                    concat!("Band ", stringify!($band), " Threshold"),
                    Db,
                    -60.0,
                    0.0,
                    -30.0,
                    Linear,
                ),
                param(
                    concat!("band", stringify!($band), "_range"),
                    concat!("Band ", stringify!($band), " Range"),
                    Db, -24.0, 24.0, -6.0, Linear,
                ),
                param(
                    concat!("band", stringify!($band), "_attack"),
                    concat!("Band ", stringify!($band), " Attack"),
                    Ms, 0.1, 200.0, 10.0, Log,
                ),
                param(
                    concat!("band", stringify!($band), "_release"),
                    concat!("Band ", stringify!($band), " Release"),
                    Ms,
                    5.0,
                    2_000.0,
                    100.0,
                    Log,
                ),
            )+
            bool_param("auto_gain", "Auto Gain", false),
            bool_param("oversample", "2x Oversampling", false),
            bool_param("oversample_fir", "2x FIR Oversampling", false),
            param(
                "output_gain",
                "Output Gain",
                Db,
                DYNAMIC_EQ_OUTPUT_GAIN_DB_RANGE.0,
                DYNAMIC_EQ_OUTPUT_GAIN_DB_RANGE.1,
                0.0,
                Linear,
            ),
            bool_param("low_cut_enabled", "Low Cut Enabled", false),
            param(
                "low_cut_freq",
                "Low Cut Frequency",
                Hz,
                20.0,
                20_000.0,
                20.0,
                Log,
            ),
            param(
                "low_cut_q",
                "Low Cut Q",
                ParamUnit::None,
                0.1,
                20.0,
                0.707,
                Log,
            ),
            enum_param("low_cut_slope", "Low Cut Slope", &DYNAMIC_EQ_CUT_SLOPES, 0),
            bool_param("high_cut_enabled", "High Cut Enabled", false),
            param(
                "high_cut_freq",
                "High Cut Frequency",
                Hz,
                20.0,
                20_000.0,
                20_000.0,
                Log,
            ),
            param(
                "high_cut_q",
                "High Cut Q",
                ParamUnit::None,
                0.1,
                20.0,
                0.707,
                Log,
            ),
            enum_param(
                "high_cut_slope",
                "High Cut Slope",
                &DYNAMIC_EQ_CUT_SLOPES,
                0,
            ),
        ];
    };
}

define_bands! {
    1 => 100.0,
    2 => 250.0,
    3 => 500.0,
    4 => 1_000.0,
    5 => 2_000.0,
    6 => 8_000.0,
    7 => 12_000.0,
    8 => 60.0,
    9 => 160.0,
    10 => 750.0,
    11 => 3_500.0,
    12 => 5_500.0,
}
