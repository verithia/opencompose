use super::*;
impl Vst3ControlHandle {
    /// Names are bounded before dispatch and resolved once on the actual UI.
    pub fn normalized_bindings_plan(&self, names: Vec<String>) -> Vst3NormalizedBindingsPlan {
        Vst3NormalizedBindingsPlan {
            link: Arc::clone(&self.link),
            names,
        }
    }
    /// Plan a metadata and parameter snapshot on the UI thread.
    pub fn metadata_plan(&self) -> Vst3MetadataReadPlan {
        Vst3MetadataReadPlan {
            link: Arc::clone(&self.link),
        }
    }
    /// Plan a state capture on the UI thread.
    pub fn capture_state_plan(&self) -> Vst3StateCapturePlan {
        Vst3StateCapturePlan {
            link: Arc::clone(&self.link),
        }
    }
    /// Plan one normalized parameter write on the UI thread.
    pub fn parameter_write_plan(&self, value: Vst3NormalizedParameter) -> Vst3ParameterWritePlan {
        Vst3ParameterWritePlan {
            link: Arc::clone(&self.link),
            value,
        }
    }
}
/// A UI-thread metadata read.
pub struct Vst3MetadataReadPlan {
    link: Arc<ProcessLink>,
}
impl Vst3MetadataReadPlan {
    /// Execute on the registered UI thread and return the snapshot.
    pub fn execute_on_ui(self) -> Result<Vst3OwnedSnapshot, Vst3OwnedError> {
        dispatch_owned(move || {
            registry::with_control(&self.link, |token, instance| {
                metadata::capture(token, instance, self.link.source)
            })
        })
    }
}
/// A UI-thread state capture.
pub struct Vst3StateCapturePlan {
    link: Arc<ProcessLink>,
}
impl Vst3StateCapturePlan {
    /// Execute and return the captured state bytes.
    pub fn execute_on_ui(self) -> Result<Vec<u8>, Vst3OwnedError> {
        dispatch_owned(move || {
            registry::with_control(&self.link, |_, instance| {
                if !instance.pending_events.is_empty() {
                    return Err(Vst3OwnedError::Busy);
                }
                instance
                    .state_bytes()
                    .map_err(|message| native_or_message(&instance.failure, message))
            })
        })
    }
}
/// A UI-thread parameter write with preflight checks.
pub struct Vst3ParameterWritePlan {
    link: Arc<ProcessLink>,
    value: Vst3NormalizedParameter,
}
impl Vst3ParameterWritePlan {
    /// Execute after inventory, preflight and capacity checks.
    pub fn execute_on_ui(self) -> Result<(), Vst3OwnedError> {
        dispatch_owned(move || {
            registry::with_control(&self.link, |token, instance| {
                if instance.parameter_inventory_revision() != self.link.snapshot.inventory_revision
                {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "VST3 parameter inventory changed; reprepare the processor",
                    ));
                }
                metadata::preflight(&self.link.snapshot, std::slice::from_ref(&self.value))?;
                if instance.pending_events.len() >= self.link.budget.events {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "VST3 parameter event capacity exhausted",
                    ));
                }
                let pending=instance.pending_events.iter().filter(|event|matches!(event,Vst3PendingEvent::Param(value) if value.id==self.value.id)).count();
                if pending >= self.link.budget.points_per_parameter {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "VST3 parameter point capacity exhausted",
                    ));
                }
                metadata::apply(token, instance, std::slice::from_ref(&self.value))
            })
        })
    }
}

/// Finite normalized automation bindings, returned in the caller's exact order.
pub struct Vst3NormalizedBindingsPlan {
    link: Arc<ProcessLink>,
    names: Vec<String>,
}
impl Vst3NormalizedBindingsPlan {
    /// Resolve names in order and execute on the UI thread.
    pub fn execute_on_ui(self) -> Result<Vec<u32>, Vst3OwnedError> {
        if self.names.len() > MAX_VST3_AUTHORED_PARAMETERS
            || self
                .names
                .iter()
                .any(|n| n.len() > MAX_VST3_AUTHORED_NAME_BYTES)
            || self
                .names
                .iter()
                .try_fold(0usize, |sum, n| sum.checked_add(n.len()))
                .is_none_or(|sum| sum > MAX_VST3_AUTHORED_TOTAL_NAME_BYTES)
        {
            return Err(Vst3OwnedError::InvalidRequest(
                "VST3 runtime binding names exceed their preparation budget",
            ));
        }
        if self.names.is_empty() {
            return Ok(Vec::new());
        }
        // Deduplicate before controller work while retaining an exact order map.
        let mut positions = HashMap::new();
        let mut unique = Vec::new();
        let order: Vec<_> = self
            .names
            .into_iter()
            .map(|name| {
                if let Some(&index) = positions.get(&name) {
                    return index;
                }
                let index = unique.len();
                positions.insert(name.clone(), index);
                unique.push(name);
                index
            })
            .collect();
        dispatch_owned(move || {
            registry::with_control(&self.link, |_, instance| {
                if instance.parameter_inventory_revision() != self.link.snapshot.inventory_revision
                {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "VST3 inventory changed before runtime binding",
                    ));
                }
                let mut owner = instance
                    .authored_parameter_owner()
                    .map_err(|kind| binding_error("<bindings>", kind))?;
                if !owner.inventory_matches(&self.link.snapshot) {
                    return Err(Vst3OwnedError::InvalidRequest(
                        "VST3 inventory changed during runtime binding",
                    ));
                }
                let ids: Result<Vec<_>, _> = unique
                    .iter()
                    .map(|name| {
                        owner
                            .normalized_binding(name)
                            .map_err(|kind| binding_error(name, kind))
                    })
                    .collect();
                let ids = ids?;
                use crate::preset_parameters::AuthoredParameterOwner;
                owner
                    .check_parameter_authority()
                    .map_err(|kind| binding_error("<bindings>", kind))?;
                Ok(order.into_iter().map(|index| ids[index]).collect())
            })
        })
    }
}
fn binding_error(name: &str, kind: crate::preset_parameters::ParameterErrorKind) -> Vst3OwnedError {
    Vst3OwnedError::Authored(Box::new(crate::preset_parameters::ParameterApplyError {
        format: crate::PluginFormat::Vst3,
        parameter: name.to_owned(),
        kind,
    }))
}
