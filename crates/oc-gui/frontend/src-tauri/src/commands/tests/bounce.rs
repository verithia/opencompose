//! Bounce and consolidate: one undo entry, atomic media, sample-aligned clips.

use std::path::Path;

use super::support::{TempDir, state_with_project};
use crate::commands::bounce::{
    BounceMode, BounceTail, capture_bounce_snapshot, capture_bounce_snapshot_with,
    capture_consolidate_snapshot, publish_bounce, publish_consolidate, render_bounce,
    render_consolidate,
};
use crate::state::AppState;
use oc_dsl::ast::BarBeat;

/// One held quarter note ending on the range end: a 200 ms release and 40 ms of
/// insert delay in two lookahead limiters, at 60 bpm so one quarter is a second.
const TAIL_PROJECT: &str = r#"@ocp "0.2.0"

@project "bounce tail"
  tempo: 60
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 0, amp.attack: 0.001s, amp.decay: 0.1s, amp.sustain: 0.8, amp.release: 0.2s, output.gain: 0.4}

@track lead | preset: tone, instrument: synth/lead_sine
  effects:
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
  @phrase held [1:1:0 - 1:2:0]
    C4 q mf
"#;

const PROJECT: &str = r#"@ocp "0.2.0"

@project "bounce test"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus hall
  routes: [master]
  effects:
    - reverb: decay=1.0s, mix=30%

@track lead | preset: tone, instrument: synth/lead_square
  mix: vol=-3dB, pan=0.25, send(hall=40%)
  effects:
    - limiter: ceiling=-1dB, lookahead=5ms, release=100ms
  @phrase held [1:1:0 - 5:1:0]
    C4 w mf
    C4 w mf
    C4 w mf
    C4 w mf
"#;

const CLIP_PROJECT: &str = r#"@ocp "0.2.0"

@project "consolidate test"
  tempo: 120
  time_signature: 4/4

@audio_clip one [1:1 - 2:1] | track: lead
  file: "media/one.wav"

@audio_clip two [2:1 - 3:1] | track: lead
  file: "media/two.wav"

@audio_clip other [1:1 - 2:1] | track: pad
  file: "media/one.wav"

@track lead | instrument: audio/clip
  effects:
    - eq: low_gain=-2dB

@track pad | instrument: audio/clip
"#;

/// Bounce the whole phrase in place: every block is inside the range.
fn bounce_in_place(state: &AppState) -> crate::commands::bounce::BounceResult {
    let snapshot = capture_bounce_snapshot(state, "lead", "1:1", "5:1", BounceMode::InPlace)
        .expect("in-place snapshot");
    let rendered = render_bounce(snapshot).expect("in-place render");
    publish_bounce(&mut state.lock_inner(), rendered).expect("in-place publish")
}

/// A project with a builtin instrument track, saved next to `dir`.
fn bounce_state(dir: &TempDir) -> AppState {
    let state = state_with_project(PROJECT, dir.path());
    {
        let mut inner = state.lock_inner();
        inner.content_epoch = 7;
    }
    state
}

/// Write a short tone so an audio clip has media to decode.
fn write_media(dir: &Path, name: &str, frequency: u32) {
    let media = dir.join("media");
    std::fs::create_dir_all(&media).unwrap();
    let rate = 44_100;
    let samples: Vec<f32> = (0..rate / 2)
        .flat_map(|frame| {
            let value = 0.25
                * (2.0 * std::f64::consts::PI * f64::from(frequency) * frame as f64
                    / f64::from(rate))
                .sin() as f32;
            [value, value]
        })
        .collect();
    oc_engine::wav::write_stereo_f32_wav(media.join(name), &samples, rate).unwrap();
}

fn media_files(dir: &Path) -> Vec<String> {
    let media = dir.join("media");
    let Ok(entries) = std::fs::read_dir(&media) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The refusal a request returns; `T` carries no `Debug`, so the error is read
/// straight out of the `Result`.
fn expect_refusal<T>(result: Result<T, String>, needle: &str) -> String {
    let error = result.err().expect("the request is refused");
    assert!(error.contains(needle), "{error}");
    error
}

#[test]
fn a_new_track_bounce_publishes_one_clip_and_one_undo_entry() {
    let dir = TempDir::new("bounce-new-track");
    let state = bounce_state(&dir);
    let source_output = {
        let inner = state.lock_inner();
        inner.project.tracks[0].mix.output.clone()
    };
    let snapshot = capture_bounce_snapshot(&state, "lead", "2:1", "3:1", BounceMode::NewTrack)
        .expect("snapshot");
    let rendered = render_bounce(snapshot).expect("render");
    let frames = rendered.frames;
    let result = publish_bounce(&mut state.lock_inner(), rendered).expect("publish");

    assert_eq!(result.target_track, "lead (bounced)");
    assert_eq!(result.mode, BounceMode::NewTrack);
    assert!(
        result.media.starts_with("media/bounce-lead-"),
        "{}",
        result.media
    );
    assert_eq!(
        result.baked,
        vec!["limiter".to_string()],
        "the inserts are baked"
    );
    assert!(
        Path::new(&result.path).is_file(),
        "the media file is published"
    );

    let inner = state.lock_inner();
    let project = &inner.project;
    assert_eq!(project.tracks.len(), 2, "the source track stays");
    assert!(
        project.tracks[0].mix.mute,
        "the source track is muted, not cleared"
    );
    assert_eq!(
        project.tracks[0].effects.len(),
        1,
        "the source keeps its inserts"
    );
    assert!(!project.tracks[0].pattern_clip_count() == 0 || !project.tracks[0].phrases.is_empty());
    let bounced = &project.tracks[1];
    assert_eq!(bounced.name, "lead (bounced)");
    assert_eq!(bounced.mix.output, source_output, "same output routing");
    assert_eq!(bounced.mix.sends.len(), 1, "same send routing");
    assert_eq!(bounced.mix.volume_db, -3.0, "the fader is not baked");
    assert_eq!(bounced.mix.pan, 0.25);
    assert!(
        bounced.effects.is_empty(),
        "the inserts are baked, not copied"
    );
    assert!(bounced.preset.is_none());
    assert_eq!(bounced.instrument.category, "audio");

    assert_eq!(project.audio_clip_count(), 1);
    let clip = project.audio_clip(0).unwrap();
    assert_eq!(clip.track, "lead (bounced)");
    assert_eq!(clip.time_range.start, BarBeat::new(2, 1, 0));
    // The clip carries the tail: 5 ms of insert delay and the 200 ms release
    // allowance, rounded up to the notated grid.
    assert_eq!(clip.time_range.end, BarBeat::new(3, 2, 1));
    assert_eq!(clip.offset_seconds, 0.0);
    assert_eq!(clip.gain_db, 0.0);
    assert_eq!(clip.file, result.media);

    let spec = hound::WavReader::open(&result.path).unwrap().spec();
    assert_eq!(spec.sample_rate, result.sample_rate);
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.channels, 2);
    assert_eq!(
        hound::WavReader::open(&result.path).unwrap().len() as usize,
        frames * 2
    );

    assert_eq!(inner.undo.undo.len(), 1, "one undo entry");
    assert_eq!(inner.undo.undo[0].label, "bounce track");
    assert!(result.report.contains("the source track is muted"));
    assert!(
        result.report.contains("limiter"),
        "the report names what was baked: {}",
        result.report
    );
    assert_eq!(inner.project.audio_clip(0).unwrap().name, result.clip);
}

#[test]
fn a_second_bounce_picks_the_next_media_number() {
    let dir = TempDir::new("bounce-number");
    let state = bounce_state(&dir);
    let first = {
        let snapshot = capture_bounce_snapshot(&state, "lead", "2:1", "3:1", BounceMode::NewTrack)
            .expect("snapshot");
        let rendered = render_bounce(snapshot).expect("render");
        publish_bounce(&mut state.lock_inner(), rendered).expect("publish")
    };
    let second = {
        let snapshot = capture_bounce_snapshot(&state, "lead", "2:1", "3:1", BounceMode::NewTrack)
            .expect("snapshot");
        let rendered = render_bounce(snapshot).expect("render");
        publish_bounce(&mut state.lock_inner(), rendered).expect("publish")
    };
    assert_eq!(first.media, "media/bounce-lead-1.wav");
    assert_eq!(second.media, "media/bounce-lead-2.wav");
    assert_ne!(
        second.target_track, first.target_track,
        "track names stay unique"
    );
    assert_eq!(state.lock_inner().undo.undo.len(), 2);
}

#[test]
fn an_in_place_bounce_bypasses_the_chain_and_replaces_the_blocks() {
    let dir = TempDir::new("bounce-in-place");
    let state = bounce_state(&dir);
    let result = bounce_in_place(&state);

    assert_eq!(result.mode, BounceMode::InPlace);
    assert_eq!(result.target_track, "lead");
    assert_eq!(result.baked, vec!["limiter".to_string()]);
    assert!(
        result.report.contains("Bypassed insert chain (limiter)"),
        "{}",
        result.report
    );
    assert!(
        result.report.contains("in place over 1:1 to 5:1"),
        "{}",
        result.report
    );

    let inner = state.lock_inner();
    let track = &inner.project.tracks[0];
    assert!(track.effects[0].bypass, "the insert chain is bypassed");
    assert!(track.phrases.is_empty(), "the covered phrase is replaced");
    assert!(track.pattern_clip_count() == 0);
    assert!(!track.mix.mute, "an in-place bounce leaves the mute alone");
    assert_eq!(inner.project.audio_clip_count(), 1);
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.track, "lead");
    assert_eq!(clip.time_range.start, BarBeat::new(1, 1, 0));
    assert_eq!(clip.time_range.end, BarBeat::new(5, 2, 1));
    assert_eq!(clip.file, result.media);
    assert_eq!(inner.undo.undo.len(), 1);
    assert_eq!(inner.undo.undo[0].label, "bounce track");
    drop(inner);
    assert!(Path::new(&result.path).is_file());
}

#[test]
fn an_in_place_bounce_refuses_when_bypassing_would_change_other_content() {
    let dir = TempDir::new("bounce-refuse-outside");
    let state = bounce_state(&dir);
    let error = expect_refusal(
        capture_bounce_snapshot(&state, "lead", "2:1", "3:1", BounceMode::InPlace),
        "phrase 'held'",
    );
    assert!(error.contains("lies outside 2:1 to 3:2:1"), "{error}");
    assert!(error.contains("bypassing"), "{error}");
    let inner = state.lock_inner();
    assert!(inner.project.audio_clips().is_empty());
    assert!(inner.undo.undo.is_empty());
    drop(inner);
    assert!(media_files(dir.path()).is_empty(), "no media was written");
}

#[test]
fn a_bounce_refuses_an_unsaved_project() {
    let state = AppState::new();
    let error = expect_refusal(
        capture_bounce_snapshot(&state, "lead", "1:1", "2:1", BounceMode::NewTrack),
        "Save the project first",
    );
    assert!(error.contains("media folder"), "{error}");
}

#[test]
fn a_bounce_refuses_a_range_that_ends_before_it_starts() {
    let dir = TempDir::new("bounce-refuse-range");
    let state = bounce_state(&dir);
    expect_refusal(
        capture_bounce_snapshot(&state, "lead", "3:1", "2:1", BounceMode::NewTrack),
        "ends at or before its start",
    );
    expect_refusal(
        capture_bounce_snapshot(&state, "lead", "2:1", "2:1", BounceMode::NewTrack),
        "ends at or before its start",
    );
    expect_refusal(
        capture_bounce_snapshot(&state, "missing", "1:1", "2:1", BounceMode::NewTrack),
        "No track named 'missing'",
    );
}

#[test]
fn a_bounce_drops_its_media_when_the_project_changes_during_the_render() {
    let dir = TempDir::new("bounce-stale");
    let state = bounce_state(&dir);
    let snapshot = capture_bounce_snapshot(&state, "lead", "2:1", "3:1", BounceMode::NewTrack)
        .expect("snapshot");
    let rendered = render_bounce(snapshot).expect("render");
    assert!(!media_files(dir.path()).is_empty(), "the media is staged");
    state.lock_inner().bump_revision();
    let error = expect_refusal(
        publish_bounce(&mut state.lock_inner(), rendered),
        "project changed during the bounce",
    );
    assert!(error.contains("was removed"), "{error}");
    assert!(
        media_files(dir.path()).is_empty(),
        "the staged media is removed: {:?}",
        media_files(dir.path())
    );
    let inner = state.lock_inner();
    assert_eq!(inner.project.tracks.len(), 1);
    assert!(inner.project.audio_clips().is_empty());
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn a_consolidate_replaces_the_selected_clips_with_one_and_keeps_the_chain() {
    let dir = TempDir::new("consolidate-happy");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 440);
    let state = state_with_project(CLIP_PROJECT, dir.path());
    {
        let mut inner = state.lock_inner();
        inner.content_epoch = 11;
    }
    let snapshot = capture_consolidate_snapshot(&state, "lead", vec![0, 1]).expect("snapshot");
    assert_eq!(snapshot.project.audio_clip_count(), 2);
    let rendered = render_consolidate(snapshot).expect("render");
    let frames = rendered.frames;
    let result = publish_consolidate(&mut state.lock_inner(), rendered).expect("publish");

    assert_eq!(result.clips, 2);
    assert_eq!(result.media, "media/consolidate-lead-1.wav");
    assert!(
        result
            .report
            .contains("Consolidated 2 audio clips on 'lead'"),
        "{}",
        result.report
    );
    assert!(
        result
            .report
            .contains("one clip replaces them over 1:1 to 3:1"),
        "{}",
        result.report
    );
    assert!(Path::new(&result.path).is_file());

    let inner = state.lock_inner();
    let clips = inner.project.audio_clips();
    assert_eq!(clips.len(), 2, "one clip replaces two, the pad clip stays");
    let replacement = clips.iter().find(|clip| clip.name == result.clip).unwrap();
    assert_eq!(replacement.track, "lead");
    assert_eq!(replacement.time_range.start, BarBeat::new(1, 1, 0));
    assert_eq!(replacement.time_range.end, BarBeat::new(3, 1, 0));
    assert_eq!(replacement.offset_seconds, 0.0);
    assert!(
        clips.iter().any(|clip| clip.track == "pad"),
        "the other track is untouched"
    );
    let track = &inner.project.tracks[0];
    assert_eq!(track.effects.len(), 1);
    assert!(
        !track.effects[0].bypass,
        "consolidate keeps the track's inserts"
    );
    assert_eq!(inner.undo.undo.len(), 1, "one undo entry");
    assert_eq!(inner.undo.undo[0].label, "consolidate clips");
    assert_eq!(frames as f64, 4.0 * f64::from(result.sample_rate));
}

#[test]
fn a_consolidate_refuses_an_empty_or_cross_track_selection() {
    let dir = TempDir::new("consolidate-refuse");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 440);
    let state = state_with_project(CLIP_PROJECT, dir.path());

    expect_refusal(
        capture_consolidate_snapshot(&state, "lead", Vec::new()),
        "at least one audio clip",
    );
    expect_refusal(
        capture_consolidate_snapshot(&state, "lead", vec![7]),
        "Audio clip 7 out of range",
    );
    let error = expect_refusal(
        capture_consolidate_snapshot(&state, "lead", vec![0, 2]),
        "Audio clip 'other' belongs to track 'pad'",
    );
    assert!(error.contains("consolidate works on one track"), "{error}");
    assert!(state.lock_inner().project.audio_clip_count() == 3);
    assert!(state.lock_inner().undo.undo.is_empty());
}

#[test]
fn a_consolidate_refuses_pattern_content_inside_the_span() {
    let dir = TempDir::new("consolidate-pattern");
    write_media(dir.path(), "one.wav", 220);
    let source = r#"@ocp "0.2.0"

@project "consolidate pattern"
  tempo: 120
  time_signature: 4/4

@pattern verse | instrument: piano/acoustic_grand
  events:
    C4 q mf

@audio_clip one [1:1 - 2:1] | track: lead
  file: "media/one.wav"

@track lead | instrument: audio/clip
  clip: verse [1:1 - 2:1]
"#;
    let state = state_with_project(source, dir.path());
    let error = expect_refusal(
        capture_consolidate_snapshot(&state, "lead", vec![0]),
        "clip 'verse' overlaps 1:1 to 2:1",
    );
    assert!(error.contains("consolidate audio clips only"), "{error}");
}

#[test]
fn a_consolidate_refuses_an_unselected_clip_inside_the_span() {
    let dir = TempDir::new("consolidate-stray");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 440);
    let source = r#"@ocp "0.2.0"

@project "consolidate stray"
  tempo: 120
  time_signature: 4/4

@audio_clip one [1:1 - 2:1] | track: lead
  file: "media/one.wav"

@audio_clip two [1:3 - 2:1] | track: lead
  file: "media/two.wav"

@audio_clip three [2:1 - 3:1] | track: lead
  file: "media/one.wav"

@track lead | instrument: audio/clip
"#;
    let state = state_with_project(source, dir.path());
    let error = expect_refusal(
        capture_consolidate_snapshot(&state, "lead", vec![0, 2]),
        "audio clip 'two' overlaps 1:1 to 3:1",
    );
    assert!(error.contains("not in the selection"), "{error}");
}

#[test]
fn a_consolidate_drops_its_media_when_the_project_changes_during_the_render() {
    let dir = TempDir::new("consolidate-stale");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 440);
    let state = state_with_project(CLIP_PROJECT, dir.path());
    let snapshot = capture_consolidate_snapshot(&state, "lead", vec![0, 1]).expect("snapshot");
    let rendered = render_consolidate(snapshot).expect("render");
    state.lock_inner().bump_revision();
    expect_refusal(
        publish_consolidate(&mut state.lock_inner(), rendered),
        "project changed during the consolidate",
    );
    assert_eq!(
        media_files(dir.path()),
        vec!["one.wav".to_string(), "two.wav".to_string()],
        "the staged media is removed and the source media stays"
    );
    let inner = state.lock_inner();
    assert_eq!(inner.project.audio_clip_count(), 3);
    assert!(inner.undo.undo.is_empty());
}

/// RMS of the interleaved stereo samples between two seconds.
fn rms_between(samples: &[f32], rate: u32, from_seconds: f64, to_seconds: f64) -> f64 {
    let start = (from_seconds * f64::from(rate)) as usize * 2;
    let end = ((to_seconds * f64::from(rate)) as usize * 2).min(samples.len());
    let window = samples.get(start..end).unwrap_or(&[]);
    if window.is_empty() {
        return 0.0;
    }
    let energy: f64 = window
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    (energy / window.len() as f64).sqrt()
}

/// Peak of the samples from one second on.
fn peak_after(samples: &[f32], rate: u32, seconds: f64) -> f32 {
    let start = (seconds * f64::from(rate)) as usize * 2;
    samples
        .get(start..)
        .unwrap_or(&[])
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

/// A project whose last note holds well past the range the test bounces.
const CUT_PROJECT: &str = r#"@ocp "0.2.0"

@project "bounce cut"
  tempo: 60
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 0, amp.attack: 0.001s, amp.decay: 0.1s, amp.sustain: 0.8, amp.release: 0.2s, output.gain: 0.4}

@track lead | preset: tone, instrument: synth/lead_sine
  effects:
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
  @phrase held [1:1:0 - 2:1:0]
    C4 w mf
"#;

#[test]
fn a_bounce_carries_the_release_and_the_insert_delay() {
    let dir = TempDir::new("bounce-tail");
    let state = state_with_project(TAIL_PROJECT, dir.path());
    let snapshot = capture_bounce_snapshot(&state, "lead", "1:1", "1:2", BounceMode::NewTrack)
        .expect("snapshot");
    // 40 ms of insert delay plus the 200 ms release, rounded up to a sixteenth.
    assert_eq!(snapshot.tail_seconds, 0.25);
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 2, 1));
    assert!(!snapshot.cuts_note, "the note ends on the range end");

    let rendered = render_bounce(snapshot).expect("render");
    let rate = rendered.snapshot.sample_rate;
    let frames = rendered.frames;
    assert_eq!(frames, (1.25 * f64::from(rate)).round() as usize);
    let result = publish_bounce(&mut state.lock_inner(), rendered).expect("publish");

    let media = hound::WavReader::open(&result.path).unwrap();
    let samples: Vec<f32> = media.into_samples::<f32>().map(|s| s.unwrap()).collect();
    assert!(
        peak_after(&samples, rate, 1.0) > 0.01,
        "the release sounds past the range end"
    );

    let inner = state.lock_inner();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.time_range.end, BarBeat::new(1, 2, 1));
    assert_eq!(clip.file, result.media);
    assert_eq!(inner.undo.undo.len(), 1, "one undo entry");
    assert!(
        result.report.contains("runs 0.25s past 1:2"),
        "{}",
        result.report
    );
}

#[test]
fn an_in_place_bounce_keeps_the_mix_at_the_boundary() {
    let dir = TempDir::new("bounce-in-place-tail");
    let state = state_with_project(TAIL_PROJECT, dir.path());
    let (project, soundfont, rate) = {
        let inner = state.lock_inner();
        (
            inner.project.clone(),
            inner.audio.soundfont_path.clone().unwrap_or_default(),
            inner.audio.sample_rate,
        )
    };
    let before = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        &soundfont,
        rate,
        &mut std::collections::HashMap::new(),
        Some(dir.path()),
    )
    .expect("mix before the bounce");

    let snapshot = capture_bounce_snapshot(&state, "lead", "1:1", "1:2", BounceMode::InPlace)
        .expect("snapshot");
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 2, 1));
    let rendered = render_bounce(snapshot).expect("render");
    publish_bounce(&mut state.lock_inner(), rendered).expect("publish");

    let after_project = state.lock_inner().project.clone();
    assert!(
        after_project.tracks[0].phrases.is_empty(),
        "the covered phrase is replaced"
    );
    assert!(after_project.tracks[0].effects[0].bypass);
    let clip = after_project.audio_clip(0).unwrap();
    assert_eq!(clip.track, "lead");
    assert_eq!(clip.time_range.end, BarBeat::new(1, 2, 1));

    let after = oc_engine::render::render_to_buffer_with_plugins_at(
        &after_project,
        &soundfont,
        rate,
        &mut std::collections::HashMap::new(),
        Some(dir.path()),
    )
    .expect("mix after the bounce");
    let before_rms = rms_between(&before, rate, 0.9, 1.1);
    let after_rms = rms_between(&after, rate, 0.9, 1.1);
    assert!(before_rms > 0.0, "the window has signal before the bounce");
    let delta_db = 20.0 * (after_rms / before_rms).log10();
    assert!(
        delta_db.abs() < 0.1,
        "the boundary moved by {delta_db} dB ({before_rms} -> {after_rms})"
    );
}

#[test]
fn an_exact_cut_keeps_the_old_length_and_reports_the_truncation() {
    let dir = TempDir::new("bounce-cut-exactly");
    let state = state_with_project(CUT_PROJECT, dir.path());

    let snapshot = capture_bounce_snapshot_with(
        &state,
        "lead",
        "1:1",
        "1:3",
        BounceMode::NewTrack,
        BounceTail::CutExactly,
    )
    .expect("snapshot");
    assert_eq!(snapshot.tail_seconds, 0.0, "an exact cut renders no tail");
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 3, 0));
    assert!(snapshot.cuts_note, "the held note outlasts the range end");

    let rendered = render_bounce(snapshot).expect("render");
    let result = publish_bounce(&mut state.lock_inner(), rendered).expect("publish");
    assert!(
        result
            .report
            .contains("cuts the release of a sounding note"),
        "{}",
        result.report
    );
    assert_eq!(
        state
            .lock_inner()
            .project
            .audio_clip(0)
            .unwrap()
            .time_range
            .end,
        BarBeat::new(1, 3, 0)
    );

    // The default keeps the release, so the clip ends a sixteenth later.
    let kept = capture_bounce_snapshot(&state, "lead", "1:1", "1:3", BounceMode::NewTrack)
        .expect("snapshot");
    assert_eq!(kept.tail_seconds, 0.25);
    assert_eq!(kept.clip_end, BarBeat::new(1, 3, 1));
}

/// The release project plus a 2 s reverb insert; `bypass` keeps the device but
/// takes the room out of the signal.
fn reverb_tail_project(bypass: bool) -> String {
    format!(
        r#"@ocp "0.2.0"

@project "bounce reverb tail"
  tempo: 60
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {{osc.a.waveform: 0, amp.attack: 0.001s, amp.decay: 0.1s, amp.sustain: 0.8, amp.release: 0.2s, output.gain: 0.4}}

@track lead | preset: tone, instrument: synth/lead_sine
  effects:
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - limiter: ceiling=-1dB, lookahead=20ms, release=100ms
    - reverb: decay=2s, mix=100%{}
  @phrase held [1:1:0 - 1:2:0]
    C4 q mf
"#,
        if bypass { ", bypass=true" } else { "" }
    )
}

#[test]
fn a_bounce_carries_the_reverb_decay() {
    let dir = TempDir::new("bounce-reverb-tail");
    let state = state_with_project(&reverb_tail_project(false), dir.path());
    let snapshot = capture_bounce_snapshot(&state, "lead", "1:1", "1:2", BounceMode::NewTrack)
        .expect("snapshot");
    // 40 ms of insert delay, a 2 s decay and the 200 ms release, rounded up to
    // a sixteenth.
    assert_eq!(snapshot.tail_seconds, 2.25);
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 4, 1));

    let rendered = render_bounce(snapshot).expect("render");
    let rate = rendered.snapshot.sample_rate;
    assert_eq!(
        rendered.frames,
        (3.25 * f64::from(rate)).round() as usize,
        "the media carries the whole tail"
    );
    let result = publish_bounce(&mut state.lock_inner(), rendered).expect("publish");
    assert!(
        result.report.contains("runs 2.25s past 1:2"),
        "{}",
        result.report
    );

    let media = hound::WavReader::open(&result.path).unwrap();
    let samples: Vec<f32> = media.into_samples::<f32>().map(|s| s.unwrap()).collect();
    assert!(
        peak_after(&samples, rate, 1.5) > 1e-3,
        "the room rings well past the range end"
    );
}

#[test]
fn a_bypassed_reverb_adds_no_bounce_tail() {
    let dir = TempDir::new("bounce-reverb-bypassed");
    let state = state_with_project(&reverb_tail_project(true), dir.path());
    let snapshot = capture_bounce_snapshot(&state, "lead", "1:1", "1:2", BounceMode::NewTrack)
        .expect("snapshot");
    // Only the insert delay and the release remain.
    assert_eq!(snapshot.tail_seconds, 0.25);
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 2, 1));

    let rendered = render_bounce(snapshot).expect("render");
    let rate = rendered.snapshot.sample_rate;
    assert_eq!(
        rendered.frames,
        (1.25 * f64::from(rate)).round() as usize,
        "no room rings past the release"
    );
    let result = publish_bounce(&mut state.lock_inner(), rendered).expect("publish");
    let media = hound::WavReader::open(&result.path).unwrap();
    let samples: Vec<f32> = media.into_samples::<f32>().map(|s| s.unwrap()).collect();
    assert!(
        peak_after(&samples, rate, 1.24) < 1e-4,
        "the bypassed reverb keeps no tail"
    );
}

#[test]
fn an_in_place_bounce_covers_the_reverb_tail_without_doubling_it() {
    let dir = TempDir::new("bounce-reverb-in-place");
    let state = state_with_project(&reverb_tail_project(false), dir.path());
    let (project, soundfont, rate) = {
        let inner = state.lock_inner();
        (
            inner.project.clone(),
            inner.audio.soundfont_path.clone().unwrap_or_default(),
            inner.audio.sample_rate,
        )
    };
    let before = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        &soundfont,
        rate,
        &mut std::collections::HashMap::new(),
        Some(dir.path()),
    )
    .expect("mix before the bounce");

    let snapshot = capture_bounce_snapshot(&state, "lead", "1:1", "1:2", BounceMode::InPlace)
        .expect("snapshot");
    assert_eq!(
        snapshot.tail_seconds, 2.25,
        "the room is in the covered window"
    );
    assert_eq!(snapshot.clip_end, BarBeat::new(1, 4, 1));
    let rendered = render_bounce(snapshot).expect("render");
    publish_bounce(&mut state.lock_inner(), rendered).expect("publish");

    let after_project = state.lock_inner().project.clone();
    assert!(
        after_project.tracks[0].effects[2].bypass,
        "the reverb is baked, not left in the chain"
    );
    let after = oc_engine::render::render_to_buffer_with_plugins_at(
        &after_project,
        &soundfont,
        rate,
        &mut std::collections::HashMap::new(),
        Some(dir.path()),
    )
    .expect("mix after the bounce");

    let before_tail = rms_between(&before, rate, 1.4, 1.6);
    let after_tail = rms_between(&after, rate, 1.4, 1.6);
    assert!(
        before_tail > 0.0,
        "the window has room signal before the bounce"
    );
    let delta_db = 20.0 * (after_tail / before_tail).log10();
    assert!(
        delta_db.abs() < 0.1,
        "the room moved by {delta_db} dB ({before_tail} -> {after_tail})"
    );
}

#[test]
fn precise_consolidation_preserves_shifted_outer_samples_and_insert_processing() {
    for sample_rate in [44_100, 48_000] {
        let dir = TempDir::new("consolidate-precise");
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        let samples: Vec<f32> = (0..sample_rate / 2)
            .flat_map(|frame| {
                let value = if frame % 7 == 0 { 0.4 } else { -0.2 };
                [value, value * 0.5]
            })
            .collect();
        oc_engine::wav::write_stereo_f32_wav(media.join("one.wav"), &samples, sample_rate).unwrap();
        let source = CLIP_PROJECT
            .replace("[1:1 - 2:1]", "[1:2 - 1:3]")
            .replace("[2:1 - 3:1]", "[1:3 - 1:4]")
            .replace("media/two.wav", "media/one.wav");
        let state = state_with_project(&source, dir.path());
        let (before, start, end) = {
            let mut inner = state.lock_inner();
            inner.audio.sample_rate = sample_rate;
            let first = inner.project.audio_clip_mut(0).unwrap();
            first.placement.start_offset_seconds = -1.0 / f64::from(sample_rate);
            first.placement.end_offset_seconds = -1.0 / f64::from(sample_rate);
            let second = inner.project.audio_clip_mut(1).unwrap();
            second.placement.start_offset_seconds = 2.0 / f64::from(sample_rate);
            second.placement.end_offset_seconds = 2.0 / f64::from(sample_rate);
            let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
            let start = clock
                .audio_clip_range_frames(inner.project.audio_clip(0).unwrap(), sample_rate)
                .unwrap()
                .0;
            let end = clock
                .audio_clip_range_frames(inner.project.audio_clip(1).unwrap(), sample_rate)
                .unwrap()
                .1;
            let before = oc_engine::render::render_track_post_fx_frame_range(
                &inner.project,
                Path::new(""),
                sample_rate,
                &mut std::collections::HashMap::new(),
                Some(dir.path()),
                0,
                0,
                end + 32,
            )
            .unwrap();
            (before, start, end)
        };
        let snapshot = capture_consolidate_snapshot(&state, "lead", vec![0, 1]).unwrap();
        assert_eq!((snapshot.from_frame, snapshot.to_frame), (start, end));
        let rendered = render_consolidate(snapshot).unwrap();
        assert_eq!(rendered.frames as u64, end - start);
        let result = publish_consolidate(&mut state.lock_inner(), rendered).unwrap();
        let inner = state.lock_inner();
        let replacement = inner
            .project
            .audio_clips()
            .into_iter()
            .find(|clip| clip.name == result.clip)
            .unwrap();
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        assert_eq!(
            clock
                .audio_clip_range_frames(replacement, sample_rate)
                .unwrap(),
            (start, end)
        );
        let after = oc_engine::render::render_track_post_fx_frame_range(
            &inner.project,
            Path::new(""),
            sample_rate,
            &mut std::collections::HashMap::new(),
            Some(dir.path()),
            0,
            0,
            end + 32,
        )
        .unwrap();
        assert_eq!(
            before, after,
            "consolidation changes neither shifted attacks nor the insert signal at {sample_rate} Hz"
        );
        assert!(
            after[start as usize * 2].abs() > 0.01,
            "the attack before the nominal boundary is retained"
        );
        assert!(
            after[(end as usize - 1) * 2].abs() > 0.01,
            "the tail after the nominal boundary is retained"
        );
        assert_eq!(inner.undo.undo.len(), 1);
    }
}

#[test]
fn consolidation_refuses_an_unselected_clip_that_overlaps_only_through_fine_placement() {
    let dir = TempDir::new("consolidate-precise-overlap");
    let state = state_with_project(CLIP_PROJECT, dir.path());
    {
        let mut inner = state.lock_inner();
        let unselected = inner.project.audio_clip_mut(1).unwrap();
        unselected.placement.start_offset_seconds = -0.002;
        unselected.placement.end_offset_seconds = -0.002;
    }
    expect_refusal(
        capture_consolidate_snapshot(&state, "lead", vec![0]),
        "not in the selection",
    );
}

#[test]
fn in_place_bounce_refuses_to_bypass_inserts_on_a_finely_shifted_outside_tail() {
    let dir = TempDir::new("bounce-precise-outside");
    let state = state_with_project(CLIP_PROJECT, dir.path());
    {
        let mut inner = state.lock_inner();
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.placement.end_offset_seconds = 0.002;
    }
    expect_refusal(
        capture_bounce_snapshot_with(
            &state,
            "lead",
            "1:1",
            "2:1",
            BounceMode::InPlace,
            BounceTail::CutExactly,
        ),
        "lies outside",
    );
}

#[test]
fn an_in_place_bounce_without_inserts_keeps_a_partially_overlapping_audio_clip_on_refusal() {
    let dir = TempDir::new("bounce-partial-audio");
    let state = state_with_project(CLIP_PROJECT, dir.path());
    let original = {
        let mut inner = state.lock_inner();
        inner.project.tracks[0].effects.clear();
        inner
            .project
            .audio_clip_mut(0)
            .unwrap()
            .placement
            .end_offset_seconds = 0.002;
        inner.project.clone()
    };
    let error = expect_refusal(
        capture_bounce_snapshot_with(
            &state,
            "lead",
            "1:1",
            "2:1",
            BounceMode::InPlace,
            BounceTail::CutExactly,
        ),
        "audio clip 'one' crosses",
    );
    assert!(
        error.contains("Select whole blocks or bounce onto a new track"),
        "{error}"
    );
    let inner = state.lock_inner();
    assert_eq!(
        inner.project, original,
        "the entire source clip remains intact"
    );
    assert!(inner.undo.undo.is_empty());
    assert!(media_files(dir.path()).is_empty());
}

#[test]
fn an_in_place_bounce_without_inserts_keeps_a_partially_overlapping_pattern_clip_on_refusal() {
    let dir = TempDir::new("bounce-partial-pattern");
    let source = r#"@ocp "0.2.0"

@project "partial pattern bounce"
  tempo: 120
  time_signature: 4/4

@pattern verse | instrument: piano/acoustic_grand
  events:
    C4 w mf

@track lead | instrument: piano/acoustic_grand
  clip: verse [1:1 - 3:1]
"#;
    let state = state_with_project(source, dir.path());
    let original = state.lock_inner().project.clone();
    let error = expect_refusal(
        capture_bounce_snapshot_with(
            &state,
            "lead",
            "2:1",
            "3:1",
            BounceMode::InPlace,
            BounceTail::CutExactly,
        ),
        "clip 'verse' crosses",
    );
    assert!(
        error.contains("Select whole blocks or bounce onto a new track"),
        "{error}"
    );
    let inner = state.lock_inner();
    assert_eq!(
        inner.project, original,
        "the pattern and its outside placement remain intact"
    );
    assert!(inner.undo.undo.is_empty());
    assert!(media_files(dir.path()).is_empty());
}

#[test]
fn an_in_place_bounce_preserves_performed_notes_and_controllers_on_refusal() {
    use oc_dsl::performance::{ChannelCommand, PerformanceEvent, PerformanceTimeline};
    let dir = TempDir::new("bounce-performance-refusal");
    let state = bounce_state(&dir);
    let original = {
        let mut inner = state.lock_inner();
        let track = &mut inner.project.tracks[0];
        track.effects.clear();
        track.phrases.clear();
        track.performance = Some(PerformanceTimeline {
            events: vec![
                PerformanceEvent {
                    quarter: 0.0,
                    channel: 0,
                    message: ChannelCommand::ControlChange {
                        controller: 64,
                        value: 127,
                    },
                },
                PerformanceEvent {
                    quarter: 0.0,
                    channel: 0,
                    message: ChannelCommand::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                },
                PerformanceEvent {
                    quarter: 4.0,
                    channel: 0,
                    message: ChannelCommand::NoteOff {
                        note: 60,
                        velocity: 0,
                    },
                },
                PerformanceEvent {
                    quarter: 5.0,
                    channel: 0,
                    message: ChannelCommand::ControlChange {
                        controller: 64,
                        value: 0,
                    },
                },
            ],
        });
        inner.project.clone()
    };
    let error = expect_refusal(
        capture_bounce_snapshot_with(
            &state,
            "lead",
            "1:1",
            "3:1",
            BounceMode::InPlace,
            BounceTail::CutExactly,
        ),
        "performed note and controller events",
    );
    assert!(error.contains("Bounce onto a new track"), "{error}");
    let inner = state.lock_inner();
    assert_eq!(inner.project, original);
    assert!(inner.undo.undo.is_empty());
    assert!(media_files(dir.path()).is_empty());
}

#[test]
fn native_jobs_bounce_worker_snapshot_keeps_receipt_and_shutdown_cancellation() {
    let dir = TempDir::new("native-bounce-worker");
    let state = bounce_state(&dir);
    let original = state.lock_inner().project.clone();
    let snapshot =
        capture_bounce_snapshot(&state, "lead", "1:1", "5:1", BounceMode::NewTrack).unwrap();
    assert_eq!(
        state.native_jobs.active(),
        1,
        "capture must transfer its receipt to the returned snapshot"
    );
    state.native_jobs.close_and_cancel();
    assert!(snapshot.cancel.is_cancelled());
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        wait.recv().unwrap();
        assert!(snapshot.cancel.is_cancelled());
        assert!(render_bounce(snapshot).err().unwrap().contains("cancelled"));
    });
    assert_eq!(
        state.native_jobs.active(),
        1,
        "worker owns receipt after capture returns"
    );
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(state.native_jobs.active(), 0);
    assert!(media_files(dir.path()).is_empty());
    assert_eq!(state.lock_inner().project, original);
}

#[test]
fn native_jobs_consolidate_worker_snapshot_keeps_receipt_and_shutdown_cancellation() {
    let dir = TempDir::new("native-consolidate-worker");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 330);
    let state = state_with_project(CLIP_PROJECT, dir.path());
    let original = state.lock_inner().project.clone();
    let before = media_files(dir.path());
    let snapshot = capture_consolidate_snapshot(&state, "lead", vec![0, 1]).unwrap();
    assert_eq!(
        state.native_jobs.active(),
        1,
        "capture must transfer its receipt to the returned snapshot"
    );
    state.native_jobs.close_and_cancel();
    assert!(snapshot.cancel.is_cancelled());
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        wait.recv().unwrap();
        assert!(snapshot.cancel.is_cancelled());
        assert!(
            render_consolidate(snapshot)
                .err()
                .unwrap()
                .contains("cancelled")
        );
    });
    assert_eq!(state.native_jobs.active(), 1);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(state.native_jobs.active(), 0);
    assert_eq!(media_files(dir.path()), before);
    assert_eq!(state.lock_inner().project, original);
}

#[test]
fn native_jobs_bounce_staged_result_keeps_receipt_until_cancelled_publication_drops_media() {
    let dir = TempDir::new("native-bounce-staged");
    let state = bounce_state(&dir);
    let original = state.lock_inner().project.clone();
    let snapshot =
        capture_bounce_snapshot(&state, "lead", "1:1", "5:1", BounceMode::NewTrack).unwrap();
    let rendered = render_bounce(snapshot).unwrap();
    assert_eq!(
        state.native_jobs.active(),
        1,
        "rendered snapshot still owns native receipt"
    );
    assert!(!media_files(dir.path()).is_empty());
    state.native_jobs.close_and_cancel();
    assert!(
        publish_bounce(&mut state.lock_inner(), rendered)
            .err()
            .unwrap()
            .contains("cancelled")
    );
    assert_eq!(state.native_jobs.active(), 0);
    assert!(media_files(dir.path()).is_empty());
    assert_eq!(state.lock_inner().project, original);
}

#[test]
fn native_jobs_consolidate_staged_result_keeps_receipt_until_cancelled_publication_drops_media() {
    let dir = TempDir::new("native-consolidate-staged");
    write_media(dir.path(), "one.wav", 220);
    write_media(dir.path(), "two.wav", 330);
    let state = state_with_project(CLIP_PROJECT, dir.path());
    let original = state.lock_inner().project.clone();
    let before = media_files(dir.path());
    let snapshot = capture_consolidate_snapshot(&state, "lead", vec![0, 1]).unwrap();
    let rendered = render_consolidate(snapshot).unwrap();
    assert_eq!(state.native_jobs.active(), 1);
    state.native_jobs.close_and_cancel();
    assert!(
        publish_consolidate(&mut state.lock_inner(), rendered)
            .err()
            .unwrap()
            .contains("cancelled")
    );
    assert_eq!(state.native_jobs.active(), 0);
    assert_eq!(media_files(dir.path()), before);
    assert_eq!(state.lock_inner().project, original);
}
