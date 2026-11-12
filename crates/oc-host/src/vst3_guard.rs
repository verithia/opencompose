//! Typed VST3 calls caught in native C++, without a Rust frame on the unwind path.
use crate::native_fault::{Fault, RawFault};
use vst3::Steinberg::Vst::{
    BusInfo, IAudioProcessor, IComponent, IEditController, ProcessData, ProcessSetup,
};
use vst3::Steinberg::{
    IBStream, IPluginFactory, IPluginFactory2, PClassInfo, PClassInfo2, TUID, tresult,
};

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn oc_vst3_view_set_frame(
        view: *mut vst3::Steinberg::IPlugView,
        frame: *mut vst3::Steinberg::IPlugFrame,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(target_os = "macos")]
    fn oc_vst3_view_removed(
        view: *mut vst3::Steinberg::IPlugView,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(target_os = "macos")]
    fn oc_vst3_view_attached(
        view: *mut vst3::Steinberg::IPlugView,
        parent: *mut std::ffi::c_void,
        platform: *const std::ffi::c_char,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_midi_assignment(
        mapping: *mut vst3::Steinberg::Vst::IMidiMapping,
        bus: i32,
        channel: i16,
        controller: i16,
        id: *mut u32,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_expression_count(
        controller: *mut vst3::Steinberg::Vst::INoteExpressionController,
        bus: i32,
        channel: i16,
        result: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_expression_info(
        controller: *mut vst3::Steinberg::Vst::INoteExpressionController,
        bus: i32,
        channel: i16,
        index: i32,
        info: *mut vst3::Steinberg::Vst::NoteExpressionTypeInfo,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_factory_count(
        factory: *mut IPluginFactory,
        result: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_factory_class_info(
        factory: *mut IPluginFactory,
        index: i32,
        info: *mut PClassInfo,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_factory_class_info2(
        factory: *mut IPluginFactory2,
        index: i32,
        info: *mut PClassInfo2,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_factory_create(
        factory: *mut IPluginFactory,
        cid: *const std::ffi::c_char,
        iid: *const std::ffi::c_char,
        output: *mut *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_controller_class_id(
        component: *mut IComponent,
        cid: *mut TUID,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_get_factory(
        entry: *mut std::ffi::c_void,
        result: *mut *mut IPluginFactory,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(any(all(unix, not(target_os = "macos")), test, feature = "test-support"))]
    fn oc_vst3_module_entry(
        entry: *mut std::ffi::c_void,
        handle: *mut std::ffi::c_void,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(any(target_os = "macos", test, feature = "test-support"))]
    fn oc_vst3_bundle_entry(
        entry: *mut std::ffi::c_void,
        bundle: *mut std::ffi::c_void,
        result: *mut u8,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(any(windows, test, feature = "test-support"))]
    fn oc_vst3_module_init(
        entry: *mut std::ffi::c_void,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_module_exit(
        entry: *mut std::ffi::c_void,
        result: *mut bool,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_parameter_count(
        controller: *mut IEditController,
        result: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_parameter_info(
        controller: *mut IEditController,
        index: i32,
        info: *mut vst3::Steinberg::Vst::ParameterInfo,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_normalized_to_plain(
        controller: *mut IEditController,
        id: u32,
        value: f64,
        result: *mut f64,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_plain_to_normalized(
        controller: *mut IEditController,
        id: u32,
        value: f64,
        result: *mut f64,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_set_normalized(
        controller: *mut IEditController,
        id: u32,
        value: f64,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;

    fn oc_vst3_query(
        object: *mut std::ffi::c_void,
        iid: *const std::ffi::c_char,
        output: *mut *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_add_ref(
        object: *mut std::ffi::c_void,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_release(
        object: *mut std::ffi::c_void,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_initialize(
        object: *mut std::ffi::c_void,
        host: *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_terminate(
        object: *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_connect(
        object: *mut std::ffi::c_void,
        peer: *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_disconnect(
        object: *mut std::ffi::c_void,
        peer: *mut std::ffi::c_void,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;

    fn oc_vst3_bus_count(
        component: *mut IComponent,
        media: i32,
        direction: i32,
        result: *mut i32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_bus_info(
        component: *mut IComponent,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_bus_arrangement(
        processor: *mut IAudioProcessor,
        direction: i32,
        index: i32,
        arrangement: *mut u64,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_bus_arrangements(
        processor: *mut IAudioProcessor,
        inputs: *mut u64,
        input_count: i32,
        outputs: *mut u64,
        output_count: i32,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_activate_bus(
        component: *mut IComponent,
        media: i32,
        direction: i32,
        index: i32,
        active: u8,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_latency(
        processor: *mut IAudioProcessor,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_tail(
        processor: *mut IAudioProcessor,
        result: *mut u32,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_process(
        processor: *mut IAudioProcessor,
        data: *mut ProcessData,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_setup(
        processor: *mut IAudioProcessor,
        setup: *mut ProcessSetup,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_processing(
        processor: *mut IAudioProcessor,
        state: u8,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_active(
        component: *mut IComponent,
        state: u8,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_component_set_state(
        component: *mut IComponent,
        stream: *mut IBStream,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_component_get_state(
        component: *mut IComponent,
        stream: *mut IBStream,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(any(test, feature = "test-support"))]
    fn oc_vst3_controller_set_state(
        controller: *mut IEditController,
        stream: *mut IBStream,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    #[cfg(any(test, feature = "test-support"))]
    fn oc_vst3_controller_get_state(
        controller: *mut IEditController,
        stream: *mut IBStream,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
    fn oc_vst3_controller_component_state(
        controller: *mut IEditController,
        stream: *mut IBStream,
        result: *mut tresult,
        fault: *mut RawFault,
    ) -> bool;
}

macro_rules! invoke {
    ($function:ident($($argument:expr),*)) => {{
        let mut result = Default::default();
        let mut fault = RawFault { kind: 0, message: [0; 256] };
        // SAFETY: each caller supplies live interfaces and operation arguments.
        // The noexcept C++ entrypoint catches native exceptions before returning.
        if unsafe { $function($($argument,)* &mut result, &mut fault) } {
            Ok(result)
        } else { Err(Fault::from_raw(fault)) }
    }};
}

/// # Safety
/// The UI-owned view and any nonnull host frame must stay live through the call.
#[cfg(target_os = "macos")]
#[expect(
    clippy::result_large_err,
    reason = "native faults remain inline at the typed boundary"
)]
pub(crate) unsafe fn view_set_frame(
    view: *mut vst3::Steinberg::IPlugView,
    frame: *mut vst3::Steinberg::IPlugFrame,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_view_set_frame(view, frame))
}

/// # Safety
/// The UI-owned view must stay live through this balanced detach operation.
#[cfg(target_os = "macos")]
#[expect(
    clippy::result_large_err,
    reason = "native faults remain inline at the typed boundary"
)]
pub(crate) unsafe fn view_removed(view: *mut vst3::Steinberg::IPlugView) -> Result<tresult, Fault> {
    invoke!(oc_vst3_view_removed(view))
}

/// # Safety
/// The UI-owned view, parent and NUL-terminated platform string must stay live
/// for the call. Neither pointer is retained by this wrapper.
#[cfg(target_os = "macos")]
#[expect(
    clippy::result_large_err,
    reason = "native faults remain inline at the typed boundary"
)]
pub(crate) unsafe fn view_attached(
    view: *mut vst3::Steinberg::IPlugView,
    parent: *mut std::ffi::c_void,
    platform: *const std::ffi::c_char,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_view_attached(view, parent, platform))
}

/// # Safety
/// The UI-owned interface and fixed-size outputs stay live through the native call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn midi_assignment(
    mapping: *mut vst3::Steinberg::Vst::IMidiMapping,
    bus: i32,
    channel: i16,
    controller: i16,
    id: *mut u32,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_midi_assignment(
        mapping, bus, channel, controller, id
    ))
}
/// # Safety
/// The UI-owned interface and fixed-size outputs stay live through the native call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn expression_count(
    controller: *mut vst3::Steinberg::Vst::INoteExpressionController,
    bus: i32,
    channel: i16,
) -> Result<i32, Fault> {
    invoke!(oc_vst3_expression_count(controller, bus, channel))
}
/// # Safety
/// The UI-owned interface and fixed-size outputs stay live through the native call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn expression_info(
    controller: *mut vst3::Steinberg::Vst::INoteExpressionController,
    bus: i32,
    channel: i16,
    index: i32,
    info: *mut vst3::Steinberg::Vst::NoteExpressionTypeInfo,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_expression_info(
        controller, bus, channel, index, info
    ))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn factory_count(factory: *mut IPluginFactory) -> Result<i32, Fault> {
    invoke!(oc_vst3_factory_count(factory))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn factory_class_info(
    factory: *mut IPluginFactory,
    index: i32,
    info: *mut PClassInfo,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_factory_class_info(factory, index, info))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn factory_class_info2(
    factory: *mut IPluginFactory2,
    index: i32,
    info: *mut PClassInfo2,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_factory_class_info2(factory, index, info))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn factory_create(
    factory: *mut IPluginFactory,
    cid: *const std::ffi::c_char,
    iid: *const std::ffi::c_char,
    output: *mut *mut std::ffi::c_void,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_factory_create(factory, cid, iid, output))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn controller_class_id(
    component: *mut IComponent,
    cid: *mut TUID,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_controller_class_id(component, cid))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn get_factory(
    entry: *mut std::ffi::c_void,
) -> Result<*mut IPluginFactory, Fault> {
    invoke!(oc_vst3_get_factory(entry))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
#[cfg(any(all(unix, not(target_os = "macos")), test, feature = "test-support"))]
pub(crate) unsafe fn module_entry(
    entry: *mut std::ffi::c_void,
    handle: *mut std::ffi::c_void,
) -> Result<bool, Fault> {
    invoke!(oc_vst3_module_entry(entry, handle))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
pub(crate) unsafe fn bundle_entry(
    entry: *mut std::ffi::c_void,
    bundle: *mut std::ffi::c_void,
) -> Result<u8, Fault> {
    invoke!(oc_vst3_bundle_entry(entry, bundle))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
#[cfg(any(windows, test, feature = "test-support"))]
pub(crate) unsafe fn module_init(entry: *mut std::ffi::c_void) -> Result<bool, Fault> {
    invoke!(oc_vst3_module_init(entry))
}
/// # Safety
/// The native interface or module symbol must be live with its specified ABI;
/// caller-owned output buffers and IDs remain valid for the synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn module_exit(entry: *mut std::ffi::c_void) -> Result<bool, Fault> {
    invoke!(oc_vst3_module_exit(entry))
}
/// # Safety
/// Caller must own the controller on its UI thread and keep all output pointers live.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn parameter_count(controller: *mut IEditController) -> Result<i32, Fault> {
    invoke!(oc_vst3_parameter_count(controller))
}
/// # Safety
/// Caller must own the controller on its UI thread and keep all output pointers live.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn parameter_info(
    controller: *mut IEditController,
    index: i32,
    info: *mut vst3::Steinberg::Vst::ParameterInfo,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_parameter_info(controller, index, info))
}
/// # Safety
/// Caller must own the controller on its UI thread and keep all output pointers live.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn normalized_to_plain(
    controller: *mut IEditController,
    id: u32,
    value: f64,
) -> Result<f64, Fault> {
    invoke!(oc_vst3_normalized_to_plain(controller, id, value))
}
/// # Safety
/// Caller must own the controller on its UI thread and keep all output pointers live.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn plain_to_normalized(
    controller: *mut IEditController,
    id: u32,
    value: f64,
) -> Result<f64, Fault> {
    invoke!(oc_vst3_plain_to_normalized(controller, id, value))
}
/// # Safety
/// Caller must own the controller on its UI thread and keep all output pointers live.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline for bounded failure publication"
)]
pub(crate) unsafe fn set_normalized(
    controller: *mut IEditController,
    id: u32,
    value: f64,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_set_normalized(controller, id, value))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn query(
    object: *mut std::ffi::c_void,
    iid: *const std::ffi::c_char,
    output: *mut *mut std::ffi::c_void,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_query(object, iid, output))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn add_ref(object: *mut std::ffi::c_void) -> Result<u32, Fault> {
    invoke!(oc_vst3_add_ref(object))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn release(object: *mut std::ffi::c_void) -> Result<u32, Fault> {
    invoke!(oc_vst3_release(object))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn initialize(
    object: *mut std::ffi::c_void,
    host: *mut std::ffi::c_void,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_initialize(object, host))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn terminate(object: *mut std::ffi::c_void) -> Result<tresult, Fault> {
    invoke!(oc_vst3_terminate(object))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn connect(
    object: *mut std::ffi::c_void,
    peer: *mut std::ffi::c_void,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_connect(object, peer))
}
/// # Safety
/// All interfaces and output pointers must stay live for the native invocation.
#[expect(
    clippy::result_large_err,
    reason = "native causes stay inline without allocating during failure publication"
)]
pub(crate) unsafe fn disconnect(
    object: *mut std::ffi::c_void,
    peer: *mut std::ffi::c_void,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_disconnect(object, peer))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn bus_count(
    component: *mut IComponent,
    media: i32,
    direction: i32,
) -> Result<i32, Fault> {
    invoke!(oc_vst3_bus_count(component, media, direction))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn bus_info(
    component: *mut IComponent,
    media: i32,
    direction: i32,
    index: i32,
    info: *mut BusInfo,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_bus_info(component, media, direction, index, info))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn bus_arrangement(
    processor: *mut IAudioProcessor,
    direction: i32,
    index: i32,
    arrangement: *mut u64,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_bus_arrangement(
        processor,
        direction,
        index,
        arrangement
    ))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn bus_arrangements(
    processor: *mut IAudioProcessor,
    inputs: *mut u64,
    input_count: i32,
    outputs: *mut u64,
    output_count: i32,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_bus_arrangements(
        processor,
        inputs,
        input_count,
        outputs,
        output_count
    ))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn activate_bus(
    component: *mut IComponent,
    media: i32,
    direction: i32,
    index: i32,
    active: bool,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_activate_bus(
        component,
        media,
        direction,
        index,
        u8::from(active)
    ))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn latency(processor: *mut IAudioProcessor) -> Result<u32, Fault> {
    invoke!(oc_vst3_latency(processor))
}
/// # Safety
/// The native interface and all output pointers must remain live for this synchronous call.
#[expect(
    clippy::result_large_err,
    reason = "bounded native causes stay inline without allocating on failure"
)]
pub(crate) unsafe fn tail(processor: *mut IAudioProcessor) -> Result<u32, Fault> {
    invoke!(oc_vst3_tail(processor))
}
/// # Safety
/// The interface and data must remain valid for one native process invocation.
#[expect(
    clippy::result_large_err,
    reason = "the native cause stays inline so failure handling never allocates on the audio thread"
)]
pub(crate) unsafe fn process(
    processor: *mut IAudioProcessor,
    data: *mut ProcessData,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_process(processor, data))
}
/// # Safety
/// The processor and setup must remain valid through this control-thread call.
#[expect(
    clippy::result_large_err,
    reason = "the native cause stays inline so failure handling never allocates on the audio thread"
)]
pub(crate) unsafe fn setup(
    processor: *mut IAudioProcessor,
    setup: *mut ProcessSetup,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_setup(processor, setup))
}
/// # Safety
/// `processor` must point to a live processor interface.
#[expect(
    clippy::result_large_err,
    reason = "the native cause stays inline so failure handling never allocates on the audio thread"
)]
pub(crate) unsafe fn processing(
    processor: *mut IAudioProcessor,
    state: bool,
) -> Result<tresult, Fault> {
    invoke!(oc_vst3_processing(processor, u8::from(state)))
}
/// # Safety
/// `component` must point to a live component interface.
#[expect(
    clippy::result_large_err,
    reason = "the native cause stays inline so failure handling never allocates on the audio thread"
)]
pub(crate) unsafe fn active(component: *mut IComponent, state: bool) -> Result<tresult, Fault> {
    invoke!(oc_vst3_active(component, u8::from(state)))
}
macro_rules! state_method {
    ($name:ident, $function:ident, $interface:ty) => {
        /// # Safety
        /// The native interface and host stream must remain valid for the call.
        #[expect(clippy::result_large_err, reason = "the native cause stays inline so failure handling never allocates on the audio thread")]
        pub(crate) unsafe fn $name(
            interface: *mut $interface,
            stream: *mut IBStream,
        ) -> Result<tresult, Fault> {
            invoke!($function(interface, stream))
        }
    };
}
state_method!(component_set_state, oc_vst3_component_set_state, IComponent);
state_method!(component_get_state, oc_vst3_component_get_state, IComponent);
#[cfg(any(test, feature = "test-support"))]
state_method!(
    controller_set_state,
    oc_vst3_controller_set_state,
    IEditController
);
#[cfg(any(test, feature = "test-support"))]
state_method!(
    controller_get_state,
    oc_vst3_controller_get_state,
    IEditController
);
state_method!(
    controller_component_state,
    oc_vst3_controller_component_state,
    IEditController
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A VST3 call guarded by the exception boundary.
pub enum Operation {
    MidiAssignment,
    ExpressionCount,
    ExpressionInfo,
    FactoryCount,
    FactoryClassInfo,
    FactoryClassInfo2,
    FactoryCreate,
    ControllerClassId,
    GetPluginFactory,
    ModuleEntry,
    ModuleExit,
    ModuleInit,
    BundleEntry,
    Process,
    SetupProcessing,
    SetProcessing,
    SetActive,
    ComponentSetState,
    ComponentGetState,
    ControllerSetState,
    ControllerGetState,
    ControllerComponentState,
    BusCount,
    BusInfo,
    GetBusArrangement,
    BusArrangements,
    ActivateBus,
    Latency,
    Tail,
    QueryInterface,
    AddRef,
    Release,
    ComponentInitialize,
    ControllerInitialize,
    ComponentTerminate,
    ControllerTerminate,
    Connect,
    Disconnect,
    ControllerAuthority,
    ParameterCount,
    ParameterInfo,
    NormalizedToPlain,
    PlainToNormalized,
    SetNormalized,
    Legacy,
}
impl Operation {
    /// VST3 method name for the operation.
    pub fn label(self) -> &'static str {
        match self {
            Self::MidiAssignment => "getMidiControllerAssignment",
            Self::ExpressionCount => "getNoteExpressionCount",
            Self::ExpressionInfo => "getNoteExpressionInfo",
            Self::FactoryCount => "countClasses",
            Self::FactoryClassInfo => "getClassInfo",
            Self::FactoryClassInfo2 => "getClassInfo2",
            Self::FactoryCreate => "createInstance",
            Self::ControllerClassId => "getControllerClassId",
            Self::GetPluginFactory => "GetPluginFactory",
            Self::ModuleEntry => "ModuleEntry",
            Self::ModuleExit => "ModuleExit",
            Self::ModuleInit => "InitDll",
            Self::BundleEntry => "bundleEntry",
            Self::Process => "process",
            Self::SetupProcessing => "setupProcessing",
            Self::SetProcessing => "setProcessing",
            Self::SetActive => "setActive",
            Self::ComponentSetState => "component setState",
            Self::ComponentGetState => "component getState",
            Self::ControllerSetState => "controller setState",
            Self::ControllerGetState => "controller getState",
            Self::ControllerComponentState => "controller setComponentState",
            Self::BusCount => "getBusCount",
            Self::BusInfo => "getBusInfo",
            Self::GetBusArrangement => "getBusArrangement",
            Self::BusArrangements => "setBusArrangements",
            Self::ActivateBus => "activateBus",
            Self::Latency => "getLatencySamples",
            Self::Tail => "getTailSamples",
            Self::QueryInterface => "queryInterface",
            Self::AddRef => "addRef",
            Self::Release => "Release",
            Self::ComponentInitialize => "component initialize",
            Self::ControllerInitialize => "controller initialize",
            Self::ComponentTerminate => "component terminate",
            Self::ControllerTerminate => "controller terminate",
            Self::Connect => "connect",
            Self::Disconnect => "disconnect",
            Self::ControllerAuthority => "controller UI authority",
            Self::ParameterCount => "getParameterCount",
            Self::ParameterInfo => "getParameterInfo",
            Self::NormalizedToPlain => "normalizedParamToPlain",
            Self::PlainToNormalized => "plainParamToNormalized",
            Self::SetNormalized => "setParamNormalized",
            Self::Legacy => "plugin operation",
        }
    }
}

/// Fixed-size failure returned by prepared audio operations. Formatting is a
/// control/reporting operation and never runs on the processing failure path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vst3ProcessError {
    pub operation: Operation,
    pub plugin_result: Option<tresult>,
    pub(crate) fault: Fault,
}
impl Vst3ProcessError {
    pub(crate) fn native(operation: Operation, fault: Fault) -> Self {
        Self {
            operation,
            plugin_result: None,
            fault,
        }
    }
    pub(crate) fn process_result(result: tresult) -> Self {
        Self::refused(Operation::Process, result)
    }
    pub(crate) fn refused(operation: Operation, result: tresult) -> Self {
        Self {
            operation,
            plugin_result: Some(result),
            fault: Fault::process_error(),
        }
    }
    pub(crate) fn host_contract(message: &'static str) -> Self {
        Self::native(Operation::Process, Fault::host_contract(message))
    }
    /// Whether the fault is a C++ or unknown exception.
    pub fn is_exception(self) -> bool {
        matches!(
            self.fault.kind,
            crate::native_fault::FaultKind::CppException
                | crate::native_fault::FaultKind::UnknownException
        )
    }
}
impl std::fmt::Display for Vst3ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(result) = self.plugin_result {
            write!(
                formatter,
                "VST3 {} returned 0x{result:x}; the instance was quarantined",
                self.operation.label()
            )
        } else {
            write!(
                formatter,
                "VST3 {}: {}",
                self.operation.label(),
                self.fault.describe_for("VST3")
            )
        }
    }
}
impl std::error::Error for Vst3ProcessError {}
