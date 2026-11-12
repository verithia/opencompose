//! Asymmetric saturation in front of a flux-driven transformer core.

use std::f64::consts::TAU;

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};
use super::oversampler::{LATENCY, Oversampler4x};

/// Flux scale calibrated so a 100 Hz tone at -12 dBFS gains about 3% H3 at drive 4.
const FLUX_BASE: f64 = 1_485.0;
const DC_BLOCK_HZ: f64 = 10.0;
const FLUX_LEAK_HZ: f64 = 2.0;
const LOWPASS_Q: f64 = 0.55;
const HIGHPASS_Q: f64 = 1.0;
const MIN_DRIVE_DB: f64 = 0.0;
const MAX_DRIVE_DB: f64 = 24.0;
const MIN_BIAS: f64 = -0.5;
const MAX_BIAS: f64 = 0.5;
const MIN_LOW_FREQ: f64 = 20.0;
const MAX_LOW_FREQ: f64 = 120.0;
const MIN_HIGH_FREQ: f64 = 8_000.0;
const MAX_HIGH_FREQ: f64 = 20_000.0;
const MIN_OUTPUT_DB: f64 = -12.0;
const MAX_OUTPUT_DB: f64 = 12.0;

/// Asymmetric saturation into a flux-driven transformer core.
pub struct Iron {
    oversampler: Oversampler4x,
    dry: [[f64; LATENCY]; 2],
    dry_pos: usize,
    high_pass: BiquadFilter,
    low_pass: BiquadFilter,
    dc_x: [f64; 2],
    dc_y: [f64; 2],
    flux: [f64; 2],
    sample_rate: f64,
    drive_db: f64,
    bias: f64,
    ceiling: f64,
    low_freq: f64,
    high_freq: f64,
    output_db: f64,
    k: f64,
    biased_tanh: f64,
    ceiling_amount: f64,
    flux_scale: f64,
    output_gain: f64,
    dc_coeff: f64,
    flux_decay: f64,
}

impl Iron {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f64;
        let mut iron = Self {
            oversampler: Oversampler4x::new(),
            dry: [[0.0; LATENCY]; 2],
            dry_pos: 0,
            high_pass: BiquadFilter::new(
                FilterType::HighPass,
                25.0,
                HIGHPASS_Q,
                0.0,
                sample_rate as u32,
            ),
            low_pass: BiquadFilter::new(
                FilterType::LowPass,
                18_000.0,
                LOWPASS_Q,
                0.0,
                sample_rate as u32,
            ),
            dc_x: [0.0; 2],
            dc_y: [0.0; 2],
            flux: [0.0; 2],
            sample_rate,
            drive_db: 4.0,
            bias: 0.0,
            ceiling: 0.0,
            low_freq: 25.0,
            high_freq: 18_000.0,
            output_db: 0.0,
            k: 0.0,
            biased_tanh: 0.0,
            ceiling_amount: 0.0,
            flux_scale: 0.0,
            output_gain: 1.0,
            dc_coeff: (-TAU * DC_BLOCK_HZ / sample_rate).exp(),
            flux_decay: (-TAU * FLUX_LEAK_HZ / sample_rate).exp(),
        };
        iron.update_drive();
        iron
    }

    fn update_drive(&mut self) {
        self.k = 10f64.powf(self.drive_db / 20.0) - 1.0;
        self.biased_tanh = (self.k * self.bias).tanh();
        self.ceiling_amount = self.ceiling * (1.0 - (-self.k).exp());
        self.flux_scale = FLUX_BASE * self.k;
    }

    fn shaper_values(&self) -> (f64, f64, f64, f64) {
        (self.k, self.bias, self.biased_tanh, self.ceiling_amount)
    }
}

impl AudioEffect for Iron {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let dt = 1.0 / self.sample_rate;
        let bypass = self.k <= 0.0;
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input = [frame[0] as f64, frame[1] as f64];
            let dry = [self.dry[0][self.dry_pos], self.dry[1][self.dry_pos]];
            self.dry[0][self.dry_pos] = input[0];
            self.dry[1][self.dry_pos] = input[1];
            let (k, bias, biased_tanh, ceiling) = self.shaper_values();
            let wet = self.oversampler.process(input, |high_rate| {
                shape(high_rate, k, bias, biased_tanh, ceiling)
            });
            for channel in 0..2 {
                let residual = wet[channel] - dry[channel];
                let blocked = residual - self.dc_x[channel] + self.dc_coeff * self.dc_y[channel];
                self.dc_x[channel] = residual;
                self.dc_y[channel] = blocked;
                let voltage = dry[channel] + blocked;
                self.flux[channel] = self.flux[channel] * self.flux_decay + voltage * dt;
                let degree = (self.flux[channel] * self.flux_scale).tanh();
                let core = 1.0 - degree * degree;
                let residual = if bypass {
                    0.0
                } else {
                    voltage * core - dry[channel]
                };
                let high_passed = self.high_pass.process_sample(channel, residual);
                let band_limited = self.low_pass.process_sample(channel, high_passed);
                frame[channel] = ((dry[channel] + band_limited) * self.output_gain) as f32;
            }
            self.dry_pos = (self.dry_pos + 1) % LATENCY;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "drive" => {
                self.drive_db = value.clamp(MIN_DRIVE_DB, MAX_DRIVE_DB);
                self.update_drive();
            }
            "bias" => {
                self.bias = value.clamp(MIN_BIAS, MAX_BIAS);
                self.update_drive();
            }
            "ceiling" => {
                self.ceiling = value.clamp(0.0, 1.0);
                self.update_drive();
            }
            "low_freq" => {
                self.low_freq = value.clamp(MIN_LOW_FREQ, MAX_LOW_FREQ);
                self.high_pass.set_params(self.low_freq, HIGHPASS_Q, 0.0);
            }
            "high_freq" => {
                self.high_freq = value.clamp(MIN_HIGH_FREQ, MAX_HIGH_FREQ);
                self.low_pass.set_params(self.high_freq, LOWPASS_Q, 0.0);
            }
            "output" => {
                self.output_db = value.clamp(MIN_OUTPUT_DB, MAX_OUTPUT_DB);
                self.output_gain = 10f64.powf(self.output_db / 20.0);
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "drive" => Some(self.drive_db),
            "bias" => Some(self.bias),
            "ceiling" => Some(self.ceiling),
            "low_freq" => Some(self.low_freq),
            "high_freq" => Some(self.high_freq),
            "output" => Some(self.output_db),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.oversampler.reset();
        self.dry = [[0.0; LATENCY]; 2];
        self.dry_pos = 0;
        self.high_pass.reset();
        self.low_pass.reset();
        self.dc_x = [0.0; 2];
        self.dc_y = [0.0; 2];
        self.flux = [0.0; 2];
    }

    fn latency_samples(&self) -> u32 {
        LATENCY as u32
    }

    fn tail_samples(&self) -> u32 {
        32
    }

    fn name(&self) -> &str {
        "iron"
    }
}

fn shape(input: [f64; 2], k: f64, bias: f64, biased_tanh: f64, ceiling: f64) -> [f64; 2] {
    let mut shaped = [0.0; 2];
    for channel in 0..2 {
        let x = input[channel];
        let mut y = if k > 0.0 {
            ((k * (x + bias)).tanh() - biased_tanh) / k
        } else {
            x
        };
        if ceiling > 0.0 && y > 0.0 {
            y -= ceiling * (y - y.tanh());
        }
        shaped[channel] = y;
    }
    shaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const AMPLITUDE: f64 = 0.25; // -12 dBFS

    fn tone(freq: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let sample = (amplitude * (TAU * freq * index as f64 / RATE as f64).sin()) as f32;
            buffer.push(sample);
            buffer.push(sample);
        }
        buffer
    }

    fn render(effect: &mut Iron, input: &[f32], block: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(block * 2) {
            effect.process(chunk, RATE);
        }
        output
    }

    fn harmonic(samples: &[f32], freq: f64, order: usize, start: usize, len: usize) -> f64 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        let mut weight_sum = 0.0;
        for offset in 0..len {
            let t = offset as f64 / (len - 1) as f64;
            let weight = 0.5 - 0.5 * (TAU * t).cos();
            let phase = TAU * freq * order as f64 * (start + offset) as f64 / RATE as f64;
            let sample = samples[(start + offset) * 2] as f64;
            real += weight * sample * phase.cos();
            imaginary -= weight * sample * phase.sin();
            weight_sum += weight;
        }
        real.hypot(imaginary) / weight_sum
    }

    fn ratios(samples: &[f32], freq: f64) -> (f64, f64) {
        let start = (RATE as f64 * 1.0) as usize;
        let len = (RATE as f64 * 0.25) as usize;
        let fundamental = harmonic(samples, freq, 1, start, len);
        let second = harmonic(samples, freq, 2, start, len).max(1e-18);
        let third = harmonic(samples, freq, 3, start, len).max(1e-18);
        (second / fundamental, third / fundamental)
    }

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.max(1e-30).log10()
    }

    #[test]
    fn zero_drive_is_a_true_null_at_the_oversampler_latency() {
        let input = tone(1_000.0, 0.5, 0.5);
        let mut effect = Iron::new(RATE);
        effect.set_param("drive", 0.0);
        effect.set_param("ceiling", 1.0);
        effect.set_param("bias", 0.4);
        let output = render(&mut effect, &input, 480);
        let latency = LATENCY * 2;
        let peak = input[..input.len() - latency]
            .iter()
            .zip(&output[latency..])
            .fold(0.0_f64, |peak, (dry, wet)| {
                peak.max((*wet as f64 - *dry as f64).abs())
            });
        let residual_db = 20.0 * (peak / 0.5).max(1e-30).log10();
        assert!(
            residual_db < -90.0,
            "zero drive residual is {residual_db:.1} dBFS"
        );
    }

    #[test]
    fn harmonic_laws_follow_level_and_bias() {
        let bias = [0.1, 0.4];
        let levels = [-24.0, -18.0, -12.0, -6.0];
        let mut h2 = [[0.0; 4]; 2];
        let mut h3 = [[0.0; 4]; 2];
        for (bias_index, bias_value) in bias.iter().enumerate() {
            for (level_index, level) in levels.iter().enumerate() {
                let amplitude = 10f64.powf(level / 20.0);
                let mut effect = Iron::new(RATE);
                effect.set_param("drive", 4.0);
                effect.set_param("bias", *bias_value);
                let output = render(&mut effect, &tone(1_000.0, amplitude, 1.4), 480);
                let (second, third) = ratios(&output, 1_000.0);
                h2[bias_index][level_index] = second;
                h3[bias_index][level_index] = third;
            }
        }
        for bias_index in 0..2 {
            for level in 1..3 {
                let slope = db(h2[bias_index][level + 1]) - db(h2[bias_index][level]);
                let expected = 6.0;
                assert!(
                    (slope - expected).abs() < 2.0,
                    "H2 level slope at bias {}: {slope:.2} dB per 6 dB",
                    bias[bias_index]
                );
            }
            for level in 1..3 {
                let slope = db(h3[bias_index][level + 1]) - db(h3[bias_index][level]);
                let expected = 12.0;
                assert!(
                    (slope - expected).abs() < 2.5,
                    "H3 level slope at bias {}: {slope:.2} dB per 6 dB",
                    bias[bias_index]
                );
            }
        }
        let bias_ratio = db(h2[1][2]) - db(h2[0][2]);
        assert!(
            (bias_ratio - 12.0).abs() < 1.5,
            "doubling the bias moved H2 by {bias_ratio:.2} dB"
        );
        let balance = (db(h2[1][2]) - db(h3[1][2])) - (db(h2[0][2]) - db(h3[0][2]));
        assert!(
            balance > 9.0,
            "bias changed the second/third balance by only {balance:.2} dB"
        );
        let third_bias = (db(h3[1][2]) - db(h3[0][2])).abs();
        assert!(third_bias < 3.0, "bias moved H3 by {third_bias:.2} dB");
    }

    #[test]
    fn a_100_hz_tone_thickens_and_a_2_khz_tone_does_not() {
        let mut low = Iron::new(RATE);
        low.set_param("drive", 4.0);
        let output = render(&mut low, &tone(100.0, AMPLITUDE, 1.4), 480);
        let (_, third_low) = ratios(&output, 100.0);
        assert!(
            (2.0..=4.0).contains(&(third_low * 100.0)),
            "100 Hz -12 dBFS gains {:.2}% H3",
            third_low * 100.0
        );
        let mut high = Iron::new(RATE);
        high.set_param("drive", 4.0);
        let output = render(&mut high, &tone(2_000.0, AMPLITUDE, 1.4), 480);
        let (_, third_high) = ratios(&output, 2_000.0);
        assert!(
            third_high * 100.0 < 0.5,
            "2 kHz -12 dBFS gains {:.3}% H3",
            third_high * 100.0
        );
    }

    #[test]
    fn the_transformer_flux_law_falls_twelve_db_per_octave() {
        let mut effect = Iron::new(RATE);
        effect.set_param("drive", 4.0);
        // Hold the shaper linear so the measurement is the transformer alone.
        effect.k = 1e-12;
        effect.ceiling_amount = 0.0;
        let levels = [50.0, 100.0, 200.0, 400.0, 2_000.0];
        let mut third = [0.0; 5];
        for (index, freq) in levels.iter().enumerate() {
            let output = render(&mut effect, &tone(*freq, AMPLITUDE, 1.4), 480);
            let (_, ratio) = ratios(&output, *freq);
            third[index] = ratio;
        }
        let per_octave = db(third[1]) - db(third[2]);
        assert!(
            (per_octave - 12.0).abs() < 1.0,
            "100 to 200 Hz fell {per_octave:.2} dB"
        );
        let octave_200_400 = db(third[2]) - db(third[3]);
        assert!(
            (octave_200_400 - 12.0).abs() < 1.0,
            "200 to 400 Hz fell {octave_200_400:.2} dB"
        );
        let decade = db(third[1]) - db(third[4]);
        assert!(
            (decade - 52.0).abs() < 3.0,
            "100 Hz to 2 kHz fell {decade:.2} dB"
        );
    }

    #[test]
    fn the_ceiling_is_one_sided_and_scales_with_drive() {
        let mut clean = Iron::new(RATE);
        clean.set_param("drive", 4.0);
        let clean_output = render(&mut clean, &tone(1_000.0, AMPLITUDE, 1.4), 480);
        let (clean_second, _) = ratios(&clean_output, 1_000.0);
        let mut ceiling = Iron::new(RATE);
        ceiling.set_param("drive", 4.0);
        ceiling.set_param("ceiling", 1.0);
        let ceiling_output = render(&mut ceiling, &tone(1_000.0, AMPLITUDE, 1.4), 480);
        let (second, _) = ratios(&ceiling_output, 1_000.0);
        assert!(
            second > clean_second * 2.0,
            "ceiling did not add even order: {:.4}% vs {:.4}%",
            second * 100.0,
            clean_second * 100.0
        );
        let mut zero = Iron::new(RATE);
        zero.set_param("drive", 0.0);
        zero.set_param("ceiling", 1.0);
        let input = tone(1_000.0, 0.5, 0.3);
        let output = render(&mut zero, &input, 480);
        let latency = LATENCY * 2;
        let peak = input[..input.len() - latency]
            .iter()
            .zip(&output[latency..])
            .fold(0.0_f64, |peak, (dry, wet)| {
                peak.max((*wet as f64 - *dry as f64).abs())
            });
        assert!(peak == 0.0, "ceiling leaked at zero drive: {peak}");
    }

    #[test]
    fn the_output_trim_is_an_exact_gain_and_the_cut_filters_work() {
        let input = tone(1_000.0, 0.25, 0.2);
        let mut effect = Iron::new(RATE);
        effect.set_param("drive", 0.0);
        effect.set_param("output", -6.0);
        let output = render(&mut effect, &input, 480);
        let latency = LATENCY * 2;
        for (dry, wet) in input[..input.len() - latency]
            .iter()
            .zip(&output[latency..])
        {
            let expected = *dry as f64 * 10f64.powf(-6.0 / 20.0);
            assert!((*wet as f64 - expected).abs() < 1e-6);
        }
        let mut bright = Iron::new(RATE);
        bright.set_param("drive", 12.0);
        let bright_output = render(&mut bright, &tone(4_000.0, AMPLITUDE, 1.4), 480);
        let mut dark = Iron::new(RATE);
        dark.set_param("drive", 12.0);
        dark.set_param("high_freq", 8_000.0);
        let dark_output = render(&mut dark, &tone(4_000.0, AMPLITUDE, 1.4), 480);
        let (_, bright_third) = ratios(&bright_output, 4_000.0);
        let (_, dark_third) = ratios(&dark_output, 4_000.0);
        assert!(
            dark_third < bright_third * 0.6,
            "the low-pass did not remove the 12 kHz harmonic: {:.3}% vs {:.3}%",
            dark_third * 100.0,
            bright_third * 100.0
        );
    }

    #[test]
    fn block_size_does_not_change_the_output() {
        let input = tone(500.0, 0.25, 0.5);
        let mut small = Iron::new(RATE);
        small.set_param("drive", 8.0);
        let mut large = Iron::new(RATE);
        large.set_param("drive", 8.0);
        let a = render(&mut small, &input, 97);
        let b = render(&mut large, &input, 480);
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
        println!("iron, drive 4 dB, stereo 48 kHz");
        for freq in [100.0, 200.0, 400.0, 1_000.0, 2_000.0] {
            let mut effect = Iron::new(RATE);
            effect.set_param("drive", 4.0);
            let output = render(&mut effect, &tone(freq, AMPLITUDE, 1.4), 480);
            let (second, third) = ratios(&output, freq);
            println!(
                "  {freq:>6.0} Hz -12 dBFS: H2 {:.3}% H3 {:.3}%",
                second * 100.0,
                third * 100.0
            );
        }
        for bias in [0.1, 0.4] {
            for level in [-24.0, -12.0, -6.0] {
                let mut effect = Iron::new(RATE);
                effect.set_param("drive", 4.0);
                effect.set_param("bias", bias);
                let output = render(
                    &mut effect,
                    &tone(1_000.0, 10f64.powf(level / 20.0), 1.4),
                    480,
                );
                let (second, third) = ratios(&output, 1_000.0);
                println!(
                    "  bias {bias:.1} {level:>4.0} dBFS: H2 {:.3}% H3 {:.3}%",
                    second * 100.0,
                    third * 100.0
                );
            }
        }
        let mut effect = Iron::new(RATE);
        effect.set_param("drive", 4.0);
        effect.k = 1e-12;
        println!("  transformer alone:");
        for freq in [50.0, 100.0, 200.0, 400.0, 2_000.0] {
            let output = render(&mut effect, &tone(freq, AMPLITUDE, 1.4), 480);
            let (_, third) = ratios(&output, freq);
            println!("    {freq:>6.0} Hz: H3 {:.4}%", third * 100.0);
        }
    }
}
