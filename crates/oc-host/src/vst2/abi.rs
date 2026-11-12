//! Raw VST2 interface declarations.
//!
//! Only the members and opcodes this host actually calls are declared. The
//! numeric ABI values are fixed by the plugin interface; the tables below are
//! the reference for them.

#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

/// `'VstP'`, the value a plugin stores in [`AEffect::magic`].
pub const EFFECT_MAGIC: i32 = 0x5673_7450;
/// `'NvEf'`, the challenge value for [`plugin_op::IDENTIFY`].
pub const IDENTIFY_CHALLENGE: isize = 0x4E76_4566;
/// Version the host reports for itself (2.4).
pub const HOST_VST_VERSION: i32 = 2400;

// AEffect::flags
/// AEffect flag: the plugin has an editor.
pub const FLAG_HAS_EDITOR: i32 = 1;
/// AEffect flag: the plugin supports processReplacing.
pub const FLAG_CAN_REPLACING: i32 = 1 << 4;
/// AEffect flag: programs use opaque chunks.
pub const FLAG_PROGRAM_CHUNKS: i32 = 1 << 5;
/// AEffect flag: the plugin is a synth.
pub const FLAG_IS_SYNTH: i32 = 1 << 8;
/// AEffect flag: the plugin produces no sound while stopped.
pub const FLAG_NO_SOUND_IN_STOP: i32 = 1 << 9;
/// AEffect flag: the plugin supports double-precision processing.
pub const FLAG_CAN_DOUBLE_REPLACING: i32 = 1 << 12;

// Buffer sizes the plugin may write into a host-provided char buffer.
/// Maximum parameter display string length.
pub const MAX_PARAM_STR: usize = 256;
/// Maximum name string length.
pub const MAX_NAME_STR: usize = 256;

/// Optional 2.4 parameter descriptor, read only to learn a control's step count.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VstParameterProperties {
    pub step_float: f32,
    pub small_step_float: f32,
    pub large_step_float: f32,
    pub label: [u8; 64],
    pub flags: i32,
    pub min_integer: i32,
    pub max_integer: i32,
    pub step_integer: i32,
    pub large_step_integer: i32,
    pub short_label: [u8; 8],
    pub display_index: i16,
    pub category: i16,
    pub num_parameters_in_category: i16,
    pub reserved: i16,
    pub category_label: [u8; 24],
    pub future: [u8; 16],
}

/// `VstParameterProperties::flags` bits this host reads.
pub mod parameter_flags {
    /// Parameter flag: the control is a switch.
    pub const IS_SWITCH: i32 = 1 << 0;
    /// Parameter flag: the integer range is meaningful.
    pub const USES_INTEGER_MIN_MAX: i32 = 1 << 4;
}

/// The host callback a plugin keeps for its lifetime.
pub type AudioMasterCallback =
    unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
/// Plugin dispatcher entry point.
pub type DispatcherProc =
    unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
/// Accumulating 32-bit process entry point.
pub type ProcessProc = unsafe extern "C" fn(*mut AEffect, *const *const f32, *mut *mut f32, i32);
/// Double-precision process entry point.
pub type ProcessDoubleProc =
    unsafe extern "C" fn(*mut AEffect, *const *const f64, *mut *mut f64, i32);
/// Parameter setter.
pub type SetParameterProc = unsafe extern "C" fn(*mut AEffect, i32, f32);
/// Parameter getter.
pub type GetParameterProc = unsafe extern "C" fn(*mut AEffect, i32) -> f32;
/// Plugin entry point.
pub type PluginMain = unsafe extern "C" fn(AudioMasterCallback) -> *mut AEffect;

/// The plugin descriptor.
///
/// Field order and size are part of the ABI. On a 64-bit target the offsets
/// are: magic 0, dispatcher 8, process 16, setParameter 24, getParameter 32,
/// numPrograms 40, numParams 44, numInputs 48, numOutputs 52, flags 56,
/// reserved1 64, reserved2 72, initialDelay 80, realQualities 84,
/// offQualities 88, ioRatio 92, object 96, user 104, uniqueID 112, version 116,
/// processReplacing 120, processDoubleReplacing 128, future 136 (size 192).
///
/// `reserved1` is host-reserved and carries compatibility callback backing.
/// Owned callback routing instead uses an independent address registry and
/// never dereferences this field to recover host backing.
#[repr(C)]
pub struct AEffect {
    pub magic: i32,
    pub dispatcher: Option<DispatcherProc>,
    /// Deprecated accumulating process entry point.
    pub process: Option<ProcessProc>,
    pub set_parameter: Option<SetParameterProc>,
    pub get_parameter: Option<GetParameterProc>,
    pub num_programs: i32,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub flags: i32,
    pub reserved1: isize,
    pub reserved2: isize,
    pub initial_delay: i32,
    /// Deprecated, always zero.
    pub real_qualities: i32,
    /// Deprecated, always zero.
    pub off_qualities: i32,
    /// Deprecated, always zero.
    pub io_ratio: f32,
    /// Plugin-owned state pointer.
    pub object: *mut c_void,
    /// Plugin-owned pointer.
    pub user: *mut c_void,
    pub unique_id: i32,
    pub version: i32,
    pub process_replacing: Option<ProcessProc>,
    pub process_double_replacing: Option<ProcessDoubleProc>,
    pub future: [u8; 56],
}

/// Plugin-side opcodes used by this host.
///
/// | value | name | use |
/// |---|---|---|
/// | 0 | open | first call after the entry point |
/// | 1 | close | release the plugin |
/// | 7 | get parameter display | UI string for an index |
/// | 8 | get parameter name | UI string for an index |
/// | 10 | set sample rate | before activation |
/// | 11 | set block size | before activation |
/// | 12 | mains changed | activate/deactivate processing |
/// | 13 | editor rect | report editor size (no window is opened) |
/// | 19 | editor idle | editor repaint tick |
/// | 22 | identify | host identity handshake |
/// | 23 | get chunk | bank state blob |
/// | 24 | set chunk | restore bank state blob |
/// | 25 | process events | MIDI and parameter events |
/// | 35 | get plug category | synth vs effect |
/// | 45 | get effect name | display name |
/// | 47 | get vendor string | vendor |
/// | 48 | get product string | product |
/// | 49 | get vendor version | integer version |
/// | 51 | can do | feature query |
/// | 52 | get tail samples | release tail |
/// | 56 | get parameter properties | 2.4 step count and switch flag |
/// | 58 | get VST version | 2400 for 2.4 |
pub mod plugin_op {
    /// first call after the entry point
    pub const OPEN: i32 = 0;
    /// release the plugin
    pub const CLOSE: i32 = 1;
    /// short parameter label
    pub const GET_PARAM_LABEL: i32 = 6;
    /// parameter display string
    pub const GET_PARAM_DISPLAY: i32 = 7;
    /// parameter name
    pub const GET_PARAM_NAME: i32 = 8;
    /// set the sample rate before activation
    pub const SET_SAMPLE_RATE: i32 = 10;
    /// set the block size before activation
    pub const SET_BLOCK_SIZE: i32 = 11;
    /// activate or deactivate processing
    pub const MAINS_CHANGED: i32 = 12;
    /// report the editor rectangle
    pub const EDIT_GET_RECT: i32 = 13;
    /// open the editor
    pub const EDIT_OPEN: i32 = 14;
    /// close the editor
    pub const EDIT_CLOSE: i32 = 15;
    /// editor repaint tick
    pub const EDIT_IDLE: i32 = 19;
    /// host identity handshake
    pub const IDENTIFY: i32 = 22;
    /// read the bank state blob
    pub const GET_CHUNK: i32 = 23;
    /// restore the bank state blob
    pub const SET_CHUNK: i32 = 24;
    /// deliver MIDI and parameter events
    pub const PROCESS_EVENTS: i32 = 25;
    /// ask whether the plugin can be automated
    pub const CAN_BE_AUTOMATED: i32 = 26;
    /// report the plugin category
    pub const GET_PLUG_CATEGORY: i32 = 35;
    /// report the display name
    pub const GET_EFFECT_NAME: i32 = 45;
    /// report the vendor string
    pub const GET_VENDOR_STRING: i32 = 47;
    /// report the product string
    pub const GET_PRODUCT_STRING: i32 = 48;
    /// report the integer version
    pub const GET_VENDOR_VERSION: i32 = 49;
    /// feature query
    pub const CAN_DO: i32 = 51;
    /// report the release tail
    pub const GET_TAIL_SAMPLES: i32 = 52;
    /// read the 2.4 parameter descriptor
    pub const GET_PARAMETER_PROPERTIES: i32 = 56;
    /// report the VST version
    pub const GET_VST_VERSION: i32 = 58;
}

/// Host-side opcodes (the `audioMaster` callback) used by this host.
///
/// | value | name | reply |
/// |---|---|---|
/// | 0 | automate | parameter changed by the plugin |
/// | 1 | version | 2400 |
/// | 2 | current id | 0 (shell plugins unsupported) |
/// | 3 | idle | 0 |
/// | 6 | want midi | 1 |
/// | 7 | get time | [`VstTimeInfo`] pointer |
/// | 8 | process events | 0 (plugin-to-host events dropped) |
/// | 10 | tempo at | tempo * 10000 |
/// | 13 | IO changed | 1, re-reads channel counts |
/// | 14 | need idle | 1 |
/// | 15 | size window | 0 (no editor window) |
/// | 16 | get sample rate | current rate |
/// | 17 | get block size | current block size |
/// | 18/19 | input/output latency | reported latency |
/// | 22 | replace or accumulate | 1 (replace) |
/// | 23 | current process level | 1 user, 2 realtime, 4 offline |
/// | 24 | automation state | 1 (reading) |
/// | 32/33/34 | host vendor/product/version | host identity |
/// | 37 | can do | 1/0/-1 |
/// | 38 | language | 1 (English) |
/// | 42 | update display | 0 |
/// | 43/44 | begin/end edit | 0 |
pub mod host_op {
    /// parameter changed by the plugin
    pub const AUTOMATE: i32 = 0;
    /// host version
    pub const VERSION: i32 = 1;
    /// current shell plugin id
    pub const CURRENT_ID: i32 = 2;
    /// idle callback
    pub const IDLE: i32 = 3;
    /// ask whether the plugin wants MIDI
    pub const WANT_MIDI: i32 = 6;
    /// request the VstTimeInfo snapshot
    pub const GET_TIME: i32 = 7;
    /// receive plugin-to-host events
    pub const PROCESS_EVENTS: i32 = 8;
    /// request the tempo at a position
    pub const TEMPO_AT: i32 = 10;
    /// request the automatable parameter count
    pub const GET_NUM_AUTOMATABLE_PARAMETERS: i32 = 11;
    /// request parameter quantization
    pub const GET_PARAMETER_QUANTIZATION: i32 = 12;
    /// report changed channel counts
    pub const IO_CHANGED: i32 = 13;
    /// ask whether the plugin needs idle calls
    pub const NEED_IDLE: i32 = 14;
    /// request an editor resize
    pub const SIZE_WINDOW: i32 = 15;
    /// request the sample rate
    pub const GET_SAMPLE_RATE: i32 = 16;
    /// request the block size
    pub const GET_BLOCK_SIZE: i32 = 17;
    /// request input latency
    pub const GET_INPUT_LATENCY: i32 = 18;
    /// request output latency
    pub const GET_OUTPUT_LATENCY: i32 = 19;
    /// ask whether processing replaces or accumulates
    pub const WILL_REPLACE_OR_ACCUMULATE: i32 = 22;
    /// request the current process level
    pub const GET_CURRENT_PROCESS_LEVEL: i32 = 23;
    /// request the automation state
    pub const GET_AUTOMATION_STATE: i32 = 24;
    /// request the host vendor string
    pub const GET_VENDOR_STRING: i32 = 32;
    /// request the host product string
    pub const GET_PRODUCT_STRING: i32 = 33;
    /// request the host version
    pub const GET_VENDOR_VERSION: i32 = 34;
    /// host feature query
    pub const CAN_DO: i32 = 37;
    /// request the host language
    pub const GET_LANGUAGE: i32 = 38;
    /// request the plugin directory
    pub const GET_DIRECTORY: i32 = 41;
    /// request an editor repaint
    pub const UPDATE_DISPLAY: i32 = 42;
    /// begin a parameter edit
    pub const BEGIN_EDIT: i32 = 43;
    /// end a parameter edit
    pub const END_EDIT: i32 = 44;
}

/// `effGetPlugCategory` replies.
pub mod plug_category {
    /// Plugin category: unknown.
    pub const UNKNOWN: i32 = 0;
    /// Plugin category: effect.
    pub const EFFECT: i32 = 1;
    /// Plugin category: synth.
    pub const SYNTH: i32 = 2;
    /// Plugin category: analysis.
    pub const ANALYSIS: i32 = 3;
    /// Plugin category: mastering.
    pub const MASTERING: i32 = 4;
    /// Plugin category: spatializer.
    pub const SPATIALIZER: i32 = 5;
    /// Plugin category: room effect.
    pub const ROOM_FX: i32 = 6;
    /// Plugin category: surround effect.
    pub const SURROUND_FX: i32 = 7;
    /// Plugin category: restoration.
    pub const RESTORATION: i32 = 8;
    /// Plugin category: offline process.
    pub const OFFLINE_PROCESS: i32 = 9;
    /// Plugin category: shell.
    pub const SHELL: i32 = 10;
    /// Plugin category: generator.
    pub const GENERATOR: i32 = 11;
}

/// `audioMasterGetCurrentProcessLevel` replies.
pub mod process_level {
    /// Process level: unknown.
    pub const UNKNOWN: i32 = 0;
    /// Process level: user interface.
    pub const USER: i32 = 1;
    /// Process level: real-time.
    pub const REALTIME: i32 = 2;
    /// Process level: prefetch.
    pub const PREFETCH: i32 = 3;
    /// Process level: offline.
    pub const OFFLINE: i32 = 4;
}

/// Feature strings accepted by `effCanDo` / `audioMasterCanDo`.
pub mod can_do {
    /// Feature string: sendVstEvents.
    pub const SEND_VST_EVENTS: &str = "sendVstEvents";
    /// Feature string: sendVstMidiEvent.
    pub const SEND_VST_MIDI_EVENT: &str = "sendVstMidiEvent";
    /// Feature string: sendVstTimeInfo.
    pub const SEND_VST_TIME_INFO: &str = "sendVstTimeInfo";
    /// Feature string: receiveVstEvents.
    pub const RECEIVE_VST_EVENTS: &str = "receiveVstEvents";
    /// Feature string: receiveVstMidiEvent.
    pub const RECEIVE_VST_MIDI_EVENT: &str = "receiveVstMidiEvent";
    /// Feature string: receiveVstTimeInfo.
    pub const RECEIVE_VST_TIME_INFO: &str = "receiveVstTimeInfo";
    /// Feature string: reportConnectionChanges.
    pub const REPORT_CONNECTION_CHANGES: &str = "reportConnectionChanges";
    /// Feature string: acceptIOChanges.
    pub const ACCEPT_IO_CHANGES: &str = "acceptIOChanges";
    /// Feature string: sizeWindow.
    pub const SIZE_WINDOW: &str = "sizeWindow";
    /// Feature string: offline.
    pub const OFFLINE: &str = "offline";
    /// Feature string: startStopProcess.
    pub const START_STOP_PROCESS: &str = "startStopProcess";
    /// Feature string: supportsDoublePrecision.
    pub const SUPPORTS_DOUBLE_PRECISION: &str = "supportsDoublePrecision";
    /// Feature string: supports32bitFloat.
    pub const SUPPORTS_32BIT_FLOAT: &str = "supports32bitFloat";
}

// VstTimeInfo::flags
/// Time-info flag: transport changed.
pub const TIME_TRANSPORT_CHANGED: i32 = 1;
/// Time-info flag: transport is playing.
pub const TIME_TRANSPORT_PLAYING: i32 = 1 << 1;
/// Time-info flag: PPQ position is valid.
pub const TIME_PPQ_POS_VALID: i32 = 1 << 9;
/// Time-info flag: tempo is valid.
pub const TIME_TEMPO_VALID: i32 = 1 << 10;
/// Time-info flag: bar position is valid.
pub const TIME_BARS_VALID: i32 = 1 << 11;
/// Time-info flag: time signature is valid.
pub const TIME_TIME_SIG_VALID: i32 = 1 << 13;

/// Timeline snapshot returned by `audioMasterGetTime`.
///
/// The host returns a read-only callback-thread record. Its storage remains
/// stable until that thread's next GET_TIME request or instance retirement;
/// another thread must request its own record.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VstTimeInfo {
    pub sample_pos: f64,
    pub sample_rate: f64,
    pub nano_seconds: f64,
    pub ppq_pos: f64,
    pub tempo: f64,
    pub bar_start_pos: f64,
    pub cycle_start_pos: f64,
    pub cycle_end_pos: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub smpte_offset: i32,
    pub smpte_frame_rate: i32,
    pub samples_to_next_clock: i32,
    pub flags: i32,
}

/// Editor rectangle used by `effEditGetRect`.
#[repr(C)]
pub struct ERect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

/// Common event header. MIDI events embed it as their first members.
pub const MIDI_EVENT_TYPE: i32 = 1;

#[repr(C)]
/// Common event header.
pub struct VstEvent {
    pub event_type: i32,
    pub byte_size: i32,
    pub delta_frames: i32,
    pub flags: i32,
    pub reserved: [u8; 16],
}

/// `kVstMidiType` event; 32 bytes on every target.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VstMidiEvent {
    pub event_type: i32,
    pub byte_size: i32,
    pub delta_frames: i32,
    pub flags: i32,
    pub note_length: i32,
    pub note_offset: i32,
    pub midi_data: [u8; 4],
    pub detune: i8,
    pub note_off_velocity: u8,
    pub reserved1: u8,
    pub reserved2: u8,
}

impl VstMidiEvent {
    /// Create a MIDI event with its data and delta frames.
    pub fn new(data: [u8; 4], delta_frames: i32) -> Self {
        Self {
            event_type: MIDI_EVENT_TYPE,
            byte_size: std::mem::size_of::<Self>() as i32,
            delta_frames,
            midi_data: data,
            ..Self::default()
        }
    }
}

/// Header of the variable-length event list; pointers follow at offset 16.
///
/// The host builds the whole list in one byte buffer: this header, then
/// `num_events` event pointers. [`VST_EVENTS_POINTER_OFFSET`] is that offset.
#[repr(C)]
pub struct VstEvents {
    pub num_events: i32,
    pub reserved: isize,
    pub events: [*mut VstEvent; 1],
}

/// Offset of the event pointer array in VstEvents.
pub const VST_EVENTS_POINTER_OFFSET: usize = 16;

/// MIDI status helpers used when building events.
pub mod midi_status {
    /// MIDI status: note off.
    pub const NOTE_OFF: u8 = 0x80;
    /// MIDI status: note on.
    pub const NOTE_ON: u8 = 0x90;
    /// MIDI status: polyphonic pressure.
    pub const POLY_PRESSURE: u8 = 0xA0;
    /// MIDI status: control change.
    pub const CONTROL_CHANGE: u8 = 0xB0;
    /// MIDI status: program change.
    pub const PROGRAM_CHANGE: u8 = 0xC0;
    /// MIDI status: channel pressure.
    pub const CHANNEL_PRESSURE: u8 = 0xD0;
    /// MIDI status: pitch bend.
    pub const PITCH_BEND: u8 = 0xE0;
}

/// Write a NUL-terminated string into a plugin-provided buffer.
///
/// # Safety
/// `ptr` must point to at least `capacity` writable bytes disjoint from `text`.
pub unsafe fn write_c_string(ptr: *mut c_void, text: &str, capacity: usize) {
    if ptr.is_null() || capacity == 0 {
        return;
    }
    let bytes = text.as_bytes();
    let length = bytes.len().min(capacity - 1);
    // SAFETY: the caller provides capacity writable bytes disjoint from text;
    // length is at most capacity-1, so the copy and trailing NUL both fit.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, ptr as *mut c_char, length);
        *(ptr as *mut c_char).add(length) = 0;
    }
}
