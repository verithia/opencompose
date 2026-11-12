//! Coupled passive EQ: one low corner shared by a bell boost and a shelf cut.

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};

const LOW_BOOST_Q: f64 = 1.0;
const LOW_CUT_Q: f64 = 0.7;
const HIGH_CUT_Q: f64 = 0.707;
const AIR_Q: f64 = 0.707;
const TILT_PIVOT_HZ: f64 = 1_000.0;
const MIN_LOW_FREQ: f64 = 30.0;
const MAX_LOW_FREQ: f64 = 300.0;
const MIN_LOW_BOOST: f64 = 0.0;
const MAX_LOW_BOOST: f64 = 10.0;
const MIN_LOW_CUT: f64 = 0.0;
const MAX_LOW_CUT: f64 = 10.0;
const MIN_HIGH_FREQ: f64 = 3_000.0;
const MAX_HIGH_FREQ: f64 = 16_000.0;
const MIN_HIGH_BOOST: f64 = 0.0;
const MAX_HIGH_BOOST: f64 = 10.0;
const MIN_HIGH_Q: f64 = 0.7;
const MAX_HIGH_Q: f64 = 2.0;
const MIN_HIGH_CUT_FREQ: f64 = 5_000.0;
const MAX_HIGH_CUT_FREQ: f64 = 20_000.0;
const MIN_HIGH_CUT: f64 = 0.0;
const MAX_HIGH_CUT: f64 = 10.0;
const MIN_TILT: f64 = -6.0;
const MAX_TILT: f64 = 6.0;
const MIN_AIR_FREQ: f64 = 8_000.0;
const MAX_AIR_FREQ: f64 = 12_000.0;
const MIN_AIR: f64 = 0.0;
const MAX_AIR: f64 = 6.0;

/// A coupled passive EQ with a shared low corner.
pub struct PassiveEq {
    low_boost: BiquadFilter,
    low_cut: BiquadFilter,
    high_boost: BiquadFilter,
    high_cut: BiquadFilter,
    tilt: BiquadFilter,
    air: BiquadFilter,
    low_freq: f64,
    low_boost_db: f64,
    low_cut_db: f64,
    high_freq: f64,
    high_boost_db: f64,
    high_q: f64,
    high_cut_freq: f64,
    high_cut_db: f64,
    tilt_db: f64,
    air_freq: f64,
    air_db: f64,
}

impl PassiveEq {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            low_boost: BiquadFilter::new(FilterType::Peak, 100.0, LOW_BOOST_Q, 0.0, sample_rate),
            low_cut: BiquadFilter::new(FilterType::LowShelf, 100.0, LOW_CUT_Q, 0.0, sample_rate),
            high_boost: BiquadFilter::new(FilterType::Peak, 8_000.0, 1.0, 0.0, sample_rate),
            high_cut: BiquadFilter::new(
                FilterType::HighShelf,
                10_000.0,
                HIGH_CUT_Q,
                0.0,
                sample_rate,
            ),
            tilt: BiquadFilter::new(FilterType::TiltShelf, TILT_PIVOT_HZ, 1.0, 0.0, sample_rate),
            air: BiquadFilter::new(FilterType::HighShelf, 10_000.0, AIR_Q, 0.0, sample_rate),
            low_freq: 100.0,
            low_boost_db: 0.0,
            low_cut_db: 0.0,
            high_freq: 8_000.0,
            high_boost_db: 0.0,
            high_q: 1.0,
            high_cut_freq: 10_000.0,
            high_cut_db: 0.0,
            tilt_db: 0.0,
            air_freq: 10_000.0,
            air_db: 0.0,
        }
    }
}

impl AudioEffect for PassiveEq {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.low_boost.process_stereo(buffer);
        self.low_cut.process_stereo(buffer);
        self.high_boost.process_stereo(buffer);
        self.high_cut.process_stereo(buffer);
        self.tilt.process_stereo(buffer);
        self.air.process_stereo(buffer);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "low_freq" => {
                self.low_freq = value.clamp(MIN_LOW_FREQ, MAX_LOW_FREQ);
                self.low_boost
                    .set_params(self.low_freq, LOW_BOOST_Q, self.low_boost_db);
                self.low_cut
                    .set_params(self.low_freq, LOW_CUT_Q, -self.low_cut_db);
            }
            "low_boost" => {
                self.low_boost_db = value.clamp(MIN_LOW_BOOST, MAX_LOW_BOOST);
                self.low_boost
                    .set_params(self.low_freq, LOW_BOOST_Q, self.low_boost_db);
            }
            "low_cut" => {
                self.low_cut_db = value.clamp(MIN_LOW_CUT, MAX_LOW_CUT);
                self.low_cut
                    .set_params(self.low_freq, LOW_CUT_Q, -self.low_cut_db);
            }
            "high_freq" => {
                self.high_freq = value.clamp(MIN_HIGH_FREQ, MAX_HIGH_FREQ);
                self.high_boost
                    .set_params(self.high_freq, self.high_q, self.high_boost_db);
            }
            "high_boost" => {
                self.high_boost_db = value.clamp(MIN_HIGH_BOOST, MAX_HIGH_BOOST);
                self.high_boost
                    .set_params(self.high_freq, self.high_q, self.high_boost_db);
            }
            "high_q" => {
                self.high_q = value.clamp(MIN_HIGH_Q, MAX_HIGH_Q);
                self.high_boost
                    .set_params(self.high_freq, self.high_q, self.high_boost_db);
            }
            "high_cut_freq" => {
                self.high_cut_freq = value.clamp(MIN_HIGH_CUT_FREQ, MAX_HIGH_CUT_FREQ);
                self.high_cut
                    .set_params(self.high_cut_freq, HIGH_CUT_Q, -self.high_cut_db);
            }
            "high_cut" => {
                self.high_cut_db = value.clamp(MIN_HIGH_CUT, MAX_HIGH_CUT);
                self.high_cut
                    .set_params(self.high_cut_freq, HIGH_CUT_Q, -self.high_cut_db);
            }
            "tilt" => {
                self.tilt_db = value.clamp(MIN_TILT, MAX_TILT);
                self.tilt.set_params(TILT_PIVOT_HZ, 1.0, self.tilt_db);
            }
            "air_freq" => {
                self.air_freq = value.clamp(MIN_AIR_FREQ, MAX_AIR_FREQ);
                self.air.set_params(self.air_freq, AIR_Q, self.air_db);
            }
            "air" => {
                self.air_db = value.clamp(MIN_AIR, MAX_AIR);
                self.air.set_params(self.air_freq, AIR_Q, self.air_db);
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "low_freq" => Some(self.low_freq),
            "low_boost" => Some(self.low_boost_db),
            "low_cut" => Some(self.low_cut_db),
            "high_freq" => Some(self.high_freq),
            "high_boost" => Some(self.high_boost_db),
            "high_q" => Some(self.high_q),
            "high_cut_freq" => Some(self.high_cut_freq),
            "high_cut" => Some(self.high_cut_db),
            "tilt" => Some(self.tilt_db),
            "air_freq" => Some(self.air_freq),
            "air" => Some(self.air_db),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.low_boost.reset();
        self.low_cut.reset();
        self.high_boost.reset();
        self.high_cut.reset();
        self.tilt.reset();
        self.air.reset();
    }

    fn name(&self) -> &str {
        "passive_eq"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 48_000;
    const AMPLITUDE: f64 = 0.25;

    fn gain_db(effect: &mut PassiveEq, freq: f64) -> f64 {
        let settle = (RATE as f64 * 0.2) as usize;
        let window = (RATE as f64 * 0.4) as usize;
        let frames = settle + window;
        let mut buffer = vec![0.0_f32; frames * 2];
        for frame in 0..frames {
            let sample = (AMPLITUDE * (TAU * freq * frame as f64 / RATE as f64).sin()) as f32;
            buffer[frame * 2] = sample;
            buffer[frame * 2 + 1] = sample;
        }
        effect.reset();
        effect.process(&mut buffer, RATE);
        let mut real = 0.0;
        let mut imaginary = 0.0;
        let mut weight_sum = 0.0;
        for offset in 0..window {
            let t = offset as f64 / (window - 1) as f64;
            let weight = 0.5 - 0.5 * (TAU * t).cos();
            let phase = TAU * freq * (settle + offset) as f64 / RATE as f64;
            let sample = buffer[(settle + offset) * 2] as f64;
            real += weight * sample * phase.cos();
            imaginary -= weight * sample * phase.sin();
            weight_sum += weight;
        }
        let measured = 2.0 * real.hypot(imaginary) / weight_sum;
        20.0 * (measured / AMPLITUDE).log10()
    }

    fn assert_near(actual: f64, expected: f64, tolerance: f64, label: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{label}: {actual:.3} dB, expected {expected:.3} dB"
        );
    }

    #[test]
    fn a_shelf_pair_cancels_but_the_coupled_corner_does_not() {
        use super::super::biquad::{BiquadCoefficients, FilterType};
        let up = BiquadCoefficients::design(FilterType::LowShelf, 100.0, 0.7, 5.0, RATE as f64);
        let down = BiquadCoefficients::design(FilterType::LowShelf, 100.0, 0.7, -5.0, RATE as f64);
        for freq in [30.0, 100.0, 300.0] {
            let sum = up.response_db(freq, RATE as f64) + down.response_db(freq, RATE as f64);
            assert!(sum.abs() < 1e-9, "shelf pair left {sum} dB at {freq} Hz");
        }
        let mut effect = PassiveEq::new(RATE);
        effect.set_param("low_freq", 100.0);
        effect.set_param("low_boost", 5.0);
        effect.set_param("low_cut", 5.0);
        assert!(gain_db(&mut effect, 100.0) > 2.0);
        assert!(gain_db(&mut effect, 30.0) < -4.0);
    }

    #[test]
    fn the_low_corner_boost_and_cut_do_not_cancel() {
        let mut effect = PassiveEq::new(RATE);
        effect.set_param("low_freq", 100.0);
        effect.set_param("low_boost", 5.0);
        effect.set_param("low_cut", 5.0);
        assert_near(gain_db(&mut effect, 100.0), 2.5, 0.15, "100 Hz corner");
        assert_near(gain_db(&mut effect, 30.0), -5.0, 0.8, "30 Hz shelf");
        let dc_side = gain_db(&mut effect, 20.0);
        assert!(
            dc_side < -4.0,
            "the low corner should stay cut at DC: {dc_side:.2} dB"
        );
    }

    #[test]
    fn neither_corner_moves_the_one_to_four_khz_band() {
        let mut low = PassiveEq::new(RATE);
        low.set_param("low_freq", 100.0);
        low.set_param("low_boost", 5.0);
        low.set_param("low_cut", 5.0);
        for freq in [1_000.0, 2_000.0, 4_000.0] {
            let gain = gain_db(&mut low, freq);
            assert!(
                gain.abs() < 0.3,
                "low corner moved {freq} Hz by {gain:.3} dB"
            );
        }
        let mut high = PassiveEq::new(RATE);
        high.set_param("high_cut", 5.0);
        high.set_param("high_cut_freq", 10_000.0);
        high.set_param("air", 6.0);
        high.set_param("air_freq", 10_000.0);
        for freq in [1_000.0, 2_000.0, 4_000.0] {
            let gain = gain_db(&mut high, freq);
            assert!(
                gain.abs() < 0.3,
                "high corner moved {freq} Hz by {gain:.3} dB"
            );
        }
    }

    #[test]
    fn a_six_db_tilt_spans_minus_three_to_plus_three() {
        let mut tilt = PassiveEq::new(RATE);
        tilt.set_param("tilt", 6.0);
        assert_near(gain_db(&mut tilt, 20.0), -3.0, 0.1, "tilt low end");
        assert_near(gain_db(&mut tilt, 20_000.0), 3.0, 0.1, "tilt high end");
        assert_near(gain_db(&mut tilt, 1_000.0), 0.0, 0.1, "tilt pivot");
        let mut negative = PassiveEq::new(RATE);
        negative.set_param("tilt", -6.0);
        assert_near(
            gain_db(&mut negative, 20.0),
            3.0,
            0.1,
            "inverse tilt low end",
        );
        assert_near(
            gain_db(&mut negative, 20_000.0),
            -3.0,
            0.1,
            "inverse tilt high end",
        );
    }

    #[test]
    fn the_high_band_is_a_bell_and_the_cut_is_a_shelf() {
        let mut boost = PassiveEq::new(RATE);
        boost.set_param("high_freq", 8_000.0);
        boost.set_param("high_q", 1.4);
        boost.set_param("high_boost", 5.0);
        assert_near(gain_db(&mut boost, 8_000.0), 5.0, 0.15, "high bell");
        assert_near(
            gain_db(&mut boost, 1_000.0),
            0.0,
            0.15,
            "high bell at 1 kHz",
        );
        let mut cut = PassiveEq::new(RATE);
        cut.set_param("high_cut_freq", 10_000.0);
        cut.set_param("high_cut", 5.0);
        assert_near(gain_db(&mut cut, 10_000.0), -2.5, 0.15, "high shelf corner");
        assert_near(gain_db(&mut cut, 20_000.0), -5.0, 0.15, "high shelf top");
        assert_near(gain_db(&mut cut, 4_000.0), 0.0, 0.2, "high shelf at 4 kHz");
    }

    #[test]
    fn air_is_a_high_shelf_after_the_rest_of_the_chain() {
        let mut effect = PassiveEq::new(RATE);
        effect.set_param("air_freq", 10_000.0);
        effect.set_param("air", 6.0);
        assert_near(gain_db(&mut effect, 20_000.0), 6.0, 0.1, "air top");
        assert_near(gain_db(&mut effect, 10_000.0), 3.0, 0.1, "air corner");
        assert_near(gain_db(&mut effect, 4_000.0), 0.1, 0.2, "air at 4 kHz");
    }

    #[test]
    fn the_default_setting_is_an_exact_identity() {
        let mut input = vec![0.0_f32; 2_048];
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = ((index as f64 * 0.13).sin() * 0.4) as f32;
        }
        let original = input.clone();
        let mut effect = PassiveEq::new(RATE);
        effect.process(&mut input, RATE);
        assert_eq!(input, original);
        assert_eq!(effect.latency_samples(), 0);
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let mut input = vec![0.0_f32; 4_800 * 2];
        for (index, sample) in input.iter_mut().enumerate() {
            *sample = ((index as f64 * 0.031).sin() * 0.3) as f32;
        }
        let configure = |effect: &mut PassiveEq| {
            effect.set_param("low_freq", 120.0);
            effect.set_param("low_boost", 4.0);
            effect.set_param("low_cut", 3.0);
            effect.set_param("high_boost", 2.0);
            effect.set_param("tilt", 2.0);
            effect.set_param("air", 3.0);
        };
        let mut small = PassiveEq::new(RATE);
        configure(&mut small);
        let mut large = PassiveEq::new(RATE);
        configure(&mut large);
        let mut a = input.clone();
        for chunk in a.chunks_mut(97 * 2) {
            small.process(chunk, RATE);
        }
        let mut b = input;
        for chunk in b.chunks_mut(480 * 2) {
            large.process(chunk, RATE);
        }
        let difference = a
            .iter()
            .zip(&b)
            .fold(0.0_f32, |diff, (a, b)| diff.max((a - b).abs()));
        assert!(
            difference == 0.0,
            "block size changed the output by {difference}"
        );
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let mut effect = PassiveEq::new(RATE);
        effect.set_param("low_freq", 100.0);
        effect.set_param("low_boost", 5.0);
        effect.set_param("low_cut", 5.0);
        println!("passive_eq, low 5/5 at 100 Hz");
        for freq in [20.0, 30.0, 50.0, 100.0, 200.0, 1_000.0, 2_000.0, 4_000.0] {
            println!("  {freq:>6.0} Hz: {:.3} dB", gain_db(&mut effect, freq));
        }
        let mut tilt = PassiveEq::new(RATE);
        tilt.set_param("tilt", 6.0);
        println!("tilt +6");
        for freq in [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
            println!("  {freq:>6.0} Hz: {:.3} dB", gain_db(&mut tilt, freq));
        }
        let mut air = PassiveEq::new(RATE);
        air.set_param("air", 6.0);
        air.set_param("air_freq", 10_000.0);
        println!("air +6 at 10 kHz");
        for freq in [1_000.0, 4_000.0, 10_000.0, 20_000.0] {
            println!("  {freq:>6.0} Hz: {:.3} dB", gain_db(&mut air, freq));
        }
    }
}
