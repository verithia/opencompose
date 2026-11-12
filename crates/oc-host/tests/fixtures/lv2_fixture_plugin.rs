//! Test-only LV2 bundle. The host build script compiles the shared native cdylib.
//!
//! Exported descriptors:
//!   0 stereo gain with one control input and state
//!   1 stereo instrument with one MIDI atom input
//!   2 descriptor used by a manifest that requires the worker extension
//!   3 mono gain
//!   4 stereo gain that reports latency
//!   5 multi-output descriptor used by a manifest that exceeds stereo
//!   6 aligned Atom input/output contract and checked MIDI payloads

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_void};

type Lv2Handle = *mut c_void;

#[repr(C)]
struct Feature {
    uri: *const c_char,
    data: *mut c_void,
}

type InstantiateFn = unsafe extern "C" fn(
    descriptor: *const Descriptor,
    sample_rate: f64,
    bundle_path: *const c_char,
    features: *const *const Feature,
) -> Lv2Handle;
type ConnectPortFn = unsafe extern "C" fn(Lv2Handle, u32, *mut c_void);
type RunFn = unsafe extern "C" fn(Lv2Handle, u32);
type SimpleFn = unsafe extern "C" fn(Lv2Handle);
type ExtensionDataFn = unsafe extern "C" fn(*const c_char) -> *const c_void;

#[repr(C)]
pub struct Descriptor {
    uri: *const c_char,
    instantiate: Option<InstantiateFn>,
    connect_port: Option<ConnectPortFn>,
    activate: Option<SimpleFn>,
    run: Option<RunFn>,
    deactivate: Option<SimpleFn>,
    cleanup: Option<SimpleFn>,
    extension_data: Option<ExtensionDataFn>,
}

unsafe impl Sync for Descriptor {}

type StateStoreFn = unsafe extern "C" fn(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> u32;
type StateRetrieveFn = unsafe extern "C" fn(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void;

#[repr(C)]
struct StateInterface {
    save: Option<
        unsafe extern "C" fn(
            handle: Lv2Handle,
            store: StateStoreFn,
            store_handle: *mut c_void,
            flags: u32,
            features: *const *const Feature,
        ) -> u32,
    >,
    restore: Option<
        unsafe extern "C" fn(
            handle: Lv2Handle,
            retrieve: StateRetrieveFn,
            retrieve_handle: *mut c_void,
            flags: u32,
            features: *const *const Feature,
        ) -> u32,
    >,
}

const STATE_SUCCESS: u32 = 0;
const STATE_TYPE: u32 = 0;
const STATE_FLAGS: u32 = 1;
const STATE_KEY: u32 = 1;

static GAIN_URI: &[u8] = b"http://opencompose.local/lv2/fixture/gain\0";
static TONE_URI: &[u8] = b"http://opencompose.local/lv2/fixture/tone\0";
static WORKER_URI: &[u8] = b"http://opencompose.local/lv2/fixture/needs-worker\0";
static MONO_URI: &[u8] = b"http://opencompose.local/lv2/fixture/mono\0";
static LATENCY_URI: &[u8] = b"http://opencompose.local/lv2/fixture/latency\0";
static WIDE_URI: &[u8] = b"http://opencompose.local/lv2/fixture/wide\0";
static STATE_URI: &[u8] = b"http://lv2plug.in/ns/ext/state#interface\0";

#[derive(Default)]
struct Gain {
    in_l: *const f32,
    in_r: *const f32,
    out_l: *mut f32,
    out_r: *mut f32,
    gain: *mut f32,
    gain_value: f32,
    latency: *mut f32,
}

#[derive(Default)]
struct Mono {
    in_l: *const f32,
    out_l: *mut f32,
    gain: *mut f32,
}

struct Tone {
    out_l: *mut f32,
    out_r: *mut f32,
    atom_in: *const u8,
    sample_rate: f64,
    phase: f64,
    frequency: f64,
    amplitude: f64,
    note: u8,
    active: bool,
}

impl Default for Tone {
    fn default() -> Self {
        Self {
            out_l: std::ptr::null_mut(),
            out_r: std::ptr::null_mut(),
            atom_in: std::ptr::null(),
            sample_rate: 48_000.0,
            phase: 0.0,
            frequency: 440.0,
            amplitude: 0.0,
            note: 0,
            active: false,
        }
    }
}

unsafe extern "C" fn gain_instantiate(
    _descriptor: *const Descriptor,
    sample_rate: f64,
    _bundle_path: *const c_char,
    _features: *const *const Feature,
) -> Lv2Handle {
    let _ = sample_rate;
    Box::into_raw(Box::new(Gain {
        gain_value: 1.0,
        ..Gain::default()
    })) as Lv2Handle
}

unsafe extern "C" fn gain_connect_port(handle: Lv2Handle, port: u32, data: *mut c_void) {
    let gain = unsafe { &mut *(handle as *mut Gain) };
    match port {
        0 => gain.in_l = data as *const f32,
        1 => gain.in_r = data as *const f32,
        2 => gain.out_l = data as *mut f32,
        3 => gain.out_r = data as *mut f32,
        4 => gain.gain = data as *mut f32,
        5 => gain.latency = data as *mut f32,
        _ => {}
    }
}

unsafe extern "C" fn gain_run(handle: Lv2Handle, frames: u32) {
    let gain = unsafe { &mut *(handle as *mut Gain) };
    let amount = if gain.gain.is_null() {
        gain.gain_value
    } else {
        unsafe { *gain.gain }
    };
    let input_l = if gain.in_l.is_null() {
        std::ptr::null()
    } else {
        gain.in_l
    };
    let input_r = if gain.in_r.is_null() {
        input_l
    } else {
        gain.in_r
    };
    for frame in 0..frames as usize {
        let left = if input_l.is_null() {
            0.0
        } else {
            unsafe { *input_l.add(frame) }
        };
        let right = if input_r.is_null() {
            0.0
        } else {
            unsafe { *input_r.add(frame) }
        };
        if !gain.out_l.is_null() {
            unsafe { *gain.out_l.add(frame) = left * amount };
        }
        if !gain.out_r.is_null() {
            unsafe { *gain.out_r.add(frame) = right * amount };
        }
    }
}

unsafe extern "C" fn gain_activate(handle: Lv2Handle) {
    let gain = unsafe { &mut *(handle as *mut Gain) };
    if !gain.latency.is_null() {
        unsafe { *gain.latency = 64.0 };
    }
}

unsafe extern "C" fn gain_cleanup(handle: Lv2Handle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut Gain) });
    }
}

unsafe extern "C" fn gain_save(
    handle: Lv2Handle,
    store: StateStoreFn,
    store_handle: *mut c_void,
    _flags: u32,
    _features: *const *const Feature,
) -> u32 {
    let gain = unsafe { &*(handle as *const Gain) };
    let value = if gain.gain.is_null() {
        gain.gain_value
    } else {
        unsafe { *gain.gain }
    };
    unsafe {
        store(
            store_handle,
            STATE_KEY,
            &value as *const f32 as *const c_void,
            std::mem::size_of::<f32>(),
            STATE_TYPE,
            STATE_FLAGS,
        )
    }
}

unsafe extern "C" fn gain_restore(
    handle: Lv2Handle,
    retrieve: StateRetrieveFn,
    retrieve_handle: *mut c_void,
    _flags: u32,
    _features: *const *const Feature,
) -> u32 {
    let gain = unsafe { &mut *(handle as *mut Gain) };
    let mut size = 0usize;
    let mut type_ = 0u32;
    let mut flags = 0u32;
    let value = unsafe {
        retrieve(
            retrieve_handle,
            STATE_KEY,
            &mut size,
            &mut type_,
            &mut flags,
        )
    };
    if value.is_null() || size < std::mem::size_of::<f32>() {
        return STATE_SUCCESS;
    }
    let value = unsafe { *(value as *const f32) };
    gain.gain_value = value;
    if !gain.gain.is_null() {
        unsafe { *gain.gain = value };
    }
    STATE_SUCCESS
}

static GAIN_STATE: StateInterface = StateInterface {
    save: Some(gain_save),
    restore: Some(gain_restore),
};

unsafe extern "C" fn gain_extension_data(uri: *const c_char) -> *const c_void {
    if uri.is_null() {
        return std::ptr::null();
    }
    let uri = unsafe { std::ffi::CStr::from_ptr(uri) };
    if uri.to_bytes() == &STATE_URI[..STATE_URI.len() - 1] {
        &GAIN_STATE as *const StateInterface as *const c_void
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn mono_instantiate(
    _descriptor: *const Descriptor,
    _sample_rate: f64,
    _bundle_path: *const c_char,
    _features: *const *const Feature,
) -> Lv2Handle {
    Box::into_raw(Box::new(Mono::default())) as Lv2Handle
}

unsafe extern "C" fn mono_connect_port(handle: Lv2Handle, port: u32, data: *mut c_void) {
    let mono = unsafe { &mut *(handle as *mut Mono) };
    match port {
        0 => mono.in_l = data as *const f32,
        1 => mono.out_l = data as *mut f32,
        2 => mono.gain = data as *mut f32,
        _ => {}
    }
}

unsafe extern "C" fn mono_cleanup(handle: Lv2Handle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut Mono) });
    }
}

unsafe extern "C" fn mono_run(handle: Lv2Handle, frames: u32) {
    let mono = unsafe { &mut *(handle as *mut Mono) };
    let amount = if mono.gain.is_null() {
        1.0
    } else {
        unsafe { *mono.gain }
    };
    for frame in 0..frames as usize {
        let value = if mono.in_l.is_null() {
            0.0
        } else {
            unsafe { *mono.in_l.add(frame) }
        };
        if !mono.out_l.is_null() {
            unsafe { *mono.out_l.add(frame) = value * amount };
        }
    }
}

unsafe extern "C" fn tone_instantiate(
    _descriptor: *const Descriptor,
    sample_rate: f64,
    _bundle_path: *const c_char,
    _features: *const *const Feature,
) -> Lv2Handle {
    Box::into_raw(Box::new(Tone {
        sample_rate,
        ..Tone::default()
    })) as Lv2Handle
}

unsafe extern "C" fn tone_connect_port(handle: Lv2Handle, port: u32, data: *mut c_void) {
    let tone = unsafe { &mut *(handle as *mut Tone) };
    match port {
        0 => tone.out_l = data as *mut f32,
        1 => tone.out_r = data as *mut f32,
        2 => tone.atom_in = data as *const u8,
        _ => {}
    }
}

fn midi(message: &[u8], tone: &mut Tone) {
    if message.len() < 3 {
        return;
    }
    let status = message[0] & 0xf0;
    let note = message[1] & 0x7f;
    let velocity = message[2] & 0x7f;
    match status {
        0x90 if velocity > 0 => {
            tone.note = note;
            tone.frequency = 440.0 * 2.0f64.powf((f64::from(note) - 69.0) / 12.0);
            tone.amplitude = 0.25 * f64::from(velocity) / 127.0;
            tone.phase = 0.0;
            tone.active = true;
        }
        0x80 | 0x90 => {
            if tone.note == note {
                tone.active = false;
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn tone_run(handle: Lv2Handle, frames: u32) {
    let tone = unsafe { &mut *(handle as *mut Tone) };
    let mut events: Vec<(usize, [u8; 3])> = Vec::new();
    if !tone.atom_in.is_null() {
        let atom_size = unsafe { *(tone.atom_in as *const u32) } as usize;
        let total = 16 + atom_size;
        let mut offset = 16usize;
        while offset + 16 <= total {
            let event_frames = unsafe { *((tone.atom_in.add(offset)) as *const i64) } as usize;
            let event_size = unsafe { *((tone.atom_in.add(offset + 8)) as *const u32) } as usize;
            let payload = offset + 16;
            let event_end = payload + event_size;
            if event_end > total {
                break;
            }
            if event_size == 3 {
                let message = unsafe { std::slice::from_raw_parts(tone.atom_in.add(payload), 3) };
                events.push((event_frames, [message[0], message[1], message[2]]));
            }
            offset = payload + (event_size + 7) / 8 * 8;
        }
    }
    events.sort_by_key(|event| event.0);
    let mut event_index = 0;
    for frame in 0..frames as usize {
        while event_index < events.len() && events[event_index].0 <= frame {
            midi(&events[event_index].1, tone);
            event_index += 1;
        }
        let step = std::f64::consts::TAU * tone.frequency / tone.sample_rate;
        let value = if tone.active {
            let sample = tone.phase.sin() * tone.amplitude;
            tone.phase += step;
            if tone.phase >= std::f64::consts::TAU {
                tone.phase -= std::f64::consts::TAU;
            }
            sample as f32
        } else {
            0.0
        };
        if !tone.out_l.is_null() {
            unsafe { *tone.out_l.add(frame) = value };
        }
        if !tone.out_r.is_null() {
            unsafe { *tone.out_r.add(frame) = value };
        }
    }
}

unsafe extern "C" fn tone_cleanup(handle: Lv2Handle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut Tone) });
    }
}

#[repr(C)]
struct FixtureUridMap {
    handle: *mut c_void,
    map: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> u32>,
}

struct AtomEcho {
    left: *mut f32,
    right: *mut f32,
    input: *const u8,
    output: *mut u8,
    mode: *const f32,
    sequence: u32,
    chunk: u32,
    midi: u32,
}

unsafe extern "C" fn atom_echo_instantiate(
    _descriptor: *const Descriptor,
    _rate: f64,
    bundle: *const c_char,
    features: *const *const Feature,
) -> Lv2Handle {
    if bundle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: LV2 instantiate receives the host's live NUL-terminated path.
    let Ok(bundle) = (unsafe { std::ffi::CStr::from_ptr(bundle) }).to_str() else {
        return std::ptr::null_mut();
    };
    if !std::path::Path::new(bundle).is_absolute()
        || !bundle.ends_with(std::path::MAIN_SEPARATOR)
        || !std::path::Path::new(bundle).join("manifest.ttl").is_file()
    {
        return std::ptr::null_mut();
    }
    for index in 0..16 {
        // SAFETY: the host supplies a null-terminated array retained through instantiate.
        let feature = unsafe { *features.add(index) };
        if feature.is_null() {
            break;
        }
        // SAFETY: a non-null entry points to the host's live Feature and URI.
        let feature = unsafe { &*feature };
        // SAFETY: Feature URI is a valid NUL-terminated ABI string.
        if unsafe { std::ffi::CStr::from_ptr(feature.uri) }.to_bytes()
            != b"http://lv2plug.in/ns/ext/urid#map"
        {
            continue;
        }
        // SAFETY: the matched feature's data has the documented URID map layout.
        let map = unsafe { &*(feature.data as *const FixtureUridMap) };
        let Some(callback) = map.map else {
            return std::ptr::null_mut();
        };
        // SAFETY: the feature handle is live and the three string literals are NUL-terminated; calls are serialized.
        let (sequence, chunk, midi) = unsafe {
            (
                callback(
                    map.handle,
                    b"http://lv2plug.in/ns/ext/atom#Sequence\0".as_ptr().cast(),
                ),
                callback(
                    map.handle,
                    b"http://lv2plug.in/ns/ext/atom#Chunk\0".as_ptr().cast(),
                ),
                callback(
                    map.handle,
                    b"http://lv2plug.in/ns/ext/midi#MidiEvent\0".as_ptr().cast(),
                ),
            )
        };
        return Box::into_raw(Box::new(AtomEcho {
            left: std::ptr::null_mut(),
            right: std::ptr::null_mut(),
            input: std::ptr::null(),
            output: std::ptr::null_mut(),
            mode: std::ptr::null(),
            sequence,
            chunk,
            midi,
        }))
        .cast();
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn atom_echo_connect(handle: Lv2Handle, port: u32, data: *mut c_void) {
    // SAFETY: instantiate returned this unique AtomEcho allocation and host serializes calls.
    let echo = unsafe { &mut *handle.cast::<AtomEcho>() };
    match port {
        0 => echo.left = data.cast(),
        1 => echo.right = data.cast(),
        2 => echo.input = data.cast(),
        3 => echo.output = data.cast(),
        4 => echo.mode = data.cast(),
        _ => {}
    }
}

unsafe extern "C" fn atom_echo_run(handle: Lv2Handle, frames: u32) {
    // SAFETY: host retains the live unique instance and every connected port for this run.
    let echo = unsafe { &mut *handle.cast::<AtomEcho>() };
    let aligned = !echo.input.is_null()
        && !echo.output.is_null()
        && echo.input as usize % 8 == 0
        && echo.output as usize % 8 == 0;
    let mut status = -777.0;
    if aligned {
        // SAFETY: alignment is checked and each connected Atom port holds at least an Atom header.
        let (input_type, input_size, output_type, output_capacity) = unsafe {
            (
                *echo.input.cast::<u32>().add(1),
                *echo.input.cast::<u32>(),
                *echo.output.cast::<u32>().add(1),
                *echo.output.cast::<u32>(),
            )
        };
        if input_type == echo.sequence
            && input_size >= 8
            && input_size <= 8184
            && output_type == echo.chunk
            && output_capacity == 8184
        {
            // SAFETY: the control port is a retained scalar connected by the host.
            let mode = if echo.mode.is_null() {
                0
            } else {
                (unsafe { *echo.mode }) as u32
            };
            // SAFETY: host advertises 8184 body bytes in the aligned retained output; each mode writes within8192.
            unsafe {
                match mode {
                    0 => {
                        // Consume the real incoming MIDI headers and offsets before copying the sequence.
                        let mut offset = 16;
                        let end = input_size as usize + 8;
                        let mut valid = true;
                        while offset < end {
                            if offset + 16 > end {
                                valid = false;
                                break;
                            }
                            let time = *echo.input.add(offset).cast::<i64>();
                            let size = *echo.input.add(offset + 8).cast::<u32>() as usize;
                            let kind = *echo.input.add(offset + 12).cast::<u32>();
                            if time < 0
                                || time >= i64::from(frames)
                                || kind != echo.midi
                                || size != 3
                                || offset + 24 > end
                            {
                                valid = false;
                                break;
                            }
                            // Read all delivered payload bytes through the actual ABI.
                            let payload =
                                std::slice::from_raw_parts(echo.input.add(offset + 16), size);
                            if payload[0] & 0x80 == 0 || payload[1] > 127 || payload[2] > 127 {
                                valid = false;
                                break;
                            }
                            offset += 24;
                        }
                        if valid {
                            std::ptr::copy_nonoverlapping(echo.input, echo.output, end);
                            status = 1.0;
                        }
                    }
                    1 => {
                        // One complete padded SysEx event exactly fills retained capacity.
                        *echo.output.cast::<u32>() = 8184;
                        *echo.output.cast::<u32>().add(1) = echo.sequence;
                        *echo.output.add(8).cast::<u64>() = 0;
                        *echo.output.add(16).cast::<i64>() = i64::from(frames.saturating_sub(1));
                        *echo.output.add(24).cast::<u32>() = 8160;
                        *echo.output.add(28).cast::<u32>() = echo.midi;
                        std::ptr::write_bytes(echo.output.add(32), 0x55, 8160);
                        *echo.output.add(32) = 0xF0;
                        *echo.output.add(8191) = 0xF7;
                        status = 2.0;
                    }
                    2 | 3 => {
                        *echo.output.cast::<u32>() = if mode == 2 { 24 } else { 48 };
                        *echo.output.cast::<u32>().add(1) = echo.sequence;
                        *echo.output.add(8).cast::<u64>() = 0;
                        *echo.output.add(16).cast::<i64>() = 0;
                        *echo.output.add(24).cast::<u32>() = if mode == 2 { 8 } else { 3 };
                        *echo.output.add(28).cast::<u32>() = echo.midi;
                        std::ptr::write_bytes(echo.output.add(32), 0x55, 8);
                        if mode == 3 {
                            *echo.output.add(40).cast::<i64>() = 1;
                            *echo.output.add(48).cast::<u32>() = 3;
                            *echo.output.add(52).cast::<u32>() = echo.midi;
                            std::ptr::write_bytes(echo.output.add(56), 0x55, 8);
                        }
                        status = mode as f32 + 1.0;
                    }
                    _ => {}
                }
            }
        }
    }
    for frame in 0..frames as usize {
        // SAFETY: host connects two writable audio arrays of at least frames elements.
        unsafe {
            *echo.left.add(frame) = status;
            *echo.right.add(frame) = status;
        }
    }
}

unsafe extern "C" fn atom_echo_cleanup(handle: Lv2Handle) {
    // SAFETY: host calls cleanup once for the Box allocated by instantiate.
    drop(unsafe { Box::from_raw(handle.cast::<AtomEcho>()) });
}

static ATOM_ECHO_DESCRIPTOR: Descriptor = Descriptor {
    uri: b"http://opencompose.local/lv2/fixture/atom-echo\0"
        .as_ptr()
        .cast(),
    instantiate: Some(atom_echo_instantiate),
    connect_port: Some(atom_echo_connect),
    activate: None,
    run: Some(atom_echo_run),
    deactivate: None,
    cleanup: Some(atom_echo_cleanup),
    extension_data: None,
};

static GAIN_DESCRIPTOR: Descriptor = Descriptor {
    uri: GAIN_URI.as_ptr() as *const c_char,
    instantiate: Some(gain_instantiate),
    connect_port: Some(gain_connect_port),
    activate: None,
    run: Some(gain_run),
    deactivate: None,
    cleanup: Some(gain_cleanup),
    extension_data: Some(gain_extension_data),
};

static WORKER_DESCRIPTOR: Descriptor = Descriptor {
    uri: WORKER_URI.as_ptr() as *const c_char,
    instantiate: Some(gain_instantiate),
    connect_port: Some(gain_connect_port),
    activate: None,
    run: Some(gain_run),
    deactivate: None,
    cleanup: Some(gain_cleanup),
    extension_data: Some(gain_extension_data),
};

static WIDE_DESCRIPTOR: Descriptor = Descriptor {
    uri: WIDE_URI.as_ptr() as *const c_char,
    instantiate: Some(gain_instantiate),
    connect_port: Some(gain_connect_port),
    activate: None,
    run: Some(gain_run),
    deactivate: None,
    cleanup: Some(gain_cleanup),
    extension_data: None,
};

static LATENCY_DESCRIPTOR: Descriptor = Descriptor {
    uri: LATENCY_URI.as_ptr() as *const c_char,
    instantiate: Some(gain_instantiate),
    connect_port: Some(gain_connect_port),
    activate: Some(gain_activate),
    run: Some(gain_run),
    deactivate: None,
    cleanup: Some(gain_cleanup),
    extension_data: None,
};

static MONO_DESCRIPTOR: Descriptor = Descriptor {
    uri: MONO_URI.as_ptr() as *const c_char,
    instantiate: Some(mono_instantiate),
    connect_port: Some(mono_connect_port),
    activate: None,
    run: Some(mono_run),
    deactivate: None,
    cleanup: Some(mono_cleanup),
    extension_data: None,
};

static TONE_DESCRIPTOR: Descriptor = Descriptor {
    uri: TONE_URI.as_ptr() as *const c_char,
    instantiate: Some(tone_instantiate),
    connect_port: Some(tone_connect_port),
    activate: None,
    run: Some(tone_run),
    deactivate: None,
    cleanup: Some(tone_cleanup),
    extension_data: None,
};

#[unsafe(no_mangle)]
pub extern "C" fn lv2_descriptor(index: u32) -> *const Descriptor {
    match index {
        0 => &GAIN_DESCRIPTOR,
        1 => &TONE_DESCRIPTOR,
        2 => &WORKER_DESCRIPTOR,
        3 => &MONO_DESCRIPTOR,
        4 => &LATENCY_DESCRIPTOR,
        5 => &WIDE_DESCRIPTOR,
        6 => &ATOM_ECHO_DESCRIPTOR,
        _ => std::ptr::null(),
    }
}
