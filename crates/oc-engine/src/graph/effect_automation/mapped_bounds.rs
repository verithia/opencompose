//! Recipient bounds apply to the curve's mapped output, not only its anchors.

use oc_dsp::schema::DeviceSchema;

pub(super) fn validate(
    parameter: &str,
    device: Option<&DeviceSchema>,
    bounds: Option<(f64, f64)>,
) -> Result<(), String> {
    oc_dsl::automation::validate_mapped_bounds(parameter, device.map(|device| device.id), bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::AutomationLane;
    use oc_dsp::schema::find_device;

    fn mapped_lane(mapping: serde_json::Value) -> AutomationLane {
        let project = oc_dsl::parse(
            "@project\n@bus out\n  effects:\n    - gain: factor=1\n  automation: fx1.factor {1:1=0, 2:1=1}\n",
        )
        .unwrap();
        let mut lane = serde_json::to_value(&project.buses[0].automation[0]).unwrap();
        lane["value_mapping"] = mapping;
        serde_json::from_value(lane).unwrap()
    }

    #[test]
    fn compilation_checks_transformed_values_after_valid_raw_anchors() {
        let properties = oc_dsl::ast::ProjectProperties::default();
        let clock = oc_dsl::timing::ProjectClock::new(&properties).unwrap();
        let gain = find_device("gain").unwrap();
        let maximum = gain.param("factor").unwrap().max;
        let invalid = mapped_lane(serde_json::json!({"curve":"linear", "scale":maximum + 1.0}));
        assert!(
            super::super::compile_lane(
                "factor",
                &invalid,
                Some(gain),
                &clock,
                48_000,
                "bus:out",
                0
            )
            .is_err()
        );
        let complement =
            mapped_lane(serde_json::json!({"curve":"linear", "scale":-1.0, "offset":1.0}));
        let compiled = super::super::compile_lane(
            "factor",
            &complement,
            Some(gain),
            &clock,
            48_000,
            "bus:out",
            0,
        )
        .unwrap();
        assert_eq!(
            compiled.input.timeline().unwrap().value_bounds(),
            Some((0.0, 1.0))
        );
        assert!(
            (compiled
                .input
                .timeline()
                .unwrap()
                .envelope_value(48_000.0)
                .unwrap()
                - 0.5)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn raw_mapped_cutoff_anchors_need_not_be_in_the_hz_range() {
        let source = "@project\n@bus out\n  effects:\n    - dynamic_eq: band12_enabled=true\n  automation: fx1.band12_freq mapping(linear, scale=1000, offset=20) {1:1=0, 2:1=1}\n";
        let project = oc_dsl::parse(source).unwrap();
        assert!(oc_dsl::validate(&project).is_ok());
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let lane = &project.buses[0].automation[0];
        let compiled = super::super::compile_lane(
            "band12_freq",
            lane,
            find_device("dynamic-eq"),
            &clock,
            48_000,
            "bus:out",
            0,
        )
        .unwrap();
        assert_eq!(
            compiled.input.timeline().unwrap().value_bounds(),
            Some((20.0, 1_020.0))
        );
        assert_eq!(
            compiled.input.timeline().unwrap().envelope_value(48_000.0),
            Some(520.0)
        );

        let invalid = oc_dsl::parse(&source.replace("offset=20", "offset=0")).unwrap();
        assert!(oc_dsl::validate(&invalid).is_err());
        assert!(
            super::super::compile_lane(
                "band12_freq",
                &invalid.buses[0].automation[0],
                find_device("dynamic-eq"),
                &clock,
                48_000,
                "bus:out",
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn compilation_rejects_mapped_plugin_values_outside_normalized_domain() {
        let properties = oc_dsl::ast::ProjectProperties::default();
        let clock = oc_dsl::timing::ProjectClock::new(&properties).unwrap();
        let lane = mapped_lane(serde_json::json!({"curve":"linear", "scale":2.0}));
        assert!(
            super::super::compile_lane("Cutoff", &lane, None, &clock, 48_000, "bus:out", 0)
                .is_err()
        );
    }

    #[test]
    fn mapped_bounds_reject_invalid_outputs_instead_of_clamping() {
        let gain = Some(find_device("gain").expect("gain schema"));
        assert!(validate("factor", gain, Some((0.4, 1.9))).is_ok());
        assert!(validate("factor", gain, Some((-0.01, 1.0))).is_err());
        let maximum = gain.unwrap().param("factor").unwrap().max;
        assert!(validate("factor", gain, Some((0.0, maximum + 0.01))).is_err());
        assert!(validate("unknown", gain, Some((0.0, 1.0))).is_err());
    }

    #[test]
    fn hosted_mapping_bounds_remain_normalized_and_finite() {
        assert!(validate("Cutoff", None, Some((0.0, 1.0))).is_ok());
        assert!(validate("Cutoff", None, None).is_ok());
        for bounds in [
            (-0.01, 1.0),
            (0.0, 1.01),
            (f64::NAN, 1.0),
            (0.0, f64::INFINITY),
            (0.8, 0.2),
        ] {
            assert!(validate("Cutoff", None, Some(bounds)).is_err());
        }
    }
}
