//! An optimized native COM plugin fixture driven through the real instance.
pub use super::factory::tests::{FactoryOwnerAudit, audit_factory_owner};
use super::*;
mod admission_audit;
pub(crate) mod factory_audit;
pub use factory_audit::{FactoryBoundaryAudit, audit_factory_boundary};
mod owned_audit;
mod tuning_audit;
pub use admission_audit::{
    OwnedAdmissionAudit, OwnedAdmissionCaseAudit, OwnedQueuePackingAudit,
    OwnedQueuePackingCaseAudit, audit_owned_admission_with_heap_counter,
    audit_owned_queue_packing_with_heap_counter,
};
pub use owned_audit::{
    OwnedHeapCounters, OwnedLifecycleAudit, audit_owned_ui_lifecycle,
    audit_owned_ui_lifecycle_with_heap_counter,
};
pub use tuning_audit::{Vst3TuningPayloadAudit, audit_tuning_payload};

#[link(name = "oc_vst3_guard_fixture", kind = "static")]
unsafe extern "C" {
    fn oc_vst3_fixture_create(
        effect: bool,
        refusal: tresult,
        not_implemented: tresult,
        component: *mut *mut IComponent,
        processor: *mut *mut IAudioProcessor,
        controller: *mut *mut IEditController,
    ) -> *mut c_void;
    fn oc_vst3_fixture_connections(handle: *mut c_void, enabled: bool);
    #[cfg(test)]
    fn oc_vst3_fixture_host(handle: *mut c_void, index: u32) -> usize;
    #[cfg(target_os = "macos")]
    fn oc_vst3_fixture_view(handle: *mut c_void) -> *mut IPlugView;
    fn oc_vst3_fixture_delivery(handle: *mut c_void, field: u32) -> u64;
    fn oc_vst3_fixture_parameters(handle: *mut c_void, mode: u32, handler: *mut c_void);
    fn oc_vst3_fixture_preparation_mutation(handle: *mut c_void, mode: u32);
    fn oc_vst3_fixture_parameter_restart(handle: *mut c_void, point: u32, flags: i32);
    fn oc_vst3_fixture_midi_layout(index: u32) -> usize;
    fn oc_vst3_fixture_midi_word(handle: *mut c_void, kind: u32, index: u32, field: u32) -> u64;
    fn oc_vst3_fixture_controller_value(handle: *mut c_void) -> f64;
    fn oc_vst3_fixture_parameter_value(handle: *mut c_void, id: u32) -> f64;
    fn oc_vst3_fixture_bind_iid(handle: *mut c_void, kind: u32, iid: *const u8);
    fn oc_vst3_fixture_secondary_fault(handle: *mut c_void, point: u32, kind: u32);
    fn oc_vst3_fixture_dispose(handle: *mut c_void);
    fn oc_vst3_fixture_fault(handle: *mut c_void, point: u32, kind: u32);
    fn oc_vst3_fixture_refusal(handle: *mut c_void, result: tresult);
    fn oc_vst3_fixture_calls(handle: *mut c_void, point: u32) -> u32;
    fn oc_vst3_fixture_refs(handle: *mut c_void) -> u32;
    #[cfg(test)]
    fn oc_vst3_fixture_thread_constructions() -> u32;
    fn oc_vst3_fixture_ui_violations(handle: *mut c_void) -> u32;
    fn oc_vst3_fixture_process_entered(handle: *mut c_void) -> bool;
    fn oc_vst3_fixture_hold_process(handle: *mut c_void, hold: bool);
    fn oc_vst3_fixture_last_call_on_ui(handle: *mut c_void, point: u32) -> bool;
    #[cfg(test)]
    fn oc_vst3_fixture_layout(index: u32) -> usize;
    #[cfg(test)]
    fn oc_vst3_fixture_channels(handle: *mut c_void, channels: i32);
}

pub struct NativeFixture {
    handle: *mut c_void,
    instance: Option<Vst3Instance>,
}
impl NativeFixture {
    pub fn new(effect: bool) -> Self {
        let mut component = ptr::null_mut();
        let mut processor = ptr::null_mut();
        let mut controller = ptr::null_mut();
        // SAFETY: native noexcept construction writes three owned COM refs;
        // the fixture's explicit owner remains alive through instance teardown.
        let handle = unsafe {
            oc_vst3_fixture_create(
                effect,
                kResultFalse,
                kNotImplemented,
                &mut component,
                &mut processor,
                &mut controller,
            )
        };
        assert!(!handle.is_null());
        // SAFETY: fixture owner is live and each interface IID is a static 16-byte value.
        unsafe {
            oc_vst3_fixture_bind_iid(handle, 0, IComponent::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 1, IAudioProcessor::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 2, IEditController::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 3, IConnectionPoint::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 4, IMidiMapping::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 5, INoteExpressionController::IID.as_ptr().cast());
        }
        // SAFETY: construction returned separately owned component/processor/controller refs.
        let (component, processor, controller) = unsafe {
            (
                ComPtr::from_raw(component).unwrap(),
                ComPtr::from_raw(processor).unwrap(),
                ComPtr::from_raw(controller).unwrap(),
            )
        };
        let instance = Vst3Instance::from_test_parts_with_controller(
            component, processor, controller, 48_000, 512,
        );
        Self {
            handle,
            instance: Some(instance),
        }
    }
    pub(super) fn load_with_fault(point: u32, kind: u32) -> (Self, Result<(), String>) {
        let mut component = ptr::null_mut();
        let mut processor = ptr::null_mut();
        let mut controller = ptr::null_mut();
        // SAFETY: native noexcept construction yields three separately owned COM refs.
        let handle = unsafe {
            oc_vst3_fixture_create(
                false,
                kResultFalse,
                kNotImplemented,
                &mut component,
                &mut processor,
                &mut controller,
            )
        };
        assert!(!handle.is_null());
        // SAFETY: fixture owner is live and each interface IID is a static 16-byte value.
        unsafe {
            oc_vst3_fixture_bind_iid(handle, 0, IComponent::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 1, IAudioProcessor::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 2, IEditController::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 3, IConnectionPoint::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 4, IMidiMapping::IID.as_ptr().cast());
            oc_vst3_fixture_bind_iid(handle, 5, INoteExpressionController::IID.as_ptr().cast());
        }
        // SAFETY: this owner is live; failure selection changes no Rust/native ownership.
        unsafe {
            oc_vst3_fixture_fault(handle, point, if kind == 5 || kind == 6 { 4 } else { kind });
            if kind == 5 {
                oc_vst3_fixture_secondary_fault(handle, if point == 24 { 25 } else { 10 }, 1);
            }
            if kind == 6 {
                oc_vst3_fixture_secondary_fault(handle, 11, 1);
            }
            oc_vst3_fixture_connections(handle, (23..=28).contains(&point));
        }
        let failure = Arc::new(Vst3Failure::default());
        // SAFETY: construction returned live owned references matching the interfaces.
        let parts = unsafe {
            Vst3LoadingParts {
                component: Vst3ForeignRef::adopt(
                    ComPtr::from_raw(component).unwrap(),
                    Arc::clone(&failure),
                ),
                processor: Vst3ForeignRef::adopt(
                    ComPtr::from_raw(processor).unwrap(),
                    Arc::clone(&failure),
                ),
                controller: Some(Vst3ForeignRef::adopt(
                    ComPtr::from_raw(controller).unwrap(),
                    Arc::clone(&failure),
                )),
                controller_is_separate: point == 22 || (23..=28).contains(&point),
                owned_host: None,
                component_handler: ptr::null_mut(),
                factory: None,
                factory2: None,
                failure,
                component_initialized: ![21, 22].contains(&point),
                controller_initialized: false,
                lib_handle: ptr::null_mut(),
                class_info: Vst3ClassInfo {
                    name: "native constructor fixture".into(),
                    category: "Audio Module Class".into(),
                    sub_categories: String::new(),
                    vendor: String::new(),
                    version: String::new(),
                    cid: [0; 16],
                },
            }
        };
        // This is the same whole post-interface constructor used by load_with_context:
        // state transfer, setup, layout negotiation, activation and instance ownership.
        match Vst3Instance::finish_load(parts, 48_000, 512, true, true) {
            Ok(instance) => (
                Self {
                    handle,
                    instance: Some(instance),
                },
                Ok(()),
            ),
            Err(error) => (
                Self {
                    handle,
                    instance: None,
                },
                Err(error),
            ),
        }
    }
    #[cfg(test)]
    fn set_channels(&self, channels: i32) {
        // SAFETY: fixture owner is live and tests serialize access on this thread.
        unsafe { oc_vst3_fixture_channels(self.handle, channels) };
    }
    #[expect(
        clippy::result_large_err,
        reason = "allocator tests exercise bounded native ownership failures without formatting"
    )]
    pub fn exercise_reference(&self, query: bool) -> Result<(), Vst3ProcessError> {
        if query {
            drop(self.instance().component.try_query::<IAudioProcessor>()?);
        } else {
            drop(self.instance().component.try_clone()?);
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    pub fn editor_lease_for_test(&mut self) -> Result<Vst3GuiHandle, String> {
        test_support::install_plug_frame(self.instance_mut());
        self.instance()
            .try_gui_handle()?
            .ok_or_else(|| "no native fixture editor controller".into())
    }
    #[cfg(target_os = "macos")]
    pub fn register_main_view_for_test(&mut self) -> Result<usize, String> {
        if !is_main_thread() {
            return Err("native fixture view registration requires AppKit main".into());
        }
        let gui = self.editor_lease_for_test()?;
        // SAFETY: fixture exports exactly one owning native view reference; its
        // explicit fixture owner remains live until this test retires the view.
        let view = unsafe { ComPtr::from_raw(oc_vst3_fixture_view(self.handle)).unwrap() };
        let view = Vst3ForeignRef::adopt(view, self.instance().failure())
            .with_editor_parent(&gui.controller);
        let handles = Arc::clone(&gui.handles);
        let key = Arc::as_ptr(&handles) as usize;
        publish_editor_view(
            handles,
            view,
            Arc::clone(&gui.frame),
            Arc::clone(&gui.controller),
        );
        Ok(key)
    }
    #[cfg(target_os = "macos")]
    pub fn retire_instance_on_worker_for_test(&mut self) {
        let instance = self.instance.take();
        std::thread::spawn(move || drop(instance))
            .join()
            .expect("native fixture retirement worker");
    }
    pub fn configure_parameters_for_test(&mut self, mode: u32) {
        let host = Vst3Host::new();
        let handler = host.to_com_ptr::<IComponentHandler>().unwrap();
        let instance = self.instance_mut();
        let backing = Arc::new(HostBacking {
            host: ManuallyDrop::new(Some(host.clone())),
            failure: instance.failure(),
        });
        instance.component.set_backing(Arc::clone(&backing));
        instance.processor.set_backing(Arc::clone(&backing));
        if let Some(controller) = instance.controller.as_mut() {
            controller.set_backing(backing);
        }
        instance._host = ManuallyDrop::new(Some(host));
        // SAFETY: fixture borrows a live Rust host handler; the instance and all
        // guarded native owners retain its callback backing until healthy retirement.
        unsafe { oc_vst3_fixture_parameters(self.handle, mode, handler.as_ptr().cast()) };
    }
    pub(super) fn midi_layout_for_test(index: u32) -> usize {
        // SAFETY: native probe returns only compile-time SDK layout constants.
        unsafe { oc_vst3_fixture_midi_layout(index) }
    }
    pub(super) fn midi_word_for_test(&self, kind: u32, index: u32, field: u32) -> u64 {
        // SAFETY: caller joins the processing worker before reading retained fixture scalars.
        unsafe { oc_vst3_fixture_midi_word(self.handle, kind, index, field) }
    }
    pub(super) fn controller_value_for_test(&self) -> f64 {
        // SAFETY: this fixture owns its live native allocation on UI.
        unsafe { oc_vst3_fixture_controller_value(self.handle) }
    }
    pub(super) fn ui_violations_for_test(&self) -> u32 {
        // SAFETY: the native allocation is retained and the counter is atomic.
        unsafe { oc_vst3_fixture_ui_violations(self.handle) }
    }
    pub(super) fn last_call_on_ui_for_test(&self, point: u32) -> bool {
        // SAFETY: the fixture retains the native owner and reads a bounded atomic counter.
        unsafe { oc_vst3_fixture_last_call_on_ui(self.handle, point) }
    }
    pub(super) fn take_instance_for_preparation(&mut self) -> Option<Vst3Instance> {
        self.instance.take()
    }
    pub(super) fn preparation_mutation_for_test(&self, mode: u32) {
        // SAFETY: this fixture owns the live native context; only the UI test thread changes its mode.
        unsafe { oc_vst3_fixture_preparation_mutation(self.handle, mode) };
    }
    pub(super) fn delivery_for_test(&self, field: u32) -> u64 {
        // SAFETY: the worker has joined and this fixture retains its native owner.
        unsafe { oc_vst3_fixture_delivery(self.handle, field) }
    }
    pub fn parameter_restart_for_test(&self, point: u32, flags: i32) {
        // SAFETY: test fixture owner is live and flags are copied synchronously.
        unsafe { oc_vst3_fixture_parameter_restart(self.handle, point, flags) };
    }
    pub fn parameter_value_for_test(&self, id: u32) -> f64 {
        // SAFETY: fixture getter reads one scalar from the live test owner.
        unsafe { oc_vst3_fixture_parameter_value(self.handle, id) }
    }
    pub fn pending_parameter_count_for_test(&self) -> usize {
        self.instance()
            .pending_events
            .iter()
            .filter(|event| matches!(event, Vst3PendingEvent::Param(_)))
            .count()
    }
    pub fn take_parameter_restart_for_test(&self) -> i32 {
        self.instance()
            ._host
            .as_ref()
            .unwrap()
            .edit_sink()
            .take_restart()
    }
    pub fn instance(&self) -> &Vst3Instance {
        self.instance.as_ref().unwrap()
    }
    pub fn instance_mut(&mut self) -> &mut Vst3Instance {
        self.instance.as_mut().unwrap()
    }
    /// Native operation IDs: process=1, setup=2, processing=3, active=4,
    /// component set/get=5/6, controller set/get/component=7/8/9.
    /// Kind: std exception=1, non-std=2, long std message=3, plugin refusal=4.
    pub fn set_fault(&self, point: u32, kind: u32) {
        // SAFETY: this fixture owns the live native owner on the current thread.
        unsafe { oc_vst3_fixture_fault(self.handle, point, kind) };
    }
    pub fn set_refusal(&self, result: tresult) {
        // SAFETY: only this fixture thread accesses the live native owner.
        unsafe { oc_vst3_fixture_refusal(self.handle, result) };
    }
    pub fn calls(&self, point: u32) -> u32 {
        // SAFETY: counters belong to this fixture's live owner.
        unsafe { oc_vst3_fixture_calls(self.handle, point) }
    }
    pub fn refs(&self) -> u32 {
        // SAFETY: the explicit fixture owner remains live even after COM retirement.
        unsafe { oc_vst3_fixture_refs(self.handle) }
    }
    pub fn retire(&mut self) {
        drop(self.instance.take());
    }
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        self.retire();
        // SAFETY: no fixture callbacks or threads exist after the synchronous
        // instance has retired. Explicit test cleanup can reclaim retained fake
        // objects; production quarantine deliberately retains third-party code.
        unsafe { oc_vst3_fixture_dispose(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    fn isolated(name: &str, test: impl FnOnce()) {
        const KEY: &str = "OCP_VST3_NATIVE_FIXTURE_CASE";
        if std::env::var(KEY).ok().as_deref() == Some(name) {
            test();
            return;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg(format!("vst3_instance::native_fixture::tests::{name}"))
            .arg("--nocapture")
            .env(KEY, name);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the child pre-exec hook only changes its core-dump limit
            // using an async-signal-safe system call; it allocates no Rust data.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "native fixture child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn native_layout_and_generated_com_dispatch_agree() {
        isolated("native_layout_and_generated_com_dispatch_agree", || {
            let expected = [
                size_of::<ProcessSetup>(),
                align_of::<ProcessSetup>(),
                std::mem::offset_of!(ProcessSetup, sampleRate),
                size_of::<ProcessData>(),
                align_of::<ProcessData>(),
                std::mem::offset_of!(ProcessData, inputs),
                std::mem::offset_of!(ProcessData, outputs),
                std::mem::offset_of!(ProcessData, inputParameterChanges),
                std::mem::offset_of!(ProcessData, outputParameterChanges),
                std::mem::offset_of!(ProcessData, inputEvents),
                std::mem::offset_of!(ProcessData, outputEvents),
                std::mem::offset_of!(ProcessData, processContext),
                size_of::<AudioBusBuffers>(),
                align_of::<AudioBusBuffers>(),
                std::mem::offset_of!(AudioBusBuffers, silenceFlags),
                std::mem::offset_of!(AudioBusBuffers, __field0),
                size_of::<BusInfo>(),
                align_of::<BusInfo>(),
                std::mem::offset_of!(BusInfo, name),
                std::mem::offset_of!(BusInfo, flags),
                size_of::<ParameterInfo>(),
                align_of::<ParameterInfo>(),
                std::mem::offset_of!(ParameterInfo, defaultNormalizedValue),
                std::mem::offset_of!(ParameterInfo, flags),
                size_of::<crate::native_fault::RawFault>(),
                align_of::<crate::native_fault::RawFault>(),
                std::mem::offset_of!(crate::native_fault::RawFault, message),
            ];
            for (index, value) in expected.into_iter().enumerate() {
                // SAFETY: the noexcept probe reads only compile-time layout constants.
                let native_layout = unsafe { oc_vst3_fixture_layout(index as u32) };
                assert_eq!(native_layout, value, "native/Rust ABI layout entry {index}");
            }
            let fixture = NativeFixture::new(false);
            let raw = fixture.instance().raw_interfaces();
            // SAFETY: the healthy fixture has not enabled throwing. These calls
            // deliberately use the independent generated Rust COM vtable slots.
            let (component, processor, controller) = unsafe {
                (
                    ComRef::<IComponent>::from_raw(raw.component.cast()).unwrap(),
                    ComRef::<IAudioProcessor>::from_raw(raw.processor.cast()).unwrap(),
                    ComRef::<IEditController>::from_raw(raw.controller.cast()).unwrap(),
                )
            };
            let mut setup = ProcessSetup {
                processMode: 0,
                symbolicSampleSize: 0,
                maxSamplesPerBlock: 512,
                sampleRate: 48_000.0,
            };
            // SAFETY: all interfaces and arguments are live, with throw mode off.
            unsafe {
                assert_eq!(processor.setupProcessing(&mut setup), kResultOk);
                assert_eq!(processor.setProcessing(1), kResultOk);
                assert_eq!(component.setActive(1), kResultOk);
                assert_eq!(processor.getLatencySamples(), 31);
                assert_eq!(processor.getTailSamples(), 37);
                assert_eq!(controller.getParameterCount(), 0);
            }
            assert_eq!(
                (
                    fixture.calls(2),
                    fixture.calls(3),
                    fixture.calls(4),
                    fixture.calls(12),
                    fixture.calls(13)
                ),
                (1, 1, 1, 1, 1)
            );
            let bus_counts_before = (fixture.calls(14), fixture.calls(15));
            // SAFETY: native fixture is healthy; generated Rust vtables and live
            // stack output arguments independently verify the mirrored native ABI.
            unsafe {
                assert_eq!(component.getBusCount(0, 1), 1);
                let mut info = std::mem::zeroed::<BusInfo>();
                assert_eq!(component.getBusInfo(0, 1, 0, &mut info), kResultOk);
                assert_eq!((info.channelCount, info.direction), (2, 1));
                let mut arrangement = 0u64;
                assert_eq!(
                    processor.getBusArrangement(1, 0, &mut arrangement),
                    kResultOk
                );
                assert_eq!(arrangement, 3);
                assert_eq!(
                    processor.setBusArrangements(ptr::null_mut(), 0, &mut arrangement, 1),
                    kResultOk
                );
                assert_eq!(component.activateBus(0, 1, 0, 1), kResultOk);
            }
            assert_eq!(
                (fixture.calls(14), fixture.calls(15)),
                (bus_counts_before.0 + 1, bus_counts_before.1 + 1)
            );
            assert_eq!(
                (fixture.calls(16), fixture.calls(17), fixture.calls(18)),
                (1, 1, 1)
            );
            let initial_refs = fixture.refs();
            let query_before = fixture.calls(20);
            let queried = component
                .cast::<IAudioProcessor>()
                .expect("native healthy queryInterface");
            assert_eq!(queried.as_ptr(), raw.processor.cast());
            assert_eq!(fixture.refs(), initial_refs + 1);
            assert_eq!(fixture.calls(20), query_before + 1);
            drop(queried);
            assert_eq!(fixture.refs(), initial_refs);
            let host = Vst3Host::new();
            let application = host.to_com_ptr::<IHostApplication>().unwrap();
            // SAFETY: generated healthy methods borrow live fixture interfaces and
            // a host-owned application; throw mode is disabled for ABI validation.
            unsafe {
                let unknown = raw.component.cast::<vst3::Steinberg::FUnknown>();
                assert_eq!(((*(*unknown).vtbl).addRef)(unknown), initial_refs + 1);
                assert_eq!(((*(*unknown).vtbl).release)(unknown), initial_refs);
                assert_eq!(component.initialize(application.as_ptr().cast()), kResultOk);
                assert_eq!(
                    controller.initialize(application.as_ptr().cast()),
                    kResultOk
                );
                assert_eq!(
                    oc_vst3_fixture_host(fixture.handle, 0),
                    application.as_ptr() as usize
                );
                assert_eq!(
                    oc_vst3_fixture_host(fixture.handle, 1),
                    application.as_ptr() as usize
                );
            }
            let mut left = [0.0f32; 4];
            let mut right = [0.0f32; 4];
            let mut channels = [left.as_mut_ptr(), right.as_mut_ptr()];
            let mut bus = AudioBusBuffers {
                numChannels: 2,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: channels.as_mut_ptr(),
                },
            };
            let mut data = ProcessData {
                processMode: 0,
                symbolicSampleSize: 0,
                numSamples: 4,
                numInputs: 0,
                numOutputs: 1,
                inputs: ptr::null_mut(),
                outputs: &mut bus,
                inputParameterChanges: ptr::null_mut(),
                outputParameterChanges: ptr::null_mut(),
                inputEvents: ptr::null_mut(),
                outputEvents: ptr::null_mut(),
                processContext: ptr::null_mut(),
            };
            // SAFETY: generated process invokes the genuine healthy native vtable method.
            assert_eq!(unsafe { processor.process(&mut data) }, kResultOk);
            assert_eq!(fixture.calls(1), 1);
            assert_eq!(left, [0.25; 4]);
            assert_eq!(right, [0.5; 4]);
            let wrapper = ComWrapper::new(Vst3StateStream::new(0.75f64.to_le_bytes().to_vec()));
            let stream = wrapper.to_com_ptr::<IBStream>().unwrap();
            // SAFETY: a host-owned stream and native interfaces remain live; all
            // native state methods are healthy during this independent ABI probe.
            unsafe {
                assert_eq!(component.setState(stream.as_ptr()), kResultOk);
                assert_eq!(stream.seek(0, 0, ptr::null_mut()), kResultOk);
                assert_eq!(controller.setComponentState(stream.as_ptr()), kResultOk);
                assert_eq!(stream.seek(0, 0, ptr::null_mut()), kResultOk);
                assert_eq!(controller.setState(stream.as_ptr()), kResultOk);
                assert_eq!(stream.seek(0, 0, ptr::null_mut()), kResultOk);
                assert_eq!(component.getState(stream.as_ptr()), kResultOk);
                assert_eq!(stream.seek(0, 0, ptr::null_mut()), kResultOk);
                assert_eq!(controller.getState(stream.as_ptr()), kResultOk);
            }
            for point in 5..=9 {
                assert_eq!(
                    fixture.calls(point),
                    1,
                    "generated native state slot {point}"
                );
            }
        });
    }

    #[test]
    fn native_process_faults_silence_and_quarantine_real_instance() {
        isolated(
            "native_process_faults_silence_and_quarantine_real_instance",
            || {
                for kind in [1, 2, 3, 4] {
                    let mut fixture = NativeFixture::new(false);
                    let mut outputs = [Vec::with_capacity(16)];
                    fixture
                        .instance_mut()
                        .process_block_outputs_into(8, None, &[0], &mut outputs)
                        .unwrap();
                    outputs[0].clear();
                    fixture.set_fault(1, kind);
                    let first = fixture
                        .instance_mut()
                        .process_block_outputs_into(8, None, &[0], &mut outputs)
                        .unwrap_err();
                    assert_eq!(outputs[0], [0.0; 16]);
                    assert!(fixture.instance().failed());
                    if kind == 4 {
                        assert_eq!(first.plugin_result, Some(kResultFalse));
                    } else {
                        assert!(first.is_exception());
                    }
                    let before = fixture.calls(1);
                    outputs[0].clear();
                    fixture.set_fault(1, 0);
                    let repeated = fixture
                        .instance_mut()
                        .process_block_outputs_into(8, None, &[0], &mut outputs)
                        .unwrap_err();
                    assert_eq!(
                        repeated, first,
                        "quarantine preserves the original bounded cause"
                    );
                    assert_eq!(fixture.calls(1), before);
                    assert_eq!(outputs[0], [0.0; 16]);
                    let failure = fixture.instance().failure();
                    fixture.retire();
                    assert_eq!(fixture.calls(10), 0, "quarantine skips native terminate");
                    assert_eq!(
                        fixture.calls(11),
                        0,
                        "quarantine retains every COM reference"
                    );
                    assert_eq!(fixture.refs(), 3);
                    assert!(failure.failed());
                    let message = failure.message().unwrap();
                    if kind == 1 {
                        assert!(message.contains("optimized native VST3 method exception"));
                    }
                    if kind == 3 {
                        assert!(message.len() < 400, "native diagnostic is bounded");
                    }
                }
            },
        );
    }

    #[test]
    fn native_effect_fault_discards_the_entire_touched_block() {
        isolated(
            "native_effect_fault_discards_the_entire_touched_block",
            || {
                let mut fixture = NativeFixture::new(true);
                let mut buffer = [1.0f32; 16];
                fixture.instance_mut().process_effect(&mut buffer, 48_000);
                assert_eq!(buffer, [0.25; 16]);
                fixture.set_fault(1, 1);
                buffer.fill(1.0);
                fixture.instance_mut().process_effect(&mut buffer, 48_000);
                assert_eq!(buffer, [0.0; 16]);
                let calls = fixture.calls(1);
                buffer.fill(1.0);
                fixture.instance_mut().process_effect(&mut buffer, 48_000);
                assert_eq!(buffer, [0.0; 16]);
                assert_eq!(fixture.calls(1), calls);
                assert!(
                    fixture
                        .instance_mut()
                        .take_fault()
                        .unwrap()
                        .contains("optimized native VST3 method exception")
                );
            },
        );
    }

    #[test]
    fn native_reference_and_lifecycle_generated_abi_agree() {
        isolated("native_reference_and_lifecycle_generated_abi_agree", || {
            let fixture = NativeFixture::new(false);
            let raw = fixture.instance().raw_interfaces();
            // SAFETY: native fixture is healthy, owned by this scope, and throwing
            // is disabled throughout the independent generated ABI probe.
            let (component, controller) = unsafe {
                (
                    ComRef::<IComponent>::from_raw(raw.component.cast()).unwrap(),
                    ComRef::<IEditController>::from_raw(raw.controller.cast()).unwrap(),
                )
            };
            // SAFETY: live fixture control only enables optional native interfaces.
            unsafe { oc_vst3_fixture_connections(fixture.handle, true) };
            let first = component.cast::<IConnectionPoint>().unwrap();
            let second = controller.cast::<IConnectionPoint>().unwrap();
            // SAFETY: acquired native connection objects and peer pointers remain
            // live; generated Rust COM slots independently verify C++ vtable order.
            unsafe {
                assert_eq!(first.connect(second.as_ptr()), kResultOk);
                assert_eq!(second.connect(first.as_ptr()), kResultOk);
                assert_eq!(second.disconnect(first.as_ptr()), kResultOk);
                assert_eq!(first.disconnect(second.as_ptr()), kResultOk);
            }
            assert_eq!(
                (
                    fixture.calls(23),
                    fixture.calls(24),
                    fixture.calls(25),
                    fixture.calls(26)
                ),
                (1, 1, 1, 1)
            );
            drop(second);
            drop(first);
            assert_eq!(fixture.refs(), 3);
            // SAFETY: this fixture-only probe stops processing and deactivates
            // before invoking generated plugin-base terminate methods.
            unsafe {
                let processor = ComRef::<IAudioProcessor>::from_raw(raw.processor.cast()).unwrap();
                assert_eq!(processor.setProcessing(0), kResultOk);
                assert_eq!(component.setActive(0), kResultOk);
                assert_eq!(controller.terminate(), kResultOk);
                assert_eq!(component.terminate(), kResultOk);
            }
            assert_eq!((fixture.calls(27), fixture.calls(28)), (1, 1));
            // The probe already retired these interfaces. Suppress a second host
            // teardown; fixture cleanup reclaims its objects without COM calls.
            fixture
                .instance()
                .failure
                .slot
                .record(Vst3ProcessError::native(
                    Operation::Legacy,
                    Fault::quarantined(),
                ));
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_fault_blocks_actual_frame_and_deferred_attach_dispatchers() {
        isolated(
            "native_fault_blocks_actual_frame_and_deferred_attach_dispatchers",
            || {
                let mut fixture = NativeFixture::new(false);
                let handles = Arc::clone(&fixture.instance().editor_handles);
                // SAFETY: fixture exports one owning reference to its genuine native
                // view object, kept alive by the fixture's explicit owner.
                let view =
                    unsafe { ComPtr::from_raw(oc_vst3_fixture_view(fixture.handle)).unwrap() };
                let view = Vst3ForeignRef::adopt(view, fixture.instance().failure());
                let raw_view = view.as_ptr();
                handles.view.store(raw_view as usize, Ordering::Release);
                let frame = ComWrapper::new(Vst3PlugFrame::new(Arc::clone(&handles)))
                    .to_com_ptr::<IPlugFrame>()
                    .unwrap();
                let mut rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 640,
                    bottom: 480,
                };
                handles.attaching.store(true, Ordering::Release);
                // SAFETY: the real host COM callback borrows a genuine live native
                // view and stack rect; attaching mode avoids any AppKit window access.
                assert_eq!(unsafe { frame.resizeView(raw_view, &mut rect) }, kResultOk);
                assert_eq!(
                    handles.pending_size.lock().unwrap().take(),
                    Some((640, 480))
                );
                let healthy = PendingAttach {
                    failure: fixture.instance().failure(),
                    view: raw_view,
                    parent: ptr::null_mut(),
                    result: Cell::new(kResultFalse),
                    done: Cell::new(false),
                };
                // SAFETY: the production deferred callback borrows this live pending
                // state; the native fixture view needs no OS parent to count attach.
                unsafe {
                    attach_on_main_run_loop((&healthy as *const PendingAttach).cast_mut().cast())
                };
                assert!(healthy.done.get());
                assert_eq!(healthy.result.get(), kResultOk);
                assert_eq!(fixture.calls(29), 1);
                fixture.set_fault(1, 1);
                let mut outputs = [Vec::with_capacity(8)];
                assert!(
                    fixture
                        .instance_mut()
                        .process_block_outputs_into(4, None, &[0], &mut outputs)
                        .is_err()
                );
                // SAFETY: same valid callback arguments. Its shared failure gate must
                // reject the call before mutating pending state or reentering the view.
                assert_eq!(
                    // SAFETY: The live in-process frame receives the same retained fixture view and writable rectangle; its failure gate must prevent native reentry.
                    unsafe { frame.resizeView(raw_view, &mut rect) },
                    kResultFalse
                );
                assert_eq!(*handles.pending_size.lock().unwrap(), None);
                let poisoned = PendingAttach {
                    failure: fixture.instance().failure(),
                    view: raw_view,
                    parent: ptr::null_mut(),
                    result: Cell::new(kResultOk),
                    done: Cell::new(false),
                };
                // SAFETY: pending state and genuine view remain live through dispatch.
                unsafe {
                    attach_on_main_run_loop((&poisoned as *const PendingAttach).cast_mut().cast())
                };
                assert!(poisoned.done.get());
                assert_eq!(poisoned.result.get(), kResultFalse);
                assert_eq!(
                    (fixture.calls(29), fixture.calls(30), fixture.calls(31)),
                    (1, 0, 0)
                );
                drop(frame);
                drop(handles);
                fixture.retire();
                assert_eq!(
                    fixture.calls(11),
                    0,
                    "known poison must not release issued view/core references"
                );
                assert_eq!(fixture.refs(), 4);
            },
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_editor_handoff_reports_its_real_reference_failure() {
        isolated(
            "native_editor_handoff_reports_its_real_reference_failure",
            || {
                let mut fixture = NativeFixture::new(false);
                test_support::install_plug_frame(fixture.instance_mut());
                fixture.set_fault(19, 1);
                let error = match fixture.instance().try_gui_handle() {
                    Err(error) => error,
                    Ok(_) => panic!("native addRef failure was hidden as no editor"),
                };
                assert!(
                    error.contains("addRef")
                        && error.contains("optimized native VST3 method exception"),
                    "{error}"
                );
                assert_eq!(fixture.calls(19), 1);
            },
        );
    }

    #[test]
    fn native_foreign_refcount_failures_are_bounded_and_never_retried() {
        isolated(
            "native_foreign_refcount_failures_are_bounded_and_never_retried",
            || {
                for point in [11, 19, 20] {
                    for kind in [1, 2, 3] {
                        let mut fixture = NativeFixture::new(false);
                        fixture.set_fault(point, kind);
                        let failure = fixture.instance().failure();
                        if point == 19 {
                            assert!(fixture.instance().component.try_clone().is_err());
                        } else if point == 20 {
                            assert!(
                                fixture
                                    .instance()
                                    .component
                                    .try_query::<IAudioProcessor>()
                                    .is_err()
                            );
                        }
                        fixture.retire();
                        let error = failure.first_fault().unwrap();
                        assert_eq!(
                            error.operation,
                            match point {
                                11 => Operation::Release,
                                19 => Operation::AddRef,
                                _ => Operation::QueryInterface,
                            }
                        );
                        assert!(error.is_exception());
                        if kind == 3 {
                            assert!(
                                error.to_string().len() < 400,
                                "native ownership diagnostic is bounded"
                            );
                        }
                        assert_eq!(fixture.calls(point), 1);
                        assert_eq!(
                            fixture.refs(),
                            if point == 11 { 2 } else { 4 },
                            "partial native mutation must not be retried or guessed reversible"
                        );
                        if point != 11 {
                            assert_eq!(fixture.calls(11), 0);
                            assert_eq!(fixture.calls(10), 0);
                        }
                    }
                }
            },
        );
    }

    #[test]
    fn native_initialization_refusals_cleanup_completed_phases() {
        isolated(
            "native_initialization_refusals_cleanup_completed_phases",
            || {
                for point in [21, 22, 23, 24] {
                    for kind in [1, 2, 4, 5, 6] {
                        let (fixture, result) = NativeFixture::load_with_fault(point, kind);
                        let error = result
                            .expect_err("failed native acquisition must not publish an instance");
                        if kind <= 2 {
                            assert!(
                                error.contains(if kind == 1 {
                                    "optimized native VST3 method exception"
                                } else {
                                    "unknown C++ exception"
                                }),
                                "{error}"
                            );
                            assert_eq!(fixture.calls(11), 0);
                        } else {
                            assert!(error.contains("refused"), "{error}");
                            if (kind == 5 && point != 21) || kind == 6 {
                                assert!(
                                    error.contains("cleanup failed")
                                        && error.contains("optimized native VST3 method exception"),
                                    "{error}"
                                );
                                assert_eq!(fixture.calls(11), if kind == 6 { 1 } else { 0 });
                                if kind == 6 {
                                    assert!(error.contains("Release"), "{error}");
                                }
                            } else {
                                assert_eq!(fixture.refs(), 0);
                                assert_eq!(fixture.calls(11), if point >= 23 { 5 } else { 3 });
                            }
                        }
                        if point == 21 {
                            assert_eq!(
                                fixture.calls(10),
                                0,
                                "failed initial phase is never terminated"
                            );
                        }
                        if point == 22 && kind == 4 {
                            assert_eq!(fixture.calls(28), 1);
                            assert_eq!(
                                fixture.calls(27),
                                0,
                                "only successfully initialized component is terminated"
                            );
                        }
                        if point == 24 && kind == 4 {
                            assert_eq!(
                                fixture.calls(25),
                                1,
                                "first successful connection is undone"
                            );
                            assert_eq!(fixture.calls(26), 0);
                        }
                    }
                }
            },
        );
    }

    #[test]
    fn native_connection_and_terminate_failures_quarantine_remaining_owners() {
        isolated(
            "native_connection_and_terminate_failures_quarantine_remaining_owners",
            || {
                for point in [25, 26, 27, 28] {
                    for kind in [1, 2, 4] {
                        let (mut fixture, result) = NativeFixture::load_with_fault(23, 0);
                        result.unwrap();
                        assert_eq!((fixture.calls(23), fixture.calls(24)), (1, 1));
                        assert_eq!(fixture.refs(), 5);
                        fixture.set_fault(point, kind);
                        let failure = fixture.instance().failure();
                        fixture.retire();
                        let error = failure.first_fault().unwrap();
                        assert_eq!(
                            error.operation,
                            match point {
                                25 | 26 => Operation::Disconnect,
                                27 => Operation::ControllerTerminate,
                                _ => Operation::ComponentTerminate,
                            }
                        );
                        if kind == 4 {
                            assert_eq!(error.plugin_result, Some(kResultFalse));
                        } else {
                            assert!(error.is_exception());
                        }
                        assert_eq!(fixture.calls(11), 0);
                        assert_eq!(fixture.refs(), 5);
                        assert_eq!(fixture.calls(point), 1);
                        if point == 26 {
                            assert_eq!(fixture.calls(25), 0);
                            assert_eq!(fixture.calls(27), 0);
                        }
                        if point == 25 {
                            assert_eq!(fixture.calls(27), 0);
                        }
                        if point == 27 {
                            assert_eq!(fixture.calls(28), 0);
                        }
                    }
                }
            },
        );
    }

    #[test]
    fn native_bus_metadata_exceptions_stop_real_construction() {
        isolated(
            "native_bus_metadata_exceptions_stop_real_construction",
            || {
                let (mut healthy, result) = NativeFixture::load_with_fault(0, 0);
                result.unwrap();
                assert!(healthy.calls(14) > 0 && healthy.calls(15) > 0);
                assert_eq!(
                    (healthy.calls(16), healthy.calls(17), healthy.calls(18)),
                    (1, 1, 2)
                );
                healthy.retire();
                assert_eq!(healthy.refs(), 0);
                for point in 14..=18 {
                    for kind in [1, 2] {
                        let (fixture, result) = NativeFixture::load_with_fault(point, kind);
                        let error =
                            result.expect_err("metadata exception must fail real construction");
                        assert!(
                            error.contains(if kind == 1 {
                                "optimized native VST3 method exception"
                            } else {
                                "unknown C++ exception"
                            }),
                            "{error}"
                        );
                        assert_eq!(fixture.calls(point), 1);
                        assert_eq!((fixture.calls(3), fixture.calls(4)), (0, 0));
                        assert_eq!((fixture.calls(10), fixture.calls(11)), (0, 0));
                        assert_eq!(fixture.refs(), 3);
                    }
                }
                // Native tresult refusals keep the existing optional negotiation
                // fallbacks; they are not confused with exceptions.
                for point in [15, 16, 17, 18] {
                    let (mut fixture, result) = NativeFixture::load_with_fault(point, 4);
                    result.unwrap();
                    assert!(!fixture.instance().failed());
                    fixture.set_fault(0, 0);
                    fixture.retire();
                    assert_eq!(fixture.refs(), 0);
                }
            },
        );
    }

    #[test]
    fn native_runtime_metadata_faults_quarantine_without_reentry() {
        isolated(
            "native_runtime_metadata_faults_quarantine_without_reentry",
            || {
                for point in [12, 13, 14, 15, 18] {
                    for kind in [1, 2] {
                        let mut fixture = NativeFixture::new(false);
                        fixture.set_fault(point, kind);
                        if point == 12 {
                            assert_eq!(AudioEffect::latency_samples(fixture.instance()), 0);
                        } else if point == 13 {
                            assert_eq!(fixture.instance().tail_samples(), u32::MAX);
                        } else {
                            if point == 18 {
                                fixture.set_channels(4);
                            }
                            let error = fixture
                                .instance_mut()
                                .set_component_state_bytes(&0.5f64.to_le_bytes())
                                .expect_err("metadata failure must fail state/reload");
                            assert!(
                                error.contains(if kind == 1 {
                                    "optimized native VST3 method exception"
                                } else {
                                    "unknown C++ exception"
                                }),
                                "{error}"
                            );
                        }
                        let error = fixture.instance().failure().first_fault().unwrap();
                        assert_eq!(
                            error.operation,
                            match point {
                                12 => Operation::Latency,
                                13 => Operation::Tail,
                                18 => Operation::ActivateBus,
                                14 => Operation::BusCount,
                                15 => Operation::BusInfo,
                                _ => unreachable!(),
                            }
                        );
                        assert!(error.is_exception());
                        let calls = fixture.calls(point);
                        assert_eq!(fixture.instance().tail_samples(), u32::MAX);
                        assert_eq!(AudioEffect::latency_samples(fixture.instance()), 0);
                        assert!(
                            fixture
                                .instance_mut()
                                .set_component_state_bytes(&0.5f64.to_le_bytes())
                                .is_err()
                        );
                        assert_eq!(
                            fixture.calls(point),
                            calls,
                            "quarantine blocks all later metadata reentry"
                        );
                        fixture.retire();
                        assert_eq!(
                            (fixture.calls(10), fixture.calls(11), fixture.refs()),
                            (0, 0, 3)
                        );
                    }
                }
            },
        );
    }

    #[test]
    fn native_teardown_refusals_and_throws_retain_every_owner() {
        isolated(
            "native_teardown_refusals_and_throws_retain_every_owner",
            || {
                // Healthy constructor and teardown establish role/argument counters.
                let (mut healthy, result) = NativeFixture::load_with_fault(0, 0);
                result.unwrap();
                assert_eq!(
                    (healthy.calls(2), healthy.calls(3), healthy.calls(4)),
                    (1, 1, 1)
                );
                healthy.retire();
                assert_eq!(
                    (
                        healthy.calls(3),
                        healthy.calls(4),
                        healthy.calls(10),
                        healthy.calls(11)
                    ),
                    (2, 2, 1, 3)
                );
                assert_eq!(healthy.refs(), 0);
                for point in [3, 4] {
                    for kind in [1, 2, 4] {
                        let (mut fixture, result) = NativeFixture::load_with_fault(0, 0);
                        result.unwrap();
                        let failure = fixture.instance().failure();
                        fixture.set_fault(point, kind);
                        fixture.retire();
                        let error = failure
                            .first_fault()
                            .expect("teardown failure is published");
                        assert_eq!(
                            error.operation,
                            if point == 3 {
                                Operation::SetProcessing
                            } else {
                                Operation::SetActive
                            }
                        );
                        if kind == 4 {
                            assert_eq!(error.plugin_result, Some(kResultFalse));
                            assert!(!error.is_exception());
                        } else {
                            assert_eq!(error.plugin_result, None);
                            assert!(error.is_exception());
                            let message = error.to_string();
                            if kind == 1 {
                                assert!(message.contains("optimized native VST3 method exception"));
                            } else {
                                assert!(message.contains("unknown C++ exception"), "{message}");
                            }
                        }
                        assert_eq!(fixture.calls(3), 2);
                        assert_eq!(fixture.calls(4), if point == 3 { 1 } else { 2 });
                        assert_eq!(fixture.calls(10), 0, "failure must not terminate");
                        assert_eq!(fixture.calls(11), 0, "failure retains all COM refs");
                        assert_eq!(fixture.refs(), 3);
                    }
                }
            },
        );
    }

    #[test]
    fn native_setup_failure_uses_the_production_constructor() {
        isolated(
            "native_setup_failure_uses_the_production_constructor",
            || {
                for kind in [1, 2] {
                    let (fixture, result) = NativeFixture::load_with_fault(2, kind);
                    let error =
                        result.expect_err("native setup throws from the shared real constructor");
                    assert!(error.contains("setupProcessing"), "{error}");
                    if kind == 1 {
                        assert!(
                            error.contains("optimized native VST3 method exception"),
                            "{error}"
                        );
                    } else {
                        assert!(error.contains("unknown C++ exception"), "{error}");
                    }
                    assert_eq!(fixture.calls(2), 1);
                    assert_eq!(
                        fixture.calls(6),
                        1,
                        "real constructor transfers initial state"
                    );
                    assert_eq!(fixture.calls(9), 1);
                    assert_eq!(
                        (fixture.calls(3), fixture.calls(4)),
                        (0, 0),
                        "no activation after failed setup"
                    );
                    assert_eq!(
                        (fixture.calls(10), fixture.calls(11)),
                        (0, 0),
                        "no teardown or Release after native exception"
                    );
                    assert_eq!(fixture.refs(), 3);
                }
            },
        );
    }

    #[test]
    fn native_state_roundtrips_and_caught_failures_retain_interfaces() {
        isolated(
            "native_state_roundtrips_and_caught_failures_retain_interfaces",
            || {
                let mut healthy = NativeFixture::new(false);
                healthy
                    .instance_mut()
                    .set_state_bytes(&0.75f64.to_le_bytes())
                    .unwrap();
                assert_eq!(
                    healthy.instance().state_bytes().unwrap(),
                    0.75f64.to_le_bytes()
                );
                assert_eq!(
                    healthy.instance().controller_state_bytes().unwrap(),
                    0.75f64.to_le_bytes()
                );
                healthy
                    .instance_mut()
                    .feed_controller_own_state(&0.375f64.to_le_bytes())
                    .unwrap();
                assert_eq!(
                    healthy.instance().controller_state_bytes().unwrap(),
                    0.375f64.to_le_bytes()
                );
                healthy.retire();
                assert_eq!(healthy.calls(11), 3);
                assert_eq!(healthy.refs(), 0);
                for point in 5..=9 {
                    for kind in [1, 2] {
                        let mut fixture = NativeFixture::new(false);
                        fixture.set_fault(point, kind);
                        let bytes = 0.75f64.to_le_bytes();
                        let error = match point {
                            5 => fixture
                                .instance_mut()
                                .set_component_state_without_layout(&bytes)
                                .map(|_| ()),
                            6 => fixture.instance().state_bytes().map(|_| ()),
                            7 => fixture
                                .instance_mut()
                                .feed_controller_own_state(&bytes)
                                .map(|_| ()),
                            8 => fixture.instance().controller_state_bytes().map(|_| ()),
                            9 => fixture
                                .instance_mut()
                                .feed_controller_component_state(&bytes)
                                .map(|_| ()),
                            _ => unreachable!(),
                        }
                        .unwrap_err();
                        assert!(error.contains("VST3"));
                        assert!(fixture.instance().failed());
                        let calls = fixture.calls(point);
                        assert!(fixture.instance().state_bytes().is_err());
                        assert_eq!(fixture.calls(point), calls);
                        fixture.retire();
                        assert_eq!(fixture.calls(10), 0);
                        assert_eq!(fixture.calls(11), 0);
                        assert_eq!(fixture.refs(), 3);
                    }
                }
            },
        );
    }
    #[test]
    fn native_state_refusals_preserve_retry_and_optional_controller_semantics() {
        isolated(
            "native_state_refusals_preserve_retry_and_optional_controller_semantics",
            || {
                let mut fixture = NativeFixture::new(false);
                fixture.set_fault(5, 4);
                let error = fixture
                    .instance_mut()
                    .set_component_state_without_layout(&0.75f64.to_le_bytes())
                    .unwrap_err();
                assert!(error.contains("setState failed"));
                assert!(
                    !fixture.instance().failed(),
                    "an ordinary refused state is not a foreign exception"
                );
                fixture.set_fault(5, 0);
                fixture
                    .instance_mut()
                    .set_component_state_without_layout(&0.75f64.to_le_bytes())
                    .unwrap();
                assert_eq!(
                    fixture.instance().state_bytes().unwrap(),
                    0.75f64.to_le_bytes()
                );
                fixture.set_refusal(kNotImplemented);
                fixture.set_fault(9, 4);
                fixture
                    .instance_mut()
                    .set_state_bytes(&0.875f64.to_le_bytes())
                    .unwrap();
                assert!(
                    !fixture.instance().failed(),
                    "optional controller component-state remains optional"
                );
                assert_eq!(
                    fixture.instance().state_bytes().unwrap(),
                    0.875f64.to_le_bytes()
                );
            },
        );
    }
}
