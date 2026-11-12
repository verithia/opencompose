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
