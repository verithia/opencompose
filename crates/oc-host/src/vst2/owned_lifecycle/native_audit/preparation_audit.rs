//! Real native state/batch/activation ordering through the complete owner.
use super::*;
use crate::preset_parameters::{ParameterErrorKind, ParameterWriteDomain};
fn row(name: &str, value: Option<f64>) -> Vst2AuthoredInput {
    Vst2AuthoredInput {
        name: name.into(),
        native_value: value,
        domain: ParameterWriteDomain::Normalized,
        claim: Vst2AuthoredClaim::NativeOnly,
    }
}
pub(super) fn audit(fixture: &Fixture, case: &str) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    // SAFETY: This static fixture observer enables deterministic native metadata
    // and phase counters; it never borrows or operates on an instance.
    unsafe { (fixture.preparation_mode)(true) };
    let mut parameters = vec![row("Gain", Some(0.75))];
    if case == "preparation-shadowed" {
        parameters.insert(0, row("#0", None));
    }
    if matches!(
        case,
        "preparation-invalid-final" | "preparation-invalid-close-fault"
    ) {
        parameters.push(row("#0", None));
    }
    let expected_fault = match case {
        "preparation-state-fault" => Some((2, 24, Vst2Operation::Dispatcher(24))),
        "preparation-param-fault" => Some((3, 0, Vst2Operation::SetParameter(0))),
        "preparation-activate-fault" => Some((2, 12, Vst2Operation::Dispatcher(12))),
        "preparation-tail-fault" => Some((
            2,
            abi::plugin_op::GET_TAIL_SAMPLES,
            Vst2Operation::Dispatcher(abi::plugin_op::GET_TAIL_SAMPLES),
        )),
        "preparation-close-fault" | "preparation-invalid-close-fault" => {
            Some((8, 0, Vst2Operation::Close))
        }
        _ => None,
    };
    if let Some((point, opcode, _)) = expected_fault {
        fixture.configure(point, opcode, 1, 0, 0);
    }
    if case == "preparation-state-refused" {
        fixture.configure(2, 24, 3, 0, 0);
    }
    let state = [0.25f32, 0.0]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect();
    let plan = Vst2OwnedAuthoredPlan {
        path: fixture.path.clone(),
        sample_rate: 48_000,
        max_frames: 64,
        state: Some(state),
        parameters,
    };
    let result = if case == "preparation-owned-success" {
        let owned = plan.execute_on_ui()?;
        let snapshot = (*owned.snapshot).clone();
        drop(owned);
        poll_owned_vst2_retirements()?;
        Ok(snapshot)
    } else {
        plan.execute_probe_on_ui()
    };
    // SAFETY: The trusted pinned observer reads only a fixed atomic phase bitset.
    let status = unsafe { (fixture.preparation_status)() };
    let mut first_report_once = true;
    let mut no_later_native_calls = true;
    let exact_first_cause = if let Some((_, _, operation)) = expected_fault {
        let failure = fault(result)?;
        let cause = failure
            .first_fault()
            .ok_or_else(|| Vst2OwnedError::Control("Preparation first cause missing".into()))?;
        first_report_once = failure.take_report() == Some(cause) && failure.take_report().is_none();
        let before = fixture.counts();
        poll_owned_vst2_retirements()?;
        no_later_native_calls = before == fixture.counts();
        cause.operation == operation && cause.fault.kind == FaultKind::CppException
    } else if case == "preparation-invalid-final" {
        matches!(result,Err(Vst2OwnedError::Parameter(error)) if matches!(error.kind,ParameterErrorKind::NonNumeric))
            && fixture.count(3) == 0
            && status == 1
    } else if case == "preparation-state-refused" {
        matches!(result, Err(Vst2OwnedError::Native(_))) && fixture.count(3) == 0 && status == 0
    } else {
        let snapshot = result?;
        snapshot.tail_samples == Some(1860)
            && snapshot.latency_samples == 147
            && snapshot.name == "Prepared Guard Fixture"
            && snapshot.unique_id == 0x4f434e47
            && snapshot.accepts_midi
            && snapshot.parameters.first().is_some_and(|p| p.value == 0.75)
            && snapshot
                .surface
                .parameters()
                .first()
                .is_some_and(|p| p.current == 0.75)
            && status == 15
            && fixture.count(3) == 1
    };
    let faulted = expected_fault.is_some();
    let close_fault = matches!(
        case,
        "preparation-close-fault" | "preparation-invalid-close-fault"
    );
    let (occupied, capacity) = callback_route::occupied_budget_for_audit();
    let payload_retained = if faulted {
        fixture.live() == 1
            && occupied == 1
            && capacity == 1024
            && fixture.count(8) == u64::from(close_fault)
    } else {
        fixture.live() == 0 && occupied == 0 && fixture.count(8) == 1
    };
    let loans_retained = if case == "preparation-state-fault" {
        // SAFETY: The trusted observer reads only the host's retained state copy,
        // after the complete request's original input Vec has been dropped.
        (unsafe { (fixture.touch)() }) > 0
    } else {
        true
    };
    Ok(Vst2NativeGuardAudit {
        exact_first_cause,
        first_report_once,
        no_later_native_calls,
        payload_retained,
        loans_retained,
        process_heap: Vst2HeapCounters::default(),
        slot_isolation: fixture.count(5) == 0 && fixture.count(6) == 0,
    })
}
