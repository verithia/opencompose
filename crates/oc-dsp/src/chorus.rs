//! Stereo chorus — modulated delay with LFO.

use super::AudioEffect;
use std::f64::consts::PI;

/// A stereo chorus with a 100 ms modulated delay line.
pub struct Chorus {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    lfo_phase: f64,
    rate_hz: f64,  // LFO rate
    depth_ms: f64, // modulation depth in ms
    mix: f64,
    sample_rate: u32,
}

impl Chorus {
    /// Prepare a chorus with buffers for the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let max_delay = (sample_rate as usize) / 10; // 100ms max
        Self {
            buf_l: vec![0.0; max_delay],
            buf_r: vec![0.0; max_delay],
            write_pos: 0,
            lfo_phase: 0.0,
            rate_hz: 1.5,
            depth_ms: 3.0,
            mix: 0.5,
            sample_rate,
        }
    }

    fn read_interpolated(buf: &[f32], pos: f64) -> f32 {
        let len = buf.len();
        let idx = pos.floor() as usize % len;
        let frac = pos.fract() as f32;
        let next = (idx + 1) % len;
        buf[idx] * (1.0 - frac) + buf[next] * frac
    }
}

impl AudioEffect for Chorus {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let sr = self.sample_rate as f64;
        let buf_len = self.buf_l.len() as f64;
        let phase_inc = self.rate_hz / sr;
        let depth_samples = self.depth_ms * 0.001 * sr;
        let base_delay = 7.0 * 0.001 * sr; // 7ms center delay
        let wet = self.mix as f32;
        let dry = 1.0 - wet;
        let len = buffer.len();
        let mut i = 0;

        while i + 1 < len {
            let in_l = buffer[i];
            let in_r = buffer[i + 1];

            // Write to buffer
            self.buf_l[self.write_pos] = in_l;
            self.buf_r[self.write_pos] = in_r;

            // LFO (stereo: L=sin, R=cos for width)
            let lfo_l = (2.0 * PI * self.lfo_phase).sin();
            let lfo_r = (2.0 * PI * self.lfo_phase + PI * 0.5).sin();

            let delay_l = base_delay + lfo_l * depth_samples;
            let delay_r = base_delay + lfo_r * depth_samples;

            let read_l = self.write_pos as f64 + buf_len - delay_l;
            let read_r = self.write_pos as f64 + buf_len - delay_r;

            let chorus_l = Self::read_interpolated(&self.buf_l, read_l);
            let chorus_r = Self::read_interpolated(&self.buf_r, read_r);

            buffer[i] = in_l * dry + chorus_l * wet;
            buffer[i + 1] = in_r * dry + chorus_r * wet;

            self.write_pos = (self.write_pos + 1) % self.buf_l.len();
            self.lfo_phase += phase_inc;
            if self.lfo_phase >= 1.0 {
                self.lfo_phase -= 1.0;
            }
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        match name {
            "rate" => self.rate_hz = value.clamp(0.1, 10.0),
            "depth" => self.depth_ms = value.clamp(0.1, 20.0),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "rate" => Some(self.rate_hz),
            "depth" => Some(self.depth_ms),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.lfo_phase = 0.0;
    }

    fn name(&self) -> &str {
        "Chorus"
    }
}
