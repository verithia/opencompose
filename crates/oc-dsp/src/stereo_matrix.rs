//! Stereo cross-mixing with a causal offset before or after the matrix.
use crate::AudioEffect;
use oc_device_schema::stereo_matrix as schema;

#[derive(Clone, Copy, Default)]
struct HistoryFrame {
    input: [f32; 2],
    mixed: [f32; 2],
}

/// Four signed channel gains and an optional inter-channel delay.
pub struct StereoMatrix {
    coefficients: [[f64; 2]; 2],
    offset_ms: f64,
    delay_frames: i32,
    offset_automated: bool,
    before: bool,
    rate: u32,
    history: Box<[HistoryFrame]>,
    next: usize,
    valid: usize,
}

impl StereoMatrix {
    /// Preallocate both histories for the device rate so automation is allocation-free.
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.clamp(1, 384_000);
        let capacity = (schema::OFFSET.max * f64::from(rate) / 1000.0).ceil() as usize;
        Self {
            coefficients: [
                [
                    schema::LEFT_FROM_LEFT.default,
                    schema::LEFT_FROM_RIGHT.default,
                ],
                [
                    schema::RIGHT_FROM_LEFT.default,
                    schema::RIGHT_FROM_RIGHT.default,
                ],
            ],
            offset_ms: schema::OFFSET.default,
            delay_frames: 0,
            offset_automated: false,
            before: schema::BEFORE.default != 0.0,
            rate,
            history: vec![HistoryFrame::default(); capacity].into_boxed_slice(),
            next: 0,
            valid: 0,
        }
    }

    fn gain_index(name: &str) -> Option<(usize, usize, &'static oc_device_schema::ParamSchema)> {
        match name {
            "left_from_left" => Some((0, 0, &schema::LEFT_FROM_LEFT)),
            "left_from_right" => Some((0, 1, &schema::LEFT_FROM_RIGHT)),
            "right_from_left" => Some((1, 0, &schema::RIGHT_FROM_LEFT)),
            "right_from_right" => Some((1, 1, &schema::RIGHT_FROM_RIGHT)),
            _ => None,
        }
    }
}

impl AudioEffect for StereoMatrix {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let delay = self.delay_frames.unsigned_abs() as usize;
        let channel = usize::from(self.delay_frames > 0);
        let gains = self.coefficients.map(|row| row.map(|value| value * 0.01));
        let mix = |input: [f32; 2]| {
            let [left, right] = input.map(f64::from);
            gains.map(|[from_left, from_right]| (left * from_left + right * from_right) as f32)
        };
        let length = self.history.len();
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input = *frame;
            let mixed = mix(input);
            *frame = mixed;
            if delay != 0 {
                let past = if delay <= self.valid {
                    let index = if self.next >= delay {
                        self.next - delay
                    } else {
                        length - (delay - self.next)
                    };
                    self.history[index]
                } else {
                    HistoryFrame::default()
                };
                if self.before {
                    let mut aligned_inputs = input;
                    aligned_inputs[channel] = past.input[channel];
                    *frame = mix(aligned_inputs);
                } else {
                    frame[channel] = past.mixed[channel];
                }
            }
            self.history[self.next] = HistoryFrame { input, mixed };
            self.next += 1;
            if self.next == length {
                self.next = 0;
            }
            self.valid = (self.valid + 1).min(length);
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        if let Some((out, input, parameter)) = Self::gain_index(name) {
            self.coefficients[out][input] = value.clamp(parameter.min, parameter.max);
        } else if name == schema::OFFSET.id {
            self.offset_ms = value.clamp(schema::OFFSET.min, schema::OFFSET.max);
            self.delay_frames = (self.offset_ms * f64::from(self.rate) / 1000.0).round() as i32;
        } else if name == schema::BEFORE.id {
            self.before = value >= 0.5;
        }
    }

    fn prepare_parameter(&mut self, name: &str) -> Option<oc_audio::PreparedParameterHandle> {
        if name == schema::OFFSET.id {
            // Render length is set before a lane can write its final offset.
            self.offset_automated = true;
        }
        None
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        if let Some((out, input, _)) = Self::gain_index(name) {
            Some(self.coefficients[out][input])
        } else if name == schema::OFFSET.id {
            Some(self.offset_ms)
        } else if name == schema::BEFORE.id {
            Some(f64::from(self.before))
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.next = 0;
        self.valid = 0;
    }

    fn tail_samples(&self) -> u32 {
        if self.offset_automated {
            self.history.len() as u32
        } else {
            self.delay_frames.unsigned_abs()
        }
    }

    fn name(&self) -> &str {
        "Stereo Matrix"
    }
}
