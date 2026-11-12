//! Native inventory arbitrates routing claims before any setter or activation.
use super::*;
use crate::preset_parameters::{ParameterErrorKind, ParameterWriteDomain};
fn row(name: &str, value: Option<f64>, claim: Vst2AuthoredClaim) -> Vst2AuthoredInput {
    Vst2AuthoredInput {
        name: name.into(),
        native_value: value,
        domain: ParameterWriteDomain::Normalized,
        claim,
    }
}
pub(super) fn audit(fixture: &Fixture, case: &str) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    use Vst2AuthoredClaim::{InstrumentOutputRouting, NativeOnly};
    let mode = match case {
        "claim-native-collision" | "claim-native-only" => 1,
        "claim-native-ambiguity" => 2,
        "claim-routing-no-setter" => 3,
        "claim-native-no-setter" => 4,
        _ => 0,
    };
    // SAFETY: Pinned static fixture selectors are set before native acquisition;
    // the mode exposes an actual inventory/missing-setter, not a Rust resolver mock.
    unsafe {
        (fixture.claim_mode)(mode);
        (fixture.preparation_mode)(true);
    }
    let rows = match case {
        "claim-native-collision" => vec![
            row("Offset", Some(0.25), NativeOnly),
            row("output_pair", Some(0.0), InstrumentOutputRouting),
        ],
        "claim-native-ambiguity" => vec![row("output_pair", Some(0.0), InstrumentOutputRouting)],
        "claim-native-only" => vec![row("output_pair", Some(0.75), NativeOnly)],
        "claim-invalid-final" => vec![
            row("Gain", Some(0.75), NativeOnly),
            row("output_pair", Some(0.0), InstrumentOutputRouting),
            row("#0", None, NativeOnly),
        ],
        "claim-unknown-profile" => vec![row("sample_root", None, NativeOnly)],
        "claim-reserved-collision" => vec![row("state_file", None, InstrumentOutputRouting)],
        "claim-native-no-setter" => vec![row("Gain", Some(0.75), NativeOnly)],
        _ => vec![row("output_pair", Some(0.0), InstrumentOutputRouting)],
    };
    let result = Vst2OwnedAuthoredPlan {
        path: fixture.path.clone(),
        sample_rate: 48_000,
        max_frames: 64,
        state: None,
        parameters: rows,
    }
    .execute_probe_on_ui();
    let expected = match case {
        "claim-native-collision" | "claim-native-ambiguity" => Some(ParameterErrorKind::Ambiguous),
        "claim-invalid-final" => Some(ParameterErrorKind::NonNumeric),
        "claim-unknown-profile" => Some(ParameterErrorKind::Unknown),
        "claim-reserved-collision" => Some(ParameterErrorKind::ReservedNameCollision),
        "claim-native-no-setter" => Some(ParameterErrorKind::MissingCallback),
        _ => None,
    };
    // SAFETY: Read only the bounded static phase bitset after synchronous UI retirement.
    let phase = unsafe { (fixture.preparation_status)() };
    let exact_first_cause = if let Some(expected) = expected {
        let matches = match result {
            Err(Vst2OwnedError::Parameter(error)) => {
                std::mem::discriminant(&error.kind) == std::mem::discriminant(&expected)
            }
            _ => false,
        };
        matches && fixture.count(3) == 0 && phase == 0
    } else {
        let snapshot = result?;
        let native = case == "claim-native-only";
        snapshot.tail_samples == Some(if native { 1760 } else { 1510 })
            && snapshot.latency_samples == if native { 83 } else { 58 }
            && fixture.count(3) == u64::from(native)
            && phase == 4
            && if case == "claim-routing-no-setter" {
                snapshot.parameters.is_empty()
            } else {
                !snapshot.parameters.is_empty()
            }
    };
    Ok(Vst2NativeGuardAudit {
        exact_first_cause,
        first_report_once: true,
        no_later_native_calls: fixture.count(5) == 0 && fixture.count(6) == 0,
        payload_retained: fixture.live() == 0
            && fixture.count(8) == 1
            && callback_route::occupied_budget_for_audit() == (0, 1024),
        loans_retained: true,
        process_heap: Vst2HeapCounters::default(),
        slot_isolation: true,
    })
}
