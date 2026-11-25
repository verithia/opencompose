use oc_dsl::ast::ParamValue;

#[test]
fn quoted_effect_state_and_labels_preserve_types_and_escaped_content() {
    let text = r#"@project
@track t | instrument: synth/lead
  effects:
    - plugin:"vst3.test": state_hex="00000000", label="a,b \"quoted\" \\path", mode=hall, ratio=2:1
"#;
    let project = oc_dsl::parse(text).unwrap();
    let params = &project.tracks[0].effects[0].params;
    assert_eq!(params[0].1, ParamValue::String("00000000".into()));
    assert_eq!(
        params[1].1,
        ParamValue::String("a,b \"quoted\" \\path".into())
    );
    let serialized = oc_dsl::serializer::serialize(&project);
    let restored = oc_dsl::parse(&serialized).unwrap();
    assert_eq!(project.tracks[0].effects, restored.tracks[0].effects);
    assert!(serialized.contains("state_hex=\"00000000\""));
    assert!(serialized.contains("mode=hall"));
    assert!(serialized.contains("ratio=2:1"));
}

#[test]
fn invalid_quoted_effect_values_are_errors_without_partial_parsing() {
    for value in [
        r#""\uD800""#,
        r#""\q""#,
        "\"unterminated",
        "\"line\nbreak\"",
    ] {
        let text = format!(
            "@project\n@track t | instrument: synth/lead\n  effects:\n    - plugin:test: state_hex={value}\n"
        );
        assert!(oc_dsl::parse(&text).is_err(), "{value}");
    }
}

#[test]
fn quoted_numeric_boolean_and_unit_text_stays_a_string() {
    let text = "@project\n@track t | instrument: synth/lead\n  effects:\n    - plugin:test: a=\"1\", b=\"true\", c=\"10ms\", sidechain=\"Kick A\"\n";
    let project = oc_dsl::parse(text).unwrap();
    assert!(
        project.tracks[0].effects[0]
            .params
            .iter()
            .all(|(_, value)| matches!(value, ParamValue::String(_)))
    );
    assert_eq!(
        project.tracks[0].effects[0].sidechain_source.as_deref(),
        Some("Kick A")
    );
    let restored = oc_dsl::parse(&oc_dsl::serializer::serialize(&project)).unwrap();
    assert_eq!(project.tracks[0].effects, restored.tracks[0].effects);
}
