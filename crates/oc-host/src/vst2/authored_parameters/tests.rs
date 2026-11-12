use super::*;
use crate::preset_parameters::{AuthoredParameter, ParameterApplyError};
fn required_fixture() -> Vst2Instance {
    let path = crate::vst2::test_support::fixture_path("OpenCompose Gain Fixture").expect(
        "host build.rs must install the controlled VST2 gain fixture; this test never skips",
    );
    Vst2Instance::load(&path, 48_000, 64)
        .expect("controlled native fixture must load and execute its callbacks")
}
fn control(instance: &Vst2Instance, opcode: i32, value: isize) -> isize {
    instance.dispatch(opcode, 0, value, std::ptr::null_mut())
}
fn calls(instance: &Vst2Instance) -> isize {
    control(instance, 10002, 0)
}
fn item(name: &str, value: f64) -> AuthoredParameter<'_> {
    AuthoredParameter {
        name,
        value,
        domain: ParameterWriteDomain::Normalized,
    }
}
fn apply(
    instance: &mut Vst2Instance,
    items: &[AuthoredParameter<'_>],
) -> Result<(), ParameterApplyError> {
    let mut owner = instance
        .authored_parameter_owner()
        .map_err(|kind| ParameterApplyError {
            format: crate::PluginFormat::Vst2,
            parameter: "<inventory>".into(),
            kind,
        })?;
    owner.apply_authored_parameters(items)
}
#[test]
fn checked_gain_and_dc_assignments_execute_native_callbacks_and_change_pcm() {
    let mut instance = required_fixture();
    assert_eq!(calls(&instance), 0);
    apply(&mut instance, &[item("Gain", 0.75), item("dc_offset", 0.5)]).unwrap();
    assert_eq!(calls(&instance), 2, "real generated setters must run");
    assert_eq!(instance.read_param(0), 0.75);
    assert_eq!(instance.read_param(1), 0.5);
    let input = vec![0.25, -0.5, 1.0, -1.0, 0.0, 0.1, -0.25, 0.5];
    let mut pcm = input.clone();
    instance
        .process_effect_into(&mut pcm, 48_000, None)
        .unwrap();
    for (actual, input) in pcm.iter().zip(input) {
        assert!(
            (*actual - (input * 1.5 + 0.125)).abs() < 1e-7,
            "checked native gain/DC must affect actual PCM"
        );
    }
}
#[test]
fn index_and_legacy_aliases_share_last_native_assignment_and_f32_truth() {
    let mut instance = required_fixture();
    apply(
        &mut instance,
        &[
            item("Gain", f64::NAN),
            item("#0", 0.123_456_789),
            item("DC Offset", 0.4),
            item("d-c_offset", 0.25),
        ],
    )
    .unwrap();
    assert_eq!(calls(&instance), 2);
    assert_eq!(instance.read_param(0), 0.123_456_789_f64 as f32);
    assert_eq!(
        instance.get_param_by_name("Gain"),
        Some(f64::from(0.123_456_789_f64 as f32))
    );
    assert_eq!(instance.read_param(1), 0.25);
}
#[test]
fn every_later_invalid_assignment_prevents_the_first_native_write() {
    for assignment in [
        item("missing", 0.2),
        item("#99", 0.2),
        item("Gain", f64::NAN),
        item("Gain", f64::INFINITY),
        item("Gain", -0.01),
        item("Gain", 1.01),
        AuthoredParameter {
            name: "Gain",
            value: 0.2,
            domain: ParameterWriteDomain::NativePlain,
        },
    ] {
        let mut instance = required_fixture();
        let error = apply(&mut instance, &[item("DC Offset", 0.5), assignment]).unwrap_err();
        assert!(matches!(
            error.kind,
            ParameterErrorKind::Unknown
                | ParameterErrorKind::NonFinite
                | ParameterErrorKind::OutOfRange { .. }
                | ParameterErrorKind::UnsupportedDomain
        ));
        assert_eq!(calls(&instance), 0);
        assert_eq!(instance.read_param(1), 0.0);
    }
}
#[test]
fn checked_name_candidates_never_prefer_an_arbitrary_exact_or_index_match() {
    for (mode, name) in [(1, "Rate Hz"), (1, "rate_hz"), (2, "#0")] {
        let mut instance = required_fixture();
        control(&instance, 10000, mode);
        assert!(matches!(
            apply(&mut instance, &[item(name, 0.2)]).unwrap_err().kind,
            ParameterErrorKind::Ambiguous
        ));
        assert_eq!(calls(&instance), 0);
    }
    let mut unicode = required_fixture();
    control(&unicode, 10000, 3);
    apply(&mut unicode, &[item("明るさ", 0.2), item("亮度", 0.4)]).unwrap();
    assert_eq!(calls(&unicode), 2);
    assert!(matches!(
        apply(&mut unicode, &[item("!!!", 0.2)]).unwrap_err().kind,
        ParameterErrorKind::Unknown
    ));
    let mut empty = required_fixture();
    control(&empty, 10000, 5);
    apply(&mut empty, &[item("Param 0", 0.1), item("#0", 0.3)]).unwrap();
    assert_eq!(calls(&empty), 1);
    assert_eq!(empty.read_param(0), 0.3);
}
#[test]
fn nonautomatable_parameters_remain_statically_writable() {
    let mut instance = required_fixture();
    control(&instance, 10000, 4);
    assert_eq!(
        instance.dispatch(op::CAN_BE_AUTOMATED, 0, 0, std::ptr::null_mut()),
        0
    );
    apply(&mut instance, &[item("Gain", 0.2), item("DC Offset", 0.1)]).unwrap();
    assert_eq!(calls(&instance), 2);
}
#[test]
fn missing_callback_or_unsupported_full_inventory_refuses_before_writes() {
    for (mode, missing) in [(6, false), (7, true)] {
        let mut instance = required_fixture();
        control(&instance, 10000, mode);
        let error = match instance.authored_parameter_owner() {
            Err(error) => error,
            Ok(_) => panic!("unusable native authority admitted"),
        };
        if missing {
            assert!(matches!(error, ParameterErrorKind::MissingCallback));
        } else {
            assert!(matches!(error, ParameterErrorKind::InvalidMetadata));
        }
        assert_eq!(calls(&instance), 0);
    }
}
#[test]
fn native_callback_changes_stop_the_next_write_without_publishing_stale_values() {
    for action in 1..=5 {
        let mut instance = required_fixture();
        control(&instance, 10001, action);
        let error =
            apply(&mut instance, &[item("Gain", 0.75), item("DC Offset", 0.5)]).unwrap_err();
        assert!(matches!(
            error.kind,
            ParameterErrorKind::MissingCallback | ParameterErrorKind::InventoryChanged
        ));
        assert_eq!(calls(&instance), 1);
        assert_eq!(instance.read_param(0), 0.75);
        assert_eq!(instance.read_param(1), 0.0);
        assert_eq!(instance.host.value(0), Some(0.5));
    }
    let mut metadata = required_fixture();
    control(&metadata, 10000, 9);
    assert!(matches!(
        apply(&mut metadata, &[item("Gain", 0.2)]).unwrap_err().kind,
        ParameterErrorKind::InventoryChanged
    ));
    assert_eq!(calls(&metadata), 0);
}
#[test]
fn alternate_setter_has_distinct_observable_behavior_and_forwards_actual_values() {
    let mut instance = required_fixture();
    control(&instance, 10001, 5);
    instance.set_param_index(0, 0.75);
    assert_eq!(calls(&instance), 1);
    assert_eq!(control(&instance, 10003, 0), 0);
    // A fresh checked preparation binds the currently published alternate. It
    // invokes both its distinct counter and the original fixture's value setter.
    apply(&mut instance, &[item("Gain", 0.25), item("DC Offset", 0.5)]).unwrap();
    assert_eq!(control(&instance, 10003, 0), 2);
    assert_eq!(calls(&instance), 3);
    assert_eq!(instance.read_param(0), 0.25);
    assert_eq!(instance.read_param(1), 0.5);
}

#[test]
fn prepared_keys_cannot_escape_their_candidate_and_existing_failure_is_preserved() {
    let mut instance = required_fixture();
    let key = {
        let mut owner = instance.authored_parameter_owner().unwrap();
        owner.resolve_authored_parameter("Gain").unwrap().key
    };
    let mut owner = instance.authored_parameter_owner().unwrap();
    assert!(matches!(
        owner.write_bound_parameter(&key, 0.2, ParameterWriteDomain::Normalized),
        Err(ParameterErrorKind::InventoryChanged)
    ));
    drop(owner);
    assert_eq!(calls(&instance), 0);
    instance.process_error = Some(Vst2ProcessError::CapacityExceeded);
    let error = match instance.authored_parameter_owner() {
        Err(error) => error,
        Ok(_) => panic!("failed instance admitted"),
    };
    match error {
        ParameterErrorKind::Native { source, .. } => assert_eq!(
            source.downcast_ref::<Vst2ProcessError>(),
            Some(&Vst2ProcessError::CapacityExceeded)
        ),
        other => panic!("original failure lost: {other}"),
    }
    assert_eq!(calls(&instance), 0);
}
