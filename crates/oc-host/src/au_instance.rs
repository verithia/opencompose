//! AudioUnit (AUv2) hosting: in-process offline render for instruments and effects.
//!
//! Every unit runs at the project sample rate on non-interleaved f32 stereo
//! buses. Instruments receive `MusicDeviceMIDIEvent` at block-relative offsets
//! and are pulled with `AudioUnitRender`; effects are pulled the same way and
//! get their input through a render callback. Preset state uses
//! `kAudioUnitProperty_ClassInfo`, the representation a `.aupreset` file holds.
//!
//! Prepared processing borrows its transport for the native call; the caller
//! retains any project clock, and the unit stores only scalar reset context.
//! Processing, parameter values and reset use exclusive instance ownership
//! without the registry/control/lifecycle mutex. By-value compatibility adapters
//! retain their ownership semantics and may drop an incoming last clock owner.

use std::ffi::{c_char, c_void};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::ports::AudioPort;
use oc_audio::PluginTransport;

#[path = "au_guard.rs"]
mod au_guard;
pub use au_guard::{AuForeignError, AuForeignFault, AuForeignFaultKind};

mod authored_parameters;
pub use authored_parameters::{AuAuthoredParameterKey, AuAuthoredParameterOwner};

// Scopes and the one stream format this host negotiates.
const SCOPE_GLOBAL: u32 = 0;
const SCOPE_INPUT: u32 = 1;
const SCOPE_OUTPUT: u32 = 2;

const PROP_CLASS_INFO: u32 = 0;
const PROP_PARAMETER_LIST: u32 = 3;
const PROP_PARAMETER_INFO: u32 = 4;
const PROP_STREAM_FORMAT: u32 = 8;
const PROP_LATENCY: u32 = 12;
const PROP_MAXIMUM_FRAMES_PER_SLICE: u32 = 14;
const PROP_TAIL_TIME: u32 = 20;
const PROP_SET_RENDER_CALLBACK: u32 = 23;
const PROP_HOST_CALLBACKS: u32 = 27;
const PROP_OFFLINE_RENDER: u32 = 37;
const CANNOT_DO_IN_CURRENT_CONTEXT: i32 = -10863;
const PROP_AUDIO_UNIT_MIDI_PROTOCOL: u32 = 64;
const PROP_HOST_MIDI_PROTOCOL: u32 = 65;

const FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
const FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;
const FORMAT_FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

const TYPE_MUSIC_DEVICE: u32 = u32::from_be_bytes(*b"aumu");
const TYPE_MUSIC_EFFECT: u32 = u32::from_be_bytes(*b"aumf");
const TYPE_EFFECT: u32 = u32::from_be_bytes(*b"aufx");

const MIDI_PROTOCOL_1_0: i32 = 1;
const MIDI_PROTOCOL_2_0: i32 = 2;

const PARAM_FLAG_CF_NAME_RELEASE: u32 = 1 << 4;
const PARAM_FLAG_HAS_CF_NAME_STRING: u32 = 1 << 27;
const PARAM_FLAG_IS_WRITABLE: u32 = 1 << 31;

const PLIST_BINARY_FORMAT: isize = 200;
const STRING_ENCODING_UTF8: u32 = 0x0800_0100;

const TIME_STAMP_SAMPLE_TIME_VALID: u32 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
/// Core Audio component description used to find and identify an AU.
pub struct AudioComponentDescription {
    pub component_type: u32,
    pub component_sub_type: u32,
    pub component_manufacturer: u32,
    pub component_flags: u32,
    pub component_flags_mask: u32,
}

impl PartialEq for AudioComponentDescription {
    fn eq(&self, other: &Self) -> bool {
        self.component_type == other.component_type
            && self.component_sub_type == other.component_sub_type
            && self.component_manufacturer == other.component_manufacturer
    }
}

impl Eq for AudioComponentDescription {}

impl AudioComponentDescription {
    /// Four-character codes as written in an `.ocp` preset, e.g. `aumu:dls:appl`.
    pub fn from_codes(component_type: &str, sub_type: &str, manufacturer: &str) -> Option<Self> {
        let code = |text: &str| -> Option<u32> {
            let bytes: [u8; 4] = text.as_bytes().try_into().ok()?;
            Some(u32::from_be_bytes(bytes))
        };
        Some(Self {
            component_type: code(component_type)?,
            component_sub_type: code(sub_type)?,
            component_manufacturer: code(manufacturer)?,
            ..Self::default()
        })
    }

    /// Four-character type code.
    pub fn type_code(&self) -> String {
        code_text(self.component_type)
    }

    /// Four-character subtype code.
    pub fn sub_type_code(&self) -> String {
        code_text(self.component_sub_type)
    }

    /// Four-character manufacturer code.
    pub fn manufacturer_code(&self) -> String {
        code_text(self.component_manufacturer)
    }

    /// `type:subtype:manufacturer` identifier.
    pub fn codes(&self) -> String {
        format!(
            "{}:{}:{}",
            self.type_code(),
            self.sub_type_code(),
            self.manufacturer_code()
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte_subframes: i16,
    smpte_subframe_divisor: i16,
    smpte_counter: u32,
    smpte_type: u32,
    smpte_flags: u32,
    smpte_hours: i16,
    smpte_minutes: i16,
    smpte_seconds: i16,
    smpte_frames: i16,
    flags: u32,
    reserved: u32,
}

impl AudioTimeStamp {
    fn at_sample(sample_time: f64) -> Self {
        Self {
            sample_time,
            host_time: 0,
            rate_scalar: 0.0,
            word_clock_time: 0,
            smpte_subframes: 0,
            smpte_subframe_divisor: 0,
            smpte_counter: 0,
            smpte_type: 0,
            smpte_flags: 0,
            smpte_hours: 0,
            smpte_minutes: 0,
            smpte_seconds: 0,
            smpte_frames: 0,
            flags: TIME_STAMP_SAMPLE_TIME_VALID,
            reserved: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

/// Room for the stereo bus only; the framework reads `number_buffers` entries.
#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

impl AudioStreamBasicDescription {
    fn non_interleaved_stereo(sample_rate: u32) -> Self {
        Self {
            sample_rate: f64::from(sample_rate),
            format_id: FORMAT_LINEAR_PCM,
            format_flags: FORMAT_FLAG_IS_FLOAT
                | FORMAT_FLAG_IS_PACKED
                | FORMAT_FLAG_IS_NON_INTERLEAVED,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: 2,
            bits_per_channel: 32,
            reserved: 0,
        }
    }
}

#[repr(C)]
struct AURenderCallbackStruct {
    input_proc: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut u32,
            *const AudioTimeStamp,
            u32,
            u32,
            *mut AudioBufferList,
        ) -> i32,
    >,
    input_proc_ref_con: *mut c_void,
}

#[repr(C)]
struct AudioUnitParameterInfo {
    name: [c_char; 52],
    unit_name: *mut c_void,
    clump_id: u32,
    cf_name_string: *mut c_void,
    unit: u32,
    min_value: f32,
    max_value: f32,
    default_value: f32,
    flags: u32,
}

/// Input planes and block origin shared with the effect's render callback.
struct InputState {
    left: AtomicPtr<f32>,
    right: AtomicPtr<f32>,
    block_start: AtomicI64,
    capacity: AtomicUsize,
    /// Samples admitted for the currently executing native render only.
    valid_frames: AtomicUsize,
}

/// Timeline snapshot an AudioUnit reads through `kAudioUnitProperty_HostCallbacks`.
///
/// The unit may call the host from its render function, so every field is
/// published atomically before the render and read without locking.
struct HostClock {
    beat: AtomicU64,
    tempo: AtomicU64,
    sample_pos: AtomicU64,
    measure_down_beat: AtomicU64,
    delta_to_next_beat: AtomicU32,
    numerator: AtomicU32,
    denominator: AtomicU32,
    playing: AtomicBool,
    recording: AtomicBool,
}

impl HostClock {
    fn new() -> Self {
        Self {
            beat: AtomicU64::new(0.0_f64.to_bits()),
            tempo: AtomicU64::new(120.0_f64.to_bits()),
            sample_pos: AtomicU64::new(0),
            measure_down_beat: AtomicU64::new(0.0_f64.to_bits()),
            delta_to_next_beat: AtomicU32::new(0),
            numerator: AtomicU32::new(4),
            denominator: AtomicU32::new(4),
            playing: AtomicBool::new(true),
            recording: AtomicBool::new(false),
        }
    }

    fn publish(&self, transport: Option<&PluginTransport>, sample_pos: u64) {
        let Some(transport) = transport else {
            self.sample_pos.store(sample_pos, Ordering::Relaxed);
            self.playing.store(false, Ordering::Relaxed);
            return;
        };
        let (numerator, denominator, bar_start, _) = transport.musical_position();
        self.beat
            .store(transport.quarter_notes().to_bits(), Ordering::Relaxed);
        self.tempo
            .store(transport.current_tempo().to_bits(), Ordering::Relaxed);
        self.sample_pos.store(sample_pos, Ordering::Relaxed);
        self.measure_down_beat
            .store(bar_start.to_bits(), Ordering::Relaxed);
        self.delta_to_next_beat
            .store(transport.frames_to_next_beat(), Ordering::Relaxed);
        self.numerator
            .store(u32::from(numerator), Ordering::Relaxed);
        self.denominator
            .store(u32::from(denominator), Ordering::Relaxed);
        self.playing.store(transport.playing, Ordering::Relaxed);
    }
}

/// Scalar reset context; retaining a render clock here would let its last Arc
/// be destroyed when a different clock arrives on the processing thread.
#[derive(Clone, Copy)]
struct HostClockSnapshot {
    beat: u64,
    tempo: u64,
    sample_pos: u64,
    measure_down_beat: u64,
    delta_to_next_beat: u32,
    numerator: u32,
    denominator: u32,
    playing: bool,
    recording: bool,
}
impl HostClock {
    fn snapshot(&self) -> HostClockSnapshot {
        HostClockSnapshot {
            beat: self.beat.load(Ordering::Relaxed),
            tempo: self.tempo.load(Ordering::Relaxed),
            sample_pos: self.sample_pos.load(Ordering::Relaxed),
            measure_down_beat: self.measure_down_beat.load(Ordering::Relaxed),
            delta_to_next_beat: self.delta_to_next_beat.load(Ordering::Relaxed),
            numerator: self.numerator.load(Ordering::Relaxed),
            denominator: self.denominator.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            recording: self.recording.load(Ordering::Relaxed),
        }
    }
    fn restore(&self, snapshot: HostClockSnapshot) {
        self.beat.store(snapshot.beat, Ordering::Relaxed);
        self.tempo.store(snapshot.tempo, Ordering::Relaxed);
        self.sample_pos
            .store(snapshot.sample_pos, Ordering::Relaxed);
        self.measure_down_beat
            .store(snapshot.measure_down_beat, Ordering::Relaxed);
        self.delta_to_next_beat
            .store(snapshot.delta_to_next_beat, Ordering::Relaxed);
        self.numerator.store(snapshot.numerator, Ordering::Relaxed);
        self.denominator
            .store(snapshot.denominator, Ordering::Relaxed);
        self.playing.store(snapshot.playing, Ordering::Relaxed);
        self.recording.store(snapshot.recording, Ordering::Relaxed);
    }
    fn reset_context(&self, transport: Option<&PluginTransport>) -> HostClockSnapshot {
        let mut snapshot = if let Some(transport) = transport {
            // The borrowed caller transport keeps the clock alive throughout
            // this temporary clone. Its Drop cannot release the last reference.
            let origin = transport.at_sample(0);
            let clock = HostClock::new();
            clock.publish(Some(&origin), 0);
            clock.snapshot()
        } else {
            self.snapshot()
        };
        snapshot.sample_pos = 0;
        snapshot.playing = false;
        snapshot
    }
}

#[repr(C)]
struct HostCallbackInfo {
    host_user_data: *mut c_void,
    beat_and_tempo_proc: Option<unsafe extern "C" fn(*mut c_void, *mut f64, *mut f64) -> i32>,
    musical_time_location_proc:
        Option<unsafe extern "C" fn(*mut c_void, *mut u32, *mut f32, *mut u32, *mut f64) -> i32>,
    transport_state_proc: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut u8,
            *mut u8,
            *mut f64,
            *mut u8,
            *mut f64,
            *mut f64,
        ) -> i32,
    >,
    transport_state_proc2: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut u8,
            *mut u8,
            *mut u8,
            *mut f64,
            *mut u8,
            *mut f64,
            *mut f64,
        ) -> i32,
    >,
}

unsafe extern "C" fn host_beat_and_tempo(
    user_data: *mut c_void,
    beat: *mut f64,
    tempo: *mut f64,
) -> i32 {
    if user_data.is_null() {
        return CANNOT_DO_IN_CURRENT_CONTEXT;
    }
    // SAFETY: registration supplies the retained boxed HostClock as user_data.
    // Its atomic-only storage survives callback detachment/disposal, or is retained on quarantine.
    let clock = unsafe { &*(user_data as *const HostClock) };
    if !beat.is_null() {
        // SAFETY: the AU callback ABI supplies writable f64 storage for each non-null beat output.
        unsafe { *beat = f64::from_bits(clock.beat.load(Ordering::Relaxed)) };
    }
    if !tempo.is_null() {
        // SAFETY: the optional non-null tempo output points to writable f64 storage for this callback.
        unsafe { *tempo = f64::from_bits(clock.tempo.load(Ordering::Relaxed)) };
    }
    0
}

unsafe extern "C" fn host_musical_time_location(
    user_data: *mut c_void,
    delta_to_next_beat: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    measure_down_beat: *mut f64,
) -> i32 {
    if user_data.is_null() {
        return CANNOT_DO_IN_CURRENT_CONTEXT;
    }
    // SAFETY: this non-null context is the retained HostClock registered with the unit;
    // its atomic fields permit callback reads without borrowing the render owner.
    let clock = unsafe { &*(user_data as *const HostClock) };
    if !delta_to_next_beat.is_null() {
        // SAFETY: the optional delta output is caller-provided writable u32 storage for this call.
        unsafe {
            *delta_to_next_beat = clock.delta_to_next_beat.load(Ordering::Relaxed);
        }
    }
    if !numerator.is_null() {
        // SAFETY: the AU ABI defines this non-null numerator output as writable f32 storage.
        unsafe { *numerator = clock.numerator.load(Ordering::Relaxed) as f32 };
    }
    if !denominator.is_null() {
        // SAFETY: the AU ABI defines this optional denominator output as writable u32 storage.
        unsafe { *denominator = clock.denominator.load(Ordering::Relaxed) };
    }
    if !measure_down_beat.is_null() {
        // SAFETY: the caller owns writable f64 downbeat storage through the synchronous callback.
        unsafe {
            *measure_down_beat = f64::from_bits(clock.measure_down_beat.load(Ordering::Relaxed));
        }
    }
    0
}

unsafe extern "C" fn host_transport_state(
    user_data: *mut c_void,
    playing: *mut u8,
    state_changed: *mut u8,
    current_sample: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> i32 {
    if user_data.is_null() {
        return CANNOT_DO_IN_CURRENT_CONTEXT;
    }
    // SAFETY: the callback context is the registered retained HostClock, whose
    // lifetime ends only after successful detach, uninitialize and disposal.
    let clock = unsafe { &*(user_data as *const HostClock) };
    if !playing.is_null() {
        // SAFETY: the optional playing output is caller-owned writable one-byte Boolean storage.
        unsafe { *playing = u8::from(clock.playing.load(Ordering::Relaxed)) };
    }
    if !state_changed.is_null() {
        // SAFETY: the optional changed output is caller-owned writable one-byte Boolean storage.
        unsafe { *state_changed = 0 };
    }
    if !current_sample.is_null() {
        // SAFETY: the optional sample-position output is writable f64 storage owned by the callback caller.
        unsafe { *current_sample = clock.sample_pos.load(Ordering::Relaxed) as f64 };
    }
    if !cycling.is_null() {
        // SAFETY: the optional cycling output is writable one-byte Boolean storage under the AU ABI.
        unsafe { *cycling = 0 };
    }
    if !cycle_start.is_null() {
        // SAFETY: the optional cycle-start output is caller-owned writable f64 storage.
        unsafe { *cycle_start = 0.0 };
    }
    if !cycle_end.is_null() {
        // SAFETY: the optional cycle-end output is caller-owned writable f64 storage.
        unsafe { *cycle_end = 0.0 };
    }
    0
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_transport_state2(
    user_data: *mut c_void,
    playing: *mut u8,
    recording: *mut u8,
    state_changed: *mut u8,
    current_sample: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> i32 {
    if user_data.is_null() {
        return CANNOT_DO_IN_CURRENT_CONTEXT;
    }
    // SAFETY: registration retains this boxed, atomic-only HostClock through all
    // callbacks; a failed retirement retains it rather than freeing the context.
    let clock = unsafe { &*(user_data as *const HostClock) };
    if !playing.is_null() {
        // SAFETY: the callback caller provides writable one-byte Boolean storage when playing is non-null.
        unsafe { *playing = u8::from(clock.playing.load(Ordering::Relaxed)) };
    }
    if !recording.is_null() {
        // SAFETY: the optional recording output is caller-provided writable one-byte Boolean storage.
        unsafe { *recording = u8::from(clock.recording.load(Ordering::Relaxed)) };
    }
    if !state_changed.is_null() {
        // SAFETY: the callback caller retains writable one-byte Boolean storage for this changed output.
        unsafe { *state_changed = 0 };
    }
    if !current_sample.is_null() {
        // SAFETY: the non-null sample-position output is writable f64 storage for this call.
        unsafe { *current_sample = clock.sample_pos.load(Ordering::Relaxed) as f64 };
    }
    if !cycling.is_null() {
        // SAFETY: the optional cycling output is caller-owned writable one-byte Boolean storage.
        unsafe { *cycling = 0 };
    }
    if !cycle_start.is_null() {
        // SAFETY: the optional cycle-start output is writable f64 storage retained through the callback.
        unsafe { *cycle_start = 0.0 };
    }
    if !cycle_end.is_null() {
        // SAFETY: the optional cycle-end output is writable f64 storage retained through the callback.
        unsafe { *cycle_end = 0.0 };
    }
    0
}

/// One MIDI or parameter event relative to the upcoming render block.
#[derive(Clone, Copy)]
enum PendingEvent {
    Midi {
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    },
    Parameter {
        id: u32,
        value: f32,
        offset: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MidiProtocol {
    Midi1,
    Midi2,
}

/// One host-visible AudioUnit parameter in the unit's own engineering units.
#[derive(Debug, Clone, PartialEq)]
pub struct AuParam {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub writable: bool,
    /// Symbol for the unit's `AudioUnitParameterUnit`; empty for a generic one.
    pub unit: String,
}

/// Symbol for an `AudioUnitParameterUnit` value, so the panel can name a value.
fn unit_symbol(unit: u32) -> String {
    let symbol = match unit {
        3 => "%",       // percent
        4 => "s",       // seconds
        5 => "frames",  // sample frames
        6 => "deg",     // phase 0..360
        7 | 8 => "Hz",  // rate, hertz
        9 | 20 => "ct", // cents, absolute cents
        10 => "st",     // relative semitones
        13 => "dB",     // decibels
        15 => "deg",    // degrees
        21 => "oct",    // octaves
        22 => "BPM",    // beats per minute
        23 => "beats",  // beats
        24 => "ms",     // milliseconds
        25 => ":1",     // ratio
        _ => "",
    };
    symbol.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Failure class for an AU operation.
pub enum AuErrorKind {
    /// No registered component matches the query.
    NotFound,
    /// More than one component matches and none of them is exact.
    Ambiguous,
    /// The unit type cannot be hosted in process (see `AuCategory`).
    UnsupportedType,
    /// The bundle names no AudioComponents entry this host can use.
    InvalidDescription,
    /// `AudioComponentInstanceNew`, `AudioUnitInitialize` or a required setup call failed.
    InitFailed,
    /// A property or parameter call failed after initialization.
    PropertyFailed,
    /// `AudioUnitRender` returned an error.
    RenderFailed,
    /// A MIDI call failed.
    MidiFailed,
    /// The preset state is not a property list or the unit rejected it.
    StateFailed,
    /// A caught C++ or Objective-C exception permanently quarantines the unit.
    ForeignException,
}

/// Maximum queued MIDI/parameter events retained by one prepared unit.
pub const MAX_PENDING_EVENTS: usize = 1024;

/// A prepared block refusal or native OSStatus, retained without formatting
/// or allocation until the render worker reports it outside DSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuProcessError {
    Foreign(AuForeignError),
    IncompleteFrame,
    CapacityExceeded,
    EventCapacityExceeded,
    EventOffsetOutOfRange,
    SampleRateChanged,
    TimelineOverflow,
    Uninitialized,
    Retired,
    Native {
        operation: &'static str,
        status: i32,
    },
}
impl std::fmt::Display for AuProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Foreign(error) => std::fmt::Display::fmt(error, formatter),
            Self::IncompleteFrame => {
                formatter.write_str("AudioUnit output must contain complete stereo frames")
            }
            Self::CapacityExceeded => {
                formatter.write_str("AudioUnit block exceeds its prepared frame capacity")
            }
            Self::EventCapacityExceeded => {
                formatter.write_str("AudioUnit event queue exceeds its prepared capacity")
            }
            Self::EventOffsetOutOfRange => {
                formatter.write_str("AudioUnit event offset is outside the upcoming block")
            }
            Self::SampleRateChanged => formatter
                .write_str("AudioUnit sample rate changed; rebuild the unit outside processing"),
            Self::TimelineOverflow => formatter.write_str("AudioUnit render position overflow"),
            Self::Retired => formatter.write_str("AudioUnit retirement is quarantined"),
            Self::Uninitialized => formatter.write_str("AudioUnit is not initialized"),
            Self::Native { operation, status } => {
                write!(formatter, "{operation} failed: OSStatus {status}")
            }
        }
    }
}
impl std::error::Error for AuProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Foreign(error) = self {
            Some(error)
        } else {
            None
        }
    }
}
impl From<AuForeignError> for AuProcessError {
    fn from(error: AuForeignError) -> Self {
        Self::Foreign(error)
    }
}

/// A typed AudioUnit failure; `status` is the OSStatus the framework returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuError {
    pub kind: AuErrorKind,
    pub status: Option<i32>,
    pub message: String,
    pub foreign: Option<Box<AuForeignFault>>,
}

impl AuError {
    fn new(kind: AuErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: message.into(),
            foreign: None,
        }
    }

    fn os(kind: AuErrorKind, action: &str, status: i32) -> Self {
        Self {
            kind,
            status: Some(status),
            message: action.to_string(),
            foreign: None,
        }
    }

    fn attach_fault(&mut self, fault: Option<AuForeignFault>) {
        if let Some(fault) = fault {
            self.foreign = Some(Box::new(fault));
        }
    }

    fn for_unit(mut self, unit: &str) -> Self {
        self.message = format!("'{unit}' {}", self.message);
        self
    }

    /// OSStatus name when known.
    pub fn status_name(&self) -> Option<&'static str> {
        self.status.and_then(status_name)
    }
}

impl std::fmt::Display for AuError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => {
                let name = self
                    .status_name()
                    .map(|name| format!("{name}, "))
                    .unwrap_or_default();
                write!(
                    formatter,
                    "{} failed: {name}OSStatus {status} ({})",
                    self.message,
                    status_code(status)
                )
            }
            None => match &self.foreign {
                Some(fault) => write!(formatter, "{}: {fault}", self.message),
                None => write!(formatter, "{}", self.message),
            },
        }
    }
}

impl std::error::Error for AuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.foreign
            .as_deref()
            .map(|fault| fault as &dyn std::error::Error)
    }
}
impl From<AuForeignError> for AuError {
    fn from(error: AuForeignError) -> Self {
        Self::new(AuErrorKind::ForeignException, error.to_string())
    }
}

fn status_code(status: i32) -> String {
    let bytes = status.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("0x{:08x}", status as u32)
    }
}

fn code_text(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        // Codes pad with trailing spaces; print the readable spelling.
        String::from_utf8_lossy(&bytes).trim_end().to_string()
    } else {
        format!("0x{code:08x}")
    }
}

fn status_name(status: i32) -> Option<&'static str> {
    Some(match status {
        -10879 => "kAudioUnitErr_InvalidProperty",
        -10878 => "kAudioUnitErr_InvalidParameter",
        -10877 => "kAudioUnitErr_InvalidElement",
        -10876 => "kAudioUnitErr_NoConnection",
        -10875 => "kAudioUnitErr_FailedInitialization",
        -10874 => "kAudioUnitErr_TooManyFramesToProcess",
        -10871 => "kAudioUnitErr_InvalidFile",
        -10870 => "kAudioUnitErr_UnknownFileType",
        -10869 => "kAudioUnitErr_FileNotSpecified",
        -10868 => "kAudioUnitErr_FormatNotSupported",
        -10867 => "kAudioUnitErr_Uninitialized",
        -10866 => "kAudioUnitErr_InvalidScope",
        -10865 => "kAudioUnitErr_PropertyNotWritable",
        -10863 => "kAudioUnitErr_CannotDoInCurrentContext",
        -10851 => "kAudioUnitErr_InvalidPropertyValue",
        -10850 => "kAudioUnitErr_PropertyNotInUse",
        -10849 => "kAudioUnitErr_Initialized",
        -10848 => "kAudioUnitErr_InvalidOfflineRender",
        -10847 => "kAudioUnitErr_Unauthorized",
        -66753 => "kAudioUnitErr_MIDIOutputBufferFull",
        -66745 => "kAudioUnitErr_RenderTimeout",
        -66744 => "kAudioUnitErr_ExtensionNotFound",
        -66743 => "kAudioUnitErr_InvalidParameterValue",
        -66742 => "kAudioUnitErr_InvalidFilePath",
        -66741 => "kAudioUnitErr_MissingKey",
        -66740 => "kAudioUnitErr_ComponentManagerNotSupported",
        -66635 => "kAudioUnitErr_MultipleVoiceProcessors",
        _ => return None,
    })
}

fn frame_count(seconds: f64, sample_rate: u32) -> u32 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * f64::from(sample_rate))
        .ceil()
        .min(f64::from(u32::MAX)) as u32
}

fn normalize_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Which share of the AU surface this host implements for a component type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuCategory {
    /// MIDI in, audio out (`aumu`).
    Instrument,
    /// Audio in/out, optionally MIDI in (`aufx`, `aumf`).
    Effect,
    /// Registered but not hostable in process: generator, mixer, output,
    /// format-converter, panner and offline-effect units need host wiring or a
    /// device graph that this renderer does not provide.
    Unsupported,
}

/// Hosting category for a component description.
pub fn category_of(description: &AudioComponentDescription) -> AuCategory {
    match description.component_type {
        TYPE_MUSIC_DEVICE => AuCategory::Instrument,
        TYPE_EFFECT | TYPE_MUSIC_EFFECT => AuCategory::Effect,
        _ => AuCategory::Unsupported,
    }
}

struct RegisteredComponent {
    description: AudioComponentDescription,
    name: String,
}

fn registered_components() -> Result<&'static [RegisteredComponent], AuError> {
    static CACHE: OnceLock<Result<Vec<RegisteredComponent>, AuError>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let _api = api_lock();
            let foreign = au_guard::Boundary::default();
            let result = (|| {
                let mut components = Vec::new();
                let wildcard = AudioComponentDescription::default();
                let mut component = ptr::null_mut();
                loop {
                    // SAFETY: registry descriptions/output remain live for this call.
                    component = unsafe {
                        foreign.find_next(au_guard::FindNext {
                            component,
                            description: &wildcard,
                        })
                    }?;
                    if component.is_null() {
                        break;
                    }
                    let mut description = AudioComponentDescription::default();
                    // SAFETY: the registry yielded this handle and the output has exact SDK layout.
                    if unsafe {
                        foreign.description(au_guard::Description {
                            component,
                            description: &mut description,
                        })
                    }? != 0
                    {
                        continue;
                    }
                    // SAFETY: description identifies a registered component retained by the framework.
                    let name =
                        unsafe { component_name(&description, &foreign) }?.unwrap_or_default();
                    components.push(RegisteredComponent { description, name });
                }
                Ok(components)
            })();
            result.map_err(|mut error: AuError| {
                error.attach_fault(foreign.fault());
                error
            })
        })
        .as_ref()
        .map(|components| components.as_slice())
        .map_err(Clone::clone)
}

/// Resolve a preset's plugin reference to a registered component.
///
/// Accepted forms: four-character codes (`aumu:dls:appl`, optionally `au:`),
/// a `.component` bundle path, or a display name as the AudioComponent registry
/// reports it (`Apple: DLSMusicDevice`, `DLSMusicDevice`).
pub fn resolve_component(query: &str) -> Result<AudioComponentDescription, AuError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AuError::new(
            AuErrorKind::NotFound,
            "empty AudioUnit reference",
        ));
    }
    let text = query.strip_prefix("au:").unwrap_or(query);
    if let Some(description) = parse_codes(text) {
        return if is_registered(&description)? {
            Ok(description)
        } else {
            Err(AuError::new(
                AuErrorKind::NotFound,
                format!("no registered AudioUnit matches {text:?}"),
            ))
        };
    }
    if text.contains('/') || text.contains('\\') {
        return component_from_bundle(Path::new(text), query);
    }
    component_by_name(query)
}

fn parse_codes(text: &str) -> Option<AudioComponentDescription> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let code = |part: &str| -> Option<u32> {
        let bytes = part.as_bytes();
        if bytes.is_empty() || bytes.len() > 4 || !bytes.is_ascii() {
            return None;
        }
        // Four-character codes pad with trailing spaces ('dls '), so accept the
        // trimmed spelling as well.
        let mut code = [b' '; 4];
        code[..bytes.len()].copy_from_slice(bytes);
        Some(u32::from_be_bytes(code))
    };
    Some(AudioComponentDescription {
        component_type: code(parts[0])?,
        component_sub_type: code(parts[1])?,
        component_manufacturer: code(parts[2])?,
        ..AudioComponentDescription::default()
    })
}

fn is_registered(description: &AudioComponentDescription) -> Result<bool, AuError> {
    let _api = api_lock();
    let foreign = au_guard::Boundary::default();
    // SAFETY: this registry query retains its description during the synchronous call.
    unsafe {
        foreign.find_next(au_guard::FindNext {
            component: ptr::null_mut(),
            description,
        })
    }
    .map(|component| !component.is_null())
    .map_err(|error| foreign.control_error(error))
}

fn component_by_name(query: &str) -> Result<AudioComponentDescription, AuError> {
    let target = normalize_key(query);
    let mut exact = Vec::new();
    let mut loose = Vec::new();
    for component in registered_components()? {
        if component.name.is_empty() {
            continue;
        }
        let (manufacturer, short_name) = split_display_name(&component.name);
        if component.name.eq_ignore_ascii_case(query)
            || short_name.eq_ignore_ascii_case(query)
            || manufacturer.eq_ignore_ascii_case(query)
        {
            exact.push(component);
        } else if normalize_key(&component.name) == target
            || normalize_key(short_name) == target
            || normalize_key(manufacturer) == target
        {
            loose.push(component);
        }
    }
    let matches = if exact.is_empty() { loose } else { exact };
    match matches.as_slice() {
        [found] => Ok(found.description),
        [] => Err(AuError::new(
            AuErrorKind::NotFound,
            format!("no registered AudioUnit matches {query:?}"),
        )),
        many => Err(AuError::new(
            AuErrorKind::Ambiguous,
            format!(
                "{query:?} matches {} AudioUnits: {}",
                many.len(),
                many.iter()
                    .map(|component| component.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn split_display_name(name: &str) -> (&str, &str) {
    match name.rsplit_once(':') {
        Some((manufacturer, short_name)) => (manufacturer.trim(), short_name.trim()),
        None => ("", name.trim()),
    }
}

fn component_from_bundle(path: &Path, query: &str) -> Result<AudioComponentDescription, AuError> {
    let info_path = if path.is_dir() {
        path.join("Contents/Info.plist")
    } else {
        path.to_path_buf()
    };
    let value = plist::Value::from_file(&info_path).map_err(|error| {
        AuError::new(
            AuErrorKind::InvalidDescription,
            format!(
                "could not read '{}' for {query:?}: {error}",
                info_path.display()
            ),
        )
    })?;
    let entries = value
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("AudioComponents"))
        .and_then(plist::Value::as_array)
        .ok_or_else(|| {
            AuError::new(
                AuErrorKind::InvalidDescription,
                format!(
                    "'{}' declares no AudioComponents entry",
                    info_path.display()
                ),
            )
        })?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let mut hostable = Vec::new();
    for entry in entries {
        let Some(entry) = entry.as_dictionary() else {
            continue;
        };
        let code = |key: &str| entry.get(key).and_then(plist::Value::as_string);
        let Some(description) = code("type")
            .zip(code("subtype"))
            .zip(code("manufacturer"))
            .and_then(|((component_type, sub_type), manufacturer)| {
                AudioComponentDescription::from_codes(component_type, sub_type, manufacturer)
            })
        else {
            continue;
        };
        if category_of(&description) == AuCategory::Unsupported {
            continue;
        }
        let name = entry.get("name").and_then(plist::Value::as_string);
        let named = name.is_some_and(|name| {
            normalize_key(name) == normalize_key(stem)
                || normalize_key(name) == normalize_key(query)
        });
        hostable.push((named, description));
    }
    if hostable.is_empty() {
        return Err(AuError::new(
            AuErrorKind::UnsupportedType,
            format!(
                "'{}' declares no instrument or effect AudioComponents entry",
                info_path.display()
            ),
        ));
    }
    let description = hostable
        .iter()
        .find(|(named, _)| *named)
        .or_else(|| hostable.first())
        .map(|(_, description)| *description)
        .ok_or_else(|| AuError::new(AuErrorKind::NotFound, "empty AudioComponents list"))?;
    if !is_registered(&description)? {
        return Err(AuError::new(
            AuErrorKind::NotFound,
            format!(
                "AudioUnit {} is not registered; rescan or reinstall the bundle",
                description.codes()
            ),
        ));
    }
    Ok(description)
}

unsafe fn component_name(
    description: &AudioComponentDescription,
    foreign: &au_guard::Boundary,
) -> Result<Option<String>, AuError> {
    let result = (|| {
        // SAFETY: the caller retains the registry description under the AU API lock.
        let component = unsafe {
            foreign.find_next(au_guard::FindNext {
                component: ptr::null_mut(),
                description,
            })
        }?;
        if component.is_null() {
            return Ok(None);
        }
        let mut name = ptr::null_mut();
        // SAFETY: the initialized output remains unpublished until the native status proves ownership.
        let status = unsafe {
            foreign.copy_name(au_guard::CopyName {
                component,
                name: &mut name,
            })
        }?;
        if status != 0 {
            // Failed output may be only partially written; it is not an owned CF reference.
            return Err(AuError::os(
                AuErrorKind::PropertyFailed,
                "AudioComponentCopyName",
                status,
            ));
        }
        if name.is_null() {
            return Ok(None);
        }
        let name = CfOwned::new(name, foreign);
        // SAFETY: the native reference guard retains the string until the bounded decode finishes.
        let text = unsafe {
            cf_string_to_string_guarded(
                name.value,
                MAX_CF_STRING_UTF16_UNITS,
                MAX_CF_STRING_UTF8_BYTES,
                foreign,
            )
        }?;
        name.close()?;
        Ok(text)
    })();
    result.map_err(|mut error: AuError| {
        error.attach_fault(foreign.fault());
        error
    })
}

/// Serializes registry/control/lifecycle transitions across units. Prepared
/// processing, parameters and reset instead use the exclusive instance borrow;
/// unrelated units must not wait for this control-plane mutex to render.
static AU_API: Mutex<()> = Mutex::new(());

fn api_lock() -> MutexGuard<'static, ()> {
    AU_API
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An acquired CF reference is finalized only while the native boundary is healthy.
struct CfOwned<'a> {
    value: *mut c_void,
    foreign: &'a au_guard::Boundary,
}
impl<'a> CfOwned<'a> {
    fn new(value: *mut c_void, foreign: &'a au_guard::Boundary) -> Self {
        Self { value, foreign }
    }
    fn close(mut self) -> Result<(), AuForeignError> {
        let value = std::mem::replace(&mut self.value, ptr::null_mut());
        if value.is_null() {
            return self.foreign.check();
        }
        // SAFETY: this owner has the sole transferred +1 release obligation.
        unsafe { self.foreign.release(au_guard::Release { value }) }
    }
}
impl Drop for CfOwned<'_> {
    fn drop(&mut self) {
        if !self.value.is_null() && !self.foreign.failed() {
            // SAFETY: this owner still holds the unreleased native +1 reference.
            let _ = unsafe {
                self.foreign
                    .release(au_guard::Release { value: self.value })
            };
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Retirement {
    Live,
    Quarantined,
    Disposed,
}

/// A live AudioUnit and the buffers an offline render needs.
///
/// Ownership: the rendering thread holds the sole mutable instance borrow;
/// control and lifecycle transitions serialize separately through `AU_API`.
/// The unit is Send but not Sync and exposes no clonable raw unit handle.
/// Host callback contexts and audio planes
/// are released only after detach, uninitialize and disposal all complete;
/// failed retirement deliberately retains them with the native unit.
pub struct AuInstance {
    foreign: Arc<au_guard::Boundary>,
    retirement: Retirement,
    unit: *mut c_void,
    description: AudioComponentDescription,
    name: String,
    sample_rate: u32,
    max_frames: usize,
    initialized: bool,
    accepts_midi: bool,
    accepts_audio_input: bool,
    midi_protocol: MidiProtocol,
    rendered_frames: u64,
    pending: Vec<PendingEvent>,
    params: Vec<AuParam>,
    param_scope: u32,
    latency: u32,
    tail: u32,
    /// The unit answered the tail-time property; `tail` is meaningful.
    tail_reported: bool,
    host_clock: ManuallyDrop<Box<HostClock>>,
    host_callbacks: bool,
    reset_clock: HostClockSnapshot,
    input: ManuallyDrop<Box<InputState>>,
    input_left: ManuallyDrop<Box<[f32]>>,
    input_right: ManuallyDrop<Box<[f32]>>,
    output_left: ManuallyDrop<Box<[f32]>>,
    output_right: ManuallyDrop<Box<[f32]>>,
    process_error: Option<AuProcessError>,
    error_reported: std::cell::Cell<bool>,
}

// SAFETY: the sole instance owner may move between workers, but this type is
// deliberately not Sync and its raw unit handle never escapes. Native calls
// retain Rust borrow exclusivity. Callbacks use separately retained atomic
// contexts and input planes prepared before render; native render completes
// before those planes change. Failed retirement retains all callback backing.
unsafe impl Send for AuInstance {}

struct InstanceRequest {
    description: AudioComponentDescription,
    name: String,
    sample_rate: u32,
    max_frames: usize,
}

impl AuInstance {
    /// Instantiate and initialize the unit named by `query`.
    ///
    /// `max_frames` is the largest block the renderer will request in one call.
    pub fn load(query: &str, sample_rate: u32, max_frames: u32) -> Result<Self, AuError> {
        let foreign = Arc::new(au_guard::Boundary::default());
        let description = resolve_component(query)?;
        let lifecycle = api_lock();
        // SAFETY: resolve_component selected this registry description and lifecycle
        // holds AU_API through name lookup; the foreign boundary owns CF output admission.
        let name = unsafe { component_name(&description, &foreign) }?
            .unwrap_or_else(|| description.codes());
        let category = category_of(&description);
        if category == AuCategory::Unsupported {
            return Err(AuError::new(
                AuErrorKind::UnsupportedType,
                format!(
                    "component type '{}' cannot be hosted in process; instruments (aumu) and \
                     effects (aufx, aumf) only",
                    description.type_code()
                ),
            )
            .for_unit(&name));
        }
        let max_frames = max_frames.max(1) as usize;
        let sample_rate = sample_rate.max(1);

        Self::instantiate(
            InstanceRequest {
                description,
                name,
                sample_rate,
                max_frames,
            },
            foreign,
            &lifecycle,
        )
    }

    fn instantiate(
        request: InstanceRequest,
        foreign: Arc<au_guard::Boundary>,
        lifecycle: &MutexGuard<'static, ()>,
    ) -> Result<Self, AuError> {
        let InstanceRequest {
            description,
            name,
            sample_rate,
            max_frames,
        } = request;
        let category = category_of(&description);
        // SAFETY: the request owns this registry description until native lookup completes.
        let component = unsafe {
            foreign.find_next(au_guard::FindNext {
                component: ptr::null_mut(),
                description: &description,
            })
        }
        .map_err(|error| foreign.control_error(error))?;
        if component.is_null() {
            return Err(AuError::new(
                AuErrorKind::NotFound,
                format!("{} is not registered", description.codes()),
            )
            .for_unit(&name));
        }
        let mut unit = ptr::null_mut();
        // SAFETY: output is initialized and unpublished until the native call completes.
        let status = unsafe {
            foreign.instance_new(au_guard::InstanceNew {
                component,
                instance: &mut unit,
            })
        }
        .map_err(|error| foreign.control_error(error))?;
        if status != 0 || unit.is_null() {
            let error = AuError::os(AuErrorKind::InitFailed, "AudioComponentInstanceNew", status)
                .for_unit(&name);
            // A failed call does not transfer ownership, even if it partly writes
            // a nonnull handle. Reentering Dispose cannot establish its validity.
            return Err(error);
        }

        let mut instance = Self {
            foreign,
            retirement: Retirement::Live,
            unit,
            description,
            name,
            sample_rate,
            max_frames,
            initialized: false,
            accepts_midi: matches!(
                description.component_type,
                TYPE_MUSIC_DEVICE | TYPE_MUSIC_EFFECT
            ),
            accepts_audio_input: category == AuCategory::Effect,
            midi_protocol: MidiProtocol::Midi1,
            rendered_frames: 0,
            pending: Vec::with_capacity(MAX_PENDING_EVENTS),
            params: Vec::new(),
            param_scope: SCOPE_GLOBAL,
            latency: 0,
            tail: 0,
            tail_reported: false,
            host_clock: ManuallyDrop::new(Box::new(HostClock::new())),
            host_callbacks: false,
            reset_clock: HostClock::new().reset_context(None),
            input: ManuallyDrop::new(Box::new(InputState {
                left: AtomicPtr::new(ptr::null_mut()),
                right: AtomicPtr::new(ptr::null_mut()),
                block_start: AtomicI64::new(0),
                capacity: AtomicUsize::new(max_frames),
                valid_frames: AtomicUsize::new(0),
            })),
            input_left: ManuallyDrop::new(vec![0.0; max_frames].into_boxed_slice()),
            input_right: ManuallyDrop::new(vec![0.0; max_frames].into_boxed_slice()),
            output_left: ManuallyDrop::new(vec![0.0; max_frames].into_boxed_slice()),
            output_right: ManuallyDrop::new(vec![0.0; max_frames].into_boxed_slice()),
            process_error: None,
            error_reported: std::cell::Cell::new(false),
        };

        let initialized = instance.initialize(lifecycle);
        if let Err(error) = initialized {
            let mut error = error;
            let _ = instance.teardown();
            error.attach_fault(instance.foreign.fault());
            let message = error.for_unit(&instance.name);
            drop(instance);
            return Err(message);
        }
        Ok(instance)
    }

    /// Set the unit up while the caller holds the lifecycle lock.
    fn initialize(&mut self, _lifecycle: &MutexGuard<'static, ()>) -> Result<(), AuError> {
        let frames = self.max_frames as u32;
        self.set_property_number(
            PROP_MAXIMUM_FRAMES_PER_SLICE,
            SCOPE_GLOBAL,
            0,
            frames,
            false,
        )?;
        // Ask for offline rendering before initialization; units that do not
        // know the property reject it and keep their realtime behaviour.
        let _ = self.set_property_number(PROP_OFFLINE_RENDER, SCOPE_GLOBAL, 0, 1, false);

        let format = AudioStreamBasicDescription::non_interleaved_stereo(self.sample_rate);
        self.set_property(
            PROP_STREAM_FORMAT,
            SCOPE_OUTPUT,
            0,
            &format as *const _ as *const c_void,
            size_of::<AudioStreamBasicDescription>() as u32,
            true,
        )?;
        if self.accepts_audio_input {
            self.set_property(
                PROP_STREAM_FORMAT,
                SCOPE_INPUT,
                0,
                &format as *const _ as *const c_void,
                size_of::<AudioStreamBasicDescription>() as u32,
                true,
            )?;
            let callback = AURenderCallbackStruct {
                input_proc: Some(render_callback),
                input_proc_ref_con: self.input.as_ref() as *const InputState as *mut c_void,
            };
            self.set_property(
                PROP_SET_RENDER_CALLBACK,
                SCOPE_INPUT,
                0,
                &callback as *const _ as *const c_void,
                size_of::<AURenderCallbackStruct>() as u32,
                true,
            )?;
            self.input
                .left
                .store(self.input_left.as_ptr() as *mut f32, Ordering::Relaxed);
            self.input
                .right
                .store(self.input_right.as_ptr() as *mut f32, Ordering::Relaxed);
        }

        if self.accepts_midi {
            self.midi_protocol = self.negotiate_midi_protocol();
        }
        self.host_callbacks = self.install_host_callbacks();

        self.foreign.check()?;
        // SAFETY: this exclusive, newly created unit has its formats and retained
        // callback backing configured under the lifecycle lock. The initialized phase
        // is published only after guarded AudioUnitInitialize succeeds.
        let status = unsafe {
            self.foreign
                .initialize(au_guard::Initialize { unit: self.unit })?
        };
        if status != 0 {
            return Err(AuError::os(
                AuErrorKind::InitFailed,
                "AudioUnitInitialize",
                status,
            ));
        }
        self.initialized = true;

        self.latency = self
            .property_f64(PROP_LATENCY)
            .map(|seconds| frame_count(seconds, self.sample_rate))
            .unwrap_or(0);
        self.tail = self
            .property_f64(PROP_TAIL_TIME)
            .map(|seconds| frame_count(seconds, self.sample_rate))
            .unwrap_or(0);
        self.tail_reported = self.property_f64(PROP_TAIL_TIME).is_some();
        self.load_params()?;
        self.foreign.check()?;
        Ok(())
    }

    /// Offer the unit a lock-free view of the host timeline. Units that do not
    /// implement the property keep rendering; callers can warn by name.
    fn install_host_callbacks(&mut self) -> bool {
        let callbacks = HostCallbackInfo {
            host_user_data: self.host_clock.as_ref() as *const HostClock as *mut c_void,
            beat_and_tempo_proc: Some(host_beat_and_tempo),
            musical_time_location_proc: Some(host_musical_time_location),
            transport_state_proc: Some(host_transport_state),
            transport_state_proc2: Some(host_transport_state2),
        };
        // SAFETY: the SDK copies this exact HostCallbackInfo property during the call.
        // Its callbacks are static functions and user_data points to the separately
        // retained HostClock; detach/disposal or quarantine determines that backing lifetime.
        let status = unsafe {
            self.foreign.set_property(au_guard::SetProperty {
                unit: self.unit,
                property: PROP_HOST_CALLBACKS,
                scope: SCOPE_GLOBAL,
                element: 0,
                data: &callbacks as *const _ as *const c_void,
                size: size_of::<HostCallbackInfo>() as u32,
            })
        };
        status.is_ok_and(|status| status == 0)
    }

    /// True when the unit accepted `kAudioUnitProperty_HostCallbacks`.
    pub fn host_callbacks_installed(&self) -> bool {
        self.host_callbacks
    }

    /// The immutable first foreign exception; no native call is made.
    pub fn foreign_fault(&self) -> Option<AuForeignFault> {
        self.foreign.fault()
    }

    /// Display name the AudioComponent registry reports for this unit.
    pub fn component_name(&self) -> &str {
        &self.name
    }

    /// Negotiate UMP delivery when the unit answers the MIDI 2.0 protocol query.
    fn negotiate_midi_protocol(&mut self) -> MidiProtocol {
        if self.foreign.api.midi_list.is_none() {
            return MidiProtocol::Midi1;
        }
        let requested = MIDI_PROTOCOL_2_0;
        if self
            .write_protocol_property(PROP_HOST_MIDI_PROTOCOL, requested)
            .is_err()
        {
            let _ = self.write_protocol_property(PROP_HOST_MIDI_PROTOCOL, MIDI_PROTOCOL_1_0);
            return MidiProtocol::Midi1;
        }
        let reported = self.read_protocol_property(PROP_AUDIO_UNIT_MIDI_PROTOCOL);
        if reported == MIDI_PROTOCOL_2_0 {
            return MidiProtocol::Midi2;
        }
        let _ = self.write_protocol_property(PROP_HOST_MIDI_PROTOCOL, MIDI_PROTOCOL_1_0);
        MidiProtocol::Midi1
    }

    fn write_protocol_property(&mut self, property: u32, value: i32) -> Result<(), AuError> {
        // SAFETY: the protocol property consumes one i32; the typed stack value
        // and live exclusively owned unit remain available for the synchronous write.
        let status = unsafe {
            self.foreign.set_property(au_guard::SetProperty {
                unit: self.unit,
                property,
                scope: SCOPE_GLOBAL,
                element: 0,
                data: &value as *const i32 as *const c_void,
                size: size_of::<i32>() as u32,
            })?
        };
        if status != 0 {
            return Err(AuError::os(
                AuErrorKind::PropertyFailed,
                "kAudioUnitProperty_HostMIDIProtocol",
                status,
            ));
        }
        Ok(())
    }

    fn read_protocol_property(&self, property: u32) -> i32 {
        let mut value: i32 = 0;
        let mut size = size_of::<i32>() as u32;
        // SAFETY: value is initialized writable i32 storage and size supplies its
        // exact capacity for this scalar protocol property; both outlive the guarded call.
        let status = unsafe {
            self.foreign.get_property(au_guard::GetProperty {
                unit: self.unit,
                property,
                scope: SCOPE_GLOBAL,
                element: 0,
                data: &mut value as *mut i32 as *mut c_void,
                size: &mut size,
            })
        };
        if status == Ok(0) { value } else { -1 }
    }

    fn set_property(
        &mut self,
        property: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
        required: bool,
    ) -> Result<(), AuError> {
        // SAFETY: this private helper is called only with retained stack u32,
        // stream-format or render-callback structs and their matching byte sizes.
        // The unit remains exclusively owned through the synchronous property copy.
        let status = unsafe {
            self.foreign.set_property(au_guard::SetProperty {
                unit: self.unit,
                property,
                scope,
                element,
                data,
                size,
            })?
        };
        if status != 0 && required {
            return Err(AuError::os(
                AuErrorKind::InitFailed,
                &format!("AudioUnitSetProperty({property})"),
                status,
            ));
        }
        Ok(())
    }

    fn set_property_number(
        &mut self,
        property: u32,
        scope: u32,
        element: u32,
        value: u32,
        required: bool,
    ) -> Result<(), AuError> {
        self.set_property(
            property,
            scope,
            element,
            &value as *const u32 as *const c_void,
            size_of::<u32>() as u32,
            required,
        )
    }

    fn property_f64(&self, property: u32) -> Option<f64> {
        let mut value: f64 = 0.0;
        let mut size = size_of::<f64>() as u32;
        // SAFETY: this latency/tail property query owns an initialized f64 output
        // and exact in/out byte capacity on the stack, retained through the guarded read.
        let status = unsafe {
            self.foreign
                .get_property(au_guard::GetProperty {
                    unit: self.unit,
                    property,
                    scope: SCOPE_GLOBAL,
                    element: 0,
                    data: &mut value as *mut f64 as *mut c_void,
                    size: &mut size,
                })
                .ok()?
        };
        (status == 0).then_some(value)
    }

    fn load_params(&mut self) -> Result<(), AuError> {
        let Some(ids) = self.parameter_list(SCOPE_GLOBAL)? else {
            return Ok(());
        };
        let ids = if ids.is_empty() {
            match self.parameter_list(SCOPE_INPUT)? {
                Some(input_ids) if !input_ids.is_empty() => {
                    self.param_scope = SCOPE_INPUT;
                    input_ids
                }
                _ => return Ok(()),
            }
        } else {
            ids
        };
        let mut params = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(param) = self.parameter_info(id)? {
                params.push(param);
            }
        }
        self.params = params;
        self.foreign
            .check()
            .map_err(|error| self.foreign.control_error(error))?;
        Ok(())
    }

    fn parameter_list(&self, scope: u32) -> Result<Option<Vec<u32>>, AuError> {
        let mut size: u32 = 0;
        let mut writable: u8 = 0;
        // SAFETY: the API guard retains the unit and exact scalar discovery outputs.
        let status = unsafe {
            self.foreign.property_info(au_guard::PropertyInfo {
                unit: self.unit,
                property: PROP_PARAMETER_LIST,
                scope,
                element: 0,
                size: &mut size,
                writable: &mut writable,
            })
        }
        .map_err(|error| self.foreign.control_error(error))?;
        if status != 0 {
            return Ok(None);
        }
        let count = authored_parameters::parameter_list_len(size).ok_or_else(|| {
            AuError::new(
                AuErrorKind::PropertyFailed,
                "AudioUnit parameter list exceeds its bounds or has an unaligned byte size",
            )
        })?;
        if count == 0 {
            return Ok(None);
        }
        let expected = size;
        let mut ids = vec![0u32; count];
        // SAFETY: the shared metadata owner validated alignment and count before allocation;
        // the advertised byte size exactly equals this retained vector's writable storage.
        let status = unsafe {
            self.foreign.get_property(au_guard::GetProperty {
                unit: self.unit,
                property: PROP_PARAMETER_LIST,
                scope,
                element: 0,
                data: ids.as_mut_ptr().cast(),
                size: &mut size,
            })
        }
        .map_err(|error| self.foreign.control_error(error))?;
        if status != 0 {
            return Err(AuError::os(
                AuErrorKind::PropertyFailed,
                "AudioUnitGetProperty",
                status,
            ));
        }
        if size != expected {
            return Err(AuError::new(
                AuErrorKind::PropertyFailed,
                "AudioUnit parameter list returned a partial or changed byte size",
            ));
        }
        Ok(Some(ids))
    }

    fn parameter_info(&self, id: u32) -> Result<Option<AuParam>, AuError> {
        // SAFETY: the SDK parameter-info record contains only scalars, fixed byte
        // arrays and nullable CF pointers, all of which admit an all-zero initial representation.
        let mut info: AudioUnitParameterInfo = unsafe { std::mem::zeroed() };
        let mut size = size_of::<AudioUnitParameterInfo>() as u32;
        // SAFETY: info has the exact SDK record layout and writable size supplies
        // its byte capacity. Status and returned size are checked before any CF pointer
        // is admitted as an owned parameter-name reference.
        let status = unsafe {
            self.foreign
                .get_property(au_guard::GetProperty {
                    unit: self.unit,
                    property: PROP_PARAMETER_INFO,
                    scope: self.param_scope,
                    element: id,
                    data: &mut info as *mut AudioUnitParameterInfo as *mut c_void,
                    size: &mut size,
                })
                .map_err(|error| self.foreign.control_error(error))?
        };
        if status != 0 {
            return Err(AuError::os(
                AuErrorKind::PropertyFailed,
                "AudioUnitGetProperty(ParameterInfo)",
                status,
            ));
        }
        if size != size_of::<AudioUnitParameterInfo>() as u32 {
            return Err(AuError::new(
                AuErrorKind::PropertyFailed,
                "AudioUnit parameter info returned a partial or changed byte size",
            ));
        }
        let (name, _) =
            authored_parameters::parameter_names(&info, &self.foreign).map_err(|_| {
                match self.foreign.fault() {
                    Some(fault) => self.foreign.control_error(fault.error),
                    None => AuError::new(
                        AuErrorKind::PropertyFailed,
                        "AudioUnit parameter name metadata is invalid",
                    ),
                }
            })?;
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(AuParam {
            id,
            name,
            min: f64::from(info.min_value),
            max: f64::from(info.max_value),
            default: f64::from(info.default_value),
            writable: info.flags & PARAM_FLAG_IS_WRITABLE != 0,
            unit: unit_symbol(info.unit),
        }))
    }

    /// Host-visible parameters in the unit's own engineering units.
    pub fn get_params(&self) -> Vec<AuParam> {
        self.params.clone()
    }

    /// The generic panel's view of every parameter this unit exposes.
    ///
    /// `kAudioUnitProperty_ParameterInfo` carries no step count, so `steps` stays
    /// `None`; a unit that names no standard unit reports an empty unit.
    /// Compatibility surface; checked consumers use `try_parameter_surface` and inspect its error.
    pub fn parameter_surface(&self) -> crate::params::ParamSurface {
        self.try_parameter_surface()
            .unwrap_or_else(|_| crate::params::surface(Vec::new()))
    }

    /// Read the parameter surface, reporting a foreign fault.
    pub fn try_parameter_surface(&self) -> Result<crate::params::ParamSurface, AuError> {
        let _api = api_lock();
        self.foreign
            .check()
            .map_err(|error| self.foreign.control_error(error))?;
        let descriptors = self
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                if !param.min.is_finite()
                    || !param.max.is_finite()
                    || !param.default.is_finite()
                    || param.min > param.max
                {
                    return Err(AuError::new(
                        AuErrorKind::PropertyFailed,
                        "AudioUnit parameter metadata has an invalid range or non-finite value",
                    ));
                }
                let current = {
                    let mut value: f32 = 0.0;
                    // SAFETY: the retained unit and inventory parameter ID stay owned by this
                    // metadata read; value is writable f32 stack storage, inspected only after success.
                    let status = unsafe {
                        self.foreign.get_parameter(au_guard::GetParameter {
                            unit: self.unit,
                            parameter: param.id,
                            scope: self.param_scope,
                            element: 0,
                            value: &mut value,
                        })
                    };
                    let status = status.map_err(|error| self.foreign.control_error(error))?;
                    if status != 0 {
                        return Err(AuError::os(
                            AuErrorKind::PropertyFailed,
                            "AudioUnitGetParameter",
                            status,
                        ));
                    }
                    if !value.is_finite() {
                        return Err(AuError::new(
                            AuErrorKind::PropertyFailed,
                            "AudioUnitGetParameter returned a non-finite value",
                        ));
                    }
                    f64::from(value).clamp(param.min, param.max)
                };
                Ok(crate::params::ParamDescriptor {
                    index: index as u32,
                    name: param.name.clone(),
                    unit: param.unit.clone(),
                    min: param.min,
                    max: param.max,
                    current,
                    display: crate::params::display_text(current, &param.unit),
                    normalized: crate::params::normalize(current, param.min, param.max),
                    default: Some(param.default),
                    steps: None,
                    automatable: param.writable,
                    by_passing: crate::params::names_bypass(&param.name) && current >= 0.5,
                    group: None,
                })
            })
            .collect::<Result<Vec<_>, AuError>>()?;
        Ok(crate::params::surface(descriptors))
    }

    /// The component description this instance was opened from.
    pub fn description(&self) -> &AudioComponentDescription {
        &self.description
    }

    /// Hosting category of the instance.
    pub fn category(&self) -> AuCategory {
        category_of(&self.description)
    }

    /// Whether the instance is a music device.
    pub fn is_instrument(&self) -> bool {
        self.description.component_type == TYPE_MUSIC_DEVICE
    }

    /// Stereo output pairs this host can read; AU multi-bus routing is not wired.
    pub fn output_count(&self) -> usize {
        1
    }

    /// Declared audio output ports; this host reads the stereo pair only.
    pub fn output_ports(&self) -> Vec<AudioPort> {
        vec![AudioPort::STEREO]
    }

    /// Declared audio input ports; effects have the main stereo pair.
    pub fn input_ports(&self) -> Vec<AudioPort> {
        if self.accepts_audio_input {
            vec![AudioPort::STEREO]
        } else {
            Vec::new()
        }
    }

    /// Display name reported by the component.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reported latency in samples.
    pub fn latency_samples(&self) -> u32 {
        self.latency
    }

    /// Reported tail in samples.
    pub fn tail_samples(&self) -> u32 {
        self.tail
    }

    /// Declared tail, or `None` when the unit answered no tail-time property.
    pub fn plugin_tail_samples(&self) -> Option<u32> {
        self.tail_reported.then_some(self.tail)
    }

    fn param_by_name(&self, name: &str) -> Option<&AuParam> {
        fn normalized(name: &str) -> impl Iterator<Item = char> + '_ {
            name.chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .map(|character| character.to_ascii_lowercase())
        }
        self.params
            .iter()
            .find(|param| normalized(&param.name).eq(normalized(name)))
    }

    /// Set a parameter immediately, in the unit's own units.
    pub fn set_param_by_name(&mut self, name: &str, value: f64) -> bool {
        let Some(param) = self.param_by_name(name) else {
            return false;
        };
        if !param.writable || !value.is_finite() {
            return false;
        }
        let (id, value) = (param.id, value.clamp(param.min, param.max) as f32);
        // SAFETY: the exclusive unit borrow retains the selected writable parameter
        // and passes only its ID/scope and a by-value SDK f32; no caller buffer escapes.
        let status = unsafe {
            self.foreign.set_parameter(au_guard::SetParameter {
                unit: self.unit,
                parameter: id,
                scope: self.param_scope,
                element: 0,
                value,
                offset: 0,
            })
        };
        let status = match status {
            Ok(status) => status,
            Err(_) => return false,
        };
        if status != 0 {
            self.note_process_error(AuProcessError::Native {
                operation: "AudioUnitSetParameter",
                status,
            });
        }
        status == 0
    }

    /// Queue a normalized 0..=1 parameter change for the next render block.
    pub fn queue_param_by_name(&mut self, name: &str, normalized: f64, sample_offset: u32) -> bool {
        let Some(param) = self.param_by_name(name) else {
            return false;
        };
        if !param.writable || !normalized.is_finite() || sample_offset as usize >= self.max_frames {
            return false;
        }
        let value = param.min + normalized.clamp(0.0, 1.0) * (param.max - param.min);
        let id = param.id;
        if self.pending.len() == MAX_PENDING_EVENTS {
            self.note_process_error(AuProcessError::EventCapacityExceeded);
            return false;
        }
        self.pending.push(PendingEvent::Parameter {
            id,
            value: value as f32,
            offset: sample_offset,
        });
        true
    }

    /// Read a parameter in the unit's own units.
    pub fn get_param_by_name(&self, name: &str) -> Option<f64> {
        let param = self.param_by_name(name)?;
        let mut value: f32 = 0.0;
        // SAFETY: this retained inventory ID addresses the live unit and value is
        // initialized writable f32 storage through the synchronous parameter read.
        let status = unsafe {
            self.foreign
                .get_parameter(au_guard::GetParameter {
                    unit: self.unit,
                    parameter: param.id,
                    scope: self.param_scope,
                    element: 0,
                    value: &mut value as *mut f32,
                })
                .ok()?
        };
        (status == 0).then_some(f64::from(value))
    }

    /// Restore a `.aupreset` or any other ClassInfo property list.
    pub fn set_state_bytes(&mut self, bytes: &[u8]) -> Result<(), AuError> {
        let _api = api_lock();
        const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
        let result = (|| {
            self.foreign.check()?;
            if bytes.is_empty() || bytes.len() > MAX_STATE_BYTES {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "state is empty or exceeds the 64 MiB limit",
                ));
            }
            // SAFETY: bytes are retained throughout this bounded copying operation.
            let raw_data = unsafe {
                self.foreign.data_create(au_guard::DataCreate {
                    allocator: ptr::null(),
                    bytes: bytes.as_ptr(),
                    length: bytes.len() as isize,
                })
            }?;
            let data = CfOwned::new(raw_data, &self.foreign);
            if data.value.is_null() {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "state could not be wrapped as CFData",
                ));
            }
            // SAFETY: the data guard retains its native reference through property-list conversion.
            let raw_plist = unsafe {
                self.foreign.plist_create(au_guard::PlistCreate {
                    allocator: ptr::null(),
                    data: data.value,
                    options: 0,
                    format: ptr::null_mut(),
                    error: ptr::null_mut(),
                })
            }?;
            let plist = CfOwned::new(raw_plist, &self.foreign);
            data.close()?;
            if plist.value.is_null() {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "state is not a property list (.aupreset expected)",
                ));
            }
            // SAFETY: the ClassInfo value and +1 native property list remain live until delivery completes.
            let status = unsafe {
                self.foreign.set_property(au_guard::SetProperty {
                    unit: self.unit,
                    property: PROP_CLASS_INFO,
                    scope: SCOPE_GLOBAL,
                    element: 0,
                    data: (&plist.value as *const *mut c_void).cast(),
                    size: size_of::<*mut c_void>() as u32,
                })
            }?;
            if status != 0 {
                return Err(AuError::os(
                    AuErrorKind::StateFailed,
                    "kAudioUnitProperty_ClassInfo",
                    status,
                ));
            }
            plist.close()?;
            Ok(())
        })();
        result.map_err(|mut error: AuError| {
            error.attach_fault(self.foreign.fault());
            error
        })
    }

    /// Capture the unit's current state as a binary property list.
    pub fn state_bytes(&self) -> Result<Vec<u8>, AuError> {
        let _api = api_lock();
        const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
        let result = (|| {
            let mut raw_plist: *mut c_void = ptr::null_mut();
            let mut size = size_of::<*mut c_void>() as u32;
            // SAFETY: native output storage matches the ClassInfo pointer property.
            let status = unsafe {
                self.foreign.get_property(au_guard::GetProperty {
                    unit: self.unit,
                    property: PROP_CLASS_INFO,
                    scope: SCOPE_GLOBAL,
                    element: 0,
                    data: (&mut raw_plist as *mut *mut c_void).cast(),
                    size: &mut size,
                })
            }?;
            if status != 0 {
                return Err(AuError::os(
                    AuErrorKind::StateFailed,
                    "kAudioUnitProperty_ClassInfo",
                    status,
                ));
            }
            if size != size_of::<*mut c_void>() as u32 || raw_plist.is_null() {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "unit reports invalid class info",
                ));
            }
            // Only a complete successful pointer result transfers a CF reference.
            let plist = CfOwned::new(raw_plist, &self.foreign);
            // SAFETY: the owned native property list remains retained during serialization.
            let raw_data = unsafe {
                self.foreign.plist_data(au_guard::PlistData {
                    allocator: ptr::null(),
                    plist: plist.value,
                    format: PLIST_BINARY_FORMAT,
                    options: 0,
                    error: ptr::null_mut(),
                })
            }?;
            let data = CfOwned::new(raw_data, &self.foreign);
            plist.close()?;
            if data.value.is_null() {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "class info could not be serialized",
                ));
            }
            // SAFETY: data is retained for the captured length and byte-pointer reads.
            let length = unsafe {
                self.foreign
                    .data_length(au_guard::DataLength { data: data.value })
            }?;
            let length = usize::try_from(length)
                .ok()
                .filter(|length| *length <= MAX_STATE_BYTES)
                .ok_or_else(|| {
                    AuError::new(
                        AuErrorKind::StateFailed,
                        "class info exceeds the 64 MiB limit",
                    )
                })?;
            // SAFETY: this owned immutable CFData retains its contiguous byte storage.
            let bytes = unsafe {
                self.foreign
                    .data_bytes(au_guard::DataBytes { data: data.value })
            }?;
            if bytes.is_null() && length != 0 {
                return Err(AuError::new(
                    AuErrorKind::StateFailed,
                    "class info has no byte storage",
                ));
            }
            let output = if length == 0 {
                Vec::new()
            } else {
                // SAFETY: CFData retains exactly its captured bounded native byte storage.
                unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec()
            };
            data.close()?;
            Ok(output)
        })();
        result.map_err(|mut error: AuError| {
            error.attach_fault(self.foreign.fault());
            error
        })
    }

    // MIDI follows the VST3/CLAP sink shape: events are queued for the next
    // render and flushed with their block-relative offsets.
    /// Queue a note-on with a block-relative sample offset.
    pub fn note_on_channel(&mut self, channel: u8, key: u8, velocity: f32, sample_offset: i32) {
        let velocity = (velocity.clamp(0.0, 1.0) * 127.0).round() as u32;
        self.queue_midi(
            0x90 | u32::from(channel & 0x0f),
            u32::from(key),
            velocity,
            sample_offset,
        );
    }

    /// Queue a note-off with a block-relative sample offset.
    pub fn note_off_channel(&mut self, channel: u8, key: u8, sample_offset: i32) {
        self.queue_midi(
            0x80 | u32::from(channel & 0x0f),
            u32::from(key),
            0,
            sample_offset,
        );
    }

    /// Queue a control change with a block-relative sample offset.
    pub fn control_change(&mut self, channel: u8, controller: u8, value: u8, sample_offset: i32) {
        self.queue_midi(
            0xb0 | u32::from(channel & 0x0f),
            u32::from(controller),
            u32::from(value),
            sample_offset,
        );
    }

    /// Queue a program change with a block-relative sample offset.
    pub fn program_change(&mut self, channel: u8, program: u8, sample_offset: i32) {
        self.queue_midi(
            0xc0 | u32::from(channel & 0x0f),
            u32::from(program),
            0,
            sample_offset,
        );
    }

    /// Queue a pitch bend in the MIDI -8192..8191 range.
    pub fn pitch_bend(&mut self, channel: u8, value: i16, sample_offset: i32) {
        let value = i32::from(value).clamp(-8192, 8191) + 8192;
        self.queue_midi(
            0xe0 | u32::from(channel & 0x0f),
            (value & 0x7f) as u32,
            ((value >> 7) & 0x7f) as u32,
            sample_offset,
        );
    }

    /// Queue channel pressure at a block-relative offset.
    pub fn channel_pressure(&mut self, channel: u8, pressure: u8, sample_offset: i32) {
        self.queue_midi(
            0xd0 | u32::from(channel & 0x0f),
            u32::from(pressure),
            0,
            sample_offset,
        );
    }

    /// Queue polyphonic pressure at a block-relative offset.
    pub fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, sample_offset: i32) {
        self.queue_midi(
            0xa0 | u32::from(channel & 0x0f),
            u32::from(note),
            u32::from(pressure),
            sample_offset,
        );
    }

    fn queue_midi(&mut self, status: u32, data1: u32, data2: u32, sample_offset: i32) {
        if !self.accepts_midi {
            return;
        }
        if self.pending.len() == MAX_PENDING_EVENTS {
            self.note_process_error(AuProcessError::EventCapacityExceeded);
            return;
        }
        self.pending.push(PendingEvent::Midi {
            status,
            data1,
            data2,
            offset: sample_offset.max(0) as u32,
        });
    }

    fn note_process_error(&mut self, error: AuProcessError) {
        if self.process_error.is_none() {
            self.process_error = Some(error);
            self.error_reported.set(false);
        }
    }

    /// Revalidate retained load-time storage without changing native history,
    /// parameters or queued events. Corrected host-side buffer/rate/offset
    /// requests may resume after this control-plane acknowledgement. Lost
    /// events and native failures require a successful explicit reset.
    pub fn prepare_block_buffers(&mut self) -> Result<(), AuProcessError> {
        self.foreign.check()?;
        if [
            self.input_left.len(),
            self.input_right.len(),
            self.output_left.len(),
            self.output_right.len(),
        ]
        .into_iter()
        .any(|length| length < self.max_frames)
            || self.pending.capacity() < MAX_PENDING_EVENTS
        {
            let error = AuProcessError::CapacityExceeded;
            self.note_process_error(error);
            return Err(error);
        }
        if let Some(
            error @ (AuProcessError::Native { .. }
            | AuProcessError::Foreign(_)
            | AuProcessError::EventCapacityExceeded),
        ) = self.process_error
        {
            return Err(error);
        }
        self.process_error = None;
        self.error_reported.set(false);
        Ok(())
    }

    fn preflight_block(
        &self,
        samples: usize,
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) -> Result<(), AuProcessError> {
        self.foreign.check()?;
        if let Some(error) = self.process_error {
            return Err(error);
        }
        if !samples.is_multiple_of(2) {
            return Err(AuProcessError::IncompleteFrame);
        }
        if sample_rate != self.sample_rate {
            return Err(AuProcessError::SampleRateChanged);
        }
        if !self.initialized {
            return Err(AuProcessError::Uninitialized);
        }
        let frames = samples / 2;
        if frames > self.max_frames {
            return Err(AuProcessError::CapacityExceeded);
        }
        if frames == 0 {
            return Ok(());
        }
        if self.pending.iter().any(|event| {
            let offset = match event {
                PendingEvent::Midi { offset, .. } | PendingEvent::Parameter { offset, .. } => {
                    *offset
                }
            };
            offset as usize >= frames
        }) {
            return Err(AuProcessError::EventOffsetOutOfRange);
        }
        if self.rendered_frames.checked_add(frames as u64).is_none()
            || transport
                .is_some_and(|transport| transport.sample_pos.checked_add(frames as u64).is_none())
        {
            return Err(AuProcessError::TimelineOverflow);
        }
        Ok(())
    }

    /// Render a single bounded block into retained caller storage. Validation
    /// rejects malformed/oversized destinations before any native call, queue
    /// consumption, output mutation or timeline advancement. Native failures
    /// retain the original OSStatus and are never successful silent audio.
    /// The caller retains the transport clock. No clock ownership is retained
    /// and no last reference is released. This path never takes the global
    /// control mutex.
    pub fn process_block_into(
        &mut self,
        output: &mut [f32],
        transport: Option<&PluginTransport>,
    ) -> Result<(), AuProcessError> {
        self.process_prepared(output, self.sample_rate, transport, false)
    }

    /// Process an effect block in place.
    pub fn process_effect_into(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<&PluginTransport>,
    ) -> Result<(), AuProcessError> {
        self.process_prepared(buffer, sample_rate, transport, true)
    }

    fn process_prepared(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<&PluginTransport>,
        input: bool,
    ) -> Result<(), AuProcessError> {
        if let Err(error) = self.preflight_block(buffer.len(), sample_rate, transport) {
            self.note_process_error(error);
            return Err(error);
        }
        let frames = buffer.len() / 2;
        if frames == 0 {
            return Ok(());
        }
        self.reset_clock = self.host_clock.reset_context(transport);
        if input {
            for frame in 0..frames {
                self.input_left[frame] = buffer[frame * 2];
                self.input_right[frame] = buffer[frame * 2 + 1];
            }
        }
        if let Err(error) = self.render_chunk(frames, input, transport) {
            self.note_process_error(error);
            return Err(error);
        }
        for frame in 0..frames {
            buffer[frame * 2] = self.output_left[frame];
            buffer[frame * 2 + 1] = self.output_right[frame];
        }
        Ok(())
    }

    /// Allocating convenience adapter. The prepared slice API owns all native
    /// processing and error status; this adapter only partitions larger blocks.
    /// It owns the incoming transport and may drop its last clock reference;
    /// strict processing callers use the borrowed prepared entrypoints.
    pub fn process_block_with_transport(
        &mut self,
        num_frames: u32,
        transport: Option<PluginTransport>,
    ) -> Vec<f32> {
        let mut output = vec![0.0; num_frames as usize * 2];
        self.process_partitioned(&mut output, self.sample_rate, transport, false);
        output
    }

    /// Process an interleaved stereo block without transport.
    pub fn process_interleaved(&mut self, buffer: &mut [f32]) {
        self.process_interleaved_with_transport(buffer, None);
    }
    /// Process an interleaved stereo block with host transport.
    pub fn process_interleaved_with_transport(
        &mut self,
        buffer: &mut [f32],
        transport: Option<PluginTransport>,
    ) {
        self.process_partitioned(buffer, self.sample_rate, transport, true);
    }

    fn process_partitioned(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
        input: bool,
    ) {
        if !buffer.len().is_multiple_of(2) {
            self.note_process_error(AuProcessError::IncompleteFrame);
            return;
        }
        let mut offset = 0;
        for chunk in buffer.chunks_mut(self.max_frames * 2) {
            let block_transport = transport.as_ref().and_then(|transport| {
                transport
                    .sample_pos
                    .checked_add(offset as u64)
                    .map(|sample| transport.at_sample(sample))
            });
            if transport.is_some() && block_transport.is_none() {
                self.note_process_error(AuProcessError::TimelineOverflow);
                return;
            }
            if self
                .process_prepared(chunk, sample_rate, block_transport.as_ref(), input)
                .is_err()
            {
                return;
            }
            offset += chunk.len() / 2;
        }
    }

    fn flush_pending(&mut self, _frames: usize) -> Result<(), AuProcessError> {
        for index in 0..self.pending.len() {
            let event = self.pending[index];
            match event {
                PendingEvent::Midi {
                    status,
                    data1,
                    data2,
                    offset,
                } => {
                    let status_code = match self.midi_protocol {
                        // SAFETY: process preflight admits this queued MIDI event and offset for
                        // the initialized exclusively borrowed unit; the guarded call uses scalar MIDI bytes only.
                        MidiProtocol::Midi1 => unsafe {
                            self.foreign.midi(au_guard::Midi {
                                unit: self.unit,
                                status,
                                data1,
                                data2,
                                offset,
                            })?
                        },
                        MidiProtocol::Midi2 => self.send_ump(status, data1, data2, offset)?,
                    };
                    if status_code != 0 {
                        return Err(AuProcessError::Native {
                            operation: match self.midi_protocol {
                                MidiProtocol::Midi1 => "MusicDeviceMIDIEvent",
                                MidiProtocol::Midi2 => "MusicDeviceMIDIEventList",
                            },
                            status: status_code,
                        });
                    }
                }
                PendingEvent::Parameter { id, value, offset } => {
                    // SAFETY: process preflight validated this queued parameter offset for
                    // the current block; the exclusive retained unit consumes only scalar ID/value arguments.
                    let status = unsafe {
                        self.foreign.set_parameter(au_guard::SetParameter {
                            unit: self.unit,
                            parameter: id,
                            scope: self.param_scope,
                            element: 0,
                            value,
                            offset,
                        })?
                    };
                    if status != 0 {
                        return Err(AuProcessError::Native {
                            operation: "AudioUnitSetParameter",
                            status,
                        });
                    }
                }
            }
        }
        self.pending.clear();
        Ok(())
    }

    fn send_ump(
        &mut self,
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    ) -> Result<i32, AuProcessError> {
        let Some(send) = self.foreign.api.midi_list else {
            return Err(AuProcessError::Native {
                operation: "MusicDeviceMIDIEventList resolution",
                status: -1,
            });
        };
        let word = 0x2000_0000 | ((status & 0xff) << 16) | ((data1 & 0x7f) << 8) | (data2 & 0x7f);
        let mut list = MidiEventList {
            protocol: MIDI_PROTOCOL_2_0 as u32,
            num_packets: 1,
            packet: [MidiEventPacket {
                time_stamp: 0,
                word_count: 1,
                words: [word, 0, 0, 0, 0, 0, 0, 0],
            }],
        };
        // SAFETY: the stack event list remains live through the synchronous native call.
        Ok(unsafe {
            self.foreign.midi_list(
                send,
                au_guard::MidiList {
                    unit: self.unit,
                    offset,
                    events: &mut list,
                },
            )
        }?)
    }

    fn render_chunk(
        &mut self,
        frames: usize,
        with_input: bool,
        transport: Option<&PluginTransport>,
    ) -> Result<(), AuProcessError> {
        debug_assert!(
            self.initialized,
            "a unit may only be rendered while it is initialised"
        );
        if frames == 0 {
            return Ok(());
        }
        if frames > self.max_frames {
            return Err(AuProcessError::CapacityExceeded);
        }
        let position = transport.map_or(self.rendered_frames, |transport| transport.sample_pos);
        self.host_clock.publish(transport, position);
        if with_input {
            self.input
                .block_start
                .store(position as i64, Ordering::Relaxed);
        }
        self.flush_pending(frames)?;

        self.output_left[..frames].fill(0.0);
        self.output_right[..frames].fill(0.0);
        let mut buffers = AudioBufferList {
            number_buffers: 2,
            buffers: [
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: (frames * size_of::<f32>()) as u32,
                    data: self.output_left.as_mut_ptr() as *mut c_void,
                },
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: (frames * size_of::<f32>()) as u32,
                    data: self.output_right.as_mut_ptr() as *mut c_void,
                },
            ],
        };
        let timestamp = AudioTimeStamp::at_sample(position as f64);
        let mut flags: u32 = 0;
        self.input
            .valid_frames
            .store(if with_input { frames } else { 0 }, Ordering::Release);
        // SAFETY: the exclusive prepared instance retains both output planes
        // for the admitted frame count; list/timestamp/flags have SDK layout and
        // remain live. Input valid_frames publishes only this block to callbacks.
        let result = unsafe {
            self.foreign.render(au_guard::Render {
                unit: self.unit,
                flags: &mut flags,
                timestamp: &timestamp,
                bus: 0,
                frames: frames as u32,
                data: &mut buffers,
            })
        };
        self.input.valid_frames.store(0, Ordering::Release);
        let status = result?;
        self.rendered_frames += frames as u64;
        if status != 0 {
            return Err(AuProcessError::Native {
                operation: "AudioUnitRender",
                status,
            });
        }
        Ok(())
    }

    /// Reset native state and host queues.
    pub fn reset(&mut self) {
        let _ = self.reset_prepared();
    }

    /// Reset native history and host timeline/queues in place. Plane and event
    /// capacities, parameters and captured state remain owned by this unit.
    pub fn reset_prepared(&mut self) -> Result<(), AuProcessError> {
        self.foreign.check()?;
        self.input.valid_frames.store(0, Ordering::Release);
        // SAFETY: this exclusive prepared unit remains live and retains callback
        // backing during reset. The input window has been retired before native reentry;
        // the boundary refuses any already-poisoned unit without another native call.
        let status = unsafe {
            self.foreign.reset(au_guard::Reset {
                unit: self.unit,
                scope: SCOPE_GLOBAL,
                element: 0,
            })?
        };
        if status != 0 {
            let error = AuProcessError::Native {
                operation: "AudioUnitReset",
                status,
            };
            self.note_process_error(error);
            return Err(error);
        }
        self.pending.clear();
        self.rendered_frames = 0;
        self.host_clock.restore(self.reset_clock);
        self.input.block_start.store(0, Ordering::Relaxed);
        self.input_left.fill(0.0);
        self.input_right.fill(0.0);
        self.output_left.fill(0.0);
        self.output_right.fill(0.0);
        self.process_error = None;
        self.error_reported.set(false);
        Ok(())
    }

    /// Undo setup while the lifecycle lock is held.
    ///
    /// Callback detachment, uninitialization and disposal must all succeed
    /// before host backing can be released. Any unknown or refused transition
    /// quarantines ownership and prevents another cleanup attempt.
    fn teardown(&mut self) -> Result<(), AuProcessError> {
        if self.retirement == Retirement::Disposed {
            return Ok(());
        }
        if self.retirement == Retirement::Quarantined {
            return Err(AuProcessError::Retired);
        }
        self.retirement = Retirement::Quarantined;
        self.foreign.check()?;
        let checked = |operation, status| {
            if status == 0 {
                Ok(())
            } else {
                Err(AuProcessError::Native { operation, status })
            }
        };
        if self.accepts_audio_input {
            let callback = AURenderCallbackStruct {
                input_proc: None,
                input_proc_ref_con: ptr::null_mut(),
            };
            // SAFETY: callback record is live; backing remains retained until all retirement succeeds.
            let status = unsafe {
                self.foreign.set_property(au_guard::SetProperty {
                    unit: self.unit,
                    property: PROP_SET_RENDER_CALLBACK,
                    scope: SCOPE_INPUT,
                    element: 0,
                    data: (&callback as *const AURenderCallbackStruct).cast(),
                    size: size_of::<AURenderCallbackStruct>() as u32,
                })
            }?;
            checked("AudioUnitSetProperty(detach render callback)", status)?;
        }
        if self.host_callbacks {
            let callbacks = HostCallbackInfo {
                host_user_data: ptr::null_mut(),
                beat_and_tempo_proc: None,
                musical_time_location_proc: None,
                transport_state_proc: None,
                transport_state_proc2: None,
            };
            // SAFETY: the empty callback record remains live until the synchronous detach completes.
            let status = unsafe {
                self.foreign.set_property(au_guard::SetProperty {
                    unit: self.unit,
                    property: PROP_HOST_CALLBACKS,
                    scope: SCOPE_GLOBAL,
                    element: 0,
                    data: (&callbacks as *const HostCallbackInfo).cast(),
                    size: size_of::<HostCallbackInfo>() as u32,
                })
            }?;
            checked("AudioUnitSetProperty(detach host callbacks)", status)?;
            self.host_callbacks = false;
        }
        if self.initialized {
            // SAFETY: the native unit remains live until disposal and callbacks have detached.
            let status = unsafe {
                self.foreign
                    .uninitialize(au_guard::Uninitialize { unit: self.unit })
            }?;
            checked("AudioUnitUninitialize", status)?;
            self.initialized = false;
        }
        // SAFETY: the live unit is uninitialized and no host callback backing has been released.
        let status = unsafe { self.foreign.dispose(au_guard::Dispose { unit: self.unit }) }?;
        checked("AudioComponentInstanceDispose", status)?;
        self.unit = ptr::null_mut();
        self.retirement = Retirement::Disposed;
        Ok(())
    }

    fn release_backing(&mut self) {
        // SAFETY: this method runs exactly once from Drop, only after successful native disposal.
        // Failed/unknown retirement deliberately retains every pointer reachable from the unit.
        unsafe {
            ManuallyDrop::drop(&mut self.host_clock);
            ManuallyDrop::drop(&mut self.input);
            ManuallyDrop::drop(&mut self.input_left);
            ManuallyDrop::drop(&mut self.input_right);
            ManuallyDrop::drop(&mut self.output_left);
            ManuallyDrop::drop(&mut self.output_right);
        }
    }
}

impl Drop for AuInstance {
    fn drop(&mut self) {
        if self.retirement == Retirement::Live {
            let _lifecycle = api_lock();
            let _ = self.teardown();
        }
        if self.retirement == Retirement::Disposed {
            self.release_backing();
        }
    }
}

impl oc_audio::AudioEffect for AuInstance {
    fn prepare_block(&mut self, _max_frames: usize) -> Result<(), oc_audio::BlockPreparationError> {
        // Native frame capacity is fixed at load. The graph splits calls at
        // max_block_frames rather than changing the AU stream format on DSP.
        self.prepare_block_buffers()
            .map_err(|_| oc_audio::BlockPreparationError::CapacityExceeded)
    }
    fn processing_failed(&self) -> bool {
        self.foreign.failed() || self.process_error.is_some()
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        if let Some(fault) = self.foreign.take_report() {
            return oc_audio::ProcessingFailureReport::Ready(format!(
                "AudioUnit {}: {fault}",
                self.name
            ));
        }
        if self.foreign.failed() {
            return if self.foreign.fault().is_some() {
                oc_audio::ProcessingFailureReport::Reported
            } else {
                oc_audio::ProcessingFailureReport::Pending
            };
        }
        match self.process_error {
            None => oc_audio::ProcessingFailureReport::Unavailable,
            Some(_) if self.error_reported.replace(true) => {
                oc_audio::ProcessingFailureReport::Reported
            }
            Some(error) => oc_audio::ProcessingFailureReport::Ready(format!(
                "AudioUnit {}: {error}",
                self.name
            )),
        }
    }
    fn max_block_frames(&self) -> u32 {
        self.max_frames as u32
    }
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_partitioned(buffer, sample_rate, None, true);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_partitioned(buffer, sample_rate, transport, true);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        self.set_param_by_name(name, value);
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        self.get_param_by_name(name)
    }

    fn reset(&mut self) {
        AuInstance::reset(self);
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn tail_samples(&self) -> u32 {
        self.tail
    }

    fn plugin_tail_samples(&self) -> Option<u32> {
        AuInstance::plugin_tail_samples(self)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Copy a bounded stereo input block into the native callback's two mono planes.
unsafe extern "C" fn render_callback(
    ref_con: *mut c_void,
    _flags: *mut u32,
    timestamp: *const AudioTimeStamp,
    _bus: u32,
    frames: u32,
    io_data: *mut AudioBufferList,
) -> i32 {
    const AUDIO_PARAM_ERROR: i32 = -50;
    if ref_con.is_null() || io_data.is_null() {
        return AUDIO_PARAM_ERROR;
    }
    // SAFETY: callback registration retains the boxed InputState until detach,
    // uninitialize and dispose finish; failed retirement retains it indefinitely.
    let state = unsafe { &*ref_con.cast::<InputState>() };
    let capacity = state.capacity.load(Ordering::Relaxed);
    let valid_frames = state.valid_frames.load(Ordering::Acquire).min(capacity);
    let count = frames as usize;
    if count > capacity {
        return AUDIO_PARAM_ERROR;
    }
    let Some(required) = count.checked_mul(size_of::<f32>()) else {
        return AUDIO_PARAM_ERROR;
    };
    let block_start = state.block_start.load(Ordering::Relaxed);
    let start = if timestamp.is_null() {
        0
    } else {
        // SAFETY: AU supplies a readable AudioTimeStamp for this synchronous
        // callback when non-null. No timestamp reference escapes the call.
        let flags = unsafe { ptr::read(ptr::addr_of!((*timestamp).flags)) };
        if flags & TIME_STAMP_SAMPLE_TIME_VALID == 0 {
            return AUDIO_PARAM_ERROR;
        }
        // SAFETY: the same readable timestamp explicitly marks its sample-time field valid.
        let sample_time = unsafe { ptr::read(ptr::addr_of!((*timestamp).sample_time)) };
        let rounded = sample_time.round();
        if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= -(i64::MIN as f64) {
            return AUDIO_PARAM_ERROR;
        }
        if sample_time < block_start as f64 {
            return AUDIO_PARAM_ERROR;
        }
        let Some(delta) = (rounded as i64).checked_sub(block_start) else {
            return AUDIO_PARAM_ERROR;
        };
        // Only the current block's input is retained. Refuse earlier requests
        // instead of incorrectly replaying its first samples at a negative offset.
        let Ok(start) = usize::try_from(delta) else {
            return AUDIO_PARAM_ERROR;
        };
        start
    };
    // SAFETY: native AudioBufferList always provides its readable count header;
    // read that scalar without forming a reference to our larger two-plane type.
    let planes = unsafe { ptr::read(ptr::addr_of!((*io_data).number_buffers)) };
    if planes != 2 {
        return AUDIO_PARAM_ERROR;
    }
    // SAFETY: the admitted native header promises two AudioBuffer descriptors.
    // Raw field projection uses the ABI offset without creating a fixed-size reference.
    let descriptions = unsafe { ptr::addr_of!((*io_data).buffers).cast::<AudioBuffer>() };
    // SAFETY: count=2 admits both readable descriptors; copy them before any PCM write.
    let buffers = unsafe { [ptr::read(descriptions), ptr::read(descriptions.add(1))] };
    let sources = [
        state.left.load(Ordering::Relaxed),
        state.right.load(Ordering::Relaxed),
    ];
    let available = if start >= valid_frames {
        0
    } else {
        count.min(valid_frames - start)
    };
    let mut destinations = [ptr::null_mut::<f32>(); 2];
    let mut destination_ranges = [(0_usize, 0_usize); 2];
    for (index, buffer) in buffers.iter().enumerate() {
        let destination = buffer.data.cast::<f32>();
        if buffer.number_channels != 1
            || (buffer.data_byte_size as usize) < required
            || !buffer
                .data_byte_size
                .is_multiple_of(size_of::<f32>() as u32)
            || (count > 0 && (destination.is_null() || !destination.is_aligned()))
        {
            return AUDIO_PARAM_ERROR;
        }
        let begin = destination as usize;
        let Some(end) = begin.checked_add(required) else {
            return AUDIO_PARAM_ERROR;
        };
        destinations[index] = destination;
        destination_ranges[index] = (begin, end);
    }
    let overlaps = |a: (usize, usize), b: (usize, usize)| a.0 < b.1 && b.0 < a.1;
    if count > 0 && overlaps(destination_ranges[0], destination_ranges[1]) {
        return AUDIO_PARAM_ERROR;
    }
    // Protect entire retained source allocations, including off-window samples
    // and zero-fill requests: a later callback in this render may read them.
    for source in sources {
        if source.is_null() {
            continue;
        }
        let begin = source as usize;
        let Some(end) = capacity
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| begin.checked_add(bytes))
        else {
            return AUDIO_PARAM_ERROR;
        };
        if destination_ranges
            .iter()
            .any(|range| overlaps(*range, (begin, end)))
        {
            return AUDIO_PARAM_ERROR;
        }
    }
    if count == 0 {
        return 0;
    }
    for index in 0..2 {
        let source = sources[index];
        let copied = if source.is_null() { 0 } else { available };
        // SAFETY: all descriptor/channel/byte capacities and both destination
        // ranges were checked before writes. Host source planes retain capacity
        // throughout native render; source/destination ranges are disjoint and
        // admitted indices stay within that capacity. Remaining frames are zeroed.
        unsafe {
            if copied > 0 {
                ptr::copy_nonoverlapping(source.add(start), destinations[index], copied);
            }
            if copied < count {
                ptr::write_bytes(destinations[index].add(copied), 0, count - copied);
            }
        }
    }
    0
}

#[repr(C)]
struct MidiEventPacket {
    time_stamp: u64,
    word_count: u32,
    words: [u32; 8],
}

#[repr(C)]
struct MidiEventList {
    protocol: u32,
    num_packets: u32,
    packet: [MidiEventPacket; 1],
}

type MidiEventListFn = unsafe extern "C" fn(*mut c_void, u32, *const MidiEventList) -> i32;

/// `MusicDeviceMIDIEventList` is macOS 12+, so resolve it at runtime instead of
/// making the host refuse to launch on older systems.
fn music_device_midi_event_list() -> Option<MidiEventListFn> {
    static SYMBOL: OnceLock<Option<MidiEventListFn>> = OnceLock::new();
    *SYMBOL.get_or_init(|| {
        let name = c"MusicDeviceMIDIEventList";
        // SAFETY: RTLD_DEFAULT searches the process-loaded system frameworks;
        // this fixed SDK symbol name is terminated and stays live for the lookup.
        let pointer = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if pointer.is_null() {
            None
        } else {
            // SAFETY: this non-null system export has the SDK MusicDeviceMIDIEventList
            // ABI encoded by MidiEventListFn; AudioToolbox remains process-resident.
            Some(unsafe { std::mem::transmute::<*mut c_void, MidiEventListFn>(pointer) })
        }
    })
}

// Native component/parameter labels are bounded independently of plugin state.
const MAX_CF_STRING_UTF16_UNITS: usize = 4096;
const MAX_CF_STRING_UTF8_BYTES: usize = 16_385;

/// Decode a native label with at most 4096 UTF-16 units and a 16 KiB UTF-8
/// payload budget plus its C terminator. Embedded NUL is invalid.
#[cfg(test)]
unsafe fn cf_string_to_string(text: *mut c_void) -> Option<String> {
    // SAFETY: the caller retains the live native CFString through conversion.
    unsafe {
        cf_string_to_string_bounded(text, MAX_CF_STRING_UTF16_UNITS, MAX_CF_STRING_UTF8_BYTES)
    }
}

/// The caller retains a live CFString; capacity derives from one length read.
#[cfg(test)]
unsafe fn cf_string_to_string_bounded(
    text: *mut c_void,
    units: usize,
    bytes: usize,
) -> Option<String> {
    let foreign = au_guard::Boundary::default();
    // SAFETY: the test caller retains this CFString for the bounded native read.
    unsafe { cf_string_to_string_guarded(text, units, bytes, &foreign) }
        .ok()
        .flatten()
}

unsafe fn cf_string_to_string_guarded(
    text: *mut c_void,
    max_utf16_units: usize,
    max_utf8_bytes: usize,
    foreign: &au_guard::Boundary,
) -> Result<Option<String>, AuForeignError> {
    if text.is_null() || max_utf8_bytes == 0 {
        return Ok(None);
    }
    // SAFETY: the caller's ownership retains this CFString during the read.
    let native_length = unsafe { foreign.string_length(au_guard::StringLength { text })? };
    let Ok(length) = usize::try_from(native_length) else {
        return Ok(None);
    };
    if length > max_utf16_units {
        return Ok(None);
    }
    // SAFETY: this sizing operation uses only the captured nonnegative length.
    let maximum = unsafe {
        foreign.string_capacity(au_guard::StringCapacity {
            length: native_length,
            encoding: STRING_ENCODING_UTF8,
        })?
    };
    let Some(capacity) = usize::try_from(maximum)
        .ok()
        .and_then(|maximum| maximum.checked_add(1))
    else {
        return Ok(None);
    };
    if capacity > max_utf8_bytes {
        return Ok(None);
    }
    let Ok(native_capacity) = isize::try_from(capacity) else {
        return Ok(None);
    };
    let mut buffer = vec![0u8; capacity];
    // SAFETY: buffer owns exactly native_capacity writable bytes. A string
    // that grows beyond this captured bound fails conversion instead of growing
    // host storage; no native length is re-read to allocate another buffer.
    let converted = unsafe {
        foreign.string_c_string(au_guard::StringCString {
            text,
            buffer: buffer.as_mut_ptr().cast(),
            capacity: native_capacity,
            encoding: STRING_ENCODING_UTF8,
        })?
    };
    if converted == 0 {
        return Ok(None);
    }
    let Some(end) = buffer.iter().position(|byte| *byte == 0) else {
        return Ok(None);
    };
    buffer.truncate(end);
    let Ok(decoded) = String::from_utf8(buffer) else {
        return Ok(None);
    };
    // C-string conversion can hide embedded NULs. The decoded UTF-16 length
    // must equal the captured CFString length, so truncation is never an alias.
    Ok((decoded.encode_utf16().count() == length).then_some(decoded))
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(
        component: *mut c_void,
        description: *const AudioComponentDescription,
    ) -> *mut c_void;
    fn AudioComponentCopyName(component: *mut c_void, name: *mut *mut c_void) -> i32;
    fn AudioComponentGetDescription(
        component: *mut c_void,
        description: *mut AudioComponentDescription,
    ) -> i32;
    fn AudioComponentInstanceNew(component: *mut c_void, instance: *mut *mut c_void) -> i32;
    fn AudioComponentInstanceDispose(instance: *mut c_void) -> i32;
    fn AudioUnitInitialize(unit: *mut c_void) -> i32;
    fn AudioUnitUninitialize(unit: *mut c_void) -> i32;
    fn AudioUnitReset(unit: *mut c_void, scope: u32, element: u32) -> i32;
    fn AudioUnitSetProperty(
        unit: *mut c_void,
        property: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> i32;
    fn AudioUnitGetProperty(
        unit: *mut c_void,
        property: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioUnitGetPropertyInfo(
        unit: *mut c_void,
        property: u32,
        scope: u32,
        element: u32,
        size: *mut u32,
        writable: *mut u8,
    ) -> i32;
    fn AudioUnitRender(
        unit: *mut c_void,
        flags: *mut u32,
        timestamp: *const AudioTimeStamp,
        bus: u32,
        frames: u32,
        data: *mut AudioBufferList,
    ) -> i32;
    fn AudioUnitSetParameter(
        unit: *mut c_void,
        parameter: u32,
        scope: u32,
        element: u32,
        value: f32,
        offset: u32,
    ) -> i32;
    fn AudioUnitGetParameter(
        unit: *mut c_void,
        parameter: u32,
        scope: u32,
        element: u32,
        value: *mut f32,
    ) -> i32;
    fn MusicDeviceMIDIEvent(
        unit: *mut c_void,
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    #[cfg(test)]
    fn CFStringCreateWithCharacters(
        allocator: *const c_void,
        characters: *const u16,
        length: isize,
    ) -> *mut c_void;
    fn CFStringGetLength(text: *mut c_void) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        text: *mut c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *mut c_void;
    fn CFDataGetLength(data: *mut c_void) -> isize;
    fn CFDataGetBytePtr(data: *mut c_void) -> *const u8;
    fn CFPropertyListCreateData(
        allocator: *const c_void,
        plist: *mut c_void,
        format: isize,
        options: usize,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    fn CFPropertyListCreateWithData(
        allocator: *const c_void,
        data: *mut c_void,
        options: usize,
        format: *mut isize,
        error: *mut *mut c_void,
    ) -> *mut c_void;
    fn CFRelease(value: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NativeString(*mut c_void);
    impl NativeString {
        fn new(text: &str) -> Self {
            let units: Vec<u16> = text.encode_utf16().collect();
            // SAFETY: CF copies these UTF-16 units into its returned +1 string.
            let text = unsafe {
                CFStringCreateWithCharacters(ptr::null(), units.as_ptr(), units.len() as isize)
            };
            assert!(!text.is_null());
            Self(text)
        }
        fn decode(&self) -> Option<String> {
            // SAFETY: this guard retains the native CFString until it drops.
            unsafe { cf_string_to_string(self.0) }
        }
    }
    impl Drop for NativeString {
        fn drop(&mut self) {
            // SAFETY: NativeString owns exactly the CF creation reference.
            unsafe { CFRelease(self.0) };
        }
    }

    #[test]
    fn bounded_cf_labels_preserve_unicode_and_the_exact_utf16_boundary() {
        for text in ["", "Gain", "低频 🎹", "e\u{301}"] {
            let native = NativeString::new(text);
            assert_eq!(native.decode().as_deref(), Some(text));
        }
        let text = "A".repeat(MAX_CF_STRING_UTF16_UNITS);
        assert_eq!(
            NativeString::new(&text).decode().as_deref(),
            Some(text.as_str())
        );
        let piano = NativeString::new("🎹");
        assert!(
            // SAFETY: piano retains the real CFString through the bounded read;
            // the deliberately small length budget cannot release its ownership.
            unsafe { cf_string_to_string_bounded(piano.0, 1, MAX_CF_STRING_UTF8_BYTES) }.is_none()
        );
    }

    #[test]
    fn bounded_cf_labels_refuse_oversized_byte_budgets_and_embedded_nul() {
        let oversized = NativeString::new(&"A".repeat(MAX_CF_STRING_UTF16_UNITS + 1));
        assert!(oversized.decode().is_none());
        for text in ["before\0after", "\0", "after\0"] {
            assert!(NativeString::new(text).decode().is_none());
        }
        let native = NativeString::new("Low gain");
        assert!(
            // SAFETY: native owns the real CFString through this bounded query;
            // the deliberately small encoding budget affects admission, not its lifetime.
            unsafe { cf_string_to_string_bounded(native.0, MAX_CF_STRING_UTF16_UNITS, 1) }
                .is_none()
        );
    }

    use std::sync::Arc;

    const SAMPLE_RATE: u32 = 48_000;
    const MAX_FRAMES: u32 = 512;

    /// Load a unit or skip loudly: system AUs ship with macOS, but CI images and
    /// stripped installs are allowed to miss one.
    fn system_unit(query: &str) -> Option<AuInstance> {
        match AuInstance::load(query, SAMPLE_RATE, MAX_FRAMES) {
            Ok(instance) => Some(instance),
            Err(error) => {
                eprintln!("skipping: AudioUnit {query:?} is not available: {error}");
                None
            }
        }
    }

    #[test]
    fn host_callbacks_report_beat_tempo_meter_and_transport_state() {
        let project = oc_dsl::parse(
            "@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n  time_sig_map:\n    2:1 = 3/4\n",
        )
        .unwrap();
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let transport = PluginTransport::new(0, SAMPLE_RATE, 120.0).with_clock(Arc::new(clock));
        let segment = transport.at_sample(96_000);
        let state = HostClock::new();
        state.publish(Some(&segment), 96_000);

        let pointer = &state as *const HostClock as *mut c_void;
        let mut beat = 0.0;
        let mut tempo = 0.0;
        // SAFETY: pointer identifies this test's live HostClock and both f64
        // outputs are distinct writable stack scalars through the callback.
        let status = unsafe { host_beat_and_tempo(pointer, &mut beat, &mut tempo) };
        assert_eq!(status, 0);
        assert!((beat - 4.0).abs() < 1e-9, "beat {beat}");
        assert!((tempo - 60.0).abs() < 1e-9, "tempo {tempo}");

        let mut delta = 0;
        let mut numerator = 0.0;
        let mut denominator = 0;
        let mut downbeat = 0.0;
        // SAFETY: the HostClock and four correctly typed, distinct writable
        // stack outputs remain alive through this synchronous callback invocation.
        let status = unsafe {
            host_musical_time_location(
                pointer,
                &mut delta,
                &mut numerator,
                &mut denominator,
                &mut downbeat,
            )
        };
        assert_eq!(status, 0);
        assert_eq!((numerator, denominator), (3.0, 4));
        assert!((downbeat - 4.0).abs() < 1e-9, "downbeat {downbeat}");
        // 60 BPM with a quarter-note beat puts the next beat one second out.
        assert_eq!(delta, 48_000);

        let mut playing = 0;
        let mut changed = 0;
        let mut sample = 0.0;
        let mut cycling = 0;
        let mut cycle_start = 0.0;
        let mut cycle_end = 0.0;
        // SAFETY: pointer names the live atomic HostClock; all six optional
        // outputs here have their AU ABI scalar types and distinct writable storage.
        let status = unsafe {
            host_transport_state(
                pointer,
                &mut playing,
                &mut changed,
                &mut sample,
                &mut cycling,
                &mut cycle_start,
                &mut cycle_end,
            )
        };
        assert_eq!(status, 0);
        assert_eq!(playing, 1);
        assert_eq!(changed, 0);
        assert_eq!(sample, 96_000.0);
        assert_eq!(cycling, 0);
    }

    #[test]
    fn host_callbacks_reject_a_missing_client_pointer() {
        let mut beat = 0.0;
        let mut tempo = 0.0;
        assert_eq!(
            // SAFETY: null user data exercises the documented early refusal;
            // both provided scalar outputs are valid and remain untouched.
            unsafe { host_beat_and_tempo(ptr::null_mut(), &mut beat, &mut tempo) },
            CANNOT_DO_IN_CURRENT_CONTEXT
        );
    }

    #[test]
    fn system_instrument_accepts_host_callbacks() {
        let Some(instance) = system_unit("Apple: DLSMusicDevice") else {
            return;
        };
        assert!(
            instance.host_callbacks_installed(),
            "DLSMusicDevice rejected kAudioUnitProperty_HostCallbacks"
        );
        assert!(
            instance.component_name().contains("DLSMusicDevice"),
            "unexpected component name {:?}",
            instance.component_name()
        );
    }

    fn rms(buffer: &[f32]) -> f64 {
        if buffer.is_empty() {
            return 0.0;
        }
        (buffer
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>()
            / buffer.len() as f64)
            .sqrt()
    }

    fn peak(buffer: &[f32]) -> f64 {
        buffer
            .iter()
            .map(|sample| f64::from(*sample).abs())
            .fold(0.0, f64::max)
    }

    fn tone(frames: usize, amplitude: f32) -> Vec<f32> {
        let mut buffer = vec![0.0; frames * 2];
        for frame in 0..frames {
            let value = (std::f64::consts::TAU * 440.0 * frame as f64 / f64::from(SAMPLE_RATE))
                .sin() as f32
                * amplitude;
            buffer[frame * 2] = value;
            buffer[frame * 2 + 1] = value;
        }
        buffer
    }

    #[test]
    fn ffi_layouts_match_audio_toolbox() {
        assert_eq!(size_of::<AudioTimeStamp>(), 64);
        assert_eq!(size_of::<AudioStreamBasicDescription>(), 40);
        assert_eq!(size_of::<AudioUnitParameterInfo>(), 104);
        assert_eq!(size_of::<AudioComponentDescription>(), 20);
        assert_eq!(size_of::<MidiEventList>(), 56);
    }

    #[test]
    fn display_name_codes_and_short_name_resolve_the_same_component() {
        let Ok(by_name) = resolve_component("Apple: DLSMusicDevice") else {
            eprintln!("skipping: 'Apple: DLSMusicDevice' is not registered");
            return;
        };
        assert_eq!(
            resolve_component("au:aumu:dls:appl").expect("trimmed four-character codes"),
            by_name
        );
        assert_eq!(
            resolve_component("au:aumu:dls :appl").expect("padded four-character codes"),
            by_name
        );
        assert_eq!(
            resolve_component("DLSMusicDevice").expect("short name"),
            by_name
        );
        assert_eq!(by_name.codes(), "aumu:dls:appl");
        assert_eq!(category_of(&by_name), AuCategory::Instrument);
    }

    #[test]
    fn component_bundle_path_resolves_through_its_info_plist() {
        let directory = Path::new("/Library/Audio/Plug-Ins/Components");
        let bundle = std::fs::read_dir(directory).ok().and_then(|entries| {
            entries.flatten().map(|entry| entry.path()).find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "component")
            })
        });
        let Some(bundle) = bundle else {
            eprintln!("skipping: no third-party .component bundle is installed");
            return;
        };
        let description = resolve_component(&bundle.to_string_lossy()).expect("bundle resolves");
        assert_ne!(category_of(&description), AuCategory::Unsupported);
        assert_eq!(
            resolve_component(&bundle.to_string_lossy()).expect("stable"),
            description
        );
    }

    #[test]
    fn unknown_component_id_is_a_typed_not_found_error() {
        let error = AuInstance::load("au:aumu:zzzz:zzzz", SAMPLE_RATE, MAX_FRAMES)
            .err()
            .expect("unknown four-character codes must not instantiate");
        assert_eq!(error.kind, AuErrorKind::NotFound);
        assert!(error.to_string().contains("zzzz:zzzz"), "{error}");
        assert_eq!(error.status, None);

        let error = resolve_component("Apple: No Such Instrument")
            .expect_err("unknown display name must not resolve");
        assert_eq!(error.kind, AuErrorKind::NotFound);
    }

    #[test]
    fn generator_units_are_reported_as_unhostable_with_their_type_code() {
        let generator = registered_components()
            .expect("registered AU metadata is readable")
            .iter()
            .find(|component| component.description.component_type == u32::from_be_bytes(*b"augn"))
            .map(|component| component.name.clone());
        let Some(query) = generator else {
            eprintln!("skipping: no generator AudioUnit is registered");
            return;
        };
        let error = AuInstance::load(&query, SAMPLE_RATE, MAX_FRAMES)
            .err()
            .expect("generator units need host wiring this renderer does not provide");
        assert_eq!(error.kind, AuErrorKind::UnsupportedType);
        assert!(error.to_string().contains("augn"), "{error}");
    }

    #[test]
    fn a_system_unit_reports_a_panel_surface_with_its_own_defaults_and_units() {
        let Some(instance) = system_unit("Apple: AUDynamicsProcessor") else {
            return;
        };
        let surface = instance.try_parameter_surface().unwrap();
        let params = surface.parameters();
        assert!(!params.is_empty(), "{surface:?}");
        assert_eq!(params[0].index, 0);
        assert!(params.iter().all(|param| !param.name.is_empty()));
        assert!(
            params.iter().all(|param| param.default.is_some()),
            "an AudioUnit reports a default for every parameter"
        );
        assert!(params.iter().all(|param| {
            (param.normalized - crate::params::normalize(param.current, param.min, param.max)).abs()
                < 1e-9
        }));
        // The unit's own symbol, from AudioUnitParameterUnit.
        assert!(
            params.iter().any(|param| !param.unit.is_empty()),
            "expected at least one parameter with a standard unit: {params:#?}"
        );
        assert!(params.iter().any(|param| param.automatable));
    }

    #[test]
    fn system_instrument_renders_non_silent_audio_for_n_frames() {
        let Some(mut instance) = system_unit("Apple: DLSMusicDevice") else {
            return;
        };
        assert!(instance.is_instrument());
        assert_eq!(instance.output_count(), 1);
        instance.program_change(0, 0, 0);
        instance.note_on_channel(0, 60, 0.9, 0);
        let mut rendered = Vec::new();
        for _ in 0..24 {
            rendered.extend(instance.process_block_with_transport(256, None));
        }
        assert_eq!(rendered.len(), 256 * 24 * 2);
        let level = rms(&rendered);
        assert!(
            level > 1e-3 && peak(&rendered) > 1e-2,
            "DLSMusicDevice rendered {level:.6} RMS; expected a note"
        );
        instance.note_off_channel(0, 60, 0);
        let _ = instance.process_block_with_transport(256, None);
    }

    #[test]
    fn system_effect_processes_input_and_reports_tail() {
        let Some(mut instance) = system_unit("Apple: AUReverb2") else {
            return;
        };
        assert_eq!(instance.category(), AuCategory::Effect);
        assert!(
            instance.tail_samples() > 0,
            "AUReverb2 reports no {:.3}s tail",
            f64::from(instance.tail_samples()) / f64::from(SAMPLE_RATE)
        );
        let mut buffer = tone(512, 0.25);
        let input_level = rms(&buffer);
        instance.process_interleaved(&mut buffer);
        let output_level = rms(&buffer);
        assert!(output_level > 1e-4, "reverb returned silence");
        assert!(
            (output_level - input_level).abs() > 1e-6,
            "reverb passed the dry input unchanged ({input_level:.6})"
        );
    }

    #[test]
    fn effect_parameters_round_trip_by_name() {
        let Some(mut instance) = system_unit("Apple: AUDynamicsProcessor") else {
            return;
        };
        let params = instance.get_params();
        let writable = params.iter().find(|param| param.writable);
        let Some(param) = writable else {
            eprintln!("skipping: AUDynamicsProcessor exposes no writable parameter");
            return;
        };
        let target = if (param.max - param.min).abs() > f64::EPSILON {
            param.min + (param.max - param.min) * 0.25
        } else {
            param.min
        };
        assert!(
            instance.set_param_by_name(&param.name, target),
            "could not set '{}'",
            param.name
        );
        let read_back = instance
            .get_param_by_name(&param.name)
            .expect("parameter value");
        assert!(
            (read_back - target).abs() <= (param.max - param.min).abs() * 1e-2,
            "{read_back} vs {target}"
        );
    }

    /// The offline renderer probes an instrument's tail on one worker while
    /// another track renders an effect, so units are created, rendered and
    /// torn down from several threads at once. Every cycle here mirrors that
    /// shape. Control transitions serialize while independent prepared render
    /// calls remain free to overlap them; no output device is opened.
    #[test]
    fn concurrent_lifecycle_and_render_cycles_stay_consistent() {
        for query in ["Apple: DLSMusicDevice", "Apple: AUReverb2"] {
            if resolve_component(query).is_err() {
                eprintln!("skipping: AudioUnit {query:?} is not registered");
                return;
            }
        }
        let workers = 8;
        let cycles = 25;
        std::thread::scope(|scope| {
            for worker in 0..workers {
                scope.spawn(move || {
                    for cycle in 0..cycles {
                        if (worker + cycle) % 2 == 0 {
                            let mut instrument =
                                AuInstance::load("Apple: DLSMusicDevice", SAMPLE_RATE, MAX_FRAMES)
                                    .expect("registered system instrument must initialize");
                            instrument.note_on_channel(0, 60, 0.9, 0);
                            for _ in 0..4 {
                                let mut block = [0.0; 512];
                                instrument
                                    .process_block_into(&mut block, None)
                                    .expect("system instrument render");
                                assert!(block.iter().all(|sample| sample.is_finite()));
                            }
                        } else {
                            let mut effect =
                                AuInstance::load("Apple: AUReverb2", SAMPLE_RATE, MAX_FRAMES)
                                    .expect("registered system effect must initialize");
                            let mut buffer = tone(512, 0.1);
                            effect
                                .process_effect_into(&mut buffer, SAMPLE_RATE, None)
                                .expect("system effect render");
                            assert!(buffer.iter().all(|sample| sample.is_finite()));
                        }
                    }
                });
            }
        });
    }

    /// Capturing and restoring `ClassInfo` is a property write like any other:
    /// it must stay valid while other threads render and cycle units. A busy
    /// suite used to make the restore fail with a framework error.
    #[test]
    fn concurrent_state_restore_stays_valid() {
        if resolve_component("Apple: DLSMusicDevice").is_err() {
            eprintln!("skipping: 'Apple: DLSMusicDevice' is not registered");
            return;
        }
        std::thread::scope(|scope| {
            for worker in 0..8_u32 {
                scope.spawn(move || {
                    for cycle in 0..16_u32 {
                        let mut instance =
                            AuInstance::load("Apple: DLSMusicDevice", SAMPLE_RATE, MAX_FRAMES)
                                .expect("registered system unit must initialize");
                        if (worker + cycle) % 2 == 0 {
                            let state = instance.state_bytes().expect("class info");
                            instance
                                .set_state_bytes(&state)
                                .expect("class info restores");
                        } else {
                            instance.note_on_channel(0, 60, 0.9, 0);
                            let mut block = [0.0; 512];
                            instance
                                .process_block_into(&mut block, None)
                                .expect("concurrent system render");
                        }
                    }
                });
            }
        });
    }

    /// Real system components render into memory only. The held host control
    /// mutex is a deterministic exclusion probe, not a throughput benchmark.
    #[test]
    fn system_prepared_au_processing_proceeds_while_control_mutex_is_held() {
        use std::sync::mpsc;
        use std::time::Duration;
        let queries = ["Apple: DLSMusicDevice", "Apple: AUReverb2"];
        for query in queries {
            match resolve_component(query) {
                Ok(_) => {}
                Err(error) if error.kind == AuErrorKind::NotFound => {
                    eprintln!("SKIP system AU concurrency: {query} is not registered");
                    return;
                }
                Err(error) => panic!("system AU discovery failed: {error}"),
            }
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let completed = std::thread::scope(|scope| {
            let mut starts = Vec::new();
            for (index, query) in queries.into_iter().enumerate() {
                let ready = ready_tx.clone();
                let done = done_tx.clone();
                let (start_tx, start_rx) = mpsc::channel();
                starts.push(start_tx);
                scope.spawn(move || {
                    let mut unit = AuInstance::load(query, SAMPLE_RATE, MAX_FRAMES)
                        .expect("registered system AU must initialize");
                    let parameter = unit
                        .params
                        .iter()
                        .find(|param| param.writable)
                        .map(|param| (param.name.clone(), param.default));
                    if index == 1 {
                        assert!(
                            parameter.is_some(),
                            "system effect must expose a writable control"
                        );
                    }
                    let project = oc_dsl::parse("@project\n  tempo: 127\n").unwrap();
                    let transport = PluginTransport::new(0, SAMPLE_RATE, 127.0).with_clock(
                        Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap()),
                    );
                    let mut output = [0.0; 512];
                    let input: [f32; 512] =
                        std::array::from_fn(|sample| ((sample / 2) as f32 * 0.02).sin() * 0.1);
                    if index == 0 {
                        unit.note_on_channel(0, 60, 0.8, 0);
                    }
                    ready.send(()).unwrap();
                    if start_rx.recv().is_err() {
                        return;
                    }
                    let mut process = Ok(());
                    let mut reset = Ok(());
                    let mut controls = true;
                    let mut finite = true;
                    let mut audible = false;
                    let heap = oc_test_support::operations(|| {
                        if let Some((name, value)) = &parameter {
                            controls = unit.set_param_by_name(name, *value)
                                && unit.get_param_by_name(name).is_some();
                        }
                        for block in 0..8 {
                            let at = transport.at_sample(block * 256);
                            if index == 1 {
                                output = input;
                                process =
                                    unit.process_effect_into(&mut output, SAMPLE_RATE, Some(&at));
                            } else {
                                process = unit.process_block_into(&mut output, Some(&at));
                            }
                            if process.is_err() {
                                break;
                            }
                            finite &= output.iter().all(|sample| sample.is_finite());
                            audible |= output.iter().any(|sample| sample.abs() > 1e-8);
                        }
                        if process.is_ok() {
                            reset = unit.reset_prepared();
                        }
                    });
                    done.send((controls, process, reset, heap, finite, audible))
                        .unwrap();
                    // Native retirement may wait for the control mutex after
                    // reporting progress; the parent releases it before joining.
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
            let (controls, process, reset, heap, finite, audible) =
                result.expect("system prepared processing blocked on the control mutex");
            assert!(controls);
            process.unwrap();
            reset.unwrap();
            assert_eq!(heap, 0, "host-side Rust heap operations");
            assert!(
                finite && audible,
                "system AU must return finite, non-silent PCM"
            );
        }
    }

    #[test]
    fn class_info_state_round_trips_through_property_list_bytes() {
        let Some(mut instance) = system_unit("Apple: DLSMusicDevice") else {
            return;
        };
        let state = instance.state_bytes().expect("class info");
        assert!(!state.is_empty());
        plist::Value::from_reader(std::io::Cursor::new(&state))
            .expect("ClassInfo serializes as a property list");
        instance
            .set_state_bytes(&state)
            .expect("class info restores");

        let error = instance
            .set_state_bytes(b"not a property list")
            .expect_err("garbage state must be rejected");
        assert_eq!(error.kind, AuErrorKind::StateFailed);
    }
}

#[cfg(test)]
mod prepared_state_tests {
    use super::*;

    #[test]
    fn prepared_au_refusal_preserves_queued_event_and_native_timeline() {
        let mut unit =
            AuInstance::load("Apple: DLSMusicDevice", 48_000, 128).expect("system DLS unit");
        unit.note_on_channel(0, 60, 0.9, 31);
        let mut output = [9.0; 256];
        assert_eq!(unit.pending.len(), 1);
        assert_eq!(
            unit.process_block_into(&mut output[..2], None),
            Err(AuProcessError::EventOffsetOutOfRange)
        );
        assert_eq!(unit.pending.len(), 1);
        assert_eq!(unit.rendered_frames, 0);
        assert_eq!(unit.host_clock.sample_pos.load(Ordering::Relaxed), 0);
        assert_eq!(output, [9.0; 256]);
        unit.prepare_block_buffers().unwrap();
        unit.process_block_into(&mut output, None).unwrap();
        assert!(unit.pending.is_empty());
        assert_eq!(unit.rendered_frames, 128);
        let capacity = unit.pending.capacity();
        unit.reset_prepared().unwrap();
        assert_eq!(unit.rendered_frames, 0);
        assert_eq!(unit.pending.capacity(), capacity);
    }
}

#[cfg(test)]
mod render_callback_tests {
    use super::*;

    const PARAM_ERROR: i32 = -50;
    fn state(left: &[f32], right: &[f32], block_start: i64) -> InputState {
        assert_eq!(left.len(), right.len());
        InputState {
            left: AtomicPtr::new(left.as_ptr().cast_mut()),
            right: AtomicPtr::new(right.as_ptr().cast_mut()),
            block_start: AtomicI64::new(block_start),
            capacity: AtomicUsize::new(left.len()),
            valid_frames: AtomicUsize::new(left.len()),
        }
    }
    fn planes(left: &mut [f32], right: &mut [f32]) -> AudioBufferList {
        AudioBufferList {
            number_buffers: 2,
            buffers: [
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: std::mem::size_of_val(left) as u32,
                    data: left.as_mut_ptr().cast(),
                },
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: std::mem::size_of_val(right) as u32,
                    data: right.as_mut_ptr().cast(),
                },
            ],
        }
    }
    fn invoke(
        state: &InputState,
        list: *mut AudioBufferList,
        frames: u32,
        timestamp: &AudioTimeStamp,
    ) -> i32 {
        // SAFETY: each test owns the state/source planes, the advertised native
        // descriptors and timestamp through this synchronous production callback.
        // Malformed counts are rejected from their actual readable header alone.
        unsafe {
            render_callback(
                (state as *const InputState).cast_mut().cast(),
                ptr::null_mut(),
                timestamp,
                0,
                frames,
                list,
            )
        }
    }

    #[test]
    fn actual_input_callback_rejects_a_native_one_plane_allocation_before_descriptor_access() {
        #[repr(C)]
        struct OnePlane {
            number_buffers: u32,
            buffers: [AudioBuffer; 1],
        }
        let source_left = [1.0; 4];
        let source_right = [2.0; 4];
        let state = state(&source_left, &source_right, 0);
        let mut sentinel = [81.0; 4];
        let mut native = Box::new(OnePlane {
            number_buffers: 1,
            buffers: [AudioBuffer {
                number_channels: 1,
                data_byte_size: 16,
                data: sentinel.as_mut_ptr().cast(),
            }],
        });
        let pointer = (&mut *native as *mut OnePlane).cast::<AudioBufferList>();
        assert_eq!(
            invoke(&state, pointer, 4, &AudioTimeStamp::at_sample(0.0)),
            PARAM_ERROR
        );
        assert_eq!(sentinel, [81.0; 4]);
    }

    #[test]
    fn actual_input_callback_checks_every_plane_before_writing_any_pcm() {
        let source_left = [1.0; 4];
        let source_right = [2.0; 4];
        let state = state(&source_left, &source_right, 0);
        for malformed in 0..7 {
            let mut left = [81.0; 6];
            let mut right = [82.0; 6];
            let mut list = planes(&mut left[1..5], &mut right[1..5]);
            let frames = if malformed == 0 { 5 } else { 4 };
            match malformed {
                1 => list.buffers[1].data_byte_size = 12,
                2 => list.buffers[1].number_channels = 2,
                3 => list.buffers[1].data = ptr::null_mut(),
                4 => list.number_buffers = 3,
                5 => list.buffers[1].data = list.buffers[0].data,
                6 => list.buffers[1].data = source_left.as_ptr().cast_mut().cast(),
                _ => {}
            }
            assert_eq!(
                invoke(&state, &mut list, frames, &AudioTimeStamp::at_sample(0.0)),
                PARAM_ERROR
            );
            assert_eq!(left, [81.0; 6], "case {malformed}");
            assert_eq!(right, [82.0; 6], "case {malformed}");
            assert_eq!(source_left, [1.0; 4]);
        }
    }

    #[test]
    fn actual_input_callback_refuses_nonfinite_and_overflowing_native_timestamps() {
        let source_left = [1.0; 4];
        let source_right = [2.0; 4];
        let state = state(&source_left, &source_right, 1);
        for time in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -(i64::MIN as f64),
            i64::MIN as f64,
        ] {
            let mut left = [81.0; 4];
            let mut right = [82.0; 4];
            let mut list = planes(&mut left, &mut right);
            assert_eq!(
                invoke(&state, &mut list, 4, &AudioTimeStamp::at_sample(time)),
                PARAM_ERROR
            );
            assert_eq!(left, [81.0; 4]);
            assert_eq!(right, [82.0; 4]);
        }
    }

    #[test]
    fn actual_input_callback_preserves_offset_pcm_and_zero_fill_without_heap_operations() {
        let source_left = [1.0, 2.0, 3.0, 4.0];
        let source_right = [11.0, 12.0, 13.0, 14.0];
        let state = state(&source_left, &source_right, 100);
        let mut left = [81.0; 6];
        let mut right = [82.0; 6];
        let mut list = planes(&mut left[1..5], &mut right[1..5]);
        let timestamp = AudioTimeStamp::at_sample(102.0);
        let mut status = PARAM_ERROR;
        let heap = oc_test_support::operations(|| {
            status = invoke(&state, &mut list, 4, &timestamp);
        });
        assert_eq!(status, 0);
        assert_eq!(left, [81.0, 3.0, 4.0, 0.0, 0.0, 81.0]);
        assert_eq!(right, [82.0, 13.0, 14.0, 0.0, 0.0, 82.0]);
        assert_eq!(heap, 0);
    }

    #[test]
    fn actual_input_callback_zeros_previous_block_tail_beyond_current_valid_frames() {
        let source_left = [1.0, 2.0, 103.0, 104.0];
        let source_right = [11.0, 12.0, 113.0, 114.0];
        let state = state(&source_left, &source_right, 100);
        state.valid_frames.store(2, Ordering::Release);
        let mut left = [81.0; 4];
        let mut right = [82.0; 4];
        let mut list = planes(&mut left, &mut right);
        assert_eq!(
            invoke(&state, &mut list, 4, &AudioTimeStamp::at_sample(100.0)),
            0
        );
        assert_eq!(left, [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(right, [11.0, 12.0, 0.0, 0.0]);
        assert_eq!(state.capacity.load(Ordering::Relaxed), 4);
        state.valid_frames.store(0, Ordering::Release);
        assert_eq!(
            invoke(&state, &mut list, 4, &AudioTimeStamp::at_sample(100.0)),
            0
        );
        assert_eq!(left, [0.0; 4]);
        assert_eq!(right, [0.0; 4]);
        assert_eq!(state.capacity.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn actual_input_callback_refuses_negative_offsets_and_invalid_sample_time_flags() {
        let source_left = [1.0; 4];
        let source_right = [2.0; 4];
        let state = state(&source_left, &source_right, 100);
        let mut invalid_flags = AudioTimeStamp::at_sample(100.0);
        invalid_flags.flags = 0;
        for timestamp in [
            AudioTimeStamp::at_sample(99.0),
            AudioTimeStamp::at_sample(99.75),
            invalid_flags,
        ] {
            let mut left = [81.0; 4];
            let mut right = [82.0; 4];
            let mut list = planes(&mut left, &mut right);
            assert_eq!(invoke(&state, &mut list, 4, &timestamp), PARAM_ERROR);
            assert_eq!(left, [81.0; 4]);
            assert_eq!(right, [82.0; 4]);
        }
    }

    #[test]
    fn actual_input_callback_protects_entire_source_allocations_even_outside_the_read_window() {
        let source_left = [1.0, 2.0, 3.0, 4.0];
        let source_right = [11.0, 12.0, 13.0, 14.0];
        let state = state(&source_left, &source_right, 100);
        // Disjoint from this read window; zero-fill has no read window; and
        // outside the current valid block but still inside its retained allocation.
        for (time, valid, offset) in [(102.0, 4, 0), (104.0, 4, 0), (100.0, 2, 2)] {
            state.valid_frames.store(valid, Ordering::Release);
            let mut left = [81.0; 2];
            let mut right = [82.0; 2];
            let mut list = planes(&mut left, &mut right);
            list.buffers[1].data = source_left[offset..].as_ptr().cast_mut().cast();
            assert_eq!(
                invoke(&state, &mut list, 2, &AudioTimeStamp::at_sample(time)),
                PARAM_ERROR
            );
            assert_eq!(left, [81.0; 2]);
            assert_eq!(right, [82.0; 2]);
            assert_eq!(source_left, [1.0, 2.0, 3.0, 4.0]);
            assert_eq!(source_right, [11.0, 12.0, 13.0, 14.0]);
        }
    }
}
