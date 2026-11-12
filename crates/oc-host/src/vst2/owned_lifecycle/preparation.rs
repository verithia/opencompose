//! Complete inactive state/batch preparation and detached UI-owned probes.
use super::*;
use crate::application_main::{UiThreadToken, ui_retirement_authority};
use crate::preset_parameters::{OwnedAuthoredParameter, ParameterApplyError, ParameterErrorKind};

/// Largest authored parameter count accepted.
pub const MAX_VST2_AUTHORED_PARAMETERS: usize = MAX_REPORTED_PARAMS as usize;
/// Largest single parameter name.
pub const MAX_VST2_AUTHORED_NAME_BYTES: usize = 1024;
/// Largest total parameter-name bytes.
pub const MAX_VST2_AUTHORED_TOTAL_NAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which native state a parameter write claims.
pub enum Vst2AuthoredClaim {
    NativeOnly,
    InstrumentOutputRouting,
}
#[derive(Debug, Clone)]
/// One authored parameter to apply at load.
pub struct Vst2AuthoredInput {
    pub name: String,
    pub native_value: Option<f64>,
    pub domain: crate::preset_parameters::ParameterWriteDomain,
    pub claim: Vst2AuthoredClaim,
}
impl From<OwnedAuthoredParameter> for Vst2AuthoredInput {
    fn from(row: OwnedAuthoredParameter) -> Self {
        Self {
            name: row.name,
            native_value: row.value,
            domain: row.domain,
            claim: Vst2AuthoredClaim::NativeOnly,
        }
    }
}

/// Plan to load a VST2 instance with state and parameters.
pub struct Vst2OwnedAuthoredPlan {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub max_frames: u32,
    pub state: Option<Vec<u8>>,
    pub parameters: Vec<Vst2AuthoredInput>,
}
impl Vst2OwnedAuthoredPlan {
    /// Load and adopt the prepared instance on the UI thread.
    pub fn execute_on_ui(self) -> Result<Vst2OwnedInstance, Vst2OwnedError> {
        self.validate()?;
        dispatch(move || {
            let token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let source = registry::reserve(&token)?;
            let native = self.prepare(&token)?;
            registry::adopt(&token, source, native)
        })?
        .wait()
        .map_err(Vst2OwnedError::Control)?
    }
    /// Load, capture metadata, and retire the probe on the UI thread.
    pub fn execute_probe_on_ui(self) -> Result<Vst2OwnedSnapshot, Vst2OwnedError> {
        self.validate()?;
        dispatch(move || {
            let token = application_main::ui_authority()?;
            let _phase = Phase::enter()?;
            let _source = registry::reserve(&token)?;
            let mut native = self.prepare(&token)?;
            let snapshot = match capture_snapshot(&mut native) {
                Ok(snapshot) => snapshot,
                Err(error) => return Err(retire_error(native, error)),
            };
            let retirement = ui_retirement_authority()?;
            native
                .retire(&retirement)
                .map_err(Vst2OwnedError::Faulted)?;
            // The closed candidate is dropped on UI before plain metadata escapes.
            drop(native);
            Ok(snapshot)
        })?
        .wait()
        .map_err(Vst2OwnedError::Control)?
    }
    pub(super) fn validate(&self) -> Result<(), Vst2OwnedError> {
        if self.sample_rate == 0 {
            return Err(Vst2OwnedError::Native(
                "VST2 sample rate must be positive".into(),
            ));
        }
        checked_block_frames(self.max_frames as usize).map_err(Vst2OwnedError::Preparation)?;
        if let Some(state) = &self.state {
            checked_state_chunk_size(state.len()).map_err(Vst2OwnedError::Native)?;
        }
        if self.parameters.len() > MAX_VST2_AUTHORED_PARAMETERS {
            return Err(Vst2OwnedError::Capacity);
        }
        let mut bytes = 0usize;
        for row in &self.parameters {
            if row.name.len() > MAX_VST2_AUTHORED_NAME_BYTES {
                return Err(Vst2OwnedError::Capacity);
            }
            bytes = bytes
                .checked_add(row.name.len())
                .filter(|sum| *sum <= MAX_VST2_AUTHORED_TOTAL_NAME_BYTES)
                .ok_or(Vst2OwnedError::Capacity)?;
        }
        Ok(())
    }
    pub(super) fn prepare(
        self,
        token: &UiThreadToken<'_>,
    ) -> Result<candidate::OwnedCandidate, Vst2OwnedError> {
        let mut native = Vst2Instance::load_owned_mode(
            token,
            &self.path,
            self.sample_rate,
            self.max_frames,
            LoadActivation::Inactive,
        )?;
        let prepared = (|| {
            if let Some(bytes) = self.state {
                let result = native.set_state_bytes(&bytes);
                check_native(&native)?;
                result.map_err(Vst2OwnedError::Native)?;
            }
            // State may request a new routing/latency layout. Accept it only through
            // the existing off-thread preparation owner before binding static writes.
            native
                .prepare_block_buffers(self.max_frames as usize)
                .map_err(Vst2OwnedError::Preparation)?;
            check_native(&native)?;
            if !self.parameters.is_empty() {
                let result = native
                    .claimed_parameter_owner()
                    .map_err(|kind| parameter_error("<batch>", kind))
                    .and_then(|mut owner| {
                        owner
                            .apply_claimed_parameters(&self.parameters)
                            .map_err(|error| Vst2OwnedError::Parameter(Box::new(error)))
                    });
                check_native(&native)?;
                result?;
            }
            native.activate();
            check_native(&native)?;
            Ok(())
        })();
        if let Err(error) = prepared {
            return Err(retire_error(native, error));
        }
        Ok(native)
    }
}
pub(super) fn retire_error(
    mut native: candidate::OwnedCandidate,
    original: Vst2OwnedError,
) -> Vst2OwnedError {
    if let Ok(token) = ui_retirement_authority()
        && let Err(failure) = native.retire(&token)
    {
        return Vst2OwnedError::Faulted(failure);
    }
    original
}
fn parameter_error(name: &str, kind: ParameterErrorKind) -> Vst2OwnedError {
    Vst2OwnedError::Parameter(Box::new(ParameterApplyError {
        format: crate::PluginFormat::Vst2,
        parameter: name.into(),
        kind,
    }))
}
fn check_native(native: &Vst2Instance) -> Result<(), Vst2OwnedError> {
    if native.host.failure.failed() {
        return Err(Vst2OwnedError::Faulted(native.host.failure.clone()));
    }
    native
        .check_callback_route()
        .map_err(Vst2OwnedError::Process)
}

pub(super) fn capture_snapshot(
    native: &mut Vst2Instance,
) -> Result<Vst2OwnedSnapshot, Vst2OwnedError> {
    check_native(native)?;
    // SAFETY: The exclusive UI candidate retains its admitted descriptor. No
    // reference spans a native call, and every callback result is checked first.
    let (count, unique_id) = unsafe { ((*native.effect).num_params, (*native.effect).unique_id) };
    if count != native.num_params || !(0..=MAX_REPORTED_PARAMS).contains(&count) {
        return Err(parameter_error(
            "<inventory>",
            ParameterErrorKind::InventoryChanged,
        ));
    }
    native.refresh_io().map_err(Vst2OwnedError::Preparation)?;
    native
        .prepare_block_buffers(native.max_block_frames as usize)
        .map_err(Vst2OwnedError::Preparation)?;
    let revision = native
        .host
        .authored_inventory_revision
        .load(Ordering::Acquire);
    native.read_param_metadata();
    check_native(native)?;
    let name = native.query_string(op::GET_EFFECT_NAME);
    let vendor = native.query_string(op::GET_VENDOR_STRING);
    let product = native.query_string(op::GET_PRODUCT_STRING);
    let version = native.dispatch(op::GET_VENDOR_VERSION, 0, 0, std::ptr::null_mut()) as i32;
    let mut category = category_from_plug_category(native.dispatch(
        op::GET_PLUG_CATEGORY,
        0,
        0,
        std::ptr::null_mut(),
    ) as i32);
    if native.flags & abi::FLAG_IS_SYNTH != 0 {
        category = PluginCategory::Instrument;
    }
    let accepts_midi = native.can_do(abi::can_do::RECEIVE_VST_MIDI_EVENT)
        || native.can_do(abi::can_do::RECEIVE_VST_EVENTS);
    let parameters = native.get_params();
    let surface = native.parameter_surface();
    let tail_samples = native.plugin_tail_samples();
    check_native(native)?;
    // SAFETY: All preceding native calls returned healthy; the same exclusive
    // candidate still owns this descriptor until checked retirement.
    let (final_count, final_inputs, final_outputs, final_flags) = unsafe {
        (
            (*native.effect).num_params,
            (*native.effect).num_inputs,
            (*native.effect).num_outputs,
            (*native.effect).flags,
        )
    };
    if final_count != count
        || final_inputs != native.num_inputs
        || final_outputs != native.num_outputs
        || final_flags != native.flags
        || native.host.io_changed.load(Ordering::Acquire)
        || native
            .host
            .authored_inventory_revision
            .load(Ordering::Acquire)
            != revision
    {
        return Err(parameter_error(
            "<inventory>",
            ParameterErrorKind::InventoryChanged,
        ));
    }
    // Latency may change without an IO_CHANGED notification during activation.
    // SAFETY: The healthy descriptor is retained by the exclusive UI candidate.
    let latency_samples = unsafe { (*native.effect).initial_delay }.max(0) as u32;
    native
        .host
        .latency
        .store(latency_samples, Ordering::Relaxed);
    native.accepts_midi = accepts_midi;
    Ok(Vst2OwnedSnapshot {
        path: native.path.clone(),
        unique_id,
        name: if name.is_empty() {
            native.name.clone()
        } else {
            name
        },
        vendor,
        product,
        version,
        category,
        accepts_midi,
        tail_samples,
        surface,
        parameters,
        inputs: native.input_ports(),
        outputs: native.output_ports(),
        sample_rate: native.sample_rate,
        max_frames: native.max_block_frames,
        latency_samples,
        inventory_revision: revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset_parameters::ParameterWriteDomain;
    fn plan() -> Vst2OwnedAuthoredPlan {
        Vst2OwnedAuthoredPlan {
            path: PathBuf::from("must-not-be-loaded"),
            sample_rate: 48_000,
            max_frames: 64,
            state: None,
            parameters: Vec::new(),
        }
    }
    #[test]
    fn ingress_refuses_limits_before_runtime_or_module_lookup() {
        let mut value = plan();
        value.sample_rate = 0;
        assert!(matches!(
            value.execute_probe_on_ui(),
            Err(Vst2OwnedError::Native(_))
        ));
        let mut value = plan();
        value.max_frames = u32::MAX;
        assert!(matches!(
            value.execute_on_ui(),
            Err(Vst2OwnedError::Preparation(_))
        ));
        let row = Vst2AuthoredInput {
            name: "Gain".into(),
            native_value: Some(0.5),
            domain: ParameterWriteDomain::Normalized,
            claim: Vst2AuthoredClaim::NativeOnly,
        };
        let mut value = plan();
        value.parameters = vec![row.clone(); MAX_VST2_AUTHORED_PARAMETERS + 1];
        assert!(matches!(
            value.execute_probe_on_ui(),
            Err(Vst2OwnedError::Capacity)
        ));
        let mut value = plan();
        value.parameters = vec![Vst2AuthoredInput {
            name: "x".repeat(MAX_VST2_AUTHORED_NAME_BYTES + 1),
            ..row.clone()
        }];
        assert!(matches!(
            value.execute_on_ui(),
            Err(Vst2OwnedError::Capacity)
        ));
        let mut value = plan();
        value.parameters = vec![
            Vst2AuthoredInput {
                name: "x".repeat(MAX_VST2_AUTHORED_NAME_BYTES),
                ..row
            };
            1025
        ];
        assert!(matches!(
            value.execute_probe_on_ui(),
            Err(Vst2OwnedError::Capacity)
        ));
    }
    #[test]
    fn shadowed_invalid_values_are_left_to_canonical_batch_validation() {
        let mut value = plan();
        value.parameters = vec![
            Vst2AuthoredInput {
                name: "Gain".into(),
                native_value: None,
                domain: ParameterWriteDomain::Normalized,
                claim: Vst2AuthoredClaim::NativeOnly,
            },
            Vst2AuthoredInput {
                name: "#0".into(),
                native_value: Some(0.7),
                domain: ParameterWriteDomain::Normalized,
                claim: Vst2AuthoredClaim::NativeOnly,
            },
        ];
        assert!(value.validate().is_ok());
    }
    #[test]
    fn raw_authored_conversion_never_grants_external_routing_ownership() {
        let row = Vst2AuthoredInput::from(OwnedAuthoredParameter {
            name: "output_pair".into(),
            value: None,
            domain: ParameterWriteDomain::Normalized,
        });
        assert_eq!(row.name, "output_pair");
        assert_eq!(row.native_value, None);
        assert_eq!(row.domain, ParameterWriteDomain::Normalized);
        assert_eq!(row.claim, Vst2AuthoredClaim::NativeOnly);
    }
}
