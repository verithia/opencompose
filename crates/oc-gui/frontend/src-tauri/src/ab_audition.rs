//! Aligned streaming A/B audition; offline loudness analysis never gates playback.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use oc_dsl::ast::{Effect, ParamValue};

use crate::audition::{candidate_render_context, factory_preset, unique_factory_preset_name};
use crate::state::AppState;

/// Seconds of the shared figure both candidates are measured over, counted
/// from the first sample. BS.1770-4 gates 400 ms blocks on a 100 ms hop, so
/// 2.0 s yields 17 overlapping blocks — enough for the relative gate to settle
/// — while still covering the whole six-note figure (which spans 1.286 s)
/// plus the beginning of its release. The rendered audition can be
/// longer (the project path adds a ring-out tail); measuring that whole buffer
/// would let the tail, not the notes, decide the number. Pinning the window
/// also keeps the readout reproducible when a patch has a longer effect tail.
pub const MATCH_WINDOW_SECONDS: f64 = 2.0;

/// A candidate whose loudness could not be measured. Kept as a short code so
/// the UI can translate it instead of showing backend prose.
const UNMEASURED_A: &str = "a";
const UNMEASURED_B: &str = "b";
const UNMEASURED_BOTH: &str = "both";
/// Both sides measured, but re-measuring after the gain did not converge.
const UNMEASURED_UNSTABLE: &str = "unstable";

/// What the switch and the readout need to know about the last A/B render.
#[derive(Clone, Debug, Serialize)]
pub struct AbSnapshot {
    pub measurement_pending: bool,
    /// `matched` or `unmeasured`; never claims a match that was not measured.
    pub status: String,
    /// `a` or `b`; the take currently selected on the audition stream.
    pub active: String,
    pub a_label: String,
    pub b_label: String,
    /// A's integrated loudness over the fixed window, after any headroom trim.
    pub a_lufs: Option<f64>,
    /// B's loudness before matching.
    pub b_raw_lufs: Option<f64>,
    /// B's loudness after matching, over the same window.
    pub b_lufs: Option<f64>,
    /// `b_raw_lufs - a_lufs`; the difference the operator would have heard.
    pub raw_delta_lu: Option<f64>,
    /// Gain applied to B to align it with A. Not including the common trim.
    pub applied_gain_db: Option<f64>,
    /// `b_lufs - a_lufs` after matching; the residual, not the applied gain.
    pub matched_delta_lu: Option<f64>,
    /// Why the pair is not matched; one of `a`, `b`, `both`, `unstable`.
    pub unmeasured: Option<String>,
    /// The fixed window used for both takes.
    pub window_seconds: f64,
}

impl AbSnapshot {
    fn empty(a_label: &str, b_label: &str) -> Self {
        Self {
            measurement_pending: true,
            status: "unmeasured".into(),
            active: "b".into(),
            a_label: a_label.to_owned(),
            b_label: b_label.to_owned(),
            a_lufs: None,
            b_raw_lufs: None,
            b_lufs: None,
            raw_delta_lu: None,
            applied_gain_db: None,
            matched_delta_lu: None,
            unmeasured: Some(UNMEASURED_BOTH.into()),
            window_seconds: MATCH_WINDOW_SECONDS,
        }
    }
}

/// One candidate as the command receives it after parameter parsing.
#[derive(Clone, Debug)]
pub struct AbCandidate {
    pub label: String,
    pub params: Vec<(String, ParamValue)>,
    pub effects: Vec<Effect>,
}

#[derive(Default)]
struct AbPair {
    snapshot: Option<AbSnapshot>,
    ready: bool,
}

/// Session state behind the paired streams and their asynchronous measurement.
#[derive(Clone)]
pub struct AbController {
    generation: Arc<AtomicU64>,
    pair: Arc<Mutex<AbPair>>,
}

impl AbController {
    /// Create an A/B controller with generation zero.
    pub fn new() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            pair: Arc::new(Mutex::new(AbPair::default())),
        }
    }

    /// Start a new pair, invalidating any render still preparing the old one.
    pub fn begin(&self, a_label: &str, b_label: &str) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.lock() = AbPair {
            snapshot: Some(AbSnapshot::empty(a_label, b_label)),
            ready: false,
        };
        generation
    }

    /// Whether a worker generation is still current.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AbPair> {
        self.pair.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Current A/B snapshot, if any.
    pub fn snapshot(&self) -> Option<AbSnapshot> {
        self.lock().snapshot.clone()
    }

    fn publish(&self, generation: u64, snapshot: AbSnapshot) -> bool {
        let mut pair = self.lock();
        if !self.is_current(generation) {
            return false;
        }
        *pair = AbPair {
            snapshot: Some(snapshot),
            ready: true,
        };
        true
    }

    fn switch(&self) -> Result<AbSnapshot, String> {
        let mut pair = self.lock();
        if !pair.ready {
            return Err("No A/B streams are ready".into());
        }
        let snapshot = pair.snapshot.as_mut().ok_or("No A/B pair")?;
        snapshot.active = if snapshot.active == "a" { "b" } else { "a" }.into();
        Ok(snapshot.clone())
    }
}

pub(crate) struct AbReservation {
    generation: u64,
    ab_generation: u64,
    epoch: u64,
    started: Instant,
}

/// Claim the private output before dispatch so a late worker cannot take it back.
pub(crate) fn reserve(
    state: &AppState,
    a: &AbCandidate,
    b: &AbCandidate,
    start_paused: bool,
) -> Result<AbReservation, String> {
    let mut inner = state.lock_inner();
    inner.audio.replace_private_audition(start_paused)?;
    let reservation = AbReservation {
        generation: state.audition.begin(&b.label),
        ab_generation: state.ab.begin(&a.label, &b.label),
        epoch: state.audition.epoch(),
        started: Instant::now(),
    };
    Ok(reservation)
}

/// Prepare independent graphs. Raw candidates remain explicitly unmeasured.
pub(crate) fn compare_reserved(
    state: &AppState,
    a: AbCandidate,
    b: AbCandidate,
    reservation: AbReservation,
) -> Result<AbSnapshot, String> {
    let AbReservation {
        generation,
        ab_generation,
        epoch,
        started,
    } = reservation;
    let current = || {
        state.ab.is_current(ab_generation)
            && state.audition.is_current(generation)
            && state.audition.epoch() == epoch
    };
    if !current() {
        return Err("A/B compare was superseded".into());
    }
    let context = candidate_render_context(state);
    let result: Result<AbSnapshot, String> = (|| {
        let prepare = |candidate: &AbCandidate| {
            let mut project = context.project.clone();
            let name = unique_factory_preset_name(&project, "A/B candidate");
            project
                .presets
                .push(factory_preset(&name, &candidate.params, &candidate.effects));
            crate::audio::prepare_preset_audition(&project, &name, context.root.as_deref())
        };
        let a_project = prepare(&a)?;
        if !current() {
            return Err("A/B compare was superseded".into());
        }
        let b_project = prepare(&b)?;
        if !current() {
            return Err("A/B compare was superseded".into());
        }
        let io = state.lock_inner().audio.audio_io.clone();
        #[cfg(test)]
        let simulated = state.audio_simulated();
        #[cfg(not(test))]
        let simulated = false;
        let prepared = crate::audio::AudioState::open_audition_pair_stream(
            &io,
            &context.spec,
            context.soundfont_path.clone().unwrap_or_default(),
            a_project.clone(),
            b_project.clone(),
            simulated,
        )?;
        let gain_state = prepared.pair_gains().expect("paired stream gains");
        let mut inner = state.lock_inner();
        if !current() {
            return Err("A/B compare was superseded".into());
        }
        inner.audio.install_audition_render(prepared)?;
        let snapshot = AbSnapshot::empty(&a.label, &b.label);
        if !current() || !state.ab.publish(ab_generation, snapshot.clone()) {
            inner.audio.stop_audition();
            return Err("A/B compare was superseded".into());
        }
        state.audition.complete(generation, started);
        drop(inner);
        spawn_analysis(AbAnalysis {
            controller: state.ab.clone(),
            generation: ab_generation,
            gains: Arc::downgrade(&gain_state),
            a: a_project,
            b: b_project,
            soundfont: context.soundfont_path.clone().unwrap_or_default(),
            rate: context.spec.sample_rate,
            a_label: a.label.clone(),
            b_label: b.label.clone(),
        });
        Ok(snapshot)
    })();
    if let Err(error) = &result {
        state.audition.fail(generation, error.clone());
    }
    result
}

#[cfg(test)]
fn compare(state: &AppState, a: AbCandidate, b: AbCandidate) -> Result<AbSnapshot, String> {
    let reservation = reserve(state, &a, &b, false)?;
    compare_reserved(state, a, b, reservation)
}

/// Switching replays a finished pair, but never resumes a mid-figure pause.
pub fn switch(state: &AppState) -> Result<AbSnapshot, String> {
    let mut inner = state.lock_inner();
    inner.audio.refuse_during_take("switching an audition")?;
    inner.audio.reap_audition_failure();
    if !inner.audio.has_audition_pair() {
        return Err("A/B streams are no longer active".into());
    }
    let snapshot = state.ab.switch()?;
    inner.audio.select_audition_side(snapshot.active == "b");
    Ok(snapshot)
}

/// Read-only polling; the webview can replace the initial unmeasured readout.
#[tauri::command]
pub fn get_ab_audition_state(state: tauri::State<'_, AppState>) -> Option<AbSnapshot> {
    state.ab.snapshot()
}

struct AbAnalysis {
    controller: AbController,
    generation: u64,
    gains: std::sync::Weak<[std::sync::atomic::AtomicU32; 2]>,
    a: oc_dsl::ast::Project,
    b: oc_dsl::ast::Project,
    soundfont: std::path::PathBuf,
    rate: u32,
    a_label: String,
    b_label: String,
}

fn spawn_analysis(request: AbAnalysis) {
    let AbAnalysis {
        controller,
        generation,
        gains,
        a,
        b,
        soundfont,
        rate,
        a_label,
        b_label,
    } = request;
    std::thread::spawn(move || {
        struct Completion(AbController, u64);
        impl Drop for Completion {
            fn drop(&mut self) {
                let mut pair = self.0.lock();
                if self.0.is_current(self.1)
                    && let Some(snapshot) = &mut pair.snapshot
                {
                    snapshot.measurement_pending = false;
                }
            }
        }
        let _completion = Completion(controller.clone(), generation);
        let current = || controller.is_current(generation) && gains.strong_count() > 0;
        // Analysis renders only a fixed two-second window, independently of
        // audible playback. Worker lookahead and collected samples are bounded.
        let measure = |project| -> Result<Vec<f32>, String> {
            use crate::render_thread::{RenderCommand, RenderThreadHandle};
            let frames = (MATCH_WINDOW_SECONDS * rate as f64) as u64;
            let render = RenderThreadHandle::spawn(rate);
            render.send(RenderCommand::ProjectChanged {
                // The analysis candidate arrives prepared, with absolute paths.
                root: None,
                project: Box::new(project),
                soundfont_path: soundfont.clone(),
                warm: false,
            });
            render.send(RenderCommand::Play {
                start_frame: 0,
                resume: false,
                end_frame: Some(frames),
            });
            let mut samples = Vec::with_capacity(frames as usize * 2);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            while samples.len() < frames as usize * 2 {
                if !current() {
                    return Err("superseded".into());
                }
                if let Some(error) = render.load_error() {
                    return Err(error);
                }
                if Instant::now() > deadline {
                    return Err("analysis timeout".into());
                }
                let mut block = [0.0; 512];
                let before = render.playback.play_cursor_frames.load(Ordering::Acquire);
                let take = block.len().min(frames as usize * 2 - samples.len());
                crate::audio::streaming_audio_callback(&mut block[..take], &render.playback);
                let count = (render.playback.play_cursor_frames.load(Ordering::Acquire) - before)
                    as usize
                    * 2;
                samples.extend_from_slice(&block[..count.min(take)]);
                if count == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            Ok(samples)
        };
        let Ok(a_raw) = measure(a) else {
            return;
        };
        let Ok(b_raw) = measure(b) else {
            return;
        };
        let raw_peaks = [peak(&a_raw), peak(&b_raw)];
        let Ok((matched_a, matched_b, mut snapshot)) =
            match_pair(a_raw, b_raw, rate, a_label, b_label)
        else {
            return;
        };
        if !current() {
            return;
        }
        let Some(gains) = gains.upgrade() else {
            return;
        };
        let mut pair = controller.lock();
        if !controller.is_current(generation) {
            return;
        }
        if let Some(previous) = &pair.snapshot {
            snapshot.active = previous.active.clone();
        }
        // The common headroom trim and B match are exactly those measured by
        // match_pair. Output approaches these targets smoothly on its callback.
        {
            for (index, measured) in [peak(&matched_a), peak(&matched_b)].into_iter().enumerate() {
                let gain = if raw_peaks[index] > 0.0 {
                    measured / raw_peaks[index]
                } else {
                    1.0
                };
                gains[index].store((gain as f32).to_bits(), Ordering::Release);
            }
        }
        pair.snapshot = Some(snapshot);
    });
}

/// Fixed-length window: the same number of frames for both sides regardless of
/// how long the renderer made each buffer. Short renders are zero-padded so a
/// mismatch can never silently shorten the measurement.
fn window(buffer: &[f32], frames: usize) -> Vec<f32> {
    let mut out = vec![0.0_f32; frames * 2];
    let copy = buffer.len().min(frames * 2);
    out[..copy].copy_from_slice(&buffer[..copy]);
    out
}

fn peak(buffer: &[f32]) -> f64 {
    buffer
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())))
}

fn scale(buffer: &mut [f32], gain_db: f64) {
    let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
    for sample in buffer.iter_mut() {
        *sample *= gain;
    }
}

fn lufs(buffer: &[f32], sample_rate: u32) -> Option<f64> {
    oc_engine::analysis::measure_stereo_interleaved(buffer, sample_rate).integrated_lufs
}

/// Window, measure, align B to A and report exactly what was measured.
///
/// Loudness gating is level-dependent, so after the first gain the residual is
/// measured again; the loop stops at the same 0.1 LU the UI is allowed to call
/// a match. A common trim is applied to both takes only when the matched pair
/// would clip, so a hot patch cannot turn the comparison into a limiter test.
pub(crate) fn match_pair(
    a_raw: Vec<f32>,
    b_raw: Vec<f32>,
    sample_rate: u32,
    a_label: String,
    b_label: String,
) -> Result<(Vec<f32>, Vec<f32>, AbSnapshot), String> {
    if sample_rate == 0 {
        return Err("A/B compare needs a non-zero sample rate".into());
    }
    let frames = (MATCH_WINDOW_SECONDS * f64::from(sample_rate)).round() as usize;
    let mut a = window(&a_raw, frames);
    let mut b = window(&b_raw, frames);

    let a_raw_lufs = lufs(&a, sample_rate);
    let b_raw_lufs = lufs(&b, sample_rate);
    let raw_delta_lu = match (a_raw_lufs, b_raw_lufs) {
        (Some(a_lufs), Some(b_lufs)) => Some(b_lufs - a_lufs),
        _ => None,
    };

    let mut applied_gain_db = None;
    let mut matched_delta_lu = None;
    let mut unmeasured = match (a_raw_lufs, b_raw_lufs) {
        (None, None) => Some(UNMEASURED_BOTH),
        (None, Some(_)) => Some(UNMEASURED_A),
        (Some(_), None) => Some(UNMEASURED_B),
        (Some(_), Some(_)) => None,
    };

    if unmeasured.is_none() {
        let mut applied = 0.0;
        for _ in 0..3 {
            let Some(a_now) = lufs(&a, sample_rate) else {
                unmeasured = Some(UNMEASURED_A);
                break;
            };
            let Some(b_now) = lufs(&b, sample_rate) else {
                unmeasured = Some(UNMEASURED_B);
                break;
            };
            let delta = a_now - b_now;
            if delta.abs() < 0.05 {
                break;
            }
            scale(&mut b, delta);
            applied += delta;
        }
        if unmeasured.is_none() {
            applied_gain_db = Some(applied);
        }
    }

    // Both takes are trimmed by the same amount, so the comparison stays
    // level-matched; only the absolute playback level drops.
    let ceiling = peak(&a).max(peak(&b));
    if ceiling > 1.0 {
        let trim = 20.0 * (0.999 / ceiling).log10();
        scale(&mut a, trim);
        scale(&mut b, trim);
    }

    let a_lufs = lufs(&a, sample_rate);
    let b_lufs = lufs(&b, sample_rate);
    if unmeasured.is_none() {
        match (a_lufs, b_lufs) {
            (Some(a_lufs), Some(b_lufs)) => {
                let residual = b_lufs - a_lufs;
                matched_delta_lu = Some(residual);
                if residual.abs() >= 0.1 {
                    unmeasured = Some(UNMEASURED_UNSTABLE);
                }
            }
            (None, _) => unmeasured = Some(UNMEASURED_A),
            (_, None) => unmeasured = Some(UNMEASURED_B),
        }
    }

    let snapshot = AbSnapshot {
        measurement_pending: false,
        status: if unmeasured.is_none() {
            "matched".into()
        } else {
            "unmeasured".into()
        },
        active: "b".into(),
        a_label,
        b_label,
        a_lufs,
        b_raw_lufs,
        b_lufs,
        raw_delta_lu,
        applied_gain_db,
        matched_delta_lu,
        unmeasured: unmeasured.map(str::to_owned),
        window_seconds: MATCH_WINDOW_SECONDS,
    };
    Ok((a, b, snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_candidate(gain: &str) -> Vec<(String, ParamValue)> {
        [
            ("osc.a.waveform", "1"),
            ("osc.a.level", "0.7"),
            ("filter.cutoff", "14000"),
            ("filter.resonance", "0.1"),
            ("filter.env.amount", "0"),
            ("amp.attack", "0.005"),
            ("amp.decay", "0.2"),
            ("amp.sustain", "0.8"),
            ("amp.release", "0.2"),
            ("output.gain", gain),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), ParamValue::Number(value.parse().unwrap())))
        .collect()
    }

    fn candidate(label: &str, gain: &str) -> AbCandidate {
        AbCandidate {
            label: label.into(),
            params: gain_candidate(gain),
            effects: Vec::new(),
        }
    }

    /// Two identical takes, one with a loud burst after the measurement window:
    /// the pinned window must ignore it instead of changing the readout.
    #[test]
    fn the_measurement_window_ignores_audio_after_it() {
        let sample_rate = 8000;
        let frames = (MATCH_WINDOW_SECONDS * f64::from(sample_rate)) as usize;
        let tone: Vec<f32> = (0..frames * 2)
            .map(|index| {
                let phase = index as f32 / 2.0 * 440.0 * std::f32::consts::TAU / 8000.0;
                0.2 * phase.sin()
            })
            .collect();
        let mut with_burst = tone.clone();
        with_burst.extend(std::iter::repeat_n(0.9_f32, frames * 2));
        let (a, b, snapshot) =
            match_pair(tone, with_burst, sample_rate, "A".into(), "B".into()).unwrap();
        assert_eq!(a, b, "the burst after the window leaked into the pair");
        assert_eq!(snapshot.status, "matched");
        assert_eq!(snapshot.applied_gain_db, Some(0.0));
        assert!(snapshot.raw_delta_lu.unwrap().abs() < 0.05);
    }

    #[test]
    fn a_half_gain_candidate_is_aligned_and_reported() {
        let sample_rate = 8000;
        let frames = (MATCH_WINDOW_SECONDS * f64::from(sample_rate)) as usize;
        let tone: Vec<f32> = (0..frames * 2)
            .map(|index| {
                let phase = index as f32 / 2.0 * 220.0 * std::f32::consts::TAU / 8000.0;
                0.25 * phase.sin()
            })
            .collect();
        let mut quiet = tone.clone();
        for sample in &mut quiet {
            *sample *= 0.5;
        }
        let (a, b, snapshot) =
            match_pair(tone, quiet, sample_rate, "A".into(), "B".into()).unwrap();
        assert_eq!(snapshot.status, "matched");
        // −6.02 dB raw difference; matching applies +6.02 dB to B.
        assert!((snapshot.raw_delta_lu.unwrap() + 6.0206).abs() < 0.02);
        assert!((snapshot.applied_gain_db.unwrap() - 6.0206).abs() < 0.05);
        assert!(snapshot.matched_delta_lu.unwrap().abs() < 0.1);
        let a_lufs = lufs(&a, sample_rate).unwrap();
        let b_lufs = lufs(&b, sample_rate).unwrap();
        assert!((a_lufs - b_lufs).abs() < 0.1);
    }

    #[test]
    fn a_silent_candidate_is_reported_not_matched() {
        let sample_rate = 8000;
        let frames = (MATCH_WINDOW_SECONDS * f64::from(sample_rate)) as usize;
        let tone: Vec<f32> = (0..frames * 2)
            .map(|index| {
                let phase = index as f32 / 2.0 * 220.0 * std::f32::consts::TAU / 8000.0;
                0.25 * phase.sin()
            })
            .collect();
        let (_, _, snapshot) = match_pair(
            tone,
            vec![0.0; frames * 2],
            sample_rate,
            "A".into(),
            "B".into(),
        )
        .unwrap();
        assert_eq!(snapshot.status, "unmeasured");
        assert_eq!(snapshot.unmeasured.as_deref(), Some(UNMEASURED_B));
        assert_eq!(snapshot.applied_gain_db, None);
        assert_eq!(snapshot.matched_delta_lu, None);
    }

    #[test]
    fn paired_stream_pending_pause_keeps_both_cursors_at_zero() {
        let state = AppState::new();
        state.lock_inner().audio.enable_simulated_output();
        let a = candidate("Paused A", "0.3");
        let b = candidate("Paused B", "0.15");
        let reservation = reserve(&state, &a, &b, true).unwrap();
        compare_reserved(&state, a, b, reservation).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut inner = state.lock_inner();
        assert_eq!(inner.audio.paired_audition_cursors(), (0, 0));
        assert_eq!(inner.audio.heard_audition_blocks(), 0);
        assert!(inner.audio.audition_paused);
        inner.audio.select_audition_side(false);
        assert!(inner.audio.audition_paused);
        inner.audio.stop_audition();
    }

    #[test]
    fn a_cancelled_compare_reservation_cannot_install_a_pair() {
        let state = AppState::new();
        let a = candidate("A", "0.3");
        let b = candidate("B", "0.15");
        let reservation = reserve(&state, &a, &b, true).unwrap();
        {
            let mut inner = state.lock_inner();
            state.audition.cancel_all();
            inner.audio.replace_private_audition(false).unwrap();
        }
        let error = compare_reserved(&state, a, b, reservation).unwrap_err();
        assert!(error.contains("superseded"));
        assert!(!state.lock_inner().audio.has_audition_pair());
        assert!(!state.audition_pause_intent.load(Ordering::Acquire));
    }

    #[test]
    fn switching_finished_pair_replays_both_streams_from_zero() {
        let state = AppState::new();
        state.lock_inner().audio.enable_simulated_output();
        compare(
            &state,
            candidate("Replay A", "0.3"),
            candidate("Replay B", "0.15"),
        )
        .unwrap();
        let end = (MATCH_WINDOW_SECONDS * candidate_render_context(&state).spec.sample_rate as f64)
            as u64;
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let cursors = state.lock_inner().audio.paired_audition_cursors();
            if cursors == (end, end) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pair did not reach its bound: {cursors:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let heard = state.lock_inner().audio.heard_audition_blocks();
        assert_eq!(switch(&state).unwrap().active, "a");
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let inner = state.lock_inner();
            let cursors = inner.audio.paired_audition_cursors();
            if inner.audio.heard_audition_blocks() > heard && cursors.0 > 0 && cursors.0 < end {
                // A callback updates two counters consecutively; inspect them
                // after Pause below to verify a stable synchronized position.
                break;
            }
            drop(inner);
            assert!(Instant::now() < deadline, "finished pair did not replay");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        state.lock_inner().audio.pause_private_audition();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cursors = state.lock_inner().audio.paired_audition_cursors();
        assert_eq!(cursors.0, cursors.1);
        assert!(cursors.0 < end);
        state.lock_inner().audio.stop_audition();
        assert!(switch(&state).is_err());
    }

    #[test]
    fn superseding_pair_cannot_publish_old_background_measurement() {
        let state = AppState::new();
        state.lock_inner().audio.enable_simulated_output();
        compare(&state, candidate("Old A", "0.3"), candidate("Old B", "0.1")).unwrap();
        state.lock_inner().audio.stop_audition();
        state.ab.begin("New A", "New B");
        assert!(!state.ab.publish(1, AbSnapshot::empty("Stale A", "Stale B")));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let snapshot = state.ab.snapshot().unwrap();
        assert_eq!(snapshot.a_label, "New A");
        assert_eq!(snapshot.b_label, "New B");
        assert!(snapshot.measurement_pending);
        assert_eq!(snapshot.applied_gain_db, None);
    }

    #[test]
    fn compare_streams_without_claiming_loudness_match_and_switch_preserves_pause() {
        let state = AppState::new();
        state.lock_inner().audio.enable_simulated_output();
        let snapshot = compare(
            &state,
            candidate("Loud", "0.32"),
            candidate("Quiet", "0.16"),
        )
        .unwrap();
        assert_eq!(snapshot.status, "unmeasured");
        assert_eq!(snapshot.applied_gain_db, None);
        assert_eq!(snapshot.active, "b");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.lock_inner().audio.heard_audition_blocks() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "paired stream never emitted audio"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let mut inner = state.lock_inner();
            assert!(
                inner.audio.audition_playback.is_none(),
                "must not retain whole audio takes"
            );
            inner.audio.pause_private_audition();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let before = state.lock_inner().audio.paired_audition_cursors();
        assert_eq!(before.0, before.1);
        assert!(before.0 > 0);
        assert_eq!(switch(&state).unwrap().active, "a");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(before, state.lock_inner().audio.paired_audition_cursors());
        state.lock_inner().audio.resume_private_audition().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let after = state.lock_inner().audio.paired_audition_cursors();
            if after.0 > before.0 {
                assert_eq!(after.0, after.1);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let measured = state.ab.snapshot().unwrap();
            if !measured.measurement_pending {
                assert_eq!(measured.status, "matched");
                assert_eq!(measured.active, "a");
                // The candidates differ by 6 dB of `output.gain`, but the
                // instrument normalises each patch, so the streams arrive
                // already matched and the comparison applies no correction.
                let applied = measured.applied_gain_db.unwrap();
                assert!(
                    applied.abs() < 0.5,
                    "the comparison still corrected {applied} dB between normalised patches"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background analysis did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        state.lock_inner().audio.stop_audition();
        assert!(switch(&state).is_err());
    }
}
