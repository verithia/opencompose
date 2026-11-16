//! Offline waveform preview for the patch editor.
//!
//! The preview renders through [`OpenSynth`], so the trace is the same signal
//! the instrument plays for that oscillator. It is an offline call: building
//! the synth and capturing the traces allocate, and nothing here runs on the
//! audio callback.

use super::*;

/// Preview pitch is a fixed note so traces are comparable between patches.
pub const PREVIEW_NOTE: u8 = 48;
/// Preview velocity only matters for patches with `velocity_to_amp`; the
/// isolated preview patch disables that anyway.
pub const PREVIEW_VELOCITY: u8 = 100;
/// Rate used for waveform previews.
pub const PREVIEW_SAMPLE_RATE: u32 = 48_000;
/// Default span: ~2.8 cycles of C3 at 48 kHz.
pub const DEFAULT_PREVIEW_SAMPLES: usize = 1024;
/// Enough samples for the minimum attack (1 ms) to open before capture.
pub const PREVIEW_PREROLL: usize = 128;
/// Fewest frames a preview may request.
pub const MIN_PREVIEW_SAMPLES: usize = 16;
/// Most frames a preview may request.
pub const MAX_PREVIEW_SAMPLES: usize = 4096;

/// One rendered oscillator preview: the mono trace, the per-unison-voice
/// traces, and level readouts.
#[derive(Debug, Clone, Serialize)]
pub struct WaveformPreview {
    /// Summed mono output, one sample per frame.
    pub mono: Vec<f32>,
    /// Per-unison-voice contributions, capped at `MAX_PREVIEW_TRACES`.
    pub traces: Vec<Vec<f32>>,
    /// True when the stack has more voices than `traces` holds.
    pub more_voices: bool,
    /// Unison voice count of the previewed oscillator.
    pub voices: usize,
    /// Largest absolute mono sample.
    pub peak: f32,
    /// Root-mean-square of the mono trace.
    pub rms: f32,
    /// Number of mono samples.
    pub samples: usize,
    pub sample_rate: u32,
    /// MIDI note the preview was rendered at.
    pub note: u8,
    /// sync window restarts inside the captured span.
    pub sync_resets: usize,
}

/// Isolate one oscillator slot for display: only that slot is audible, the
/// filter is bypassed and the amplitude envelope is held open. Without this
/// the filter and the note attack would colour the shape being edited.
fn isolated_patch(patch: &SynthPatch, slot: usize) -> SynthPatch {
    let mut isolated = patch.clone();
    isolated.oscillator_a_level = if slot == 0 { 1.0 } else { 0.0 };
    isolated.oscillator_b_level = if slot == 1 { 1.0 } else { 0.0 };
    isolated.oscillator_c_level = if slot == 2 { 1.0 } else { 0.0 };
    isolated.ring_mod_level = 0.0;
    isolated.sub_level = 0.0;
    isolated.noise_level = 0.0;
    isolated.filter_wet = 0.0;
    isolated.filter2_enabled = false;
    isolated.filter2_wet = 0.0;
    isolated.filter_envelope_amount = 0.0;
    isolated.attack = 0.0;
    isolated.decay = 0.0;
    isolated.sustain = 1.0;
    isolated.release = 0.1;
    isolated.velocity_to_amp = 0.0;
    isolated.gain = 1.0;
    isolated.glide_time = 0.0;
    isolated.arpeggiator = None;
    isolated.sampler = None;
    isolated.fx.clear();
    isolated.fx_mod_routes.clear();
    // The preview pitch is fixed; global transpose belongs to the patch, not
    // to the shape of one oscillator.
    isolated.tuning_transpose = 0.0;
    isolated.tuning_fine_cents = 0.0;
    isolated
}

/// Reusable offline preview renderer. Holding one keeps the FX rack's buffers
/// across calls, so a parameter drag does not rebuild the reverb/delay state.
#[derive(Default)]
pub struct WaveformPreviewer {
    fx_chain: Option<fx::SynthFxChain>,
}

impl WaveformPreviewer {
    /// Create a previewer with no captured voices.
    pub fn new() -> Self {
        Self::default()
    }

    /// Render `samples` frames of one oscillator slot at C3 and capture each
    /// unison voice. `slot` is 0/1/2 for A/B/C; out-of-range slots clamp to C.
    pub fn render(
        &mut self,
        patch: &SynthPatch,
        slot: usize,
        samples: usize,
        sample_rate: u32,
    ) -> WaveformPreview {
        let slot = slot.min(2);
        let samples = samples.clamp(MIN_PREVIEW_SAMPLES, MAX_PREVIEW_SAMPLES);
        let sample_rate = sample_rate.clamp(8_000, 384_000);
        let isolated = isolated_patch(patch, slot);
        let fx_chain = self.fx_chain.take().unwrap_or_default();
        let mut synth = OpenSynth::new_with_fx_chain(isolated, 120.0, fx_chain);
        synth.note_on(0, PREVIEW_NOTE, PREVIEW_VELOCITY);
        // Let the minimum attack open before the trace starts.
        for _ in 0..PREVIEW_PREROLL {
            let _ = synth.next_stereo(sample_rate);
        }
        synth.start_unison_capture(slot, samples);
        let mut mono = Vec::with_capacity(samples);
        let mut peak = 0.0f32;
        let mut sum_squares = 0.0f64;
        for _ in 0..samples {
            let [left, right] = synth.next_stereo(sample_rate);
            let sample = 0.5 * (left + right);
            peak = peak.max(sample.abs());
            sum_squares += f64::from(sample) * f64::from(sample);
            mono.push(sample);
        }
        let capture = synth.take_unison_capture().unwrap_or_default();
        let rms = (sum_squares / samples as f64).sqrt() as f32;
        self.fx_chain = Some(synth.into_fx_chain());
        WaveformPreview {
            mono,
            traces: capture.traces,
            more_voices: capture.more_voices,
            voices: capture.voices,
            peak,
            rms,
            samples,
            sample_rate,
            note: PREVIEW_NOTE,
            sync_resets: capture.resets,
        }
    }
}

/// One-shot preview; prefer [`WaveformPreviewer`] when rendering repeatedly.
pub fn waveform_preview(
    patch: &SynthPatch,
    slot: usize,
    samples: usize,
    sample_rate: u32,
) -> WaveformPreview {
    WaveformPreviewer::new().render(patch, slot, samples, sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saw_patch() -> SynthPatch {
        SynthPatch {
            oscillator_a_waveform: Waveform::Saw,
            oscillator_a_level: 0.8,
            unison_voices: 2,
            detune_cents: 7.0,
            amp_curve: 0.0,
            ..Default::default()
        }
    }

    fn rms_difference(left: &[f32], right: &[f32]) -> f64 {
        let count = left.len().min(right.len()).max(1);
        let sum: f64 = left
            .iter()
            .zip(right)
            .map(|(a, b)| f64::from(a - b).powi(2))
            .sum();
        (sum / count as f64).sqrt()
    }

    fn rms_of(samples: &[f32]) -> f64 {
        let count = samples.len().max(1);
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / count as f64).sqrt()
    }

    #[test]
    fn preview_matches_a_direct_voice_render_sample_exact() {
        let patch = SynthPatch {
            oscillator_a_waveform: Waveform::Wavetable,
            oscillator_a_wavetable_position: 0.58,
            oscillator_a_warp_mode: WarpMode::BendPos,
            oscillator_a_warp_amount: 0.6,
            unison_voices: 5,
            detune_cents: 17.1,
            stereo_width: 0.4,
            ..saw_patch()
        };
        let preview = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE);
        let direct_patch = isolated_patch(&patch, 0);
        let event = MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: PREVIEW_NOTE,
                velocity: PREVIEW_VELOCITY,
            },
            track_index: 0,
        };
        let direct = render_track_with_patch(
            &[&event],
            PREVIEW_PREROLL + DEFAULT_PREVIEW_SAMPLES,
            PREVIEW_SAMPLE_RATE,
            direct_patch,
            120.0,
        );
        let mut max_error = 0.0f64;
        for (index, sample) in preview.mono.iter().enumerate() {
            let frame = PREVIEW_PREROLL + index;
            let mono = 0.5 * (direct[frame * 2] + direct[frame * 2 + 1]);
            max_error = max_error.max(f64::from((mono - sample).abs()));
        }
        eprintln!("preview vs engine render: max |error| = {max_error:e}");
        assert!(
            max_error <= 1.0e-6,
            "preview diverged from the engine render by {max_error}"
        );
        assert_eq!(preview.samples, DEFAULT_PREVIEW_SAMPLES);
        assert_eq!(preview.voices, 5);
        assert_eq!(preview.traces.len(), 5);
        assert!(!preview.more_voices);
    }

    #[test]
    fn warp_amount_changes_the_rendered_waveform() {
        let mut patch = saw_patch();
        patch.unison_voices = 1;
        patch.detune_cents = 0.0;
        patch.oscillator_a_warp_mode = WarpMode::Sync;
        patch.oscillator_a_warp_amount = 0.0;
        let off = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE).mono;
        patch.oscillator_a_warp_amount = 0.8;
        let warped = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE).mono;
        // Stated threshold: the waveform itself changes, not just its level.
        let difference = rms_difference(&off, &warped);
        eprintln!("warp 0 -> 0.8 difference RMS = {difference:.6}");
        assert!(
            difference > 0.1,
            "warp 0 -> 0.8 changed the trace by {difference} RMS"
        );
    }

    #[test]
    fn sync_warp_restarts_once_per_oscillator_cycle() {
        // The engine's sync window advances at the unwarped rate, so the
        // restart count is the number of base cycles in the captured span;
        // amount changes the phase increment (brightness), not that count.
        let mut patch = SynthPatch {
            oscillator_a_waveform: Waveform::Sine,
            oscillator_a_warp_mode: WarpMode::Sync,
            oscillator_a_warp_amount: 0.8,
            unison_voices: 1,
            detune_cents: 0.0,
            ..Default::default()
        };
        let frames = DEFAULT_PREVIEW_SAMPLES;
        let preview = waveform_preview(&patch, 0, frames, PREVIEW_SAMPLE_RATE);
        let frequency = 440.0 * 2f64.powf((f64::from(PREVIEW_NOTE) - 69.0) / 12.0);
        let increment = frequency / f64::from(PREVIEW_SAMPLE_RATE);
        let start = (PREVIEW_PREROLL as f64 * increment).fract();
        let expected = (start + frames as f64 * increment).floor() as usize;
        assert_eq!(
            preview.sync_resets, expected,
            "sync restarts must equal the oscillator cycles in the span"
        );
        assert!(preview.sync_resets > 0);
        // Restarts are a property of the mode, not of the amount knob.
        patch.oscillator_a_warp_amount = 0.0;
        let quiet = waveform_preview(&patch, 0, frames, PREVIEW_SAMPLE_RATE);
        assert_eq!(quiet.sync_resets, preview.sync_resets);
    }

    #[test]
    fn unison_detune_zero_keeps_traces_identical_and_30_cents_diverges() {
        let mut patch = saw_patch();
        patch.unison_voices = 2;
        patch.detune_cents = 0.0;
        let coherent = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE);
        assert_eq!(coherent.voices, 2);
        assert_eq!(coherent.traces.len(), 2);
        assert_eq!(
            coherent.traces[0], coherent.traces[1],
            "zero detune must keep the voices phase-coherent"
        );

        patch.detune_cents = 30.0;
        let detuned = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE);
        let spread = rms_difference(&detuned.traces[0], &detuned.traces[1])
            / rms_of(&detuned.traces[0])
                .max(rms_of(&detuned.traces[1]))
                .max(1.0e-9);
        eprintln!("30-cent voice trace spread = {spread:.6}");
        assert!(
            spread > 0.1,
            "30 cents spread the voice traces by only {spread}"
        );
    }

    #[cfg(feature = "perf-tests")]
    #[test]
    fn preview_renders_1024_samples_of_sixteen_voices_quickly() {
        let mut patch = saw_patch();
        patch.unison_voices = MAX_UNISON_VOICES;
        patch.detune_cents = 17.1;
        // Warm run: the first call builds the shared wavetable bank and the
        // previewer keeps the FX rack for subsequent renders.
        let mut previewer = WaveformPreviewer::new();
        let _ = previewer.render(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE);
        let mut timings = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let preview = std::hint::black_box(previewer.render(
                &patch,
                0,
                DEFAULT_PREVIEW_SAMPLES,
                PREVIEW_SAMPLE_RATE,
            ));
            std::hint::black_box(preview);
            timings.push(start.elapsed().as_secs_f64() * 1_000.0);
        }
        timings.sort_by(f64::total_cmp);
        let median = timings[timings.len() / 2];
        eprintln!("16-voice 1024-sample preview: {median:.3} ms (median of 5)");
        let budget = if cfg!(debug_assertions) { 10.0 } else { 2.0 };
        assert!(
            median < budget,
            "preview took {median:.3} ms, budget {budget:.1} ms"
        );
    }

    #[test]
    fn analysis_shape_and_level_readouts_describe_the_mono_trace() {
        let patch = saw_patch();
        let preview = waveform_preview(&patch, 0, DEFAULT_PREVIEW_SAMPLES, PREVIEW_SAMPLE_RATE);
        let mut peak = 0.0f32;
        let mut sum = 0.0f64;
        for sample in &preview.mono {
            peak = peak.max(sample.abs());
            sum += f64::from(*sample).powi(2);
        }
        assert!((preview.peak - peak).abs() < 1.0e-6);
        assert!((preview.rms - (sum / preview.samples as f64).sqrt() as f32).abs() < 1.0e-6);
    }
}
