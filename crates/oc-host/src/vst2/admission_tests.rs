//! Admission tests use an owned synthetic ABI descriptor; no module is loaded.
use super::*;
use std::cell::Cell;

// macOS already installs the same allocator in the AU test owner. Other
// platforms need this one owner so the heap assertions below are not vacuous.
#[cfg(not(target_os = "macos"))]
#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

thread_local! {
    static CALLS: Cell<[u32; 64]> = const { Cell::new([0; 64]) };
    static CHUNK_SIZE: Cell<isize> = const { Cell::new(4) };
    static RECT_MODE: Cell<u8> = const { Cell::new(2) };
    static RATE_AT_NOTIFICATION: Cell<f64> = const { Cell::new(0.0) };
}
static CHUNK: &[u8; 4] = b"good";
static RECT: abi::ERect = abi::ERect {
    top: 0,
    left: 0,
    bottom: 240,
    right: 320,
};
static BAD_RECT: abi::ERect = abi::ERect {
    top: 10,
    left: 10,
    bottom: 0,
    right: 0,
};

unsafe extern "C" fn dispatcher(
    effect: *mut abi::AEffect,
    opcode: i32,
    _index: i32,
    _value: isize,
    output: *mut c_void,
    _opt: f32,
) -> isize {
    CALLS.with(|calls| {
        let mut values = calls.get();
        if let Some(slot) = values.get_mut(opcode as usize) {
            *slot += 1;
        }
        calls.set(values);
    });
    match opcode {
        op::SET_SAMPLE_RATE => {
            // SAFETY: The fixture retains descriptor and context through this
            // synchronous dispatcher; the callback returns a thread-local borrow.
            let reply = unsafe {
                audio_master(
                    effect,
                    abi::host_op::GET_TIME,
                    0,
                    0,
                    std::ptr::null_mut(),
                    0.0,
                )
            };
            if reply != 0 {
                // SAFETY: Read-only same-thread borrow, before another request.
                let rate = unsafe { (reply as *const abi::VstTimeInfo).read() }.sample_rate;
                RATE_AT_NOTIFICATION.with(|value| value.set(rate));
            }
            1
        }
        op::GET_CHUNK => {
            let size = CHUNK_SIZE.with(Cell::get);
            let bytes = if size == 4 {
                CHUNK.as_ptr().cast_mut().cast()
            } else {
                std::ptr::NonNull::<u8>::dangling().as_ptr().cast()
            };
            // SAFETY: the production call supplies a writable pointer-sized
            // output. The oversized case deliberately supplies no readable data:
            // admission must reject its count before borrowing a native slice.
            unsafe { output.cast::<*mut c_void>().write(bytes) };
            size
        }
        op::EDIT_GET_RECT => {
            let mode = RECT_MODE.with(Cell::get);
            let rectangle = match mode {
                0 => std::ptr::NonNull::<abi::ERect>::dangling().as_ptr(),
                1 => std::ptr::null_mut(),
                3 => (&BAD_RECT as *const abi::ERect).cast_mut(),
                4 => std::ptr::NonNull::<u8>::dangling().as_ptr().cast(),
                _ => (&RECT as *const abi::ERect).cast_mut(),
            };
            // SAFETY: editor_size supplies writable ERect-pointer output storage.
            // Refusal may partially write a sentinel; its target is never read.
            unsafe { output.cast::<*mut abi::ERect>().write(rectangle) };
            if mode == 0 { 0 } else { 1 }
        }
        _ => 0,
    }
}

struct Fixture {
    instance: Vst2Instance,
    // Fields drop in declaration order, so the descriptor outlives instance CLOSE.
    _descriptor: Box<abi::AEffect>,
}
impl Fixture {
    fn new() -> Self {
        CALLS.with(|calls| calls.set([0; 64]));
        CHUNK_SIZE.with(|size| size.set(4));
        RECT_MODE.with(|mode| mode.set(2));
        // SAFETY: AEffect consists of scalars, raw pointers and nullable C
        // function pointers, whose all-zero representations are valid.
        let mut descriptor: Box<abi::AEffect> = Box::new(unsafe { std::mem::zeroed() });
        descriptor.magic = abi::EFFECT_MAGIC;
        descriptor.dispatcher = Some(dispatcher);
        let host = Arc::new(HostContext::new(48_000, 8, 0));
        descriptor.reserved1 = (&*host as *const HostContext) as isize;
        let instance = Vst2Instance {
            effect: &mut *descriptor,
            close_dispatcher: dispatcher,
            host,
            binding: CallbackBinding::Legacy,
            module: None,
            call_scratch: None,
            path: PathBuf::new(),
            name: "Admission fixture".into(),
            vendor: String::new(),
            product: String::new(),
            version: 0,
            category: PluginCategory::Effect,
            flags: abi::FLAG_HAS_EDITOR | abi::FLAG_PROGRAM_CHUNKS,
            num_inputs: 0,
            num_outputs: 0,
            num_params: 0,
            param_names: Vec::new(),
            param_labels: Vec::new(),
            param_defaults: Vec::new(),
            param_can_automate: Vec::new(),
            sample_rate: 48_000,
            max_block_frames: 8,
            accepts_midi: false,
            supports_double: false,
            active: false,
            pending_midi: Vec::new(),
            midi_events: Vec::new(),
            event_pointers: Vec::new(),
            event_list: Vec::new(),
            input_channels: Vec::new(),
            output_channels: Vec::new(),
            last_time_info: abi::VstTimeInfo::default(),
            process_error: None,
            error_reported: Cell::new(false),
        };
        Self {
            instance,
            _descriptor: descriptor,
        }
    }
}
fn calls(opcode: i32) -> u32 {
    CALLS.with(|calls| calls.get()[opcode as usize])
}

#[test]
fn oversized_preparation_refuses_before_allocation_topology_consumption_or_native_block_setter() {
    let mut fixture = Fixture::new();
    fixture
        .instance
        .host
        .io_changed
        .store(true, Ordering::Release);
    for requested in [
        MAX_PREPARED_BLOCK_FRAMES + 1,
        i32::MAX as usize + 1,
        usize::MAX,
    ] {
        let mut result = Ok(());
        let heap = oc_test_support::operations(|| {
            result = fixture.instance.prepare_block_buffers(requested);
        });
        assert_eq!(
            result,
            Err(oc_audio::BlockPreparationError::CapacityExceeded)
        );
        assert_eq!(heap, 0);
        assert_eq!(fixture.instance.max_block_frames, 8);
        assert!(fixture.instance.host.io_changed.load(Ordering::Acquire));
        assert!(fixture.instance.input_channels.is_empty());
        assert_eq!(calls(op::SET_BLOCK_SIZE), 0);
    }
    assert_eq!(
        checked_block_frames(MAX_PREPARED_BLOCK_FRAMES),
        Ok(MAX_PREPARED_BLOCK_FRAMES as u32)
    );
    assert_eq!(checked_block_frames(0), Ok(1));
    let error = match Vst2Instance::load(Path::new("must-not-be-resolved"), 48_000, u32::MAX) {
        Ok(_) => panic!("invalid frame count must be refused before module loading"),
        Err(error) => error,
    };
    assert!(error.contains("block preparation refused"));
}

#[test]
fn oversized_allocating_adapter_records_refusal_without_allocating_requested_output() {
    let mut fixture = Fixture::new();
    let mut output = Vec::new();
    let heap = oc_test_support::operations(|| {
        output = fixture
            .instance
            .process_block_with_transport(u32::MAX, None);
    });
    assert!(output.is_empty());
    assert_eq!(heap, 0);
    assert_eq!(
        fixture.instance.process_error,
        Some(Vst2ProcessError::CapacityExceeded)
    );
    assert_eq!(calls(op::SET_BLOCK_SIZE), 0);
}

#[test]
fn state_capture_rejects_oversized_native_count_before_reading_its_sentinel_pointer() {
    let fixture = Fixture::new();
    CHUNK_SIZE.with(|size| size.set((oc_audio::bounded_file::MAX_PLUGIN_STATE_BYTES + 1) as isize));
    assert!(
        fixture
            .instance
            .state_bytes()
            .unwrap_err()
            .contains("exceeds")
    );
    assert_eq!(calls(op::GET_CHUNK), 1);
    CHUNK_SIZE.with(|size| size.set(4));
    assert_eq!(fixture.instance.state_bytes().unwrap(), b"good");
    assert!(checked_state_chunk_size(oc_audio::bounded_file::MAX_PLUGIN_STATE_BYTES).is_ok());
}

#[test]
fn state_restore_uses_shared_byte_limit_before_native_setter() {
    let mut fixture = Fixture::new();
    let bytes = vec![0_u8; oc_audio::bounded_file::MAX_PLUGIN_STATE_BYTES + 1];
    assert!(
        fixture
            .instance
            .set_state_bytes(&bytes)
            .unwrap_err()
            .contains("exceeds")
    );
    assert_eq!(calls(op::SET_CHUNK), 0);
    fixture.instance.set_state_bytes(b"good").unwrap();
    assert_eq!(calls(op::SET_CHUNK), 1);
}

#[test]
fn editor_rectangle_requires_success_before_output_admission() {
    let fixture = Fixture::new();
    for mode in [0, 1, 3, 4] {
        RECT_MODE.with(|value| value.set(mode));
        assert_eq!(fixture.instance.editor_size(), None, "mode {mode}");
    }
    RECT_MODE.with(|value| value.set(2));
    assert_eq!(fixture.instance.editor_size(), Some((320, 240)));
}

#[cfg(unix)]
#[test]
fn unix_module_path_retains_non_utf8_bytes_and_refuses_embedded_nul() {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(std::ffi::OsStr::from_bytes(b"plugin-\xff.so"));
    assert_eq!(
        module_path_cstring(path).unwrap().as_bytes(),
        b"plugin-\xff.so"
    );
    assert!(module_path_cstring(Path::new(std::ffi::OsStr::from_bytes(b"plugin\0.so"))).is_err());
}

#[test]
fn time_info_zero_rate_is_refused_before_load_or_native_rate_notification() {
    let error = match Vst2Instance::load(Path::new("must-not-be-resolved"), 0, 64) {
        Ok(_) => panic!("zero sample rate must not enter module loading"),
        Err(error) => error,
    };
    assert_eq!(error, "VST2 sample rate must be positive");
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.instance.set_sample_rate(0),
        Err(Vst2ProcessError::SampleRateChanged)
    );
    assert_eq!(fixture.instance.sample_rate, 48_000);
    assert_eq!(calls(op::SET_SAMPLE_RATE), 0);
    let mut buffer = [0.25; 4];
    let heap = oc_test_support::operations(|| {
        fixture
            .instance
            .process_effect_with_transport(&mut buffer, 0, None);
    });
    assert_eq!(heap, 0);
    assert_eq!(buffer, [0.25; 4]);
    assert_eq!(
        fixture.instance.process_error,
        Some(Vst2ProcessError::SampleRateChanged)
    );
    assert_eq!(calls(op::SET_SAMPLE_RATE), 0);
    fixture.instance.set_sample_rate(96_000).unwrap();
    assert_eq!(calls(op::SET_SAMPLE_RATE), 1);
    assert_eq!(RATE_AT_NOTIFICATION.with(Cell::get), 96_000.0);
    let stopped = fixture.instance.host.set_time(None);
    assert_eq!(stopped.sample_rate, 96_000.0);
    assert_eq!(stopped.sample_pos, 0.0);
    assert_eq!(stopped.flags, 0);
}

#[test]
fn callback_feature_queries_accept_exact_names_and_refuse_other_bytes_without_heap() {
    let names: &[&[u8]] = &[
        b"sendVstEvents\0",
        b"sendVstMidiEvent\0",
        b"sendVstTimeInfo\0",
        b"receiveVstEvents\0",
        b"receiveVstMidiEvent\0",
        b"receiveVstTimeInfo\0",
        b"reportConnectionChanges\0",
        b"acceptIOChanges\0",
        b"startStopProcess\0",
    ];
    let rejected: &[&[u8]] = &[
        b"\0",
        b"\xff\0",
        b"sendVst\0",
        b"sendVstEvents-extra\0",
        b"reportConnectionChangesx\0",
    ];
    let heap = oc_test_support::operations(|| {
        for name in names {
            // SAFETY: Each input is an independently sized NUL-terminated byte
            // allocation. Short inputs must not be expanded into a budget slice.
            let supported = unsafe { supported_host_feature(name.as_ptr()) };
            assert!(supported);
        }
        for name in rejected {
            // SAFETY: The same bounded native C-string contract holds even
            // when its bytes are not UTF-8 or its feature is unsupported.
            let supported = unsafe { supported_host_feature(name.as_ptr()) };
            assert!(!supported);
        }
        // SAFETY: Null is explicitly accepted as an unavailable query, with no read.
        let supported = unsafe { supported_host_feature(std::ptr::null()) };
        assert!(!supported);
    });
    assert_eq!(heap, 0);
}
