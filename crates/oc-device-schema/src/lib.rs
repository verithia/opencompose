//! Typed contract for every built-in mixer device.
//!
//! The catalog is the single description of what a control is called, what it
//! ranges over and which inputs a device has; the parser, the renderer, the
//! editor and the agent surface all read it.
//!
//! ```
//! let device = oc_device_schema::find_device("compression").expect("a known device");
//! assert_eq!(device.name, "Compression");
//! assert!(device.params.iter().any(|param| param.id == "threshold"));
//! ```

use serde::Serialize;

pub mod frontend;
pub mod gm;
pub mod synth;
pub mod units;
pub use units::Unit as ParamUnit;

/// Option labels shared by the tuner device and its parameter schema.
pub const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
/// Scale labels the tuner's `scale` parameter indexes.
pub const SCALE_NAMES: [&str; 12] = [
    "Chromatic",
    "Major",
    "Natural Minor",
    "Harmonic Minor",
    "Dorian",
    "Mixolydian",
    "Lydian",
    "Phrygian",
    "Locrian",
    "Major Pentatonic",
    "Minor Pentatonic",
    "Blues",
];
/// Detection modes the tuner's `mode` parameter indexes.
pub const TUNER_MODES: [&str; 2] = ["Full Range", "Low Latency"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// The browser group a device belongs to.
pub enum DeviceCategory {
    Eq,
    Dynamics,
    Time,
    Color,
    Utility,
}

impl DeviceCategory {
    /// Stable category id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Dynamics => "dynamics",
            Self::Time => "time",
            Self::Color => "color",
            Self::Utility => "utility",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// How a normalized 0..1 position maps onto a parameter's range.
///
/// `Log` gives the low end of a frequency or time range more travel, which is
/// what makes a cutoff or a short release adjustable.
pub enum ParamCurve {
    Linear,
    Log,
}

/// How the UI renders a parameter. `Continuous` keeps the legacy JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Continuous,
    Bool,
    Enum,
}

impl ParamKind {
    fn is_continuous(&self) -> bool {
        *self == Self::Continuous
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
/// One control of a device: its key, its range, its unit and how the editor
/// draws it.
pub struct ParamSchema {
    pub id: &'static str,
    pub name: &'static str,
    pub unit: ParamUnit,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub curve: ParamCurve,
    pub steps: Option<u32>,
    #[serde(skip_serializing_if = "ParamKind::is_continuous")]
    pub kind: ParamKind,
    /// Enum labels indexed by value; empty for other kinds.
    #[serde(skip_serializing_if = "options_are_empty")]
    pub options: &'static [&'static str],
    /// Optional UI note; empty for most parameters.
    #[serde(skip_serializing_if = "description_is_empty")]
    pub description: &'static str,
    #[serde(skip)]
    pub aliases: &'static [&'static str],
}

fn options_are_empty(options: &&'static [&'static str]) -> bool {
    options.is_empty()
}

fn description_is_empty(description: &&'static str) -> bool {
    description.is_empty()
}

/// What a declared device port carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// Interleaved stereo audio.
    Audio,
    /// One normalized 0..1 control value per frame. It is never audio.
    Control,
}

/// One declared device port.
///
/// `sums` is the whole silence rule: a port that does not sum is silent by
/// construction, whether it carries an audio key or a control signal. A device
/// that accepts a control signal declares a `control` port; the graph refuses
/// to connect a control edge anywhere else.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PortSchema {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: PortKind,
    /// True when the port's signal reaches the device's output sum.
    pub sums: bool,
}

/// The only input of a device that does not detect an external key.
pub const AUDIO_IN: &[PortSchema] = &[PortSchema {
    id: "in",
    name: "In",
    kind: PortKind::Audio,
    sums: true,
}];

/// An audio key input and no control input.
pub const AUDIO_KEY: &[PortSchema] = &[
    PortSchema {
        id: "in",
        name: "In",
        kind: PortKind::Audio,
        sums: true,
    },
    PortSchema {
        id: "key",
        name: "Key",
        kind: PortKind::Audio,
        sums: false,
    },
];

/// An audio key input plus a control input, both silent by construction.
pub const AUDIO_KEY_AND_CONTROL: &[PortSchema] = &[
    PortSchema {
        id: "in",
        name: "In",
        kind: PortKind::Audio,
        sums: true,
    },
    PortSchema {
        id: "key",
        name: "Key",
        kind: PortKind::Audio,
        sums: false,
    },
    PortSchema {
        id: "control",
        name: "Control",
        kind: PortKind::Control,
        sums: false,
    },
];

#[derive(Debug, Clone, Copy, Serialize)]
/// One device: the keyword a project writes, its parameters and its ports.
pub struct DeviceSchema {
    pub id: &'static str,
    pub name: &'static str,
    pub category: DeviceCategory,
    pub params: &'static [ParamSchema],
    /// Declared ports, main input first. The catalog keeps one entry per
    /// declared port; a device that detects a key or reads a control signal
    /// names those ports here.
    pub ports: &'static [PortSchema],
}

/// Schema version of the serialized device catalog.
pub const DEVICE_CATALOG_VERSION: u32 = 2;

#[derive(Debug, Serialize)]
/// The catalog in the shape the editor and the agent surface serialise.
pub struct DeviceCatalog {
    pub version: u32,
    pub units: &'static [units::UnitDefinition],
    pub devices: &'static [DeviceSchema],
}

use ParamCurve::{Linear, Log};
use ParamUnit::{Db, Fraction, Hz, Ms, Percent, Ratio};

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

/// Channel targets in `ChannelTarget` code order.
const CHANNEL_TARGETS: [&str; 5] = ["Stereo", "Mid", "Side", "Left", "Right"];

const DEESSER_MODES: [&str; 2] = ["Wide Band", "Split Band"];
const DEESSER_THRESHOLD_MODES: [&str; 2] = ["Absolute", "Relative"];
const DEESSER_DETECTORS: [&str; 2] = ["Sidechain", "In-band"];
const DEESSER_BANDS: [&str; 6] = ["Auto", "5 kHz", "6 kHz", "7 kHz", "8 kHz", "9 kHz"];
const DEESSER_ESTIMATION: [&str; 2] = ["Off", "Learn"];
const DEESSER_SEGMENTS: [&str; 2] = ["Off", "On"];
const TUNER_KEYS: [&str; 12] = crate::KEY_NAMES;
const TUNER_SCALES: [&str; 12] = crate::SCALE_NAMES;
const TUNER_MODE_LABELS: [&str; 2] = TUNER_MODES;
const MULTIBAND_COUNTS: [&str; 3] = ["2 Bands", "3 Bands", "4 Bands"];
const MULTIBAND_SLOPES: [&str; 5] = [
    "6 dB/oct",
    "12 dB/oct",
    "24 dB/oct",
    "36 dB/oct",
    "48 dB/oct",
];
const MULTIBAND_PHASES: [&str; 3] = ["Minimum", "Dynamic", "Linear"];
const MULTIBAND_DETECTORS: [&str; 2] = ["Peak", "Programme"];
/// Where one multiband band reads its detector level from.
const MULTIBAND_BAND_DETECTORS: [&str; 2] = ["In band", "Key"];

/// Detector sidechain EQ shapes, in the DSP's filter-type code order.
const SIDECHAIN_EQ_TYPES: [&str; 5] = ["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut"];

const fn param(
    id: &'static str,
    name: &'static str,
    unit: ParamUnit,
    min: f64,
    max: f64,
    default: f64,
    curve: ParamCurve,
) -> ParamSchema {
    ParamSchema {
        id,
        name,
        unit,
        min,
        max,
        default,
        curve,
        steps: None,
        kind: ParamKind::Continuous,
        options: &[],
        description: "",
        aliases: &[],
    }
}

/// A 0/1 switch rendered as a toggle. The DSL accepts `true`/`false` or 0/1.
const fn bool_param(id: &'static str, name: &'static str, default: bool) -> ParamSchema {
    ParamSchema {
        id,
        name,
        unit: ParamUnit::None,
        min: 0.0,
        max: 1.0,
        default: if default { 1.0 } else { 0.0 },
        curve: Linear,
        steps: Some(2),
        kind: ParamKind::Bool,
        options: &[],
        description: "",
        aliases: &[],
    }
}

/// A stepped choice rendered as a select; `default` indexes `options`.
const fn enum_param(
    id: &'static str,
    name: &'static str,
    options: &'static [&'static str],
    default: usize,
) -> ParamSchema {
    ParamSchema {
        id,
        name,
        unit: ParamUnit::None,
        min: 0.0,
        max: options.len() as f64 - 1.0,
        default: default as f64,
        curve: Linear,
        steps: Some(options.len() as u32),
        kind: ParamKind::Enum,
        options,
        description: "",
        aliases: &[],
    }
}

/// A continuous parameter with discrete positions between `min` and `max`.
// Schema constructor mirrors the DSL parameter fields.
#[allow(clippy::too_many_arguments)]
const fn stepped_param(
    id: &'static str,
    name: &'static str,
    unit: ParamUnit,
    min: f64,
    max: f64,
    default: f64,
    steps: u32,
    curve: ParamCurve,
) -> ParamSchema {
    ParamSchema {
        id,
        name,
        unit,
        min,
        max,
        default,
        curve,
        steps: Some(steps),
        kind: ParamKind::Continuous,
        options: &[],
        description: "",
        aliases: &[],
    }
}

const fn with_aliases(mut schema: ParamSchema, aliases: &'static [&'static str]) -> ParamSchema {
    schema.aliases = aliases;
    schema
}

const fn with_description(mut schema: ParamSchema, description: &'static str) -> ParamSchema {
    schema.description = description;
    schema
}

/// Every built-in device, in the order the browser lists them.
pub static DEVICES: &[DeviceSchema] = &[
    DeviceSchema {
        id: "gain",
        name: "Gain",
        category: DeviceCategory::Utility,
        params: &[param(
            "factor",
            "Gain",
            ParamUnit::None,
            0.0,
            16.0,
            1.0,
            Linear,
        )],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "eq",
        name: "Parametric EQ",
        category: DeviceCategory::Eq,
        params: &[
            param("low_freq", "Low Frequency", Hz, 20.0, 20_000.0, 200.0, Log),
            param("low_gain", "Low Gain", Db, -24.0, 24.0, 0.0, Linear),
            with_aliases(
                param(
                    "mid_freq",
                    "Mid Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                &["freq"],
            ),
            with_aliases(
                param("mid_gain", "Mid Gain", Db, -24.0, 24.0, 0.0, Linear),
                &["gain"],
            ),
            param("q", "Mid Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param(
                "high_freq",
                "High Frequency",
                Hz,
                20.0,
                20_000.0,
                8_000.0,
                Log,
            ),
            param("high_gain", "High Gain", Db, -24.0, 24.0, 0.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "passive-eq",
        name: "Passive EQ",
        category: DeviceCategory::Eq,
        params: &[
            with_description(
                param("low_freq", "Low Frequency", Hz, 30.0, 300.0, 100.0, Log),
                "One corner: the boost bell peaks here and the cut shelf turns over here.",
            ),
            with_description(
                param("low_boost", "Low Boost", Db, 0.0, 10.0, 0.0, Linear),
                "Resonant bell at low_freq (Q 1.0); flat at DC and at high frequencies.",
            ),
            with_description(
                param("low_cut", "Low Cut", Db, 0.0, 10.0, 0.0, Linear),
                "Second-order shelf down at low_freq (Q 0.7); -C at DC and about -C/2 at low_freq.",
            ),
            with_description(
                param(
                    "high_freq",
                    "High Frequency",
                    Hz,
                    3_000.0,
                    16_000.0,
                    8_000.0,
                    Log,
                ),
                "Centre of the high boost bell.",
            ),
            with_description(
                param("high_boost", "High Boost", Db, 0.0, 10.0, 0.0, Linear),
                "Bell gain at high_freq.",
            ),
            with_description(
                param("high_q", "High Q", ParamUnit::None, 0.7, 2.0, 1.0, Log),
                "High bell width: 0.7 broad, 2 narrow.",
            ),
            with_description(
                param(
                    "high_cut_freq",
                    "High Cut Frequency",
                    Hz,
                    5_000.0,
                    20_000.0,
                    10_000.0,
                    Log,
                ),
                "Corner of the high cut shelf.",
            ),
            with_description(
                param("high_cut", "High Cut", Db, 0.0, 10.0, 0.0, Linear),
                "High shelf cut; the shelf lies at high_cut_freq.",
            ),
            with_description(
                param("tilt", "Tilt", Db, -6.0, 6.0, 0.0, Linear),
                "First-order tilt around 1 kHz; +G spans -G/2 to +G/2.",
            ),
            with_description(
                param(
                    "air_freq",
                    "Air Frequency",
                    Hz,
                    8_000.0,
                    12_000.0,
                    10_000.0,
                    Log,
                ),
                "Corner of the air shelf.",
            ),
            with_description(
                param("air", "Air", Db, 0.0, 6.0, 0.0, Linear),
                "High shelf after the rest of the chain; applied last.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "dynamic-eq",
        name: "Dynamic EQ",
        category: DeviceCategory::Eq,
        // Six dynamic bands plus low/high cuts; ids match `dynamic_eq.rs`.
        params: &[
            bool_param("band1_enabled", "Band 1 Enabled", false),
            bool_param("band1_solo", "Band 1 Solo", false),
            enum_param("band1_channel", "Band 1 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band1_type", "Band 1 Type", &BAND_TYPES, 0),
            param(
                "band1_freq",
                "Band 1 Frequency",
                Hz,
                20.0,
                20_000.0,
                100.0,
                Log,
            ),
            param("band1_q", "Band 1 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band1_gain", "Band 1 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band1_dynamic", "Band 1 Dynamic", false),
            bool_param("band1_sidechain", "Band 1 Sidechain", false),
            param(
                "band1_threshold",
                "Band 1 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band1_range", "Band 1 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band1_attack", "Band 1 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band1_release",
                "Band 1 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("band2_enabled", "Band 2 Enabled", false),
            bool_param("band2_solo", "Band 2 Solo", false),
            enum_param("band2_channel", "Band 2 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band2_type", "Band 2 Type", &BAND_TYPES, 0),
            param(
                "band2_freq",
                "Band 2 Frequency",
                Hz,
                20.0,
                20_000.0,
                250.0,
                Log,
            ),
            param("band2_q", "Band 2 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band2_gain", "Band 2 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band2_dynamic", "Band 2 Dynamic", false),
            bool_param("band2_sidechain", "Band 2 Sidechain", false),
            param(
                "band2_threshold",
                "Band 2 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band2_range", "Band 2 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band2_attack", "Band 2 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band2_release",
                "Band 2 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("band3_enabled", "Band 3 Enabled", false),
            bool_param("band3_solo", "Band 3 Solo", false),
            enum_param("band3_channel", "Band 3 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band3_type", "Band 3 Type", &BAND_TYPES, 0),
            param(
                "band3_freq",
                "Band 3 Frequency",
                Hz,
                20.0,
                20_000.0,
                500.0,
                Log,
            ),
            param("band3_q", "Band 3 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band3_gain", "Band 3 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band3_dynamic", "Band 3 Dynamic", false),
            bool_param("band3_sidechain", "Band 3 Sidechain", false),
            param(
                "band3_threshold",
                "Band 3 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band3_range", "Band 3 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band3_attack", "Band 3 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band3_release",
                "Band 3 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("band4_enabled", "Band 4 Enabled", false),
            bool_param("band4_solo", "Band 4 Solo", false),
            enum_param("band4_channel", "Band 4 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band4_type", "Band 4 Type", &BAND_TYPES, 0),
            param(
                "band4_freq",
                "Band 4 Frequency",
                Hz,
                20.0,
                20_000.0,
                1_000.0,
                Log,
            ),
            param("band4_q", "Band 4 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band4_gain", "Band 4 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band4_dynamic", "Band 4 Dynamic", false),
            bool_param("band4_sidechain", "Band 4 Sidechain", false),
            param(
                "band4_threshold",
                "Band 4 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band4_range", "Band 4 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band4_attack", "Band 4 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band4_release",
                "Band 4 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("band5_enabled", "Band 5 Enabled", false),
            bool_param("band5_solo", "Band 5 Solo", false),
            enum_param("band5_channel", "Band 5 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band5_type", "Band 5 Type", &BAND_TYPES, 0),
            param(
                "band5_freq",
                "Band 5 Frequency",
                Hz,
                20.0,
                20_000.0,
                2_000.0,
                Log,
            ),
            param("band5_q", "Band 5 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band5_gain", "Band 5 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band5_dynamic", "Band 5 Dynamic", false),
            bool_param("band5_sidechain", "Band 5 Sidechain", false),
            param(
                "band5_threshold",
                "Band 5 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band5_range", "Band 5 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band5_attack", "Band 5 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band5_release",
                "Band 5 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("band6_enabled", "Band 6 Enabled", false),
            bool_param("band6_solo", "Band 6 Solo", false),
            enum_param("band6_channel", "Band 6 Channel", &CHANNEL_TARGETS, 0),
            enum_param("band6_type", "Band 6 Type", &BAND_TYPES, 0),
            param(
                "band6_freq",
                "Band 6 Frequency",
                Hz,
                20.0,
                20_000.0,
                8_000.0,
                Log,
            ),
            param("band6_q", "Band 6 Q", ParamUnit::None, 0.1, 20.0, 1.0, Log),
            param("band6_gain", "Band 6 Gain", Db, -24.0, 24.0, 0.0, Linear),
            bool_param("band6_dynamic", "Band 6 Dynamic", false),
            bool_param("band6_sidechain", "Band 6 Sidechain", false),
            param(
                "band6_threshold",
                "Band 6 Threshold",
                Db,
                -60.0,
                0.0,
                -30.0,
                Linear,
            ),
            param("band6_range", "Band 6 Range", Db, -24.0, 24.0, -6.0, Linear),
            param("band6_attack", "Band 6 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band6_release",
                "Band 6 Release",
                Ms,
                5.0,
                2_000.0,
                100.0,
                Log,
            ),
            bool_param("auto_gain", "Auto Gain", false),
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
        ],
        ports: AUDIO_KEY,
    },
    DeviceSchema {
        id: "compression",
        name: "Compression",
        category: DeviceCategory::Dynamics,
        params: &[
            param("threshold", "Threshold", Db, -60.0, 0.0, -12.0, Linear),
            param("ratio", "Ratio", Ratio, 1.0, 100.0, 4.0, Log),
            param(
                "ratio_plain",
                "Ratio (plain 0-1)",
                ParamUnit::None,
                0.0,
                1.0,
                0.6,
                Linear,
            ),
            param("attack", "Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "attack_plain",
                "Attack (plain 0-1)",
                ParamUnit::None,
                0.0,
                1.0,
                0.32,
                Linear,
            ),
            param("release", "Release", Ms, 10.0, 2_000.0, 100.0, Log),
            param(
                "release_plain",
                "Release (plain 0-1)",
                ParamUnit::None,
                0.0,
                1.0,
                0.44,
                Linear,
            ),
            param("knee", "Knee", Db, 0.0, 72.0, 6.0, Linear),
            param("range", "Range", Db, 0.0, 60.0, 60.0, Linear),
            param("makeup", "Makeup Gain", Db, -12.0, 24.0, 0.0, Linear),
            param("lookahead", "Lookahead", Ms, 0.0, 20.0, 0.0, Linear),
            param(
                "max_lookahead",
                "Maximum Lookahead",
                Ms,
                0.0,
                20.0,
                20.0,
                Linear,
            ),
            param(
                "detector_hpf",
                "Detector HPF (20 = off)",
                Hz,
                20.0,
                500.0,
                20.0,
                Log,
            ),
            param(
                "detector",
                "Detector (accepted; always peak)",
                ParamUnit::None,
                0.0,
                1.0,
                0.0,
                Linear,
            ),
            param(
                "stereo_link",
                "Stereo Link",
                Percent,
                0.0,
                100.0,
                100.0,
                Linear,
            ),
            param("hold", "Hold", Ms, 0.0, 500.0, 0.0, Linear),
            param(
                "auto_release",
                "Auto Release (0/1)",
                ParamUnit::None,
                0.0,
                1.0,
                0.0,
                Linear,
            ),
            param("mix", "Mix", Fraction, 0.0, 1.0, 1.0, Linear),
            param(
                "auto_makeup",
                "Auto Makeup (0/1)",
                ParamUnit::None,
                0.0,
                1.0,
                0.0,
                Linear,
            ),
            param(
                "mode",
                "Mode (0 = legacy, 1 = modern)",
                ParamUnit::None,
                0.0,
                1.0,
                1.0,
                Linear,
            ),
            with_description(
                bool_param("sidechain_eq1_on", "Side Chain EQ 1 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq1_type",
                    "Side Chain EQ 1 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq1_frequency",
                    "Side Chain EQ 1 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq1_gain",
                    "Side Chain EQ 1 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq1_q",
                    "Side Chain EQ 1 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
            with_description(
                bool_param("sidechain_eq2_on", "Side Chain EQ 2 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq2_type",
                    "Side Chain EQ 2 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq2_frequency",
                    "Side Chain EQ 2 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq2_gain",
                    "Side Chain EQ 2 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq2_q",
                    "Side Chain EQ 2 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
            with_description(
                bool_param("sidechain_eq3_on", "Side Chain EQ 3 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq3_type",
                    "Side Chain EQ 3 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq3_frequency",
                    "Side Chain EQ 3 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq3_gain",
                    "Side Chain EQ 3 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq3_q",
                    "Side Chain EQ 3 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
            with_description(
                bool_param("sidechain_eq4_on", "Side Chain EQ 4 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq4_type",
                    "Side Chain EQ 4 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq4_frequency",
                    "Side Chain EQ 4 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq4_gain",
                    "Side Chain EQ 4 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq4_q",
                    "Side Chain EQ 4 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
            with_description(
                bool_param("sidechain_eq5_on", "Side Chain EQ 5 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq5_type",
                    "Side Chain EQ 5 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq5_frequency",
                    "Side Chain EQ 5 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq5_gain",
                    "Side Chain EQ 5 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq5_q",
                    "Side Chain EQ 5 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
            with_description(
                bool_param("sidechain_eq6_on", "Side Chain EQ 6 On", false),
                "Detector-only filter band; off keeps the detector path unchanged.",
            ),
            with_description(
                enum_param(
                    "sidechain_eq6_type",
                    "Side Chain EQ 6 Type",
                    &SIDECHAIN_EQ_TYPES,
                    0,
                ),
                "Filter shape applied to the detector signal.",
            ),
            with_description(
                param(
                    "sidechain_eq6_frequency",
                    "Side Chain EQ 6 Frequency",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Corner or centre frequency of this detector band.",
            ),
            with_description(
                param(
                    "sidechain_eq6_gain",
                    "Side Chain EQ 6 Gain",
                    Db,
                    -30.0,
                    30.0,
                    0.0,
                    Linear,
                ),
                "Boost or cut on the detector signal only; the audio path is untouched.",
            ),
            with_description(
                param(
                    "sidechain_eq6_q",
                    "Side Chain EQ 6 Q",
                    ParamUnit::None,
                    0.1,
                    10.0,
                    0.707,
                    Log,
                ),
                "Resonance of a bell or the knee of a cut.",
            ),
        ],
        ports: AUDIO_KEY_AND_CONTROL,
    },
    DeviceSchema {
        id: "leveler",
        name: "Leveler",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("target", "Target", Db, -40.0, 0.0, -20.0, Linear),
                "K-weighted short-term level the rider aims for.",
            ),
            with_description(
                param(
                    "speed",
                    "Speed (dB/s)",
                    ParamUnit::None,
                    2.0,
                    6.0,
                    3.0,
                    Linear,
                ),
                "Gain slew limit; faster sounds like compression.",
            ),
            param("range", "Range", Db, 12.0, 24.0, 18.0, Linear),
            with_description(
                param("window", "Window", Ms, 500.0, 3_000.0, 1_000.0, Log),
                "Short-term loudness integration window.",
            ),
            with_description(
                param("gate", "Gate", Db, -70.0, -20.0, -45.0, Linear),
                "Below this block level the gain holds, then returns to unity.",
            ),
            with_description(
                param(
                    "gate_release",
                    "Gate Release",
                    Ms,
                    1_000.0,
                    2_000.0,
                    1_500.0,
                    Log,
                ),
                "Return-to-unity time constant after the 300 ms hold.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "opto",
        name: "Opto Compressor",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param(
                    "peak_reduction",
                    "Peak Reduction",
                    Db,
                    0.0,
                    20.0,
                    6.0,
                    Linear,
                ),
                "Sidechain drive into the cell; the ratio is fixed by the cell.",
            ),
            with_description(
                param("release", "Release", Ms, 500.0, 5_000.0, 1_500.0, Log),
                "Slow release state; the fast state is fixed at 60 ms.",
            ),
            param("makeup", "Makeup Gain", Db, -12.0, 12.0, 0.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "fet",
        name: "FET Compressor",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("drive", "Input Drive", Db, 0.0, 24.0, 0.0, Linear),
                "Drive into the channel; the bias sets where the channel sits on its law.",
            ),
            with_description(
                param("attack", "Attack", Ms, 0.05, 20.0, 0.3, Log),
                "Peak detector attack; the FET's fast attack is part of the character.",
            ),
            with_description(
                param("release", "Release", Ms, 20.0, 2_000.0, 120.0, Log),
                "Peak detector release.",
            ),
            param("output", "Output", Db, -12.0, 12.0, 0.0, Linear),
            with_description(
                param("bias", "Bias", Percent, 0.0, 100.0, 25.0, Linear),
                "Quiescent bias; 100% is nearest pinch-off (all buttons) and raises the ratio.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "de-esser",
        name: "De-esser",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("threshold", "Threshold", Db, -60.0, 0.0, -36.0, Linear),
                "Band-relative peak threshold; the detector reads the sidechain band, not the input.",
            ),
            with_description(
                enum_param(
                    "threshold_mode",
                    "Threshold Mode",
                    &DEESSER_THRESHOLD_MODES,
                    0,
                ),
                "Absolute compares the band peak with Threshold; Relative moves the threshold with the slow program level.",
            ),
            with_description(
                param(
                    "relative_threshold",
                    "Relative Threshold",
                    Db,
                    -36.0,
                    0.0,
                    -12.0,
                    Linear,
                ),
                "Relative mode: the band peak must exceed the program level by this much; the threshold tracks the program 1:1.",
            ),
            with_description(
                param("range", "Range", Db, 0.0, 24.0, 6.0, Linear),
                "Gain-law slope and ceiling: GR = clamp((Range/36)*(L - Threshold), 0, Range).",
            ),
            with_description(
                param("attack", "Attack", Ms, 0.1, 50.0, 11.0, Log),
                "63% settling time of the fixed four-pole gain-reduction ballistic.",
            ),
            with_description(
                param("release", "Release", Ms, 5.0, 1_000.0, 15.5, Log),
                "63% release time of the fixed four-pole gain-reduction ballistic.",
            ),
            with_description(
                param(
                    "high_frequency",
                    "High Frequency",
                    Hz,
                    2_000.0,
                    20_000.0,
                    7_500.0,
                    Log,
                ),
                "Sidechain high-pass corner (24 dB/oct Butterworth); also places the split at HP/4.",
            ),
            with_description(
                param(
                    "low_frequency",
                    "Low Frequency",
                    Hz,
                    2_000.0,
                    20_000.0,
                    14_000.0,
                    Log,
                ),
                "Sidechain low-pass corner (24 dB/oct Butterworth).",
            ),
            with_description(
                enum_param("sibilance_band", "Sibilance Band", &DEESSER_BANDS, 0),
                "Auto uses the HP/LP corners and the split at HP/4; a step sets HP, LP = 1.87*HP and the split at the band.",
            ),
            with_description(
                enum_param("band_estimation", "Band Estimation", &DEESSER_ESTIMATION, 0),
                "Learn tracks the take's sibilant centroid and moves HP, LP and the split there; until a band is found the device keeps the default and does not act.",
            ),
            with_description(
                param(
                    "band_override",
                    "Band Override",
                    Hz,
                    0.0,
                    20_000.0,
                    0.0,
                    Linear,
                ),
                "Pins the sidechain high-pass and split corner in Hz, overriding the band steps and the estimator; 0 follows them.",
            ),
            with_description(
                enum_param(
                    "segment_detection",
                    "Segment Detection",
                    &DEESSER_SEGMENTS,
                    0,
                ),
                "On gates the reduction to detected sibilant segments; a sustained sibilant stays engaged.",
            ),
            with_description(
                enum_param("band_processing", "Band Processing", &DEESSER_MODES, 0),
                "Wide scales the full band; Split scales only the zero-phase x^2/(1+x^2) high band.",
            ),
            with_description(
                enum_param("detector", "Detector", &DEESSER_DETECTORS, 0),
                "Sidechain detects the band-limited program before the split; In-band detects the band after the split.",
            ),
            with_description(
                param(
                    "stereo_link",
                    "Stereo Link",
                    Percent,
                    0.0,
                    100.0,
                    100.0,
                    Linear,
                ),
                "0% independent; 100% computes both reductions from the mid signal (L+R)/2.",
            ),
            with_description(
                param("lookahead", "Lookahead", Ms, 0.0, 15.0, 0.0, Linear),
                "Above 0 enables a fixed 15 ms line; the knob sets detector anticipation, not audio delay.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "breath-split",
        name: "Breath Split",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("breath_level", "Breath Level", Db, -60.0, 12.0, 0.0, Linear),
                "Gain on the breath path in windows the detector classifies as breath.",
            ),
            with_description(
                param("voice_level", "Voice Level", Db, -60.0, 12.0, 0.0, Linear),
                "Gain on the voiced (low) path at all times.",
            ),
            with_description(
                param(
                    "threshold",
                    "Threshold",
                    ParamUnit::None,
                    0.0,
                    1.0,
                    0.5,
                    Linear,
                ),
                "Breath confidence (0-1) a window must exceed; raise it to treat fewer windows as breath.",
            ),
            with_description(
                param("smoothing", "Smoothing", Ms, 1.0, 200.0, 20.0, Log),
                "Time constant of the breath-path gain; long enough not to click.",
            ),
            with_description(
                bool_param("monitor", "Monitor Breath", false),
                "Listen-only: output the breath path so the split and detector can be set up.",
            ),
            with_description(
                param(
                    "split_frequency",
                    "Split Frequency",
                    Hz,
                    1_500.0,
                    6_000.0,
                    2_000.0,
                    Log,
                ),
                "Low-pass corner; the breath path is the exact complement above it.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "plosive",
        name: "Plosive",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("amount", "Amount", Percent, 0.0, 100.0, 0.0, Linear),
                "Depth of the low shelf; 100 % removes 12 dB at DC, 0 % is a bit-exact bypass.",
            ),
            with_description(
                param("threshold", "Threshold", Db, 2.0, 24.0, 6.0, Linear),
                "Rise over the slow envelope a burst needs before the shelf engages.",
            ),
            with_description(
                param("frequency", "Frequency", Hz, 80.0, 150.0, 120.0, Log),
                "Low-shelf corner; the detector reads a 24 dB/oct low band at the same corner.",
            ),
            with_description(
                param("release", "Release", Ms, 10.0, 300.0, 40.0, Log),
                "Time for the shelf to fall 63 % of the way back to unity after the burst.",
            ),
            with_description(
                bool_param("monitor", "Monitor Removed", false),
                "Listen-only: output the low band the shelf is removing so the detector can be set up.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "multiband",
        name: "Multiband Compander",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                enum_param("band_count", "Band Count", &MULTIBAND_COUNTS, 0),
                "Two to four bands; the active splits are the first N-1 crossovers.",
            ),
            with_description(
                enum_param("slope", "Slope", &MULTIBAND_SLOPES, 2),
                "Butterworth/Linkwitz-Riley slope of every split.",
            ),
            with_description(
                enum_param("phase_mode", "Phase Mode", &MULTIBAND_PHASES, 0),
                "Minimum: IIR split, zero latency. Dynamic: IIR plus a program-dependent alignment allpass. Linear: FFT split, linear phase, 1024 frames of latency at 48 kHz.",
            ),
            with_description(
                param(
                    "crossover_1",
                    "Crossover 1",
                    Hz,
                    20.0,
                    20_000.0,
                    1_000.0,
                    Log,
                ),
                "Lowest split frequency.",
            ),
            with_description(
                param(
                    "crossover_2",
                    "Crossover 2",
                    Hz,
                    20.0,
                    20_000.0,
                    4_000.0,
                    Log,
                ),
                "Second split frequency; used by three and four bands.",
            ),
            with_description(
                param(
                    "crossover_3",
                    "Crossover 3",
                    Hz,
                    20.0,
                    20_000.0,
                    10_000.0,
                    Log,
                ),
                "Third split frequency; used by four bands.",
            ),
            with_description(
                param("mix", "Mix", Fraction, 0.0, 1.0, 1.0, Linear),
                "Dry/wet; the dry path is delay-aligned with the wet path.",
            ),
            with_description(
                param("lookahead", "Lookahead", Ms, 0.0, 20.0, 0.0, Linear),
                "Shared audio delay in front of the bands and the reported latency; 0 keeps every band immediate.",
            ),
            with_description(
                bool_param("band1_enabled", "Band 1 Enabled", true),
                "Off bypasses this band's dynamic gain change; the band still sums into the output.",
            ),
            with_description(
                param(
                    "band1_threshold",
                    "Band 1 Threshold",
                    Db,
                    -60.0,
                    0.0,
                    0.0,
                    Linear,
                ),
                "Compander pivot; the gain law is zero at threshold.",
            ),
            with_description(
                param("band1_range", "Band 1 Range", Db, -60.0, 60.0, 0.0, Linear),
                "Bipolar clamp on the gain change. +N allows +-N dB (boost quiet, cut loud); -N cuts only, at most N dB; 0 is neutral.",
            ),
            with_description(
                param("band1_ratio", "Band 1 Ratio", Ratio, 1.0, 20.0, 1.0, Log),
                "Gain-law slope below and above threshold; 1:1 is neutral.",
            ),
            with_description(
                param("band1_attack", "Band 1 Attack", Ms, 0.1, 200.0, 10.0, Log),
                "63% time of the fast ballistic stage.",
            ),
            with_description(
                param(
                    "band1_release",
                    "Band 1 Release",
                    Ms,
                    10.0,
                    2_000.0,
                    100.0,
                    Log,
                ),
                "63% time of the fast release stage; the slow stage is 3x longer.",
            ),
            with_description(
                param("band1_knee", "Band 1 Knee", Db, 0.0, 24.0, 6.0, Linear),
                "Soft-knee width around threshold; 0 is a hard corner.",
            ),
            with_description(
                param(
                    "band1_makeup",
                    "Band 1 Makeup",
                    Db,
                    -12.0,
                    24.0,
                    0.0,
                    Linear,
                ),
                "Static gain added after the clamped gain change.",
            ),
            with_description(
                param(
                    "band1_lookahead",
                    "Band 1 Lookahead",
                    Ms,
                    0.0,
                    20.0,
                    0.0,
                    Linear,
                ),
                "Detector anticipation inside the shared Lookahead line; clamped to Lookahead.",
            ),
            with_description(
                bool_param("band1_solo", "Band 1 Solo", false),
                "Solo drops the other active bands from the sum; this band keeps its gain law.",
            ),
            with_description(
                bool_param("band1_mute", "Band 1 Mute", false),
                "Mute drops this band from the sum; mute wins over solo.",
            ),
            with_description(
                enum_param(
                    "band1_detector",
                    "Band 1 Detector",
                    &MULTIBAND_BAND_DETECTORS,
                    0,
                ),
                "In band follows the split signal; Key ducks this band from the audio key input, leaving the other bands untouched.",
            ),
            with_description(
                bool_param("band2_enabled", "Band 2 Enabled", true),
                "Off bypasses this band's dynamic gain change; the band still sums into the output.",
            ),
            param(
                "band2_threshold",
                "Band 2 Threshold",
                Db,
                -60.0,
                0.0,
                0.0,
                Linear,
            ),
            with_description(
                param("band2_range", "Band 2 Range", Db, -60.0, 60.0, 0.0, Linear),
                "Bipolar clamp on the gain change. +N allows +-N dB (boost quiet, cut loud); -N cuts only, at most N dB; 0 is neutral.",
            ),
            param("band2_ratio", "Band 2 Ratio", Ratio, 1.0, 20.0, 1.0, Log),
            param("band2_attack", "Band 2 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band2_release",
                "Band 2 Release",
                Ms,
                10.0,
                2_000.0,
                100.0,
                Log,
            ),
            param("band2_knee", "Band 2 Knee", Db, 0.0, 24.0, 6.0, Linear),
            param(
                "band2_makeup",
                "Band 2 Makeup",
                Db,
                -12.0,
                24.0,
                0.0,
                Linear,
            ),
            param(
                "band2_lookahead",
                "Band 2 Lookahead",
                Ms,
                0.0,
                20.0,
                0.0,
                Linear,
            ),
            bool_param("band2_solo", "Band 2 Solo", false),
            bool_param("band2_mute", "Band 2 Mute", false),
            with_description(
                enum_param(
                    "band2_detector",
                    "Band 2 Detector",
                    &MULTIBAND_BAND_DETECTORS,
                    0,
                ),
                "In band follows the split signal; Key ducks this band from the audio key input, leaving the other bands untouched.",
            ),
            with_description(
                bool_param("band3_enabled", "Band 3 Enabled", true),
                "Off bypasses this band's dynamic gain change; the band still sums into the output.",
            ),
            param(
                "band3_threshold",
                "Band 3 Threshold",
                Db,
                -60.0,
                0.0,
                0.0,
                Linear,
            ),
            with_description(
                param("band3_range", "Band 3 Range", Db, -60.0, 60.0, 0.0, Linear),
                "Bipolar clamp on the gain change. +N allows +-N dB (boost quiet, cut loud); -N cuts only, at most N dB; 0 is neutral.",
            ),
            param("band3_ratio", "Band 3 Ratio", Ratio, 1.0, 20.0, 1.0, Log),
            param("band3_attack", "Band 3 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band3_release",
                "Band 3 Release",
                Ms,
                10.0,
                2_000.0,
                100.0,
                Log,
            ),
            param("band3_knee", "Band 3 Knee", Db, 0.0, 24.0, 6.0, Linear),
            param(
                "band3_makeup",
                "Band 3 Makeup",
                Db,
                -12.0,
                24.0,
                0.0,
                Linear,
            ),
            param(
                "band3_lookahead",
                "Band 3 Lookahead",
                Ms,
                0.0,
                20.0,
                0.0,
                Linear,
            ),
            bool_param("band3_solo", "Band 3 Solo", false),
            bool_param("band3_mute", "Band 3 Mute", false),
            with_description(
                enum_param(
                    "band3_detector",
                    "Band 3 Detector",
                    &MULTIBAND_BAND_DETECTORS,
                    0,
                ),
                "In band follows the split signal; Key ducks this band from the audio key input, leaving the other bands untouched.",
            ),
            with_description(
                bool_param("band4_enabled", "Band 4 Enabled", true),
                "Off bypasses this band's dynamic gain change; the band still sums into the output.",
            ),
            param(
                "band4_threshold",
                "Band 4 Threshold",
                Db,
                -60.0,
                0.0,
                0.0,
                Linear,
            ),
            with_description(
                param("band4_range", "Band 4 Range", Db, -60.0, 60.0, 0.0, Linear),
                "Bipolar clamp on the gain change. +N allows +-N dB (boost quiet, cut loud); -N cuts only, at most N dB; 0 is neutral.",
            ),
            param("band4_ratio", "Band 4 Ratio", Ratio, 1.0, 20.0, 1.0, Log),
            param("band4_attack", "Band 4 Attack", Ms, 0.1, 200.0, 10.0, Log),
            param(
                "band4_release",
                "Band 4 Release",
                Ms,
                10.0,
                2_000.0,
                100.0,
                Log,
            ),
            param("band4_knee", "Band 4 Knee", Db, 0.0, 24.0, 6.0, Linear),
            param(
                "band4_makeup",
                "Band 4 Makeup",
                Db,
                -12.0,
                24.0,
                0.0,
                Linear,
            ),
            param(
                "band4_lookahead",
                "Band 4 Lookahead",
                Ms,
                0.0,
                20.0,
                0.0,
                Linear,
            ),
            bool_param("band4_solo", "Band 4 Solo", false),
            bool_param("band4_mute", "Band 4 Mute", false),
            with_description(
                enum_param(
                    "band4_detector",
                    "Band 4 Detector",
                    &MULTIBAND_BAND_DETECTORS,
                    0,
                ),
                "In band follows the split signal; Key ducks this band from the audio key input, leaving the other bands untouched.",
            ),
            with_description(
                enum_param("detector", "Detector", &MULTIBAND_DETECTORS, 0),
                "Peak: one stereo-linked peak follower on every band, the fixed two-stage ballistic. Programme: a fast stage delivers the reduction the attack has not reached yet and releases in tens of milliseconds, so a transient is reduced at its onset while a sustained level settles on the same law.",
            ),
        ],
        ports: AUDIO_KEY,
    },
    DeviceSchema {
        id: "limiter",
        name: "Limiter",
        category: DeviceCategory::Dynamics,
        params: &[
            param("ceiling", "Ceiling", Db, -12.0, 0.0, -0.3, Linear),
            param("release", "Release", Ms, 5.0, 500.0, 50.0, Log),
            enum_param(
                "release_mode",
                "Release Mode",
                &["Fast", "Auto", "Punch", "Sustain"],
                1,
            ),
            param("lookahead", "Lookahead", Ms, 0.0, 20.0, 5.0, Linear),
            bool_param("true_peak", "True Peak", true),
            enum_param("oversampling", "Oversampling", &["4x", "8x", "16x"], 0),
            // 100 % is the linked detector; 0 % limits each channel on its own.
            param("link", "Link", Percent, 0.0, 100.0, 100.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "reverb",
        name: "Reverb",
        category: DeviceCategory::Time,
        params: &[
            param("decay", "Decay", Ms, 10.0, 60_000.0, 2_000.0, Log),
            param("predelay", "Pre-delay", Ms, 0.0, 300.0, 0.0, Linear),
            param("damping", "Damping", Percent, 0.0, 100.0, 30.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 30.0, Linear),
            // Long-form controls; their defaults are the legacy signal path.
            param("modulation", "Modulation", Percent, 0.0, 100.0, 0.0, Linear),
            param(
                "early_late",
                "Early / Late",
                Percent,
                0.0,
                100.0,
                0.0,
                Linear,
            ),
            param(
                "stereo_width",
                "Stereo Width",
                Percent,
                0.0,
                200.0,
                100.0,
                Linear,
            ),
            param("low_cut", "Low Cut", Hz, 20.0, 2_000.0, 20.0, Log),
            param("high_cut", "High Cut", Hz, 2_000.0, 20_000.0, 20_000.0, Log),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "reverb-fdn",
        name: "FDN Reverb",
        category: DeviceCategory::Time,
        params: &[
            param("decay", "Decay", Ms, 200.0, 10_000.0, 2_500.0, Log),
            param("space", "Space", Percent, 0.0, 100.0, 50.0, Linear),
            param(
                "decay_rate",
                "Decay Rate",
                Percent,
                50.0,
                200.0,
                100.0,
                Linear,
            ),
            param(
                "brightness",
                "Brightness",
                Percent,
                0.0,
                100.0,
                50.0,
                Linear,
            ),
            param("character", "Character", Percent, 0.0, 100.0, 30.0, Linear),
            param("distance", "Distance", Percent, 0.0, 100.0, 50.0, Linear),
            param("predelay", "Pre-delay", Ms, 0.0, 500.0, 0.0, Linear),
            param(
                "stereo_width",
                "Stereo Width",
                Percent,
                0.0,
                120.0,
                100.0,
                Linear,
            ),
            param("mix", "Mix", Percent, 0.0, 100.0, 30.0, Linear),
            // Per-band decay trims in dB, applied to the measured RT30 breakpoints.
            param("decay_band1", "Decay Band 1", Db, -24.0, 12.0, 0.0, Linear),
            param("decay_band2", "Decay Band 2", Db, -24.0, 12.0, 0.0, Linear),
            param("decay_band3", "Decay Band 3", Db, -24.0, 12.0, 0.0, Linear),
            param("decay_band4", "Decay Band 4", Db, -24.0, 12.0, 0.0, Linear),
            param("decay_band5", "Decay Band 5", Db, -24.0, 12.0, 0.0, Linear),
            param("decay_band6", "Decay Band 6", Db, -24.0, 12.0, 0.0, Linear),
            // Wet post EQ: six bands, flat at the default gain of 0 dB.
            param(
                "post_eq1_freq",
                "Post EQ 1 Frequency",
                Hz,
                20.0,
                20_000.0,
                100.0,
                Log,
            ),
            param(
                "post_eq1_gain",
                "Post EQ 1 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq1_q",
                "Post EQ 1 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq1_shape",
                "Post EQ 1 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            param(
                "post_eq2_freq",
                "Post EQ 2 Frequency",
                Hz,
                20.0,
                20_000.0,
                300.0,
                Log,
            ),
            param(
                "post_eq2_gain",
                "Post EQ 2 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq2_q",
                "Post EQ 2 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq2_shape",
                "Post EQ 2 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            param(
                "post_eq3_freq",
                "Post EQ 3 Frequency",
                Hz,
                20.0,
                20_000.0,
                1_000.0,
                Log,
            ),
            param(
                "post_eq3_gain",
                "Post EQ 3 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq3_q",
                "Post EQ 3 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq3_shape",
                "Post EQ 3 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            param(
                "post_eq4_freq",
                "Post EQ 4 Frequency",
                Hz,
                20.0,
                20_000.0,
                3_000.0,
                Log,
            ),
            param(
                "post_eq4_gain",
                "Post EQ 4 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq4_q",
                "Post EQ 4 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq4_shape",
                "Post EQ 4 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            param(
                "post_eq5_freq",
                "Post EQ 5 Frequency",
                Hz,
                20.0,
                20_000.0,
                8_000.0,
                Log,
            ),
            param(
                "post_eq5_gain",
                "Post EQ 5 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq5_q",
                "Post EQ 5 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq5_shape",
                "Post EQ 5 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            param(
                "post_eq6_freq",
                "Post EQ 6 Frequency",
                Hz,
                20.0,
                20_000.0,
                16_000.0,
                Log,
            ),
            param(
                "post_eq6_gain",
                "Post EQ 6 Gain",
                Db,
                -30.0,
                30.0,
                0.0,
                Linear,
            ),
            param(
                "post_eq6_q",
                "Post EQ 6 Q",
                ParamUnit::None,
                0.05,
                20.0,
                1.0,
                Log,
            ),
            enum_param(
                "post_eq6_shape",
                "Post EQ 6 Shape",
                &[
                    "Bell",
                    "Low Shelf",
                    "High Shelf",
                    "Low Cut",
                    "High Cut",
                    "Notch",
                ],
                0,
            ),
            // Compatibility aliases whose old meaning maps onto a long-form control.
            param("damping", "Damping", Percent, 0.0, 100.0, 50.0, Linear),
            param(
                "modulation",
                "Modulation",
                Percent,
                0.0,
                100.0,
                30.0,
                Linear,
            ),
            param(
                "early_late",
                "Early / Late",
                Percent,
                0.0,
                100.0,
                50.0,
                Linear,
            ),
            // Decay-EQ placement: frequency, Q and shape for each band trim.
            param(
                "decay_band1_freq",
                "Decay Band 1 Frequency",
                Hz,
                20.0,
                20_000.0,
                125.0,
                Log,
            ),
            param(
                "decay_band1_q",
                "Decay Band 1 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band1_shape",
                "Decay Band 1 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
            param(
                "decay_band2_freq",
                "Decay Band 2 Frequency",
                Hz,
                20.0,
                20_000.0,
                1_000.0,
                Log,
            ),
            param(
                "decay_band2_q",
                "Decay Band 2 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band2_shape",
                "Decay Band 2 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
            param(
                "decay_band3_freq",
                "Decay Band 3 Frequency",
                Hz,
                20.0,
                20_000.0,
                2_000.0,
                Log,
            ),
            param(
                "decay_band3_q",
                "Decay Band 3 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band3_shape",
                "Decay Band 3 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
            param(
                "decay_band4_freq",
                "Decay Band 4 Frequency",
                Hz,
                20.0,
                20_000.0,
                4_000.0,
                Log,
            ),
            param(
                "decay_band4_q",
                "Decay Band 4 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band4_shape",
                "Decay Band 4 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
            param(
                "decay_band5_freq",
                "Decay Band 5 Frequency",
                Hz,
                20.0,
                20_000.0,
                8_000.0,
                Log,
            ),
            param(
                "decay_band5_q",
                "Decay Band 5 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band5_shape",
                "Decay Band 5 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
            param(
                "decay_band6_freq",
                "Decay Band 6 Frequency",
                Hz,
                20.0,
                20_000.0,
                16_000.0,
                Log,
            ),
            param(
                "decay_band6_q",
                "Decay Band 6 Q",
                ParamUnit::None,
                0.1,
                10.0,
                0.707,
                Log,
            ),
            enum_param(
                "decay_band6_shape",
                "Decay Band 6 Shape",
                &["None", "Low Shelf", "Bell", "High Shelf"],
                0,
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "delay",
        name: "Delay",
        category: DeviceCategory::Time,
        params: &[
            with_aliases(
                param("time_l", "Left Time", Ms, 1.0, 2_000.0, 375.0, Log),
                &["time"],
            ),
            param("time_r", "Right Time", Ms, 1.0, 2_000.0, 250.0, Log),
            param("feedback", "Feedback", Percent, 0.0, 95.0, 35.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 30.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "chorus",
        name: "Chorus",
        category: DeviceCategory::Time,
        params: &[
            param("rate", "Rate", Hz, 0.1, 10.0, 1.5, Log),
            param("depth", "Depth", Ms, 0.1, 20.0, 3.0, Log),
            param("mix", "Mix", Percent, 0.0, 100.0, 50.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "saturator",
        name: "Saturator",
        category: DeviceCategory::Color,
        params: &[
            param("drive", "Drive", Percent, 0.0, 100.0, 0.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 100.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "iron",
        name: "Iron",
        category: DeviceCategory::Color,
        params: &[
            with_description(
                param("drive", "Drive", Db, 0.0, 24.0, 4.0, Linear),
                "Shaper and transformer-core drive; 0 dB bypasses the colour path.",
            ),
            with_description(
                param("bias", "Bias", ParamUnit::None, -0.5, 0.5, 0.0, Linear),
                "Asymmetry before the shaper; sets the second-order balance.",
            ),
            with_description(
                param("ceiling", "Ceiling", Percent, 0.0, 100.0, 0.0, Linear),
                "One-sided soft ceiling on the positive half; scales with drive.",
            ),
            with_description(
                param("low_freq", "Low Limit", Hz, 20.0, 120.0, 25.0, Log),
                "Second-order high-pass on the colour (Q 1.0).",
            ),
            with_description(
                param(
                    "high_freq",
                    "High Limit",
                    Hz,
                    8_000.0,
                    20_000.0,
                    18_000.0,
                    Log,
                ),
                "Second-order low-pass on the colour (Q 0.55).",
            ),
            with_description(
                param("output", "Output", Db, -12.0, 12.0, 0.0, Linear),
                "Output trim after the colour path.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "doubler",
        name: "Doubler",
        category: DeviceCategory::Time,
        params: &[
            with_description(
                stepped_param(
                    "voices",
                    "Voices",
                    ParamUnit::None,
                    2.0,
                    4.0,
                    2.0,
                    3,
                    Linear,
                ),
                "Two to four short delay voices, each with its own LFO.",
            ),
            with_description(
                param("delay", "Delay", Ms, 8.0, 30.0, 15.0, Log),
                "Centre of the per-voice delay spread (voices sit +-4 ms around it).",
            ),
            with_description(
                param("rate", "Rate", Hz, 0.2, 0.5, 0.3, Log),
                "LFO rate of voice one; the other voices are scaled around it.",
            ),
            with_description(
                param("depth", "Depth", Ms, 1.0, 3.0, 2.0, Log),
                "Peak delay modulation of voice one (1-3 ms).",
            ),
            with_description(
                param(
                    "detune",
                    "Detune (cents)",
                    ParamUnit::None,
                    0.0,
                    20.0,
                    0.0,
                    Linear,
                ),
                "Static read-rate detune, alternating sign across the voices.",
            ),
            with_description(
                param("focus", "Focus", Hz, 200.0, 400.0, 300.0, Log),
                "High-pass before the taps; below it the wet path stays mono.",
            ),
            param("mix", "Mix", Percent, 0.0, 100.0, 50.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "exciter",
        name: "Air Exciter",
        category: DeviceCategory::Color,
        params: &[
            with_description(
                param("amount", "Amount", Percent, 0.0, 100.0, 50.0, Linear),
                "Level of the synthesised air band; zero is exact silence.",
            ),
            with_description(
                param("drive", "Drive", Db, 0.0, 24.0, 6.0, Linear),
                "Drive into the harmonic law; zero is exact silence.",
            ),
            with_description(
                param("character", "Character", Percent, 0.0, 100.0, 0.0, Linear),
                "Even-to-odd blend: 0 is the alias-safe square law, 100 the oversampled cubic law.",
            ),
            with_description(
                param("low_freq", "Low Band", Hz, 9_000.0, 12_000.0, 9_500.0, Log),
                "Fourth-order high-pass on the synthesised band.",
            ),
            with_description(
                param(
                    "high_freq",
                    "High Band",
                    Hz,
                    14_000.0,
                    20_000.0,
                    16_000.0,
                    Log,
                ),
                "Fourth-order low-pass on the synthesised band.",
            ),
            param("output", "Output", Db, -12.0, 12.0, 0.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "ducked-send",
        name: "Ducked Send",
        category: DeviceCategory::Time,
        params: &[
            with_description(
                param("predelay", "Pre-delay", Ms, 20.0, 40.0, 30.0, Linear),
                "Wet-only delay on the return, giving the duck time to engage before the onset.",
            ),
            with_description(
                param("depth", "Depth", Db, 0.0, 24.0, 9.0, Linear),
                "Return attenuation while the dry key is above the threshold.",
            ),
            with_description(
                param("attack", "Attack", Ms, 5.0, 100.0, 20.0, Log),
                "Time from key to 95% of the duck depth.",
            ),
            with_description(
                param("release", "Release", Ms, 100.0, 1_000.0, 320.0, Log),
                "Time from key release to 95% of the return.",
            ),
            with_description(
                param("threshold", "Threshold", Db, -60.0, 0.0, -45.0, Linear),
                "Key level below which the return stays open.",
            ),
            with_description(
                param("width", "Width", Percent, 0.0, 200.0, 120.0, Linear),
                "Mid/side width of the return only; the dry is untouched.",
            ),
        ],
        ports: AUDIO_KEY,
    },
    DeviceSchema {
        id: "resonance",
        name: "Resonance Suppressor",
        category: DeviceCategory::Dynamics,
        params: &[
            with_description(
                param("threshold", "Threshold", Db, -12.0, 18.0, 6.0, Linear),
                "Prominence above the local envelope needed before a band is reduced.",
            ),
            with_description(
                param("depth", "Depth", Db, 0.0, 24.0, 9.0, Linear),
                "Maximum reduction of a band; zero is a bit-exact bypass.",
            ),
            with_description(
                param(
                    "selectivity",
                    "Selectivity",
                    Percent,
                    0.0,
                    100.0,
                    35.0,
                    Linear,
                ),
                "Band-count proxy: 12 broad bands at 0 %, 52 narrow bands at 100 %; 26 is one-third octave.",
            ),
            with_description(
                param("attack", "Attack", Ms, 1.0, 100.0, 10.0, Log),
                "Time for the reduction to reach 63 % of a new, deeper target.",
            ),
            with_description(
                param("release", "Release", Ms, 20.0, 1_000.0, 150.0, Log),
                "Time for the reduction to fall 63 % of the way back to zero.",
            ),
            param("mix", "Mix", Percent, 0.0, 100.0, 100.0, Linear),
            with_description(
                bool_param("enabled", "Enabled", true),
                "Off is a bit-exact bypass with zero reported latency.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "tuner",
        name: "Tuner",
        category: DeviceCategory::Utility,
        params: &[
            with_description(
                param("amount", "Amount", Percent, 0.0, 100.0, 100.0, Linear),
                "Correction depth; 0 % is a bit-exact bypass with zero reported latency.",
            ),
            with_description(
                param("retune", "Retune", Ms, 10.0, 300.0, 50.0, Log),
                "Time constant of the correction one-pole; longer keeps more vibrato.",
            ),
            with_description(
                enum_param("key", "Key", &TUNER_KEYS, 0),
                "Root of the scale the note snap uses.",
            ),
            with_description(
                enum_param("scale", "Scale", &TUNER_SCALES, 0),
                "Chromatic is the no-assumption set; a note-level snap keeps vibrato intact.",
            ),
            with_description(
                enum_param("mode", "Mode", &TUNER_MODE_LABELS, 0),
                "Full Range detects 80 Hz and reports 40 ms; Low Latency starts at 120 Hz and reports 20 ms.",
            ),
            with_description(
                bool_param("enabled", "Enabled", true),
                "Off is a bit-exact bypass with zero reported latency.",
            ),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "distortion",
        name: "Distortion",
        category: DeviceCategory::Color,
        params: &[
            param("drive", "Drive", Percent, 0.0, 100.0, 50.0, Linear),
            param("tone", "Tone", Percent, 0.0, 100.0, 50.0, Linear),
            param("output", "Output", Db, -24.0, 24.0, 0.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 100.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "flanger",
        name: "Flanger",
        category: DeviceCategory::Time,
        params: &[
            param("rate", "Rate", Hz, 0.05, 10.0, 0.5, Log),
            param("depth", "Depth", Ms, 0.1, 20.0, 3.0, Log),
            param("feedback", "Feedback", Percent, 0.0, 95.0, 50.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 50.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "phaser",
        name: "Phaser",
        category: DeviceCategory::Time,
        params: &[
            param("rate", "Rate", Hz, 0.05, 10.0, 0.5, Log),
            param("depth", "Depth", Percent, 0.0, 100.0, 50.0, Linear),
            param("feedback", "Feedback", Percent, 0.0, 95.0, 30.0, Linear),
            param("mix", "Mix", Percent, 0.0, 100.0, 50.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "gate",
        name: "Gate",
        category: DeviceCategory::Dynamics,
        params: &[
            param("threshold", "Threshold", Db, -80.0, 0.0, -40.0, Linear),
            param("attack", "Attack", Ms, 0.1, 200.0, 1.0, Log),
            param("hold", "Hold", Ms, 0.0, 1_000.0, 50.0, Linear),
            param("release", "Release", Ms, 5.0, 2_000.0, 100.0, Log),
            param("range", "Range", Db, -80.0, 0.0, -60.0, Linear),
        ],
        ports: AUDIO_IN,
    },
    DeviceSchema {
        id: "stereo-width",
        name: "Stereo Width",
        category: DeviceCategory::Utility,
        params: &[param(
            "amount", "Amount", Percent, 0.0, 200.0, 100.0, Linear,
        )],
        ports: AUDIO_IN,
    },
];

/// Looks up one device by the keyword a project writes.
pub fn find_device(id: &str) -> Option<&'static DeviceSchema> {
    DEVICES.iter().find(|device| device.id == id)
}

/// One retired effect key and the current controls it drives.
/// Every rule for a source device names the same target; explicit targets win.
#[derive(Debug, Clone, Copy)]
pub struct Shorthand {
    pub device: &'static str,
    pub name: &'static str,
    pub value: ShorthandKind,
    pub target_device: &'static str,
    pub targets: &'static [ShorthandTarget],
    pub note: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The value shape a retired effect key carries.
pub enum ShorthandKind {
    /// `-2dB@10kHz`: one gain and one frequency.
    GainAtFrequency,
    /// `30Hz`: one frequency.
    Frequency,
    /// A fixed word such as `hall`.
    Word(&'static str),
}

#[derive(Debug, Clone, Copy)]
/// One parameter a retired key sets, and where that value comes from.
pub struct ShorthandTarget {
    pub param: &'static str,
    pub source: ShorthandSource,
}

#[derive(Debug, Clone, Copy)]
/// Where a retired key's value for one target comes from.
pub enum ShorthandSource {
    /// Gain taken from the `-2dB@10kHz` value.
    Gain,
    /// Frequency taken from the shorthand value.
    Frequency,
    /// Constant in the target parameter's declared unit.
    Fixed(f64),
    /// Constant percentage.
    Percent(f64),
    /// Constant time in seconds.
    Seconds(f64),
    /// Enum label resolved by the target device.
    Label(&'static str),
}

/// Every retired effect key and the current controls it sets.
pub static SHORTHANDS: &[Shorthand] = &[
    Shorthand {
        device: "eq",
        name: "low_shelf",
        value: ShorthandKind::GainAtFrequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "band1_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "band1_gain",
                source: ShorthandSource::Gain,
            },
            ShorthandTarget {
                param: "band1_q",
                source: ShorthandSource::Fixed(0.707),
            },
            ShorthandTarget {
                param: "band1_type",
                source: ShorthandSource::Label("low_shelf"),
            },
            ShorthandTarget {
                param: "band1_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as a static low-shelf band on the dynamic-eq engine (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "presence",
        value: ShorthandKind::GainAtFrequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "band2_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "band2_gain",
                source: ShorthandSource::Gain,
            },
            ShorthandTarget {
                param: "band2_type",
                source: ShorthandSource::Label("bell"),
            },
            ShorthandTarget {
                param: "band2_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as a static bell band on the dynamic-eq engine (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "notch",
        value: ShorthandKind::GainAtFrequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "band3_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "band3_gain",
                source: ShorthandSource::Gain,
            },
            ShorthandTarget {
                param: "band3_q",
                source: ShorthandSource::Fixed(4.0),
            },
            ShorthandTarget {
                param: "band3_type",
                source: ShorthandSource::Label("notch"),
            },
            ShorthandTarget {
                param: "band3_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as a narrow notch band on the dynamic-eq engine (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "high_shelf",
        value: ShorthandKind::GainAtFrequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "band4_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "band4_gain",
                source: ShorthandSource::Gain,
            },
            ShorthandTarget {
                param: "band4_q",
                source: ShorthandSource::Fixed(0.707),
            },
            ShorthandTarget {
                param: "band4_type",
                source: ShorthandSource::Label("high_shelf"),
            },
            ShorthandTarget {
                param: "band4_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as a static high-shelf band on the dynamic-eq engine (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "low_cut",
        value: ShorthandKind::Frequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "low_cut_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "low_cut_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as the dynamic-eq low cut (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "high_pass",
        value: ShorthandKind::Frequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "low_cut_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "low_cut_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as the dynamic-eq low cut (explicit controls win)",
    },
    Shorthand {
        device: "eq",
        name: "high_cut",
        value: ShorthandKind::Frequency,
        target_device: "dynamic-eq",
        targets: &[
            ShorthandTarget {
                param: "high_cut_freq",
                source: ShorthandSource::Frequency,
            },
            ShorthandTarget {
                param: "high_cut_enabled",
                source: ShorthandSource::Fixed(1.0),
            },
        ],
        note: "is rendered as the dynamic-eq high cut (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "type",
        value: ShorthandKind::Word("hall"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "damping",
                source: ShorthandSource::Percent(25.0),
            },
            ShorthandTarget {
                param: "modulation",
                source: ShorthandSource::Percent(20.0),
            },
            ShorthandTarget {
                param: "early_late",
                source: ShorthandSource::Percent(20.0),
            },
        ],
        note: "is rendered as damping, modulation and early_late; the device has no room model (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "type",
        value: ShorthandKind::Word("plate"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "damping",
                source: ShorthandSource::Percent(10.0),
            },
            ShorthandTarget {
                param: "modulation",
                source: ShorthandSource::Percent(35.0),
            },
            ShorthandTarget {
                param: "early_late",
                source: ShorthandSource::Percent(5.0),
            },
        ],
        note: "is rendered as damping, modulation and early_late; the device has no room model (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "type",
        value: ShorthandKind::Word("room"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "damping",
                source: ShorthandSource::Percent(45.0),
            },
            ShorthandTarget {
                param: "modulation",
                source: ShorthandSource::Percent(10.0),
            },
            ShorthandTarget {
                param: "early_late",
                source: ShorthandSource::Percent(60.0),
            },
        ],
        note: "is rendered as damping, modulation and early_late; the device has no room model (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "size",
        value: ShorthandKind::Word("small"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "decay",
                source: ShorthandSource::Seconds(0.7),
            },
            ShorthandTarget {
                param: "predelay",
                source: ShorthandSource::Seconds(0.008),
            },
            ShorthandTarget {
                param: "stereo_width",
                source: ShorthandSource::Percent(90.0),
            },
        ],
        note: "is rendered as decay, predelay and stereo_width; the device has no size control (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "size",
        value: ShorthandKind::Word("medium"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "decay",
                source: ShorthandSource::Seconds(1.8),
            },
            ShorthandTarget {
                param: "predelay",
                source: ShorthandSource::Seconds(0.020),
            },
            ShorthandTarget {
                param: "stereo_width",
                source: ShorthandSource::Percent(100.0),
            },
        ],
        note: "is rendered as decay, predelay and stereo_width; the device has no size control (explicit controls win)",
    },
    Shorthand {
        device: "reverb",
        name: "size",
        value: ShorthandKind::Word("large"),
        target_device: "reverb",
        targets: &[
            ShorthandTarget {
                param: "decay",
                source: ShorthandSource::Seconds(3.0),
            },
            ShorthandTarget {
                param: "predelay",
                source: ShorthandSource::Seconds(0.030),
            },
            ShorthandTarget {
                param: "stereo_width",
                source: ShorthandSource::Percent(115.0),
            },
        ],
        note: "is rendered as decay, predelay and stereo_width; the device has no size control (explicit controls win)",
    },
];

/// Every shorthand rule that applies to one device.
pub fn shorthands_for(device_id: &str) -> impl Iterator<Item = &'static Shorthand> {
    SHORTHANDS
        .iter()
        .filter(move |rule| rule.device == device_id)
}

/// The whole device catalog, in browser order.
pub fn catalog() -> DeviceCatalog {
    DeviceCatalog {
        version: DEVICE_CATALOG_VERSION,
        units: units::DEFINITIONS,
        devices: DEVICES,
    }
}

/// The catalog as JSON, which is what the editor and the agent surface read.
/// The parameters worth changing first, per device: the ten to twenty controls
/// that carry the sound. A model that reads every parameter of every device
/// wastes its context on names it will never set, so the catalog names these
/// and the tool tells the model to start here.
pub const PRIMARY_PARAMS: &[(&str, &[&str])] = &[
    ("gain", &["factor"]),
    (
        "eq",
        &["low_gain", "mid_gain", "mid_freq", "q", "high_gain"],
    ),
    (
        "passive-eq",
        &[
            "low_boost",
            "low_cut",
            "high_boost",
            "high_q",
            "high_cut_freq",
            "tilt",
            "air",
        ],
    ),
    (
        "dynamic-eq",
        &[
            "band1_freq",
            "band1_gain",
            "band1_threshold",
            "band1_range",
            "band2_freq",
            "band2_gain",
            "band2_threshold",
            "band2_range",
        ],
    ),
    (
        "compression",
        &["threshold", "ratio", "attack", "release", "knee", "makeup"],
    ),
    ("leveler", &["target", "speed", "range"]),
    ("opto", &["peak_reduction", "release", "makeup"]),
    ("fet", &["drive", "attack", "release", "output"]),
    (
        "de-esser",
        &[
            "threshold",
            "range",
            "high_frequency",
            "low_frequency",
            "attack",
            "release",
        ],
    ),
    (
        "breath-split",
        &[
            "breath_level",
            "voice_level",
            "threshold",
            "split_frequency",
        ],
    ),
    ("plosive", &["amount", "threshold", "frequency"]),
    (
        "multiband",
        &[
            "mix",
            "crossover_1",
            "crossover_2",
            "crossover_3",
            "band1_threshold",
            "band1_ratio",
            "band1_attack",
            "band1_release",
        ],
    ),
    ("limiter", &["ceiling", "release", "lookahead"]),
    (
        "reverb",
        &["decay", "predelay", "damping", "mix", "stereo_width"],
    ),
    (
        "reverb-fdn",
        &[
            "decay",
            "space",
            "predelay",
            "mix",
            "brightness",
            "character",
            "stereo_width",
        ],
    ),
    ("delay", &["time_l", "time_r", "feedback", "mix"]),
    ("chorus", &["rate", "depth", "mix"]),
    ("saturator", &["drive", "mix"]),
    ("iron", &["drive", "bias", "ceiling", "output"]),
    ("doubler", &["delay", "rate", "depth", "detune", "mix"]),
    (
        "exciter",
        &[
            "amount",
            "drive",
            "character",
            "low_freq",
            "high_freq",
            "output",
        ],
    ),
    (
        "ducked-send",
        &["predelay", "depth", "attack", "release", "width"],
    ),
    (
        "resonance",
        &[
            "threshold",
            "depth",
            "selectivity",
            "attack",
            "release",
            "mix",
        ],
    ),
    ("tuner", &["amount", "retune"]),
    ("distortion", &["drive", "tone", "output", "mix"]),
    ("flanger", &["rate", "depth", "feedback", "mix"]),
    ("phaser", &["rate", "depth", "feedback", "mix"]),
    ("gate", &["threshold", "attack", "hold", "release", "range"]),
    ("stereo-width", &["amount"]),
];

/// The primary parameters of one device, or an empty slice for a device this
/// table does not know.
pub fn primary_params(device: &str) -> &'static [&'static str] {
    PRIMARY_PARAMS
        .iter()
        .find(|(id, _)| *id == device)
        .map(|(_, params)| *params)
        .unwrap_or(&[])
}

/// The catalog as JSON for the editor and agent.
pub fn catalog_json() -> serde_json::Value {
    serde_json::to_value(catalog()).expect("device catalog is serializable")
}

impl DeviceSchema {
    /// Looks up one parameter by key, accepting its declared aliases.
    pub fn param(&self, id: &str) -> Option<&'static ParamSchema> {
        self.params
            .iter()
            .find(|param| param.id == id || param.aliases.contains(&id))
    }
}

#[cfg(test)]
mod primary_tests {
    use super::*;

    #[test]
    fn every_primary_key_is_a_parameter_of_its_device() {
        for (device, primaries) in PRIMARY_PARAMS {
            let schema = find_device(device).unwrap_or_else(|| panic!("{device} is not a device"));
            assert!(
                !primaries.is_empty(),
                "{device} lists no primary parameters"
            );
            assert!(
                primaries.len() <= 20,
                "{device} lists too many primary parameters"
            );
            for key in *primaries {
                assert!(
                    schema.param(key).is_some(),
                    "{device}: primary '{key}' is not a parameter"
                );
            }
        }
    }

    #[test]
    fn every_device_with_parameters_lists_primaries() {
        for device in DEVICES {
            if device.params.is_empty() {
                continue;
            }
            assert!(
                !primary_params(device.id).is_empty(),
                "{} has parameters but no primary set",
                device.id
            );
        }
    }

    #[test]
    fn an_unknown_device_has_no_primaries() {
        assert!(primary_params("not-a-device").is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_device_declares_a_summing_audio_input() {
        for device in DEVICES {
            let input = device
                .ports
                .first()
                .expect("every device declares its input");
            assert_eq!(input.kind, PortKind::Audio, "{}", device.id);
            assert!(input.sums, "{} main input must sum", device.id);
            let controls: Vec<_> = device
                .ports
                .iter()
                .filter(|port| port.kind == PortKind::Control)
                .collect();
            assert!(controls.iter().all(|port| !port.sums));
            for port in device.ports.iter().filter(|port| !port.sums) {
                assert!(
                    port.kind == PortKind::Control || port.id == "key",
                    "{}",
                    device.id
                );
            }
        }
    }

    #[test]
    fn only_the_devices_that_implement_one_declare_a_control_input() {
        let with_control: Vec<&str> = DEVICES
            .iter()
            .filter(|device| {
                device
                    .ports
                    .iter()
                    .any(|port| port.kind == PortKind::Control)
            })
            .map(|device| device.id)
            .collect();
        assert_eq!(with_control, vec!["compression"]);
        let compression = find_device("compression").unwrap();
        assert!(compression.ports.iter().any(|port| port.id == "control"));
        let eq = find_device("eq").unwrap();
        assert!(!eq.ports.iter().any(|port| port.kind == PortKind::Control));
    }

    #[test]
    fn the_catalog_serializes_port_kinds_and_sums() {
        let catalog = catalog_json();
        let compression = catalog["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["id"] == "compression")
            .unwrap();
        let control = compression["ports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["id"] == "control")
            .unwrap();
        assert_eq!(control["kind"], "control");
        assert_eq!(control["sums"], false);
    }

    #[test]
    fn device_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for device in DEVICES {
            assert!(ids.insert(device.id), "duplicate device id {}", device.id);
            let mut params = std::collections::HashSet::new();
            for param in device.params {
                assert!(
                    params.insert(param.id),
                    "duplicate parameter {}.{}",
                    device.id,
                    param.id
                );
            }
        }
    }

    #[test]
    fn shorthand_rules_target_existing_controls_of_one_device() {
        for rule in SHORTHANDS {
            assert!(find_device(rule.device).is_some(), "{}", rule.device);
            let target = find_device(rule.target_device).unwrap_or_else(|| {
                panic!(
                    "{} targets missing device {}",
                    rule.name, rule.target_device
                )
            });
            for spec in rule.targets {
                assert!(
                    target.param(spec.param).is_some(),
                    "{} targets missing {}.{}",
                    rule.name,
                    target.id,
                    spec.param
                );
            }
        }
        for rule in SHORTHANDS {
            let expected: Vec<_> = SHORTHANDS
                .iter()
                .filter(|other| other.device == rule.device)
                .map(|other| other.target_device)
                .collect();
            assert!(
                expected.iter().all(|target| *target == rule.target_device),
                "{} rules disagree about the target device",
                rule.device
            );
        }
    }

    #[test]
    fn defaults_are_inside_min_max() {
        for device in DEVICES {
            for param in device.params {
                assert!(
                    param.default >= param.min && param.default <= param.max,
                    "{}.{} default {} is outside {}..{}",
                    device.id,
                    param.id,
                    param.default,
                    param.min,
                    param.max
                );
            }
        }
    }

    #[test]
    fn log_curve_parameters_have_positive_min() {
        for device in DEVICES {
            for param in device.params {
                if param.curve == ParamCurve::Log {
                    assert!(
                        param.min > 0.0,
                        "{}.{} is logarithmic with min <= 0",
                        device.id,
                        param.id
                    );
                }
            }
        }
    }

    #[test]
    fn same_parameter_name_resolves_per_device() {
        let compression = find_device("compression").unwrap();
        let limiter = find_device("limiter").unwrap();
        let chorus = find_device("chorus").unwrap();
        let flanger = find_device("flanger").unwrap();
        assert_eq!(compression.param("release").unwrap().min, 10.0);
        assert_eq!(limiter.param("release").unwrap().min, 5.0);
        assert_eq!(compression.param("release").unwrap().max, 2_000.0);
        assert_eq!(limiter.param("release").unwrap().max, 500.0);
        let release_mode = limiter.param("release_mode").unwrap();
        assert_eq!(release_mode.kind, ParamKind::Enum);
        assert_eq!(release_mode.options, &["Fast", "Auto", "Punch", "Sustain"]);
        assert_eq!(release_mode.default, 1.0);
        assert_eq!(limiter.param("lookahead").unwrap().default, 5.0);
        assert_eq!(limiter.param("true_peak").unwrap().default, 1.0);
        let link = limiter.param("link").unwrap();
        assert_eq!(link.unit, ParamUnit::Percent);
        assert_eq!((link.min, link.max), (0.0, 100.0));
        assert_eq!(link.default, 100.0);
        let oversampling = limiter.param("oversampling").unwrap();
        assert_eq!(oversampling.kind, ParamKind::Enum);
        assert_eq!(oversampling.options, &["4x", "8x", "16x"]);
        assert_eq!(oversampling.default, 0.0);
        assert_eq!(chorus.param("rate").unwrap().default, 1.5);
        assert_eq!(flanger.param("rate").unwrap().default, 0.5);
        assert_eq!(
            find_device("distortion")
                .unwrap()
                .param("output")
                .unwrap()
                .default,
            0.0
        );
        assert_eq!(
            find_device("gate").unwrap().param("hold").unwrap().default,
            50.0
        );
    }

    #[test]
    fn reverb_fdn_exposes_a_placed_six_band_decay_eq() {
        let device = find_device("reverb-fdn").unwrap();
        assert_eq!(device.params.len(), 60);
        // The trim keeps its old slot; the placement controls are appended.
        assert_eq!(device.params[9].id, "decay_band1");
        for (band, frequency) in [125.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0]
            .iter()
            .enumerate()
        {
            let prefix = format!("decay_band{}", band + 1);
            let freq = device.param(&format!("{prefix}_freq")).unwrap();
            assert_eq!(freq.unit, ParamUnit::Hz);
            assert_eq!(freq.default, *frequency);
            assert_eq!(freq.min, 20.0);
            assert_eq!(freq.max, 20_000.0);
            let q = device.param(&format!("{prefix}_q")).unwrap();
            assert_eq!(q.default, 0.707);
            assert_eq!((q.min, q.max), (0.1, 10.0));
            let shape = device.param(&format!("{prefix}_shape")).unwrap();
            assert_eq!(shape.kind, ParamKind::Enum);
            assert_eq!(shape.options, &["None", "Low Shelf", "Bell", "High Shelf"]);
            assert_eq!(shape.default, 0.0);
        }
    }

    #[test]
    fn the_tone_devices_expose_their_law() {
        let iron = find_device("iron").unwrap();
        assert_eq!(iron.params.len(), 6);
        assert_eq!(iron.param("drive").unwrap().max, 24.0);
        assert_eq!(iron.param("bias").unwrap().min, -0.5);
        assert_eq!(iron.param("ceiling").unwrap().max, 100.0);
        assert_eq!(iron.param("low_freq").unwrap().default, 25.0);
        assert_eq!(iron.param("high_freq").unwrap().default, 18_000.0);
        assert_eq!(iron.param("output").unwrap().default, 0.0);

        let passive = find_device("passive-eq").unwrap();
        assert_eq!(passive.params.len(), 11);
        assert_eq!(passive.param("low_freq").unwrap().max, 300.0);
        assert_eq!(passive.param("low_boost").unwrap().max, 10.0);
        assert_eq!(passive.param("low_cut").unwrap().min, 0.0);
        assert_eq!(passive.param("high_q").unwrap().min, 0.7);
        assert_eq!(passive.param("high_q").unwrap().max, 2.0);
        assert_eq!(passive.param("tilt").unwrap().min, -6.0);
        assert_eq!(passive.param("tilt").unwrap().max, 6.0);
        assert_eq!(passive.param("air").unwrap().max, 6.0);
    }

    #[test]
    fn leveler_and_opto_expose_the_law_not_a_ratio() {
        let leveler = find_device("leveler").unwrap();
        assert_eq!(leveler.params.len(), 6);
        assert_eq!(leveler.param("target").unwrap().default, -20.0);
        assert_eq!(leveler.param("speed").unwrap().min, 2.0);
        assert_eq!(leveler.param("speed").unwrap().max, 6.0);
        assert_eq!(leveler.param("range").unwrap().min, 12.0);
        assert_eq!(leveler.param("range").unwrap().max, 24.0);
        assert_eq!(leveler.param("window").unwrap().min, 500.0);
        assert_eq!(leveler.param("window").unwrap().max, 3_000.0);
        assert_eq!(leveler.param("gate_release").unwrap().min, 1_000.0);
        assert_eq!(leveler.param("gate_release").unwrap().max, 2_000.0);

        let opto = find_device("opto").unwrap();
        assert_eq!(opto.params.len(), 3);
        assert!(opto.param("ratio").is_none());
        assert!(opto.param("threshold").is_none());
        assert!(opto.param("attack").is_none());
        assert_eq!(opto.param("peak_reduction").unwrap().max, 20.0);
        assert_eq!(opto.param("release").unwrap().min, 500.0);
        assert_eq!(opto.param("release").unwrap().max, 5_000.0);
    }

    #[test]
    fn fet_exposes_a_bias_not_a_ratio() {
        let fet = find_device("fet").unwrap();
        assert_eq!(fet.params.len(), 5);
        assert!(fet.param("ratio").is_none());
        assert!(fet.param("threshold").is_none());
        assert_eq!(fet.param("drive").unwrap().max, 24.0);
        assert_eq!(fet.param("attack").unwrap().min, 0.05);
        assert_eq!(fet.param("attack").unwrap().max, 20.0);
        assert_eq!(fet.param("release").unwrap().max, 2_000.0);
        assert_eq!(fet.param("bias").unwrap().max, 100.0);
        assert_eq!(fet.param("bias").unwrap().default, 25.0);
    }

    #[test]
    fn the_space_devices_expose_their_law() {
        let doubler = find_device("doubler").unwrap();
        assert_eq!(doubler.params.len(), 7);
        assert_eq!(doubler.param("voices").unwrap().min, 2.0);
        assert_eq!(doubler.param("voices").unwrap().max, 4.0);
        assert_eq!(doubler.param("voices").unwrap().steps, Some(3));
        assert_eq!(doubler.param("delay").unwrap().min, 8.0);
        assert_eq!(doubler.param("delay").unwrap().max, 30.0);
        assert_eq!(doubler.param("rate").unwrap().min, 0.2);
        assert_eq!(doubler.param("rate").unwrap().max, 0.5);
        assert_eq!(doubler.param("depth").unwrap().min, 1.0);
        assert_eq!(doubler.param("depth").unwrap().max, 3.0);
        assert_eq!(doubler.param("detune").unwrap().max, 20.0);
        assert_eq!(doubler.param("focus").unwrap().min, 200.0);
        assert_eq!(doubler.param("focus").unwrap().max, 400.0);

        let exciter = find_device("exciter").unwrap();
        assert_eq!(exciter.params.len(), 6);
        assert_eq!(exciter.param("amount").unwrap().min, 0.0);
        assert_eq!(exciter.param("drive").unwrap().min, 0.0);
        assert_eq!(exciter.param("low_freq").unwrap().min, 9_000.0);
        assert_eq!(exciter.param("high_freq").unwrap().max, 20_000.0);

        let ducked = find_device("ducked-send").unwrap();
        assert_eq!(ducked.params.len(), 6);
        assert_eq!(ducked.param("predelay").unwrap().min, 20.0);
        assert_eq!(ducked.param("predelay").unwrap().max, 40.0);
        assert_eq!(ducked.param("attack").unwrap().min, 5.0);
        assert_eq!(ducked.param("release").unwrap().min, 100.0);
        assert_eq!(ducked.param("depth").unwrap().max, 24.0);
        assert_eq!(ducked.param("width").unwrap().max, 200.0);
    }

    #[test]
    fn tuner_exposes_its_scale_and_latency_controls() {
        let device = find_device("tuner").unwrap();
        assert_eq!(device.params.len(), 6);
        assert_eq!(device.param("amount").unwrap().default, 100.0);
        assert_eq!(device.param("retune").unwrap().min, 10.0);
        assert_eq!(device.param("retune").unwrap().max, 300.0);
        assert_eq!(device.param("key").unwrap().options.len(), 12);
        assert_eq!(device.param("scale").unwrap().options.len(), 12);
        assert_eq!(device.param("scale").unwrap().default, 0.0);
        assert_eq!(device.param("scale").unwrap().options[0], "Chromatic");
        assert_eq!(device.param("mode").unwrap().options.len(), 2);
        assert_eq!(device.param("mode").unwrap().options[1], "Low Latency");
        assert_eq!(device.param("enabled").unwrap().kind, ParamKind::Bool);
    }

    #[test]
    fn resonance_exposes_the_spectral_controls() {
        let device = find_device("resonance").unwrap();
        assert_eq!(device.params.len(), 7);
        assert_eq!(device.param("threshold").unwrap().default, 6.0);
        assert_eq!(device.param("threshold").unwrap().min, -12.0);
        assert_eq!(device.param("depth").unwrap().min, 0.0);
        assert_eq!(device.param("depth").unwrap().max, 24.0);
        assert_eq!(device.param("selectivity").unwrap().default, 35.0);
        assert_eq!(device.param("attack").unwrap().min, 1.0);
        assert_eq!(device.param("release").unwrap().max, 1_000.0);
        assert_eq!(device.param("mix").unwrap().default, 100.0);
        assert_eq!(device.param("enabled").unwrap().kind, ParamKind::Bool);
    }

    #[test]
    fn catalog_v2_distinguishes_fractional_and_native_percentage_domains() {
        let value = catalog_json();
        assert_eq!(value["version"], 2);
        assert_eq!(
            value["units"].as_array().unwrap().len(),
            units::DEFINITIONS.len()
        );
        let fractions: Vec<_> = DEVICES
            .iter()
            .flat_map(|device| {
                device
                    .params
                    .iter()
                    .filter(|param| param.unit == ParamUnit::Fraction)
                    .map(move |param| (device.id, param.id, param.min, param.max, param.default))
            })
            .collect();
        assert_eq!(
            fractions,
            [
                ("compression", "mix", 0.0, 1.0, 1.0),
                ("multiband", "mix", 0.0, 1.0, 1.0)
            ]
        );
        let reverb = find_device("reverb").unwrap().param("mix").unwrap();
        assert_eq!(reverb.unit, ParamUnit::Percent);
        assert_eq!((reverb.min, reverb.max), (0.0, 100.0));
    }

    #[test]
    fn catalog_has_the_fixed_json_shape() {
        let value = catalog_json();
        let devices = value["devices"].as_array().unwrap();
        assert_eq!(devices.len(), DEVICES.len());
        for device in devices {
            assert_eq!(
                device
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
                ["id", "name", "category", "params", "ports"]
                    .map(String::from)
                    .into()
            );
            for param in device["params"].as_array().unwrap() {
                let kind = param["kind"].as_str().unwrap_or("continuous");
                assert!(matches!(kind, "continuous" | "bool" | "enum"));
                if kind == "continuous" {
                    assert!(param["steps"].is_null() || param["steps"].as_u64().unwrap() >= 2);
                    assert!(param.get("options").is_none());
                } else {
                    assert!(param["steps"].as_u64().unwrap() >= 2);
                }
                assert!(
                    ["Hz", "dB", "ms", "%", "fraction", "ratio", "none"]
                        .contains(&param["unit"].as_str().unwrap())
                );
            }
        }
    }

    #[test]
    fn dynamic_eq_exposes_six_bands_and_both_cuts() {
        let device = find_device("dynamic-eq").unwrap();
        assert_eq!(device.params.len(), 85);
        let fields = [
            "enabled",
            "solo",
            "channel",
            "type",
            "freq",
            "q",
            "gain",
            "dynamic",
            "sidechain",
            "threshold",
            "range",
            "attack",
            "release",
        ];
        for band in 1..=6 {
            for field in fields {
                let id = format!("band{band}_{field}");
                assert!(device.param(&id).is_some(), "missing {id}");
            }
        }
        for cut in ["low", "high"] {
            for field in ["enabled", "freq", "q"] {
                let id = format!("{cut}_cut_{field}");
                assert!(device.param(&id).is_some(), "missing {id}");
            }
        }
        // DSP defaults: enabled/dynamic off, 100 Hz start, 100 ms release.
        let freq = device.param("band1_freq").unwrap();
        assert_eq!((freq.default, freq.min, freq.max), (100.0, 20.0, 20_000.0));
        assert_eq!(device.param("band1_enabled").unwrap().default, 0.0);
        assert_eq!(device.param("band1_dynamic").unwrap().default, 0.0);
        assert_eq!(device.param("band1_sidechain").unwrap().default, 0.0);
        assert_eq!(device.param("auto_gain").unwrap().default, 0.0);
        assert_eq!(device.param("band1_release").unwrap().default, 100.0);
        assert_eq!(device.param("band1_range").unwrap().default, -6.0);
        assert_eq!(device.param("band6_freq").unwrap().default, 8_000.0);
        assert_eq!(device.param("low_cut_freq").unwrap().default, 20.0);
        assert_eq!(device.param("high_cut_freq").unwrap().default, 20_000.0);
        // Legacy band-1 ids keep working with unchanged ranges.
        assert_eq!(device.param("band1_q").unwrap().max, 20.0);
        assert_eq!(device.param("band1_gain").unwrap().max, 24.0);
        assert_eq!(device.param("band1_threshold").unwrap().min, -60.0);
    }

    #[test]
    fn breath_split_exposes_the_detector_and_level_controls() {
        let device = find_device("breath-split").unwrap();
        assert_eq!(device.params.len(), 6);
        assert_eq!(device.param("breath_level").unwrap().default, 0.0);
        assert_eq!(device.param("breath_level").unwrap().min, -60.0);
        assert_eq!(device.param("breath_level").unwrap().max, 12.0);
        assert_eq!(device.param("voice_level").unwrap().default, 0.0);
        assert_eq!(device.param("threshold").unwrap().default, 0.5);
        assert_eq!(device.param("threshold").unwrap().min, 0.0);
        assert_eq!(device.param("threshold").unwrap().max, 1.0);
        assert_eq!(device.param("smoothing").unwrap().default, 20.0);
        assert_eq!(device.param("smoothing").unwrap().min, 1.0);
        assert_eq!(device.param("monitor").unwrap().kind, ParamKind::Bool);
        assert_eq!(device.param("monitor").unwrap().default, 0.0);
        assert_eq!(device.param("split_frequency").unwrap().default, 2_000.0);
        assert_eq!(device.param("split_frequency").unwrap().min, 1_500.0);
        assert_eq!(device.param("split_frequency").unwrap().max, 6_000.0);
        assert!(
            device
                .param("split_frequency")
                .unwrap()
                .description
                .contains("complement")
        );
    }

    #[test]
    fn plosive_exposes_the_detector_and_shelf_controls() {
        let device = find_device("plosive").unwrap();
        assert_eq!(device.name, "Plosive");
        assert_eq!(device.category, DeviceCategory::Dynamics);
        assert_eq!(device.params.len(), 5);
        let amount = device.param("amount").unwrap();
        assert_eq!((amount.default, amount.min, amount.max), (0.0, 0.0, 100.0));
        let threshold = device.param("threshold").unwrap();
        assert_eq!(
            (threshold.default, threshold.min, threshold.max),
            (6.0, 2.0, 24.0)
        );
        let frequency = device.param("frequency").unwrap();
        assert_eq!(
            (frequency.default, frequency.min, frequency.max),
            (120.0, 80.0, 150.0)
        );
        let release = device.param("release").unwrap();
        assert_eq!(
            (release.default, release.min, release.max),
            (40.0, 10.0, 300.0)
        );
        assert_eq!(device.param("monitor").unwrap().kind, ParamKind::Bool);
        assert_eq!(device.param("monitor").unwrap().default, 0.0);
        assert!(
            device
                .param("amount")
                .unwrap()
                .description
                .contains("bypass")
        );
    }

    #[test]
    fn deesser_exposes_the_shipped_and_analog_controls() {
        let device = find_device("de-esser").unwrap();
        assert_eq!(device.params.len(), 16);
        assert_eq!(device.param("threshold").unwrap().default, -36.0);
        assert_eq!(device.param("relative_threshold").unwrap().default, -12.0);
        assert_eq!(device.param("range").unwrap().default, 6.0);
        assert_eq!(device.param("attack").unwrap().default, 11.0);
        assert_eq!(device.param("release").unwrap().default, 15.5);
        assert_eq!(device.param("high_frequency").unwrap().default, 7_500.0);
        assert_eq!(device.param("low_frequency").unwrap().default, 14_000.0);
        assert_eq!(device.param("stereo_link").unwrap().default, 100.0);
        assert_eq!(device.param("lookahead").unwrap().default, 0.0);
        assert_eq!(device.param("sibilance_band").unwrap().default, 0.0);
        assert_eq!(device.param("band_estimation").unwrap().default, 0.0);
        assert_eq!(device.param("band_override").unwrap().default, 0.0);
        assert_eq!(device.param("segment_detection").unwrap().default, 0.0);
        assert_eq!(device.param("threshold_mode").unwrap().default, 0.0);
        assert_eq!(device.param("detector").unwrap().default, 0.0);
        assert!(
            device
                .param("range")
                .unwrap()
                .description
                .contains("Range/36")
        );
        assert!(
            device
                .param("stereo_link")
                .unwrap()
                .description
                .contains("mid")
        );
        let mode = device.param("band_processing").unwrap();
        assert_eq!(mode.kind, ParamKind::Enum);
        assert_eq!(mode.options, &["Wide Band", "Split Band"]);
        assert_eq!(mode.default, 0.0);
    }

    #[test]
    fn compression_exposes_a_six_band_detector_eq_with_an_off_default() {
        let device = find_device("compression").unwrap();
        assert_eq!(device.params.len(), 50);
        for band in 1..=6 {
            let prefix = format!("sidechain_eq{band}");
            let switch = device
                .param(&format!("{prefix}_on"))
                .unwrap_or_else(|| panic!("missing {prefix}_on"));
            assert_eq!(switch.kind, ParamKind::Bool);
            assert_eq!(switch.default, 0.0);
            let shape = device.param(&format!("{prefix}_type")).unwrap();
            assert_eq!(shape.kind, ParamKind::Enum);
            assert_eq!(
                shape.options,
                &["Bell", "Low Shelf", "High Shelf", "Low Cut", "High Cut"]
            );
            assert_eq!(shape.default, 0.0);
            let frequency = device.param(&format!("{prefix}_frequency")).unwrap();
            assert_eq!(frequency.unit, ParamUnit::Hz);
            assert_eq!(
                (frequency.min, frequency.max, frequency.default),
                (20.0, 20_000.0, 1_000.0)
            );
            let gain = device.param(&format!("{prefix}_gain")).unwrap();
            assert_eq!(gain.unit, ParamUnit::Db);
            assert_eq!((gain.min, gain.max, gain.default), (-30.0, 30.0, 0.0));
            let q = device.param(&format!("{prefix}_q")).unwrap();
            assert_eq!((q.min, q.max, q.default), (0.1, 10.0, 0.707));
        }
        assert!(
            device
                .param("sidechain_eq1_gain")
                .unwrap()
                .description
                .contains("detector signal only")
        );
        assert_eq!(device.param("detector_hpf").unwrap().default, 20.0);
    }

    #[test]
    fn multiband_exposes_mode_slope_range_and_enable_controls() {
        let device = find_device("multiband").unwrap();
        assert_eq!(device.name, "Multiband Compander");
        assert_eq!(device.params.len(), 57);
        let count = device.param("band_count").unwrap();
        assert_eq!(count.kind, ParamKind::Enum);
        assert_eq!(count.options, &["2 Bands", "3 Bands", "4 Bands"]);
        assert_eq!(count.default, 0.0);
        let slope = device.param("slope").unwrap();
        assert_eq!(slope.kind, ParamKind::Enum);
        assert_eq!(slope.options.len(), 5);
        assert_eq!(slope.default, 2.0);
        let phase = device.param("phase_mode").unwrap();
        assert_eq!(phase.kind, ParamKind::Enum);
        assert_eq!(phase.options, &["Minimum", "Dynamic", "Linear"]);
        assert_eq!(phase.default, 0.0);
        let detector = device.param("detector").expect("missing detector");
        assert_eq!(detector.kind, ParamKind::Enum);
        assert_eq!(detector.options, &["Peak", "Programme"]);
        assert_eq!(detector.default, 0.0);
        assert!(
            detector.description.contains("transient"),
            "the detector description must name what the mode changes"
        );
        for crossover in ["crossover_1", "crossover_2", "crossover_3"] {
            assert!(device.param(crossover).is_some(), "missing {crossover}");
        }
        for band in 1..=4 {
            let range = device
                .param(&format!("band{band}_range"))
                .expect("missing range");
            assert_eq!(range.min, -60.0);
            assert_eq!(range.max, 60.0);
            assert_eq!(range.default, 0.0);
            assert!(range.description.contains("Bipolar"));
            let enabled = device
                .param(&format!("band{band}_enabled"))
                .expect("missing enable");
            assert_eq!(enabled.kind, ParamKind::Bool);
            assert_eq!(enabled.default, 1.0);
            for field in ["threshold", "ratio", "attack", "release", "knee", "makeup"] {
                let id = format!("band{band}_{field}");
                assert!(device.param(&id).is_some(), "missing {id}");
            }
            let lookahead = device
                .param(&format!("band{band}_lookahead"))
                .expect("missing per-band lookahead");
            assert_eq!(lookahead.unit, ParamUnit::Ms);
            assert_eq!(lookahead.min, 0.0);
            assert_eq!(lookahead.max, 20.0);
            assert_eq!(lookahead.default, 0.0);
            for field in ["solo", "mute"] {
                let id = format!("band{band}_{field}");
                let switch = device.param(&id).unwrap_or_else(|| panic!("missing {id}"));
                assert_eq!(switch.kind, ParamKind::Bool);
                assert_eq!(switch.default, 0.0);
            }
        }
        let lookahead = device.param("lookahead").expect("missing lookahead");
        assert_eq!(lookahead.unit, ParamUnit::Ms);
        assert_eq!(lookahead.min, 0.0);
        assert_eq!(lookahead.max, 20.0);
        assert_eq!(lookahead.default, 0.0);
        assert_eq!(device.param("band1_threshold").unwrap().default, 0.0);
        assert_eq!(device.param("band1_ratio").unwrap().default, 1.0);
        assert_eq!(device.param("band2_threshold").unwrap().default, 0.0);
        assert_eq!(device.param("crossover_1").unwrap().default, 1_000.0);
        assert!(
            device
                .param("phase_mode")
                .unwrap()
                .description
                .contains("1024")
        );
    }

    #[test]
    fn reverb_exposes_new_controls_with_no_op_defaults() {
        let device = find_device("reverb").unwrap();
        assert_eq!(device.param("modulation").unwrap().default, 0.0);
        assert_eq!(device.param("early_late").unwrap().default, 0.0);
        assert_eq!(device.param("stereo_width").unwrap().default, 100.0);
        assert_eq!(device.param("low_cut").unwrap().default, 20.0);
        assert_eq!(device.param("high_cut").unwrap().default, 20_000.0);
        assert_eq!(device.param("decay").unwrap().default, 2_000.0);
        assert_eq!(device.param("mix").unwrap().default, 30.0);
    }

    #[test]
    fn bool_and_enum_parameters_serialize_with_kind_and_options() {
        let value = catalog_json();
        let devices = value["devices"].as_array().unwrap();
        let dynamic = devices
            .iter()
            .find(|device| device["id"] == "dynamic-eq")
            .unwrap();
        let param = |id: &str| {
            dynamic["params"]
                .as_array()
                .unwrap()
                .iter()
                .find(|param| param["id"] == id)
                .unwrap()
        };
        let enabled = param("band3_enabled");
        assert_eq!(enabled["kind"], "bool");
        assert_eq!(enabled["steps"], 2);
        assert_eq!(enabled["min"], 0.0);
        assert_eq!(enabled["max"], 1.0);
        assert_eq!(enabled["unit"], "none");
        assert!(enabled.get("options").is_none());
        let kind = param("band3_type");
        assert_eq!(kind["kind"], "enum");
        assert_eq!(kind["steps"], 7);
        assert_eq!(kind["max"], 6.0);
        assert_eq!(
            kind["options"],
            serde_json::json!([
                "Bell",
                "Low Shelf",
                "High Shelf",
                "Low Cut",
                "High Cut",
                "Notch",
                "Band Pass"
            ])
        );
        // Continuous params keep the historical shape, so old consumers still parse.
        assert!(param("band3_freq").get("kind").is_none());
        assert!(param("band3_freq").get("options").is_none());
    }
}
