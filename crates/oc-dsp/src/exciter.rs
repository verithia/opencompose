//! Air exciter: synthesises a 10-16 kHz band from existing 5-8 kHz content.

use super::AudioEffect;
use super::biquad::{BiquadFilter, FilterType};
use super::oversampler::{LATENCY, Oversampler4x};

const SOURCE_LOW_HZ: f64 = 4_800.0;
const SOURCE_HIGH_HZ: f64 = 8_000.0;
const SOURCE_Q: f64 = 0.707;
const ODD_GAIN: f64 = 8.0;
const MIN_LOW_FREQ: f64 = 9_000.0;
const MAX_LOW_FREQ: f64 = 12_000.0;
const MIN_HIGH_FREQ: f64 = 14_000.0;
const MAX_HIGH_FREQ: f64 = 20_000.0;
const MIN_AMOUNT: f64 = 0.0;
const MAX_AMOUNT: f64 = 1.0;
const MIN_DRIVE_DB: f64 = 0.0;
const MAX_DRIVE_DB: f64 = 24.0;
const MIN_CHARACTER: f64 = 0.0;
const MAX_CHARACTER: f64 = 1.0;
const MIN_OUTPUT_DB: f64 = -12.0;
const MAX_OUTPUT_DB: f64 = 12.0;

/// An air exciter that synthesises 10-16 kHz from 5-8 kHz content.
pub struct Exciter {
    oversampler: Oversampler4x,
    source_high_pass: BiquadFilter,
    source_low_pass: BiquadFilter,
    output_high_pass: [BiquadFilter; 2],
    output_low_pass: [BiquadFilter; 2],
    amount: f64,
    drive_db: f64,
    character: f64,
    low_freq: f64,
    high_freq: f64,
    output_db: f64,
    drive_gain: f64,
    output_gain: f64,
}

impl Exciter {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut exciter = Self {
            oversampler: Oversampler4x::new(),
            source_high_pass: BiquadFilter::new(
                FilterType::HighPass,
                SOURCE_LOW_HZ,
                SOURCE_Q,
                0.0,
                sample_rate,
            ),
            source_low_pass: BiquadFilter::new(
                FilterType::LowPass,
                SOURCE_HIGH_HZ,
                SOURCE_Q,
                0.0,
                sample_rate,
            ),
            output_high_pass: [
                BiquadFilter::new(FilterType::HighPass, 9_500.0, 0.707, 0.0, sample_rate),
                BiquadFilter::new(FilterType::HighPass, 9_500.0, 0.707, 0.0, sample_rate),
            ],
            output_low_pass: [
                BiquadFilter::new(FilterType::LowPass, 16_000.0, 0.55, 0.0, sample_rate),
                BiquadFilter::new(FilterType::LowPass, 16_000.0, 0.707, 0.0, sample_rate),
            ],
            amount: 0.5,
            drive_db: 6.0,
            character: 0.0,
            low_freq: 9_500.0,
            high_freq: 16_000.0,
            output_db: 0.0,
            drive_gain: 0.0,
            output_gain: 1.0,
        };
        exciter.update_drive();
        exciter
    }

    fn update_drive(&mut self) {
        self.drive_gain = 10f64.powf(self.drive_db / 20.0) - 1.0;
    }

    fn band_limit(&mut self, channel: usize, input: f64) -> f64 {
        let high = self.output_high_pass[0].process_sample(channel, input);
        let high = self.output_high_pass[1].process_sample(channel, high);
        let low = self.output_low_pass[0].process_sample(channel, high);
        self.output_low_pass[1].process_sample(channel, low)
    }
}

fn shape(input: [f64; 2], gain: f64, even: f64, character: f64) -> [f64; 2] {
    let mut output = [0.0; 2];
    for channel in 0..2 {
        let u = gain * input[channel];
        output[channel] = even * u * u + character * ODD_GAIN * u * u * u;
    }
    output
}

impl AudioEffect for Exciter {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let gain = self.amount * self.output_gain;
        for frame in buffer.as_chunks_mut::<2>().0 {
            let high_l = self.source_high_pass.process_sample(0, frame[0] as f64);
            let high_r = self.source_high_pass.process_sample(1, frame[1] as f64);
            let source = [
                self.source_low_pass.process_sample(0, high_l),
                self.source_low_pass.process_sample(1, high_r),
            ];
            let drive_gain = self.drive_gain;
            let even = 1.0 - self.character;
            let character = self.character;
            let wet = self.oversampler.process(source, |high_rate| {
                shape(high_rate, drive_gain, even, character)
            });
            let air_l = self.band_limit(0, wet[0]);
            let air_r = self.band_limit(1, wet[1]);
            frame[0] = (air_l * gain) as f32;
            frame[1] = (air_r * gain) as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "amount" => self.amount = value.clamp(MIN_AMOUNT, MAX_AMOUNT),
            "drive" => {
                self.drive_db = value.clamp(MIN_DRIVE_DB, MAX_DRIVE_DB);
                self.update_drive();
            }
            "character" => self.character = value.clamp(MIN_CHARACTER, MAX_CHARACTER),
            "low_freq" => {
                self.low_freq = value.clamp(MIN_LOW_FREQ, MAX_LOW_FREQ);
                for filter in &mut self.output_high_pass {
                    filter.set_params(self.low_freq, 0.707, 0.0);
                }
            }
            "high_freq" => {
                self.high_freq = value.clamp(MIN_HIGH_FREQ, MAX_HIGH_FREQ);
                self.output_low_pass[0].set_params(self.high_freq, 0.55, 0.0);
                self.output_low_pass[1].set_params(self.high_freq, 0.707, 0.0);
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
            "amount" => Some(self.amount),
            "drive" => Some(self.drive_db),
            "character" => Some(self.character),
            "low_freq" => Some(self.low_freq),
            "high_freq" => Some(self.high_freq),
            "output" => Some(self.output_db),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.oversampler.reset();
        self.source_high_pass.reset();
        self.source_low_pass.reset();
        for filter in &mut self.output_high_pass {
            filter.reset();
        }
        for filter in &mut self.output_low_pass {
            filter.reset();
        }
    }

    fn name(&self) -> &str {
        "exciter"
    }

    fn latency_samples(&self) -> u32 {
        LATENCY as u32
    }

    fn tail_samples(&self) -> u32 {
        32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 48_000;

    fn tone(freq: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (RATE as f64 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let value = (amplitude * (TAU * freq * index as f64 / RATE as f64).sin()) as f32;
            buffer.push(value);
            buffer.push(value);
        }
        buffer
    }

    fn band_rms(buffer: &[f32], start_frame: usize, high: f64) -> f64 {
        let mut high_pass = BiquadFilter::new(FilterType::HighPass, 20.0, 0.707, 0.0, RATE);
        let mut low_pass = BiquadFilter::new(FilterType::LowPass, high, 0.707, 0.0, RATE);
        let mut sum = 0.0;
        let mut count = 0;
        for frame in start_frame..buffer.len() / 2 {
            let mut value = buffer[frame * 2] as f64;
            value = high_pass.process_sample(0, value);
            value = low_pass.process_sample(0, value);
            sum += value * value;
            count += 1;
        }
        (sum / count.max(1) as f64).sqrt()
    }

    fn loudest_below(buffer: &[f32], start_frame: usize, high: f64) -> (f64, f64) {
        let mut loudest = 0.0;
        let mut loudest_freq = 0.0;
        let mut freq = 100.0;
        while freq < high {
            let value = magnitude(buffer, freq, start_frame);
            if value > loudest {
                loudest = value;
                loudest_freq = freq;
            }
            freq += 100.0;
        }
        (loudest_freq, loudest)
    }

    fn db(value: f64) -> f64 {
        20.0 * value.max(1e-12).log10()
    }

    fn magnitude(buffer: &[f32], freq: f64, start_frame: usize) -> f64 {
        let frames = buffer.len() / 2;
        let length = frames - start_frame;
        let mut real = 0.0;
        let mut imag = 0.0;
        for index in 0..length {
            let window = 0.5 - 0.5 * (TAU * index as f64 / length as f64).cos();
            let sample = buffer[(start_frame + index) * 2] as f64 * window;
            let phase = TAU * freq * index as f64 / RATE as f64;
            real += sample * phase.cos();
            imag -= sample * phase.sin();
        }
        2.0 * (real * real + imag * imag).sqrt() / length as f64
    }

    fn make() -> Exciter {
        let mut exciter = Exciter::new(RATE);
        exciter.set_param("amount", 1.0);
        exciter.set_param("drive", 6.0);
        exciter
    }

    #[test]
    fn a_five_khz_tone_appears_above_nine_khz_and_nothing_below_five_khz() {
        let input = tone(5_000.0, 0.25, 2.0);
        let mut output = input.clone();
        make().process(&mut output, RATE);
        let start = RATE as usize / 2;
        let air = magnitude(&output, 10_000.0, start);
        assert!(db(air) > -60.0, "air component is {:.1} dBFS", db(air));
        let (below_freq, below) = loudest_below(&output, start, 4_900.0);
        assert!(
            db(below) < -60.0,
            "content below 5 kHz is {:.1} dBFS at {below_freq:.0} Hz",
            db(below)
        );
        let fundamental = magnitude(&output, 5_000.0, start);
        assert!(
            db(fundamental) < -60.0,
            "dry 5 kHz leaked at {:.1} dB",
            db(fundamental)
        );
    }

    #[test]
    fn zero_amount_and_zero_drive_are_exact_silence() {
        let input = tone(5_000.0, 0.25, 1.0);
        let mut silent = input.clone();
        let mut exciter = make();
        exciter.set_param("amount", 0.0);
        exciter.process(&mut silent, RATE);
        assert!(silent.iter().all(|sample| *sample == 0.0));

        let mut silent = input.clone();
        let mut exciter = make();
        exciter.set_param("drive", 0.0);
        exciter.process(&mut silent, RATE);
        assert!(silent.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn alias_content_stays_below_minus_sixty_dbc() {
        let input = tone(7_900.0, 0.25, 2.0);
        let mut output = input.clone();
        let mut exciter = make();
        exciter.set_param("high_freq", 20_000.0);
        exciter.process(&mut output, RATE);
        let start = RATE as usize / 2;
        let desired = magnitude(&output, 15_800.0, start);
        let mut loudest = 0.0_f64;
        let mut loudest_freq = 0.0;
        let mut freq = 9_000.0_f64;
        while freq <= 20_000.0 {
            if (freq - 15_800.0).abs() > 50.0 {
                let value = magnitude(&output, freq, start);
                if value > loudest {
                    loudest = value;
                    loudest_freq = freq;
                }
            }
            freq += 25.0;
        }
        let dbc = 20.0 * (loudest / desired.max(1e-12)).log10();
        assert!(dbc < -60.0, "alias at {loudest_freq:.0} Hz is {dbc:.1} dBc");
    }

    #[test]
    fn the_odd_law_runs_through_the_oversampler_and_keeps_the_alias_low() {
        let input = tone(5_300.0, 0.25, 2.0);
        let mut output = input.clone();
        let mut exciter = make();
        exciter.set_param("character", 1.0);
        exciter.process(&mut output, RATE);
        let start = RATE as usize / 2;
        let desired = magnitude(&output, 15_900.0, start);
        assert!(db(desired) > -60.0, "odd air is {:.1} dBFS", db(desired));
        let mut loudest = 0.0_f64;
        let mut freq = 9_000.0_f64;
        while freq <= 20_000.0 {
            if (freq - 15_900.0).abs() > 50.0 {
                loudest = loudest.max(magnitude(&output, freq, start));
            }
            freq += 25.0;
        }
        let dbc = 20.0 * (loudest / desired.max(1e-12)).log10();
        assert!(dbc < -60.0, "odd alias is {dbc:.1} dBc");
    }

    #[test]
    fn the_output_trim_is_an_exact_gain_and_the_band_edges_limit_the_band() {
        let input = tone(5_000.0, 0.25, 1.5);
        let mut reference = input.clone();
        make().process(&mut reference, RATE);
        let reference_rms = band_rms(&reference, RATE as usize / 2, 20_000.0);

        let mut trimmed = input.clone();
        let mut exciter = make();
        exciter.set_param("output", -6.0);
        exciter.process(&mut trimmed, RATE);
        let trimmed_rms = band_rms(&trimmed, RATE as usize / 2, 20_000.0);
        let gain = trimmed_rms / reference_rms;
        assert!(
            (gain - 10f64.powf(-6.0 / 20.0)).abs() < 1e-3,
            "gain {gain:.4}"
        );

        let mut narrowed = input.clone();
        let mut exciter = make();
        exciter.set_param("low_freq", 12_000.0);
        exciter.process(&mut narrowed, RATE);
        let reference_air = magnitude(&reference, 10_000.0, RATE as usize / 2);
        let narrowed_air = magnitude(&narrowed, 10_000.0, RATE as usize / 2);
        let change_db = 20.0 * (narrowed_air / reference_air.max(1e-12)).log10();
        assert!(
            change_db < -6.0,
            "the 12 kHz high-pass moved the 10 kHz air by only {change_db:.1} dB"
        );
    }

    #[test]
    #[ignore]
    fn measurements_for_docs() {
        let input = tone(5_000.0, 0.25, 2.0);
        let mut output = input.clone();
        make().process(&mut output, RATE);
        let start = RATE as usize / 2;
        let air = magnitude(&output, 10_000.0, start);
        let fundamental = magnitude(&output, 5_000.0, start);
        let (below_freq, below) = loudest_below(&output, start, 4_900.0);
        println!(
            "exciter 5 kHz in: 10 kHz {:.1} dBFS, 5 kHz {:.1} dBFS, loudest below 5 kHz {:.1} dBFS at {below_freq:.0} Hz",
            db(air),
            db(fundamental),
            db(below)
        );

        for (freq, character) in [(7_900.0, 0.0), (5_300.0, 1.0)] {
            let input = tone(freq, 0.25, 2.0);
            let mut output = input.clone();
            let mut exciter = make();
            exciter.set_param("character", character);
            exciter.set_param("high_freq", 20_000.0);
            exciter.process(&mut output, RATE);
            let start = RATE as usize / 2;
            let target = if character > 0.5 {
                freq * 3.0
            } else {
                freq * 2.0
            };
            let desired = magnitude(&output, target, start);
            let mut loudest = 0.0_f64;
            let mut loudest_freq = 0.0;
            let mut scan = 9_000.0_f64;
            while scan <= 20_000.0 {
                if (scan - target).abs() > 50.0 {
                    let value = magnitude(&output, scan, start);
                    if value > loudest {
                        loudest = value;
                        loudest_freq = scan;
                    }
                }
                scan += 25.0;
            }
            println!(
                "exciter {freq:.0} Hz character {character:.0}: desired {:.1} dBFS, loudest other {:.1} dBFS at {loudest_freq:.0} Hz ({:.1} dBc)",
                db(desired),
                db(loudest),
                20.0 * (loudest / desired.max(1e-12)).log10()
            );
        }

        let input = tone(5_000.0, 0.25, 1.0);
        let mut silent = input.clone();
        let mut exciter = make();
        exciter.set_param("amount", 0.0);
        exciter.process(&mut silent, RATE);
        let zero_amount = silent
            .iter()
            .fold(0.0f32, |max, value| max.max(value.abs()));
        let mut silent = input.clone();
        let mut exciter = make();
        exciter.set_param("drive", 0.0);
        exciter.process(&mut silent, RATE);
        let zero_drive = silent
            .iter()
            .fold(0.0f32, |max, value| max.max(value.abs()));
        println!(
            "exciter nulls: amount 0 residual {:.1} dBFS, drive 0 residual {:.1} dBFS",
            db(zero_amount as f64),
            db(zero_drive as f64)
        );
    }

    #[test]
    fn the_exciter_reports_the_shared_oversampler_latency_and_is_block_size_invariant() {
        let input = tone(5_000.0, 0.25, 0.5);
        let mut small = input.clone();
        let mut large = input.clone();
        let mut first = make();
        let mut second = make();
        for chunk in small.chunks_mut(97 * 2) {
            first.process(chunk, RATE);
        }
        for chunk in large.chunks_mut(480 * 2) {
            second.process(chunk, RATE);
        }
        let difference = small
            .iter()
            .zip(&large)
            .fold(0.0_f32, |diff, (a, b)| diff.max((a - b).abs()));
        assert!(
            difference < 1e-7,
            "block size changed the output by {difference}"
        );
        assert_eq!(first.latency_samples(), LATENCY as u32);
    }
}
