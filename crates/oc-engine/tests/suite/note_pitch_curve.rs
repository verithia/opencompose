use oc_engine::interpret::{InterpretSettings, MidiMessage, try_interpret};
use oc_synth::synth::NativeVoiceEvent;

fn project() -> oc_dsl::ast::Project {
    oc_dsl::parse(
        r#"@project "Pitch curve"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track keys | preset: tone, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
    @0 C4 q pitch_curve="0:0,1:100"
    @0 G4 q
"#,
    )
    .unwrap()
}

#[test]
fn curve_is_note_local_and_roundtrips() {
    let p = project();
    assert_eq!(p, oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap());
    let events = try_interpret(
        &p,
        &InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    let curves: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { token, semitones }) => {
                Some((e.time_samples, *token, *semitones))
            }
            _ => None,
        })
        .collect();
    assert_eq!(curves.len(), 101);
    assert!(curves.iter().all(|(_, token, _)| *token == 1));
    assert_eq!(curves[50], (12000, 1, 0.5));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, MidiMessage::PitchBend { .. }))
    );
}

#[test]
fn compact_steps_emit_the_same_voice_updates_across_a_tempo_change() {
    let source = oc_dsl::serialize(&project())
        .replace("  tempo: 120", "  tempo: 120\n  tempo_map:\n    1:2 = 80");
    let points = "0:100,0.5:200,1:300,1.5:400,2:500";
    let explicit = source.replace(
        "C4 q pitch_curve=\"0:0,1:100\"",
        &format!("C4 h pitch_curve=\"{points}\" pitch_curve_mode=step"),
    );
    let compact = explicit.replace(points, "steps(2,5,0,500)");
    assert_ne!(explicit, compact);
    let updates = |text: &str| {
        let parsed = oc_dsl::parse(text).unwrap();
        let roundtrip = oc_dsl::parse(&oc_dsl::serialize(&parsed)).unwrap();
        assert_eq!(parsed, roundtrip);
        let events = try_interpret(
            &roundtrip,
            &InterpretSettings {
                sample_rate: 48_000,
                ..Default::default()
            },
        )
        .unwrap();
        events
            .iter()
            .filter_map(|event| match event.event {
                MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { token, semitones }) => {
                    Some((event.time_samples, token, semitones.to_bits()))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let expected = updates(&explicit);
    assert_eq!(
        expected
            .iter()
            .map(|(frame, _, _)| *frame)
            .collect::<Vec<_>>(),
        [0, 12_000, 24_000, 42_000, 60_000],
    );
    assert_eq!(updates(&compact), expected);
}

#[test]
fn unsupported_host_rejects() {
    let mut p = project();
    p.tracks[0].preset = None;
    assert!(try_interpret(&p, &InterpretSettings::default()).is_err());
}

#[test]
fn a_plain_note_at_a_curve_pitch_gets_its_own_voice() {
    let mut p = project();
    p.tracks[0].phrases[0].events[1].pitch = p.tracks[0].phrases[0].events[0].pitch;
    let events = try_interpret(&p, &InterpretSettings::default()).unwrap();
    let tokens: Vec<u64> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NativeVoice(NativeVoiceEvent::On {
                token, note: 60, ..
            }) => Some(token),
            _ => None,
        })
        .collect();
    assert_eq!(tokens.len(), 2, "both A4 notes own a voice id");
    assert_ne!(tokens[0], tokens[1]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.event, MidiMessage::NoteOn { note: 60, .. }))
    );
}

#[test]
fn two_curves_at_one_pitch_keep_separate_voices() {
    let p = oc_dsl::parse(
        r#"@project "Pitch curve"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track keys | preset: tone, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
    @0 A4 q pitch_curve="0:0,1:100"
    @0 A4 q pitch_curve="0:0,1:-100"
"#,
    )
    .unwrap();
    let events = try_interpret(&p, &InterpretSettings::default()).unwrap();
    let tokens: Vec<u64> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NativeVoice(NativeVoiceEvent::On {
                token, note: 69, ..
            }) => Some(token),
            _ => None,
        })
        .collect();
    assert_eq!(tokens.len(), 2, "each curve owns a voice id");
    assert_ne!(tokens[0], tokens[1]);
    for token in &tokens {
        assert!(
            events.iter().any(|event| matches!(
                event.event,
                MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { token: t, .. }) if t == *token
            )),
            "curve {token} has no pitch events"
        );
    }
    let refs = events.iter().collect::<Vec<_>>();
    let audio = oc_synth::synth::try_render_track_with_patch_and_clock(
        &refs,
        48_000,
        48_000,
        oc_synth::synth::SynthPatch::default(),
        120.0,
        None,
    )
    .unwrap();
    assert!(audio.iter().all(|value| value.is_finite()));
    assert!(audio.iter().any(|value| value.abs() > 0.0001));
}

#[test]
fn an_unidentified_plain_note_at_a_curve_pitch_still_rejects() {
    // The legato keyswitch is a plain note at the curve's own pitch; it has no
    // voice id, so pitch alone cannot say whether it addresses the curve.
    let p = oc_dsl::parse(
        r#"@project "Pitch curve"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: { articulation.legato.keyswitch: A4 }
@track keys | preset: tone, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
    @0 A4 q articulation=legato pitch_curve="0:0,1:100"
    @1 E5 q
"#,
    )
    .unwrap();
    let error = try_interpret(&p, &InterpretSettings::default()).unwrap_err();
    assert!(error.contains("no voice id"), "{error}");
}

#[test]
fn wide_curve_retains_semitones_without_channel_bend() {
    let p = project();
    let source = oc_dsl::serialize(&p).replace("0:0,1:100", "0:-1200,1:1400");
    let p = oc_dsl::parse(&source).unwrap();
    let events = try_interpret(
        &p,
        &InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    let midpoint = events
        .iter()
        .find(|e| {
            e.time_samples == 12000
                && matches!(
                    &e.event,
                    MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { .. })
                )
        })
        .unwrap();
    assert!(
        matches!(&midpoint.event, MidiMessage::NativeVoice(NativeVoiceEvent::Pitch {semitones,..}) if (*semitones - 1.0).abs() < 1e-9)
    );
}

#[test]
fn hermite_curve_roundtrip_and_midpoint() {
    let source = oc_dsl::serialize(&project()).replace(
        "pitch_curve=\"0:0,1:100\"",
        "pitch_curve=\"0:0,1:100\" pitch_curve_tension=1",
    );
    let p = oc_dsl::parse(&source).unwrap();
    assert_eq!(p, oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap());
    let events = try_interpret(
        &p,
        &InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(events.iter().any(|e| e.time_samples==12000 && matches!(&e.event,
        MidiMessage::NativeVoice(NativeVoiceEvent::Pitch {semitones,..}) if (*semitones-0.625).abs()<1e-9)));
}

#[test]
fn tail_horizon_preserves_note_off_and_voice_identity() {
    let source = oc_dsl::serialize(&project()).replace(
        "pitch_curve=\"0:0,1:100\"",
        "pitch_curve=\"0:0,2:100\" pitch_curve_horizon=2",
    );
    let p = oc_dsl::parse(&source).unwrap();
    assert_eq!(p, oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap());
    let events = try_interpret(
        &p,
        &InterpretSettings {
            sample_rate: 48000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(events.iter().any(|e| e.time_samples == 24000
        && matches!(
            e.event,
            MidiMessage::NativeVoice(NativeVoiceEvent::Off { token: 1 })
        )));
    assert!(events.iter().any(|e| e.time_samples == 48000
        && matches!(e.event,
        MidiMessage::NativeVoice(NativeVoiceEvent::Pitch {token:1,semitones}) if semitones==1.0)));
    let refs = events.iter().collect::<Vec<_>>();
    let audio = oc_synth::synth::try_render_track_with_patch_and_clock(
        &refs,
        72000,
        48000,
        oc_synth::synth::SynthPatch::default(),
        120.0,
        None,
    )
    .unwrap();
    assert!(audio.iter().all(|v| v.is_finite()));
    assert!(audio.iter().any(|v| v.abs() > 0.0001));
    for value in ["0.5", "257", "-1"] {
        let bad = source.replace(
            "pitch_curve_horizon=2",
            &format!("pitch_curve_horizon={value}"),
        );
        assert!(
            try_interpret(&oc_dsl::parse(&bad).unwrap(), &InterpretSettings::default()).is_err()
        );
    }
}

#[test]
fn curve_tokens_follow_playback_not_source_order() {
    let mut p = project();
    let mut second = p.tracks[0].phrases[0].clone();
    second.events = vec![second.events[0].clone()];
    // Place the later phrase first in the document to exercise token rebinding.
    let source = oc_dsl::serialize(&p).replace("[1:1 - 2:1]", "[2:1 - 3:1]");
    p = oc_dsl::parse(&source).unwrap();
    p.tracks[0].phrases.push(second);
    let events = try_interpret(&p, &InterpretSettings::default()).unwrap();
    let tokens: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NativeVoice(NativeVoiceEvent::On { token, .. }) => Some(token),
            _ => None,
        })
        .collect();
    assert_eq!(tokens, vec![1, 2]);
    let refs = events.iter().collect::<Vec<_>>();
    oc_synth::synth::try_render_track_with_patch_and_clock(
        &refs,
        192000,
        48000,
        oc_synth::synth::SynthPatch::default(),
        120.0,
        None,
    )
    .unwrap();
}

#[test]
#[cfg(feature = "plugin-host")]
fn native_tail_project_exports_and_replays_without_soundfont() {
    let source = oc_dsl::serialize(&project()).replace(
        "pitch_curve=\"0:0,1:100\"",
        "pitch_curve=\"0:0,2:100\" pitch_curve_horizon=2",
    );
    let p = oc_dsl::parse(&source).unwrap();
    let missing = std::path::Path::new("/nonexistent/must-not-use-global.sf2");
    let mut cache = oc_engine::render_cache::RenderCache::new();
    let mut plugins = std::collections::HashMap::new();
    let first = cache.render_full(&p, missing, 48000, &mut plugins).unwrap();
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap();
    let second = cache
        .render_full(&reopened, missing, 48000, &mut plugins)
        .unwrap();
    assert_eq!(first, second);
    assert!(first.len() >= 96000);
    assert!(first.iter().all(|v| v.is_finite()));
    assert!(first.iter().any(|v| v.abs() > 0.0001));
}

#[test]
fn wide_curve_preserves_original_key_and_gate_through_its_explicit_horizon() {
    let p = oc_dsl::parse(
        r#"@project "Wide pitch"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track keys | preset: tone, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
    @0 C5 q pitch_curve="0:0,2:-3700" pitch_curve_horizon=2
"#,
    )
    .unwrap();
    assert_eq!(p, oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap());
    let events = try_interpret(
        &p,
        &InterpretSettings {
            sample_rate: 48_000,
            ..Default::default()
        },
    )
    .unwrap();
    let on: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NativeVoice(NativeVoiceEvent::On { token, note, .. }) => {
                Some((token, note))
            }
            _ => None,
        })
        .collect();
    assert_eq!(on.len(), 1);
    assert_eq!(on[0].1, 72);
    let token = on[0].0;
    assert!(events.iter().any(|e| e.time_samples == 24_000 && matches!(e.event, MidiMessage::NativeVoice(NativeVoiceEvent::Off { token: t }) if t == token)));
    assert!(events.iter().any(|e| e.time_samples == 48_000 && matches!(e.event, MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { token: t, semitones: -37.0 }) if t == token)));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, MidiMessage::NoteOn { .. }))
    );
    let invalid = oc_dsl::parse(&oc_dsl::serialize(&p).replace("-3700", "-9601")).unwrap();
    assert!(try_interpret(&invalid, &InterpretSettings::default()).is_err());
    let overshoot = oc_dsl::parse(&oc_dsl::serialize(&p).replace(
        "pitch_curve=\"0:0,2:-3700\"",
        "pitch_curve=\"0:-9600,1:9600,2:9600\" pitch_curve_tension=1",
    ))
    .unwrap();
    assert!(try_interpret(&overshoot, &InterpretSettings::default()).is_err());
}

fn step_project(curve: &str, mode: &str) -> oc_dsl::ast::Project {
    oc_dsl::parse(&format!(
        r#"@project "Stepped pitch"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track keys | preset: tone, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
    @0 A4 q pitch_curve="{curve}" pitch_curve_mode="{mode}" pitch_curve_horizon=2
"#
    ))
    .unwrap()
}

#[test]
fn step_points_use_exact_authored_frames_and_same_frame_order() {
    let p = step_project("0:0,0.000001:100,0.000002:200,1:300,2:400", "step");
    assert_eq!(p, oc_dsl::parse(&oc_dsl::serialize(&p)).unwrap());
    let settings = InterpretSettings {
        sample_rate: 48_000,
        ..Default::default()
    };
    let events = try_interpret(&p, &settings).unwrap();
    let native: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::NativeVoice(ref v) => Some((e.time_samples, v.clone())),
            _ => None,
        })
        .collect();
    assert!(matches!(
        native[0],
        (
            0,
            NativeVoiceEvent::On {
                token: 1,
                note: 69,
                ..
            }
        )
    ));
    for (i, semitones) in [0.0, 1.0, 2.0].into_iter().enumerate() {
        assert!(
            matches!(native[i+1], (0, NativeVoiceEvent::Pitch { token: 1, semitones: s }) if s == semitones)
        );
    }
    assert!(matches!(
        native[4],
        (24_000, NativeVoiceEvent::Off { token: 1 })
    ));
    assert!(matches!(
        native[5],
        (
            24_000,
            NativeVoiceEvent::Pitch {
                token: 1,
                semitones: 3.0
            }
        )
    ));
    assert!(matches!(
        native[6],
        (
            48_000,
            NativeVoiceEvent::Pitch {
                token: 1,
                semitones: 4.0
            }
        )
    ));
    assert_eq!(native.len(), 7, "step mode emits only authored points");

    let mut mapped = p.clone();
    mapped = oc_dsl::parse(&oc_dsl::serialize(&mapped).replace(
        "tempo: 120",
        "tempo: 120\n  tempo_map:\n    1:1 + 0.125q = 60 bpm",
    ))
    .unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&mapped.properties).unwrap();
    let mapped_events = try_interpret(&mapped, &settings).unwrap();
    for quarter in [1.0, 2.0] {
        let expected = clock.sample_at_quarter(quarter, 48_000).unwrap();
        assert!(mapped_events.iter().any(|e| e.time_samples == expected
            && matches!(
                e.event,
                MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { .. })
            )));
    }
}

#[test]
fn dense_step_curve_refusal_is_atomic_and_defaults_stay_continuous() {
    use oc_synth::note_pitch_curve::MAX_PITCH_CURVE_POINTS;
    let dense = (0..MAX_PITCH_CURVE_POINTS)
        .map(|i| format!("{}:{}", i as f64 / MAX_PITCH_CURVE_POINTS as f64, i % 1200))
        .collect::<Vec<_>>()
        .join(",");
    let p = step_project(&dense, "step");
    let before = oc_dsl::serialize(&p);
    let events = try_interpret(&p, &InterpretSettings::default()).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e.event,
                MidiMessage::NativeVoice(NativeVoiceEvent::Pitch { .. })
            ))
            .count(),
        MAX_PITCH_CURVE_POINTS
    );
    let invalid = step_project(&(dense + ",1:0"), "step");
    assert!(try_interpret(&invalid, &InterpretSettings::default()).is_err());
    assert_eq!(oc_dsl::serialize(&p), before);
    assert!(
        try_interpret(
            &step_project("0:0,1:100", "unknown"),
            &InterpretSettings::default()
        )
        .is_err()
    );
    let explicit = step_project("0:0,1:100", "linear");
    let implicit =
        oc_dsl::parse(&oc_dsl::serialize(&explicit).replace(" pitch_curve_mode=\"linear\"", ""))
            .unwrap();
    let a = try_interpret(&explicit, &InterpretSettings::default()).unwrap();
    let b = try_interpret(&implicit, &InterpretSettings::default()).unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn dense_step_events_render_and_replay_without_heap_operations() {
    let dense = (0..241)
        .map(|i| {
            format!(
                "{}:{}",
                i as f64 / 120.0,
                if i % 2 == 0 { -1200 } else { 1200 }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let p = step_project(&dense, "step");
    let source = oc_dsl::serialize(&p).replace("    @0 A4", "    @0.5 A4 q\n    @0 A4");
    let p = oc_dsl::parse(&source).unwrap();
    let settings = InterpretSettings {
        sample_rate: 8_000,
        ..Default::default()
    };
    let events = try_interpret(&p, &settings).unwrap();
    let native: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.event, MidiMessage::NativeVoice(_)))
        .collect();
    let mut synth = oc_synth::synth::OpenSynth::new(oc_synth::synth::SynthPatch {
        oscillator_a_waveform: oc_synth::synth::Waveform::Sine,
        oscillator_b_level: 0.0,
        sub_level: 0.0,
        ..Default::default()
    });
    let mut first = [0.0_f32; 10_000];
    let mut replay = [0.0_f32; 10_000];
    let operations = oc_test_support::operations(|| {
        for output in [&mut first, &mut replay] {
            synth.reset();
            let mut cursor = 0;
            for (frame, value) in output.iter_mut().enumerate() {
                while cursor < native.len() && native[cursor].time_samples <= frame as u64 {
                    synth.send_midi(&native[cursor].event).unwrap();
                    cursor += 1;
                }
                *value = synth.next_stereo(8_000)[0];
            }
        }
        synth.reset();
    });
    assert_eq!(operations, 0);
    assert_eq!(
        first, replay,
        "reset/replay reconstructs every cursor frame"
    );
    assert!(first.iter().all(|v| v.is_finite()));
    assert!(first.iter().any(|v| v.abs() > 0.001));
}
