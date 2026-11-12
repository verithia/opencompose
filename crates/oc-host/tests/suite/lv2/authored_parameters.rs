//! Checked static assignments exercise the actual generated LV2 binary. The
//! existing fixture builder is mandatory; these cases never skip plugin setup.

use super::*;
use oc_host::preset_parameters::{AuthoredParameter, ParameterErrorKind, ParameterWriteDomain};

fn assignment(name: &str, value: f64) -> AuthoredParameter<'_> {
    AuthoredParameter {
        name,
        value,
        domain: ParameterWriteDomain::NativePlain,
    }
}
fn extra_controls(name: &str, controls: &str) -> FixtureBundle {
    FixtureBundle::new(
        name,
        &manifest(&format!(
            r#"<{GAIN_URI}> a lv2:Plugin ; lv2:binary <{{{{BINARY}}}}> ;
            doap:name "Fixture Authored Controls" ; {GAIN_PORTS}, {controls} ."#
        )),
    )
}

#[test]
fn lv2_authored_batch_uses_last_native_alias_and_changes_actual_pcm() {
    let bundle = gain_bundle("authored-gain");
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    let input = vec![0.25, -0.5, 1.0, -1.0];
    let mut before = input.clone();
    instance.process_interleaved(&mut before, 48_000);
    assert_eq!(before, input);
    instance
        .apply_authored_parameters(&[assignment("Gain", f64::NAN), assignment("gain_Gain", 0.5)])
        .unwrap();
    let mut after = input;
    instance.process_interleaved(&mut after, 48_000);
    assert_eq!(after, [0.125, -0.25, 0.5, -0.5]);
    assert_eq!(instance.get_param("gain"), Some(0.5));
    instance
        .apply_authored_parameters(&[assignment("Gain", 0.1)])
        .unwrap();
    assert_eq!(instance.get_param("gain"), Some(f64::from(0.1f32)));
}

#[test]
fn lv2_authored_late_invalid_assignment_never_mutates_controls_or_pcm() {
    let bundle = gain_bundle("authored-atomic");
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    for invalid in [
        assignment("Gain", -0.5),
        assignment("Gain", 4.5),
        assignment("Gain", f64::MAX),
        assignment("Gain", f64::NAN),
        assignment("Gain", f64::INFINITY),
        assignment("missing", 1.0),
    ] {
        assert!(
            instance
                .apply_authored_parameters(&[assignment("gain", 0.25), invalid])
                .is_err()
        );
        assert_eq!(instance.get_param("gain"), Some(1.0));
        let mut output = vec![0.5, -0.5];
        instance.process_interleaved(&mut output, 48_000);
        assert_eq!(output, [0.5, -0.5]);
    }
    let error = instance
        .apply_authored_parameters(&[AuthoredParameter {
            name: "Gain",
            value: 0.5,
            domain: ParameterWriteDomain::Normalized,
        }])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::UnsupportedDomain));
    assert_eq!(instance.get_param("gain"), Some(1.0));
}

#[test]
fn lv2_authored_reads_full_ports_but_refuses_output_and_host_rate_controls() {
    let bundle = extra_controls(
        "authored-port-policy",
        r#"[
        a lv2:ControlPort, lv2:OutputPort ; lv2:index 5 ; lv2:symbol "meter" ;
        lv2:name "Meter" ; lv2:minimum 0 ; lv2:maximum 128 ; lv2:default 0
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 6 ; lv2:symbol "depth" ;
        lv2:name "Hidden Depth" ; lv2:minimum -2 ; lv2:maximum 2 ; lv2:default 0 ;
        lv2:portProperty lv2:notOnGUI
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 7 ; lv2:symbol "sample_rate" ;
        lv2:name "Host Rate" ; lv2:minimum 8000 ; lv2:maximum 192000 ;
        lv2:default 48000 ; lv2:designation lv2:sampleRate
    ]"#,
    );
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    for name in ["meter", "sample_rate"] {
        let error = instance
            .apply_authored_parameters(&[assignment("gain", 0.25), assignment(name, 48000.0)])
            .unwrap_err();
        assert!(matches!(error.kind, ParameterErrorKind::ReadOnly));
        assert_eq!(instance.get_param("gain"), Some(1.0));
    }
    instance
        .apply_authored_parameters(&[assignment("depth_Hidden Depth", -1.5)])
        .unwrap();
    assert_eq!(instance.get_param("depth"), Some(-1.5));
    assert_eq!(instance.get_param("sample_rate"), Some(48000.0));
}

#[test]
fn lv2_authored_ambiguous_names_require_an_existing_unambiguous_alias() {
    let bundle = extra_controls(
        "authored-ambiguous",
        r#"[
        a lv2:ControlPort, lv2:OutputPort ; lv2:index 5 ; lv2:symbol "meter" ;
        lv2:name "Meter" ; lv2:minimum 0 ; lv2:maximum 128 ; lv2:default 0
    ], [
        a lv2:ControlPort, lv2:InputPort ; lv2:index 6 ; lv2:symbol "aux_gain" ;
        lv2:name "Gain" ; lv2:minimum 0 ; lv2:maximum 4 ; lv2:default 1
    ]"#,
    );
    let mut instance = Lv2Instance::load(bundle.path(), GAIN_URI, 48_000, 128).unwrap();
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Gain", 0.5)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::Ambiguous
    ));
    assert_eq!(instance.get_param("aux_gain"), Some(1.0));
    instance
        .apply_authored_parameters(&[
            assignment("gain_Gain", 0.5),
            assignment("aux_gain_Gain", 2.0),
        ])
        .unwrap();
    assert_eq!(instance.get_param("aux_gain"), Some(2.0));
    let mut output = vec![0.5, -0.5];
    instance.process_interleaved(&mut output, 48_000);
    assert_eq!(output, [0.25, -0.25]);
}
