//! Native throws enter the same typed bridge used by real AU operations.
use super::*;

#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "oc_au_guard_fixture", kind = "static")]
unsafe extern "C" {
    fn oc_au_fixture_mode(mode: u32);
    fn oc_au_fixture_calls(index: u32) -> u32;
    fn oc_au_unit_config(operation: u32, hit: u32, mode: u32, status: i32);
    fn oc_au_unit_parameter_value(value: f32);
    fn oc_au_unit_list_size(bytes: u32);
    fn oc_au_partial_parameter_info()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32;
    fn oc_au_list_reads_reset();
    fn oc_au_list_reads() -> u32;
    fn oc_au_partial_state_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32;
    fn oc_au_partial_copy_name() -> unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    fn oc_au_counted_release() -> unsafe extern "C" fn(*mut c_void);
    fn oc_au_malformed_list_info()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32;
    fn oc_au_counted_list_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32;

    fn oc_au_unit_input_refcon() -> *mut c_void;
    fn oc_au_unit_clock_refcon() -> *mut c_void;
    fn oc_au_unit_forget_callbacks();
    fn oc_au_unit_copy_name() -> unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    fn oc_au_fixture_find_next()
    -> unsafe extern "C" fn(*mut c_void, *const AudioComponentDescription) -> *mut c_void;
    fn oc_au_unit_find_next()
    -> unsafe extern "C" fn(*mut c_void, *const AudioComponentDescription) -> *mut c_void;
    fn oc_au_fixture_copy_name() -> unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    fn oc_au_fixture_description()
    -> unsafe extern "C" fn(*mut c_void, *mut AudioComponentDescription) -> i32;
    fn oc_au_unit_description()
    -> unsafe extern "C" fn(*mut c_void, *mut AudioComponentDescription) -> i32;
    fn oc_au_fixture_instance_new() -> unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    fn oc_au_unit_instance_new() -> unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    fn oc_au_fixture_dispose() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_unit_dispose() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_fixture_initialize() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_unit_initialize() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_fixture_uninitialize() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_unit_uninitialize() -> unsafe extern "C" fn(*mut c_void) -> i32;
    fn oc_au_fixture_reset() -> unsafe extern "C" fn(*mut c_void, u32, u32) -> i32;
    fn oc_au_unit_reset() -> unsafe extern "C" fn(*mut c_void, u32, u32) -> i32;
    fn oc_au_fixture_set_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32;
    fn oc_au_unit_set_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32;
    fn oc_au_fixture_get_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32;
    fn oc_au_unit_get_property()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32;
    fn oc_au_fixture_property_info()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32;
    fn oc_au_unit_property_info()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32;
    fn oc_au_fixture_render() -> unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const AudioTimeStamp,
        u32,
        u32,
        *mut AudioBufferList,
    ) -> i32;
    fn oc_au_unit_render() -> unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const AudioTimeStamp,
        u32,
        u32,
        *mut AudioBufferList,
    ) -> i32;
    fn oc_au_fixture_set_parameter()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32;
    fn oc_au_unit_set_parameter()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32;
    fn oc_au_fixture_get_parameter()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut f32) -> i32;
    fn oc_au_unit_get_parameter()
    -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut f32) -> i32;
    fn oc_au_fixture_midi() -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> i32;
    fn oc_au_unit_midi() -> unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> i32;
    fn oc_au_fixture_midi_list()
    -> unsafe extern "C" fn(*mut c_void, u32, *const MidiEventList) -> i32;
    fn oc_au_unit_midi_list() -> unsafe extern "C" fn(*mut c_void, u32, *const MidiEventList) -> i32;
    fn oc_au_fixture_string_length() -> unsafe extern "C" fn(*mut c_void) -> isize;
    fn oc_au_fixture_string_capacity() -> unsafe extern "C" fn(isize, u32) -> isize;
    fn oc_au_fixture_string_c_string()
    -> unsafe extern "C" fn(*mut c_void, *mut c_char, isize, u32) -> u8;
    fn oc_au_fixture_data_create()
    -> unsafe extern "C" fn(*const c_void, *const u8, isize) -> *mut c_void;
    fn oc_au_fixture_data_length() -> unsafe extern "C" fn(*mut c_void) -> isize;
    fn oc_au_fixture_data_bytes() -> unsafe extern "C" fn(*mut c_void) -> *const u8;
    fn oc_au_fixture_plist_data() -> unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        isize,
        usize,
        *mut *mut c_void,
    ) -> *mut c_void;
    fn oc_au_fixture_plist_create() -> unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        usize,
        *mut isize,
        *mut *mut c_void,
    ) -> *mut c_void;
    fn oc_au_fixture_release() -> unsafe extern "C" fn(*mut c_void);
}

fn api() -> Api {
    // SAFETY: the getters return native functions with exactly the declared SDK ABI.
    unsafe {
        Api {
            midi_list: Some(oc_au_fixture_midi_list()),
            find_next: oc_au_fixture_find_next(),
            copy_name: oc_au_fixture_copy_name(),
            description: oc_au_fixture_description(),
            instance_new: oc_au_fixture_instance_new(),
            dispose: oc_au_fixture_dispose(),
            initialize: oc_au_fixture_initialize(),
            uninitialize: oc_au_fixture_uninitialize(),
            reset: oc_au_fixture_reset(),
            set_property: oc_au_fixture_set_property(),
            get_property: oc_au_fixture_get_property(),
            property_info: oc_au_fixture_property_info(),
            render: oc_au_fixture_render(),
            set_parameter: oc_au_fixture_set_parameter(),
            get_parameter: oc_au_fixture_get_parameter(),
            midi: oc_au_fixture_midi(),
            string_length: oc_au_fixture_string_length(),
            string_capacity: oc_au_fixture_string_capacity(),
            string_c_string: oc_au_fixture_string_c_string(),
            data_create: oc_au_fixture_data_create(),
            data_length: oc_au_fixture_data_length(),
            data_bytes: oc_au_fixture_data_bytes(),
            plist_data: oc_au_fixture_plist_data(),
            plist_create: oc_au_fixture_plist_create(),
            release: oc_au_fixture_release(),
        }
    }
}

fn unit_api() -> Api {
    // SAFETY: every override is a deterministic native unit fixture with matching ABI.
    unsafe {
        Api {
            midi_list: Some(oc_au_unit_midi_list()),
            find_next: oc_au_unit_find_next(),
            description: oc_au_unit_description(),
            instance_new: oc_au_unit_instance_new(),
            dispose: oc_au_unit_dispose(),
            initialize: oc_au_unit_initialize(),
            uninitialize: oc_au_unit_uninitialize(),
            reset: oc_au_unit_reset(),
            set_property: oc_au_unit_set_property(),
            property_info: oc_au_unit_property_info(),
            get_property: oc_au_unit_get_property(),
            get_parameter: oc_au_unit_get_parameter(),
            set_parameter: oc_au_unit_set_parameter(),
            midi: oc_au_unit_midi(),
            render: oc_au_unit_render(),
            copy_name: oc_au_unit_copy_name(),
            ..Api::default()
        }
    }
}

#[test]
fn every_au_operation_preserves_native_results_and_contains_each_exception_kind() {
    fn assert_native<T: std::fmt::Debug + PartialEq>(operation: &str, actual: T, expected: T) {
        assert_eq!(actual, expected, "native return from {operation}");
    }
    macro_rules! case {
        ($index:literal, $label:literal, |$owner:ident| $call:expr, $expected:expr) => {
            for mode in 0..=3 {
                // SAFETY: configuration and counters belong to the calling native test thread.
                unsafe { oc_au_fixture_mode(mode) };
                let $owner = Boundary::new(api());
                // SAFETY: the native fixture never dereferences placeholder arguments.
                let result = unsafe { $call };
                if mode == 0 {
                    assert_native($label, result.unwrap(), $expected);
                    assert!($owner.fault().is_none());
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(error.operation, $label);
                    assert_eq!(
                        error.kind,
                        match mode {
                            1 => AuForeignFaultKind::Cpp,
                            2 => AuForeignFaultKind::Unknown,
                            _ => AuForeignFaultKind::ObjectiveC,
                        }
                    );
                    // SAFETY: the poisoned boundary refuses before invoking the fixture again.
                    assert_eq!(unsafe { $call }, Err(error));
                    assert_eq!($owner.fault().map(|fault| fault.error), Some(error));
                }
                // SAFETY: this read accesses only the calling thread's native counter.
                assert_eq!(unsafe { oc_au_fixture_calls($index) }, 1);
            }
        };
    }
    case!(
        0,
        "AudioComponentFindNext",
        |boundary| boundary.find_next(FindNext {
            component: ptr::null_mut(),
            description: ptr::null()
        }),
        ptr::dangling_mut::<c_void>()
    );
    case!(
        1,
        "AudioComponentCopyName",
        |boundary| boundary.copy_name(CopyName {
            component: ptr::null_mut(),
            name: ptr::null_mut()
        }),
        71
    );
    case!(
        2,
        "AudioComponentGetDescription",
        |boundary| boundary.description(Description {
            component: ptr::null_mut(),
            description: ptr::null_mut()
        }),
        71
    );
    case!(
        3,
        "AudioComponentInstanceNew",
        |boundary| boundary.instance_new(InstanceNew {
            component: ptr::null_mut(),
            instance: ptr::null_mut()
        }),
        71
    );
    case!(
        4,
        "AudioComponentInstanceDispose",
        |boundary| boundary.dispose(Dispose {
            unit: ptr::null_mut()
        }),
        71
    );
    case!(
        5,
        "AudioUnitInitialize",
        |boundary| boundary.initialize(Initialize {
            unit: ptr::null_mut()
        }),
        71
    );
    case!(
        6,
        "AudioUnitUninitialize",
        |boundary| boundary.uninitialize(Uninitialize {
            unit: ptr::null_mut()
        }),
        71
    );
    case!(
        7,
        "AudioUnitReset",
        |boundary| boundary.reset(Reset {
            unit: ptr::null_mut(),
            scope: 0,
            element: 0
        }),
        71
    );
    case!(
        8,
        "AudioUnitSetProperty",
        |boundary| boundary.set_property(SetProperty {
            unit: ptr::null_mut(),
            property: 0,
            scope: 0,
            element: 0,
            data: ptr::null(),
            size: 0
        }),
        71
    );
    case!(
        9,
        "AudioUnitGetProperty",
        |boundary| boundary.get_property(GetProperty {
            unit: ptr::null_mut(),
            property: 0,
            scope: 0,
            element: 0,
            data: ptr::null_mut(),
            size: ptr::null_mut()
        }),
        71
    );
    case!(
        10,
        "AudioUnitGetPropertyInfo",
        |boundary| boundary.property_info(PropertyInfo {
            unit: ptr::null_mut(),
            property: 0,
            scope: 0,
            element: 0,
            size: ptr::null_mut(),
            writable: ptr::null_mut()
        }),
        71
    );
    case!(
        11,
        "AudioUnitRender",
        |boundary| boundary.render(Render {
            unit: ptr::null_mut(),
            flags: ptr::null_mut(),
            timestamp: ptr::null(),
            bus: 0,
            frames: 0,
            data: ptr::null_mut()
        }),
        71
    );
    case!(
        12,
        "AudioUnitSetParameter",
        |boundary| boundary.set_parameter(SetParameter {
            unit: ptr::null_mut(),
            parameter: 0,
            scope: 0,
            element: 0,
            value: 0.0,
            offset: 0
        }),
        71
    );
    case!(
        13,
        "AudioUnitGetParameter",
        |boundary| boundary.get_parameter(GetParameter {
            unit: ptr::null_mut(),
            parameter: 0,
            scope: 0,
            element: 0,
            value: ptr::null_mut()
        }),
        71
    );
    case!(
        14,
        "MusicDeviceMIDIEvent",
        |boundary| boundary.midi(Midi {
            unit: ptr::null_mut(),
            status: 0x90,
            data1: 0,
            data2: 0,
            offset: 0
        }),
        71
    );
    case!(
        15,
        "MusicDeviceMIDIEventList",
        |boundary| boundary.midi_list(
            oc_au_fixture_midi_list(),
            MidiList {
                unit: ptr::null_mut(),
                offset: 0,
                events: ptr::null()
            }
        ),
        71
    );
    case!(
        16,
        "CFStringGetLength",
        |boundary| boundary.string_length(StringLength {
            text: ptr::null_mut()
        }),
        9
    );
    case!(
        17,
        "CFStringGetMaximumSizeForEncoding",
        |boundary| boundary.string_capacity(StringCapacity {
            length: 0,
            encoding: 0
        }),
        9
    );
    case!(
        18,
        "CFStringGetCString",
        |boundary| boundary.string_c_string(StringCString {
            text: ptr::null_mut(),
            buffer: ptr::null_mut(),
            capacity: 0,
            encoding: 0
        }),
        1
    );
    case!(
        19,
        "CFDataCreate",
        |boundary| boundary.data_create(DataCreate {
            allocator: ptr::null(),
            bytes: ptr::null(),
            length: 0
        }),
        ptr::dangling_mut::<c_void>()
    );
    case!(
        20,
        "CFDataGetLength",
        |boundary| boundary.data_length(DataLength {
            data: ptr::null_mut()
        }),
        9
    );
    case!(
        21,
        "CFDataGetBytePtr",
        |boundary| boundary.data_bytes(DataBytes {
            data: ptr::null_mut()
        }),
        ptr::dangling::<u8>()
    );
    case!(
        22,
        "CFPropertyListCreateData",
        |boundary| boundary.plist_data(PlistData {
            allocator: ptr::null(),
            plist: ptr::null_mut(),
            format: 0,
            options: 0,
            error: ptr::null_mut()
        }),
        ptr::dangling_mut::<c_void>()
    );
    case!(
        23,
        "CFPropertyListCreateWithData",
        |boundary| boundary.plist_create(PlistCreate {
            allocator: ptr::null(),
            data: ptr::null_mut(),
            options: 0,
            format: ptr::null_mut(),
            error: ptr::null_mut()
        }),
        ptr::dangling_mut::<c_void>()
    );
    case!(
        24,
        "CFRelease",
        |boundary| boundary.release(Release {
            value: ptr::null_mut()
        }),
        ()
    );
}

fn configure(operation: u32, occurrence: u32, mode: u32, status: i32) {
    // SAFETY: the native fixture state is thread-local and used by this test only.
    unsafe { oc_au_unit_config(operation, occurrence, mode, status) };
}
fn calls(operation: u32) -> u32 {
    // SAFETY: the bounded index reads thread-local native fixture counters.
    unsafe { oc_au_fixture_calls(operation) }
}
fn fixture_with(api: Api, effect: bool) -> AuInstance {
    configure(u32::MAX, 1, 0, 0);
    let foreign = Arc::new(Boundary::new(api));
    let lifecycle = api_lock();
    AuInstance::instantiate(
        InstanceRequest {
            description: AudioComponentDescription {
                component_type: if effect {
                    TYPE_EFFECT
                } else {
                    TYPE_MUSIC_DEVICE
                },
                ..AudioComponentDescription::default()
            },
            name: "Deterministic AU fixture".into(),
            sample_rate: 48_000,
            max_frames: 32,
        },
        foreign,
        &lifecycle,
    )
    .unwrap()
}
fn fixture() -> AuInstance {
    fixture_with(unit_api(), false)
}

#[test]
fn native_fixture_first_render_events_and_reset_preserve_samples_without_host_heap_work() {
    let mut unit = fixture();
    configure(u32::MAX, 1, 0, 0);
    unit.note_on_channel(0, 60, 0.8, 0);
    assert!(unit.queue_param_by_name("Gain", 0.75, 3));
    let mut output = [99.0; 64];
    let mut rendered = Ok(());
    let count = oc_test_support::operations(|| {
        rendered = unit.process_block_into(&mut output, None);
    });
    assert_eq!(count, 0);
    rendered.unwrap();
    assert_eq!(calls(11), 1);
    assert_eq!(calls(14), 1);
    assert_eq!(calls(12), 1);
    for frame in 0..32 {
        assert_eq!(
            output[frame * 2].to_bits(),
            ((frame + 1) as f32 / 64.0).to_bits()
        );
        assert_eq!(
            output[frame * 2 + 1].to_bits(),
            (-((frame + 1) as f32) / 64.0).to_bits()
        );
    }
    let reference = output;
    let mut reset = Ok(());
    assert_eq!(
        oc_test_support::operations(|| {
            reset = unit.reset_prepared();
        }),
        0
    );
    reset.unwrap();
    assert_eq!(
        oc_test_support::operations(|| {
            rendered = unit.process_block_into(&mut output, None);
        }),
        0
    );
    rendered.unwrap();
    assert_eq!(output, reference);
}

#[test]
fn actual_instance_refuses_every_followup_after_render_midi_or_parameter_exception() {
    for (operation, label) in [
        (11, "AudioUnitRender"),
        (14, "MusicDeviceMIDIEvent"),
        (15, "MusicDeviceMIDIEventList"),
        (12, "AudioUnitSetParameter"),
    ] {
        let mut unit = fixture();
        if operation == 15 {
            unit.midi_protocol = MidiProtocol::Midi2;
        }
        if operation == 14 || operation == 15 {
            unit.note_on_channel(0, 60, 0.5, 0);
        }
        if operation == 12 {
            assert!(unit.queue_param_by_name("Gain", 0.5, 0));
        }
        configure(operation, 1, 2, 0);
        let mut output = [17.0; 64];
        let mut result = Ok(());
        // The fixture throws an integer, so exception construction itself does not
        // enter Rust's allocator. This assertion measures host-side heap operations.
        assert_eq!(
            oc_test_support::operations(|| {
                result = unit.process_block_into(&mut output, None);
            }),
            0
        );
        let error = result.unwrap_err();
        assert!(
            matches!(error, AuProcessError::Foreign(AuForeignError { operation, kind: AuForeignFaultKind::Unknown }) if operation == label)
        );
        assert_eq!(output, [17.0; 64]);
        let first = unit.foreign_fault().unwrap();
        let counters: [u32; 25] = std::array::from_fn(|index| calls(index as u32));
        assert_eq!(unit.reset_prepared(), Err(error));
        assert_eq!(unit.prepare_block_buffers(), Err(error));
        assert_eq!(unit.process_block_into(&mut output, None), Err(error));
        assert!(!unit.set_param_by_name("Gain", 0.2));
        assert_eq!(unit.get_param_by_name("Gain"), None);
        assert!(unit.try_parameter_surface().is_err());
        assert!(unit.authored_parameter_owner().is_err());
        assert!(unit.set_state_bytes(b"not a plist").is_err());
        assert!(unit.state_bytes().is_err());
        assert_eq!(unit.foreign_fault(), Some(first));
        drop(unit);
        assert_eq!(
            std::array::from_fn::<_, 25, _>(|index| calls(index as u32)),
            counters
        );
    }
}

#[test]
fn original_render_status_is_distinct_from_exception_and_explicit_reset_can_recover() {
    let mut unit = fixture();
    configure(11, 1, 0, -7731);
    let mut output = [6.0; 64];
    assert_eq!(
        unit.process_block_into(&mut output, None),
        Err(AuProcessError::Native {
            operation: "AudioUnitRender",
            status: -7731
        })
    );
    assert!(unit.foreign_fault().is_none());
    assert_eq!(output, [6.0; 64]);
    unit.reset_prepared().unwrap();
    unit.process_block_into(&mut output, None).unwrap();
    assert_eq!(calls(11), 2);
}

#[test]
fn first_foreign_fault_reports_once_even_after_an_ordinary_error_was_reported() {
    use oc_audio::{AudioEffect, ProcessingFailureReport};

    let mut unit = fixture();
    configure(11, 1, 0, -7731);
    assert!(unit.process_block_into(&mut [0.0; 64], None).is_err());
    let ProcessingFailureReport::Ready(ordinary) = unit.take_processing_failure() else {
        panic!("the ordinary native error must be reportable");
    };
    assert!(ordinary.contains("AudioUnitRender"));
    assert!(ordinary.contains("-7731"));
    assert!(matches!(
        unit.take_processing_failure(),
        ProcessingFailureReport::Reported
    ));

    configure(12, 1, 2, 0);
    assert!(!unit.set_param_by_name("Gain", 0.25));
    let first = unit.foreign_fault().unwrap();
    assert_eq!(first.error.operation, "AudioUnitSetParameter");
    let ProcessingFailureReport::Ready(foreign) = unit.take_processing_failure() else {
        panic!("a first foreign fault owns an independent report");
    };
    assert!(foreign.contains("AudioUnitSetParameter"));
    assert!(foreign.contains("Nonstandard native exception"));
    assert!(matches!(
        unit.take_processing_failure(),
        ProcessingFailureReport::Reported
    ));
    assert!(unit.reset_prepared().is_err());
    assert_eq!(calls(7), 0);
    assert_eq!(unit.foreign_fault(), Some(first));
    assert!(matches!(
        unit.take_processing_failure(),
        ProcessingFailureReport::Reported
    ));
}

#[test]
fn ordinary_error_reporting_stays_one_shot_and_reset_rearms_it() {
    use oc_audio::{AudioEffect, ProcessingFailureReport};

    let mut unit = fixture();
    assert!(matches!(
        unit.take_processing_failure(),
        ProcessingFailureReport::Unavailable
    ));
    for status in [-7731, -7732] {
        configure(11, 1, 0, status);
        assert!(unit.process_block_into(&mut [0.0; 64], None).is_err());
        let ProcessingFailureReport::Ready(message) = unit.take_processing_failure() else {
            panic!("a newly latched ordinary error must report once");
        };
        assert!(message.contains(&status.to_string()));
        assert!(matches!(
            unit.take_processing_failure(),
            ProcessingFailureReport::Reported
        ));
        unit.reset_prepared().unwrap();
        assert!(matches!(
            unit.take_processing_failure(),
            ProcessingFailureReport::Unavailable
        ));
    }
    assert!(unit.foreign_fault().is_none());
}

#[test]
fn checked_owner_contains_native_query_and_setter_exceptions_without_later_writes() {
    use crate::preset_parameters::{
        AuthoredParameterOwner, ParameterErrorKind, ParameterWriteDomain,
    };
    let mut unit = fixture();
    configure(10, 1, 1, 0);
    let error = unit.authored_parameter_owner().err().unwrap();
    assert!(matches!(
        error,
        ParameterErrorKind::Native {
            operation: "AudioUnitGetPropertyInfo",
            ..
        }
    ));
    assert_eq!(calls(10), 1);
    assert_eq!(calls(12), 0);
    drop(unit);
    assert_eq!(calls(12), 0);

    let mut unit = fixture();
    configure(u32::MAX, 1, 0, 0);
    let mut owner = unit.authored_parameter_owner().unwrap();
    let binding = owner.resolve_authored_parameter("Gain").unwrap();
    let prepared = owner
        .prepare_bound_value(&binding.key, 0.25, ParameterWriteDomain::NativePlain)
        .unwrap();
    configure(12, 1, 3, 0);
    let error = owner
        .write_bound_parameter(&binding.key, prepared.value, prepared.domain)
        .unwrap_err();
    assert!(matches!(
        error,
        ParameterErrorKind::Native {
            operation: "AudioUnitSetParameter",
            ..
        }
    ));
    assert!(owner.check_parameter_authority().is_err());
    assert_eq!(calls(12), 1);
    drop(owner);
    drop(unit);
    assert_eq!(calls(12), 1);

    let mut unit = fixture();
    configure(u32::MAX, 1, 0, 0);
    let mut owner = unit.authored_parameter_owner().unwrap();
    let binding = owner.resolve_authored_parameter("Gain").unwrap();
    let prepared = owner
        .prepare_bound_value(&binding.key, 0.25, ParameterWriteDomain::NativePlain)
        .unwrap();
    owner
        .write_bound_parameter(&binding.key, prepared.value, prepared.domain)
        .unwrap();
    configure(9, 1, 2, 0);
    let error = owner.check_parameter_authority().unwrap_err();
    assert!(matches!(
        error,
        ParameterErrorKind::Native {
            operation: "AudioUnitGetProperty",
            ..
        }
    ));
    assert!(
        owner
            .write_bound_parameter(&binding.key, prepared.value, prepared.domain)
            .is_err()
    );
    assert_eq!(calls(12), 0);
}

#[test]
fn successful_retirement_detaches_callbacks_and_disposes_exactly_once() {
    let unit = fixture_with(unit_api(), true);
    // SAFETY: native getters read only the fixture's thread-local retained refcons.
    unsafe {
        assert_eq!(
            oc_au_unit_input_refcon(),
            unit.input.as_ref() as *const InputState as *mut c_void
        );
        assert_eq!(
            oc_au_unit_clock_refcon(),
            unit.host_clock.as_ref() as *const HostClock as *mut c_void
        );
    }
    configure(u32::MAX, 1, 0, 0);
    drop(unit);
    assert_eq!(calls(8), 2);
    assert_eq!(calls(6), 1);
    assert_eq!(calls(4), 1);
    // SAFETY: successful production teardown clears both fixture callbacks before disposal.
    unsafe {
        assert!(oc_au_unit_input_refcon().is_null());
        assert!(oc_au_unit_clock_refcon().is_null());
    }
}

#[test]
fn failed_retirement_retains_all_callback_backing_and_never_reenters_native_cleanup() {
    for (operation, mode, status) in [(8, 1, 0), (8, 0, -991), (6, 3, 0), (4, 2, 0), (4, 0, -992)] {
        let mut unit = fixture_with(unit_api(), true);
        let input = unit.input.as_ref() as *const InputState;
        let clock = unit.host_clock.as_ref() as *const HostClock;
        let left = unit.input_left.as_ptr();
        let right = unit.input_right.as_ptr();
        let output_left = unit.output_left.as_ptr();
        let output_right = unit.output_right.as_ptr();
        configure(operation, 1, mode, status);
        let lifecycle = api_lock();
        assert!(unit.teardown().is_err());
        drop(lifecycle);
        assert!(unit.retirement == Retirement::Quarantined);
        let counters: [u32; 25] = std::array::from_fn(|index| calls(index as u32));
        drop(unit);
        assert_eq!(
            std::array::from_fn::<_, 25, _>(|index| calls(index as u32)),
            counters
        );
        // SAFETY: failed production retirement deliberately retains all six allocations.
        // The fixture has no native worker; after checking retention we can reclaim
        // these test-owned boxes without exposing any dangling production callback.
        unsafe {
            oc_au_unit_forget_callbacks();
            assert_eq!((*input).capacity.load(Ordering::Relaxed), 32);
            assert_eq!((*input).left.load(Ordering::Relaxed), left as *mut f32);
            assert_eq!((*input).right.load(Ordering::Relaxed), right as *mut f32);
            let _ = (*clock).playing.load(Ordering::Relaxed);
            drop(Box::from_raw(input as *mut InputState));
            drop(Box::from_raw(clock as *mut HostClock));
            for plane in [left, right, output_left, output_right] {
                drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
                    plane as *mut f32,
                    32,
                )));
            }
        }
    }
}

#[test]
fn state_roundtrip_uses_guarded_cf_and_preserves_original_status() {
    let mut unit = fixture();
    configure(u32::MAX, 1, 0, 0);
    let state = unit.state_bytes().unwrap();
    assert!(!state.is_empty());
    unit.set_state_bytes(&state).unwrap();
    configure(8, 1, 0, -7543);
    let error = unit.set_state_bytes(&state).unwrap_err();
    assert_eq!(error.status, Some(-7543));
    assert!(error.foreign.is_none());
}

#[test]
fn partial_constructor_output_is_never_disposed_after_a_foreign_exception() {
    configure(3, 1, 1, 0);
    let foreign = Arc::new(Boundary::new(unit_api()));
    let lifecycle = api_lock();
    let result = AuInstance::instantiate(
        InstanceRequest {
            description: AudioComponentDescription {
                component_type: TYPE_EFFECT,
                ..Default::default()
            },
            name: "Throwing AU fixture".into(),
            sample_rate: 48_000,
            max_frames: 16,
        },
        foreign.clone(),
        &lifecycle,
    );
    let error = result.err().unwrap();
    assert_eq!(error.kind, AuErrorKind::ForeignException);
    assert_eq!(error.status, None);
    let first = error.foreign.unwrap();
    assert_eq!(first.error.operation, "AudioComponentInstanceNew");
    assert_eq!(first.error.kind, AuForeignFaultKind::Cpp);
    assert_eq!(foreign.fault(), Some(*first));
    assert_eq!(calls(3), 1);
    assert_eq!(calls(4), 0);
    assert_eq!(calls(5), 0);
}

#[test]
fn failed_initialization_never_publishes_a_partial_metadata_inventory() {
    for (operation, label) in [
        (5, "AudioUnitInitialize"),
        (10, "AudioUnitGetPropertyInfo"),
        (9, "AudioUnitGetProperty"),
    ] {
        configure(operation, 1, 2, 0);
        let foreign = Arc::new(Boundary::new(unit_api()));
        let lifecycle = api_lock();
        let result = AuInstance::instantiate(
            InstanceRequest {
                description: AudioComponentDescription {
                    component_type: TYPE_EFFECT,
                    ..Default::default()
                },
                name: "Faulting AU fixture".into(),
                sample_rate: 48_000,
                max_frames: 16,
            },
            foreign.clone(),
            &lifecycle,
        );
        let error = result.err().unwrap();
        assert_eq!(error.kind, AuErrorKind::ForeignException);
        assert_eq!(error.foreign.as_ref().unwrap().error.operation, label);
        assert_eq!(calls(operation), 1);
        assert_eq!(calls(4), 0);
        assert_eq!(calls(6), 0);
    }
}

#[test]
fn cf_state_faults_are_typed_and_prevent_finalization_or_later_au_reentry() {
    for operation in [20, 21, 22, 24] {
        let mut native = unit_api();
        // SAFETY: each replacement is the matching native ABI throw fixture.
        unsafe {
            match operation {
                20 => native.data_length = oc_au_fixture_data_length(),
                21 => native.data_bytes = oc_au_fixture_data_bytes(),
                22 => native.plist_data = oc_au_fixture_plist_data(),
                24 => native.release = oc_au_fixture_release(),
                _ => unreachable!(),
            }
        }
        let mut unit = fixture_with(native, false);
        configure(operation, 1, 3, 0);
        let error = unit.state_bytes().unwrap_err();
        assert_eq!(error.status, None);
        let first = error.foreign.unwrap();
        assert_eq!(first.error.kind, AuForeignFaultKind::ObjectiveC);
        assert_eq!(calls(operation), 1);
        let counters: [u32; 25] = std::array::from_fn(|index| calls(index as u32));
        assert!(unit.set_state_bytes(b"invalid").is_err());
        assert!(unit.state_bytes().is_err());
        assert!(unit.reset_prepared().is_err());
        drop(unit);
        assert_eq!(
            std::array::from_fn::<_, 25, _>(|index| calls(index as u32)),
            counters
        );
    }
}

#[test]
fn copied_cf_component_names_keep_bounded_decoder_faults_and_release_faults_explicit() {
    for operation in [16, 17, 18, 24] {
        let mut native = unit_api();
        // SAFETY: each getter returns the native fixture with exactly this CF ABI.
        unsafe {
            match operation {
                16 => native.string_length = oc_au_fixture_string_length(),
                17 => native.string_capacity = oc_au_fixture_string_capacity(),
                18 => native.string_c_string = oc_au_fixture_string_c_string(),
                24 => native.release = oc_au_fixture_release(),
                _ => unreachable!(),
            }
        }
        configure(operation, 1, 2, 0);
        let foreign = Boundary::new(native);
        let _api = api_lock();
        // SAFETY: the fixture supplies a real retained CFString through the production name adapter.
        let error =
            unsafe { component_name(&AudioComponentDescription::default(), &foreign) }.unwrap_err();
        assert_eq!(error.kind, AuErrorKind::ForeignException);
        assert_eq!(error.status, None);
        assert_eq!(
            error.foreign.unwrap().error.kind,
            AuForeignFaultKind::Unknown
        );
        assert_eq!(calls(operation), 1);
        let counters: [u32; 25] = std::array::from_fn(|index| calls(index as u32));
        assert!(
            // SAFETY: the description remains live; this already-poisoned boundary
            // refuses before entering native registry/CF code or adopting any reference.
            unsafe { component_name(&AudioComponentDescription::default(), &foreign) }.is_err()
        );
        assert_eq!(
            std::array::from_fn::<_, 25, _>(|index| calls(index as u32)),
            counters
        );
    }
}

#[test]
fn checked_surface_preserves_parameter_read_status_without_poisoning_or_default_substitution() {
    let unit = fixture();
    // SAFETY: this setter changes only the calling thread's native fixture value.
    unsafe { oc_au_unit_parameter_value(0.25) };
    configure(13, 1, 0, -7521);
    let error = unit.try_parameter_surface().unwrap_err();
    assert_eq!(error.kind, AuErrorKind::PropertyFailed);
    assert_eq!(error.status, Some(-7521));
    assert_eq!(error.message, "AudioUnitGetParameter");
    assert!(error.foreign.is_none());
    assert!(unit.foreign_fault().is_none());
    assert_eq!(calls(13), 1);

    configure(13, 1, 0, -7521);
    assert_eq!(unit.parameter_surface(), crate::params::surface(Vec::new()));
    let recovered = unit.try_parameter_surface().unwrap();
    assert_eq!(recovered.parameters()[0].current, 0.25);
    assert_eq!(recovered.parameters()[0].default, Some(0.5));
    assert!(unit.foreign_fault().is_none());
}

#[test]
fn checked_surface_refuses_nonfinite_native_values_before_publication_and_can_retry() {
    let unit = fixture();
    configure(u32::MAX, 1, 0, 0);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        // SAFETY: this setter supplies a malformed native value to the actual getter adapter.
        unsafe { oc_au_unit_parameter_value(value) };
        let error = unit.try_parameter_surface().unwrap_err();
        assert_eq!(error.kind, AuErrorKind::PropertyFailed);
        assert_eq!(error.status, None);
        assert!(error.foreign.is_none());
        assert!(unit.foreign_fault().is_none());
        assert_eq!(unit.parameter_surface(), crate::params::surface(Vec::new()));
    }
    // SAFETY: restore a finite native value so the same instance can retry without reset.
    unsafe { oc_au_unit_parameter_value(0.25) };
    let recovered = unit.try_parameter_surface().unwrap();
    let json = serde_json::to_value(&recovered).unwrap();
    for field in ["current", "normalized", "default"] {
        assert!(json["parameters"][0][field].as_f64().unwrap().is_finite());
    }
    assert_eq!(json["parameters"][0]["current"], 0.25);
}

#[test]
fn checked_surface_refuses_invalid_cached_metadata_before_native_read_or_json_publication() {
    let mut unit = fixture();
    let valid = unit.params[0].clone();
    for (min, max, default) in [
        (f64::NAN, 1.0, 0.5),
        (0.0, f64::INFINITY, 0.5),
        (0.0, 1.0, f64::NAN),
        (1.0, 0.0, 0.5),
    ] {
        configure(u32::MAX, 1, 0, 0);
        unit.params[0].min = min;
        unit.params[0].max = max;
        unit.params[0].default = default;
        let error = unit.try_parameter_surface().unwrap_err();
        assert_eq!(error.kind, AuErrorKind::PropertyFailed);
        assert_eq!(error.status, None);
        assert!(error.foreign.is_none());
        assert_eq!(calls(13), 0);
        assert_eq!(unit.parameter_surface(), crate::params::surface(Vec::new()));
        assert_eq!(calls(13), 0);
        unit.params[0] = valid.clone();
    }
}

#[test]
fn partial_class_info_outputs_never_enter_cf_release_without_success_and_exact_size() {
    let mut native = unit_api();
    // SAFETY: getters return matching native ABIs; release counts sentinel entry without dereferencing it.
    unsafe {
        native.get_property = oc_au_partial_state_property();
        native.release = oc_au_counted_release();
    }
    let unit = fixture_with(native, false);
    for status in [-7661, 0] {
        configure(9, 1, 0, status);
        let error = unit.state_bytes().unwrap_err();
        assert_eq!(error.kind, AuErrorKind::StateFailed);
        assert_eq!(error.status, if status == 0 { None } else { Some(status) });
        assert!(error.foreign.is_none());
        assert!(unit.foreign_fault().is_none());
        assert_eq!(calls(9), 1);
        assert_eq!(calls(24), 0);
    }
}

#[test]
fn failed_partial_component_name_output_is_unowned_and_preserves_status_without_poison() {
    let mut native = unit_api();
    // SAFETY: the injected signatures exactly match CopyName and Release.
    unsafe {
        native.copy_name = oc_au_partial_copy_name();
        native.release = oc_au_counted_release();
    }
    let mut foreign = Boundary::new(native);
    configure(1, 1, 0, -7662);
    let _api = api_lock();
    // SAFETY: the native fixture uses this description only as an opaque query identity.
    let error =
        unsafe { component_name(&AudioComponentDescription::default(), &foreign) }.unwrap_err();
    assert_eq!(error.kind, AuErrorKind::PropertyFailed);
    assert_eq!(error.status, Some(-7662));
    assert_eq!(error.message, "AudioComponentCopyName");
    assert!(error.foreign.is_none());
    assert!(foreign.fault().is_none());
    assert_eq!(calls(24), 0);
    // SAFETY: switch to the valid native CFString provider on the same healthy boundary.
    foreign.api.copy_name = unsafe { oc_au_unit_copy_name() };
    configure(u32::MAX, 1, 0, 0);
    // SAFETY: a successful complete CopyName now transfers a real native CFString reference.
    let name = unsafe { component_name(&AudioComponentDescription::default(), &foreign) }.unwrap();
    assert_eq!(name.as_deref(), Some("OpenCompose AU fixture"));
    assert_eq!(calls(24), 1);
    assert!(foreign.fault().is_none());
}

#[test]
fn ordinary_failed_constructor_output_never_enters_dispose() {
    configure(3, 1, 0, -7663);
    let foreign = Arc::new(Boundary::new(unit_api()));
    let lifecycle = api_lock();
    let error = AuInstance::instantiate(
        InstanceRequest {
            description: AudioComponentDescription {
                component_type: TYPE_EFFECT,
                ..Default::default()
            },
            name: "Partial AU constructor".into(),
            sample_rate: 48_000,
            max_frames: 16,
        },
        foreign.clone(),
        &lifecycle,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, AuErrorKind::InitFailed);
    assert_eq!(error.status, Some(-7663));
    assert!(error.foreign.is_none());
    assert!(foreign.fault().is_none());
    assert_eq!(calls(3), 1);
    assert_eq!(calls(4), 0);
}

#[test]
fn malformed_load_time_parameter_sizes_share_checked_owner_bounds_and_never_enter_list_reader() {
    for bytes in [5, 16_388, u32::MAX, u32::MAX - 3] {
        let mut count = Some(0);
        assert_eq!(
            oc_test_support::operations(|| {
                count = authored_parameters::parameter_list_len(bytes);
            }),
            0
        );
        assert_eq!(count, None);
        let mut native = unit_api();
        // SAFETY: native getters match the owner ABIs, with a malformed discovery length only.
        unsafe {
            native.property_info = oc_au_malformed_list_info();
            native.get_property = oc_au_counted_list_property();
            oc_au_unit_list_size(bytes);
            oc_au_list_reads_reset();
        }
        configure(u32::MAX, 1, 0, 0);
        let foreign = Arc::new(Boundary::new(native));
        let lifecycle = api_lock();
        let error = AuInstance::instantiate(
            InstanceRequest {
                description: AudioComponentDescription {
                    component_type: TYPE_EFFECT,
                    ..Default::default()
                },
                name: "Malformed AU list fixture".into(),
                sample_rate: 48_000,
                max_frames: 16,
            },
            foreign.clone(),
            &lifecycle,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, AuErrorKind::PropertyFailed);
        assert!(error.foreign.is_none());
        assert!(foreign.fault().is_none());
        // SAFETY: this is only the calling thread's native list-reader counter.
        assert_eq!(unsafe { oc_au_list_reads() }, 0);
        assert_eq!(calls(10), 1);
    }
    assert_eq!(authored_parameters::parameter_list_len(0), Some(0));
    assert_eq!(authored_parameters::parameter_list_len(16_384), Some(4096));
}

#[test]
fn partial_parameter_info_never_admits_cf_name_ownership_or_publishes_empty_inventory() {
    let mut native = unit_api();
    // SAFETY: metadata fixture supplies a partial result within the advertised full buffer capacity.
    unsafe {
        native.get_property = oc_au_partial_parameter_info();
        native.release = oc_au_counted_release();
    }
    configure(u32::MAX, 1, 0, 0);
    let foreign = Arc::new(Boundary::new(native));
    let lifecycle = api_lock();
    let error = AuInstance::instantiate(
        InstanceRequest {
            description: AudioComponentDescription {
                component_type: TYPE_EFFECT,
                ..Default::default()
            },
            name: "Partial AU metadata fixture".into(),
            sample_rate: 48_000,
            max_frames: 16,
        },
        foreign.clone(),
        &lifecycle,
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, AuErrorKind::PropertyFailed);
    assert_eq!(error.status, None);
    assert!(error.foreign.is_none());
    assert!(foreign.fault().is_none());
    assert_eq!(calls(16), 0);
    assert_eq!(calls(24), 0);
}

#[test]
fn prepared_au_mapped_transport_keeps_only_caller_clock_and_restores_reset_context() {
    let mut unit = fixture();
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n  time_sig_map:\n    2:1 = 3/4\n",
    )
    .unwrap();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
    let weak = Arc::downgrade(&clock);
    let transport = PluginTransport::new(96_000, 48_000, 120.0).with_clock(clock);
    assert_eq!(weak.strong_count(), 1);
    let properties_before = calls(8);
    let mut output = [0.0; 64];
    let mut result = Ok(());
    let mut reset = Ok(());
    assert_eq!(
        oc_test_support::operations(|| {
            result = unit.process_block_into(&mut output, Some(&transport));
            if result.is_ok() {
                reset = unit.reset_prepared();
            }
        }),
        0
    );
    result.unwrap();
    reset.unwrap();
    assert_eq!(
        calls(8),
        properties_before,
        "render/reset must not write an invalid transport property"
    );
    assert_eq!(
        weak.strong_count(),
        1,
        "AU processing must not retain a mapped clock"
    );
    assert_eq!(
        unit.host_clock.tempo.load(Ordering::Relaxed),
        120.0f64.to_bits()
    );
    assert_eq!(unit.host_clock.sample_pos.load(Ordering::Relaxed), 0);
    assert!(!unit.host_clock.playing.load(Ordering::Relaxed));
    drop(transport);
    assert_eq!(
        weak.strong_count(),
        0,
        "only caller retirement releases the clock"
    );
    // Reset still has its scalar origin after the caller retires the clock.
    assert_eq!(
        oc_test_support::operations(|| {
            reset = unit.reset_prepared();
        }),
        0
    );
    reset.unwrap();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
    let transport = PluginTransport::new(96_000, 48_000, 120.0).with_clock(clock);
    assert_eq!(
        oc_test_support::operations(|| {
            result = unit.process_block_into(&mut output, Some(&transport));
        }),
        0
    );
    result.unwrap();
    assert_eq!(
        unit.host_clock.tempo.load(Ordering::Relaxed),
        60.0f64.to_bits()
    );
    assert_eq!(unit.host_clock.numerator.load(Ordering::Relaxed), 3);
    assert_eq!(unit.host_clock.sample_pos.load(Ordering::Relaxed), 96_000);
}

#[test]
fn prepared_au_processing_parameters_and_reset_do_not_acquire_control_mutex() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let completed = std::thread::scope(|scope| {
        let mut starts = Vec::new();
        for effect in [false, true] {
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            let (start_tx, start_rx) = mpsc::channel();
            starts.push(start_tx);
            scope.spawn(move || {
                // Each fixture's native context is thread-local. Construct and
                // retire it on this worker, before/after the control lock hold.
                let mut unit = fixture_with(unit_api(), effect);
                let project = oc_dsl::parse("@project\n  tempo: 127\n").unwrap();
                let transport = PluginTransport::new(0, 48_000, 127.0).with_clock(Arc::new(
                    oc_dsl::timing::ProjectClock::new(&project.properties).unwrap(),
                ));
                ready.send(()).unwrap();
                if start_rx.recv().is_err() {
                    return;
                }
                let mut output = [0.0; 64];
                let mut result = Ok(());
                let mut reset = Ok(());
                let mut set = false;
                let mut read = None;
                let heap = oc_test_support::operations(|| {
                    set = unit.set_param_by_name("Gain", 0.75);
                    read = unit.get_param_by_name("Gain");
                    if effect {
                        result = unit.process_effect_into(&mut output, 48_000, Some(&transport));
                    } else {
                        result = unit.process_block_into(&mut output, Some(&transport));
                    }
                    if result.is_ok() {
                        reset = unit.reset_prepared();
                    }
                });
                done.send((set, read, result, reset, heap, output)).unwrap();
                // Drop may wait for serialized lifecycle; the parent releases
                // that guard before joining, including a failed watchdog.
            });
        }
        let ready = (
            ready_rx.recv_timeout(Duration::from_secs(10)),
            ready_rx.recv_timeout(Duration::from_secs(10)),
        );
        let lifecycle = api_lock();
        for start in starts {
            let _ = start.send(());
        }
        let first = done_rx.recv_timeout(Duration::from_secs(10));
        let second = done_rx.recv_timeout(Duration::from_secs(10));
        drop(lifecycle);
        (ready, first, second)
    });
    assert!(
        completed.0.0.is_ok() && completed.0.1.is_ok(),
        "fixture construction did not finish"
    );
    for result in [completed.1, completed.2] {
        let (set, read, process, reset, heap, output) =
            result.expect("prepared operations blocked on the held control mutex");
        assert!(set);
        assert_eq!(read, Some(0.75));
        process.unwrap();
        reset.unwrap();
        assert_eq!(heap, 0);
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert_ne!(output, [0.0; 64]);
    }
}

#[test]
fn prepared_au_matches_direct_native_render_parameter_reset_and_status_contracts() {
    let mut unit = fixture();
    let parameter = unit.params[0].id;
    assert_eq!(
        // SAFETY: the test retains this healthy fixture and the exact scalar output.
        unsafe {
            unit.foreign.set_parameter(SetParameter {
                unit: unit.unit,
                parameter,
                scope: SCOPE_GLOBAL,
                element: 0,
                value: 0.75,
                offset: 0,
            })
        }
        .unwrap(),
        0
    );
    let mut direct_value = 0.0;
    assert_eq!(
        // SAFETY: retained unit and writable scalar outlive the synchronous read.
        unsafe {
            unit.foreign.get_parameter(GetParameter {
                unit: unit.unit,
                parameter,
                scope: SCOPE_GLOBAL,
                element: 0,
                value: &mut direct_value,
            })
        }
        .unwrap(),
        0
    );
    assert_eq!(
        unit.get_param_by_name("Gain"),
        Some(f64::from(direct_value))
    );
    let mut left = [0.0f32; 32];
    let mut right = [0.0f32; 32];
    let mut planes = AudioBufferList {
        number_buffers: 2,
        buffers: [
            AudioBuffer {
                number_channels: 1,
                data_byte_size: 128,
                data: left.as_mut_ptr().cast(),
            },
            AudioBuffer {
                number_channels: 1,
                data_byte_size: 128,
                data: right.as_mut_ptr().cast(),
            },
        ],
    };
    let timestamp = AudioTimeStamp::at_sample(0.0);
    let mut flags = 0;
    assert_eq!(
        // SAFETY: both retained planes and all typed render arguments remain live.
        unsafe {
            unit.foreign.render(Render {
                unit: unit.unit,
                flags: &mut flags,
                timestamp: &timestamp,
                bus: 0,
                frames: 32,
                data: &mut planes,
            })
        }
        .unwrap(),
        0
    );
    assert_eq!(
        // SAFETY: exclusive fixture ownership retains the native unit through reset.
        unsafe {
            unit.foreign.reset(Reset {
                unit: unit.unit,
                scope: SCOPE_GLOBAL,
                element: 0,
            })
        }
        .unwrap(),
        0
    );
    let mut output = [0.0; 64];
    assert!(unit.set_param_by_name("Gain", 0.75));
    unit.process_block_into(&mut output, None).unwrap();
    for frame in 0..32 {
        assert_eq!(output[frame * 2].to_bits(), left[frame].to_bits());
        assert_eq!(output[frame * 2 + 1].to_bits(), right[frame].to_bits());
    }
    unit.reset_prepared().unwrap();
    let mut after_reset = [0.0; 64];
    unit.process_block_into(&mut after_reset, None).unwrap();
    assert_eq!(after_reset, output);
    configure(12, 1, 0, -7729);
    assert!(!unit.set_param_by_name("Gain", 0.5));
    assert_eq!(
        unit.process_error,
        Some(AuProcessError::Native {
            operation: "AudioUnitSetParameter",
            status: -7729
        })
    );
    assert!(
        !unit.foreign.failed(),
        "ordinary OSStatus must remain distinct from foreign quarantine"
    );
}

#[test]
fn prepared_au_retires_valid_input_frames_after_render_and_reset_without_resizing_planes() {
    let mut unit = fixture();
    let capacity = unit.input.capacity.load(Ordering::Relaxed);
    assert_eq!(unit.input.valid_frames.load(Ordering::Acquire), 0);
    let mut output = [0.25; 16];
    unit.process_effect_into(&mut output, 48_000, None).unwrap();
    assert_eq!(unit.input.valid_frames.load(Ordering::Acquire), 0);
    assert_eq!(unit.input.capacity.load(Ordering::Relaxed), capacity);
    unit.input.valid_frames.store(8, Ordering::Release);
    unit.reset_prepared().unwrap();
    assert_eq!(unit.input.valid_frames.load(Ordering::Acquire), 0);
    assert_eq!(unit.input.capacity.load(Ordering::Relaxed), capacity);
}
