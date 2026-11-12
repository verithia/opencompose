//! Waveform peak pyramid: per-bucket min/max/rms for cheap canvas drawing.
//! Built offline from decoded stereo frames and cached beside the decoded media.

use std::sync::Arc;

/// Frames per bucket at the finest level (~5.3 ms at 48 kHz).
pub const BASE_BUCKET_FRAMES: u32 = 256;
/// File transient analysis uses a finer bucket and one bucket of lead-in.
pub const FILE_TRANSIENT_BUCKET_FRAMES: usize = 64;
/// Levels stop once a level has two buckets or fewer.
pub const MAX_LEVELS: usize = 20;
/// Wire header: magic, version, flags, access stamp, rate, frames, peak bytes.
pub const HEADER_BYTES: usize = 30;
const MAGIC: &[u8; 4] = b"OCPK";
const VERSION: u8 = 1;

/// One pyramid level; all three arrays share the same bucket count.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakLevel {
    pub bucket_frames: u32,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub rms: Vec<f32>,
}

impl PeakLevel {
    /// Bucket count.
    pub fn buckets(&self) -> usize {
        self.min.len()
    }
}

/// Monophonic peak ladder for one decoded file.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakPyramid {
    pub sample_rate: u32,
    pub frames: u64,
    pub levels: Vec<PeakLevel>,
}

#[derive(Debug, serde::Serialize)]
/// One transient in source time.
pub struct SourceTransient {
    pub source_seconds: f64,
    pub strength: f32,
}

#[derive(Debug, serde::Serialize)]
/// Transients reported in source time.
pub struct SourceTransients {
    pub source_duration_seconds: f64,
    pub analyzed_end_seconds: f64,
    pub resolution_seconds: f64,
    pub transients: Vec<SourceTransient>,
}

#[derive(Debug, serde::Serialize)]
/// One transient in file time.
pub struct FileTransient {
    pub file_seconds: f64,
    pub strength: f32,
}

#[derive(Debug, serde::Serialize)]
/// Transients reported over a file window.
pub struct FileTransients {
    pub start_s: f64,
    pub end_s: f64,
    pub resolution_seconds: f64,
    pub effective_gap_ms: f64,
    pub attacks: Vec<FileTransient>,
}

fn rank_attacks(
    level: &PeakLevel,
    start: usize,
    end: usize,
    gap: usize,
    limit: usize,
) -> Vec<(usize, f32)> {
    let envelope = |index: usize| {
        0.65 * level.rms[index] + 0.35 * level.min[index].abs().max(level.max[index].abs())
    };
    let mut candidates = Vec::new();
    for index in start..end {
        let before = index.checked_sub(1).map_or(0.0, envelope);
        let current = envelope(index);
        let strength = current - before;
        if strength > (0.14 * before).max(0.002) {
            candidates.push((index, strength));
        }
    }
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    let mut accepted: Vec<(usize, f32)> = Vec::new();
    for (index, strength) in candidates {
        if accepted
            .iter()
            .all(|&(other, _)| index.abs_diff(other) >= gap)
        {
            accepted.push((index, strength));
            if accepted.len() == limit {
                break;
            }
        }
    }
    accepted
}

/// Measure a decoded window in file time. Leading context before
/// `window_start_s` seeds the attack detector without becoming a candidate.
pub fn file_transients(
    buffer: &crate::analysis::AudioBuffer,
    file_start_s: f64,
    window_start_s: f64,
    min_gap_ms: f64,
    limit: usize,
) -> Result<FileTransients, String> {
    const BUCKET: usize = FILE_TRANSIENT_BUCKET_FRAMES;
    if buffer.sample_rate == 0
        || buffer.channels.is_empty()
        || !file_start_s.is_finite()
        || file_start_s < 0.0
        || !window_start_s.is_finite()
        || window_start_s < file_start_s
        || !(1.0..=250.0).contains(&min_gap_ms)
        || !(1..=128).contains(&limit)
    {
        return Err("Invalid transient window, minimum spacing or result limit".into());
    }
    let rate = f64::from(buffer.sample_rate);
    let frames = buffer.frames();
    if buffer
        .channels
        .iter()
        .any(|channel| channel.len() != frames)
    {
        return Err("Transient analysis requires equally sized audio channels".into());
    }
    let start = ((window_start_s - file_start_s) * rate).round() as usize;
    if start >= frames {
        return Err("The transient window has no audio frames".into());
    }
    let context = start.saturating_sub(BUCKET)..start;
    let mut level = PeakLevel::with_capacity(BUCKET as u32, frames - context.start);
    if !context.is_empty() {
        level.push_file_bucket(buffer, context);
    }
    let first = level.buckets();
    for index in (start..frames).step_by(BUCKET) {
        level.push_file_bucket(buffer, index..(index + BUCKET).min(frames));
    }
    let resolution_seconds = BUCKET as f64 / rate;
    let gap = (min_gap_ms / 1000.0 / resolution_seconds).ceil() as usize;
    let start_s = file_start_s + start as f64 / rate;
    Ok(FileTransients {
        start_s,
        end_s: file_start_s + frames as f64 / rate,
        resolution_seconds,
        effective_gap_ms: gap as f64 * resolution_seconds * 1000.0,
        attacks: rank_attacks(&level, first, level.buckets(), gap, limit)
            .into_iter()
            .map(|(index, strength)| FileTransient {
                file_seconds: start_s + (index - first) as f64 * resolution_seconds,
                strength,
            })
            .collect(),
    })
}

impl PeakPyramid {
    /// Build from interleaved stereo frames. Buckets span both channels.
    pub fn build(samples: &[f32], sample_rate: u32) -> Result<Self, String> {
        if !samples.len().is_multiple_of(2) {
            return Err("peak input must be interleaved stereo".into());
        }
        let frames = samples.len() / 2;
        if frames == 0 || sample_rate == 0 {
            return Err("peak input has no frames".into());
        }
        Ok(Self::from_base(
            PeakLevel::from_samples(samples, BASE_BUCKET_FRAMES),
            sample_rate,
            frames,
        ))
    }

    pub(crate) fn build_pcm(
        pcm: &crate::clip_pcm::ClipPcm,
        sample_rate: u32,
        cancellation: crate::clip_pcm::ClipCancellation<'_>,
    ) -> Result<Self, String> {
        let frames = pcm.frames();
        let mut base = PeakLevel::with_capacity(BASE_BUCKET_FRAMES, frames);
        // PCM visits use a 2048-frame scratch buffer, an exact multiple of the
        // finest peak bucket. Only the final visit can end inside a bucket.
        pcm.try_visit(0, frames, |offset, samples| {
            debug_assert!(offset.is_multiple_of(BASE_BUCKET_FRAMES as usize));
            for block in samples.chunks(4096) {
                cancellation.check()?;
                base.extend_samples(block);
            }
            Ok(())
        })?;
        Ok(Self::from_base(base, sample_rate, frames))
    }

    fn from_base(base: PeakLevel, sample_rate: u32, frames: usize) -> Self {
        let mut levels = vec![base];
        while levels.len() < MAX_LEVELS {
            let last = levels.last().expect("pyramid has a base level");
            if last.buckets() <= 2 {
                break;
            }
            let bucket_frames = last.bucket_frames.saturating_mul(2);
            levels.push(PeakLevel::from_lower(last, frames as u64, bucket_frames));
        }
        Self {
            sample_rate,
            frames: frames as u64,
            levels,
        }
    }

    /// Estimate attack times from the finest waveform buckets. Peak and RMS
    /// evidence span both channels, including transients that cancel in mono.
    /// Times mark the start of a bucket, with uncertainty of roughly one bucket.
    pub fn source_transients(
        &self,
        start_seconds: f64,
        duration_seconds: f64,
        limit: usize,
    ) -> Result<SourceTransients, String> {
        if !(start_seconds.is_finite()
            && start_seconds >= 0.0
            && 0.0 < duration_seconds
            && duration_seconds <= 30.0
            && (1..=128).contains(&limit))
        {
            return Err("Expected a nonnegative source start, a window up to 30 seconds, and 1–128 transients.".into());
        }
        let level = &self.levels[0];
        let rate = f64::from(self.sample_rate);
        let resolution = f64::from(level.bucket_frames) / rate;
        let source_duration = self.frames as f64 / rate;
        let end_seconds = (start_seconds + duration_seconds).min(source_duration);
        let start = ((start_seconds / resolution).ceil() as usize).min(level.buckets());
        let end = ((end_seconds / resolution).ceil() as usize).min(level.buckets());
        let gap = (0.035 / resolution).ceil() as usize;
        let accepted = rank_attacks(level, start, end, gap, limit);
        Ok(SourceTransients {
            source_duration_seconds: source_duration,
            analyzed_end_seconds: end_seconds,
            resolution_seconds: resolution,
            transients: accepted
                .into_iter()
                .map(|(index, strength)| SourceTransient {
                    source_seconds: index as f64 * f64::from(level.bucket_frames) / rate,
                    strength,
                })
                .collect(),
        })
    }

    /// Bytes for one file: header plus every level's three arrays.
    pub fn encoded_len(&self) -> usize {
        HEADER_BYTES
            + self
                .levels
                .iter()
                .map(|level| 8 + level.buckets() * 3 * 4)
                .sum::<usize>()
    }

    /// Encode the pyramid for the frontend.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(0);
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        let body = self.body_len();
        out.extend_from_slice(&(body as u32).to_le_bytes());
        self.write_body(&mut out);
        debug_assert_eq!(out.len(), self.encoded_len());
        out
    }

    /// Decode a peak pyramid payload.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES {
            return Err("peak payload is shorter than its header".into());
        }
        if &bytes[0..4] != MAGIC {
            return Err("peak payload has the wrong magic".into());
        }
        if bytes[4] != VERSION {
            return Err(format!("unsupported peak payload version {}", bytes[4]));
        }
        let sample_rate = u32::from_le_bytes(bytes[14..18].try_into().unwrap());
        let frames = u64::from_le_bytes(bytes[18..26].try_into().unwrap());
        let body_len = u32::from_le_bytes(bytes[26..30].try_into().unwrap()) as usize;
        if bytes.len() < HEADER_BYTES + body_len {
            return Err("peak payload is truncated".into());
        }
        let mut cursor = HEADER_BYTES;
        let mut levels = Vec::new();
        while cursor < HEADER_BYTES + body_len {
            if HEADER_BYTES + body_len - cursor < 8 {
                return Err("peak level header is truncated".into());
            }
            let bucket_frames = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
            let buckets =
                u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            cursor += 8;
            let needed = buckets * 3 * 4;
            if HEADER_BYTES + body_len - cursor < needed {
                return Err("peak level array is truncated".into());
            }
            let mut level = PeakLevel {
                bucket_frames,
                min: Vec::with_capacity(buckets),
                max: Vec::with_capacity(buckets),
                rms: Vec::with_capacity(buckets),
            };
            for target in [&mut level.min, &mut level.max, &mut level.rms] {
                for index in 0..buckets {
                    let start = cursor + index * 4;
                    target.push(f32::from_le_bytes(
                        bytes[start..start + 4].try_into().unwrap(),
                    ));
                }
                cursor += buckets * 4;
            }
            if bucket_frames == 0 {
                return Err("peak level has a zero bucket size".into());
            }
            levels.push(level);
        }
        Ok(Self {
            sample_rate,
            frames,
            levels,
        })
    }

    fn body_len(&self) -> usize {
        self.encoded_len() - HEADER_BYTES
    }

    fn write_body(&self, out: &mut Vec<u8>) {
        for level in &self.levels {
            out.extend_from_slice(&level.bucket_frames.to_le_bytes());
            out.extend_from_slice(&(level.buckets() as u32).to_le_bytes());
            for values in [&level.min, &level.max, &level.rms] {
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
    }

    /// Finest level whose buckets still cover at least one bucket per pixel.
    /// Falls back to the base level when the clip is wider than one bucket.
    pub fn level_for(&self, frames_per_pixel: f64) -> Option<&PeakLevel> {
        if frames_per_pixel <= 0.0 {
            return self.levels.first();
        }
        self.levels
            .iter()
            .filter(|level| f64::from(level.bucket_frames) <= frames_per_pixel)
            .max_by_key(|level| level.bucket_frames)
            .or_else(|| self.levels.first())
    }
}

impl PeakLevel {
    fn from_samples(samples: &[f32], bucket_frames: u32) -> Self {
        let mut level = Self::with_capacity(bucket_frames, samples.len() / 2);
        level.extend_samples(samples);
        level
    }

    fn with_capacity(bucket_frames: u32, frames: usize) -> Self {
        let bucket_frames = bucket_frames.max(1);
        let buckets = frames.div_ceil(bucket_frames as usize);
        Self {
            bucket_frames,
            min: Vec::with_capacity(buckets),
            max: Vec::with_capacity(buckets),
            rms: Vec::with_capacity(buckets),
        }
    }

    fn extend_samples(&mut self, samples: &[f32]) {
        for bucket in samples.chunks(self.bucket_frames as usize * 2) {
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            let mut energy = 0.0_f64;
            for &sample in bucket {
                low = low.min(sample);
                high = high.max(sample);
                energy += f64::from(sample) * f64::from(sample);
            }
            self.min.push(low);
            self.max.push(high);
            self.rms.push((energy / bucket.len() as f64).sqrt() as f32);
        }
    }

    fn push_file_bucket(
        &mut self,
        buffer: &crate::analysis::AudioBuffer,
        frames: std::ops::Range<usize>,
    ) {
        let mut low = f32::INFINITY;
        let mut high = f32::NEG_INFINITY;
        let mut energy = 0.0_f64;
        for channel in &buffer.channels {
            for &sample in &channel[frames.clone()] {
                low = low.min(sample as f32);
                high = high.max(sample as f32);
                energy += sample * sample;
            }
        }
        self.min.push(low);
        self.max.push(high);
        self.rms
            .push((energy / (frames.len() * buffer.channels.len()) as f64).sqrt() as f32);
    }

    /// Fold pairs of buckets from the next-finer level; rms is energy-weighted.
    fn from_lower(lower: &PeakLevel, frames: u64, bucket_frames: u32) -> Self {
        let lower_frames = lower.bucket_frames as u64;
        let buckets = lower.buckets().div_ceil(2);
        let mut level = Self {
            bucket_frames,
            min: Vec::with_capacity(buckets),
            max: Vec::with_capacity(buckets),
            rms: Vec::with_capacity(buckets),
        };
        for bucket in 0..buckets {
            let first = bucket * 2;
            let last = (first + 2).min(lower.buckets());
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            let mut energy = 0.0_f64;
            let mut count = 0.0_f64;
            for index in first..last {
                low = low.min(lower.min[index]);
                high = high.max(lower.max[index]);
                let frames_here = (frames - (index as u64 * lower_frames)).min(lower_frames) as f64;
                energy += f64::from(lower.rms[index]).powi(2) * frames_here;
                count += frames_here;
            }
            level.min.push(low);
            level.max.push(high);
            level.rms.push((energy / count.max(1.0)).sqrt() as f32);
        }
        level
    }
}

/// Shared peak payload for cache entries that expose it across threads.
pub type SharedPyramid = Arc<PeakPyramid>;

#[cfg(test)]
mod tests {
    #[test]
    fn source_transients_rank_stereo_attacks_and_bound_the_window() {
        let mut stereo = vec![0.0; 48_000 * 2];
        for (frame, amplitude) in [(12_000, 0.12), (24_000, 0.6), (36_000, 0.3)] {
            for pair in stereo[frame * 2..(frame + 480) * 2].chunks_exact_mut(2) {
                pair.copy_from_slice(&[amplitude, -amplitude]);
            }
        }
        let pyramid = PeakPyramid::build(&stereo, 48_000).unwrap();
        let result = pyramid.source_transients(0.1, 0.6, 2).unwrap();
        assert_eq!(result.source_duration_seconds, 1.0);
        assert_eq!(result.transients.len(), 2);
        for (event, time) in result.transients.iter().zip([0.5, 0.25]) {
            assert!((event.source_seconds - time).abs() <= result.resolution_seconds);
        }
        assert!(result.transients[0].strength > result.transients[1].strength);
        assert!(pyramid.source_transients(0.0, 31.0, 2).is_err());
        assert!(
            pyramid
                .source_transients(2.0, 1.0, 2)
                .unwrap()
                .transients
                .is_empty()
        );
    }

    use super::*;

    fn stereo(values: &[f32]) -> Vec<f32> {
        values.iter().flat_map(|value| [*value, *value]).collect()
    }

    fn sine(frames: usize, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|frame| (frame as f32 * 0.01).sin() * amplitude)
            .collect()
    }

    #[test]
    fn buckets_carry_exact_min_max_and_rms() {
        let values = [
            0.0, 0.5, -0.5, 0.25, -0.25, 1.0, -1.0, 0.0, 0.5, 0.5, -0.5, -0.5, 0.1, -0.1, 0.2, -0.2,
        ];
        let samples = stereo(&values);
        let level = PeakLevel::from_samples(&samples, 4);
        assert_eq!(level.buckets(), 4);
        assert_eq!(level.min[0], -0.5);
        assert_eq!(level.max[0], 0.5);
        assert_eq!(level.min[1], -1.0);
        assert_eq!(level.max[1], 1.0);
        assert_eq!(level.min[2], -0.5);
        assert_eq!(level.max[2], 0.5);
        assert_eq!(level.min[3], -0.2);
        assert_eq!(level.max[3], 0.2);
        let expected_rms = ((0.0 + 0.25 + 0.25 + 0.0625) / 4.0_f32).sqrt();
        assert!((level.rms[0] - expected_rms).abs() < 1e-6);
        // Partial trailing bucket averages only the frames it covers.
        let level = PeakLevel::from_samples(&stereo(&values[..6]), 4);
        assert_eq!(level.buckets(), 2);
        let tail = ((0.0625 + 1.0) / 2.0_f32).sqrt();
        assert!((level.rms[1] - tail).abs() < 1e-6);
    }

    #[test]
    fn stereo_peaks_do_not_cancel_in_antiphase() {
        let samples = [0.5, -0.5, 0.25, -0.25];
        let level = PeakLevel::from_samples(&samples, 2);
        assert_eq!(level.buckets(), 1);
        assert_eq!(level.min[0], -0.5);
        assert_eq!(level.max[0], 0.5);
        let expected = ((0.25 + 0.25 + 0.0625 + 0.0625) / 4.0_f32).sqrt();
        assert!((level.rms[0] - expected).abs() < 1e-6);
    }

    #[test]
    fn pyramid_levels_agree_with_the_flat_scan() {
        let samples = stereo(&sine(4096, 0.8));
        let pyramid = PeakPyramid::build(&samples, 48_000).unwrap();
        assert!(pyramid.levels.len() > 1);
        for window in pyramid.levels.windows(2) {
            let fine = &window[0];
            let coarse = &window[1];
            assert_eq!(coarse.bucket_frames, fine.bucket_frames * 2);
            assert_eq!(coarse.buckets(), fine.buckets().div_ceil(2));
            for bucket in 0..coarse.buckets() {
                let first = bucket * 2;
                let last = (first + 2).min(fine.buckets());
                let expected_min = fine.min[first..last]
                    .iter()
                    .copied()
                    .fold(f32::INFINITY, f32::min);
                let expected_max = fine.max[first..last]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                assert_eq!(coarse.min[bucket], expected_min);
                assert_eq!(coarse.max[bucket], expected_max);
                let mut energy = 0.0_f64;
                let mut count = 0.0_f64;
                for index in first..last {
                    let frames = (4096 - index * fine.bucket_frames as usize)
                        .min(fine.bucket_frames as usize) as f64;
                    energy += f64::from(fine.rms[index]).powi(2) * frames;
                    count += frames;
                }
                assert!((coarse.rms[bucket] as f64 - (energy / count).sqrt()).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn encoded_payload_round_trips() {
        let pyramid = PeakPyramid::build(&stereo(&sine(2048, 0.5)), 44_100).unwrap();
        let bytes = pyramid.to_bytes();
        assert_eq!(bytes.len(), pyramid.encoded_len());
        let decoded = PeakPyramid::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, pyramid);
    }

    #[test]
    fn level_selection_prefers_one_bucket_per_pixel() {
        let pyramid = PeakPyramid::build(&stereo(&sine(48_000, 0.5)), 48_000).unwrap();
        assert_eq!(
            pyramid.level_for(1.0).unwrap().bucket_frames,
            BASE_BUCKET_FRAMES
        );
        assert_eq!(pyramid.level_for(3000.0).unwrap().bucket_frames, 2048);
        assert_eq!(pyramid.level_for(4096.0).unwrap().bucket_frames, 4096);
        // Wider than the base bucket still has enough resolution.
        assert_eq!(
            pyramid.level_for(0.5).unwrap().bucket_frames,
            BASE_BUCKET_FRAMES
        );
    }
}
