//! Profiles interpreter tests.

use super::super::*;

#[test]
fn articulation_legato_extends_note_duration() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track strings | instrument: strings/violin
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf articulation=legato
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let note_off = events
        .iter()
        .find(|e| matches!(e.event, MidiMessage::NoteOff { note: 60, .. }))
        .unwrap();
    assert_eq!(note_off.time_samples, beats_to_samples(1.05, 120.0, 44100));
}

#[test]
fn vibrato_and_pitch_bend_emit_pitch_events() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track lead | instrument: synth/lead_saw
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf bend=100ct vibrato={rate:5Hz,depth:20ct,delay:0s}
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let bends: Vec<_> = events
        .iter()
        .filter_map(|e| match e.event {
            MidiMessage::PitchBend { value, .. } => Some(value),
            _ => None,
        })
        .collect();

    assert!(bends.contains(&4096));
    assert!(bends.contains(&819));
    assert!(bends.contains(&0));
}

#[test]
fn expression_and_automation_emit_control_changes() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track flute | instrument: woodwinds/flute
  automation: brightness { 1:1:0=25%, 1:2:0=75% }
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf expression={dynamics:mp->ff,breathiness:20%}
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 11,
            value: 64,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 11,
            value: 112,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 2,
            value: 25,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 74,
            value: 31,
            ..
        }
    )));
}

#[test]
fn encoded_plugin_automation_lane_emits_exact_parameter_name() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track lead | instrument: synth/lead_saw
  automation: pluginparam_46696c746572204375746f6666 { 1:1:0=25%, 1:2:0=75% }
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let values: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.event {
            MidiMessage::PluginParam { name, value } if name == "Filter Cutoff" => Some(*value),
            _ => None,
        })
        .collect();

    assert_eq!(values, vec![0.25, 0.75]);
}

/// The GUI writes built-in synth lanes with the hosted-plugin encoding; this
/// literal is the lane name `utils/automationTarget.ts` produces for
/// `filter.cutoff`, and the engine must decode it back to that exact key.
#[test]
fn synth_automation_lane_decodes_to_the_synth_parameter_key() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track lead | instrument: synth/lead_saw
  automation: pluginparam_66696c7465722e6375746f6666 { 1:1:0=0.926, 2:1:0=1 }
  @phrase intro [1:1:0 - 2:1:0]
C4 w mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let values: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.event {
            MidiMessage::PluginParam { name, value } if name == "filter.cutoff" => Some(*value),
            _ => None,
        })
        .collect();

    assert_eq!(values.len(), 2, "{values:?}");
    assert!((values[0] - 0.926).abs() < 1e-9);
    assert!((values[1] - 1.0).abs() < 1e-9);
    assert!(
        events.iter().all(|event| !matches!(
            &event.event,
            MidiMessage::PluginParam { name, .. } if name != "filter.cutoff"
        )),
        "the lane must not also decode as a profile control"
    );
}

#[test]
fn preset_profile_emits_high_resolution_control_changes() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset expressive_strings
  source: clap
  plugin: "example.strings"
  params: {control_dynamics_cc14: 1}

@track strings | instrument: strings/violin, preset: expressive_strings
  automation: dynamics { 1:1:0=50% }
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange14 { channel: 0, controller: 1, value }
            if (value as i32 - 8191).abs() <= 1
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, MidiMessage::ControlChange { controller: 1, .. }))
    );
}

#[test]
fn pedal_automation_emits_standard_control_changes() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track piano | instrument: keys/piano
  automation: sustain { 1:1:0=true, 1:2:0=false }
  automation: sostenuto { 1:3:0=127 }
  automation: una_corda { 1:4:0=1 }
  automation: half_pedal { 2:1:0=50% }
  @phrase intro [1:1:0 - 2:2:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 64,
            value: 127,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 64,
            value: 0,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 66,
            value: 127,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 67,
            value: 127,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 64,
            value: 63,
            ..
        }
    )));
}

#[test]
fn preset_profile_emits_control_noise_triggers() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset noisy_piano
  source: clap
  plugin: "example.piano"
  params: {control_sustain_on_note: "C0", control_sustain_off_note: "C#0", control_sustain_noise_velocity: 20, control_sustain_noise_duration_ms: 25}

@track piano | instrument: keys/piano, preset: noisy_piano
  automation: sustain { 1:1:0=true, 1:2:0=false }
  @phrase intro [1:1:0 - 1:3:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let on_time = 0;
    let off_time = beats_to_samples(1.0, 120.0, settings.sample_rate);
    let duration = (settings.sample_rate as f64 * 0.025) as u64;

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { channel: 0, note: 12, velocity: 20 }
            if e.time_samples == on_time
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOff { channel: 0, note: 12 }
            if e.time_samples == on_time + duration
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { channel: 0, note: 13, velocity: 20 }
            if e.time_samples == off_time
    )));
}

#[test]
fn preset_profile_emits_control_noise_on_range_entry() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset noisy_piano
  source: clap
  plugin: "example.piano"
  params: {control_sustain_on_note: "C0", control_sustain_range_min: 25%, control_sustain_range_max: 75%, control_sustain_noise_velocity: 20}

@track piano | instrument: keys/piano, preset: noisy_piano
  automation: sustain { 1:1:0=10%, 1:2:0=50%, 1:3:0=90% }
  @phrase intro [1:1:0 - 1:4:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let entry_time = beats_to_samples(1.0, 120.0, settings.sample_rate);

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { channel: 0, note: 12, velocity: 20 }
            if e.time_samples == entry_time
    )));
    assert!(!events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { channel: 0, note: 12, velocity: 20 }
            if e.time_samples == beats_to_samples(2.0, 120.0, settings.sample_rate)
    )));
}

#[test]
fn preset_profile_injects_keyswitch_and_overrides_expression_cc() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset expressive_strings
  source: clap
  plugin: "example.strings"
  params: {articulation_legato_keyswitch: "C0", articulation_legato_cc: 68, articulation_legato_cc_value: 127, articulation_legato_cc_reset: 0, articulation_legato_param: "Mode", articulation_legato_param_value: 50%, articulation_legato_param_reset: 0, articulation_legato_release_note: "D0", articulation_legato_release_velocity: 12, articulation_legato_release_duration_ms: 30, articulation_legato_duration_scale: 1.1, articulation_legato_overlap_ms: 10, cc_dynamics: 1}

@track strings | instrument: strings/violin, preset: expressive_strings
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf articulation=legato expression={dynamics:mp->ff}
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn {
            note: 12,
            velocity: 1,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 1,
            value: 64,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 68,
            value: 127,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::PluginParam { name, value }
            if name == "Mode" && (*value - 0.5).abs() < 0.0001
    )));
    let note_off = events
        .iter()
        .find(|e| matches!(e.event, MidiMessage::NoteOff { note: 60, .. }))
        .unwrap();
    assert_eq!(
        note_off.time_samples,
        beats_to_samples(1.05 * 1.1, 120.0, 44100) + 441
    );
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 68,
            value: 0,
            ..
        } if e.time_samples == note_off.time_samples
    )));
    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::PluginParam { name, value }
            if name == "Mode" && *value == 0.0 && e.time_samples == note_off.time_samples
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn {
            note: 14,
            velocity: 12,
            ..
        } if e.time_samples == note_off.time_samples
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOff { note: 14, .. }
            if e.time_samples == note_off.time_samples + 1323
    )));
    assert!(!events.iter().any(|e| matches!(
        e.event,
        MidiMessage::ControlChange {
            controller: 11,
            value: 64,
            ..
        }
    )));
}

#[test]
fn preset_profile_delays_sustain_aware_release_trigger_until_pedal_release() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset sustained_piano
  source: clap
  plugin: "example.piano"
  params: {articulation_legato_release_note: "D0", articulation_legato_release_velocity: 12, articulation_legato_release_duration_ms: 30, articulation_legato_release_sustain_aware: true}

@track piano | instrument: keys/piano, preset: sustained_piano
  automation: sustain { 1:1:0=true, 1:3:0=false }
  @phrase intro [1:1:0 - 1:3:0]
C4 q mf articulation=legato
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let note_off_time = beats_to_samples(1.05, 120.0, settings.sample_rate);
    let pedal_release_time = beats_to_samples(2.0, 120.0, settings.sample_rate);

    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOff { note: 60, .. } if e.time_samples == note_off_time
    )));
    assert!(events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { note: 14, velocity: 12, .. }
            if e.time_samples == pedal_release_time
    )));
    assert!(!events.iter().any(|e| matches!(
        e.event,
        MidiMessage::NoteOn { note: 14, .. } if e.time_samples == note_off_time
    )));
}

#[test]
fn preset_profile_maps_expression_to_plugin_param() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset expressive_strings
  source: vst3
  plugin: "example.strings"
  params: {param_dynamics: "Expression"}

@track strings | instrument: strings/violin, preset: expressive_strings
  automation: dynamics { 1:1:0=25%, 1:2:0=75% }
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf expression={dynamics:mp->ff}
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::PluginParam { name, value }
            if name == "Expression" && (*value - (64.0 / 127.0)).abs() < 0.0001
    )));
    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::PluginParam { name, value }
            if name == "Expression" && (*value - 0.75).abs() < 0.0001
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, MidiMessage::ControlChange { controller: 11, .. }))
    );
}

#[test]
fn preset_profile_maps_expression_to_note_expression() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset expressive_strings
  source: clap
  plugin: "example.strings"
  params: {control_brightness_note_expression: "brightness"}

@track strings | instrument: strings/violin, preset: expressive_strings
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf expression={brightness:25%->75%}
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::NoteExpression { channel: 0, note: 60, expression, value }
            if expression == "brightness" && (*value - 0.25).abs() < 0.0001
    )));
    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::NoteExpression { channel: 0, note: 60, expression, value }
            if expression == "brightness" && (*value - 0.75).abs() < 0.0001
    )));
}

#[test]
fn preset_profile_emits_microtuning_note_expression() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset tuned_piano
  source: vst3
  plugin: "example.piano"
  params: {microtuning_C4_cents: "-12.5", tuning_61_cents: 8}

@track piano | instrument: keys/piano, preset: tuned_piano
  @phrase intro [1:1:0 - 1:2:0]
C4 q mf
C#4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());

    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::NoteExpression { channel: 0, note: 60, expression, value }
            if expression == "tuning" && (*value + 0.125).abs() < 0.0001
    )));
    assert!(events.iter().any(|e| matches!(
        &e.event,
        MidiMessage::NoteExpression { channel: 0, note: 61, expression, value }
            if expression == "tuning" && (*value - 0.08).abs() < 0.0001
    )));
}

#[test]
fn preset_profile_allocates_mpe_note_channels() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset mpe_synth
  source: clap
  plugin: "example.synth"
  params: {mpe_enabled: true, mpe_first_channel: 2, mpe_last_channel: 3, microtuning_C4_cents: 10}

@track lead | instrument: synth/pad, preset: mpe_synth
  @phrase p [1:1:0 - 1:2:0]
@0.0 C4 q mf
@0.0 E4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let note_channels: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { channel, note, .. } => Some((channel, note)),
            _ => None,
        })
        .collect();

    assert_eq!(note_channels, vec![(2, 60), (3, 64)]);
    assert!(events.iter().any(|event| matches!(
        &event.event,
        MidiMessage::NoteExpression { channel: 2, note: 60, expression, .. }
            if expression == "tuning"
    )));
}

#[test]
fn preset_profile_humanizes_articulated_notes_deterministically() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset expressive_strings
  source: clap
  plugin: "example.strings"
  params: {articulation_staccato_humanize_timing_ms: 8, articulation_staccato_humanize_velocity: 5}

@track strings | instrument: strings/violin, preset: expressive_strings
  @phrase intro [1:2:0 - 1:3:0]
C4 q mf articulation=staccato
"#;
    let project = oc_dsl::parse(source).unwrap();
    let settings = InterpretSettings::default();
    let events = interpret(&project, &settings);
    let base_on = beats_to_samples(1.0, 120.0, settings.sample_rate);
    let expected = articulation_humanize_for_note(
        Some(Articulation::Staccato),
        &InstrumentProfile::from_preset_params(&project.presets[0].params, settings.sample_rate),
        0,
        0,
        60,
        base_on,
    );
    let note_on = events
        .iter()
        .find(|e| matches!(e.event, MidiMessage::NoteOn { note: 60, .. }))
        .unwrap();
    let note_off = events
        .iter()
        .find(|e| matches!(e.event, MidiMessage::NoteOff { note: 60, .. }))
        .unwrap();

    assert_eq!(
        note_on.time_samples,
        apply_sample_offset(base_on, expected.timing_offset)
    );
    assert!(matches!(
        note_on.event,
        MidiMessage::NoteOn { velocity, .. }
            if velocity == apply_velocity_offset(80, expected.velocity_offset)
    ));
    assert_eq!(
        note_off.time_samples,
        apply_sample_offset(
            beats_to_samples(1.5, 120.0, settings.sample_rate),
            expected.timing_offset
        )
    );
}

#[test]
fn preset_profile_round_robins_articulation_keyswitches_deterministically() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@preset rr_strings
  source: clap
  plugin: "example.strings"
  params: {articulation_staccato_round_robin_keyswitches: "C0 C#0 D0"}

@track strings | instrument: strings/violin, preset: rr_strings
  @phrase intro [1:1:0 - 2:1:0]
C4 q mf articulation=staccato
D4 q mf articulation=staccato
E4 q mf articulation=staccato
F4 q mf articulation=staccato
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let keyswitches: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn {
                note, velocity: 1, ..
            } if note < 20 => Some(note),
            _ => None,
        })
        .collect();

    assert_eq!(keyswitches, vec![12, 13, 14, 12]);
}

/// A control lane that is a clip emits nothing before its head, opens on its
/// first value, and stops writing at its exclusive end. A muted lane emits
/// nothing anywhere.
#[test]
fn a_ranged_control_lane_writes_only_inside_its_span() {
    let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4

@track flute | instrument: woodwinds/flute
  automation: brightness [2:1 - 3:1] { 2:2:0=25%, 2:4:0=75% }
  @phrase intro [1:1:0 - 3:1:0]
C4 q mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let events = interpret(&project, &InterpretSettings::default());
    let changes: Vec<(u64, u8)> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::ControlChange {
                controller: 74,
                value,
                ..
            } => Some((event.time_samples, value)),
            _ => None,
        })
        .collect();

    // 120 bpm at the default 44.1 kHz: bar 2 is sample 88 200, bar 2 beat 2 is
    // 110 250, bar 2 beat 4 is 154 350 and bar 3 is the exclusive end.
    assert_eq!(changes, vec![(88_200, 31), (110_250, 31), (154_350, 95)]);

    let muted = oc_dsl::parse(&source.replace("[2:1 - 3:1]", "muted")).unwrap();
    let events = interpret(&muted, &InterpretSettings::default());
    assert!(
        !events.iter().any(|event| matches!(
            event.event,
            MidiMessage::ControlChange { controller: 74, .. }
        )),
        "a muted lane still emitted a control change"
    );
}
