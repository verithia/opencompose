//! WAV container encoder: depth and dither live here, not in the render.

use std::path::Path;

pub use oc_dsl::ast::SampleFormat;

use super::{AudioEncoder, EncodeSpec};

/// How one WAV file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WavOptions {
    /// Sample depth written to the file.
    pub depth: SampleFormat,
    /// TPDF dither at one LSB peak (two LSBs peak to peak) before integer quantisation.
    pub dither: bool,
    /// Seed for the dither; the same seed and frames always produce the same bytes.
    pub dither_seed: u64,
}

impl WavOptions {
    /// Reject dither where it cannot do anything. Float output already carries
    /// the internal sample values, so dithering it would only add noise.
    pub fn validate(&self) -> Result<(), String> {
        if self.dither && !self.depth.is_integer() {
            return Err(format!(
                "dither is meaningless for {}: 32-bit float already carries the internal mix precision; use i16 or i24, or turn dither off",
                self.depth
            ));
        }
        Ok(())
    }
}

/// Writes interleaved float samples as a WAV file.
pub struct WavEncoder {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    options: WavOptions,
    channels: usize,
}

impl AudioEncoder for WavEncoder {
    fn open(path: &Path, spec: &EncodeSpec) -> Result<Self, String> {
        spec.validate()?;
        let channels = spec.channels;
        let wav_spec = hound::WavSpec {
            channels,
            sample_rate: spec.sample_rate,
            bits_per_sample: spec.wav.depth.bits_per_sample(),
            sample_format: if spec.wav.depth.is_integer() {
                hound::SampleFormat::Int
            } else {
                hound::SampleFormat::Float
            },
        };
        let writer = hound::WavWriter::create(path, wav_spec)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            writer,
            options: spec.wav,
            channels: usize::from(channels),
        })
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        for (index, &sample) in samples.iter().enumerate() {
            let frame = (index / self.channels) as u64;
            let channel = (index % self.channels) as u64;
            match self.options.depth {
                SampleFormat::F32 => self
                    .writer
                    .write_sample(sample)
                    .map_err(|error| error.to_string())?,
                SampleFormat::I16 if !self.options.dither => {
                    // Historical export bytes: truncation, no dither.
                    let value = super::quantise(sample, SampleFormat::I16) as i16;
                    self.writer
                        .write_sample(value)
                        .map_err(|error| error.to_string())?;
                }
                SampleFormat::I16 => {
                    let noise = dither_lsb(frame, channel, self.options.dither_seed);
                    let value = (f64::from(sample) * 32767.0 + noise).round();
                    let value = value.clamp(-32768.0, 32767.0) as i16;
                    self.writer
                        .write_sample(value)
                        .map_err(|error| error.to_string())?;
                }
                SampleFormat::I24 if !self.options.dither => {
                    let value = super::quantise(sample, SampleFormat::I24);
                    self.writer
                        .write_sample(value)
                        .map_err(|error| error.to_string())?;
                }
                SampleFormat::I24 => {
                    let noise = dither_lsb(frame, channel, self.options.dither_seed);
                    let value = (f64::from(sample) * 8_388_607.0 + noise)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0) as i32;
                    self.writer
                        .write_sample(value)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<(), String> {
        self.writer.finalize().map_err(|error| error.to_string())
    }
}

/// TPDF dither in LSB units, peak-to-peak two LSBs.
///
/// The value is a pure function of the frame index, the channel and the seed,
/// so an isolated re-render repeats byte for byte.
fn dither_lsb(frame: u64, channel: u64, seed: u64) -> f64 {
    let base = seed
        ^ frame.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ channel.wrapping_mul(0xD1B5_4A32_D192_ED03);
    uniform(base) - uniform(base ^ 0xA24B_AED4_963E_E407)
}

/// One uniform sample in `[0, 1)` from a SplitMix64 counter.
fn uniform(value: u64) -> f64 {
    let mut x = value;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::seed_for_name;

    const FRAMES: u64 = 200_000;

    fn dither_sequence(seed: u64) -> Vec<f64> {
        (0..FRAMES)
            .map(|frame| dither_lsb(frame, 0, seed))
            .collect()
    }

    fn correlation(first: &[f64], second: &[f64]) -> f64 {
        let count = first.len() as f64;
        let mean_first = first.iter().sum::<f64>() / count;
        let mean_second = second.iter().sum::<f64>() / count;
        let mut covariance = 0.0;
        let mut variance_first = 0.0;
        let mut variance_second = 0.0;
        for (a, b) in first.iter().zip(second) {
            let da = a - mean_first;
            let db = b - mean_second;
            covariance += da * db;
            variance_first += da * da;
            variance_second += db * db;
        }
        covariance / (variance_first.sqrt() * variance_second.sqrt())
    }

    /// Two stems that share a seed would carry the same noise and sum
    /// coherently. Seeds derived from the file name must not.
    #[test]
    fn seeds_derived_from_names_decorrelate_stem_dither() {
        let lead = seed_for_name(0, "song-lead");
        let bass = seed_for_name(0, "song-bass");
        assert_ne!(lead, bass);

        let first = dither_sequence(lead);
        let second = dither_sequence(bass);
        assert_eq!(first, dither_sequence(lead), "a seed repeats exactly");
        assert!(
            correlation(&first, &second).abs() < 0.02,
            "stem dither must decorrelate, correlation {}",
            correlation(&first, &second)
        );
        assert!((correlation(&first, &first) - 1.0).abs() < 1e-12);
        // The same name always yields the same seed, so a rerun repeats.
        assert_eq!(seed_for_name(0, "song-lead"), lead);
        assert_ne!(seed_for_name(7, "song-lead"), lead);
    }
}
