#[test]
fn preserves_fractional_explicit_beats_exactly() {
    let source = "@project \"fractional\"\n@track lead | instrument:piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @2.6875 C4 16th mp v=64\n    @3.3125 D4 16th mp v=64\n";
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    let serialized = oc_dsl::serialize(&project);
    assert!(serialized.contains("@2.6875"));
    let reopened = oc_dsl::parse(&serialized).unwrap();
    assert_eq!(
        serde_json::to_value(project).unwrap(),
        serde_json::to_value(reopened).unwrap()
    );
}
