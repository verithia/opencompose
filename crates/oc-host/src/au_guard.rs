//! Typed AU calls catch foreign exceptions before returning into Rust.
use super::*;
use crate::native_fault::{FirstFault, RawFault};
use std::mem::MaybeUninit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of foreign exception caught at the AU boundary.
pub enum AuForeignFaultKind {
    Cpp,
    Unknown,
    ObjectiveC,
    Quarantined,
}

/// Fixed diagnostics are formatted only by the control-side caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuForeignError {
    pub operation: &'static str,
    pub kind: AuForeignFaultKind,
}
impl std::fmt::Display for AuForeignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} raised {:?}", self.operation, self.kind)
    }
}
impl std::error::Error for AuForeignError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A captured AU exception with fixed-size diagnostics.
pub struct AuForeignFault {
    pub error: AuForeignError,
    message: [u8; 256],
}
impl std::fmt::Display for AuForeignFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let end = self
            .message
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(256);
        write!(
            f,
            "{} raised {:?}: {}",
            self.error.operation,
            self.error.kind,
            String::from_utf8_lossy(&self.message[..end])
        )
    }
}
impl std::error::Error for AuForeignFault {}

#[derive(Clone, Copy)]
pub(super) struct Api {
    pub midi_list: Option<MidiEventListFn>,
    pub find_next:
        unsafe extern "C" fn(*mut c_void, *const AudioComponentDescription) -> *mut c_void,
    pub copy_name: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    pub description: unsafe extern "C" fn(*mut c_void, *mut AudioComponentDescription) -> i32,
    pub instance_new: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    pub dispose: unsafe extern "C" fn(*mut c_void) -> i32,
    pub initialize: unsafe extern "C" fn(*mut c_void) -> i32,
    pub uninitialize: unsafe extern "C" fn(*mut c_void) -> i32,
    pub reset: unsafe extern "C" fn(*mut c_void, u32, u32) -> i32,
    pub set_property: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32,
    pub get_property:
        unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32,
    pub property_info: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32,
    pub render: unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const AudioTimeStamp,
        u32,
        u32,
        *mut AudioBufferList,
    ) -> i32,
    pub set_parameter: unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32,
    pub get_parameter: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut f32) -> i32,
    pub midi: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> i32,
    pub string_length: unsafe extern "C" fn(*mut c_void) -> isize,
    pub string_capacity: unsafe extern "C" fn(isize, u32) -> isize,
    pub string_c_string: unsafe extern "C" fn(*mut c_void, *mut c_char, isize, u32) -> u8,
    pub data_create: unsafe extern "C" fn(*const c_void, *const u8, isize) -> *mut c_void,
    pub data_length: unsafe extern "C" fn(*mut c_void) -> isize,
    pub data_bytes: unsafe extern "C" fn(*mut c_void) -> *const u8,
    pub plist_data: unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        isize,
        usize,
        *mut *mut c_void,
    ) -> *mut c_void,
    pub plist_create: unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        usize,
        *mut isize,
        *mut *mut c_void,
    ) -> *mut c_void,
    pub release: unsafe extern "C" fn(*mut c_void),
}
impl Default for Api {
    fn default() -> Self {
        Self {
            midi_list: music_device_midi_event_list(),
            find_next: AudioComponentFindNext,
            copy_name: AudioComponentCopyName,
            description: AudioComponentGetDescription,
            instance_new: AudioComponentInstanceNew,
            dispose: AudioComponentInstanceDispose,
            initialize: AudioUnitInitialize,
            uninitialize: AudioUnitUninitialize,
            reset: AudioUnitReset,
            set_property: AudioUnitSetProperty,
            get_property: AudioUnitGetProperty,
            property_info: AudioUnitGetPropertyInfo,
            render: AudioUnitRender,
            set_parameter: AudioUnitSetParameter,
            get_parameter: AudioUnitGetParameter,
            midi: MusicDeviceMIDIEvent,
            string_length: CFStringGetLength,
            string_capacity: CFStringGetMaximumSizeForEncoding,
            string_c_string: CFStringGetCString,
            data_create: CFDataCreate,
            data_length: CFDataGetLength,
            data_bytes: CFDataGetBytePtr,
            plist_data: CFPropertyListCreateData,
            plist_create: CFPropertyListCreateWithData,
            release: CFRelease,
        }
    }
}

pub(super) struct Boundary {
    pub api: Api,
    fault: FirstFault<AuForeignFault>,
}
impl Default for Boundary {
    fn default() -> Self {
        Self::new(Api::default())
    }
}
impl Boundary {
    /// Build a guard around one AU function table.
    pub fn new(api: Api) -> Self {
        Self {
            api,
            fault: FirstFault::default(),
        }
    }
    /// The first foreign fault, if any.
    pub fn fault(&self) -> Option<AuForeignFault> {
        self.fault.first()
    }
    /// Take the fault for control-side reporting.
    pub fn take_report(&self) -> Option<AuForeignFault> {
        self.fault.take_report()
    }
    /// Whether a foreign fault was recorded.
    pub fn failed(&self) -> bool {
        self.fault.failed()
    }
    /// Refuse further calls after a quarantined fault.
    pub fn check(&self) -> Result<(), AuForeignError> {
        if self.failed() {
            Err(self
                .fault()
                .map(|fault| fault.error)
                .unwrap_or(AuForeignError {
                    operation: "AU native boundary",
                    kind: AuForeignFaultKind::Quarantined,
                }))
        } else {
            Ok(())
        }
    }
    /// Attach the captured fault to a control-side error.
    pub fn control_error(&self, error: AuForeignError) -> AuError {
        let mut result = AuError::from(error);
        result.attach_fault(self.fault());
        result
    }
    fn call<T>(
        &self,
        operation: &'static str,
        invoke: impl FnOnce(*mut T, *mut RawFault) -> bool,
    ) -> Result<T, AuForeignError> {
        if self.failed() {
            return Err(self
                .fault()
                .map(|fault| fault.error)
                .unwrap_or(AuForeignError {
                    operation,
                    kind: AuForeignFaultKind::Quarantined,
                }));
        }
        let mut result = MaybeUninit::uninit();
        let mut raw = RawFault {
            kind: 0,
            message: [0; 256],
        };
        if invoke(result.as_mut_ptr(), &mut raw) {
            // SAFETY: true publishes the complete typed output from the native wrapper.
            Ok(unsafe { result.assume_init() })
        } else {
            let fault = AuForeignFault {
                error: AuForeignError {
                    operation,
                    kind: match raw.kind {
                        1 => AuForeignFaultKind::Cpp,
                        3 => AuForeignFaultKind::ObjectiveC,
                        _ => AuForeignFaultKind::Unknown,
                    },
                },
                message: raw.message,
            };
            self.fault.record(fault);
            Err(self.fault().unwrap_or(fault).error)
        }
    }
    /// Find the next AudioComponent matching `args`; pointers must be valid.
    pub unsafe fn find_next(&self, args: FindNext) -> Result<*mut c_void, AuForeignError> {
        let native = NativeFindNext {
            call: self.api.find_next,
            args,
        };
        self.call("AudioComponentFindNext", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_find_next(&native, output, fault) }
        })
    }
    /// Copy the component name; out-pointers must be valid.
    pub unsafe fn copy_name(&self, args: CopyName) -> Result<i32, AuForeignError> {
        let native = NativeCopyName {
            call: self.api.copy_name,
            args,
        };
        self.call("AudioComponentCopyName", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_copy_name(&native, output, fault) }
        })
    }
    /// Read the component description; pointers must be valid.
    pub unsafe fn description(&self, args: Description) -> Result<i32, AuForeignError> {
        let native = NativeDescription {
            call: self.api.description,
            args,
        };
        self.call("AudioComponentGetDescription", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_description(&native, output, fault) }
        })
    }
    /// Instantiate the audio unit; out-pointers must be valid.
    pub unsafe fn instance_new(&self, args: InstanceNew) -> Result<i32, AuForeignError> {
        let native = NativeInstanceNew {
            call: self.api.instance_new,
            args,
        };
        self.call("AudioComponentInstanceNew", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_instance_new(&native, output, fault) }
        })
    }
    /// Dispose an audio unit instance; the handle must be live.
    pub unsafe fn dispose(&self, args: Dispose) -> Result<i32, AuForeignError> {
        let native = NativeDispose {
            call: self.api.dispose,
            args,
        };
        self.call("AudioComponentInstanceDispose", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_dispose(&native, output, fault) }
        })
    }
    /// Initialize the audio unit; the handle must be live.
    pub unsafe fn initialize(&self, args: Initialize) -> Result<i32, AuForeignError> {
        let native = NativeInitialize {
            call: self.api.initialize,
            args,
        };
        self.call("AudioUnitInitialize", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_initialize(&native, output, fault) }
        })
    }
    /// Uninitialize the audio unit; the handle must be live.
    pub unsafe fn uninitialize(&self, args: Uninitialize) -> Result<i32, AuForeignError> {
        let native = NativeUninitialize {
            call: self.api.uninitialize,
            args,
        };
        self.call("AudioUnitUninitialize", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_uninitialize(&native, output, fault) }
        })
    }
    /// Reset the audio unit render state; the handle must be live.
    pub unsafe fn reset(&self, args: Reset) -> Result<i32, AuForeignError> {
        let native = NativeReset {
            call: self.api.reset,
            args,
        };
        self.call("AudioUnitReset", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_reset(&native, output, fault) }
        })
    }
    /// Set a unit property; pointers must be valid for the call.
    pub unsafe fn set_property(&self, args: SetProperty) -> Result<i32, AuForeignError> {
        let native = NativeSetProperty {
            call: self.api.set_property,
            args,
        };
        self.call("AudioUnitSetProperty", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_set_property(&native, output, fault) }
        })
    }
    /// Read a unit property; pointers must be valid for the call.
    pub unsafe fn get_property(&self, args: GetProperty) -> Result<i32, AuForeignError> {
        let native = NativeGetProperty {
            call: self.api.get_property,
            args,
        };
        self.call("AudioUnitGetProperty", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_get_property(&native, output, fault) }
        })
    }
    /// Read a unit property with an explicit callback context.
    pub unsafe fn get_property_with(
        &self,
        call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32,
        args: GetProperty,
    ) -> Result<i32, AuForeignError> {
        let native = NativeGetProperty { call, args };
        self.call("AudioUnitGetProperty", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_get_property(&native, output, fault) }
        })
    }
    /// Read property metadata; pointers must be valid for the call.
    pub unsafe fn property_info(&self, args: PropertyInfo) -> Result<i32, AuForeignError> {
        let native = NativePropertyInfo {
            call: self.api.property_info,
            args,
        };
        self.call("AudioUnitGetPropertyInfo", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_property_info(&native, output, fault) }
        })
    }
    /// Read property metadata with an explicit callback context.
    pub unsafe fn property_info_with(
        &self,
        call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32,
        args: PropertyInfo,
    ) -> Result<i32, AuForeignError> {
        let native = NativePropertyInfo { call, args };
        self.call("AudioUnitGetPropertyInfo", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_property_info(&native, output, fault) }
        })
    }
    /// Render audio through the unit; buffers must be valid for the call.
    pub unsafe fn render(&self, args: Render) -> Result<i32, AuForeignError> {
        let native = NativeRender {
            call: self.api.render,
            args,
        };
        self.call("AudioUnitRender", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_render(&native, output, fault) }
        })
    }
    /// Set one unit parameter; the handle must be live.
    pub unsafe fn set_parameter(&self, args: SetParameter) -> Result<i32, AuForeignError> {
        let native = NativeSetParameter {
            call: self.api.set_parameter,
            args,
        };
        self.call("AudioUnitSetParameter", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_set_parameter(&native, output, fault) }
        })
    }
    /// Set one parameter with an explicit callback context.
    pub unsafe fn set_parameter_with(
        &self,
        call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32,
        args: SetParameter,
    ) -> Result<i32, AuForeignError> {
        let native = NativeSetParameter { call, args };
        self.call("AudioUnitSetParameter", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_set_parameter(&native, output, fault) }
        })
    }
    /// Read one unit parameter; out-pointers must be valid.
    pub unsafe fn get_parameter(&self, args: GetParameter) -> Result<i32, AuForeignError> {
        let native = NativeGetParameter {
            call: self.api.get_parameter,
            args,
        };
        self.call("AudioUnitGetParameter", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_get_parameter(&native, output, fault) }
        })
    }
    /// Send one MIDI event to the unit; the handle must be live.
    pub unsafe fn midi(&self, args: Midi) -> Result<i32, AuForeignError> {
        let native = NativeMidi {
            call: self.api.midi,
            args,
        };
        self.call("MusicDeviceMIDIEvent", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_midi(&native, output, fault) }
        })
    }
    /// Send a MIDI event list; event pointers must be valid.
    pub unsafe fn midi_list(
        &self,
        call: MidiEventListFn,
        args: MidiList,
    ) -> Result<i32, AuForeignError> {
        let native = NativeMidiList { call, args };
        self.call("MusicDeviceMIDIEventList", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_midi_list(&native, output, fault) }
        })
    }
    /// Query an AU string length; the handle must be live.
    pub unsafe fn string_length(&self, args: StringLength) -> Result<isize, AuForeignError> {
        let native = NativeStringLength {
            call: self.api.string_length,
            args,
        };
        self.call("CFStringGetLength", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_string_length(&native, output, fault) }
        })
    }
    /// Query an AU string capacity; the handle must be live.
    pub unsafe fn string_capacity(&self, args: StringCapacity) -> Result<isize, AuForeignError> {
        let native = NativeStringCapacity {
            call: self.api.string_capacity,
            args,
        };
        self.call("CFStringGetMaximumSizeForEncoding", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_string_capacity(&native, output, fault) }
        })
    }
    /// Copy an AU string as C characters; out-pointers must be valid.
    pub unsafe fn string_c_string(&self, args: StringCString) -> Result<u8, AuForeignError> {
        let native = NativeStringCString {
            call: self.api.string_c_string,
            args,
        };
        self.call("CFStringGetCString", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_string_c_string(&native, output, fault) }
        })
    }
    /// Create an AU data payload; input pointers must be valid.
    pub unsafe fn data_create(&self, args: DataCreate) -> Result<*mut c_void, AuForeignError> {
        let native = NativeDataCreate {
            call: self.api.data_create,
            args,
        };
        self.call("CFDataCreate", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_data_create(&native, output, fault) }
        })
    }
    /// Measure an AU data payload; the handle must be live.
    pub unsafe fn data_length(&self, args: DataLength) -> Result<isize, AuForeignError> {
        let native = NativeDataLength {
            call: self.api.data_length,
            args,
        };
        self.call("CFDataGetLength", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_data_length(&native, output, fault) }
        })
    }
    /// Read an AU data payload; the handle must be live.
    pub unsafe fn data_bytes(&self, args: DataBytes) -> Result<*const u8, AuForeignError> {
        let native = NativeDataBytes {
            call: self.api.data_bytes,
            args,
        };
        self.call("CFDataGetBytePtr", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_data_bytes(&native, output, fault) }
        })
    }
    /// Read a plist payload; out-pointers must be valid.
    pub unsafe fn plist_data(&self, args: PlistData) -> Result<*mut c_void, AuForeignError> {
        let native = NativePlistData {
            call: self.api.plist_data,
            args,
        };
        self.call("CFPropertyListCreateData", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_plist_data(&native, output, fault) }
        })
    }
    /// Create a plist payload; input pointers must be valid.
    pub unsafe fn plist_create(&self, args: PlistCreate) -> Result<*mut c_void, AuForeignError> {
        let native = NativePlistCreate {
            call: self.api.plist_create,
            args,
        };
        self.call("CFPropertyListCreateWithData", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_plist_create(&native, output, fault) }
        })
    }
    /// Release an AU object; the handle must be live.
    pub unsafe fn release(&self, args: Release) -> Result<(), AuForeignError> {
        let native = NativeRelease {
            call: self.api.release,
            args,
        };
        self.call::<u8>("CFRelease", |output, fault| {
            // SAFETY: caller retains the typed native arguments for this synchronous call.
            unsafe { oc_au_release(&native, output, fault) }
        })
        .map(|_| ())
    }
}

#[repr(C)]
pub(super) struct FindNext {
    pub component: *mut c_void,
    pub description: *const AudioComponentDescription,
}
#[repr(C)]
struct NativeFindNext {
    call: unsafe extern "C" fn(*mut c_void, *const AudioComponentDescription) -> *mut c_void,
    args: FindNext,
}
unsafe extern "C" {
    fn oc_au_find_next(
        args: *const NativeFindNext,
        output: *mut *mut c_void,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct CopyName {
    pub component: *mut c_void,
    pub name: *mut *mut c_void,
}
#[repr(C)]
struct NativeCopyName {
    call: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    args: CopyName,
}
unsafe extern "C" {
    fn oc_au_copy_name(args: *const NativeCopyName, output: *mut i32, fault: *mut RawFault)
    -> bool;
}

#[repr(C)]
pub(super) struct Description {
    pub component: *mut c_void,
    pub description: *mut AudioComponentDescription,
}
#[repr(C)]
struct NativeDescription {
    call: unsafe extern "C" fn(*mut c_void, *mut AudioComponentDescription) -> i32,
    args: Description,
}
unsafe extern "C" {
    fn oc_au_description(
        args: *const NativeDescription,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct InstanceNew {
    pub component: *mut c_void,
    pub instance: *mut *mut c_void,
}
#[repr(C)]
struct NativeInstanceNew {
    call: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    args: InstanceNew,
}
unsafe extern "C" {
    fn oc_au_instance_new(
        args: *const NativeInstanceNew,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Dispose {
    pub unit: *mut c_void,
}
#[repr(C)]
struct NativeDispose {
    call: unsafe extern "C" fn(*mut c_void) -> i32,
    args: Dispose,
}
unsafe extern "C" {
    fn oc_au_dispose(args: *const NativeDispose, output: *mut i32, fault: *mut RawFault) -> bool;
}

#[repr(C)]
pub(super) struct Initialize {
    pub unit: *mut c_void,
}
#[repr(C)]
struct NativeInitialize {
    call: unsafe extern "C" fn(*mut c_void) -> i32,
    args: Initialize,
}
unsafe extern "C" {
    fn oc_au_initialize(
        args: *const NativeInitialize,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Uninitialize {
    pub unit: *mut c_void,
}
#[repr(C)]
struct NativeUninitialize {
    call: unsafe extern "C" fn(*mut c_void) -> i32,
    args: Uninitialize,
}
unsafe extern "C" {
    fn oc_au_uninitialize(
        args: *const NativeUninitialize,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Reset {
    pub unit: *mut c_void,
    pub scope: u32,
    pub element: u32,
}
#[repr(C)]
struct NativeReset {
    call: unsafe extern "C" fn(*mut c_void, u32, u32) -> i32,
    args: Reset,
}
unsafe extern "C" {
    fn oc_au_reset(args: *const NativeReset, output: *mut i32, fault: *mut RawFault) -> bool;
}

#[repr(C)]
pub(super) struct SetProperty {
    pub unit: *mut c_void,
    pub property: u32,
    pub scope: u32,
    pub element: u32,
    pub data: *const c_void,
    pub size: u32,
}
#[repr(C)]
struct NativeSetProperty {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32,
    args: SetProperty,
}
unsafe extern "C" {
    fn oc_au_set_property(
        args: *const NativeSetProperty,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct GetProperty {
    pub unit: *mut c_void,
    pub property: u32,
    pub scope: u32,
    pub element: u32,
    pub data: *mut c_void,
    pub size: *mut u32,
}
#[repr(C)]
struct NativeGetProperty {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32,
    args: GetProperty,
}
unsafe extern "C" {
    fn oc_au_get_property(
        args: *const NativeGetProperty,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct PropertyInfo {
    pub unit: *mut c_void,
    pub property: u32,
    pub scope: u32,
    pub element: u32,
    pub size: *mut u32,
    pub writable: *mut u8,
}
#[repr(C)]
struct NativePropertyInfo {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32,
    args: PropertyInfo,
}
unsafe extern "C" {
    fn oc_au_property_info(
        args: *const NativePropertyInfo,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Render {
    pub unit: *mut c_void,
    pub flags: *mut u32,
    pub timestamp: *const AudioTimeStamp,
    pub bus: u32,
    pub frames: u32,
    pub data: *mut AudioBufferList,
}
#[repr(C)]
struct NativeRender {
    call: unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const AudioTimeStamp,
        u32,
        u32,
        *mut AudioBufferList,
    ) -> i32,
    args: Render,
}
unsafe extern "C" {
    fn oc_au_render(args: *const NativeRender, output: *mut i32, fault: *mut RawFault) -> bool;
}

#[repr(C)]
pub(super) struct SetParameter {
    pub unit: *mut c_void,
    pub parameter: u32,
    pub scope: u32,
    pub element: u32,
    pub value: f32,
    pub offset: u32,
}
#[repr(C)]
struct NativeSetParameter {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, f32, u32) -> i32,
    args: SetParameter,
}
unsafe extern "C" {
    fn oc_au_set_parameter(
        args: *const NativeSetParameter,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct GetParameter {
    pub unit: *mut c_void,
    pub parameter: u32,
    pub scope: u32,
    pub element: u32,
    pub value: *mut f32,
}
#[repr(C)]
struct NativeGetParameter {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut f32) -> i32,
    args: GetParameter,
}
unsafe extern "C" {
    fn oc_au_get_parameter(
        args: *const NativeGetParameter,
        output: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Midi {
    pub unit: *mut c_void,
    pub status: u32,
    pub data1: u32,
    pub data2: u32,
    pub offset: u32,
}
#[repr(C)]
struct NativeMidi {
    call: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> i32,
    args: Midi,
}
unsafe extern "C" {
    fn oc_au_midi(args: *const NativeMidi, output: *mut i32, fault: *mut RawFault) -> bool;
}

#[repr(C)]
pub(super) struct MidiList {
    pub unit: *mut c_void,
    pub offset: u32,
    pub events: *const MidiEventList,
}
#[repr(C)]
struct NativeMidiList {
    call: unsafe extern "C" fn(*mut c_void, u32, *const MidiEventList) -> i32,
    args: MidiList,
}
unsafe extern "C" {
    fn oc_au_midi_list(args: *const NativeMidiList, output: *mut i32, fault: *mut RawFault)
    -> bool;
}

#[repr(C)]
pub(super) struct StringLength {
    pub text: *mut c_void,
}
#[repr(C)]
struct NativeStringLength {
    call: unsafe extern "C" fn(*mut c_void) -> isize,
    args: StringLength,
}
unsafe extern "C" {
    fn oc_au_string_length(
        args: *const NativeStringLength,
        output: *mut isize,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct StringCapacity {
    pub length: isize,
    pub encoding: u32,
}
#[repr(C)]
struct NativeStringCapacity {
    call: unsafe extern "C" fn(isize, u32) -> isize,
    args: StringCapacity,
}
unsafe extern "C" {
    fn oc_au_string_capacity(
        args: *const NativeStringCapacity,
        output: *mut isize,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct StringCString {
    pub text: *mut c_void,
    pub buffer: *mut c_char,
    pub capacity: isize,
    pub encoding: u32,
}
#[repr(C)]
struct NativeStringCString {
    call: unsafe extern "C" fn(*mut c_void, *mut c_char, isize, u32) -> u8,
    args: StringCString,
}
unsafe extern "C" {
    fn oc_au_string_c_string(
        args: *const NativeStringCString,
        output: *mut u8,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct DataCreate {
    pub allocator: *const c_void,
    pub bytes: *const u8,
    pub length: isize,
}
#[repr(C)]
struct NativeDataCreate {
    call: unsafe extern "C" fn(*const c_void, *const u8, isize) -> *mut c_void,
    args: DataCreate,
}
unsafe extern "C" {
    fn oc_au_data_create(
        args: *const NativeDataCreate,
        output: *mut *mut c_void,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct DataLength {
    pub data: *mut c_void,
}
#[repr(C)]
struct NativeDataLength {
    call: unsafe extern "C" fn(*mut c_void) -> isize,
    args: DataLength,
}
unsafe extern "C" {
    fn oc_au_data_length(
        args: *const NativeDataLength,
        output: *mut isize,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct DataBytes {
    pub data: *mut c_void,
}
#[repr(C)]
struct NativeDataBytes {
    call: unsafe extern "C" fn(*mut c_void) -> *const u8,
    args: DataBytes,
}
unsafe extern "C" {
    fn oc_au_data_bytes(
        args: *const NativeDataBytes,
        output: *mut *const u8,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct PlistData {
    pub allocator: *const c_void,
    pub plist: *mut c_void,
    pub format: isize,
    pub options: usize,
    pub error: *mut *mut c_void,
}
#[repr(C)]
struct NativePlistData {
    call: unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        isize,
        usize,
        *mut *mut c_void,
    ) -> *mut c_void,
    args: PlistData,
}
unsafe extern "C" {
    fn oc_au_plist_data(
        args: *const NativePlistData,
        output: *mut *mut c_void,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct PlistCreate {
    pub allocator: *const c_void,
    pub data: *mut c_void,
    pub options: usize,
    pub format: *mut isize,
    pub error: *mut *mut c_void,
}
#[repr(C)]
struct NativePlistCreate {
    call: unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        usize,
        *mut isize,
        *mut *mut c_void,
    ) -> *mut c_void,
    args: PlistCreate,
}
unsafe extern "C" {
    fn oc_au_plist_create(
        args: *const NativePlistCreate,
        output: *mut *mut c_void,
        fault: *mut RawFault,
    ) -> bool;
}

#[repr(C)]
pub(super) struct Release {
    pub value: *mut c_void,
}
#[repr(C)]
struct NativeRelease {
    call: unsafe extern "C" fn(*mut c_void),
    args: Release,
}
unsafe extern "C" {
    fn oc_au_release(args: *const NativeRelease, output: *mut u8, fault: *mut RawFault) -> bool;
}

#[cfg(test)]
#[path = "au_guard_tests.rs"]
mod tests;
