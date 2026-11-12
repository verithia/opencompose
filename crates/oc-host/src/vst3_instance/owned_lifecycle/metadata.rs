use super::*;
fn native<T>(
    instance: &Vst3Instance,
    operation: Operation,
    result: Result<T, Fault>,
) -> Result<T, Vst3OwnedError> {
    result
        .map_err(|fault| Vst3OwnedError::Native(Box::new(instance.native_fault(operation, fault))))
}
pub(super) fn capture(
    _token: &UiThreadToken<'_>,
    instance: &Vst3Instance,
    source: u64,
) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
    application_main::ui_authority()?;
    instance
        .healthy()
        .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
    let inventory_revision = instance.parameter_inventory_revision();
    let mut parameters = Vec::new();
    if let Some(controller) = instance.controller.as_ref() {
        // SAFETY: real UI admission and the registry's control permit retain this controller.
        let count = native(instance, Operation::ParameterCount, unsafe {
            vst3_guard::parameter_count(controller.as_ptr())
        })?;
        if count < 0 || count as usize > MAX_PARAMETERS {
            return Err(Vst3OwnedError::InvalidRequest(
                "Invalid VST3 parameter count",
            ));
        }
        parameters.reserve(count as usize);
        for index in 0..count {
            application_main::ui_authority()?;
            // SAFETY: metadata has only inline primitive fields and the output is retained across the call.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            // SAFETY: the bounded index, UI-owned controller and stack output are live.
            let code = native(instance, Operation::ParameterInfo, unsafe {
                vst3_guard::parameter_info(controller.as_ptr(), index, &mut info)
            })?;
            if code != kResultOk {
                return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
                    Operation::ParameterInfo,
                    code,
                ))));
            }
            if !info.defaultNormalizedValue.is_finite()
                || !(0.0..=1.0).contains(&info.defaultNormalizedValue)
                || info.stepCount < 0
            {
                return Err(Vst3OwnedError::InvalidRequest(
                    "Invalid VST3 normalized default or parameter step count",
                ));
            }
            parameters.push(Vst3OwnedParameter {
                id: info.id,
                name: char16_to_string(&info.title),
                unit: char16_to_string(&info.units),
                default_normalized: info.defaultNormalizedValue,
                flags: info.flags,
                steps: info.stepCount,
            });
        }
    }
    application_main::ui_authority()?;
    let latency_samples = instance.processor_latency_samples();
    application_main::ui_authority()?;
    let tail_samples = instance.tail_samples();
    instance
        .healthy()
        .map_err(|error| Vst3OwnedError::Native(Box::new(error)))?;
    if inventory_revision != instance.parameter_inventory_revision() {
        return Err(Vst3OwnedError::InventoryChanged);
    }
    Ok(Vst3OwnedSnapshot {
        parameters,
        inputs: instance.input_ports(),
        outputs: instance.output_ports(),
        latency_samples,
        tail_samples,
        max_frames: instance.max_frames as u32,
        source,
        inventory_revision,
    })
}
pub(super) fn preflight(
    snapshot: &Vst3OwnedSnapshot,
    values: &[Vst3NormalizedParameter],
) -> Result<(), Vst3OwnedError> {
    let mut writable = HashMap::with_capacity(snapshot.parameters.len());
    for parameter in &snapshot.parameters {
        let permission = parameter.flags & ParameterFlags_::kIsReadOnly == 0;
        writable
            .entry(parameter.id)
            .and_modify(|existing: &mut bool| *existing &= permission)
            .or_insert(permission);
    }
    for value in values {
        if !value.value.is_finite() || !(0.0..=1.0).contains(&value.value) {
            return Err(Vst3OwnedError::InvalidRequest(
                "VST3 normalized value must be finite and within 0..1",
            ));
        }
        match writable.get(&value.id) {
            None => return Err(Vst3OwnedError::InvalidRequest("Unknown VST3 parameter ID")),
            Some(false) => {
                return Err(Vst3OwnedError::InvalidRequest(
                    "VST3 parameter is read-only",
                ));
            }
            Some(true) => {}
        }
    }
    Ok(())
}

// Full native descriptor verification is bounded across an entire initial batch,
// including its first capture. This limits UI work without weakening identity checks.
pub(super) const MAX_INITIAL_METADATA_QUERIES: usize = 1_048_576;
pub(super) fn initial_verification_queries(
    parameters: usize,
    writes: usize,
) -> Result<usize, Vst3OwnedError> {
    parameters
        .checked_add(3)
        .and_then(|per_capture| {
            writes
                .checked_add(1)
                .and_then(|captures| per_capture.checked_mul(captures))
        })
        .filter(|queries| *queries <= MAX_INITIAL_METADATA_QUERIES)
        .ok_or(Vst3OwnedError::InvalidRequest(
            "Initial VST3 metadata verification exceeds its work budget",
        ))
}

pub(super) fn apply_initial(
    token: &UiThreadToken<'_>,
    instance: &mut Vst3Instance,
    before: Vst3OwnedSnapshot,
    values: &[Vst3NormalizedParameter],
) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
    initial_verification_queries(before.parameters.len(), values.len())?;
    let mut last = None;
    for value in values {
        if instance.parameter_inventory_revision() != before.inventory_revision {
            return Err(Vst3OwnedError::InvalidRequest(
                "VST3 inventory changed during initial parameter application",
            ));
        }
        apply(token, instance, std::slice::from_ref(value))?;
        let current = capture(token, instance, before.source)?;
        if current.inventory_revision != before.inventory_revision
            || current.parameters != before.parameters
            || current.inputs != before.inputs
            || current.outputs != before.outputs
            || current.max_frames != before.max_frames
        {
            return Err(Vst3OwnedError::InvalidRequest(
                "VST3 inventory changed during initial parameter application",
            ));
        }
        last = Some(current);
    }
    Ok(last.unwrap_or(before))
}

pub(super) fn apply(
    _token: &UiThreadToken<'_>,
    instance: &mut Vst3Instance,
    values: &[Vst3NormalizedParameter],
) -> Result<(), Vst3OwnedError> {
    let Some(controller) = instance.controller.as_ref() else {
        return if values.is_empty() {
            Ok(())
        } else {
            Err(Vst3OwnedError::InvalidRequest("No VST3 edit controller"))
        };
    };
    for value in values {
        application_main::ui_authority()?;
        // SAFETY: the caller preflighted IDs/values and owns the synchronous UI control permit.
        let code = native(instance, Operation::SetNormalized, unsafe {
            vst3_guard::set_normalized(controller.as_ptr(), value.id, value.value)
        })?;
        if code != kResultOk {
            return Err(Vst3OwnedError::Native(Box::new(Vst3ProcessError::refused(
                Operation::SetNormalized,
                code,
            ))));
        }
        instance
            .pending_events
            .push(Vst3PendingEvent::Param(Vst3ParamEvent {
                id: value.id,
                value: value.value,
                sample_offset: 0,
            }));
    }
    Ok(())
}
pub(super) fn prepare_processing(
    instance: &mut Vst3Instance,
    snapshot: &Vst3OwnedSnapshot,
    budget: Vst3ProcessingBudget,
) -> Result<(), Vst3OwnedError> {
    if snapshot
        .parameters
        .len()
        .checked_mul(budget.points_per_parameter)
        .is_none_or(|points| points > PREPARED_PARAMETER_POINTS)
    {
        return Err(Vst3OwnedError::InvalidRequest(
            "VST3 prepared parameter storage exceeds capacity",
        ));
    }
    instance
        .pending_events
        .reserve(budget.events.saturating_sub(instance.pending_events.len()));
    instance.note_ids.reserve(midi::active_capacity(budget));
    instance.event_scratch.reserve(budget.events);
    instance.param_scratch.reserve(budget.events);
    let mut queues = Vst3ProcessQueues::new();
    queues.events.events.borrow_mut().reserve(budget.events);
    queues
        .parameters
        .queues
        .borrow_mut()
        .reserve(snapshot.parameters.len());
    queues.value_queues.reserve(snapshot.parameters.len());
    queues.queue_ids.reserve(snapshot.parameters.len());
    let mut ids = std::collections::HashSet::with_capacity(snapshot.parameters.len());
    for parameter in &snapshot.parameters {
        if !ids.insert(parameter.id) {
            continue;
        }
        queues.queue_ids.push(parameter.id);
        queues
            .value_queues
            .push(ComWrapper::new(SimpleParamValueQueue {
                id: parameter.id,
                points: RefCell::new(Vec::with_capacity(budget.points_per_parameter)),
            }));
    }
    queues.prepare_parameter_index();
    *instance.process_queues = Some(queues);
    Ok(())
}

// Each authored row permits two endpoint conversions, one plain conversion and
// one setter, each followed by a full descriptor check. Initial/final captures
// and the conversion entries themselves are included before the first write.
pub(super) fn authored_verification_queries(
    parameters: usize,
    assignments: usize,
) -> Result<usize, Vst3OwnedError> {
    parameters
        .checked_add(3)
        .and_then(|capture| {
            assignments
                .checked_mul(4)
                .and_then(|n| n.checked_add(2))
                .and_then(|n| capture.checked_mul(n))
        })
        .and_then(|n| {
            assignments
                .checked_mul(3)
                .and_then(|conversion| n.checked_add(conversion))
        })
        .filter(|n| *n <= MAX_INITIAL_METADATA_QUERIES)
        .ok_or(Vst3OwnedError::InvalidRequest(
            "Authored VST3 metadata verification exceeds its work budget",
        ))
}
