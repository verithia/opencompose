//! A stereo carrier bank shaped by the modulator's band envelopes.

use crate::{
    AudioEffect, AudioProcessContext, AudioProcessInput,
    biquad::{BiquadCoefficients, BiquadState, FilterType},
    detector::{MIN_LEVEL, db_to_linear},
};
use oc_device_schema::vocoder as schema;

const MAX_BANDS: usize = schema::BANDS.max as usize;
const LOW_HZ: f64 = 90.0;
const HIGH_HZ: f64 = 10_500.0;

#[derive(Clone, Copy, Default)]
struct Band {
    filter: BiquadCoefficients,
    carrier: [BiquadState; 2],
    modulator: [BiquadState; 2],
    envelope: f64,
}

/// A routed key supplies the modulator. An unkeyed instance self-vocodes.
pub struct Vocoder {
    sample_rate: f64,
    bands: [Band; MAX_BANDS],
    band_count: usize,
    attack_ms: f64,
    release_ms: f64,
    attack: f64,
    release: f64,
    gain_db: f64,
    gain: f64,
    mix: f64,
}

impl Vocoder {
    pub fn new(sample_rate: u32) -> Self {
        let mut effect = Self {
            sample_rate: f64::from(sample_rate.max(1_000)),
            bands: [Band::default(); MAX_BANDS],
            band_count: schema::BANDS.default as usize,
            attack_ms: schema::ATTACK.default,
            release_ms: schema::RELEASE.default,
            attack: 0.0,
            release: 0.0,
            gain_db: schema::OUTPUT_GAIN.default,
            gain: db_to_linear(schema::OUTPUT_GAIN.default),
            mix: schema::MIX.default * 0.01,
        };
        effect.attack = effect.envelope_step(effect.attack_ms);
        effect.release = effect.envelope_step(effect.release_ms);
        effect.design_bands();
        effect
    }

    fn envelope_step(&self, ms: f64) -> f64 {
        -(-1_000.0 / (self.sample_rate * ms)).exp_m1()
    }

    fn design_bands(&mut self) {
        let count = self.band_count;
        let ratio = (HIGH_HZ.min(self.sample_rate * 0.4) / LOW_HZ).powf(1.0 / count as f64);
        // The band's half-power width spans one logarithmic interval.
        let q = ratio.sqrt() / (ratio - 1.0);
        let mut center = LOW_HZ * ratio.sqrt();
        for band in &mut self.bands[..count] {
            *band = Band {
                filter: BiquadCoefficients::design(
                    FilterType::BandPass,
                    center,
                    q,
                    0.0,
                    self.sample_rate,
                ),
                ..Band::default()
            };
            center *= ratio;
        }
    }

    fn render(&mut self, buffer: &mut [f32], key: Option<&[f32]>) {
        let key_frames = key.map(|samples| samples.as_chunks::<2>().0);
        let bands = &mut self.bands[..self.band_count];
        let dry_gain = 1.0 - self.mix;
        let wet_gain = self.mix * self.gain;
        for (index, frame) in buffer.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let dry = frame.map(f64::from);
            let modulation = key_frames
                .map(|frames| {
                    frames
                        .get(index)
                        .copied()
                        .unwrap_or_default()
                        .map(f64::from)
                })
                .unwrap_or(dry);
            let mut wet = [0.0; 2];
            for band in &mut *bands {
                let left = band.modulator[0].process(&band.filter, modulation[0]);
                let right = band.modulator[1].process(&band.filter, modulation[1]);
                // Combine detector levels after rectifying, so polarity in the
                // key does not cancel before envelope detection.
                let level = (left.abs() + right.abs()) * 0.5;
                let step = if level > band.envelope {
                    self.attack
                } else {
                    self.release
                };
                band.envelope += step * (level - band.envelope);
                if band.envelope < MIN_LEVEL {
                    band.envelope = 0.0;
                }
                for ((sum, state), input) in wet.iter_mut().zip(&mut band.carrier).zip(dry) {
                    *sum += state.process(&band.filter, input) * band.envelope;
                }
            }
            for ((sample, direct), filtered) in frame.iter_mut().zip(dry).zip(wet) {
                *sample = (direct * dry_gain + filtered * wet_gain) as f32;
            }
        }
    }
}

impl AudioEffect for Vocoder {
    fn name(&self) -> &str {
        "Vocoder"
    }

    fn process(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        self.render(buffer, None);
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        let key = match context.input {
            AudioProcessInput::Sidechain(key) => Some(key),
            _ => None,
        };
        self.render(buffer, key);
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        _sample_rate: u32,
        _transport: Option<oc_audio::PluginTransport>,
    ) {
        self.render(buffer, Some(sidechain));
    }

    fn supports_sidechain(&self) -> bool {
        true
    }

    fn set_param(&mut self, name: &str, value: f64) {
        let Some(parameter) = schema::PARAMS.iter().find(|param| param.id == name) else {
            return;
        };
        if !value.is_finite() {
            return;
        }
        let value = value.clamp(parameter.min, parameter.max);
        match name {
            "bands" => {
                let band_count = value.round() as usize;
                if band_count != self.band_count {
                    self.band_count = band_count;
                    self.design_bands();
                }
            }
            "attack" => {
                self.attack_ms = value;
                self.attack = self.envelope_step(value);
            }
            "release" => {
                self.release_ms = value;
                self.release = self.envelope_step(value);
            }
            "output_gain" => {
                self.gain_db = value;
                self.gain = db_to_linear(value);
            }
            "mix" => self.mix = value * 0.01,
            _ => {}
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        Some(match name {
            "bands" => self.band_count as f64,
            "attack" => self.attack_ms,
            "release" => self.release_ms,
            "output_gain" => self.gain_db,
            "mix" => self.mix * 100.0,
            _ => return None,
        })
    }

    fn reset(&mut self) {
        for band in &mut self.bands[..self.band_count] {
            band.carrier = [BiquadState::default(); 2];
            band.modulator = [BiquadState::default(); 2];
            band.envelope = 0.0;
        }
    }
}
