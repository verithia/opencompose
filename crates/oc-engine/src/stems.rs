//! The band fallback behind `separate_stems`.
//!
//! This is a filter split, not a source separation: one Linkwitz-Riley
//! crossover at [`SPLIT_HZ`] writes a low band and a high band. The two bands
//! sum to an allpass, so the split keeps the mix's magnitude and level while
//! moving content between the bands. A stereo source also yields `centre`, the
//! mono mid: in equal-weight M/S, mid minus side is the right channel, so the
//! fallback writes the mid itself.
use std::path::Path;

use oc_dsp::crossover::{MultibandCrossover, PhaseMode, Slope};

use crate::analysis::AudioBuffer;

/// Crossfade corner between the low and high bands.
pub const SPLIT_HZ: f64 = 200.0;
/// 24 dB/oct: the low band is 3 kHz content about 90 dB down.
pub const SLOPE: Slope = Slope::Oct24;

/// Two band buffers plus the optional mid, at the source rate.
#[derive(Debug, Clone, PartialEq)]
pub struct BandSplit {
    pub sample_rate: u32,
    pub low: Vec<Vec<f64>>,
    pub high: Vec<Vec<f64>>,
    /// Mono mid, present only for a stereo source.
    pub centre: Option<Vec<Vec<f64>>>,
}

impl BandSplit {
    /// Frames per band.
    pub fn frames(&self) -> usize {
        self.low.first().map_or(0, Vec::len)
    }
}

/// Split a mono or stereo buffer at [`SPLIT_HZ`], preserving channel count.
pub fn band_split(buffer: &AudioBuffer) -> Result<BandSplit, String> {
    if buffer.channel_count() == 0 || buffer.channel_count() > 2 {
        return Err(format!(
            "the band split handles mono or stereo; this file has {} channels",
            buffer.channel_count()
        ));
    }
    if buffer.frames() == 0 {
        return Err("the WAV has no audio frames".into());
    }
    let channels = buffer.channel_count();
    let mut crossover = MultibandCrossover::new(buffer.sample_rate);
    crossover.configure(2, [SPLIT_HZ; 3], SLOPE, PhaseMode::Minimum);
    let mut low = vec![Vec::with_capacity(buffer.frames()); channels];
    let mut high = vec![Vec::with_capacity(buffer.frames()); channels];
    for frame in 0..buffer.frames() {
        let left = buffer.channels[0][frame];
        let right = if channels == 2 {
            buffer.channels[1][frame]
        } else {
            left
        };
        let bands = crossover.process_frame(left, right);
        low[0].push(bands[0][0]);
        high[0].push(bands[1][0]);
        if channels == 2 {
            low[1].push(bands[0][1]);
            high[1].push(bands[1][1]);
        }
    }
    let centre = (channels == 2).then(|| {
        let mid: Vec<f64> = buffer.channels[0]
            .iter()
            .zip(&buffer.channels[1])
            .map(|(left, right)| 0.5 * (left + right))
            .collect();
        vec![mid]
    });
    Ok(BandSplit {
        sample_rate: buffer.sample_rate,
        low,
        high,
        centre,
    })
}

/// Peak of every channel in dBFS; `None` when the stem is digital silence.
pub fn peak_dbfs(channels: &[Vec<f64>]) -> Option<f64> {
    let peak = channels
        .iter()
        .flat_map(|channel| channel.iter())
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
    (peak > 0.0).then(|| 20.0 * peak.log10())
}

/// Write one stem as a 16-bit WAV at the source rate, `channels` interleaved.
pub fn write_stem_wav(
    path: impl AsRef<Path>,
    channels: &[Vec<f64>],
    sample_rate: u32,
) -> Result<(), String> {
    let path = path.as_ref();
    if channels.is_empty() {
        return Err("the stem has no channels".into());
    }
    let frames = channels[0].len();
    if channels.iter().any(|channel| channel.len() != frames) {
        return Err("the stem's channels differ in length".into());
    }
    if frames == 0 {
        return Err("the stem has no frames".into());
    }
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    for frame in 0..frames {
        for channel in channels {
            let value = (channel[frame] * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer
                .write_sample(value)
                .map_err(|error| error.to_string())?;
        }
    }
    writer.finalize().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn two_tones(seconds: f64) -> AudioBuffer {
        let frames = (seconds * f64::from(RATE)) as usize;
        let samples = (0..frames)
            .map(|frame| {
                let time = frame as f64 / f64::from(RATE);
                let tau = std::f64::consts::TAU;
                0.4 * (tau * 200.0 * time).sin() + 0.3 * (tau * 3_000.0 * time).sin()
            })
            .collect();
        AudioBuffer {
            sample_rate: RATE,
            channels: vec![samples],
        }
    }

    fn amplitude(samples: &[f64], hz: f64) -> f64 {
        let window: Vec<f64> = (0..samples.len())
            .map(|index| {
                0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / samples.len() as f64).cos()
            })
            .collect();
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (index, (sample, weight)) in samples.iter().zip(&window).enumerate() {
            let angle = std::f64::consts::TAU * hz * index as f64 / f64::from(RATE);
            real += weight * sample * angle.cos();
            imaginary -= weight * sample * angle.sin();
        }
        let magnitude = (real * real + imaginary * imaginary).sqrt();
        2.0 * magnitude / window.iter().sum::<f64>()
    }

    fn rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn the_split_moves_each_tone_into_its_band_and_sums_back_in_level() {
        let input = two_tones(2.0);
        let split = band_split(&input).unwrap();
        assert_eq!(split.sample_rate, RATE);
        assert!(split.centre.is_none(), "a mono source has no mid");

        let low_200 = amplitude(&split.low[0], 200.0);
        let low_3k = amplitude(&split.low[0], 3_000.0);
        let high_3k = amplitude(&split.high[0], 3_000.0);
        let high_200 = amplitude(&split.high[0], 200.0);
        assert!(low_200 > 0.1, "low stem is silent at 200 Hz: {low_200}");
        assert!(
            low_200 > low_3k * 100.0,
            "low stem: 200 Hz {low_200} vs 3 kHz {low_3k}"
        );
        assert!(high_3k > 0.1, "high stem is silent at 3 kHz: {high_3k}");
        assert!(
            high_3k > high_200 * 1.4,
            "high stem: 3 kHz {high_3k} vs 200 Hz {high_200}"
        );

        let sum: Vec<f64> = split.low[0]
            .iter()
            .zip(&split.high[0])
            .map(|(low, high)| low + high)
            .collect();
        let deviation = 20.0 * (rms(&sum) / rms(&input.channels[0])).log10();
        assert!(
            deviation.abs() < 0.5,
            "the band sum is {deviation} dB from the input"
        );
    }

    #[test]
    fn a_stereo_source_also_yields_the_mid() {
        let frames = 4_800;
        let left: Vec<f64> = (0..frames)
            .map(|frame| {
                0.5 * (std::f64::consts::TAU * 200.0 * frame as f64 / f64::from(RATE)).sin()
            })
            .collect();
        let right: Vec<f64> = left.iter().map(|sample| -sample).collect();
        let split = band_split(&AudioBuffer {
            sample_rate: RATE,
            channels: vec![left, right],
        })
        .unwrap();
        let centre = split.centre.expect("a stereo source has a mid");
        assert_eq!(centre.len(), 1);
        assert!(centre[0].iter().all(|sample| sample.abs() < 1e-9));
        assert_eq!(split.low.len(), 2);
        assert_eq!(split.high[1].len(), frames);
    }

    #[test]
    fn a_written_stem_is_16_bit_at_the_source_rate() {
        let split = band_split(&two_tones(0.2)).unwrap();
        let dir = std::env::temp_dir().join(format!("oc-stems-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("low.wav");
        write_stem_wav(&path, &split.low, split.sample_rate).unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_rate, RATE);
        assert_eq!(spec.channels, 1);
        assert_eq!(reader.len() as usize, split.frames());
        assert!(peak_dbfs(&split.low).unwrap() < 0.0);
        assert!(peak_dbfs(&[vec![0.0; 8]]).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
