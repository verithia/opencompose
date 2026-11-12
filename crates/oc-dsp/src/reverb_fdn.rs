//! Long-form FDN reverb.
//!
//! Two independent per-channel networks: a 16-line Hadamard FDN and a 2-line
//! FDN, each with a six-band feedback T60 tilt, plus a measured 256-tap
//! early-reflection table and two Character-modulated delay stages. The six
//! decay bands place their dB trim with a frequency, Q and shape.

use super::AudioEffect;
use super::biquad::{BiquadCoefficients, BiquadState, FilterType};

const FDN_LINES: usize = 16;
const TANK_LINES: usize = 2;
const TILT_BANDS: usize = 6;
const TILT_HZ: [f64; TILT_BANDS] = [125.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

/// Per-channel delay lengths in milliseconds.
const FDN_MS: [[f64; FDN_LINES]; 2] = [
    [
        219.0, 246.0, 276.0, 311.0, 349.0, 392.0, 441.0, 495.0, 86.0, 97.0, 109.0, 122.0, 137.0,
        154.0, 173.0, 195.0,
    ],
    [
        232.0, 261.0, 293.0, 329.0, 370.0, 416.0, 467.0, 525.0, 91.0, 103.0, 115.0, 130.0, 146.0,
        164.0, 184.0, 206.0,
    ],
];
const TANK_MS: [[f64; TANK_LINES]; 2] = [[170.0, 471.0], [290.0, 523.0]];

/// Space display anchors in milliseconds at normalised 0, 0.5 and 1.
const SPACE_DISPLAY_MS: [f64; 3] = [200.0, 2_500.0, 10_000.0];

/// Space control breakpoints for the measured RT30 tables.
const SPACE_T60_N: [f64; 9] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];

/// Per-band RT30 (125 Hz..16 kHz) at the nine measured Space points.
const SPACE_BAND_T60: [[f64; TILT_BANDS]; 9] = [
    [0.24, 0.22, 0.23, 0.23, 0.22, 0.21],
    [0.48, 0.58, 0.56, 0.52, 0.47, 0.42],
    [1.25, 1.13, 1.09, 1.04, 0.93, 0.82],
    [2.00, 1.92, 1.85, 1.74, 1.60, 1.39],
    [2.87, 2.85, 2.72, 2.50, 2.23, 1.95],
    [3.90, 3.73, 3.65, 3.37, 2.96, 2.62],
    [5.32, 5.06, 4.94, 4.39, 3.80, 3.26],
    [7.93, 7.18, 6.80, 5.93, 4.92, 4.06],
    [11.43, 11.01, 10.03, 8.47, 6.81, 5.55],
];

/// Measured brightness tilt relative to 50% at 0/25/50/75%.
const BRIGHT_POINTS: [f64; 4] = [0.0, 25.0, 50.0, 75.0];
const BRIGHT_RATIO: [[f64; 4]; TILT_BANDS] = [
    [1.000, 1.000, 1.000, 1.000],
    [0.961, 0.982, 1.000, 1.011],
    [0.924, 0.961, 1.000, 1.035],
    [0.888, 0.940, 1.000, 1.060],
    [0.848, 0.924, 1.000, 1.085],
    [0.749, 0.882, 1.000, 1.123],
];

/// ER peak level versus Space, relative to Space 50%, measured at 0/25/50/75/100%.
const ER_LEVEL_N: [f64; 5] = [0.0, 25.0, 50.0, 75.0, 100.0];
const ER_LEVEL: [f64; 5] = [3.28, 1.462, 1.0, 0.918, 0.853];

/// Distance scales the ER energy by these measured factors (amplitude).
const ER_DISTANCE_N: [f64; 6] = [0.0, 25.0, 50.0, 75.0, 90.0, 100.0];
const ER_DISTANCE_GAIN: [f64; 6] = [2.695, 1.420, 1.0, 0.661, 0.386, 0.314];

/// Late-field output calibration for the feedback network.
const LATE_LEVEL: f64 = 2.1;

/// One-pole early-reflection absorption cutoff, derived from the tap tails.
const ER_CUTOFF_HZ: f64 = 6100.0;
const ER_DISTANCE_SHIFT_MS: f64 = 5.667;

/// Feedback gains below this are muted to avoid denormal tails.
const DENORMAL_FLOOR: f32 = 1e-20;

/// Modulation seed for a device built without an authored location.
///
/// Renderers pass the seed of the device's place in the project; this constant
/// keeps a bare `ReverbFdn::new` repeatable as well.
pub const DEFAULT_SEED: u32 = 0x5eed_1234;

/// Per-modulator seed offsets, indexed `[stage][channel]`.
const STAGE_SEED: [[u32; 2]; 2] = [[0x1111, 0x2222], [0x3333, 0x4444]];
/// Offset between a stage's time and amplitude modulators.
const AMP_SEED: u32 = 0x005a_5a5a;

fn stage_seed(seed: u32, stage: usize, channel: usize) -> u32 {
    seed ^ STAGE_SEED[stage][channel]
}

const DIFFUSER_MS: [[f64; 6]; 2] = [
    [4.77, 6.31, 8.11, 9.83, 11.71, 13.29],
    [5.13, 6.79, 8.63, 10.37, 12.31, 13.97],
];
const DIFFUSER_GAIN: f32 = 0.6;
const POST_EQ_BANDS: usize = 6;

#[derive(Clone, Copy)]
struct PostEqBand {
    filter_type: FilterType,
    freq: f64,
    q: f64,
    gain_db: f64,
    coefficients: BiquadCoefficients,
}

impl PostEqBand {
    fn flat(filter_type: FilterType, freq: f64) -> Self {
        Self {
            filter_type,
            freq,
            q: 1.0,
            gain_db: 0.0,
            coefficients: BiquadCoefficients::default(),
        }
    }

    fn engaged(&self) -> bool {
        match self.filter_type {
            FilterType::Peak | FilterType::LowShelf | FilterType::HighShelf => {
                self.gain_db.abs() > 1e-9
            }
            _ => true,
        }
    }

    fn rebuild(&mut self, sample_rate: f64) {
        self.coefficients = if self.engaged() {
            let gain = match self.filter_type {
                FilterType::Peak | FilterType::LowShelf | FilterType::HighShelf => self.gain_db,
                _ => 0.0,
            };
            BiquadCoefficients::design(self.filter_type, self.freq, self.q, gain, sample_rate)
        } else {
            BiquadCoefficients::default()
        };
    }
}

fn post_eq_filter_type(value: f64) -> FilterType {
    match value.round().clamp(0.0, 5.0) as u8 {
        1 => FilterType::LowShelf,
        2 => FilterType::HighShelf,
        3 => FilterType::HighPass,
        4 => FilterType::LowPass,
        5 => FilterType::Notch,
        _ => FilterType::Peak,
    }
}

fn decay_band_index(name: &str) -> Option<usize> {
    let band = name.strip_prefix("decay_band")?.parse::<usize>().ok()?;
    (1..=TILT_BANDS).contains(&band).then_some(band - 1)
}

fn decay_eq_param(name: &str) -> Option<(usize, &str)> {
    let (band, field) = name.strip_prefix("decay_band")?.split_once('_')?;
    let band = band.parse::<usize>().ok()?;
    (1..=TILT_BANDS)
        .contains(&band)
        .then_some((band - 1, field))
}

/// Shape of one decay-EQ band; `None` keeps the plain per-band trim.
#[derive(Clone, Copy)]
enum DecayEqShape {
    None,
    LowShelf,
    Bell,
    HighShelf,
}

impl DecayEqShape {
    fn from_value(value: f64) -> Self {
        match value.round() as i32 {
            1 => Self::LowShelf,
            2 => Self::Bell,
            3 => Self::HighShelf,
            _ => Self::None,
        }
    }

    fn value(self) -> f64 {
        match self {
            Self::None => 0.0,
            Self::LowShelf => 1.0,
            Self::Bell => 2.0,
            Self::HighShelf => 3.0,
        }
    }

    fn filter_type(self) -> Option<FilterType> {
        match self {
            Self::None => None,
            Self::LowShelf => Some(FilterType::LowShelf),
            Self::Bell => Some(FilterType::Peak),
            Self::HighShelf => Some(FilterType::HighShelf),
        }
    }
}

/// One decay-EQ band: its dB trim is `decay_band_db`, this places it.
#[derive(Clone, Copy)]
struct DecayEqBand {
    freq: f64,
    q: f64,
    shape: DecayEqShape,
}

impl DecayEqBand {
    fn flat(freq: f64) -> Self {
        Self {
            freq,
            q: 0.707,
            shape: DecayEqShape::None,
        }
    }
}

/// Normalise a shape token to the spelling the decoders match: the schema
/// labels carry spaces (`Low Shelf`), preset JSON carries underscores
/// (`low_shelf`), and the DSL may carry either.
fn normalize_shape(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

/// Decode a DSL shape name for a decay-EQ band.
pub fn decay_eq_shape_code(name: &str) -> Option<f64> {
    match normalize_shape(name).as_str() {
        "none" | "off" | "flat" => Some(0.0),
        "low_shelf" | "lowshelf" => Some(1.0),
        "bell" | "peak" => Some(2.0),
        "high_shelf" | "highshelf" => Some(3.0),
        _ => None,
    }
}

/// Decode a shape name for a post-EQ band. Every schema label
/// (`Bell`, `Low Shelf`, `High Shelf`, `Low Cut`, `High Cut`, `Notch`) decodes,
/// as does its snake_case spelling.
pub fn post_eq_shape_code(name: &str) -> Option<f64> {
    match normalize_shape(name).as_str() {
        "bell" | "peak" => Some(0.0),
        "low_shelf" | "lowshelf" => Some(1.0),
        "high_shelf" | "highshelf" => Some(2.0),
        "low_cut" | "lowcut" => Some(3.0),
        "high_cut" | "highcut" => Some(4.0),
        "notch" => Some(5.0),
        _ => None,
    }
}

fn post_eq_param(name: &str) -> Option<(usize, &str)> {
    let (band, field) = name.strip_prefix("post_eq")?.split_once('_')?;
    let band = band.parse::<usize>().ok()?;
    (1..=POST_EQ_BANDS)
        .contains(&band)
        .then_some((band - 1, field))
}

/// Alternating output tap signs decorrelate the late stereo field.
const TAP_SIGNS: [f32; 16] = [
    1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0,
];

// Early-reflection taps (48 kHz, Distance 50%, 0.5 impulse).
// Delay in ms and per-channel gain before the one-pole absorption filter.
const ER_TAPS: [(f32, f32, f32); 256] = [
    (6.687500, 0.037175, 0.000000),
    (8.312500, 0.000000, 0.036996),
    (8.979167, 0.031183, 0.000000),
    (10.979167, 0.000000, -0.009151),
    (11.333333, -0.019601, 0.000000),
    (12.500000, 0.000000, 0.025227),
    (14.375000, -0.007955, 0.000000),
    (14.625000, -0.005341, 0.000000),
    (15.604167, 0.005944, 0.000000),
    (15.916667, 0.000000, 0.026876),
    (16.166667, 0.000000, 0.018046),
    (16.812500, 0.011442, 0.000000),
    (17.145833, 0.000000, -0.020084),
    (18.145833, 0.032276, 0.000000),
    (18.354167, -0.103259, 0.000000),
    (18.812500, -0.013983, 0.038615),
    (19.520833, 0.008360, 0.000000),
    (20.145833, 0.000000, -0.009472),
    (20.708333, 0.016924, 0.000000),
    (20.916667, 0.027610, 0.000000),
    (21.062500, 0.000158, -0.028247),
    (21.125000, 0.000014, -0.016485),
    (21.979167, 0.000000, 0.004092),
    (22.458333, 0.000000, 0.008109),
    (22.833333, -0.013207, 0.000000),
    (23.625000, 0.024974, 0.000000),
    (23.875000, 0.000003, -0.004953),
    (24.083333, -0.008891, -0.000003),
    (24.833333, 0.000000, -0.044572),
    (25.166667, 0.000000, 0.007337),
    (25.562500, -0.004471, 0.000000),
    (25.625000, -0.000597, 0.030040),
    (28.229167, -0.015099, 0.000000),
    (28.458333, 0.035066, 0.000000),
    (28.520833, -0.034283, 0.000000),
    (28.729167, -0.000018, -0.015047),
    (29.166667, -0.009328, 0.000000),
    (29.583333, 0.003455, 0.000000),
    (29.708333, -0.006337, 0.000000),
    (30.520833, 0.000000, 0.011435),
    (30.708333, 0.000000, 0.031528),
    (31.395833, 0.000000, 0.004419),
    (32.270833, -0.021386, 0.000000),
    (32.416667, -0.000122, -0.114360),
    (32.875000, 0.000000, -0.021475),
    (33.312500, 0.006038, 0.000000),
    (33.916667, 0.000000, -0.011596),
    (34.375000, 0.044192, 0.000000),
    (34.437500, 0.032926, 0.000000),
    (34.583333, 0.013759, 0.000000),
    (35.437500, 0.000000, 0.006259),
    (35.625000, 0.005474, 0.000007),
    (36.000000, 0.000000, 0.008459),
    (36.229167, 0.005623, 0.000002),
    (36.375000, 0.000032, -0.012969),
    (36.479167, 0.000001, 0.019955),
    (36.520833, -0.020558, 0.005618),
    (36.562500, -0.012229, 0.043597),
    (38.375000, -0.035084, 0.000000),
    (38.500000, 0.013844, 0.000000),
    (39.291667, -0.010636, 0.000000),
    (39.395833, -0.000300, 0.018925),
    (39.708333, 0.000000, 0.006409),
    (39.854167, -0.019895, 0.000037),
    (39.916667, -0.002656, 0.014344),
    (39.958333, -0.000561, -0.007217),
    (40.770833, 0.014919, 0.000000),
    (42.458333, 0.014236, 0.000000),
    (42.875000, 0.029959, 0.001178),
    (43.020833, 0.000171, 0.005827),
    (43.625000, 0.000000, -0.003104),
    (43.687500, 0.011280, -0.000414),
    (43.854167, -0.036429, -0.000001),
    (44.437500, -0.019401, 0.004915),
    (44.708333, -0.004492, 0.000000),
    (45.729167, 0.012339, 0.000000),
    (45.854167, 0.000157, 0.010699),
    (46.187500, 0.031316, 0.000000),
    (46.583333, -0.007957, 0.000000),
    (46.791667, 0.016808, 0.004154),
    (47.500000, -0.005480, 0.000000),
    (47.625000, -0.000070, 0.006048),
    (47.729167, -0.000001, 0.041813),
    (47.875000, -0.006714, 0.000239),
    (48.000000, 0.001633, 0.003092),
    (48.166667, 0.007977, -0.008608),
    (48.208333, 0.032609, -0.005128),
    (48.458333, 0.003588, 0.000000),
    (48.562500, -0.021139, 0.000000),
    (48.791667, -0.000010, 0.006170),
    (49.104167, -0.009761, 0.000000),
    (49.520833, -0.011669, 0.000000),
    (49.708333, -0.000013, -0.026939),
    (49.770833, -0.000001, 0.005911),
    (49.979167, 0.000000, -0.005238),
    (50.104167, 0.006406, 0.018366),
    (50.687500, 0.002785, 0.000000),
    (50.979167, -0.017298, 0.000000),
    (51.020833, -0.004846, -0.021211),
    (51.270833, 0.000000, 0.010930),
    (51.645833, 0.000000, -0.021643),
    (51.833333, 0.000000, 0.018365),
    (52.437500, 0.000000, -0.010324),
    (52.854167, 0.007784, 0.012593),
    (53.500000, 0.003807, 0.000000),
    (53.750000, 0.000000, -0.004921),
    (53.979167, 0.000000, -0.003405),
    (54.645833, 0.009688, 0.000000),
    (54.729167, -0.002904, 0.000000),
    (55.145833, 0.000000, -0.009342),
    (55.354167, 0.006333, 0.023769),
    (55.500000, -0.040962, 0.000136),
    (56.125000, -0.010374, 0.000000),
    (56.250000, -0.030838, 0.001236),
    (56.562500, -0.021201, 0.000000),
    (56.625000, -0.005446, 0.030694),
    (56.937500, -0.004414, -0.004911),
    (57.250000, -0.003036, 0.000000),
    (57.500000, 0.000000, 0.009209),
    (57.791667, 0.000000, 0.032369),
    (57.937500, 0.016070, 0.000138),
    (58.125000, 0.000019, -0.006635),
    (58.625000, 0.013186, 0.000000),
    (58.729167, 0.000372, 0.014344),
    (59.104167, 0.006220, 0.000000),
    (59.395833, -0.005826, 0.005803),
    (59.437500, -0.003466, -0.005820),
    (59.770833, -0.002627, 0.000000),
    (59.812500, -0.000736, -0.002649),
    (60.354167, -0.030935, 0.001349),
    (61.041667, 0.005347, 0.002040),
    (61.083333, 0.001999, -0.003958),
    (61.375000, 0.000658, -0.002306),
    (61.520833, -0.002559, -0.000013),
    (61.645833, 0.001776, 0.000000),
    (61.895833, 0.012467, 0.003917),
    (62.354167, 0.000654, 0.009028),
    (62.583333, 0.000561, -0.021074),
    (62.791667, 0.000546, -0.002382),
    (62.958333, 0.000551, 0.003643),
    (63.208333, -0.009427, -0.000194),
    (63.437500, 0.000708, -0.016758),
    (63.520833, 0.000174, 0.002405),
    (63.979167, -0.020202, -0.000176),
    (64.208333, 0.000112, 0.016092),
    (64.395833, 0.000629, -0.002025),
    (64.625000, -0.013199, -0.000162),
    (64.895833, 0.037475, -0.000156),
    (65.000000, -0.028170, -0.000153),
    (65.208333, 0.001090, -0.033231),
    (65.395833, 0.001472, 0.000378),
    (65.500000, 0.002995, -0.005198),
    (65.645833, -0.011391, 0.000471),
    (65.958333, 0.001117, 0.010619),
    (66.020833, -0.004164, 0.002626),
    (66.187500, -0.011677, 0.000541),
    (66.354167, 0.001054, 0.003958),
    (66.541667, 0.001060, -0.008155),
    (66.750000, 0.030644, 0.000627),
    (66.895833, 0.001181, -0.009689),
    (67.000000, 0.021711, 0.000361),
    (67.104167, -0.023415, 0.000641),
    (67.458333, -0.008227, 0.000617),
    (67.562500, 0.005441, 0.000609),
    (67.645833, 0.001820, -0.042292),
    (67.791667, 0.001576, 0.004264),
    (67.854167, -0.007900, 0.001091),
    (68.083333, 0.001719, 0.000568),
    (68.333333, 0.003796, 0.000550),
    (68.375000, -0.004198, 0.000547),
    (68.479167, 0.001442, 0.001272),
    (68.562500, 0.009382, 0.000579),
    (68.750000, 0.001589, -0.008179),
    (68.916667, -0.029642, 0.000487),
    (69.000000, 0.009116, 0.026474),
    (69.395833, 0.001451, 0.033397),
    (69.687500, 0.001396, -0.000403),
    (69.875000, -0.005500, -0.000403),
    (69.916667, -0.002043, -0.010394),
    (70.270833, 0.008185, -0.000327),
    (70.354167, -0.005206, -0.000377),
    (70.437500, -0.008707, -0.008976),
    (70.500000, -0.001696, -0.013882),
    (70.729167, 0.001185, 0.008823),
    (70.895833, -0.010899, -0.000329),
    (71.645833, -0.017996, -0.000320),
    (71.770833, 0.026719, -0.000280),
    (72.020833, -0.026057, -0.000304),
    (72.270833, 0.001153, 0.022604),
    (72.375000, 0.032924, -0.020668),
    (72.833333, 0.001062, -0.001959),
    (72.895833, 0.001064, 0.004210),
    (72.937500, 0.003522, 0.001429),
    (73.062500, 0.001105, -0.017354),
    (73.208333, 0.001012, -0.013912),
    (73.291667, 0.001000, 0.006470),
    (73.333333, 0.000995, 0.008006),
    (73.645833, 0.009373, 0.000288),
    (74.250000, 0.027586, 0.000144),
    (74.291667, 0.008378, 0.010489),
    (74.500000, 0.028057, 0.000167),
    (74.812500, 0.004271, -0.010552),
    (74.895833, -0.013588, 0.000907),
    (75.020833, 0.005915, 0.000274),
    (75.187500, 0.001517, 0.008092),
    (75.270833, 0.000728, 0.008328),
    (75.354167, -0.006171, 0.001101),
    (75.541667, 0.000116, -0.007628),
    (75.645833, -0.018276, 0.030451),
    (75.875000, -0.003728, -0.000040),
    (76.229167, -0.007502, -0.007482),
    (76.458333, 0.000080, 0.021885),
    (76.541667, 0.000081, -0.026334),
    (76.708333, -0.001864, -0.022653),
    (77.020833, -0.021175, 0.000534),
    (77.187500, 0.000020, -0.004660),
    (77.291667, 0.000025, -0.018351),
    (77.770833, 0.000069, 0.026375),
    (78.041667, 0.000053, -0.028228),
    (78.125000, -0.005959, -0.005175),
    (78.229167, 0.008367, 0.000997),
    (78.333333, -0.010636, 0.001299),
    (78.541667, 0.008535, -0.004596),
    (78.666667, 0.018391, 0.001069),
    (78.770833, -0.021770, 0.000813),
    (78.812500, -0.053280, 0.001130),
    (79.145833, 0.026161, 0.001141),
    (79.291667, 0.000796, 0.001138),
    (79.770833, -0.000179, 0.009610),
    (80.208333, -0.000169, 0.032210),
    (80.312500, -0.000167, 0.016362),
    (80.541667, -0.000186, -0.007733),
    (80.687500, -0.000159, 0.007994),
    (80.833333, -0.000156, 0.012498),
    (81.375000, -0.000145, 0.007478),
    (81.833333, -0.000112, -0.005164),
    (81.937500, 0.005978, 0.006251),
    (82.000000, -0.005411, 0.003731),
    (82.125000, -0.027844, 0.000746),
    (82.333333, -0.001540, 0.000810),
    (82.437500, 0.009549, -0.010050),
    (82.500000, -0.029441, -0.001208),
    (82.666667, 0.000523, -0.003347),
    (82.770833, 0.016740, -0.000193),
    (82.854167, 0.010548, -0.000104),
    (82.916667, -0.034054, -0.000100),
    (83.312500, 0.002537, -0.000094),
    (83.395833, 0.022562, -0.000092),
    (83.625000, 0.001676, -0.000090),
    (83.791667, 0.041369, -0.000089),
    (84.041667, 0.000959, -0.009704),
    (84.395833, 0.000911, -0.029957),
    (84.458333, 0.000904, 0.020912),
    (84.541667, -0.002376, 0.001629),
    (84.729167, 0.000863, 0.002297),
    (84.916667, 0.000871, 0.011632),
];
// taps=256

fn space_ms_from_n(n: f64) -> f64 {
    let n = n.clamp(0.0, 1.0);
    if n <= 0.5 {
        SPACE_DISPLAY_MS[0] * (SPACE_DISPLAY_MS[1] / SPACE_DISPLAY_MS[0]).powf(n / 0.5)
    } else {
        SPACE_DISPLAY_MS[1] * (SPACE_DISPLAY_MS[2] / SPACE_DISPLAY_MS[1]).powf((n - 0.5) / 0.5)
    }
}

fn space_n_from_ms(ms: f64) -> f64 {
    let ms = ms.clamp(SPACE_DISPLAY_MS[0], SPACE_DISPLAY_MS[2]);
    if ms <= SPACE_DISPLAY_MS[1] {
        0.5 * (ms / SPACE_DISPLAY_MS[0]).ln() / (SPACE_DISPLAY_MS[1] / SPACE_DISPLAY_MS[0]).ln()
    } else {
        0.5 + 0.5 * (ms / SPACE_DISPLAY_MS[1]).ln()
            / (SPACE_DISPLAY_MS[2] / SPACE_DISPLAY_MS[1]).ln()
    }
}

fn space_band_t60(n: f64) -> [f64; TILT_BANDS] {
    let n = n.clamp(0.0, 1.0);
    let mut band = [SPACE_BAND_T60[0][0]; TILT_BANDS];
    for i in 1..SPACE_T60_N.len() {
        if n <= SPACE_T60_N[i] {
            let span = SPACE_T60_N[i] - SPACE_T60_N[i - 1];
            let frac = (n - SPACE_T60_N[i - 1]) / span;
            for index in 0..TILT_BANDS {
                band[index] = SPACE_BAND_T60[i - 1][index]
                    + (SPACE_BAND_T60[i][index] - SPACE_BAND_T60[i - 1][index]) * frac;
            }
            return band;
        }
    }
    SPACE_BAND_T60[SPACE_BAND_T60.len() - 1]
}

fn measured_brightness_ratios(brightness_percent: f64) -> [f64; TILT_BANDS] {
    let b = brightness_percent.clamp(0.0, 75.0);
    let mut index = 0;
    while index + 2 < BRIGHT_POINTS.len() && b > BRIGHT_POINTS[index + 1] {
        index += 1;
    }
    let a = BRIGHT_POINTS[index];
    let span = (BRIGHT_POINTS[index + 1] - a).max(1e-9);
    let frac = ((b - a) / span).clamp(0.0, 1.0);
    let mut ratios = [1.0; TILT_BANDS];
    for band in 0..TILT_BANDS {
        ratios[band] = BRIGHT_RATIO[band][index]
            + (BRIGHT_RATIO[band][index + 1] - BRIGHT_RATIO[band][index]) * frac;
    }
    ratios
}

/// Brightness tilt for the whole control range.
///
/// The measured table ends at 75%, where every band still decays faster than
/// the 1 kHz loop-gain reference. Above 75% the tilt is removed linearly: the
/// endpoint at 100% gives every band the slowest band's 75% RT30, so the
/// feedback chain has no frequency tilt left and only the tank's one-pole
/// absorption still touches the top octave.
fn brightness_ratios(
    brightness_percent: f64,
    space_band_t60: &[f64; TILT_BANDS],
) -> [f64; TILT_BANDS] {
    if brightness_percent <= 75.0 {
        return measured_brightness_ratios(brightness_percent);
    }
    let measured = measured_brightness_ratios(75.0);
    let mut flat_t60 = 0.0f64;
    for band in 0..TILT_BANDS {
        flat_t60 = flat_t60.max(space_band_t60[band] * measured[band]);
    }
    let frac = ((brightness_percent.clamp(75.0, 100.0) - 75.0) / 25.0).clamp(0.0, 1.0);
    let mut ratios = [1.0; TILT_BANDS];
    for band in 0..TILT_BANDS {
        let flat_ratio = flat_t60 / space_band_t60[band];
        ratios[band] = measured[band] + (flat_ratio - measured[band]) * frac;
    }
    ratios
}

fn er_level(n: f64) -> f64 {
    let percent = n.clamp(0.0, 1.0) * 100.0;
    interpolate(&ER_LEVEL_N, &ER_LEVEL, percent)
}

fn distance_gain(distance_percent: f64) -> f64 {
    interpolate(&ER_DISTANCE_N, &ER_DISTANCE_GAIN, distance_percent)
}

fn interpolate(nodes: &[f64], values: &[f64], x: f64) -> f64 {
    if x <= nodes[0] {
        return values[0];
    }
    for index in 1..nodes.len() {
        if x <= nodes[index] {
            let span = (nodes[index] - nodes[index - 1]).max(1e-9);
            let frac = (x - nodes[index - 1]) / span;
            return values[index - 1] + (values[index] - values[index - 1]) * frac;
        }
    }
    values[values.len() - 1]
}

fn er_pole(sample_rate: f64) -> f32 {
    (-std::f64::consts::TAU * ER_CUTOFF_HZ / sample_rate).exp() as f32
}

fn er_pole_of(cutoff_hz: f64, sample_rate: f64) -> f32 {
    (-std::f64::consts::TAU * cutoff_hz / sample_rate).exp() as f32
}

/// FDN input damping cutoff against Brightness, matched to the measured
/// late-field octave spectrum. At 100% the cutoff reaches the 16 kHz tilt
/// breakpoint, so the absorption does not cut the top octave.
const TANK_DAMPING_N: [f64; 5] = [0.0, 25.0, 50.0, 75.0, 100.0];
const TANK_DAMPING_HZ: [f64; 5] = [1_640.0, 2_350.0, 5_850.0, 11_200.0, 16_000.0];

fn tank_damping_cutoff(brightness: f64) -> f64 {
    let b = brightness.clamp(0.0, 100.0);
    for index in 1..TANK_DAMPING_N.len() {
        if b <= TANK_DAMPING_N[index] {
            let span = TANK_DAMPING_N[index] - TANK_DAMPING_N[index - 1];
            let frac = (b - TANK_DAMPING_N[index - 1]) / span;
            return TANK_DAMPING_HZ[index - 1]
                + (TANK_DAMPING_HZ[index] - TANK_DAMPING_HZ[index - 1]) * frac;
        }
    }
    TANK_DAMPING_HZ[TANK_DAMPING_HZ.len() - 1]
}

#[derive(Clone)]
struct DelayLine {
    buffer: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    fn write(&mut self, value: f32) {
        self.buffer[self.pos] = value;
        self.pos += 1;
        if self.pos == self.buffer.len() {
            self.pos = 0;
        }
    }

    fn read_oldest(&self) -> f32 {
        self.buffer[self.pos]
    }

    fn read_delay(&self, delay: usize) -> f32 {
        let len = self.buffer.len();
        let delay = delay.min(len - 1);
        self.buffer[(self.pos + len - 1 - delay) % len]
    }
}

struct Allpass {
    line: DelayLine,
    gain: f32,
}

impl Allpass {
    fn new(delay_ms: f64, sample_rate: u32, gain: f32) -> Self {
        let len = ((delay_ms * 0.001 * sample_rate as f64).round() as usize).max(1);
        Self {
            line: DelayLine::new(len),
            gain,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.line.read_oldest();
        let output = delayed - self.gain * input;
        self.line.write(input + self.gain * output);
        output
    }
}

struct FdnLine {
    line: DelayLine,
    gain: f32,
    input_gain: f32,
    tilt: [BiquadCoefficients; TILT_BANDS],
    state: [BiquadState; TILT_BANDS],
}

impl FdnLine {
    fn new(delay_ms: f64, sample_rate: u32) -> Self {
        let len = ((delay_ms * 0.001 * sample_rate as f64).round() as usize).max(1);
        let tilt = std::array::from_fn(|band| {
            BiquadCoefficients::design(
                FilterType::Peak,
                TILT_HZ[band],
                1.0,
                0.0,
                sample_rate as f64,
            )
        });
        Self {
            line: DelayLine::new(len),
            gain: 0.0,
            input_gain: 0.0,
            tilt,
            state: [BiquadState::default(); TILT_BANDS],
        }
    }

    fn recompute(&mut self, delay_seconds: f64, band_t60: &[f64; TILT_BANDS], sample_rate: f64) {
        let t60_mid = band_t60[1];
        self.gain = 10.0f64.powf(-3.0 * delay_seconds / t60_mid) as f32;
        // Input injection is weaker than the loop gain at short T60; the
        // exponent is calibrated to the measured short-Space RT60.
        self.input_gain = self.gain.powf(0.8);
        let mut target = [0.0; TILT_BANDS];
        for band in 0..TILT_BANDS {
            target[band] = 60.0 * delay_seconds * (1.0 / t60_mid - 1.0 / band_t60[band]);
        }
        let mut coefficients = self.tilt;
        for _ in 0..3 {
            for band in 0..TILT_BANDS {
                let mut current = 0.0;
                for coefficient in &coefficients {
                    current += coefficient.response_db(TILT_HZ[band], sample_rate);
                }
                let own = coefficients[band].response_db(TILT_HZ[band], sample_rate);
                let gain = (target[band] - (current - own)).clamp(-60.0, 60.0);
                coefficients[band] = if band == TILT_BANDS - 1 {
                    BiquadCoefficients::design(
                        FilterType::HighShelf,
                        12_000.0,
                        0.707,
                        gain,
                        sample_rate,
                    )
                } else {
                    BiquadCoefficients::design(
                        FilterType::Peak,
                        TILT_HZ[band],
                        1.3,
                        gain,
                        sample_rate,
                    )
                };
            }
        }
        self.tilt = coefficients;
    }

    fn process(&mut self, output: f32) -> f32 {
        let mut value = output as f64;
        for band in 0..TILT_BANDS {
            value = self.state[band].process(&self.tilt[band], value);
        }
        (value * self.gain as f64) as f32
    }

    fn clear(&mut self) {
        self.line.buffer.fill(0.0);
        self.line.pos = 0;
        for state in &mut self.state {
            state.reset();
        }
    }
}

fn hadamard<const N: usize>(values: &mut [f32; N]) {
    let mut half = 1;
    while half < N {
        let mut base = 0;
        while base < N {
            for index in base..base + half {
                let a = values[index];
                let b = values[index + half];
                values[index] = a + b;
                values[index + half] = a - b;
            }
            base += half * 2;
        }
        half *= 2;
    }
}

struct Tank<const N: usize> {
    lines: [FdnLine; N],
    scale: f32,
    input_gain: f32,
    output_gain: f32,
    taps: [f32; N],
}

impl<const N: usize> Tank<N> {
    fn new(delays_ms: [f64; N], sample_rate: u32) -> Self {
        Self {
            lines: std::array::from_fn(|index| FdnLine::new(delays_ms[index], sample_rate)),
            scale: 1.0 / (N as f32).sqrt(),
            input_gain: 1.0 / (N as f32).sqrt(),
            // The 2-line tank's share of the late field is not separable in the
            // measurement; this level is a deliberate calibration.
            output_gain: if N == TANK_LINES { 0.12 } else { 1.0 },
            taps: std::array::from_fn(|index| TAP_SIGNS[index]),
        }
    }

    fn recompute(&mut self, band_t60: &[f64; TILT_BANDS], sample_rate: u32) {
        for line in &mut self.lines {
            let delay_seconds = line.line.buffer.len() as f64 / sample_rate as f64;
            line.recompute(delay_seconds, band_t60, sample_rate as f64);
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut output = [0.0f32; N];
        let mut feedback = [0.0f32; N];
        for ((output, feedback), line) in output
            .iter_mut()
            .zip(feedback.iter_mut())
            .zip(self.lines.iter_mut())
        {
            *output = line.line.read_oldest();
            *feedback = line.process(*output);
        }
        hadamard(&mut feedback);
        for value in &mut feedback {
            *value *= self.scale;
        }
        for (line, &feedback) in self.lines.iter_mut().zip(feedback.iter()) {
            // Input injection follows the per-line decay gain; the exponent is
            // calibrated to the measured short-Space RT60.
            let value = input * self.input_gain * line.gain.powf(0.8) + feedback;
            line.line.write(if value.abs() < DENORMAL_FLOOR {
                0.0
            } else {
                value
            });
        }
        let sum: f32 = output
            .iter()
            .zip(self.taps.iter())
            .map(|(output, tap)| output * tap)
            .sum();
        sum * self.output_gain * self.scale
    }

    fn clear(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
    }
}

/// Deterministic xorshift walk scaled to a uniform 24-bit value.
struct RandomModulator {
    state: u32,
    phase: f64,
    /// Rate-independent phase step; `rate` scales it at use, so changing the
    /// rate never rewinds or skips part of the sequence.
    step: f64,
    current: f64,
    target: f64,
    rate: f64,
    mode: u8,
}

impl RandomModulator {
    fn seeded(seed: u32, mode: u8) -> Self {
        let mut modulator = Self {
            state: (seed & 0x00ff_ffff).max(1),
            phase: 0.0,
            step: 0.01,
            current: 0.5,
            target: 0.5,
            rate: 0.01,
            mode,
        };
        modulator.advance();
        modulator
    }

    fn next_uniform(&mut self) -> f64 {
        let state = self.state;
        let mut next = (state << 1) & 0x00ff_ffff;
        if state & 0x0080_0000 != 0 {
            next ^= 0x0011_d872b;
        }
        self.state = if next == 0 { 1 } else { next };
        (next as f64 - 8_388_608.0) * (1.0 / 16_777_216.0) + 0.5
    }

    fn advance(&mut self) {
        self.current = self.target;
        let walk = self.next_uniform() * 2.0 - 1.0;
        let mut candidate = self.current + walk;
        if candidate < 0.0 {
            candidate = -candidate * 0.9;
        }
        if candidate > 1.0 {
            candidate = 2.0 - candidate;
            candidate = 1.0 - (1.0 - candidate) * 0.9;
        }
        self.target = candidate.clamp(0.0, 1.0);
        self.phase = 0.0;
        self.step = self.next_uniform() + 0.8;
    }

    fn next_sample(&mut self) -> f64 {
        let x = self.phase.clamp(0.0, 1.0);
        let shaped = if self.mode == 1 {
            x * x * (3.0 - 2.0 * x)
        } else {
            x
        };
        let value = self.current + (self.target - self.current) * shaped;
        self.phase += self.step * self.rate;
        if self.phase >= 1.0 {
            self.advance();
        }
        value
    }

    fn reseed(&mut self, seed: u32) {
        self.state = (seed & 0x00ff_ffff).max(1);
        self.phase = 0.0;
        self.current = 0.5;
        self.target = 0.5;
        self.advance();
    }
}

struct ModulatedStage {
    line: DelayLine,
    time_mod: RandomModulator,
    amp_mod: RandomModulator,
    base_delay: f64,
    depth: f64,
    amp_depth: f64,
    blend: f64,
}

impl ModulatedStage {
    fn new(sample_rate: u32, base_ms: f64, seed: u32) -> Self {
        let len = ((0.030 * sample_rate as f64) as usize).max(4);
        Self {
            line: DelayLine::new(len),
            time_mod: RandomModulator::seeded(seed, 1),
            amp_mod: RandomModulator::seeded(seed ^ AMP_SEED, 1),
            base_delay: base_ms * 0.001 * sample_rate as f64,
            depth: 0.0,
            amp_depth: 0.0,
            blend: 1.0,
        }
    }

    /// Restore both modulators to the start of the sequence `seed` names.
    fn reseed(&mut self, seed: u32) {
        self.time_mod.reseed(seed);
        self.amp_mod.reseed(seed ^ AMP_SEED);
    }

    fn configure(
        &mut self,
        rate_hz: f64,
        depth: f64,
        amp_depth: f64,
        blend: f64,
        sample_rate: u32,
    ) {
        let raw_rate = rate_hz / (1.3 * sample_rate as f64);
        self.time_mod.rate = raw_rate;
        self.amp_mod.rate = raw_rate;
        self.depth = depth;
        self.amp_depth = amp_depth;
        self.blend = blend;
    }

    fn process(&mut self, input: f32) -> f32 {
        self.line.write(input);
        let time = self.time_mod.next_sample() * 2.0 - 1.0;
        let amp = self.amp_mod.next_sample() * 2.0 - 1.0;
        let limit = (self.line.buffer.len() - 2) as f64;
        let read = (self.base_delay + time * self.depth).clamp(1.0, limit);
        let index = read.floor() as usize;
        let frac = (read - index as f64) as f32;
        let delayed =
            self.line.read_delay(index) * (1.0 - frac) + self.line.read_delay(index + 1) * frac;
        let modulated = delayed * (1.0 + amp as f32 * self.amp_depth as f32);
        modulated * self.blend as f32 + input * (1.0 - self.blend as f32)
    }

    fn clear(&mut self) {
        self.line.buffer.fill(0.0);
        self.line.pos = 0;
    }
}

/// A long-form feedback-delay-network reverb.
pub struct ReverbFdn {
    sample_rate: u32,
    tank16: [Tank<FDN_LINES>; 2],
    tank2: [Tank<TANK_LINES>; 2],
    diffuser: [[Allpass; 6]; 2],
    early: [DelayLine; 2],
    er_taps: [(u32, f32, f32); 256],
    er_gain: f32,
    er_level: f64,
    er_pole: f32,
    er_state: [f32; 2],
    tank_damp: [[f32; 2]; 2],
    tank_damp_pole: f32,
    predelay: [DelayLine; 2],
    predelay_samples: usize,
    stage1: [ModulatedStage; 2],
    stage2: [ModulatedStage; 2],
    character_on: bool,
    seed: u32,
    space_percent: f64,
    decay_ms: f64,
    decay_rate_percent: f64,
    brightness_percent: f64,
    character_percent: f64,
    distance_percent: f64,
    predelay_ms: f64,
    width_percent: f64,
    mix_percent: f64,
    t60_mid: f64,
    band_t60: [f64; TILT_BANDS],
    decay_band_db: [f64; TILT_BANDS],
    decay_eq: [DecayEqBand; TILT_BANDS],
    post_eq: [PostEqBand; POST_EQ_BANDS],
    post_eq_state: [[BiquadState; POST_EQ_BANDS]; 2],
    post_eq_active: bool,
}

impl ReverbFdn {
    /// Build a device with the default modulation seed.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_seed(sample_rate, DEFAULT_SEED)
    }

    /// Build a device whose Character sequence comes from `seed`.
    ///
    /// A renderer seeds from the device's place in the project so that the
    /// same project always renders the same audio. Seeds must not come from
    /// the clock, an address or a run counter.
    pub fn with_seed(sample_rate: u32, seed: u32) -> Self {
        let seed = seed.max(1);
        let rate = sample_rate as f64;
        let predelay_len = (0.5 * rate) as usize + 1;
        let early_len = (0.2 * rate) as usize + 1;
        let er_pole_value = er_pole(rate);
        let er_gain_scale = (1.0 - er_pole(48_000.0)) / (1.0 - er_pole_value);
        let er_taps = std::array::from_fn(|index| {
            let (delay_ms, gain_l, gain_r) = ER_TAPS[index];
            let delay = (((delay_ms as f64 * 0.001 * rate).round() as i64) - 1).max(1) as u32;
            (delay, gain_l * er_gain_scale, gain_r * er_gain_scale)
        });
        let mut effect = Self {
            sample_rate,
            tank16: [
                Tank::new(FDN_MS[0], sample_rate),
                Tank::new(FDN_MS[1], sample_rate),
            ],
            tank2: [
                Tank::new(TANK_MS[0], sample_rate),
                Tank::new(TANK_MS[1], sample_rate),
            ],
            diffuser: [
                std::array::from_fn(|index| {
                    Allpass::new(DIFFUSER_MS[0][index], sample_rate, DIFFUSER_GAIN)
                }),
                std::array::from_fn(|index| {
                    Allpass::new(DIFFUSER_MS[1][index], sample_rate, DIFFUSER_GAIN)
                }),
            ],
            early: [DelayLine::new(early_len), DelayLine::new(early_len)],
            er_taps,
            er_gain: 1.0,
            er_level: 1.0,
            er_pole: er_pole_value,
            er_state: [0.0; 2],
            tank_damp: [[0.0; 2]; 2],
            tank_damp_pole: 0.0,
            predelay: [DelayLine::new(predelay_len), DelayLine::new(predelay_len)],
            predelay_samples: 0,
            stage1: [
                ModulatedStage::new(sample_rate, 5.0, stage_seed(seed, 0, 0)),
                ModulatedStage::new(sample_rate, 5.3, stage_seed(seed, 0, 1)),
            ],
            stage2: [
                ModulatedStage::new(sample_rate, 7.0, stage_seed(seed, 1, 0)),
                ModulatedStage::new(sample_rate, 7.4, stage_seed(seed, 1, 1)),
            ],
            character_on: false,
            seed,
            space_percent: 50.0,
            decay_ms: 2500.0,
            decay_rate_percent: 100.0,
            brightness_percent: 50.0,
            character_percent: 30.0,
            distance_percent: 50.0,
            predelay_ms: 0.0,
            width_percent: 100.0,
            mix_percent: 30.0,
            t60_mid: 2.85,
            band_t60: [1.0; TILT_BANDS],
            decay_band_db: [0.0; TILT_BANDS],
            decay_eq: std::array::from_fn(|band| DecayEqBand::flat(TILT_HZ[band])),
            post_eq: [
                PostEqBand::flat(FilterType::Peak, 100.0),
                PostEqBand::flat(FilterType::Peak, 300.0),
                PostEqBand::flat(FilterType::Peak, 1_000.0),
                PostEqBand::flat(FilterType::Peak, 3_000.0),
                PostEqBand::flat(FilterType::Peak, 8_000.0),
                PostEqBand::flat(FilterType::Peak, 16_000.0),
            ],
            post_eq_state: [[BiquadState::default(); POST_EQ_BANDS]; 2],
            post_eq_active: false,
        };
        effect.recompute_space();
        effect.recompute_post_eq();
        effect.recompute_distance();
        effect.recompute_character();
        effect
    }

    /// Select the Character modulation sequence for this instance.
    ///
    /// Reseeding rewinds every modulator to the start of the named sequence,
    /// so a render that sets the same seed twice produces the same audio.
    pub fn set_seed(&mut self, seed: u32) {
        self.seed = seed.max(1);
        self.reseed_modulators();
    }

    fn reseed_modulators(&mut self) {
        for channel in 0..2 {
            self.stage1[channel].reseed(stage_seed(self.seed, 0, channel));
            self.stage2[channel].reseed(stage_seed(self.seed, 1, channel));
        }
    }

    fn recompute_space(&mut self) {
        let n = (self.space_percent / 100.0).clamp(0.0, 1.0);
        let multiplier = (self.decay_rate_percent / 100.0).clamp(0.01, 10.0);
        let mut band_t60 = space_band_t60(n);
        let ratios = brightness_ratios(self.brightness_percent, &band_t60);
        for (band, ratio) in band_t60.iter_mut().zip(ratios) {
            *band *= ratio * multiplier;
        }
        let trims = self.decay_eq_trims_db();
        for (band, trim_db) in band_t60.iter_mut().zip(trims) {
            if trim_db != 0.0 {
                *band *= 10.0_f64.powf(trim_db / 20.0);
            }
        }
        self.band_t60 = band_t60;
        self.t60_mid = band_t60[1];
        self.tank_damp_pole = er_pole_of(
            tank_damping_cutoff(self.brightness_percent),
            self.sample_rate as f64,
        );
        for channel in 0..2 {
            self.tank16[channel].recompute(&band_t60, self.sample_rate);
            self.tank2[channel].recompute(&band_t60, self.sample_rate);
        }
        self.er_level = er_level(n);
    }

    /// Gain in dB that each feedback band's trim contributes, before it is
    /// folded into the RT30 targets. `None` shapes keep the bare band trim.
    fn decay_eq_trims_db(&self) -> [f64; TILT_BANDS] {
        let rate = self.sample_rate as f64;
        let mut trims = [0.0; TILT_BANDS];
        for (band, eq) in self.decay_eq.iter().enumerate() {
            let trim = self.decay_band_db[band];
            if trim == 0.0 {
                continue;
            }
            match eq.shape.filter_type() {
                None => trims[band] += trim,
                Some(filter_type) => {
                    let coefficients =
                        BiquadCoefficients::design(filter_type, eq.freq, eq.q, trim, rate);
                    for (target, value) in trims.iter_mut().enumerate() {
                        *value += coefficients.response_db(TILT_HZ[target], rate);
                    }
                }
            }
        }
        trims
    }

    fn recompute_post_eq(&mut self) {
        let rate = self.sample_rate as f64;
        let mut active = false;
        for band in &mut self.post_eq {
            band.rebuild(rate);
            active |= band.engaged();
        }
        self.post_eq_active = active;
    }

    /// Combined post-EQ magnitude response in dB at `freq_hz`.
    pub fn post_eq_response_db(&self, freq_hz: f64) -> f64 {
        if !self.post_eq_active {
            return 0.0;
        }
        self.post_eq
            .iter()
            .filter(|band| band.engaged())
            .map(|band| {
                band.coefficients
                    .response_db(freq_hz, self.sample_rate as f64)
            })
            .sum()
    }

    fn recompute_distance(&mut self) {
        let distance = (self.distance_percent / 100.0).clamp(0.0, 1.0);
        self.er_gain = distance_gain(self.distance_percent) as f32;
        let shift_ms = ER_DISTANCE_SHIFT_MS * ((distance - 0.5) / 0.25).clamp(0.0, 1.0);
        let rate = self.sample_rate as f64;
        let er_gain_scale = ((1.0 - er_pole(48_000.0) as f64) / (1.0 - self.er_pole as f64)) as f32;
        for (&(delay_ms, gain_l, gain_r), tap) in ER_TAPS.iter().zip(self.er_taps.iter_mut()) {
            let shifted = (delay_ms as f64 - shift_ms).max(0.1);
            let delay = (((shifted * 0.001 * rate).round() as i64) - 1).max(1) as u32;
            *tap = (delay, gain_l * er_gain_scale, gain_r * er_gain_scale);
        }
    }

    fn recompute_character(&mut self) {
        let character = (self.character_percent / 100.0).clamp(0.0, 1.0);
        self.character_on = character > 0.0;
        if !self.character_on {
            return;
        }
        let m = ((2.0 * character - 0.8).powi(2) + 0.21).min(1.0);
        let (p, q) = if character < 0.7 {
            let x = character / 0.7;
            (((2.0 - x * x).sqrt() - 1.0) * 0.8 + 1.0, 0.0)
        } else {
            (1.0 + 0.5 * character, 1.0 - 1.25 * character)
        };
        let rate = 0.4 + 3.5 * character * character;
        let time_depth = 0.00025 * character * self.sample_rate as f64;
        // Amplitude depth is calibrated to the measured envelope ripple.
        let amp_scale = 1.4 + 5.0 * (1.0 - character) * (1.0 - character);
        for channel in 0..2 {
            self.stage1[channel].configure(
                rate,
                time_depth * 0.6,
                0.35 * character * amp_scale,
                m,
                self.sample_rate,
            );
            self.stage2[channel].configure(
                rate * p,
                time_depth * 0.4,
                (0.05 + 0.7 * character) * (1.0 - 0.2 * q).max(0.0) * amp_scale,
                1.0,
                self.sample_rate,
            );
        }
    }

    fn set_space_percent(&mut self, percent: f64) {
        self.space_percent = percent.clamp(0.0, 100.0);
        self.decay_ms = space_ms_from_n(self.space_percent / 100.0);
        self.recompute_space();
    }

    /// Mid-band T60 target currently in force, in seconds.
    pub fn t60_seconds(&self) -> f64 {
        self.t60_mid
    }

    /// T60 target of the six feedback tilt bands, in seconds.
    pub fn tilt_t60(&self) -> [f64; TILT_BANDS] {
        self.band_t60
    }
}

impl AudioEffect for ReverbFdn {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let width = (self.width_percent / 100.0) as f32;
        let wet = (self.mix_percent / 100.0) as f32;
        let dry = 1.0 - wet;
        let er_gain = self.er_gain * self.er_level as f32;
        let late_gain = LATE_LEVEL as f32;
        let pole = self.er_pole;
        let character_on = self.character_on;
        let predelay = self.predelay_samples;
        let post_eq_active = self.post_eq_active;
        let post_eq = self.post_eq;
        let post_eq_engaged = self.post_eq.map(|band| band.engaged());
        let mut index = 0;
        while index + 1 < buffer.len() {
            let in_l = buffer[index];
            let in_r = buffer[index + 1];

            self.predelay[0].write(in_l);
            self.predelay[1].write(in_r);
            let delayed_l = self.predelay[0].read_delay(predelay);
            let delayed_r = self.predelay[1].read_delay(predelay);

            self.early[0].write(delayed_l);
            self.early[1].write(delayed_r);
            let mut early_l = 0.0f32;
            let mut early_r = 0.0f32;
            for &(delay, gain_l, gain_r) in &self.er_taps {
                early_l += self.early[0].read_delay(delay as usize) * gain_l;
                early_r += self.early[1].read_delay(delay as usize) * gain_r;
            }
            self.er_state[0] = (1.0 - pole) * early_l + pole * self.er_state[0];
            self.er_state[1] = (1.0 - pole) * early_r + pole * self.er_state[1];
            let early_l = self.er_state[0] * er_gain;
            let early_r = self.er_state[1] * er_gain;

            let pole = self.tank_damp_pole;
            self.tank_damp[0][0] = (1.0 - pole) * delayed_l + pole * self.tank_damp[0][0];
            self.tank_damp[0][1] =
                (1.0 - pole) * self.tank_damp[0][0] + pole * self.tank_damp[0][1];
            self.tank_damp[1][0] = (1.0 - pole) * delayed_r + pole * self.tank_damp[1][0];
            self.tank_damp[1][1] =
                (1.0 - pole) * self.tank_damp[1][0] + pole * self.tank_damp[1][1];
            let mut diffuse_l = self.tank_damp[0][1];
            let mut diffuse_r = self.tank_damp[1][1];
            for stage in &mut self.diffuser[0] {
                diffuse_l = stage.process(diffuse_l);
            }
            for stage in &mut self.diffuser[1] {
                diffuse_r = stage.process(diffuse_r);
            }

            let mut late_l = self.tank16[0].process(diffuse_l) + self.tank2[0].process(diffuse_l);
            let mut late_r = self.tank16[1].process(diffuse_r) + self.tank2[1].process(diffuse_r);
            if character_on {
                late_l = self.stage1[0].process(late_l);
                late_l = self.stage2[0].process(late_l);
                late_r = self.stage1[1].process(late_r);
                late_r = self.stage2[1].process(late_r);
            }

            let mut out_l = early_l + late_l * late_gain;
            let mut out_r = early_r + late_r * late_gain;
            let mid = (out_l + out_r) * 0.5;
            let side = (out_l - out_r) * 0.5 * width;
            out_l = mid + side;
            out_r = mid - side;
            if post_eq_active {
                for band in 0..POST_EQ_BANDS {
                    if !post_eq_engaged[band] {
                        continue;
                    }
                    let coefficients = &post_eq[band].coefficients;
                    out_l =
                        self.post_eq_state[0][band].process(coefficients, f64::from(out_l)) as f32;
                    out_r =
                        self.post_eq_state[1][band].process(coefficients, f64::from(out_r)) as f32;
                }
            }

            buffer[index] = in_l * dry + out_l * wet;
            buffer[index + 1] = in_r * dry + out_r * wet;
            index += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        if let Some(band) = decay_band_index(name) {
            self.decay_band_db[band] = value.clamp(-24.0, 12.0);
            self.recompute_space();
            return;
        }
        if let Some((band, field)) = decay_eq_param(name) {
            let eq = &mut self.decay_eq[band];
            match field {
                "freq" => eq.freq = value.clamp(20.0, 20_000.0),
                "q" => eq.q = value.clamp(0.1, 10.0),
                "shape" => eq.shape = DecayEqShape::from_value(value),
                _ => return,
            }
            self.recompute_space();
            return;
        }
        if let Some((band, field)) = post_eq_param(name) {
            let post = &mut self.post_eq[band];
            match field {
                "freq" => post.freq = value.clamp(20.0, 20_000.0),
                "gain" => post.gain_db = value.clamp(-30.0, 30.0),
                "q" => post.q = value.clamp(0.05, 20.0),
                "shape" => post.filter_type = post_eq_filter_type(value),
                _ => return,
            }
            post.rebuild(self.sample_rate as f64);
            self.recompute_post_eq();
            return;
        }
        match name {
            "decay" => {
                self.decay_ms = value.clamp(200.0, 10_000.0);
                self.space_percent = space_n_from_ms(self.decay_ms) * 100.0;
                self.recompute_space();
            }
            "space" => self.set_space_percent(value),
            "decay_rate" => {
                self.decay_rate_percent = value.clamp(50.0, 200.0);
                self.recompute_space();
            }
            "brightness" => {
                self.brightness_percent = value.clamp(0.0, 100.0);
                self.recompute_space();
            }
            "damping" => {
                self.brightness_percent = (100.0 - value).clamp(0.0, 100.0);
                self.recompute_space();
            }
            "character" | "modulation" => {
                self.character_percent = value.clamp(0.0, 100.0);
                self.recompute_character();
            }
            "distance" => {
                self.distance_percent = value.clamp(0.0, 100.0);
                self.recompute_distance();
            }
            "early_late" => {
                self.distance_percent = (100.0 - value).clamp(0.0, 100.0);
                self.recompute_distance();
            }
            "predelay" => {
                self.predelay_ms = value.clamp(0.0, 500.0);
                self.predelay_samples =
                    (self.predelay_ms * 0.001 * self.sample_rate as f64).round() as usize;
            }
            "stereo_width" => self.width_percent = value.clamp(0.0, 120.0),
            "mix" => self.mix_percent = value.clamp(0.0, 100.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        if let Some(band) = decay_band_index(name) {
            return Some(self.decay_band_db[band]);
        }
        if let Some((band, field)) = decay_eq_param(name) {
            let eq = &self.decay_eq[band];
            return match field {
                "freq" => Some(eq.freq),
                "q" => Some(eq.q),
                "shape" => Some(eq.shape.value()),
                _ => None,
            };
        }
        if let Some((band, field)) = post_eq_param(name) {
            let post = &self.post_eq[band];
            return match field {
                "freq" => Some(post.freq),
                "gain" => Some(post.gain_db),
                "q" => Some(post.q),
                "shape" => Some(match post.filter_type {
                    FilterType::LowShelf => 1.0,
                    FilterType::HighShelf => 2.0,
                    FilterType::HighPass => 3.0,
                    FilterType::LowPass => 4.0,
                    FilterType::Notch => 5.0,
                    _ => 0.0,
                }),
                _ => None,
            };
        }
        match name {
            "decay" => Some(self.decay_ms),
            "space" => Some(self.space_percent),
            "decay_rate" => Some(self.decay_rate_percent),
            "brightness" => Some(self.brightness_percent),
            "damping" => Some(100.0 - self.brightness_percent),
            "character" | "modulation" => Some(self.character_percent),
            "distance" => Some(self.distance_percent),
            "early_late" => Some(100.0 - self.distance_percent),
            "predelay" => Some(self.predelay_ms),
            "stereo_width" => Some(self.width_percent),
            "mix" => Some(self.mix_percent),
            _ => None,
        }
    }

    fn reset(&mut self) {
        for tank in &mut self.tank16 {
            tank.clear();
        }
        for tank in &mut self.tank2 {
            tank.clear();
        }
        for channel in 0..2 {
            for stage in &mut self.diffuser[channel] {
                stage.line.buffer.fill(0.0);
                stage.line.pos = 0;
            }
            self.early[channel].buffer.fill(0.0);
            self.early[channel].pos = 0;
            self.predelay[channel].buffer.fill(0.0);
            self.predelay[channel].pos = 0;
        }
        self.er_state = [0.0; 2];
        self.tank_damp = [[0.0; 2]; 2];
        for stage in self.stage1.iter_mut().chain(self.stage2.iter_mut()) {
            stage.clear();
        }
        self.reseed_modulators();
        for channel in &mut self.post_eq_state {
            for state in channel {
                state.reset();
            }
        }
    }

    fn set_seed(&mut self, seed: u32) {
        ReverbFdn::set_seed(self, seed);
    }

    fn name(&self) -> &str {
        "reverb-fdn"
    }

    fn bounce_tail_samples(&self) -> u32 {
        // A fully dry reverb passes no wet signal, so a bounce keeps nothing.
        if self.mix_percent <= 0.0 {
            return 0;
        }
        // The slowest band sets the audible ring-out; the decay trims and the
        // decay rate are already folded into `band_t60`.
        let slowest = self
            .band_t60
            .iter()
            .fold(0.0_f64, |slowest, band| slowest.max(*band));
        let rate = f64::from(self.sample_rate.max(1));
        let seconds = slowest + self.predelay_ms * 0.001;
        (seconds * rate).round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse(frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        buffer[480 * 2] = 0.5;
        buffer[480 * 2 + 1] = 0.5;
        buffer
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * 2];
        let mut state = 0x1234_5678u32;
        for sample in &mut buffer {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *sample = (state >> 8) as f32 / 8_388_608.0 - 1.0;
        }
        buffer
    }

    fn render(effect: &mut ReverbFdn, mut buffer: Vec<f32>) -> Vec<f32> {
        effect.process(&mut buffer, 48_000);
        buffer
    }

    fn character_zero() -> ReverbFdn {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("character", 0.0);
        effect
    }

    fn wet_only() -> ReverbFdn {
        let mut effect = character_zero();
        effect.set_param("mix", 100.0);
        effect
    }

    #[test]
    fn mid_band_t60_matches_the_measured_space_table() {
        let measured = [0.22, 0.58, 1.13, 1.92, 2.85, 3.73, 5.06, 7.18, 11.01];
        for (index, n) in SPACE_T60_N.iter().enumerate() {
            assert!((space_band_t60(*n)[1] - measured[index]).abs() < 1e-9);
        }
        assert!((space_band_t60(0.5)[1] - 2.85).abs() < 1e-9);
    }

    #[test]
    fn space_tilt_matches_the_measured_per_band_table() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("brightness", 50.0);
        for n in [0.25, 0.5, 0.75, 1.0] {
            effect.set_param("space", n * 100.0);
            let t60 = effect.tilt_t60();
            let row = space_band_t60(n);
            for band in 0..TILT_BANDS {
                assert!(
                    (t60[band] - row[band]).abs() < 1e-9,
                    "space {n} band {band}"
                );
            }
        }
    }

    #[test]
    fn decay_rate_is_a_linear_multiplier() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 50.0);
        effect.set_param("decay_rate", 50.0);
        assert!((effect.t60_seconds() - 2.85 * 0.5).abs() < 1e-9);
        effect.set_param("decay_rate", 200.0);
        assert!((effect.t60_seconds() - 2.85 * 2.0).abs() < 1e-9);
    }

    #[test]
    fn space_seconds_round_trips_through_the_display_law() {
        for ms in [200.0, 750.0, 2_500.0, 4_600.0, 10_000.0] {
            let n = space_n_from_ms(ms);
            assert!((space_ms_from_n(n) - ms).abs() < 1e-6, "{ms}");
        }
    }

    #[test]
    fn brightness_tilt_matches_the_measured_t60_table() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 50.0);
        let cases = [
            (0.0, [2.74, 2.22, 1.89, 1.46]),
            (25.0, [2.80, 2.35, 2.06, 1.72]),
            (50.0, [2.85, 2.50, 2.23, 1.95]),
            (75.0, [2.88, 2.65, 2.42, 2.19]),
        ];
        for (brightness, [one_k, four_k, eight_k, sixteen_k]) in cases {
            effect.set_param("brightness", brightness);
            let t60 = effect.tilt_t60();
            for (band, expected) in [(1, one_k), (3, four_k), (4, eight_k), (5, sixteen_k)] {
                assert!(
                    (t60[band] - expected).abs() / expected < 0.05,
                    "brightness {brightness}: band {band} T60 {} vs {expected}",
                    t60[band]
                );
            }
        }
    }

    /// The measured law is frozen: adding the 75..100 extension must not move
    /// a single sample below 75%.
    #[test]
    fn measured_brightness_law_is_frozen_below_75() {
        let cases = [
            (
                0.0,
                crate::golden_references::select(
                    0x40df_18b6_149b_9969,
                    0xbd90_8f08_1b01_afb4,
                    0xbd90_8f08_1b01_afb4,
                ),
            ),
            (25.0, 0xc57b_6006_d6bf_4462),
            (50.0, 0xfba4_8c21_30fd_a57b),
            (
                75.0,
                crate::golden_references::select(
                    0xcb80_4cf6_0385_6284,
                    0x4c2c_c462_1b09_86d3,
                    0x4c2c_c462_1b09_86d3,
                ),
            ),
        ];
        let mut actual_hashes = Vec::new();
        let mut expected_hashes = Vec::new();
        for (brightness, expected) in cases {
            let mut effect = ReverbFdn::new(48_000);
            effect.set_param("space", 50.0);
            effect.set_param("character", 0.0);
            effect.set_param("mix", 100.0);
            effect.set_param("brightness", brightness);
            let input = noise(24_000);
            let input_hash = digest(&input);
            assert_eq!(
                input_hash, 0x9785_3415_3ebd_ef27,
                "reverb golden input at brightness {brightness}"
            );
            let buffer = render(&mut effect, input);
            let actual = digest(&buffer);
            let (power, max_abs) =
                buffer
                    .iter()
                    .fold((0.0_f64, 0.0_f64), |(power, max_abs), &sample| {
                        let value = f64::from(sample);
                        (power + value * value, max_abs.max(value.abs()))
                    });
            let rms = (power / buffer.len() as f64).sqrt();
            let nonfinite = buffer.iter().filter(|sample| !sample.is_finite()).count();
            eprintln!(
                "reverb_fdn golden brightness={brightness} samples={} input_hash={input_hash:016x} output_hash={actual:016x} expected={expected:016x} output_rms={rms:.12e} output_max_abs={max_abs:.12e} nonfinite={nonfinite}",
                buffer.len()
            );
            actual_hashes.push(actual);
            expected_hashes.push(expected);
        }
        assert_eq!(
            actual_hashes, expected_hashes,
            "all reverb brightness golden cases in declared order"
        );
    }

    #[test]
    fn brightness_extension_raises_the_top_octaves() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 50.0);
        let mut previous = [f64::NEG_INFINITY; TILT_BANDS];
        for brightness in [75.0, 80.0, 90.0, 100.0] {
            effect.set_param("brightness", brightness);
            let t60 = effect.tilt_t60();
            println!(
                "brightness {brightness}: 1k {:.4} 4k {:.4} 8k {:.4} 16k {:.4}",
                t60[1], t60[3], t60[4], t60[5]
            );
            for band in [1, 3, 4, 5] {
                assert!(
                    t60[band] >= previous[band] - 1e-12,
                    "brightness {brightness}: band {band} fell from {} to {}",
                    previous[band],
                    t60[band]
                );
            }
            previous = t60;
        }
        effect.set_param("brightness", 100.0);
        let full = effect.tilt_t60();
        effect.set_param("brightness", 75.0);
        let measured = effect.tilt_t60();
        assert!(
            full[4] - measured[4] >= 0.30,
            "8 kHz gained only {:.3} s",
            full[4] - measured[4]
        );
        assert!(
            full[5] - measured[5] >= 0.50,
            "16 kHz gained only {:.3} s",
            full[5] - measured[5]
        );
    }

    #[test]
    fn tank_damping_cutoff_opens_above_75() {
        assert_eq!(tank_damping_cutoff(75.0), 11_200.0);
        assert_eq!(tank_damping_cutoff(100.0), 16_000.0);
        let mut previous = tank_damping_cutoff(75.0);
        for brightness in [80.0, 90.0, 100.0] {
            let cutoff = tank_damping_cutoff(brightness);
            assert!(
                cutoff > previous,
                "cutoff at {brightness}%: {cutoff} Hz does not exceed {previous} Hz"
            );
            previous = cutoff;
        }
    }

    /// Long Hall, Dark as authored in the preset bank.
    fn long_hall_dark(brightness: f64) -> ReverbFdn {
        let mut effect = ReverbFdn::new(48_000);
        for (name, value) in [
            ("space", 74.0),
            ("decay_rate", 100.0),
            ("brightness", brightness),
            ("character", 55.0),
            ("distance", 62.0),
            ("predelay", 35.0),
            ("stereo_width", 95.0),
            ("mix", 22.0),
            ("decay_band1", 2.0),
            ("decay_band2", 1.0),
            ("decay_band5", -1.0),
            ("decay_band6", -3.0),
        ] {
            effect.set_param(name, value);
        }
        effect
    }

    /// Run-to-run floor of the tail measurement for this device.
    const PRESET_MEASUREMENT_FLOOR_DB: f64 = 3.31;

    fn band_tail_db(signal: &[f32], start_frame: usize) -> f64 {
        let rate = 48_000.0;
        let high_pass = BiquadCoefficients::design(FilterType::HighPass, 500.0, 0.707, 0.0, rate);
        let low_pass = BiquadCoefficients::design(FilterType::LowPass, 8_000.0, 0.707, 0.0, rate);
        let mut high_state = BiquadState::default();
        let mut low_state = BiquadState::default();
        let mut energy = 0.0;
        let mut count = 0usize;
        for (index, frame) in signal.chunks_exact(2).enumerate() {
            let filtered = low_state.process(
                &low_pass,
                high_state.process(&high_pass, f64::from(frame[0])),
            );
            if index >= start_frame {
                energy += filtered * filtered;
                count += 1;
            }
        }
        10.0 * (energy / count.max(1) as f64).max(1e-30).log10()
    }

    /// Per-octave decay of Long Hall, Dark at four Brightness values.
    ///
    /// The preset's Character modulation is bypassed and Mix is wet-only so
    /// the RT30 fit sees the late field instead of the dry impulse and the
    /// amplitude modulation.
    #[test]
    fn long_hall_dark_brightness_measurement() {
        let mut rows = Vec::new();
        for brightness in [34.0, 48.0, 75.0, 100.0] {
            let mut effect = long_hall_dark(brightness);
            effect.set_param("character", 0.0);
            effect.set_param("mix", 100.0);
            let signal = render(&mut effect, impulse(288_000));
            let octaves = std::array::from_fn::<f64, TILT_BANDS, _>(|band| {
                rt30_from_impulse(&signal, TILT_HZ[band])
            });
            let tail = band_tail_db(&signal, 48_000);
            println!("long-hall-dark {brightness}%: {octaves:?} tail 500-8000 {tail:.2} dB");
            rows.push((brightness, octaves, tail));
        }
        let row = |brightness: f64| {
            *rows
                .iter()
                .find(|(value, _, _)| *value == brightness)
                .expect("brightness row")
        };
        let (_, low, low_tail) = row(34.0);
        let (_, mid, _) = row(48.0);
        let (_, full, full_tail) = row(100.0);
        assert!(
            mid[4] >= low[4] * 1.02,
            "34 -> 48 moved 8 kHz from {:.3} s to {:.3} s",
            low[4],
            mid[4]
        );
        assert!(
            mid[5] >= low[5] * 1.03,
            "34 -> 48 moved 16 kHz from {:.3} s to {:.3} s",
            low[5],
            mid[5]
        );
        assert!(
            full_tail >= low_tail + PRESET_MEASUREMENT_FLOOR_DB,
            "500 Hz - 8 kHz tail at 100% is {full_tail:.2} dB, only {:.2} dB over 34%",
            full_tail - low_tail
        );
        for band in [3, 4, 5] {
            assert!(
                full[band] > mid[band] * 1.10,
                "band {band}: 100% {} s vs 48% {} s",
                full[band],
                mid[band]
            );
        }
    }

    /// The authored preset, including its Character modulation and dry/wet
    /// mix, must still gain tail energy when Brightness rises.
    #[test]
    fn long_hall_dark_preset_tail_gains_energy_at_full_brightness() {
        let mut tails = Vec::new();
        for brightness in [34.0, 48.0, 100.0] {
            let mut effect = long_hall_dark(brightness);
            let signal = render(&mut effect, impulse(288_000));
            let tail = band_tail_db(&signal, 48_000);
            println!("long-hall-dark preset {brightness}%: tail 500-8000 {tail:.2} dB");
            tails.push((brightness, tail));
        }
        let tail = |brightness: f64| {
            tails
                .iter()
                .find(|(value, _)| *value == brightness)
                .expect("brightness row")
                .1
        };
        assert!(
            tail(48.0) >= tail(34.0) + 1.0,
            "34 -> 48 gained only {:.2} dB",
            tail(48.0) - tail(34.0)
        );
        assert!(
            tail(100.0) >= tail(34.0) + PRESET_MEASUREMENT_FLOOR_DB,
            "100% has only {:.2} dB more tail than 34%",
            tail(100.0) - tail(34.0)
        );
    }

    #[test]
    fn feedback_gain_uses_the_per_line_delay_and_band_t60() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 50.0);
        for channel in 0..2 {
            for line in &effect.tank16[channel].lines {
                let delay = line.line.buffer.len() as f64 / 48_000.0;
                let expected = 10.0f64.powf(-3.0 * delay / effect.t60_seconds()).powi(1) as f32;
                assert!((line.gain - expected).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn feedback_eq_hits_the_band_t60_targets() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 50.0);
        effect.set_param("brightness", 50.0);
        let line = &effect.tank16[0].lines[0];
        let delay = line.line.buffer.len() as f64 / 48_000.0;
        let t60_mid = 2.85;
        for (band, &hz) in TILT_HZ.iter().enumerate() {
            let mut response = 0.0;
            for coefficient in &line.tilt {
                response += coefficient.response_db(hz, 48_000.0);
            }
            let target = 60.0 * delay * (1.0 / t60_mid - 1.0 / effect.tilt_t60()[band]);
            assert!(
                (response - target).abs() < 3.0,
                "band {band}: eq {response:.2} dB vs target {target:.2} dB"
            );
        }
    }

    #[test]
    fn early_reflection_first_tap_matches_distance() {
        for (distance, onset) in [(25.0, 320), (50.0, 320), (75.0, 48), (100.0, 48)] {
            let mut effect = wet_only();
            effect.set_param("distance", distance);
            let output = render(&mut effect, impulse(4096));
            let first = output
                .chunks_exact(2)
                .position(|frame| frame[0] != 0.0 || frame[1] != 0.0)
                .unwrap();
            assert_eq!(first, 480 + onset, "distance {distance}");
        }
    }

    #[test]
    fn distance_scales_the_er_energy() {
        let mut reference = wet_only();
        reference.set_param("distance", 50.0);
        let output = render(&mut reference, impulse(4600));
        let base: f32 = output[480..4600].iter().map(|value| value * value).sum();

        let mut louder = wet_only();
        louder.set_param("distance", 0.0);
        let output = render(&mut louder, impulse(4600));
        let near: f32 = output[480..4600].iter().map(|value| value * value).sum();
        assert!(near / base > 3.0, "distance 0 should be louder");

        let mut quieter = wet_only();
        quieter.set_param("distance", 100.0);
        let output = render(&mut quieter, impulse(4600));
        let far: f32 = output[480..4600].iter().map(|value| value * value).sum();
        assert!(far / base < 0.3, "distance 100 should be quieter");
    }

    #[test]
    fn predelay_shifts_the_onset() {
        let mut effect = wet_only();
        effect.set_param("predelay", 10.0);
        let output = render(&mut effect, impulse(4096));
        let onset = output
            .chunks_exact(2)
            .position(|frame| frame[0] != 0.0 || frame[1] != 0.0)
            .unwrap();
        assert_eq!(onset, 480 + 320 + 480, "10 ms predelay");
    }

    #[test]
    fn character_zero_is_bit_reproducible() {
        let first = render(&mut character_zero(), noise(24_000));
        let second = render(&mut character_zero(), noise(24_000));
        assert_eq!(first, second);
    }

    fn digest(buffer: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for sample in buffer {
            hash ^= sample.to_bits() as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    /// A Character>0 render must repeat: same settings, same seed, same audio.
    #[test]
    fn character_positive_is_reproducible_across_instances() {
        for character in [30.0, 60.0, 100.0] {
            let mut first = ReverbFdn::new(48_000);
            first.set_param("mix", 100.0);
            first.set_param("character", character);
            let mut second = ReverbFdn::new(48_000);
            second.set_param("mix", 100.0);
            second.set_param("character", character);
            let first = render(&mut first, noise(24_000));
            let second = render(&mut second, noise(24_000));
            assert!(
                first == second,
                "character {character} rendered two different tails"
            );
        }
    }

    /// A reset rewinds to the same sequence instead of picking a new one.
    #[test]
    fn reset_restores_the_character_sequence() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("mix", 100.0);
        effect.set_param("character", 60.0);
        let reference = digest(&render(&mut effect, noise(24_000)));
        effect.reset();
        let repeated = digest(&render(&mut effect, noise(24_000)));
        assert_eq!(reference, repeated, "reset changed the modulation sequence");
    }

    #[test]
    fn character_zero_bypasses_the_modulators() {
        let mut effect = character_zero();
        let reference = render(&mut effect, noise(24_000));
        effect.reset();
        effect.set_param("character", 0.0);
        assert_eq!(reference, render(&mut effect, noise(24_000)));
    }

    /// Determinism must not mean the modulation was removed.
    #[test]
    fn character_modulates_the_tail() {
        let mut plain = ReverbFdn::new(48_000);
        plain.set_param("mix", 100.0);
        plain.set_param("character", 0.0);
        let mut modulated = ReverbFdn::new(48_000);
        modulated.set_param("mix", 100.0);
        modulated.set_param("character", 60.0);
        let plain = render(&mut plain, noise(24_000));
        let modulated = render(&mut modulated, noise(24_000));
        let peak = plain
            .iter()
            .zip(&modulated)
            .map(|(plain, modulated)| (plain - modulated).abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 1e-3, "character 60 changed the tail by only {peak}");
    }

    /// Two seeds must decorrelate, so a render is not a static effect either.
    #[test]
    fn different_seeds_render_differently() {
        let mut first = ReverbFdn::with_seed(48_000, 0x1234_5678);
        first.set_param("mix", 100.0);
        first.set_param("character", 60.0);
        let mut second = ReverbFdn::with_seed(48_000, 0x8765_4321);
        second.set_param("mix", 100.0);
        second.set_param("character", 60.0);
        assert_ne!(
            digest(&render(&mut first, noise(24_000))),
            digest(&render(&mut second, noise(24_000)))
        );
    }

    /// Character 0 must not depend on the seed at all.
    #[test]
    fn character_zero_ignores_the_seed() {
        let mut first = ReverbFdn::with_seed(48_000, 1);
        first.set_param("character", 0.0);
        let mut second = ReverbFdn::with_seed(48_000, 0xffff_ffff);
        second.set_param("character", 0.0);
        let first = render(&mut first, noise(24_000));
        let second = render(&mut second, noise(24_000));
        assert!(first == second, "character 0 depends on the seed");
    }

    #[test]
    fn stereo_width_zero_is_exact_mono() {
        let mut effect = wet_only();
        effect.set_param("stereo_width", 0.0);
        let output = render(&mut effect, noise(24_000));
        for frame in output.chunks_exact(2) {
            assert_eq!(frame[0], frame[1]);
        }
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("space", 100.0);
        effect.set_param("decay_rate", 200.0);
        effect.set_param("brightness", 0.0);
        effect.set_param("character", 100.0);
        effect.set_param("distance", 0.0);
        effect.set_param("predelay", 500.0);
        effect.set_param("stereo_width", 120.0);
        effect.set_param("mix", 100.0);
        let output = render(&mut effect, noise(48_000));
        assert!(output.iter().all(|value| value.is_finite()));
        assert!(output.iter().all(|value| value.abs() < 100.0));
    }

    fn rt30_from_impulse(signal: &[f32], center: f64) -> f64 {
        let rate = 48_000.0;
        let coefficients = BiquadCoefficients::design(FilterType::BandPass, center, 6.0, 0.0, rate);
        let mut first = BiquadState::default();
        let mut second = BiquadState::default();
        let smooth = (-1.0 / (0.02 * rate)).exp();
        let mut energy = 0.0;
        let mut curve = Vec::new();
        for (index, frame) in signal.chunks_exact(2).enumerate() {
            let filtered = second.process(
                &coefficients,
                first.process(&coefficients, f64::from(frame[0])),
            );
            energy = smooth * energy + (1.0 - smooth) * filtered * filtered;
            if index >= 4_800 {
                curve.push((index as f64 / rate, 10.0 * energy.max(1e-30).log10()));
            }
        }
        let peak = curve
            .iter()
            .fold(f64::NEG_INFINITY, |peak, (_, db)| peak.max(*db));
        let start = curve
            .iter()
            .find(|(_, db)| *db <= peak - 5.0)
            .map(|(time, _)| *time);
        let end = curve
            .iter()
            .find(|(_, db)| *db <= peak - 35.0)
            .map(|(time, _)| *time);
        let (start, end) = match (start, end) {
            (Some(start), Some(end)) if end > start => (start, end),
            _ => return f64::NAN,
        };
        let points: Vec<(f64, f64)> = curve
            .iter()
            .copied()
            .filter(|(time, _)| *time >= start && *time <= end)
            .collect();
        let count = points.len() as f64;
        let mean_time = points.iter().map(|(time, _)| time).sum::<f64>() / count;
        let mean_db = points.iter().map(|(_, db)| db).sum::<f64>() / count;
        let numerator = points
            .iter()
            .map(|(time, db)| (time - mean_time) * (db - mean_db))
            .sum::<f64>();
        let denominator = points
            .iter()
            .map(|(time, _)| (time - mean_time) * (time - mean_time))
            .sum::<f64>();
        let slope = numerator / denominator;
        60.0 / -slope
    }

    fn rt30_bands(effect: &mut ReverbFdn, frames: usize) -> [f64; TILT_BANDS] {
        let signal = render(effect, impulse(frames));
        std::array::from_fn(|band| rt30_from_impulse(&signal, TILT_HZ[band]))
    }

    #[test]
    fn decay_band_trim_moves_only_that_band() {
        let base = rt30_bands(&mut wet_only(), 480_000);
        let mut changed = wet_only();
        changed.set_param("decay_band1", 6.0);
        let boosted = rt30_bands(&mut changed, 480_000);
        let expected = 10.0_f64.powf(6.0 / 20.0);
        println!("rt30 default {base:?}");
        println!("rt30 band1 +6 dB {boosted:?}");
        let ratios: [f64; TILT_BANDS] = std::array::from_fn(|band| boosted[band] / base[band]);
        println!("rt30 ratios {ratios:?}");
        assert!(
            (ratios[0] / expected - 1.0).abs() < 0.05,
            "band 1 measured RT30 ratio {} vs expected {expected}",
            ratios[0]
        );
        for (band, ratio) in ratios.iter().enumerate().skip(1) {
            assert!((ratio - 1.0).abs() < 0.05, "band {band} moved by {ratio}");
        }
    }

    fn sine(frames: usize, frequency: f64, amplitude: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 48_000.0;
                let sample = (2.0 * std::f64::consts::PI * frequency * t).sin() * amplitude;
                [sample as f32, sample as f32]
            })
            .collect()
    }

    fn tail_rms(buffer: &[f32], start_frame: usize) -> f64 {
        let samples: Vec<f64> = buffer[start_frame * 2..]
            .iter()
            .step_by(2)
            .map(|value| f64::from(*value))
            .collect();
        (samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn decay_bands_are_six_independent_parameters() {
        let mut effect = ReverbFdn::new(48_000);
        let base = effect.tilt_t60();
        for band in 1..=TILT_BANDS {
            let name = format!("decay_band{band}");
            assert_eq!(effect.get_param(&name), Some(0.0));
            effect.set_param(&name, 6.0);
            assert_eq!(effect.get_param(&name), Some(6.0));
            let boosted = effect.tilt_t60();
            let expected = base[band - 1] * 10.0_f64.powf(6.0 / 20.0);
            assert!(
                (boosted[band - 1] / expected - 1.0).abs() < 1e-9,
                "band {band}: {} vs {expected}",
                boosted[band - 1]
            );
            for other in (0..TILT_BANDS).filter(|other| *other != band - 1) {
                assert!((boosted[other] - base[other]).abs() < 1e-9);
            }
            effect.set_param(&name, 0.0);
        }
    }

    #[test]
    fn post_eq_defaults_are_transparent() {
        let expected = render(&mut character_zero(), noise(24_000));
        let mut effect = character_zero();
        assert!(!effect.post_eq_active);
        for (band, frequency) in [100.0, 300.0, 1_000.0, 3_000.0, 8_000.0, 16_000.0]
            .iter()
            .enumerate()
        {
            let prefix = format!("post_eq{}", band + 1);
            effect.set_param(&format!("{prefix}_freq"), *frequency);
            effect.set_param(&format!("{prefix}_gain"), 0.0);
            effect.set_param(&format!("{prefix}_q"), 1.0);
            effect.set_param(&format!("{prefix}_shape"), 0.0);
            assert_eq!(
                effect.get_param(&format!("{prefix}_freq")),
                Some(*frequency)
            );
            assert_eq!(effect.get_param(&format!("{prefix}_gain")), Some(0.0));
            assert_eq!(effect.get_param(&format!("{prefix}_q")), Some(1.0));
            assert_eq!(effect.get_param(&format!("{prefix}_shape")), Some(0.0));
        }
        assert!(!effect.post_eq_active);
        assert_eq!(render(&mut effect, noise(24_000)), expected);
    }

    #[test]
    fn post_eq_gain_applies_at_its_frequency() {
        let flat = render(&mut wet_only(), sine(192_000, 1_000.0, 0.1));
        let mut boosted = wet_only();
        boosted.set_param("post_eq3_gain", 6.0);
        assert!((boosted.post_eq_response_db(1_000.0) - 6.0).abs() < 0.05);
        let output = render(&mut boosted, sine(192_000, 1_000.0, 0.1));
        let gain_db = 20.0 * (tail_rms(&output, 144_000) / tail_rms(&flat, 144_000)).log10();
        assert!(
            (gain_db - 6.0).abs() < 0.1,
            "measured post EQ gain {gain_db}"
        );
    }

    #[test]
    fn post_eq_shapes_are_parameterised() {
        let mut effect = ReverbFdn::new(48_000);
        effect.set_param("post_eq1_shape", 3.0);
        effect.set_param("post_eq1_freq", 1_000.0);
        assert_eq!(effect.get_param("post_eq1_shape"), Some(3.0));
        assert_eq!(effect.get_param("post_eq1_freq"), Some(1_000.0));
        assert!(effect.post_eq_active);
        assert!(
            effect.post_eq_response_db(100.0) < -20.0,
            "low cut should reject 100 Hz"
        );
    }

    #[test]
    fn defaults_keep_the_frozen_render() {
        let mut effect = character_zero();
        let output = render(&mut effect, noise(24_000));
        assert_eq!(digest(&output), 0x9fc7_cf19_0ebd_7637);
    }

    #[test]
    fn reset_restores_the_character_zero_render() {
        let mut effect = character_zero();
        let first = render(&mut effect, noise(24_000));
        effect.reset();
        let second = render(&mut effect, noise(24_000));
        assert_eq!(first, second);
    }

    #[test]
    fn decay_eq_parameters_are_independent() {
        let mut effect = ReverbFdn::new(48_000);
        for band in 1..=TILT_BANDS {
            let prefix = format!("decay_band{band}");
            assert_eq!(
                effect.get_param(&format!("{prefix}_freq")),
                Some(TILT_HZ[band - 1])
            );
            assert_eq!(effect.get_param(&format!("{prefix}_q")), Some(0.707));
            assert_eq!(effect.get_param(&format!("{prefix}_shape")), Some(0.0));
            effect.set_param(&format!("{prefix}_freq"), 300.0);
            effect.set_param(&format!("{prefix}_q"), 4.0);
            effect.set_param(&format!("{prefix}_shape"), 2.0);
            assert_eq!(effect.get_param(&format!("{prefix}_freq")), Some(300.0));
            assert_eq!(effect.get_param(&format!("{prefix}_q")), Some(4.0));
            assert_eq!(effect.get_param(&format!("{prefix}_shape")), Some(2.0));
        }
        effect.set_param("decay_band1_q", 100.0);
        assert_eq!(effect.get_param("decay_band1_q"), Some(10.0));
        effect.set_param("decay_band1_freq", 1.0);
        assert_eq!(effect.get_param("decay_band1_freq"), Some(20.0));
        effect.set_param("decay_band1_shape", -3.0);
        assert_eq!(effect.get_param("decay_band1_shape"), Some(0.0));
    }

    #[test]
    fn decay_eq_shape_none_matches_the_plain_trim() {
        let mut plain = ReverbFdn::new(48_000);
        plain.set_param("decay_band1", 6.0);
        let expected = plain.tilt_t60();

        let mut placed = ReverbFdn::new(48_000);
        placed.set_param("decay_band1_freq", 500.0);
        placed.set_param("decay_band1_q", 4.0);
        placed.set_param("decay_band1_shape", 0.0);
        placed.set_param("decay_band1", 6.0);
        assert_eq!(placed.tilt_t60(), expected);
    }

    #[test]
    fn decay_eq_low_shelf_lengthens_the_tail_below_its_corner() {
        let base = rt30_bands(&mut wet_only(), 480_000);
        let mut changed = wet_only();
        changed.set_param("decay_band1", 6.0);
        changed.set_param("decay_band1_freq", 4_000.0);
        changed.set_param("decay_band1_q", 0.707);
        changed.set_param("decay_band1_shape", 1.0);
        let boosted = rt30_bands(&mut changed, 480_000);

        let expected = 10.0_f64.powf(6.0 / 20.0);
        let ratios: [f64; TILT_BANDS] = std::array::from_fn(|band| boosted[band] / base[band]);
        // 1 kHz lies below the 4 kHz corner, 16 kHz above it.
        let contrast = (boosted[1] / boosted[5]) / (base[1] / base[5]);
        println!("rt30 default {base:?}");
        println!("rt30 low shelf {boosted:?}");
        println!("low shelf ratios {ratios:?}, below/above contrast {contrast}");
        assert!(
            (contrast / expected - 1.0).abs() < 0.15,
            "low-shelf below/above contrast {contrast} vs expected {expected}"
        );
        for band in [0, 1] {
            assert!(
                (ratios[band] / expected - 1.0).abs() < 0.15,
                "band {band} RT30 ratio {} vs expected {expected}",
                ratios[band]
            );
        }
        for band in [4, 5] {
            assert!(
                (ratios[band] - 1.0).abs() < 0.15,
                "band {band} RT30 ratio {} should be untouched",
                ratios[band]
            );
        }
    }

    #[test]
    fn decay_eq_bell_places_the_trim_at_its_own_frequency() {
        let base = rt30_bands(&mut wet_only(), 480_000);
        let mut changed = wet_only();
        changed.set_param("decay_band3", 6.0);
        changed.set_param("decay_band3_freq", 2_000.0);
        changed.set_param("decay_band3_q", 2.0);
        changed.set_param("decay_band3_shape", 2.0);
        let boosted = rt30_bands(&mut changed, 480_000);

        let expected = 10.0_f64.powf(6.0 / 20.0);
        let at_bell = boosted[2] / base[2];
        let outside = boosted[3] / base[3];
        println!("bell at 2 kHz: band 2 ratio {at_bell}, band 3 ratio {outside}");
        assert!(
            (at_bell / expected - 1.0).abs() < 0.15,
            "2 kHz RT30 ratio {at_bell} vs expected {expected}"
        );
        assert!((outside - 1.0).abs() < 0.15, "4 kHz RT30 ratio {outside}");
    }

    #[test]
    fn bounce_tail_follows_the_decay_and_never_moves_the_full_mix_length() {
        let rate = 48_000;
        let mut effect = ReverbFdn::new(rate);
        assert_eq!(effect.tail_samples(), 0, "the full-mix length is unchanged");
        let long = effect.bounce_tail_samples();
        assert!(
            long >= 2 * rate,
            "the default room rings about 2.9 s, got {long} frames"
        );

        effect.set_param("decay", 500.0);
        let short = effect.bounce_tail_samples();
        assert!(
            short * 2 < long,
            "a short decay rings shorter ({short} vs {long} frames)"
        );

        let before_predelay = effect.bounce_tail_samples();
        effect.set_param("predelay", 100.0);
        assert_eq!(
            effect.bounce_tail_samples(),
            before_predelay + 4_800,
            "the predelay is part of the ring-out"
        );

        effect.set_param("mix", 0.0);
        assert_eq!(
            effect.bounce_tail_samples(),
            0,
            "a dry reverb keeps nothing"
        );
    }
}
