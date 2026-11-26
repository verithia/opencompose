//! Minimal VST2 plugin used by the host tests.
//!
//! Compiled by `build.rs` with plain `rustc` into a loadable bundle. It is a
//! standalone program on purpose: it declares the plugin side of the interface
//! itself, exactly like a third-party plugin would.
//!
//! Variants (selected with `--cfg`):
//! * default      gain + DC offset effect, 2 in / 2 out, chunk state
//! * program_bank gain + DC offset effect with three named parameter programs
//! * instrument   note-driven tone, 0 in / 2 out, accumulating `process` only
//! * broken       entry point returns no plugin
//! * no_output    descriptor reports zero outputs
//! * multi_out    descriptor reports no inputs and four stereo output pairs

#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, c_char, c_void};
use std::os::raw::c_int;

const EFFECT_MAGIC: i32 = 0x5673_7450;
const FLAG_CAN_REPLACING: i32 = 1 << 4;
const FLAG_PROGRAM_CHUNKS: i32 = 1 << 5;
#[cfg(any(instrument_mode, multi_out_mode))]
const FLAG_IS_SYNTH: i32 = 1 << 8;
const MIDI_EVENT_TYPE: i32 = 1;

type HostCallback = unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type Dispatcher = unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
type Process = unsafe extern "C" fn(*mut AEffect, *const *const f32, *mut *mut f32, i32);
type SetParameter = unsafe extern "C" fn(*mut AEffect, i32, f32);
type GetParameter = unsafe extern "C" fn(*mut AEffect, i32) -> f32;

#[repr(C)]
pub struct AEffect {
    magic: i32,
    dispatcher: Option<Dispatcher>,
    process: Option<Process>,
    set_parameter: Option<SetParameter>,
    get_parameter: Option<GetParameter>,
    num_programs: i32,
    num_params: i32,
    num_inputs: i32,
    num_outputs: i32,
    flags: i32,
    reserved1: isize,
    reserved2: isize,
    initial_delay: i32,
    real_qualities: i32,
    off_qualities: i32,
    io_ratio: f32,
    object: *mut c_void,
    user: *mut c_void,
    unique_id: i32,
    version: i32,
    process_replacing: Option<Process>,
    process_double_replacing: Option<*mut c_void>,
    future: [u8; 56],
}

#[repr(C)]
struct VstTimeInfo {
    sample_pos: f64,
    sample_rate: f64,
    nano_seconds: f64,
    ppq_pos: f64,
    tempo: f64,
    bar_start_pos: f64,
    cycle_start_pos: f64,
    cycle_end_pos: f64,
    time_sig_numerator: i32,
    time_sig_denominator: i32,
    smpte_offset: i32,
    smpte_frame_rate: i32,
    samples_to_next_clock: i32,
    flags: i32,
}

#[repr(C)]
struct VstMidiEvent {
    event_type: i32,
    byte_size: i32,
    delta_frames: i32,
    flags: i32,
    note_length: i32,
    note_offset: i32,
    midi_data: [u8; 4],
    detune: i8,
    note_off_velocity: u8,
    reserved1: u8,
    reserved2: u8,
}

struct Fixture {
    host: HostCallback,
    effect: *mut AEffect,
    params: [f32; 2],
    #[cfg(program_bank_mode)]
    program_index: usize,
    #[cfg(program_bank_mode)]
    programs: [([f32; 2], [u8; 25]); 3],
    authored_name_mode: u32,
    authored_setter_action: u32,
    authored_setter_calls: usize,
    authored_alternate_calls: usize,
    sample_rate: f32,
    block_size: i32,
    active: bool,
    history_enabled: bool,
    history: [f32; 8],
    note: Option<u8>,
    voice_amplitude: f32,
    #[cfg(instrument_mode)]
    has_processed: bool,
    #[cfg(multi_out_mode)]
    processed_blocks: u32,
    last_tempo: f64,
    last_ppq: f64,
    last_sample_pos: f64,
    host_version: isize,
    chunk: Vec<u8>,
}

impl Fixture {
    fn store_name(&self, ptr: *mut c_void, text: &str) {
        if ptr.is_null() {
            return;
        }
        let bytes = text.as_bytes();
        let length = bytes.len().min(63);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, length);
            *(ptr as *mut u8).add(length) = 0;
        }
    }

    fn display(&self, index: i32) -> String {
        if index == 1 {
            // Encodes the host timeline so tests can assert what the plugin saw.
            format!(
                "dc={:.4} t={:.1} p={:.3} v={}",
                f64::from(self.params[1]),
                self.last_tempo,
                self.last_ppq,
                self.host_version
            )
        } else {
            format!("{:.4}", self.params[index as usize])
        }
    }

    fn get_chunk(&mut self) -> (usize, *mut c_void) {
        self.chunk.clear();
        self.chunk.extend_from_slice(b"OCF2");
        self.chunk.extend_from_slice(&self.params[0].to_le_bytes());
        self.chunk.extend_from_slice(&self.params[1].to_le_bytes());
        (self.chunk.len(), self.chunk.as_mut_ptr() as *mut c_void)
    }

    fn set_chunk(&mut self, index: i32, bytes: *const u8, size: usize) -> isize {
        if bytes.is_null() || size < 12 {
            return 1;
        }
        let bytes = unsafe { std::slice::from_raw_parts(bytes, size) };
        let signature = match index {
            0 => b"OCF2",
            1 => b"OCP2",
            _ => return 1,
        };
        if &bytes[0..4] != signature {
            return 1;
        }
        #[cfg(stall_restore_mode)]
        if bytes == b"OCF2stalling" {
            use std::io::Write;
            if let Some(marker) = std::env::var_os("OCP_TEST_VST2_RESTORE_MARKER") {
                let mut trace = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(marker)
                    .unwrap();
                writeln!(trace, "restore entered").unwrap();
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        self.params[0] = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
        self.params[1] = f32::from_le_bytes(bytes[8..12].try_into().unwrap());
        0
    }

    fn handle_events(&mut self, events: *const u8) {
        if events.is_null() {
            return;
        }
        let count = unsafe { *(events as *const i32) };
        let pointers = unsafe { events.add(16) as *const *mut VstMidiEvent };
        for index in 0..count.max(0) as usize {
            let event = unsafe { *pointers.add(index) };
            if event.is_null() {
                continue;
            }
            let event = unsafe { &*event };
            if event.event_type != MIDI_EVENT_TYPE {
                continue;
            }
            let status = event.midi_data[0] & 0xf0;
            match status {
                0x90 if event.midi_data[1] > 0 => {
                    // The tone fixture's unused DC knob enables a processor
                    // which ignores notes received before its first block.
                    #[cfg(instrument_mode)]
                    if self.params[1] > 0.5 && !self.has_processed {
                        continue;
                    }
                    self.note = Some(event.midi_data[1]);
                    self.voice_amplitude = 0.5 * f32::from(event.midi_data[2]) / 127.0;
                }
                0x80 | 0x90 => {
                    if self.note == Some(event.midi_data[1]) {
                        self.note = None;
                    }
                }
                _ => {}
            }
            // Exercise unchanged notices and a real topology change at the
            // same MIDI dispatch boundary.
            if status == 0xb0 && event.midi_data[1] == 119 && event.midi_data[2] >= 126 {
                // SAFETY: The fixture owns this live descriptor during MIDI dispatch.
                unsafe {
                    if event.midi_data[2] == 127 {
                        (*self.effect).num_outputs = 1;
                    }
                    (self.host)(
                        self.effect,
                        13, /* audioMasterIOChanged */
                        0,
                        0,
                        std::ptr::null_mut(),
                        0.0,
                    );
                }
            }
            if status == 0xb0 && event.midi_data[1] == 7 {
                let value = f32::from(event.midi_data[2]) / 127.0;
                self.params[0] = value;
                unsafe {
                    (self.host)(
                        self.effect,
                        0, // audioMasterAutomate
                        0,
                        0,
                        std::ptr::null_mut(),
                        value,
                    );
                }
            }
        }
    }

    fn process(&mut self, inputs: *const *const f32, outputs: *mut *mut f32, frames: c_int) {
        #[cfg(any(instrument_mode, multi_out_mode))]
        let _ = inputs;
        let frames = frames.max(0) as usize;
        let output_count = unsafe { (*self.effect).num_outputs }.clamp(0, 8) as usize;
        if !self.active {
            for channel in 0..output_count {
                let output = unsafe { *outputs.add(channel) };
                if !output.is_null() {
                    unsafe { std::slice::from_raw_parts_mut(output, frames).fill(0.0) };
                }
            }
            return;
        }
        let info = unsafe { (self.host)(self.effect, 7, 0, 0, std::ptr::null_mut(), 0.0) };
        if info != 0 {
            let info = unsafe { &*(info as *const VstTimeInfo) };
            self.last_tempo = info.tempo;
            self.last_ppq = info.ppq_pos;
            self.last_sample_pos = info.sample_pos;
        }
        let gain = self.params[0] * 2.0;
        #[cfg(instrument_mode)]
        {
            self.has_processed |= frames > 0;
        }
        #[cfg(multi_out_mode)]
        {
            self.processed_blocks = self.processed_blocks % 8 + 1;
        }
        #[cfg(not(any(instrument_mode, multi_out_mode)))]
        let dc = self.params[1] * 0.25;
        for channel in 0..output_count {
            let output = unsafe { *outputs.add(channel) };
            if output.is_null() {
                continue;
            }
            let destination = unsafe { std::slice::from_raw_parts_mut(output, frames) };
            #[cfg(any(instrument_mode, multi_out_mode))]
            {
                let amplitude = self.note.map_or(0.0, |_| self.voice_amplitude) * gain;
                #[cfg(multi_out_mode)]
                let amplitude = if channel == 7 && self.params[1] > 0.5 {
                    f32::NAN
                } else {
                    amplitude * (channel as f32 / 8.0 + self.processed_blocks as f32 / 64.0)
                };
                destination.fill(amplitude);
            }
            #[cfg(not(any(instrument_mode, multi_out_mode)))]
            {
                let input = if inputs.is_null() {
                    std::ptr::null()
                } else {
                    unsafe { *inputs.add(channel) }
                };
                if input.is_null() {
                    destination.fill(dc);
                } else {
                    let source = unsafe { std::slice::from_raw_parts(input, frames) };
                    #[cfg(sidechain_mode)]
                    let key =
                        unsafe { std::slice::from_raw_parts(*inputs.add(channel + 2), frames) };
                    for frame in 0..frames {
                        #[cfg(sidechain_mode)]
                        let signal = source[frame] + key[frame];
                        #[cfg(not(sidechain_mode))]
                        let signal = source[frame];
                        let sample = if self.history_enabled {
                            std::mem::replace(&mut self.history[channel], signal)
                        } else {
                            signal
                        };
                        destination[frame] = sample * gain + dc;
                    }
                }
            }
        }
    }
}

unsafe extern "C" fn dispatcher(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    opt: f32,
) -> isize {
    let fixture = unsafe { &mut *((*effect).object as *mut Fixture) };
    match opcode {
        0 => 1, // effOpen
        #[cfg(exit_on_close_mode)]
        1 => std::process::exit(79),
        #[cfg(not(exit_on_close_mode))]
        1 => {
            // Controlled native lifecycle observation, ignored by ordinary hosts.
            unsafe { (fixture.host)(effect, 0x4f43_0002, 0, 0, std::ptr::null_mut(), 0.0) };
            drop(unsafe { Box::from_raw((*effect).object as *mut Fixture) });
            drop(unsafe { Box::from_raw(effect) });
            0
        }
        #[cfg(program_bank_mode)]
        2 if (0..3).contains(&value) => {
            fixture.programs[fixture.program_index].0 = fixture.params;
            fixture.program_index = value as usize;
            fixture.params = fixture.programs[fixture.program_index].0;
            0
        }
        #[cfg(program_bank_mode)]
        3 => fixture.program_index as isize,
        #[cfg(program_bank_mode)]
        4 if !ptr.is_null() => {
            // SAFETY: The dispatcher's program name is a nul-terminated input
            // and the fixture copies its bytes before returning.
            let name = unsafe { CStr::from_ptr(ptr.cast::<c_char>()) }.to_bytes();
            let text = &mut fixture.programs[fixture.program_index].1;
            text.fill(0);
            let length = name.len().min(text.len() - 1);
            text[..length].copy_from_slice(&name[..length]);
            0
        }
        #[cfg(program_bank_mode)]
        5 => {
            let text = &fixture.programs[fixture.program_index].1;
            let name = CStr::from_bytes_until_nul(text).unwrap().to_string_lossy();
            fixture.store_name(ptr, &name);
            1
        }
        6 | 7 | 8 => {
            // param label / display / name
            let text = match opcode {
                8 => {
                    if fixture.authored_name_mode == 9 {
                        // SAFETY: the fixture retains this live AEffect and host callback until close.
                        unsafe { (fixture.host)(effect, 42, 0, 0, std::ptr::null_mut(), 0.0) };
                    }
                    match (fixture.authored_name_mode, index) {
                        (1, 0) => "Rate Hz".into(),
                        (1, 1) => "Rate-Hz".into(),
                        (2, 1) => "#0".into(),
                        (3, 0) => "明るさ".into(),
                        (3, 1) => "亮度".into(),
                        (5, 0) => String::new(),
                        (_, 0) => "Gain".into(),
                        (_, 1) => "DC Offset".into(),
                        _ => String::new(),
                    }
                }
                6 => match index {
                    0 => "dB".to_string(),
                    1 => "%".to_string(),
                    _ => String::new(),
                },
                _ => fixture.display(index),
            };
            fixture.store_name(ptr, &text);
            1
        }
        10 => {
            fixture.sample_rate = opt;
            1
        }
        11 => {
            fixture.block_size = value as i32;
            1
        }
        12 => {
            fixture.active = value != 0;
            fixture.history.fill(0.0);
            #[cfg(instrument_mode)]
            {
                fixture.has_processed = false;
            }
            1
        }
        13 => 0, // no editor
        22 => EFFECT_MAGIC as isize,
        23 => {
            let (size, pointer) = fixture.get_chunk();
            unsafe { *(ptr as *mut *mut c_void) = pointer };
            size as isize
        }
        24 => fixture.set_chunk(index, ptr as *const u8, value as usize),
        25 => {
            unsafe { (fixture.host)(effect, 0x4f43_0004, 0, 0, std::ptr::null_mut(), 0.0) };
            fixture.handle_events(ptr as *const u8);
            1
        }
        26 => {
            if fixture.authored_name_mode == 4 {
                0
            } else {
                1
            }
        }
        35 => {
            #[cfg(any(instrument_mode, multi_out_mode))]
            {
                2
            }
            #[cfg(not(any(instrument_mode, multi_out_mode)))]
            {
                1
            }
        }
        45 => {
            #[cfg(instrument_mode)]
            let name = "OpenCompose Tone Fixture";
            #[cfg(multi_out_mode)]
            let name = "OpenCompose Multi Out Fixture";
            #[cfg(not(any(instrument_mode, multi_out_mode)))]
            let name = "OpenCompose Gain Fixture";
            fixture.store_name(ptr, name);
            1
        }
        47 => {
            fixture.store_name(ptr, "OpenCompose Tests");
            1
        }
        48 => {
            fixture.store_name(ptr, "Host Fixture");
            1
        }
        49 => 100,
        51 => {
            let feature = unsafe { CStr::from_ptr(ptr as *const c_char) }
                .to_str()
                .unwrap_or("");
            match feature {
                "receiveVstEvents" | "receiveVstMidiEvent" | "sendVstEvents" => 1,
                "supportsDoublePrecision" => -1,
                _ => 0,
            }
        }
        52 => 0,
        58 => 2400,
        // Private generated-fixture controls. Tests require callback execution;
        // none of these opcodes are implemented or sent by production code.
        10000 => {
            fixture.authored_name_mode = value as u32;
            // SAFETY: these private controls mutate only the fixture-owned live descriptor.
            unsafe {
                if value == 6 {
                    (*effect).num_params = 4097;
                }
                if value == 7 {
                    (*effect).set_parameter = None;
                }
            }
            1
        }
        10001 => {
            fixture.authored_setter_action = value as u32;
            1
        }
        10002 => fixture.authored_setter_calls as isize,
        10003 => fixture.authored_alternate_calls as isize,
        10004 => {
            fixture.history_enabled = value != 0;
            1
        }
        _ => 0,
    }
}

unsafe extern "C" fn set_parameter(effect: *mut AEffect, index: i32, value: f32) {
    // SAFETY: the fixture entry allocated this object and it remains live until effClose.
    let fixture = unsafe { &mut *((*effect).object as *mut Fixture) };
    fixture.authored_setter_calls += 1;
    if let Some(slot) = fixture.params.get_mut(index as usize) {
        *slot = value.clamp(0.0, 1.0);
    }
    match fixture.authored_setter_action {
        1 => {
            // SAFETY: callback control changes only this fixture's live descriptor.
            unsafe {
                (*effect).set_parameter = None;
            }
        }
        2 | 3 => {
            // SAFETY: live AEffect and callback are retained by the fixture; the
            // host only publishes atomic inventory/topology notifications here.
            unsafe {
                (fixture.host)(
                    effect,
                    if fixture.authored_setter_action == 2 {
                        13
                    } else {
                        42
                    },
                    0,
                    0,
                    std::ptr::null_mut(),
                    0.0,
                )
            };
        }
        4 => {
            // SAFETY: controlled mutation of this live fixture descriptor's scalar count.
            unsafe {
                (*effect).num_params = 3;
            }
        }
        5 => {
            // SAFETY: the alternate callback has the same ABI and lifetime as this one.
            unsafe {
                (*effect).set_parameter = Some(set_parameter_alternate);
            }
        }
        _ => {}
    }
}
unsafe extern "C" fn set_parameter_alternate(effect: *mut AEffect, index: i32, value: f32) {
    // SAFETY: the entry's object remains live until effClose; this observable
    // alternate-only count keeps its behavior distinct under code folding.
    unsafe {
        (*((*effect).object as *mut Fixture)).authored_alternate_calls += 1;
    }
    // SAFETY: forwards the unchanged valid fixture callback arguments.
    unsafe { set_parameter(effect, index, value) };
}

unsafe extern "C" fn get_parameter(effect: *mut AEffect, index: i32) -> f32 {
    let fixture = unsafe { &*((*effect).object as *mut Fixture) };
    fixture.params.get(index as usize).copied().unwrap_or(0.0)
}

unsafe extern "C" fn process_accumulating(
    effect: *mut AEffect,
    inputs: *const *const f32,
    outputs: *mut *mut f32,
    frames: c_int,
) {
    let fixture = unsafe { &mut *((*effect).object as *mut Fixture) };
    unsafe { (fixture.host)(effect, 0x4f43_0003, 0, 0, std::ptr::null_mut(), 0.0) };
    fixture.process(inputs, outputs, frames);
}

#[no_mangle]
pub unsafe extern "C" fn VSTPluginMain(host: HostCallback) -> *mut AEffect {
    #[cfg(broken_mode)]
    {
        let _ = host;
        return std::ptr::null_mut();
    }
    let mut effect = unsafe { Box::<AEffect>::new(std::mem::zeroed()) };
    effect.magic = EFFECT_MAGIC;
    effect.dispatcher = Some(dispatcher);
    effect.process = Some(process_accumulating);
    effect.set_parameter = Some(set_parameter);
    effect.get_parameter = Some(get_parameter);
    effect.num_programs = 1;
    effect.num_params = 2;
    effect.num_inputs = 2;
    effect.num_outputs = 2;
    #[cfg(sidechain_mode)]
    {
        effect.num_inputs = 4;
    }
    effect.flags = FLAG_PROGRAM_CHUNKS;
    #[cfg(program_bank_mode)]
    {
        effect.flags &= !FLAG_PROGRAM_CHUNKS;
        effect.num_programs = 3;
    }
    #[cfg(not(instrument_mode))]
    {
        effect.flags |= FLAG_CAN_REPLACING;
        effect.process_replacing = Some(process_accumulating);
    }
    #[cfg(instrument_mode)]
    {
        effect.flags |= FLAG_IS_SYNTH;
        effect.num_inputs = 0;
    }
    #[cfg(no_output_mode)]
    {
        effect.num_outputs = 0;
    }
    #[cfg(multi_out_mode)]
    {
        effect.flags |= FLAG_IS_SYNTH;
        effect.num_inputs = 0;
        effect.num_outputs = 8;
    }
    effect.unique_id = 0x4f43_4632;
    effect.version = 100;
    let effect_pointer = &mut *effect as *mut AEffect;
    let fixture = Box::new(Fixture {
        host,
        effect: effect_pointer,
        params: [0.5, 0.0],
        #[cfg(program_bank_mode)]
        program_index: 0,
        #[cfg(program_bank_mode)]
        programs: [([0.5, 0.0], [0; 25]); 3],
        authored_name_mode: 0,
        authored_setter_action: 0,
        authored_setter_calls: 0,
        authored_alternate_calls: 0,
        sample_rate: 44_100.0,
        block_size: 512,
        active: false,
        history_enabled: false,
        history: [0.0; 8],
        note: None,
        voice_amplitude: 0.0,
        #[cfg(multi_out_mode)]
        processed_blocks: 0,
        #[cfg(instrument_mode)]
        has_processed: false,
        last_tempo: 0.0,
        last_ppq: 0.0,
        last_sample_pos: 0.0,
        host_version: 0,
        chunk: Vec::new(),
    });
    effect.object = Box::into_raw(fixture) as *mut c_void;
    // Ask the host for its version while the host callback is reachable.
    unsafe { host(effect_pointer, 0x4f43_0001, 0, 0, std::ptr::null_mut(), 0.0) };
    let version = unsafe { host(effect_pointer, 1, 0, 0, std::ptr::null_mut(), 0.0) };
    let fixture_pointer = unsafe { (*effect_pointer).object as *mut Fixture };
    unsafe { (*fixture_pointer).host_version = version };
    Box::into_raw(effect)
}
