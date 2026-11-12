//! Complete UI preparation shared by metadata probes and processor publication.
use super::*;
use crate::PluginFormat;
use crate::preset_parameters::{
    AuthoredParameterOwner, OwnedAuthoredParameter, ParameterApplyError, ParameterErrorKind,
    ParameterWriteDomain,
};

/// Largest authored parameter count accepted.
pub const MAX_VST3_AUTHORED_PARAMETERS: usize = 4096;
/// Largest single parameter name.
pub const MAX_VST3_AUTHORED_NAME_BYTES: usize = 1024;
/// Largest total parameter-name bytes.
pub const MAX_VST3_AUTHORED_TOTAL_NAME_BYTES: usize = 1024 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// An external claim is evidence from actual engine value acceptance, rather
/// than key classification. A native/external collision requires disambiguation.
#[derive(Debug, Clone)]
pub struct Vst3AuthoredInput {
    pub name: String,
    pub native_value: Option<f64>,
    pub domain: ParameterWriteDomain,
    pub external_claim: bool,
}
/// Plan to load a VST3 instance with state and parameters.
pub struct Vst3OwnedAuthoredPlan {
    pub path: std::path::PathBuf,
    pub sample_rate: u32,
    pub max_frames: u32,
    pub component_state: Option<Vec<u8>>,
    pub parameters: Vec<Vst3AuthoredInput>,
    pub budget: Vst3ProcessingBudget,
}
impl Vst3OwnedAuthoredPlan {
    /// Publishes processing/control leases only after complete UI-owned state,
    /// authored-parameter, activation and authoritative metadata preparation.
    pub fn execute_on_ui(self) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
        self.dispatch_on_ui()?.wait()
    }

    /// Queued work is cancelled when this job drops before admission. After
    /// admission, undelivered leases retire through the actual UI registry.
    pub fn dispatch_on_ui(self) -> Result<Vst3OwnedLoadJob, Vst3OwnedError> {
        validate_authored_ingress(&self.parameters, self.budget)?;
        dispatch_owned_job(move || {
            let token = application_main::ui_authority()?;
            let budget = self.budget;
            let prepared = prepare(
                &token,
                self.path,
                self.sample_rate,
                self.max_frames,
                self.component_state,
                InitialParameters::Authored(self.parameters),
            )?;
            publish_runtime(&token, prepared, budget)
        })
        .map(|job| Vst3OwnedLoadJob { job })
    }

    /// Returns detached metadata only after UI-thread native disposal succeeds.
    /// This probe does not process a warmup block or open an audio device.
    pub fn execute_probe_on_ui(self) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
        validate_authored_ingress(&self.parameters, self.budget)?;
        dispatch_owned(move || {
            let token = application_main::ui_authority()?;
            let prepared = prepare(
                &token,
                self.path,
                self.sample_rate,
                self.max_frames,
                self.component_state,
                InitialParameters::Authored(self.parameters),
            )?;
            retire_probe(prepared)
        })
    }
}
/// Cancellation and result ownership use the application's existing control job.
pub struct Vst3OwnedLoadJob {
    job: application_main::ApplicationControlJob<Result<Vst3OwnedInstance, Vst3OwnedError>>,
}
impl Vst3OwnedLoadJob {
    /// Wait from an unlocked control worker while the registered UI keeps pumping.
    pub fn wait(self) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
        self.job.wait().map_err(Vst3OwnedError::Control)?
    }
}

pub(super) enum InitialParameters {
    Normalized(Vec<Vst3NormalizedParameter>),
    Authored(Vec<Vst3AuthoredInput>),
}
pub(super) struct PreparedCandidate {
    pub candidate: registry::Candidate,
    pub snapshot: Vst3OwnedSnapshot,
}
fn validate_authored_ingress(
    rows: &[Vst3AuthoredInput],
    budget: Vst3ProcessingBudget,
) -> Result<(), Vst3OwnedError> {
    budget.validate()?;
    if rows.len() > MAX_VST3_AUTHORED_PARAMETERS || rows.len() > budget.events {
        return Err(Vst3OwnedError::InvalidRequest(
            "Authored VST3 assignment budget exceeded",
        ));
    }
    let mut total = 0usize;
    for row in rows {
        if row.name.len() > MAX_VST3_AUTHORED_NAME_BYTES {
            return Err(Vst3OwnedError::InvalidRequest(
                "Authored VST3 parameter name exceeds its byte limit",
            ));
        }
        total = total
            .checked_add(row.name.len())
            .ok_or(Vst3OwnedError::InvalidRequest(
                "Authored VST3 name budget overflow",
            ))?;
        if total > MAX_VST3_AUTHORED_TOTAL_NAME_BYTES {
            return Err(Vst3OwnedError::InvalidRequest(
                "Authored VST3 names exceed their total byte limit",
            ));
        }
    }
    Ok(())
}
fn apply_authored(
    instance: &mut Vst3Instance,
    rows: Vec<Vst3AuthoredInput>,
) -> Result<(), Vst3OwnedError> {
    let mut owner = instance.authored_parameter_owner().map_err(|kind| {
        Vst3OwnedError::Authored(Box::new(ParameterApplyError {
            format: PluginFormat::Vst3,
            parameter: "<batch>".into(),
            kind,
        }))
    })?;
    let mut native = Vec::with_capacity(rows.len());
    for row in rows {
        let fail = |kind| {
            Vst3OwnedError::Authored(Box::new(ParameterApplyError {
                format: PluginFormat::Vst3,
                parameter: row.name.clone(),
                kind,
            }))
        };
        match (
            owner.matches_native(&row.name).map_err(fail)?,
            row.external_claim,
        ) {
            (true, true) => return Err(fail(ParameterErrorKind::Ambiguous)),
            (false, false) => return Err(fail(ParameterErrorKind::Unknown)),
            (false, true) => {}
            (true, false) => native.push(OwnedAuthoredParameter {
                name: row.name,
                value: row.native_value,
                domain: row.domain,
            }),
        }
    }
    owner
        .apply_owned_authored_parameters(&native)
        .map_err(|error| Vst3OwnedError::Authored(Box::new(error)))
}
pub(super) fn prepare(
    token: &UiThreadToken<'_>,
    path: std::path::PathBuf,
    sample_rate: u32,
    max_frames: u32,
    state: Option<Vec<u8>>,
    initial: InitialParameters,
) -> Result<PreparedCandidate, Vst3OwnedError> {
    application_main::ui_authority()?;
    registry::check_capacity(token)?;
    if sample_rate == 0 || max_frames == 0 || max_frames > 16384 {
        return Err(Vst3OwnedError::InvalidRequest(
            "Invalid VST3 processing shape",
        ));
    }
    if state
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_STATE_BYTES)
    {
        return Err(Vst3OwnedError::InvalidRequest(
            "VST3 component state exceeds 64 MiB",
        ));
    }
    let failure = Arc::new(Vst3Failure::default());
    let mut slot = registry::reserve(token, Arc::clone(&failure))?;
    let native = load_inactive(&path, sample_rate, max_frames, &failure)?;
    // Test acquisition constructs its own retained foreign references. Reserve
    // before acquisition, then bind quota to the actual owner's fault identity.
    slot.failure = native.failure();
    let candidate = registry::Candidate {
        native: Box::new(native),
        slot,
    };
    #[cfg(any(test, feature = "test-support"))]
    let candidate = {
        let mut candidate = candidate;
        if candidate.native.active || candidate.native.processing {
            checked_inactive(&mut candidate.native)?;
        }
        candidate
    };
    complete_candidate(token, candidate, state, initial)
}
pub(super) fn complete_candidate(
    token: &UiThreadToken<'_>,
    mut candidate: registry::Candidate,
    state: Option<Vec<u8>>,
    initial: InitialParameters,
) -> Result<PreparedCandidate, Vst3OwnedError> {
    application_main::ui_authority()?;
    let failure = candidate.native.failure();
    if let Some(state) = state {
        candidate
            .native
            .set_state_bytes(&state)
            .map_err(|message| native_or_message(&failure, message))?;
    }
    match initial {
        InitialParameters::Normalized(values) => {
            let before = metadata::capture(token, &candidate.native, 0)?;
            metadata::preflight(&before, &values)?;
            admit_normalized_verification(before.parameters.len(), values.len())?;
            metadata::apply_initial(token, &mut candidate.native, before, &values)?;
        }
        InitialParameters::Authored(rows) => apply_authored(&mut candidate.native, rows)?,
    }
    activation::activate(token, &mut candidate.native)?;
    let snapshot = metadata::capture(token, &candidate.native, 0)?;
    Ok(PreparedCandidate {
        candidate,
        snapshot,
    })
}
fn admit_normalized_verification(parameters: usize, writes: usize) -> Result<(), Vst3OwnedError> {
    // apply_initial includes its first capture and each post-write capture;
    // complete preparation additionally captures after checked activation.
    let writes_with_final = writes.checked_add(1).ok_or(Vst3OwnedError::InvalidRequest(
        "Initial VST3 metadata verification exceeds its work budget",
    ))?;
    metadata::initial_verification_queries(parameters, writes_with_final).map(|_| ())
}
fn load_inactive(
    path: &std::path::Path,
    sample_rate: u32,
    max_frames: u32,
    failure: &Arc<Vst3Failure>,
) -> Result<Vst3Instance, Vst3OwnedError> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(mut instance) = path
        .to_str()
        .and_then(|id| super::super::test_support::fake_parameter_instance_for_id(id, max_frames))
    {
        // Injection is post-authority/acquisition only. Exercise the same actual
        // inactive/state/parameter/activation/final-metadata/disposal pipeline.
        instance.sample_rate = sample_rate as f64;
        return Ok(instance);
    }
    Vst3Instance::load_with_context_inner(
        path,
        sample_rate,
        max_frames,
        Vst3LoadContext {
            host_context: ptr::null_mut(),
            component_handler: ptr::null_mut(),
            transfer_initial_state: true,
            activate_on_load: false,
            failure: Arc::clone(failure),
        },
    )
    .map_err(|message| native_or_message(failure, message))
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn checked_inactive(instance: &mut Vst3Instance) -> Result<(), Vst3OwnedError> {
    application_main::ui_authority()?;
    // SAFETY: the actual UI owns the newly acquired, healthy synthetic instance.
    let status =
        unsafe { vst3_guard::processing(instance.processor.as_ptr(), false) }.map_err(|fault| {
            Vst3OwnedError::Native(Box::new(
                instance.native_fault(Operation::SetProcessing, fault),
            ))
        })?;
    if status != kResultOk {
        return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
            Operation::SetProcessing,
            status,
        ))));
    }
    instance.processing = false;
    // SAFETY: processing stopped and UI ownership still retains the component.
    let status =
        unsafe { vst3_guard::active(instance.component.as_ptr(), false) }.map_err(|fault| {
            Vst3OwnedError::Native(Box::new(instance.native_fault(Operation::SetActive, fault)))
        })?;
    if status != kResultOk {
        return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
            Operation::SetActive,
            status,
        ))));
    }
    instance.active = false;
    Ok(())
}

pub(super) fn publish_runtime(
    token: &UiThreadToken<'_>,
    prepared: PreparedCandidate,
    budget: Vst3ProcessingBudget,
) -> Result<Vst3OwnedInstance, Vst3OwnedError> {
    registry::adopt_prepared(token, prepared.candidate, prepared.snapshot, budget)
}

pub(super) fn retire_probe(
    prepared: PreparedCandidate,
) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
    application_main::ui_authority()?;
    let failure = prepared.candidate.native.failure();
    let snapshot = prepared.snapshot;
    // Native-before-slot drop order retains quota on cleanup failure.
    drop(prepared.candidate);
    if let Some(error) = failure.first_fault() {
        return Err(Vst3OwnedError::Native(Box::new(error)));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_ingress_bounds_preserve_raw_values_until_binding() {
        let row = |name: String| Vst3AuthoredInput {
            name,
            native_value: None,
            domain: ParameterWriteDomain::LegacyVst3Auto,
            external_claim: false,
        };
        let budget = Vst3ProcessingBudget {
            events: 65536,
            points_per_parameter: 1024,
        };
        assert!(
            validate_authored_ingress(&vec![row("x".into()); MAX_VST3_AUTHORED_PARAMETERS], budget)
                .is_ok()
        );
        assert!(
            validate_authored_ingress(
                &vec![row("x".into()); MAX_VST3_AUTHORED_PARAMETERS + 1],
                budget
            )
            .is_err()
        );
        assert!(
            validate_authored_ingress(&[row("x".repeat(MAX_VST3_AUTHORED_NAME_BYTES))], budget)
                .is_ok()
        );
        assert!(
            validate_authored_ingress(&[row("x".repeat(MAX_VST3_AUTHORED_NAME_BYTES + 1))], budget)
                .is_err()
        );
        let name = row("x".repeat(MAX_VST3_AUTHORED_NAME_BYTES));
        assert!(validate_authored_ingress(&vec![name.clone(); 1024], budget).is_ok());
        assert!(validate_authored_ingress(&vec![name; 1025], budget).is_err());
    }
    #[test]
    fn normalized_verification_includes_final_activation_capture() {
        let limit = metadata::MAX_INITIAL_METADATA_QUERIES;
        assert!(admit_normalized_verification(limit / 2 - 3, 0).is_ok());
        assert!(admit_normalized_verification(limit / 2 - 2, 0).is_err());
        assert!(admit_normalized_verification(0, usize::MAX).is_err());
        // The initial-only reader would allow this combination; the complete
        // owner refuses the additional activation capture before any setter.
        assert!(metadata::initial_verification_queries(limit / 2 - 3, 1).is_ok());
        assert!(admit_normalized_verification(limit / 2 - 3, 1).is_err());
    }
    #[test]
    fn authored_verification_work_has_checked_exact_boundary() {
        // N=1, one row: six captures at four entries plus three conversions.
        assert_eq!(metadata::authored_verification_queries(1, 1).unwrap(), 27);
        let limit = metadata::MAX_INITIAL_METADATA_QUERIES;
        assert_eq!(
            metadata::authored_verification_queries(limit / 2 - 3, 0).unwrap(),
            limit
        );
        assert!(metadata::authored_verification_queries(limit / 2 - 2, 0).is_err());
        assert!(metadata::authored_verification_queries(usize::MAX, 0).is_err());
        assert!(metadata::authored_verification_queries(0, usize::MAX).is_err());
    }
}
