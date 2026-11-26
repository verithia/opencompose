//! Admission and interpretation agree on shaped built-in parameter lanes.

#[test]
fn encoded_discrete_builtin_curves_are_refused_before_interpretation() {
    let source = "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track t | preset: p, instrument: synth/lead_saw\n  automation: PARAM {1:1=0.2 curve=exponential curvature=-4, 2:1=0.8}\n";
    for (parameter, accepted) in [("filter.model", false), ("filter.cutoff", true)] {
        let text = source.replace(
            "PARAM",
            &oc_dsl::automation::encode_plugin_automation_param(parameter),
        );
        let project = oc_dsl::parse(&text).unwrap();
        assert_eq!(oc_dsl::validate(&project).is_ok(), accepted);
        let result = super::try_interpret(&project, &Default::default());
        assert_eq!(result.is_ok(), accepted, "{parameter}: {result:?}");
    }
    let external = source
        .replace("source: builtin", "source: vst3")
        .replace("builtin.open_synth", "test.plugin")
        .replace(
            "PARAM",
            &oc_dsl::automation::encode_plugin_automation_param("filter.model"),
        );
    let project = oc_dsl::parse(&external).unwrap();
    oc_dsl::validate(&project).unwrap();
    super::try_interpret(&project, &Default::default()).unwrap();
}
