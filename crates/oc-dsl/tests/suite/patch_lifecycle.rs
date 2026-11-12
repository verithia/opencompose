//! A project preset carries its provenance (`patch:`) and its effect chain;
//! both must survive the `.ocp` round trip byte for byte.

const FIXTURE: &str = r#"@ocp "0.2.0"

@project "Preset lifecycle"
  tempo: 120
  time_signature: 4/4

@preset warm_pad
  source: builtin
  plugin: "builtin.open_synth"
  patch: "Glass Pad"
  params: {filter.cutoff: 4200, osc.a.level: 0.58}
  effects:
    - chorus: rate=0.6, depth=0.3, mix=30%
    - delay: time=0.3, feedback=0.35, mix=22%

@preset warm_pad_lead
  source: builtin
  plugin: "builtin.open_synth"
  extends: warm_pad
  params: {voice.mono: 1}

@track lead | preset: warm_pad_lead, instrument: synth/lead_square
  @phrase lead [1:1 - 2:1]
    C4 q mf
"#;

#[test]
fn patch_provenance_and_effects_round_trip_byte_identically() {
    let project = oc_dsl::parse(FIXTURE).expect("fixture parses");
    let preset = &project.presets[0];
    assert_eq!(preset.patch.as_deref(), Some("Glass Pad"));
    assert_eq!(preset.effects.len(), 2);

    let serialized = oc_dsl::serialize(&project);
    assert_eq!(
        serialized, FIXTURE,
        "a parsed project did not serialise back"
    );
    let reparsed = oc_dsl::parse(&serialized).expect("serialized project parses");
    assert_eq!(reparsed.presets[0], project.presets[0]);
    assert_eq!(oc_dsl::serialize(&reparsed), serialized);
}

#[test]
fn a_project_without_patch_or_effects_keeps_its_old_shape() {
    let source = r#"@ocp "0.2.0"

@project "Old project"
  tempo: 120
  time_signature: 4/4

@preset plain
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.level: 0.5}

@track lead | preset: plain, instrument: synth/lead_square
  @phrase lead [1:1 - 2:1]
    C4 q mf
"#;
    let project = oc_dsl::parse(source).expect("old project parses");
    assert_eq!(project.presets[0].patch, None);
    assert!(project.presets[0].effects.is_empty());
    assert_eq!(oc_dsl::serialize(&project), source);
}
