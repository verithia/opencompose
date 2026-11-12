//! Waveshaper distortion with a post low-pass tone stage.
//! Transfer: y = (1-d)x + d*tanh(g*x)/tanh(g), g = 10^(24d/20).
//! Drive 0 is the identity, so unity drive leaves RMS unchanged.

use super::AudioEffect;
use std::f64::consts::PI;

/// A waveshaper distortion with a post low-pass tone stage.
pub struct Distortion {
    drive: f64,
    tone: f64,
    mix: f64,
    output_db: f64,
    gain: f64,
    lowpass: [f64; 2],
}

impl Default for Distortion {
    fn default() -> Self {
        Self::new()
    }
}

impl Distortion {
    /// Create the processor with its default settings.
    pub fn new() -> Self {
        let mut distortion = Self {
            drive: 0.5,
            tone: 0.5,
            mix: 1.0,
            output_db: 0.0,
            gain: 1.0,
            lowpass: [0.0; 2],
        };
        distortion.update_gain();
        distortion
    }

    fn update_gain(&mut self) {
        self.gain = 10.0_f64.powf(self.drive * 24.0 / 20.0);
    }
}

impl AudioEffect for Distortion {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let sample_rate = sample_rate.max(1) as f64;
        let cutoff = (200.0 * 100.0_f64.powf(self.tone)).clamp(20.0, sample_rate * 0.45);
        let tone_coeff = 1.0 - (-2.0 * PI * cutoff / sample_rate).exp();
        let drive = self.drive;
        let gain = self.gain;
        let norm = 1.0 / gain.tanh();
        let output = 10.0_f64.powf(self.output_db / 20.0);
        let wet = self.mix;
        let dry = 1.0 - wet;

        for frame in buffer.as_chunks_mut::<2>().0 {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let input = *sample as f64;
                let shaped = (1.0 - drive) * input + drive * (gain * input).tanh() * norm;
                let filtered =
                    self.lowpass[channel] + tone_coeff * (shaped - self.lowpass[channel]);
                self.lowpass[channel] = if filtered.abs() < 1e-20 {
                    0.0
                } else {
                    filtered
                };
                *sample = (input * dry + self.lowpass[channel] * output * wet) as f32;
            }
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        match name {
            "drive" => {
                self.drive = value.clamp(0.0, 1.0);
                self.update_gain();
            }
            "tone" => self.tone = value.clamp(0.0, 1.0),
            "output" => self.output_db = value.clamp(-24.0, 24.0),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "drive" => Some(self.drive),
            "tone" => Some(self.tone),
            "output" => Some(self.output_db),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.lowpass = [0.0; 2];
    }

    fn name(&self) -> &str {
        "Distortion"
    }
}

#[test]
fn bypass_at_mix_zero_is_exact_and_unity_drive_keeps_rms() {
    let original: Vec<f32> = (0..9600)
        .flat_map(|i| {
            let sample = (2.0 * PI * 440.0 * i as f64 / 48000.0).sin() as f32 * 0.5;
            [sample, -sample]
        })
        .collect();
    let mut bypass = Distortion::new();
    bypass.set_param("mix", 0.0);
    let mut audio = original.clone();
    bypass.process(&mut audio, 48000);
    assert_eq!(audio, original);

    let mut unity = Distortion::new();
    unity.set_param("drive", 0.0);
    unity.set_param("tone", 1.0);
    unity.set_param("mix", 1.0);
    let mut audio = original.clone();
    unity.process(&mut audio, 48000);
    let rms = |samples: &[f32]| {
        (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    };
    assert!((rms(&audio) / rms(&original) - 1.0).abs() < 0.01);
}

#[test]
fn extreme_params_stay_finite_and_bounded() {
    let mut effect = Distortion::new();
    effect.set_param("drive", 100.0);
    effect.set_param("tone", 0.0);
    effect.set_param("output", 24.0);
    effect.set_param("mix", 1.0);
    let mut audio: Vec<f32> = (0..4800)
        .flat_map(|i| {
            let sample = (2.0 * PI * 220.0 * i as f64 / 48000.0).sin() as f32;
            [sample, sample]
        })
        .collect();
    effect.process(&mut audio, 48000);
    assert!(audio.iter().all(|v| v.is_finite() && v.abs() <= 20.0));
}
