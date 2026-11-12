//! Project-recorded MPE zones and per-note expression round-trip.

const MPE_SOURCE: &str = r#"@project
  tempo: 120
  mpe: {lower: 2-8, upper: 15-9, bend_range: 48}
@track lead | instrument: synth/lead_square
  @phrase notes [1:1 - 1:3]
    C4 q mf bend=100ct expression={pressure: 20%->90%, timbre: 10%->80%}
"#;

#[test]
fn mpe_config_and_expression_round_trip_dsl_and_json() {
    let project = oc_dsl::parse(MPE_SOURCE).expect("MPE project must parse");
    oc_dsl::validate(&project).expect("MPE project must validate");
    let mpe = project.properties.mpe.expect("zone config");
    assert_eq!(mpe.bend_range, Some(48.0));
    assert_eq!(mpe.lower.unwrap().first, 2);
    assert_eq!(mpe.upper.unwrap().last, 9);
    assert_eq!(mpe.channel_for_note(0), Some(1));
    assert_eq!(mpe.channel_for_note(1), Some(2));
    assert_eq!(mpe.channel_for_note(6), Some(7));
    assert_eq!(mpe.channel_for_note(7), Some(1));
    assert_eq!(mpe.bend_range_semitones(), 48.0);

    let text = oc_dsl::serialize(&project);
    assert!(text.contains("mpe: {lower: 2-8, upper: 15-9, bend_range: 48}"));
    assert!(text.contains("bend=100ct"));
    assert!(text.contains("pressure: 20%->90%"));
    let reopened = oc_dsl::parse(&text).expect("formatted MPE project must re-parse");
    assert_eq!(project, reopened);

    let json = serde_json::to_value(&project).unwrap();
    assert_eq!(json["properties"]["mpe"]["lower"]["first"], 2);
    assert_eq!(json["properties"]["mpe"]["bend_range"], 48.0);
    let restored: oc_dsl::ast::Project = serde_json::from_value(json).unwrap();
    assert_eq!(project, restored);
}

#[test]
fn project_without_mpe_is_unchanged() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 1:2]\n    C4 q mf\n",
    )
    .unwrap();
    assert!(project.properties.mpe.is_none());
    let text = oc_dsl::serialize(&project);
    assert!(!text.contains("mpe:"));
    let json = serde_json::to_value(&project).unwrap();
    assert!(json["properties"].get("mpe").is_none());
}
