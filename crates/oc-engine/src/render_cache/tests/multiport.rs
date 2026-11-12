//! Explicit plugin output ports: per-port track routing and refusals.

use super::*;

#[test]
fn a_pair_override_selects_within_a_wide_port() {
    let ports = [
        AudioPort {
            channels: 4,
            first_pair: 0,
            kind: oc_host::ports::PortKind::Audio,
        },
        AudioPort {
            channels: 2,
            first_pair: 2,
            kind: oc_host::ports::PortKind::Audio,
        },
    ];
    let mut profile = InstrumentProfile::from_preset_params(&[], 48_000);
    profile.set_output_port(Some(0), Some(1));
    assert_eq!(
        resolve_output_pair(&ports, &profile, "VST3", "wide", "track").unwrap(),
        1
    );
    // Beyond the port's own pairs is a project error, not a silent fallback.
    profile.set_output_port(Some(0), Some(2));
    let error = resolve_output_pair(&ports, &profile, "VST3", "wide", "track").unwrap_err();
    assert!(error.contains("port 1 has 2 stereo pair(s)"), "{error}");
}

#[test]
fn only_a_later_pair_counts_as_a_lost_output() {
    let track = |routing: &str| {
        oc_dsl::parse(&format!(
            "@project\n  tempo: 120\n@preset four\n  source: vst3\n  plugin: \"test.fake.four_bus\"\n@track out | preset: four, instrument: keys/piano{routing}\n  @phrase body [1:1 - 2:1]\n    C4 q mf\n"
        ))
        .unwrap()
    };
    // No routing at all asks for the main pair, so a fallback to it is not a
    // lost output and must stay out of the warning log.
    assert!(!track_requests_non_main_output(&track(""), 0, 48_000));
    assert!(!track_requests_non_main_output(
        &track(", port: 0"),
        0,
        48_000
    ));
    assert!(track_requests_non_main_output(
        &track(", port: 2"),
        0,
        48_000
    ));
    assert!(track_requests_non_main_output(
        &track(", port: 0, pair: 1"),
        0,
        48_000
    ));
    let mut legacy = track("");
    legacy.tracks[0].engine_params = Some(vec![(
        "plugin_output_pair".to_string(),
        oc_dsl::ast::ParamValue::Number(2.0),
    )]);
    assert!(track_requests_non_main_output(&legacy, 0, 48_000));
}

#[test]
fn tracks_without_a_port_keep_reading_pair_zero() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset four\n  source: vst3\n  plugin: \"test.fake.four_bus\"\n@track out | preset: four, instrument: keys/piano\n  @phrase body [1:1 - 2:1]\n    C4 q mf\n",
    )
    .unwrap();
    let profile = instrument_profile_for_track(&project, 0, 48_000);
    assert_eq!(profile.output_port(), None);
    assert_eq!(profile.output_pair(), 0);
    let ports =
        oc_host::vst3_instance::test_support::fake_instance_for_id("test.fake.four_bus", 512)
            .expect("four-bus fake")
            .output_ports();
    assert_eq!(
        resolve_output_pair(&ports, &profile, "VST3", "four", "out").unwrap(),
        0
    );
}

#[test]
fn first_class_pair_without_port_overrides_legacy_absolute_selection_across_formats() {
    let wide = [AudioPort {
        channels: 4,
        first_pair: 0,
        kind: oc_host::ports::PortKind::Audio,
    }];
    for format in ["CLAP", "VST3", "VST2", "AU", "LV2"] {
        let mut profile = InstrumentProfile::from_preset_params(
            &[("plugin_output_pair".into(), ParamValue::Number(2.0))],
            48_000,
        );
        profile.set_output_port(None, Some(0));
        assert_eq!(
            resolve_output_pair(&[AudioPort::STEREO], &profile, format, "typed", "t").unwrap(),
            0
        );
        profile.set_output_port(None, Some(1));
        assert!(profile.requests_non_main_output());
        assert_eq!(
            resolve_output_pair(&wide, &profile, format, "typed", "t").unwrap(),
            1
        );
        assert!(resolve_output_pair(&[AudioPort::STEREO], &profile, format, "typed", "t").is_err());
        profile.set_output_port(None, Some(2));
        assert!(resolve_output_pair(&wide, &profile, format, "typed", "t").is_err());
        profile.set_output_port(None, None);
        assert_eq!(
            resolve_output_pair(&wide, &profile, format, "legacy", "t").unwrap(),
            1
        );
    }
}
