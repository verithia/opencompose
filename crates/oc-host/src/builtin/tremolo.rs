//! Tremolo — amplitude modulation via LFO.
//!
//! Parameters:
//!   rate: 0.1 to 20.0 Hz (default 4.0)
//!   depth: 0.0 to 1.0 (default 0.5)
//!   shape: 0=sine, 1=square, 2=triangle (default 0)

use oc_dsp::AudioEffect;
use std::f64::consts::PI;

/// Built-in tremolo effect.
pub struct Tremolo {
    rate: f64,
    depth: f64,
    shape: u32,
    phase: f64,
}

impl Default for Tremolo {
    fn default() -> Self {
        Self::new()
    }
}

impl Tremolo {
    /// Create the effect with its default settings.
    pub fn new() -> Self {
        Self {
            rate: 4.0,
            depth: 0.5,
            shape: 0,
            phase: 0.0,
        }
    }
}

impl AudioEffect for Tremolo {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let sr = sample_rate as f64;
        let phase_inc = self.rate / sr;
        let depth = self.depth as f32;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            let lfo = match self.shape {
                0 => ((2.0 * PI * self.phase).sin() as f32 + 1.0) * 0.5, // sine: 0..1
                1 => {
                    if self.phase % 1.0 < 0.5 {
                        1.0
                    } else {
                        0.0
                    }
                } // square
                _ => {
                    // triangle
                    let t = (self.phase % 1.0) as f32;
                    if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 }
                }
            };

            let gain = 1.0 - depth + lfo * depth;
            buffer[i] *= gain;
            buffer[i + 1] *= gain;

            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "rate" => self.rate = value.clamp(0.1, 20.0),
            "depth" => self.depth = value.clamp(0.0, 1.0),
            "shape" => self.shape = (value as u32).min(2),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "rate" => Some(self.rate),
            "depth" => Some(self.depth),
            "shape" => Some(self.shape as f64),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }
    fn name(&self) -> &str {
        "Tremolo"
    }
}
