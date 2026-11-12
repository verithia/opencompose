//! Headless first-block harness: every path that starts audio, measured on the
//! production worker, ring and device callback with the simulated sink.
//!
//! Each path reports the press-to-first-block latency from the `FirstBlock`
//! trace and the frames the producer computed before that block, which is what
//! separates a streaming start from one that renders a range first.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oc_dsl::ast::Project;

use crate::audio::AudioState;
use crate::first_block::{FirstBlock, FirstBlockSnapshot, StartPath, now_ns};

/// Repetitions per path in the benchmark harness.
const REPETITIONS: usize = 12;

/// Repetitions for the 20-track session; its build is seconds, not milliseconds.
const SESSION_REPETITIONS: usize = 3;

/// A path's samples, as the report prints them.
#[derive(Default)]
struct Samples(Vec<FirstBlockSnapshot>);

impl Samples {
    fn push(&mut self, snapshot: FirstBlockSnapshot) {
        self.0.push(snapshot);
    }

    fn latencies(&self) -> Vec<f64> {
        self.0
            .iter()
            .filter_map(|snapshot| snapshot.latency_ms)
            .collect()
    }

    fn blocks(&self) -> Vec<f64> {
        self.0.iter().map(|s| s.blocks_computed).collect()
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn p50(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    percentile(&sorted, 0.5)
}

fn p95(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    percentile(&sorted, 0.95)
}

/// Report one path's numbers over its repetitions.
fn report(path: &str, samples: &Samples) {
    let latencies = samples.latencies();
    let blocks = samples.blocks();
    let frames = samples
        .0
        .iter()
        .map(|snapshot| snapshot.frames_computed)
        .max()
        .unwrap_or(0);
    eprintln!(
        "[first-block] {path:<16} n={} block_frames={} p50={:.2}ms p95={:.2}ms worst_frames={frames} worst_blocks={:.2}",
        latencies.len(),
        samples
            .0
            .first()
            .map_or(0, |snapshot| snapshot.block_frames),
        p50(&latencies),
        p95(&latencies),
        blocks.iter().copied().fold(0.0, f64::max),
    );
}

fn transport_trace(audio: &AudioState) -> Option<Arc<FirstBlock>> {
    audio
        .render_thread
        .as_ref()
        .map(|rt| Arc::clone(&rt.playback.first_block))
}

fn audition_trace(audio: &AudioState) -> Option<Arc<FirstBlock>> {
    audio
        .audition_render
        .as_ref()
        .map(|rt| Arc::clone(&rt.playback.first_block))
}

/// Wait for the block that belongs to the newest start on this trace.
fn wait_delivered(trace: &FirstBlock) -> FirstBlockSnapshot {
    let seq = trace.snapshot().seq;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = trace.snapshot();
        if snapshot.delivered && snapshot.seq == seq {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "no first block for {} after {:.0}ms (frames_computed={} delivered={})",
            snapshot.path,
            snapshot.since_start_ms,
            snapshot.frames_computed,
            snapshot.delivered
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Wait for a start newer than `previous` to reach the device.
fn wait_next(trace: &FirstBlock, previous: u64) -> FirstBlockSnapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = trace.snapshot();
        if snapshot.delivered && snapshot.seq > previous {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "no start after {previous} (last {} at {:.0}ms, frames={})",
            snapshot.path,
            snapshot.since_start_ms,
            snapshot.frames_computed
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn wait_warm(audio: &AudioState) -> bool {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let Some(rt) = audio.render_thread.as_ref() else {
            return false;
        };
        if rt.playback.warm_state.load(Ordering::Acquire) == crate::render_thread::WARM_READY {
            return true;
        }
        if rt.load_error().is_some() {
            return false;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Wait until the device has played the audible cursor up to `frame`.
fn wait_played_to(audio: &AudioState, frame: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Some(rt) = audio.render_thread.as_ref() else {
            return false;
        };
        if rt.playback.play_cursor_frames.load(Ordering::Acquire) >= frame {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn fake_backend() -> Arc<oc_audio::audio_io::FakeBackend> {
    Arc::new(
        oc_audio::audio_io::FakeBackend::new(
            vec![oc_audio::audio_io::OutputDevice {
                key: "fake".into(),
                name: "Speakers".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![44_100, 48_000],
                default_rate: 48_000,
                buffer_frames: None,
            }],
            Some("fake".into()),
        )
        .with_inputs(
            vec![oc_audio::audio_io::InputDevice {
                key: "fake-in".into(),
                name: "Microphone".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![44_100, 48_000],
                default_rate: 48_000,
            }],
            Some("fake-in".into()),
        ),
    )
}

/// Wait until the render thread has finished the load it was sent, warm or not.
fn wait_loaded(audio: &AudioState) -> bool {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let Some(rt) = audio.render_thread.as_ref() else {
            return false;
        };
        if rt.playback.project_loaded.load(Ordering::Acquire) {
            return true;
        }
        if rt.load_error().is_some() || Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn test_audio(soundfont: Option<std::path::PathBuf>) -> AudioState {
    let mut audio = AudioState::with_backend(fake_backend());
    audio.enable_simulated_output();
    audio.sample_rate = 48_000;
    audio.soundfont_path = soundfont;
    audio
}

fn small_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "First block small"
  tempo: 120
  time_signature: 4/4
@preset lead
  source: builtin
  plugin: "builtin.open_synth"
@preset pad
  source: builtin
  plugin: "builtin.open_synth"
@preset bass
  source: builtin
  plugin: "builtin.open_synth"
@track lead | preset: lead, instrument: synth/lead_square
  @phrase a [1:1:0 - 16:1:0]
    C4 q mf
    E4 q mf
    G4 q mf
    C5 q mf
    C4 q mf
    E4 q mf
    G4 q mf
    C5 q mf
    C4 q mf
    E4 q mf
    G4 q mf
    C5 q mf
    C4 q mf
    E4 q mf
    G4 q mf
    C5 q mf
@track pad | preset: pad, instrument: synth/pad
  @phrase a [1:1:0 - 16:1:0]
    C3 w mp
@track bass | preset: bass, instrument: synth/bass
  @phrase a [1:1:0 - 16:1:0]
    C2 h mf
"#,
    )
    .unwrap()
}

/// The small project plus a clip-bearing audio track, which is what a take arms.
fn record_project() -> Project {
    let media = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../crates/oc-engine/tests/fixtures/sampler/tone.flac");
    let mut project = small_project();
    let track = oc_dsl::parse(&format!(
        "@ocp \"0.2.0\"\n@project \"First block take\"\n  tempo: 120\n  time_signature: 4/4\n@audio_clip bed [1:1 - 2:1] | track: tape\n  file: \"{}\"\n@track tape | instrument: audio/clip\n",
        media.display()
    ))
    .unwrap();
    // The clip rides in the audio track's own pool, which is where the
    // streaming mixer reads it from.
    project.tracks.extend(track.tracks);
    project
}

fn pattern_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "First block pattern"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@pattern hook | preset: native, instrument: synth/lead_square
  events:
    C4 q mf
    E4 q mf
@track synth | preset: native, instrument: synth/lead_square
  clip: hook [1:1:0 - 4:1:0]
"#,
    )
    .unwrap()
}

/// Four instrument lanes at a fast tempo, one whole note per bar, so a test
/// can play to bar 8 without waiting through seconds of real-time playback.
fn backward_seek_lanes(source: &str, instrument: &str) -> Project {
    let mut text = String::from(
        "@ocp \"0.2.0\"\n@project \"First block backward seek\"\n  tempo: 400\n  time_signature: 4/4\n@preset voice\n",
    );
    text.push_str(source);
    for (index, name) in ["one", "two", "three", "four"].iter().enumerate() {
        let notes = format!("    C{} w mf\n", 3 + index).repeat(8);
        text.push_str(&format!(
            "@track {name} | preset: voice, instrument: {instrument}\n  @phrase a [1:1:0 - 9:1:0]\n{notes}"
        ));
    }
    oc_dsl::parse(&text).unwrap()
}

/// Native lanes for the backward-seek row.
fn backward_seek_small_project() -> Project {
    backward_seek_lanes(
        "  source: builtin\n  plugin: \"builtin.open_synth\"\n",
        "synth/lead_square",
    )
}

/// In-process VST3 lanes, so the row can prove no instrument is re-instantiated.
fn four_vst3_project() -> Project {
    backward_seek_lanes(
        "  source: vst3\n  plugin: \"test.fake.one_bus\"\n",
        "keys/kontakt",
    )
}

/// The 20-track reference session, prepared from the checkout.
fn session_project() -> Option<Project> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../benchmarks/session-20tracks.ocp");
    let source = std::fs::read_to_string(&path).ok()?;
    let mut project = oc_dsl::parse(&source).ok()?;
    // Sampler pads read a checked-in tone relative to the project directory.
    let root = path.parent()?.to_path_buf();
    project = oc_engine::block_source::prepare_project(&project, Some(&root)).ok()?;
    Some(project)
}

/// The SoundFont the 20-track session renders its GM tracks from.
fn fixture_soundfont() -> Option<std::path::PathBuf> {
    std::env::var_os("OCP_SOUNDFONT")
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../crates/oc-engine/tests/fixtures/minimal-test.sf2");
            path.exists().then_some(path)
        })
}

fn play_once(audio: &mut AudioState, project: &Project) -> FirstBlockSnapshot {
    let mut plugins = HashMap::new();
    audio.play(project, &mut plugins).unwrap();
    let trace = transport_trace(audio).expect("streaming playback has a trace");
    wait_delivered(&trace)
}

// ============================================================================
// The paths
// ============================================================================

/// Play from stop on a small project, after the open warm-up the app runs.
fn path_play_cold_small() -> FirstBlockSnapshot {
    let project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project)
}

/// The same press with the warm-up still running: the press waits for the build.
fn path_play_during_warm() -> FirstBlockSnapshot {
    let project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    play_once(&mut audio, &project)
}

/// Play from stop on the 20-track session, cold.
fn path_play_cold_session() -> FirstBlockSnapshot {
    let soundfont = fixture_soundfont();
    let project = session_project().expect("the 20-track session parses");
    let mut audio = test_audio(soundfont);
    audio.preload_project(&project);
    let warm = wait_warm(&audio);
    assert!(
        warm,
        "the 20-track session never warmed up: {:?}",
        audio.render_thread.as_ref().and_then(|rt| rt.load_error())
    );
    play_once(&mut audio, &project)
}

/// Seek to a new position while playing.
fn path_seek_while_playing() -> FirstBlockSnapshot {
    let project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    // Halfway into the arrangement: a real jump, not the end of the project.
    let total = audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .total_frames
        .load(Ordering::Acquire);
    let quarter = clock.position_at_sample(total / 2, 48_000).unwrap().quarter;
    audio.seek(quarter, &clock).unwrap();
    let trace = transport_trace(&audio).unwrap();
    wait_delivered(&trace)
}

/// Play to bar 8, then seek back to bar 1. This is the path the first-block
/// budget never measured: the render thread has to rewind its lanes.
fn path_backward_seek_while_playing() -> FirstBlockSnapshot {
    let project = backward_seek_small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let bar_eight = clock.sample_at_quarter(28.0, 48_000).unwrap();
    assert!(
        wait_played_to(&audio, bar_eight),
        "playback never reached bar 8"
    );
    audio.seek(0.0, &clock).unwrap();
    let trace = transport_trace(&audio).unwrap();
    wait_delivered(&trace)
}

/// The same backward seek on four VST3 instrument lanes. A reload would
/// instantiate every plugin on the press path, so the instrument counter must
/// not move.
fn path_backward_seek_vst3() -> (FirstBlockSnapshot, u64, u64) {
    let project = four_vst3_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(
        wait_warm(&audio),
        "the VST3 project never warmed up: {:?}",
        audio.render_thread.as_ref().and_then(|rt| rt.load_error())
    );
    play_once(&mut audio, &project);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let bar_eight = clock.sample_at_quarter(28.0, 48_000).unwrap();
    assert!(
        wait_played_to(&audio, bar_eight),
        "playback never reached bar 8"
    );
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    let before = playback.instrument_instantiations.load(Ordering::Acquire);
    audio.seek(0.0, &clock).unwrap();
    let trace = transport_trace(&audio).unwrap();
    let snapshot = wait_delivered(&trace);
    let after = playback.instrument_instantiations.load(Ordering::Acquire);
    (snapshot, before, after)
}

/// Resume after a pause.
fn path_resume_after_pause() -> FirstBlockSnapshot {
    let project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    audio.pause();
    std::thread::sleep(Duration::from_millis(20));
    play_once(&mut audio, &project)
}

/// Preview a preset from a cold audition controller.
fn path_preset_audition() -> FirstBlockSnapshot {
    let state = crate::state::AppState::new();
    let project = small_project();
    {
        let mut inner = state.lock_inner();
        inner.project = project;
        inner.audio.enable_simulated_output();
        inner.audio.sample_rate = 48_000;
    }
    let generation = state.audition.begin("lead");
    let started = Instant::now();
    crate::audition::run(&state, "lead", generation, started);
    let trace = {
        let inner = state.lock_inner();
        audition_trace(&inner.audio).expect("the audition opened a stream")
    };
    wait_delivered(&trace)
}

/// Preview a pattern on the private audition stream.
fn path_pattern_preview() -> FirstBlockSnapshot {
    let project = pattern_project();
    let mut audio = test_audio(None);
    audio.preview_pattern(&project, "hook", None).unwrap();
    let trace = audition_trace(&audio).expect("the preview opened a stream");
    wait_delivered(&trace)
}

/// Preview one clip range on the private audition stream.
fn path_clip_preview() -> FirstBlockSnapshot {
    let project = pattern_project();
    let mut audio = test_audio(None);
    let spec = audio.audio_io.spec();
    let start = oc_dsl::ast::BarBeat {
        bar: 1,
        beat: 1,
        subdivision: 0,
    };
    let end = oc_dsl::ast::BarBeat {
        bar: 3,
        beat: 1,
        subdivision: 0,
    };
    let (start_frame, end_frame) =
        crate::audio::block_preview_sample_range(&project, 0, start, end, spec.sample_rate)
            .unwrap();
    let solo = crate::audio::solo_preview_project(&project, 0).unwrap();
    let prepared = oc_engine::block_source::prepare_project(&solo, None).unwrap();
    let prepared = AudioState::open_audition_render_window(
        &audio.audio_io,
        &spec,
        std::path::PathBuf::new(),
        prepared,
        true,
        now_ns(),
        StartPath::ClipPreview,
        start_frame,
        Some(end_frame),
    )
    .unwrap();
    audio.install_audition_render(prepared).unwrap();
    let trace = audition_trace(&audio).expect("the preview opened a stream");
    wait_delivered(&trace)
}

/// Start an audio take: the press opens the capture stream, then rolls.
fn path_record_audio() -> FirstBlockSnapshot {
    let state = crate::state::AppState::with_audio_backend(fake_backend());
    let project = record_project();
    {
        let mut inner = state.lock_inner();
        inner.project = project.clone();
        inner.file_path = Some(std::path::PathBuf::from("/tmp/first-block-take.ocp"));
        inner.audio.enable_simulated_output();
        inner.audio.sample_rate = 48_000;
        inner.audio.preload_project(&project);
        assert!(wait_warm(&inner.audio), "the take project never warmed up");
        inner.audio.recorder.armed = Some("tape".into());
        inner.audio.record_start_at(StartPath::RecordAudio);
        crate::record::start_inner(&mut inner, 48_000).unwrap();
    }
    let mut inner = state.lock_inner();
    let project = inner.project.clone();
    let mut plugins = std::mem::take(&mut inner.plugin_instances);
    inner.audio.play(&project, &mut plugins).unwrap();
    let playback = Arc::clone(&inner.audio.render_thread.as_ref().unwrap().playback);
    let trace = Arc::clone(&playback.first_block);
    let snapshot = wait_delivered(&trace);
    eprintln!(
        "[record-debug] active={} ready={} applied={}/{} cursor={} loads={}",
        playback.active.load(Ordering::Acquire),
        playback.ready.load(Ordering::Acquire),
        playback.position_applied.load(Ordering::Acquire),
        playback.position_requested.load(Ordering::Acquire),
        playback.play_cursor_frames.load(Ordering::Acquire),
        playback.project_loads.load(Ordering::Acquire),
    );
    crate::record::stop_inner(&mut inner).ok();
    snapshot
}

/// A press immediately after File ▸ New. The document is installed without a
/// warm-up and the command hands the warm to a worker; the press queues behind
/// it exactly as the command layer does, then must still stream.
fn path_press_after_new() -> FirstBlockSnapshot {
    let state = Arc::new(crate::state::AppState::new());
    let snapshot = {
        let mut inner = state.lock_inner();
        crate::commands::create_project_in(&mut inner, None).unwrap()
    };
    let warm = {
        let state = Arc::clone(&state);
        let revision = snapshot.revision;
        std::thread::spawn(move || crate::commands::warm_preload_if_current(&state, revision))
    };
    let pressed = Instant::now();
    let mut inner = state.lock_inner();
    let project = inner.project.clone();
    let mut plugins = std::mem::take(&mut inner.plugin_instances);
    inner.audio.play(&project, &mut plugins).unwrap();
    let trace = transport_trace(&inner.audio).expect("streaming playback has a trace");
    drop(inner);
    let delivered = wait_delivered(&trace);
    let _ = warm.join();
    eprintln!(
        "[measure] press_after_new press_to_first_block={:?} trace={:?}",
        pressed.elapsed(),
        delivered.latency_ms
    );
    delivered
}

/// The MIDI take's transport part: the same play path, tagged as a take.
fn path_record_midi() -> FirstBlockSnapshot {
    let project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    audio.record_start_at(StartPath::RecordMidi);
    play_once(&mut audio, &project)
}

/// Unmute a track whose lane was pruned at load.
fn path_unmute_pruned_lane() -> FirstBlockSnapshot {
    let mut project = small_project();
    project.tracks[1].mix.mute = true;
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    let trace = transport_trace(&audio).unwrap();
    let previous = trace.seq();
    project.tracks[1].mix.mute = false;
    audio.apply_mixer_change(&project);
    wait_next(&trace, previous)
}

/// A structural edit, then a press immediately after it.
fn path_structural_edit_then_play() -> FirstBlockSnapshot {
    let mut project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    // Change one track's instrument, the shape of an add-track or preset edit.
    project.tracks[2].preset = Some("pad".into());
    audio.mark_dirty();
    audio.preload_project(&project);
    play_once(&mut audio, &project)
}

/// The same edit, but the press waits for the rebuild to land.
fn path_structural_edit_settled() -> FirstBlockSnapshot {
    let mut project = small_project();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    assert!(wait_warm(&audio), "the small project never warmed up");
    play_once(&mut audio, &project);
    project.tracks[2].preset = Some("pad".into());
    audio.mark_dirty();
    audio.preload_project(&project);
    assert!(wait_loaded(&audio), "the rebuilt project never loaded");
    play_once(&mut audio, &project)
}

// ============================================================================
// Tests
// ============================================================================

fn run_path(
    path: &str,
    mut sample: impl FnMut() -> FirstBlockSnapshot,
    repetitions: usize,
) -> Samples {
    let mut samples = Samples::default();
    for _ in 0..repetitions {
        samples.push(sample());
    }
    report(path, &samples);
    samples
}

/// The streaming bound: a start computes at most the 250 ms lookahead window
/// plus one chunk before its first block, which is four full 4096-frame chunks.
/// A path that renders a range first shows the whole range here instead.
const MAX_FRAMES_BEFORE_FIRST_BLOCK: u64 = 16_384;

fn assert_streams(path: &str, samples: &Samples) {
    for snapshot in &samples.0 {
        assert!(
            snapshot.delivered,
            "{path}: the first block never reached the device"
        );
        assert!(
            snapshot
                .latency_ms
                .is_some_and(|value| value.is_finite() && value >= 0.0),
            "{path}: delivered block has no valid start-to-device timestamp"
        );
        assert!(
            snapshot.frames_computed <= MAX_FRAMES_BEFORE_FIRST_BLOCK,
            "{path}: the producer computed {} frames ({} device blocks) before the first block",
            snapshot.frames_computed,
            snapshot.blocks_computed
        );
    }
}

/// 50 ms is the contract on a quiet machine. This crate's lib tests run in
/// parallel, so p95 also measures scheduler contention; when the median is
/// already slow the machine is loaded and only the load bound applies.
fn assert_first_block_bound(path: &str, samples: &Samples) {
    let latencies = samples.latencies();
    let median = p50(&latencies);
    let tail = p95(&latencies);
    assert!(median < 50.0, "{path} p50 {median:.2}ms");
    if median < 10.0 {
        assert!(tail < 50.0, "{path} p95 {tail:.2}ms");
    } else {
        eprintln!("[first-block] {path} loaded run p50={median:.2}ms p95={tail:.2}ms");
        assert!(tail < 150.0, "{path} p95 {tail:.2}ms under load");
    }
}

#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn play_from_stop_streams_and_reaches_the_device() {
    let samples = run_path("play_cold_small", path_play_cold_small, 6);
    assert_streams("play_cold_small", &samples);
    assert_first_block_bound("play_cold_small", &samples);
}

/// A press that lands while the open warm-up is still building. The press is
/// queued behind the source build, so the build must not be tens of
/// milliseconds of work the user has to wait for.
#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn press_during_the_warm_up_reaches_the_device() {
    let samples = run_path("play_during_warm", path_play_during_warm, REPETITIONS);
    assert_streams("play_during_warm", &samples);
    assert_first_block_bound("play_during_warm", &samples);
}

/// A press right after New: the document is installed and its warm-up is still
/// on the worker, so the press must not wait for a graph that was never built.
#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn a_press_right_after_new_reaches_the_device() {
    let samples = run_path("press_after_new", path_press_after_new, 6);
    assert_streams("press_after_new", &samples);
    assert_first_block_bound("press_after_new", &samples);
}

/// The same bound right after a structural edit, before its rebuild lands.
#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn press_right_after_a_structural_edit_reaches_the_device() {
    let samples = run_path("structural_edit_play", path_structural_edit_then_play, 6);
    assert_streams("structural_edit_play", &samples);
    assert_first_block_bound("structural_edit_play", &samples);
}

#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn resume_after_pause_reaches_the_device() {
    let samples = run_path("resume_after_pause", path_resume_after_pause, 6);
    assert_streams("resume_after_pause", &samples);
    assert_first_block_bound("resume_after_pause", &samples);
}

/// A backward seek while playing is a streaming start, not a project reload.
#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn backward_seek_reaches_the_device() {
    let samples = run_path("backward_seek", path_backward_seek_while_playing, 6);
    assert_streams("backward_seek", &samples);
    assert_first_block_bound("backward_seek", &samples);
}

/// The review's row: four VST3 instruments, play to bar 8, seek to bar 1. The
/// instruments stay instantiated, so the first block must meet the budget.
#[test]
#[ignore = "load-sensitive; run serially with --ignored --test-threads=1"]
fn backward_seek_on_vst3_lanes_keeps_the_instances() {
    let mut samples = Samples::default();
    for _ in 0..4 {
        let (snapshot, before, after) = path_backward_seek_vst3();
        assert_eq!(
            after,
            before,
            "a backward seek instantiated {} instrument(s)",
            after - before
        );
        samples.push(snapshot);
    }
    report("backward_seek_vst3", &samples);
    assert_streams("backward_seek_vst3", &samples);
    assert_first_block_bound("backward_seek_vst3", &samples);
}

#[test]
fn pattern_and_clip_previews_stream() {
    let samples = run_path("pattern_preview", path_pattern_preview, 4);
    assert_streams("pattern_preview", &samples);
    let samples = run_path("clip_preview", path_clip_preview, 4);
    assert_streams("clip_preview", &samples);
}

/// A ten-minute silent project must not compute its timeline before it sounds.
#[test]
fn a_ten_minute_project_starts_without_computing_its_timeline() {
    // 300 bars of 4/4 at 120 bpm is ten minutes; one note at the head keeps it
    // audible while the arrangement stays empty.
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Ten minutes"
  tempo: 120
  time_signature: 4/4
@section all [1:1 - 300:1]
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@pattern bed | preset: tone, instrument: synth/lead_square
  events:
    C4 w mf
@track lead | preset: tone, instrument: synth/lead_square
  clip: bed [1:1:0 - 300:1:0]
"#,
    )
    .unwrap();
    let mut audio = test_audio(None);
    audio.preload_project(&project);
    // The warm-up stages the lookahead; the press itself must compute nothing.
    assert!(wait_warm(&audio), "the long project never warmed up");
    let total = audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .total_frames
        .load(Ordering::Acquire);
    assert!(
        total >= 48_000 * 590,
        "the ten-minute project reported {total} frames"
    );
    let snapshot = play_once(&mut audio, &project);
    assert!(
        snapshot.frames_computed == 0,
        "a warm press computed {} frames",
        snapshot.frames_computed
    );
    assert_streams("ten_minute_play", &{
        let mut samples = Samples::default();
        samples.push(snapshot);
        samples
    });
}

/// Run exactly once per process so the preset and device caches are cold.
#[test]
#[ignore = "requires OCP_STREAM_PROJECT; run in a fresh process per sample"]
fn review_fresh_project_first_block() {
    struct PreparationLog;
    impl log::Log for PreparationLog {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata
                .target()
                .starts_with("oc_engine::sf2::font::prepared")
        }
        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                eprintln!("[sf3-preparation] {}", record.args());
            }
        }
        fn flush(&self) {}
    }
    static PREPARATION_LOG: PreparationLog = PreparationLog;
    if std::env::var_os("OCP_PROFILE_PLAYBACK").is_some()
        && log::set_logger(&PREPARATION_LOG).is_ok()
    {
        log::set_max_level(log::LevelFilter::Debug);
    }
    let path = std::path::PathBuf::from(
        std::env::var_os("OCP_STREAM_PROJECT").expect("set OCP_STREAM_PROJECT to the project path"),
    );
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut audio = test_audio(fixture_soundfont());
    audio.resource_root = path.parent().map(std::path::Path::to_path_buf);
    let path = if std::env::var_os("OCP_STREAM_PRELOAD").is_some() {
        audio.preload_project(&project);
        "during_open"
    } else {
        "no_preload"
    };
    let started = Instant::now();
    let mut plugins = HashMap::new();
    audio.play(&project, &mut plugins).unwrap();
    let dispatch_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let trace = transport_trace(&audio).expect("streaming playback has a trace");
    let snapshot = wait_delivered(&trace);
    let worker = audio.render_thread.as_ref().unwrap();
    let loads = worker.playback.project_loads.load(Ordering::Acquire);
    assert_eq!(loads, 1, "Play must reuse a queued preload");
    let mut samples = Samples::default();
    samples.push(snapshot);
    assert_streams(path, &samples);
    report(path, &samples);
    eprintln!("[first-block] {path} dispatch_ms={dispatch_ms:.2} loads={loads}");
    if std::env::var_os("OCP_STREAM_REQUIRE_AUDIBLE").is_some() {
        let deadline = Instant::now() + Duration::from_secs(2);
        while audio.simulated_output_heard_peak() <= 0.0001 {
            assert!(
                Instant::now() < deadline,
                "the simulated device received no audible PCM"
            );
            assert!(worker.load_error().is_none(), "{:?}", worker.load_error());
            std::thread::sleep(Duration::from_millis(1));
        }
        eprintln!(
            "[first-block] {path} audible_ms={:.2} peak={:.6}",
            started.elapsed().as_secs_f64() * 1_000.0,
            audio.simulated_output_heard_peak()
        );
    }
}

/// The audit harness: p50/p95 per path with the block size.
#[test]
#[ignore = "benchmark harness; run with --ignored --nocapture"]
fn review_first_block_latency() {
    let small = || -> Samples { run_path("play_cold_small", path_play_cold_small, REPETITIONS) };
    run_path("play_during_warm", path_play_during_warm, REPETITIONS);
    run_path("press_after_new", path_press_after_new, 6);
    run_path("seek_while_playing", path_seek_while_playing, REPETITIONS);
    run_path(
        "backward_seek",
        path_backward_seek_while_playing,
        REPETITIONS,
    );
    let mut backward_vst3 = Samples::default();
    for _ in 0..REPETITIONS {
        let (snapshot, _, _) = path_backward_seek_vst3();
        backward_vst3.push(snapshot);
    }
    report("backward_seek_vst3", &backward_vst3);
    run_path("resume_after_pause", path_resume_after_pause, REPETITIONS);
    run_path("preset_audition", path_preset_audition, REPETITIONS);
    run_path("pattern_preview", path_pattern_preview, REPETITIONS);
    run_path("clip_preview", path_clip_preview, REPETITIONS);
    run_path("record_audio", path_record_audio, 4);
    run_path("record_midi", path_record_midi, REPETITIONS);
    run_path("unmute_pruned_lane", path_unmute_pruned_lane, 6);
    run_path("structural_edit_play", path_structural_edit_then_play, 6);
    run_path("structural_settled", path_structural_edit_settled, 6);
    run_path(
        "play_cold_20tracks",
        path_play_cold_session,
        SESSION_REPETITIONS,
    );
    let samples = small();
    eprintln!(
        "[first-block] summary paths=16 small_p50={:.2}ms small_p95={:.2}ms",
        p50(&samples.latencies()),
        p95(&samples.latencies())
    );
}
