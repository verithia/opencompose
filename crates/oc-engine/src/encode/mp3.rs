//! MP3 (MPEG Layer III) container encoder over 16-bit PCM.
//!
//! Layer III has no sample depth and no dither: the encoder takes 16-bit PCM at
//! one of seven rates, which split into three MPEG versions with different
//! bitrate tables. The mix therefore enters this writer through the same integer
//! grid a 16-bit WAV uses, and a rate or bitrate outside the tables is refused
//! here rather than resampled or snapped.

use oc_dsl::ast::{
    DEFAULT_MP3_BITRATE_KBPS, MAX_MP3_VBR_QUALITY, MIN_MP3_VBR_QUALITY, SampleFormat,
};

#[cfg(feature = "mp3")]
use std::io::{Seek, Write};

#[cfg(feature = "mp3")]
use super::{AudioEncoder, EncodeSpec, quantise};

/// Sentence a build without the `mp3` feature reports for the format.
pub const NO_MP3_ENCODER: &str = "This build has no MP3 encoder";

/// Sample rates MPEG-1 Layer III writes, in hertz.
pub const MPEG1_SAMPLE_RATES: [u32; 2] = [44_100, 48_000];
/// Sample rates MPEG-2 Layer III writes, in hertz. 16 kHz is the rate an
/// audition clip is rendered at, so it must be encodable without conversion.
pub const MPEG2_SAMPLE_RATES: [u32; 3] = [16_000, 22_050, 24_000];
/// Sample rates MPEG-2.5 Layer III writes, in hertz.
pub const MPEG25_SAMPLE_RATES: [u32; 2] = [11_025, 12_000];
/// Every rate an MP3 export accepts, in table order.
pub const MP3_SAMPLE_RATES: [u32; 7] = [
    MPEG1_SAMPLE_RATES[0],
    MPEG1_SAMPLE_RATES[1],
    MPEG2_SAMPLE_RATES[0],
    MPEG2_SAMPLE_RATES[1],
    MPEG2_SAMPLE_RATES[2],
    MPEG25_SAMPLE_RATES[0],
    MPEG25_SAMPLE_RATES[1],
];

/// CBR bitrates MPEG-1 Layer III writes, in kbps.
pub const MPEG1_BITRATES: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
/// CBR bitrates MPEG-2 and MPEG-2.5 Layer III write, in kbps.
pub const MPEG2_BITRATES: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// The MPEG version a sample rate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Version {
    /// MPEG-1: 44.1 and 48 kHz.
    Mpeg1,
    /// MPEG-2: 16, 22.05 and 24 kHz.
    Mpeg2,
    /// MPEG-2.5: 11.025 and 12 kHz.
    Mpeg25,
}

impl Mp3Version {
    /// The version that carries `sample_rate`, or `None` when MP3 has no form
    /// for it.
    pub fn for_sample_rate(sample_rate: u32) -> Option<Self> {
        if MPEG1_SAMPLE_RATES.contains(&sample_rate) {
            Some(Self::Mpeg1)
        } else if MPEG2_SAMPLE_RATES.contains(&sample_rate) {
            Some(Self::Mpeg2)
        } else if MPEG25_SAMPLE_RATES.contains(&sample_rate) {
            Some(Self::Mpeg25)
        } else {
            None
        }
    }

    /// The sample rates this version writes, in hertz.
    pub fn sample_rates(self) -> &'static [u32] {
        match self {
            Self::Mpeg1 => &MPEG1_SAMPLE_RATES,
            Self::Mpeg2 => &MPEG2_SAMPLE_RATES,
            Self::Mpeg25 => &MPEG25_SAMPLE_RATES,
        }
    }

    /// The CBR bitrates this version writes, in kbps.
    pub fn bitrates(self) -> &'static [u32] {
        match self {
            Self::Mpeg1 => &MPEG1_BITRATES,
            _ => &MPEG2_BITRATES,
        }
    }

    /// Lowest and highest CBR bitrate this version writes, in kbps.
    pub fn bitrate_range(self) -> (u32, u32) {
        let table = self.bitrates();
        (table[0], table[table.len() - 1])
    }

    /// Name the version in a message.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mpeg1 => "MPEG-1",
            Self::Mpeg2 => "MPEG-2",
            Self::Mpeg25 => "MPEG-2.5",
        }
    }
}

/// How one MP3 file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp3Options {
    /// CBR bitrate in kbps. A VBR encode picks its own average, so this value
    /// does not apply when `vbr_quality` is set.
    pub bitrate: u32,
    /// VBR quality index, 0 (best) to 9 (smallest). Missing is a CBR encode.
    pub vbr_quality: Option<u8>,
}

impl Default for Mp3Options {
    fn default() -> Self {
        Self {
            bitrate: DEFAULT_MP3_BITRATE_KBPS,
            vbr_quality: None,
        }
    }
}

impl Mp3Options {
    /// Reject a rate or bitrate MPEG Layer III cannot write.
    pub fn validate(&self, sample_rate: u32) -> Result<(), String> {
        let Some(version) = Mp3Version::for_sample_rate(sample_rate) else {
            return Err(format!(
                "{sample_rate} Hz is not an MP3 sample rate: MPEG Layer III writes {} Hz and this build never resamples to reach one",
                rate_list()
            ));
        };
        if let Some(quality) = self.vbr_quality {
            if quality > MAX_MP3_VBR_QUALITY {
                return Err(format!(
                    "MP3 VBR quality {quality} is outside {MIN_MP3_VBR_QUALITY}-{MAX_MP3_VBR_QUALITY}; 0 is the best and 9 the smallest"
                ));
            }
            return Ok(());
        }
        if !version.bitrates().contains(&self.bitrate) {
            let (low, high) = version.bitrate_range();
            return Err(format!(
                "MP3 bitrate {} kbps is not in the {} Layer III table; {} writes {low}-{high} kbps: {}",
                self.bitrate,
                version.name(),
                version.name(),
                bitrate_list(version)
            ));
        }
        Ok(())
    }

    /// Sample depth the encoder consumes. Layer III has no depth of its own, so
    /// the mix is quantised to 16-bit PCM at the encoder input.
    pub fn depth(self) -> SampleFormat {
        SampleFormat::I16
    }
}

/// "44100, 48000, 16000, 22050, 24000, 11025 or 12000".
fn rate_list() -> String {
    match MP3_SAMPLE_RATES.split_last() {
        Some((last, rest)) => format!(
            "{} or {last}",
            rest.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => String::new(),
    }
}

/// "32, 40, 48, … or 320".
fn bitrate_list(version: Mp3Version) -> String {
    match version.bitrates().split_last() {
        Some((last, rest)) => format!(
            "{} or {last}",
            rest.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => String::new(),
    }
}

/// Writes interleaved float samples as an MPEG Layer III file.
#[cfg(feature = "mp3")]
pub struct Mp3Encoder {
    path: std::path::PathBuf,
    file: std::io::BufWriter<std::fs::File>,
    encoder: rusty_mp3::Mp3Encoder,
    channels: u16,
    sample_rate: u32,
    written: bool,
}

#[cfg(feature = "mp3")]
impl Mp3Encoder {
    /// Write every packet the encoder has produced so far.
    fn drain(&mut self) -> Result<(), String> {
        loop {
            match self.encoder.next_packet() {
                Ok(packet) => self
                    .file
                    .write_all(&packet)
                    .map_err(|error| format!("{}: {error}", self.path.display()))?,
                Err(rusty_mp3::Error::Again | rusty_mp3::Error::Eof) => return Ok(()),
                Err(error) => return Err(format!("MP3 encoding failed: {error}")),
            }
        }
    }
}

#[cfg(feature = "mp3")]
impl AudioEncoder for Mp3Encoder {
    fn open(path: &std::path::Path, spec: &EncodeSpec) -> Result<Self, String> {
        spec.validate()?;
        // The crate maps the 0-9 quality index onto its own target average
        // bitrate, so a `--mp3-vbr` choice lands on the average users expect.
        let vbr_quality = spec
            .mp3
            .vbr_quality
            .map(|quality| rusty_mp3::vbr_quality_index(f32::from(quality)));
        let encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
            bitrate_kbps: spec.mp3.bitrate,
            vbr_quality,
        });
        let file =
            std::fs::File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file: std::io::BufWriter::new(file),
            encoder,
            channels: spec.channels,
            sample_rate: spec.sample_rate,
            written: false,
        })
    }

    fn write(&mut self, interleaved_f32: &[f32]) -> Result<(), String> {
        self.written = true;
        // Layer III takes 16-bit PCM; the mix reaches it on the same integer
        // grid a 16-bit WAV writes.
        let samples: Vec<i16> = interleaved_f32
            .iter()
            .map(|sample| quantise(*sample, SampleFormat::I16) as i16)
            .collect();
        self.encoder
            .push_pcm_s16(&samples, self.channels, self.sample_rate)
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        self.drain()
    }

    fn finish(mut self) -> Result<(), String> {
        // An empty mix still writes a stream a player accepts: pushing nothing
        // fixes the header, and the encoder emits its minimum two frames.
        if !self.written {
            self.encoder
                .push_pcm_s16(&[], self.channels, self.sample_rate)
                .map_err(|error| format!("{}: {error}", self.path.display()))?;
        }
        self.encoder.finish();
        self.drain()?;
        // The first frame carried the tag placeholder; it is the same length, so
        // the completed Info/Xing tag goes over it in place and the file plays
        // back gapless with a correct duration.
        if let Some(tag) = self.encoder.lame_tag_frame() {
            let tag = tag.to_vec();
            self.file
                .seek(std::io::SeekFrom::Start(0))
                .map_err(|error| format!("{}: {error}", self.path.display()))?;
            self.file
                .write_all(&tag)
                .map_err(|error| format!("{}: {error}", self.path.display()))?;
        }
        self.file
            .into_inner()
            .map_err(|error| format!("{}: {error}", self.path.display()))?
            .flush()
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }
}

/// Transcode a file at its native rate without retaining the decoded mix.
/// The caller supplies a staging path and publishes it only on success.
pub fn transcode_at_source_rate(
    source: &std::path::Path,
    stage: &std::path::Path,
    spec: &super::EncodeSpec,
) -> Result<(), String> {
    #[cfg(not(feature = "mp3"))]
    {
        let _ = (source, stage, spec);
        Err(NO_MP3_ENCODER.into())
    }
    #[cfg(feature = "mp3")]
    {
        transcode_blocks(source, stage, spec, |emit| {
            let sample_rate = crate::audio_clip::decode_packets(source, |block| {
                emit(block).map_err(|error| error.to_string().into())
            })?;
            if sample_rate != spec.sample_rate {
                return Err("The source sample rate does not match the MP3 target".into());
            }
            Ok(())
        })
    }
}

/// Resample a seekable WAV straight into the MP3 encoder on a staging path.
pub fn transcode_resampled_wav<R: std::io::Read + std::io::Seek>(
    source: &std::path::Path,
    conversion: &mut crate::wav::PcmWavConversion<R>,
    stage: &std::path::Path,
    spec: &super::EncodeSpec,
) -> Result<(), String> {
    #[cfg(not(feature = "mp3"))]
    {
        let _ = (source, conversion, stage, spec);
        Err(NO_MP3_ENCODER.into())
    }
    #[cfg(feature = "mp3")]
    {
        if conversion.target_rate() != spec.sample_rate {
            return Err("The resampled rate does not match the MP3 target".into());
        }
        transcode_blocks(source, stage, spec, |emit| {
            conversion
                .for_each_block(emit)
                .map_err(|error| error.to_string())
        })
    }
}

#[cfg(feature = "mp3")]
fn transcode_blocks(
    source: &std::path::Path,
    stage: &std::path::Path,
    spec: &EncodeSpec,
    read: impl FnOnce(&mut dyn FnMut(&[f32]) -> std::io::Result<()>) -> Result<(), String>,
) -> Result<(), String> {
    if spec.channels != 2 || spec.container != oc_dsl::ast::ExportContainer::Mp3 {
        return Err("MP3 transcoding requires a stereo MP3 target".into());
    }
    let mut encoder = Mp3Encoder::open(stage, spec)?;
    let mut peak = 0.0_f32;
    read(&mut |block| {
        peak = block
            .iter()
            .fold(peak, |peak, sample| peak.max(sample.abs()));
        if super::linear_to_dbfs(f64::from(peak)) > super::CLIP_REFUSAL_MARGIN_DB {
            return Err(std::io::Error::other(
                "The source exceeds the MP3 encoding peak limit",
            ));
        }
        encoder.write(block).map_err(std::io::Error::other)
    })?;
    encoder.finish()?;
    if peak > 1.0 {
        crate::render_warnings::push(format!(
            "{} reaches {:+.2} dBFS before quantisation and was clipped at full scale",
            source.display(),
            super::linear_to_dbfs(f64::from(peak)),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "mp3")]
    use crate::encode::encode_to_file;
    use crate::encode::{EncodeSpec, WavOptions};
    use oc_dsl::ast::ExportContainer;

    const RATE: u32 = 48_000;
    #[cfg(feature = "mp3")]
    const FRAMES: usize = 24_000;

    #[cfg(feature = "mp3")]
    fn temp_path(name: &str) -> std::path::PathBuf {
        oc_test_support::temp_dir("ocp-mp3", name).join(format!("{name}.mp3"))
    }

    fn spec(sample_rate: u32, bitrate: u32, vbr_quality: Option<u8>) -> EncodeSpec {
        EncodeSpec {
            container: ExportContainer::Mp3,
            wav: WavOptions::default(),
            flac: Default::default(),
            mp3: Mp3Options {
                bitrate,
                vbr_quality,
            },
            sample_rate,
            channels: 2,
        }
    }

    /// A deterministic stereo tone well below full scale.
    #[cfg(feature = "mp3")]
    fn mix(frames: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let phase = frame as f32 * 0.031;
            samples.push(0.4 * phase.sin());
            samples.push(0.3 * (phase * 1.5).cos());
        }
        samples
    }

    #[cfg(feature = "mp3")]
    fn decode(path: &std::path::Path) -> crate::audio_clip::DecodedAudio {
        crate::audio_clip::decode(path).expect("the MP3 decodes")
    }

    #[test]
    fn the_version_table_maps_every_rate_to_its_bitrate_range() {
        for rate in MPEG1_SAMPLE_RATES {
            let version = Mp3Version::for_sample_rate(rate).unwrap();
            assert_eq!(version, Mp3Version::Mpeg1);
            assert_eq!(version.bitrate_range(), (32, 320));
            assert!(version.bitrates().contains(&320));
        }
        for rate in MPEG2_SAMPLE_RATES {
            let version = Mp3Version::for_sample_rate(rate).unwrap();
            assert_eq!(version, Mp3Version::Mpeg2);
            assert_eq!(version.bitrate_range(), (8, 160));
        }
        for rate in MPEG25_SAMPLE_RATES {
            let version = Mp3Version::for_sample_rate(rate).unwrap();
            assert_eq!(version, Mp3Version::Mpeg25);
            assert_eq!(version.bitrate_range(), (8, 160));
        }
        for rate in [8_000, 32_000, 88_200, 96_000, 192_000, 0] {
            assert_eq!(Mp3Version::for_sample_rate(rate), None, "{rate}");
        }
    }

    #[test]
    fn every_table_entry_is_accepted_and_the_range_edges_are_named() {
        for rate in MP3_SAMPLE_RATES {
            let version = Mp3Version::for_sample_rate(rate).unwrap();
            for &bitrate in version.bitrates() {
                Mp3Options {
                    bitrate,
                    vbr_quality: None,
                }
                .validate(rate)
                .unwrap_or_else(|error| panic!("{rate} Hz / {bitrate} kbps: {error}"));
            }
            // A value between two table entries is refused, not snapped.
            let gap = if version == Mp3Version::Mpeg1 {
                200
            } else {
                100
            };
            let error = Mp3Options {
                bitrate: gap,
                vbr_quality: None,
            }
            .validate(rate)
            .expect_err("a bitrate between table entries must be refused");
            let (low, high) = version.bitrate_range();
            assert!(error.contains(&format!("{gap} kbps")), "{error}");
            assert!(error.contains(&format!("{low}-{high} kbps")), "{error}");
        }
    }

    #[test]
    fn a_rate_outside_the_table_is_refused_with_the_rate_and_the_allowed_set() {
        for rate in [96_000, 88_200, 32_000, 0] {
            let error = Mp3Options::default()
                .validate(rate)
                .expect_err("a rate with no MPEG form must be refused");
            assert!(error.contains(&format!("{rate} Hz")), "{error}");
            assert!(
                error.contains("44100, 48000, 16000, 22050, 24000, 11025 or 12000"),
                "{error}"
            );
            assert!(error.contains("never resamples"), "{error}");
        }
        for rate in MP3_SAMPLE_RATES {
            let version = Mp3Version::for_sample_rate(rate).unwrap();
            Mp3Options {
                bitrate: version.bitrates()[8],
                vbr_quality: None,
            }
            .validate(rate)
            .expect("table rate");
        }
    }

    #[test]
    fn a_vbr_quality_outside_the_index_is_refused() {
        let error = Mp3Options {
            bitrate: 192,
            vbr_quality: Some(MAX_MP3_VBR_QUALITY + 1),
        }
        .validate(RATE)
        .expect_err("quality 10 must be refused");
        assert!(error.contains("0-9"), "{error}");
        Mp3Options {
            bitrate: 999,
            vbr_quality: Some(2),
        }
        .validate(RATE)
        .expect("VBR chooses its own average bitrate");
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn mp3_packet_transcode_matches_encoding_the_same_decoded_mix() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("stereo.wav");
        let reference = dir.path().join("reference.mp3");
        let streamed = dir.path().join("streamed.mp3");
        let input: Vec<f32> = (0..(2 * FRAMES + 193))
            .flat_map(|frame| {
                let phase = frame as f32 * 0.057;
                [phase.sin() * 0.45, (phase * 1.7).cos() * 0.2]
            })
            .collect();
        std::fs::write(&source, crate::wav::pcm_wav_bytes(&input, RATE, 2).unwrap()).unwrap();
        let decoded = crate::audio_clip::decode(&source).unwrap();
        let spec = spec(RATE, 192, None);
        encode_to_file(&reference, &spec, &decoded.samples).unwrap();
        super::transcode_at_source_rate(&source, &streamed, &spec).unwrap();
        assert_eq!(
            std::fs::read(streamed).unwrap(),
            std::fs::read(reference).unwrap()
        );
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn mp3_resampled_wav_matches_encoding_the_full_converted_mix() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("stereo.wav");
        let reference = dir.path().join("reference.mp3");
        let streamed = dir.path().join("streamed.mp3");
        let source_rate = 96_000;
        let input: Vec<f32> = (0..(4 * FRAMES + 193))
            .flat_map(|frame| {
                let phase = frame as f32 * 0.057;
                [phase.sin() * 0.45, (phase * 1.7).cos() * 0.2]
            })
            .collect();
        std::fs::write(
            &source,
            crate::wav::pcm_wav_bytes(&input, source_rate, 2).unwrap(),
        )
        .unwrap();
        let decoded = crate::audio_clip::decode(&source).unwrap();
        let converted = crate::resample::resample(
            &decoded.samples,
            source_rate,
            RATE,
            crate::resample::ResampleQuality::DEFAULT_RENDER,
        )
        .unwrap();
        let spec = spec(RATE, 192, None);
        encode_to_file(&reference, &spec, &converted).unwrap();
        let wav = hound::WavReader::open(&source).unwrap();
        let mut conversion = crate::wav::PcmWavConversion::new(wav, RATE).unwrap();
        super::transcode_resampled_wav(&source, &mut conversion, &streamed, &spec).unwrap();
        assert_eq!(
            std::fs::read(streamed).unwrap(),
            std::fs::read(reference).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn a_mp3_file_decodes_back_to_the_expected_tone_and_length() {
        let path = temp_path("roundtrip");
        let samples = mix(FRAMES);
        encode_to_file(&path, &spec(RATE, 192, None), &samples).expect("the MP3 writes");
        let decoded = decode(&path);
        assert_eq!(decoded.sample_rate, RATE);
        assert!(
            decoded.samples.iter().all(|sample| sample.is_finite()),
            "the decoder must not produce NaN or infinity"
        );
        // Layer III pads to whole frames on both ends; the Info tag says how much
        // to trim, and a frame is 1152 samples.
        let difference = decoded.frames().abs_diff(FRAMES);
        assert!(
            difference <= 1_152,
            "decoded {} frames against {FRAMES} written",
            decoded.frames()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn the_first_frame_is_an_info_tag_with_the_requested_rate_and_bitrate() {
        let path = temp_path("header");
        encode_to_file(&path, &spec(RATE, 192, None), &mix(FRAMES)).expect("the MP3 writes");
        let bytes = std::fs::read(&path).unwrap();
        let header =
            rusty_mp3::header::FrameHeader::parse([bytes[0], bytes[1], bytes[2], bytes[3]])
                .expect("the file starts with a frame header");
        assert_eq!(header.sample_rate, RATE);
        assert_eq!(header.bitrate_kbps, 192);
        assert_eq!(header.version, rusty_mp3::header::MpegVersion::V1);
        let tag = &bytes[4 + header.side_info_len()..];
        assert_eq!(&tag[..4], b"Info", "CBR writes an Info tag, not XING");
        // The extension block carries the encoder delay and padding, so a
        // player trims the stream back to the mix length.
        // The extension block starts with the marker earlier encoders wrote;
        // readers key their delay/padding parsing on those four bytes.
        assert!(
            tag.windows(4).any(|window| window == b"LAME"),
            "no extension block"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn the_file_is_a_whole_number_of_fixed_size_frames() {
        let path = temp_path("frames");
        let samples = mix(FRAMES);
        encode_to_file(&path, &spec(RATE, 192, None), &samples).expect("the MP3 writes");
        let bytes = std::fs::read(&path).unwrap();
        let frame_size = (192 * 1_000 / 8 * 1_152 / RATE) as usize;
        assert_eq!(
            bytes.len() % frame_size,
            0,
            "{} bytes is not a whole number of {frame_size}-byte frames",
            bytes.len()
        );
        // The mix plus both codec delays, rounded up to frames, plus the tag.
        let expected = (FRAMES + 1_057).div_ceil(1_152) + 1;
        assert_eq!(bytes.len() / frame_size, expected);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn a_mpeg2_rate_writes_its_own_version_and_bitrate_table() {
        let path = temp_path("mpeg2");
        encode_to_file(&path, &spec(22_050, 64, None), &mix(11_025)).expect("the MP3 writes");
        let bytes = std::fs::read(&path).unwrap();
        let header =
            rusty_mp3::header::FrameHeader::parse([bytes[0], bytes[1], bytes[2], bytes[3]])
                .expect("the file starts with a frame header");
        assert_eq!(header.sample_rate, 22_050);
        assert_eq!(header.bitrate_kbps, 64);
        assert_eq!(header.version, rusty_mp3::header::MpegVersion::V2);
        let decoded = decode(&path);
        assert_eq!(decoded.sample_rate, 22_050);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn a_vbr_encode_writes_a_xing_tag_and_a_different_size() {
        let cbr_path = temp_path("cbr");
        let vbr_path = temp_path("vbr");
        let samples = mix(FRAMES);
        encode_to_file(&cbr_path, &spec(RATE, 192, None), &samples).expect("CBR writes");
        // Quality 9 targets a much lower average bitrate, so the sizes differ;
        // quality 2 lands on the CBR 192 size and would prove nothing.
        encode_to_file(&vbr_path, &spec(RATE, 192, Some(9)), &samples).expect("VBR writes");
        let vbr = std::fs::read(&vbr_path).unwrap();
        let header = rusty_mp3::header::FrameHeader::parse([vbr[0], vbr[1], vbr[2], vbr[3]])
            .expect("header");
        let tag = &vbr[4 + header.side_info_len()..];
        assert_eq!(&tag[..4], b"Xing", "VBR writes a Xing tag");
        assert_ne!(
            std::fs::metadata(&cbr_path).unwrap().len(),
            std::fs::metadata(&vbr_path).unwrap().len()
        );
        let decoded = decode(&vbr_path);
        assert!(
            decoded.frames().abs_diff(FRAMES) <= 1_152,
            "decoded {} frames against {FRAMES} written",
            decoded.frames()
        );
        let _ = std::fs::remove_dir_all(cbr_path.parent().unwrap());
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn an_empty_mix_still_writes_a_stream_a_player_accepts() {
        let path = temp_path("empty");
        encode_to_file(&path, &spec(RATE, 192, None), &[]).expect("an empty mix writes");
        let bytes = std::fs::read(&path).unwrap();
        let header =
            rusty_mp3::header::FrameHeader::parse([bytes[0], bytes[1], bytes[2], bytes[3]])
                .expect("the file starts with a frame header");
        let frame_size = header.frame_size();
        // The tag frame plus the crate's minimum of two audio frames.
        assert_eq!(bytes.len(), frame_size * 3);
        // Gapless trimming reduces an empty input to zero samples, so the shared
        // clip decoder refuses it; the codec's own decoder still reads frames.
        let mut decoder = rusty_mp3::Mp3Decoder::new();
        decoder.push(&bytes);
        decoder.flush();
        let decoded = decoder.next_frame().expect("the empty stream decodes");
        assert_eq!(decoded.sample_rate, RATE);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(not(feature = "mp3"))]
    #[test]
    fn an_mp3_spec_without_the_codec_refuses_before_codec_options() {
        for dither in [false, true] {
            let mut unavailable = spec(RATE, 192, None);
            unavailable.wav.dither = dither;
            assert_eq!(unavailable.validate().unwrap_err(), NO_MP3_ENCODER);
        }
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn an_mp3_spec_refuses_dither() {
        let mut with_dither = spec(RATE, 192, None);
        with_dither.wav.dither = true;
        let error = with_dither.validate().expect_err("MP3 has no dither step");
        assert!(error.contains("dither"), "{error}");
    }

    #[test]
    #[cfg(feature = "mp3")]
    fn the_encoder_carries_the_same_integer_grid_as_a_16_bit_wav() {
        let wav_path = temp_path("grid-wav");
        let mp3_path = temp_path("grid-mp3");
        let samples = mix(4_800);
        let mut wav = spec(RATE, 192, None);
        wav.container = ExportContainer::Wav;
        wav.wav.depth = SampleFormat::I16;
        encode_to_file(&wav_path.with_extension("wav"), &wav, &samples).expect("WAV writes");
        encode_to_file(&mp3_path, &spec(RATE, 192, None), &samples).expect("MP3 writes");
        // The MP3 is lossy, so the check is that its decoded first frame is
        // close to the WAV's integer samples rather than equal to them.
        let decoded = decode(&mp3_path);
        let mut reader = hound::WavReader::open(wav_path.with_extension("wav")).unwrap();
        let pcm: Vec<i32> = reader
            .samples::<i32>()
            .map(|sample| sample.unwrap())
            .collect();
        let decoded: Vec<f32> = decoded.samples.iter().copied().collect();
        assert!(decoded.len() >= pcm.len());
        let mut worst = 0.0_f32;
        for (index, expected) in pcm.iter().enumerate() {
            let expected = *expected as f32 / 32_768.0;
            worst = worst.max((decoded[index] - expected).abs());
        }
        assert!(worst < 0.05, "peak difference {worst}");
        let _ = std::fs::remove_dir_all(wav_path.parent().unwrap());
    }
}
