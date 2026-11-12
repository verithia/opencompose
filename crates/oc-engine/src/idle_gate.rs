//! Streaming-only idle gate for hosted plugins.
//!
//! A hosted plugin that has rendered silence for [`IDLE_SECONDS`] while it
//! received no input stops being called until input arrives again. The gate
//! fades the contribution out before the first skipped block and resumes one
//! block before the fade-in, so the splice never steps.

use crate::dsp::AudioEffect;

/// Reserved effect parameter that keeps a hosted plugin processing while its
/// lane is silent. The gate is enabled unless it is set to `false`.
pub const SMART_DISABLE_PARAM: &str = "smart_disable";

/// Continuous silence, in render time, before a plugin stops processing.
pub const IDLE_SECONDS: f64 = 4.0;

/// Block peak at or below which output counts as silence (-80 dBFS).
pub const SILENCE_PEAK: f32 = 1.0e-4;

/// Length of the pause/resume fade, in samples (5.3 ms at 48 kHz).
pub const RAMP_SAMPLES: usize = 256;

/// What the caller does with the block the gate returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateAction {
    /// Run the plugin and keep its output.
    Process,
    /// Run the plugin to advance its state, then discard the block.
    Mute,
    /// Do not call the plugin; pass the near-silent input through.
    Skip,
}

/// Fade applied to one block after the plugin ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRamp {
    /// Ramp up from silence over the first `samples` frames.
    FadeIn { samples: usize },
    /// Ramp down to silence over the last `samples` frames.
    FadeOut { samples: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateState {
    Running,
    /// Processed and faded out; later blocks are skipped.
    Paused,
    /// Resumed with a muted block; the next block fades in.
    FadeInPending,
}

/// Per-instance state for the streaming smart-disable rule.
///
/// The gate is inert until a streaming renderer enables it, and it stays inert
/// when the plugin reports a tail at least as long as the idle window or when
/// the user opts out.
#[derive(Clone, Copy, Debug)]
pub struct IdleGate {
    idle_frames: u64,
    /// The plugin's tail ends before the idle window.
    tail_ok: bool,
    opt_out: bool,
    streaming: bool,
    silent_frames: u64,
    state: GateState,
    /// Fade due after the current block runs.
    ramp: Option<GateRamp>,
}

impl IdleGate {
    /// Gate for a hosted plugin. `tail_samples` is `None` when the format
    /// reports no tail; such a plugin never pauses.
    pub fn new(sample_rate: u32, tail_samples: Option<u32>, opt_out: bool) -> Self {
        let idle_frames = (IDLE_SECONDS * f64::from(sample_rate)).round().max(1.0) as u64;
        let tail_ok = tail_samples.is_some_and(|tail| u64::from(tail) < idle_frames);
        Self {
            idle_frames,
            tail_ok,
            opt_out,
            streaming: false,
            silent_frames: 0,
            state: GateState::Running,
            ramp: None,
        }
    }

    /// Gate that never pauses anything.
    pub fn disabled() -> Self {
        Self {
            idle_frames: 0,
            tail_ok: false,
            opt_out: false,
            streaming: false,
            silent_frames: 0,
            state: GateState::Running,
            ramp: None,
        }
    }

    /// True when this gate is allowed to pause the plugin at all.
    pub fn is_active(&self) -> bool {
        self.streaming && self.tail_ok && !self.opt_out
    }

    /// Enable the gate for the streaming renderer. Disabling it also clears
    /// any pause so a later offline pass never skips a block.
    pub fn set_streaming(&mut self, streaming: bool) {
        if !streaming {
            self.state = GateState::Running;
            self.silent_frames = 0;
            self.ramp = None;
        }
        self.streaming = streaming;
    }

    /// Forget the run of silent blocks and any pending fade, keeping the
    /// gate's configuration. A rewound lane starts as if it were new.
    pub fn rewind(&mut self) {
        self.silent_frames = 0;
        self.state = GateState::Running;
        self.ramp = None;
    }

    /// Decide this block. `input_silent` is true when the plugin received no
    /// audio (an insert) or no event (an instrument) for the block.
    pub fn begin(&mut self, input_silent: bool) -> GateAction {
        if !self.is_active() {
            return GateAction::Process;
        }
        match self.state {
            GateState::Paused if input_silent => GateAction::Skip,
            GateState::Paused => {
                self.state = GateState::FadeInPending;
                GateAction::Mute
            }
            GateState::FadeInPending => {
                self.state = GateState::Running;
                self.ramp = Some(GateRamp::FadeIn {
                    samples: RAMP_SAMPLES,
                });
                GateAction::Process
            }
            GateState::Running => GateAction::Process,
        }
    }

    /// Update the gate after the plugin ran and return the fade to apply.
    pub fn end(&mut self, input_silent: bool, output_peak: f32, frames: usize) -> Option<GateRamp> {
        if !self.is_active() {
            return None;
        }
        if let Some(ramp) = self.ramp.take() {
            return Some(ramp);
        }
        if !input_silent || output_peak > SILENCE_PEAK {
            self.silent_frames = 0;
            return None;
        }
        self.silent_frames = self.silent_frames.saturating_add(frames as u64);
        if self.silent_frames < self.idle_frames {
            return None;
        }
        self.silent_frames = 0;
        self.state = GateState::Paused;
        Some(GateRamp::FadeOut {
            samples: RAMP_SAMPLES.min(frames),
        })
    }

    /// Block size the gate needs to reach the idle window, for tests.
    pub fn idle_frames(&self) -> u64 {
        self.idle_frames
    }
}

/// Peak absolute sample of an interleaved buffer.
pub fn buffer_peak(buffer: &[f32]) -> f32 {
    buffer.iter().fold(0.0_f32, |peak, sample| {
        peak.max(if sample.is_finite() {
            sample.abs()
        } else {
            0.0
        })
    })
}

/// Apply a fade to an interleaved stereo buffer.
pub fn apply_ramp(buffer: &mut [f32], ramp: GateRamp) {
    let frames = buffer.len() / 2;
    let (samples, fade_in) = match ramp {
        GateRamp::FadeIn { samples } => (samples.min(frames), true),
        GateRamp::FadeOut { samples } => (samples.min(frames), false),
    };
    if samples == 0 {
        return;
    }
    for index in 0..samples {
        let gain = ramp_gain(index, samples, fade_in);
        let frame = if fade_in {
            index
        } else {
            frames - samples + index
        };
        for channel in 0..2 {
            buffer[frame * 2 + channel] *= gain;
        }
    }
}

/// Raised-cosine gain: starts and ends on a zero slope, so the first and last
/// steps are far below the click threshold.
fn ramp_gain(index: usize, samples: usize, fade_in: bool) -> f32 {
    if samples <= 1 {
        return if fade_in { 1.0 } else { 0.0 };
    }
    let phase = std::f32::consts::PI * index as f32 / (samples - 1) as f32;
    if fade_in {
        0.5 * (1.0 - phase.cos())
    } else {
        0.5 * (1.0 + phase.cos())
    }
}

/// An effect wrapped in its idle gate.
///
/// The wrapper sits outside every other chain wrapper so one call covers a
/// whole render block, not an automation or clock segment.
pub(crate) struct IdleGatedPlugin {
    inner: Box<dyn AudioEffect>,
    gate: IdleGate,
}

impl IdleGatedPlugin {
    pub(crate) fn new(inner: Box<dyn AudioEffect>, gate: IdleGate) -> Self {
        Self { inner, gate }
    }

    fn run(&mut self, buffer: &mut [f32], run: impl FnOnce(&mut dyn AudioEffect, &mut [f32])) {
        let frames = buffer.len() / 2;
        if !self.gate.is_active() {
            run(self.inner.as_mut(), buffer);
            return;
        }
        let input_silent = buffer_peak(buffer) <= SILENCE_PEAK;
        match self.gate.begin(input_silent) {
            // The input is already below the silence threshold; passing it
            // through is what the paused plugin would have produced.
            GateAction::Skip => {}
            GateAction::Mute => {
                run(self.inner.as_mut(), buffer);
                buffer.fill(0.0);
            }
            GateAction::Process => {
                run(self.inner.as_mut(), buffer);
                let peak = buffer_peak(buffer);
                if let Some(ramp) = self.gate.end(input_silent, peak, frames) {
                    apply_ramp(buffer, ramp);
                }
            }
        }
    }
}

impl AudioEffect for IdleGatedPlugin {
    fn processing_failed(&self) -> bool {
        self.inner.processing_failed()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        self.inner.take_processing_failure()
    }

    fn prepare_block(&mut self, max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        self.inner.prepare_block(max_frames)
    }

    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.run(buffer, |inner, buffer| inner.process(buffer, sample_rate));
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<oc_audio::PluginTransport>,
    ) {
        self.run(buffer, |inner, buffer| {
            inner.process_with_transport(buffer, sample_rate, transport);
        });
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        sidechain: &[f32],
        sample_rate: u32,
        transport: Option<oc_audio::PluginTransport>,
    ) {
        self.run(buffer, |inner, buffer| {
            inner.process_with_sidechain(buffer, sidechain, sample_rate, transport);
        });
    }

    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        control: &[f32],
        sample_rate: u32,
        transport: Option<oc_audio::PluginTransport>,
    ) {
        self.run(buffer, |inner, buffer| {
            inner.process_with_control(buffer, control, sample_rate, transport);
        });
    }

    fn supports_sidechain(&self) -> bool {
        self.inner.supports_sidechain()
    }

    fn sidechain_source(&self) -> Option<&str> {
        self.inner.sidechain_source()
    }

    fn supports_control_input(&self) -> bool {
        self.inner.supports_control_input()
    }

    fn control_source(&self) -> Option<&str> {
        self.inner.control_source()
    }

    fn configure_sidechain_alignment(&mut self, main_latency: u32, sidechain_latency: u32) -> u32 {
        self.inner
            .configure_sidechain_alignment(main_latency, sidechain_latency)
    }

    fn set_param(&mut self, name: &str, value: f64) {
        self.inner.set_param(name, value);
    }

    fn queue_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        self.inner.queue_param_at(name, value, sample_offset)
    }

    fn supports_param_offsets(&self) -> bool {
        self.inner.supports_param_offsets()
    }

    fn max_block_frames(&self) -> u32 {
        self.inner.max_block_frames()
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        self.inner.get_param(name)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn end_processing(&mut self) {
        self.inner.end_processing();
    }

    fn latency_samples(&self) -> u32 {
        self.inner.latency_samples()
    }

    fn tail_samples(&self) -> u32 {
        self.inner.tail_samples()
    }

    fn bounce_tail_samples(&self) -> u32 {
        self.inner.bounce_tail_samples()
    }

    fn plugin_tail_samples(&self) -> Option<u32> {
        self.inner.plugin_tail_samples()
    }

    fn gain_reduction_db(&self) -> f64 {
        self.inner.gain_reduction_db()
    }

    fn band_count(&self) -> usize {
        self.inner.band_count()
    }

    fn band_gain_db(&self, index: usize) -> Option<f64> {
        self.inner.band_gain_db(index)
    }

    fn max_gain_reduction_db(&self) -> f64 {
        self.inner.max_gain_reduction_db()
    }

    fn ceiling_margin_db(&self) -> Option<f64> {
        self.inner.ceiling_margin_db()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn set_streaming_idle_gate(&mut self, enabled: bool) {
        self.gate.set_streaming(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 128-frame stereo block of silence at 48 kHz.
    const BLOCK: usize = 128;

    fn silent_block() -> Vec<f32> {
        vec![0.0; BLOCK * 2]
    }

    /// Model one gated block exactly like `IdleGatedPlugin`.
    fn run(gate: &mut IdleGate, input: &[f32], output: &mut [f32]) -> GateAction {
        let input_silent = buffer_peak(input) <= SILENCE_PEAK;
        let action = gate.begin(input_silent);
        match action {
            GateAction::Skip => output.fill(0.0),
            GateAction::Mute => output.fill(0.0),
            GateAction::Process => {
                output.copy_from_slice(input);
                let peak = buffer_peak(output);
                if let Some(ramp) = gate.end(input_silent, peak, BLOCK) {
                    apply_ramp(output, ramp);
                }
            }
        }
        action
    }

    #[test]
    fn silence_pauses_only_while_the_plugin_has_no_input() {
        let mut gate = IdleGate::new(48_000, Some(0), false);
        gate.set_streaming(true);
        let mut output = silent_block();
        let blocks = gate.idle_frames() / BLOCK as u64;
        for _ in 0..blocks {
            assert_eq!(
                run(&mut gate, &silent_block(), &mut output),
                GateAction::Process
            );
        }
        assert_eq!(
            run(&mut gate, &silent_block(), &mut output),
            GateAction::Skip
        );
        // Input resumes processing on the same block, muted, then fades in.
        let input = vec![0.5; BLOCK * 2];
        assert_eq!(run(&mut gate, &input, &mut output), GateAction::Mute);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert_eq!(run(&mut gate, &input, &mut output), GateAction::Process);
        assert_eq!(output[0], 0.0, "the fade-in starts at silence");
        assert!(
            output[(BLOCK - 1) * 2].abs() > 0.4,
            "the fade-in reaches the signal by the end of the block"
        );
        // A later block is at full level again.
        assert_eq!(run(&mut gate, &input, &mut output), GateAction::Process);
        assert!(buffer_peak(&output) > 0.4);
    }

    #[test]
    fn input_resets_the_idle_window() {
        let mut gate = IdleGate::new(48_000, Some(0), false);
        gate.set_streaming(true);
        let mut output = silent_block();
        let blocks = gate.idle_frames() / BLOCK as u64;
        let input = vec![0.1; BLOCK * 2];
        for _ in 0..blocks / 2 {
            run(&mut gate, &silent_block(), &mut output);
        }
        // One event or one non-silent block restarts the window.
        run(&mut gate, &input, &mut output);
        for _ in 0..blocks {
            assert_ne!(
                run(&mut gate, &silent_block(), &mut output),
                GateAction::Skip
            );
        }
    }

    #[test]
    fn a_long_tail_or_an_opt_out_keeps_the_plugin_running() {
        for gate in [
            IdleGate::new(48_000, Some(10 * 48_000), false),
            IdleGate::new(48_000, None, false),
            IdleGate::new(48_000, Some(0), true),
        ] {
            let mut gate = gate;
            gate.set_streaming(true);
            assert!(!gate.is_active());
            let mut output = silent_block();
            for _ in 0..3 * 48_000 / BLOCK {
                assert_eq!(
                    run(&mut gate, &silent_block(), &mut output),
                    GateAction::Process
                );
            }
        }
    }

    #[test]
    fn the_seam_steps_below_minus_80_dbfs() {
        let mut gate = IdleGate::new(48_000, Some(0), false);
        gate.set_streaming(true);
        let mut stream: Vec<f32> = Vec::new();
        let mut seams: Vec<usize> = Vec::new();
        let mut output = silent_block();
        let blocks = gate.idle_frames() / BLOCK as u64;
        let loud = vec![0.75; BLOCK * 2];
        // Four seconds of silence: the last silent block fades out, the next
        // block is skipped.
        for _ in 0..blocks - 1 {
            run(&mut gate, &silent_block(), &mut output);
            stream.extend_from_slice(&output);
        }
        assert_eq!(
            run(&mut gate, &silent_block(), &mut output),
            GateAction::Process
        );
        stream.extend_from_slice(&output);
        // Pause seam: last faded sample → first skipped sample.
        seams.push(stream.len());
        assert_eq!(
            run(&mut gate, &silent_block(), &mut output),
            GateAction::Skip
        );
        stream.extend_from_slice(&output);
        // Resume seam: last skipped sample → first muted sample.
        seams.push(stream.len());
        assert_eq!(run(&mut gate, &loud, &mut output), GateAction::Mute);
        stream.extend_from_slice(&output);
        // Fade-in seam: last muted sample → first ramped sample.
        seams.push(stream.len());
        assert_eq!(run(&mut gate, &loud, &mut output), GateAction::Process);
        stream.extend_from_slice(&output);
        let step = seams
            .iter()
            .map(|&index| (stream[index] - stream[index - 1]).abs())
            .fold(0.0_f32, f32::max);
        assert!(step < 1.0e-4, "seam step {step} is not below -80 dBFS");
        // The fade is real: the resumed block starts at silence and rises.
        let resumed = &stream[seams[2]..seams[2] + BLOCK * 2];
        assert_eq!(resumed[0], 0.0);
        assert!(
            resumed[BLOCK * 2 - 2].abs() > 0.5,
            "the fade-in never reaches the signal"
        );
        // Without the fade the same transient would step by its full level.
        assert!(loud[0] > 1.0e-4);
    }
}
