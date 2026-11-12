//! Container-aware export encoding.
//!
//! A render produces interleaved 32-bit float samples. An encoder decides how
//! that mix reaches a file: sample depth, dither and container framing live
//! here, never in the render. The mix itself is the same for every container.

pub mod flac;
pub mod mp3;
pub mod wav;

pub use flac::{FlacEncoder, FlacOptions};
#[cfg(feature = "mp3")]
pub use mp3::Mp3Encoder;
pub use mp3::{Mp3Options, Mp3Version};
pub use wav::{WavEncoder, WavOptions};

use std::path::Path;

use oc_dsl::ast::{ExportContainer, Project, SampleFormat};

use crate::analysis;
use crate::render_warnings;

/// Lowest sample rate the sampler accepts, matching the render flags.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
/// Highest sample rate the sampler accepts.
pub const MAX_SAMPLE_RATE: u32 = 192_000;
/// A mix more than this far above full scale is refused instead of written.
///
/// Clipping is expected on a loud master and only warns; twice full scale is
/// almost always a gain-staging mistake, so it stops the export.
pub const CLIP_REFUSAL_MARGIN_DB: f64 = 6.0;

/// Reject a sample rate the sampler cannot run.
pub fn validate_sample_rate(sample_rate: u32) -> Result<(), String> {
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(format!(
            "sample rate {sample_rate} Hz is outside the supported {MIN_SAMPLE_RATE}-{MAX_SAMPLE_RATE} Hz range; enter a rate the sampler can run, such as 44100 or 48000"
        ));
    }
    Ok(())
}

/// Everything one export needs: the container, its options, and the format of
/// the samples the render produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodeSpec {
    /// Container to write.
    pub container: ExportContainer,
    /// Options for the WAV container.
    pub wav: WavOptions,
    /// Options for the FLAC container.
    pub flac: FlacOptions,
    /// Options for the MP3 container.
    pub mp3: Mp3Options,
    /// Sample rate of the interleaved mix.
    pub sample_rate: u32,
    /// Interleaved channel count of the mix.
    pub channels: u16,
}

impl EncodeSpec {
    /// The project's declared export choice at `sample_rate`.
    pub fn for_project(project: &Project, sample_rate: u32) -> Self {
        let export = project.properties.export.unwrap_or_default();
        Self {
            container: export.container,
            wav: WavOptions {
                depth: export.wav.depth,
                // Dither is a WAV option. A project that selects FLAC keeps its
                // WAV block for a later WAV render without dithering the FLAC.
                dither: export.wav.dither && export.container == ExportContainer::Wav,
                dither_seed: 0,
            },
            flac: FlacOptions {
                depth: export.flac.depth,
                level: export.flac.level,
            },
            mp3: Mp3Options {
                bitrate: export.mp3.bitrate,
                vbr_quality: export.mp3.vbr_quality,
            },
            sample_rate,
            channels: 2,
        }
    }

    /// Sample depth the selected container writes.
    ///
    /// MP3 reports 16-bit PCM: it has no depth of its own, and its encoder takes
    /// the mix on the same integer grid a 16-bit WAV writes.
    pub fn depth(&self) -> SampleFormat {
        match self.container {
            ExportContainer::Wav => self.wav.depth,
            ExportContainer::Flac => self.flac.depth,
            ExportContainer::Mp3 => self.mp3.depth(),
        }
    }

    /// True when the selected container is written with dither.
    ///
    /// FLAC is always false: it is lossless, so dither would be stored rather
    /// than removed. MP3 is always false: it has no dither step at all.
    pub fn dither(&self) -> bool {
        match self.container {
            ExportContainer::Wav => self.wav.dither,
            ExportContainer::Flac | ExportContainer::Mp3 => false,
        }
    }

    /// File extension the selected container writes.
    pub fn extension(&self) -> &'static str {
        self.container.extension()
    }

    /// Reject a container or option combination that cannot be written.
    pub fn validate(&self) -> Result<(), String> {
        validate_sample_rate(self.sample_rate)?;
        if self.channels != 2 {
            return Err(format!(
                "this build writes stereo output; {} channels is not supported",
                self.channels
            ));
        }
        match self.container {
            ExportContainer::Wav => self.wav.validate(),
            ExportContainer::Flac => {
                self.flac.validate()?;
                if self.wav.dither {
                    return Err(
                        "dither is a WAV option: FLAC is lossless, so dither would be stored, not removed; turn dither off"
                            .into(),
                    );
                }
                Ok(())
            }
            ExportContainer::Mp3 => {
                if !cfg!(feature = "mp3") {
                    return Err(mp3::NO_MP3_ENCODER.into());
                }
                self.mp3.validate(self.sample_rate)?;
                if self.wav.dither {
                    return Err(
                        "dither is a WAV option: MP3 has no dither step, so turn dither off".into(),
                    );
                }
                Ok(())
            }
        }
    }

    /// The same spec writing with a different dither seed.
    ///
    /// The seed only affects WAV: FLAC and MP3 refuse dither.
    pub fn with_dither_seed(mut self, dither_seed: u64) -> Self {
        self.wav.dither_seed = dither_seed;
        self
    }
}

/// What one export did to the mix before quantisation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncodeReport {
    /// Sample peak of the mix before the writer quantises it.
    pub pre_clamp_peak_dbfs: f64,
    /// True peak of the mix before the writer quantises it.
    pub pre_clamp_true_peak_dbfs: f64,
    /// True when any sample exceeded full scale.
    ///
    /// A float file keeps those samples; an integer file clamps them, so this
    /// reports the overload rather than the loss.
    pub above_full_scale: bool,
    /// True when a sample above full scale reached the file clamped.
    ///
    /// Only an integer depth clamps, so a float export never reports this.
    pub clipped: bool,
}

/// Measure a mix before an encoder quantises it at `depth`.
///
/// The peak is what the mix reached; `clipped` is true only when the selected
/// depth cannot represent it and the writer clamps it.
pub fn measure(samples: &[f32], depth: SampleFormat) -> EncodeReport {
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let above_full_scale = peak > 1.0;
    EncodeReport {
        pre_clamp_peak_dbfs: linear_to_dbfs(f64::from(peak)),
        pre_clamp_true_peak_dbfs: analysis::true_peak_dbfs(samples),
        above_full_scale,
        clipped: above_full_scale && depth.is_integer(),
    }
}

/// Encode `samples` into `path` and report what the mix did before quantisation.
///
/// A mix more than [`CLIP_REFUSAL_MARGIN_DB`] above full scale is refused before
/// the file is opened. A mix above full scale is written clipped and warns, so a
/// loud master still exports; a float file keeps the samples and warns that an
/// integer export would clamp them.
pub fn encode_to_file(
    path: &Path,
    spec: &EncodeSpec,
    samples: &[f32],
) -> Result<EncodeReport, String> {
    spec.validate()?;
    let report = measure(samples, spec.depth());
    if report.pre_clamp_peak_dbfs > CLIP_REFUSAL_MARGIN_DB {
        return Err(format!(
            "the mix reaches {:+.2} dBFS, more than {CLIP_REFUSAL_MARGIN_DB:.0} dB above full scale; export refused (lower the master fader or add a `loudness:` target)",
            report.pre_clamp_peak_dbfs
        ));
    }
    match spec.container {
        ExportContainer::Wav => {
            let mut encoder = WavEncoder::open(path, spec)?;
            encoder.write(samples)?;
            encoder.finish()?;
        }
        ExportContainer::Flac => {
            let mut encoder = FlacEncoder::open(path, spec)?;
            encoder.write(samples)?;
            encoder.finish()?;
        }
        ExportContainer::Mp3 => {
            #[cfg(feature = "mp3")]
            {
                let mut encoder = mp3::Mp3Encoder::open(path, spec)?;
                encoder.write(samples)?;
                encoder.finish()?;
            }
            #[cfg(not(feature = "mp3"))]
            return Err(mp3::NO_MP3_ENCODER.into());
        }
    }
    if report.clipped {
        render_warnings::push(format!(
            "{} reaches {:+.2} dBFS before quantisation and was clipped at full scale; lower the level or add a `loudness:` target",
            path.display(),
            report.pre_clamp_peak_dbfs
        ));
    } else if report.above_full_scale {
        render_warnings::push(format!(
            "{} reaches {:+.2} dBFS before quantisation, above full scale; the float file keeps those peaks and an integer export would clamp them at full scale",
            path.display(),
            report.pre_clamp_peak_dbfs
        ));
    }
    Ok(report)
}

/// Place one float sample on the integer grid a depth writes.
///
/// The WAV and FLAC writers share this so both containers deliver the same
/// integer samples at the same depth: `i16` keeps the historical truncation,
/// `i24` rounds.
pub fn quantise(sample: f32, depth: SampleFormat) -> i32 {
    match depth {
        SampleFormat::I16 => (sample * 32_767.0).clamp(-32_768.0, 32_767.0) as i16 as i32,
        SampleFormat::I24 => (f64::from(sample) * 8_388_607.0)
            .round()
            .clamp(-8_388_608.0, 8_388_607.0) as i32,
        SampleFormat::F32 => unreachable!("float output is not quantised"),
    }
}

/// One dither seed per file, derived from the file's base name.
///
/// Stems that share a seed would carry the same noise sequence and sum
/// coherently; naming them apart decorrelates the set.
pub fn seed_for_name(base_seed: u64, name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    base_seed ^ hash
}

fn linear_to_dbfs(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Write interleaved float samples through a container encoder.
pub trait AudioEncoder {
    /// Create the file and prepare to write `spec.channels` interleaved channels.
    fn open(path: &Path, spec: &EncodeSpec) -> Result<Self, String>
    where
        Self: Sized;

    /// Write the whole interleaved mix.
    fn write(&mut self, interleaved_f32: &[f32]) -> Result<(), String>;

    /// Finish the container and close the file.
    fn finish(self) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_rate_outside_the_sampler_range_is_refused_with_the_range() {
        for rate in [0, 7_999, 192_001, u32::MAX] {
            let error = validate_sample_rate(rate).expect_err("rate must be refused");
            assert!(error.contains("8000-192000"), "{error}");
        }
        for rate in [MIN_SAMPLE_RATE, 44_100, 48_000, MAX_SAMPLE_RATE] {
            validate_sample_rate(rate).expect("rate must be accepted");
        }
    }

    #[test]
    fn a_mix_above_full_scale_reports_clipping_and_still_writes() {
        let path = std::env::temp_dir().join(format!(
            "ocp-clip-report-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let spec = EncodeSpec {
            container: ExportContainer::Wav,
            wav: WavOptions::default(),
            flac: FlacOptions::default(),
            mp3: Mp3Options::default(),
            sample_rate: 48_000,
            channels: 2,
        };
        let mix = [0.5_f32, -1.5, 1.5, -0.5];
        let report = encode_to_file(&path, &spec, &mix).expect("a clipped mix still writes");
        assert!(report.clipped);
        assert!(report.above_full_scale);
        assert!(
            (report.pre_clamp_peak_dbfs - 3.521_825).abs() < 1e-4,
            "{report:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_float_export_keeps_peaks_above_full_scale_and_is_not_clipped() {
        let path = std::env::temp_dir().join(format!(
            "ocp-float-hot-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let spec = EncodeSpec {
            container: ExportContainer::Wav,
            wav: WavOptions {
                depth: SampleFormat::F32,
                dither: false,
                dither_seed: 0,
            },
            flac: FlacOptions::default(),
            mp3: Mp3Options::default(),
            sample_rate: 48_000,
            channels: 2,
        };
        let mix = [1.995_f32, -1.995];
        let report = encode_to_file(&path, &spec, &mix).expect("a hot float mix still writes");
        assert!(report.above_full_scale, "{report:?}");
        assert!(!report.clipped, "a float file clamps nothing: {report:?}");
        assert!(
            (report.pre_clamp_peak_dbfs - 5.998).abs() < 0.01,
            "{report:?}"
        );

        // The written sample keeps the overload; only an integer depth clamps.
        let written = crate::analysis::read_wav(&path).unwrap();
        let peak = written
            .channels
            .iter()
            .flat_map(|channel| channel.iter())
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 1.0, "the float file keeps the peak, got {peak}");
        let _ = std::fs::remove_file(&path);

        // The same mix through the integer writer is clamped and reported.
        let integer_path = std::env::temp_dir().join(format!(
            "ocp-int-hot-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let integer_spec = EncodeSpec {
            wav: WavOptions::default(),
            ..spec
        };
        let report = encode_to_file(&integer_path, &integer_spec, &mix)
            .expect("an integer export still writes");
        assert!(report.above_full_scale, "{report:?}");
        assert!(report.clipped, "the integer writer clamps: {report:?}");
        let _ = std::fs::remove_file(&integer_path);
    }

    #[test]
    fn a_mix_far_above_full_scale_is_refused_without_a_file() {
        let path = std::env::temp_dir().join(format!(
            "ocp-clip-refusal-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let spec = EncodeSpec {
            container: ExportContainer::Wav,
            wav: WavOptions::default(),
            flac: FlacOptions::default(),
            mp3: Mp3Options::default(),
            sample_rate: 48_000,
            channels: 2,
        };
        let error = encode_to_file(&path, &spec, &[8.0, -8.0])
            .expect_err("twice full scale must be refused");
        assert!(error.contains("more than 6 dB above full scale"), "{error}");
        assert!(!path.exists(), "a refused export left a file behind");
    }
}
