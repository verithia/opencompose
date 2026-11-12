//! MIDI recording: arming, capture, the committed pattern edit and its undo
//! entry, the punch range and a take whose input died.

use oc_dsl::ast;

use crate::commands::tests::support::{default_project, make_track};
use crate::midi_bridge::{FakeBackend, LiveEvent};
use crate::midi_record::CapturedKind;
use crate::state::AppState;

/// One quarter at 120 bpm is half a second, so `quarter * rate / 2` frames.
fn frames(rate: u32, quarter: f64) -> u64 {
    (quarter * f64::from(rate) * 0.5).round() as u64
}

/// A note with a plain quarter duration, for the pattern the take edits.
fn existing_note(start: f64, key: u8) -> ast::NoteEvent {
    ast::NoteEvent {
        start_beat: start,
        explicit_position: true,
        pitch: crate::midi_record::pitch_for_key(key).unwrap(),
        duration: ast::Duration {
            base: ast::BaseDuration::Quarter,
            dots: 0,
            triplet: false,
            tuplet: None,
            ties: vec![],
        },
        dynamics: None,
        velocity: Some(64),
        articulation: None,
        vibrato: None,
        expression: None,
        pitch_bend: None,
        muted: false,
        grace: false,
        text: None,
        modifiers: vec![],
    }
}

/// A project whose instrument track plays one pattern, so a take has a target.
fn state() -> AppState {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        let mut track = make_track("Lead");
        track.push_pattern_clip(ast::Clip {
            pattern: "hook".into(),
            time_range: ast::TimeRange {
                start: ast::BarBeat::new(1, 1, 0),
                end: ast::BarBeat::new(5, 1, 0),
            },
            offset_quarters: 0.0,
            loop_quarters: 0.0,
            id: None,
        });
        project.tracks.push(track);
        project.patterns.push(ast::PatternDef {
            name: "hook".into(),
            instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
            preset: None,
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: vec![],
            events: vec![existing_note(0.5, 67)],
            pedals: vec![],
            hairpins: Vec::new(),
            hits: vec![],
            color: None,
        });
        inner.project = project;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        // A file-less MidiLive, so a test never rewrites the operator's
        // ~/.opencompose/midi.json.
        inner.midi_live = crate::midi_live::MidiLive::new(None);
        inner
            .midi_live
            .configure_with_mpe(Some("Keys".into()), false, None, |_| {
                panic!("monitoring is off, so no device opens")
            });
    }
    state
}

/// Arm nothing; `state()` leaves the track unarmed.
fn arm(state: &AppState) {
    state.lock_inner().audio.recorder.armed = Some("Lead".into());
}

/// Start the take through a fake MIDI input.
fn start(state: &AppState) {
    let mut inner = state.lock_inner();
    let rate = inner.audio.sample_rate;
    crate::midi_record::start_inner(&mut inner, rate, |selector, sink| {
        crate::midi_live::MidiRuntime::start_with(selector, FakeBackend::new(&["Keys"], &[]), sink)
    })
    .expect("the take starts");
}

/// Push scripted events into the live capture.
fn capture(state: &AppState, events: &[(f64, CapturedKind)]) {
    let inner = state.lock_inner();
    let rate = inner.audio.sample_rate;
    for (quarter, kind) in events {
        inner
            .audio
            .midi_recorder
            .capture
            .record_at(frames(rate, *quarter), *kind);
    }
}

fn stop(state: &AppState) {
    let mut inner = state.lock_inner();
    crate::midi_record::stop_inner(&mut inner).expect("the take stops");
}

/// One held quarter on middle C starting at beat 0.
fn one_note() -> Vec<(f64, CapturedKind)> {
    vec![
        (
            0.0,
            CapturedKind::NoteOn {
                key: 60,
                velocity: 100,
            },
        ),
        (1.0, CapturedKind::NoteOff { key: 60 }),
    ]
}

#[test]
fn a_take_replaces_the_recorded_span_in_one_undo_entry() {
    let state = state();
    arm(&state);
    start(&state);
    capture(&state, &one_note());
    stop(&state);

    let inner = state.lock_inner();
    assert_eq!(inner.undo.undo.len(), 1, "a take is one undo entry");
    assert_eq!(inner.undo.undo[0].label, "record midi take");
    let events = &inner.project.patterns[0].events;
    assert_eq!(events.len(), 1, "the note inside the span is replaced");
    assert_eq!(events[0].start_beat, 0.0);
    assert_eq!(events[0].pitch.to_midi(), 60);
    assert_eq!(events[0].velocity, Some(100));
    assert_eq!(events[0].duration.to_beats(), 1.0);
    let report = inner.audio.midi_recorder.report.clone().expect("a report");
    assert_eq!(report, "Recorded 1 note into hook");
    assert_eq!(
        inner.audio.recorder.report, None,
        "only the take that spoke is reported"
    );
}

#[test]
fn stopping_before_roll_drops_midi_even_after_the_nominal_count_in_duration() {
    let state = state();
    arm(&state);
    state.lock_inner().audio.click.count_in = oc_audio::click::CountIn::OneBar;
    start(&state);
    let notes = one_note()
        .into_iter()
        .map(|(beat, kind)| (beat + 5.0, kind))
        .collect::<Vec<_>>();
    capture(&state, &notes);
    stop(&state);
    let inner = state.lock_inner();
    assert_eq!(inner.project.patterns[0].events.len(), 1);
    assert_eq!(inner.project.patterns[0].events[0].pitch.to_midi(), 67);
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn the_arm_target_and_midi_take_follow_the_parked_cursor() {
    let state = state();
    arm(&state);
    {
        let mut inner = state.lock_inner();
        let mut later = inner.project.patterns[0].clone();
        later.name = "later".into();
        later.events.clear();
        inner.project.patterns.push(later);
        inner.project.tracks[0].push_pattern_clip(ast::Clip {
            pattern: "later".into(),
            time_range: ast::TimeRange {
                start: ast::BarBeat::new(5, 1, 0),
                end: ast::BarBeat::new(6, 1, 0),
            },
            offset_quarters: 0.0,
            loop_quarters: 0.0,
            id: None,
        });
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        assert_eq!(
            crate::midi_record::view(&inner).target.as_deref(),
            Some("hook")
        );
        inner.audio.seek(16.0, &clock).unwrap();
        assert_eq!(
            crate::midi_record::view(&inner).target.as_deref(),
            Some("later")
        );
    }
    start(&state);
    let notes = one_note()
        .into_iter()
        .map(|(beat, kind)| (beat + 16.0, kind))
        .collect::<Vec<_>>();
    capture(&state, &notes);
    stop(&state);

    let inner = state.lock_inner();
    let patterns = &inner.project.patterns;
    assert_eq!(patterns[0].events[0].pitch.to_midi(), 67);
    assert_eq!(patterns[1].events.len(), 1);
    assert_eq!(patterns[1].events[0].start_beat, 0.0);
    assert_eq!(patterns[1].events[0].pitch.to_midi(), 60);
}

/// A take that lost events to a contended capture lock says how many, and why,
/// in the line the transport shows. A clean take claims no loss.
#[test]
fn a_take_that_lost_events_reports_the_count_and_the_reason() {
    let state = state();
    arm(&state);
    start(&state);
    capture(&state, &one_note());
    {
        let inner = state.lock_inner();
        inner
            .audio
            .midi_recorder
            .capture
            .record_while_busy(LiveEvent::NoteOn {
                channel: 0,
                note: 64,
                velocity: 90,
            });
    }
    stop(&state);

    let inner = state.lock_inner();
    assert_eq!(inner.project.patterns[0].events.len(), 1, "the take landed");
    assert_eq!(
        inner.audio.midi_recorder.report.as_deref(),
        Some("Recorded 1 note into hook (1 event was dropped because the capture buffer was busy)")
    );
}

#[test]
fn a_take_that_lost_every_event_names_the_loss_instead_of_no_notes() {
    let state = state();
    let before = state.lock_inner().project.clone();
    arm(&state);
    start(&state);
    {
        let inner = state.lock_inner();
        inner
            .audio
            .midi_recorder
            .capture
            .record_while_busy(LiveEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            });
    }
    stop(&state);

    let inner = state.lock_inner();
    assert_eq!(inner.project, before, "nothing was written");
    assert_eq!(
        inner.audio.midi_recorder.report.as_deref(),
        Some("Nothing was recorded: 1 event was dropped because the capture buffer was busy.")
    );
}

#[test]
fn undo_restores_the_span_the_take_replaced() {
    let state = state();
    let before = state.lock_inner().project.clone();
    arm(&state);
    start(&state);
    capture(&state, &one_note());
    stop(&state);
    {
        let mut guard = state.lock_inner();
        let inner = &mut *guard;
        inner.undo.undo(&mut inner.project).expect("undo applies");
    }
    assert_eq!(state.lock_inner().project, before);
}

#[test]
fn overdub_keeps_the_notes_already_in_the_pattern() {
    let state = state();
    arm(&state);
    state.lock_inner().audio.midi_recorder.settings.overdub = true;
    start(&state);
    capture(&state, &one_note());
    stop(&state);

    let inner = state.lock_inner();
    assert_eq!(inner.undo.undo.len(), 1);
    let events = &inner.project.patterns[0].events;
    assert_eq!(events.len(), 2, "the earlier note stays");
    assert_eq!(events[0].start_beat, 0.0);
    assert_eq!(events[1].start_beat, 0.5);
}

#[test]
fn a_take_without_notes_reports_and_adds_no_undo_entry() {
    let state = state();
    let before = state.lock_inner().project.clone();
    arm(&state);
    start(&state);
    stop(&state);
    let inner = state.lock_inner();
    assert_eq!(
        inner.audio.midi_recorder.report.as_deref(),
        Some("Nothing was recorded: no notes arrived.")
    );
    assert!(inner.undo.undo.is_empty(), "an empty take is not an edit");
    assert_eq!(inner.project, before);
}

#[test]
fn a_take_without_a_midi_input_is_refused() {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.tracks.push(make_track("Lead"));
        inner.project = project;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.audio.recorder.armed = Some("Lead".into());
        // No input is selected: `MidiLive::new(None)` starts with no preference.
    }
    let mut inner = state.lock_inner();
    let rate = inner.audio.sample_rate;
    let error = crate::midi_record::start_inner(&mut inner, rate, |_, _| {
        panic!("no input is selected, so no thread opens")
    })
    .unwrap_err();
    assert_eq!(error, "No MIDI input is selected.");
    assert_eq!(
        inner.audio.midi_recorder.report.as_deref(),
        Some(
            "MIDI recording could not start: no MIDI input is selected. The project is unchanged."
        )
    );
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn the_punch_range_discards_notes_that_start_outside_it() {
    let state = state();
    arm(&state);
    {
        let mut inner = state.lock_inner();
        inner
            .audio
            .set_punch_region(Some(0.25), Some(0.75), true)
            .expect("the range is valid");
    }
    start(&state);
    capture(
        &state,
        &[
            (
                0.0,
                CapturedKind::NoteOn {
                    key: 60,
                    velocity: 100,
                },
            ),
            (0.1, CapturedKind::NoteOff { key: 60 }),
            (
                0.5,
                CapturedKind::NoteOn {
                    key: 64,
                    velocity: 90,
                },
            ),
            (0.9, CapturedKind::NoteOff { key: 64 }),
        ],
    );
    stop(&state);
    let inner = state.lock_inner();
    let events = &inner.project.patterns[0].events;
    assert_eq!(events.len(), 1, "only the note inside the range is kept");
    assert_eq!(events[0].pitch.to_midi(), 64);
    let report = inner.audio.midi_recorder.report.clone().expect("a report");
    assert!(
        report.contains("1 discarded outside the punch range"),
        "{report}"
    );
}

#[test]
fn an_input_that_dies_keeps_the_notes_it_had() {
    let state = state();
    arm(&state);
    start(&state);
    capture(
        &state,
        &[
            (
                0.0,
                CapturedKind::NoteOn {
                    key: 60,
                    velocity: 100,
                },
            ),
            (0.5, CapturedKind::NoteOff { key: 60 }),
            (
                1.0,
                CapturedKind::NoteOn {
                    key: 64,
                    velocity: 90,
                },
            ),
            (1.5, CapturedKind::NoteOff { key: 64 }),
        ],
    );
    {
        let mut inner = state.lock_inner();
        // The thread is gone: the poll finalises the take instead of losing it.
        inner.midi_live.stop();
        crate::midi_record::poll_inner(&mut inner).expect("poll");
    }
    let inner = state.lock_inner();
    assert!(!inner.audio.midi_recorder.recording(), "the take is over");
    assert_eq!(inner.undo.undo.len(), 1, "the notes that arrived are kept");
    assert_eq!(inner.project.patterns[0].events.len(), 2);
    assert_eq!(
        inner.audio.midi_recorder.report.as_deref(),
        Some("The MIDI input stopped: 2 notes were kept up to the last event.")
    );
}

#[test]
fn an_empty_take_reports_device_input_overflow() {
    for poll in [true, false] {
        let state = state();
        arm(&state);
        let mut inner = state.lock_inner();
        let rate = inner.audio.sample_rate;
        crate::midi_record::start_inner(&mut inner, rate, |selector, sink| {
            let mut backend = FakeBackend::new(&["Keys"], &[]);
            backend.queue_input_failure(
                "Keys",
                oc_engine::midi_io::MidiError::InputOverflow {
                    name: "Keys".into(),
                },
            );
            crate::midi_live::MidiRuntime::start_with(selector, backend, sink)
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while inner.midi_live.active() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if poll {
            crate::midi_record::poll_inner(&mut inner).unwrap();
        } else {
            crate::midi_record::stop_inner(&mut inner).unwrap();
        }
        assert!(!inner.audio.midi_recorder.recording());
        assert!(inner.undo.undo.is_empty());
        assert_eq!(
            inner.audio.midi_recorder.report.as_deref(),
            Some("MIDI input buffer overflow on 'Keys'. Kept 0 notes up to the last event.")
        );
    }
}

#[test]
fn a_take_over_no_clip_creates_a_pattern_and_places_its_clip() {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        let mut project = default_project();
        project.tracks.push(make_track("Lead"));
        inner.project = project;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.audio.recorder.armed = Some("Lead".into());
        inner.midi_live = crate::midi_live::MidiLive::new(None);
        inner
            .midi_live
            .configure_with_mpe(Some("Keys".into()), false, None, |_| unreachable!());
    }
    start(&state);
    capture(&state, &one_note());
    stop(&state);

    let inner = state.lock_inner();
    let pattern = inner
        .project
        .patterns
        .iter()
        .find(|pattern| pattern.name == "Lead take")
        .expect("the take creates its pattern");
    assert_eq!(pattern.events.len(), 1);
    let clip = inner.project.tracks[0]
        .pattern_clips()
        .last()
        .expect("the pattern is placed on the track");
    assert_eq!(clip.pattern, "Lead take");
    assert_eq!(clip.time_range.start, ast::BarBeat::new(1, 1, 0));
    assert!(
        clip.time_range.end != clip.time_range.start,
        "the clip covers the take"
    );
    assert_eq!(inner.undo.undo.len(), 1);
}

#[test]
fn the_quantise_setting_reaches_the_written_notes() {
    let state = state();
    arm(&state);
    state.lock_inner().audio.midi_recorder.settings.quantise =
        crate::midi_record::Quantise::Sixteenth;
    start(&state);
    capture(
        &state,
        &[
            (
                0.02,
                CapturedKind::NoteOn {
                    key: 60,
                    velocity: 100,
                },
            ),
            (0.30, CapturedKind::NoteOff { key: 60 }),
        ],
    );
    stop(&state);
    let inner = state.lock_inner();
    let event = &inner.project.patterns[0].events[0];
    assert_eq!(event.start_beat, 0.0, "a 1/16 grid snaps the start");
    assert_eq!(event.duration.to_beats(), 0.25, "and the end");
}
