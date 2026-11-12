//! The inspector's three-part latency readout command.

use crate::commands::mixer::plan_track_latency;

const SOURCE: &str = r#"@ocp "0.2.0"

@project "latency readout"
  tempo: 120
  time_signature: 4/4

@track early | instrument: keys/piano
  mix: latency=32
  @phrase line [1:1 - 2:1]
    C4 w mf

@track late | instrument: keys/piano
  mix: latency=-16
  effects:
    - limiter: ceiling=-0.3dB
  @phrase line [2:1 - 3:1]
    E4 w mf
"#;

#[test]
fn readout_splits_reported_latency_from_the_manual_offset() {
    let project = oc_dsl::parse(SOURCE).unwrap();
    oc_dsl::validate(&project).unwrap();

    let early = plan_track_latency(&project, 0).unwrap();
    assert_eq!(early.readout.track, "early");
    assert_eq!(early.readout.reported_samples, 0);
    assert_eq!(early.readout.requested_offset_samples, 32);
    // The limiter track defines the reference, so 32 samples of headroom exist.
    let reference = early.readout.destinations[0].reference_samples;
    assert!(reference > 0, "the limiter must report latency");
    assert_eq!(early.readout.applied_offset_samples, 32);
    assert!(!early.readout.offset_clamped);
    assert_eq!(early.readout.effective_samples, 32);
    assert_eq!(early.readout.destinations.len(), 1);
    assert_eq!(early.readout.destinations[0].destination, "master");
    assert_eq!(
        early.readout.destinations[0].compensation_samples,
        reference - 32
    );
    assert!(early.note.is_none());

    let late = plan_track_latency(&project, 1).unwrap();
    assert_eq!(late.readout.reported_samples, reference);
    assert_eq!(late.readout.requested_offset_samples, -16);
    assert_eq!(late.readout.applied_offset_samples, -16);
    assert_eq!(late.readout.effective_samples, reference - 16);
    assert_eq!(late.readout.destinations[0].compensation_samples, 16);
}

#[test]
fn readout_refuses_a_track_index_past_the_project() {
    let project = oc_dsl::parse(SOURCE).unwrap();
    assert!(plan_track_latency(&project, 9).is_err());
}

#[test]
fn readout_leaves_plugin_effects_out_with_a_note() {
    let mut project = oc_dsl::parse(SOURCE).unwrap();
    project.tracks[1].effects.push(oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Plugin("org.example.absent".into()),
        params: vec![],
        bypass: false,
        sidechain_source: None,
    });
    let readout = plan_track_latency(&project, 1).unwrap();
    assert!(
        readout.note.is_some(),
        "a stripped plugin effect must be reported, not hidden"
    );
}
