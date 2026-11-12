//! These callbacks execute optimized C++ code through the real CLAP C signatures.
use super::*;
use crate::clap_guard::FaultKind;
use std::sync::atomic::AtomicBool;

#[link(name = "oc_clap_guard_fixture", kind = "static")]
unsafe extern "C" {
    fn oc_clap_fixture_reset(point: u32);
    fn oc_clap_fixture_check_audio(
        callback: Option<unsafe extern "C" fn(*const clap_host) -> bool>,
        host: *const clap_host,
    );
    fn oc_clap_fixture_audio_mask() -> u32;
    fn oc_clap_fixture_on_main(plugin: *const clap_plugin);
    fn oc_clap_fixture_reset_plugin(plugin: *const clap_plugin);
    fn oc_clap_fixture_count() -> u32;
    fn oc_clap_fixture_event(index: u32) -> u32;
    fn oc_clap_fixture_callback(
        callback: Option<unsafe extern "C" fn(*const clap_host)>,
        host: *const clap_host,
    );
    fn oc_clap_fixture_init(plugin: *const clap_plugin) -> bool;
    fn oc_clap_fixture_activate(
        plugin: *const clap_plugin,
        rate: f64,
        minimum: u32,
        maximum: u32,
    ) -> bool;
    fn oc_clap_fixture_start(plugin: *const clap_plugin) -> bool;
    fn oc_clap_fixture_process(plugin: *const clap_plugin, process: *const clap_process) -> i32;
    fn oc_clap_fixture_stop(plugin: *const clap_plugin);
    fn oc_clap_fixture_deactivate(plugin: *const clap_plugin);
    fn oc_clap_fixture_destroy(plugin: *const clap_plugin);
    fn oc_clap_fixture_extension(
        plugin: *const clap_plugin,
        id: *const std::ffi::c_char,
    ) -> *const c_void;
    fn oc_clap_fixture_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool;
    fn oc_clap_fixture_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool;
    fn oc_clap_fixture_gui_api(
        plugin: *const clap_plugin,
        api: *const std::ffi::c_char,
        floating: bool,
    ) -> bool;
    fn oc_clap_fixture_gui_size(
        plugin: *const clap_plugin,
        width: *mut u32,
        height: *mut u32,
    ) -> bool;
    fn oc_clap_fixture_param_value(plugin: *const clap_plugin, id: u32, value: *mut f64) -> bool;
    fn oc_clap_fixture_ports_count(plugin: *const clap_plugin, input: bool) -> u32;
}

fn events() -> Vec<u32> {
    // SAFETY: the fixture's per-thread bounded counters are readable on this thread.
    let count = unsafe { oc_clap_fixture_count() };
    (0..count)
        .map(|index| {
            // SAFETY: index is below the fixture's recorded count.
            unsafe { oc_clap_fixture_event(index) }
        })
        .collect()
}

struct Fixture {
    instance: Option<ClapInstance>,
    _plugin: Box<clap_plugin>,
}
impl Fixture {
    fn new(fault_at: u32) -> Self {
        // SAFETY: resetting this thread's fixture has no callbacks or foreign throws.
        unsafe { oc_clap_fixture_reset(fault_at) };
        let failure = Arc::new(ClapFailure::default());
        let mut host = Box::new(ClapHostData {
            failure: Arc::clone(&failure),
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: c"Fixture".as_ptr(),
                vendor: c"Tests".as_ptr(),
                url: c"https://example.test".as_ptr(),
                version: c"1".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            main_thread: std::thread::current().id(),
            audio_thread: std::sync::atomic::AtomicPtr::new(ptr::null_mut()),
            audio_busy: AtomicBool::new(false),
            main_callback_pending: AtomicBool::new(false),
            gui_close_pending: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            params_changed: AtomicBool::new(false),
        });
        host.host.host_data = host.as_ref() as *const ClapHostData as *mut c_void;
        let plugin = Box::new(clap_plugin {
            desc: ptr::null(),
            plugin_data: ptr::null_mut(),
            init: Some(oc_clap_fixture_init),
            destroy: Some(oc_clap_fixture_destroy),
            activate: Some(oc_clap_fixture_activate),
            deactivate: Some(oc_clap_fixture_deactivate),
            start_processing: Some(oc_clap_fixture_start),
            stop_processing: Some(oc_clap_fixture_stop),
            reset: None,
            process: Some(oc_clap_fixture_process),
            get_extension: Some(oc_clap_fixture_extension),
            on_main_thread: None,
        });
        let gui_session = Arc::new(Mutex::new(GuiSession {
            failure: Arc::clone(&failure),
            ..GuiSession::default()
        }));
        let instance = ClapInstance {
            plugin: plugin.as_ref(),
            bundle_path: "fixture.clap".into(),
            plugin_id: "fixture".into(),
            core: ClapCore::new(
                plugin.as_ref(),
                host,
                Arc::clone(&failure),
                Arc::clone(&gui_session),
                None,
            ),
            failure: Arc::clone(&failure),
            sample_rate: 48_000,
            max_frames: 512,
            active: false,
            processing: false,
            audio_ports: AudioPortLayout {
                inputs: Vec::new(),
                outputs: vec![AudioPortInfo {
                    channels: 2,
                    flags: 0,
                }],
            },
            sidechain_input_index: 0,
            pending_events: Vec::new(),
            event_scratch: Vec::new(),
            event_header_scratch: Vec::new(),
            gui_session,
            param_cache: ParamCache::default(),
            prepared_tail: None,
            prepared_latency: 0,
            prepared_gui: 0,
            prepared_midi1: false,
            prepared_parameters: Vec::new(),
        };
        Self {
            instance: Some(instance),
            _plugin: plugin,
        }
    }

    fn run(&mut self) -> Result<(), String> {
        let instance = self.instance.as_mut().unwrap();
        // SAFETY: the fixture callback and the boxed descriptor are live throughout the call.
        instance
            // SAFETY: The test owns its initialized fixture descriptor and callback storage throughout this synchronous exact-signature guarded call.
            .foreign(ClapOperation::Init, || unsafe {
                clap_guard::plugin_bool(oc_clap_fixture_init, instance.plugin)
            })
            .map_err(|fault| fault.describe_for("CLAP"))?;
        instance.activate()?;
        instance.process_block_outputs_into(8, None, &[0], &mut [Vec::with_capacity(16)])
    }

    fn retire(&mut self) {
        if let Some(instance) = self.instance.take() {
            let failed = Arc::clone(&instance.failure);
            let host = instance.core.host.as_ref() as *const ClapHostData as *mut ClapHostData;
            drop(instance);
            if failed.failed() {
                // SAFETY: this synchronous fixture never retains callbacks or
                // starts workers. Only the test may reclaim quarantined host data.
                unsafe { drop(Box::from_raw(host)) };
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.retire();
    }
}

#[test]
fn typed_clap_foreign_lifecycle_faults_stop_calls_and_keep_bounded_diagnostics() {
    for point in 1..=7 {
        let mut fixture = Fixture::new(point);
        let result = fixture.run();
        if point <= 4 {
            assert!(result.is_err(), "point {point}");
            let instance = fixture.instance.as_mut().unwrap();
            assert_eq!(instance.failure().unwrap().kind, FaultKind::CppException);
            assert!(
                !instance.reset(),
                "a failed instance must not re-enter its plugin"
            );
        } else {
            result.unwrap();
        }
        let failure = fixture.instance.as_ref().unwrap().failure_handle();
        fixture.retire();
        let expected = [
            ClapOperation::Init,
            ClapOperation::Activate,
            ClapOperation::StartProcessing,
            ClapOperation::Process,
            ClapOperation::StopProcessing,
            ClapOperation::Deactivate,
            ClapOperation::Destroy,
        ][point as usize - 1];
        assert_eq!(failure.take_report().unwrap().operation, expected);
        assert!(failure.take_report().is_none());
        assert_eq!(events(), (1..=point).collect::<Vec<_>>(), "point {point}");
    }
    assert!(std::mem::size_of::<Fault>() <= 264);
}

#[test]
fn typed_clap_host_callback_panics_return_through_cpp_without_unwinding() {
    let mut fixture = Fixture::new(0);
    let instance = fixture.instance.as_ref().unwrap();
    // SAFETY: the fixture retains this host only for the synchronous process call.
    unsafe { oc_clap_fixture_callback(Some(host_request_callback), &instance.core.host.host) };
    clap_guard::panic_on_next_callback();
    assert!(fixture.run().is_err());
    assert_eq!(
        fixture.instance.as_ref().unwrap().failure().unwrap().kind,
        FaultKind::CallbackPanic
    );
    let failure = fixture.instance.as_ref().unwrap().failure_handle();
    assert_eq!(
        failure.first_fault().unwrap().operation,
        ClapOperation::HostCallback
    );
    fixture.retire();
    assert!(
        failure.failed(),
        "the failure handle survives instance retirement"
    );
    assert_eq!(
        failure.take_report().unwrap().operation,
        ClapOperation::HostCallback
    );
    assert!(failure.take_report().is_none());
    assert_eq!(events(), [1, 2, 3, 4]);
}

#[test]
fn typed_clap_state_guards_bound_long_messages_and_catch_unknown_exceptions() {
    let fixture = Fixture::new(208);
    let plugin = fixture.instance.as_ref().unwrap().plugin;
    let stream = clap_istream {
        ctx: ptr::null_mut(),
        read: None,
    };
    // SAFETY: this fixture's load callback uses no stream fields and may throw in C++.
    let fault = unsafe { clap_guard::load(oc_clap_fixture_load, plugin, &stream) }.unwrap_err();
    assert_eq!(fault.kind, FaultKind::CppException);
    assert!(fault.describe_for("CLAP").len() < 300);
    // SAFETY: resetting this thread's fixture does not affect a running callback.
    unsafe { oc_clap_fixture_reset(109) };
    let stream = clap_ostream {
        ctx: ptr::null_mut(),
        write: None,
    };
    // SAFETY: the fake save callback uses the correct CLAP stream ABI.
    let fault = unsafe { clap_guard::save(oc_clap_fixture_save, plugin, &stream) }.unwrap_err();
    assert_eq!(fault.kind, FaultKind::UnknownException);
}

#[test]
fn typed_clap_gui_and_parameter_signatures_preserve_outputs_and_boolean_arguments() {
    let fixture = Fixture::new(0);
    let plugin = fixture.instance.as_ref().unwrap().plugin;
    let (mut width, mut height, mut value) = (0, 0, 0.0);
    // SAFETY: the exact native signatures receive valid borrowed output storage.
    unsafe {
        assert!(
            clap_guard::gui_api(oc_clap_fixture_gui_api, plugin, c"test".as_ptr(), true).unwrap()
        );
        assert!(
            !clap_guard::gui_api(oc_clap_fixture_gui_api, plugin, c"test".as_ptr(), false).unwrap()
        );
        assert!(
            clap_guard::gui_size(oc_clap_fixture_gui_size, plugin, &mut width, &mut height)
                .unwrap()
        );
        assert!(
            clap_guard::param_value(oc_clap_fixture_param_value, plugin, 2, &mut value).unwrap()
        );
        assert_eq!(
            clap_guard::ports_count(oc_clap_fixture_ports_count, plugin, true).unwrap(),
            1
        );
        assert_eq!(
            clap_guard::ports_count(oc_clap_fixture_ports_count, plugin, false).unwrap(),
            2
        );
    }
    assert_eq!((width, height, value), (123, 45, 2.25));
}

#[test]
fn typed_clap_process_error_status_is_reported_and_prevents_reentry() {
    let mut fixture = Fixture::new(304);
    assert!(!AudioEffect::processing_failed(
        fixture.instance.as_ref().unwrap()
    ));
    assert_eq!(
        AudioEffect::take_processing_failure(fixture.instance.as_ref().unwrap()),
        oc_audio::ProcessingFailureReport::Unavailable,
    );
    assert!(fixture.run().is_err());
    let instance = fixture.instance.as_ref().unwrap();
    assert!(AudioEffect::processing_failed(instance));
    let oc_audio::ProcessingFailureReport::Ready(report) =
        AudioEffect::take_processing_failure(instance)
    else {
        panic!("the first concrete CLAP cause must be ready");
    };
    assert!(report.starts_with("CLAP Process:"));
    assert!(report.contains("processing error"));
    assert_eq!(
        AudioEffect::take_processing_failure(instance),
        oc_audio::ProcessingFailureReport::Reported,
    );
    assert!(AudioEffect::processing_failed(instance));
    assert_eq!(
        fixture.instance.as_ref().unwrap().failure().unwrap().kind,
        FaultKind::ProcessError
    );
    fixture.retire();
    assert_eq!(events(), [1, 2, 3, 4]);
}

#[test]
fn asynchronous_host_callback_fault_publishes_without_a_foreign_call_on_the_owner() {
    let mut fixture = Fixture::new(0);
    let instance = fixture.instance.as_ref().unwrap();
    let failure = instance.failure_handle();
    let host = &instance.core.host.host as *const clap_host as usize;
    std::thread::spawn(move || {
        clap_guard::panic_on_next_callback();
        // SAFETY: this fixture retains the boxed host until the joined worker
        // returns; request_process is a CLAP thread-safe host callback.
        unsafe { host_request_process(host as *const clap_host) };
    })
    .join()
    .unwrap();
    assert!(failure.failed());
    assert_eq!(
        failure.first_fault().unwrap().operation,
        ClapOperation::HostCallback
    );
    assert_eq!(
        failure.first_fault().unwrap().fault.kind,
        FaultKind::CallbackPanic
    );
    fixture.retire();
    assert_eq!(
        failure.take_report().unwrap().operation,
        ClapOperation::HostCallback
    );
    assert!(failure.take_report().is_none());
    assert!(
        events().is_empty(),
        "no plugin lifecycle callback is admitted after failure"
    );
}

#[test]
fn queued_gui_lease_retains_native_core_and_cancels_open_after_audio_owner_retires() {
    let mut fixture = Fixture::new(0);
    fixture.run().unwrap();
    let instance = fixture.instance.take().unwrap();
    let gui = GuiPtrs {
        plugin: instance.plugin,
        // A cancelled open must reject before consulting this absent extension.
        gui: ptr::null(),
        session: Arc::clone(&instance.gui_session),
        owner: GuiOwner::Instance(Arc::clone(&instance.core)),
    };
    drop(instance);
    assert_eq!(events(), [1, 2, 3, 4, 5]);
    assert!(gui.open().unwrap_err().contains("retired"));
    assert_eq!(events(), [1, 2, 3, 4, 5]);
    drop(gui);
    assert_eq!(events(), [1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn symbolic_audio_admission_brackets_start_process_reset_stop_and_clears_after_each_call() {
    let mut fixture = Fixture::new(0);
    fixture._plugin.reset = Some(oc_clap_fixture_reset_plugin);
    let instance = fixture.instance.as_ref().unwrap();
    let host = &instance.core.host.host;
    // SAFETY: this synchronous fixture retains the host until all native checks finish.
    unsafe { oc_clap_fixture_check_audio(Some(host_is_audio_thread), host) };
    fixture.run().unwrap();
    let instance = fixture.instance.as_mut().unwrap();
    assert!(instance.reset());
    // SAFETY: The test retains its instance/Core and boxed host descriptor throughout this synchronous thread-role query.
    assert!(!unsafe { host_is_audio_thread(&instance.core.host.host) });
    fixture.retire();
    // SAFETY: the native fixture retains only its bounded thread-local counters now.
    assert_eq!(
        // SAFETY: The linked optimized fixture export has this exact no-argument u32 ABI and reads only this test thread's counters.
        unsafe { oc_clap_fixture_audio_mask() },
        (1 << 3) | (1 << 4) | (1 << 5) | (1 << 14)
    );
}

#[test]
fn main_callback_request_is_separate_from_parameter_flush_and_serviced_once() {
    use crate::clap_main::MainService;
    let mut fixture = Fixture::new(0);
    fixture._plugin.on_main_thread = Some(oc_clap_fixture_on_main);
    fixture.run().unwrap();
    let instance = fixture.instance.as_ref().unwrap();
    // SAFETY: the fixture retains this host descriptor throughout the request.
    unsafe { host_request_callback(&instance.core.host.host) };
    assert!(!instance.needs_flush());
    assert!(
        instance
            .core
            .host
            .main_callback_pending
            .load(std::sync::atomic::Ordering::Acquire)
    );
    let executor = crate::clap_main::ClapMainExecutor::fixture_current(|_| unreachable!());
    let token = executor.enter().unwrap();
    instance.core.service_main(&token);
    instance.core.service_main(&token);
    assert_eq!(events(), [1, 2, 3, 4, 15]);
    assert!(!instance.needs_flush());
    assert!(
        !instance
            .core
            .host
            .main_callback_pending
            .load(std::sync::atomic::Ordering::Acquire)
    );
}

#[test]
fn symbolic_audio_owner_can_migrate_workers_without_retaining_the_old_thread_marker() {
    let mut fixture = Fixture::new(0);
    fixture.run().unwrap();
    let instance = fixture.instance.take().unwrap();
    let (instance, mask, cleared) = std::thread::spawn(move || {
        let mut instance = instance;
        // SAFETY: this worker owns the instance and retains its descriptor during checks.
        unsafe {
            oc_clap_fixture_check_audio(Some(host_is_audio_thread), &instance.core.host.host)
        };
        instance
            .process_block_outputs_into(8, None, &[0], &mut [Vec::with_capacity(16)])
            .unwrap();
        // SAFETY: thread-local native counters and the retained host are live here.
        let mask = unsafe { oc_clap_fixture_audio_mask() };
        // SAFETY: The test retains its instance/Core and boxed host descriptor throughout this synchronous thread-role query.
        let cleared = !unsafe { host_is_audio_thread(&instance.core.host.host) };
        (instance, mask, cleared)
    })
    .join()
    .unwrap();
    assert_eq!(mask, 1 << 4);
    assert!(cleared);
    // SAFETY: The test retains its instance/Core and boxed host descriptor throughout this synchronous thread-role query.
    assert!(!unsafe { host_is_audio_thread(&instance.core.host.host) });
    fixture.instance = Some(instance);
}
