//! Six-stage first-order allpass phaser with a swept centre frequency.

use super::AudioEffect;
use std::f64::consts::PI;

const STAGES: usize = 6;
const STAGGER: [f64; STAGES] = [1.0, 1.32, 1.74, 2.30, 3.04, 4.02];
const CENTER_HZ: f64 = 600.0;

/// A six-stage allpass phaser with a swept centre frequency.
pub struct Phaser {
    state_x: [[f64; STAGES]; 2],
    state_y: [[f64; STAGES]; 2],
    feedback_state: [f64; 2],
    phase: f64,
    rate_hz: f64,
    depth: f64,
    feedback: f64,
    mix: f64,
    sample_rate: u32,
}

impl Phaser {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            state_x: [[0.0; STAGES]; 2],
            state_y: [[0.0; STAGES]; 2],
            feedback_state: [0.0; 2],
            phase: 0.0,
            rate_hz: 0.5,
            depth: 0.5,
            feedback: 0.3,
            mix: 0.5,
            sample_rate,
        }
    }
}

impl AudioEffect for Phaser {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let sample_rate = self.sample_rate as f64;
        let phase_inc = self.rate_hz / sample_rate;
        let wet = self.mix;
        let dry = 1.0 - wet;

        for frame in buffer.as_chunks_mut::<2>().0 {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let lfo = (2.0 * PI * (self.phase + channel as f64 * 0.25)).sin();
                let centre = CENTER_HZ * 2.0_f64.powf(lfo * self.depth * 2.5);
                let centre = centre.clamp(40.0, sample_rate * 0.45);
                let input = *sample as f64;
                let mut signal = input + self.feedback * self.feedback_state[channel].tanh();

                for (stage, &stagger) in STAGGER.iter().enumerate() {
                    let tangent = (PI * centre * stagger / sample_rate).tan().min(6.3138);
                    let coefficient = (1.0 - tangent) / (1.0 + tangent);
                    let output = coefficient * signal + self.state_x[channel][stage]
                        - coefficient * self.state_y[channel][stage];
                    self.state_x[channel][stage] = signal;
                    self.state_y[channel][stage] = if output.abs() < 1e-20 { 0.0 } else { output };
                    signal = self.state_y[channel][stage];
                }

                self.feedback_state[channel] = signal;
                *sample = (input * dry + signal * wet) as f32;
            }

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
            "depth" => self.depth = value.clamp(0.0, 1.0),
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "rate" => Some(self.rate_hz),
            "depth" => Some(self.depth),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.state_x = [[0.0; STAGES]; 2];
        self.state_y = [[0.0; STAGES]; 2];
        self.feedback_state = [0.0; 2];
        self.phase = 0.0;
    }

    fn name(&self) -> &str {
        "Phaser"
    }
}

#[test]
fn phaser_changes_the_signal_and_bypasses_exactly_at_mix_zero() {
    let original: Vec<f32> = (0..9600)
        .flat_map(|i| {
            let sample = (2.0 * PI * 440.0 * i as f64 / 48000.0).sin() as f32 * 0.4;
            [sample, sample]
        })
        .collect();
    let mut effect = Phaser::new(48000);
    effect.set_param("depth", 0.8);
    effect.set_param("feedback", 0.5);
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
fn phaser_stays_bounded_at_extreme_settings() {
    let mut effect = Phaser::new(48000);
    effect.set_param("rate", 10.0);
    effect.set_param("depth", 1.0);
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
