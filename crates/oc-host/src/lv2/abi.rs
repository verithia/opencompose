//! Minimal LV2 C ABI declarations.
//!
//! Only the core descriptor, atom sequence, bounded block length, URID and
//! state interfaces are declared. Everything is `repr(C)` and matches the
//! published ABI; no external header or library is linked.

use std::ffi::{c_char, c_void};

/// Opaque LV2 plugin handle.
pub type Lv2Handle = *mut c_void;
/// Interned LV2 URI identifier.
pub type Lv2Urid = u32;

#[repr(C)]
/// One LV2 feature.
pub struct Feature {
    pub uri: *const c_char,
    pub data: *mut c_void,
}

/// LV2 instantiate entry point.
pub type InstantiateFn = unsafe extern "C" fn(
    descriptor: *const Descriptor,
    sample_rate: f64,
    bundle_path: *const c_char,
    features: *const *const Feature,
) -> Lv2Handle;
/// LV2 port connection entry point.
pub type ConnectPortFn = unsafe extern "C" fn(Lv2Handle, u32, *mut c_void);
/// LV2 run entry point.
pub type RunFn = unsafe extern "C" fn(Lv2Handle, u32);
/// LV2 entry point without arguments.
pub type SimpleFn = unsafe extern "C" fn(Lv2Handle);
/// LV2 extension-data entry point.
pub type ExtensionDataFn = unsafe extern "C" fn(*const c_char) -> *const c_void;

#[repr(C)]
/// LV2 plugin descriptor.
pub struct Descriptor {
    pub uri: *const c_char,
    pub instantiate: Option<InstantiateFn>,
    pub connect_port: Option<ConnectPortFn>,
    pub activate: Option<SimpleFn>,
    pub run: Option<RunFn>,
    pub deactivate: Option<SimpleFn>,
    pub cleanup: Option<SimpleFn>,
    pub extension_data: Option<ExtensionDataFn>,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// LV2 atom header.
pub struct Atom {
    pub size: u32,
    pub type_: Lv2Urid,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// LV2 atom sequence body header.
pub struct AtomSequenceBody {
    pub unit: Lv2Urid,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// LV2 atom sequence header.
pub struct AtomSequence {
    pub atom: Atom,
    pub body: AtomSequenceBody,
}

/// Header of one sequence event. The atom payload follows this header inline.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AtomEvent {
    pub frames: i64,
    pub body: Atom,
}

/// Size of the atom sequence header.
pub const ATOM_SEQUENCE_HEADER_SIZE: usize = std::mem::size_of::<AtomSequence>();
/// Size of one atom event header.
pub const ATOM_EVENT_HEADER_SIZE: usize = std::mem::size_of::<AtomEvent>();

#[repr(C)]
/// LV2 URID map feature.
pub struct UridMap {
    pub handle: *mut c_void,
    pub map: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> Lv2Urid>,
}

#[repr(C)]
/// LV2 URID unmap feature.
pub struct UridUnmap {
    pub handle: *mut c_void,
    pub unmap: Option<unsafe extern "C" fn(*mut c_void, Lv2Urid) -> *const c_char>,
}

#[repr(C)]
/// LV2 bounded-block-length feature.
pub struct BoundedBlockLength {
    pub min_block_length: u32,
    pub max_block_length: u32,
}

/// LV2 state operation status.
pub type StateStatus = u32;
/// State status: success.
pub const STATE_SUCCESS: StateStatus = 0;
/// State status: the store has no space.
pub const STATE_NO_SPACE: StateStatus = 1;
/// State status: the operation is not implemented.
pub const STATE_NOT_IMPLEMENTED: StateStatus = 10;

/// State flag: the value is plain old data.
pub const STATE_FLAG_IS_POD: u32 = 1 << 0;
/// State flag: the value is portable.
pub const STATE_FLAG_IS_PORTABLE: u32 = 1 << 1;
/// State flag: the value is native.
pub const STATE_FLAG_IS_NATIVE: u32 = 1 << 2;

/// LV2 state store callback.
pub type StateStoreFn = unsafe extern "C" fn(
    handle: *mut c_void,
    key: Lv2Urid,
    value: *const c_void,
    size: usize,
    type_: Lv2Urid,
    flags: u32,
) -> StateStatus;
/// LV2 state retrieve callback.
pub type StateRetrieveFn = unsafe extern "C" fn(
    handle: *mut c_void,
    key: Lv2Urid,
    size: *mut usize,
    type_: *mut Lv2Urid,
    flags: *mut u32,
) -> *const c_void;

#[repr(C)]
/// LV2 state interface.
pub struct StateInterface {
    pub save: Option<
        unsafe extern "C" fn(
            handle: Lv2Handle,
            store: StateStoreFn,
            store_handle: *mut c_void,
            flags: u32,
            features: *const *const Feature,
        ) -> StateStatus,
    >,
    pub restore: Option<
        unsafe extern "C" fn(
            handle: Lv2Handle,
            retrieve: StateRetrieveFn,
            retrieve_handle: *mut c_void,
            flags: u32,
            features: *const *const Feature,
        ) -> StateStatus,
    >,
}

// Core, atom, MIDI, URID, state and bounded-block-length vocabulary.
/// LV2 core namespace URI for `plugin`.
pub const LV2__PLUGIN: &str = "http://lv2plug.in/ns/lv2core#Plugin";
/// LV2 core namespace URI for `instrument plugin`.
pub const LV2__INSTRUMENT_PLUGIN: &str = "http://lv2plug.in/ns/lv2core#InstrumentPlugin";
/// LV2 core namespace URI for `binary`.
pub const LV2__BINARY: &str = "http://lv2plug.in/ns/lv2core#binary";
/// LV2 core namespace URI for `required feature`.
pub const LV2__REQUIRED_FEATURE: &str = "http://lv2plug.in/ns/lv2core#requiredFeature";
/// LV2 core namespace URI for `optional feature`.
pub const LV2__OPTIONAL_FEATURE: &str = "http://lv2plug.in/ns/lv2core#optionalFeature";
/// LV2 core namespace URI for `port`.
pub const LV2__PORT: &str = "http://lv2plug.in/ns/lv2core#port";
/// LV2 core namespace URI for `index`.
pub const LV2__INDEX: &str = "http://lv2plug.in/ns/lv2core#index";
/// LV2 core namespace URI for `symbol`.
pub const LV2__SYMBOL: &str = "http://lv2plug.in/ns/lv2core#symbol";
/// LV2 core namespace URI for `name`.
pub const LV2__NAME: &str = "http://lv2plug.in/ns/lv2core#name";
/// LV2 core namespace URI for `minimum`.
pub const LV2__MINIMUM: &str = "http://lv2plug.in/ns/lv2core#minimum";
/// LV2 core namespace URI for `maximum`.
pub const LV2__MAXIMUM: &str = "http://lv2plug.in/ns/lv2core#maximum";
/// LV2 core namespace URI for `default`.
pub const LV2__DEFAULT: &str = "http://lv2plug.in/ns/lv2core#default";
/// LV2 core namespace URI for `designation`.
pub const LV2__DESIGNATION: &str = "http://lv2plug.in/ns/lv2core#designation";
/// LV2 core namespace URI for `latency`.
pub const LV2__LATENCY: &str = "http://lv2plug.in/ns/lv2core#latency";
/// LV2 core namespace URI for `enabled`.
pub const LV2__ENABLED: &str = "http://lv2plug.in/ns/lv2core#enabled";
/// LV2 core namespace URI for `sample rate`.
pub const LV2__SAMPLE_RATE: &str = "http://lv2plug.in/ns/lv2core#sampleRate";
/// LV2 core namespace URI for `audio port`.
pub const LV2__AUDIO_PORT: &str = "http://lv2plug.in/ns/lv2core#AudioPort";
/// LV2 core namespace URI for `control port`.
pub const LV2__CONTROL_PORT: &str = "http://lv2plug.in/ns/lv2core#ControlPort";
/// LV2 core namespace URI for `atom port`.
pub const LV2__ATOM_PORT: &str = "http://lv2plug.in/ns/lv2core#AtomPort";
/// LV2 core namespace URI for `cv port`.
pub const LV2__CV_PORT: &str = "http://lv2plug.in/ns/lv2core#CVPort";
/// LV2 core namespace URI for `input port`.
pub const LV2__INPUT_PORT: &str = "http://lv2plug.in/ns/lv2core#InputPort";
/// LV2 core namespace URI for `output port`.
pub const LV2__OUTPUT_PORT: &str = "http://lv2plug.in/ns/lv2core#OutputPort";
/// LV2 core namespace URI for `extension data`.
pub const LV2__EXTENSION_DATA: &str = "http://lv2plug.in/ns/lv2core#extensionData";
/// LV2 core namespace URI for `hard rt capable`.
pub const LV2__HARD_RT_CAPABLE: &str = "http://lv2plug.in/ns/lv2core#hardRTCapable";

/// LV2 atom namespace URI for `atom`.
pub const ATOM__ATOM: &str = "http://lv2plug.in/ns/ext/atom#Atom";
/// LV2 atom namespace URI for `sequence`.
pub const ATOM__SEQUENCE: &str = "http://lv2plug.in/ns/ext/atom#Sequence";
/// LV2 atom namespace URI for `supports`.
pub const ATOM__SUPPORTS: &str = "http://lv2plug.in/ns/ext/atom#supports";
/// LV2 atom namespace URI for `buffer type`.
pub const ATOM__BUFFER_TYPE: &str = "http://lv2plug.in/ns/ext/atom#bufferType";
/// LV2 atom namespace URI for `chunk`.
pub const ATOM__CHUNK: &str = "http://lv2plug.in/ns/ext/atom#Chunk";
/// LV2 atom namespace URI for `float`.
pub const ATOM__FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
/// LV2 atom namespace URI for `int`.
pub const ATOM__INT: &str = "http://lv2plug.in/ns/ext/atom#Int";
/// LV2 atom namespace URI for `double`.
pub const ATOM__DOUBLE: &str = "http://lv2plug.in/ns/ext/atom#Double";
/// LV2 atom namespace URI for `bool`.
pub const ATOM__BOOL: &str = "http://lv2plug.in/ns/ext/atom#Bool";
/// LV2 MIDI namespace URI for `midi event`.
pub const MIDI__MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";

/// LV2 state namespace URI for `interface`.
pub const STATE__INTERFACE: &str = "http://lv2plug.in/ns/ext/state#interface";

/// LV2 URID namespace URI for `map`.
pub const URID__MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
/// LV2 URID namespace URI for `unmap`.
pub const URID__UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
/// LV2 bounded-block-length namespace URI for `feature`.
pub const BOUNDED_BLOCK_LENGTH__FEATURE: &str =
    "http://lv2plug.in/ns/ext/bounded-block-length#boundedBlockLength";

/// LV2 worker namespace URI for `schedule`.
pub const WORKER__SCHEDULE: &str = "http://lv2plug.in/ns/ext/worker#schedule";
/// LV2 state namespace URI for `make path`.
pub const STATE__MAKE_PATH: &str = "http://lv2plug.in/ns/ext/state#makePath";
/// LV2 log namespace URI for `log`.
pub const LOG__LOG: &str = "http://lv2plug.in/ns/ext/log#log";
/// LV2 options namespace URI for `options`.
pub const OPTIONS__OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";

/// DOAP namespace URI for `name`.
pub const DOAP__NAME: &str = "http://usefulinc.com/ns/doap#name";
/// DOAP namespace URI for `license`.
pub const DOAP__LICENSE: &str = "http://usefulinc.com/ns/doap#license";
/// DOAP namespace URI for `maintainer`.
pub const DOAP__MAINTAINER: &str = "http://usefulinc.com/ns/doap#maintainer";
/// DOAP namespace URI for `developer`.
pub const DOAP__DEVELOPER: &str = "http://usefulinc.com/ns/doap#developer";
/// DOAP namespace URI for `project`.
pub const DOAP__PROJECT: &str = "http://usefulinc.com/ns/doap#project";
/// RDFS namespace URI for `label`.
pub const RDFS__LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// FOAF namespace URI for `name`.
pub const FOAF__NAME: &str = "http://xmlns.com/foaf/0.1/name";

// Port properties and units the parameter panel reads; a bundle that omits them
// still loads, it just reports no step count, unit or group.
/// LV2 core namespace URI for `port property`.
pub const LV2__PORT_PROPERTY: &str = "http://lv2plug.in/ns/lv2core#portProperty";
/// LV2 core namespace URI for `toggled`.
pub const LV2__TOGGLED: &str = "http://lv2plug.in/ns/lv2core#toggled";
/// LV2 core namespace URI for `integer`.
pub const LV2__INTEGER: &str = "http://lv2plug.in/ns/lv2core#integer";
/// LV2 core namespace URI for `enumeration`.
pub const LV2__ENUMERATION: &str = "http://lv2plug.in/ns/lv2core#enumeration";
/// LV2 core namespace URI for `scale point`.
pub const LV2__SCALE_POINT: &str = "http://lv2plug.in/ns/lv2core#scalePoint";
/// LV2 units namespace URI for `unit`.
pub const UNITS__UNIT: &str = "http://lv2plug.in/ns/ext/units#unit";
/// LV2 units namespace URI for `symbol`.
pub const UNITS__SYMBOL: &str = "http://lv2plug.in/ns/ext/units#symbol";
/// LV2 units namespace URI for `name`.
pub const UNITS__NAME: &str = "http://lv2plug.in/ns/ext/units#name";
/// LV2 port-groups namespace URI for `group`.
pub const PG__GROUP: &str = "http://lv2plug.in/ns/ext/port-groups#group";
