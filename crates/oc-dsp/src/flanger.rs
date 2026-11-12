//! Stereo flanger — a 0.5–10 ms swept delay with interpolated reads.

use super::AudioEffect;
use std::f64::consts::PI;

const MIN_DELAY_MS: f64 = 0.5;
const MAX_DELAY_MS: f64 = 10.0;

/// A stereo flanger with a swept 0.5-10 ms delay.
pub struct Flanger {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    phase: f64,
    rate_hz: f64,
    depth_ms: f64,
    feedback: f64,
    mix: f64,
    sample_rate: u32,
}

impl Flanger {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let max_delay = (sample_rate as f64 * 0.011).ceil() as usize + 2;
        Self {
            buf_l: vec![0.0; max_delay],
            buf_r: vec![0.0; max_delay],
            write_pos: 0,
            phase: 0.0,
            rate_hz: 0.5,
            depth_ms: 3.0,
            feedback: 0.5,
            mix: 0.5,
            sample_rate,
        }
    }

    fn read_interpolated(buf: &[f32], position: f64) -> f32 {
        let len = buf.len() as isize;
        let base = position.floor() as isize;
        let frac = position - base as f64;
        let first = base.rem_euclid(len) as usize;
        let second = (first + 1) % buf.len();
        buf[first] * (1.0 - frac) as f32 + buf[second] * frac as f32
    }

    fn delay_samples(&self, lfo: f64) -> f64 {
        // Depth is capped so the swept delay stays inside 0.5–10 ms.
        let depth = self.depth_ms.clamp(0.1, MAX_DELAY_MS - MIN_DELAY_MS);
        let delay_ms = MIN_DELAY_MS + depth * (0.5 + 0.5 * lfo);
        delay_ms * 0.001 * self.sample_rate as f64
    }
}

impl AudioEffect for Flanger {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let phase_inc = self.rate_hz / self.sample_rate as f64;
        let feedback = self.feedback;
        let wet = self.mix as f32;
        let dry = 1.0 - wet;
        let len = self.buf_l.len();

        for frame in buffer.as_chunks_mut::<2>().0 {
            let in_l = frame[0];
            let in_r = frame[1];

            let lfo_l = (2.0 * PI * self.phase).sin();
            let lfo_r = (2.0 * PI * (self.phase + 0.25)).sin();
            let delay_l = self.delay_samples(lfo_l);
            let delay_r = self.delay_samples(lfo_r);
            let read_l = self.write_pos as f64 + len as f64 - delay_l;
            let read_r = self.write_pos as f64 + len as f64 - delay_r;
            let delayed_l = Self::read_interpolated(&self.buf_l, read_l);
            let delayed_r = Self::read_interpolated(&self.buf_r, read_r);

            let write_l = in_l + (feedback * delayed_l as f64).tanh() as f32;
            let write_r = in_r + (feedback * delayed_r as f64).tanh() as f32;
            self.buf_l[self.write_pos] = if write_l.abs() < 1e-20 { 0.0 } else { write_l };
            self.buf_r[self.write_pos] = if write_r.abs() < 1e-20 { 0.0 } else { write_r };

            frame[0] = in_l * dry + delayed_l * wet;
            frame[1] = in_r * dry + delayed_r * wet;

            self.write_pos = (self.write_pos + 1) % len;
            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "rate" => self.rate_hz = value.clamp(0.05, 10.0),
            "depth" => self.depth_ms = value.clamp(0.1, 20.0),
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "rate" => Some(self.rate_hz),
            "depth" => Some(self.depth_ms),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write_pos = 0;
        self.phase = 0.0;
    }

    fn name(&self) -> &str {
        "Flanger"
    }
}

#[test]
fn flanger_changes_the_signal_and_bypasses_exactly_at_mix_zero() {
    let original: Vec<f32> = (0..9600)
        .flat_map(|i| {
            let sample = (2.0 * PI * 330.0 * i as f64 / 48000.0).sin() as f32 * 0.4;
            [sample, sample]
        })
        .collect();
    let mut effect = Flanger::new(48000);
    effect.set_param("depth", 4.0);
    effect.set_param("feedback", 0.6);
    effect.set_param("mix", 0.5);
    let mut audio = original.clone();
    effect.process(&mut audio, 48000);
    assert!(audio.iter().all(|v| v.is_finite()));
    let difference: f64 = audio
        .iter()
        .zip(&original)
        .map(|(a, b)| (*a - *b).abs() as f64)
        .sum();
    assert!(difference > 1.0);

    effect.reset();
    effect.set_param("mix", 0.0);
    let mut audio = original.clone();
    effect.process(&mut audio, 48000);
    assert_eq!(audio, original);
}

#[test]
fn flanger_stays_bounded_at_extreme_settings() {
    let mut effect = Flanger::new(48000);
    effect.set_param("rate", 10.0);
    effect.set_param("depth", 20.0);
    effect.set_param("feedback", 0.95);
    effect.set_param("mix", 1.0);
    let mut audio: Vec<f32> = (0..96000)
        .flat_map(|i| {
            let sample = (2.0 * PI * 110.0 * i as f64 / 48000.0).sin() as f32;
            [sample, sample]
        })
        .collect();
    effect.process(&mut audio, 48000);
    assert!(audio.iter().all(|v| v.is_finite() && v.abs() <= 4.0));
}
