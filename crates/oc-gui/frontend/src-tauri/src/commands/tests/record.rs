//! Take recording: arming, capture, latency compensation, refusals.

use std::sync::Arc;

use oc_audio::audio_io::{FakeBackend, InputDevice, OutputDevice};
use oc_audio::click::CountIn;

use crate::audio::PlaybackState;
use crate::commands::preview::begin_inner;
use crate::commands::tests::support::{TempDir, make_audio_track, make_track};
use crate::preview::READ_ONLY;
use crate::record;
use crate::state::AppState;

const RATE: u32 = 48_000;
/// Frames of one quarter at the test project's 120 BPM.
const QUARTER: usize = 24_000;
/// Frames of one bar in the test project's 4/4.
const BAR: usize = 4 * QUARTER;

/// The armed audio track with a quieter mix, as a proposal would send it.
const VOX_QUIETER: &str = r#"@track vox | instrument: audio/input
  mix: vol=-6dB
"#;

fn backend() -> Arc<FakeBackend> {
    backend_with_capture_rate(RATE)
}

fn backend_with_capture_rate(rate: u32) -> Arc<FakeBackend> {
    Arc::new(
        FakeBackend::new(
            vec![OutputDevice {
                key: "coreaudio:out".into(),
                name: "Speakers".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![RATE],
                default_rate: RATE,
                buffer_frames: None,
            }],
            Some("coreaudio:out".into()),
        )
        .with_inputs(
            vec![InputDevice {
                key: "coreaudio:mic".into(),
                name: "Built-in Microphone".into(),
                is_default: true,
                channels: 1,
                sample_rates: vec![rate],
                default_rate: rate,
            }],
            Some("coreaudio:mic".into()),
        ),
    )
}

/// A project with one track, saved inside `dir`.
fn state(dir: &TempDir) -> (AppState, Arc<FakeBackend>) {
    state_with_backend(dir, backend())
}

fn state_with_backend(dir: &TempDir, backend: Arc<FakeBackend>) -> (AppState, Arc<FakeBackend>) {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        let mut project = crate::commands::tests::support::default_project();
        project.tracks.push(make_audio_track("vox"));
        inner.project = project;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.file_path = Some(dir.path().join("song.ocp"));
        inner.audio = crate::audio::AudioState::with_backend(backend.clone());
        inner.audio.ensure_input();
    }
    (state, backend)
}

#[test]
fn punch_uses_the_project_clock_and_writes_at_the_capture_rate() {
    let dir = TempDir::new("record-input-rate");
    let capture_rate = 44_100;
    let (state, backend) = state_with_backend(&dir, backend_with_capture_rate(capture_rate));
    {
        let mut inner = state.lock_inner();
        assert_eq!(inner.audio.sample_rate, RATE);
        assert_eq!(inner.audio.capture_format(), Some((capture_rate, 1)));
        inner
            .audio
            .set_punch_region(Some(0.5), Some(1.5), true)
            .unwrap();
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
    }
    push(&state, &backend, (capture_rate * 2) as usize);
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).unwrap();
    let clip = inner.project.audio_clips()[0];
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(1, 1, 2));
    assert_eq!(clip.time_range.end, oc_dsl::ast::BarBeat::new(1, 2, 2));
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.spec().sample_rate, capture_rate);
    assert_eq!(reader.len(), capture_rate / 2);
}

#[test]
fn device_changes_cannot_replace_a_take_but_monitor_controls_still_work() {
    use oc_audio::audio_io::{AudioFailure, InputChannels};

    let dir = TempDir::new("record-device-settings");
    let (state, backend) = state(&dir);
    let mut inner = state.lock_inner();
    inner.audio.recorder.armed = Some("vox".into());
    record::start_inner(&mut inner, RATE).unwrap();
    let opens = backend.input_opens().len();

    let mut input = inner.audio.audio_io.input_selection();
    input.input_channels = InputChannels::Mono;
    let view = inner.audio.set_input_io(input);
    assert_eq!(
        view.status.input_failure,
        Some(AudioFailure::RecordingActive)
    );
    assert_eq!(view.status.input.input_channels, InputChannels::Stereo);

    let mut output = inner.audio.audio_io.selection();
    output.sample_rate = Some(44_100);
    let view = inner.audio.set_audio_io(output);
    assert_eq!(view.status.failure, Some(AudioFailure::RecordingActive));
    assert_eq!(view.status.selection.sample_rate, None);

    let view = inner.audio.set_input_monitor(true, -12.0);
    assert!(view.input.monitoring);
    assert_eq!(view.input.monitor_gain_db, -12.0);
    assert_eq!(backend.input_opens().len(), opens);
    assert!(inner.audio.recorder.recording());
}

#[test]
fn an_unexpected_capture_layout_preserves_only_valid_frames_and_reopens() {
    let dir = TempDir::new("record-layout-change");
    let (state, backend) = state(&dir);
    let mut inner = state.lock_inner();
    inner.audio.recorder.armed = Some("vox".into());
    record::start_inner(&mut inner, RATE).unwrap();
    backend.push_input(&[0.25; 512], 1);
    backend.push_input(&[0.75; 512], 2);
    backend.push_input(&[0.75; 512], 1);
    drop(inner);
    let snapshot = crate::commands::transport::transport_state_inner(&state).unwrap();
    assert!(
        snapshot.project.is_some(),
        "the timeline receives the recovered take"
    );

    let mut inner = state.lock_inner();
    assert!(!inner.audio.recorder.recording());
    let clip = inner.project.audio_clips()[0];
    let mut reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.len(), 512);
    assert!(
        reader
            .samples::<f32>()
            .all(|sample| sample.unwrap() == 0.25)
    );
    assert!(
        inner
            .audio
            .recorder
            .report
            .as_deref()
            .unwrap()
            .contains("channel layout")
    );
    assert!(!inner.audio.input_view().open);
    record::start_inner(&mut inner, RATE).unwrap();
    assert!(inner.audio.input_view().open);
    record::stop_inner(&mut inner).unwrap();
}

#[test]
fn an_output_rate_change_finishes_the_take_on_the_old_clock() {
    let dir = TempDir::new("record-output-change");
    let (state, backend) = state(&dir);
    let mut inner = state.lock_inner();
    inner.audio.recorder.armed = Some("vox".into());
    record::start_inner(&mut inner, RATE).unwrap();
    backend.push_input(&[0.25; 512], 1);
    backend.set_devices(vec![OutputDevice {
        key: "coreaudio:other".into(),
        name: "Other output".into(),
        is_default: true,
        channels: 2,
        sample_rates: vec![44_100],
        default_rate: 44_100,
        buffer_frames: None,
    }]);
    inner.audio.refresh_audio_io();
    assert_eq!(
        inner.audio.sample_rate, RATE,
        "the take still owns its clock"
    );
    backend.push_input(&[0.75; 512], 1);
    drop(inner);

    let snapshot = crate::commands::transport::transport_state_inner(&state).unwrap();
    assert!(snapshot.project.is_some());
    let mut inner = state.lock_inner();
    assert!(!inner.audio.recorder.recording());
    let clip = inner.project.audio_clips()[0];
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.spec().sample_rate, RATE);
    assert_eq!(reader.len(), 512);
    inner.audio.refresh_audio_io();
    assert_eq!(inner.audio.sample_rate, 44_100);
}

/// Arm, start, push `frames` of capture, drain, and stop. Returns the clip.
fn record(state: &AppState, backend: &FakeBackend, frames: usize) -> oc_dsl::ast::AudioClip {
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    let block = vec![0.25f32; 512];
    let mut pushed = 0;
    while pushed < frames {
        let take = (frames - pushed).min(block.len());
        backend.push_input(&block[..take], 1);
        pushed += take;
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
    }
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    inner
        .project
        .audio_clips()
        .into_iter()
        .last()
        .cloned()
        .expect("a recorded clip")
}

#[test]
fn a_recording_lands_as_a_clip_at_the_record_position() {
    let dir = TempDir::new("record-clip");
    let (state, backend) = state(&dir);
    let clip = record(&state, &backend, 24_000);
    assert_eq!(clip.track, "vox");
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(1, 1, 0));
    assert!(clip.time_range.end != clip.time_range.start);
    assert!(clip.file.starts_with("media/"), "{}", clip.file);
    let path = dir.path().join(&clip.file);
    let reader = hound::WavReader::open(&path).expect("the take is a readable WAV");
    assert_eq!(reader.spec().sample_rate, RATE);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(
        reader.len() as usize,
        24_000,
        "every captured frame is written"
    );
    // The clip's range covers the whole take, rounded up to a beat subdivision.
    let clock = oc_dsl::timing::ProjectClock::new(&state.lock_inner().project.properties).unwrap();
    let start = clock.quarter_notes_at(&clip.time_range.start).unwrap();
    let end = clock.quarter_notes_at(&clip.time_range.end).unwrap();
    let seconds = clock.seconds_at_quarter(end).unwrap() - clock.seconds_at_quarter(start).unwrap();
    assert!(
        (0.499..0.75).contains(&seconds),
        "the clip covers the half-second take: {seconds}"
    );
}

#[test]
fn a_cold_recording_starts_at_the_parked_cursor() {
    let dir = TempDir::new("record-parked-cursor");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner.audio.seek(4.0, &clock).unwrap();
        assert_eq!(inner.audio.playhead_frame(), 0, "nothing has rendered");
    }
    let clip = record(&state, &backend, QUARTER);
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(2, 1, 0));
    assert_eq!(clip.time_range.end, oc_dsl::ast::BarBeat::new(2, 2, 0));
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.len() as usize, QUARTER);
}

#[test]
fn the_take_is_trimmed_by_the_reported_latency() {
    let dir = TempDir::new("record-latency");
    let (state, backend) = state(&dir);
    // One capture callback of 512 frames is the input side's own delay.
    backend.push_input(&vec![0.0f32; 512], 1);
    {
        let mut inner = state.lock_inner();
        assert_eq!(inner.audio.input_latency_frames(), 512);
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    let block = vec![0.5f32; 512];
    for _ in 0..48 {
        backend.push_input(&block, 1);
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
    }
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    let clip = inner
        .project
        .audio_clips()
        .into_iter()
        .last()
        .cloned()
        .expect("clip");
    // The clip starts at the record position and the file starts there too:
    // the latency is dropped from the head, so the clip carries no offset.
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(1, 1, 0));
    assert_eq!(
        clip.offset_seconds, 0.0,
        "the file starts at the clip position, so the take carries no offset"
    );
    let path = dir.path().join(&clip.file);
    let reader = hound::WavReader::open(&path).expect("wav");
    assert_eq!(
        reader.len() as usize,
        48 * 512 - 512,
        "the leading latency frames are dropped"
    );
}

#[test]
fn a_refused_start_leaves_the_project_unchanged() {
    let dir = TempDir::new("record-refused");
    let (state, backend) = state(&dir);
    let before = state.lock_inner().project.clone();
    {
        let mut inner = state.lock_inner();
        let error = record::start_inner(&mut inner, RATE).unwrap_err();
        assert_eq!(error, "No track is armed for recording.");
        assert_eq!(
            inner.audio.recorder.report.as_deref(),
            Some(
                "Recording could not start: No track is armed for recording. The project is unchanged."
            )
        );
    }
    // With an armed track but no input, the press is refused for that reason.
    backend.set_inputs(Vec::new());
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        let error = record::start_inner(&mut inner, RATE).unwrap_err();
        assert_eq!(error, "No audio input is connected.");
    }
    let after = state.lock_inner().project.clone();
    assert_eq!(after, before, "a refused press changes nothing");
    assert!(after.audio_clips().is_empty());
}

#[test]
fn an_unsaved_project_asks_for_a_save_instead_of_writing_a_take() {
    let dir = TempDir::new("record-unsaved");
    let (state, _backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.file_path = None;
        inner.audio.recorder.armed = Some("vox".to_string());
        let error = record::start_inner(&mut inner, RATE).unwrap_err();
        assert_eq!(
            error,
            "Save the project first: a take needs a media folder."
        );
        assert!(inner.project.audio_clips().is_empty());
    }
}

#[test]
fn a_capture_error_finalises_the_take_and_says_why() {
    let dir = TempDir::new("record-interrupted");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    backend.push_input(&vec![0.2f32; 4_800], 1);
    {
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
    }
    backend.disconnect_input("the microphone was unplugged");
    {
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
        assert!(!inner.audio.recorder.recording(), "the take is over");
        assert_eq!(inner.project.audio_clip_count(), 1, "the take is kept");
        let report = inner.audio.recorder.report.clone().expect("a report");
        assert!(
            report.contains("the microphone was unplugged") && report.contains("kept"),
            "{report}"
        );
    }
}

#[test]
fn arming_only_accepts_a_track_the_project_has() {
    let dir = TempDir::new("record-arm");
    let (state, _backend) = state(&dir);
    let mut inner = state.lock_inner();
    inner.audio.recorder.armed = Some("vox".to_string());
    // Disarming always works.
    inner.audio.recorder.armed = None;
    assert!(inner.audio.recorder.armed.is_none());
    // A name the project lacks is refused by the command path.
    let error = record::disabled_reason(&inner);
    assert!(error.is_none(), "an armed or armable project has no reason");
}

#[test]
fn record_state_reports_the_armed_track_and_the_take_progress() {
    let dir = TempDir::new("record-state");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
        let view = record::state_of(&inner);
        assert_eq!(view.armed.as_deref(), Some("vox"));
        assert!(view.recording);
        assert!(view.disabled_reason.is_none());
    }
    backend.push_input(&vec![0.1f32; 1_000], 1);
    {
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
        record::stop_inner(&mut inner).expect("stop");
        let view = record::state_of(&inner);
        assert!(!view.recording, "stopping never leaves a stuck state");
        assert!(view.report.is_some());
    }
}

/// Push `frames` of input and poll until the transport has taken them.
fn push(state: &AppState, backend: &FakeBackend, frames: usize) {
    let block = vec![0.25f32; 512];
    let mut pushed = 0;
    while pushed < frames {
        let take = (frames - pushed).min(block.len());
        backend.push_input(&block[..take], 1);
        pushed += take;
        let mut inner = state.lock_inner();
        record::poll_inner(&mut inner).expect("poll");
    }
}

#[test]
fn a_loop_session_writes_one_take_per_pass_and_plays_the_newest() {
    let dir = TempDir::new("record-loop");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        // Four quarters at 120 BPM: 96 000 frames.
        inner
            .audio
            .set_loop_region(Some(0.0), Some(4.0), true, &clock)
            .unwrap();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    push(&state, &backend, 96_000);
    push(&state, &backend, 4_800);
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 2, "one clip per pass");
    assert_eq!(clips[0].take, Some(1));
    assert_eq!(clips[1].take, Some(2));
    assert!(clips.iter().all(|clip| clip.take_group.is_some()));
    assert_eq!(
        clips[0].take_group, clips[1].take_group,
        "passes of one session share a group"
    );
    assert!(clips[0].muted, "the earlier pass is muted");
    assert!(!clips[1].muted, "the newest pass plays");
    assert_eq!(clips[0].file, "media/take-vox-1.wav");
    assert_eq!(clips[1].file, "media/take-vox-2.wav");
    assert_eq!(
        clips[0].time_range,
        oc_dsl::ast::TimeRange {
            start: oc_dsl::ast::BarBeat::new(1, 1, 0),
            end: oc_dsl::ast::BarBeat::new(2, 1, 0),
        }
    );
    let wav = dir.path().join(&clips[0].file);
    let reader = hound::WavReader::open(&wav).expect("first take wav");
    assert_eq!(reader.len() as usize, 96_000);
    let report = inner.audio.recorder.report.clone().expect("report");
    assert!(report.contains("Recorded 2 takes"), "{report}");
    assert!(
        report.contains("Take 2 is playing; open Takes to comp."),
        "{report}"
    );
}

#[test]
fn a_count_in_longer_than_the_loop_does_not_use_any_pass_frames() {
    let dir = TempDir::new("record-short-loop-count-in");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner
            .audio
            .set_loop_region(Some(0.0), Some(1.0), true, &clock)
            .unwrap();
        inner.audio.click.count_in = CountIn::OneBar;
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
    }
    // A whole bar and a capture period arrive before the first performance block.
    for _ in 0..(BAR / 500 + 1) {
        backend.push_input(&[0.125; 500], 1);
        record::poll_at(&mut state.lock_inner(), 0).unwrap();
    }
    assert!(!dir.path().join("media/take-vox-2.wav").exists());

    // The first timeline poll sees one performance block plus the input delay.
    backend.push_input(&[0.25; 500], 1);
    record::poll_at(&mut state.lock_inner(), 1_000).unwrap();
    push(&state, &backend, QUARTER - 600);
    // The final callback crosses the seam before the next transport poll.
    let final_block = [[0.25; 100].as_slice(), &[0.75; 512]].concat();
    backend.push_input(&final_block, 1);
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).unwrap();
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 2);
    for (clip, (frames, expected)) in clips.iter().zip([(QUARTER, 0.25), (512, 0.75)]) {
        let mut wav = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
        assert_eq!(wav.len() as usize, frames);
        assert!(
            wav.samples::<f32>()
                .all(|sample| sample.unwrap() == expected)
        );
    }
}

#[test]
fn a_later_pass_open_failure_preserves_the_completed_take() {
    let dir = TempDir::new("record-later-pass-open-failure");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner
            .audio
            .set_loop_region(Some(0.0), Some(1.0), true, &clock)
            .unwrap();
        inner.audio.recorder.armed = Some("vox".into());
        inner.audio.recorder.fail_next_pass_open = true;
        record::start_inner(&mut inner, RATE).unwrap();
    }
    let first_block = [vec![0.125; 512], vec![0.25; QUARTER - 512]].concat();
    backend.push_input(&first_block, 1);
    let mut inner = state.lock_inner();
    // One input period precedes the performance. Opening pass two fails
    // immediately after the first WAV has been finalized.
    backend.push_input(&[0.25; 512], 1);
    record::poll_at(&mut inner, QUARTER as u64 + 512).unwrap();
    assert!(!inner.audio.recorder.recording());
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 1);
    assert!(!clips[0].muted);
    let mut reader = hound::WavReader::open(dir.path().join(&clips[0].file)).unwrap();
    assert_eq!(reader.len() as usize, QUARTER);
    assert!(
        reader
            .samples::<f32>()
            .all(|sample| sample.unwrap() == 0.25)
    );
    let report = inner.audio.recorder.report.as_deref().unwrap();
    assert!(
        report.contains("no space left for the next take"),
        "{report}"
    );
    assert!(report.contains("Recorded 1 take"), "{report}");
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn stop_returns_the_recovered_project_when_the_next_pass_cannot_open() {
    let dir = TempDir::new("record-stop-later-pass-open-failure");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner
            .audio
            .set_loop_region(Some(0.0), Some(1.0), true, &clock)
            .unwrap();
        inner.audio.recorder.armed = Some("vox".into());
        inner.audio.recorder.fail_next_pass_open = true;
        record::start_inner(&mut inner, RATE).unwrap();
    }
    backend.push_input(&vec![0.25; QUARTER + 512], 1);
    let stopped = crate::commands::transport::stop_inner(&state).unwrap();
    assert!(!stopped.playing);
    assert!(!stopped.record.recording);
    let report = stopped.record.report.unwrap();
    assert!(
        report.contains("no space left for the next take"),
        "{report}"
    );
    let project = stopped.project.unwrap().project;
    let clip = project.audio_clips()[0];
    assert!(!clip.muted);
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.len() as usize, QUARTER);
}

#[test]
fn a_refused_placement_reports_the_saved_wav_path() {
    for moved_project in [false, true] {
        let dir = TempDir::new("record-preserve-unplaced-take");
        let (state, backend) = state(&dir);
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
        backend.push_input(&[0.25; 512], 1);
        if moved_project {
            inner.file_path = Some(dir.path().join("other/song.ocp"));
        } else {
            inner.project.tracks.clear();
        }
        drop(inner);
        let error = crate::commands::transport::stop_inner(&state).unwrap_err();
        let path = dir.path().join("media/take-vox-1.wav");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
        if moved_project {
            assert!(error.contains("project folder changed"), "{error}");
        }
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.len(), 512);
        let inner = state.lock_inner();
        assert!(inner.project.audio_clips().is_empty());
        assert!(!inner.audio.recorder.recording());
        assert_eq!(inner.audio.recorder.report.as_deref(), Some(error.as_str()));
        assert!(inner.undo.undo.is_empty());
    }
}

#[test]
fn replacing_or_moving_the_project_waits_for_the_active_take() {
    use crate::commands::project::{
        create_project_in, open_project, publish_save, snapshot_for_save,
    };

    let dir = TempDir::new("record-project-guard");
    let (state, backend) = state(&dir);
    let elsewhere = dir.path().join("elsewhere/song.ocp");
    let elsewhere = elsewhere.to_string_lossy().into_owned();
    let pending_save = snapshot_for_save(Some(elsewhere.clone()), &state).unwrap();
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
        assert!(create_project_in(&mut inner, None).is_err());
        assert!(open_project(&mut inner, "@project\n  tempo: 100\n", None).is_err());
        assert!(inner.audio.recorder.recording());
    }
    assert!(snapshot_for_save(None, &state).is_ok());
    assert!(snapshot_for_save(Some(elsewhere), &state).is_err());
    assert_eq!(publish_save(&state, &pending_save), None);
    backend.push_input(&[0.25; 512], 1);
    let result = crate::commands::transport::stop_inner(&state).unwrap();
    assert_eq!(
        result.project.unwrap().project.audio_clips()[0].track,
        "vox"
    );
}

#[test]
fn an_active_take_blocks_exit_until_the_user_finishes_it() {
    let dir = TempDir::new("record-quit-finalize");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.dirty = false;
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
        assert!(crate::quit_intent::needs_decision(&inner));
        assert!(crate::quit_intent::arm_exit(&mut inner).is_err());
        assert!(!inner.exit_ready);
    }
    backend.push_input(&[0.25; 512], 1);
    let prepared = crate::quit_intent::prepare(&state);
    assert!(prepared.recording);
    assert!(!prepared.dirty);
    let stopped = crate::commands::transport::stop_inner(&state).unwrap();
    assert!(crate::quit_intent::prepare(&state).dirty);
    assert!(!stopped.record.recording);
    let project = stopped.project.unwrap().project;
    let clip = project.audio_clips()[0];
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).unwrap();
    assert_eq!(reader.len(), 512);
    assert!(crate::quit_intent::arm_exit(&mut state.lock_inner()).is_ok());
    let error = crate::commands::transport::start_recording_inner(&state).unwrap_err();
    assert!(error.contains("while quitting"));
}

#[test]
fn a_take_refuses_seek_pause_and_auditions_without_interrupting_capture() {
    use crate::commands::transport::{pause_inner, preview_track_inner, seek_inner, stop_inner};
    let dir = TempDir::new("record-transport-controls");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".into());
        record::start_inner(&mut inner, RATE).unwrap();
        assert!(inner.audio.replace_private_audition(false).is_err());
        assert!(inner.audio.play_audition_buffer(vec![0.5; 64]).is_err());
    }
    assert!(
        seek_inner(4.0, &state)
            .unwrap_err()
            .contains("Stop recording")
    );
    assert!(pause_inner(&state).unwrap_err().contains("Stop recording"));
    assert!(
        preview_track_inner(0, false, &state)
            .unwrap_err()
            .contains("Stop recording")
    );
    backend.push_input(&[0.25; 512], 1);
    let stopped = stop_inner(&state).unwrap();
    assert!(!stopped.record.recording);
    assert_eq!(stopped.project.unwrap().project.audio_clips().len(), 1);
    assert_eq!(seek_inner(4.0, &state).unwrap().position_beats, 4.0);
}

#[test]
fn a_record_press_leaves_a_paused_private_audition_untouched() {
    let dir = TempDir::new("record-active-audition");
    let (state, _) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".into());
        inner.audio.replace_private_audition(true).unwrap();
    }
    let error = crate::commands::transport::start_recording_inner(&state).unwrap_err();
    assert!(error.contains("Stop the audition"));
    let inner = state.lock_inner();
    assert!(inner.audio.audition_blocks_recording());
    assert!(!inner.audio.recorder.recording());
    assert!(!dir.path().join("media/take-vox-1.wav").exists());
}

#[cfg(unix)]
#[test]
fn a_dangling_take_symlink_is_an_occupied_name() {
    let dir = TempDir::new("record-occupied-take");
    let (state, backend) = state(&dir);
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let target = dir.path().join("missing.wav");
    std::os::unix::fs::symlink(&target, media.join("take-vox-1.wav")).unwrap();

    let clip = record(&state, &backend, 512);
    assert_eq!(clip.file, "media/take-vox-1-2.wav");
    assert!(!target.exists());
}

#[test]
fn punch_trims_the_take_and_moves_the_clip_to_the_punch_in() {
    let dir = TempDir::new("record-punch");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        // Keep only 0.5 s .. 1.5 s of a 2 s pass.
        inner
            .audio
            .set_punch_region(Some(0.5), Some(1.5), true)
            .unwrap();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    push(&state, &backend, 96_000);
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    let clip = inner
        .project
        .audio_clips()
        .into_iter()
        .last()
        .cloned()
        .expect("clip");
    assert_eq!(clip.time_range.start, oc_dsl::ast::BarBeat::new(1, 1, 2));
    assert_eq!(clip.time_range.end, oc_dsl::ast::BarBeat::new(1, 2, 2));
    assert_eq!(clip.offset_seconds, 0.0, "the file starts at the punch in");
    let reader = hound::WavReader::open(dir.path().join(&clip.file)).expect("wav");
    assert_eq!(reader.len() as usize, 24_000, "only the punch span is kept");
}

/// One recorded impulse and where it ended up in the committed clip.
struct Impulse {
    clip: oc_dsl::ast::AudioClip,
    /// Frames the impulse plays away from the frame it was performed on.
    error_frames: f64,
}

/// Record a take with an impulse performed `after_roll` frames after the
/// transport rolled, and measure the beat it was written at.
///
/// The capture is pushed one callback block at a time and the transport is
/// driven at the position of the samples the callback captured, which is what
/// a capture device does: it hands the pump one block late.
fn record_impulse(
    label: &str,
    wait: usize,
    count_in: CountIn,
    punch: Option<(f64, f64)>,
    after_roll: usize,
) -> Impulse {
    const BLOCK: usize = 512;
    let dir = TempDir::new(label);
    let (state, backend) = state(&dir);
    let count_in_frames = usize::from(count_in.bars()) * BAR;
    // Frames captured between the press and the roll: the wait for the first
    // block, then the count-in.
    let roll = wait + count_in_frames;
    let punch_skip = punch.map_or(0, |(start, _)| (start * QUARTER as f64).round() as usize);
    // The impulse is performed on the roll, or on the punch in when armed.
    let played_at = roll + punch_skip + after_roll;
    let total = played_at + 2 * QUARTER;
    {
        let mut inner = state.lock_inner();
        inner.audio.click.count_in = count_in;
        if let Some((start, end)) = punch {
            inner
                .audio
                .set_punch_region(Some(start), Some(end), true)
                .expect("punch");
        }
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
    }
    let mut delivered = 0usize;
    while delivered < total {
        let block = (total - delivered).min(BLOCK);
        let mut samples = vec![0.0f32; block];
        if played_at >= delivered && played_at < delivered + block {
            samples[played_at - delivered] = 1.0;
        }
        backend.push_input(&samples, 1);
        delivered += block;
        // The playhead is timeline frames: it holds at the record start through
        // the wait for the first block and the count-in, then reports the
        // frames played past the roll. The callback hands the pump a block one
        // block late, so the capture position is a block further on.
        let captured = delivered + BLOCK;
        let playhead = captured.saturating_sub(roll);
        let mut inner = state.lock_inner();
        record::poll_at(&mut inner, playhead as u64).expect("poll");
    }
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    let clip = inner
        .project
        .audio_clips()
        .into_iter()
        .last()
        .cloned()
        .expect("a recorded clip");
    let path = dir.path().join(&clip.file);
    let mut reader = hound::WavReader::open(&path).expect("wav");
    let samples: Vec<f32> = reader
        .samples::<f32>()
        .map(|s| s.expect("sample"))
        .collect();
    let index = samples
        .iter()
        .position(|sample| sample.abs() > 0.5)
        .expect("the impulse is in the take");
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).expect("clock");
    let start = clock
        .quarter_notes_at(&clip.time_range.start)
        .expect("clip start");
    // The clip plays media frame zero at its start, so the impulse's beat is
    // its file frame plus however many media frames the clip offsets past.
    let played_frames = index as f64 + clip.offset_seconds * f64::from(RATE);
    let expected = start + after_roll as f64 / QUARTER as f64;
    let measured = start + played_frames / QUARTER as f64;
    let error_frames = (measured - expected) * QUARTER as f64;
    println!("{label}: the impulse plays {error_frames:.1} frames from where it was performed");
    Impulse { error_frames, clip }
}

#[test]
fn an_impulse_on_beat_one_lines_up_with_the_count_in_off() {
    // A cold press waits 100 ms for the first block.
    let take = record_impulse("record-align-off", 4_800, CountIn::Off, None, 6_000);
    assert_eq!(
        take.clip.time_range.start,
        oc_dsl::ast::BarBeat::new(1, 1, 0)
    );
    assert_eq!(take.clip.offset_seconds, 0.0);
    assert!(
        take.error_frames.abs() < 1.0,
        "the impulse plays {:.1} frames away from beat 1",
        take.error_frames
    );
}

#[test]
fn an_impulse_on_beat_one_lines_up_through_a_one_bar_count_in() {
    // The take waits 150 ms for the first block, then a full bar of count-in.
    let take = record_impulse("record-align-count-in", 7_200, CountIn::OneBar, None, 6_000);
    assert_eq!(
        take.clip.time_range.start,
        oc_dsl::ast::BarBeat::new(1, 1, 0)
    );
    assert_eq!(take.clip.offset_seconds, 0.0);
    assert!(
        take.error_frames.abs() < 1.0,
        "the impulse plays {:.1} frames away from beat 1",
        take.error_frames
    );
}

#[test]
fn stopping_during_the_count_in_writes_no_take() {
    let dir = TempDir::new("record-cancel-count-in");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner
            .audio
            .set_loop_region(Some(0.0), Some(1.0), true, &clock)
            .unwrap();
        inner.audio.click.count_in = CountIn::OneBar;
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
        assert_eq!(
            inner.audio.playhead_frame(),
            0,
            "the take holds at the record start"
        );
    }
    // The count-in is still sounding: the transport holds the playhead at the
    // record start, so none of the captured frames belongs to the take.
    for _ in 0..(QUARTER / 512 + 1) {
        backend.push_input(&[0.5; 512], 1);
        let mut inner = state.lock_inner();
        record::poll_at(&mut inner, 0).expect("poll");
    }
    let mut inner = state.lock_inner();
    record::stop_inner(&mut inner).expect("stop");
    assert!(!inner.audio.recorder.recording(), "the take is closed");
    assert!(
        inner.project.audio_clips().is_empty(),
        "a cancelled count-in writes no clip"
    );
    assert!(
        inner.audio.recorder.report.is_none(),
        "a cancelled count-in reports nothing: {:?}",
        inner.audio.recorder.report
    );
    assert!(
        !dir.path().join("media/take-vox-1.wav").exists(),
        "the take file is removed"
    );
    assert!(!dir.path().join("media/take-vox-2.wav").exists());
    assert_eq!(
        inner.audio.playhead_frame(),
        0,
        "the transport is stopped at the record start"
    );
}

#[test]
fn an_impulse_on_the_punch_in_lines_up() {
    // Punch in on beat 2 with a one-bar count-in; the impulse is on the punch in.
    let take = record_impulse(
        "record-align-punch",
        9_600,
        CountIn::OneBar,
        Some((1.0, 2.0)),
        6_000,
    );
    assert_eq!(
        take.clip.time_range.start,
        oc_dsl::ast::BarBeat::new(1, 2, 0)
    );
    assert_eq!(take.clip.offset_seconds, 0.0);
    assert!(
        take.error_frames.abs() < 1.0,
        "the impulse plays {:.1} frames away from the punch in",
        take.error_frames
    );
}

#[test]
fn a_silent_take_writes_no_clip_and_says_so() {
    let dir = TempDir::new("record-silent");
    let (state, _backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
        record::stop_inner(&mut inner).expect("stop");
        assert!(inner.project.audio_clips().is_empty());
        assert_eq!(
            inner.audio.recorder.report.as_deref(),
            Some("Nothing was recorded: the input was silent.")
        );
    }
    assert!(
        !dir.path().join("media/take-vox-1.wav").exists(),
        "an empty take is removed"
    );
}

/// A project the streaming mixer refuses: the play after the press fails.
fn unsupported_track() -> oc_dsl::ast::Track {
    let mut track = make_track("keys");
    // A preset the project does not declare makes the project unstreamable.
    track.preset = Some("missing".to_string());
    track
}

#[test]
fn a_record_press_whose_transport_fails_drops_the_take() {
    let dir = TempDir::new("record-play-fail");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.project.tracks.push(unsupported_track());
        inner.audio.click.count_in = CountIn::OneBar;
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("the take opens");
        assert!(inner.audio.recorder.recording());
        assert!(
            inner.audio.pending_count_in.is_some(),
            "the press queues the count-in"
        );
        assert!(dir.path().join("media/take-vox-1.wav").exists());
    }
    let error =
        crate::commands::transport::play_inner(&state).expect_err("the project cannot stream");
    assert!(error.contains("does not support"), "{error}");
    backend.push_input(&[0.25; 512], 1);
    {
        let mut inner = state.lock_inner();
        // The command's play-error path.
        crate::commands::transport::cancel_press(&mut inner, false);
        assert!(!inner.audio.recorder.recording(), "the take is dropped");
        assert!(
            inner.audio.pending_count_in.is_none(),
            "the count-in is dropped"
        );
        assert!(inner.project.audio_clips().is_empty(), "no clip is written");
    }
    assert!(
        !dir.path().join("media/take-vox-1.wav").exists(),
        "the take file is removed"
    );
    // The capture stopped filling the record ring.
    backend.push_input(&[0.5; 512], 1);
    {
        let inner = state.lock_inner();
        let mut captured = Vec::new();
        inner.audio.input_port.take_recorded(&mut captured, 1 << 20);
        assert!(captured.is_empty(), "the port stopped capturing");
    }
    // A second press starts a fresh take.
    {
        let mut inner = state.lock_inner();
        inner.project.tracks.retain(|track| track.name != "keys");
        record::start_inner(&mut inner, RATE).expect("a second press starts a fresh take");
        assert!(inner.audio.recorder.recording());
    }
    assert!(
        dir.path().join("media/take-vox-1.wav").exists(),
        "the second take writes its own file"
    );
}

#[test]
fn a_take_write_error_closes_the_take_and_a_later_stop_works() {
    let dir = TempDir::new("record-write-error");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
        inner
            .audio
            .recorder
            .session
            .as_mut()
            .expect("a take is open")
            .fail_writes();
    }
    // The transport rolls, so the take is anchored and writes what it is fed.
    backend.push_input(&vec![0.3f32; 512], 1);
    {
        let mut inner = state.lock_inner();
        record::poll_at(&mut inner, 512).expect("poll");
    }
    backend.push_input(&vec![0.3f32; 512], 1);
    let error =
        crate::commands::transport::stop_inner(&state).expect_err("the take cannot be written");
    assert!(error.contains("no space left on device"), "{error}");
    {
        let inner = state.lock_inner();
        assert!(!inner.audio.recorder.recording(), "the session is cleared");
        assert_eq!(
            inner.audio.playback_state,
            PlaybackState::Stopped,
            "the transport is stopped"
        );
        let report = inner
            .audio
            .recorder
            .report
            .clone()
            .expect("the failure is reported");
        assert!(report.contains("Recording failed"), "{report}");
    }
    assert!(
        !dir.path().join("media/take-vox-1.wav").exists(),
        "the broken file is removed"
    );
    // Every later Stop is an ordinary one.
    let snapshot = crate::commands::transport::stop_inner(&state).expect("a later Stop is Ok");
    assert!(!snapshot.playing, "the transport stays stopped");
}

#[test]
fn a_record_press_is_refused_while_a_preview_is_open() {
    let dir = TempDir::new("record-preview");
    let (state, _backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        let base = inner.dsl_source.clone();
        begin_inner(
            &mut inner,
            VOX_QUIETER.to_string(),
            "track:vox".to_string(),
            base,
        )
        .expect("the preview opens");
        let error = record::start_inner(&mut inner, RATE).unwrap_err();
        assert_eq!(error, READ_ONLY, "the press is refused like any edit");
        assert!(!inner.audio.recorder.recording(), "nothing is armed");
        assert!(inner.preview.is_some(), "the preview is still open");
        assert_eq!(
            inner.audio.recorder.report.as_deref(),
            Some(READ_ONLY),
            "the press shows the same sentence as every other refused edit"
        );
    }
    assert!(
        !dir.path().join("media").exists(),
        "no media folder is made"
    );
}

#[test]
fn a_preview_cannot_interrupt_a_recording_and_the_take_commits() {
    let dir = TempDir::new("record-preview-blocked");
    let (state, backend) = state(&dir);
    {
        let mut inner = state.lock_inner();
        inner.audio.recorder.armed = Some("vox".to_string());
        record::start_inner(&mut inner, RATE).expect("start");
        let project = inner.project.clone();
        let mut plugins = std::mem::take(&mut inner.plugin_instances);
        inner
            .audio
            .play(&project, &mut plugins)
            .expect("the transport starts");
        inner.plugin_instances = plugins;
        assert_eq!(inner.audio.playback_state, PlaybackState::Playing);
    }
    // The take is anchored as the transport rolls, so it writes frames.
    for step in 1..=8u64 {
        backend.push_input(&vec![0.4f32; 512], 1);
        let mut inner = state.lock_inner();
        record::poll_at(&mut inner, step * 512).expect("poll");
    }
    {
        let mut inner = state.lock_inner();
        let base = inner.dsl_source.clone();
        let error = begin_inner(
            &mut inner,
            VOX_QUIETER.to_string(),
            "track:vox".to_string(),
            base.clone(),
        )
        .unwrap_err();
        assert_eq!(error, "Stop recording before previewing an edit.");
        assert_eq!(inner.dsl_source, base);
        assert!(inner.preview.is_none());
        assert!(inner.audio.recorder.recording());
        assert_eq!(inner.audio.playback_state, PlaybackState::Playing);
    }
    crate::commands::transport::stop_inner(&state).expect("the recorded clip commits");
    {
        let inner = state.lock_inner();
        assert!(!inner.audio.recorder.recording(), "the session is cleared");
        assert_eq!(
            inner.audio.playback_state,
            PlaybackState::Stopped,
            "the transport is stopped"
        );
        assert_eq!(inner.project.audio_clips().len(), 1);
        assert!(inner.preview.is_none());
    }
    assert!(
        dir.path().join("media/take-vox-1.wav").exists(),
        "the committed clip has a WAV on disk"
    );
}
