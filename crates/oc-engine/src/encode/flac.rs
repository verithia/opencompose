//! FLAC container encoder: lossless framing over the same integer grid WAV writes.

use std::io::Write;
use std::path::{Path, PathBuf};

use flacenc::component::BitRepr;
use flacenc::error::Verify;

pub use oc_dsl::ast::SampleFormat;
use oc_dsl::ast::{DEFAULT_FLAC_LEVEL, MAX_FLAC_LEVEL, MIN_FLAC_LEVEL};

use super::{AudioEncoder, EncodeSpec, quantise};

/// How one FLAC file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlacOptions {
    /// Sample depth written to the file.
    pub depth: SampleFormat,
    /// Compression level, 0 (fastest) to 8 (smallest).
    pub level: u8,
}

impl Default for FlacOptions {
    fn default() -> Self {
        Self {
            depth: SampleFormat::I16,
            level: DEFAULT_FLAC_LEVEL,
        }
    }
}

impl FlacOptions {
    /// Reject a depth or level FLAC cannot write.
    pub fn validate(&self) -> Result<(), String> {
        if !self.depth.is_integer() {
            return Err(format!(
                "{} is not a FLAC depth: FLAC stores 16- or 24-bit integer samples, and 32-bit float has no lossless integer representation",
                self.depth
            ));
        }
        if self.level > MAX_FLAC_LEVEL {
            return Err(format!(
                "FLAC compression level {} is outside {}-{}; level 0 is fastest and 8 smallest",
                self.level, MIN_FLAC_LEVEL, MAX_FLAC_LEVEL
            ));
        }
        Ok(())
    }
}

/// Writes interleaved float samples as a native FLAC file.
pub struct FlacEncoder {
    path: PathBuf,
    file: std::io::BufWriter<std::fs::File>,
    options: FlacOptions,
    channels: usize,
    sample_rate: u32,
    written: bool,
}

impl AudioEncoder for FlacEncoder {
    fn open(path: &Path, spec: &EncodeSpec) -> Result<Self, String> {
        spec.validate()?;
        let file =
            std::fs::File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file: std::io::BufWriter::new(file),
            options: spec.flac,
            channels: usize::from(spec.channels),
            sample_rate: spec.sample_rate,
            written: false,
        })
    }

    fn write(&mut self, interleaved_f32: &[f32]) -> Result<(), String> {
        if self.written {
            return Err(format!(
                "{}: the FLAC encoder writes one stream per file; the whole mix must arrive in one call",
                self.path.display()
            ));
        }
        self.written = true;
        let samples: Vec<i32> = interleaved_f32
            .iter()
            .map(|sample| quantise(*sample, self.options.depth))
            .collect();
        let stream = encode_stream(&samples, self.options, self.channels, self.sample_rate)?;
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream
            .write(&mut sink)
            .map_err(|error| format!("FLAC bitstream: {error}"))?;
        self.file
            .write_all(sink.as_slice())
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }

    fn finish(mut self) -> Result<(), String> {
        // A render that produced no frames still writes a valid empty stream.
        if !self.written {
            let stream = encode_stream(&[], self.options, self.channels, self.sample_rate)?;
            let mut sink = flacenc::bitsink::ByteSink::new();
            stream
                .write(&mut sink)
                .map_err(|error| format!("FLAC bitstream: {error}"))?;
            self.file
                .write_all(sink.as_slice())
                .map_err(|error| format!("{}: {error}", self.path.display()))?;
        }
        self.file
            .into_inner()
            .map_err(|error| format!("{}: {error}", self.path.display()))?
            .flush()
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }
}

/// Encode one interleaved integer mix into a FLAC stream.
fn encode_stream(
    samples: &[i32],
    options: FlacOptions,
    channels: usize,
    sample_rate: u32,
) -> Result<flacenc::component::Stream, String> {
    let config = config_for(options.level)
        .into_verified()
        .map_err(|(_, error)| format!("FLAC encoder configuration: {error}"))?;
    let source = flacenc::source::MemSource::from_samples(
        samples,
        channels,
        usize::from(options.depth.bits_per_sample()),
        sample_rate as usize,
    );
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|error| format!("FLAC encoding failed: {error}"))?;
    // Every frame uses the frame-number sequence, so the stream is fixed-block
    // even when the last frame is short. The encoder lowers the minimum to the
    // short frame; a strict reader then sees a variable-block stream carrying
    // fixed-block frames and gives up. Restore the nominal block size.
    stream
        .stream_info_mut()
        .set_block_sizes(config.block_size, config.block_size)
        .map_err(|error| format!("FLAC stream info: {error}"))?;
    Ok(stream)
}

/// Encoder configuration for one compression level.
///
/// Every level stays lossless; the level trades search time for bytes. Levels 0-3
/// use fixed prediction only, levels 4-8 enable quantised LPC with a longer
/// predictor each step, and the block grows from 1152 to 4096 samples.
fn config_for(level: u8) -> flacenc::config::Encoder {
    let mut config = flacenc::config::Encoder::default();
    // One file already runs beside the render's track-level parallelism, so the
    // encoder stays single-threaded and deterministic.
    config.multithread = false;
    let (block_size, use_lpc, lpc_order) = match level {
        0 => (1_152, false, 1),
        1 => (1_152, false, 1),
        2 => (2_304, false, 2),
        3 => (3_072, false, 4),
        4 => (4_096, true, 6),
        5 => (4_096, true, 8),
        6 => (4_096, true, 10),
        7 => (4_096, true, 12),
        _ => (4_096, true, 16),
    };
    config.block_size = block_size;
    config.subframe_coding.use_lpc = use_lpc;
    config.subframe_coding.qlpc.lpc_order = lpc_order;
    // Order 0 codes the sample itself: Rice coding of 24-bit noise costs far
    // more than the WAV it replaces, so the fastest levels still predict.
    config.subframe_coding.fixed.max_order = match level {
        0 => 2,
        1 => 3,
        2 => 4,
        _ => 4,
    };
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{EncodeSpec, WavOptions, encode_to_file};
    use oc_dsl::ast::ExportContainer;

    const RATE: u32 = 48_000;
    const FRAMES: usize = 4_800;

    fn temp_path(name: &str) -> PathBuf {
        oc_test_support::temp_dir("ocp-flac", name).join(format!("{name}.flac"))
    }

    fn spec(depth: SampleFormat, level: u8) -> EncodeSpec {
        EncodeSpec {
            container: ExportContainer::Flac,
            wav: WavOptions::default(),
            flac: FlacOptions { depth, level },
            mp3: Default::default(),
            sample_rate: RATE,
            channels: 2,
        }
    }

    /// A deterministic stereo tone well below full scale.
    fn mix() -> Vec<f32> {
        let mut samples = Vec::with_capacity(FRAMES * 2);
        for frame in 0..FRAMES {
            let phase = frame as f32 * 0.031;
            samples.push(0.4 * phase.sin());
            samples.push(0.3 * (phase * 1.5).cos());
        }
        samples
    }

    fn decode(path: &Path) -> (Vec<i32>, u16, u32) {
        let mut reader = claxon::FlacReader::open(path).expect("the FLAC decodes");
        let info = reader.streaminfo();
        let samples = reader.samples().map(|s| s.unwrap()).collect();
        (samples, info.bits_per_sample as u16, info.sample_rate)
    }

    #[test]
    fn a_flac_file_decodes_back_to_the_same_pcm_at_16_bit() {
        let path = temp_path("round16");
        let mix = mix();
        encode_to_file(&path, &spec(SampleFormat::I16, 5), &mix).expect("the FLAC writes");
        let (decoded, bits, rate) = decode(&path);
        assert_eq!(bits, 16);
        assert_eq!(rate, RATE);
        let expected: Vec<i32> = mix
            .iter()
            .map(|sample| quantise(*sample, SampleFormat::I16))
            .collect();
        assert_eq!(decoded, expected, "FLAC is lossless over the integer grid");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_flac_file_decodes_back_to_the_same_pcm_at_24_bit() {
        let path = temp_path("round24");
        let mix = mix();
        encode_to_file(&path, &spec(SampleFormat::I24, 5), &mix).expect("the FLAC writes");
        let (decoded, bits, rate) = decode(&path);
        assert_eq!(bits, 24);
        assert_eq!(rate, RATE);
        let expected: Vec<i32> = mix
            .iter()
            .map(|sample| quantise(*sample, SampleFormat::I24))
            .collect();
        assert_eq!(decoded, expected);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_wav_and_flac_writers_share_the_integer_grid() {
        let flac_path = temp_path("same-flac");
        let wav_path = temp_path("same-wav");
        let mix = mix();
        let flac = spec(SampleFormat::I24, 5);
        encode_to_file(&flac_path, &flac, &mix).expect("FLAC writes");
        let mut wav = flac;
        wav.container = ExportContainer::Wav;
        wav.wav.depth = SampleFormat::I24;
        encode_to_file(&wav_path, &wav, &mix).expect("WAV writes");

        let (from_flac, _, _) = decode(&flac_path);
        let mut reader = hound::WavReader::open(&wav_path).unwrap();
        let from_wav: Vec<i32> = reader.samples::<i32>().map(|s| s.unwrap()).collect();
        assert_eq!(from_flac, from_wav);
        let _ = std::fs::remove_dir_all(flac_path.parent().unwrap());
    }

    #[test]
    fn twenty_four_bit_keeps_detail_a_sixteen_bit_grid_would_drop() {
        // Half scale is exactly representable at both depths; the tiny detail
        // is one 24-bit LSB and less than one 16-bit LSB.
        let detail = 1.0 / 8_388_608.0;
        let mix = [0.5, 0.5, detail, -detail];
        let deep_path = temp_path("detail24");
        let shallow_path = temp_path("detail16");
        encode_to_file(&deep_path, &spec(SampleFormat::I24, 5), &mix).expect("FLAC writes");
        encode_to_file(&shallow_path, &spec(SampleFormat::I16, 5), &mix).expect("FLAC writes");
        assert_eq!(
            decode(&deep_path).0,
            vec![4_194_304, 4_194_304, 1, -1],
            "24-bit keeps the detail on its own grid"
        );
        assert_eq!(
            decode(&shallow_path).0,
            vec![16_383, 16_383, 0, 0],
            "16-bit truncates it away"
        );
        let _ = std::fs::remove_dir_all(deep_path.parent().unwrap());
    }

    #[test]
    fn a_higher_compression_level_writes_fewer_bytes_and_the_same_audio() {
        let fast_path = temp_path("fast");
        let best_path = temp_path("best");
        let mix = mix();
        encode_to_file(&fast_path, &spec(SampleFormat::I24, 0), &mix).expect("level 0 writes");
        encode_to_file(&best_path, &spec(SampleFormat::I24, 8), &mix).expect("level 8 writes");

        let fast = std::fs::metadata(&fast_path).unwrap().len();
        let best = std::fs::metadata(&best_path).unwrap().len();
        assert!(
            best < fast,
            "level 8 ({best} B) must not be larger than level 0 ({fast} B)"
        );
        assert_eq!(
            decode(&fast_path).0,
            decode(&best_path).0,
            "the level changes bytes, not audio"
        );
        let _ = std::fs::remove_dir_all(fast_path.parent().unwrap());
    }

    #[test]
    fn the_streaminfo_state_and_the_frame_count_match_the_mix() {
        let mix = mix();
        let options = FlacOptions {
            depth: SampleFormat::I24,
            level: 5,
        };
        let config = config_for(options.level);
        let samples: Vec<i32> = mix
            .iter()
            .map(|sample| quantise(*sample, SampleFormat::I24))
            .collect();
        let stream = encode_stream(&samples, options, 2, RATE).expect("the stream encodes");
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).unwrap();
        assert_eq!(
            &sink.as_slice()[..4],
            b"fLaC",
            "the stream starts with the marker"
        );
        let frames = FRAMES.div_ceil(config.block_size);
        assert_eq!(stream.frame_count(), frames);
        // A short last frame must not turn the stream into a variable-block one.
        assert_eq!(stream.stream_info().min_block_size(), config.block_size);
        assert_eq!(stream.stream_info().max_block_size(), config.block_size);
        assert_eq!(stream.stream_info().total_samples(), FRAMES);
        assert_eq!(stream.stream_info().bits_per_sample(), 24);
        assert_eq!(stream.stream_info().sample_rate(), RATE as usize);
        assert_eq!(stream.stream_info().channels(), 2);
    }

    #[test]
    fn a_strict_readers_decoder_accepts_the_streaminfo_and_the_frames() {
        let path = temp_path("strict");
        let mix = mix();
        encode_to_file(&path, &spec(SampleFormat::I24, 8), &mix).expect("the FLAC writes");
        // `audio_clip::decode` uses symphonia, which validates STREAMINFO against
        // the frame headers and rejected a short last frame's block size.
        let decoded = crate::audio_clip::decode(&path).expect("symphonia reads the FLAC");
        assert_eq!(decoded.sample_rate, RATE);
        assert_eq!(decoded.frames(), FRAMES);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn float_depth_and_an_out_of_range_level_are_refused_with_a_message() {
        let error = FlacOptions {
            depth: SampleFormat::F32,
            level: 5,
        }
        .validate()
        .expect_err("float has no FLAC depth");
        assert!(error.contains("FLAC stores 16- or 24-bit"), "{error}");

        let error = FlacOptions {
            depth: SampleFormat::I16,
            level: 9,
        }
        .validate()
        .expect_err("level 9 is outside the range");
        assert!(error.contains("outside 0-8"), "{error}");
    }

    #[test]
    fn a_flac_spec_that_carries_wav_dither_is_refused() {
        let mut spec = spec(SampleFormat::I24, 5);
        spec.wav.dither = true;
        let error = spec
            .validate()
            .expect_err("FLAC stores dither instead of removing it");
        assert!(error.contains("dither"), "{error}");
    }
}
