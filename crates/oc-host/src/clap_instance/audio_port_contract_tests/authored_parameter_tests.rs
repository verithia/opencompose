use super::*;
use crate::preset_parameters::{AuthoredParameter, ParameterErrorKind, ParameterWriteDomain};

fn assignment(name: &str, value: f64) -> AuthoredParameter<'_> {
    AuthoredParameter {
        name,
        value,
        domain: ParameterWriteDomain::NativePlain,
    }
}
fn effect(instance: &ClapInstance) -> &FakeEffect {
    // SAFETY: fake_instance owns this allocation through its retained Core and
    // every test keeps the instance alive while borrowing fixture observations.
    unsafe { fake(instance.plugin) }
}
fn add_parameter(instance: &mut ClapInstance, parameter: FakeParam) {
    // SAFETY: The test holds exclusive idle instance access. No native callback
    // retains a reference to this fixture's resizable metadata collection.
    unsafe { &mut *(instance.plugin as *mut FakeEffect) }
        .params
        .push(parameter);
}

#[test]
fn clap_authored_preflight_refuses_late_invalid_values_without_flushing() {
    let mut instance = fake_instance(vec![2], vec![2]);
    for invalid in [
        assignment("Gain", 2.0),
        assignment("missing", 0.5),
        assignment("Gain", f64::NAN),
    ] {
        assert!(
            instance
                .apply_authored_parameters(&[assignment("Gain", 0.2), invalid])
                .is_err()
        );
        assert_eq!(effect(&instance).flushes.get(), 0);
        assert_eq!(effect(&instance).value.get(), 0.5);
    }
    effect(&instance).reject_info.set(true);
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Gain", 0.2)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::InvalidMetadata
    ));
    assert_eq!(effect(&instance).flushes.get(), 0);
}

#[test]
fn clap_authored_aliases_last_write_wins_and_hidden_static_controls_remain_writable() {
    let mut instance = fake_instance(vec![2], vec![2]);
    add_parameter(
        &mut instance,
        FakeParam {
            id: 8,
            name: "Depth",
            module: "Hidden",
            min: -10.0,
            max: 10.0,
            flags: clap_sys::ext::params::CLAP_PARAM_IS_HIDDEN,
        },
    );
    add_parameter(
        &mut instance,
        FakeParam {
            id: 9,
            name: "Meter",
            module: "",
            min: 0.0,
            max: 1.0,
            flags: clap_sys::ext::params::CLAP_PARAM_IS_READONLY,
        },
    );
    instance
        .apply_authored_parameters(&[
            assignment("Gain", f64::NAN),
            assignment("g-a-i-n", 0.75),
            assignment("Hidden_Depth", -4.0),
        ])
        .unwrap();
    assert_eq!(
        *effect(&instance).authored_writes.borrow(),
        [(7, 0.75), (8, -4.0)]
    );
    assert_eq!(effect(&instance).flushes.get(), 2);
    assert!(
        !instance
            .get_params()
            .iter()
            .any(|parameter| parameter.name == "Depth")
    );
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Meter", 0.5)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::ReadOnly
    ));
    assert_eq!(effect(&instance).flushes.get(), 2);
}

#[test]
fn clap_authored_ambiguous_aliases_and_duplicate_native_ids_are_refused() {
    let mut instance = fake_instance(vec![2], vec![2]);
    add_parameter(
        &mut instance,
        FakeParam {
            id: 8,
            name: "Gain",
            module: "Second",
            min: 0.0,
            max: 1.0,
            flags: 0,
        },
    );
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Gain", 0.25)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::Ambiguous
    ));
    assert_eq!(effect(&instance).flushes.get(), 0);
    instance
        .apply_authored_parameters(&[assignment("Second_Gain", 0.25)])
        .unwrap();
    assert_eq!(*effect(&instance).authored_writes.borrow(), [(8, 0.25)]);
    add_parameter(
        &mut instance,
        FakeParam {
            id: 8,
            name: "Duplicate",
            module: "",
            min: 0.0,
            max: 1.0,
            flags: 0,
        },
    );
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Second_Gain", 0.5)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::InvalidMetadata
    ));
    assert_eq!(effect(&instance).flushes.get(), 1);
}

#[test]
fn clap_authored_native_gain_changes_controlled_pcm() {
    let mut instance = fake_instance(vec![2], vec![2]);
    effect(&instance).gain_audio.set(true);
    let input = vec![0.8f32; 128];
    let before = instance.process_effect_block(&input);
    instance.stop_processing_now();
    instance
        .apply_authored_parameters(&[assignment("Gain", 0.25)])
        .unwrap();
    let after = instance.process_effect_block(&input);
    assert!(before.iter().all(|sample| *sample == 0.4));
    assert!(after.iter().all(|sample| *sample == 0.2));
    assert_eq!(effect(&instance).flushes.get(), 1);
}

#[test]
fn clap_authored_rescan_during_flush_stops_before_the_next_native_write() {
    let mut instance = fake_instance(vec![2], vec![2]);
    add_parameter(
        &mut instance,
        FakeParam {
            id: 8,
            name: "Depth",
            module: "",
            min: 0.0,
            max: 1.0,
            flags: 0,
        },
    );
    effect(&instance).rescan_on_flush.set(true);
    let error = instance
        .apply_authored_parameters(&[assignment("Gain", 0.25), assignment("Depth", 0.5)])
        .unwrap_err();
    assert!(matches!(error.kind, ParameterErrorKind::InventoryChanged));
    assert_eq!(*effect(&instance).authored_writes.borrow(), [(7, 0.25)]);
    assert_eq!(effect(&instance).flushes.get(), 1);
}

#[test]
fn clap_authored_cpp_flush_fault_retains_original_operation_and_payload() {
    use std::error::Error;
    let mut instance = fake_instance(vec![2], vec![2]);
    effect(&instance).throw_on_flush.set(true);
    // SAFETY: The native fixture uses thread-local fault configuration and its
    // reset cannot throw or touch another test's instance.
    unsafe { oc_clap_fixture_reset(16) };
    let error = instance
        .apply_authored_parameters(&[assignment("Gain", 0.25)])
        .unwrap_err();
    let source = error
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<ClapAuthoredParameterFailure>()
        .unwrap();
    let ClapAuthoredParameterFailure::Fault(record) = source else {
        panic!("original native failure missing")
    };
    assert_eq!(record.operation, ClapOperation::ParameterFlush);
    assert_eq!(
        record.fault.kind,
        crate::clap_guard::FaultKind::CppException
    );
    assert!(
        error
            .to_string()
            .contains("optimized CLAP fixture exception")
    );
    // SAFETY: The fixture count/event readers inspect this thread's fixed log.
    let events: Vec<_> = (0..unsafe { oc_clap_fixture_count() })
        .map(|index| {
            // SAFETY: index is below the fixture's current fixed log length.
            unsafe { oc_clap_fixture_event(index) }
        })
        .collect();
    assert_eq!(events, [16]);
    let failed = instance
        .apply_authored_parameters(&[assignment("Gain", 0.5)])
        .unwrap_err();
    assert!(
        failed
            .to_string()
            .contains("optimized CLAP fixture exception")
    );
}

#[test]
fn clap_authored_missing_flush_and_busy_owner_refuse_before_native_writes() {
    let mut instance = fake_instance(vec![2], vec![2]);
    effect(&instance).no_flush.set(true);
    assert!(matches!(
        instance
            .apply_authored_parameters(&[assignment("Gain", 0.25)])
            .unwrap_err()
            .kind,
        ParameterErrorKind::MissingCallback
    ));
    assert_eq!(effect(&instance).flushes.get(), 0);
    effect(&instance).no_flush.set(false);
    instance
        .core
        .host
        .audio_busy
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(
        instance
            .apply_authored_parameters(&[assignment("Gain", 0.25)])
            .is_err()
    );
    assert_eq!(effect(&instance).flushes.get(), 0);
    instance
        .core
        .host
        .audio_busy
        .store(false, std::sync::atomic::Ordering::Release);
}
