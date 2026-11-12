//! Actual-main proof of the production candidate preparation and disposal core.
use super::super::native_fixture::NativeFixture;
use super::preparation::{self, InitialParameters, Vst3AuthoredInput};
use super::*;
use crate::preset_parameters::{ParameterErrorKind, ParameterWriteDomain};

#[derive(Debug)]
/// Audit results for authored preparation.
pub struct Vst3AuthoredPreparationAudit {
    pub tail_samples: u32,
    pub latency_samples: u32,
    pub typed_refusals_before_write: usize,
    pub changed_inventory_stopped_later_write: bool,
    pub cleanup_failure_refused_success: bool,
    pub synthetic_probe_used_actual_ui: bool,
}
fn require(value: bool, message: &'static str) -> Result<(), Vst3OwnedError> {
    value
        .then_some(())
        .ok_or(Vst3OwnedError::InvalidRequest(message))
}
fn row(name: &str, value: Option<f64>, external_claim: bool) -> Vst3AuthoredInput {
    Vst3AuthoredInput {
        name: name.into(),
        native_value: value,
        domain: ParameterWriteDomain::LegacyVst3Auto,
        external_claim,
    }
}
pub(super) fn fixture(
    token: &UiThreadToken<'_>,
) -> Result<(NativeFixture, registry::Candidate), Vst3OwnedError> {
    fixture_with_mode(token, 7)
}
pub(super) fn fixture_with_mode(
    token: &UiThreadToken<'_>,
    mode: u32,
) -> Result<(NativeFixture, registry::Candidate), Vst3OwnedError> {
    application_main::ui_authority()?;
    let (mut fixture, result) = NativeFixture::load_with_fault(22, 0);
    result.map_err(Vst3OwnedError::Control)?;
    fixture.configure_parameters_for_test(mode);
    let instance = fixture
        .take_instance_for_preparation()
        .ok_or(Vst3OwnedError::Retired)?;
    let mut candidate = registry::Candidate {
        slot: registry::reserve(token, instance.failure())?,
        native: Box::new(instance),
    };
    preparation::checked_inactive(&mut candidate.native)?;
    Ok((fixture, candidate))
}
/// Call only from a genuine application main bootstrap. No audio or plugin UI.
pub fn audit_owned_authored_preparation() -> Result<Vst3AuthoredPreparationAudit, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let (valid, candidate) = fixture(&token)?;
    let rows = vec![
        row("Gain", None, false),
        row("Hidden Depth", Some(-6.0), false),
        row("Gain Amount", Some(75.0), false),
        row("engine string", None, true),
    ];
    let prepared = preparation::complete_candidate(
        &token,
        candidate,
        Some(0.2f64.to_le_bytes().to_vec()),
        InitialParameters::Authored(rows),
    )?;
    require(
        valid.calls(5) > 0 && valid.calls(41) == 2,
        "State/authored preparation did not reach native entries",
    )?;
    require(
        valid.calls(1) == 0,
        "Metadata preparation silently processed a warmup block",
    )?;
    require(
        valid.parameter_value_for_test(100) == 0.75 && valid.parameter_value_for_test(101) == 0.25,
        "Native authored conversions or alias winners changed",
    )?;
    let snapshot = preparation::retire_probe(prepared)?;
    let tail_samples = snapshot.tail_samples;
    let latency_samples = snapshot.latency_samples;
    require(
        tail_samples == 287 && latency_samples == 106,
        "Final probe metadata ignored authored values or activation",
    )?;
    require(
        valid.refs() == 0,
        "Successful probe did not dispose native ownership on UI",
    )?;
    drop(valid);
    // Numeric callers traverse the same inactive preparation, not a separate
    // activation-first compatibility algorithm.
    let (normalized, candidate) = fixture(&token)?;
    let prepared = preparation::complete_candidate(
        &token,
        candidate,
        None,
        InitialParameters::Normalized(vec![
            Vst3NormalizedParameter {
                id: 100,
                value: 0.5,
            },
            Vst3NormalizedParameter {
                id: 101,
                value: 0.75,
            },
        ]),
    )?;
    let numeric = preparation::retire_probe(prepared)?;
    require(
        numeric.latency_samples == 81
            && numeric.tail_samples == 787
            && normalized.calls(41) == 2
            && normalized.calls(1) == 0,
        "Normalized callers did not use complete inactive preparation",
    )?;
    drop(normalized);
    let mut typed_refusals_before_write = 0;
    for (rows, expected) in [
        (vec![row("Gain", Some(0.5), true)], 0),
        (vec![row("unknown", Some(0.5), false)], 1),
        (
            vec![
                row("Gain", Some(0.5), false),
                row("Gain Amount", None, false),
            ],
            2,
        ),
        (vec![row("Gain", Some(f64::NAN), false)], 3),
        (vec![row("Read Only", Some(0.5), false)], 4),
        (vec![row("Rate Hz", Some(0.5), false)], 0),
    ] {
        let (native, candidate) = fixture(&token)?;
        let result = preparation::complete_candidate(
            &token,
            candidate,
            None,
            InitialParameters::Authored(rows),
        );
        let matched = match result {
            Err(Vst3OwnedError::Authored(error)) => matches!(
                (expected, error.kind),
                (0, ParameterErrorKind::Ambiguous)
                    | (1, ParameterErrorKind::Unknown)
                    | (2, ParameterErrorKind::NonNumeric)
                    | (3, ParameterErrorKind::NonFinite)
                    | (4, ParameterErrorKind::ReadOnly)
            ),
            _ => false,
        };
        require(
            matched && native.calls(41) == 0,
            "Authored preparation lost typed all-winner preflight refusal",
        )?;
        typed_refusals_before_write += 1;
    }
    let (native, candidate) = fixture(&token)?;
    native.preparation_mutation_for_test(1);
    let result = preparation::complete_candidate(
        &token,
        candidate,
        None,
        InitialParameters::Authored(vec![
            row("Gain", Some(0.75), false),
            row("Hidden Depth", Some(0.25), false),
        ]),
    );
    require(
        matches!(result,Err(Vst3OwnedError::Authored(error)) if matches!(error.kind,ParameterErrorKind::InventoryChanged))
            && native.calls(41) == 1,
        "Metadata-only change did not stop second authored write",
    )?;
    drop(native);
    let (native, candidate) = fixture(&token)?;
    let prepared = preparation::complete_candidate(
        &token,
        candidate,
        None,
        InitialParameters::Authored(vec![]),
    )?;
    native.set_fault(10, 1);
    require(
        matches!(
            preparation::retire_probe(prepared),
            Err(Vst3OwnedError::Native(_))
        ),
        "Cleanup fault published a successful tail probe",
    )?;
    // Fixture owner retains poisoned backing; its Drop follows the owning fixture policy.
    drop(native);
    let synthetic = Vst3OwnedAuthoredPlan {
        path: super::super::test_support::FAKE_ONE_BUS_ID.into(),
        sample_rate: 48000,
        max_frames: 512,
        component_state: None,
        parameters: vec![row("Gain", Some(0.75), false)],
        budget: Vst3ProcessingBudget::default(),
    }
    .execute_probe_on_ui()?;
    require(
        synthetic.outputs.len() == 1
            && synthetic.parameters.iter().any(|parameter| {
                parameter.id == super::super::test_support::FAKE_PARAM_ID
                    && parameter.name == "Gain"
            }),
        "Synthetic load bypassed actual production preparation",
    )?;
    Ok(Vst3AuthoredPreparationAudit {
        tail_samples,
        latency_samples,
        typed_refusals_before_write,
        changed_inventory_stopped_later_write: true,
        cleanup_failure_refused_success: true,
        synthetic_probe_used_actual_ui: true,
    })
}
