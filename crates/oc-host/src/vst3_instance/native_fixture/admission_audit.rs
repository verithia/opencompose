//! Native audits distinguish admission-only and full indexed queue-packing coverage.
use super::*;
use crate::application_main;

#[derive(Debug)]
/// Counters for one owned-admission case.
pub struct OwnedAdmissionCaseAudit {
    pub descriptors: usize,
    pub pending: usize,
    pub submitted: usize,
    pub resets: usize,
    pub lookups: usize,
    pub comparisons: usize,
    pub accepted_heap: OwnedHeapCounters,
    pub rejected_heap: OwnedHeapCounters,
    pub final_worker_drop_heap: OwnedHeapCounters,
    pub refused_before_native_or_queue_mutation: bool,
}
#[derive(Debug)]
/// Owned-admission audit across cases.
pub struct OwnedAdmissionAudit {
    pub maximum_distinct: OwnedAdmissionCaseAudit,
    pub maximum_repeated: OwnedAdmissionCaseAudit,
}
struct Case {
    fixture: ManuallyDrop<NativeFixture>,
    control: Option<Vst3ControlHandle>,
}
impl Drop for Case {
    fn drop(&mut self) {
        drop(self.control.take());
        if poll_owned_vst3_retirements().is_ok() {
            // SAFETY: no worker or external processing/control ticket escapes
            // this synchronous audit before its explicit fixture owner retires.
            unsafe { ManuallyDrop::drop(&mut self.fixture) };
        }
    }
}
fn require(condition: bool, message: &'static str) -> Result<(), Vst3OwnedError> {
    condition
        .then_some(())
        .ok_or(Vst3OwnedError::InvalidRequest(message))
}
fn delta(after: OwnedHeapCounters, before: OwnedHeapCounters) -> OwnedHeapCounters {
    OwnedHeapCounters {
        allocations: after.allocations - before.allocations,
        reallocations: after.reallocations - before.reallocations,
        deallocations: after.deallocations - before.deallocations,
    }
}
fn run_case(
    repeated: bool,
    read: fn() -> OwnedHeapCounters,
) -> Result<OwnedAdmissionCaseAudit, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let (fixture, loaded) = NativeFixture::load_with_fault(22, 0);
    let mut case = Case {
        fixture: ManuallyDrop::new(fixture),
        control: None,
    };
    loaded.map_err(Vst3OwnedError::Control)?;
    case.fixture
        .configure_parameters_for_test(if repeated { 5 } else { 8 });
    let instance = case
        .fixture
        .instance
        .take()
        .expect("loaded admission fixture");
    let budget = Vst3ProcessingBudget {
        events: 65536,
        points_per_parameter: if repeated { 1024 } else { 4 },
    };
    let owned = owned_lifecycle::adopt_native_fixture_with_budget(&token, instance, budget)?;
    let descriptors = owned.snapshot.parameters.len();
    let mut processor = owned.processor;
    case.control = Some(owned.control);
    let first_id = if repeated { 100 } else { 1000 };
    if repeated {
        case.control
            .as_ref()
            .unwrap()
            .parameter_write_plan(Vst3NormalizedParameter {
                id: first_id,
                value: 0.25,
            })
            .execute_on_ui()?;
    }
    let values: Vec<_> = (0..if repeated { 1023 } else { 65536 })
        .map(|index| Vst3NormalizedEvent {
            id: if repeated {
                first_id
            } else {
                first_id + index % 16384
            },
            value: 0.5,
            sample_offset: 17,
        })
        .collect();
    let mut invalid = values.clone();
    if repeated {
        invalid.push(Vst3NormalizedEvent {
            id: first_id,
            value: 0.5,
            sample_offset: 17,
        });
    } else {
        invalid.last_mut().unwrap().id = u32::MAX;
    }
    let processes = case.fixture.calls(1);
    let (returned, outcome) = std::thread::spawn(move || {
        let _ = read();
        let before = read();
        let accepted = processor.preflight_admission_for_test(64, &values);
        let accepted_heap = delta(read(), before);
        let work: owned_lifecycle::AdmissionWork = processor.admission_work_for_test();
        let mut output = [0.625; 128];
        let before = read();
        let result = processor.process_stereo_into(Vst3OwnedProcessBlock {
            frames: 64,
            transport: None,
            parameters: &invalid,
            input: None,
            sidechain: None,
            output: &mut output,
        });
        let rejected_heap = delta(read(), before);
        let refused = if repeated {
            matches!(result, Err(Vst3ProcessLeaseError::EventCapacity))
        } else {
            matches!(result, Err(Vst3ProcessLeaseError::UnpreparedParameter))
        };
        let unchanged = output.iter().all(|value| *value == 0.625);
        let pending = processor.preflight_admission_for_test(64, &[]).is_ok()
            && processor.admission_work_for_test().pending_visited == usize::from(repeated);
        (
            processor,
            (
                accepted.is_ok(),
                work,
                accepted_heap,
                rejected_heap,
                refused && unchanged && pending,
            ),
        )
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Admission audit worker panicked".into()))?;
    processor = returned;
    let (accepted, work, accepted_heap, rejected_heap, refused) = outcome;
    require(
        accepted && refused && case.fixture.calls(1) == processes,
        "Maximum admission mutated queues/output or entered native processing on refusal",
    )?;
    let expected = if repeated { 1023 } else { 65536 };
    let unique = if repeated { 7 } else { 16384 };
    let logarithmic_limit = if repeated { 3 } else { 15 };
    require(
        work.resets == unique
            && work.submitted_visited == expected
            && work.pending_visited == usize::from(repeated)
            && work.lookups == expected + usize::from(repeated)
            && work.comparisons <= work.lookups * logarithmic_limit,
        "Admission exceeded its bounded single-pass/binary-lookup work",
    )?;
    require(
        accepted_heap == OwnedHeapCounters::default()
            && rejected_heap == OwnedHeapCounters::default(),
        "Maximum admission allocated, reallocated or freed host storage",
    )?;
    drop(case.control.take());
    let final_worker_drop_heap = std::thread::spawn(move || {
        let _ = read();
        let before = read();
        drop(processor);
        delta(read(), before)
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Admission retirement worker panicked".into()))?;
    require(
        final_worker_drop_heap == OwnedHeapCounters::default(),
        "Final worker drop freed prepared admission storage",
    )?;
    poll_owned_vst3_retirements()?;
    require(
        case.fixture.refs() == 0,
        "Prepared admission owner did not retire on UI",
    )?;
    Ok(OwnedAdmissionCaseAudit {
        descriptors,
        pending: work.pending_visited,
        submitted: work.submitted_visited,
        resets: work.resets,
        lookups: work.lookups,
        comparisons: work.comparisons,
        accepted_heap,
        rejected_heap,
        final_worker_drop_heap,
        refused_before_native_or_queue_mutation: true,
    })
}

/// Run from the registered actual executable main thread. Maximum successful
/// batches exercise the real admission owner only; queue packing is not measured.
pub fn audit_owned_admission_with_heap_counter(
    read: fn() -> OwnedHeapCounters,
) -> Result<OwnedAdmissionAudit, Vst3OwnedError> {
    application_main::ui_authority()?;
    Ok(OwnedAdmissionAudit {
        maximum_distinct: run_case(false, read)?,
        maximum_repeated: run_case(true, read)?,
    })
}

#[derive(Debug)]
/// Counters for one queue-packing case.
pub struct OwnedQueuePackingCaseAudit {
    pub events: usize,
    pub queues: usize,
    pub comparisons: usize,
    pub sort_key_reads: usize,
    pub native_points: u64,
    pub native_digest: u64,
    pub instrument_heap: OwnedHeapCounters,
    pub effect_heap: OwnedHeapCounters,
    pub pcm_and_payload_verified: bool,
}
#[derive(Debug)]
/// Queue-packing audit across cases.
pub struct OwnedQueuePackingAudit {
    pub maximum_distinct: OwnedQueuePackingCaseAudit,
    pub maximum_repeated: OwnedQueuePackingCaseAudit,
}
fn expected_delivery_digest(repeated: bool) -> u64 {
    let mut digest = 14695981039346656037u64;
    for queue in 0..if repeated { 1 } else { 16384 } {
        for point in 0..if repeated { 1024 } else { 4 } {
            let id = if repeated { 100 } else { 1000 + queue };
            let offset = if repeated {
                if point == 0 {
                    0
                } else {
                    let j = point - 1;
                    if j < 255 {
                        1 + j / 17
                    } else {
                        16 + (j - 255) / 16
                    }
                }
            } else {
                point * 16
            };
            let value: f64 = if repeated {
                if point == 0 { 0.25 } else { 0.5 }
            } else {
                (4 - point) as f64 / 8.0
            };
            for item in [id as u64, offset as u64, value.to_bits()] {
                digest = (digest ^ item).wrapping_mul(1099511628211);
            }
        }
    }
    digest
}
fn run_packing_case(
    repeated: bool,
    read: fn() -> OwnedHeapCounters,
) -> Result<OwnedQueuePackingCaseAudit, Vst3OwnedError> {
    let token = application_main::ui_authority()?;
    let (fixture, loaded) = NativeFixture::load_with_fault(22, 0);
    let mut case = Case {
        fixture: ManuallyDrop::new(fixture),
        control: None,
    };
    loaded.map_err(Vst3OwnedError::Control)?;
    case.fixture
        .configure_parameters_for_test(if repeated { 9 } else { 8 });
    let instance = case
        .fixture
        .instance
        .take()
        .expect("loaded packing fixture");
    let owned = owned_lifecycle::adopt_native_fixture_with_budget(
        &token,
        instance,
        Vst3ProcessingBudget {
            events: 65536,
            points_per_parameter: if repeated { 1024 } else { 4 },
        },
    )?;
    let mut processor = owned.processor;
    case.control = Some(owned.control);
    if repeated {
        case.control
            .as_ref()
            .unwrap()
            .parameter_write_plan(Vst3NormalizedParameter {
                id: 100,
                value: 0.25,
            })
            .execute_on_ui()?;
    }
    let values: Vec<_> = (0..if repeated { 1023 } else { 65536 })
        .map(|index| Vst3NormalizedEvent {
            id: if repeated { 100 } else { 1000 + index % 16384 },
            sample_offset: if repeated {
                1 + index % 63
            } else {
                48 - (index / 16384) * 16
            },
            value: if repeated {
                0.5
            } else {
                (index / 16384 + 1) as f64 / 8.0
            },
        })
        .collect();
    let mut effect_values = Vec::with_capacity(if repeated { 1024 } else { 65536 });
    if repeated {
        effect_values.push(Vst3NormalizedEvent {
            id: 100,
            sample_offset: 0,
            value: 0.25,
        });
    }
    effect_values.extend(values.iter().copied());
    let (returned, instrument_heap, effect_heap, work, verified) = std::thread::spawn(move || {
        let _ = read();
        let mut output = [0.0; 128];
        let before = read();
        let instrument = processor.process_stereo_into(Vst3OwnedProcessBlock {
            frames: 64,
            transport: None,
            parameters: &values,
            input: None,
            sidechain: None,
            output: &mut output,
        });
        let instrument_heap = delta(read(), before);
        let expected = if repeated { 0.5 } else { 0.125 };
        let instrument_pcm = output
            .chunks_exact(2)
            .all(|pair| pair == [expected, expected * 2.0]);
        let input = [1.0; 128];
        let before = read();
        let effect = processor.process_stereo_into(Vst3OwnedProcessBlock {
            frames: 64,
            transport: None,
            parameters: &effect_values,
            input: Some(&input),
            sidechain: None,
            output: &mut output,
        });
        let effect_heap = delta(read(), before);
        let effect_pcm = output
            .chunks_exact(2)
            .all(|pair| pair == [expected, expected * 2.0]);
        let work = processor.packing_work_for_test();
        (
            processor,
            instrument_heap,
            effect_heap,
            work.ok(),
            instrument.is_ok() && effect.is_ok() && instrument_pcm && effect_pcm,
        )
    })
    .join()
    .map_err(|_| Vst3OwnedError::Control("Full packing audit worker panicked".into()))?;
    processor = returned;
    let work = work.ok_or(Vst3OwnedError::InvalidRequest(
        "Missing actual packing counters",
    ))?;
    let events = if repeated { 1024 } else { 65536 };
    let queues = if repeated { 7 } else { 16384 };
    let log = if repeated { 3 } else { 15 };
    require(
        verified && case.fixture.calls(1) == 2,
        "Full packing changed native PCM or refused maximum accepted data",
    )?;
    require(
        work.events == events
            && work.lookups == events
            && work.comparisons <= events * log
            && work.queue_resets == queues
            && work.queue_visits == queues
            && work.discovered == 0
            && work.sort_key_reads <= events * 640,
        "Packing exceeded bounded indexed lookup/sort work or discovered an unprepared queue",
    )?;
    require(
        instrument_heap == OwnedHeapCounters::default()
            && effect_heap == OwnedHeapCounters::default(),
        "Full maximum processing allocated, reallocated or freed host memory",
    )?;
    let native_points = case.fixture.delivery_for_test(1);
    let native_digest = case.fixture.delivery_for_test(2);
    require(
        case.fixture.delivery_for_test(0) == if repeated { 1 } else { 16384 }
            && native_points == events as u64
            && native_digest == expected_delivery_digest(repeated),
        "Native ABI did not receive complete ordered IDs/points/offsets/values",
    )?;
    drop(processor);
    drop(case.control.take());
    poll_owned_vst3_retirements()?;
    require(
        case.fixture.refs() == 0,
        "Packing owner did not retire on UI",
    )?;
    Ok(OwnedQueuePackingCaseAudit {
        events,
        queues,
        comparisons: work.comparisons,
        sort_key_reads: work.sort_key_reads,
        native_points,
        native_digest,
        instrument_heap,
        effect_heap,
        pcm_and_payload_verified: true,
    })
}

/// Maximum accepted blocks traverse admission, packing, native queue reads and
/// both processing branches using the real executable-main ownership protocol.
pub fn audit_owned_queue_packing_with_heap_counter(
    read: fn() -> OwnedHeapCounters,
) -> Result<OwnedQueuePackingAudit, Vst3OwnedError> {
    application_main::ui_authority()?;
    Ok(OwnedQueuePackingAudit {
        maximum_distinct: run_packing_case(false, read)?,
        maximum_repeated: run_packing_case(true, read)?,
    })
}
