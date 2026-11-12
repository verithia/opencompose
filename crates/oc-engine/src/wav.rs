//! WAV helpers shared by offline renders and analysis.
//!
//! The mixer keeps 32-bit float samples end to end; the container encoder in
//! [`crate::encode`] is the only place that decides how much of that precision
//! reaches a file. This module keeps the small in-memory and test writers on
//! that same path.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub use oc_audio::wav::{broadcast_time_reference, open_file_reader, open_reader};
pub use oc_dsl::ast::SampleFormat;

/// Mono 16-bit WAV bytes. Small inline clips (a listen-only copy for a model)
/// never need a file, so this writes into memory with the same quantisation as
/// the integer stereo writer.
pub fn mono_wav_bytes(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Vec::new();
    {
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec)
            .map_err(|error| error.to_string())?;
        for &sample in samples {
            let value = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer
                .write_sample(value)
                .map_err(|error| error.to_string())?;
        }
        writer.finalize().map_err(|error| error.to_string())?;
    }
    Ok(bytes)
}

/// Write interleaved samples of 1 or 2 channels to 16-bit WAV bytes, for a
/// clip that has to travel inside a request.
pub fn pcm_wav_bytes(samples: &[f32], sample_rate: u32, channels: u16) -> Result<Vec<u8>, String> {
    if channels == 0 || channels > 2 {
        return Err("a clip carries one or two channels".into());
    }
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Vec::new();
    {
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec)
            .map_err(|error| error.to_string())?;
        for &sample in samples {
            let value = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer
                .write_sample(value)
                .map_err(|error| error.to_string())?;
        }
        writer.finalize().map_err(|error| error.to_string())?;
    }
    Ok(bytes)
}

/// A seekable WAV with a channel layout the PCM archive can convert in blocks.
pub struct PcmWavConversion<R> {
    source: hound::WavReader<R>,
    target_rate: u32,
    frames: usize,
}

impl<R: Read + Seek> PcmWavConversion<R> {
    /// Return unsupported sources to the caller's general audio decoder.
    pub fn new(source: hound::WavReader<R>, target_rate: u32) -> Option<Self> {
        let spec = source.spec();
        let source_frames = u64::from(source.duration());
        let source_samples = u64::from(source.len());
        if !(1..=2).contains(&spec.channels)
            || crate::encode::validate_sample_rate(spec.sample_rate).is_err()
            || crate::encode::validate_sample_rate(target_rate).is_err()
            || source_frames == 0
            || source_frames > u64::from(spec.sample_rate) * crate::audio_clip::MAX_CLIP_SECONDS
            || !matches!(
                (spec.sample_format, spec.bits_per_sample),
                (hound::SampleFormat::Int, 16 | 24 | 32) | (hound::SampleFormat::Float, 32)
            )
        {
            return None;
        }
        // Hound reports valid bits. A different container width can decode
        // differently in the general audio decoder, so keep that path.
        let mut input = source.into_inner();
        let data_offset = input.stream_position().ok()?;
        let length = input.seek(SeekFrom::End(0)).ok()?;
        let mut header = [0; 36];
        input.rewind().ok()?;
        input.read_exact(&mut header).ok()?;
        let coded_bits = u16::from_le_bytes(header[34..36].try_into().ok()?);
        let block_align = u16::from_le_bytes(header[32..34].try_into().ok()?);
        let riff_end = 8 + u64::from(u32::from_le_bytes(header[4..8].try_into().ok()?));
        let data_end =
            data_offset.checked_add(source_samples * u64::from(coded_bits.div_ceil(8)))?;
        if &header[12..16] != b"fmt "
            || data_end > length.min(riff_end)
            || coded_bits != spec.bits_per_sample
            || block_align != spec.channels * coded_bits.div_ceil(8)
        {
            return None;
        }
        input.rewind().ok()?;
        let source = hound::WavReader::new(input).ok()?;
        let frames = crate::resample::converted_frames(
            source_frames as usize,
            spec.sample_rate,
            target_rate,
        );
        Some(Self {
            source,
            target_rate,
            frames,
        })
    }

    /// The stereo PCM16 RIFF header is 44 bytes, with four bytes per frame.
    pub fn encoded_len(&self) -> u64 {
        44 + self.frames as u64 * 4
    }

    /// Reuse the render resampler's absolute positions and overlap its input
    /// windows so the output is independent of block boundaries.
    pub fn write(&mut self, output: impl io::Write + Seek) -> io::Result<()> {
        let mut writer = hound::WavWriter::new(
            io::BufWriter::new(output),
            hound::WavSpec {
                channels: 2,
                sample_rate: self.target_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .map_err(io::Error::other)?;
        self.for_each_block(|samples| {
            for &sample in samples {
                let sample = crate::encode::quantise(sample, SampleFormat::I16) as i16;
                writer.write_sample(sample).map_err(io::Error::other)?;
            }
            Ok(())
        })?;
        writer.finalize().map_err(io::Error::other)
    }

    #[cfg(feature = "mp3")]
    pub(crate) fn target_rate(&self) -> u32 {
        self.target_rate
    }

    pub(crate) fn for_each_block(
        &mut self,
        mut emit: impl FnMut(&[f32]) -> io::Result<()>,
    ) -> io::Result<()> {
        use crate::resample::{ResampleQuality, resample_read_window};
        const OUTPUT_BLOCK: usize = 16_384;

        let spec = self.source.spec();
        let source_frames = self.source.duration() as usize;
        for start in (0..self.frames).step_by(OUTPUT_BLOCK) {
            let end = (start + OUTPUT_BLOCK).min(self.frames);
            let samples = resample_read_window(
                source_frames,
                spec.sample_rate,
                self.target_rate,
                ResampleQuality::DEFAULT_RENDER,
                start..end,
                Default::default(),
                |range| read_stereo_window(&mut self.source, range),
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            emit(&samples)?;
        }
        Ok(())
    }

    /// Return the inner writer.
    pub fn into_inner(self) -> R {
        self.source.into_inner()
    }
}

/// Read interleaved stereo from an already-open WAV, duplicating a mono source.
pub fn read_stereo_window(
    source: &mut hound::WavReader<impl Read + Seek>,
    frames: std::ops::Range<usize>,
) -> Result<Vec<f32>, String> {
    let spec = source.spec();
    let channels = usize::from(spec.channels);
    if !matches!(channels, 1 | 2)
        || frames.start > frames.end
        || frames.end > source.duration() as usize
    {
        return Err("The WAV window requires one or two channels and valid frame bounds".into());
    }
    source
        .seek(frames.start as u32)
        .map_err(|error| error.to_string())?;
    let samples = frames.len() * channels;
    let mut output = Vec::with_capacity(frames.len() * 2);
    let mut push = |value| {
        output.push(value);
        if channels == 1 {
            output.push(value);
        }
    };
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in source.samples::<f32>().take(samples) {
                push(sample.map_err(|error| error.to_string())?);
            }
        }
        hound::SampleFormat::Int => {
            let magnitude = spec
                .bits_per_sample
                .checked_sub(1)
                .and_then(|shift| 1_u64.checked_shl(u32::from(shift)))
                .ok_or("invalid WAV sample width")?;
            let scale = 1.0 / magnitude as f32;
            for sample in source.samples::<i32>().take(samples) {
                push(sample.map_err(|error| error.to_string())? as f32 * scale);
            }
        }
    }
    if output.len() != frames.len() * 2 {
        return Err("The WAV ended before the requested window".into());
    }
    Ok(output)
}

/// Write interleaved stereo f32 samples to a 16-bit WAV file.
pub fn write_stereo_wav(
    path: impl AsRef<Path>,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), String> {
    let spec = crate::encode::EncodeSpec {
        container: oc_dsl::ast::ExportContainer::Wav,
        wav: crate::encode::WavOptions::default(),
        flac: crate::encode::FlacOptions::default(),
        mp3: crate::encode::Mp3Options::default(),
        sample_rate,
        channels: 2,
    };
    crate::encode::encode_to_file(path.as_ref(), &spec, samples).map(|_| ())
}

/// Write interleaved stereo f32 samples to a 32-bit float WAV file.
///
/// The samples are stored exactly as they are. A bounced track keeps the
/// headroom a float file carries instead of being clipped at full scale, and
/// no dither is added because the file is not the final delivery.
pub fn write_stereo_f32_wav(
    path: impl AsRef<Path>,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), String> {
    let path = path.as_ref();
    if !samples.len().is_multiple_of(2) {
        return Err("bounce samples must be interleaved stereo".into());
    }
    if sample_rate == 0 {
        return Err("bounce sample rate must be non-zero".into());
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    writer
        .finalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_block_conversion_leaves_padded_sample_width_to_the_general_decoder() {
        let mut source = std::io::Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new_with_spec_ex(
            &mut source,
            hound::WavSpecEx {
                spec: hound::WavSpec {
                    channels: 1,
                    sample_rate: 48_000,
                    bits_per_sample: 24,
                    sample_format: hound::SampleFormat::Int,
                },
                bytes_per_sample: 4,
            },
        )
        .unwrap();
        writer.write_sample(42_i32).unwrap();
        writer.finalize().unwrap();
        source.set_position(0);
        let wav = hound::WavReader::new(source).unwrap();
        assert!(super::PcmWavConversion::new(wav, 44_100).is_none());
    }

    #[test]
    fn wav_block_conversion_rejects_incomplete_pcm_before_writing() {
        let mut source = super::pcm_wav_bytes(&[0.5; 64], 48_000, 2).unwrap();
        source.truncate(source.len() - 4);
        let wav = hound::WavReader::new(std::io::Cursor::new(source)).unwrap();
        assert!(super::PcmWavConversion::new(wav, 44_100).is_none());
    }

    #[test]
    fn wav_block_conversion_matches_legacy_samples_and_file_bytes() {
        use super::{PcmWavConversion, pcm_wav_bytes};
        use crate::resample::{ResampleQuality, resample};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.wav");
        let rate = 48_000;
        let target = 44_100;
        for (format, bits, channels) in [
            (hound::SampleFormat::Int, 16, 2),
            (hound::SampleFormat::Int, 24, 2),
            (hound::SampleFormat::Int, 32, 1),
            (hound::SampleFormat::Float, 32, 2),
        ] {
            let mut writer = hound::WavWriter::create(
                &path,
                hound::WavSpec {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: bits,
                    sample_format: format,
                },
            )
            .unwrap();
            for i in 0..57_817 {
                for channel in 0..channels {
                    let phase = i as f32 * (0.019 + 0.037 * f32::from(channel));
                    let sample = (phase.sin() + 0.21 * (phase * 3.0).cos()) * 1.12;
                    if format == hound::SampleFormat::Float {
                        writer.write_sample(sample).unwrap();
                    } else {
                        let max = (1_u64 << (bits - 1)) - 1;
                        let value = (f64::from(sample.clamp(-1.0, 1.0)) * max as f64) as i32;
                        writer.write_sample(value).unwrap();
                    }
                }
            }
            writer.finalize().unwrap();
            let legacy = crate::audio_clip::decode(&path).unwrap();
            let legacy = resample(
                &legacy.samples,
                rate,
                target,
                ResampleQuality::DEFAULT_RENDER,
            )
            .unwrap();
            let legacy = pcm_wav_bytes(&legacy, target, 2).unwrap();
            let mut conversion =
                PcmWavConversion::new(hound::WavReader::open(&path).unwrap(), target).unwrap();
            let mut output = std::io::Cursor::new(Vec::new());
            conversion.write(&mut output).unwrap();
            assert_eq!(output.get_ref().len() as u64, conversion.encoded_len());
            assert_eq!(
                output.into_inner(),
                legacy,
                "{format:?} {bits}-bit {channels}ch"
            );
        }
    }

    use super::*;

    #[test]
    fn padded_wav_preserves_stereo_float_offsets_in_analysis_and_clip_decode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("padded.wav");
        let samples = [0.0, 0.125, 0.25, -0.5, 0.375, 0.75, -0.25, 0.5];
        write_stereo_f32_wav(&path, &samples, 8000).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.splice(12..12, *b"JUNK\x03\0\0\0abc\0");
        let mut bext = [0; 602];
        bext[338..346].copy_from_slice(&1234_u64.to_le_bytes());
        bytes.extend_from_slice(b"bext");
        bytes.extend_from_slice(&(bext.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&bext);
        let riff_size = (bytes.len() as u32 - 8).to_le_bytes();
        bytes[4..8].copy_from_slice(&riff_size);
        std::fs::write(&path, &bytes).unwrap();

        let mut reader = open_reader(&path).unwrap();
        assert_eq!(reader.duration(), 4);
        reader.seek(3).unwrap();
        assert_eq!(reader.samples::<f32>().next().unwrap().unwrap(), -0.25);
        reader.seek(1).unwrap();
        assert_eq!(reader.samples::<f32>().next().unwrap().unwrap(), 0.25);
        let mut stream = reader.into_inner();
        let data_end = stream.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
        stream.seek(SeekFrom::End(-4)).unwrap();
        let mut last = [0; 4];
        stream.read_exact(&mut last).unwrap();
        assert_eq!(f32::from_le_bytes(last), 0.5);
        assert_eq!(stream.stream_position().unwrap(), data_end);
        drop(stream);
        assert_eq!(broadcast_time_reference(&path).unwrap(), Some(1234));

        let (window, start) =
            crate::analysis::read_wav_window_with_lead(&path, 2.0 / 8000.0, 1.0 / 8000.0, 1)
                .unwrap();
        assert_eq!(start, 1.0 / 8000.0);
        assert_eq!(window.channels, [vec![0.25, 0.375], vec![-0.5, 0.75]]);
        let clip = crate::audio_clip::decode(&path).unwrap();
        assert_eq!(clip.sample_rate, 8000);
        assert_eq!(clip.samples.as_slice(), samples);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let mut crowded = bytes.clone();
        for _ in 0..4096 {
            crowded.extend_from_slice(b"JUNK\0\0\0\0");
        }
        let riff_size = (crowded.len() as u32 - 8).to_le_bytes();
        crowded[4..8].copy_from_slice(&riff_size);
        std::fs::write(&path, &crowded).unwrap();
        assert_eq!(
            broadcast_time_reference(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        bytes.truncate(bytes.len() - 602 - 8 - 1);
        std::fs::write(&path, &bytes).unwrap();
        assert!(open_reader(&path).is_err());
    }

    #[test]
    fn invalid_extensible_pcm_width_is_an_analysis_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid-width.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 8000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(0_i32).unwrap();
        writer.write_sample(0_i32).unwrap();
        writer.finalize().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[20..22], &0xfffe_u16.to_le_bytes());
        bytes[38..40].copy_from_slice(&65_u16.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let mut source = open_reader(&path).unwrap();
        assert!(
            read_stereo_window(&mut source, 0..1)
                .unwrap_err()
                .contains("invalid WAV sample width")
        );
        assert!(
            crate::analysis::read_wav(&path)
                .err()
                .unwrap()
                .contains("invalid WAV sample width")
        );
    }
}
