//! Stereo flanger with one or more interpolated, swept taps.

use super::AudioEffect;
use oc_device_schema::flanger::{MAX_DELAY_MS, MAX_GAIN_PERCENT, MAX_VOICES, MIN_DELAY_MS};
use std::f64::consts::PI;

const SWEEP_CAP_MS: f64 = 9.5;
const DELAY_SPREAD: f64 = 0.25;

#[derive(Clone, Copy, Default)]
struct Tap {
    base_ms: f64,
    sweep_ms: f64,
    phase_step: f64,
    phase_offset: f64,
}

impl Tap {
    fn delay_samples(self, phase: f64, sample_rate: u32) -> f64 {
        let lfo = (2.0 * PI * phase).sin();
        (self.base_ms + self.sweep_ms * (0.5 + 0.5 * lfo)) * 0.001 * f64::from(sample_rate)
    }
}

/// A modulated stereo delay with a bounded stack of taps in a shared feedback loop.
pub struct Flanger {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    phases: [f64; MAX_VOICES],
    rate_hz: f64,
    depth_ms: f64,
    feedback: f64,
    mix: f64,
    voices: usize,
    delay_ms: f64,
    spread: f64,
    stereo_cross: f64,
    dry_gain: f64,
    wet_gain: f64,
    sample_rate: u32,
}

impl Flanger {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let max_ms = (MAX_DELAY_MS + SWEEP_CAP_MS) * (1.0 + DELAY_SPREAD);
        let max_delay = (f64::from(sample_rate) * max_ms * 0.001).ceil() as usize + 2;
        Self {
            buf_l: vec![0.0; max_delay],
            buf_r: vec![0.0; max_delay],
            write_pos: 0,
            phases: [0.0; MAX_VOICES],
            rate_hz: 0.5,
            depth_ms: 3.0,
            feedback: 0.5,
            mix: 0.5,
            voices: 1,
            delay_ms: MIN_DELAY_MS,
            spread: 0.0,
            stereo_cross: 0.0,
            dry_gain: 100.0,
            wet_gain: 100.0,
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

    fn tap_geometry(&self, taps: &mut [Tap]) {
        let span = self.voices.saturating_sub(1).max(1) as f64;
        let spread = self.spread * 0.01;
        let sweep_ms = self.depth_ms.min(SWEEP_CAP_MS);
        for (index, tap) in taps.iter_mut().enumerate() {
            let pan = if self.voices == 1 {
                0.0
            } else {
                2.0 * index as f64 / span - 1.0
            };
            let separation = pan * spread;
            let delay_scale = 1.0 + separation * DELAY_SPREAD;
            *tap = Tap {
                base_ms: (self.delay_ms * delay_scale).max(MIN_DELAY_MS),
                sweep_ms: sweep_ms * delay_scale,
                phase_step: self.rate_hz * (1.0 + separation * 0.35) / f64::from(self.sample_rate),
                phase_offset: separation * 0.25,
            };
        }
    }
}

impl AudioEffect for Flanger {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let feedback = self.feedback;
        let mix = self.mix as f32;
        let wet = mix * (self.wet_gain * 0.01) as f32;
        let dry = (1.0 - mix) * (self.dry_gain * 0.01) as f32;
        let cross = (self.stereo_cross * 0.01) as f32;
        let wet_self = 1.0 - cross.abs();
        let weight = 1.0 / self.voices as f32;
        let len = self.buf_l.len();
        let mut taps = [Tap::default(); MAX_VOICES];
        let taps = &mut taps[..self.voices];
        self.tap_geometry(taps);

        for frame in buffer.as_chunks_mut::<2>().0 {
            let in_l = frame[0];
            let in_r = frame[1];

            let position = self.write_pos as f64 + len as f64;
            let mut delayed_l = 0.0;
            let mut delayed_r = 0.0;
            for (&tap, phase) in taps.iter().zip(&mut self.phases) {
                let left_phase = *phase + tap.phase_offset;
                let read_l = position - tap.delay_samples(left_phase, self.sample_rate);
                let read_r = position - tap.delay_samples(left_phase + 0.25, self.sample_rate);
                delayed_l += Self::read_interpolated(&self.buf_l, read_l);
                delayed_r += Self::read_interpolated(&self.buf_r, read_r);
                *phase += tap.phase_step;
                if *phase >= 1.0 {
                    *phase -= phase.floor();
                }
            }
            delayed_l *= weight;
            delayed_r *= weight;

            let write_l = in_l + (feedback * delayed_l as f64).tanh() as f32;
            let write_r = in_r + (feedback * delayed_r as f64).tanh() as f32;
            self.buf_l[self.write_pos] = if write_l.abs() < 1e-20 { 0.0 } else { write_l };
            self.buf_r[self.write_pos] = if write_r.abs() < 1e-20 { 0.0 } else { write_r };

            frame[0] = in_l * dry + (wet_self * delayed_l + cross * delayed_r) * wet;
            frame[1] = in_r * dry + (wet_self * delayed_r + cross * delayed_l) * wet;

            self.write_pos = (self.write_pos + 1) % len;
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
            "voices" => self.voices = value.round().clamp(1.0, MAX_VOICES as f64) as usize,
            "delay" => self.delay_ms = value.clamp(MIN_DELAY_MS, MAX_DELAY_MS),
            "spread" => self.spread = value.clamp(0.0, 100.0),
            "stereo_cross" => self.stereo_cross = value.clamp(-100.0, 100.0),
            "dry_gain" => self.dry_gain = value.clamp(-MAX_GAIN_PERCENT, MAX_GAIN_PERCENT),
            "wet_gain" => self.wet_gain = value.clamp(-MAX_GAIN_PERCENT, MAX_GAIN_PERCENT),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "rate" => Some(self.rate_hz),
            "depth" => Some(self.depth_ms),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            "voices" => Some(self.voices as f64),
            "delay" => Some(self.delay_ms),
            "spread" => Some(self.spread),
            "stereo_cross" => Some(self.stereo_cross),
            "dry_gain" => Some(self.dry_gain),
            "wet_gain" => Some(self.wet_gain),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write_pos = 0;
        self.phases.fill(0.0);
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

#[test]
fn flanger_stack_preserves_channels_crosses_wet_only_and_resets_without_allocating() {
    let mut input = [0.0; 4096];
    input[0] = 0.125;
    input[784] = -0.25;
    let mut effect = Flanger::new(48_000);
    effect.set_param("voices", 7.0);
    effect.set_param("spread", 100.0);
    effect.set_param("delay", 12.0);
    effect.set_param("feedback", 0.0);
    effect.set_param("mix", 1.0);
    let mut normal = input;
    assert_eq!(
        oc_test_support::operations(|| effect.process(&mut normal, 48_000)),
        0
    );
    assert!(normal[..860].iter().all(|x| *x == 0.0));
    assert!(normal.chunks_exact(2).any(|frame| frame[0].abs() > 0.001));
    assert!(normal.chunks_exact(2).all(|frame| frame[1] == 0.0));

    effect.reset();
    effect.set_param("stereo_cross", -100.0);
    let mut crossed = input;
    for block in crossed.chunks_mut(62) {
        effect.process(block, 48_000);
    }
    for (original, cross) in normal
        .as_chunks::<2>()
        .0
        .iter()
        .zip(crossed.as_chunks::<2>().0)
    {
        assert_eq!(cross, &[0.0, -original[0]]);
    }

    effect.reset();
    effect.set_param("mix", 0.5);
    effect.set_param("dry_gain", -200.0);
    effect.set_param("wet_gain", 0.0);
    let mut dry = input;
    effect.process(&mut dry, 48_000);
    assert_eq!(dry, input.map(|sample| -sample));
}
