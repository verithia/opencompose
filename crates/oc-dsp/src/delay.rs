//! Stereo delay with feedback and optional ping-pong.

use oc_audio::{AudioProcessContext, PluginTransport};
use oc_device_schema::delay::{
    DEFAULT_CUTOFF_HZ, DelayRouting, FeedbackFilter, MAX_CUTOFF_HZ, MAX_GAIN_PERCENT, MAX_QUARTERS,
    MAX_TIME_MS, MIN_CUTOFF_HZ, MIN_QUARTERS,
};

use super::AudioEffect;
use crate::biquad::{BiquadCoefficients, BiquadState, FilterType};

/// A stereo delay with feedback and optional ping-pong.
pub struct StereoDelay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    delay_samples_l: f64,
    delay_samples_r: f64,
    interpolate: bool,
    feedback: f64,
    mix: f64,
    dry_gain: f64,
    wet_gain: f64,
    routing: DelayRouting,
    tempo_sync: bool,
    quarters_l: f64,
    quarters_r: f64,
    filter: FeedbackFilter,
    cutoff: f64,
    feedback_coefficients: Option<BiquadCoefficients>,
    filter_state: [BiquadState; 2],
    sample_rate: u32,
}

impl StereoDelay {
    /// Prepare the processor at the given sample rate.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        // Keep the whole and fractional taps distinct from the write position
        // even at the longest supported delay.
        let length = (f64::from(sample_rate) * MAX_TIME_MS / 1_000.0) as usize + 2;
        Self {
            buf_l: vec![0.0; length],
            buf_r: vec![0.0; length],
            write_pos: 0,
            delay_samples_l: sample_rate as f64 * 0.375,
            delay_samples_r: sample_rate as f64 * 0.250,
            interpolate: false,
            feedback: 0.35,
            mix: 0.3,
            dry_gain: 100.0,
            wet_gain: 100.0,
            routing: DelayRouting::Stereo,
            tempo_sync: false,
            quarters_l: 0.75,
            quarters_r: 0.5,
            filter: FeedbackFilter::Off,
            cutoff: DEFAULT_CUTOFF_HZ,
            feedback_coefficients: None,
            filter_state: [BiquadState::default(); 2],
            sample_rate,
        }
    }

    fn tap(buffer: &[f32], write_pos: usize, whole: usize, fraction: f32) -> f32 {
        let length = buffer.len();
        let index = (write_pos + length - whole) % length;
        let near = buffer[index];
        if fraction != 0.0 {
            let far = buffer[if index == 0 { length - 1 } else { index - 1 }];
            near + (far - near) * fraction
        } else {
            near
        }
    }

    fn read_position(&self, delay: f64) -> (usize, f32) {
        let whole = delay as usize;
        let fraction = if self.interpolate {
            (delay - whole as f64) as f32
        } else {
            0.0
        };
        (whole, fraction)
    }

    fn delay_ms(&self, samples: f64) -> f64 {
        let effective = if self.interpolate {
            samples
        } else {
            samples.floor()
        };
        effective / f64::from(self.sample_rate) * 1000.0
    }

    fn bounded_delay(&self, samples: f64) -> f64 {
        samples.clamp(1.0, (self.buf_l.len() - 2) as f64)
    }

    fn tap_lengths(&self, transport: Option<&PluginTransport>) -> (f64, f64) {
        if !self.tempo_sync {
            return (self.delay_samples_l, self.delay_samples_r);
        }
        let tempo = transport
            .map(PluginTransport::current_tempo)
            .filter(|tempo| tempo.is_finite() && *tempo > 0.0)
            .unwrap_or(120.0);
        let quarter = f64::from(self.sample_rate) * 60.0 / tempo;
        (
            self.bounded_delay(self.quarters_l * quarter),
            self.bounded_delay(self.quarters_r * quarter),
        )
    }

    fn update_filter(&mut self) {
        let shape = match self.filter {
            FeedbackFilter::Off => None,
            FeedbackFilter::LowPass => Some(FilterType::LowPass),
            FeedbackFilter::HighPass => Some(FilterType::HighPass),
            FeedbackFilter::BandPass => Some(FilterType::BandPass),
        };
        let rate = f64::from(self.sample_rate);
        self.feedback_coefficients = shape.filter(|_| rate * 0.45 >= MIN_CUTOFF_HZ).map(|shape| {
            BiquadCoefficients::design(
                shape,
                self.cutoff,
                std::f64::consts::FRAC_1_SQRT_2,
                0.0,
                rate,
            )
        });
    }

    fn process_block(&mut self, buffer: &mut [f32], transport: Option<&PluginTransport>) {
        let max_len = self.buf_l.len();
        let mix = self.mix as f32;
        let wet = mix * (self.wet_gain * 0.01) as f32;
        let dry = (1.0 - mix) * (self.dry_gain * 0.01) as f32;
        let fb = self.feedback as f32;
        let (left, right) = self.tap_lengths(transport);
        let (whole_l, fraction_l) = self.read_position(left);
        let (whole_r, fraction_r) = self.read_position(right);

        for frame in buffer.chunks_exact_mut(2) {
            let in_l = frame[0];
            let in_r = frame[1];
            let del_l = Self::tap(&self.buf_l, self.write_pos, whole_l, fraction_l);
            let del_r = Self::tap(&self.buf_r, self.write_pos, whole_r, fraction_r);

            let (input_l, input_r) = if self.routing == DelayRouting::Mono {
                let mono = in_l * 0.5 + in_r * 0.5;
                (mono, mono)
            } else {
                (in_l, in_r)
            };
            let (return_l, return_r) = if let Some(coefficients) = &self.feedback_coefficients {
                (
                    self.filter_state[0].process(coefficients, f64::from(del_l)) as f32,
                    self.filter_state[1].process(coefficients, f64::from(del_r)) as f32,
                )
            } else {
                (del_l, del_r)
            };
            let (return_l, return_r) = if self.routing == DelayRouting::PingPong {
                (return_r, return_l)
            } else {
                (return_l, return_r)
            };
            self.buf_l[self.write_pos] = input_l + return_l * fb;
            self.buf_r[self.write_pos] = input_r + return_r * fb;

            let (output_l, output_r) = if self.routing == DelayRouting::Swapped {
                (del_r, del_l)
            } else {
                (del_l, del_r)
            };
            frame[0] = in_l * dry + output_l * wet;
            frame[1] = in_r * dry + output_r * wet;

            self.write_pos = (self.write_pos + 1) % max_len;
        }
    }
}

impl AudioEffect for StereoDelay {
    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.process_block(buffer, None);
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        self.process_block(buffer, context.transport);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        _rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_block(buffer, transport.as_ref());
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if !value.is_finite() {
            return;
        }
        let samples = || self.bounded_delay(value * 0.001 * f64::from(self.sample_rate));
        match name {
            "time" | "time_l" => self.delay_samples_l = samples(),
            "time_r" => self.delay_samples_r = samples(),
            "interpolate" => self.interpolate = value >= 0.5,
            "feedback" => self.feedback = value.clamp(0.0, 0.95),
            "mix" => self.mix = value.clamp(0.0, 1.0),
            "dry_gain" => self.dry_gain = value.clamp(0.0, MAX_GAIN_PERCENT),
            "wet_gain" => self.wet_gain = value.clamp(0.0, MAX_GAIN_PERCENT),
            "routing" => {
                if let Some(routing) = DelayRouting::from_code(value) {
                    self.routing = routing;
                }
            }
            "tempo_sync" => self.tempo_sync = value >= 0.5,
            "quarters_l" => self.quarters_l = value.clamp(MIN_QUARTERS, MAX_QUARTERS),
            "quarters_r" => self.quarters_r = value.clamp(MIN_QUARTERS, MAX_QUARTERS),
            "feedback_filter" => {
                if let Some(filter) = FeedbackFilter::from_code(value)
                    && filter != self.filter
                {
                    self.filter = filter;
                    self.filter_state.iter_mut().for_each(BiquadState::reset);
                    self.update_filter();
                }
            }
            "feedback_cutoff" => {
                let cutoff = value.clamp(MIN_CUTOFF_HZ, MAX_CUTOFF_HZ);
                if cutoff != self.cutoff {
                    self.cutoff = cutoff;
                    self.update_filter();
                }
            }
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        match name {
            "time_l" => Some(self.delay_ms(self.delay_samples_l)),
            "time_r" => Some(self.delay_ms(self.delay_samples_r)),
            "interpolate" => Some(f64::from(u8::from(self.interpolate))),
            "feedback" => Some(self.feedback),
            "mix" => Some(self.mix),
            "dry_gain" => Some(self.dry_gain),
            "wet_gain" => Some(self.wet_gain),
            "routing" => Some(self.routing.code()),
            "tempo_sync" => Some(f64::from(u8::from(self.tempo_sync))),
            "quarters_l" => Some(self.quarters_l),
            "quarters_r" => Some(self.quarters_r),
            "feedback_filter" => Some(self.filter.code()),
            "feedback_cutoff" => Some(self.cutoff),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write_pos = 0;
        self.filter_state.iter_mut().for_each(BiquadState::reset);
    }

    fn name(&self) -> &str {
        "Stereo Delay"
    }
}
