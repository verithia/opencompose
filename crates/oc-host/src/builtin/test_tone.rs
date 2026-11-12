//! Test tone generator — outputs a sine wave at a configurable frequency.
//! Useful for testing the audio signal chain.
//!
//! Parameters:
//!   frequency: 20.0 to 20000.0 Hz (default 440.0)
//!   amplitude: 0.0 to 1.0 (default 0.5)
//!   waveform: 0=sine, 1=saw, 2=square, 3=white noise

use oc_dsp::AudioEffect;
use std::f64::consts::PI;

/// Built-in test-tone generator.
pub struct TestTone {
    frequency: f64,
    amplitude: f64,
    waveform: u32, // 0=sine, 1=saw, 2=square, 3=noise
    phase: f64,
    rng_state: u64,
}

impl Default for TestTone {
    fn default() -> Self {
        Self::new()
    }
}

impl TestTone {
    /// Create the effect with its default settings.
    pub fn new() -> Self {
        Self {
            frequency: 440.0,
            amplitude: 0.5,
            waveform: 0,
            phase: 0.0,
            rng_state: 0x12345678_9ABCDEF0,
        }
    }

    fn next_noise(&mut self) -> f64 {
        // xorshift64
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as f64 / u64::MAX as f64) * 2.0 - 1.0
    }
}

impl AudioEffect for TestTone {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let sr = sample_rate as f64;
        let phase_inc = self.frequency / sr;
        let amp = self.amplitude as f32;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            let sample = match self.waveform {
                0 => (2.0 * PI * self.phase).sin(),
                1 => 2.0 * (self.phase % 1.0) - 1.0, // saw
                2 => {
                    if self.phase % 1.0 < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                } // square
                _ => self.next_noise(),              // noise
            } as f32
                * amp;

            buffer[i] += sample; // add to L
            buffer[i + 1] += sample; // add to R

            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "frequency" | "freq" => self.frequency = value.clamp(20.0, 20000.0),
            "amplitude" | "amp" => self.amplitude = value.clamp(0.0, 1.0),
            "waveform" | "wave" => self.waveform = (value as u32).min(3),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "frequency" | "freq" => Some(self.frequency),
            "amplitude" | "amp" => Some(self.amplitude),
            "waveform" | "wave" => Some(self.waveform as f64),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }
    fn name(&self) -> &str {
        "Test Tone"
    }
}
