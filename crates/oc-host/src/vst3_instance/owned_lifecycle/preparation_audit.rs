//! Actual-main native admission checks for final initial-value metadata.
use super::super::native_fixture::NativeFixture;
use super::*;

#[derive(Debug)]
/// Audit results for initial preparation.
pub struct Vst3InitialPreparationAudit {
    pub latency_samples: u32,
    pub tail_samples: u32,
    pub descriptor_change_refused_before_later_write: bool,
    pub generation_change_refused_before_later_write: bool,
    pub invalid_metadata_refusals: usize,
    pub layout_change_refusals: usize,
    pub teardown_fault_keeps_reservation: bool,
    pub oversized_refused_before_metadata: bool,
}

struct Fixture {
    native: ManuallyDrop<NativeFixture>,
    owned: Option<Vst3OwnedInstance>,
}
impl Fixture {
    fn new() -> Result<Self, Vst3OwnedError> {
        application_main::ui_authority()?;
        let (native, loaded) = NativeFixture::load_with_fault(22, 0);
        let mut fixture = Self {
            native: ManuallyDrop::new(native),
            owned: None,
        };
        loaded.map_err(Vst3OwnedError::Control)?;
        fixture.native.configure_parameters_for_test(7);
        Ok(fixture)
    }
    fn adopt(
        &mut self,
        token: &UiThreadToken<'_>,
        values: &[Vst3NormalizedParameter],
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3OwnedError> {
        let instance = self
            .native
            .take_instance_for_preparation()
            .ok_or(Vst3OwnedError::Retired)?;
        let slot = registry::reserve(token, instance.failure())?;
        self.owned = Some(registry::adopt(token, instance, budget, values, slot)?);
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.owned.take());
        if poll_owned_vst3_retirements().is_ok() {
            // SAFETY: this private guard exposes no native/lease ownership; all
            // its tickets retire before the deterministic no-worker fixture owner.
            unsafe { ManuallyDrop::drop(&mut self.native) };
        }
    }
}
fn require(condition: bool, message: &'static str) -> Result<(), Vst3OwnedError> {
    condition
        .then_some(())
        .ok_or(Vst3OwnedError::InvalidRequest(message))
}

/// Requires the real executable bootstrap; it opens no plugin window or device.
pub fn audit_owned_initial_preparation() -> Result<Vst3InitialPreparationAudit, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let values = [
        Vst3NormalizedParameter {
            id: 100,
            value: 0.75,
        },
        Vst3NormalizedParameter {
            id: 101,
            value: 0.25,
        },
    ];
    let mut valid = Fixture::new()?;
    valid.adopt(&token, &values, Vst3ProcessingBudget::default())?;
    let owned = valid
        .owned
        .as_ref()
        .expect("successful adoption publishes one owner");
    let latency_samples = owned.snapshot.latency_samples;
    let tail_samples = owned.snapshot.tail_samples;
    require(
        latency_samples == 106 && tail_samples == 287,
        "Owned snapshot ignored accepted initial values or activation",
    )?;
    let queues = registry::with_control(&owned.control.link, |_, instance| {
        let queues = instance
            .process_queues
            .as_ref()
            .ok_or(Vst3OwnedError::InvalidRequest("Missing prepared queues"))?;
        Ok((queues.queue_ids.clone(), instance.pending_events.len()))
    })?;
    require(
        owned
            .snapshot
            .parameters
            .iter()
            .all(|parameter| queues.0.contains(&parameter.id))
            && queues.1 == 2,
        "Final snapshot and prepared parameter queues disagree",
    )?;
    drop(valid);

    let mut layout_change_refusals = 0;
    for (mutation, restart_flags) in [
        (1, 0),
        (2, RestartFlags_::kParamTitlesChanged),
        (2, RestartFlags_::kIoChanged),
        (2, RestartFlags_::kRoutingInfoChanged),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.native.preparation_mutation_for_test(mutation);
        if mutation == 2 {
            fixture.native.parameter_restart_for_test(0, restart_flags);
        }
        let before = fixture.native.calls(41);
        let result = fixture.adopt(&token, &values, Vst3ProcessingBudget::default());
        require(
            matches!(result, Err(Vst3OwnedError::InvalidRequest(_))) && fixture.owned.is_none(),
            "Changed inventory published a ready owned lease",
        )?;
        require(
            fixture.native.calls(41) == before + 1 && fixture.native.calls(1) == 0,
            "Changed inventory allowed a later initial setter or processor entry",
        )?;
        if restart_flags & (RestartFlags_::kIoChanged | RestartFlags_::kRoutingInfoChanged) != 0 {
            layout_change_refusals += 1;
        }
    }
    let mut invalid_metadata_refusals = 0;
    for mutation in [3, 4] {
        let mut fixture = Fixture::new()?;
        fixture.native.preparation_mutation_for_test(mutation);
        require(
            matches!(
                fixture.adopt(&token, &values, Vst3ProcessingBudget::default()),
                Err(Vst3OwnedError::InvalidRequest(_))
            ) && fixture.owned.is_none()
                && fixture.native.calls(41) == 0,
            "Malformed normalized metadata was published or written",
        )?;
        invalid_metadata_refusals += 1;
    }
    let mut oversized = Fixture::new()?;
    let before = (
        oversized.native.calls(37),
        oversized.native.calls(38),
        oversized.native.calls(41),
    );
    require(
        matches!(
            oversized.adopt(
                &token,
                &values,
                Vst3ProcessingBudget {
                    events: 1,
                    points_per_parameter: 1
                }
            ),
            Err(Vst3OwnedError::InvalidRequest(_))
        ) && oversized.owned.is_none(),
        "Oversized initial request was admitted",
    )?;
    require(
        before
            == (
                oversized.native.calls(37),
                oversized.native.calls(38),
                oversized.native.calls(41),
            ),
        "Oversized initial request entered native metadata or setters",
    )?;
    let before_slots = registry::reserved_slots_for_audit();
    let mut teardown_fault = Fixture::new()?;
    teardown_fault.native.set_fault(3, 1);
    require(
        matches!(
            teardown_fault.adopt(
                &token,
                &[],
                Vst3ProcessingBudget {
                    events: 0,
                    points_per_parameter: 1
                },
            ),
            Err(Vst3OwnedError::InvalidRequest(_))
        ) && teardown_fault.owned.is_none()
            && teardown_fault.native.refs() > 0
            && registry::reserved_slots_for_audit() == before_slots + 1,
        "Rejected candidate released its reservation before teardown quarantined it",
    )?;
    Ok(Vst3InitialPreparationAudit {
        latency_samples,
        tail_samples,
        descriptor_change_refused_before_later_write: true,
        generation_change_refused_before_later_write: true,
        invalid_metadata_refusals,
        layout_change_refusals,
        teardown_fault_keeps_reservation: true,
        oversized_refused_before_metadata: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verification_work_accepts_exact_limit_and_refuses_growth_or_arithmetic_overflow() {
        assert_eq!(
            metadata::initial_verification_queries(13, 65535).unwrap(),
            1_048_576
        );
        for (parameters, writes) in [
            (13, 65536),
            (usize::MAX, 0),
            (0, usize::MAX),
            (usize::MAX / 2, 2),
        ] {
            assert!(metadata::initial_verification_queries(parameters, writes).is_err());
        }
    }
    #[test]
    fn indexed_initial_validation_accepts_large_distinct_batches_and_refuses_readonly_duplicates() {
        let budget = Vst3ProcessingBudget {
            events: 65536,
            points_per_parameter: 4,
        };
        let values: Vec<_> = (0..65536)
            .map(|index| Vst3NormalizedParameter {
                id: index % 16384,
                value: 0.5,
            })
            .collect();
        registry::validate_initial_budget(budget, &values).unwrap();
        let parameters: Vec<_> = (0..16384)
            .map(|id| Vst3OwnedParameter {
                id,
                name: String::new(),
                unit: String::new(),
                default_normalized: 0.5,
                flags: 0,
                steps: 0,
            })
            .collect();
        let mut snapshot = Vst3OwnedSnapshot {
            parameters,
            inputs: Vec::new(),
            outputs: Vec::new(),
            latency_samples: 0,
            tail_samples: 0,
            max_frames: 64,
            source: 1,
            inventory_revision: 0,
        };
        metadata::preflight(&snapshot, &values).unwrap();
        snapshot.parameters.push(Vst3OwnedParameter {
            flags: ParameterFlags_::kIsReadOnly,
            ..snapshot.parameters[0].clone()
        });
        assert!(metadata::preflight(&snapshot, &values).is_err());
        assert!(
            registry::validate_initial_budget(
                Vst3ProcessingBudget {
                    events: 65535,
                    ..budget
                },
                &values
            )
            .is_err()
        );
        let repeated = [Vst3NormalizedParameter { id: 0, value: 0.5 }; 5];
        assert!(registry::validate_initial_budget(budget, &repeated).is_err());
    }
}
