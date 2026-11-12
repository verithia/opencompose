//! Actual worker batch ordering, atomic refusal and retained first native fault.
use super::*;
fn observe(detail: RuntimeDetail, field: u32) -> u64 {
    // SAFETY: The trusted fixture observer reads pinned static counters or its
    // current native thread identity, never a native instance or argument loan.
    unsafe { detail(field) }
}
fn counts(count: Count) -> [u64; 9] {
    std::array::from_fn(|point| {
        // SAFETY: Static fixture counters remain mapped through the image authority.
        unsafe { count(point as u32) }
    })
}
fn plan(fixture: &Fixture, names: Vec<String>, points: usize) -> Vst2RuntimePreparationPlan {
    Vst2RuntimePreparationPlan {
        plugin: Vst2OwnedAuthoredPlan {
            path: fixture.path.clone(),
            sample_rate: 48_000,
            max_frames: 64,
            state: None,
            parameters: Vec::new(),
        },
        parameter_names: names,
        max_parameter_points: points,
    }
}
fn native_unchanged(processor: &Vst2ProcessorLease) -> bool {
    // SAFETY: Called after a rejected synchronous batch on the only processing
    // owner; registry retention and the absence of native calls preserve backing.
    let native = unsafe { &*processor.link.native.as_ptr() };
    native.pending_midi.len() == 1
        && native.pending_midi[0].status == 0xc2
        && native.pending_midi[0].data1 == 65
        && native.host.value(0) == Some(0.5)
        && native.last_time_info.sample_pos == 0.0
}
pub(super) fn audit(
    fixture: &Fixture,
    case: &str,
    heap: fn() -> Vst2HeapCounters,
) -> Result<Vst2NativeGuardAudit, Vst2OwnedError> {
    let mode = match case {
        "runtime-batch-structure" => 2,
        "runtime-batch-setter-identity" => 3,
        "runtime-batch-no-setter" | "runtime-batch-no-setter-binding" => 4,
        _ => 1,
    };
    // SAFETY: Static fixture configuration is set on UI before native acquisition.
    unsafe { (fixture.runtime_mode)(mode, 1024) };
    if case == "runtime-batch-no-setter-binding" {
        let result = plan(fixture, vec!["Gain".into()], 4).execute_on_ui();
        return Ok(Vst2NativeGuardAudit {
            exact_first_cause: matches!(result,Err(Vst2OwnedError::Parameter(error)) if matches!(error.kind,crate::preset_parameters::ParameterErrorKind::MissingCallback)),
            first_report_once: true,
            no_later_native_calls: fixture.count(3) == 0 && fixture.count(5) == 0,
            payload_retained: fixture.live() == 0 && fixture.count(8) == 1,
            loans_retained: true,
            process_heap: Vst2HeapCounters::default(),
            slot_isolation: true,
        });
    }
    let no_setter = case == "runtime-batch-no-setter";
    let names = if no_setter {
        Vec::new()
    } else {
        vec!["Gain".into()]
    };
    let extra = if case == "runtime-batch-wrong-source" {
        Some(plan(fixture, names.clone(), 4).execute_on_ui()?)
    } else {
        None
    };
    let foreign_key = extra
        .as_ref()
        .and_then(|instance| instance.runtime_parameters.first())
        .map(|binding| binding.key);
    let instance = plan(fixture, names, if no_setter { 0 } else { 4 }).execute_on_ui()?;
    let key = instance
        .runtime_parameters
        .first()
        .map(|binding| binding.key);
    let Vst2OwnedInstance {
        mut processor,
        control,
        snapshot,
        runtime_parameters,
    } = instance;
    drop(control);
    drop(snapshot);
    drop(runtime_parameters);
    let failure = processor.link.failure.clone();
    if case == "runtime-batch-setter-fault" {
        fixture.configure(3, 0, 1, 0, 0);
    }
    if case == "runtime-batch-events-fault" {
        fixture.configure(2, 25, 1, 0, 0);
    }
    let count = fixture.count;
    let detail = fixture.runtime_detail;
    let preflight = case == "runtime-batch-preflight";
    let wrong_source = case == "runtime-batch-wrong-source";
    let native_fault = matches!(
        case,
        "runtime-batch-setter-fault" | "runtime-batch-events-fault"
    );
    let structure = matches!(
        case,
        "runtime-batch-structure" | "runtime-batch-setter-identity"
    );
    let ui_thread = observe(detail, 0);
    let worker = std::thread::spawn(move || {
        let worker_thread = observe(detail, 0);
        let _ = heap();
        let clock = PluginTransport::new(1024, 48_000, 120.0);
        let mut output = [0.333; 128];
        let midi = [
            Vst2MidiMessage {
                status: 0xc2,
                data1: 65,
                data2: 0,
                offset: 0,
            },
            Vst2MidiMessage {
                status: 0x92,
                data1: 60,
                data2: 100,
                offset: 63,
            },
        ];
        let points = key.map(|key| {
            [
                Vst2ParameterPoint { key, value: 0.25 },
                Vst2ParameterPoint { key, value: 0.75 },
            ]
        });
        if preflight {
            processor.queue_midi(0xc2, 65, 0, 0)?;
        }
        let before = heap();
        let mut correct = true;
        if preflight {
            let key = key.unwrap();
            let native_before = counts(count);
            for value in [f64::NAN, -0.01, 1.01] {
                let bad = [
                    Vst2ParameterPoint { key, value: 0.25 },
                    Vst2ParameterPoint { key, value },
                ];
                let result = processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &bad,
                        midi: &midi,
                    },
                );
                correct &= matches!(
                    result,
                    Err(Vst2OwnedError::Runtime(
                        Vst2RuntimeError::NonFinite | Vst2RuntimeError::OutOfRange
                    ))
                ) && output == [0.333; 128]
                    && native_unchanged(&processor)
                    && counts(count) == native_before;
            }
            let too_many = [Vst2ParameterPoint { key, value: 0.25 }; 5];
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output[..0],
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &too_many[..1],
                        midi: &[]
                    }
                ),
                Err(Vst2OwnedError::Runtime(Vst2RuntimeError::EmptyBlock))
            );
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &too_many,
                        midi: &[]
                    }
                ),
                Err(Vst2OwnedError::Runtime(Vst2RuntimeError::Capacity))
            );
            let invalid = [
                midi[0],
                Vst2MidiMessage {
                    status: 0xf8,
                    ..midi[1]
                },
            ];
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &[],
                        midi: &invalid
                    }
                ),
                Err(Vst2OwnedError::Midi(Vst2MidiError::Status))
            );
            let invalid_data = [
                midi[0],
                Vst2MidiMessage {
                    data2: 128,
                    ..midi[1]
                },
            ];
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &[],
                        midi: &invalid_data
                    }
                ),
                Err(Vst2OwnedError::Midi(Vst2MidiError::DataByte))
            );
            let invalid_unused = [Vst2MidiMessage {
                status: 0xc2,
                data1: 65,
                data2: 1,
                offset: 0,
            }];
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &[],
                        midi: &invalid_unused
                    }
                ),
                Err(Vst2OwnedError::Midi(Vst2MidiError::UnusedDataByte))
            );
            let full = [midi[0]; MAX_PENDING_MIDI];
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &[],
                        midi: &full
                    }
                ),
                Err(Vst2OwnedError::Midi(Vst2MidiError::Capacity))
            );
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output[..64],
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &[],
                        midi: &midi[1..]
                    }
                ),
                Err(Vst2OwnedError::Midi(Vst2MidiError::Offset))
            );
            let mut bad_meter = clock.clone();
            bad_meter.time_sig_numerator = 0;
            for bad in [
                PluginTransport::new(1024, 44_100, 120.0),
                PluginTransport::new(1024, 0, 120.0),
                PluginTransport::new(1024, 48_000, f64::NAN),
                PluginTransport::new(u64::MAX, 48_000, 120.0),
                bad_meter,
            ] {
                correct &= matches!(
                    processor.process_instrument_batch_into(
                        &mut output,
                        Some(&bad),
                        Vst2BlockBatch {
                            parameters: &[],
                            midi: &[]
                        }
                    ),
                    Err(Vst2OwnedError::Clock(_))
                );
            }
            correct &= output == [0.333; 128]
                && native_unchanged(&processor)
                && counts(count) == native_before;
        }
        if let Some(foreign) = foreign_key {
            let bad = [Vst2ParameterPoint {
                key: foreign,
                value: 0.75,
            }];
            let before = counts(count);
            correct &= matches!(
                processor.process_instrument_batch_into(
                    &mut output,
                    Some(&clock),
                    Vst2BlockBatch {
                        parameters: &bad,
                        midi: &midi
                    }
                ),
                Err(Vst2OwnedError::Runtime(Vst2RuntimeError::WrongSource))
            ) && counts(count) == before
                && output == [0.333; 128];
        }
        let result = processor.process_instrument_batch_into(
            &mut output,
            Some(&clock),
            Vst2BlockBatch {
                parameters: points.as_ref().map_or(&[], |p| p.as_slice()),
                midi: if preflight { &midi[1..] } else { &midi },
            },
        );
        if native_fault {
            correct &= matches!(result, Err(Vst2OwnedError::Faulted(_))) && output == [0.0; 128];
        } else if structure {
            correct &= matches!(
                result,
                Err(Vst2OwnedError::Runtime(Vst2RuntimeError::InventoryChanged))
            ) && output == [0.0; 128];
            let before = counts(count);
            correct &= matches!(
                processor.process_instrument_into(&mut output, Some(&clock)),
                Err(Vst2OwnedError::Runtime(Vst2RuntimeError::InventoryChanged))
            ) && counts(count) == before;
        } else {
            correct &= result.is_ok() && output == [if no_setter { 0.125 } else { 0.1875 }; 128];
            let trace = observe(detail, 2);
            let checks = observe(detail, 3);
            correct &= trace == if no_setter { 0x89 } else { 0x1189 }
                && checks == if no_setter { 4 } else { 7 }
                && observe(detail, 4) == worker_thread
                && worker_thread != ui_thread;
            if !no_setter {
                correct &= observe(detail, 1) == worker_thread;
            }
            // Display-only notifications occurred in both setter and MIDI paths. A
            // subsequent ordinary block must retain the same immutable numeric authority.
            correct &= processor
                .process_instrument_batch_into(&mut output, Some(&clock), Vst2BlockBatch::default())
                .is_ok()
                && output == [if no_setter { 0.125 } else { 0.1875 }; 128];
            output[64..].fill(-0.5);
            correct &= processor
                .process_instrument_batch_into(
                    &mut output[..64],
                    Some(&clock),
                    Vst2BlockBatch::default(),
                )
                .is_ok()
                && output[..64]
                    .iter()
                    .all(|sample| *sample == if no_setter { 0.125 } else { 0.1875 })
                && output[64..].iter().all(|sample| *sample == -0.5);
        }
        drop(processor);
        let after = heap();
        Ok::<_, Vst2OwnedError>((
            correct,
            Vst2HeapCounters {
                allocations: after.allocations - before.allocations,
                reallocations: after.reallocations - before.reallocations,
                deallocations: after.deallocations - before.deallocations,
            },
        ))
    });
    let (mut correct, process_heap) = worker
        .join()
        .map_err(|_| Vst2OwnedError::Control("Runtime native audit worker panicked".into()))??;
    let mut first_report_once = true;
    if native_fault {
        let expected = if case == "runtime-batch-setter-fault" {
            Vst2Operation::SetParameter(0)
        } else {
            Vst2Operation::Dispatcher(25)
        };
        let record = failure
            .first_fault()
            .ok_or_else(|| Vst2OwnedError::Control("Runtime first cause missing".into()))?;
        correct &= record.operation == expected && record.fault.kind == FaultKind::CppException;
        first_report_once =
            failure.take_report() == Some(record) && failure.take_report().is_none();
        correct &= fixture.count(5) == 0;
        if case == "runtime-batch-setter-fault" {
            correct &= fixture.count(3) == 1 && fixture.count(2) > 0 && observe(detail, 2) == 1;
        }
    }
    drop(extra);
    let before = fixture.counts();
    poll_owned_vst2_retirements()?;
    let payload_retained = if native_fault {
        fixture.live() == 1
            && fixture.count(8) == 0
            && callback_route::occupied_budget_for_audit() == (1, 1024)
    } else {
        fixture.live() == 0 && fixture.count(8) == if wrong_source { 2 } else { 1 }
    };
    let no_later_native_calls = !native_fault || before == fixture.counts();
    let loans_retained = if case == "runtime-batch-events-fault" {
        // SAFETY: The static observer traverses the retained host event packet and
        // both distinct MIDI events after worker drop and failed UI retirement.
        (unsafe { (fixture.touch)() }) >= 3
    } else {
        true
    };
    Ok(Vst2NativeGuardAudit {
        exact_first_cause: correct,
        first_report_once,
        no_later_native_calls,
        payload_retained,
        loans_retained,
        process_heap,
        slot_isolation: true,
    })
}
