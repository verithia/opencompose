//! Stretch planning for audio clips.
//! Runs at load or offline render time, outside the audio callback.

use oc_dsl::ast::{AudioClip, MAX_PITCH_SEMITONES, Project, StretchMode};

use crate::clip_pcm::ClipCancellation;
use crate::resample::ResampleQuality;

/// Bumped whenever the stretch DSP changes, so old derived entries are ignored.
pub const ALGO_VERSION: u32 = 8;
/// Accepted ratios. Outside this range the render refuses instead of allocating.
pub const MIN_RATIO: f64 = 0.25;
/// Largest accepted stretch ratio.
pub const MAX_RATIO: f64 = 4.0;

/// Effective processing for one clip: the ratio, the algorithm and the pitch step.
#[derive(Debug, Clone, PartialEq)]
pub struct StretchPlan {
    /// Output duration over the duration of the stretcher's input.
    ///
    /// With a pitch step that input is already resampled, so the output still
    /// lasts `media × stretch`.
    pub ratio: f64,
    pub mode: StretchMode,
    /// Playback-speed resample rate; zero when the clip is not transposed.
    pub pitch_rate: u32,
    /// Actual pitch multiplier of the resample step; 1.0 without a transposition.
    pub pitch_ratio: f64,
    pub formant_cents: f32,
    /// Cumulative boundaries after pitch resampling, paired with output frames.
    pub warp: Vec<(usize, usize)>,
}

impl StretchPlan {
    /// A plan without a pitch step; the ratio goes straight to the stretcher.
    pub fn stretched(ratio: f64, mode: StretchMode) -> Self {
        Self {
            ratio,
            mode,
            pitch_rate: 0,
            pitch_ratio: 1.0,
            formant_cents: 0.0,
            warp: Vec::new(),
        }
    }

    /// A plan that passes media through unchanged.
    pub fn bypass() -> Self {
        Self::stretched(1.0, StretchMode::Off)
    }

    /// A bypass plan must read the decoded media unchanged.
    pub fn is_bypass(&self) -> bool {
        self.warp.is_empty()
            && self.formant_cents == 0.0
            && self.pitch_rate == 0
            && (self.ratio == 1.0 || self.mode == StretchMode::Off)
    }

    /// Frames the stretcher sees: the media after the playback-speed resample.
    pub fn pitched_frames(&self, media_frames: usize, target_rate: u32) -> usize {
        if self.pitch_rate == 0 {
            return media_frames;
        }
        crate::resample::converted_frames(media_frames, target_rate, self.pitch_rate)
    }

    /// Output frame count after pitch, stretch and warp.
    pub fn output_frames(&self, media_frames: usize, target_rate: u32) -> usize {
        self.warp.last().map_or_else(
            || stretched_frames(self.pitched_frames(media_frames, target_rate), self.ratio),
            |&(_, end)| end,
        )
    }

    fn stretch_samples(
        &self,
        samples: &[f32],
        cancellation: ClipCancellation<'_>,
    ) -> Result<Vec<f32>, String> {
        let Some(&(_, total_frames)) = self.warp.last() else {
            return render_cancellable(samples, self.ratio, self.mode, cancellation);
        };
        let mut output = Vec::with_capacity(total_frames * 2);
        let (mut from, mut out_from) = (0, 0);
        for &(to, out_to) in &self.warp {
            let ratio = (out_to - out_from) as f64 / (to - from) as f64;
            let mut segment = render_cancellable(
                &samples[from * 2..to * 2],
                ratio.clamp(MIN_RATIO, MAX_RATIO),
                self.mode,
                cancellation,
            )?;
            segment.resize((out_to - out_from) * 2, 0.0);
            // Fade the boundary discontinuity without moving the anchored
            // transient or changing the number of frames on either side.
            if output.len() >= 2 {
                splice_head(
                    &mut segment,
                    [output[output.len() - 2], output[output.len() - 1]],
                );
            }
            output.extend(segment);
            (from, out_from) = (to, out_to);
        }
        Ok(output)
    }

    /// Bake a disk-backed variant without keeping the full pitched or derived
    /// buffer in memory. Authored warp boundaries remain fixed. Long segments
    /// use overlapping windows, with independent DSP state in each window.
    pub(crate) fn bake_blocks(
        &self,
        samples: &crate::clip_pcm::ClipPcm,
        target_rate: u32,
        quality: ResampleQuality,
        cancellation: ClipCancellation<'_>,
        mut emit: impl FnMut(&[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        cancellation.check()?;
        const BLOCK_FRAMES: usize = 48_000 * 4;
        const OVERLAP_FRAMES: usize = 1024;

        if self.mode == StretchMode::Varispeed && samples.frames() > 0 {
            let (input, output) = varispeed_rates(samples.frames(), self.ratio)?;
            for start in (0..output as usize).step_by(BLOCK_FRAMES) {
                let end = (start + BLOCK_FRAMES).min(output as usize);
                emit(&crate::resample::resample_pcm_window(
                    samples,
                    input,
                    output,
                    quality,
                    start..end,
                    cancellation,
                )?)?;
            }
            return Ok(());
        }

        let pitch_rate = if self.pitch_rate == 0 {
            target_rate
        } else {
            self.pitch_rate
        };
        let input_frames = self.pitched_frames(samples.frames(), target_rate);
        let output_frames = self.output_frames(samples.frames(), target_rate);
        if input_frames <= BLOCK_FRAMES && output_frames <= BLOCK_FRAMES {
            return emit(&self.bake_cancellable(
                &samples.materialize()?,
                target_rate,
                quality,
                cancellation,
            )?);
        }
        let single = [(input_frames, output_frames)];
        let boundaries = if self.warp.is_empty() {
            single.as_slice()
        } else {
            &self.warp
        };
        let mut source_start = 0;
        let mut output_start = 0;
        let mut previous = None;
        for &(source_end, output_end) in boundaries {
            let source_len = source_end - source_start;
            let output_len = output_end - output_start;
            let ratio = output_len as f64 / source_len as f64;
            let source_at = |frame: usize| {
                source_start + ((frame as f64 / ratio).round() as usize).min(source_len)
            };
            let mut tail = Vec::new();
            for start in (0..output_len).step_by(BLOCK_FRAMES) {
                cancellation.check()?;
                let end = (start + BLOCK_FRAMES).min(output_len);
                let padded_end = (end + OVERLAP_FRAMES).min(output_len);
                let input_start = source_at(start);
                let input_end = source_at(padded_end).max(input_start + 1).min(source_end);
                let input = crate::resample::resample_pcm_window(
                    samples,
                    target_rate,
                    pitch_rate,
                    quality,
                    input_start..input_end,
                    cancellation,
                )?;
                let mut block = render_cancellable(
                    &input,
                    ratio.clamp(MIN_RATIO, MAX_RATIO),
                    self.mode,
                    cancellation,
                )?;
                block.resize((padded_end - start) * 2, 0.0);
                oc_dsp::vocal_shift::shift_stereo_formants_while(
                    &mut block,
                    target_rate,
                    self.formant_cents,
                    || !cancellation.is_cancelled(),
                );
                cancellation.check()?;
                if start == 0
                    && let Some(previous) = previous
                {
                    splice_head(&mut block, previous);
                }
                let overlap = tail.len() / 2;
                for (frame, (head, earlier)) in block
                    .chunks_exact_mut(2)
                    .zip(tail.chunks_exact(2))
                    .enumerate()
                {
                    let weight = frame as f32 / overlap as f32;
                    for ch in 0..2 {
                        head[ch] = earlier[ch] * (1.0 - weight) + head[ch] * weight;
                    }
                }
                let written = (end - start) * 2;
                emit(&block[..written])?;
                previous = Some([block[written - 2], block[written - 1]]);
                tail.clear();
                tail.extend_from_slice(&block[written..]);
            }
            source_start = source_end;
            output_start = output_end;
        }
        Ok(())
    }

    /// Bake the variant: optional playback-speed resample, then the stretcher.
    ///
    /// A plan without pitch calls the stretcher on the input, unchanged.
    pub fn bake(
        &self,
        samples: &[f32],
        target_rate: u32,
        quality: ResampleQuality,
    ) -> Result<Vec<f32>, String> {
        self.bake_cancellable(samples, target_rate, quality, ClipCancellation::default())
    }

    pub(crate) fn bake_cancellable(
        &self,
        samples: &[f32],
        target_rate: u32,
        quality: ResampleQuality,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Vec<f32>, String> {
        cancellation.check()?;
        if self.mode == StretchMode::Varispeed {
            return resample_varispeed(samples, self.ratio, quality, cancellation);
        }
        let mut output = if self.pitch_rate == 0 {
            self.stretch_samples(samples, cancellation)?
        } else {
            let pitched = crate::resample::resample_cancellable(
                samples,
                target_rate,
                self.pitch_rate,
                quality,
                cancellation,
            )?;
            self.stretch_samples(&pitched, cancellation)?
        };
        cancellation.check()?;
        oc_dsp::vocal_shift::shift_stereo_formants_while(
            &mut output,
            target_rate,
            self.formant_cents,
            || !cancellation.is_cancelled(),
        );
        cancellation.check()?;
        Ok(output)
    }
}

/// Resolve the effective processing for a clip whose project-rate media is
/// `media_frames` long.
///
/// Tempo-follow fits the whole media to the clip range; otherwise the explicit
/// `stretch` is used. A pitch is a playback-speed resample by `2^(pitch/12)` and
/// the stretch is scaled by the same factor, so the duration does not move. A
/// mode of `off` or a ratio of exactly 1.0 without pitch is a bypass.
pub fn resolve(
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    media_frames: usize,
) -> Result<StretchPlan, String> {
    if clip.stretch_mode == StretchMode::Varispeed && !clip.varispeed_controls_compatible() {
        return Err(format!(
            "audio clip '{}': varispeed links pitch to speed and requires independent pitch, formant processing and warp markers to be cleared",
            clip.name
        ));
    }
    if !clip.pitch.is_finite() || clip.pitch.abs() > MAX_PITCH_SEMITONES {
        return Err(format!(
            "audio clip '{}': pitch {} is outside -{MAX_PITCH_SEMITONES}..={MAX_PITCH_SEMITONES} \
             semitones",
            clip.name, clip.pitch
        ));
    }
    if clip.pitch != 0.0 && clip.stretch_mode == StretchMode::Off {
        return Err(format!(
            "audio clip '{}': pitch needs a stretch mode other than off",
            clip.name
        ));
    }
    if clip.stretch_mode == StretchMode::Off
        && (clip.formant != 0.0 || !clip.warp_markers.is_empty())
    {
        return Err(format!(
            "audio clip '{}': formants and warp markers need stretch processing",
            clip.name
        ));
    }
    if clip.tempo_follow && !clip.warp_markers.is_empty() {
        return Err(format!(
            "audio clip '{}': warp markers need tempo follow off",
            clip.name
        ));
    }
    if clip.bypass_processing || clip.stretch_mode == StretchMode::Off || media_frames == 0 {
        return Ok(StretchPlan::bypass());
    }
    let stretch = if clip.tempo_follow {
        let (_, range_frames) = crate::audio_clip::range_frames(project, clip, sample_rate)
            .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
        range_frames as f64 / media_frames as f64
    } else {
        clip.stretch
    };
    let (pitch_rate, pitch_ratio) = pitch_step(sample_rate, clip.pitch);
    let ratio = stretch * pitch_ratio;
    if !ratio.is_finite() || !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!(
            "audio clip '{}': stretch ratio {ratio:.4} is outside {MIN_RATIO}..={MAX_RATIO}",
            clip.name
        ));
    }
    let correction = clip.formant
        - if clip.preserve_formants {
            12.0 * pitch_ratio.log2()
        } else {
            0.0
        };
    let formant_cents = (correction * 100.0) as f32;
    if !formant_cents.is_finite()
        || formant_cents.abs() > oc_dsp::vocal_shift::MAX_FORMANT_CENTS + 0.1
    {
        return Err(format!(
            "audio clip '{}': formant correction must be within ±12 semitones",
            clip.name
        ));
    }
    let mut plan = StretchPlan {
        ratio,
        mode: clip.stretch_mode,
        pitch_rate,
        pitch_ratio,
        formant_cents,
        warp: Vec::new(),
    };
    if !clip.warp_markers.is_empty() {
        plan.warp = warp_boundaries(clip, &plan, sample_rate, media_frames)?;
    }
    if pitch_rate == 0 && ratio == 1.0 && formant_cents == 0.0 && plan.warp.is_empty() {
        return Ok(StretchPlan::bypass());
    }
    Ok(plan)
}

/// Resample rate and its actual multiplier for a transposition in semitones.
///
/// The rate is rounded to a whole number, so the multiplier is the ratio the
/// resampler really applies; `(0, 1.0)` means no pitch step.
fn pitch_step(sample_rate: u32, semitones: f64) -> (u32, f64) {
    if semitones == 0.0 || sample_rate == 0 {
        return (0, 1.0);
    }
    let nominal = 2.0_f64.powf(semitones / 12.0);
    let rate = (f64::from(sample_rate) / nominal).round().max(1.0) as u32;
    (rate, f64::from(sample_rate) / f64::from(rate))
}

fn warp_boundaries(
    clip: &AudioClip,
    plan: &StretchPlan,
    sample_rate: u32,
    media_frames: usize,
) -> Result<Vec<(usize, usize)>, String> {
    let error = |detail: &str| format!("audio clip '{}': {detail}", clip.name);
    let rate = f64::from(sample_rate);
    let input_end = plan.pitched_frames(media_frames, sample_rate);
    let mut boundaries = Vec::with_capacity(clip.warp_markers.len() + 1);
    let (mut from, mut out_from) = (0, 0);
    for marker in &clip.warp_markers {
        if marker.source_seconds > media_frames as f64 / rate + 0.5 / rate {
            return Err(error("warp marker exceeds the source length"));
        }
        let to =
            ((marker.source_seconds * rate / plan.pitch_ratio).round() as usize).min(input_end);
        let out_to = (marker.target_seconds * rate).round() as usize;
        if to <= from || out_to <= out_from {
            return Err(error("warp anchors must be at least one frame apart"));
        }
        let ratio = (out_to - out_from) as f64 / (to - from) as f64;
        if !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
            return Err(error(
                "warp segment ratio must be within 0.25 to 4 after pitch",
            ));
        }
        boundaries.push((to, out_to));
        (from, out_from) = (to, out_to);
    }
    if from < input_end {
        boundaries.push((
            input_end,
            out_from + stretched_frames(input_end - from, plan.ratio),
        ));
    }
    Ok(boundaries)
}

/// Bake one stretched variant with the algorithm `mode` selects.
///
/// `Off` returns the input unchanged; the cache is never reached for it.
pub fn render(samples: &[f32], ratio: f64, mode: StretchMode) -> Result<Vec<f32>, String> {
    render_cancellable(samples, ratio, mode, ClipCancellation::default())
}

fn render_cancellable(
    samples: &[f32],
    ratio: f64,
    mode: StretchMode,
    cancellation: ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    cancellation.check()?;
    match mode {
        StretchMode::Percussive => wsola_cancellable(samples, ratio, cancellation),
        StretchMode::Musical => {
            crate::phase_vocoder::stretch_cancellable(samples, ratio, cancellation)
        }
        StretchMode::Varispeed => resample_varispeed(
            samples,
            ratio,
            ResampleQuality::DEFAULT_RENDER,
            cancellation,
        ),
        StretchMode::Off => Ok(samples.to_vec()),
    }
}

fn varispeed_rates(frames: usize, ratio: f64) -> Result<(u32, u32), String> {
    if !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!(
            "Varispeed ratio must be between {MIN_RATIO} and {MAX_RATIO}"
        ));
    }
    // The sinc converter uses only the quotient. Frame counts keep its end
    // exact even when the requested speed has no integer-Hz conversion rate.
    let count =
        |frames| u32::try_from(frames).map_err(|_| "Varispeed clip is too long".to_string());
    Ok((count(frames)?, count(stretched_frames(frames, ratio))?))
}

fn resample_varispeed(
    samples: &[f32],
    ratio: f64,
    quality: ResampleQuality,
    cancellation: ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let (input, output) = varispeed_rates(samples.len() / 2, ratio)?;
    crate::resample::resample_cancellable(samples, input, output, quality, cancellation)
}

/// Output frame count for a plan: `round(input × ratio)`, at least one frame.
pub fn stretched_frames(media_frames: usize, ratio: f64) -> usize {
    scaled_frame(media_frames, ratio).max(1)
}

fn scaled_frame(frame: usize, ratio: f64) -> usize {
    (frame as f64 * ratio).round() as usize
}

/// WSOLA synthesis window: 40 ms at the 48 kHz project rate.
const WINDOW: usize = 1920;
/// WSOLA synthesis hop: 20 ms, half the window, so Hann windows sum to one.
const HOP: usize = 960;
/// Similarity search half-width: ±10 ms.
const SEARCH: isize = 480;
/// Correlation decimation for the coarse search; the best lag is refined at 1×.
const DECIMATION: usize = 4;
/// Transient detector hop and window.
const ONSET_HOP: usize = 64;
const ONSET_WINDOW: usize = 256;
/// Mean-sample distance that distinguishes separate transients.
const ONSET_GAP: usize = 32;
/// Samples of linear fade at a segment splice.
const SPLICE_FADE: usize = 32;

fn splice_head(segment: &mut [f32], previous: [f32; 2]) {
    let fade = SPLICE_FADE.min(segment.len() / 2);
    if fade == 0 {
        return;
    }
    for ch in 0..2 {
        let delta = previous[ch] - segment[ch];
        for frame in 0..fade {
            segment[frame * 2 + ch] += delta * (1.0 - frame as f32 / fade as f32);
        }
    }
}

/// Transient-anchored WSOLA time-stretch of interleaved stereo.
///
/// Output length is `round(input × ratio)`; pitch is unchanged. The signal is
/// cut at detected transients so each attack is copied once at its stretched
/// time instead of being faded across overlapping windows. Sample counts follow
/// the 48 kHz project rate.
pub fn wsola(samples: &[f32], ratio: f64) -> Result<Vec<f32>, String> {
    wsola_cancellable(samples, ratio, ClipCancellation::default())
}

fn wsola_cancellable(
    samples: &[f32],
    ratio: f64,
    cancellation: ClipCancellation<'_>,
) -> Result<Vec<f32>, String> {
    if !samples.len().is_multiple_of(2) {
        return Err("stretch input must be interleaved stereo".into());
    }
    if !ratio.is_finite() || !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(format!(
            "stretch ratio {ratio:.4} is outside {MIN_RATIO}..={MAX_RATIO}"
        ));
    }
    let frames = samples.len() / 2;
    if frames == 0 || ratio == 1.0 {
        return Ok(samples.to_vec());
    }
    let mono: Vec<f32> = samples
        .chunks_exact(2)
        .map(|frame| 0.5 * (frame[0] + frame[1]))
        .collect();
    let anchors = detect_transients(&mono, cancellation)?;
    let out_frames = stretched_frames(frames, ratio);
    let mut output = vec![0.0_f32; out_frames * 2];
    for (index, &start) in anchors.iter().enumerate() {
        cancellation.check()?;
        let end = anchors.get(index + 1).copied().unwrap_or(frames);
        let out_start = scaled_frame(start, ratio);
        let out_end = match anchors.get(index + 1) {
            Some(&next) => scaled_frame(next, ratio),
            None => out_frames,
        };
        if out_end <= out_start || end <= start {
            continue;
        }
        let last = anchors.get(index + 1).is_none();
        stretch_segment(
            samples,
            &mono,
            (start, end),
            &mut output[out_start * 2..out_end * 2],
            ratio,
            last,
            cancellation,
        )?;
    }
    Ok(output)
}

/// Cut points: sample zero plus every energy rise above −20 dB of the peak.
fn detect_transients(
    mono: &[f32],
    cancellation: ClipCancellation<'_>,
) -> Result<Vec<usize>, String> {
    let mut anchors = vec![0];
    if mono.len() < ONSET_WINDOW {
        return Ok(anchors);
    }
    let mut energy = Vec::with_capacity(mono.len().div_ceil(ONSET_HOP));
    for start in (0..mono.len()).step_by(ONSET_HOP) {
        if start.is_multiple_of(4096) {
            cancellation.check()?;
        }
        energy.push(
            mono[start..(start + ONSET_WINDOW).min(mono.len())]
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum(),
        );
    }
    let peak_energy = energy.iter().copied().fold(0.0_f64, f64::max);
    let peak_amplitude = mono
        .iter()
        .fold(0.0_f32, |max, sample| max.max(sample.abs()));
    if peak_energy <= 0.0 || peak_amplitude <= 0.0 {
        return Ok(anchors);
    }
    let energy_floor = peak_energy * 0.01;
    let amplitude_floor = peak_amplitude * 0.05;
    for index in 1..energy.len() {
        if energy[index] <= energy_floor || energy[index - 1] > energy_floor {
            continue;
        }
        let window_start = (index - 1) * ONSET_HOP;
        let window_end = (index * ONSET_HOP + ONSET_WINDOW).min(mono.len());
        let candidate = (window_start..window_end)
            .find(|&frame| mono[frame].abs() > amplitude_floor)
            .unwrap_or(window_start);
        if candidate > anchors.last().copied().unwrap_or(0) + ONSET_GAP {
            anchors.push(candidate);
        }
    }
    Ok(anchors)
}

/// Stretch one transient-bounded segment into `output` (already sliced).
fn stretch_segment(
    samples: &[f32],
    mono: &[f32],
    range: (usize, usize),
    output: &mut [f32],
    ratio: f64,
    is_last: bool,
    cancellation: ClipCancellation<'_>,
) -> Result<(), String> {
    let out_frames = output.len() / 2;
    let frames = range.1 - range.0;
    if out_frames == 0 || frames == 0 {
        return Ok(());
    }
    let window = WINDOW.min(frames);
    let hop = if window == WINDOW {
        HOP
    } else {
        (window / 2).max(1)
    };
    let search = SEARCH.min(window as isize);
    // The final segment normalises its tail; inner segments splice into the next attack.
    let mut weights = is_last.then(|| vec![0.0_f32; out_frames]);
    let head = hop.min(out_frames).min(frames);
    for frame in 0..head {
        let source = (range.0 + frame) * 2;
        output[frame * 2] = samples[source];
        output[frame * 2 + 1] = samples[source + 1];
    }
    if let Some(weights) = weights.as_mut() {
        weights[..head].fill(1.0);
    }
    // Frame zero continues from the flat head; only its tail is overlap-added.
    for offset in hop..window.min(out_frames) {
        if !in_range(range, offset as isize) {
            continue;
        }
        let weight = hann(offset, window);
        let (left, right) = sample_pair(samples, range, offset as isize);
        output[offset * 2] += weight * left;
        output[offset * 2 + 1] += weight * right;
        if let Some(weights) = weights.as_mut() {
            weights[offset] += weight;
        }
    }
    let overlap = window - hop;
    let analysis_hop = hop as f64 / ratio;
    let mut previous = range.0 as f64;
    let mut frame = 1;
    while frame * hop < out_frames {
        cancellation.check()?;
        // Correlate against the previous grain, but anchor the next search to
        // the segment clock. Accumulating its phase corrections can exhaust
        // the source before the output reaches an authored warp boundary.
        let nominal = (range.0 as f64 + frame as f64 * analysis_hop).round() as isize;
        let target = previous.round() as isize + hop as isize;
        let mut position = best_position(mono, range, nominal, target, overlap.max(1), search)
            .max(range.0 as isize);
        let destination_start = frame * hop;
        let remaining = (out_frames - destination_start).min(window);
        let last_full_grain = range.1 - remaining;
        // Use a complete grain when one fits beyond the transient head.
        // A very short segment may only fit by replaying its attack.
        if last_full_grain >= range.0 + head {
            position = position.min(last_full_grain as isize);
        }
        previous = position as f64;
        for offset in 0..remaining {
            let destination = destination_start + offset;
            let source = position + offset as isize;
            if !in_range(range, source) {
                continue;
            }
            let weight = hann(offset, window);
            let (left, right) = sample_pair(samples, range, source);
            output[destination * 2] += weight * left;
            output[destination * 2 + 1] += weight * right;
            if let Some(weights) = weights.as_mut() {
                weights[destination] += weight;
            }
        }
        frame += 1;
    }
    if let Some(weights) = weights {
        for (frame, weight) in weights.iter().enumerate() {
            let weight = weight.min(1.0);
            if weight >= 0.05 {
                let gain = 1.0 / weight;
                output[frame * 2] *= gain;
                output[frame * 2 + 1] *= gain;
            }
        }
    } else {
        let fade = SPLICE_FADE.min(out_frames / 4);
        for index in 0..fade {
            let gain = index as f32 / fade as f32;
            let destination = (out_frames - 1 - index) * 2;
            output[destination] *= gain;
            output[destination + 1] *= gain;
        }
    }
    Ok(())
}

fn in_range(range: (usize, usize), index: isize) -> bool {
    index >= range.0 as isize && index < range.1 as isize
}

fn hann(index: usize, size: usize) -> f32 {
    (0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / size as f64).cos()) as f32
}

fn sample_pair(samples: &[f32], range: (usize, usize), index: isize) -> (f32, f32) {
    if index < range.0 as isize || index >= range.1 as isize {
        return (0.0, 0.0);
    }
    let offset = index as usize * 2;
    (samples[offset], samples[offset + 1])
}

/// Normalised cross-correlation of a candidate window with the target tail.
///
/// Only samples inside the segment are compared: zero padding at the segment
/// end must not become evidence that shifts the last frames forward.
fn similarity(
    mono: &[f32],
    range: (usize, usize),
    candidate: isize,
    target: isize,
    length: usize,
    stride: usize,
) -> f32 {
    let mut dot = 0.0_f64;
    let mut candidate_energy = 0.0_f64;
    let mut target_energy = 0.0_f64;
    let mut compared = 0;
    let mut offset = 0;
    while offset < length {
        let candidate_index = candidate + offset as isize;
        let target_index = target + offset as isize;
        if in_range(range, candidate_index) && in_range(range, target_index) {
            let candidate_sample = f64::from(mono[candidate_index as usize]);
            let target_sample = f64::from(mono[target_index as usize]);
            dot += candidate_sample * target_sample;
            candidate_energy += candidate_sample * candidate_sample;
            target_energy += target_sample * target_sample;
            compared += 1;
        }
        offset += stride;
    }
    if compared * stride < length / 2 || candidate_energy < 1e-12 || target_energy < 1e-12 {
        return 0.0;
    }
    (dot / (candidate_energy.sqrt() * target_energy.sqrt())) as f32
}

/// Coarse decimated search then one-sample refinement; zero lag wins ties.
fn best_position(
    mono: &[f32],
    range: (usize, usize),
    nominal: isize,
    target: isize,
    length: usize,
    search: isize,
) -> isize {
    let mut best_delta = 0;
    let mut best_score = similarity(mono, range, nominal, target, length, DECIMATION);
    let mut delta = -search;
    while delta <= search {
        if delta != 0 {
            let score = similarity(mono, range, nominal + delta, target, length, DECIMATION);
            if score > best_score + 1e-6 {
                best_score = score;
                best_delta = delta;
            }
        }
        delta += DECIMATION as isize;
    }
    let mut refined = best_delta;
    let mut refined_score = similarity(mono, range, nominal + best_delta, target, length, 1);
    for delta in best_delta - (DECIMATION as isize - 1)..=best_delta + (DECIMATION as isize - 1) {
        if delta == best_delta || delta < -search || delta > search {
            continue;
        }
        let score = similarity(mono, range, nominal + delta, target, length, 1);
        if score > refined_score + 1e-6 {
            refined_score = score;
            refined = delta;
        }
    }
    (nominal + refined).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_project(tempo: f64, stretch: f64, mode: StretchMode, tempo_follow: bool) -> Project {
        let text = format!(
            r#"@project "Stretch plan"
  tempo: {tempo}
@audio_clip hit [1:1 - 3:1] | track: tape
  file: "hit.wav"
  stretch: {stretch}
  mode: {}
  tempo_follow: {tempo_follow}
@track tape | instrument: audio/clip
"#,
            mode.as_str()
        );
        oc_dsl::parse(&text).unwrap()
    }

    #[test]
    fn varispeed_links_pitch_and_duration_across_bake_blocks() {
        use std::sync::Arc;
        let project = make_project(120.0, 2.0, StretchMode::Varispeed, false);
        let rate = 48_000;
        let input_frames = 100_000;
        let source: Vec<_> = (0..input_frames)
            .flat_map(|i| {
                let value =
                    (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(rate)).sin() as f32;
                [value, value]
            })
            .collect();
        let clip = project.audio_clip(0).unwrap();
        let plan = resolve(&project, clip, rate, input_frames).unwrap();
        assert_eq!(plan.output_frames(input_frames, rate), 2 * input_frames);
        let whole = plan
            .bake(&source, rate, ResampleQuality::DEFAULT_RENDER)
            .unwrap();
        assert_eq!(whole.len(), 2 * plan.output_frames(input_frames, rate));
        let crossings = whole
            .chunks_exact(2)
            .skip(48_000)
            .take(48_000)
            .map(|frame| frame[0])
            .zip(whole.chunks_exact(2).skip(48_001).map(|frame| frame[0]))
            .filter(|(a, b)| *a <= 0.0 && *b > 0.0)
            .count();
        assert_eq!(crossings, 220);
        let pcm = crate::clip_pcm::ClipPcm::Resident(Arc::new(source));
        let mut start = 0;
        let mut blocks = 0;
        plan.bake_blocks(
            &pcm,
            rate,
            ResampleQuality::DEFAULT_RENDER,
            Default::default(),
            |block| {
                assert_eq!(block, &whole[start..start + block.len()]);
                start += block.len();
                blocks += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(blocks, 2);
        assert_eq!(start, whole.len());
    }

    #[test]
    fn spooled_bake_stops_before_the_next_output_block() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let source = crate::clip_pcm::ClipPcm::Resident(Arc::new(vec![0.1; 400_000]));
        let plan = StretchPlan::bypass();
        let cancelled = AtomicBool::new(false);
        let mut blocks = 0;
        let result = plan.bake_blocks(
            &source,
            48_000,
            ResampleQuality::PRESERVED,
            ClipCancellation::new(&cancelled, &|| false),
            |_| {
                blocks += 1;
                cancelled.store(true, Ordering::Relaxed);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(blocks, 1);
    }

    #[test]
    fn explicit_ratio_is_used_as_written() {
        let project = make_project(120.0, 1.25, StretchMode::Percussive, false);
        let resolved = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert_eq!(resolved.ratio, 1.25);
        assert_eq!(resolved.mode, StretchMode::Percussive);
        assert!(!resolved.is_bypass());
    }

    #[test]
    fn tempo_follow_fits_the_range_to_the_media() {
        // [1:1 - 3:1] is four seconds at 120 BPM; two seconds of media needs 2.0.
        let project = make_project(120.0, 1.0, StretchMode::Musical, true);
        let resolved = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert_eq!(resolved.ratio, 2.0);

        let slow = make_project(60.0, 1.0, StretchMode::Musical, true);
        let resolved = resolve(&slow, slow.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert_eq!(resolved.ratio, 4.0);
    }

    #[test]
    fn ratio_one_and_mode_off_are_bypasses() {
        let unity = make_project(120.0, 1.0, StretchMode::Musical, false);
        assert!(
            resolve(&unity, unity.audio_clip(0).unwrap(), 48_000, 96_000)
                .unwrap()
                .is_bypass()
        );
        let off = make_project(120.0, 1.75, StretchMode::Off, false);
        let resolved = resolve(&off, off.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert!(resolved.is_bypass());
        assert_eq!(resolved.ratio, 1.0);
    }

    #[test]
    fn out_of_range_ratios_are_refused_with_the_clip_name() {
        for value in [0.1, 4.5, f64::NAN] {
            let mut project = make_project(120.0, 1.5, StretchMode::Musical, false);
            project.audio_clip_mut(0).unwrap().stretch = value;
            let error =
                resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap_err();
            assert!(error.contains("audio clip 'hit'"), "{error}");
            assert!(error.contains("outside"), "{error}");
        }
    }

    #[test]
    fn tempo_follow_outside_the_range_is_refused() {
        // Eight seconds of media in the four-second range needs 0.5.
        let project = make_project(120.0, 1.0, StretchMode::Musical, true);
        let resolved = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 384_000).unwrap();
        assert_eq!(resolved.ratio, 0.5);

        // Thirty-two seconds of media needs 0.125.
        let error =
            resolve(&project, project.audio_clip(0).unwrap(), 48_000, 1_536_000).unwrap_err();
        assert!(error.contains("outside"), "{error}");
    }

    #[test]
    fn pitch_composes_with_the_stretch_ratio_and_the_duration_holds() {
        let project = make_project(120.0, 1.25, StretchMode::Musical, false);
        let mut planned = project.clone();
        planned.audio_clip_mut(0).unwrap().pitch = 12.0;
        let plan = resolve(&planned, planned.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert_eq!(plan.pitch_rate, 24_000);
        assert_eq!(plan.pitch_ratio, 2.0);
        assert_eq!(plan.ratio, 2.5);
        assert_eq!(plan.pitched_frames(96_000, 48_000), 48_000);
        assert!(!plan.is_bypass());

        // Two seconds of media stay two seconds of clip at every transposition.
        let tone = tone(2.0, 440.0);
        for semitones in [12.0, -12.0, 5.0, 0.5] {
            let mut project = project.clone();
            project.audio_clip_mut(0).unwrap().pitch = semitones;
            project.audio_clip_mut(0).unwrap().stretch = 1.0;
            let plan = resolve(
                &project,
                project.audio_clip(0).unwrap(),
                48_000,
                tone.len() / 2,
            )
            .unwrap();
            let baked = plan
                .bake(&tone, 48_000, ResampleQuality::DEFAULT_RENDER)
                .unwrap();
            let expected =
                stretched_frames(plan.pitched_frames(tone.len() / 2, 48_000), plan.ratio);
            assert_eq!(baked.len() / 2, expected, "{semitones}");
            assert_eq!(baked.len() / 2, tone.len() / 2, "{semitones}");
        }
    }

    #[test]
    fn pitch_zero_keeps_the_plan_a_bypass_and_bakes_the_same_samples() {
        let project = make_project(120.0, 1.5, StretchMode::Percussive, false);
        let plan = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert_eq!(plan.pitch_rate, 0);
        assert_eq!(plan.pitch_ratio, 1.0);
        assert_eq!(plan.ratio, 1.5);

        let input = click_train(20_000, 3_000);
        let baked = plan
            .bake(&input, 48_000, ResampleQuality::DEFAULT_RENDER)
            .unwrap();
        assert_eq!(baked, render(&input, 1.5, StretchMode::Percussive).unwrap());

        let unity = make_project(120.0, 1.0, StretchMode::Musical, false);
        let plan = resolve(&unity, unity.audio_clip(0).unwrap(), 48_000, 96_000).unwrap();
        assert!(plan.is_bypass());
    }

    #[test]
    fn pitch_outside_the_range_is_refused_with_the_clip_name() {
        for value in [24.5, -24.5, f64::NAN, f64::INFINITY] {
            let mut project = make_project(120.0, 1.0, StretchMode::Musical, false);
            project.audio_clip_mut(0).unwrap().pitch = value;
            let error =
                resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap_err();
            assert!(error.contains("audio clip 'hit'"), "{error}");
            assert!(error.contains("pitch"), "{error}");
            assert!(error.contains("-24..=24"), "{error}");
        }
    }

    #[test]
    fn pitch_with_mode_off_is_refused() {
        let mut project = make_project(120.0, 1.0, StretchMode::Off, false);
        project.audio_clip_mut(0).unwrap().pitch = 12.0;
        let error = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap_err();
        assert!(error.contains("audio clip 'hit'"), "{error}");
        assert!(error.contains("stretch mode other than off"), "{error}");
    }

    #[test]
    fn a_pitch_that_pushes_the_ratio_over_the_bound_is_refused() {
        let mut project = make_project(120.0, 4.0, StretchMode::Musical, false);
        project.audio_clip_mut(0).unwrap().pitch = 12.0;
        let error = resolve(&project, project.audio_clip(0).unwrap(), 48_000, 96_000).unwrap_err();
        assert!(error.contains("outside 0.25..=4"), "{error}");
    }

    #[test]
    fn stretched_length_rounds_to_the_nearest_frame() {
        assert_eq!(stretched_frames(96_000, 2.0), 192_000);
        assert_eq!(stretched_frames(96_001, 0.5), 48_001);
        assert_eq!(stretched_frames(3, 1.25), 4);
        assert_eq!(stretched_frames(0, 2.0), 1);
    }

    fn tone(seconds: f64, frequency: f64) -> Vec<f32> {
        let frames = (seconds * 48_000.0).round() as usize;
        let mut samples = vec![0.0_f32; frames * 2];
        for frame in 0..frames {
            let value =
                (std::f64::consts::TAU * frequency * frame as f64 / 48_000.0).sin() as f32 * 0.4;
            samples[frame * 2] = value;
            samples[frame * 2 + 1] = value;
        }
        samples
    }

    fn click_train(frames: usize, spacing: usize) -> Vec<f32> {
        let mut samples = vec![0.0_f32; frames * 2];
        for frame in (0..frames).step_by(spacing) {
            samples[frame * 2] = 1.0;
            samples[frame * 2 + 1] = 1.0;
        }
        samples
    }

    /// Rising edges of the left channel above 10% of the peak.
    fn click_positions(samples: &[f32]) -> Vec<usize> {
        let peak = samples
            .iter()
            .fold(0.0_f32, |max, sample| max.max(sample.abs()));
        if peak <= 0.0 {
            return Vec::new();
        }
        let mut positions = Vec::new();
        let mut frame = 0;
        while frame < samples.len() / 2 {
            if samples[frame * 2].abs() > peak * 0.1 {
                positions.push(frame);
                while frame < samples.len() / 2 && samples[frame * 2].abs() > peak * 0.01 {
                    frame += 1;
                }
            }
            frame += 1;
        }
        positions
    }

    #[test]
    fn unity_returns_the_input_and_silence_stays_zero() {
        let input = click_train(4096, 512);
        assert_eq!(wsola(&input, 1.0).unwrap(), input);
        let silence = vec![0.0_f32; 8192];
        let stretched = wsola(&silence, 2.0).unwrap();
        assert!(stretched.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn output_length_rounds_to_the_ratio() {
        let frames = 10_000;
        let input = vec![0.25_f32; frames * 2];
        for ratio in [0.5, 0.75, 1.25, 1.5, 2.0, 3.0] {
            let output = wsola(&input, ratio).unwrap();
            assert_eq!(output.len() / 2, stretched_frames(frames, ratio), "{ratio}");
        }
    }

    #[test]
    fn dc_offset_is_preserved_without_oscillation() {
        let input = vec![0.25_f32; 48_000 * 2];
        let output = wsola(&input, 1.25).unwrap();
        // The last window runs out of source and fades; check the steady state.
        for (index, sample) in output
            .iter()
            .enumerate()
            .take(output.len() - 1_000)
            .skip(200)
        {
            assert!(
                (sample - 0.25).abs() < 1e-3,
                "DC deviated to {sample} at {index} of {}",
                output.len()
            );
        }
    }

    #[test]
    fn a_sustained_ramp_stays_present_across_a_percussive_warp_boundary() {
        let input = (0..144_000)
            .flat_map(|frame| {
                let level = frame as f32 * 1e-6;
                [level, level * 0.5]
            })
            .collect::<Vec<_>>();
        let mut plan = StretchPlan::stretched(1.25, StretchMode::Percussive);
        let anchor = 12_000;
        plan.warp = vec![(9_600, anchor), (144_000, 180_000)];
        let output = plan
            .stretch_samples(&input, ClipCancellation::default())
            .unwrap();
        assert_eq!(output.len(), 180_000 * 2);
        let seam = &output[(anchor - 240) * 2..(anchor + 240) * 2];
        for (offset, frame) in seam.chunks_exact(2).enumerate() {
            assert!(
                frame[0] > 0.004,
                "the warp cut a sustained signal to {} at frame {}",
                frame[0],
                anchor - 240 + offset
            );
            assert_eq!(frame[1], frame[0] * 0.5);
        }
        let jump = (output[anchor * 2] - output[(anchor - 1) * 2]).abs();
        assert!(jump < 1e-6, "splice step {jump}");
    }

    #[test]
    fn click_train_keeps_one_hit_per_onset_at_two_to_one() {
        let input = click_train(50_000, 4_800);
        let output = wsola(&input, 2.0).unwrap();
        let positions = click_positions(&output);
        assert_eq!(positions.len(), 11, "smear or double hits: {positions:?}");
        for (index, position) in positions.iter().enumerate() {
            let expected = index * 9_600;
            assert!(
                (*position as i64 - expected as i64).abs() <= 96,
                "onset {index} at {position}, expected {expected}"
            );
        }
    }

    #[test]
    fn hard_left_signal_keeps_the_right_channel_silent() {
        let mut input = vec![0.0_f32; 48_000 * 2];
        for frame in 0..48_000 {
            input[frame * 2] =
                (std::f32::consts::TAU * 440.0 * frame as f32 / 48_000.0).sin() * 0.5;
        }
        let output = wsola(&input, 1.5).unwrap();
        assert!(
            output
                .iter()
                .skip(1)
                .step_by(2)
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn short_input_keeps_its_length_and_attack() {
        let mut input = vec![0.0_f32; 100 * 2];
        input[0] = 1.0;
        input[1] = 1.0;
        let output = wsola(&input, 2.0).unwrap();
        assert_eq!(output.len() / 2, 200);
        assert_eq!(output[0], 1.0);
        assert_eq!(output[1], 1.0);
    }

    #[test]
    fn invalid_ratios_and_layouts_are_refused() {
        assert!(wsola(&[0.0; 8], 5.0).is_err());
        assert!(wsola(&[0.0; 8], f64::NAN).is_err());
        assert!(wsola(&[0.0; 7], 2.0).is_err());
    }

    #[test]
    fn stretching_is_deterministic() {
        let input = click_train(20_000, 3_000);
        let first = wsola(&input, 1.5).unwrap();
        let second = wsola(&input, 1.5).unwrap();
        assert_eq!(first, second);
    }
}
