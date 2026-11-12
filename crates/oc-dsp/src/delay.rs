//! Stereo delay with feedback and optional ping-pong.

use super::AudioEffect;

/// A stereo delay with feedback and optional ping-pong.
pub struct StereoDelay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    delay_samples_l: usize,
    delay_samples_r: usize,
    feedback: f64,
    mix: f64,
    sample_rate: u32,
}

impl StereoDelay {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let max_delay = sample_rate as usize * 2; // max 2 seconds
        Self {
            buf_l: vec![0.0; max_delay],
            buf_r: vec![0.0; max_delay],
            write_pos: 0,
            delay_samples_l: (sample_rate as f64 * 0.375) as usize, // 375ms default
            delay_samples_r: (sample_rate as f64 * 0.250) as usize, // 250ms default
            feedback: 0.35,
            mix: 0.3,
            sample_rate,
        }
    }
}

impl AudioEffect for StereoDelay {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let max_len = self.buf_l.len();
        let len = buffer.len();
        let wet = self.mix as f32;
        let dry = 1.0 - wet;
        let fb = self.feedback as f32;
        let mut i = 0;

        while i + 1 < len {
            let in_l = buffer[i];
            let in_r = buffer[i + 1];

            // Read from delay lines
            let read_l = (self.write_pos + max_len - self.delay_samples_l) % max_len;
            let read_r = (self.write_pos + max_len - self.delay_samples_r) % max_len;
            let del_l = self.buf_l[read_l];
            let del_r = self.buf_r[read_r];

            // Write with feedback
            self.buf_l[self.write_pos] = in_l + del_l * fb;
            self.buf_r[self.write_pos] = in_r + del_r * fb;

            buffer[i] = in_l * dry + del_l * wet;
            buffer[i + 1] = in_r * dry + del_r * wet;

            self.write_pos = (self.write_pos + 1) % max_len;
            i += 2;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        let max_samples = self.buf_l.len();
        match name {
            "time" | "time_l" => {
                self.delay_samples_l = (value * 0.001 * self.sample_rate as f64)
                    .min(max_samples as f64 - 1.0) as usize;
            }
            "time_r" => {
                self.delay_samples_r = (value * 0.001 * self.sample_rate as f64)
                    .min(max_samples as f64 - 1.0) as usize;
            }
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "time_l" => Some(self.delay_samples_l as f64 / self.sample_rate as f64 * 1000.0),
            "time_r" => Some(self.delay_samples_r as f64 / self.sample_rate as f64 * 1000.0),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
    }

    fn name(&self) -> &str {
        "Stereo Delay"
    }
}
