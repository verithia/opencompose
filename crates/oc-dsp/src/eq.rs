//! Parametric EQ — 3-band (low shelf + parametric mid + high shelf).

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};

#[test]
fn independent_eq_controls_survive_sequential_updates_and_reset() {
    let settings = [
        ("low_freq", 150.0),
        ("low_gain", -3.0),
        ("freq", 2500.0),
        ("gain", 6.0),
        ("q", 2.0),
        ("high_freq", 10000.0),
        ("high_gain", -4.0),
    ];
    let mut forward = ParametricEq::new(48000);
    let mut reverse = ParametricEq::new(48000);
    for (name, value) in settings {
        forward.set_param(name, value);
    }
    for (name, value) in settings.into_iter().rev() {
        reverse.set_param(name, value);
    }
    for (name, value) in settings {
        assert_eq!(forward.get_param(name), Some(value));
        assert_eq!(reverse.get_param(name), Some(value));
    }
    let mut impulse = vec![0.0; 4096];
    impulse[0] = 1.0;
    impulse[1] = 1.0;
    let original = impulse.clone();
    let mut other = impulse.clone();
    forward.process(&mut impulse, 48000);
    reverse.process(&mut other, 48000);
    assert_eq!(impulse, other);
    assert_ne!(impulse, original);
    forward.reset();
    let mut replay = original;
    forward.process(&mut replay, 48000);
    assert_eq!(impulse, replay);
}

/// Three-band parametric EQ with low shelf, mid bell and high shelf.
pub struct ParametricEq {
    low: BiquadFilter,
    mid: BiquadFilter,
    high: BiquadFilter,
    #[allow(dead_code)]
    sample_rate: u32,
}

impl ParametricEq {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            low: BiquadFilter::new(FilterType::LowShelf, 200.0, 0.707, 0.0, sample_rate),
            mid: BiquadFilter::new(FilterType::Peak, 1000.0, 1.0, 0.0, sample_rate),
            high: BiquadFilter::new(FilterType::HighShelf, 8000.0, 0.707, 0.0, sample_rate),
            sample_rate,
        }
    }
}

impl AudioEffect for ParametricEq {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.low.process_stereo(buffer);
        self.mid.process_stereo(buffer);
        self.high.process_stereo(buffer);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "low_freq" => {
                self.low
                    .set_params(value, 0.707, self.get_param("low_gain").unwrap_or(0.0))
            }
            "low_gain" => {
                self.low
                    .set_params(self.get_param("low_freq").unwrap_or(200.0), 0.707, value)
            }
            "freq" | "mid_freq" => self.mid.set_params(
                value,
                self.get_param("q").unwrap_or(1.0),
                self.get_param("mid_gain").unwrap_or(0.0),
            ),
            "gain" | "mid_gain" => self.mid.set_params(
                self.get_param("mid_freq").unwrap_or(1000.0),
                self.get_param("q").unwrap_or(1.0),
                value,
            ),
            "q" => self.mid.set_params(
                self.get_param("mid_freq").unwrap_or(1000.0),
                value,
                self.get_param("mid_gain").unwrap_or(0.0),
            ),
            "high_freq" => {
                self.high
                    .set_params(value, 0.707, self.get_param("high_gain").unwrap_or(0.0))
            }
            "high_gain" => {
                self.high
                    .set_params(self.get_param("high_freq").unwrap_or(8000.0), 0.707, value)
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        let (low_freq, _, low_gain) = self.low.params();
        let (mid_freq, q, mid_gain) = self.mid.params();
        let (high_freq, _, high_gain) = self.high.params();
        match name {
            "low_freq" => Some(low_freq),
            "low_gain" => Some(low_gain),
            "freq" | "mid_freq" => Some(mid_freq),
            "gain" | "mid_gain" => Some(mid_gain),
            "q" => Some(q),
            "high_freq" => Some(high_freq),
            "high_gain" => Some(high_gain),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.low.reset();
        self.mid.reset();
        self.high.reset();
    }

    fn name(&self) -> &str {
        "Parametric EQ"
    }
}

#[test]
fn legacy_eq_impulse_response_is_unchanged() {
    // Captured from the pre-DynamicEq implementation; this guards the legacy
    // three-band device while the shared biquad helper evolves.
    let expected: [u32; 16] = [
        0x3f95_4d73,
        0,
        0xbe70_88ce,
        0,
        0xbd93_7f37,
        0,
        0xb967_708e,
        0,
        0x3afd_f204,
        0,
        0xbba2_0169,
        0,
        0xbb08_9adb,
        0,
        0x3bb7_69c3,
        0,
    ];
    let mut eq = ParametricEq::new(48000);
    for (name, value) in [
        ("low_freq", 120.0),
        ("low_gain", 3.0),
        ("freq", 1500.0),
        ("gain", -4.0),
        ("q", 1.7),
        ("high_freq", 9000.0),
        ("high_gain", 2.5),
    ] {
        eq.set_param(name, value);
    }
    let mut impulse = [0.0f32; 64];
    impulse[0] = 1.0;
    eq.process(&mut impulse, 48000);
    let actual: [u32; 16] = std::array::from_fn(|i| impulse[i].to_bits());
    assert_eq!(actual, expected);
}
