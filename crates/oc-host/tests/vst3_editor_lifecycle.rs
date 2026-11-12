//! Headless lifecycle test for a hosted VST3 editor.
//!
//! Runs as its own binary so the AppKit work happens on the process main thread.
//! The controller and view are in-process fakes: they record the host call order
//! and assert the contract a real editor relies on.

fn main() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: this literal executable entrypoint owns the process main thread
        // and services all synchronous checked-controller work before shutdown.
        let queue = unsafe {
            oc_host::application_main::ApplicationMainQueue::register_runtime_current(
                oc_host::application_main::MainRuntime::CommandLine,
            )
        }
        .expect("register editor lifecycle actual main thread");
        macos::run();
        queue.pump().expect("drain editor lifecycle UI queue");
    }
    #[cfg(not(target_os = "macos"))]
    println!("vst3 editor lifecycle: skipped (macOS only)");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, c_void};
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use objc2_app_kit::NSView;
    use objc2_foundation::{NSDate, NSRunLoop};
    use oc_host::vst3_instance::{Vst3GuiHandle, Vst3Instance, is_main_thread, run_on_main_thread};
    use vst3::Steinberg::Vst::{
        IComponentHandler, IEditController, IEditControllerTrait, ParamID, ParamValue,
        ParameterInfo, String128, TChar,
    };
    use vst3::Steinberg::{
        FIDString, IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, IPluginBaseTrait, TBool,
        ViewRect, char16, int16, int32, kNotInitialized, kResultFalse, kResultOk, tresult,
    };
    use vst3::{Class, ComRef, ComWrapper};

    #[derive(Default)]
    struct Calls(Mutex<Vec<&'static str>>);

    impl Calls {
        fn push(&self, call: &'static str) {
            self.0.lock().unwrap().push(call);
        }

        fn snapshot(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    /// What one fake view does, so each case encodes one size rule.
    #[derive(Clone, Copy)]
    struct ViewOptions {
        /// Size before `attached`; `None` answers `kNotInitialized`.
        size_before_attach: Option<(i32, i32)>,
        /// Size once attached.
        size_after_attach: (i32, i32),
        /// Size the view asks for from inside `attached`.
        resize_in_attach: Option<(i32, i32)>,
        /// Also ask for a degenerate 0x0 resize from inside `attached`.
        degenerate_resize_in_attach: bool,
        resizable: bool,
    }

    impl Default for ViewOptions {
        fn default() -> Self {
            Self {
                size_before_attach: Some((400, 300)),
                size_after_attach: (400, 300),
                resize_in_attach: None,
                degenerate_resize_in_attach: false,
                resizable: false,
            }
        }
    }

    type AttachAction = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

    struct FakeView {
        calls: Arc<Calls>,
        /// Options are shared with the case so a re-open can change them.
        options: Arc<Mutex<ViewOptions>>,
        frame: Mutex<*mut IPlugFrame>,
        attached: AtomicBool,
        /// Window content size the host had when `attached` was called.
        window_at_attach: Arc<Mutex<Option<(i32, i32)>>>,
        /// Every size the host delivered through `onSize`, in order.
        observed_sizes: Arc<Mutex<Vec<(i32, i32)>>>,
        /// Results of the resizes this view requested from inside `attached`.
        resize_results: Arc<Mutex<Vec<tresult>>>,
        attach_action: AttachAction,
    }

    impl IPlugViewTrait for FakeView {
        unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
            self.calls.push("isPlatformTypeSupported");
            // SAFETY: the fixture caller supplies a live NUL-terminated platform
            // identifier for this call; the null case short-circuits the read.
            let supported =
                !platform.is_null() && unsafe { CStr::from_ptr(platform) }.to_bytes() == b"NSView";
            if supported { kResultOk } else { kResultFalse }
        }

        unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
            self.calls.push("attached");
            assert!(!parent.is_null(), "attached() needs a parent NSView");
            assert_eq!(
                // SAFETY: the host passes its static NUL-terminated NSView
                // platform identifier for the duration of attached.
                unsafe { CStr::from_ptr(platform) }.to_bytes(),
                b"NSView",
                "attached() platform type"
            );
            self.attached.store(true, Ordering::Release);
            // Release the action lock before reentering the host close path.
            let action = self.attach_action.lock().unwrap().take();
            if let Some(action) = action {
                action();
            }
            // Record the window size the host chose before this call, which is
            // how the "open at the host default" rule is observed.
            // SAFETY: the host keeps the parent view alive for the call.
            let parent = unsafe { &*(parent as *const NSView) };
            let window_size = parent
                .window()
                .and_then(|window| window.contentView())
                .map(|content| content.frame().size);
            *self.window_at_attach.lock().unwrap() =
                window_size.map(|size| (size.width as i32, size.height as i32));

            // An editor may ask the host to resize while it is attaching. The
            // host must not call back into the view until this returns.
            let options = *self.options.lock().unwrap();
            let frame = *self.frame.lock().unwrap();
            if !frame.is_null() {
                // SAFETY: setFrame supplied this nonnull host COM interface;
                // the enclosing attach transaction retains its backing through
                // reentrant close until this synchronous attached call returns.
                let frame = unsafe { ComRef::<IPlugFrame>::from_raw_unchecked(frame) };
                if let Some((width, height)) = options.resize_in_attach {
                    let mut rect = ViewRect {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    };
                    // SAFETY: the retained host frame accepts a null view in
                    // this fixture; rect is writable and lives through the call.
                    let result = unsafe { frame.resizeView(ptr::null_mut(), &mut rect) };
                    self.resize_results.lock().unwrap().push(result);
                }
                if options.degenerate_resize_in_attach {
                    let mut rect = ViewRect {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    };
                    // SAFETY: the retained host frame accepts a null view in
                    // this fixture; rect is writable and lives through the call.
                    let result = unsafe { frame.resizeView(ptr::null_mut(), &mut rect) };
                    self.resize_results.lock().unwrap().push(result);
                }
            }
            self.calls.push("attached done");
            kResultOk
        }

        unsafe fn removed(&self) -> tresult {
            self.calls.push("removed");
            self.attached.store(false, Ordering::Release);
            kResultOk
        }

        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultFalse
        }

        unsafe fn onKeyDown(&self, _key: char16, _code: int16, _mods: int16) -> tresult {
            kResultFalse
        }

        unsafe fn onKeyUp(&self, _key: char16, _code: int16, _mods: int16) -> tresult {
            kResultFalse
        }

        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            self.calls.push("getSize");
            if size.is_null() {
                return kResultFalse;
            }
            let options = *self.options.lock().unwrap();
            let reported = if self.attached.load(Ordering::Acquire) {
                Some(options.size_after_attach)
            } else {
                options.size_before_attach
            };
            let Some((width, height)) = reported else {
                // A Qt editor answers this until its widget is in a window.
                return kNotInitialized;
            };
            // SAFETY: getSize callers provide a writable ViewRect for this
            // synchronous ABI call; null was rejected above.
            unsafe {
                *size = ViewRect {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                }
            };
            kResultOk
        }

        unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
            self.calls.push("onSize");
            if new_size.is_null() {
                return kResultFalse;
            }
            // SAFETY: onSize callers keep this initialized ViewRect live for
            // the call; null was rejected and no reference escapes the callback.
            let rect = unsafe { &*new_size };
            self.observed_sizes
                .lock()
                .unwrap()
                .push((rect.right, rect.bottom));
            kResultOk
        }

        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }

        unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
            if frame.is_null() {
                self.calls.push("setFrame(null)");
            } else {
                self.calls.push("setFrame");
            }
            *self.frame.lock().unwrap() = frame;
            // A frame is installed before any size or attach request.
            let seen = self.calls.snapshot();
            let set_frame = seen.iter().position(|call| *call == "setFrame").unwrap();
            let get_size = seen.iter().position(|call| *call == "getSize");
            assert!(
                get_size.is_none_or(|index| set_frame < index),
                "setFrame must precede getSize: {seen:?}"
            );
            kResultOk
        }

        unsafe fn canResize(&self) -> tresult {
            if self.options.lock().unwrap().resizable {
                kResultOk
            } else {
                kResultFalse
            }
        }

        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    impl Class for FakeView {
        type Interfaces = (IPlugView,);
    }

    struct FakeController {
        calls: Arc<Calls>,
        created_views: Arc<AtomicUsize>,
        options: Arc<Mutex<ViewOptions>>,
        window_at_attach: Arc<Mutex<Option<(i32, i32)>>>,
        observed_sizes: Arc<Mutex<Vec<(i32, i32)>>>,
        resize_results: Arc<Mutex<Vec<tresult>>>,
        attach_action: AttachAction,
    }

    impl IEditControllerTrait for FakeController {
        unsafe fn setComponentState(&self, _state: *mut vst3::Steinberg::IBStream) -> tresult {
            kResultOk
        }

        unsafe fn setState(&self, _state: *mut vst3::Steinberg::IBStream) -> tresult {
            kResultOk
        }

        unsafe fn getState(&self, _state: *mut vst3::Steinberg::IBStream) -> tresult {
            kResultOk
        }

        unsafe fn getParameterCount(&self) -> int32 {
            0
        }

        unsafe fn getParameterInfo(&self, _index: int32, _info: *mut ParameterInfo) -> tresult {
            kResultFalse
        }

        unsafe fn getParamStringByValue(
            &self,
            _id: ParamID,
            _value: ParamValue,
            _string: *mut String128,
        ) -> tresult {
            kResultFalse
        }

        unsafe fn getParamValueByString(
            &self,
            _id: ParamID,
            _string: *mut TChar,
            _value: *mut ParamValue,
        ) -> tresult {
            kResultFalse
        }

        unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
            value
        }

        unsafe fn plainParamToNormalized(&self, _id: ParamID, value: ParamValue) -> ParamValue {
            value
        }

        unsafe fn getParamNormalized(&self, _id: ParamID) -> ParamValue {
            0.0
        }

        unsafe fn setParamNormalized(&self, _id: ParamID, _value: ParamValue) -> tresult {
            kResultOk
        }

        unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
            kResultOk
        }

        unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
            self.calls.push("createView");
            self.created_views.fetch_add(1, Ordering::Relaxed);
            let view = ComWrapper::new(FakeView {
                calls: Arc::clone(&self.calls),
                options: Arc::clone(&self.options),
                frame: Mutex::new(ptr::null_mut()),
                attached: AtomicBool::new(false),
                window_at_attach: Arc::clone(&self.window_at_attach),
                observed_sizes: Arc::clone(&self.observed_sizes),
                resize_results: Arc::clone(&self.resize_results),
                attach_action: Arc::clone(&self.attach_action),
            });
            let view = view.to_com_ptr::<IPlugView>().expect("view pointer");
            view.into_raw()
        }
    }

    impl IPluginBaseTrait for FakeController {
        unsafe fn initialize(&self, _context: *mut vst3::Steinberg::FUnknown) -> tresult {
            kResultOk
        }

        unsafe fn terminate(&self) -> tresult {
            kResultOk
        }
    }

    impl Class for FakeController {
        type Interfaces = (IEditController,);
    }

    /// One controller and its recorders, so a case can inspect the host's work.
    struct Harness {
        handle: Vst3GuiHandle,
        calls: Arc<Calls>,
        created: Arc<AtomicUsize>,
        options: Arc<Mutex<ViewOptions>>,
        window_at_attach: Arc<Mutex<Option<(i32, i32)>>>,
        observed_sizes: Arc<Mutex<Vec<(i32, i32)>>>,
        resize_results: Arc<Mutex<Vec<tresult>>>,
        attach_action: AttachAction,
    }

    fn harness(options: ViewOptions) -> Harness {
        let calls = Arc::new(Calls::default());
        let created = Arc::new(AtomicUsize::new(0));
        let window_at_attach = Arc::new(Mutex::new(None));
        let observed_sizes = Arc::new(Mutex::new(Vec::new()));
        let resize_results = Arc::new(Mutex::new(Vec::new()));
        let options = Arc::new(Mutex::new(options));
        let attach_action: AttachAction = Arc::new(Mutex::new(None));
        let controller = ComWrapper::new(FakeController {
            calls: Arc::clone(&calls),
            created_views: Arc::clone(&created),
            options: Arc::clone(&options),
            window_at_attach: Arc::clone(&window_at_attach),
            observed_sizes: Arc::clone(&observed_sizes),
            resize_results: Arc::clone(&resize_results),
            attach_action: Arc::clone(&attach_action),
        });
        let controller = controller
            .to_com_ptr::<IEditController>()
            .expect("controller pointer");
        Harness {
            handle: Vst3GuiHandle::for_controller(controller),
            calls,
            created,
            options,
            window_at_attach,
            observed_sizes,
            resize_results,
            attach_action,
        }
    }

    fn default_editor_lifecycle() {
        let Harness {
            handle,
            calls,
            created,
            ..
        } = harness(ViewOptions {
            resize_in_attach: Some((520, 340)),
            ..ViewOptions::default()
        });

        let size = handle.open_hidden("lifecycle test").expect("open editor");
        assert_eq!(size, (520, 340), "open reports the attached size");
        assert!(handle.is_open());

        let order = calls.snapshot();
        assert_eq!(
            order,
            vec![
                "createView",
                "isPlatformTypeSupported",
                "setFrame",
                "getSize",
                "attached",
                "attached done",
                "onSize",
            ],
            "host call order around attach"
        );

        handle.close().expect("close editor");
        assert!(!handle.is_open());
        let order = calls.snapshot();
        let removed = order.iter().position(|call| *call == "removed").unwrap();
        let cleared = order
            .iter()
            .rposition(|call| *call == "setFrame(null)")
            .expect("frame cleared on close");
        assert!(removed < cleared, "removed must precede setFrame(null)");

        // A closed editor can be opened again, with a fresh view at the size
        // the window was left at.
        let size = handle.open_hidden("lifecycle test").expect("reopen editor");
        assert_eq!(size, (520, 340));
        assert_eq!(created.load(Ordering::Relaxed), 2);
        handle.close().expect("close reopened editor");

        // Opening twice without closing is an error, not a second window.
        handle.open_hidden("lifecycle test").expect("open editor");
        let error = handle.open_hidden("lifecycle test").unwrap_err();
        assert!(error.contains("already open"), "unexpected error: {error}");
        handle.close().expect("close editor");

        // The window has its own close button: AppKit closes it without telling
        // the host, so the handle must report a window that is gone as closed.
        handle.open_hidden("lifecycle test").expect("open editor");
        handle.simulate_close_button();
        assert!(!handle.is_open(), "a window the user closed is not open");
        let size = handle
            .open_hidden("lifecycle test")
            .expect("reopen after the window was closed");
        assert_eq!(size, (520, 340), "the reopened editor attaches again");
        assert_eq!(created.load(Ordering::Relaxed), 5, "a fresh view per open");
        handle.close().expect("close editor");
    }

    /// `getSize` answers `kNotInitialized` before `attached`: the host opens at
    /// its own default and adopts the size the view reports once attached.
    fn uninitialized_size_before_attach() {
        let Harness {
            handle,
            window_at_attach,
            observed_sizes,
            ..
        } = harness(ViewOptions {
            size_before_attach: None,
            size_after_attach: (400, 300),
            ..ViewOptions::default()
        });

        let size = handle.open_hidden("size test").expect("open editor");
        assert_eq!(
            size,
            (400, 300),
            "the attached size wins over the host default"
        );
        assert_eq!(
            *window_at_attach.lock().unwrap(),
            Some((1024, 768)),
            "the window opens at the host default when the view cannot report a size"
        );
        assert_eq!(
            *observed_sizes.lock().unwrap(),
            vec![(400, 300)],
            "the adopted size reaches the view"
        );
        handle.close().expect("close editor");
    }

    /// A plugin that requests a size during `attached` wins over the size it
    /// reports from `getSize`.
    fn resize_during_attach_wins() {
        let Harness {
            handle,
            observed_sizes,
            ..
        } = harness(ViewOptions {
            size_before_attach: Some((400, 300)),
            size_after_attach: (300, 200),
            resize_in_attach: Some((520, 340)),
            ..ViewOptions::default()
        });

        let size = handle.open_hidden("resize test").expect("open editor");
        assert_eq!(size, (520, 340));
        assert_eq!(*observed_sizes.lock().unwrap(), vec![(520, 340)]);
        handle.close().expect("close editor");
    }

    /// A degenerate resize request is refused instead of collapsing the window.
    fn degenerate_resize_is_refused() {
        let Harness {
            handle,
            resize_results,
            observed_sizes,
            ..
        } = harness(ViewOptions {
            degenerate_resize_in_attach: true,
            ..ViewOptions::default()
        });

        let size = handle.open_hidden("degenerate resize test").expect("open");
        assert_eq!(
            *resize_results.lock().unwrap(),
            vec![kResultFalse],
            "a 0x0 resize is refused"
        );
        assert_eq!(size, (400, 300), "the window keeps its size");
        assert!(observed_sizes.lock().unwrap().is_empty());
        handle.close().expect("close editor");
    }

    /// A re-attached view is told the size the window was left at.
    fn reattach_reapplies_the_window_size() {
        let Harness {
            handle,
            observed_sizes,
            ..
        } = harness(ViewOptions {
            size_before_attach: Some((640, 480)),
            size_after_attach: (640, 480),
            resize_in_attach: Some((700, 500)),
            ..ViewOptions::default()
        });

        assert_eq!(
            handle.open_hidden("re-attach test").expect("open"),
            (700, 500)
        );
        handle.close().expect("close editor");

        observed_sizes.lock().unwrap().clear();
        assert_eq!(
            handle.open_hidden("re-attach test").expect("reopen"),
            (700, 500),
            "the remembered window size survives the close"
        );
        assert_eq!(
            *observed_sizes.lock().unwrap(),
            vec![(700, 500)],
            "the fresh view is told the window size"
        );
        handle.close().expect("close editor");
    }

    /// A re-attach whose view cannot report a size before `attached` still keeps
    /// the remembered window size: the host, not the view, owns it.
    fn reattach_with_uninitialized_size_keeps_the_window_size() {
        let harness = harness(ViewOptions {
            size_before_attach: Some((640, 480)),
            size_after_attach: (640, 480),
            resize_in_attach: Some((700, 500)),
            ..ViewOptions::default()
        });

        assert_eq!(
            harness
                .handle
                .open_hidden("re-attach size test")
                .expect("open"),
            (700, 500)
        );
        harness.handle.close().expect("close editor");

        // The next view answers `kNotInitialized` before `attached` and reports
        // a different size afterwards; the remembered window size still wins.
        *harness.options.lock().unwrap() = ViewOptions {
            size_before_attach: None,
            size_after_attach: (500, 400),
            resize_in_attach: None,
            ..ViewOptions::default()
        };
        harness.observed_sizes.lock().unwrap().clear();
        assert_eq!(
            harness
                .handle
                .open_hidden("re-attach size test")
                .expect("reopen"),
            (700, 500)
        );
        assert_eq!(*harness.observed_sizes.lock().unwrap(), vec![(700, 500)]);
        harness.handle.close().expect("close editor");
    }

    /// Plugin instantiation owns the main-thread contract: a worker call is
    /// dispatched and waits, and the work runs on the AppKit main thread.
    fn dispatch_reaches_the_main_thread() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let dispatched = run_on_main_thread(|| (is_main_thread(), 7));
            let load = Vst3Instance::load_on_main_thread(
                std::path::Path::new("/nonexistent/opencompose-test.vst3"),
                48_000,
                512,
            );
            sender.send((dispatched, load.map(|_| ()))).ok();
        });

        let deadline = Instant::now() + Duration::from_secs(20);
        let payload = loop {
            match receiver.try_recv() {
                Ok(payload) => break payload,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let limit = NSDate::dateWithTimeIntervalSinceNow(0.01);
                    NSRunLoop::currentRunLoop().runUntilDate(&limit);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("the worker ended without a result")
                }
            }
            assert!(
                Instant::now() < deadline,
                "the main thread never ran the dispatched plugin work"
            );
        };
        worker.join().expect("worker thread");

        assert_eq!(
            payload.0,
            Ok((true, 7)),
            "dispatched work must run on the AppKit main thread"
        );
        let error = payload
            .1
            .expect_err("a missing bundle must be a clean load error");
        assert!(error.contains("Failed to load VST3"), "{error}");
    }

    #[cfg(feature = "test-support")]
    fn native_editor_reference_retirement_stays_on_main() {
        use oc_host::vst3_instance::native_fixture::NativeFixture;
        use oc_host::vst3_instance::{
            drain_editor_retirements_for_test, retire_registered_editor_view_for_test,
        };
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Vst3GuiHandle>();
        assert!(is_main_thread());
        let mut controller = NativeFixture::new(false);
        let handle = controller.editor_lease_for_test().unwrap();
        assert_eq!(controller.refs(), 4);
        std::thread::spawn(move || drop(handle)).join().unwrap();
        assert_eq!(
            controller.calls(11),
            0,
            "last editor token must not release native controller on worker"
        );
        assert!(
            std::thread::spawn(drain_editor_retirements_for_test)
                .join()
                .unwrap()
                .is_err()
        );
        assert_eq!(controller.calls(11), 0);
        drain_editor_retirements_for_test().unwrap();
        assert_eq!(controller.calls(11), 1);
        assert_eq!(controller.refs(), 3);
        controller.retire();
        assert_eq!(controller.refs(), 0);

        let mut view = NativeFixture::new(false);
        let key = view.register_main_view_for_test().unwrap();
        assert_eq!(view.refs(), 5);
        view.retire_instance_on_worker_for_test();
        assert_eq!(
            view.calls(36),
            0,
            "worker instance retirement cannot release TLS-owned native view"
        );
        assert_eq!(
            view.refs(),
            2,
            "main owner retains view and editor controller plus callback frame backing"
        );
        assert!(
            std::thread::spawn(move || retire_registered_editor_view_for_test(key))
                .join()
                .unwrap()
                .is_err()
        );
        assert_eq!(view.calls(36), 0);
        retire_registered_editor_view_for_test(key).unwrap();
        assert_eq!(view.calls(36), 1);
        assert_eq!(view.refs(), 0);
    }

    #[cfg(feature = "test-support")]
    fn native_checked_authored_parameters() {
        use ParameterWriteDomain::{LegacyVst3Auto, NativePlain, Normalized};
        use oc_host::preset_parameters::{
            AuthoredParameter, AuthoredParameterOwner, ParameterApplyError, ParameterErrorKind,
            ParameterWriteDomain,
        };
        use oc_host::vst3_instance::native_fixture::NativeFixture;
        use oc_host::vst3_instance::{Vst3Operation, Vst3ParameterRefusal, Vst3ProcessError};
        use vst3::Steinberg::Vst::RestartFlags_;
        fn item(name: &str, value: f64, domain: ParameterWriteDomain) -> AuthoredParameter<'_> {
            AuthoredParameter {
                name,
                value,
                domain,
            }
        }
        fn configured(mode: u32) -> NativeFixture {
            let mut fixture = NativeFixture::new(false);
            fixture.configure_parameters_for_test(mode);
            fixture
        }
        fn apply(
            fixture: &mut NativeFixture,
            items: &[AuthoredParameter<'_>],
        ) -> Result<(), ParameterApplyError> {
            let mut owner = fixture
                .instance_mut()
                .authored_parameter_owner()
                .map_err(|kind| ParameterApplyError {
                    format: oc_host::PluginFormat::Vst3,
                    parameter: "<inventory>".into(),
                    kind,
                })?;
            owner.apply_authored_parameters(items)
        }
        assert!(is_main_thread());
        // Independent generated healthy COM calls prove native metadata/conversion
        // and setter slots before the checked backend's exception/refusal cases.
        let abi = configured(1);
        let raw = abi.instance().raw_interfaces();
        // SAFETY: genuine native fixture controller stays live on the process UI
        // thread, throw mode is off, and each stack metadata pointer is writable.
        unsafe {
            let controller = ComRef::<IEditController>::from_raw(raw.controller.cast()).unwrap();
            assert_eq!(controller.getParameterCount(), 8);
            let mut info: ParameterInfo = std::mem::zeroed();
            assert_eq!(controller.getParameterInfo(0, &mut info), kResultOk);
            assert_eq!((info.id, info.flags), (100, 0));
            assert_eq!(controller.normalizedParamToPlain(100, 0.25), 25.0);
            assert_eq!(controller.plainParamToNormalized(100, 75.0), 0.75);
            assert_eq!(controller.setParamNormalized(100, 0.2), kResultOk);
        }
        assert_eq!(
            (
                abi.calls(37),
                abi.calls(38),
                abi.calls(39),
                abi.calls(40),
                abi.calls(41)
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(abi.parameter_value_for_test(100), 0.2);

        let mut aliases = configured(1);
        apply(
            &mut aliases,
            &[
                item("Gain", f64::NAN, Normalized),
                item("Gain Amount", 0.75, Normalized),
            ],
        )
        .unwrap();
        assert_eq!(
            aliases.calls(41),
            1,
            "same native ID aliases share effective last assignment"
        );
        assert_eq!(aliases.parameter_value_for_test(100), 0.75);
        assert_eq!(aliases.pending_parameter_count_for_test(), 1);
        let mut hidden = configured(1);
        apply(
            &mut hidden,
            &[
                item("Hidden Depth", -12.0, NativePlain),
                item("明るさ", 0.4, Normalized),
            ],
        )
        .unwrap();
        assert_eq!(hidden.parameter_value_for_test(101), 0.0);
        assert_eq!(hidden.parameter_value_for_test(107), 0.4);
        for (name, value, domain) in [
            ("unknown", 0.2, Normalized),
            ("Rate Hz", 0.2, Normalized),
            ("Read Only", 0.2, Normalized),
            ("Gain", f64::NAN, Normalized),
            ("Gain", f64::INFINITY, NativePlain),
            ("Gain", -0.01, Normalized),
            ("Gain", 1.01, Normalized),
            ("Gain", -0.01, NativePlain),
            ("Gain", 100.01, NativePlain),
            ("Bad Plain Range", 0.5, NativePlain),
        ] {
            let mut fixture = configured(1);
            let error = apply(
                &mut fixture,
                &[
                    item("Hidden Depth", 0.5, Normalized),
                    item(name, value, domain),
                ],
            )
            .unwrap_err();
            match name {
                "unknown" => assert!(matches!(error.kind, ParameterErrorKind::Unknown)),
                "Rate Hz" => assert!(matches!(error.kind, ParameterErrorKind::Ambiguous)),
                "Read Only" => assert!(matches!(error.kind, ParameterErrorKind::ReadOnly)),
                "Bad Plain Range" => {
                    assert!(matches!(error.kind, ParameterErrorKind::InvalidMetadata))
                }
                _ => assert!(matches!(
                    error.kind,
                    ParameterErrorKind::NonFinite | ParameterErrorKind::OutOfRange { .. }
                )),
            }
            assert_eq!(
                fixture.calls(41),
                0,
                "a later invalid assignment must prevent the first native setter"
            );
            assert_eq!(fixture.pending_parameter_count_for_test(), 0);
        }
        // Normalized authoring does not depend on a broken native plain range.
        let mut normalized = configured(1);
        apply(&mut normalized, &[item("Bad Plain Range", 0.5, Normalized)]).unwrap();
        for (value, domain, expected) in [
            (0.0, Normalized, 0.0),
            (1.0, Normalized, 1.0),
            (-0.0, LegacyVst3Auto, 0.0),
            (1.0, LegacyVst3Auto, 1.0),
            (25.0, LegacyVst3Auto, 0.25),
            (1.01, LegacyVst3Auto, 0.0101),
            (100.0, NativePlain, 1.0),
        ] {
            let mut fixture = configured(1);
            apply(&mut fixture, &[item("Gain", value, domain)]).unwrap();
            assert!((fixture.parameter_value_for_test(100) - expected).abs() < 1e-12);
        }
        let mut reversed = configured(4);
        apply(&mut reversed, &[item("Gain", 25.0, NativePlain)]).unwrap();
        assert_eq!(reversed.parameter_value_for_test(100), 0.75);
        for mode in [2, 3] {
            let mut fixture = configured(mode);
            assert!(matches!(
                apply(&mut fixture, &[item("Gain", 25.0, NativePlain)])
                    .unwrap_err()
                    .kind,
                ParameterErrorKind::Unrepresentable
            ));
            assert_eq!(fixture.calls(41), 0);
        }
        // A private key cannot be replayed against a new preparation, even on
        // the same plugin instance with the same native parameter ID.
        let mut bindings = configured(1);
        let key = {
            let mut owner = bindings.instance_mut().authored_parameter_owner().unwrap();
            owner.resolve_authored_parameter("Gain").unwrap().key
        };
        let mut owner = bindings.instance_mut().authored_parameter_owner().unwrap();
        assert!(matches!(
            owner.write_bound_parameter(&key, 0.2, Normalized),
            Err(ParameterErrorKind::InventoryChanged)
        ));
        drop(owner);
        assert_eq!(bindings.calls(41), 0);

        for point in [38, 41] {
            let mut fixture = configured(1);
            fixture.set_fault(point, 4);
            let error = apply(&mut fixture, &[item("Gain", 0.2, Normalized)]).unwrap_err();
            match error.kind {
                ParameterErrorKind::Native { source, .. } => {
                    let refusal = source
                        .downcast_ref::<Vst3ParameterRefusal>()
                        .expect("typed native refusal is retained");
                    assert_eq!(refusal.result, kResultFalse);
                    assert_eq!(
                        refusal.operation,
                        if point == 38 {
                            Vst3Operation::ParameterInfo
                        } else {
                            Vst3Operation::SetNormalized
                        }
                    );
                }
                other => panic!("unexpected refusal: {other}"),
            }
            assert!(
                !fixture.instance().failed(),
                "ordinary refusal is distinct from exception quarantine"
            );
            assert_eq!(fixture.pending_parameter_count_for_test(), 0);
        }
        for point in [37, 38, 39, 40, 41] {
            for kind in [1, 2, 3] {
                let mut fixture = configured(1);
                fixture.set_fault(point, kind);
                let error = apply(&mut fixture, &[item("Gain", 25.0, NativePlain)]).unwrap_err();
                let native = match &error.kind {
                    ParameterErrorKind::Native { source, .. } => {
                        source.downcast_ref::<Vst3ProcessError>().unwrap()
                    }
                    other => panic!("unexpected native failure {other}"),
                };
                assert_eq!(
                    native.operation,
                    match point {
                        37 => Vst3Operation::ParameterCount,
                        38 => Vst3Operation::ParameterInfo,
                        39 => Vst3Operation::NormalizedToPlain,
                        40 => Vst3Operation::PlainToNormalized,
                        _ => Vst3Operation::SetNormalized,
                    }
                );
                assert!(native.is_exception());
                if kind == 1 {
                    assert!(
                        error
                            .to_string()
                            .contains("optimized native VST3 method exception")
                    );
                }
                if kind == 3 {
                    assert!(native.to_string().len() < 400);
                }
                assert!(fixture.instance().failed());
                assert_eq!(fixture.pending_parameter_count_for_test(), 0);
                fixture.retire();
                assert_eq!(fixture.calls(11), 0);
            }
        }
        // Inventory epochs never consume UI notifications. Conversion callbacks
        // invalidate all prepared writes; setter callbacks stop the next setter.
        for point in [37, 38, 39, 40, 41] {
            let mut fixture = configured(1);
            fixture.parameter_restart_for_test(point, RestartFlags_::kParamTitlesChanged);
            let error = apply(
                &mut fixture,
                &[
                    item("Gain", 25.0, NativePlain),
                    item("Hidden Depth", 0.5, Normalized),
                ],
            )
            .unwrap_err();
            assert!(
                matches!(error.kind, ParameterErrorKind::InventoryChanged),
                "{error}"
            );
            assert_eq!(fixture.calls(41), if point == 41 { 1 } else { 0 });
            assert_eq!(fixture.pending_parameter_count_for_test(), 0);
            assert_eq!(
                fixture.take_parameter_restart_for_test(),
                RestartFlags_::kParamTitlesChanged
            );
            assert_eq!(fixture.take_parameter_restart_for_test(), 0);
            fixture.parameter_restart_for_test(0, 0);
            apply(&mut fixture, &[item("Gain", 0.6, Normalized)]).unwrap();
        }
        let mut value_restart = configured(1);
        value_restart.parameter_restart_for_test(41, RestartFlags_::kParamValuesChanged);
        apply(
            &mut value_restart,
            &[
                item("Gain", 0.2, Normalized),
                item("Hidden Depth", 0.3, Normalized),
            ],
        )
        .unwrap();
        assert_eq!(
            value_restart.calls(41),
            2,
            "value notifications do not invalidate native IDs"
        );
        assert_eq!(
            value_restart.take_parameter_restart_for_test(),
            RestartFlags_::kParamValuesChanged
        );
        println!("VST3 checked authored parameter UI-owner cases: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_cancels_on_unwind_and_completes_on_main() {
        use oc_host::vst3_instance::validate_deferred_attach_delay_for_test;
        for invalid in [
            "-10000", "-1", "NaN", "inf", "-inf", "1e308", "30001", "invalid",
        ] {
            assert!(
                validate_deferred_attach_delay_for_test(Some(invalid)).is_err(),
                "accepted {invalid}"
            );
        }
        assert_eq!(
            validate_deferred_attach_delay_for_test(Some("30000")).unwrap(),
            Duration::from_secs(30)
        );
        let Harness { handle, calls, .. } = harness(ViewOptions::default());
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = handle.open_deferred_for_test(
                "unwind after schedule",
                Some("0"),
                None,
                Some(Box::new(|| {
                    panic!("controlled unwind after scheduling editor attach")
                })),
            );
        }));
        assert!(unwound.is_err());
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.1));
        assert!(!handle.is_open());
        assert_eq!(
            calls
                .snapshot()
                .iter()
                .filter(|&&call| call == "attached")
                .count(),
            0
        );
        handle
            .open_deferred_for_test("complete deferred attach", Some("0"), None, None)
            .unwrap();
        assert!(handle.is_open());
        assert_eq!(
            calls
                .snapshot()
                .iter()
                .filter(|&&call| call == "attached")
                .count(),
            1
        );
        handle.close().unwrap();
        println!("VST3 deferred attach invalid-delay/unwind/success cases: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_contains_panics_before_objc_callback_returns() {
        let Harness { handle, calls, .. } = harness(ViewOptions::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle.open_deferred_for_test(
                "panic inside scheduled ObjC callback",
                Some("0"),
                Some(Box::new(|| panic!("controlled before-dispatch panic"))),
                None,
            )
        }));
        let error = result
            .expect("callback panic must not unwind through ObjC into its Rust caller")
            .unwrap_err();
        assert!(error.contains("host callback panicked"));
        assert_eq!(
            calls
                .snapshot()
                .iter()
                .filter(|&&call| call == "attached")
                .count(),
            0
        );
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.1));
        assert_eq!(
            calls
                .snapshot()
                .iter()
                .filter(|&&call| call == "attached")
                .count(),
            0,
            "no stale selector dispatch after callback panic"
        );
        assert!(!handle.is_open());
        let retry = handle
            .open_hidden("quarantined after callback panic")
            .unwrap_err();
        assert_eq!(retry, error, "first callback fault remains authoritative");
        println!("VST3 before-dispatch panic remains inside ObjC callback boundary: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_reentrant_close_cancels_without_reopening() {
        let Harness { handle, calls, .. } = harness(ViewOptions::default());
        let close_handle = handle.clone();
        let close_result = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&close_result);
        let result = handle.open_deferred_for_test(
            "close in nested run loop",
            Some("0"),
            Some(Box::new(move || {
                *observed.lock().unwrap() = Some(close_handle.close());
            })),
            None,
        );
        assert!(result.unwrap_err().contains("closed while attaching"));
        assert!(close_result.lock().unwrap().as_ref().unwrap().is_ok());
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        assert!(!handle.is_open());
        assert!(handle.window_number().is_none());
        assert_eq!(
            calls
                .snapshot()
                .iter()
                .filter(|&&call| call == "attached")
                .count(),
            0
        );
        handle
            .open_hidden("explicit reopen after canceled attach")
            .unwrap();
        assert!(handle.is_open());
        handle.close().unwrap();
        println!("VST3 nested run-loop close cancels attach without republishing: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_close_inside_native_attach_balances_rollback() {
        let Harness {
            handle,
            calls,
            attach_action,
            ..
        } = harness(ViewOptions::default());
        let close_handle = handle.clone();
        let close_result = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&close_result);
        *attach_action.lock().unwrap() = Some(Box::new(move || {
            *observed.lock().unwrap() = Some(close_handle.close());
        }));
        let result =
            handle.open_deferred_for_test("close inside native attached", Some("0"), None, None);
        assert!(result.unwrap_err().contains("closed while attaching"));
        assert!(close_result.lock().unwrap().as_ref().unwrap().is_ok());
        assert!(!handle.is_open());
        assert!(handle.window_number().is_none());
        let order = calls.snapshot();
        assert_eq!(order.iter().filter(|&&call| call == "attached").count(), 1);
        assert_eq!(
            order.iter().filter(|&&call| call == "removed").count(),
            1,
            "successful native attach is balanced before parent/window release"
        );
        println!("VST3 close from native attached balances canceled transaction: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_registry_loan_survives_reentrant_retirement() {
        use oc_host::vst3_instance::native_fixture::NativeFixture;
        use oc_host::vst3_instance::{
            exercise_deferred_attach_for_test, retire_registered_editor_view_for_test,
        };
        let mut fixture = NativeFixture::new(false);
        let handle = fixture.editor_lease_for_test().unwrap();
        let key = fixture.register_main_view_for_test().unwrap();
        let fixture = std::rc::Rc::new(fixture);
        let retained_fixture = std::rc::Rc::clone(&fixture);
        let release_during_retirement = Arc::new(AtomicUsize::new(usize::MAX));
        let observed = Arc::clone(&release_during_retirement);
        let retirement_result = Arc::new(Mutex::new(None));
        let retired = Arc::clone(&retirement_result);
        // SAFETY: this controlled native fixture view is registered but never
        // attached; its test ABI accepts the temporary host NSView parent.
        let result = unsafe {
            exercise_deferred_attach_for_test(
                &handle,
                Some("0"),
                Some(Box::new(move || {
                    *retired.lock().unwrap() = Some(retire_registered_editor_view_for_test(key));
                    observed.store(retained_fixture.calls(36) as usize, Ordering::Release);
                })),
            )
        };
        assert!(result.unwrap_err().contains("closed while attaching"));
        assert!(retirement_result.lock().unwrap().as_ref().unwrap().is_ok());
        assert_eq!(
            release_during_retirement.load(Ordering::Acquire),
            0,
            "main registry loan pins native view during callback"
        );
        assert_eq!(
            fixture.calls(29),
            0,
            "retired view is never attached after cancellation"
        );
        assert_eq!(
            fixture.calls(36),
            1,
            "loan releases only after nested callback and scheduler exit"
        );
        println!("VST3 registered-view loan pins backing across nested retirement: ok");
    }

    #[cfg(feature = "test-support")]
    fn deferred_attach_preserves_native_refusal_and_quarantines_faults() {
        use oc_host::vst3_instance::native_fixture::NativeFixture;
        use oc_host::vst3_instance::{
            exercise_deferred_attach_for_test, retire_registered_editor_view_for_test,
        };
        assert!(is_main_thread());
        for kind in [0, 4, 1, 2] {
            let mut fixture = NativeFixture::new(false);
            let handle = fixture.editor_lease_for_test().unwrap();
            let key = fixture.register_main_view_for_test().unwrap();
            fixture.set_fault(29, kind);
            let failure = fixture.instance().failure();
            // SAFETY: registered controlled fixture view is initially unattached;
            // helper balances successful attach before releasing its temporary parent.
            let result = unsafe { exercise_deferred_attach_for_test(&handle, Some("0"), None) };
            assert_eq!(fixture.calls(29), 1);
            if kind == 0 {
                assert_eq!(result.unwrap(), kResultOk);
                assert_eq!(
                    fixture.calls(31),
                    1,
                    "successful fixture attach is balanced by native removed"
                );
                assert!(!failure.failed());
            } else if kind == 4 {
                assert_eq!(result.unwrap(), kResultFalse);
                assert!(!failure.failed());
                fixture.set_fault(29, 0);
            } else {
                assert!(result.is_err());
                let first = failure.first_fault().unwrap();
                assert!(
                    // SAFETY: this same controlled fixture is quarantined; the
                    // helper refuses before another native call or parent creation.
                    unsafe { exercise_deferred_attach_for_test(&handle, Some("0"), None) }.is_err()
                );
                assert_eq!(fixture.calls(29), 1);
                assert_eq!(failure.first_fault(), Some(first));
            }
            retire_registered_editor_view_for_test(key).unwrap();
            if kind == 1 || kind == 2 {
                assert_eq!(fixture.calls(36), 0);
            }
            drop(handle);
        }
        println!("VST3 deferred attach native success/refusal/exception/quarantine cases: ok");
    }

    #[cfg(feature = "test-support")]
    fn normal_close_native_failure_retains_complete_backing() {
        use oc_host::vst3_instance::native_fixture::NativeFixture;
        for (point, kind) in [(31, 1), (31, 2), (31, 4), (46, 1), (46, 4), (11, 1)] {
            let mut fixture = NativeFixture::new(false);
            let handle = fixture.editor_lease_for_test().unwrap();
            fixture.register_main_view_for_test().unwrap();
            let backing = handle.backing_probe_for_test().unwrap();
            let failure = fixture.instance().failure();
            let releases_before = fixture.calls(11);
            fixture.set_fault(point, kind);
            assert!(
                handle.close().is_err(),
                "normal close must report native point {point} kind {kind}"
            );
            let first = failure.first_fault().unwrap();
            assert_eq!(fixture.calls(31), 1);
            assert_eq!(
                fixture.calls(46),
                u32::from(point != 31),
                "setFrame never follows failed removed"
            );
            assert_eq!(
                fixture.calls(36),
                u32::from(point == 11),
                "view Release never follows detach/frame failure"
            );
            let releases_after = fixture.calls(11);
            assert_eq!(releases_after - releases_before, u32::from(point == 11));
            assert!(handle.close().is_err());
            drop(handle);
            fixture.retire_instance_on_worker_for_test();
            assert!(
                backing.alive(),
                "quarantine retains actual registered frame/controller/handles, not just native view"
            );
            assert_eq!(
                fixture.calls(11),
                releases_after,
                "no controller or other foreign release follows first fault"
            );
            assert_eq!(failure.first_fault(), Some(first));
        }
        println!("VST3 normal-close removed/frame/release failures retain full backing: ok");
    }

    pub fn run() {
        #[cfg(feature = "test-support")]
        {
            native_checked_authored_parameters();
            if std::env::args().any(|arg| arg == "--authored-parameters-only") {
                return;
            }
        }
        #[cfg(feature = "test-support")]
        native_editor_reference_retirement_stays_on_main();
        #[cfg(feature = "test-support")]
        {
            deferred_attach_cancels_on_unwind_and_completes_on_main();
            deferred_attach_contains_panics_before_objc_callback_returns();
            deferred_attach_reentrant_close_cancels_without_reopening();
            deferred_attach_close_inside_native_attach_balances_rollback();
            deferred_attach_registry_loan_survives_reentrant_retirement();
            deferred_attach_preserves_native_refusal_and_quarantines_faults();
            normal_close_native_failure_retains_complete_backing();
        }
        default_editor_lifecycle();
        uninitialized_size_before_attach();
        resize_during_attach_wins();
        degenerate_resize_is_refused();
        reattach_reapplies_the_window_size();
        reattach_with_uninitialized_size_keeps_the_window_size();
        dispatch_reaches_the_main_thread();
        println!("vst3 editor lifecycle: ok");
    }
}
