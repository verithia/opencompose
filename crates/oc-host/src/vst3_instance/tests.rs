use super::*;

fn class_info(name: &str, vendor: &str) -> Vst3ClassInfo {
    Vst3ClassInfo {
        name: name.into(),
        category: "Audio Module Class".into(),
        sub_categories: "Instrument".into(),
        vendor: vendor.into(),
        version: "0.0.0".into(),
        cid: [0; 16],
    }
}

#[test]
fn a_listed_editor_is_reported_unhostable() {
    let table = ["Gated Plugin"];
    let reason = blocked_editor_reason_in(&class_info("Gated Plugin", ""), &table)
        .expect("a listed editor must be refused");
    assert!(reason.contains("crashes inside the plugin"));
    assert!(reason.contains("inspector"));
    assert!(blocked_editor_reason_in(&class_info("gated plugin", "Vendor"), &table).is_some());
    assert!(blocked_editor_reason_in(&class_info("Other Plugin", ""), &table).is_none());
    assert!(blocked_editor_reason(&class_info("Gated Plugin", "")).is_none());
}

#[test]
fn other_editors_are_not_gated() {
    for info in [
        class_info("Installed EQ", "Vendor"),
        class_info("Other Instrument", "Other Vendor"),
        class_info("Other Product", "Test Vendor"),
    ] {
        assert!(
            blocked_editor_reason(&info).is_none(),
            "{} must not be gated",
            info.name
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn plug_frame_answers_only_its_own_interface() {
    use vst3::Steinberg::Linux::IRunLoop;
    use vst3::Steinberg::{FUnknown, TUID, kNoInterface};

    let handles = Arc::new(Vst3EditorHandles::default());
    let wrapper = ComWrapper::new(Vst3PlugFrame::new(handles));
    let frame = wrapper.to_com_ptr::<IPlugFrame>().expect("IPlugFrame");

    assert!(frame.cast::<IPlugFrame>().is_some());
    assert!(frame.cast::<FUnknown>().is_some());
    assert!(frame.cast::<IRunLoop>().is_none());

    let raw = frame.as_ptr() as *mut FUnknown;
    let mut object: *mut c_void = ptr::null_mut();
    // SAFETY: `raw` is a live IPlugFrame and `IRunLoop::IID` is a valid TUID.
    let result = unsafe {
        ((*(*raw).vtbl).queryInterface)(raw, IRunLoop::IID.as_ptr() as *const TUID, &mut object)
    };
    assert_eq!(result, kNoInterface);
    assert!(object.is_null());
}

#[cfg(target_os = "macos")]
#[test]
fn plug_frame_refuses_a_degenerate_resize() {
    let frame = Vst3PlugFrame::new(Arc::new(Vst3EditorHandles::default()));
    // SAFETY: these calls pass no view and no rect; the frame must tolerate both.
    assert_ne!(
        // SAFETY: The live in-process frame rejects null view/rectangle arguments before dereferencing either.
        unsafe { frame.resizeView(ptr::null_mut(), ptr::null_mut()) },
        kResultOk
    );
    // A rect with no area is refused instead of collapsing the window.
    let mut empty = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    assert_eq!(
        // SAFETY: The live in-process frame receives a writable empty rectangle and rejects its size without dereferencing the null view.
        unsafe { frame.resizeView(ptr::null_mut(), &mut empty) },
        kResultFalse
    );
    let mut inverted = ViewRect {
        left: 100,
        top: 100,
        right: 40,
        bottom: 40,
    };
    assert_eq!(
        // SAFETY: The live in-process frame receives a writable inverted rectangle and rejects it before using the null view.
        unsafe { frame.resizeView(ptr::null_mut(), &mut inverted) },
        kResultFalse
    );
    // A valid rect with no view yet is accepted: a view may resize before
    // its window exists.
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 640,
        bottom: 480,
    };
    // SAFETY: `rect` is a valid ViewRect and the frame does not retain it.
    assert_eq!(
        // SAFETY: The live in-process frame borrows this valid rectangle and supports a null view while attachment is pending.
        unsafe { frame.resizeView(ptr::null_mut(), &mut rect) },
        kResultOk
    );
}

#[cfg(target_os = "macos")]
#[test]
fn imported_editor_view_reports_a_size() {
    if !crate::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    let Some(plugin) = std::env::var_os("OCP_TEST_VST3_BUNDLE").map(std::path::PathBuf::from)
    else {
        eprintln!("skipping: set OCP_TEST_VST3_BUNDLE to an installed VST3 bundle");
        return;
    };
    if !plugin.exists() {
        eprintln!("skipping: {} is not installed", plugin.display());
        return;
    }
    let instance = Vst3Instance::load(&plugin, 48_000, 512).expect("load plugin");
    let info = instance
        .editor_info()
        .expect("editor info")
        .expect("editor view");
    eprintln!(
        "plugin editor: {}x{} resizable={} nsview={}",
        info.width, info.height, info.resizable, info.platform_supported
    );
    assert!(info.width > 0 && info.height > 0);
    assert!(info.platform_supported);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "loads an installed VST3 plugin"]
fn gated_editor_is_refused_before_attaching() {
    if !crate::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    if std::env::var_os("OCP_VST3_EDITOR_FORCE").is_some() {
        eprintln!("skipping: OCP_VST3_EDITOR_FORCE bypasses the gate");
        return;
    }
    if UNHOSTABLE_EDITORS.is_empty() {
        eprintln!("skipping: no editors are gated");
        return;
    }
    let Some(path) = std::env::var_os("OCP_TEST_EDITOR_PLUGIN").map(std::path::PathBuf::from)
    else {
        eprintln!("skipping: set OCP_TEST_EDITOR_PLUGIN to a gated editor");
        return;
    };
    if !path.exists() {
        eprintln!("skipping: the configured plugin is not installed");
        return;
    }
    let instance = Vst3Instance::load(&path, 48_000, 512).expect("load plugin");
    let gui = instance.gui_handle().expect("editor handle");
    let reason = gui
        .blocked_reason()
        .expect("this plugin's editor is expected to be gated")
        .to_string();
    let error = gui
        .open_hidden("gated editor")
        .expect_err("the gate must refuse before creating a window");
    assert_eq!(error, reason);
    assert!(!gui.is_open());
}

#[test]
fn appends_requested_output_pairs_in_order() {
    let outputs = vec![
        vec![1.0, 2.0],
        vec![3.0, 4.0],
        vec![5.0, 6.0],
        vec![7.0, 8.0],
    ];
    let mut rendered = vec![Vec::new(), Vec::new(), Vec::new()];

    append_output_pairs(&outputs, &[1, 0, 9], 2, &mut rendered);

    assert_eq!(
        rendered,
        vec![
            vec![5.0, 7.0, 6.0, 8.0],
            vec![1.0, 3.0, 2.0, 4.0],
            vec![0.0; 4],
        ]
    );
}

#[test]
fn parameter_changes_group_and_sort_points() {
    let mut queues = Vst3ProcessQueues::new();
    let mut events = vec![
        Vst3ParamEvent {
            id: 7,
            value: 0.75,
            sample_offset: 22,
        },
        Vst3ParamEvent {
            id: 7,
            value: 0.25,
            sample_offset: 3,
        },
        Vst3ParamEvent {
            id: 9,
            value: 0.5,
            sample_offset: 11,
        },
    ];
    queues.load_parameters(&mut events);

    let changes = &queues.parameters;
    // SAFETY: The in-process changes list remains owned by queues for this synchronous count query.
    assert_eq!(unsafe { changes.getParameterCount() }, 2);

    let stored = changes.queues.borrow();
    let first_queue = &stored[0];
    // SAFETY: The RefCell borrow retains this in-process queue while its scalar ID is read.
    assert_eq!(unsafe { first_queue.getParameterId() }, 7);
    // SAFETY: The same borrowed in-process queue remains live for this scalar point-count query.
    assert_eq!(unsafe { first_queue.getPointCount() }, 2);

    let mut sample_offset = 0;
    let mut value = 0.0;
    assert_eq!(
        // SAFETY: The borrowed in-process queue receives a valid point index and writable output scalars.
        unsafe { first_queue.getPoint(0, &mut sample_offset, &mut value) },
        kResultOk
    );
    assert_eq!(sample_offset, 3);
    assert_eq!(value, 0.25);

    assert_eq!(
        // SAFETY: The same live queue receives its second valid point index and writable output scalars.
        unsafe { first_queue.getPoint(1, &mut sample_offset, &mut value) },
        kResultOk
    );
    assert_eq!(sample_offset, 22);
    assert_eq!(value, 0.75);
    assert!(events.is_empty(), "loading drains the scratch events");
}

#[test]
fn input_layout_reallocates_only_when_the_shape_changes() {
    let mut layout = Vst3InputLayout::new(vec![2, 2], 64);
    let channels = layout.channels.as_ptr();

    layout.ensure(&[2, 2], 32);
    layout.ensure(&[2, 2], 64);
    assert_eq!(
        layout.channels.as_ptr(),
        channels,
        "the same bus shape must reuse the planar buffers"
    );

    layout.ensure(&[2], 64);
    assert_eq!(
        layout.bus_channel_counts,
        vec![2],
        "a channel-count change must rebuild the buses"
    );

    layout.ensure(&[2], 128);
    assert_eq!(
        layout.frames, 128,
        "a larger block must grow the frame capacity"
    );
}

#[test]
fn a_plugin_that_throws_in_teardown_marks_the_instance_failed() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    let failure = instance.failure();
    instance.inject_fault(Some(FaultPoint::Terminate));

    drop(instance);

    assert!(failure.failed(), "teardown failure must mark the instance");
    let message = failure.message().unwrap_or_default();
    assert!(message.contains("terminate"), "{message}");
    assert!(message.contains("threw in terminate"), "{message}");
}

#[test]
fn parameter_can_automate_checks_vst3_flag() {
    assert!(parameter_can_automate(ParameterFlags_::kCanAutomate));
    assert!(parameter_can_automate(
        ParameterFlags_::kCanAutomate | ParameterFlags_::kIsBypass
    ));
    assert!(!parameter_can_automate(ParameterFlags_::kNoFlags));
    assert!(!parameter_can_automate(ParameterFlags_::kIsReadOnly));
    assert!(!parameter_can_automate(
        ParameterFlags_::kCanAutomate | ParameterFlags_::kIsReadOnly
    ));
    assert!(!parameter_can_automate(
        ParameterFlags_::kCanAutomate | ParameterFlags_::kIsReadOnly | ParameterFlags_::kIsBypass
    ));
}

#[test]
fn note_ids_track_overlapping_notes_by_channel_and_key() {
    let mut note_ids = Vst3NoteIds::default();

    let first = note_ids.note_on(0, 60);
    let other_channel = note_ids.note_on(1, 60);
    let overlap = note_ids.note_on(0, 60);

    assert_eq!(first, 0);
    assert_eq!(other_channel, 1);
    assert_eq!(overlap, 2);
    assert_eq!(note_ids.active_note_id(0, 60), overlap);
    assert_eq!(note_ids.active_note_id(1, 60), other_channel);

    assert_eq!(note_ids.note_off(0, 60), overlap);
    assert_eq!(note_ids.active_note_id(0, 60), first);
    assert_eq!(note_ids.note_off(0, 60), first);
    assert_eq!(note_ids.active_note_id(0, 60), -1);
    assert_eq!(note_ids.note_off(0, 60), -1);
}

#[test]
fn vst3_events_carry_tracked_note_ids() {
    let note_on = note_event_to_vst3(Vst3NoteEvent {
        on: true,
        channel: 2,
        key: 64,
        velocity: 0.5,
        sample_offset: 9,
        note_id: 42,
    });
    let note_expression = note_expression_event_to_vst3(Vst3NoteExpressionEvent {
        type_id: 2,
        value: 0.25,
        sample_offset: 11,
        note_id: 42,
    });
    let pressure = poly_pressure_event_to_vst3(Vst3PolyPressureEvent {
        channel: 2,
        key: 64,
        pressure: 0.75,
        sample_offset: 13,
        note_id: 42,
    });

    // SAFETY: note_on was constructed as the note-on variant, so noteOn is its initialized active union payload.
    assert_eq!(unsafe { note_on.__field0.noteOn.noteId }, 42);
    assert_eq!(
        // SAFETY: note_expression was constructed as the expression variant, so noteExpressionValue is its initialized active payload.
        unsafe { note_expression.__field0.noteExpressionValue.noteId },
        42
    );
    // SAFETY: pressure was constructed as the poly-pressure variant, so polyPressure is its initialized active payload.
    assert_eq!(unsafe { pressure.__field0.polyPressure.noteId }, 42);
}

#[test]
fn state_stream_reads_writes_and_seeks() {
    let stream = Vst3StateStream::new(vec![1, 2, 3, 4]);
    let mut read_buffer = [0u8; 2];
    let mut count = 0;

    assert_eq!(
        // SAFETY: The live in-process stream receives a writable byte buffer of the declared capacity and writable count.
        unsafe {
            stream.read(
                read_buffer.as_mut_ptr() as *mut c_void,
                read_buffer.len() as int32,
                &mut count,
            )
        },
        kResultOk
    );
    assert_eq!(read_buffer, [1, 2]);
    assert_eq!(count, 2);

    let mut pos = 0;
    assert_eq!(
        // SAFETY: The live in-process stream receives a supported seek origin and writable position output.
        unsafe { stream.seek(1, IStreamSeekMode_::kIBSeekSet as int32, &mut pos) },
        kResultOk
    );
    assert_eq!(pos, 1);

    let mut write_buffer = [9u8, 8u8, 7u8];
    assert_eq!(
        // SAFETY: The live in-process stream receives initialized write bytes of the declared length and a writable count.
        unsafe {
            stream.write(
                write_buffer.as_mut_ptr() as *mut c_void,
                write_buffer.len() as int32,
                &mut count,
            )
        },
        kResultOk
    );
    assert_eq!(count, 3);
    assert_eq!(*stream.data.borrow(), vec![1, 9, 8, 7]);

    assert_eq!(
        // SAFETY: The live in-process stream supports this seek-from-end request and writes only to the supplied position scalar.
        unsafe { stream.seek(-2, IStreamSeekMode_::kIBSeekEnd as int32, &mut pos) },
        kResultOk
    );
    assert_eq!(pos, 2);
    // SAFETY: The live in-process stream receives a writable position scalar for this synchronous query.
    assert_eq!(unsafe { stream.tell(&mut pos) }, kResultOk);
    assert_eq!(pos, 2);
}

#[test]
fn state_stream_rejects_overflowing_writes() {
    let stream = Vst3StateStream::new(vec![]);
    stream.position.set(usize::MAX);
    let mut byte = 1u8;

    assert_eq!(
        // SAFETY: The live read-only stream receives one valid input byte and supports a null optional count pointer.
        unsafe { stream.write(&mut byte as *mut u8 as *mut c_void, 1, ptr::null_mut()) },
        -1
    );
    assert!(stream.data.borrow().is_empty());
}

#[test]
fn event_list_rejects_null_event_output() {
    let list = SimpleEventList {
        events: RefCell::new(vec![note_event_to_vst3(Vst3NoteEvent {
            on: true,
            channel: 0,
            key: 60,
            velocity: 1.0,
            sample_offset: 0,
            note_id: 1,
        })]),
    };

    // SAFETY: The in-process event list checks the null event output before attempting to write or read its payload.
    assert_eq!(unsafe { list.getEvent(0, ptr::null_mut()) }, -1);
}

#[test]
fn vst3_transport_context_reports_tempo_and_position() {
    let context = vst3_process_context(PluginTransport::new(44_100, 44_100, 120.0));

    let playing = StatesAndFlags_::kPlaying;
    let tempo_valid = StatesAndFlags_::kTempoValid;
    // The Windows enum constants are signed; ProcessContext's ABI field is u32.
    #[cfg(target_os = "windows")]
    let (playing, tempo_valid) = (playing as u32, tempo_valid as u32);
    assert_ne!(context.state & playing, 0);
    assert_ne!(context.state & tempo_valid, 0);
    assert_eq!(context.sampleRate, 44_100.0);
    assert_eq!(context.projectTimeSamples, 44_100);
    assert_eq!(context.projectTimeMusic, 2.0);
    assert_eq!(context.barPositionMusic, 0.0);
    assert_eq!(context.tempo, 120.0);
    assert_eq!(context.timeSigNumerator, 4);
    assert_eq!(context.timeSigDenominator, 4);
}

#[test]
fn vst3_mapped_context_retains_meter_origin_and_integrated_ppq() {
    let context = vst3_process_context(crate::mapped_transport_fixture().at_sample(312000));
    assert_eq!(context.tempo, 60.0);
    assert_eq!(context.projectTimeSamples, 312000);
    assert_eq!(context.projectTimeMusic, 7.5);
    assert_eq!(context.barPositionMusic, 7.0);
    assert_eq!(
        (context.timeSigNumerator, context.timeSigDenominator),
        (3, 4)
    );
}

#[test]
fn fake_two_bus_plugin_renders_each_output_bus() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .bus_count
        .store(2, std::sync::atomic::Ordering::Relaxed);
    control
        .state_bus_count
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let mut instance = test_support::fake_instance(&control, 2, 512);

    assert_eq!(instance.output_count(), 2);
    assert_eq!(instance.output_bus_count(), 2);
    assert_eq!(instance.output_bus_channels(), &[2, 2]);

    let mut outputs = vec![Vec::new(), Vec::new()];
    instance
        .process_block_outputs_into(4, None, &[0, 1], &mut outputs)
        .unwrap();

    assert_eq!(outputs[0], vec![1.0, 1.5, 1.0, 1.5, 1.0, 1.5, 1.0, 1.5]);
    assert_eq!(outputs[1], vec![2.0, 2.5, 2.0, 2.5, 2.0, 2.5, 2.0, 2.5]);

    let mut legacy = test_support::fake_instance(&control, 2, 512);
    assert_eq!(legacy.process_block_with_transport(4, None), outputs[0]);
}

#[test]
fn declared_buses_are_reported_as_ports_with_default_pairs() {
    let mut instance = test_support::fake_instance_for_id(test_support::FAKE_FOUR_BUS_ID, 512)
        .expect("four-bus fake");
    assert_eq!(instance.output_ports().len(), 4);
    assert_eq!(
        instance.output_ports(),
        vec![
            crate::ports::AudioPort {
                channels: 2,
                first_pair: 0,
                kind: crate::ports::PortKind::Audio,
            },
            crate::ports::AudioPort {
                channels: 2,
                first_pair: 1,
                kind: crate::ports::PortKind::Audio,
            },
            crate::ports::AudioPort {
                channels: 2,
                first_pair: 2,
                kind: crate::ports::PortKind::Audio,
            },
            crate::ports::AudioPort {
                channels: 2,
                first_pair: 3,
                kind: crate::ports::PortKind::Audio,
            },
        ]
    );
    // Instruments declare no audio input, so there is no sidechain port.
    assert!(instance.input_ports().is_empty());
    assert_eq!(instance.sidechain_input_count(), 0);
    let mut outputs = (0..4).map(|_| Vec::new()).collect::<Vec<_>>();
    instance
        .process_block_outputs_into(2, None, &[0, 1, 2, 3], &mut outputs)
        .unwrap();
    assert_eq!(outputs[3], vec![4.0, 4.5, 4.0, 4.5]);
}

#[test]
fn sidechain_audio_reaches_the_selected_input_bus() {
    let (mut instance, control) = test_support::fake_instance_with_buses(1, 3, 512);
    assert_eq!(instance.output_ports().len(), 1);
    assert_eq!(instance.input_ports().len(), 3);
    assert_eq!(instance.sidechain_input_count(), 2);

    let input = vec![0.1f32; 8];
    let sidechain = vec![0.75f32; 8];
    instance.process_effect_with_sidechain(&mut input.clone(), None, 48_000, None);
    let main_only = control.input_bus_peaks.lock().unwrap().clone();
    assert!(main_only[0] > 0.09, "{main_only:?}");
    assert_eq!(main_only[1], 0.0);
    assert_eq!(main_only[2], 0.0);

    instance.set_sidechain_input_index(1);
    let mut buffer = input.clone();
    instance.process_effect_with_sidechain(&mut buffer, Some(&sidechain), 48_000, None);
    let peaks = control.input_bus_peaks.lock().unwrap().clone();
    assert_eq!(peaks.len(), 3);
    assert!((peaks[0] - 0.1).abs() < 1e-6, "{peaks:?}");
    assert_eq!(peaks[1], 0.0, "unselected sidechain input stays silent");
    assert!((peaks[2] - 0.75).abs() < 1e-6, "{peaks:?}");
}

#[test]
fn fake_single_bus_matches_legacy_output_path() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .bus_count
        .store(1, std::sync::atomic::Ordering::Relaxed);
    control
        .state_bus_count
        .store(1, std::sync::atomic::Ordering::Relaxed);
    let mut legacy = test_support::fake_instance(&control, 2, 512);
    let mut multi = test_support::fake_instance(&control, 2, 512);

    assert_eq!(legacy.output_count(), 1);
    let old = legacy.process_block_with_transport(4, None);
    let mut outputs = vec![Vec::new()];
    multi
        .process_block_outputs_into(4, None, &[0], &mut outputs)
        .unwrap();

    assert_eq!(old, outputs[0]);
    assert_eq!(old, vec![1.0, 1.5, 1.0, 1.5, 1.0, 1.5, 1.0, 1.5]);
}

#[test]
fn fake_plugin_receives_requested_midi_channel() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    let mut outputs = vec![Vec::new()];

    instance.note_on(60, 0.5, 0);
    instance
        .process_block_outputs_into(2, None, &[0], &mut outputs)
        .unwrap();
    assert_eq!(
        control
            .last_note_channel
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    instance.note_on_channel(2, 62, 0.5, 0);
    instance
        .process_block_outputs_into(2, None, &[0], &mut outputs)
        .unwrap();
    assert_eq!(
        control
            .last_note_channel
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[test]
fn state_load_negotiates_output_buses_once_not_per_block() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .bus_count
        .store(1, std::sync::atomic::Ordering::Relaxed);
    control
        .state_bus_count
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let mut instance = test_support::fake_instance(&control, 2, 512);

    assert_eq!(instance.output_count(), 1);
    let mut outputs = vec![Vec::new()];
    instance
        .process_block_outputs_into(2, None, &[0], &mut outputs)
        .unwrap();
    let queries_after_render = control
        .bus_queries
        .load(std::sync::atomic::Ordering::Relaxed);
    instance
        .process_block_outputs_into(2, None, &[0], &mut outputs)
        .unwrap();
    assert_eq!(
        control
            .bus_queries
            .load(std::sync::atomic::Ordering::Relaxed),
        queries_after_render
    );

    instance.set_component_state_bytes(&[]).unwrap();

    assert_eq!(instance.output_count(), 2);
    let queries_after_state = control
        .bus_queries
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(queries_after_state > queries_after_render);
    let mut outputs = vec![Vec::new(), Vec::new()];
    instance
        .process_block_outputs_into(2, None, &[0, 1], &mut outputs)
        .unwrap();
    assert_eq!(
        control
            .bus_queries
            .load(std::sync::atomic::Ordering::Relaxed),
        queries_after_state
    );
}

fn queued_param(id: ParamID, value: f64, sample_offset: i32) -> Vst3PendingEvent {
    Vst3PendingEvent::Param(Vst3ParamEvent {
        id,
        value,
        sample_offset,
    })
}

#[test]
fn effect_path_delivers_queued_parameters_and_drops_notes() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    instance.pending_events.push(queued_param(42, 0.75, 0));
    instance.note_on(60, 0.8, 0);

    let mut buffer = vec![0.0f32; 64 * 2];
    instance.process_effect(&mut buffer, 48_000);

    assert!(instance.pending_events.is_empty());
    assert!(
        control
            .input_events_null
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    assert_eq!(
        *control.param_changes.lock().unwrap(),
        vec![test_support::CapturedParamChange {
            process_call: 0,
            param_id: 42,
            sample_offset: 0,
            value: 0.75,
        }]
    );

    // A second block must not replay the drained edit.
    instance.process_effect(&mut buffer, 48_000);
    assert_eq!(control.param_changes.lock().unwrap().len(), 1);
    assert_eq!(
        control
            .process_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[test]
fn effect_path_splits_parameter_offsets_across_blocks_and_defers_future_events() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 32);
    for (offset, value) in [(8, 0.25), (64, 0.5), (200, 0.75)] {
        instance.pending_events.push(queued_param(7, value, offset));
    }

    let mut buffer = vec![0.0f32; 128 * 2];
    instance.process_effect(&mut buffer, 48_000);
    assert_eq!(
        *control.param_changes.lock().unwrap(),
        vec![
            test_support::CapturedParamChange {
                process_call: 0,
                param_id: 7,
                sample_offset: 8,
                value: 0.25,
            },
            test_support::CapturedParamChange {
                process_call: 2,
                param_id: 7,
                sample_offset: 0,
                value: 0.5,
            },
        ]
    );
    match instance.pending_events.as_slice() {
        [Vst3PendingEvent::Param(event)] => {
            assert_eq!((event.id, event.sample_offset, event.value), (7, 72, 0.75));
        }
        other => panic!("unexpected pending events: {}", other.len()),
    }

    // The deferred point lands in the block starting at frame 64.
    control.param_changes.lock().unwrap().clear();
    instance.process_effect(&mut buffer, 48_000);
    assert_eq!(
        *control.param_changes.lock().unwrap(),
        vec![test_support::CapturedParamChange {
            process_call: 6,
            param_id: 7,
            sample_offset: 8,
            value: 0.75,
        }]
    );
    assert!(instance.pending_events.is_empty());
}

#[test]
fn an_editor_perform_edit_reaches_the_live_processor_next_block() {
    let editor_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut editor = test_support::fake_editor_instance(&editor_control, 2, 512);
    editor.bind_preset_edits("live-edit-test");
    editor.publish_controller_values();

    let live_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut live = test_support::fake_effect_instance(&live_control, 2, 512);
    live.bind_preset_edits("live-edit-test");

    let mut buffer = vec![0.0f32; 64 * 2];
    live.process_effect(&mut buffer, 48_000);
    let default_rms = rms(&buffer);
    // The baseline edit from `publish_controller_values` landed first.
    live_control.param_changes.lock().unwrap().clear();

    assert_eq!(
        test_support::editor_perform_edit(&editor_control, test_support::FAKE_PARAM_ID, 0.75),
        kResultOk
    );

    live.process_effect(&mut buffer, 48_000);
    let edited_rms = rms(&buffer);
    assert!(
        (edited_rms / default_rms - 1.5).abs() < 1e-5,
        "the edit did not change the live block: {default_rms} -> {edited_rms}"
    );
    let changes = live_control.param_changes.lock().unwrap().clone();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].param_id, test_support::FAKE_PARAM_ID);
    assert_eq!(changes[0].sample_offset, 0);
    assert!((changes[0].value - 0.75).abs() < 1e-9);
}

#[test]
fn a_gesture_with_many_moves_lands_one_value_per_block() {
    let editor_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut editor = test_support::fake_editor_instance(&editor_control, 2, 512);
    editor.bind_preset_edits("gesture-test");

    let live_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut live = test_support::fake_effect_instance(&live_control, 2, 512);
    live.bind_preset_edits("gesture-test");

    assert_eq!(
        test_support::editor_begin_edit(&editor_control, test_support::FAKE_PARAM_ID),
        kResultOk
    );
    for step in 0..1_000 {
        test_support::editor_perform_edit(
            &editor_control,
            test_support::FAKE_PARAM_ID,
            f64::from(step) / 1_000.0,
        );
    }
    assert_eq!(
        test_support::editor_end_edit(&editor_control, test_support::FAKE_PARAM_ID),
        kResultOk
    );

    let mut buffer = vec![0.0f32; 128 * 2];
    live.process_effect(&mut buffer, 48_000);
    let changes = live_control.param_changes.lock().unwrap().clone();
    assert_eq!(
        changes.len(),
        1,
        "a fast drag must coalesce to one point per block: {changes:?}"
    );
    assert!(
        (changes[0].value - 0.999).abs() < 1e-9,
        "the final value was lost: {:?}",
        changes[0].value
    );

    live.process_effect(&mut buffer, 48_000);
    assert_eq!(live_control.param_changes.lock().unwrap().len(), 1);
}

#[test]
fn a_save_after_editor_edits_writes_the_new_value() {
    let editor_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut editor = test_support::fake_editor_instance(&editor_control, 2, 512);
    editor.bind_preset_edits("save-test");

    let live_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut live = test_support::fake_effect_instance(&live_control, 2, 512);
    live.bind_preset_edits("save-test");

    test_support::editor_perform_edit(&editor_control, test_support::FAKE_PARAM_ID, 0.8);
    assert_eq!(live.absorb_editor_edits(), 1);
    assert_eq!(
        live.get_param_by_name(test_support::FAKE_PARAM_NAME),
        Some(0.8)
    );

    live.sync_processor_from_controller();
    let bytes = live.state_bytes().expect("fake state");
    assert_eq!(bytes.len(), 8);
    assert!((f64::from_le_bytes(bytes.clone().try_into().unwrap()) - 0.8).abs() < 1e-9);

    let reloaded_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut reloaded = test_support::fake_editor_instance(&reloaded_control, 2, 512);
    reloaded.set_state_bytes(&bytes).expect("state loads");
    assert_eq!(
        reloaded.get_param_by_name(test_support::FAKE_PARAM_NAME),
        Some(0.8),
        "the saved preset did not carry the edited value"
    );
}

const HOSTED_EQ_OUTPUT_LEVEL: &str = "Output Level";

fn hosted_eq_path() -> Option<std::path::PathBuf> {
    std::env::var_os("OCP_TEST_HOSTED_EQ").map(std::path::PathBuf::from)
}

fn sine_buffer(frames: usize, frequency: f64, amplitude: f64) -> Vec<f32> {
    let mut buffer = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let sample = (amplitude
            * (std::f64::consts::TAU * frequency * frame as f64 / 48_000.0).sin())
            as f32;
        buffer.push(sample);
        buffer.push(sample);
    }
    buffer
}

fn rms(buffer: &[f32]) -> f64 {
    let power = buffer
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    (power / buffer.len() as f64).sqrt()
}

fn render_hosted_eq(input: &[f32], output_level: Option<f64>) -> Vec<f32> {
    // Callers skip first; an empty buffer here keeps the helper total.
    if !crate::external_plugin_tests_enabled() {
        return Vec::new();
    }
    let Some(path) = hosted_eq_path() else {
        return Vec::new();
    };
    let mut instance =
        Vst3Instance::load(&path, 48_000, 512).expect("installed VST3 effect should load");
    if let Some(value) = output_level {
        assert!(
            instance.set_param_by_name(HOSTED_EQ_OUTPUT_LEVEL, value),
            "plugin rejected {HOSTED_EQ_OUTPUT_LEVEL}"
        );
    }
    let mut buffer = input.to_vec();
    instance.process_effect(&mut buffer, 48_000);
    buffer
}

/// A real plug-in: an editor-side edit changes the next block of a separate
/// live instance without a preset save.
#[test]
fn installed_vst3_editor_edit_reaches_the_live_render() {
    use vst3::Steinberg::Vst::IComponentHandlerTrait;

    if !crate::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    let Some(path) = hosted_eq_path() else {
        eprintln!("skipping: set OCP_TEST_HOSTED_EQ");
        return;
    };
    if !path.exists() {
        eprintln!("skipping: {} is not installed", path.display());
        return;
    }
    let preset = "installed-editor-edit-test";
    let mut editor = Vst3Instance::load(&path, 48_000, 512).expect("editor instance loads");
    editor.bind_preset_edits(preset);
    let parameter = editor
        .get_params()
        .into_iter()
        .find(|parameter| parameter.name == HOSTED_EQ_OUTPUT_LEVEL)
        .expect("the plugin exposes its output level");
    let handler = editor.edit_handler().expect("the host handler");

    let mut live = Vst3Instance::load(&path, 48_000, 512).expect("live instance loads");
    live.bind_preset_edits(preset);

    let input = sine_buffer(512, 440.0, 0.1);
    let mut buffer = input.clone();
    live.process_effect(&mut buffer, 48_000);
    let default_rms = rms(&buffer);

    // SAFETY: the handler outlives the instance's controller.
    unsafe {
        assert_eq!(handler.beginEdit(parameter.id), kResultOk);
        assert_eq!(handler.performEdit(parameter.id, 1.0), kResultOk);
        assert_eq!(handler.endEdit(parameter.id), kResultOk);
    }

    let mut buffer = input;
    live.process_effect(&mut buffer, 48_000);
    let edited_rms = rms(&buffer);
    eprintln!(
        "[edit] default rms {default_rms:.6}, edited rms {edited_rms:.6}, ratio {:.2}",
        edited_rms / default_rms
    );
    assert!(
        edited_rms > default_rms * 2.0,
        "the editor edit did not reach the live render: {default_rms} -> {edited_rms}"
    );
}

#[test]
fn installed_vst3_effect_parameter_change_alters_render() {
    if !crate::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    let Some(path) = hosted_eq_path() else {
        eprintln!("skipping: set OCP_TEST_HOSTED_EQ");
        return;
    };
    if !path.exists() {
        eprintln!("skipping: {} is not installed", path.display());
        return;
    }
    let input = sine_buffer(1024, 440.0, 0.1);
    let default_out = render_hosted_eq(&input, None);
    let changed_out = render_hosted_eq(&input, Some(1.0));

    let default_rms = rms(&default_out);
    let changed_rms = rms(&changed_out);
    assert!(
        default_rms > 0.01,
        "effect rendered near-silence at its defaults: {default_rms}"
    );
    assert!(
        changed_rms > default_rms * 4.0,
        "queued parameter edit did not reach the processor: default {default_rms}, changed {changed_rms}"
    );
}

#[test]
fn the_fake_plugin_exposes_a_native_program_list() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let instance = test_support::fake_instance(&control, 2, 512);

    let programs = instance.list_programs();
    let names: Vec<_> = programs
        .iter()
        .map(|program| program.name.as_str())
        .collect();
    assert_eq!(names, test_support::FAKE_PROGRAM_NAMES);
    assert!(
        programs
            .iter()
            .all(|program| program.list_id == test_support::FAKE_PROGRAM_LIST_ID)
    );
}

#[test]
fn a_native_program_data_blob_applies_as_state() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);

    let before = instance.state_bytes().unwrap();
    let data = instance
        .program_data(test_support::FAKE_PROGRAM_LIST_ID, 1)
        .expect("the fake exposes program data");
    assert_eq!(data.len(), 8);
    instance.set_state_bytes(&data).unwrap();
    let after = instance.state_bytes().unwrap();

    assert_ne!(before, after, "the selected program must change state");
    assert_eq!(
        *control.param_value.lock().unwrap(),
        test_support::FAKE_PROGRAM_VALUES[1]
    );
}

#[test]
fn an_unknown_program_list_has_no_data() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let instance = test_support::fake_instance(&control, 2, 512);
    assert!(
        instance
            .program_data(test_support::FAKE_PROGRAM_LIST_ID + 1, 0)
            .is_none()
    );
    assert!(
        instance
            .program_data(test_support::FAKE_PROGRAM_LIST_ID, 9)
            .is_none()
    );
}

#[test]
fn a_plugin_that_throws_in_process_becomes_an_error() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    instance.inject_fault(Some(FaultPoint::Process));

    let mut outputs = vec![Vec::new()];
    let error = instance
        .process_block_outputs_into(4, None, &[0], &mut outputs)
        .expect_err("a throwing process must not be reported as success");
    assert!(error.is_exception(), "{error}");
    assert!(error.to_string().contains("threw in process"), "{error}");
    assert_eq!(outputs[0], vec![0.0; 8], "a failed block is silent");
    assert!(instance.take_fault().is_some());
    assert!(
        instance.failed(),
        "a process throw marks the instance failed"
    );

    // Clearing the fixture cannot revive an instance quarantined by its first fault.
    instance.inject_fault(None);
    let mut outputs = vec![Vec::new()];
    instance
        .process_block_outputs_into(4, None, &[0], &mut outputs)
        .expect_err("quarantine is latched");
    assert_eq!(outputs[0], vec![0.0; 8]);
}

#[test]
fn a_plugin_that_throws_in_the_effect_path_leaves_a_silent_block() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    instance.inject_fault(Some(FaultPoint::Process));

    let mut buffer = vec![1.0f32; 32 * 2];
    instance.process_effect(&mut buffer, 48_000);
    assert!(buffer.iter().all(|sample| *sample == 0.0), "{buffer:?}");
    let fault = instance.take_fault().expect("the failure is reportable");
    assert!(fault.contains("threw in process"), "{fault}");
}

#[test]
fn a_controller_without_component_state_is_not_a_failure() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .controller_state_result
        .store(kNotImplemented, std::sync::atomic::Ordering::Relaxed);
    let mut instance = test_support::fake_editor_instance(&control, 2, 512);

    instance
        .set_state_bytes(&0.25_f64.to_le_bytes())
        .expect("a processor-only controller is not a load failure");
    assert_eq!(
        *control.param_value.lock().unwrap(),
        0.25,
        "the processor state still applies"
    );
}

#[test]
fn a_controller_that_rejects_the_state_gets_the_known_values() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_editor_instance(&control, 2, 512);
    assert!(instance.set_param_by_name(test_support::FAKE_PARAM_NAME, 0.75));
    control
        .controller_state_result
        .store(kResultFalse, std::sync::atomic::Ordering::Relaxed);

    // The processor takes the blob, the controller refuses it, and the host
    // re-applies the value it had already set.
    instance
        .set_state_bytes(&0.25_f64.to_le_bytes())
        .expect("a rejecting controller does not fail the load");
    assert_eq!(
        instance.get_param_by_name(test_support::FAKE_PARAM_NAME),
        Some(0.75)
    );
}

#[test]
fn a_parameter_name_is_resolved_once_until_the_titles_change() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_editor_instance(&control, 2, 512);
    let calls = || {
        control
            .param_info_queries
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    let before = calls();

    assert!(instance.queue_param_by_name(test_support::FAKE_PARAM_NAME, 0.4, 0));
    let built = calls();
    assert_eq!(built, before + 1, "one enumeration builds the cache");

    for _ in 0..8 {
        assert!(instance.queue_param_by_name(test_support::FAKE_PARAM_NAME, 0.6, 0));
    }
    assert_eq!(calls(), built, "writes reuse the cached lookup");

    instance.invalidate_param_cache();
    assert!(instance.queue_param_by_name(test_support::FAKE_PARAM_NAME, 0.5, 0));
    assert_eq!(calls(), built + 1, "a title change rebuilds the cache");
}

#[test]
fn a_controller_that_throws_in_component_state_is_an_error() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_editor_instance(&control, 2, 512);
    instance.inject_fault(Some(FaultPoint::SetComponentState));

    let error = instance
        .set_state_bytes(&0.25_f64.to_le_bytes())
        .expect_err("a throwing setComponentState is an error");
    assert!(error.contains("plugin threw"), "{error}");
    assert!(error.contains("threw in setComponentState"), "{error}");
}

#[test]
fn a_plugin_that_throws_in_get_state_returns_an_error() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    instance.inject_fault(Some(FaultPoint::GetState));

    let error = instance
        .state_bytes()
        .expect_err("a throwing getState is an error");
    assert!(error.contains("plugin threw"), "{error}");
    assert!(error.contains("threw in getState"), "{error}");
}

#[test]
fn a_plugin_that_throws_in_set_state_returns_an_error() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_instance(&control, 2, 512);
    instance.inject_fault(Some(FaultPoint::SetState));

    let error = instance
        .set_component_state_bytes(&[])
        .expect_err("a throwing setState is an error");
    assert!(error.contains("plugin threw"), "{error}");
    assert!(error.contains("threw in setState"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_synthetic_editor_failure_is_reported_before_native_ui_dispatch() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut instance = test_support::fake_editor_instance(&control, 2, 512);
    test_support::install_plug_frame(&mut instance);
    instance.inject_fault(Some(FaultPoint::CreateView));

    let error = instance
        .editor_info()
        .expect_err("a throwing createView is an error");
    assert!(error.contains("plugin threw"), "{error}");
    assert!(error.contains("threw in createView"), "{error}");
    assert!(
        instance.failed(),
        "synthetic failure follows quarantine policy without calling native UI from libtest's worker"
    );
}

#[test]
fn a_restart_component_notification_refreshes_the_live_values() {
    let editor_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut editor = test_support::fake_editor_instance(&editor_control, 2, 512);
    editor.bind_preset_edits("restart-test");
    editor.publish_controller_values();

    let live_control = std::sync::Arc::new(test_support::FakeControl::default());
    let mut live = test_support::fake_effect_instance(&live_control, 2, 512);
    live.bind_preset_edits("restart-test");
    let mut buffer = vec![0.0f32; 64 * 2];
    live.process_effect(&mut buffer, 48_000);
    live_control.param_changes.lock().unwrap().clear();

    // The plugin loads a program: it changes the value, then reports it.
    *editor_control.param_value.lock().unwrap() = 0.9;
    assert_eq!(
        test_support::editor_restart_component(&editor_control, RestartFlags_::kParamValuesChanged),
        kResultOk
    );
    assert_eq!(
        editor.poll_host_events(),
        vec![Vst3HostEvent::ParamValuesChanged]
    );

    live.process_effect(&mut buffer, 48_000);
    let changes = live_control.param_changes.lock().unwrap().clone();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].param_id, test_support::FAKE_PARAM_ID);
    assert!((changes[0].value - 0.9).abs() < 1e-9);
    assert!(
        editor.poll_host_events().is_empty(),
        "a notification is delivered once"
    );
}

#[test]
fn a_plugin_can_mark_the_document_dirty_and_ask_for_its_editor() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let instance = test_support::fake_editor_instance(&control, 2, 512);

    assert!(!instance.take_document_dirty());
    assert_eq!(test_support::editor_set_dirty(&control, true), kResultOk);
    assert!(instance.take_document_dirty());
    assert!(!instance.take_document_dirty(), "the flag is taken once");

    assert!(instance.take_editor_request().is_none());
    assert_eq!(
        test_support::editor_request_open_editor(&control, "editor"),
        kResultOk
    );
    assert_eq!(instance.take_editor_request().as_deref(), Some("editor"));
    assert!(instance.take_editor_request().is_none());
}

#[test]
fn a_sampler_keyswitch_surface_is_exposed_and_found_by_key() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .keyswitch_count
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let instance = test_support::fake_effect_instance(&control, 2, 512);

    assert_eq!(instance.keyswitch_count(), 2);
    let switches = instance.keyswitches();
    assert_eq!(switches.len(), 2, "{switches:?}");
    assert_eq!(switches[0].name, "Kick");
    assert_eq!((switches[0].min_key, switches[0].max_key), (36, 36));
    assert_eq!(switches[0].remapped_key, Some(36));
    assert_eq!(
        instance.keyswitch_by_key(38).map(|switch| switch.name),
        Some("Snare".to_string())
    );
    assert!(instance.keyswitch_by_key(40).is_none());

    // A plugin without the interface reports nothing instead of failing.
    let plain = test_support::fake_instance(
        &std::sync::Arc::new(test_support::FakeControl::default()),
        2,
        512,
    );
    assert_eq!(plain.keyswitch_count(), 0);
    assert!(plain.keyswitches().is_empty());
}

#[test]
fn a_note_expression_surface_is_exposed_and_found_by_id() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    control
        .note_expression_count
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let instance = test_support::fake_effect_instance(&control, 2, 512);

    assert_eq!(instance.note_expression_count(), 2);
    let expressions = instance.note_expressions();
    assert_eq!(expressions.len(), 2, "{expressions:?}");
    assert_eq!(expressions[0].name, "Pan");
    assert!(expressions[0].bipolar);
    assert!(!expressions[0].one_shot);
    assert_eq!(
        expressions[0].associated_parameter,
        test_support::FAKE_PARAM_ID
    );
    assert_eq!(expressions[1].unit, "ct");
    assert_eq!(
        instance
            .note_expression_by_id(2)
            .map(|expression| expression.name),
        Some("Tuning".to_string())
    );
    assert!(instance.note_expression_by_id(9).is_none());
}

#[test]
fn mapped_midi_normalization_preserves_domains_and_program_steps() {
    assert_eq!(mapped_midi_normalized(64, 127, None), Some(1.0));
    assert_eq!(mapped_midi_normalized(128, 0, None), Some(0.0));
    assert_eq!(mapped_midi_normalized(129, -8192, None), Some(0.0));
    assert_eq!(mapped_midi_normalized(129, 8191, None), Some(1.0));
    assert_eq!(mapped_midi_normalized(130, 7, Some(7)), Some(1.0));
    assert_eq!(mapped_midi_normalized(130, 8, Some(7)), None);
    assert_eq!(mapped_midi_normalized(130, 0, None), None);
    assert_eq!(mapped_midi_normalized(130, 0, Some(0)), Some(0.0));
    assert_eq!(mapped_midi_normalized(1, 128, None), None);
}

#[test]
fn fake_editor_safe_callbacks_are_inactive_after_instance_drop() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let instance = test_support::fake_editor_instance(&control, 2, 512);
    assert_eq!(
        test_support::editor_begin_edit(&control, test_support::FAKE_PARAM_ID),
        kResultOk
    );
    drop(instance);
    let before = *control.param_value.lock().unwrap();
    assert_eq!(
        test_support::editor_perform_edit(&control, test_support::FAKE_PARAM_ID, 0.75),
        kResultFalse
    );
    assert_eq!(*control.param_value.lock().unwrap(), before);
    assert_eq!(
        test_support::editor_begin_edit(&control, test_support::FAKE_PARAM_ID),
        kResultFalse
    );
    assert_eq!(
        test_support::editor_end_edit(&control, test_support::FAKE_PARAM_ID),
        kResultFalse
    );
    assert_eq!(
        test_support::editor_restart_component(&control, 0),
        kResultFalse
    );
    assert_eq!(test_support::editor_set_dirty(&control, true), kResultFalse);
    assert_eq!(
        test_support::editor_request_open_editor(&control, "editor"),
        kResultFalse
    );
    test_support::drain_editor_handlers();
}

#[test]
fn fake_editor_safe_callbacks_refuse_a_foreign_thread_then_retire_on_owner_thread() {
    let control = std::sync::Arc::new(test_support::FakeControl::default());
    let instance = test_support::fake_editor_instance(&control, 2, 512);
    let foreign = control.clone();
    let result = std::thread::spawn(move || {
        test_support::editor_perform_edit(&foreign, test_support::FAKE_PARAM_ID, 0.75)
    })
    .join()
    .unwrap();
    assert_eq!(result, kResultFalse);
    assert_eq!(*control.param_value.lock().unwrap(), 0.5);
    assert_eq!(
        test_support::editor_perform_edit(&control, test_support::FAKE_PARAM_ID, 0.75),
        kResultOk
    );
    std::thread::spawn(move || drop(instance)).join().unwrap();
    assert_eq!(
        test_support::editor_begin_edit(&control, test_support::FAKE_PARAM_ID),
        kResultFalse
    );
    test_support::drain_editor_handlers();
}
