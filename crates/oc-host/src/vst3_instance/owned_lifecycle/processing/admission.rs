#![expect(
    clippy::result_large_err,
    reason = "admission preserves the shared inline realtime refusal type without allocating"
)]
use super::*;
mod midi;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::vst3_instance) struct AdmissionWork {
    pub resets: usize,
    pub pending_visited: usize,
    pub submitted_visited: usize,
    pub lookups: usize,
    pub comparisons: usize,
}
struct Binding {
    id: u32,
    automatable: bool,
    count: usize,
}
/// Registry-owned scratch is retained until the last processing/control ticket retires on UI.
pub(in crate::vst3_instance::owned_lifecycle) struct PreparedAdmission {
    bindings: Vec<Binding>,
    work: AdmissionWork,
    midi_events: Vec<Vst3PendingEvent>,
    notes: Vst3NoteIds,
    active_limit: usize,
    midi_notes_staged: bool,
}
impl PreparedAdmission {
    #[cfg(test)]
    pub fn new(snapshot: &Vst3OwnedSnapshot) -> Self {
        Self::with_budget(snapshot, Vst3ProcessingBudget::default())
    }
    pub fn with_budget(snapshot: &Vst3OwnedSnapshot, budget: Vst3ProcessingBudget) -> Self {
        let mut permissions = HashMap::with_capacity(snapshot.parameters.len());
        for parameter in &snapshot.parameters {
            let permission = parameter_can_automate(parameter.flags);
            permissions
                .entry(parameter.id)
                .and_modify(|value: &mut bool| *value &= permission)
                .or_insert(permission);
        }
        let mut bindings: Vec<_> = permissions
            .into_iter()
            .map(|(id, automatable)| Binding {
                id,
                automatable,
                count: 0,
            })
            .collect();
        bindings.sort_unstable_by_key(|binding| binding.id);
        Self {
            bindings,
            work: AdmissionWork::default(),
            midi_events: Vec::with_capacity(budget.events),
            notes: Vst3NoteIds::with_capacity(super::super::midi::active_capacity(budget)),
            active_limit: super::super::midi::active_capacity(budget),
            midi_notes_staged: false,
        }
    }
    fn lookup(&mut self, id: u32) -> Option<usize> {
        self.work.lookups += 1;
        let (mut low, mut high) = (0, self.bindings.len());
        while low < high {
            self.work.comparisons += 1;
            let middle = low + (high - low) / 2;
            match self.bindings[middle].id.cmp(&id) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Some(middle),
            }
        }
        None
    }
    fn increment(&mut self, index: usize, limit: usize) -> Result<(), Vst3ProcessLeaseError> {
        let count = &mut self.bindings[index].count;
        if *count >= limit {
            return Err(Vst3ProcessLeaseError::EventCapacity);
        }
        *count += 1;
        Ok(())
    }
    pub fn validate(
        &mut self,
        pending: &[Vst3PendingEvent],
        values: &[Vst3NormalizedEvent],
        frames: u32,
        budget: Vst3ProcessingBudget,
    ) -> Result<(), Vst3ProcessLeaseError> {
        self.work = AdmissionWork::default();
        if pending
            .len()
            .checked_add(values.len())
            .is_none_or(|total| total > budget.events)
        {
            return Err(Vst3ProcessLeaseError::EventCapacity);
        }
        for binding in &mut self.bindings {
            binding.count = 0;
            self.work.resets += 1;
        }
        for event in pending {
            self.work.pending_visited += 1;
            if let Vst3PendingEvent::Param(value) = event {
                let index = self
                    .lookup(value.id)
                    .ok_or(Vst3ProcessLeaseError::UnpreparedParameter)?;
                self.increment(index, budget.points_per_parameter)?;
            }
        }
        for value in values {
            self.work.submitted_visited += 1;
            let index = self.normalized_binding(value, frames)?;
            self.increment(index, budget.points_per_parameter)?;
        }

        Ok(())
    }
    fn normalized_binding(
        &mut self,
        value: &Vst3NormalizedEvent,
        frames: u32,
    ) -> Result<usize, Vst3ProcessLeaseError> {
        if value.sample_offset >= frames
            || !value.value.is_finite()
            || !(0.0..=1.0).contains(&value.value)
        {
            return Err(Vst3ProcessLeaseError::InvalidBlock);
        }
        let index = self
            .lookup(value.id)
            .ok_or(Vst3ProcessLeaseError::UnpreparedParameter)?;
        if !self.bindings[index].automatable {
            return Err(Vst3ProcessLeaseError::UnpreparedParameter);
        }
        Ok(index)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn work(&self) -> AdmissionWork {
        self.work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(count: u32) -> Vst3OwnedSnapshot {
        Vst3OwnedSnapshot {
            parameters: (0..count)
                .map(|id| Vst3OwnedParameter {
                    id,
                    name: String::new(),
                    unit: String::new(),
                    default_normalized: 0.5,
                    flags: ParameterFlags_::kCanAutomate,
                    steps: 0,
                })
                .collect(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            latency_samples: 0,
            tail_samples: 0,
            max_frames: 64,
            source: 1,
            inventory_revision: 0,
        }
    }
    fn event(id: u32) -> Vst3NormalizedEvent {
        Vst3NormalizedEvent {
            id,
            value: 0.5,
            sample_offset: 17,
        }
    }
    #[test]
    fn maximum_distinct_and_repeated_batches_have_bounded_lookup_work() {
        let mut admission = PreparedAdmission::new(&snapshot(16384));
        let values: Vec<_> = (0..65536).map(|index| event(index % 16384)).collect();
        admission
            .validate(
                &[],
                &values,
                64,
                Vst3ProcessingBudget {
                    events: 65536,
                    points_per_parameter: 4,
                },
            )
            .unwrap();
        let work = admission.work();
        assert_eq!(work.resets, 16384);
        assert_eq!(work.submitted_visited, 65536);
        assert_eq!(work.lookups, 65536);
        assert!(work.comparisons <= work.lookups * 15);
        let repeated = vec![event(0); 1024];
        admission
            .validate(
                &[],
                &repeated,
                64,
                Vst3ProcessingBudget {
                    events: 65536,
                    points_per_parameter: 1024,
                },
            )
            .unwrap();
        assert_eq!(admission.work().submitted_visited, 1024);
        assert!(admission.work().comparisons <= 1024 * 15);
    }
    #[test]
    fn pending_points_count_once_and_failed_batches_leave_caller_state_unchanged() {
        let mut admission = PreparedAdmission::new(&snapshot(2));
        let pending = [Vst3PendingEvent::Param(Vst3ParamEvent {
            id: 0,
            value: 0.25,
            sample_offset: 3,
        })];
        let budget = Vst3ProcessingBudget {
            events: 8,
            points_per_parameter: 2,
        };
        assert!(matches!(
            admission.validate(&pending, &[event(0), event(0)], 64, budget),
            Err(Vst3ProcessLeaseError::EventCapacity)
        ));
        admission
            .validate(&pending, &[event(1), event(0)], 64, budget)
            .unwrap();
        assert_eq!(admission.work().pending_visited, 1);
        assert_eq!(admission.work().submitted_visited, 2);
        assert_eq!(admission.work().lookups, 3);
        let Vst3PendingEvent::Param(unchanged) = &pending[0] else {
            panic!("retained parameter event");
        };
        assert_eq!(
            (unchanged.id, unchanged.value, unchanged.sample_offset),
            (0, 0.25, 3)
        );
        assert!(matches!(
            admission.validate(&pending, &[event(2)], 64, budget),
            Err(Vst3ProcessLeaseError::UnpreparedParameter)
        ));
        assert!(matches!(
            admission.validate(
                &pending,
                &[Vst3NormalizedEvent {
                    sample_offset: 64,
                    ..event(1)
                }],
                64,
                budget
            ),
            Err(Vst3ProcessLeaseError::InvalidBlock)
        ));
        assert!(matches!(
            admission.validate(
                &pending,
                &[Vst3NormalizedEvent {
                    value: f64::NAN,
                    ..event(1)
                }],
                64,
                budget
            ),
            Err(Vst3ProcessLeaseError::InvalidBlock)
        ));
        admission
            .validate(&pending, &[event(0)], 64, budget)
            .unwrap();
    }
    #[test]
    fn duplicate_descriptors_preserve_readonly_and_nonautomatable_refusals() {
        let mut source = snapshot(2);
        source.parameters.push(Vst3OwnedParameter {
            flags: ParameterFlags_::kCanAutomate | ParameterFlags_::kIsReadOnly,
            ..source.parameters[0].clone()
        });
        source.parameters.push(Vst3OwnedParameter {
            flags: 0,
            ..source.parameters[1].clone()
        });
        let mut admission = PreparedAdmission::new(&source);
        let budget = Vst3ProcessingBudget::default();
        for id in [0, 1] {
            assert!(matches!(
                admission.validate(&[], &[event(id)], 64, budget),
                Err(Vst3ProcessLeaseError::UnpreparedParameter)
            ));
        }
        let pending = [Vst3PendingEvent::Param(Vst3ParamEvent {
            id: 1,
            value: 0.5,
            sample_offset: 0,
        })];
        admission.validate(&pending, &[], 64, budget).unwrap();
    }
}
