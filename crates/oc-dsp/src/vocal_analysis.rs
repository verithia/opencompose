//! Offline monophonic pitch analysis: decimated FFT autocorrelation, a global
//! candidate decode, median smoothing and note segmentation.
//!
//! The analyzer owns no files and no caches; `oc-engine::vocal_analysis` keys
//! the result on media identity. Everything here is deterministic for a given
//! input buffer and parameter set.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::tuner::Decimator;

/// Bump when a change to this module can move any output bit.
pub const ANALYZER_VERSION: u32 = 6;

/// Share of the analysis window a new note must fill before a frame flips from
/// unvoiced to voiced, and the same share for the last voiced frame.
///
/// The flip threshold is low because the difference function normalises by its
/// own cumulative mean: a window that is mostly silence still reads as periodic
/// once a few periods of the new note are inside it.
const EDGE_FLIP_FRACTION: f64 = 0.05;

/// Half-width of the pitch window searched around a candidate cut, in seconds.
///
/// A transition is measured inside this window, so a cut can move by at most
/// this much before the clamp below.
const INFLECTION_WINDOW_SECONDS: f64 = 0.080;

/// A transition slower than this is a scoop, not a note change.
///
/// The peak slope of an exponential glide in cents per second separates a step
/// (a semitone inside 100 ms or faster) from a portamento that should keep the
/// frame the band crossing chose.
const INFLECTION_MIN_SLOPE_CENTS_PER_SECOND: f64 = 1_000.0;

/// Largest distance a cut may move from the frame the band crossing chose.
const INFLECTION_MAX_SHIFT_SECONDS: f64 = 0.060;

/// One analysis frame. `f0_hz` and `confidence` are zero when unvoiced.
#[derive(Debug, Clone, PartialEq)]
pub struct VocalFrame {
    /// Frame centre in seconds from the start of the analysed buffer.
    pub time_s: f64,
    pub f0_hz: f64,
    pub confidence: f64,
    pub voiced: bool,
}

/// One segmented note. Times are seconds from the start of the analysed buffer.
///
/// The reported span (`start_s`, `end_s`) sits on the analysis frame grid: it
/// is the timing the project, the note grid and the tuner's window read. The
/// detected edges (`onset_s`, `offset_s`) carry the event itself, which the
/// frame grid cannot express because a frame flips between voiced and unvoiced
/// part way through a note's attack or release.
#[derive(Debug, Clone, PartialEq)]
pub struct VocalNote {
    /// First frame of the note on the frame grid.
    pub start_s: f64,
    /// One hop past the last frame of the note, clamped to the buffer.
    pub end_s: f64,
    /// Detected start of the note: `start_s` moved onto the attack when the
    /// note opens out of an unvoiced run, else `start_s`.
    pub onset_s: f64,
    /// Detected end of the note: `end_s` moved onto the release when the note
    /// closes into an unvoiced run, else `end_s`.
    pub offset_s: f64,
    pub f0_median_hz: f64,
    pub confidence: f64,
    pub voiced_ratio: f64,
}

/// Analysis result at the detector's internal rate plus the note list.
#[derive(Debug, Clone, PartialEq)]
pub struct VocalAnalysis {
    /// Internal rate after decimation (about 16 kHz, or a division of it).
    pub analysis_rate_hz: f64,
    pub hop_seconds: f64,
    /// Length of the analysed buffer in seconds.
    pub duration_s: f64,
    pub frames: Vec<VocalFrame>,
    pub notes: Vec<VocalNote>,
}

impl VocalAnalysis {
    fn empty(analysis_rate_hz: f64, hop_seconds: f64, duration_s: f64) -> Self {
        Self {
            analysis_rate_hz,
            hop_seconds,
            duration_s,
            frames: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// All detector knobs. Defaults are the measured values documented in
/// `docs/vocal-analysis.md`; every field feeds the cache key through `stamp`.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorParams {
    pub f0_min_hz: f64,
    pub f0_max_hz: f64,
    pub hop_seconds: f64,
    pub min_window_seconds: f64,
    pub window_cycles: f64,
    /// A lag is a candidate when the normalised difference is below this.
    pub candidate_threshold: f64,
    pub max_candidates: usize,
    /// Window RMS below this is silence.
    pub silence_rms: f64,
    /// Window RMS at which the voicing factor reaches one.
    pub full_rms: f64,
    pub median_frames: usize,
    /// A period this far above the local reference is an octave-up candidate.
    pub octave_up_ratio: f64,
    /// A period this far below the local reference is an octave-down candidate.
    pub octave_down_ratio: f64,
    /// The halved/doubled period must land this close to the reference.
    pub octave_tolerance: f64,
    pub octave_repair_neighbors: usize,
    /// A longest-period candidate whose half lag is within this of its own
    /// cumulative mean is the octave-down alias of a real fundamental.
    pub subharmonic_margin: f64,
    /// A note holds while the frame is within this of the segment median.
    pub split_cents: f64,
    /// Out-of-band frames a pitch change must sustain before it is a split.
    pub split_frames: usize,
    /// Out-of-band frames that arm a fast cut inside a short note. Zero keeps
    /// the full `split_frames` hold.
    pub fast_cut_frames: usize,
    /// Pitch distance across the candidate boundary a fast cut must measure.
    pub fast_cut_cents: f64,
    /// Trailing excursion frames that must also sit past the distance, so an
    /// excursion already returning to the band is not a cut.
    pub fast_cut_confirm_frames: usize,
    pub onset_rise_db: f64,
    pub onset_window_seconds: f64,
    /// Segments shorter than this are absorbed by a neighbour.
    pub min_note_seconds: f64,
    /// Unvoiced gaps up to this many frames can stay inside one note.
    pub bridge_gap_frames: usize,
    /// Dynamic-programming costs.
    pub jump_cost_per_cent: f64,
    pub octave_cost: f64,
    pub unvoiced_cost: f64,
    pub confidence_cost: f64,
    /// A jump this close to 1200 cents pays the octave cost. Half-width.
    pub octave_window_cents: f64,
}

impl Default for DetectorParams {
    fn default() -> Self {
        Self {
            f0_min_hz: 65.0,
            f0_max_hz: 1_200.0,
            hop_seconds: 0.005,
            min_window_seconds: 0.040,
            window_cycles: 3.0,
            candidate_threshold: 0.5,
            max_candidates: 3,
            silence_rms: 1e-4,
            full_rms: 1e-3,
            median_frames: 3,
            octave_up_ratio: 1.75,
            octave_down_ratio: 0.57,
            octave_tolerance: 0.20,
            octave_repair_neighbors: 2,
            subharmonic_margin: 0.05,
            split_cents: 60.0,
            split_frames: 30,
            fast_cut_frames: 25,
            fast_cut_cents: 120.0,
            fast_cut_confirm_frames: 6,
            onset_rise_db: 4.0,
            onset_window_seconds: 0.010,
            min_note_seconds: 0.050,
            bridge_gap_frames: 3,
            jump_cost_per_cent: 0.002,
            octave_cost: 2.0,
            unvoiced_cost: 0.6,
            confidence_cost: 1.0,
            octave_window_cents: 100.0,
        }
    }
}

impl DetectorParams {
    /// Canonical string for the cache key; floats are compared by bits.
    pub fn stamp(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.f0_min_hz.to_bits(),
            self.f0_max_hz.to_bits(),
            self.hop_seconds.to_bits(),
            self.min_window_seconds.to_bits(),
            self.window_cycles.to_bits(),
            self.candidate_threshold.to_bits(),
            self.max_candidates,
            self.silence_rms.to_bits(),
            self.full_rms.to_bits(),
            self.median_frames,
            self.octave_up_ratio.to_bits(),
            self.octave_down_ratio.to_bits(),
            self.octave_tolerance.to_bits(),
            self.octave_repair_neighbors,
            self.split_cents.to_bits(),
            self.split_frames,
            self.fast_cut_frames,
            self.fast_cut_cents.to_bits(),
            self.fast_cut_confirm_frames,
            self.onset_rise_db.to_bits(),
            self.onset_window_seconds.to_bits(),
            self.min_note_seconds.to_bits(),
            self.bridge_gap_frames,
            self.jump_cost_per_cent.to_bits(),
            self.octave_cost.to_bits(),
            self.unvoiced_cost.to_bits(),
            self.confidence_cost.to_bits(),
            self.octave_window_cents.to_bits(),
            self.subharmonic_margin.to_bits(),
        )
    }

    /// Stable 64-bit hash of `stamp`, used in analysis headers.
    pub fn hash(&self) -> u64 {
        fnv1a(self.stamp().as_bytes())
    }

    fn validate(&self) -> Result<(), String> {
        let finite = [
            self.f0_min_hz,
            self.f0_max_hz,
            self.hop_seconds,
            self.min_window_seconds,
            self.window_cycles,
            self.candidate_threshold,
            self.silence_rms,
            self.full_rms,
            self.split_cents,
            self.fast_cut_cents,
            self.onset_rise_db,
            self.onset_window_seconds,
            self.min_note_seconds,
            self.jump_cost_per_cent,
            self.octave_cost,
            self.unvoiced_cost,
            self.confidence_cost,
            self.octave_window_cents,
            self.subharmonic_margin,
        ]
        .iter()
        .all(|value| value.is_finite());
        if !finite {
            return Err("detector parameters must be finite".into());
        }
        if self.f0_min_hz <= 0.0 || self.f0_max_hz <= self.f0_min_hz {
            return Err("detector f0 range must be positive and ordered".into());
        }
        if self.hop_seconds <= 0.0 || self.min_window_seconds <= 0.0 || self.window_cycles <= 0.0 {
            return Err("detector windows and hop must be positive".into());
        }
        if self.max_candidates == 0 || self.max_candidates > 8 {
            return Err("detector candidate count must be 1..=8".into());
        }
        if self.median_frames < 1
            || (self.median_frames > 1 && self.median_frames.is_multiple_of(2))
        {
            return Err("detector median width must be odd".into());
        }
        if self.split_frames == 0 || self.split_cents <= 0.0 || self.min_note_seconds < 0.0 {
            return Err("detector segmentation bounds are invalid".into());
        }
        if self.fast_cut_frames > self.split_frames
            || (self.fast_cut_frames > 0
                && (self.fast_cut_cents <= 0.0
                    || self.fast_cut_confirm_frames == 0
                    || self.fast_cut_confirm_frames > self.fast_cut_frames))
        {
            return Err("detector fast-cut bounds are invalid".into());
        }
        if self.onset_window_seconds <= 0.0 || self.onset_rise_db <= 0.0 {
            return Err("detector onset bounds are invalid".into());
        }
        if self.candidate_threshold <= 0.0 || self.candidate_threshold >= 1.0 {
            return Err("detector candidate threshold must be inside (0, 1)".into());
        }
        if self.full_rms <= self.silence_rms || self.silence_rms < 0.0 {
            return Err("detector RMS gate must be ordered".into());
        }
        if !(0.0..1.0).contains(&self.subharmonic_margin) {
            return Err("detector subharmonic margin must be inside [0, 1)".into());
        }
        if self.jump_cost_per_cent < 0.0
            || self.octave_cost < 0.0
            || self.unvoiced_cost < 0.0
            || self.confidence_cost < 0.0
        {
            return Err("detector costs must be non-negative".into());
        }
        Ok(())
    }
}

/// Analyse one mono playback buffer at `sample_rate`.
pub fn analyze(
    samples: &[f32],
    sample_rate: u32,
    params: &DetectorParams,
) -> Result<VocalAnalysis, String> {
    params.validate()?;
    if sample_rate == 0 {
        return Err("sample rate must be positive".into());
    }
    let duration_s = samples.len() as f64 / f64::from(sample_rate);

    let mut decimator = Decimator::new(sample_rate);
    let mut decimated = Vec::with_capacity(samples.len());
    for &sample in samples {
        if let Some(value) = decimator.push(f64::from(sample)) {
            decimated.push(value);
        }
    }
    let rate = decimator.rate();
    let hop = (rate * params.hop_seconds).round().max(1.0) as usize;
    let hop_seconds = hop as f64 / rate;

    let tau_min = ((rate / params.f0_max_hz).floor() as usize).max(2);
    let tau_max = ((rate / params.f0_min_hz).ceil() as usize) + 1;
    let window = ((params.window_cycles / params.f0_min_hz) * rate)
        .max(params.min_window_seconds * rate)
        .round()
        .max(16.0) as usize;
    if decimated.len() < window + tau_max || tau_min >= tau_max {
        return Ok(VocalAnalysis::empty(rate, hop_seconds, duration_s));
    }

    let frame_count = (decimated.len() - (window + tau_max)) / hop + 1;
    let nfft = (window + 2 * tau_max + 1).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(nfft);
    let inverse = planner.plan_fft_inverse(nfft);
    let mut spectrum = vec![Complex::default(); nfft];
    let mut squares = vec![0.0_f64; window + tau_max + 1];
    let mut difference = vec![0.0_f64; tau_max + 1];
    let mut normalised = vec![1.0_f64; tau_max + 1];
    let mut candidates = Vec::with_capacity(params.max_candidates);

    let onset_window = (rate * params.onset_window_seconds).round().max(1.0) as usize;
    let mut frames = Vec::with_capacity(frame_count);
    let mut onsets = Vec::with_capacity(frame_count);

    for index in 0..frame_count {
        let start = index * hop;
        let end = start + window + tau_max;
        let frame = &decimated[start..end];

        squares[0] = 0.0;
        for (offset, value) in frame.iter().enumerate() {
            squares[offset + 1] = squares[offset] + value * value;
        }
        let energy = squares[window];
        let rms = (energy / window as f64).sqrt();
        onsets.push(onset_at(
            &decimated,
            start + window / 2,
            onset_window,
            params,
        ));

        candidates.clear();
        if rms >= params.silence_rms {
            for (slot, value) in spectrum.iter_mut().enumerate() {
                *value = if slot < frame.len() {
                    Complex {
                        re: frame[slot],
                        im: 0.0,
                    }
                } else {
                    Complex::default()
                };
            }
            forward.process(&mut spectrum);
            for value in &mut spectrum {
                *value = Complex {
                    re: value.re * value.re + value.im * value.im,
                    im: 0.0,
                };
            }
            inverse.process(&mut spectrum);
            let scale = 1.0 / nfft as f64;
            difference[0] = 0.0;
            for tau in 1..=tau_max {
                // The FFT gives the full-frame autocorrelation; the YIN
                // difference needs the first-window correlation, so remove
                // the tail terms beyond the window.
                let mut correlation = spectrum[tau].re * scale;
                let end = window + tau_max - tau;
                for index in window..end {
                    correlation -= frame[index] * frame[index + tau];
                }
                let trailing = squares[tau + window] - squares[tau];
                difference[tau] = (energy + trailing - 2.0 * correlation).max(0.0);
            }
            normalised[0] = 1.0;
            let mut running = 0.0;
            for tau in 1..=tau_max {
                running += difference[tau];
                normalised[tau] = if running > 0.0 {
                    (difference[tau] * tau as f64 / running).min(1.0)
                } else {
                    1.0
                };
            }
            let voicing = ((rms - params.silence_rms) / (params.full_rms - params.silence_rms))
                .clamp(0.0, 1.0);
            collect_candidates(
                &normalised,
                &difference,
                tau_min,
                tau_max,
                params.candidate_threshold,
                params.subharmonic_margin,
                params.max_candidates,
                rate,
                voicing,
                &mut candidates,
            );
        }
        frames.push(candidates.clone());
    }

    let decoded = decode(&frames, params);
    let mut output: Vec<VocalFrame> = decoded
        .into_iter()
        .enumerate()
        .map(|(index, state)| match state {
            Some((f0, confidence)) => VocalFrame {
                time_s: (index * hop) as f64 / rate + window as f64 * 0.5 / rate,
                f0_hz: f0,
                confidence,
                voiced: true,
            },
            None => VocalFrame {
                time_s: (index * hop) as f64 / rate + window as f64 * 0.5 / rate,
                f0_hz: 0.0,
                confidence: 0.0,
                voiced: false,
            },
        })
        .collect();
    median_filter(&mut output, params.median_frames);
    repair_octaves(&mut output, params);
    let notes = segment_notes(
        &output,
        &onsets,
        hop_seconds,
        duration_s,
        window as f64 / rate,
        params,
    );
    Ok(VocalAnalysis {
        analysis_rate_hz: rate,
        hop_seconds,
        duration_s,
        frames: output,
        notes,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    normalised: &[f64],
    difference: &[f64],
    tau_min: usize,
    tau_max: usize,
    threshold: f64,
    margin: f64,
    limit: usize,
    rate: f64,
    voicing: f64,
    out: &mut Vec<(f64, f64)>,
) {
    if tau_max <= tau_min + 1 {
        return;
    }
    let mut accepted: Vec<(f64, f64)> = Vec::with_capacity(limit);
    let mut aliases: Vec<bool> = Vec::with_capacity(limit);
    for tau in tau_min..tau_max {
        if normalised[tau] >= threshold
            || normalised[tau] >= normalised[tau - 1]
            || normalised[tau] >= normalised[tau + 1]
        {
            continue;
        }
        let period = parabolic_dip(difference, tau, tau_min, tau_max);
        if !period.is_finite() || period <= 0.0 {
            continue;
        }
        // A dip at an integer multiple of an earlier, equally good dip is the
        // same periodicity, not an alternative f0. Keeping it would let a
        // subharmonic win on round-off alone; a clearly better multiple (the
        // octave-down repair case) is still kept.
        let multiple = accepted.iter().any(|(earlier, value)| {
            let ratio = period / earlier;
            let nearest = ratio.round();
            nearest >= 2.0 && (ratio - nearest).abs() <= 0.03 && normalised[tau] >= *value - 0.05
        });
        if multiple {
            continue;
        }
        let f0 = rate / period;
        if !f0.is_finite() || f0 < 1.0 {
            continue;
        }
        let confidence = (1.0 - normalised[tau]).clamp(0.0, 1.0) * voicing;
        accepted.push((period, normalised[tau]));
        aliases.push(prefers_half_lag(
            parabolic_value(normalised, tau, tau_min, tau_max),
            half_lag_value(normalised, period, tau_min, tau_max),
            margin,
        ));
        out.push((f0, confidence));
        if out.len() >= limit {
            break;
        }
    }
    // Candidates run from the shortest period to the longest; only the
    // longest-period hypotheses (the ones an octave-down lock would use) are
    // removed, so a frame whose lowest candidate is the fundamental keeps its
    // set and path.
    while aliases.last() == Some(&true) {
        aliases.pop();
        out.pop();
    }
}

fn parabolic_dip(values: &[f64], dip: usize, low: usize, high: usize) -> f64 {
    if dip <= low || dip >= high {
        return dip as f64;
    }
    let left = values[dip - 1];
    let center = values[dip];
    let right = values[dip + 1];
    let denominator = left - 2.0 * center + right;
    if denominator.abs() < 1e-12 {
        return dip as f64;
    }
    let offset = (0.5 * (left - right) / denominator).clamp(-0.5, 0.5);
    dip as f64 + offset
}

/// Value of the parabola through a dip's three samples at the dip.
fn parabolic_value(values: &[f64], dip: usize, low: usize, high: usize) -> f64 {
    if dip <= low || dip >= high {
        return values[dip];
    }
    let left = values[dip - 1];
    let centre = values[dip];
    let right = values[dip + 1];
    let denominator = left - 2.0 * centre + right;
    if denominator.abs() < 1e-12 {
        return centre;
    }
    let offset = (0.5 * (left - right) / denominator).clamp(-0.5, 0.5);
    centre - 0.25 * (left - right) * offset
}

/// Cumulative mean at half the candidate's lag, or infinity when out of range.
fn half_lag_value(values: &[f64], period: f64, tau_min: usize, tau_max: usize) -> f64 {
    let half = 0.5 * period;
    if half < tau_min as f64 || half > tau_max as f64 {
        return f64::INFINITY;
    }
    let nearest = (half.round() as usize).clamp(tau_min, tau_max);
    parabolic_value(values, nearest, tau_min, tau_max)
}

/// The octave-down hypothesis: the cumulative mean at half the lag is within
/// `margin` of the value at this lag.
fn prefers_half_lag(norm: f64, half: f64, margin: f64) -> bool {
    half <= norm + margin
}

fn onset_at(decimated: &[f64], centre: usize, width: usize, params: &DetectorParams) -> bool {
    if centre < width || centre + width > decimated.len() {
        return false;
    }
    let mut before = 0.0;
    let mut after = 0.0;
    for value in &decimated[centre - width..centre] {
        before += value * value;
    }
    for value in &decimated[centre..centre + width] {
        after += value * value;
    }
    let before = (before / width as f64).max(params.silence_rms * params.silence_rms);
    let after = after / width as f64;
    if after <= params.silence_rms * params.silence_rms {
        return false;
    }
    10.0 * (after / before).log10() > params.onset_rise_db
}

/// Global decode over candidate states plus unvoiced. Returns one state per frame.
fn decode(frames: &[Vec<(f64, f64)>], params: &DetectorParams) -> Vec<Option<(f64, f64)>> {
    const MAX_STATES: usize = 9;
    let frame_count = frames.len();
    if frame_count == 0 {
        return Vec::new();
    }
    let unvoiced = params.max_candidates;
    let mut cost = vec![[f64::INFINITY; MAX_STATES]; frame_count];
    let mut back = vec![[usize::MAX; MAX_STATES]; frame_count];

    for (state, candidate) in frames[0].iter().enumerate() {
        cost[0][state] = params.confidence_cost * (1.0 - candidate.1);
    }
    cost[0][unvoiced] = params.unvoiced_cost;

    for index in 1..frame_count {
        let previous = &frames[index - 1];
        let current = &frames[index];
        for (state, candidate) in current.iter().enumerate() {
            let emission = params.confidence_cost * (1.0 - candidate.1);
            for (previous_state, previous_candidate) in previous.iter().enumerate() {
                let base = cost[index - 1][previous_state];
                if !base.is_finite() {
                    continue;
                }
                let total = base + transition_cost(previous_candidate.0, candidate.0, params);
                if total < cost[index][state] {
                    cost[index][state] = total;
                    back[index][state] = previous_state;
                }
            }
            let base = cost[index - 1][unvoiced];
            if base.is_finite() && base < cost[index][state] {
                cost[index][state] = base;
                back[index][state] = unvoiced;
            }
            if cost[index][state].is_finite() {
                cost[index][state] += emission;
            }
        }
        let mut best = cost[index - 1][unvoiced];
        let mut best_previous = unvoiced;
        for (previous_state, _) in previous.iter().enumerate() {
            let base = cost[index - 1][previous_state];
            if base.is_finite() && base < best {
                best = base;
                best_previous = previous_state;
            }
        }
        if best.is_finite() {
            cost[index][unvoiced] = best + params.unvoiced_cost;
            back[index][unvoiced] = best_previous;
        }
    }

    let mut state = (0..=unvoiced)
        .min_by(|left, right| {
            cost[frame_count - 1][*left].total_cmp(&cost[frame_count - 1][*right])
        })
        .unwrap_or(unvoiced);
    if !cost[frame_count - 1][state].is_finite() {
        state = unvoiced;
    }
    let mut decoded = vec![None; frame_count];
    for index in (0..frame_count).rev() {
        if state != unvoiced
            && let Some(candidate) = frames[index].get(state)
        {
            decoded[index] = Some(*candidate);
        }
        if index > 0 {
            state = back[index][state];
            if state == usize::MAX {
                state = unvoiced;
            }
        }
    }
    decoded
}

fn transition_cost(previous: f64, current: f64, params: &DetectorParams) -> f64 {
    if previous <= 0.0 || current <= 0.0 {
        return 0.0;
    }
    let cents = 1_200.0 * (current / previous).log2().abs();
    let mut cost = params.jump_cost_per_cent * cents;
    if (cents - 1_200.0).abs() <= params.octave_window_cents {
        cost += params.octave_cost;
    }
    cost
}

fn median_filter(frames: &mut [VocalFrame], width: usize) {
    if width < 3 || frames.is_empty() {
        return;
    }
    let half = width / 2;
    let original: Vec<f64> = frames.iter().map(|frame| frame.f0_hz).collect();
    let voiced: Vec<bool> = frames.iter().map(|frame| frame.voiced).collect();
    let mut values = Vec::with_capacity(width);
    for index in 0..frames.len() {
        if !voiced[index] {
            continue;
        }
        values.clear();
        for neighbour in index.saturating_sub(half)..=(index + half).min(frames.len() - 1) {
            if voiced[neighbour] {
                values.push(original[neighbour]);
            }
        }
        values.sort_by(|left, right| left.total_cmp(right));
        frames[index].f0_hz = median_sorted(&values);
    }
}

fn repair_octaves(frames: &mut [VocalFrame], params: &DetectorParams) {
    if frames.is_empty() || params.octave_repair_neighbors == 0 {
        return;
    }
    let original: Vec<f64> = frames.iter().map(|frame| frame.f0_hz).collect();
    let voiced: Vec<bool> = frames.iter().map(|frame| frame.voiced).collect();
    let reach = params.octave_repair_neighbors;
    let mut references = Vec::with_capacity(reach * 2);
    for index in 0..frames.len() {
        if !voiced[index] {
            continue;
        }
        references.clear();
        for neighbour in index.saturating_sub(reach)..=(index + reach).min(frames.len() - 1) {
            if neighbour != index && voiced[neighbour] {
                references.push(original[neighbour]);
            }
        }
        if references.len() < 2 {
            continue;
        }
        references.sort_by(|left, right| left.total_cmp(right));
        let reference = median_sorted(&references);
        if reference <= 0.0 {
            continue;
        }
        let ratio = original[index] / reference;
        if ratio > params.octave_up_ratio {
            let halved = original[index] * 0.5;
            if (halved / reference - 1.0).abs() < params.octave_tolerance {
                frames[index].f0_hz = halved;
            }
        } else if ratio < params.octave_down_ratio {
            let doubled = original[index] * 2.0;
            if (doubled / reference - 1.0).abs() < params.octave_tolerance {
                frames[index].f0_hz = doubled;
            }
        }
    }
}

fn segment_notes(
    frames: &[VocalFrame],
    onsets: &[bool],
    hop_seconds: f64,
    duration_s: f64,
    window_seconds: f64,
    params: &DetectorParams,
) -> Vec<VocalNote> {
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < frames.len() {
        if !frames[index].voiced {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index;
        let mut cursor = index + 1;
        while cursor < frames.len() {
            if frames[cursor].voiced {
                end = cursor;
                cursor += 1;
                continue;
            }
            let gap_start = cursor;
            while cursor < frames.len() && !frames[cursor].voiced {
                cursor += 1;
            }
            if cursor >= frames.len()
                || cursor - gap_start > params.bridge_gap_frames
                || !pitch_continuous(frames, end, cursor, params.split_cents)
            {
                break;
            }
            end = cursor;
            cursor += 1;
        }
        segment_run(frames, onsets, start, end, params, &mut segments);
        index = end + 1;
    }

    segments.retain(|(start, end)| (*start..=*end).any(|index| frames[index].voiced));
    let min_frames =
        merge_short_segments(frames, hop_seconds, window_seconds, params, &mut segments);
    refine_segment_boundaries(frames, hop_seconds, min_frames, &mut segments);

    let mut notes = Vec::with_capacity(segments.len());
    for (start, end) in segments {
        let mut values = Vec::new();
        let mut confidence = 0.0;
        let mut voiced = 0;
        for frame in &frames[start..=end] {
            if frame.voiced {
                values.push(frame.f0_hz.ln());
                confidence += frame.confidence;
                voiced += 1;
            }
        }
        if values.is_empty() {
            continue;
        }
        values.sort_by(|left, right| left.total_cmp(right));
        let median = median_sorted(&values).exp();
        let total = end - start + 1;
        let start_s = frames[start].time_s;
        let end_s = (frames[end].time_s + hop_seconds).min(duration_s);
        if end_s <= start_s {
            continue;
        }
        // A frame flips at the edge of a voiced run once the note fills
        // EDGE_FLIP_FRACTION of the window, so the frame centre sits half a
        // window minus that share away from the event: before it at a start,
        // after it at an end. Move the detected edges onto the event and leave
        // the reported span on the frame grid.
        let edge = (0.5 - EDGE_FLIP_FRACTION) * window_seconds;
        let onset_s = if start > 0 && !frames[start - 1].voiced {
            (start_s + edge).min(end_s)
        } else {
            start_s
        };
        let offset_s = if end + 1 < frames.len() && !frames[end + 1].voiced {
            (end_s - edge).max(onset_s)
        } else {
            end_s
        };
        notes.push(VocalNote {
            start_s,
            end_s,
            onset_s,
            offset_s,
            f0_median_hz: median,
            confidence: confidence / voiced as f64,
            voiced_ratio: voiced as f64 / total as f64,
        });
    }
    notes
}

/// Split one voiced run on forced onsets and on sustained pitch changes.
///
/// An excursion out of the band is a new note only when it stays out: vibrato
/// leaves the band for at most half a cycle, so a short excursion returns and
/// is folded back, while a real change is cut at the frame where it started.
/// Undecided frames are kept out of the reference median; otherwise a slow
/// drift drags the band along and the cut never happens.
fn segment_run(
    frames: &[VocalFrame],
    onsets: &[bool],
    start: usize,
    end: usize,
    params: &DetectorParams,
    out: &mut Vec<(usize, usize)>,
) {
    let mut segment_start = start;
    let mut median = RunningMedian::new();
    let mut violations = 0;
    let mut violation_start = start;
    let mut pending: Vec<f64> = Vec::new();
    for index in start..=end {
        if onsets[index] && index > segment_start {
            out.push((segment_start, index - 1));
            segment_start = index;
            median.clear();
            pending.clear();
            violations = 0;
        }
        if !frames[index].voiced {
            continue;
        }
        if median.is_empty() {
            median.push(frames[index].f0_hz.ln());
            continue;
        }
        let reference = median.median().exp();
        let cents = 1_200.0 * (frames[index].f0_hz / reference).log2();
        if cents.abs() > params.split_cents {
            if violations == 0 {
                violation_start = index;
                pending.clear();
            }
            violations += 1;
            pending.push(frames[index].f0_hz.ln());
            let measured = params.fast_cut_frames > 0
                && violations >= params.fast_cut_frames
                && measured_pitch_change(&pending, reference, params);
            if measured || violations >= params.split_frames {
                if violation_start > segment_start {
                    out.push((segment_start, violation_start - 1));
                    segment_start = violation_start;
                }
                median.clear();
                for value in pending.drain(..) {
                    median.push(value);
                }
                violations = 0;
            }
            continue;
        }
        violations = 0;
        for value in pending.drain(..) {
            median.push(value);
        }
        median.push(frames[index].f0_hz.ln());
    }
    out.push((segment_start, end));
}

/// True when the out-of-band frames measure a real pitch change: the whole
/// excursion and its trailing frames both sit at least `fast_cut_cents` from
/// the segment reference on the same side. A vibrato excursion is already
/// returning to the band when the fast hold expires, so its trailing frames
/// fail the test and the note keeps the full hold.
fn measured_pitch_change(pending: &[f64], reference: f64, params: &DetectorParams) -> bool {
    if pending.len() < params.fast_cut_frames || params.fast_cut_confirm_frames == 0 {
        return false;
    }
    let tail_len = params.fast_cut_confirm_frames.min(pending.len());
    let tail = &pending[pending.len() - tail_len..];
    let mut whole: Vec<f64> = pending.to_vec();
    let mut tail: Vec<f64> = tail.to_vec();
    whole.sort_by(|left, right| left.total_cmp(right));
    tail.sort_by(|left, right| left.total_cmp(right));
    let distance_cents =
        |values: &[f64]| 1_200.0 * (median_sorted(values).exp() / reference).log2();
    let whole_cents = distance_cents(&whole);
    let tail_cents = distance_cents(&tail);
    whole_cents.abs() >= params.fast_cut_cents
        && tail_cents.abs() >= params.fast_cut_cents
        && whole_cents.signum() == tail_cents.signum()
}

fn pitch_continuous(frames: &[VocalFrame], before: usize, after: usize, cents: f64) -> bool {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut cursor = before;
    while cursor > 0 && left.len() < 3 {
        if frames[cursor].voiced {
            left.push(frames[cursor].f0_hz.ln());
        }
        cursor -= 1;
    }
    let mut cursor = after;
    while cursor < frames.len() && right.len() < 3 {
        if frames[cursor].voiced {
            right.push(frames[cursor].f0_hz.ln());
        }
        cursor += 1;
    }
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left.sort_by(|a, b| a.total_cmp(b));
    right.sort_by(|a, b| a.total_cmp(b));
    let difference = 1_200.0 * (median_sorted(&right).exp() / median_sorted(&left).exp()).log2();
    difference.abs() <= cents
}

fn merge_short_segments(
    frames: &[VocalFrame],
    hop_seconds: f64,
    window_seconds: f64,
    params: &DetectorParams,
    segments: &mut Vec<(usize, usize)>,
) -> usize {
    // A boundary cannot be placed closer than half an analysis window, so a
    // segment measures up to that much longer than the event it came from.
    let smear = 0.5 * window_seconds;
    let min_frames = (((params.min_note_seconds + smear) / hop_seconds).round() as usize).max(1);
    while segments.len() > 1 {
        let mut shortest: Option<(usize, usize)> = None;
        for (index, (start, end)) in segments.iter().enumerate() {
            let length = end - start + 1;
            if length < min_frames
                && shortest.is_none_or(|(_, shortest_length)| length < shortest_length)
            {
                shortest = Some((index, length));
            }
        }
        let Some((index, _)) = shortest else {
            break;
        };
        let previous = index.checked_sub(1);
        let next = (index + 1 < segments.len()).then_some(index + 1);
        let target = match (previous, next) {
            (Some(left), Some(right)) => {
                let left_distance = segment_distance(frames, segments[index], segments[left]);
                let right_distance = segment_distance(frames, segments[index], segments[right]);
                if left_distance <= right_distance {
                    left
                } else {
                    right
                }
            }
            (Some(left), None) => left,
            (None, Some(right)) => right,
            (None, None) => break,
        };
        let merged = (
            segments[index].0.min(segments[target].0),
            segments[index].1.max(segments[target].1),
        );
        segments[target] = merged;
        segments.remove(index);
    }
    min_frames
}

/// Move every internal cut onto the pitch inflection of its transition.
///
/// The band crossing that armed the hold sits where the pitch first leaves the
/// band, which is before a fast transition has moved. The physical boundary is
/// the steepest part of the transition, so the cut moves to the slope-weighted
/// centre of the pitch difference inside a window around it. A slow scoop (a
/// portamento) is not a note change and keeps the frame the band crossing
/// chose. The note count is already fixed by the merge above, so this only
/// moves cuts: the span of each note follows the cut and no note appears or
/// disappears.
fn refine_segment_boundaries(
    frames: &[VocalFrame],
    hop_seconds: f64,
    min_frames: usize,
    segments: &mut [(usize, usize)],
) {
    if segments.len() < 2 {
        return;
    }
    let max_shift = (INFLECTION_MAX_SHIFT_SECONDS / hop_seconds).round() as usize;
    for index in 1..segments.len() {
        let boundary = segments[index].0;
        // Both notes keep at least the minimum length the merge enforced, so
        // the note count cannot change when a cut moves into a short neighbour.
        let first = (segments[index - 1].0 + min_frames).max(boundary.saturating_sub(max_shift));
        let Some(right_last) = (boundary..=segments[index].1)
            .rev()
            .find(|&frame| frames[frame].voiced)
        else {
            continue;
        };
        let last = (segments[index].1 + 1)
            .saturating_sub(min_frames)
            .min(right_last)
            .min(boundary + max_shift);
        if first > last {
            continue;
        }
        let Some(target) = pitch_inflection(frames, boundary, hop_seconds, first, last) else {
            continue;
        };
        if target == boundary {
            continue;
        }
        segments[index - 1].1 = target - 1;
        segments[index].0 = target;
    }
}

/// Frame index near the steepest part of the pitch transition around `boundary`.
///
/// The pitch difference between neighbouring frames is weighted by its own size,
/// so the centre of the transition is a sub-frame estimate rounded onto the
/// frame grid. `first..=last` is the range the cut may take, and the returned
/// frame always has voiced neighbours. Returns `None` when the window holds no
/// voiced pair or when the transition is slower than a step.
fn pitch_inflection(
    frames: &[VocalFrame],
    boundary: usize,
    hop_seconds: f64,
    first: usize,
    last: usize,
) -> Option<usize> {
    if boundary == 0 || boundary >= frames.len() || first > last {
        return None;
    }
    if !frames[boundary - 1].voiced || !frames[boundary].voiced {
        return None;
    }
    let window = (INFLECTION_WINDOW_SECONDS / hop_seconds).round().max(1.0) as usize;
    let start = boundary.saturating_sub(window).max(1);
    let end = (boundary + window).min(frames.len() - 1);
    let mut weight = 0.0;
    let mut weighted = 0.0;
    let mut peak = 0.0_f64;
    for index in start..=end {
        if !frames[index].voiced || !frames[index - 1].voiced {
            continue;
        }
        let cents = 1_200.0 * (frames[index].f0_hz / frames[index - 1].f0_hz).log2();
        if !cents.is_finite() {
            continue;
        }
        let slope = cents.abs();
        peak = peak.max(slope / hop_seconds);
        weight += slope;
        weighted += index as f64 * slope;
    }
    if weight <= 0.0 || peak < INFLECTION_MIN_SLOPE_CENTS_PER_SECOND {
        return None;
    }
    let centre = (weighted / weight).round() as usize;
    let centre = centre.clamp(first, last);
    // A note that starts on an unvoiced frame would take the release correction
    // at an internal boundary, so the cut always lands on a voiced pair.
    for distance in 0..=(last - first) {
        if let Some(candidate) = centre.checked_sub(distance)
            && candidate >= first
            && frames[candidate - 1].voiced
            && frames[candidate].voiced
        {
            return Some(candidate);
        }
        let candidate = centre + distance;
        if candidate <= last && frames[candidate - 1].voiced && frames[candidate].voiced {
            return Some(candidate);
        }
    }
    None
}

fn segment_distance(frames: &[VocalFrame], a: (usize, usize), b: (usize, usize)) -> f64 {
    let left = segment_median(frames, a);
    let right = segment_median(frames, b);
    if left <= 0.0 || right <= 0.0 {
        return f64::INFINITY;
    }
    1_200.0 * (left / right).log2().abs()
}

fn segment_median(frames: &[VocalFrame], (start, end): (usize, usize)) -> f64 {
    let mut values: Vec<f64> = frames[start..=end]
        .iter()
        .filter(|frame| frame.voiced)
        .map(|frame| frame.f0_hz)
        .collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    median_sorted(&values)
}

fn median_sorted(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    if values.len().is_multiple_of(2) {
        0.5 * (values[values.len() / 2 - 1] + values[values.len() / 2])
    } else {
        values[values.len() / 2]
    }
}

struct RunningMedian {
    values: Vec<f64>,
}

impl RunningMedian {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn clear(&mut self) {
        self.values.clear();
    }

    fn push(&mut self, value: f64) {
        let position = self.values.partition_point(|candidate| *candidate <= value);
        self.values.insert(position, value);
    }

    fn median(&self) -> f64 {
        median_sorted(&self.values)
    }
}

/// 64-bit FNV-1a; stable across platforms and released toolchains.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const RATE: u32 = 48_000;

    fn sine(f0: f64, amplitude: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        (0..frames)
            .map(|index| {
                let phase = std::f64::consts::TAU * f0 * index as f64 / rate as f64;
                (amplitude * phase.sin()) as f32
            })
            .collect()
    }

    fn harmonic_tone(f0: f64, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        (0..frames)
            .map(|index| {
                let phase = std::f64::consts::TAU * f0 * index as f64 / rate as f64;
                (0.3 * phase.sin() + 0.15 * (2.0 * phase).sin()) as f32
            })
            .collect()
    }

    fn add_white(samples: &[f32], snr_db: f64) -> Vec<f32> {
        let signal_rms = (samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        let noise_rms = signal_rms * 10f64.powf(-snr_db / 20.0);
        let scale = noise_rms * 3f64.sqrt();
        let mut state = 0x5eed_1234_u32;
        samples
            .iter()
            .map(|sample| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let white = (f64::from(state) / f64::from(u32::MAX)) * 2.0 - 1.0;
                (*sample as f64 + white * scale) as f32
            })
            .collect()
    }

    fn sequence(segments: &[(f64, f64)], amplitude: f64, rate: u32) -> Vec<f32> {
        let total = segments.iter().map(|(_, seconds)| seconds).sum::<f64>();
        let frames = (rate as f64 * total) as usize;
        let mut samples = Vec::with_capacity(frames);
        let mut phase: f64 = 0.0;
        let mut segment = 0;
        let mut remaining = (segments[0].1 * rate as f64) as usize;
        for _ in 0..frames {
            while remaining == 0 && segment + 1 < segments.len() {
                segment += 1;
                remaining = (segments[segment].1 * rate as f64) as usize;
            }
            let f0 = segments[segment].0;
            samples.push((amplitude * phase.sin()) as f32);
            phase += std::f64::consts::TAU * f0 / rate as f64;
            remaining = remaining.saturating_sub(1);
        }
        samples
    }

    /// A phrase whose pitch glides between plateaus, so the voicing stays
    /// continuous across each change and the hold decides the cut.
    fn legato(notes: &[(f64, f64)], glide_seconds: f64, amplitude: f64, rate: u32) -> Vec<f32> {
        let total = notes.iter().map(|(_, seconds)| seconds).sum::<f64>()
            + glide_seconds * notes.len().saturating_sub(1) as f64;
        let frames = (rate as f64 * total) as usize;
        let glide_frames = (glide_seconds * rate as f64) as usize;
        let mut samples = Vec::with_capacity(frames);
        let mut phase: f64 = 0.0;
        let mut index = 0;
        let mut remaining = (notes[0].1 * rate as f64) as usize;
        let mut gliding = 0;
        let mut from = notes[0].0;
        for _ in 0..frames {
            if remaining == 0 && gliding == 0 && index + 1 < notes.len() {
                from = notes[index].0;
                gliding = glide_frames;
            }
            let f0 = if gliding > 0 {
                let progress = 1.0 - gliding as f64 / glide_frames as f64;
                from * (notes[index + 1].0 / from).powf(progress)
            } else {
                notes[index].0
            };
            samples.push((amplitude * phase.sin()) as f32);
            phase += std::f64::consts::TAU * f0 / rate as f64;
            if gliding > 0 {
                gliding -= 1;
                if gliding == 0 {
                    index += 1;
                    remaining = (notes[index].1 * rate as f64) as usize;
                }
            } else {
                remaining = remaining.saturating_sub(1);
            }
        }
        samples
    }

    fn glide(start: f64, end: f64, seconds: f64, amplitude: f64, rate: u32) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(frames);
        for index in 0..frames {
            let t = index as f64 / rate as f64;
            let f0 = start * (end / start).powf(t / seconds);
            samples.push((amplitude * phase.sin()) as f32);
            phase += std::f64::consts::TAU * f0 / rate as f64;
        }
        samples
    }

    fn run(samples: &[f32], rate: u32) -> VocalAnalysis {
        analyze(samples, rate, &DetectorParams::default()).unwrap()
    }

    fn cents(measured: f64, reference: f64) -> f64 {
        1_200.0 * (measured / reference).log2()
    }

    fn percentile(values: &mut [f64], fraction: f64) -> f64 {
        values.sort_by(|left, right| left.total_cmp(right));
        if values.is_empty() {
            return 0.0;
        }
        let index = ((values.len() - 1) as f64 * fraction).round() as usize;
        values[index]
    }

    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(|left, right| left.total_cmp(right));
        if values.is_empty() {
            return 0.0;
        }
        if values.len().is_multiple_of(2) {
            0.5 * (values[values.len() / 2 - 1] + values[values.len() / 2])
        } else {
            values[values.len() / 2]
        }
    }

    fn settled_errors(analysis: &VocalAnalysis, reference: f64, edge: f64) -> Vec<f64> {
        analysis
            .frames
            .iter()
            .filter(|frame| frame.voiced)
            .filter(|frame| frame.time_s >= edge && frame.time_s <= analysis.duration_s - edge)
            .map(|frame| cents(frame.f0_hz, reference))
            .collect()
    }

    fn octave_errors(analysis: &VocalAnalysis, reference: f64) -> usize {
        analysis
            .frames
            .iter()
            .filter(|frame| frame.voiced && cents(frame.f0_hz, reference).abs() > 600.0)
            .count()
    }

    fn boundary_errors(analysis: &VocalAnalysis, truth: &[f64]) -> Vec<f64> {
        let starts: Vec<f64> = analysis
            .notes
            .iter()
            .skip(1)
            .map(|note| note.start_s)
            .collect();
        truth
            .iter()
            .map(|target| {
                starts
                    .iter()
                    .map(|start| (start - target).abs())
                    .fold(f64::INFINITY, f64::min)
            })
            .collect()
    }

    fn assert_boundaries(analysis: &VocalAnalysis, truth: &[f64]) {
        let mut errors = boundary_errors(analysis, truth);
        let p90 = percentile(&mut errors, 0.9);
        assert!(
            p90 <= 0.020,
            "boundary p90 {:.1} ms on {:.1?}",
            p90 * 1_000.0,
            truth
        );
    }

    #[test]
    fn harmonic_tone_meets_the_cents_contract() {
        let samples = harmonic_tone(220.0, 3.0, RATE);
        let analysis = run(&samples, RATE);
        let mut errors = settled_errors(&analysis, 220.0, 0.1);
        let median_error = median(&mut errors);
        let p95 = percentile(&mut errors, 0.95);
        println!(
            "S1 220 Hz: median {median_error:+.3} cents, p95 {p95:.3} cents, {} notes",
            analysis.notes.len()
        );
        assert!(median_error.abs() <= 2.0, "median {median_error:.3} cents");
        assert!(p95 <= 5.0, "p95 {p95:.3} cents");
        assert_eq!(analysis.notes.len(), 1, "one sustained tone is one note");
    }

    #[test]
    fn breathy_tone_has_no_octave_errors() {
        for snr_db in [12.0, 6.0] {
            let samples = add_white(&harmonic_tone(220.0, 1.5, RATE), snr_db);
            let analysis = run(&samples, RATE);
            let mut errors = settled_errors(&analysis, 220.0, 0.1);
            let median_error = median(&mut errors);
            let octave = octave_errors(&analysis, 220.0);
            println!(
                "S4 220 Hz at {snr_db:.0} dB SNR: median {median_error:+.3} cents, {octave} octave errors"
            );
            assert!(
                median_error.abs() <= 5.0,
                "{snr_db} dB median {median_error:.3}"
            );
            assert_eq!(octave, 0, "{snr_db} dB reported an octave error");
        }
    }

    #[test]
    fn sample_rates_agree_within_half_a_cent() {
        let mut medians = Vec::new();
        for rate in [44_100, 48_000, 96_000] {
            let analysis = run(&harmonic_tone(220.0, 1.0, rate), rate);
            let mut errors = settled_errors(&analysis, 220.0, 0.1);
            medians.push(median(&mut errors));
            println!("{rate} Hz input: {:+0.4} cents", medians.last().unwrap());
        }
        let spread = medians.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            - medians.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(spread <= 0.5, "rate spread {spread:.4} cents");
    }

    #[test]
    fn exponential_glide_is_tracked() {
        let seconds = 2.0;
        let samples = glide(165.0, 330.0, seconds, 0.3, RATE);
        let analysis = run(&samples, RATE);
        let mut errors: Vec<f64> = analysis
            .frames
            .iter()
            .filter(|frame| frame.voiced)
            .filter(|frame| frame.time_s >= 0.1 && frame.time_s <= seconds - 0.1)
            .map(|frame| {
                let t = frame.time_s;
                cents(frame.f0_hz, 165.0 * 2f64.powf(t / seconds))
            })
            .collect();
        let median_error = median(&mut errors);
        let p95 = percentile(&mut errors, 0.95);
        println!(
            "S3 glide: median {median_error:+.3} cents, p95 {p95:.3} cents, {} notes",
            analysis.notes.len()
        );
        assert!(median_error.abs() <= 5.0, "median {median_error:.3} cents");
        assert!(p95 <= 10.0, "p95 {p95:.3} cents");
    }

    #[test]
    fn c_major_scale_reports_eight_detuned_notes() {
        let targets: Vec<f64> = (0..8)
            .map(|index| {
                440.0 * 2f64.powf((60.0 + index as f64 - 69.0) / 12.0) * 2f64.powf(-40.0 / 1_200.0)
            })
            .collect();
        let segments: Vec<(f64, f64)> = targets.iter().map(|f0| (*f0, 0.5)).collect();
        let samples = sequence(&segments, 0.3, RATE);
        let analysis = run(&samples, RATE);
        assert_eq!(
            analysis.notes.len(),
            8,
            "detected notes: {:.1?}",
            analysis.notes
        );
        for (index, note) in analysis.notes.iter().enumerate() {
            let error = cents(note.f0_median_hz, targets[index]);
            println!("S5 note {}: {:+0.2} cents", index + 1, error);
            assert!(
                error.abs() <= 5.0,
                "note {} error {error:.2} cents",
                index + 1
            );
        }
        let truth: Vec<f64> = (1..8).map(|index| index as f64 * 0.5).collect();
        assert_boundaries(&analysis, &truth);
    }

    #[test]
    fn rising_phrase_splits_at_the_truth_boundaries() {
        let segments = [(220.0, 0.4), (277.183, 0.4), (349.228, 0.4)];
        let samples = sequence(&segments, 0.3, RATE);
        let analysis = run(&samples, RATE);
        println!(
            "rising phrase notes: {:.1?}",
            analysis
                .notes
                .iter()
                .map(|note| note.start_s)
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 3, "notes: {:.1?}", analysis.notes);
        for (index, note) in analysis.notes.iter().enumerate() {
            let error = cents(note.f0_median_hz, segments[index].0);
            assert!(
                error.abs() <= 5.0,
                "note {} error {error:.2} cents",
                index + 1
            );
        }
        assert_boundaries(&analysis, &[0.4, 0.8]);
    }

    #[test]
    fn legato_glide_stays_one_note() {
        let samples = glide(220.0, 233.082, 1.2, 0.3, RATE);
        let analysis = run(&samples, RATE);
        println!(
            "legato glide: {} notes, starts {:.3?}",
            analysis.notes.len(),
            analysis
                .notes
                .iter()
                .map(|note| note.start_s)
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 1, "a hundred-cent drift is one note");
    }

    #[test]
    fn a_note_after_silence_puts_the_detected_edges_on_the_event() {
        // A note opening out of silence flips the voicing decision 0.45 of a
        // window before the event, so the reported span is a window smear
        // longer than the note. The detected edges carry the event; the
        // reported span keeps the frame grid the project reads.
        let mut samples = vec![0.0_f32; (0.300 * f64::from(RATE)) as usize];
        samples.extend_from_slice(&harmonic_tone(220.0, 0.400, RATE));
        samples.extend(std::iter::repeat_n(
            0.0_f32,
            (0.300 * f64::from(RATE)) as usize,
        ));
        let analysis = run(&samples, RATE);
        assert_eq!(analysis.notes.len(), 1, "notes: {:.4?}", analysis.notes);
        let note = &analysis.notes[0];
        assert!(
            (note.onset_s - 0.300).abs() <= 0.008,
            "onset {:.4} s, event 0.300 s",
            note.onset_s
        );
        assert!(
            (note.offset_s - 0.700).abs() <= 0.008,
            "offset {:.4} s, event 0.700 s",
            note.offset_s
        );
        // The reported span is the frame grid: it starts before the event and
        // ends after it by the same window smear.
        let params = DetectorParams::default();
        let window = (params.window_cycles / params.f0_min_hz).max(params.min_window_seconds);
        let smear = (0.5 - EDGE_FLIP_FRACTION) * window;
        assert!(
            (note.onset_s - note.start_s - smear).abs() <= 0.001,
            "reported start {:.4} s is not a smear before the onset {:.4} s",
            note.start_s,
            note.onset_s
        );
        assert!(
            (note.end_s - note.offset_s - smear).abs() <= 0.001,
            "reported end {:.4} s is not a smear after the offset {:.4} s",
            note.end_s,
            note.offset_s
        );
    }

    #[test]
    fn a_note_inside_a_voiced_run_keeps_one_span() {
        // Back-to-back notes: neither edge is a voiced/unvoiced flip, so the
        // reported span and the detected edges agree and no duration moves.
        let segments = [(220.0, 0.400), (277.183, 0.400)];
        let samples = sequence(&segments, 0.3, RATE);
        let analysis = run(&samples, RATE);
        assert_eq!(analysis.notes.len(), 2, "notes: {:.4?}", analysis.notes);
        for note in &analysis.notes {
            assert_eq!(note.onset_s, note.start_s, "{note:?}");
            assert_eq!(note.offset_s, note.end_s, "{note:?}");
        }
    }

    #[test]
    fn deep_vibrato_stays_one_note() {
        // Trained 6 Hz, +-90 cent vibrato on a sustained note. The excursion
        // leaves the 60 cent band for about half a cycle (80 ms), well short
        // of the 150 ms a real pitch change has to sustain.
        let seconds = 2.0;
        let frames = (f64::from(RATE) * seconds) as usize;
        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(frames);
        for index in 0..frames {
            let t = index as f64 / f64::from(RATE);
            let depth = std::f64::consts::TAU * 6.0 * t;
            let f0 = 260.0 * 2f64.powf(90.0 / 1_200.0 * depth.sin());
            samples.push((0.3 * phase.sin()) as f32);
            phase += std::f64::consts::TAU * f0 / f64::from(RATE);
        }
        let analysis = run(&samples, RATE);
        assert_eq!(
            analysis.notes.len(),
            1,
            "vibrato split into {} notes",
            analysis.notes.len()
        );
    }

    #[test]
    fn fast_gate_accepts_a_sustained_octave_transition() {
        let params = DetectorParams::default();
        let pending: Vec<f64> =
            std::iter::repeat_n(440.0_f64.ln(), params.fast_cut_frames).collect();
        assert!(
            measured_pitch_change(&pending, 220.0, &params),
            "a sustained octave is a measured pitch change"
        );
    }

    #[test]
    fn fast_gate_rejects_a_returning_excursion() {
        // A re-articulation at the same pitch scoops a hundred and fifty cents
        // down and returns. The excursion is long enough to arm the fast cut,
        // but its trailing frames are back inside the gate, so the note holds.
        let params = DetectorParams::default();
        let mut pending = Vec::new();
        for index in 0..params.fast_cut_frames {
            let phase = index as f64 / params.fast_cut_frames as f64;
            let cents = -150.0 * (1.0 - (2.0 * phase - 1.0).abs());
            pending.push((220.0 * 2f64.powf(cents / 1_200.0)).ln());
        }
        assert!(
            !measured_pitch_change(&pending, 220.0, &params),
            "an excursion returning to its own pitch is not a cut"
        );
    }

    #[test]
    fn a_step_places_the_cut_at_the_inflection_not_the_band_crossing() {
        // A 200 cent step through a 100 ms glide. The band crossing sits 60
        // cents into the glide, 30 ms after it starts; the physical boundary is
        // the steepest part of the transition, 50 ms in.
        let target = 220.0 * 2f64.powf(200.0 / 1_200.0);
        let samples = legato(&[(220.0, 0.400), (target, 0.500)], 0.100, 0.3, RATE);
        let analysis = run(&samples, RATE);
        println!(
            "step: {:.4?}",
            analysis
                .notes
                .iter()
                .map(|note| (note.start_s, note.end_s, note.f0_median_hz))
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 2, "notes: {:.1?}", analysis.notes);
        let cut = analysis.notes[1].start_s;
        let band_crossing = 0.400 + 0.100 * 60.0 / 200.0;
        assert!(
            (cut - 0.450).abs() <= 0.015,
            "cut {cut:.4} s is not the step at 0.450 s"
        );
        assert!(
            cut - band_crossing >= 0.010,
            "cut {cut:.4} s is still the band crossing at {band_crossing:.4} s"
        );
        // A cut inside a voiced run keeps the detected edge on the cut.
        assert_eq!(analysis.notes[1].onset_s, cut);
    }

    #[test]
    fn a_slow_scoop_keeps_the_frame_the_band_crossing_chose() {
        // A hundred cent scoop through 300 ms is a portamento, not a note
        // change: the pitch moves at a third of the step's rate, below the
        // inflection gate, so the cut stays where the band crossing put it
        // instead of drifting back to the middle of the scoop.
        let target = 220.0 * 2f64.powf(100.0 / 1_200.0);
        let samples = legato(&[(220.0, 0.400), (target, 0.500)], 0.300, 0.3, RATE);
        let analysis = run(&samples, RATE);
        println!(
            "scoop: {:.4?}",
            analysis
                .notes
                .iter()
                .map(|note| (note.start_s, note.end_s, note.f0_median_hz))
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 2, "notes: {:.1?}", analysis.notes);
        let cut = analysis.notes[1].start_s;
        let band_crossing = 0.400 + 0.300 * 60.0 / 100.0;
        assert!(
            (cut - band_crossing).abs() <= 0.030,
            "cut {cut:.4} s moved away from the band crossing at {band_crossing:.4} s"
        );
        assert!(
            cut > 0.560,
            "cut {cut:.4} s moved back to the middle of the scoop"
        );
        assert_eq!(analysis.notes[1].onset_s, cut);
    }

    #[test]
    fn a_short_legato_note_splits_before_the_old_hold() {
        // The middle note is an 80 ms plateau with a 20 ms glide on each side,
        // shorter than the 150 ms hold, so the full hold folds it into its
        // neighbours. The glide keeps the voicing continuous, so the hold is
        // what decides the cut, and the fast cut places both boundaries.
        let samples = legato(
            &[(220.0, 0.300), (277.183, 0.080), (220.0, 0.300)],
            0.020,
            0.3,
            RATE,
        );
        // The forced onset is an independent cut. Raise it out of reach so the
        // test measures the hold and the fast cut alone.
        let mut params = DetectorParams {
            onset_rise_db: 1e9,
            ..DetectorParams::default()
        };
        let analysis = analyze(&samples, RATE, &params).unwrap();
        println!(
            "short legato: {} notes, {:.4?}",
            analysis.notes.len(),
            analysis
                .notes
                .iter()
                .map(|note| (note.start_s, note.end_s, note.f0_median_hz))
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 3, "notes: {:.1?}", analysis.notes);
        assert!(analysis.notes[1].f0_median_hz > 250.0);
        // The cut sits on the frame grid, which lags the event by the window
        // smear at both ends of the short note.
        assert!((analysis.notes[1].start_s - 0.300).abs() <= 0.030);
        assert!((analysis.notes[1].end_s - 0.430).abs() <= 0.030);

        params.fast_cut_frames = 0;
        let held = analyze(&samples, RATE, &params).unwrap();
        assert_eq!(
            held.notes.len(),
            1,
            "the 150 ms hold absorbed the short note: {:.1?}",
            held.notes
        );
    }

    #[test]
    fn a_same_pitch_rearticulation_stays_one_note() {
        // A gentle amplitude re-articulation at the same pitch stays under the
        // onset rise, so there is no pitch change to measure and no cut.
        let seconds = 1.0;
        let frames = (f64::from(RATE) * seconds) as usize;
        let mut phase: f64 = 0.0;
        let mut samples = Vec::with_capacity(frames);
        for index in 0..frames {
            let t = index as f64 / f64::from(RATE);
            let dip = 1.0 - 0.3 * (-((t - 0.5) / 0.05).powi(2)).exp();
            samples.push((0.3 * dip * phase.sin()) as f32);
            phase += std::f64::consts::TAU * 220.0 / f64::from(RATE);
        }
        let analysis = run(&samples, RATE);
        assert_eq!(
            analysis.notes.len(),
            1,
            "a same-pitch re-articulation split into {} notes",
            analysis.notes.len()
        );
    }

    #[test]
    fn short_blip_merges_into_a_neighbour() {
        let segments = [(220.0, 0.5), (330.0, 0.030), (220.0, 0.5)];
        let samples = sequence(&segments, 0.3, RATE);
        let analysis = run(&samples, RATE);
        println!(
            "blip: {} notes, {:.4?}",
            analysis.notes.len(),
            analysis
                .notes
                .iter()
                .map(|note| (note.start_s, note.end_s, note.f0_median_hz))
                .collect::<Vec<_>>()
        );
        assert_eq!(analysis.notes.len(), 2, "a 30 ms blip is absorbed");
        for note in &analysis.notes {
            assert!(
                note.end_s - note.start_s >= 0.05,
                "a short segment survived"
            );
            assert!(cents(note.f0_median_hz, 220.0).abs() <= 5.0);
        }
        assert!(
            analysis.notes[1].start_s <= 0.51,
            "the absorbed blip region was not covered"
        );
    }

    #[test]
    fn analysis_is_bit_identical_across_runs() {
        let samples = sequence(&[(220.0, 0.4), (277.183, 0.4), (349.228, 0.4)], 0.3, RATE);
        let first = run(&samples, RATE);
        let second = run(&samples, RATE);
        assert_eq!(first.frames.len(), second.frames.len());
        for (left, right) in first.frames.iter().zip(second.frames.iter()) {
            assert_eq!(left.f0_hz.to_bits(), right.f0_hz.to_bits());
            assert_eq!(left.confidence.to_bits(), right.confidence.to_bits());
            assert_eq!(left.voiced, right.voiced);
            assert_eq!(left.time_s.to_bits(), right.time_s.to_bits());
        }
        assert_eq!(first.notes.len(), second.notes.len());
        for (left, right) in first.notes.iter().zip(second.notes.iter()) {
            assert_eq!(left.start_s.to_bits(), right.start_s.to_bits());
            assert_eq!(left.end_s.to_bits(), right.end_s.to_bits());
            assert_eq!(left.f0_median_hz.to_bits(), right.f0_median_hz.to_bits());
            assert_eq!(left.confidence.to_bits(), right.confidence.to_bits());
            assert_eq!(left.voiced_ratio.to_bits(), right.voiced_ratio.to_bits());
        }
    }

    #[test]
    fn silence_is_unvoiced_and_short_buffers_are_empty() {
        let analysis = run(&vec![0.0; RATE as usize], RATE);
        assert!(analysis.frames.iter().all(|frame| !frame.voiced));
        assert!(analysis.notes.is_empty());

        let short = run(&sine(220.0, 0.3, 0.010, RATE), RATE);
        assert!(
            short.frames.is_empty(),
            "a buffer shorter than one window has no frames"
        );
        assert!(short.notes.is_empty());
    }

    /// Wall-clock budget as a multiple of the reference workload. Measured at
    /// 4.0 units under the `perf` profile; the ceiling is twice that, so a
    /// doubling of the work still fails while a loaded machine does not.
    #[cfg(feature = "perf-tests")]
    const ANALYSIS_UNIT_BUDGET: f64 = 8.0;

    #[cfg(feature = "perf-tests")]
    #[test]
    fn cpu_budget_is_met_with_slack() {
        let samples = harmonic_tone(220.0, 5.0, RATE);
        // Best of three: the budget is a perf-profile ceiling, and a loaded
        // machine otherwise turns this into a flake.
        let mut best = f64::INFINITY;
        let mut frames = 0;
        for _ in 0..3 {
            let started = Instant::now();
            frames = run(&samples, RATE).frames.len();
            best = best.min(started.elapsed().as_secs_f64());
        }
        let unit = crate::test_support::reference_seconds();
        let ratio = best / 5.0 / unit;
        println!(
            "CPU: {:.4} s per audio second ({} frames, perf profile), {ratio:.1}x the timing unit",
            best / 5.0,
            frames
        );
        assert!(
            ratio <= ANALYSIS_UNIT_BUDGET,
            "analysis used {:.3} s per audio second, {ratio:.1}x the timing unit (budget {ANALYSIS_UNIT_BUDGET})",
            best / 5.0
        );
    }

    fn candidates_for(difference: &[f64]) -> Vec<f64> {
        let mut out = Vec::new();
        collect_candidates(
            difference, difference, 20, 300, 0.5, 0.05, 3, 16_000.0, 1.0, &mut out,
        );
        out.into_iter().map(|(f0, _)| f0).collect()
    }

    #[test]
    fn subharmonic_lowest_hypothesis_is_removed() {
        // The 80 Hz dip at 200 samples is a candidate; its half lag (100) is
        // just above the 0.5 candidate threshold, so only the margin sees it.
        // It is the longest period, so it is removed.
        let mut difference = vec![1.0_f64; 301];
        difference[100] = 0.52;
        difference[200] = 0.48;
        let candidates = candidates_for(&difference);
        assert!(
            candidates.iter().all(|f0| (f0 - 80.0).abs() >= 0.5),
            "{candidates:?}"
        );

        // A genuine long period is not periodic at half the lag, so it stays.
        difference[100] = 0.90;
        let candidates = candidates_for(&difference);
        assert!(
            candidates.iter().any(|f0| (f0 - 80.0).abs() < 0.5),
            "{candidates:?}"
        );
    }

    #[test]
    fn subharmonic_margin_boundaries() {
        assert!(prefers_half_lag(0.30, 0.15, 0.0));
        assert!(!prefers_half_lag(0.30, 0.36, 0.05));
        assert!(prefers_half_lag(0.30, 0.36, 0.10));
        assert!(!prefers_half_lag(0.30, f64::INFINITY, 0.25));
    }

    #[test]
    fn detector_hash_covers_the_subharmonic_margin() {
        let mut params = DetectorParams::default();
        let before = params.hash();
        params.subharmonic_margin += 0.01;
        assert_ne!(before, params.hash());
    }

    #[test]
    #[ignore = "measurement harness; prints the table used by docs/vocal-analysis.md"]
    fn measurements_for_docs() {
        let cases: Vec<(&str, Vec<f32>, f64)> = vec![
            (
                "S1 220 Hz + H2 -6 dB",
                harmonic_tone(220.0, 3.0, RATE),
                220.0,
            ),
            (
                "S4 220 Hz 12 dB SNR",
                add_white(&harmonic_tone(220.0, 1.5, RATE), 12.0),
                220.0,
            ),
            (
                "S4 220 Hz 6 dB SNR",
                add_white(&harmonic_tone(220.0, 1.5, RATE), 6.0),
                220.0,
            ),
        ];
        for (name, samples, reference) in cases {
            let analysis = run(&samples, RATE);
            let mut errors = settled_errors(&analysis, reference, 0.1);
            println!(
                "{name}: median {:.3} cents, p95 {:.3} cents, octave {}, notes {}",
                median(&mut errors),
                percentile(&mut errors, 0.95),
                octave_errors(&analysis, reference),
                analysis.notes.len()
            );
        }
        let samples = glide(165.0, 330.0, 2.0, 0.3, RATE);
        let analysis = run(&samples, RATE);
        let mut errors: Vec<f64> = analysis
            .frames
            .iter()
            .filter(|frame| frame.voiced)
            .filter(|frame| frame.time_s >= 0.1 && frame.time_s <= 1.9)
            .map(|frame| cents(frame.f0_hz, 165.0 * 2f64.powf(frame.time_s / 2.0)))
            .collect();
        println!(
            "S3 glide: median {:.3} cents, p95 {:.3} cents, {} notes",
            median(&mut errors),
            percentile(&mut errors, 0.95),
            analysis.notes.len()
        );

        let targets: Vec<f64> = (0..8)
            .map(|index| {
                440.0 * 2f64.powf((60.0 + index as f64 - 69.0) / 12.0) * 2f64.powf(-40.0 / 1_200.0)
            })
            .collect();
        let segments: Vec<(f64, f64)> = targets.iter().map(|f0| (*f0, 0.5)).collect();
        let analysis = run(&sequence(&segments, 0.3, RATE), RATE);
        let errors: Vec<f64> = analysis
            .notes
            .iter()
            .zip(&targets)
            .map(|(note, target)| cents(note.f0_median_hz, *target))
            .collect();
        let mut boundary = boundary_errors(&analysis, &[0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        println!(
            "S5 scale: {} notes, max note error {:.3} cents, boundary p90 {:.1} ms",
            analysis.notes.len(),
            errors.iter().map(|error| error.abs()).fold(0.0, f64::max),
            percentile(&mut boundary, 0.9) * 1_000.0
        );
        let analysis = run(
            &sequence(&[(220.0, 0.4), (277.183, 0.4), (349.228, 0.4)], 0.3, RATE),
            RATE,
        );
        let mut boundary = boundary_errors(&analysis, &[0.4, 0.8]);
        println!(
            "rising phrase: {} notes, boundary p90 {:.1} ms",
            analysis.notes.len(),
            percentile(&mut boundary, 0.9) * 1_000.0
        );
        let analysis = run(
            &sequence(&[(220.0, 0.5), (330.0, 0.030), (220.0, 0.5)], 0.3, RATE),
            RATE,
        );
        println!(
            "30 ms blip: {} notes, durations {:?}",
            analysis.notes.len(),
            analysis
                .notes
                .iter()
                .map(|note| ((note.end_s - note.start_s) * 1_000.0).round())
                .collect::<Vec<_>>()
        );

        let weight_sets = [
            ("A 0.002/2.0/0.6", 0.002, 2.0, 0.6),
            ("B 0.010/2.0/0.6", 0.010, 2.0, 0.6),
            ("C 0.002/1.0/0.3", 0.002, 1.0, 0.3),
            ("D 0.002/3.0/1.2", 0.002, 3.0, 1.2),
        ];
        let mut gated = vec![0.0_f32; (0.4 * RATE as f64) as usize];
        gated.extend(harmonic_tone(220.0, 0.5, RATE));
        gated.extend(vec![0.0_f32; (0.3 * RATE as f64) as usize]);
        let leap = sequence(&[(220.0, 0.5), (440.0, 0.5)], 0.3, RATE);
        for (name, jump, octave, unvoiced) in weight_sets {
            let params = DetectorParams {
                jump_cost_per_cent: jump,
                octave_cost: octave,
                unvoiced_cost: unvoiced,
                ..DetectorParams::default()
            };
            let mut report = Vec::new();
            for (label, signal, reference) in [
                (
                    "6dB",
                    add_white(&harmonic_tone(220.0, 1.5, RATE), 6.0),
                    220.0,
                ),
                (
                    "0dB",
                    add_white(&harmonic_tone(220.0, 1.5, RATE), 0.0),
                    220.0,
                ),
            ] {
                let analysis = analyze(&signal, RATE, &params).unwrap();
                let mut errors = settled_errors(&analysis, reference, 0.1);
                let voiced = analysis.frames.iter().filter(|frame| frame.voiced).count();
                report.push(format!(
                    "{label} median {:.2}c voiced {:.0}% octave {}",
                    median(&mut errors),
                    voiced as f64 * 100.0 / analysis.frames.len().max(1) as f64,
                    octave_errors(&analysis, reference)
                ));
            }
            let analysis = analyze(&gated, RATE, &params).unwrap();
            let outside: Vec<&VocalFrame> = analysis
                .frames
                .iter()
                .filter(|frame| frame.time_s < 0.35 || frame.time_s > 0.95)
                .collect();
            let inside: Vec<&VocalFrame> = analysis
                .frames
                .iter()
                .filter(|frame| frame.time_s > 0.45 && frame.time_s < 0.85)
                .collect();
            let false_voiced = outside.iter().filter(|frame| frame.voiced).count() as f64 * 100.0
                / outside.len().max(1) as f64;
            let coverage = inside.iter().filter(|frame| frame.voiced).count() as f64 * 100.0
                / inside.len().max(1) as f64;
            let analysis = analyze(&leap, RATE, &params).unwrap();
            let second: Vec<&VocalFrame> = analysis
                .frames
                .iter()
                .filter(|frame| frame.time_s > 0.55 && frame.time_s < 0.95)
                .collect();
            let leap_coverage = second.iter().filter(|frame| frame.voiced).count() as f64 * 100.0
                / second.len().max(1) as f64;
            let mut leap_errors: Vec<f64> = second
                .iter()
                .filter(|frame| frame.voiced)
                .map(|frame| cents(frame.f0_hz, 440.0))
                .collect();
            println!(
                "weights {name}: {}; gated false-voiced {false_voiced:.0}% coverage {coverage:.0}%; leap coverage {leap_coverage:.0}% median {:.2}c",
                report.join(", "),
                median(&mut leap_errors),
            );
        }

        let samples = harmonic_tone(220.0, 10.0, RATE);
        let started = Instant::now();
        let analysis = run(&samples, RATE);
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "CPU: {:.4} s per audio second ({} frames)",
            elapsed / 10.0,
            analysis.frames.len()
        );
    }
}
