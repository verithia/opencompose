//! Real UI preparation and native payload proof for owned MIDI processing.
use super::super::native_fixture::{NativeFixture, OwnedHeapCounters};
use super::preparation::InitialParameters;
use super::*;
use midi::{Vst3MidiEvent as E, Vst3MidiMessage as M};

#[derive(Debug)]
/// Audit results for MIDI ingress.
pub struct Vst3MidiIngressAudit {
    pub native_event_payloads: usize,
    pub native_parameter_points: usize,
    pub atomic_refusals: usize,
    pub metadata_refusals: usize,
    pub active_note_capacity: usize,
    pub heap_operations: OwnedHeapCounters,
}
struct Fixture {
    native: ManuallyDrop<NativeFixture>,
    owned: Option<Vst3OwnedInstance>,
}
impl Fixture {
    fn new(mode: u32, budget: Vst3ProcessingBudget) -> Result<Self, Vst3OwnedError> {
        let token = application_main::ui_authority()?;
        let (native, candidate) =
            super::authored_preparation_audit::fixture_with_mode(&token, mode)?;
        let mut candidate = candidate;
        if mode == 19 {
            candidate.native.reload_output_layout();
        }
        let prepared = preparation::complete_candidate(
            &token,
            candidate,
            None,
            InitialParameters::Normalized(vec![]),
        )?;
        let owned = preparation::publish_runtime(&token, prepared, budget)?;
        Ok(Self {
            native: ManuallyDrop::new(native),
            owned: Some(owned),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.owned.take();
        if poll_owned_vst3_retirements().is_ok() {
            // SAFETY: every worker has joined and external leases dropped before
            // UI registry retirement; the controlled native allocation is inert.
            unsafe { ManuallyDrop::drop(&mut self.native) };
        }
    }
}
fn event(offset: u32, message: M) -> E {
    E {
        channel: 2,
        sample_offset: offset,
        message,
    }
}
fn submit(
    fixture: &mut Fixture,
    events: &[E],
    parameters: &[Vst3NormalizedEvent],
    input: bool,
    heap: fn() -> OwnedHeapCounters,
) -> (Result<(), Vst3ProcessLeaseError>, [f32; 256]) {
    let processor = &mut fixture.owned.as_mut().unwrap().processor;
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let mut output = [81.0; 256];
                let source = [0.0; 256];
                let before = heap();
                let result = processor.process_stereo_with_midi_into(
                    Vst3OwnedProcessBlock {
                        frames: 128,
                        transport: None,
                        parameters,
                        input: input.then_some(source.as_slice()),
                        sidechain: None,
                        output: &mut output,
                    },
                    events,
                );
                let after = heap();
                assert_eq!(
                    after, before,
                    "owned MIDI ingress/process performed heap operations"
                );
                (result, output)
            })
            .join()
            .unwrap()
    })
}
/// Called by the existing literal-main audit with its worker-local allocator counter.
pub fn audit_owned_midi_ingress(
    heap: fn() -> OwnedHeapCounters,
) -> Result<Vst3MidiIngressAudit, Vst3OwnedError> {
    application_main::ui_authority()?;
    use std::mem::{align_of, offset_of, size_of};
    let layout = [
        size_of::<NoteExpressionTypeInfo>(),
        align_of::<NoteExpressionTypeInfo>(),
        offset_of!(NoteExpressionTypeInfo, valueDesc),
        offset_of!(NoteExpressionTypeInfo, flags),
        size_of::<vst3::Steinberg::Vst::NoteExpressionValueDescription>(),
        offset_of!(
            vst3::Steinberg::Vst::NoteExpressionValueDescription,
            stepCount
        ),
        offset_of!(NoteOnEvent, velocity),
        offset_of!(NoteOffEvent, noteId),
        offset_of!(vst3::Steinberg::Vst::PolyPressureEvent, noteId),
    ];
    for (index, expected) in layout.into_iter().enumerate() {
        assert_eq!(NativeFixture::midi_layout_for_test(index as u32), expected);
    }
    audit_multi_outputs(heap)?;
    audit_latency_restart(heap)?;
    let budget = Vst3ProcessingBudget {
        events: 16,
        points_per_parameter: 2,
    };
    let mut fixture = Fixture::new(11, budget)?;
    assert_eq!(fixture.native.calls(43), 16 * 131);
    assert_eq!(fixture.native.calls(44), 16);
    assert_eq!(fixture.native.calls(45), 16 * 6);
    let metadata = (
        fixture.native.calls(43),
        fixture.native.calls(44),
        fixture.native.calls(45),
    );
    let messages = [
        event(
            0,
            M::NoteOn {
                key: 60,
                velocity: 100,
            },
        ),
        event(
            1,
            M::NoteOn {
                key: 64,
                velocity: 127,
            },
        ),
        event(
            2,
            M::NoteOn {
                key: 60,
                velocity: 80,
            },
        ),
        event(
            3,
            M::PolyPressure {
                key: 60,
                pressure: 64,
            },
        ),
        event(
            4,
            M::NoteExpression {
                key: 60,
                kind: Vst3StandardNoteExpression::Tuning,
                value: Vst3NoteExpressionValue::TuningSemitones(12.0),
            },
        ),
        event(
            5,
            M::NoteOff {
                key: 60,
                velocity: 32,
            },
        ),
        event(
            6,
            M::NoteExpression {
                key: 60,
                kind: Vst3StandardNoteExpression::Volume,
                value: Vst3NoteExpressionValue::Normalized(0.25),
            },
        ),
        event(
            7,
            M::NoteOff {
                key: 60,
                velocity: 96,
            },
        ),
        event(
            8,
            M::NoteOff {
                key: 64,
                velocity: 0,
            },
        ),
        event(
            10,
            M::ControlChange {
                controller: 1,
                value: 64,
            },
        ),
        event(
            11,
            M::ControlChange14 {
                controller: 1,
                value: 0x1234,
            },
        ),
        event(12, M::PitchBend { value: 0 }),
        event(13, M::ChannelPressure { pressure: 96 }),
        event(14, M::ProgramChange { program: 7 }),
    ];
    let (result, output) = submit(&mut fixture, &messages, &[], false, heap);
    assert!(result.is_ok());
    assert_ne!(output, [81.0; 256]);
    assert_eq!(fixture.native.midi_word_for_test(0, 0, 0), 9);
    assert_eq!(fixture.native.midi_word_for_test(1, 0, 0), 6);
    let note_ids = [0, 1, 2, 2, 2, 2, 0, 0, 1];
    let types = [0, 0, 0, 3, 4, 1, 4, 1, 1];
    for (index, (id, kind)) in note_ids.into_iter().zip(types).enumerate() {
        assert_eq!(fixture.native.midi_word_for_test(2, index as u32, 0), kind);
        let id_field = if kind == 4 { 3 } else { 4 };
        assert_eq!(
            fixture.native.midi_word_for_test(2, index as u32, id_field),
            id
        );
        assert_eq!(
            fixture.native.midi_word_for_test(2, index as u32, 1),
            index as u64
        );
    }
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 4, 4)),
        0.55
    );
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 6, 4)),
        0.25
    );
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 5, 5)),
        f64::from(32.0f32 / 127.0)
    );
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 7, 5)),
        f64::from(96.0f32 / 127.0)
    );
    let expected = [
        (100, 10, 64.0 / 127.0),
        (100, 11, 36.0 / 127.0),
        (101, 11, 52.0 / 127.0),
        (103, 13, 96.0 / 127.0),
        (104, 12, 8192.0 / 16383.0),
        (107, 14, 7.0 / 15.0),
    ];
    for (index, (id, offset, value)) in expected.into_iter().enumerate() {
        assert_eq!(fixture.native.midi_word_for_test(3, index as u32, 0), id);
        assert_eq!(
            fixture.native.midi_word_for_test(3, index as u32, 1),
            offset
        );
        assert_eq!(
            f64::from_bits(fixture.native.midi_word_for_test(3, index as u32, 2)),
            value
        );
    }
    assert_eq!(
        metadata,
        (
            fixture.native.calls(43),
            fixture.native.calls(44),
            fixture.native.calls(45)
        )
    );
    let mut atomic_refusals = 0;
    for message in [
        M::ControlChange {
            controller: 99,
            value: 10,
        },
        M::ControlChange {
            controller: 7,
            value: 10,
        },
        M::PitchBend { value: 8192 },
        M::ProgramChange { program: 16 },
        M::ControlChange14 {
            controller: 32,
            value: 1,
        },
    ] {
        let before = fixture.native.calls(1);
        let (result, output) = submit(
            &mut fixture,
            &[
                event(
                    0,
                    M::NoteOn {
                        key: 67,
                        velocity: 100,
                    },
                ),
                event(1, message),
            ],
            &[],
            false,
            heap,
        );
        assert!(result.is_err());
        assert_eq!(output, [81.0; 256]);
        assert_eq!(fixture.native.calls(1), before);
        atomic_refusals += 1;
    }
    let (result, _) = submit(
        &mut fixture,
        &[
            event(
                0,
                M::NoteOn {
                    key: 67,
                    velocity: 0,
                },
            ),
            event(
                1,
                M::NoteOff {
                    key: 67,
                    velocity: 127,
                },
            ),
        ],
        &[],
        false,
        heap,
    );
    assert!(result.is_ok());
    assert_eq!(
        fixture.native.midi_word_for_test(2, 0, 4),
        3,
        "refused blocks must not consume note identities"
    );
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 0, 5)),
        0.0
    );
    assert_eq!(
        f64::from_bits(fixture.native.midi_word_for_test(2, 1, 5)),
        1.0
    );
    let (result, output) = submit(&mut fixture, &messages[..1], &[], true, heap);
    assert!(matches!(result, Err(Vst3ProcessLeaseError::InvalidBlock)));
    assert_eq!(output, [81.0; 256]);
    atomic_refusals += 1;
    let (result, output) = submit(
        &mut fixture,
        &[event(
            0,
            M::NoteExpression {
                key: 60,
                kind: Vst3StandardNoteExpression::Volume,
                value: Vst3NoteExpressionValue::Normalized(0.5),
            },
        )],
        &[],
        false,
        heap,
    );
    assert!(matches!(result, Err(Vst3ProcessLeaseError::UnknownNote)));
    assert_eq!(output, [81.0; 256]);
    atomic_refusals += 1;
    let invalid_note = event(
        0,
        M::NoteOn {
            key: 67,
            velocity: 100,
        },
    );
    let mut bad_channel = invalid_note;
    bad_channel.channel = 16;
    let mut bad_offset = invalid_note;
    bad_offset.sample_offset = 128;
    for invalid in [
        vec![bad_channel],
        vec![bad_offset],
        vec![event(
            0,
            M::NoteOn {
                key: 128,
                velocity: 100,
            },
        )],
        vec![event(
            0,
            M::NoteOn {
                key: 67,
                velocity: 128,
            },
        )],
        vec![event(
            0,
            M::ControlChange14 {
                controller: 1,
                value: 16384,
            },
        )],
        vec![
            invalid_note,
            event(
                1,
                M::NoteExpression {
                    key: 67,
                    kind: Vst3StandardNoteExpression::Tuning,
                    value: Vst3NoteExpressionValue::TuningSemitones(121.0),
                },
            ),
        ],
        vec![
            event(
                1,
                M::NoteOn {
                    key: 67,
                    velocity: 100,
                },
            ),
            event(
                0,
                M::NoteOff {
                    key: 67,
                    velocity: 0,
                },
            ),
        ],
    ] {
        let before = fixture.native.calls(1);
        let (result, output) = submit(&mut fixture, &invalid, &[], false, heap);
        assert!(matches!(result, Err(Vst3ProcessLeaseError::InvalidBlock)));
        assert_eq!(output, [81.0; 256]);
        assert_eq!(fixture.native.calls(1), before);
        atomic_refusals += 1;
    }
    drop(fixture);
    for (case, (mode, budget, params)) in [
        (14, budget, vec![]),
        (
            11,
            Vst3ProcessingBudget {
                events: 1,
                points_per_parameter: 2,
            },
            vec![],
        ),
        (
            11,
            Vst3ProcessingBudget {
                events: 3,
                points_per_parameter: 1,
            },
            vec![Vst3NormalizedEvent {
                id: 100,
                value: 0.2,
                sample_offset: 0,
            }],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut fixture = Fixture::new(mode, budget)?;
        let before = fixture.native.calls(1);
        let (result, output) = submit(
            &mut fixture,
            &[event(
                0,
                M::ControlChange14 {
                    controller: 1,
                    value: 1,
                },
            )],
            &params,
            false,
            heap,
        );
        if case == 0 {
            assert!(matches!(
                result,
                Err(Vst3ProcessLeaseError::UnsupportedMidi)
            ));
        } else {
            assert!(matches!(result, Err(Vst3ProcessLeaseError::EventCapacity)));
        }
        assert_eq!(output, [81.0; 256]);
        assert_eq!(fixture.native.calls(1), before);
        atomic_refusals += 1;
    }
    let mut exact = Fixture::new(
        11,
        Vst3ProcessingBudget {
            events: 2,
            points_per_parameter: 1,
        },
    )?;
    assert!(
        submit(
            &mut exact,
            &[event(
                0,
                M::ControlChange14 {
                    controller: 1,
                    value: 16383
                }
            )],
            &[],
            false,
            heap
        )
        .0
        .is_ok()
    );
    drop(exact);
    let capacity = 2048;
    let mut capacity_fixture = Fixture::new(
        11,
        Vst3ProcessingBudget {
            events: capacity,
            points_per_parameter: 1,
        },
    )?;
    let notes = vec![
        event(
            0,
            M::NoteOn {
                key: 60,
                velocity: 1
            }
        );
        capacity
    ];
    assert!(
        submit(&mut capacity_fixture, &notes, &[], false, heap)
            .0
            .is_ok()
    );
    let (result, output) = submit(&mut capacity_fixture, &notes[..1], &[], false, heap);
    assert!(matches!(result, Err(Vst3ProcessLeaseError::NoteCapacity)));
    assert_eq!(output, [81.0; 256]);
    atomic_refusals += 1;
    assert!(
        submit(
            &mut capacity_fixture,
            &[
                event(
                    0,
                    M::NoteOff {
                        key: 60,
                        velocity: 5
                    }
                ),
                event(
                    1,
                    M::NoteOn {
                        key: 61,
                        velocity: 2
                    }
                )
            ],
            &[],
            false,
            heap
        )
        .0
        .is_ok()
    );
    assert_eq!(capacity_fixture.native.midi_word_for_test(2, 0, 4), 2047);
    assert_eq!(capacity_fixture.native.midi_word_for_test(2, 1, 4), 2048);
    drop(capacity_fixture);
    for mode in [15, 16] {
        let mut bus = Fixture::new(mode, budget)?;
        let expected_channels = if mode == 15 { 0 } else { 2 };
        assert_eq!(bus.native.calls(43), expected_channels * 131);
        assert_eq!(bus.native.calls(44), expected_channels);
        let (result, output) = submit(&mut bus, &messages[..1], &[], false, heap);
        assert!(matches!(
            result,
            Err(Vst3ProcessLeaseError::UnsupportedMidi)
        ));
        assert_eq!(output, [81.0; 256]);
        atomic_refusals += 1;
        // Empty MIDI and parameter-only calls must not depend on an event input.
        let parameter = [Vst3NormalizedEvent {
            id: 100,
            value: 0.25,
            sample_offset: 0,
        }];
        assert!(submit(&mut bus, &[], &parameter, false, heap).0.is_ok());
        if mode == 16 {
            let mut admitted = event(
                0,
                M::NoteOn {
                    key: 60,
                    velocity: 127,
                },
            );
            admitted.channel = 1;
            assert!(submit(&mut bus, &[admitted], &[], false, heap).0.is_ok());
            assert_eq!(bus.native.midi_word_for_test(2, 0, 2), 1);
        }
    }
    let mut metadata_refusals = 0;
    for flags in [
        RestartFlags_::kMidiCCAssignmentChanged,
        RestartFlags_::kNoteExpressionChanged,
    ] {
        let mut fixture = Fixture::new(11, budget)?;
        fixture.native.parameter_restart_for_test(37, flags);
        assert!(
            fixture
                .owned
                .as_ref()
                .unwrap()
                .control
                .metadata_plan()
                .execute_on_ui()
                .is_err()
        );
        let before = fixture.native.calls(1);
        let (result, output) = submit(&mut fixture, &messages[..1], &[], false, heap);
        assert!(matches!(
            result,
            Err(Vst3ProcessLeaseError::InventoryChanged)
        ));
        assert_eq!(output, [81.0; 256]);
        assert_eq!(fixture.native.calls(1), before);
        metadata_refusals += 1;
    }
    let mut changed_during_process = Fixture::new(11, budget)?;
    changed_during_process
        .native
        .parameter_restart_for_test(1, RestartFlags_::kNoteExpressionChanged);
    let (result, output) = submit(
        &mut changed_during_process,
        &messages[..1],
        &[],
        false,
        heap,
    );
    assert!(matches!(
        result,
        Err(Vst3ProcessLeaseError::InventoryChanged)
    ));
    assert_eq!(
        output, [0.0; 256],
        "native generation changes must not publish stale audio"
    );
    metadata_refusals += 1;
    drop(changed_during_process);
    let token = application_main::ui_authority()?;
    for mode in [12, 13, 17, 18] {
        let (native, candidate) =
            super::authored_preparation_audit::fixture_with_mode(&token, mode)?;
        let prepared = preparation::complete_candidate(
            &token,
            candidate,
            None,
            InitialParameters::Normalized(vec![]),
        )?;
        assert!(preparation::publish_runtime(&token, prepared, budget).is_err());
        assert_eq!(native.refs(), 0);
        metadata_refusals += 1;
    }
    for point in [43, 44, 45] {
        let (native, candidate) = super::authored_preparation_audit::fixture_with_mode(&token, 11)?;
        let prepared = preparation::complete_candidate(
            &token,
            candidate,
            None,
            InitialParameters::Normalized(vec![]),
        )?;
        native.set_fault(point, 1);
        assert!(matches!(
            preparation::publish_runtime(&token, prepared, budget),
            Err(Vst3OwnedError::Native(_))
        ));
        assert!(native.refs() > 0);
        metadata_refusals += 1;
    }
    let (native, candidate) = super::authored_preparation_audit::fixture_with_mode(&token, 11)?;
    let prepared = preparation::complete_candidate(
        &token,
        candidate,
        None,
        InitialParameters::Normalized(vec![]),
    )?;
    native.parameter_restart_for_test(43, RestartFlags_::kMidiCCAssignmentChanged);
    assert!(preparation::publish_runtime(&token, prepared, budget).is_err());
    assert_eq!(native.calls(43), 1);
    assert_eq!(native.refs(), 0);
    metadata_refusals += 1;
    Ok(Vst3MidiIngressAudit {
        native_event_payloads: 9,
        native_parameter_points: 6,
        atomic_refusals,
        metadata_refusals,
        active_note_capacity: capacity,
        heap_operations: OwnedHeapCounters::default(),
    })
}

fn audit_multi_outputs(heap: fn() -> OwnedHeapCounters) -> Result<(), Vst3OwnedError> {
    let mut fixture = Fixture::new(
        19,
        Vst3ProcessingBudget {
            events: 128,
            points_per_parameter: 128,
        },
    )?;
    let owned = fixture.owned.as_mut().unwrap();
    assert_eq!(
        owned
            .snapshot
            .outputs
            .iter()
            .map(|p| p.channels)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let captures = fixture.native.calls(37);
    let ids = owned
        .control
        .normalized_bindings_plan(vec!["Gain".into(), "Gain Amount".into(), "Gain".into()])
        .execute_on_ui()?;
    assert_eq!(ids, [100, 100, 100]);
    assert_eq!(
        fixture.native.calls(37),
        captures + 1,
        "one inventory capture per finite binding batch"
    );
    for name in ["missing", "Read Only", "Rate Hz", "明るさ"] {
        assert!(
            owned
                .control
                .normalized_bindings_plan(vec![name.into()])
                .execute_on_ui()
                .is_err(),
            "{name}"
        );
    }
    let base = PluginTransport::new(0, 48_000, 120.0);
    let mut zero = base.clone();
    zero.sample_rate = 0;
    let mismatch = PluginTransport::new(0, 44_100, 120.0);
    let mut nan = base.clone();
    nan.tempo = f64::NAN;
    let mut meter = base.clone();
    meter.time_sig_numerator = 0;
    let mut denominator = base.clone();
    denominator.time_sig_denominator = 0;
    let overflow = PluginTransport::new(u64::MAX - 127, 48_000, 120.0);
    let signed_overflow = PluginTransport::new(i64::MAX as u64 - 127, 48_000, 120.0);
    let above_signed = PluginTransport::new(i64::MAX as u64 + 1, 48_000, 120.0);
    let bad_clocks = [
        zero,
        mismatch,
        nan,
        meter,
        denominator,
        overflow,
        signed_overflow,
        above_signed,
    ];
    // Invalid contents make capacity-first refusal observable: neither path
    // may inspect even the first element before its exact count is admitted.
    let oversized_midi = [Vst3MidiEvent {
        channel: 255,
        sample_offset: u32::MAX,
        message: M::NoteOn {
            key: 255,
            velocity: 255,
        },
    }; 129];
    let oversized_ordered = [Vst3OrderedEvent::Parameter(Vst3NormalizedEvent {
        id: u32::MAX,
        value: f64::NAN,
        sample_offset: u32::MAX,
    }); 129];
    let before_process = fixture.native.calls(1);
    let mut outputs = vec![vec![81.0; 256]; 3];
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let processor = &mut owned.processor;
                let before = heap();
                for ordered in [false, true] {
                    let block = Vst3OwnedInstrumentBlock {
                        frames: 128,
                        transport: None,
                        parameters: &[],
                        output_pairs: &[1, 0, 1],
                        outputs: &mut outputs,
                    };
                    let refused = if ordered {
                        processor
                            .process_outputs_with_ordered_events_into(block, &oversized_ordered)
                    } else {
                        processor.process_outputs_with_midi_into(block, &oversized_midi)
                    };
                    assert!(
                        matches!(refused, Err(Vst3ProcessLeaseError::EventCapacity)),
                        "oversized input must refuse before inspecting invalid elements"
                    );
                    assert!(outputs.iter().flatten().all(|value| *value == 81.0));
                    assert_eq!(heap(), before, "oversized admission allocated");
                }
                for (index, clock) in bad_clocks.iter().enumerate() {
                    let refusal = processor.process_outputs_with_midi_into(
                        Vst3OwnedInstrumentBlock {
                            frames: 128,
                            transport: Some(clock),
                            parameters: &[Vst3NormalizedEvent {
                                id: 100,
                                value: 0.5,
                                sample_offset: 0,
                            }],
                            output_pairs: &[1, 0, 1],
                            outputs: &mut outputs,
                        },
                        &[event(
                            0,
                            M::NoteOn {
                                key: 60,
                                velocity: 100,
                            },
                        )],
                    );
                    match (index, refusal) {
                        (
                            0,
                            Err(Vst3ProcessLeaseError::Clock(
                                oc_audio::TransportBlockError::InvalidSampleRate,
                            )),
                        )
                        | (
                            1,
                            Err(Vst3ProcessLeaseError::Clock(
                                oc_audio::TransportBlockError::SampleRateMismatch {
                                    prepared: 48_000,
                                    transport: 44_100,
                                },
                            )),
                        )
                        | (
                            2,
                            Err(Vst3ProcessLeaseError::Clock(
                                oc_audio::TransportBlockError::InvalidTempo,
                            )),
                        )
                        | (
                            3 | 4,
                            Err(Vst3ProcessLeaseError::Clock(
                                oc_audio::TransportBlockError::InvalidMeter,
                            )),
                        )
                        | (
                            5,
                            Err(Vst3ProcessLeaseError::Clock(
                                oc_audio::TransportBlockError::Range(_),
                            )),
                        )
                        | (6 | 7, Err(Vst3ProcessLeaseError::NativeSampleRange { .. })) => {}
                        (index, result) => panic!("clock {index} returned {result:?}"),
                    }
                    assert!(
                        outputs.iter().flatten().all(|value| *value == 81.0),
                        "bad clock changed output"
                    );
                    assert_eq!(heap(), before, "clock admission allocated");
                }
                for pairs in [&[][..], &[2][..]] {
                    let count = pairs.len();
                    let refusal = processor.process_outputs_with_midi_into(
                        Vst3OwnedInstrumentBlock {
                            frames: 128,
                            transport: None,
                            parameters: &[],
                            output_pairs: pairs,
                            outputs: &mut outputs[..count],
                        },
                        &[],
                    );
                    assert!(matches!(refusal, Err(Vst3ProcessLeaseError::InvalidBlock)));
                }
                outputs[0].pop();
                let refusal = processor.process_outputs_with_midi_into(
                    Vst3OwnedInstrumentBlock {
                        frames: 128,
                        transport: None,
                        parameters: &[],
                        output_pairs: &[0],
                        outputs: &mut outputs[..1],
                    },
                    &[event(
                        0,
                        M::NoteOn {
                            key: 60,
                            velocity: 100,
                        },
                    )],
                );
                assert!(matches!(refusal, Err(Vst3ProcessLeaseError::InvalidBlock)));
                outputs[0].push(81.0);
                assert!(outputs.iter().flatten().all(|v| *v == 81.0));
                assert_eq!(heap(), before, "invalid pair admission allocated");
                let result = processor.process_outputs_with_midi_into(
                    Vst3OwnedInstrumentBlock {
                        frames: 128,
                        transport: Some(&PluginTransport::new(
                            i64::MAX as u64 - 128,
                            48_000,
                            120.0,
                        )),
                        parameters: &[],
                        output_pairs: &[1, 0, 1],
                        outputs: &mut outputs,
                    },
                    &[event(
                        0,
                        M::NoteOn {
                            key: 60,
                            velocity: 100,
                        },
                    )],
                );
                assert_eq!(heap(), before, "multi-output processing allocated");
                assert!(
                    result.is_ok(),
                    "multi-output native processing failed: {result:?}"
                );
            })
            .join()
            .unwrap()
    });
    assert_eq!(
        fixture.native.calls(1),
        before_process + 1,
        "all pairs use exactly one native process call"
    );
    assert_eq!(
        fixture.native.midi_word_for_test(1, 0, 0),
        0,
        "invalid clocks did not publish normalized parameter points"
    );
    assert_eq!(&outputs[0][..2], &[0.75, 0.0]);
    assert_eq!(&outputs[1][..2], &[0.25, 0.5]);
    assert_eq!(outputs[0], outputs[2]);
    assert_eq!(
        fixture.native.midi_word_for_test(2, 0, 4),
        0,
        "invalid admission did not consume the first note ID"
    );
    for reverse in [false, true] {
        let pair = [
            Vst3OrderedEvent::Parameter(Vst3NormalizedEvent {
                id: 100,
                value: 0.2,
                sample_offset: 7,
            }),
            Vst3OrderedEvent::Midi(event(
                7,
                M::ControlChange {
                    controller: 1,
                    value: 127,
                },
            )),
        ];
        let events: Vec<_> = (0..32)
            .flat_map(|_| if reverse { [pair[1], pair[0]] } else { pair })
            .collect();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let before = heap();
                    let result = owned.processor.process_outputs_with_ordered_events_into(
                        Vst3OwnedInstrumentBlock {
                            frames: 128,
                            transport: None,
                            parameters: &[],
                            output_pairs: &[1, 0, 1],
                            outputs: &mut outputs,
                        },
                        &events,
                    );
                    assert_eq!(heap(), before, "ordered native ingress allocated");
                    assert!(result.is_ok(), "ordered ingress failed: {result:?}");
                })
                .join()
                .unwrap()
        });
        let expected = if reverse {
            [1.0f64, 0.2]
        } else {
            [0.2f64, 1.0]
        };
        assert_eq!(fixture.native.midi_word_for_test(1, 0, 0), 64);
        for index in 0..64 {
            let value = expected[index % 2];
            assert_eq!(fixture.native.midi_word_for_test(3, index as u32, 0), 100);
            assert_eq!(fixture.native.midi_word_for_test(3, index as u32, 1), 7);
            assert_eq!(
                fixture.native.midi_word_for_test(3, index as u32, 2),
                value.to_bits()
            );
        }
        assert_eq!(
            outputs[1][0], expected[1] as f32,
            "PCM uses the final equal-time native Gain point"
        );
    }
    Ok(())
}

fn audit_latency_restart(heap: fn() -> OwnedHeapCounters) -> Result<(), Vst3OwnedError> {
    let mut fixture = Fixture::new(
        11,
        Vst3ProcessingBudget {
            events: 16,
            points_per_parameter: 8,
        },
    )?;
    fixture
        .native
        .parameter_restart_for_test(1, RestartFlags_::kLatencyChanged);
    let latency_queries = fixture.native.calls(12);
    let process_calls = fixture.native.calls(1);
    let message = [event(
        0,
        M::NoteOn {
            key: 60,
            velocity: 100,
        },
    )];
    let (result, output) = submit(&mut fixture, &message, &[], false, heap);
    assert!(matches!(
        result,
        Err(Vst3ProcessLeaseError::InventoryChanged)
    ));
    assert_eq!(output, [0.0; 256], "latency-changed audio must not publish");
    assert_eq!(
        fixture.native.calls(12),
        latency_queries,
        "DSP path must not query latency"
    );
    let (result, output) = submit(&mut fixture, &message, &[], false, heap);
    assert!(matches!(
        result,
        Err(Vst3ProcessLeaseError::InventoryChanged)
    ));
    assert_eq!(output, [81.0; 256]);
    assert_eq!(
        fixture.native.calls(1),
        process_calls + 1,
        "stale latency lease cannot call native again"
    );
    assert_eq!(fixture.native.calls(12), latency_queries);
    Ok(())
}
