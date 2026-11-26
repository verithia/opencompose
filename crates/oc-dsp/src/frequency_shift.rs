//! Single-sideband frequency shifting with a finite stereo quadrature filter.
use std::f64::consts::{PI, TAU};

use oc_device_schema::frequency_shift as schema;

use crate::AudioEffect;

const TAPS: usize = 63;
pub const QUADRATURE_LATENCY: usize = TAPS / 2;

/// Windowed Hilbert transform paired with a sample-aligned direct signal.
pub struct StereoQuadrature {
    coefficients: [f64; TAPS],
    history: [[f64; TAPS]; 2],
    write: usize,
}

impl Default for StereoQuadrature {
    fn default() -> Self {
        Self {
            coefficients: std::array::from_fn(|index| {
                let offset = index as i32 - QUADRATURE_LATENCY as i32;
                if offset % 2 == 0 {
                    return 0.0;
                }
                let window = 0.54 - 0.46 * (TAU * index as f64 / (TAPS - 1) as f64).cos();
                2.0 / (PI * offset as f64) * window
            }),
            history: [[0.0; TAPS]; 2],
            write: 0,
        }
    }
}

impl StereoQuadrature {
    /// Return the aligned in-phase and quadrature components per channel.
    pub fn push(&mut self, input: [f64; 2]) -> [[f64; 2]; 2] {
        let write = self.write;
        let output = std::array::from_fn(|channel| {
            let history = &mut self.history[channel];
            history[write] = input[channel];
            let mut quadrature = 0.0;
            // The odd-symmetric kernel has zero coefficients at alternate taps.
            for tap in (0..TAPS).step_by(2) {
                quadrature += self.coefficients[tap] * history[(write + TAPS - tap) % TAPS];
            }
            [
                history[(write + TAPS - QUADRATURE_LATENCY) % TAPS],
                quadrature,
            ]
        });
        self.write = (write + 1) % TAPS;
        output
    }

    pub fn reset(&mut self) {
        self.history.fill([0.0; TAPS]);
        self.write = 0;
    }
}

/// A mixer insert with signed sideband selection, feedback and aligned dry audio.
pub struct FrequencyShifter {
    quadrature: StereoQuadrature,
    dry: [[f64; 2]; QUADRATURE_LATENCY],
    dry_pos: usize,
    oscillator: f64,
    previous_wet: [f64; 2],
    shift: f64,
    sidebands: [f64; 2],
    phase: f64,
    stereo_phase: f64,
    feedback: f64,
    mix: f64,
    width: f64,
}

impl Default for FrequencyShifter {
    fn default() -> Self {
        Self {
            quadrature: StereoQuadrature::default(),
            dry: [[0.0; 2]; QUADRATURE_LATENCY],
            dry_pos: 0,
            oscillator: 0.0,
            previous_wet: [0.0; 2],
            shift: schema::SHIFT.default,
            sidebands: [schema::LEFT.default, schema::RIGHT.default],
            phase: schema::PHASE.default,
            stereo_phase: schema::STEREO_PHASE.default,
            feedback: schema::FEEDBACK.default,
            mix: schema::MIX.default,
            width: schema::WIDTH.default,
        }
    }
}

impl FrequencyShifter {
    fn parameter(&mut self, name: &str) -> Option<&mut f64> {
        Some(match name {
            "shift" => &mut self.shift,
            "sideband_left" => &mut self.sidebands[0],
            "sideband_right" => &mut self.sidebands[1],
            "phase" => &mut self.phase,
            "stereo_phase" => &mut self.stereo_phase,
            "feedback" => &mut self.feedback,
            "mix" => &mut self.mix,
            "width" => &mut self.width,
            _ => return None,
        })
    }
}

impl AudioEffect for FrequencyShifter {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        let step = TAU * self.shift / f64::from(sample_rate.max(1));
        let offsets = [
            self.phase * (TAU / 100.0),
            (self.phase + self.stereo_phase) * (TAU / 100.0),
        ];
        let (feedback, mix, width) = (self.feedback * 0.01, self.mix * 0.01, self.width * 0.01);
        for frame in buffer.as_chunks_mut::<2>().0 {
            let input = frame.map(f64::from);
            let dry = std::mem::replace(&mut self.dry[self.dry_pos], input);
            self.dry_pos = (self.dry_pos + 1) % QUADRATURE_LATENCY;
            let analytic = self.quadrature.push(std::array::from_fn(|channel| {
                // Bound the feedback path even when automation moves the oscillator rapidly.
                input[channel] + feedback * self.previous_wet[channel].clamp(-4.0, 4.0)
            }));
            let wet = std::array::from_fn::<_, 2, _>(|channel| {
                let (sine, cosine) = (self.oscillator + offsets[channel]).sin_cos();
                let [direct, quadrature] = analytic[channel];
                direct * cosine - quadrature * sine * (self.sidebands[channel] * 0.01)
            });
            self.previous_wet = wet;
            self.oscillator = (self.oscillator + step).rem_euclid(TAU);
            let mid = (wet[0] + wet[1]) * 0.5;
            let side = (wet[0] - wet[1]) * (0.5 * width);
            frame[0] = (dry[0] * (1.0 - mix) + (mid + side) * mix) as f32;
            frame[1] = (dry[1] * (1.0 - mix) + (mid - side) * mix) as f32;
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if value.is_finite()
            && let Some(param) = schema::PARAMS.iter().find(|p| p.id == name)
            && (param.min..=param.max).contains(&value)
            && let Some(current) = self.parameter(name)
        {
            *current = value;
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        Some(match name {
            "shift" => self.shift,
            "sideband_left" => self.sidebands[0],
            "sideband_right" => self.sidebands[1],
            "phase" => self.phase,
            "stereo_phase" => self.stereo_phase,
            "feedback" => self.feedback,
            "mix" => self.mix,
            "width" => self.width,
            _ => return None,
        })
    }

    fn reset(&mut self) {
        self.quadrature.reset();
        self.dry.fill([0.0; 2]);
        self.dry_pos = 0;
        self.oscillator = 0.0;
        self.previous_wet = [0.0; 2];
    }

    fn latency_samples(&self) -> u32 {
        QUADRATURE_LATENCY as u32
    }

    fn tail_samples(&self) -> u32 {
        if self.feedback == 0.0 {
            (TAPS - 1) as u32
        } else {
            8192
        }
    }

    fn name(&self) -> &str {
        "Frequency Shifter"
    }
}
