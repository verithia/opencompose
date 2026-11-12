//! Offline audio analysis and A/B comparison.
//!
//! Metric definitions follow `scripts/wav_compare.py` so an agent can
//! reproduce the A/B numbers without a Python dependency: octave band energy,
//! 64-band log-mel distance, BS.1770-4 integrated loudness and the heuristic
//! headline score. Analysis is pure computation on decoded samples.

/// Beat-evidence helpers used by analysis.
pub mod beat_evidence;

use std::path::Path;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::Serialize;

const NEG_INF_DB: f64 = -120.0;
const MEL_BANDS: usize = 64;
const MEL_FMIN: f64 = 30.0;
const MEL_FMAX: f64 = 16_000.0;
const OCTAVE_CENTERS: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0,
];
const THIRD_OCTAVE_CENTERS: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10_000.0, 12_500.0, 16_000.0, 20_000.0,
];
const SHAPE_N_FFT: usize = 2048;
const SHAPE_HOP: usize = 512;
const MFCC_BANDS: usize = 64;
const MFCC_COEFFICIENTS: usize = 13;
const MFCC_DCT_OUT: usize = 20;
const LSD_FLOOR: f64 = 1e-12;
const SPECTRAL_SCALE_DB: f64 = 12.0;
const LEVEL_SCALE_LU: f64 = 6.0;
const BODY_SCALE_DB: f64 = 4.0;
const TILT_SCALE_DB: f64 = 10.0;
const CREST_SCALE_DB: f64 = 6.0;
const FLUX_SCALE_DB: f64 = 6.0;
const HARMONIC_SCALE_DB: f64 = 6.0;
const MFCC_SCALE: f64 = 0.10;
const THIRD_OCTAVE_SCALE_DB: f64 = 6.0;
const LSD_SCALE_DB: f64 = 12.0;
const ONSET_SCALE_DB: f64 = 6.0;
const DENSITY_SCALE_DB: f64 = 6.0;
/// Events/s from fewer than this many detected attacks is not a density estimate.
const DENSITY_MIN_EVENTS: usize = 5;
/// v1.2.0 weights scaled by 0.78; the five v1.3.0 terms add the released 0.22.
const SCORE_WEIGHTS: [f64; 12] = [
    0.2800, 0.1270, 0.1010, 0.0940, 0.0780, 0.0540, 0.0460, 0.0600, 0.0500, 0.0450, 0.0400, 0.0250,
];
const PREVIOUS_SCORE_WEIGHTS: [f64; 7] = [0.3575, 0.1625, 0.13, 0.12, 0.10, 0.07, 0.06];
const LEGACY_SCORE_WEIGHTS: [f64; 3] = [0.55, 0.25, 0.20];
const BODY_LOW_HZ: (f64, f64) = (20.0, 120.0);
const BODY_MID_HZ: (f64, f64) = (120.0, 2000.0);
const TILT_LOW_CENTERS: [f64; 4] = [63.0, 125.0, 250.0, 500.0];
const TILT_HIGH_CENTERS: [f64; 3] = [4000.0, 8000.0, 16_000.0];
const HARMONIC_COUNT: usize = 8;
const HARMONIC_F0_RANGE: (f64, f64) = (20.0, 300.0);
const HARMONIC_N_FFT: usize = 8192;
const HARMONIC_HOP: usize = 2048;
const HARMONIC_MIN_FRAMES: usize = 5;
const HARMONIC_FLOOR_DB: f64 = -60.0;
const SILENCE_DBFS: f64 = -60.0;
const SILENCE_WINDOW_S: f64 = 0.02;
const SILENCE_HOP_S: f64 = 0.01;
const MIN_SILENCE_S: f64 = 0.1;
const MIN_SECTION_S: f64 = 0.05;
const TOP_SPECTRAL_PEAKS: usize = 8;
const PEAK_N_FFT: usize = 16_384;
const PEAK_HOP: usize = 8_192;
const DTW_N_FFT: usize = 2048;
const DTW_HOP: usize = 1024;
const DTW_MEL_BANDS: usize = 40;
const DTW_MEL_FMIN: f64 = 30.0;
const DTW_MEL_FMAX: f64 = 16_000.0;
const DTW_MFCC_COUNT: usize = 14;
const PITCH_FRAME: usize = 8192;
const PITCH_HOP_S: f64 = 0.05;
const PITCH_CLARITY_GATE: f64 = 0.5;
const PITCH_MIN_COMPARED_FRAMES: usize = 10;
const PITCH_MEDIAN_CLARITY_GATE: f64 = 0.6;
const PITCH_MAX_MEDIAN_CENTS: f64 = 1200.0;

fn db(amplitude: f64) -> f64 {
    (20.0 * amplitude.max(1e-12).log10()).max(NEG_INF_DB)
}

fn power_db(power: f64) -> f64 {
    (10.0 * power.max(1e-12).log10()).max(NEG_INF_DB)
}

// ============================================================================
// Decoded audio
// ============================================================================

/// Deinterleaved f64 samples at a single sample rate.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f64>>,
}

impl AudioBuffer {
    /// Frame count from the first channel.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Number of channels.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Duration in seconds.
    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate)
    }

    /// Sum channels into a mono buffer.
    pub fn mono(&self) -> Vec<f64> {
        let frames = self.frames();
        let mut mono = vec![0.0; frames];
        if self.channels.is_empty() {
            return mono;
        }
        for channel in &self.channels {
            for (out, value) in mono.iter_mut().zip(channel) {
                *out += value;
            }
        }
        let scale = 1.0 / self.channels.len() as f64;
        for value in &mut mono {
            *value *= scale;
        }
        mono
    }
}

/// Resolve a complete passage at a source's native rate. Round duration
/// separately so different starts at the same rate still yield equal lengths.
pub fn source_window_frames(
    sample_rate: u32,
    available: usize,
    start: f64,
    duration: f64,
) -> Result<std::ops::Range<usize>, String> {
    if sample_rate == 0
        || !start.is_finite()
        || start < 0.0
        || !duration.is_finite()
        || duration <= 0.0
    {
        return Err("Audio passage needs a finite nonnegative start and positive duration".into());
    }
    let rate = f64::from(sample_rate);
    let first = (start * rate).round() as usize;
    let count = (duration * rate).round() as usize;
    if count == 0 || count > 30 * 192_000 {
        return Err("Audio passage is too short or exceeds the sample budget".into());
    }
    let end = first.saturating_add(count);
    if first >= available || end > available {
        return Err(format!(
            "Audio passage exceeds the source duration of {:.6} s",
            available as f64 / rate
        ));
    }
    Ok(first..end)
}

/// Read a PCM/IEEE-float RIFF/WAVE file into normalized f64 samples.
pub fn read_wav(path: impl AsRef<Path>) -> Result<AudioBuffer, String> {
    read_wav_selection(path.as_ref(), None, 0).map(|(buffer, _)| buffer)
}

/// Seek directly to a passage, preserving the WAV's sample rate and channels.
/// Also returns the actual source start after rounding to a frame.
pub fn read_wav_window(
    path: impl AsRef<Path>,
    start: f64,
    duration: f64,
) -> Result<(AudioBuffer, f64), String> {
    read_wav_window_with_lead(path, start, duration, 0)
}

/// Read the same passage with up to `lead_frames` of preceding context.
pub fn read_wav_window_with_lead(
    path: impl AsRef<Path>,
    start: f64,
    duration: f64,
    lead_frames: usize,
) -> Result<(AudioBuffer, f64), String> {
    read_wav_selection(path.as_ref(), Some((start, duration)), lead_frames)
}

fn read_wav_selection(
    path: &Path,
    window: Option<(f64, f64)>,
    lead_frames: usize,
) -> Result<(AudioBuffer, f64), String> {
    let mut reader =
        crate::wav::open_reader(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(format!(
            "{}: invalid channel count or sample rate",
            path.display()
        ));
    }
    let channels = usize::from(spec.channels);
    let window = window
        .map(|(start, duration)| {
            source_window_frames(
                spec.sample_rate,
                reader.duration() as usize,
                start,
                duration,
            )
        })
        .transpose()?
        .map(|range| range.start.saturating_sub(lead_frames)..range.end);
    if let Some(range) = &window {
        if channels > 2 {
            return Err("only mono and stereo files are supported".into());
        }
        reader
            .seek(range.start as u32)
            .map_err(|error| error.to_string())?;
    }
    let limit = window
        .as_ref()
        .map_or(usize::MAX, |range| range.len() * channels);
    let mut deinterleaved = vec![Vec::new(); channels];
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (index, sample) in reader.samples::<f32>().take(limit).enumerate() {
                let value = sample.map_err(|error| error.to_string())?;
                if !value.is_finite() {
                    return Err(format!("{}: non-finite sample", path.display()));
                }
                deinterleaved[index % channels].push(f64::from(value));
            }
        }
        hound::SampleFormat::Int => {
            let magnitude = spec
                .bits_per_sample
                .checked_sub(1)
                .and_then(|shift| 1_u64.checked_shl(u32::from(shift)))
                .ok_or_else(|| format!("{}: invalid WAV sample width", path.display()))?;
            let scale = 1.0 / magnitude as f64;
            for (index, sample) in reader.samples::<i32>().take(limit).enumerate() {
                let value = sample.map_err(|error| error.to_string())?;
                deinterleaved[index % channels].push(f64::from(value) * scale);
            }
        }
    }
    let frames = deinterleaved.first().map_or(0, Vec::len);
    if window.as_ref().is_some_and(|range| {
        deinterleaved
            .iter()
            .any(|channel| channel.len() != range.len())
    }) {
        return Err("Audio source ended before the requested passage was read".into());
    }
    for channel in &mut deinterleaved {
        channel.truncate(frames);
    }
    Ok((
        AudioBuffer {
            sample_rate: spec.sample_rate,
            channels: deinterleaved,
        },
        window.map_or(0.0, |range| {
            range.start as f64 / f64::from(spec.sample_rate)
        }),
    ))
}

// ============================================================================
// Spectral primitives
// ============================================================================

fn hann_window(n_fft: usize) -> Vec<f64> {
    if n_fft <= 1 {
        return vec![1.0];
    }
    let scale = 2.0 * std::f64::consts::PI / (n_fft - 1) as f64;
    (0..n_fft)
        .map(|index| 0.5 - 0.5 * (scale * index as f64).cos())
        .collect()
}

struct FramePower {
    n_fft: usize,
    hop: usize,
    window: Vec<f64>,
    window_energy: f64,
    scratch: Vec<Complex<f64>>,
    power: Vec<f64>,
    fft: std::sync::Arc<dyn rustfft::Fft<f64>>,
}

impl FramePower {
    fn new(n_fft: usize, hop: usize) -> Self {
        let window = hann_window(n_fft);
        let window_energy = window.iter().map(|value| value * value).sum::<f64>();
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n_fft);
        Self {
            n_fft,
            hop,
            window,
            window_energy,
            scratch: vec![Complex::new(0.0, 0.0); n_fft],
            power: vec![0.0; n_fft / 2 + 1],
            fft,
        }
    }

    fn frames(&self, buffer: &AudioBuffer) -> usize {
        let frames = buffer.frames();
        if frames < self.n_fft {
            0
        } else {
            1 + (frames - self.n_fft) / self.hop
        }
    }

    /// Mean-square-scaled power spectrum of one frame, averaged over channels.
    fn power(&mut self, buffer: &AudioBuffer, frame: usize) -> &[f64] {
        let bins = self.n_fft / 2 + 1;
        self.power.iter_mut().for_each(|value| *value = 0.0);
        let channels = buffer.channel_count().max(1) as f64;
        for channel in &buffer.channels {
            let offset = frame * self.hop;
            for (index, slot) in self.scratch.iter_mut().enumerate() {
                *slot = Complex::new(channel[offset + index] * self.window[index], 0.0);
            }
            self.fft.process(&mut self.scratch);
            for (bin, slot) in self.power.iter_mut().enumerate() {
                let mut value = self.scratch[bin].norm_sqr();
                if bin != 0 && bin != self.n_fft / 2 {
                    value *= 2.0;
                }
                *slot += value;
            }
        }
        let scale = 1.0 / (self.window_energy * self.n_fft as f64 * channels);
        for value in &mut self.power {
            *value *= scale;
        }
        &self.power[..bins]
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10.0_f64.powf(mel / 2595.0) - 1.0)
}

fn mel_filterbank(
    sample_rate: u32,
    n_fft: usize,
    n_mels: usize,
    fmin: f64,
    fmax: f64,
) -> Vec<Vec<f64>> {
    let bins = n_fft / 2 + 1;
    let freqs: Vec<f64> = (0..bins)
        .map(|bin| bin as f64 * f64::from(sample_rate) / n_fft as f64)
        .collect();
    let low = hz_to_mel(fmin);
    let high = hz_to_mel(fmax.min(f64::from(sample_rate) / 2.0));
    let points: Vec<f64> = (0..=n_mels + 1)
        .map(|index| {
            let position = index as f64 / (n_mels + 1) as f64;
            mel_to_hz(low + (high - low) * position)
        })
        .collect();
    let mut filters = vec![vec![0.0; bins]; n_mels];
    for (band, filter) in filters.iter_mut().enumerate() {
        let (left, center, right) = (points[band], points[band + 1], points[band + 2]);
        let width = (right - left).max(1e-9);
        for (bin, value) in filter.iter_mut().enumerate() {
            let rising = (freqs[bin] - left) / (center - left).max(1e-9);
            let falling = (right - freqs[bin]) / (right - center).max(1e-9);
            *value = rising.min(falling).max(0.0) * 2.0 / width;
        }
    }
    filters
}

/// Log-mel spectrogram, one row of `n_mels` values per STFT frame.
fn log_mel_spectrogram(
    buffer: &AudioBuffer,
    n_fft: usize,
    hop: usize,
    n_mels: usize,
    fmin: f64,
    fmax: f64,
) -> Vec<Vec<f64>> {
    let mut analyzer = FramePower::new(n_fft, hop);
    let frames = analyzer.frames(buffer);
    let filters = mel_filterbank(buffer.sample_rate, n_fft, n_mels, fmin, fmax);
    // Mel triangles overlap by at most two filters per bin, so accumulate the
    // sparse weights instead of scanning all bins for every band.
    let mut bin_weights: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n_fft / 2 + 1];
    for (band, filter) in filters.iter().enumerate() {
        for (bin, &weight) in filter.iter().enumerate() {
            if weight > 0.0 {
                bin_weights[bin].push((band, weight));
            }
        }
    }
    let mut rows = Vec::with_capacity(frames);
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        let mut row = vec![0.0; n_mels];
        for (bin, weights) in bin_weights.iter().enumerate() {
            for &(band, weight) in weights {
                row[band] += weight * power[bin];
            }
        }
        for value in &mut row {
            // Keep the relative 80 dB floor authoritative; an absolute -120 dB
            // floor here would bind 33 dB earlier than compare.py's scaling.
            *value = 10.0 * value.max(1e-30).log10();
        }
        rows.push(row);
    }
    rows
}

/// Mean STFT power per octave band, normalized to mean square (a 0.5-amplitude
/// sine peaks at -9 dBFS); compare.py's absolute scale differs by a constant.
fn octave_energies(buffer: &AudioBuffer) -> Vec<f64> {
    octave_energies_with_fft(buffer, 16_384)
}

fn octave_energies_with_fft(buffer: &AudioBuffer, n_fft: usize) -> Vec<f64> {
    let mut analyzer = FramePower::new(n_fft, n_fft / 2);
    let frames = analyzer.frames(buffer);
    if frames == 0 {
        return vec![NEG_INF_DB; OCTAVE_CENTERS.len()];
    }
    let bins = n_fft / 2 + 1;
    let bin_hz = f64::from(buffer.sample_rate) / n_fft as f64;
    let mut totals = vec![0.0; OCTAVE_CENTERS.len()];
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        for (band, center) in OCTAVE_CENTERS.iter().enumerate() {
            let low = center / std::f64::consts::SQRT_2;
            let high = center * std::f64::consts::SQRT_2;
            let first = ((low / bin_hz).ceil() as usize).min(bins);
            let last = ((high / bin_hz).ceil() as usize).min(bins);
            totals[band] += power[first..last].iter().sum::<f64>();
        }
    }
    totals
        .iter()
        .map(|total| power_db(total / frames as f64))
        .collect()
}

/// Energy change from the stereo channel mean to an uncompensated mono fold.
#[derive(Debug, Serialize)]
pub struct MonoFoldOctave {
    pub center_hz: f64,
    pub stereo_dbfs: f64,
    pub mono_dbfs: f64,
    /// Undefined when the stereo band is already at the measurement floor.
    pub loss_db: Option<f64>,
    /// The actual loss can be larger when the mono level reaches the floor.
    pub mono_at_floor: bool,
}

#[derive(Debug, Serialize)]
/// Octave-band mono-fold loss per band and the gate floor.
pub struct MonoFoldReport {
    pub resolution_hz: f64,
    pub floor_dbfs: f64,
    pub bands: Vec<MonoFoldOctave>,
}

/// Mono-fold octave band report for a stereo buffer.
pub fn mono_fold_octaves(buffer: &AudioBuffer) -> Result<MonoFoldReport, String> {
    if buffer.channel_count() != 2 {
        return Err("Mono fold analysis needs two source channels".into());
    }
    let frames = buffer.frames().min(16_384);
    if frames < 2 || buffer.sample_rate == 0 || buffer.duration_s() < 0.5 {
        return Err("Mono fold analysis needs at least half a second of stereo audio".into());
    }
    let n_fft = 1 << frames.ilog2();
    let mono = AudioBuffer {
        sample_rate: buffer.sample_rate,
        channels: vec![buffer.mono()],
    };
    let resolution_hz = f64::from(buffer.sample_rate) / n_fft as f64;
    let stereo_db = octave_energies_with_fft(buffer, n_fft);
    let mono_db = octave_energies_with_fft(&mono, n_fft);
    let bands = OCTAVE_CENTERS
        .into_iter()
        .zip(stereo_db.into_iter().zip(mono_db))
        .filter(|(center, _)| {
            center / std::f64::consts::SQRT_2 >= resolution_hz
                && center * std::f64::consts::SQRT_2 <= f64::from(buffer.sample_rate) * 0.5
        })
        .map(|(center_hz, (stereo_dbfs, mono_dbfs))| MonoFoldOctave {
            center_hz,
            stereo_dbfs,
            mono_dbfs,
            loss_db: (stereo_dbfs > NEG_INF_DB).then_some((stereo_dbfs - mono_dbfs).max(0.0)),
            mono_at_floor: mono_dbfs <= NEG_INF_DB,
        })
        .collect();
    Ok(MonoFoldReport {
        resolution_hz,
        floor_dbfs: NEG_INF_DB,
        bands,
    })
}

/// Mean band power in compare.py's absolute scale (its `stft_power` is
/// `FramePower` times `n_fft` for the same window and channel average).
fn band_energies_scaled(
    buffer: &AudioBuffer,
    centers: &[f64],
    factor: f64,
    scale: f64,
) -> Vec<f64> {
    let n_fft = 16_384;
    let hop = 8_192;
    let mut analyzer = FramePower::new(n_fft, hop);
    let frames = analyzer.frames(buffer);
    if frames == 0 {
        return vec![NEG_INF_DB; centers.len()];
    }
    let bins = n_fft / 2 + 1;
    let bin_hz = f64::from(buffer.sample_rate) / n_fft as f64;
    let mut totals = vec![0.0; centers.len()];
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        for (band, center) in centers.iter().enumerate() {
            let low = center / factor;
            let high = center * factor;
            let first = ((low / bin_hz).ceil() as usize).min(bins);
            let last = ((high / bin_hz).ceil() as usize).min(bins);
            totals[band] += power[first..last].iter().sum::<f64>();
        }
    }
    totals
        .iter()
        .map(|total| power_db(total / frames as f64 * scale))
        .collect()
}

fn band_table(
    reference: &AudioBuffer,
    candidate: &AudioBuffer,
    centers: &[f64],
    factor: f64,
    scale: f64,
) -> BandTable {
    let reference_db = band_energies_scaled(reference, centers, factor, scale);
    let candidate_db = band_energies_scaled(candidate, centers, factor, scale);
    let delta_db = candidate_db
        .iter()
        .zip(&reference_db)
        .map(|(candidate, reference)| candidate - reference)
        .collect();
    BandTable {
        centers_hz: centers.to_vec(),
        reference_db,
        candidate_db,
        delta_db,
    }
}

fn third_octave_table(reference: &AudioBuffer, candidate: &AudioBuffer) -> BandTable {
    band_table(
        reference,
        candidate,
        &THIRD_OCTAVE_CENTERS,
        2.0_f64.powf(1.0 / 6.0),
        16_384.0,
    )
}

/// RMS third-octave delta over bands within 60 dB of the loudest band. The
/// level-matched value removes the median band delta so gain does not read as
/// spectral shape; the score uses that one.
fn third_octave_rms(third: &BandTable) -> (Option<f64>, Option<f64>) {
    let peak = third
        .reference_db
        .iter()
        .chain(&third.candidate_db)
        .fold(f64::NEG_INFINITY, |max, &value| max.max(value));
    let usable: Vec<f64> = third
        .delta_db
        .iter()
        .zip(&third.reference_db)
        .zip(&third.candidate_db)
        .filter_map(|((&delta, &reference), &candidate)| {
            (reference.max(candidate) >= peak - 60.0).then_some(delta)
        })
        .collect();
    if usable.is_empty() {
        return (None, None);
    }
    let raw = (usable.iter().map(|value| value * value).sum::<f64>() / usable.len() as f64).sqrt();
    let middle = median(&usable);
    let matched = (usable
        .iter()
        .map(|value| (value - middle) * (value - middle))
        .sum::<f64>()
        / usable.len() as f64)
        .sqrt();
    (Some(raw), Some(matched))
}

fn mel_log_rows(
    analyzer: &mut FramePower,
    buffer: &AudioBuffer,
    bin_weights: &[Vec<(usize, f64)>],
    bands: usize,
    scale: f64,
    floor: f64,
) -> Vec<Vec<f64>> {
    let frames = analyzer.frames(buffer);
    let mut rows = Vec::with_capacity(frames);
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        let mut row = vec![0.0; bands];
        for (bin, weights) in bin_weights.iter().enumerate() {
            for &(band, weight) in weights {
                row[band] += weight * power[bin];
            }
        }
        for value in &mut row {
            *value = 10.0 * (*value * scale).max(floor).log10();
        }
        rows.push(row);
    }
    rows
}

fn mfcc_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> MfccMetrics {
    let mut ref_analyzer = FramePower::new(SHAPE_N_FFT, SHAPE_HOP);
    let mut cand_analyzer = FramePower::new(SHAPE_N_FFT, SHAPE_HOP);
    let frames = ref_analyzer
        .frames(reference)
        .min(cand_analyzer.frames(candidate));
    if frames < 2 {
        return MfccMetrics {
            coefficients: MFCC_COEFFICIENTS,
            mean_cosine_distance: None,
            mean_vector_cosine_distance: None,
        };
    }
    let filters = mel_filterbank(
        reference.sample_rate,
        SHAPE_N_FFT,
        MFCC_BANDS,
        MEL_FMIN,
        MEL_FMAX,
    );
    let mut bin_weights: Vec<Vec<(usize, f64)>> = vec![Vec::new(); SHAPE_N_FFT / 2 + 1];
    for (band, filter) in filters.iter().enumerate() {
        for (bin, &weight) in filter.iter().enumerate() {
            if weight > 0.0 {
                bin_weights[bin].push((band, weight));
            }
        }
    }
    let mut ref_rows = Vec::with_capacity(frames);
    let mut cand_rows = Vec::with_capacity(frames);
    for frame in 0..frames {
        for (analyzer, buffer, rows) in [
            (&mut ref_analyzer, reference, &mut ref_rows),
            (&mut cand_analyzer, candidate, &mut cand_rows),
        ] {
            let power = analyzer.power(buffer, frame);
            let mut row = vec![0.0; MFCC_BANDS];
            for (bin, weights) in bin_weights.iter().enumerate() {
                for &(band, weight) in weights {
                    row[band] += weight * power[bin];
                }
            }
            for value in &mut row {
                *value = 10.0 * (*value * SHAPE_N_FFT as f64).max(LSD_FLOOR).log10();
            }
            rows.push(row);
        }
    }
    let peak = ref_rows
        .iter()
        .chain(&cand_rows)
        .flat_map(|row| row.iter())
        .fold(f64::NEG_INFINITY, |max, &value| max.max(value));
    let floor = peak - 80.0;
    for row in ref_rows.iter_mut().chain(cand_rows.iter_mut()) {
        for value in row {
            *value = value.max(floor);
        }
    }
    let dct: Vec<Vec<f64>> = (0..MFCC_DCT_OUT)
        .map(|index| {
            (0..MFCC_BANDS)
                .map(|sample| {
                    let mut value = (std::f64::consts::PI / MFCC_BANDS as f64
                        * (sample as f64 + 0.5)
                        * index as f64)
                        .cos()
                        * (2.0 / MFCC_BANDS as f64).sqrt();
                    if index == 0 {
                        value /= std::f64::consts::SQRT_2;
                    }
                    value
                })
                .collect()
        })
        .collect();
    let transform = |rows: &[Vec<f64>]| -> Vec<Vec<f64>> {
        rows.iter()
            .map(|row| {
                dct.iter()
                    .map(|weights| {
                        weights
                            .iter()
                            .zip(row)
                            .map(|(weight, value)| weight * value)
                            .sum::<f64>()
                    })
                    .collect()
            })
            .collect()
    };
    let ref_mfcc = transform(&ref_rows);
    let cand_mfcc = transform(&cand_rows);
    let mut ref_mean = [0.0; MFCC_COEFFICIENTS];
    let mut cand_mean = [0.0; MFCC_COEFFICIENTS];
    for row in &ref_mfcc {
        for (slot, value) in ref_mean.iter_mut().zip(&row[1..14]) {
            *slot += value;
        }
    }
    for row in &cand_mfcc {
        for (slot, value) in cand_mean.iter_mut().zip(&row[1..14]) {
            *slot += value;
        }
    }
    for value in ref_mean.iter_mut().chain(cand_mean.iter_mut()) {
        *value /= frames as f64;
    }
    let ref_norm = ref_mean
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    let cand_norm = cand_mean
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    let denominator = ref_norm * cand_norm;
    let vector_cosine = (denominator > 1e-12).then(|| {
        1.0 - ref_mean
            .iter()
            .zip(&cand_mean)
            .map(|(reference, candidate)| reference * candidate)
            .sum::<f64>()
            / denominator
    });
    let mut cosine_sum = 0.0;
    let mut usable = 0usize;
    for (ref_row, cand_row) in ref_mfcc.iter().zip(&cand_mfcc) {
        let ref_norm = ref_row[1..14]
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        let cand_norm = cand_row[1..14]
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if ref_norm > 1e-9 && cand_norm > 1e-9 {
            cosine_sum += 1.0
                - ref_row[1..14]
                    .iter()
                    .zip(&cand_row[1..14])
                    .map(|(reference, candidate)| reference * candidate)
                    .sum::<f64>()
                    / (ref_norm * cand_norm);
            usable += 1;
        }
    }
    MfccMetrics {
        coefficients: MFCC_COEFFICIENTS,
        mean_cosine_distance: (usable > 0).then(|| cosine_sum / usable as f64),
        mean_vector_cosine_distance: vector_cosine,
    }
}

fn lsd_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> LsdMetrics {
    let mut ref_analyzer = FramePower::new(SHAPE_N_FFT, SHAPE_HOP);
    let mut cand_analyzer = FramePower::new(SHAPE_N_FFT, SHAPE_HOP);
    let frames = ref_analyzer
        .frames(reference)
        .min(cand_analyzer.frames(candidate));
    if frames == 0 {
        return LsdMetrics {
            broadband_db: 0.0,
            level_matched_db: 0.0,
        };
    }
    let bins = SHAPE_N_FFT / 2 + 1;
    let mut broadband = 0.0;
    let mut level_matched = 0.0;
    for frame in 0..frames {
        let ref_power = ref_analyzer.power(reference, frame);
        let ref_db: Vec<f64> = ref_power
            .iter()
            .map(|power| 10.0 * (power * SHAPE_N_FFT as f64).max(LSD_FLOOR).log10())
            .collect();
        let cand_power = cand_analyzer.power(candidate, frame);
        let cand_db: Vec<f64> = cand_power
            .iter()
            .map(|power| 10.0 * (power * SHAPE_N_FFT as f64).max(LSD_FLOOR).log10())
            .collect();
        broadband += (ref_db
            .iter()
            .zip(&cand_db)
            .map(|(reference, candidate)| (reference - candidate).powi(2))
            .sum::<f64>()
            / bins as f64)
            .sqrt();
        let ref_mean = ref_db.iter().sum::<f64>() / bins as f64;
        let cand_mean = cand_db.iter().sum::<f64>() / bins as f64;
        level_matched += (ref_db
            .iter()
            .zip(&cand_db)
            .map(|(reference, candidate)| (reference - ref_mean - (candidate - cand_mean)).powi(2))
            .sum::<f64>()
            / bins as f64)
            .sqrt();
    }
    LsdMetrics {
        broadband_db: broadband / frames as f64,
        level_matched_db: level_matched / frames as f64,
    }
}

fn onset_flux_floor(buffer: &AudioBuffer, floor: f64) -> Vec<f64> {
    let mut analyzer = FramePower::new(1024, 240);
    let filters = mel_filterbank(buffer.sample_rate, 1024, 48, MEL_FMIN, MEL_FMAX);
    let mut bin_weights: Vec<Vec<(usize, f64)>> = vec![Vec::new(); 1024 / 2 + 1];
    for (band, filter) in filters.iter().enumerate() {
        for (bin, &weight) in filter.iter().enumerate() {
            if weight > 0.0 {
                bin_weights[bin].push((band, weight));
            }
        }
    }
    let rows = mel_log_rows(&mut analyzer, buffer, &bin_weights, 48, 1024.0, floor);
    let mut flux = Vec::with_capacity(rows.len().saturating_sub(1));
    for pair in rows.windows(2) {
        let mut sum = 0.0;
        for (current, previous) in pair[1].iter().zip(&pair[0]) {
            if current > previous {
                sum += current - previous;
            }
        }
        flux.push(sum);
    }
    flux
}

fn best_shift(a: &[f64], b: &[f64], max_frames: usize) -> (f64, i64) {
    let n = a.len().min(b.len());
    if n < 4 {
        return (0.0, 0);
    }
    // A short passage can be smaller than the requested alignment search.
    let max_frames = max_frames.min(n - 4);
    let a_mean = a[..n].iter().sum::<f64>() / n as f64;
    let b_mean = b[..n].iter().sum::<f64>() / n as f64;
    let a_centered: Vec<f64> = a[..n].iter().map(|value| value - a_mean).collect();
    let b_centered: Vec<f64> = b[..n].iter().map(|value| value - b_mean).collect();
    let mut prefix_a = vec![0.0; n + 1];
    let mut prefix_b = vec![0.0; n + 1];
    for index in 0..n {
        prefix_a[index + 1] = prefix_a[index] + a_centered[index] * a_centered[index];
        prefix_b[index + 1] = prefix_b[index] + b_centered[index] * b_centered[index];
    }
    let mut best_corr = 0.0;
    let mut best_lag = -(max_frames as i64);
    for lag in -(max_frames as i64)..=(max_frames as i64) {
        let (i0, i1, j0, j1) = if lag >= 0 {
            (0usize, n - lag as usize, lag as usize, n)
        } else {
            ((-lag) as usize, n, 0usize, (n as i64 + lag) as usize)
        };
        if i1 <= i0 || j1 <= j0 {
            continue;
        }
        let energy_a = prefix_a[i1] - prefix_a[i0];
        let energy_b = prefix_b[j1] - prefix_b[j0];
        if energy_a <= 1e-12 || energy_b <= 1e-12 {
            continue;
        }
        let dot = a_centered[i0..i1]
            .iter()
            .zip(&b_centered[j0..j1])
            .map(|(reference, candidate)| reference * candidate)
            .sum::<f64>();
        let corr = dot / (energy_a * energy_b).sqrt();
        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }
    (best_corr, best_lag)
}

fn detect_onsets(strength: &[f64], frame_rate: f64) -> Vec<usize> {
    if strength.len() < 3 {
        return Vec::new();
    }
    let n = strength.len();
    let smoothed: Vec<f64> = (0..n)
        .map(|index| {
            let left = if index == 0 { 0.0 } else { strength[index - 1] };
            let right = if index + 1 == n {
                0.0
            } else {
                strength[index + 1]
            };
            (left + strength[index] + right) / 3.0
        })
        .collect();
    let middle = median(&smoothed);
    let deviations: Vec<f64> = smoothed
        .iter()
        .map(|value| (value - middle).abs())
        .collect();
    let mad = median(&deviations);
    let peak = smoothed
        .iter()
        .fold(f64::NEG_INFINITY, |max, &value| max.max(value));
    let threshold = (middle + 4.0 * mad).max(0.05 * peak);
    let mut candidates: Vec<usize> = (1..n - 1)
        .filter(|&index| {
            smoothed[index] >= threshold
                && smoothed[index] >= smoothed[index - 1]
                && smoothed[index] >= smoothed[index + 1]
        })
        .collect();
    candidates.sort_by(|&left, &right| smoothed[right].total_cmp(&smoothed[left]));
    let min_gap = (0.08 * frame_rate).round().max(1.0) as usize;
    let mut accepted: Vec<usize> = Vec::new();
    for index in candidates {
        if accepted
            .iter()
            .all(|&other| index.abs_diff(other) >= min_gap)
        {
            accepted.push(index);
        }
    }
    accepted.sort_unstable();
    accepted
}

fn onset_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> OnsetMetrics {
    let flux_ref = onset_flux_floor(reference, 1e-12);
    let flux_cand = onset_flux_floor(candidate, 1e-12);
    let frame_rate: f64 = 200.0;
    let max_frames = (300.0 * frame_rate / 1000.0).round() as usize;
    let (best_corr, best_lag) = best_shift(&flux_ref, &flux_cand, max_frames);
    let onsets_ref = detect_onsets(&flux_ref, frame_rate);
    let onsets_cand = detect_onsets(&flux_cand, frame_rate);
    let duration = reference.duration_s().min(candidate.duration_s()).max(1e-9);
    let reference_density = onsets_ref.len() as f64 / duration;
    let candidate_density = onsets_cand.len() as f64 / duration;
    let density_ratio = if reference_density > 0.0 && candidate_density > 0.0 {
        Some(20.0 * (candidate_density / reference_density).log10())
    } else if reference_density <= 0.0 && candidate_density <= 0.0 {
        Some(0.0)
    } else {
        None
    };
    let mut strengths = Vec::new();
    for &index in &onsets_ref {
        let time = index as f64 / frame_rate;
        let nearest = onsets_cand
            .iter()
            .map(|&other| ((other as f64 / frame_rate - time).abs(), other))
            .fold(None, |best: Option<(f64, usize)>, current| match best {
                Some(best) if best.0 <= current.0 => Some(best),
                _ => Some(current),
            });
        let Some((distance, candidate_index)) = nearest else {
            continue;
        };
        if distance > 0.06 {
            continue;
        }
        let before = flux_ref[index.saturating_sub(3)..(index + 4).min(flux_ref.len())]
            .iter()
            .fold(0.0_f64, |max, &value| max.max(value));
        let after = flux_cand
            [candidate_index.saturating_sub(3)..(candidate_index + 4).min(flux_cand.len())]
            .iter()
            .fold(0.0_f64, |max, &value| max.max(value));
        if before > 1e-9 && after > 1e-9 {
            strengths.push(20.0 * (after / before).log10());
        }
    }
    OnsetMetrics {
        reference_count: onsets_ref.len(),
        candidate_count: onsets_cand.len(),
        best_lag_ms: best_lag as f64 * 5.0,
        best_correlation: best_corr,
        search_ms: 300.0,
        reference_density_per_s: reference_density,
        candidate_density_per_s: candidate_density,
        density_ratio_db: density_ratio,
        paired_count: strengths.len(),
        onset_strength_median_db: (!strengths.is_empty()).then(|| median(&strengths)),
    }
}

// ============================================================================
// Level and loudness
// ============================================================================

#[derive(Debug, Clone, Serialize)]
/// Peak, RMS, crest and loudness readings for one passage.
pub struct LevelMetrics {
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub crest_db: f64,
    pub integrated_lufs: Option<f64>,
    /// Loudest 3 s short-term window; `None` for clips shorter than 3 s.
    pub short_term_max_lufs: Option<f64>,
}

/// Measure interleaved stereo f32 samples with the same meter as file analysis.
/// Buffers shorter than 400 ms or silent across every gating block report
/// `integrated_lufs = None`; peaks and RMS stay valid.
pub fn measure_stereo_interleaved(samples: &[f32], sample_rate: u32) -> LevelMetrics {
    let frames = samples.len() / 2;
    let mut channels: Vec<Vec<f64>> = (0..2).map(|_| Vec::with_capacity(frames)).collect();
    for (index, &sample) in samples.iter().take(frames * 2).enumerate() {
        channels[index % 2].push(f64::from(sample));
    }
    level_metrics(&AudioBuffer {
        sample_rate,
        channels,
    })
}

use oc_dsp::limiter::{TRUE_PEAK_PHASES, TRUE_PEAK_TAPS, TruePeakDetector, true_peak_coefficients};
pub use oc_dsp::loudness::{
    CEILING_LUFS, FLOOR_LUFS, LOUDNESS_OFFSET_LU, SHORT_TERM_SECONDS, ShortTermLoudness,
};

fn true_peak_channel_peak(channel: &[f64], coeffs: &[f64]) -> f64 {
    let mut history = [0.0_f64; TRUE_PEAK_TAPS];
    let mut position = 0;
    let mut peak = 0.0_f64;
    for &sample in channel {
        history[position] = sample;
        peak = peak.max(sample.abs());
        for phase in 0..TRUE_PEAK_PHASES {
            let mut sum = 0.0_f64;
            for tap in 0..TRUE_PEAK_TAPS {
                let index = (position + TRUE_PEAK_TAPS - tap) % TRUE_PEAK_TAPS;
                sum += history[index] * coeffs[tap * TRUE_PEAK_PHASES + phase];
            }
            peak = peak.max(sum.abs());
        }
        position = (position + 1) % TRUE_PEAK_TAPS;
    }
    peak
}

/// 4x polyphase true-peak estimate in dBFS for interleaved stereo samples.
/// It runs the limiter's own detector, so both report the same number.
pub fn true_peak_dbfs(samples: &[f32]) -> f64 {
    let mut detector = TruePeakDetector::new();
    let mut peak = 0.0_f64;
    for frame in samples.chunks_exact(2) {
        peak = peak.max(detector.peak(frame[0], frame[1]));
    }
    db(peak)
}

/// True-peak estimate for a decoded buffer of any channel count.
pub fn true_peak_buffer(buffer: &AudioBuffer) -> f64 {
    let coeffs = true_peak_coefficients();
    let peak = buffer
        .channels
        .iter()
        .map(|channel| true_peak_channel_peak(channel, &coeffs))
        .fold(0.0_f64, f64::max);
    db(peak)
}

/// Measure levels over the supplied samples without an alignment or spectrum pass.
pub fn level_metrics(buffer: &AudioBuffer) -> LevelMetrics {
    let mut peak = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    let mut count = 0usize;
    for channel in &buffer.channels {
        for &sample in channel {
            peak = peak.max(sample.abs());
            sum_squares += sample * sample;
            count += 1;
        }
    }
    let rms = if count == 0 {
        0.0
    } else {
        (sum_squares / count as f64).sqrt()
    };
    LevelMetrics {
        peak_dbfs: db(peak),
        rms_dbfs: db(rms),
        crest_db: db(peak) - db(rms),
        integrated_lufs: integrated_lufs(buffer),
        short_term_max_lufs: short_term_max_lufs(buffer),
    }
}

fn biquad(input: &[f64], b: [f64; 3], a: [f64; 3]) -> Vec<f64> {
    let mut output = vec![0.0; input.len()];
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for (out, &x0) in output.iter_mut().zip(input) {
        let y0 = b[0] * x0 + b[1] * x1 + b[2] * x2 - a[1] * y1 - a[2] * y2;
        *out = y0;
        x2 = x1;
        x1 = x0;
        y2 = y1;
        y1 = y0;
    }
    output
}

/// Windowed-sinc resampling; amplitude preserving and adequate for loudness
/// and spectral comparison (not a mastering-grade converter).
///
/// Analysis-only, f64 and uncached: the media/render path uses the quality
/// ladder in `crate::resample`, which is f32 and would move these metrics.
fn resample(channel: &[f64], source_rate: u32, target_rate: u32) -> Vec<f64> {
    if source_rate == target_rate || channel.is_empty() {
        return channel.to_vec();
    }
    let ratio = f64::from(target_rate) / f64::from(source_rate);
    let out_len = ((channel.len() as f64) * ratio).round().max(1.0) as usize;
    let half = 32isize;
    let cutoff_hz = f64::from(source_rate.min(target_rate)) * 0.5;
    let cutoff = cutoff_hz / f64::from(source_rate);
    let mut output = vec![0.0; out_len];
    for (index, out) in output.iter_mut().enumerate() {
        let position = index as f64 / ratio;
        let center = position.floor() as isize;
        let mut sum = 0.0;
        let mut norm = 0.0;
        for tap in -half + 1..=half {
            let source = center + tap;
            if source < 0 || source as usize >= channel.len() {
                continue;
            }
            let offset = source as f64 - position;
            let sinc = if offset.abs() < 1e-12 {
                1.0
            } else {
                (2.0 * std::f64::consts::PI * cutoff * offset).sin()
                    / (std::f64::consts::PI * offset)
            };
            let window = 0.5 + 0.5 * (std::f64::consts::PI * offset / half as f64).cos();
            let coefficient = 2.0 * cutoff * sinc * window;
            sum += channel[source as usize] * coefficient;
            norm += coefficient;
        }
        *out = if norm.abs() > 1e-12 { sum / norm } else { sum };
    }
    output
}

fn k_weighted(buffer: &AudioBuffer) -> Vec<Vec<f64>> {
    const SHELF_B: [f64; 3] = [1.53512485958697, -2.69169618940638, 1.19839281085285];
    const SHELF_A: [f64; 3] = [1.0, -1.69065929318241, 0.73248077421585];
    const HIGHPASS_B: [f64; 3] = [1.0, -2.0, 1.0];
    const HIGHPASS_A: [f64; 3] = [1.0, -1.99004745483398, 0.99007225036621];
    buffer
        .channels
        .iter()
        .map(|channel| {
            let resampled = resample(channel, buffer.sample_rate, 48_000);
            let shelf = biquad(&resampled, SHELF_B, SHELF_A);
            biquad(&shelf, HIGHPASS_B, HIGHPASS_A)
        })
        .collect()
}

/// Integrated loudness of the given samples. Silence and passages shorter
/// than the meter window have no integrated reading.
pub fn integrated_lufs(buffer: &AudioBuffer) -> Option<f64> {
    let weighted = k_weighted(buffer);
    let frames = weighted.iter().map(Vec::len).min().unwrap_or(0);
    let window = (0.4_f64 * 48_000.0).round() as usize;
    let hop = (0.1_f64 * 48_000.0).round() as usize;
    if frames < window || window == 0 || hop == 0 {
        return None;
    }
    let mut channel_sum = vec![0.0; frames];
    for channel in &weighted {
        for (sum, &sample) in channel_sum.iter_mut().zip(channel) {
            *sum += sample * sample;
        }
    }
    let mut cumulative = vec![0.0; frames + 1];
    for index in 0..frames {
        cumulative[index + 1] = cumulative[index] + channel_sum[index];
    }
    let mut powers = Vec::new();
    let mut start = 0;
    while start + window <= frames {
        powers.push((cumulative[start + window] - cumulative[start]) / window as f64);
        start += hop;
    }
    if powers.is_empty() {
        return None;
    }
    let loudness: Vec<f64> = powers
        .iter()
        .map(|power| LOUDNESS_OFFSET_LU + 10.0 * power.max(1e-12).log10())
        .collect();
    let absolute: Vec<bool> = loudness.iter().map(|value| *value > -70.0).collect();
    let absolute_powers: Vec<f64> = powers
        .iter()
        .zip(&absolute)
        .filter_map(|(power, keep)| keep.then_some(*power))
        .collect();
    if absolute_powers.is_empty() {
        return None;
    }
    let relative = LOUDNESS_OFFSET_LU
        + 10.0 * (absolute_powers.iter().sum::<f64>() / absolute_powers.len() as f64).log10()
        - 10.0;
    let kept: Vec<f64> = powers
        .iter()
        .zip(&loudness)
        .filter_map(|(power, value)| (*value > relative).then_some(*power))
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(LOUDNESS_OFFSET_LU + 10.0 * (kept.iter().sum::<f64>() / kept.len() as f64).log10())
}

/// Loudest 3 s short-term window (BS.1770-4), no gating.
fn short_term_max_lufs(buffer: &AudioBuffer) -> Option<f64> {
    let weighted = k_weighted(buffer);
    let frames = weighted.iter().map(Vec::len).min().unwrap_or(0);
    let window = (3.0_f64 * 48_000.0).round() as usize;
    let hop = (0.1_f64 * 48_000.0).round() as usize;
    if frames < window || hop == 0 {
        return None;
    }
    let mut channel_sum = vec![0.0; frames];
    for channel in &weighted {
        for (sum, &sample) in channel_sum.iter_mut().zip(channel) {
            *sum += sample * sample;
        }
    }
    let mut cumulative = vec![0.0; frames + 1];
    for index in 0..frames {
        cumulative[index + 1] = cumulative[index] + channel_sum[index];
    }
    let mut loudest = f64::NEG_INFINITY;
    let mut start = 0;
    while start + window <= frames {
        let power = (cumulative[start + window] - cumulative[start]) / window as f64;
        loudest = loudest.max(power);
        start += hop;
    }
    (loudest > 1e-12).then(|| LOUDNESS_OFFSET_LU + 10.0 * loudest.log10())
}

// ============================================================================
// Single-file analysis
// ============================================================================

#[derive(Debug, Clone, Serialize)]
/// Octave-band centers and mean energies.
pub struct SpectrumMetrics {
    pub centers_hz: Vec<f64>,
    pub energy_db: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// One spectral peak with absolute and relative level.
pub struct SpectralPeak {
    pub frequency_hz: f64,
    pub level_db: f64,
    pub relative_db: f64,
}

#[derive(Debug, Clone, Serialize)]
/// Peaks found above one resolution, ranked by level.
pub struct SpectralPeaks {
    pub resolution_hz: f64,
    pub peaks: Vec<SpectralPeak>,
}

/// Mean-power local maxima of the octave STFT, ranked by level.
fn spectral_peaks(buffer: &AudioBuffer, count: usize) -> SpectralPeaks {
    let resolution_hz = f64::from(buffer.sample_rate) / PEAK_N_FFT as f64;
    let mut analyzer = FramePower::new(PEAK_N_FFT, PEAK_HOP);
    let frames = analyzer.frames(buffer);
    let bins = PEAK_N_FFT / 2 + 1;
    if frames == 0 || count == 0 || bins < 3 {
        return SpectralPeaks {
            resolution_hz,
            peaks: Vec::new(),
        };
    }
    let mut mean = vec![0.0; bins];
    for frame in 0..frames {
        for (slot, &value) in mean.iter_mut().zip(analyzer.power(buffer, frame)) {
            *slot += value;
        }
    }
    for value in &mut mean {
        *value /= frames as f64;
    }
    let levels: Vec<f64> = mean.iter().map(|&power| power_db(power)).collect();
    let loudest = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut candidates: Vec<usize> = (1..bins - 1)
        .filter(|&bin| levels[bin] > levels[bin - 1] && levels[bin] >= levels[bin + 1])
        .collect();
    candidates.sort_by(|&left, &right| levels[right].total_cmp(&levels[left]));
    candidates.truncate(count);
    let peaks = candidates
        .into_iter()
        .map(|bin| {
            // Parabolic interpolation on log power keeps the frequency estimate
            // below the raw bin width.
            let (left, center, right) = (levels[bin - 1], levels[bin], levels[bin + 1]);
            let denominator = left - 2.0 * center + right;
            let offset = if denominator.abs() > 1e-12 {
                (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            SpectralPeak {
                frequency_hz: (bin as f64 + offset) * resolution_hz,
                level_db: center,
                relative_db: center - loudest,
            }
        })
        .collect();
    SpectralPeaks {
        resolution_hz,
        peaks,
    }
}

#[derive(Debug, Clone, Serialize)]
/// Correlation and mid/side balance of a stereo file.
pub struct StereoMetrics {
    pub correlation: Option<f64>,
    pub mid_side_ratio_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// One silent span with its start, end and duration.
pub struct SilenceRegion {
    pub start_s: f64,
    pub end_s: f64,
    pub duration_s: f64,
}

#[derive(Debug, Clone, Serialize)]
/// One detected section with its level summary.
pub struct Section {
    pub start_s: f64,
    pub end_s: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
}

#[derive(Debug, Clone, Serialize)]
/// Single-file analysis report returned by the analyzer.
pub struct AnalysisReport {
    pub path: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_s: f64,
    pub level: LevelMetrics,
    pub true_peak_dbfs: f64,
    pub octave: SpectrumMetrics,
    pub spectral_peaks: SpectralPeaks,
    pub stereo: StereoMetrics,
    pub silence: Vec<SilenceRegion>,
    pub sections: Vec<Section>,
    pub warnings: Vec<String>,
}

fn stereo_metrics(buffer: &AudioBuffer) -> StereoMetrics {
    if buffer.channel_count() < 2 {
        return StereoMetrics {
            correlation: None,
            mid_side_ratio_db: None,
        };
    }
    let left = &buffer.channels[0];
    let right = &buffer.channels[1];
    let count = left.len().min(right.len());
    if count == 0 {
        return StereoMetrics {
            correlation: None,
            mid_side_ratio_db: None,
        };
    }
    let mean = |values: &[f64]| values.iter().take(count).sum::<f64>() / count as f64;
    let (left_mean, right_mean) = (mean(left), mean(right));
    let (mut covariance, mut left_var, mut right_var) = (0.0, 0.0, 0.0);
    let (mut mid_power, mut side_power) = (0.0, 0.0);
    for index in 0..count {
        let l = left[index] - left_mean;
        let r = right[index] - right_mean;
        covariance += l * r;
        left_var += l * l;
        right_var += r * r;
        let mid = (left[index] + right[index]) * 0.5;
        let side = (left[index] - right[index]) * 0.5;
        mid_power += mid * mid;
        side_power += side * side;
    }
    let correlation = if left_var < 1e-18 || right_var < 1e-18 {
        None
    } else {
        Some(covariance / (left_var.sqrt() * right_var.sqrt()))
    };
    let ratio = if mid_power <= 1e-12 && side_power <= 1e-12 {
        None
    } else {
        Some((10.0 * (mid_power.max(1e-12) / side_power.max(1e-12)).log10()).clamp(-60.0, 60.0))
    };
    StereoMetrics {
        correlation,
        mid_side_ratio_db: ratio,
    }
}

fn silence_and_sections(buffer: &AudioBuffer) -> (Vec<SilenceRegion>, Vec<Section>) {
    let frames = buffer.frames();
    let window = (SILENCE_WINDOW_S * f64::from(buffer.sample_rate)).round() as usize;
    let hop = (SILENCE_HOP_S * f64::from(buffer.sample_rate)).round() as usize;
    if frames < window || window == 0 || hop == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut mono = vec![0.0; frames];
    for channel in &buffer.channels {
        for (sum, &sample) in mono.iter_mut().zip(channel) {
            *sum += sample;
        }
    }
    let scale = 1.0 / buffer.channel_count().max(1) as f64;
    for value in &mut mono {
        *value *= scale;
    }
    let mut cumulative = vec![0.0; frames + 1];
    for index in 0..frames {
        cumulative[index + 1] = cumulative[index] + mono[index] * mono[index];
    }
    let threshold = 10.0_f64.powf(SILENCE_DBFS / 20.0);
    let mut flags = Vec::new();
    let mut start = 0;
    while start + window <= frames {
        let power = (cumulative[start + window] - cumulative[start]) / window as f64;
        flags.push(power.sqrt() >= threshold);
        start += hop;
    }
    if flags.is_empty() {
        return (Vec::new(), Vec::new());
    }
    // Merge runs shorter than one frame hop into their neighbours to avoid
    // boundary chatter from a single low-level analysis window.
    let frame_s = hop as f64 / f64::from(buffer.sample_rate);
    let min_silence_frames = ((MIN_SILENCE_S / frame_s).round() as usize).max(1);
    let min_section_frames = ((MIN_SECTION_S / frame_s).round() as usize).max(1);
    let mut runs: Vec<(bool, usize, usize)> = Vec::new();
    let mut run_start = 0;
    for index in 1..=flags.len() {
        if index == flags.len() || flags[index] != flags[run_start] {
            runs.push((flags[run_start], run_start, index));
            run_start = index;
        }
    }
    let mut silence = Vec::new();
    let mut sections = Vec::new();
    for (sounding, first, last) in runs {
        let start_s = first as f64 * frame_s;
        let end_s = (last as f64 * frame_s).min(buffer.duration_s());
        if sounding {
            if last - first < min_section_frames {
                continue;
            }
            let first_sample = (start_s * f64::from(buffer.sample_rate)) as usize;
            let last_sample = ((end_s * f64::from(buffer.sample_rate)) as usize).min(frames);
            let slice = &mono[first_sample..last_sample.max(first_sample)];
            if slice.is_empty() {
                continue;
            }
            let peak = slice
                .iter()
                .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
            let rms = (slice.iter().map(|sample| sample * sample).sum::<f64>()
                / slice.len() as f64)
                .sqrt();
            sections.push(Section {
                start_s,
                end_s,
                peak_dbfs: db(peak),
                rms_dbfs: db(rms),
            });
        } else if last - first >= min_silence_frames {
            silence.push(SilenceRegion {
                start_s,
                end_s,
                duration_s: end_s - start_s,
            });
        }
    }
    (silence, sections)
}

/// Full report with the default number of spectral peaks.
pub fn analyze_buffer(path: &str, buffer: &AudioBuffer) -> AnalysisReport {
    analyze_buffer_with_peaks(path, buffer, TOP_SPECTRAL_PEAKS)
}

/// Full report including `top_peaks` spectral maxima (0 disables the scan).
/// Level numbers come from the same functions the limiter/meter path uses.
pub fn analyze_buffer_with_peaks(
    path: &str,
    buffer: &AudioBuffer,
    top_peaks: usize,
) -> AnalysisReport {
    let mut warnings = Vec::new();
    if buffer.channel_count() == 1 {
        warnings.push("mono input: stereo correlation and mid/side ratio are null".to_string());
    }
    let (silence, sections) = silence_and_sections(buffer);
    AnalysisReport {
        path: path.to_string(),
        sample_rate: buffer.sample_rate,
        channels: buffer.channel_count(),
        duration_s: buffer.duration_s(),
        level: level_metrics(buffer),
        true_peak_dbfs: true_peak_buffer(buffer),
        octave: SpectrumMetrics {
            centers_hz: OCTAVE_CENTERS.to_vec(),
            energy_db: octave_energies(buffer),
        },
        spectral_peaks: spectral_peaks(buffer, top_peaks),
        stereo: stereo_metrics(buffer),
        silence,
        sections,
        warnings,
    }
}

/// Analyze a WAV file with the default peak count.
pub fn analyze_wav(path: impl AsRef<Path>) -> Result<AnalysisReport, String> {
    analyze_wav_with_peaks(path, TOP_SPECTRAL_PEAKS)
}

/// Analyze a WAV file with a chosen spectral peak count.
pub fn analyze_wav_with_peaks(
    path: impl AsRef<Path>,
    top_peaks: usize,
) -> Result<AnalysisReport, String> {
    let path = path.as_ref();
    let buffer = read_wav(path)?;
    Ok(analyze_buffer_with_peaks(
        &path.display().to_string(),
        &buffer,
        top_peaks,
    ))
}

// ============================================================================
// Two-file comparison
// ============================================================================

#[derive(Debug, Clone, Serialize)]
/// Format identity of one compared file.
pub struct FileInfo {
    pub path: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_s: f64,
}

/// Sample-domain difference on the aligned overlap. `correlation` is the
/// Pearson coefficient of the mono mix and is null when either side is flat.
#[derive(Debug, Clone, Serialize)]
pub struct SampleMetrics {
    pub max_abs_difference: f64,
    pub max_abs_difference_at_s: f64,
    pub max_abs_difference_channel: usize,
    pub correlation: Option<f64>,
}

/// Comparison contract. `allow_mismatch` opts into resampling a sample-rate
/// mismatch and trimming the longer file; without it unequal formats are refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    pub stems: bool,
    pub allow_mismatch: bool,
}

#[derive(Debug, Clone, Serialize)]
/// Candidate-minus-reference level readings.
pub struct LevelDelta {
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub crest_db: Option<f64>,
    pub integrated_lufs: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// Level metrics for both files and their delta.
pub struct CompareLevel {
    pub reference: LevelMetrics,
    pub candidate: LevelMetrics,
    pub delta: LevelDelta,
}

#[derive(Debug, Clone, Serialize)]
/// Best alignment lag and its confidence.
pub struct Alignment {
    pub lag_ms: f64,
    pub lag_samples: i64,
    pub spectral_corr: f64,
    pub zero_lag_corr: f64,
    pub search_ms: f64,
    pub confidence: String,
    pub method: String,
}

#[derive(Debug, Clone, Serialize)]
/// Band-limited spectral distance before and after level matching.
pub struct LogMelDistance {
    pub bands: usize,
    pub floor_db: f64,
    pub mean_abs_delta_db: f64,
    pub mean_abs_delta_db_level_matched: f64,
    pub p90_abs_delta_db: f64,
    pub frame_rate_hz: f64,
}

#[derive(Debug, Clone, Serialize)]
/// Correlation of the two amplitude envelopes.
pub struct EnvelopeCorrelation {
    pub correlation: Option<f64>,
    pub frames: usize,
}

#[derive(Debug, Clone, Serialize)]
/// Low or mid band level comparison, gated when silent.
pub struct BodyBand {
    pub range_hz: [f64; 2],
    pub reference_rms_dbfs: f64,
    pub candidate_rms_dbfs: f64,
    pub rms_delta_db: f64,
    pub reference_peak_dbfs: Option<f64>,
    pub candidate_peak_dbfs: Option<f64>,
    pub peak_delta_db: Option<f64>,
    pub gated: bool,
}

#[derive(Debug, Clone, Serialize)]
/// Low and mid body comparison.
pub struct BodyMetrics {
    pub low: BodyBand,
    pub mid: BodyBand,
}

#[derive(Debug, Clone, Serialize)]
/// Spectral tilt and per-octave deltas.
pub struct TiltMetrics {
    pub level_matched: bool,
    pub octave_delta_db: Vec<f64>,
    pub gated_octaves: Vec<bool>,
    pub low_centers_hz: Vec<f64>,
    pub high_centers_hz: Vec<f64>,
    pub low_mean_delta_db: Option<f64>,
    pub high_mean_delta_db: Option<f64>,
    pub tilt_db: Option<f64>,
    pub sub_gate_octaves_hz: Vec<f64>,
    pub sub_gate_low_mean_delta_db: Option<f64>,
    pub sub_gate_high_mean_delta_db: Option<f64>,
    pub sub_gate_tilt_db: Option<f64>,
    pub sub_gate_scored: bool,
}

/// Octave or third-octave mean band energy, both files and `candidate - reference`.
#[derive(Debug, Clone, Serialize)]
pub struct BandTable {
    pub centers_hz: Vec<f64>,
    pub reference_db: Vec<f64>,
    pub candidate_db: Vec<f64>,
    pub delta_db: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// MFCC frame distance measures.
pub struct MfccMetrics {
    pub coefficients: usize,
    pub mean_cosine_distance: Option<f64>,
    pub mean_vector_cosine_distance: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// Log-spectral distance, broadband and level-matched.
pub struct LsdMetrics {
    pub broadband_db: f64,
    pub level_matched_db: f64,
}

/// Onset placement and density. `flux_ratio_db` stays in `TransientMetrics`.
#[derive(Debug, Clone, Serialize)]
pub struct OnsetMetrics {
    pub reference_count: usize,
    pub candidate_count: usize,
    pub best_lag_ms: f64,
    pub best_correlation: f64,
    pub search_ms: f64,
    pub reference_density_per_s: f64,
    pub candidate_density_per_s: f64,
    pub density_ratio_db: Option<f64>,
    pub paired_count: usize,
    pub onset_strength_median_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// Analyzed frame counts for both files.
pub struct HarmonicFrames {
    pub reference: usize,
    pub candidate: usize,
}

#[derive(Debug, Clone, Serialize)]
/// Harmonic profile comparison and THD.
pub struct HarmonicProfile {
    pub status: String,
    pub harmonics: usize,
    pub frames: HarmonicFrames,
    pub fundamental_hz: [Option<f64>; 2],
    pub harmonic_delta_db: Option<Vec<f64>>,
    pub distance_db: Option<f64>,
    pub thd_percent: [Option<f64>; 2],
}

#[derive(Debug, Clone, Serialize)]
/// Transient flux summary for both files.
pub struct TransientMetrics {
    pub frames: usize,
    pub reference_flux_mean: Option<f64>,
    pub candidate_flux_mean: Option<f64>,
    pub flux_ratio_db: Option<f64>,
}

/// Banded Sakoe-Chiba DTW summary over standardized MFCC frames.
#[derive(Debug, Clone, Serialize)]
pub struct DtwMetrics {
    pub mean_abs_warp_ms: Option<f64>,
    pub max_abs_warp_ms: Option<f64>,
    pub median_signed_warp_ms: Option<f64>,
    pub path_cost: Option<f64>,
    pub band_ms: f64,
    pub hop_ms: f64,
    pub frames: usize,
    pub note: String,
}

/// YIN-style f0 comparison for a monophonic stem. Numeric results stay null
/// unless `status` is `ok`; the N/A reason says why.
#[derive(Debug, Clone, Serialize)]
pub struct PitchMetrics {
    pub f0_range_hz: [f64; 2],
    pub status: String,
    pub reason: Option<String>,
    pub confidence: String,
    pub clarity_gate: f64,
    pub reference_frames: usize,
    pub reference_voiced_frames: usize,
    pub candidate_voiced_frames: usize,
    pub reference_median_clarity: Option<f64>,
    pub candidate_median_clarity: Option<f64>,
    pub compared_frames: usize,
    pub reference_median_f0_hz: Option<f64>,
    pub candidate_median_f0_hz: Option<f64>,
    pub median_cents_error: Option<f64>,
    pub median_abs_cents_error: Option<f64>,
    pub p90_abs_cents_error: Option<f64>,
    pub fraction_within_25_cents: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// One weighted score term.
pub struct ScoreComponent {
    pub term: Option<f64>,
    pub weight: f64,
    pub contribution: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
/// Aggregate similarity score and its terms.
pub struct Score {
    pub value: f64,
    pub label: String,
    pub legacy_value: f64,
    pub previous_value: f64,
    pub spectral: ScoreComponent,
    pub level: ScoreComponent,
    pub envelope: ScoreComponent,
    pub body: ScoreComponent,
    pub tilt: ScoreComponent,
    pub crest_attack: ScoreComponent,
    pub harmonic: ScoreComponent,
    pub mfcc: ScoreComponent,
    pub third_octave: ScoreComponent,
    pub lsd: ScoreComponent,
    pub onset: ScoreComponent,
    pub density: ScoreComponent,
    pub spectral_scale_db: f64,
    pub level_scale_lu: f64,
    pub body_scale_db: f64,
    pub tilt_scale_db: f64,
    pub crest_scale_db: f64,
    pub flux_scale_db: f64,
    pub harmonic_scale_db: f64,
    pub mfcc_scale: f64,
    pub third_octave_scale_db: f64,
    pub lsd_scale_db: f64,
    pub onset_scale_db: f64,
    pub density_scale_db: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
/// Two-file comparison report returned by the comparator.
pub struct CompareReport {
    pub reference: FileInfo,
    pub candidate: FileInfo,
    pub aligned_duration_s: f64,
    pub alignment: Alignment,
    pub sample: SampleMetrics,
    pub level: CompareLevel,
    pub log_mel: LogMelDistance,
    pub envelope_correlation: EnvelopeCorrelation,
    pub body: BodyMetrics,
    pub tilt: TiltMetrics,
    pub third_octave: BandTable,
    pub mfcc: MfccMetrics,
    pub lsd: LsdMetrics,
    pub harmonics: HarmonicProfile,
    pub transient: TransientMetrics,
    pub onset: OnsetMetrics,
    pub dtw: DtwMetrics,
    pub pitch: PitchMetrics,
    pub score: Score,
    pub warnings: Vec<String>,
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let position = (fraction.clamp(0.0, 1.0)) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
}

fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}

fn slice_channel(channel: &[f64], lag: i64) -> Vec<f64> {
    let len = channel.len() as i64;
    let lag = lag.clamp(-len, len);
    if lag >= 0 {
        let end = (len - lag).max(0) as usize;
        channel[..end].to_vec()
    } else {
        let start = (-lag).max(0) as usize;
        channel[start.min(channel.len())..].to_vec()
    }
}

fn estimate_alignment(
    reference: &AudioBuffer,
    candidate: &AudioBuffer,
    stems: bool,
) -> (Alignment, i64) {
    let hop = 512usize;
    let n_fft = 2048usize;
    let ref_mel = log_mel_spectrogram(reference, n_fft, hop, 40, MEL_FMIN, MEL_FMAX);
    let cand_mel = log_mel_spectrogram(candidate, n_fft, hop, 40, MEL_FMIN, MEL_FMAX);
    let frames = ref_mel.len().min(cand_mel.len());
    let max_lag_ms = if stems { 300.0 } else { 1500.0 };
    let frame_ms = 1000.0 * hop as f64 / f64::from(reference.sample_rate);
    let max_frames = ((max_lag_ms / frame_ms).round() as i64).max(1);
    let mut best_lag = 0i64;
    let mut best_score = f64::NEG_INFINITY;
    let mut best_corr = 0.0;
    let mut zero_corr = 0.0;
    if frames >= 8 {
        for lag in -max_frames..=max_frames {
            let lag_usize = lag.unsigned_abs() as usize;
            if lag_usize >= frames {
                continue;
            }
            let mut correlation = 0.0;
            let mut bands = 0;
            for band in 0..40 {
                let a: Vec<f64> = if lag >= 0 {
                    (0..frames - lag_usize).map(|i| ref_mel[i][band]).collect()
                } else {
                    (lag_usize..frames).map(|i| ref_mel[i][band]).collect()
                };
                let b: Vec<f64> = if lag >= 0 {
                    (lag_usize..frames).map(|i| cand_mel[i][band]).collect()
                } else {
                    (0..frames - lag_usize).map(|i| cand_mel[i][band]).collect()
                };
                if a.len() < 8 {
                    continue;
                }
                let mean_a = a.iter().sum::<f64>() / a.len() as f64;
                let mean_b = b.iter().sum::<f64>() / b.len() as f64;
                let mut covariance = 0.0;
                let mut var_a = 0.0;
                let mut var_b = 0.0;
                for (x, y) in a.iter().zip(&b) {
                    let dx = x - mean_a;
                    let dy = y - mean_b;
                    covariance += dx * dy;
                    var_a += dx * dx;
                    var_b += dy * dy;
                }
                if var_a < 1e-12 || var_b < 1e-12 {
                    continue;
                }
                correlation += covariance / (var_a.sqrt() * var_b.sqrt());
                bands += 1;
            }
            if bands < 2 {
                continue;
            }
            let correlation = correlation / bands as f64;
            if lag == 0 {
                zero_corr = correlation;
            }
            let prior = 0.05 * (lag as f64 / max_frames as f64).powi(2);
            if correlation - prior > best_score {
                best_score = correlation - prior;
                best_corr = correlation;
                best_lag = lag;
            }
        }
    }
    let spectral_corr = if best_lag == 0 { zero_corr } else { best_corr };
    let advantage = spectral_corr - zero_corr;
    let (lag_frames, spectral_corr, _advantage) = if advantage < 0.01 {
        (0, zero_corr, 0.0)
    } else {
        (best_lag, spectral_corr, advantage)
    };
    let confidence = if spectral_corr >= 0.50 {
        "high"
    } else if spectral_corr >= 0.20 {
        "medium"
    } else {
        "low"
    };
    let lag_frames = if confidence == "low" { 0 } else { lag_frames };
    let lag_ms = lag_frames as f64 * frame_ms;
    let lag_samples = (lag_ms * f64::from(reference.sample_rate) / 1000.0).round() as i64;
    (
        Alignment {
            lag_ms,
            lag_samples,
            spectral_corr,
            zero_lag_corr: zero_corr,
            search_ms: max_lag_ms,
            confidence: confidence.to_string(),
            method: "log-mel 2-D correlation over the aligned overlap".to_string(),
        },
        lag_samples,
    )
}

fn sample_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> SampleMetrics {
    let frames = reference.frames().min(candidate.frames());
    let channels = reference.channel_count().min(candidate.channel_count());
    let (mut max_abs, mut max_index, mut max_channel) = (0.0_f64, 0usize, 0usize);
    for channel in 0..channels {
        let (left, right) = (&reference.channels[channel], &candidate.channels[channel]);
        for index in 0..frames {
            let difference = (left[index] - right[index]).abs();
            if difference > max_abs {
                max_abs = difference;
                max_index = index;
                max_channel = channel;
            }
        }
    }
    let correlation = if frames < 2 {
        None
    } else {
        let (a, b) = (reference.mono(), candidate.mono());
        let mean_a = a[..frames].iter().sum::<f64>() / frames as f64;
        let mean_b = b[..frames].iter().sum::<f64>() / frames as f64;
        let (mut covariance, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
        for index in 0..frames {
            let da = a[index] - mean_a;
            let db = b[index] - mean_b;
            covariance += da * db;
            var_a += da * da;
            var_b += db * db;
        }
        if var_a < 1e-18 || var_b < 1e-18 {
            None
        } else {
            Some((covariance / (var_a.sqrt() * var_b.sqrt())).clamp(-1.0, 1.0))
        }
    };
    SampleMetrics {
        max_abs_difference: max_abs,
        max_abs_difference_at_s: max_index as f64 / f64::from(reference.sample_rate),
        max_abs_difference_channel: max_channel,
        correlation,
    }
}

fn envelope_correlation(reference: &AudioBuffer, candidate: &AudioBuffer) -> EnvelopeCorrelation {
    fn envelope(buffer: &AudioBuffer) -> Vec<f64> {
        let hop = (f64::from(buffer.sample_rate) / 100.0).round().max(1.0) as usize;
        let window = (0.01 * f64::from(buffer.sample_rate)).round().max(1.0) as usize;
        let frames = buffer.frames();
        if frames < window {
            return Vec::new();
        }
        let mut power = vec![0.0; frames];
        for channel in &buffer.channels {
            for (sum, &sample) in power.iter_mut().zip(channel) {
                *sum += sample * sample;
            }
        }
        let scale = 1.0 / buffer.channel_count().max(1) as f64;
        for value in &mut power {
            *value *= scale;
        }
        let mut cumulative = vec![0.0; frames + 1];
        for index in 0..frames {
            cumulative[index + 1] = cumulative[index] + power[index];
        }
        let mut output = Vec::new();
        let mut start = 0;
        while start + window <= frames {
            let value = (cumulative[start + window] - cumulative[start]) / window as f64;
            output.push(value.max(0.0).sqrt());
            start += hop;
        }
        output
    }
    let ref_env = envelope(reference);
    let cand_env = envelope(candidate);
    let size = ref_env.len().min(cand_env.len());
    if size < 4 {
        return EnvelopeCorrelation {
            correlation: None,
            frames: size,
        };
    }
    let a = &ref_env[..size];
    let b = &cand_env[..size];
    let mean_a = a.iter().sum::<f64>() / size as f64;
    let mean_b = b.iter().sum::<f64>() / size as f64;
    let (mut covariance, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let dx = x - mean_a;
        let dy = y - mean_b;
        covariance += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }
    if var_a < 1e-18 || var_b < 1e-18 {
        if var_a < 1e-18 && var_b < 1e-18 {
            return EnvelopeCorrelation {
                correlation: Some(1.0),
                frames: size,
            };
        }
        return EnvelopeCorrelation {
            correlation: None,
            frames: size,
        };
    }
    EnvelopeCorrelation {
        correlation: Some((covariance / (var_a.sqrt() * var_b.sqrt())).clamp(-1.0, 1.0)),
        frames: size,
    }
}

fn log_mel_distance(reference: &AudioBuffer, candidate: &AudioBuffer) -> LogMelDistance {
    let n_fft = 2048;
    let hop = 512;
    let ref_mel = log_mel_spectrogram(reference, n_fft, hop, MEL_BANDS, MEL_FMIN, MEL_FMAX);
    let cand_mel = log_mel_spectrogram(candidate, n_fft, hop, MEL_BANDS, MEL_FMIN, MEL_FMAX);
    let frames = ref_mel.len().min(cand_mel.len());
    let frame_rate_hz = f64::from(reference.sample_rate) / hop as f64;
    if frames == 0 {
        return LogMelDistance {
            bands: MEL_BANDS,
            floor_db: NEG_INF_DB,
            mean_abs_delta_db: 0.0,
            mean_abs_delta_db_level_matched: 0.0,
            p90_abs_delta_db: 0.0,
            frame_rate_hz,
        };
    }
    let max_energy = ref_mel[..frames]
        .iter()
        .chain(cand_mel[..frames].iter())
        .flat_map(|row| row.iter())
        .fold(f64::NEG_INFINITY, |max, &value| max.max(value));
    let floor_db = max_energy - 80.0;
    let mut differences = Vec::with_capacity(frames * MEL_BANDS);
    for frame in 0..frames {
        for band in 0..MEL_BANDS {
            differences
                .push(ref_mel[frame][band].max(floor_db) - cand_mel[frame][band].max(floor_db));
        }
    }
    let mean_abs =
        differences.iter().map(|value| value.abs()).sum::<f64>() / differences.len() as f64;
    let median = percentile(&differences, 0.5);
    let level_matched = differences
        .iter()
        .map(|value| (value - median).abs())
        .sum::<f64>()
        / differences.len() as f64;
    let abs_values: Vec<f64> = differences.iter().map(|value| value.abs()).collect();
    LogMelDistance {
        bands: MEL_BANDS,
        floor_db,
        mean_abs_delta_db: mean_abs,
        mean_abs_delta_db_level_matched: level_matched,
        p90_abs_delta_db: percentile(&abs_values, 0.9),
        frame_rate_hz,
    }
}

// ============================================================================
// Body level, octave tilt, transients and harmonic profile
// ============================================================================

/// Peak of a brick-wall FFT band split over all channels.
fn band_peak(buffer: &AudioBuffer, low_hz: f64, high_hz: f64) -> f64 {
    let frames = buffer.frames();
    if frames < 2 {
        return 0.0;
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(frames);
    let inverse = planner.plan_fft_inverse(frames);
    let bins = frames / 2 + 1;
    let bin_hz = f64::from(buffer.sample_rate) / frames as f64;
    let mut spectrum = vec![Complex::new(0.0, 0.0); frames];
    let mut scratch = vec![Complex::new(0.0, 0.0); frames];
    let mut peak = 0.0_f64;
    for channel in &buffer.channels {
        for (slot, &sample) in spectrum.iter_mut().zip(channel) {
            *slot = Complex::new(sample, 0.0);
        }
        forward.process(&mut spectrum);
        for (bin, slot) in spectrum.iter_mut().enumerate() {
            let hz = bin as f64 * bin_hz;
            if bin >= bins || hz < low_hz || hz >= high_hz {
                *slot = Complex::new(0.0, 0.0);
            }
        }
        scratch.copy_from_slice(&spectrum);
        inverse.process(&mut scratch);
        let scale = 1.0 / frames as f64;
        for value in &scratch {
            peak = peak.max((value.re * scale).abs());
        }
    }
    peak
}

/// Band mean square from the same 16k STFT used by the octave table.
fn band_power_stft(buffer: &AudioBuffer, low_hz: f64, high_hz: f64) -> f64 {
    let mut analyzer = FramePower::new(16_384, 8_192);
    let frames = analyzer.frames(buffer);
    if frames == 0 {
        return 0.0;
    }
    let bins = 16_384 / 2 + 1;
    let bin_hz = f64::from(buffer.sample_rate) / 16_384.0;
    let first = ((low_hz / bin_hz).ceil() as usize).min(bins);
    let last = ((high_hz / bin_hz).ceil() as usize).min(bins);
    let mut total = 0.0;
    for frame in 0..frames {
        total += analyzer.power(buffer, frame)[first..last]
            .iter()
            .sum::<f64>();
    }
    total / frames as f64
}

fn mean_square(buffer: &AudioBuffer) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for channel in &buffer.channels {
        for &sample in channel {
            sum += sample * sample;
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn body_band(
    reference: &AudioBuffer,
    candidate: &AudioBuffer,
    range: (f64, f64),
    gate: f64,
) -> BodyBand {
    let reference_power = band_power_stft(reference, range.0, range.1);
    let candidate_power = band_power_stft(candidate, range.0, range.1);
    let reference_peak = band_peak(reference, range.0, range.1);
    let candidate_peak = band_peak(candidate, range.0, range.1);
    let gated = reference_power.max(candidate_power) < gate;
    BodyBand {
        range_hz: [range.0, range.1],
        reference_rms_dbfs: power_db(reference_power),
        candidate_rms_dbfs: power_db(candidate_power),
        rms_delta_db: if gated {
            0.0
        } else {
            power_db(candidate_power) - power_db(reference_power)
        },
        reference_peak_dbfs: Some(db(reference_peak)),
        candidate_peak_dbfs: Some(db(candidate_peak)),
        peak_delta_db: Some(if gated {
            0.0
        } else {
            db(candidate_peak) - db(reference_peak)
        }),
        gated,
    }
}

fn body_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> BodyMetrics {
    // Bands more than 60 dB below the louder file overall are numerical floor.
    let gate = mean_square(reference).max(mean_square(candidate)) * 1e-6;
    BodyMetrics {
        low: body_band(reference, candidate, BODY_LOW_HZ, gate),
        mid: body_band(reference, candidate, BODY_MID_HZ, gate),
    }
}

fn octave_delta(
    reference: &AudioBuffer,
    candidate: &AudioBuffer,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    // Same absolute scale as compare.py, so the -120 dB floor gates the same bands.
    let reference = band_energies_scaled(
        reference,
        &OCTAVE_CENTERS,
        std::f64::consts::SQRT_2,
        16_384.0,
    );
    let candidate = band_energies_scaled(
        candidate,
        &OCTAVE_CENTERS,
        std::f64::consts::SQRT_2,
        16_384.0,
    );
    let delta = reference
        .iter()
        .zip(&candidate)
        .map(|(reference, candidate)| candidate - reference)
        .collect();
    (reference, candidate, delta)
}

fn octave_center_mean(centers: &[f64], deltas: &[f64]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for center in centers {
        if let Some(index) = OCTAVE_CENTERS
            .iter()
            .position(|value| (value - center).abs() < 1e-9)
            && let Some(&delta) = deltas.get(index)
            && delta.is_finite()
        {
            sum += delta;
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f64)
}

fn tilt_metrics(reference: &[f64], candidate: &[f64], deltas: &[f64]) -> TiltMetrics {
    let peak = reference
        .iter()
        .chain(candidate)
        .fold(f64::NEG_INFINITY, |max, &value| max.max(value));
    let gated_flags: Vec<bool> = reference
        .iter()
        .zip(candidate)
        .map(|(&reference, &candidate)| reference.max(candidate) < peak - 60.0)
        .collect();
    // Octaves more than 60 dB below the loudest one carry no tilt evidence.
    let gated: Vec<f64> = deltas
        .iter()
        .zip(&gated_flags)
        .map(|(&delta, &gated)| if gated { 0.0 } else { delta })
        .collect();
    let low_mean = octave_center_mean(&TILT_LOW_CENTERS, &gated);
    let high_mean = octave_center_mean(&TILT_HIGH_CENTERS, &gated);
    let tilt = match (low_mean, high_mean) {
        (Some(low), Some(high)) => Some(low - high),
        _ => None,
    };
    let sub_gate = |centers: &[f64]| -> Vec<f64> {
        centers
            .iter()
            .filter_map(|center| {
                let index = OCTAVE_CENTERS
                    .iter()
                    .position(|value| (value - center).abs() < 1e-9)?;
                (gated_flags.get(index) == Some(&true)).then(|| deltas[index])
            })
            .collect()
    };
    let sub_low = sub_gate(&TILT_LOW_CENTERS);
    let sub_high = sub_gate(&TILT_HIGH_CENTERS);
    let sub_low_mean =
        (!sub_low.is_empty()).then(|| sub_low.iter().sum::<f64>() / sub_low.len() as f64);
    let sub_high_mean =
        (!sub_high.is_empty()).then(|| sub_high.iter().sum::<f64>() / sub_high.len() as f64);
    let sub_tilt = match (sub_low_mean, sub_high_mean) {
        (Some(low), Some(high)) => Some(low - high),
        _ => None,
    };
    TiltMetrics {
        level_matched: false,
        octave_delta_db: gated,
        gated_octaves: gated_flags.clone(),
        low_centers_hz: TILT_LOW_CENTERS.to_vec(),
        high_centers_hz: TILT_HIGH_CENTERS.to_vec(),
        low_mean_delta_db: low_mean,
        high_mean_delta_db: high_mean,
        tilt_db: tilt,
        sub_gate_octaves_hz: OCTAVE_CENTERS
            .iter()
            .zip(&gated_flags)
            .filter_map(|(&center, &gated)| gated.then_some(center))
            .collect(),
        sub_gate_low_mean_delta_db: sub_low_mean,
        sub_gate_high_mean_delta_db: sub_high_mean,
        sub_gate_tilt_db: sub_tilt,
        sub_gate_scored: false,
    }
}

fn onset_flux(buffer: &AudioBuffer) -> Vec<f64> {
    let rows = log_mel_spectrogram(buffer, 1024, 240, 48, MEL_FMIN, MEL_FMAX);
    let mut flux = Vec::with_capacity(rows.len().saturating_sub(1));
    for pair in rows.windows(2) {
        let mut sum = 0.0;
        for (current, previous) in pair[1].iter().zip(&pair[0]) {
            let difference = current - previous;
            if difference > 0.0 {
                sum += difference;
            }
        }
        flux.push(sum);
    }
    flux
}

fn transient_metrics(reference: &AudioBuffer, candidate: &AudioBuffer) -> TransientMetrics {
    let reference_flux = onset_flux(reference);
    let candidate_flux = onset_flux(candidate);
    let frames = reference_flux.len().min(candidate_flux.len());
    if frames < 4 {
        return TransientMetrics {
            frames,
            reference_flux_mean: None,
            candidate_flux_mean: None,
            flux_ratio_db: None,
        };
    }
    let reference_mean = reference_flux[..frames].iter().sum::<f64>() / frames as f64;
    let candidate_mean = candidate_flux[..frames].iter().sum::<f64>() / frames as f64;
    let ratio = if reference_mean > 1e-12 && candidate_mean > 1e-12 {
        Some(20.0 * (candidate_mean / reference_mean).log10())
    } else {
        None
    };
    TransientMetrics {
        frames,
        reference_flux_mean: Some(reference_mean),
        candidate_flux_mean: Some(candidate_mean),
        flux_ratio_db: ratio,
    }
}

/// MFCC c0..c13 frames standardized per file; compare.py's DTW input.
fn dtw_features(buffer: &AudioBuffer) -> Vec<Vec<f64>> {
    let mut analyzer = FramePower::new(DTW_N_FFT, DTW_HOP);
    let frames = analyzer.frames(buffer);
    if frames < 2 {
        return Vec::new();
    }
    let filters = mel_filterbank(
        buffer.sample_rate,
        DTW_N_FFT,
        DTW_MEL_BANDS,
        DTW_MEL_FMIN,
        DTW_MEL_FMAX,
    );
    let mut bin_weights: Vec<Vec<(usize, f64)>> = vec![Vec::new(); DTW_N_FFT / 2 + 1];
    for (band, filter) in filters.iter().enumerate() {
        for (bin, &weight) in filter.iter().enumerate() {
            if weight > 0.0 {
                bin_weights[bin].push((band, weight));
            }
        }
    }
    let dct: Vec<Vec<f64>> = (0..DTW_MFCC_COUNT)
        .map(|index| {
            (0..DTW_MEL_BANDS)
                .map(|sample| {
                    let mut value = (std::f64::consts::PI / DTW_MEL_BANDS as f64
                        * (sample as f64 + 0.5)
                        * index as f64)
                        .cos()
                        * (2.0 / DTW_MEL_BANDS as f64).sqrt();
                    if index == 0 {
                        value /= std::f64::consts::SQRT_2;
                    }
                    value
                })
                .collect()
        })
        .collect();
    let mut features = Vec::with_capacity(frames);
    let mut mel = vec![0.0; DTW_MEL_BANDS];
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        mel.iter_mut().for_each(|value| *value = 0.0);
        for (bin, weights) in bin_weights.iter().enumerate() {
            for &(band, weight) in weights {
                mel[band] += weight * power[bin];
            }
        }
        // FramePower is 1/n_fft below compare.py's scale; match its 1e-12 floor.
        for value in &mut mel {
            *value = 10.0 * (*value * DTW_N_FFT as f64).max(1e-12).log10();
        }
        let mut row = vec![0.0; DTW_MFCC_COUNT];
        for (coefficient, weights) in dct.iter().enumerate() {
            row[coefficient] = weights
                .iter()
                .zip(&mel)
                .map(|(weight, value)| weight * value)
                .sum();
        }
        features.push(row);
    }
    let count = features.len() as f64;
    let mut mean = [0.0; DTW_MFCC_COUNT];
    for row in &features {
        for (sum, value) in mean.iter_mut().zip(row) {
            *sum += value;
        }
    }
    mean.iter_mut().for_each(|value| *value /= count);
    let mut std = [0.0; DTW_MFCC_COUNT];
    for row in &features {
        for (dimension, value) in row.iter().enumerate() {
            std[dimension] += (value - mean[dimension]) * (value - mean[dimension]);
        }
    }
    for variance in &mut std {
        *variance = (*variance / count).sqrt();
        if *variance < 1e-9 {
            *variance = 1.0;
        }
    }
    for row in &mut features {
        for (dimension, value) in row.iter_mut().enumerate() {
            *value = (*value - mean[dimension]) / std[dimension];
        }
    }
    features
}

/// Sakoe-Chiba banded DTW with squared Euclidean frame cost.
fn banded_dtw(a: &[Vec<f64>], b: &[Vec<f64>], band: usize) -> Option<(Vec<(usize, usize)>, f64)> {
    let (n, m) = (a.len(), b.len());
    if n < 2 || m < 2 {
        return None;
    }
    let band = band.max(n.abs_diff(m) + 2).max(1);
    let width = 2 * band + 1;
    const INFINITY: f64 = 1e18;
    let mut previous = vec![INFINITY; width];
    let mut back = vec![0u8; n * width];
    for i in 0..n {
        let mut current = vec![INFINITY; width];
        let base = i as isize - band as isize;
        for k in 0..width {
            let j = base + k as isize;
            if j < 0 || j >= m as isize {
                continue;
            }
            let j = j as usize;
            let cost: f64 = a[i]
                .iter()
                .zip(&b[j])
                .map(|(left, right)| (left - right) * (left - right))
                .sum();
            if i == 0 && j == 0 {
                current[k] = cost;
                continue;
            }
            let mut best = INFINITY;
            let mut pointer = 0u8;
            if i > 0 && previous[k] < best {
                best = previous[k];
            }
            if i > 0 && k + 1 < width && previous[k + 1] < best {
                best = previous[k + 1];
                pointer = 1;
            }
            if k > 0 && current[k - 1] < best {
                best = current[k - 1];
                pointer = 2;
            }
            if best < INFINITY {
                current[k] = best + cost;
                back[i * width + k] = pointer;
            }
        }
        previous = current;
    }
    let end_k = (m as isize - 1) - ((n as isize - 1) - band as isize);
    if end_k < 0 || end_k >= width as isize {
        return None;
    }
    let end_k = end_k as usize;
    if previous[end_k] >= INFINITY {
        return None;
    }
    let mut path = Vec::new();
    let mut i = n as isize - 1;
    let mut k = end_k as isize;
    while i >= 0 && k >= 0 {
        let j = i - band as isize + k;
        path.push((i as usize, j as usize));
        if i == 0 && j == 0 {
            break;
        }
        match back[i as usize * width + k as usize] {
            0 => i -= 1,
            1 => {
                i -= 1;
                k += 1;
            }
            _ => k -= 1,
        }
    }
    path.reverse();
    Some((path, previous[end_k]))
}

fn dtw_metrics(reference: &AudioBuffer, candidate: &AudioBuffer, stems: bool) -> DtwMetrics {
    let features_reference = dtw_features(reference);
    let features_candidate = dtw_features(candidate);
    let hop_s = DTW_HOP as f64 / f64::from(reference.sample_rate);
    let duration = reference.duration_s().min(candidate.duration_s());
    let band_s = if stems {
        (0.03 * duration).clamp(0.10, 0.40)
    } else {
        (0.05 * duration).clamp(0.25, 1.50)
    };
    let band_frames = ((band_s / hop_s).round() as usize).max(2);
    let note = "Mean warp includes low-energy and silent regions, where the path can drift cheaply. On sparse arrangements it is an upper bound and a path-shape diagnostic, not a precise timing error.";
    let result = banded_dtw(&features_reference, &features_candidate, band_frames);
    let Some((path, cost)) = result else {
        return DtwMetrics {
            mean_abs_warp_ms: None,
            max_abs_warp_ms: None,
            median_signed_warp_ms: None,
            path_cost: None,
            band_ms: band_s * 1000.0,
            hop_ms: hop_s * 1000.0,
            frames: 0,
            note: note.to_string(),
        };
    };
    let warps: Vec<f64> = path
        .iter()
        .map(|&(i, j)| (j as f64 - i as f64) * hop_s * 1000.0)
        .collect();
    let absolute: Vec<f64> = warps.iter().map(|warp| warp.abs()).collect();
    DtwMetrics {
        mean_abs_warp_ms: Some(absolute.iter().sum::<f64>() / absolute.len() as f64),
        max_abs_warp_ms: Some(absolute.iter().copied().fold(0.0, f64::max)),
        median_signed_warp_ms: Some(median(&warps)),
        path_cost: Some(cost),
        band_ms: band_s * 1000.0,
        hop_ms: hop_s * 1000.0,
        frames: path.len(),
        note: note.to_string(),
    }
}

/// YIN-style f0 track: FFT autocorrelation, cumulative-mean normalization.
fn f0_track(buffer: &AudioBuffer, fmin: f64, fmax: f64) -> (Vec<f64>, Vec<bool>, Vec<f64>) {
    let mono = buffer.mono();
    let rate = f64::from(buffer.sample_rate);
    let frame = PITCH_FRAME;
    let hop = ((PITCH_HOP_S * rate).round() as usize).max(1);
    if mono.len() < frame {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let count = 1 + (mono.len() - frame) / hop;
    let mut estimates = vec![f64::NAN; count];
    let mut voiced = vec![false; count];
    let mut clarity = vec![0.0; count];
    let lag_min = ((rate / fmax * 0.9).floor() as usize).max(2);
    let tau_max = ((rate / fmin * 1.1).ceil() as usize).min(frame - 2);
    if tau_max <= lag_min + 2 {
        return (estimates, voiced, clarity);
    }
    let nfft = (frame + tau_max).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(nfft);
    let inverse = planner.plan_fft_inverse(nfft);
    let mut spectrum = vec![Complex::new(0.0, 0.0); nfft];
    let mut centered = vec![0.0; frame];
    let mut cumulative = vec![0.0; frame + 1];
    let mut difference = vec![0.0; tau_max + 1];
    let mut normalized = vec![1.0; tau_max + 1];
    for index in 0..count {
        let segment = &mono[index * hop..index * hop + frame];
        let rms = (segment.iter().map(|value| value * value).sum::<f64>() / frame as f64).sqrt();
        if rms < 1e-4 {
            continue;
        }
        let mean = segment.iter().sum::<f64>() / frame as f64;
        cumulative[0] = 0.0;
        for sample in 0..frame {
            centered[sample] = segment[sample] - mean;
            cumulative[sample + 1] = cumulative[sample] + centered[sample] * centered[sample];
        }
        spectrum
            .iter_mut()
            .for_each(|slot| *slot = Complex::new(0.0, 0.0));
        for (slot, &value) in spectrum.iter_mut().zip(&centered) {
            *slot = Complex::new(value, 0.0);
        }
        forward.process(&mut spectrum);
        for slot in &mut spectrum {
            *slot = Complex::new(slot.norm_sqr(), 0.0);
        }
        inverse.process(&mut spectrum);
        let scale = 1.0 / nfft as f64;
        for tau in 0..=tau_max {
            let head = cumulative[frame - tau];
            let tail = cumulative[frame] - cumulative[tau];
            let autocorrelation = spectrum[tau].re * scale;
            difference[tau] = (head + tail - 2.0 * autocorrelation).max(0.0);
        }
        let mut running = 0.0;
        normalized[0] = 1.0;
        for tau in 1..=tau_max {
            running += difference[tau];
            normalized[tau] = difference[tau] * tau as f64 / running.max(1e-12);
        }
        let mut best = None;
        for tau in (lag_min + 1)..tau_max {
            if normalized[tau] <= normalized[tau - 1]
                && normalized[tau] <= normalized[tau + 1]
                && normalized[tau] < 0.25
            {
                best = Some(tau);
                break;
            }
        }
        let best = match best {
            Some(best) => best,
            None => {
                let mut candidate = lag_min;
                let mut minimum = f64::INFINITY;
                for (tau, &value) in normalized
                    .iter()
                    .enumerate()
                    .take(tau_max + 1)
                    .skip(lag_min)
                {
                    if value < minimum {
                        minimum = value;
                        candidate = tau;
                    }
                }
                if normalized[candidate] > 0.35 {
                    continue;
                }
                candidate
            }
        };
        if best <= lag_min || best >= tau_max {
            continue;
        }
        let (left, center, right) = (normalized[best - 1], normalized[best], normalized[best + 1]);
        let denominator = left - 2.0 * center + right;
        let offset = if denominator > 1e-12 {
            (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let frequency = rate / (best as f64 + offset);
        if fmin <= frequency && frequency <= fmax {
            estimates[index] = frequency;
            voiced[index] = true;
            clarity[index] = 1.0 - normalized[best];
        }
    }
    (estimates, voiced, clarity)
}

fn pitch_metrics(
    reference: &AudioBuffer,
    candidate: &AudioBuffer,
    stems: bool,
    audible: bool,
) -> PitchMetrics {
    let (fmin, fmax) = if stems {
        (30.0, 1000.0)
    } else {
        (50.0, 1000.0)
    };
    let (f0_reference, voiced_reference, clarity_reference) = f0_track(reference, fmin, fmax);
    let (f0_candidate, voiced_candidate, clarity_candidate) = f0_track(candidate, fmin, fmax);
    let count = f0_reference.len().min(f0_candidate.len());
    let mut errors = Vec::new();
    for index in 0..count {
        if voiced_reference[index]
            && voiced_candidate[index]
            && clarity_reference[index] >= PITCH_CLARITY_GATE
            && clarity_candidate[index] >= PITCH_CLARITY_GATE
        {
            errors.push(1200.0 * (f0_candidate[index] / f0_reference[index]).log2());
        }
    }
    let within = errors.iter().filter(|error| error.abs() <= 25.0).count();
    let reference_voiced: Vec<f64> = (0..count)
        .filter(|&index| voiced_reference[index])
        .map(|index| f0_reference[index])
        .collect();
    let candidate_voiced: Vec<f64> = (0..count)
        .filter(|&index| voiced_candidate[index])
        .map(|index| f0_candidate[index])
        .collect();
    let reference_clarity: Vec<f64> = (0..count)
        .filter(|&index| voiced_reference[index])
        .map(|index| clarity_reference[index])
        .collect();
    let candidate_clarity: Vec<f64> = (0..count)
        .filter(|&index| voiced_candidate[index])
        .map(|index| clarity_candidate[index])
        .collect();
    let median_reference_clarity = if reference_clarity.is_empty() {
        0.0
    } else {
        median(&reference_clarity)
    };
    let median_candidate_clarity = if candidate_clarity.is_empty() {
        0.0
    } else {
        median(&candidate_clarity)
    };
    let absolute_errors: Vec<f64> = errors.iter().map(|error| error.abs()).collect();
    let median_abs_errors = if absolute_errors.is_empty() {
        0.0
    } else {
        median(&absolute_errors)
    };

    let (status, reason) = if !stems {
        (
            "not_applicable",
            Some("full mix is polyphonic; compare a monophonic stem with stems=true"),
        )
    } else if !audible {
        ("not_applicable", Some("at least one render is near-silent"))
    } else if errors.len() < PITCH_MIN_COMPARED_FRAMES {
        (
            "not_applicable",
            Some("fewer than 10 frames with confident f0"),
        )
    } else if median_reference_clarity < PITCH_MEDIAN_CLARITY_GATE
        || median_candidate_clarity < PITCH_MEDIAN_CLARITY_GATE
    {
        (
            "not_applicable",
            Some("weak periodicity (polyphonic or sub-heavy source)"),
        )
    } else if median_abs_errors > PITCH_MAX_MEDIAN_CENTS {
        (
            "not_applicable",
            Some("octave/harmonic ambiguity (median |cents| > 1200)"),
        )
    } else {
        ("ok", None)
    };
    let visible = status == "ok";
    let median_or_none = |values: &[f64]| -> Option<f64> {
        if visible && !values.is_empty() {
            Some(median(values))
        } else {
            None
        }
    };
    PitchMetrics {
        f0_range_hz: [fmin, fmax],
        status: status.to_string(),
        reason: reason.map(str::to_string),
        confidence: if visible { "medium" } else { "low" }.to_string(),
        clarity_gate: PITCH_CLARITY_GATE,
        reference_frames: count,
        reference_voiced_frames: reference_voiced.len(),
        candidate_voiced_frames: candidate_voiced.len(),
        reference_median_clarity: (!reference_clarity.is_empty())
            .then(|| median(&reference_clarity)),
        candidate_median_clarity: (!candidate_clarity.is_empty())
            .then(|| median(&candidate_clarity)),
        compared_frames: errors.len(),
        reference_median_f0_hz: median_or_none(&reference_voiced),
        candidate_median_f0_hz: median_or_none(&candidate_voiced),
        median_cents_error: median_or_none(&errors),
        median_abs_cents_error: median_or_none(&absolute_errors),
        p90_abs_cents_error: if visible && !absolute_errors.is_empty() {
            Some(percentile(&absolute_errors, 0.9))
        } else {
            None
        },
        fraction_within_25_cents: if visible && !errors.is_empty() {
            Some(within as f64 / errors.len() as f64)
        } else {
            None
        },
    }
}

/// Relative first-8 harmonic levels (dB re H1) of the detected bass f0.
fn harmonic_rows(buffer: &AudioBuffer) -> (Vec<[f64; HARMONIC_COUNT]>, Vec<f64>) {
    let mut analyzer = FramePower::new(HARMONIC_N_FFT, HARMONIC_HOP);
    let frames = analyzer.frames(buffer);
    let bins = HARMONIC_N_FFT / 2 + 1;
    let bin_hz = f64::from(buffer.sample_rate) / HARMONIC_N_FFT as f64;
    let first = ((HARMONIC_F0_RANGE.0 / bin_hz).ceil() as usize).min(bins - 1);
    let last = ((HARMONIC_F0_RANGE.1 / bin_hz).floor() as usize).min(bins - 1);
    let mut rows = Vec::new();
    let mut fundamentals = Vec::new();
    if last <= first {
        return (rows, fundamentals);
    }
    for frame in 0..frames {
        let power = analyzer.power(buffer, frame);
        let frame_peak = power.iter().fold(0.0_f64, |max, &value| max.max(value));
        if frame_peak <= 1e-12 {
            continue;
        }
        let mut peak = first;
        for (bin, &value) in power.iter().enumerate().take(last + 1).skip(first) {
            if value > power[peak] {
                peak = bin;
            }
        }
        if power[peak] < 1e-4 * frame_peak || peak == 0 || peak + 1 >= bins {
            continue;
        }
        let left = power[peak - 1].max(1e-12).ln();
        let center = power[peak].max(1e-12).ln();
        let right = power[peak + 1].max(1e-12).ln();
        let denominator = left - 2.0 * center + right;
        let offset = if denominator.abs() > 1e-12 {
            (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let fundamental = (peak as f64 + offset) * bin_hz;
        if !(HARMONIC_F0_RANGE.0..=HARMONIC_F0_RANGE.1).contains(&fundamental) {
            continue;
        }
        let mut levels = [0.0_f64; HARMONIC_COUNT];
        let mut valid = true;
        for harmonic in 1..=HARMONIC_COUNT {
            let center_hz = harmonic as f64 * fundamental;
            if center_hz >= f64::from(buffer.sample_rate) / 2.0 {
                valid = false;
                break;
            }
            let half = (0.02 * center_hz).max(2.0);
            let low = (((center_hz - half) / bin_hz).floor() as isize).max(0) as usize;
            let high = ((((center_hz + half) / bin_hz).ceil() as usize) + 1).min(bins);
            levels[harmonic - 1] = power[low..high].iter().sum();
        }
        if !valid || levels[0] <= 1e-12 {
            continue;
        }
        let mut row = [0.0_f64; HARMONIC_COUNT];
        for harmonic in 0..HARMONIC_COUNT {
            row[harmonic] =
                (10.0 * (levels[harmonic].max(1e-12) / levels[0]).log10()).max(HARMONIC_FLOOR_DB);
        }
        rows.push(row);
        fundamentals.push(fundamental);
    }
    (rows, fundamentals)
}

fn harmonic_thd(levels_db: &[f64]) -> f64 {
    // `levels_db` holds H2..HN; the fundamental is not part of THD.
    levels_db
        .iter()
        .map(|value| 10.0_f64.powf(value / 10.0))
        .sum::<f64>()
        .sqrt()
        * 100.0
}

fn harmonic_profile(reference: &AudioBuffer, candidate: &AudioBuffer) -> HarmonicProfile {
    let (reference_rows, reference_f0) = harmonic_rows(reference);
    let (candidate_rows, candidate_f0) = harmonic_rows(candidate);
    if reference_rows.len() < HARMONIC_MIN_FRAMES || candidate_rows.len() < HARMONIC_MIN_FRAMES {
        return HarmonicProfile {
            status: "not_applicable".into(),
            harmonics: HARMONIC_COUNT,
            frames: HarmonicFrames {
                reference: reference_rows.len(),
                candidate: candidate_rows.len(),
            },
            fundamental_hz: [None, None],
            harmonic_delta_db: None,
            distance_db: None,
            thd_percent: [None, None],
        };
    }
    let mut reference_db = [0.0_f64; HARMONIC_COUNT - 1];
    let mut candidate_db = [0.0_f64; HARMONIC_COUNT - 1];
    for harmonic in 1..HARMONIC_COUNT {
        let reference_values: Vec<f64> = reference_rows.iter().map(|row| row[harmonic]).collect();
        let candidate_values: Vec<f64> = candidate_rows.iter().map(|row| row[harmonic]).collect();
        reference_db[harmonic - 1] = percentile(&reference_values, 0.5);
        candidate_db[harmonic - 1] = percentile(&candidate_values, 0.5);
    }
    let delta: Vec<f64> = reference_db
        .iter()
        .zip(&candidate_db)
        .map(|(reference, candidate)| candidate - reference)
        .collect();
    let distance = delta.iter().map(|value| value.abs()).sum::<f64>() / delta.len() as f64;
    HarmonicProfile {
        status: "ok".into(),
        harmonics: HARMONIC_COUNT,
        frames: HarmonicFrames {
            reference: reference_rows.len(),
            candidate: candidate_rows.len(),
        },
        fundamental_hz: [
            Some(percentile(&reference_f0, 0.5)),
            Some(percentile(&candidate_f0, 0.5)),
        ],
        harmonic_delta_db: Some(delta),
        distance_db: Some(distance),
        thd_percent: [
            Some(harmonic_thd(&reference_db)),
            Some(harmonic_thd(&candidate_db)),
        ],
    }
}

fn component(term: Option<f64>, weight: f64) -> ScoreComponent {
    ScoreComponent {
        term,
        weight,
        contribution: term.map(|value| value * weight),
    }
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
fn weighted_distance(terms: &[Option<f64>], weights: &[f64]) -> f64 {
    let available: f64 = terms
        .iter()
        .zip(weights)
        .filter_map(|(term, weight)| term.map(|_| *weight))
        .sum();
    if available <= 0.0 {
        return 0.0;
    }
    terms
        .iter()
        .zip(weights)
        .filter_map(|(term, weight)| term.map(|value| *weight * value))
        .sum::<f64>()
        / available
}

#[allow(clippy::too_many_arguments)]
fn score(
    level: &CompareLevel,
    log_mel: &LogMelDistance,
    envelope: &EnvelopeCorrelation,
    body: &BodyMetrics,
    tilt: &TiltMetrics,
    third: &BandTable,
    mfcc: &MfccMetrics,
    lsd: &LsdMetrics,
    harmonics: &HarmonicProfile,
    transient: &TransientMetrics,
    onset: &OnsetMetrics,
    warnings: &mut Vec<String>,
) -> Score {
    let silent = level.reference.rms_dbfs < SILENCE_DBFS
        || level.candidate.rms_dbfs < SILENCE_DBFS
        || level.reference.peak_dbfs < SILENCE_DBFS
        || level.candidate.peak_dbfs < SILENCE_DBFS;
    let spectral_db = log_mel.mean_abs_delta_db_level_matched;
    let lufs_delta = match (level.delta.integrated_lufs, level.delta.rms_dbfs) {
        (Some(value), _) => value.abs(),
        (None, Some(value)) => value.abs(),
        (None, None) => 0.0,
    };
    // NaN saturates to the top of the range here; `clamp` would propagate it.
    #[allow(clippy::manual_clamp)]
    let spectral_term = (spectral_db / SPECTRAL_SCALE_DB).min(1.0).max(0.0);
    #[allow(clippy::manual_clamp)]
    let level_term = (lufs_delta / LEVEL_SCALE_LU).min(1.0).max(0.0);
    let envelope_term = match envelope.correlation {
        Some(value) => 1.0 - value.clamp(0.0, 1.0),
        None => 0.0,
    };
    let body_term = Some(
        0.5 * ((body.low.rms_delta_db.abs() / BODY_SCALE_DB).min(1.0)
            + (body.mid.rms_delta_db.abs() / BODY_SCALE_DB).min(1.0)),
    );
    let tilt_term = tilt
        .tilt_db
        .map(|value| (value.abs() / TILT_SCALE_DB).min(1.0));
    let crest_term = level
        .delta
        .crest_db
        .map(|value| (value.abs() / CREST_SCALE_DB).min(1.0))
        .unwrap_or(0.0);
    let transient_term = transient
        .flux_ratio_db
        .map(|value| (value.abs() / FLUX_SCALE_DB).min(1.0))
        .unwrap_or(0.0);
    let crest_attack_term = Some(crest_term);
    let harmonic_term = if harmonics.status == "ok" {
        harmonics
            .distance_db
            .map(|value| (value / HARMONIC_SCALE_DB).min(1.0))
    } else {
        None
    };
    let mfcc_term = mfcc
        .mean_cosine_distance
        .map(|value| (value / MFCC_SCALE).min(1.0));
    let third_term = third_octave_rms(third)
        .1
        .map(|value| (value / THIRD_OCTAVE_SCALE_DB).min(1.0));
    let lsd_term = Some(lsd_score_term(lsd.level_matched_db));
    let onset_term = onset
        .onset_strength_median_db
        .map(|value| (value.abs() / ONSET_SCALE_DB).min(1.0));
    let density_term = if onset.reference_count == 0 && onset.candidate_count == 0 {
        Some(0.0)
    } else if onset.reference_count < DENSITY_MIN_EVENTS
        || onset.candidate_count < DENSITY_MIN_EVENTS
    {
        None
    } else {
        Some(
            onset
                .density_ratio_db
                .map(|value| (value.abs() / DENSITY_SCALE_DB).min(1.0))
                .unwrap_or(1.0),
        )
    };
    let terms = [
        Some(spectral_term),
        Some(level_term),
        Some(envelope_term),
        body_term,
        tilt_term,
        crest_attack_term,
        harmonic_term,
        mfcc_term,
        third_term,
        lsd_term,
        onset_term,
        density_term,
    ];
    let distance = weighted_distance(&terms, &SCORE_WEIGHTS);
    let legacy_distance = LEGACY_SCORE_WEIGHTS[0] * spectral_term
        + LEGACY_SCORE_WEIGHTS[1] * level_term
        + LEGACY_SCORE_WEIGHTS[2] * envelope_term;
    let previous_terms = [
        Some(spectral_term),
        Some(level_term),
        Some(envelope_term),
        body_term,
        tilt_term,
        Some(0.5 * (crest_term + transient_term)),
        harmonic_term,
    ];
    let previous_distance = weighted_distance(&previous_terms, &PREVIOUS_SCORE_WEIGHTS);
    let mut value = 100.0 * (1.0 - distance);
    let mut legacy_value = 100.0 * (1.0 - legacy_distance);
    let mut previous_value = 100.0 * (1.0 - previous_distance);
    if silent {
        value = 0.0;
        legacy_value = 0.0;
        previous_value = 0.0;
        warnings
            .push("headline score pinned to 0 because at least one render is near-silent".into());
    }
    let label = if value >= 90.0 {
        "close"
    } else if value >= 75.0 {
        "audible differences"
    } else if value >= 50.0 {
        "clearly different"
    } else {
        "broken / very different"
    };
    Score {
        value,
        label: label.to_string(),
        legacy_value,
        previous_value,
        spectral: component(terms[0], SCORE_WEIGHTS[0]),
        level: component(terms[1], SCORE_WEIGHTS[1]),
        envelope: component(terms[2], SCORE_WEIGHTS[2]),
        body: component(terms[3], SCORE_WEIGHTS[3]),
        tilt: component(terms[4], SCORE_WEIGHTS[4]),
        crest_attack: component(terms[5], SCORE_WEIGHTS[5]),
        harmonic: component(terms[6], SCORE_WEIGHTS[6]),
        mfcc: component(terms[7], SCORE_WEIGHTS[7]),
        third_octave: component(terms[8], SCORE_WEIGHTS[8]),
        lsd: component(terms[9], SCORE_WEIGHTS[9]),
        onset: component(terms[10], SCORE_WEIGHTS[10]),
        density: component(terms[11], SCORE_WEIGHTS[11]),
        spectral_scale_db: SPECTRAL_SCALE_DB,
        level_scale_lu: LEVEL_SCALE_LU,
        body_scale_db: BODY_SCALE_DB,
        tilt_scale_db: TILT_SCALE_DB,
        crest_scale_db: CREST_SCALE_DB,
        flux_scale_db: FLUX_SCALE_DB,
        harmonic_scale_db: HARMONIC_SCALE_DB,
        mfcc_scale: MFCC_SCALE,
        third_octave_scale_db: THIRD_OCTAVE_SCALE_DB,
        lsd_scale_db: LSD_SCALE_DB,
        onset_scale_db: ONSET_SCALE_DB,
        density_scale_db: DENSITY_SCALE_DB,
        note: "Heuristic 0-100 similarity (100 = identical); compare only within the same track and gain staging. legacy_value is the pre-1.1 three-term score; previous_value is the v1.2.0 seven-term score."
            .into(),
    }
}

/// Compare two decoded buffers and report their differences.
pub fn compare_buffers(
    reference_path: &str,
    reference: &AudioBuffer,
    candidate_path: &str,
    candidate: &AudioBuffer,
    options: CompareOptions,
) -> Result<CompareReport, String> {
    if reference.channel_count() > 2 || candidate.channel_count() > 2 {
        return Err("only mono and stereo files are supported".into());
    }
    let stems = options.stems;
    if !options.allow_mismatch {
        if reference.sample_rate != candidate.sample_rate {
            return Err(format!(
                "sample-rate mismatch: reference {} Hz, candidate {} Hz; resample first or set allow_mismatch",
                reference.sample_rate, candidate.sample_rate
            ));
        }
        if reference.frames() != candidate.frames() {
            return Err(format!(
                "length mismatch: reference {:.6} s ({} frames), candidate {:.6} s ({} frames); trim first or set allow_mismatch",
                reference.duration_s(),
                reference.frames(),
                candidate.duration_s(),
                candidate.frames()
            ));
        }
    }
    let mut warnings = Vec::new();
    let reference_info = FileInfo {
        path: reference_path.to_string(),
        sample_rate: reference.sample_rate,
        channels: reference.channel_count(),
        duration_s: reference.duration_s(),
    };
    let mut candidate_info = FileInfo {
        path: candidate_path.to_string(),
        sample_rate: candidate.sample_rate,
        channels: candidate.channel_count(),
        duration_s: candidate.duration_s(),
    };
    let candidate_owned;
    let candidate = if candidate.sample_rate != reference.sample_rate {
        let rate = candidate.sample_rate;
        let channels: Vec<Vec<f64>> = candidate
            .channels
            .iter()
            .map(|channel| resample(channel, rate, reference.sample_rate))
            .collect();
        warnings.push(format!(
            "sample-rate mismatch: candidate {rate} Hz resampled to {} Hz for analysis",
            reference.sample_rate
        ));
        candidate_owned = AudioBuffer {
            sample_rate: reference.sample_rate,
            channels,
        };
        &candidate_owned
    } else {
        candidate
    };
    candidate_info.sample_rate = candidate.sample_rate;
    if (candidate_info.duration_s - reference_info.duration_s).abs() > 0.1 {
        warnings.push(format!(
            "duration mismatch: reference {:.3} s, candidate {:.3} s",
            reference_info.duration_s, candidate_info.duration_s
        ));
    }
    if reference.channel_count() == 1 || candidate.channel_count() == 1 {
        warnings.push("mono input: stereo correlation and mid/side ratio are null".into());
    }

    let reference_level = level_metrics(reference);
    let candidate_level = level_metrics(candidate);
    let delta = LevelDelta {
        peak_dbfs: Some(candidate_level.peak_dbfs - reference_level.peak_dbfs),
        rms_dbfs: Some(candidate_level.rms_dbfs - reference_level.rms_dbfs),
        crest_db: Some(candidate_level.crest_db - reference_level.crest_db),
        integrated_lufs: match (
            reference_level.integrated_lufs,
            candidate_level.integrated_lufs,
        ) {
            (Some(reference), Some(candidate)) => Some(candidate - reference),
            _ => None,
        },
    };

    let (alignment, lag_samples) = estimate_alignment(reference, candidate, stems);
    if alignment.confidence == "low" {
        warnings.push(
            "weak alignment evidence; global lag forced to 0 ms and comparison uses the overlap"
                .into(),
        );
    }
    let aligned_reference = AudioBuffer {
        sample_rate: reference.sample_rate,
        channels: reference
            .channels
            .iter()
            .map(|channel| slice_channel(channel, lag_samples))
            .collect(),
    };
    let aligned_candidate = AudioBuffer {
        sample_rate: candidate.sample_rate,
        channels: candidate
            .channels
            .iter()
            .map(|channel| slice_channel(channel, -lag_samples))
            .collect(),
    };
    let frames = aligned_reference.frames().min(aligned_candidate.frames());
    if frames < 1024 {
        return Err("aligned overlap is shorter than one analysis window".into());
    }
    let aligned_reference = AudioBuffer {
        sample_rate: aligned_reference.sample_rate,
        channels: aligned_reference
            .channels
            .iter()
            .map(|channel| channel[..frames].to_vec())
            .collect(),
    };
    let aligned_candidate = AudioBuffer {
        sample_rate: aligned_candidate.sample_rate,
        channels: aligned_candidate
            .channels
            .iter()
            .map(|channel| channel[..frames].to_vec())
            .collect(),
    };

    let sample = sample_metrics(&aligned_reference, &aligned_candidate);
    let log_mel = log_mel_distance(&aligned_reference, &aligned_candidate);
    let envelope = envelope_correlation(&aligned_reference, &aligned_candidate);
    let body = body_metrics(&aligned_reference, &aligned_candidate);
    let (octave_reference, octave_candidate, octave_deltas) =
        octave_delta(&aligned_reference, &aligned_candidate);
    let tilt = tilt_metrics(&octave_reference, &octave_candidate, &octave_deltas);
    let third_octave = third_octave_table(&aligned_reference, &aligned_candidate);
    let mfcc = mfcc_metrics(&aligned_reference, &aligned_candidate);
    let lsd = lsd_metrics(&aligned_reference, &aligned_candidate);
    let harmonics = harmonic_profile(&aligned_reference, &aligned_candidate);
    let transient = transient_metrics(&aligned_reference, &aligned_candidate);
    let onset = onset_metrics(&aligned_reference, &aligned_candidate);
    let audible = reference_level.rms_dbfs >= SILENCE_DBFS
        && candidate_level.rms_dbfs >= SILENCE_DBFS
        && reference_level.peak_dbfs >= SILENCE_DBFS
        && candidate_level.peak_dbfs >= SILENCE_DBFS;
    let dtw = dtw_metrics(&aligned_reference, &aligned_candidate, stems);
    let pitch = pitch_metrics(&aligned_reference, &aligned_candidate, stems, audible);
    let level = CompareLevel {
        reference: reference_level,
        candidate: candidate_level,
        delta,
    };
    let score = score(
        &level,
        &log_mel,
        &envelope,
        &body,
        &tilt,
        &third_octave,
        &mfcc,
        &lsd,
        &harmonics,
        &transient,
        &onset,
        &mut warnings,
    );
    if score.value > 0.0 {
        if level.reference.rms_dbfs < SILENCE_DBFS || level.reference.peak_dbfs < SILENCE_DBFS {
            warnings.push("reference is near-silent; most metrics are undefined".into());
        }
        if level.candidate.rms_dbfs < SILENCE_DBFS || level.candidate.peak_dbfs < SILENCE_DBFS {
            warnings
                .push("candidate is near-silent; envelope and spectral metrics are weak".into());
        }
    }
    if pitch.status != "ok" {
        warnings.push(format!(
            "pitch not applicable: {}",
            pitch.reason.as_deref().unwrap_or("unknown")
        ));
    }
    Ok(CompareReport {
        reference: reference_info,
        candidate: candidate_info,
        aligned_duration_s: frames as f64 / f64::from(reference.sample_rate),
        alignment,
        sample,
        level,
        log_mel,
        envelope_correlation: envelope,
        body,
        tilt,
        third_octave,
        mfcc,
        lsd,
        harmonics,
        transient,
        onset,
        dtw,
        pitch,
        score,
        warnings,
    })
}

/// Compare two WAV files.
pub fn compare_wavs(
    reference: impl AsRef<Path>,
    candidate: impl AsRef<Path>,
    options: CompareOptions,
) -> Result<CompareReport, String> {
    let reference_path = reference.as_ref();
    let candidate_path = candidate.as_ref();
    let reference_buffer = read_wav(reference_path)?;
    let candidate_buffer = read_wav(candidate_path)?;
    compare_buffers(
        &reference_path.display().to_string(),
        &reference_buffer,
        &candidate_path.display().to_string(),
        &candidate_buffer,
        options,
    )
}

// ============================================================================
// Tests
// ============================================================================

/// Undefined LSD scores map to one. Infinities saturate and either signed zero
/// maps to positive zero.
fn lsd_score_term(level_matched_db: f64) -> f64 {
    let ratio = level_matched_db / LSD_SCALE_DB;
    if ratio.is_nan() {
        1.0
    } else {
        ratio.clamp(0.0, 1.0).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsd_score_term_preserves_nonfinite_and_boundary_behavior() {
        for (input, expected) in [
            (f64::NAN, 1.0),
            (f64::INFINITY, 1.0),
            (f64::NEG_INFINITY, 0.0),
            (-1.0, 0.0),
            (0.0, 0.0),
            (LSD_SCALE_DB * 0.5, 0.5),
            (LSD_SCALE_DB, 1.0),
            (LSD_SCALE_DB * 2.0, 1.0),
        ] {
            assert_eq!(lsd_score_term(input), expected);
        }
        for zero in [0.0, -0.0] {
            assert_eq!(lsd_score_term(zero).to_bits(), 0.0_f64.to_bits());
        }
    }

    #[test]
    fn short_alignment_search_cannot_run_past_its_frames() {
        let onset = [0.0, 0.8, 0.1, 0.3, 0.9, 0.0];
        let (correlation, lag) = best_shift(&onset, &onset, 150);
        assert_eq!(lag, 0);
        assert!((correlation - 1.0).abs() < 1e-12);
    }

    #[test]
    fn true_peak_estimate_sees_intersample_overshoot() {
        let rate = 48_000;
        let mut samples = Vec::with_capacity(rate as usize * 2);
        for index in 0..rate as usize {
            let value = (2.0 * std::f64::consts::PI * 12_000.0 * index as f64 / f64::from(rate)
                + std::f64::consts::FRAC_PI_4)
                .sin() as f32;
            samples.extend_from_slice(&[value, value]);
        }
        let sample_peak = samples
            .iter()
            .fold(0.0_f32, |peak, value| peak.max(value.abs()));
        let true_peak = 10.0_f64.powf(true_peak_dbfs(&samples) / 20.0);
        assert!(true_peak > f64::from(sample_peak) + 0.01);
        assert!(true_peak <= 1.01);
    }

    #[test]
    fn true_peak_estimate_agrees_with_the_limiter_detector() {
        use crate::dsp::AudioEffect;
        use oc_dsp::limiter::Limiter;
        for (frequency, phase) in [
            (12_000.0, std::f64::consts::FRAC_PI_4),
            (997.0, 0.31),
            (19_000.0, 0.7),
        ] {
            let mut samples = Vec::with_capacity(48_000 * 2);
            for index in 0..48_000 {
                let value = (2.0 * std::f64::consts::PI * frequency * index as f64 / 48_000.0
                    + phase)
                    .sin() as f32;
                samples.extend_from_slice(&[value, -value * 0.7]);
            }
            let estimate = true_peak_dbfs(&samples);
            let mut limiter = Limiter::new(48_000);
            limiter.set_param("ceiling", 0.0);
            let mut copy = samples.clone();
            limiter.process(&mut copy, 48_000);
            let detector = limiter.max_input_peak_dbfs();
            assert!(
                (estimate - detector).abs() < 1e-9,
                "{frequency} Hz: analysis {estimate} vs detector {detector}"
            );
            // The limiter must not alter a signal that sits below the ceiling.
            if estimate < -0.5 {
                assert_eq!(copy, samples);
            }
        }
    }

    fn sine_buffer(frequency: f64, amplitude: f64, seconds: f64, rate: u32) -> AudioBuffer {
        let frames = (seconds * f64::from(rate)).round() as usize;
        let channel: Vec<f64> = (0..frames)
            .map(|index| {
                amplitude
                    * (2.0 * std::f64::consts::PI * frequency * index as f64 / f64::from(rate))
                        .sin()
            })
            .collect();
        AudioBuffer {
            sample_rate: rate,
            channels: vec![channel],
        }
    }

    fn stereo(left: Vec<f64>, right: Vec<f64>) -> AudioBuffer {
        AudioBuffer {
            sample_rate: 48_000,
            channels: vec![left, right],
        }
    }

    fn silence(rate: u32, seconds: f64) -> Vec<f64> {
        vec![0.0; (seconds * f64::from(rate)).round() as usize]
    }

    /// Deterministic broadband noise fills every mel band above the 80 dB
    /// floor so a gain change only shifts the level, not the spectral shape.
    fn broadband_buffer(amplitude: f64, seconds: f64, rate: u32) -> AudioBuffer {
        let frames = (seconds * f64::from(rate)).round() as usize;
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut channel = Vec::with_capacity(frames);
        for _ in 0..frames {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let value = (state >> 11) as f64 / (1u64 << 53) as f64;
            channel.push(amplitude * (value * 2.0 - 1.0));
        }
        AudioBuffer {
            sample_rate: rate,
            channels: vec![channel],
        }
    }

    #[test]
    fn level_metrics_match_analytic_sine() {
        let buffer = sine_buffer(1000.0, 0.5, 2.0, 48_000);
        let level = level_metrics(&buffer);
        assert!((level.peak_dbfs - -6.0206).abs() < 0.001, "{level:?}");
        assert!((level.rms_dbfs - -9.0309).abs() < 0.001, "{level:?}");
        assert!((level.crest_db - 3.0103).abs() < 0.001, "{level:?}");
        let lufs = level.integrated_lufs.expect("lufs");
        assert!((lufs - -9.02).abs() < 0.1, "{lufs}");
    }

    #[test]
    fn lufs_survives_a_44_1_khz_source() {
        let buffer = sine_buffer(1000.0, 0.5, 2.0, 44_100);
        let lufs = level_metrics(&buffer).integrated_lufs.expect("lufs");
        assert!((lufs - -9.02).abs() < 0.2, "{lufs}");
    }

    /// The live short-term meter is the file meter's own measurement: on a
    /// steady tone the streamed 3 s reading equals the loudest 3 s window.
    #[test]
    fn streaming_short_term_loudness_matches_the_file_meter() {
        for rate in [48_000, 44_100] {
            let single = sine_buffer(1_000.0, 0.5, 4.0, rate);
            let channel = single.channels[0].clone();
            let dual = stereo(channel.clone(), channel.clone());
            let file = short_term_max_lufs(&dual).expect("the file meter reads");
            let mut live = oc_dsp::loudness::ShortTermLoudness::new(rate);
            let mut interleaved = Vec::with_capacity(channel.len() * 2);
            for sample in &channel {
                interleaved.push(*sample as f32);
                interleaved.push(*sample as f32);
            }
            for chunk in interleaved.chunks(4_096 * 2) {
                live.push_interleaved(chunk);
            }
            let live = live.lufs().expect("the live meter reads");
            // The file meter resamples a non-48 kHz source with a 32-tap
            // windowed sinc, which carries its own few tenths of a dB there;
            // see `lufs_survives_a_44_1_khz_source`. The live meter filters at
            // the stream's own rate, so it is compared tightly at 48 kHz.
            let tolerance = if rate == 48_000 { 0.1 } else { 0.4 };
            assert!(
                (live - file).abs() < tolerance,
                "rate {rate}: live {live} LUFS against file {file} LUFS"
            );
        }
    }

    /// The live meter is rate-aware: the same tone reads the same at 44.1 kHz
    /// as at 48 kHz, because the K-weighting is derived at the stream's rate.
    #[test]
    fn streaming_loudness_does_not_depend_on_the_stream_rate() {
        fn reading(rate: u32) -> f64 {
            let single = sine_buffer(1_000.0, 0.5, 4.0, rate);
            let channel = single.channels[0].clone();
            let mut live = oc_dsp::loudness::ShortTermLoudness::new(rate);
            let mut interleaved = Vec::with_capacity(channel.len() * 2);
            for sample in &channel {
                interleaved.push(*sample as f32);
                interleaved.push(*sample as f32);
            }
            for chunk in interleaved.chunks(4_096 * 2) {
                live.push_interleaved(chunk);
            }
            live.lufs().expect("the live meter reads")
        }
        let difference = (reading(48_000) - reading(44_100)).abs();
        assert!(difference < 0.1, "rates differ by {difference} LU");
    }

    #[test]
    fn dual_mono_is_three_lu_louder_than_mono() {
        let mono = sine_buffer(1000.0, 0.5, 2.0, 48_000);
        let channel = mono.channels[0].clone();
        let dual = stereo(channel.clone(), channel);
        let mono_lufs = level_metrics(&mono).integrated_lufs.unwrap();
        let dual_lufs = level_metrics(&dual).integrated_lufs.unwrap();
        assert!((dual_lufs - mono_lufs - 3.0103).abs() < 0.05);
        assert!((level_metrics(&dual).rms_dbfs - level_metrics(&mono).rms_dbfs).abs() < 0.001);
    }

    #[test]
    fn octave_band_energy_is_analytic_for_a_sine() {
        let buffer = sine_buffer(1000.0, 0.5, 1.0, 48_000);
        let energies = octave_energies(&buffer);
        let index = OCTAVE_CENTERS
            .iter()
            .position(|center| (*center - 1000.0).abs() < 1.0)
            .unwrap();
        // A 0.5-amplitude sine has -9.03 dB mean square and all of its energy is
        // inside the 707-1414 Hz octave band.
        assert!((energies[index] - -9.0309).abs() < 1.5, "{energies:?}");
        let neighbour = energies[index + 1];
        assert!(neighbour < -40.0, "{energies:?}");
    }

    #[test]
    fn mono_fold_octaves_distinguish_preserved_energy_from_silence() {
        let mono = sine_buffer(63.0, 0.25, 0.5, 8_000);
        assert!(mono_fold_octaves(&mono).is_err());
        let channel = &mono.channels[0];
        let dual_mono = AudioBuffer {
            sample_rate: 8_000,
            channels: vec![channel.clone(), channel.clone()],
        };
        let report = mono_fold_octaves(&dual_mono).unwrap();
        assert_eq!(report.resolution_hz, 8_000.0 / 2_048.0);
        assert!(report.bands.iter().all(|band| band.center_hz < 4_000.0));
        let sub = report
            .bands
            .iter()
            .find(|band| band.center_hz == 63.0)
            .unwrap();
        assert_eq!(sub.loss_db, Some(0.0));
        assert!(!sub.mono_at_floor);

        let silence = AudioBuffer {
            sample_rate: 8_000,
            channels: vec![vec![0.0; 4_000]; 2],
        };
        let report = mono_fold_octaves(&silence).unwrap();
        assert!(
            report
                .bands
                .iter()
                .all(|band| band.loss_db.is_none() && band.mono_at_floor)
        );
    }

    #[test]
    fn stereo_correlation_and_mid_side_track_phase() {
        let left = sine_buffer(1000.0, 0.5, 1.0, 48_000).channels.remove(0);
        let same = stereo_metrics(&stereo(left.clone(), left.clone()));
        assert!((same.correlation.unwrap() - 1.0).abs() < 1e-9);
        let inverted = stereo(left.clone(), left.iter().map(|value| -value).collect());
        let metrics = stereo_metrics(&inverted);
        assert!((metrics.correlation.unwrap() + 1.0).abs() < 1e-9);
        assert!(metrics.mid_side_ratio_db.unwrap() < -50.0);
        let mono = super::stereo_metrics(&sine_buffer(1000.0, 0.5, 1.0, 48_000));
        assert!(mono.correlation.is_none());
    }

    #[test]
    fn silence_and_section_boundaries_are_detected() {
        let rate = 48_000;
        let mut channel = silence(rate, 0.5);
        channel.extend(
            sine_buffer(1000.0, 0.5, 0.5, rate).channels[0]
                .iter()
                .copied(),
        );
        channel.extend(silence(rate, 0.5));
        let buffer = AudioBuffer {
            sample_rate: rate,
            channels: vec![channel],
        };
        let (silence_regions, sections) = silence_and_sections(&buffer);
        assert_eq!(sections.len(), 1, "{sections:?}");
        assert!((sections[0].start_s - 0.5).abs() < 0.03, "{sections:?}");
        assert!((sections[0].end_s - 1.0).abs() < 0.03, "{sections:?}");
        assert_eq!(silence_regions.len(), 2, "{silence_regions:?}");
        assert!(silence_regions[0].start_s.abs() < 0.03);
        assert!((silence_regions[0].end_s - 0.5).abs() < 0.03);
        assert!((silence_regions[1].start_s - 1.0).abs() < 0.03);
        assert!((silence_regions[1].duration_s - 0.5).abs() < 0.05);
    }

    #[test]
    fn analysis_levels_are_the_meter_levels() {
        let mono = sine_buffer(1000.0, 0.5, 2.0, 48_000);
        // WAV analysis reads f32; quantize first so both paths see the same bits.
        let channel: Vec<f64> = mono.channels[0]
            .iter()
            .map(|sample| f64::from(*sample as f32))
            .collect();
        let buffer = stereo(channel.clone(), channel);
        let report = analyze_buffer("tone.wav", &buffer);
        let interleaved: Vec<f32> = (0..buffer.frames())
            .flat_map(|frame| {
                [
                    buffer.channels[0][frame] as f32,
                    buffer.channels[1][frame] as f32,
                ]
            })
            .collect();
        let meter = measure_stereo_interleaved(&interleaved, 48_000);
        assert_eq!(report.level.peak_dbfs, meter.peak_dbfs);
        assert_eq!(report.level.rms_dbfs, meter.rms_dbfs);
        assert_eq!(report.level.crest_db, meter.crest_db);
        assert_eq!(report.level.integrated_lufs, meter.integrated_lufs);
    }

    #[test]
    fn analysis_reports_true_peak_and_the_fundamental_peak() {
        let buffer = sine_buffer(1200.0, 0.5, 1.0, 48_000);
        let report = analyze_buffer("tone.wav", &buffer);
        assert!(
            report.true_peak_dbfs >= report.level.peak_dbfs,
            "{report:?}"
        );
        assert!(report.spectral_peaks.resolution_hz > 0.0);
        let loudest = report
            .spectral_peaks
            .peaks
            .first()
            .expect("a spectral peak");
        assert!((loudest.frequency_hz - 1200.0).abs() < 5.0, "{loudest:?}");
        assert!(loudest.relative_db.abs() < 1e-9);
    }

    #[test]
    fn spectral_peaks_rank_by_level() {
        let low = sine_buffer(500.0, 0.4, 2.0, 48_000);
        let high = sine_buffer(3000.0, 0.05, 2.0, 48_000);
        let channel: Vec<f64> = low.channels[0]
            .iter()
            .zip(&high.channels[0])
            .map(|(low, high)| low + high)
            .collect();
        let report = analyze_buffer(
            "two-tone.wav",
            &AudioBuffer {
                sample_rate: 48_000,
                channels: vec![channel],
            },
        );
        let peaks = &report.spectral_peaks.peaks;
        assert!(peaks.len() >= 2, "{peaks:?}");
        assert!((peaks[0].frequency_hz - 500.0).abs() < 5.0, "{peaks:?}");
        assert!((peaks[1].frequency_hz - 3000.0).abs() < 10.0, "{peaks:?}");
        assert!(peaks[1].relative_db < -10.0, "{peaks:?}");
    }

    #[test]
    fn wav_roundtrip_preserves_level() {
        let dir = std::env::temp_dir().join(format!("oc-analysis-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let buffer = sine_buffer(1000.0, 0.5, 1.0, 48_000);
        let stereo: Vec<f32> = buffer.channels[0]
            .iter()
            .flat_map(|&sample| [sample as f32, sample as f32])
            .collect();
        crate::wav::write_stereo_wav(&path, &stereo, 48_000).unwrap();
        let read = read_wav(&path).unwrap();
        assert_eq!(read.sample_rate, 48_000);
        assert_eq!(read.channel_count(), 2);
        let level = level_metrics(&read);
        assert!((level.peak_dbfs - -6.0206).abs() < 0.01, "{level:?}");
        assert!((level.rms_dbfs - -9.0309).abs() < 0.01, "{level:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_renders_score_one_hundred() {
        let buffer = sine_buffer(440.0, 0.4, 2.0, 48_000);
        let report = compare_buffers(
            "a.wav",
            &buffer,
            "b.wav",
            &buffer,
            CompareOptions::default(),
        )
        .unwrap();
        assert!(
            (report.score.value - 100.0).abs() < 1e-9,
            "{:?}",
            report.score
        );
        assert!(report.log_mel.mean_abs_delta_db <= 1e-9);
        assert!((report.envelope_correlation.correlation.unwrap() - 1.0).abs() < 1e-9);
        assert!(report.level.delta.integrated_lufs.unwrap().abs() < 1e-9);
    }

    #[test]
    fn gain_change_is_level_and_body_but_keeps_the_legacy_score() {
        let reference = broadband_buffer(0.08, 2.0, 48_000);
        let candidate = broadband_buffer(0.04, 2.0, 48_000);
        let report = compare_buffers(
            "ref.wav",
            &reference,
            "cand.wav",
            &candidate,
            CompareOptions::default(),
        )
        .unwrap();
        let delta = report.level.delta.integrated_lufs.unwrap();
        assert!((delta + 6.0206).abs() < 0.05, "{delta}");
        assert!(report.log_mel.mean_abs_delta_db_level_matched < 0.01);
        // A 6 dB broadband gain is a level plus body error; the legacy
        // three-term score is unchanged at 75.
        assert!(
            (report.score.legacy_value - 75.0).abs() < 0.5,
            "{:?}",
            report.score
        );
        assert!(
            (report.score.value - 77.0).abs() < 0.5,
            "{:?}",
            report.score
        );
        assert!(
            (report.score.previous_value - 71.75).abs() < 0.01,
            "{:?}",
            report.score
        );
        assert_eq!(report.score.body.term, Some(1.0));
        // The 6 dB gain change is visible in the sample domain too.
        assert!(
            report.sample.max_abs_difference > 0.02,
            "{:?}",
            report.sample
        );
        assert!((report.sample.correlation.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn compare_refuses_format_mismatches_unless_allowed() {
        let reference = broadband_buffer(0.08, 2.0, 48_000);
        let longer = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![vec![0.0; 3 * 48_000]],
        };
        let error = compare_buffers(
            "a.wav",
            &reference,
            "b.wav",
            &longer,
            CompareOptions::default(),
        )
        .unwrap_err();
        assert!(error.contains("length mismatch"), "{error}");
        let mut other_rate = broadband_buffer(0.08, 2.0, 44_100);
        other_rate.sample_rate = 44_100;
        let error = compare_buffers(
            "a.wav",
            &reference,
            "b.wav",
            &other_rate,
            CompareOptions::default(),
        )
        .unwrap_err();
        assert!(error.contains("sample-rate mismatch"), "{error}");
        let report = compare_buffers(
            "a.wav",
            &reference,
            "b.wav",
            &other_rate,
            CompareOptions {
                allow_mismatch: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.sample.max_abs_difference.is_finite());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("sample-rate"))
        );
    }

    #[test]
    fn identical_renders_have_zero_sample_difference() {
        let buffer = sine_buffer(440.0, 0.4, 2.0, 48_000);
        let report = compare_buffers(
            "a.wav",
            &buffer,
            "b.wav",
            &buffer,
            CompareOptions::default(),
        )
        .unwrap();
        assert_eq!(report.sample.max_abs_difference, 0.0);
        assert!((report.sample.correlation.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn compare_is_symmetric() {
        let reference = sine_buffer(440.0, 0.5, 2.0, 48_000);
        let candidate = sine_buffer(880.0, 0.35, 2.0, 48_000);
        let forward = compare_buffers(
            "a.wav",
            &reference,
            "b.wav",
            &candidate,
            CompareOptions::default(),
        )
        .unwrap();
        let reverse = compare_buffers(
            "b.wav",
            &candidate,
            "a.wav",
            &reference,
            CompareOptions::default(),
        )
        .unwrap();
        assert!((forward.score.value - reverse.score.value).abs() < 1e-9);
        assert!(
            (forward.log_mel.mean_abs_delta_db_level_matched
                - reverse.log_mel.mean_abs_delta_db_level_matched)
                .abs()
                < 1e-9
        );
        assert!(
            (forward.envelope_correlation.correlation.unwrap()
                - reverse.envelope_correlation.correlation.unwrap())
            .abs()
                < 1e-9
        );
        assert_eq!(
            forward.alignment.lag_samples,
            -reverse.alignment.lag_samples
        );
    }

    #[test]
    fn compare_rejects_short_and_wide_inputs() {
        let short = sine_buffer(1000.0, 0.5, 0.01, 48_000);
        assert!(compare_buffers("a", &short, "b", &short, CompareOptions::default()).is_err());
        let wide = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![vec![0.0; 48_000]; 3],
        };
        assert!(compare_buffers("a", &wide, "b", &wide, CompareOptions::default()).is_err());
    }

    fn mixed_buffer(parts: &[(f64, f64)], seconds: f64, rate: u32) -> AudioBuffer {
        let frames = (seconds * f64::from(rate)).round() as usize;
        let channel: Vec<f64> = (0..frames)
            .map(|index| {
                parts
                    .iter()
                    .map(|(frequency, amplitude)| {
                        amplitude
                            * (2.0 * std::f64::consts::PI * frequency * index as f64
                                / f64::from(rate))
                            .sin()
                    })
                    .sum()
            })
            .collect();
        AudioBuffer {
            sample_rate: rate,
            channels: vec![channel],
        }
    }

    #[test]
    fn body_and_tilt_follow_a_low_band_cut() {
        let reference = mixed_buffer(
            &[
                (63.0, 0.2),
                (125.0, 0.2),
                (250.0, 0.2),
                (500.0, 0.2),
                (8000.0, 0.2),
            ],
            2.0,
            48_000,
        );
        let candidate = mixed_buffer(
            &[
                (63.0, 0.1),
                (125.0, 0.1),
                (250.0, 0.1),
                (500.0, 0.1),
                (8000.0, 0.2),
            ],
            2.0,
            48_000,
        );
        let report = compare_buffers(
            "ref.wav",
            &reference,
            "cand.wav",
            &candidate,
            CompareOptions {
                stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            (report.body.low.rms_delta_db + 6.0206).abs() < 0.2,
            "{}",
            report.body.low.rms_delta_db
        );
        assert!(
            (report.body.mid.rms_delta_db + 6.0206).abs() < 0.2,
            "{}",
            report.body.mid.rms_delta_db
        );
        let tilt = report.tilt.tilt_db.unwrap();
        assert!((tilt + 6.0206).abs() < 0.3, "{tilt}");
        assert!(!report.tilt.level_matched);
    }

    #[test]
    fn harmonic_profile_reports_a_third_harmonic() {
        let fundamental = sine_buffer(65.4, 0.2, 2.0, 48_000);
        let third = sine_buffer(3.0 * 65.4, 0.02, 2.0, 48_000);
        let candidate = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![
                fundamental.channels[0]
                    .iter()
                    .zip(&third.channels[0])
                    .map(|(fundamental, third)| fundamental + third)
                    .collect(),
            ],
        };
        let report = compare_buffers(
            "ref.wav",
            &fundamental,
            "cand.wav",
            &candidate,
            CompareOptions {
                stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        let harmonics = &report.harmonics;
        assert_eq!(harmonics.status, "ok");
        let f0 = harmonics.fundamental_hz[1].unwrap();
        assert!((f0 - 65.4).abs() < 0.5, "{f0}");
        let thd = harmonics.thd_percent[1].unwrap();
        assert!((8.0..12.0).contains(&thd), "{thd}");
        let distance = harmonics.distance_db.unwrap();
        assert!((distance - 5.714).abs() < 0.4, "{distance}");
    }

    #[test]
    fn flattened_crest_raises_the_crest_attack_term() {
        let mut sparse = vec![0.0; 96_000];
        for index in (0..sparse.len()).step_by(12_000) {
            sparse[index] = 0.8;
        }
        let body = sine_buffer(1000.0, 1.0, 2.0, 48_000).channels.remove(0);
        let flattened: Vec<f64> = sparse
            .iter()
            .zip(&body)
            .map(|(peak, body)| 0.4 * peak + 0.2 * body)
            .collect();
        let reference = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![sparse],
        };
        let candidate = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![flattened],
        };
        let report = compare_buffers(
            "ref.wav",
            &reference,
            "cand.wav",
            &candidate,
            CompareOptions {
                stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        let crest = report.level.delta.crest_db.unwrap();
        assert!(crest < -10.0, "{crest}");
        let term = report.score.crest_attack.term.unwrap();
        assert!(term > 0.5, "{term}");
    }

    /// Cross-checks the score, spectral-shape, onset, DTW and pitch terms against
    /// `scripts/wav_compare.py`. Skipped when python3 or numpy is unavailable.
    /// Tolerances cover Python's JSON rounding (2 decimals for bands, dB and
    /// cents; 4 for MFCC), its 0.1 ms lag quantization, and FFT-level
    /// differences: 2.0 score points (also for `previous_value`), 0.05 dB per
    /// third-octave band and sub-gate mean, 0.005 on MFCC cosines, 0.1 dB on LSD
    /// and per-attack strength, 0.005 on onset correlation, 5 ms on onset lag,
    /// 0.01 events/s and 0.1 dB on density, and 0.02 on each new score term.
    #[test]
    fn parity_with_python_compare() {
        let probe = std::process::Command::new("python3")
            .args(["-c", "import numpy"])
            .output();
        if !matches!(probe, Ok(output) if output.status.success()) {
            eprintln!("skipping python parity test: python3 with numpy is unavailable");
            return;
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/wav_compare.py")
            .canonicalize()
            .expect("compare.py path");
        let directory = std::env::temp_dir().join(format!(
            "oc-analysis-parity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let noisy = broadband_buffer(0.08, 2.0, 48_000);
        let mut high_cut = noisy.channels[0].clone();
        for (value, pair) in high_cut[1..].iter_mut().zip(noisy.channels[0].windows(2)) {
            *value = 0.5 * (pair[1] + pair[0]);
        }
        let harmonic_reference = mixed_buffer(&[(65.4, 0.2), (8000.0, 0.05)], 2.0, 48_000);
        let third = sine_buffer(3.0 * 65.4, 0.02, 2.0, 48_000);
        let harmonic_candidate = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![
                harmonic_reference.channels[0]
                    .iter()
                    .zip(&third.channels[0])
                    .map(|(reference, third)| reference + third)
                    .collect(),
            ],
        };
        let pitch_reference =
            mixed_buffer(&[(220.0, 0.3), (440.0, 0.06), (660.0, 0.03)], 2.0, 48_000);
        let pitch_frequency = 220.0 * 2.0_f64.powf(18.0 / 1200.0);
        let pitch_candidate = mixed_buffer(
            &[
                (pitch_frequency, 0.3),
                (pitch_frequency * 2.0, 0.06),
                (pitch_frequency * 3.0, 0.03),
            ],
            2.0,
            48_000,
        );
        // Transients exercise the onset-strength and density terms: five attacks
        // on both sides, the candidate's clicks 26 dB weaker.
        let burst_frames = 96_000usize;
        let mut onset_reference = vec![0.0; burst_frames];
        let mut onset_candidate = vec![0.0; burst_frames];
        for index in 0..burst_frames {
            let body = 0.05 * (2.0 * std::f64::consts::PI * 220.0 * index as f64 / 48_000.0).sin();
            onset_reference[index] += body;
            onset_candidate[index] += body;
        }
        for start in [4_800usize, 24_000, 43_200, 62_400, 81_600] {
            let length = 288usize; // 6 ms at 48 kHz
            for index in 0..length {
                let window = 0.5
                    - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / (length - 1) as f64).cos();
                let click =
                    window * (2.0 * std::f64::consts::PI * 3000.0 * index as f64 / 48_000.0).sin();
                onset_reference[start + index] += 0.4 * click;
                onset_candidate[start + index] += 0.02 * click;
            }
        }
        let onset_reference = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![onset_reference],
        };
        let onset_candidate = AudioBuffer {
            sample_rate: 48_000,
            channels: vec![onset_candidate],
        };
        let cases: Vec<(&str, AudioBuffer, AudioBuffer)> = vec![
            (
                "tilt",
                noisy,
                AudioBuffer {
                    sample_rate: 48_000,
                    channels: vec![high_cut],
                },
            ),
            ("harmonics", harmonic_reference, harmonic_candidate),
            (
                "gain",
                broadband_buffer(0.08, 2.0, 48_000),
                broadband_buffer(0.04, 2.0, 48_000),
            ),
            ("pitch", pitch_reference, pitch_candidate),
            ("onsets", onset_reference, onset_candidate),
        ];
        for (name, reference, candidate) in cases {
            let reference_path = directory.join(format!("{name}-ref.wav"));
            let candidate_path = directory.join(format!("{name}-cand.wav"));
            let write = |path: &Path, buffer: &AudioBuffer| {
                let interleaved: Vec<f32> = (0..buffer.frames())
                    .flat_map(|frame| {
                        let sample = buffer.channels[0][frame] as f32;
                        [sample, sample]
                    })
                    .collect();
                crate::wav::write_stereo_wav(path, &interleaved, buffer.sample_rate).unwrap();
            };
            write(&reference_path, &reference);
            write(&candidate_path, &candidate);
            let json_path = directory.join(format!("{name}.json"));
            let output = std::process::Command::new("python3")
                .arg(&script)
                .arg(&reference_path)
                .arg(&candidate_path)
                .arg("--stems")
                .arg("--json")
                .arg(&json_path)
                .output()
                .expect("run compare.py");
            assert!(
                output.status.success(),
                "compare.py failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let python: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
            let python_score = python["score"]["value"].as_f64().unwrap();
            let python_legacy = python["score"]["legacy_value"].as_f64().unwrap();
            let rust = compare_wavs(
                &reference_path,
                &candidate_path,
                CompareOptions {
                    stems: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let json = serde_json::to_value(&rust).unwrap();
            for key in ["dtw", "pitch", "score", "transient", "warnings"] {
                assert!(json.get(key).is_some(), "{name}: JSON lost {key}");
            }
            for key in [
                "mean_abs_warp_ms",
                "max_abs_warp_ms",
                "median_signed_warp_ms",
                "path_cost",
                "band_ms",
                "hop_ms",
                "frames",
                "note",
            ] {
                assert!(
                    json["dtw"].get(key).is_some(),
                    "{name}: dtw JSON lost {key}"
                );
            }
            for key in [
                "f0_range_hz",
                "status",
                "reason",
                "confidence",
                "median_cents_error",
                "fraction_within_25_cents",
            ] {
                assert!(
                    json["pitch"].get(key).is_some(),
                    "{name}: pitch JSON lost {key}"
                );
            }
            assert!(
                (rust.score.value - python_score).abs() <= 2.0,
                "{name}: rust {:.3} vs python {:.3}",
                rust.score.value,
                python_score
            );
            assert!(
                (rust.score.legacy_value - python_legacy).abs() <= 1.0,
                "{name}: legacy rust {:.3} vs python {:.3}",
                rust.score.legacy_value,
                python_legacy
            );
            let python_previous = python["score"]["previous_value"].as_f64().unwrap();
            assert!(
                (rust.score.previous_value - python_previous).abs() <= 2.0,
                "{name}: previous rust {:.3} vs python {:.3}",
                rust.score.previous_value,
                python_previous
            );
            for key in ["third_octave", "mfcc", "lsd", "onset"] {
                assert!(json.get(key).is_some(), "{name}: JSON lost {key}");
            }

            let python_third = python["spectrum"]["bands"]["third_octave"]["delta_db"]
                .as_array()
                .unwrap();
            assert_eq!(
                rust.third_octave.delta_db.len(),
                python_third.len(),
                "{name}: third-octave band count"
            );
            let mut third_delta: f64 = 0.0;
            for (index, python_value) in python_third.iter().enumerate() {
                let python_value = python_value.as_f64().unwrap();
                let difference = (rust.third_octave.delta_db[index] - python_value).abs();
                third_delta = third_delta.max(difference);
                assert!(
                    difference <= 0.05,
                    "{name}: third-octave band {index} rust {} vs python {python_value}",
                    rust.third_octave.delta_db[index]
                );
            }

            let python_mfcc = &python["spectrum"]["mfcc"];
            for (label, rust_value, python_key) in [
                (
                    "mfcc mean cosine",
                    rust.mfcc.mean_cosine_distance,
                    "mean_cosine_distance",
                ),
                (
                    "mfcc vector cosine",
                    rust.mfcc.mean_vector_cosine_distance,
                    "mean_vector_cosine_distance",
                ),
            ] {
                let python_value = python_mfcc[python_key].as_f64();
                match (rust_value, python_value) {
                    (Some(rust_value), Some(python_value)) => assert!(
                        (rust_value - python_value).abs() <= 0.005,
                        "{name}: {label} rust {rust_value} vs python {python_value}"
                    ),
                    (None, None) => {}
                    _ => panic!("{name}: {label} presence"),
                }
            }

            let python_lsd = &python["spectrum"]["lsd"];
            for (label, rust_value, python_key) in [
                ("broadband", rust.lsd.broadband_db, "broadband_db"),
                (
                    "level-matched",
                    rust.lsd.level_matched_db,
                    "level_matched_db",
                ),
            ] {
                let python_value = python_lsd[python_key].as_f64().unwrap();
                assert!(
                    (rust_value - python_value).abs() <= 0.1,
                    "{name}: lsd {label} rust {rust_value} vs python {python_value}"
                );
            }

            let python_onset = &python["time"]["onset"];
            let onset = &rust.onset;
            assert!(
                onset
                    .reference_count
                    .abs_diff(python_onset["reference_count"].as_u64().unwrap() as usize)
                    <= 1,
                "{name}: onset reference count"
            );
            assert!(
                onset
                    .candidate_count
                    .abs_diff(python_onset["candidate_count"].as_u64().unwrap() as usize)
                    <= 1,
                "{name}: onset candidate count"
            );
            assert!(
                (onset.best_correlation - python_onset["best_correlation"].as_f64().unwrap()).abs()
                    <= 0.005,
                "{name}: onset correlation rust {} vs python {}",
                onset.best_correlation,
                python_onset["best_correlation"]
            );
            assert!(
                (onset.best_lag_ms - python_onset["best_lag_ms"].as_f64().unwrap()).abs() <= 5.0,
                "{name}: onset lag rust {} vs python {}",
                onset.best_lag_ms,
                python_onset["best_lag_ms"]
            );
            assert!(
                (onset.reference_density_per_s
                    - python_onset["reference_density_per_s"].as_f64().unwrap())
                .abs()
                    <= 0.01,
                "{name}: reference density"
            );
            match (
                onset.density_ratio_db,
                python_onset["density_ratio_db"].as_f64(),
            ) {
                (Some(rust_value), Some(python_value)) => assert!(
                    (rust_value - python_value).abs() <= 0.1,
                    "{name}: density ratio rust {rust_value} vs python {python_value}"
                ),
                (None, None) => {}
                _ => panic!("{name}: density ratio presence"),
            }
            assert!(
                onset
                    .paired_count
                    .abs_diff(python_onset["paired_count"].as_u64().unwrap() as usize)
                    <= 1,
                "{name}: onset paired count"
            );
            match (
                onset.onset_strength_median_db,
                python_onset["onset_strength_median_db"].as_f64(),
            ) {
                (Some(rust_value), Some(python_value)) => assert!(
                    (rust_value - python_value).abs() <= 0.1,
                    "{name}: onset strength rust {rust_value} vs python {python_value}"
                ),
                (None, None) => {}
                _ => panic!("{name}: onset strength presence"),
            }

            let python_tilt = &python["tilt"];
            for (label, rust_value, python_key) in [
                (
                    "sub-gate tilt",
                    rust.tilt.sub_gate_tilt_db,
                    "sub_gate_tilt_db",
                ),
                (
                    "sub-gate low mean",
                    rust.tilt.sub_gate_low_mean_delta_db,
                    "sub_gate_low_mean_delta_db",
                ),
                (
                    "sub-gate high mean",
                    rust.tilt.sub_gate_high_mean_delta_db,
                    "sub_gate_high_mean_delta_db",
                ),
            ] {
                let python_value = python_tilt[python_key].as_f64();
                match (rust_value, python_value) {
                    (Some(rust_value), Some(python_value)) => assert!(
                        (rust_value - python_value).abs() <= 0.05,
                        "{name}: {label} rust {rust_value} vs python {python_value}"
                    ),
                    (None, None) => {}
                    _ => panic!("{name}: {label} presence"),
                }
            }

            for (label, rust_term, component) in [
                ("mfcc", rust.score.mfcc.term, "mfcc"),
                ("third octave", rust.score.third_octave.term, "third_octave"),
                ("lsd", rust.score.lsd.term, "lsd"),
                ("onset", rust.score.onset.term, "onset"),
                ("density", rust.score.density.term, "density"),
            ] {
                let python_value = python["score"]["components"][component]["term"].as_f64();
                match (rust_term, python_value) {
                    (Some(rust_value), Some(python_value)) => assert!(
                        (rust_value - python_value).abs() <= 0.02,
                        "{name}: {label} term rust {rust_value} vs python {python_value}"
                    ),
                    (None, None) => {}
                    _ => panic!("{name}: {label} term presence"),
                }
            }
            eprintln!(
                "parity {name}: third max Δ {:.3} dB; mfcc {:?}/{:?}; lsd b {:.3}/{:.3} lm {:.3}/{:.3}; onset {}/{} corr {:.3}/{:.3} density {:?}/{:?} strength {:?}/{:?}; sub-gate {:?}/{:?}; previous {:.1}/{:.1}",
                third_delta,
                rust.mfcc.mean_cosine_distance,
                python["spectrum"]["mfcc"]["mean_cosine_distance"],
                rust.lsd.broadband_db,
                python_lsd["broadband_db"],
                rust.lsd.level_matched_db,
                python_lsd["level_matched_db"],
                rust.onset.reference_count,
                python_onset["reference_count"],
                rust.onset.best_correlation,
                python_onset["best_correlation"],
                rust.onset.density_ratio_db,
                python_onset["density_ratio_db"],
                rust.onset.onset_strength_median_db,
                python_onset["onset_strength_median_db"],
                rust.tilt.sub_gate_tilt_db,
                python_tilt["sub_gate_tilt_db"],
                rust.score.previous_value,
                python_previous,
            );

            let python_dtw = &python["time"]["dtw"];
            let dtw = &rust.dtw;
            let python_frames = python_dtw["frames"].as_u64().unwrap() as usize;
            assert!(
                dtw.frames.abs_diff(python_frames) <= 1,
                "{name}: dtw frames rust {} vs python {python_frames}",
                dtw.frames
            );
            assert!(
                (dtw.band_ms - python_dtw["band_ms"].as_f64().unwrap()).abs() <= 0.05,
                "{name}: dtw band {} vs {}",
                dtw.band_ms,
                python_dtw["band_ms"]
            );
            assert!(
                (dtw.hop_ms - python_dtw["hop_ms"].as_f64().unwrap()).abs() <= 0.01,
                "{name}: dtw hop {} vs {}",
                dtw.hop_ms,
                python_dtw["hop_ms"]
            );
            for (label, rust_value, python_value) in [
                (
                    "mean_abs_warp_ms",
                    dtw.mean_abs_warp_ms,
                    python_dtw["mean_abs_warp_ms"].as_f64(),
                ),
                (
                    "max_abs_warp_ms",
                    dtw.max_abs_warp_ms,
                    python_dtw["max_abs_warp_ms"].as_f64(),
                ),
                (
                    "median_signed_warp_ms",
                    dtw.median_signed_warp_ms,
                    python_dtw["median_signed_warp_ms"].as_f64(),
                ),
            ] {
                match (rust_value, python_value) {
                    (Some(rust_value), Some(python_value)) => assert!(
                        (rust_value - python_value).abs() <= 0.5,
                        "{name}: dtw {label} rust {rust_value} vs python {python_value}"
                    ),
                    (None, None) => {}
                    _ => panic!(
                        "{name}: dtw {label} presence rust {rust_value:?} python {python_value:?}"
                    ),
                }
            }
            match (dtw.path_cost, python_dtw["path_cost"].as_f64()) {
                (Some(rust_value), Some(python_value)) => assert!(
                    (rust_value - python_value).abs() <= 0.01_f64.max(python_value.abs() * 0.001),
                    "{name}: dtw path_cost rust {rust_value} vs python {python_value}"
                ),
                (None, None) => {}
                _ => panic!("{name}: dtw path_cost presence"),
            }

            let python_pitch = &python["pitch"];
            let pitch = &rust.pitch;
            assert_eq!(
                pitch.status,
                python_pitch["status"].as_str().unwrap(),
                "{name}: pitch status"
            );
            assert_eq!(
                pitch.reference_frames,
                python_pitch["reference_frames"].as_u64().unwrap() as usize,
                "{name}: pitch reference frames"
            );
            for (label, rust_value, python_key) in [
                (
                    "reference voiced frames",
                    pitch.reference_voiced_frames,
                    "reference_voiced_frames",
                ),
                (
                    "candidate voiced frames",
                    pitch.candidate_voiced_frames,
                    "candidate_voiced_frames",
                ),
                ("compared frames", pitch.compared_frames, "compared_frames"),
            ] {
                let python_value = python_pitch[python_key].as_u64().unwrap() as usize;
                assert!(
                    rust_value.abs_diff(python_value) <= 1,
                    "{name}: pitch {label} rust {rust_value} vs python {python_value}"
                );
            }
            match (
                pitch.median_cents_error,
                python_pitch["median_cents_error"].as_f64(),
            ) {
                (Some(cents), Some(python_cents)) => {
                    assert!(
                        (cents - python_cents).abs() <= 1.0,
                        "{name}: pitch median cents rust {cents} vs python {python_cents}"
                    );
                    let absolute = pitch.median_abs_cents_error.unwrap();
                    let python_absolute = python_pitch["median_abs_cents_error"].as_f64().unwrap();
                    assert!(
                        (absolute - python_absolute).abs() <= 1.0,
                        "{name}: pitch median |cents| rust {absolute} vs python {python_absolute}"
                    );
                    let p90 = pitch.p90_abs_cents_error.unwrap();
                    let python_p90 = python_pitch["p90_abs_cents_error"].as_f64().unwrap();
                    assert!(
                        (p90 - python_p90).abs() <= 1.0,
                        "{name}: pitch p90 rust {p90} vs python {python_p90}"
                    );
                    let fraction = pitch.fraction_within_25_cents.unwrap();
                    let python_fraction =
                        python_pitch["fraction_within_25_cents"].as_f64().unwrap();
                    assert!(
                        (fraction - python_fraction).abs() <= 0.01,
                        "{name}: pitch fraction rust {fraction} vs python {python_fraction}"
                    );
                }
                (None, None) => {
                    assert!(pitch.median_abs_cents_error.is_none());
                    assert!(pitch.fraction_within_25_cents.is_none());
                }
                _ => panic!("{name}: pitch cents presence"),
            }
            if name == "pitch" {
                assert_eq!(pitch.status, "ok", "{name}: pitch should be applicable");
                let cents = pitch.median_cents_error.unwrap();
                assert!((cents - 18.0).abs() <= 1.5, "{name}: {cents} cents");
            }
            if name == "onsets" {
                assert_eq!(rust.onset.reference_count, 5, "{name}: reference onsets");
                assert_eq!(rust.onset.candidate_count, 5, "{name}: candidate onsets");
                let strength = rust.onset.onset_strength_median_db.unwrap();
                assert!(
                    (-10.0..-1.0).contains(&strength),
                    "{name}: per-attack strength {strength} dB"
                );
                assert!(
                    rust.onset.density_ratio_db.unwrap().abs() < 0.01,
                    "{name}: density ratio"
                );
            }
            eprintln!(
                "parity {name}: dtw frames {}/{} band {:.1} ms mean {:.2}/{:?} max {:.2}/{:?} median {:?}/{:?} cost {:.3}/{:?}; pitch {} frames {}/{}, voiced {}/{}, cents {:?}/{:?}",
                dtw.frames,
                python_frames,
                dtw.band_ms,
                dtw.mean_abs_warp_ms.unwrap_or(f64::NAN),
                python_dtw["mean_abs_warp_ms"],
                dtw.max_abs_warp_ms.unwrap_or(f64::NAN),
                python_dtw["max_abs_warp_ms"],
                dtw.median_signed_warp_ms,
                python_dtw["median_signed_warp_ms"],
                dtw.path_cost.unwrap_or(f64::NAN),
                python_dtw["path_cost"],
                pitch.status,
                pitch.compared_frames,
                python_pitch["compared_frames"],
                pitch.reference_voiced_frames,
                python_pitch["reference_voiced_frames"],
                pitch.median_cents_error,
                python_pitch["median_cents_error"],
            );
        }
        let _ = std::fs::remove_dir_all(&directory);
    }
}
