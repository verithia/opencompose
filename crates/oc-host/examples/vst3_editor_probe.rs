//! Headless probe for creating and attaching a VST3 plugin's editor view.
//!
//! `cargo run -p oc-host --example vst3_editor_probe --features test-support --
//! --plugin <bundle> [--context] [--handler] [--attach [--hidden]]
//! [--variant <name>] [--no-state] [--roundtrip] [--restore-state]
//! [--restore-both] [--no-activate] [--preset <preset>] [--controller-state]
//! [--controller-only] [--state-before-activate] [--getstate] [--process <n>]
//! [--hold <seconds>] [--hold-process <seconds>]`
//!
//! The bundle may also come from `OCP_TEST_PLUGIN`.
//! This in-process AppKit probe requires macOS. Other platforms can use the
//! portable `oc-vst3-editor-host` helper for native editor hosting.
//!
//! Each stage is printed and flushed before it runs, so a crash inside the
//! plugin still shows how far the host got. Without `--attach` the view is only
//! created and inspected. With `--attach` the probe runs the real host path,
//! opens a window, pumps the main run loop for two seconds, renders a frame and
//! checks that the plugin installed a view in the window.

#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use oc_host::PluginTransport;
#[cfg(target_os = "macos")]
use oc_host::editor_monitor;
#[cfg(target_os = "macos")]
use oc_host::vst3_instance::{EditorAttachVariant, Vst3Instance};
#[cfg(target_os = "macos")]
use oc_synth_import::ForeignPreset;
#[cfg(target_os = "macos")]
use vst3::Steinberg::Vst::{
    IAttributeList, IComponentHandler, IComponentHandler2, IComponentHandler2Trait,
    IComponentHandler3, IComponentHandler3Trait, IComponentHandlerTrait, IConnectionPoint,
    IConnectionPointTrait, IContextMenu, IHostApplication, IHostApplicationTrait, IMessage,
    IPlugInterfaceSupport, IPlugInterfaceSupportTrait, ParamID, String128,
};
#[cfg(target_os = "macos")]
use vst3::Steinberg::{TUID, char16, kInvalidArgument, kNotImplemented, kResultOk, tresult};
#[cfg(target_os = "macos")]
use vst3::{Class, ComWrapper, Interface};

#[cfg(target_os = "macos")]
fn hex_tuid(ptr: *const TUID) -> String {
    if ptr.is_null() {
        return "null".to_string();
    }
    // SAFETY: The non-null pointer is a complete 16-byte TUID loaned by createInstance for this synchronous diagnostic read.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, 16) };
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Leave without running the module's teardown.
///
/// A hosted module registers C++ destructors that tear down UI state belonging
/// to the thread that created it; running them from this process's exit path is
/// a SIGSEGV. `oc_host::exit_process` flushes and then leaves through `_exit`
/// once a plugin module is loaded.
#[cfg(target_os = "macos")]
fn exit(code: i32) -> ! {
    oc_host::exit_process(code)
}

#[cfg(not(target_os = "macos"))]
fn exit(code: i32) -> ! {
    std::process::exit(code)
}

#[cfg(target_os = "macos")]
fn stage(message: &str) {
    let _ = writeln!(std::io::stderr(), "[probe] {message}");
    let _ = std::io::stderr().flush();
}

#[cfg(target_os = "macos")]
struct ProbeHost;

#[cfg(target_os = "macos")]
impl IHostApplicationTrait for ProbeHost {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let text = "ocp editor probe";
        for (index, unit) in text.encode_utf16().take(127).enumerate() {
            // SAFETY: The unsafe COM caller supplies writable String128 storage; index is below the 127-unit text bound.
            unsafe { (*name)[index] = unit as char16 };
        }
        // SAFETY: The same writable String128 has 128 units; this ASCII label ends at an index no greater than 127.
        unsafe { (*name)[text.len().min(127)] = 0 };
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> tresult {
        if std::env::var_os("OCP_PROBE_VERBOSE").is_some() {
            stage(&format!(
                "host createInstance cid={} iid={}",
                hex_tuid(cid),
                hex_tuid(iid)
            ));
        }
        if obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: The unsafe COM caller supplies a readable 16-byte class ID for the complete createInstance call.
        let cid = unsafe { std::slice::from_raw_parts(cid as *const u8, 16) };
        // SAFETY: The unsafe COM caller supplies a readable 16-byte interface ID for the complete createInstance call.
        let iid = unsafe { std::slice::from_raw_parts(iid as *const u8, 16) };
        let message = oc_host::host_message::create_message() as *mut std::ffi::c_void;
        if cid == IMessage::IID && iid == IMessage::IID {
            // SAFETY: The non-null output points to writable interface-pointer storage; the newly created message transfers one owned reference.
            unsafe { *obj = message };
            return kResultOk;
        }
        let attributes = oc_host::host_message::create_attribute_list() as *mut std::ffi::c_void;
        if cid == IAttributeList::IID || iid == IAttributeList::IID {
            // SAFETY: The same checked output slot is writable; the newly created attribute list transfers one owned reference.
            unsafe { *obj = attributes };
            return kResultOk;
        }
        kNotImplemented
    }
}

#[cfg(target_os = "macos")]
impl IComponentHandlerTrait for ProbeHost {
    unsafe fn beginEdit(&self, _id: u32) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: u32, _value: f64) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: u32) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: i32) -> tresult {
        if std::env::var_os("OCP_PROBE_VERBOSE").is_some() {
            stage("host restartComponent called");
        }
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl IComponentHandler3Trait for ProbeHost {
    unsafe fn createContextMenu(
        &self,
        _view: *mut vst3::Steinberg::IPlugView,
        _param: *const ParamID,
    ) -> *mut IContextMenu {
        if std::env::var_os("OCP_PROBE_VERBOSE").is_some() {
            stage("host createContextMenu called");
        }
        std::ptr::null_mut()
    }
}

#[cfg(target_os = "macos")]
impl IPlugInterfaceSupportTrait for ProbeHost {
    unsafe fn isPlugInterfaceSupported(&self, _iid: *const vst3::Steinberg::TUID) -> tresult {
        if std::env::var_os("OCP_PROBE_VERBOSE").is_some() {
            stage("host isPlugInterfaceSupported called");
        }
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl IComponentHandler2Trait for ProbeHost {
    unsafe fn setDirty(&self, _state: vst3::Steinberg::TBool) -> tresult {
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: vst3::Steinberg::FIDString) -> tresult {
        kResultOk
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl IConnectionPointTrait for ProbeHost {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        if std::env::var_os("OCP_PROBE_VERBOSE").is_some() {
            stage("host notify called");
        }
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl Class for ProbeHost {
    type Interfaces = (
        IHostApplication,
        IComponentHandler,
        IComponentHandler2,
        IComponentHandler3,
        IPlugInterfaceSupport,
        IConnectionPoint,
    );
}

/// Host variation under test. `baseline` is the shipped attach path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "macos")]
enum Variant {
    Baseline,
    Layer,
    ContentView,
    FrameLate,
    Async,
    Scale,
    Handler,
    Handler3,
    FrameNoResize,
    ChildWindow,
    OnSizeSync,
    OnSizeEarly,
    Detached,
}

#[cfg(target_os = "macos")]
impl Variant {
    fn parse(name: &str) -> Self {
        match name {
            "baseline" => Self::Baseline,
            "layer" => Self::Layer,
            "contentview" => Self::ContentView,
            "framelate" => Self::FrameLate,
            "async" => Self::Async,
            "scale" => Self::Scale,
            "handler" => Self::Handler,
            "handler3" => Self::Handler3,
            "frame-noresize" => Self::FrameNoResize,
            "childwindow" => Self::ChildWindow,
            "onsize-sync" => Self::OnSizeSync,
            "onsize-early" => Self::OnSizeEarly,
            "detached" => Self::Detached,
            other => panic!("unknown variant: {other}"),
        }
    }

    fn all() -> [&'static str; 12] {
        [
            "layer",
            "contentview",
            "framelate",
            "async",
            "scale",
            "handler",
            "handler3",
            "frame-noresize",
            "childwindow",
            "onsize-sync",
            "onsize-early",
            "detached",
        ]
    }
}

/// The probe's `handler` variant is set up before `load`; it uses the default attach.
#[cfg(target_os = "macos")]
fn host_variant(variant: Variant) -> Option<EditorAttachVariant> {
    match variant {
        Variant::Baseline | Variant::Handler | Variant::Handler3 => None,
        Variant::Layer => Some(EditorAttachVariant::Layer),
        Variant::ContentView => Some(EditorAttachVariant::ContentView),
        Variant::FrameLate => Some(EditorAttachVariant::FrameLate),
        Variant::Async => Some(EditorAttachVariant::Async),
        Variant::Scale => Some(EditorAttachVariant::Scale),
        Variant::FrameNoResize => Some(EditorAttachVariant::FrameNoResize),
        Variant::ChildWindow => Some(EditorAttachVariant::ChildWindow),
        Variant::OnSizeSync => Some(EditorAttachVariant::OnSizeSync),
        Variant::OnSizeEarly => Some(EditorAttachVariant::OnSizeEarly),
        Variant::Detached => Some(EditorAttachVariant::Detached),
    }
}

#[cfg(target_os = "macos")]
struct Options {
    plugin: PathBuf,
    with_context: bool,
    handler: bool,
    no_state: bool,
    roundtrip: bool,
    restore_state: bool,
    restore_both: bool,
    no_activate: bool,
    attach: bool,
    hidden: bool,
    variant: Variant,
    preset: Option<PathBuf>,
    controller_state: bool,
    controller_own_state: bool,
    controller_only: bool,
    state_before_activate: bool,
    get_state: bool,
    dump_state: Option<PathBuf>,
    process_blocks: u32,
    hold_seconds: u32,
    hold_process_seconds: u32,
    monitor_seconds: u32,
    screenshot: Option<PathBuf>,
    wav: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
fn parse_args() -> Options {
    let mut plugin = PathBuf::from(std::env::var("OCP_TEST_PLUGIN").unwrap_or_default());
    let mut options = Options {
        plugin: PathBuf::new(),
        with_context: false,
        handler: false,
        no_state: false,
        roundtrip: false,
        restore_state: false,
        restore_both: false,
        no_activate: false,
        attach: false,
        hidden: false,
        variant: Variant::Baseline,
        preset: None,
        controller_state: false,
        controller_own_state: false,
        controller_only: false,
        state_before_activate: false,
        get_state: false,
        dump_state: None,
        process_blocks: 0,
        hold_seconds: 0,
        hold_process_seconds: 0,
        monitor_seconds: 0,
        screenshot: None,
        wav: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin" => plugin = PathBuf::from(args.next().expect("plugin path")),
            "--context" => options.with_context = true,
            "--handler" => options.handler = true,
            "--no-state" => options.no_state = true,
            "--roundtrip" => options.roundtrip = true,
            "--restore-state" => options.restore_state = true,
            "--restore-both" => options.restore_both = true,
            "--no-activate" => options.no_activate = true,
            "--attach" => options.attach = true,
            "--hidden" => options.hidden = true,
            "--variant" => {
                let name = args.next().expect("variant name");
                options.variant = Variant::parse(&name);
                options.attach = true;
            }
            "--preset" => options.preset = Some(PathBuf::from(args.next().expect("preset path"))),
            "--controller-state" => options.controller_state = true,
            "--controller-own-state" => options.controller_own_state = true,
            "--controller-only" => options.controller_only = true,
            "--state-before-activate" => options.state_before_activate = true,
            "--getstate" => options.get_state = true,
            "--dump-state" => {
                options.dump_state = Some(PathBuf::from(args.next().expect("state output path")))
            }
            "--process" => {
                options.process_blocks = args
                    .next()
                    .expect("block count")
                    .parse()
                    .expect("block count is a number")
            }
            "--hold-process" => {
                options.hold_process_seconds = args
                    .next()
                    .expect("hold process seconds")
                    .parse()
                    .expect("hold process seconds is a number")
            }
            "--hold" => {
                options.hold_seconds = args
                    .next()
                    .expect("hold seconds")
                    .parse()
                    .expect("hold seconds is a number")
            }
            "--monitor" => {
                options.monitor_seconds = args
                    .next()
                    .expect("monitor seconds")
                    .parse()
                    .expect("monitor seconds is a number")
            }
            "--screenshot" => options.screenshot = Some(PathBuf::from(args.next().expect("path"))),
            "--wav" => options.wav = Some(PathBuf::from(args.next().expect("path"))),
            other => panic!("unknown argument: {other}"),
        }
    }
    options.plugin = plugin;
    options
}

#[cfg(target_os = "macos")]
fn init_application() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().expect("probe must run on the process main thread");
    let app = NSApplication::sharedApplication(mtm);
    let accessory = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    stage(&format!("NSApplication accessory policy: {accessory}"));
}

/// Report the host window the editor was attached to, then render one frame.
#[cfg(target_os = "macos")]
fn verify_window(title: &str, minimum_views: usize) -> Result<(u32, u32), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use objc2_foundation::{NSDate, NSRunLoop, NSSize};

    let mtm = MainThreadMarker::new().ok_or("not on the AppKit main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    let mut found = None;
    for window in windows.iter() {
        if window.title().to_string() == title {
            found = Some(window);
            break;
        }
    }
    let window = found.ok_or_else(|| format!("host window '{title}' not found"))?;
    let size: NSSize = window
        .contentView()
        .map(|view| view.frame().size)
        .unwrap_or(NSSize::new(0.0, 0.0));
    stage(&format!(
        "window: {}x{} visible={}",
        size.width,
        size.height,
        window.isVisible()
    ));
    stage("pumping the main run loop for 2s");
    let deadline = NSDate::dateWithTimeIntervalSinceNow(2.0);
    NSRunLoop::currentRunLoop().runUntilDate(&deadline);
    fn descendants(view: &NSView) -> usize {
        view.subviews()
            .iter()
            .map(|child| 1 + descendants(&child))
            .sum()
    }
    let subviews = window
        .contentView()
        .map(|view| descendants(&view))
        .unwrap_or(0);
    stage(&format!("editor views in hierarchy: {subviews}"));
    if subviews < minimum_views {
        return Err(format!(
            "plugin did not install a view in the host window ({subviews} < {minimum_views})"
        ));
    }
    stage("displayIfNeeded");
    window.displayIfNeeded();
    stage("frame rendered");
    Ok((size.width as u32, size.height as u32))
}

/// Keep the editor window on screen so an external observer can inspect it.
#[cfg(target_os = "macos")]
fn hold_window(seconds: u32) {
    use objc2_foundation::{NSDate, NSRunLoop};

    for second in 1..=seconds {
        let deadline = NSDate::dateWithTimeIntervalSinceNow(1.0);
        NSRunLoop::currentRunLoop().runUntilDate(&deadline);
        stage(&format!("holding {second}/{seconds}s"));
    }
}

/// Keep the editor on screen while feeding the processor silent blocks.
#[cfg(target_os = "macos")]
fn hold_window_processing(instance: &mut Vst3Instance, seconds: u32) {
    use objc2_foundation::{NSDate, NSRunLoop};

    for second in 1..=seconds {
        for _ in 0..100 {
            let deadline = NSDate::dateWithTimeIntervalSinceNow(0.0107);
            NSRunLoop::currentRunLoop().runUntilDate(&deadline);
            let _ = instance.process_block(512);
        }
        stage(&format!("holding with processing {second}/{seconds}s"));
    }
}

#[cfg(target_os = "macos")]
fn peak(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(sample.abs()))
        .fold(0.0, f64::max)
}

#[cfg(target_os = "macos")]
fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let power = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    (power / samples.len() as f64).sqrt()
}

#[cfg(target_os = "macos")]
fn write_wav_f32(path: &Path, samples: &[f32], rate: u32) {
    use std::io::Write;

    let data_len = (samples.len() * 4) as u32;
    let mut file = std::fs::File::create(path).expect("create wav");
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&3u16.to_le_bytes()).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&rate.to_le_bytes()).unwrap();
    file.write_all(&(rate * 8).to_le_bytes()).unwrap();
    file.write_all(&8u16.to_le_bytes()).unwrap();
    file.write_all(&32u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_len.to_le_bytes()).unwrap();
    for sample in samples {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}

/// Drive the editor's instance at real time through the monitor route and
/// report the level the device callback would hear.
#[cfg(target_os = "macos")]
fn monitor_editor(
    instance: &mut Vst3Instance,
    gui: &oc_host::vst3_instance::Vst3GuiHandle,
    seconds: u32,
    screenshot: Option<&Path>,
    wav: Option<&Path>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const RATE: u32 = 48_000;
    const BLOCK: u32 = 512;

    let gate = Arc::new(AtomicBool::new(true));
    let (mut writer, mut reader) =
        editor_monitor::channel(editor_monitor::DEFAULT_CAPACITY_FRAMES, Arc::clone(&gate));
    // Pre-fill the ring so the simulated device does not start on an underrun.
    for _ in 0..2 {
        let block = instance
            .process_block_with_transport(BLOCK, Some(PluginTransport::new(0, RATE, 120.0)));
        writer.publish(&block, true);
    }
    let drain_stop = Arc::new(AtomicBool::new(false));
    let drained: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let drain_flag = Arc::clone(&drain_stop);
    let drain_sink = Arc::clone(&drained);
    let drain = std::thread::spawn(move || {
        let mut block = vec![0.0f32; (BLOCK * 2) as usize];
        let mut next = Instant::now();
        while !drain_flag.load(Ordering::Acquire) {
            reader.read(&mut block);
            drain_sink.lock().unwrap().extend_from_slice(&block);
            next += Duration::from_secs_f64(f64::from(BLOCK) / f64::from(RATE));
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    });

    let total_blocks = seconds * RATE / BLOCK;
    let transport = PluginTransport::new(0, RATE, 120.0);
    let notes = [
        (0.5, true, 48u8, 0.9f32),
        (2.0, false, 48, 0.0),
        (2.2, true, 52, 0.8),
        (3.7, false, 52, 0.0),
        (3.9, true, 55, 0.9),
        (6.0, false, 55, 0.0),
    ];
    let started = Instant::now();
    let mut raw = Vec::with_capacity((total_blocks * BLOCK * 2) as usize);
    let mut queued_frames = 0usize;
    let mut note_index = 0usize;
    let mut steady_report = None;
    let mut warmup_report = None;
    let mut blocks_done = 0usize;
    let mut next_publish = Instant::now();
    let period = Duration::from_secs_f64(f64::from(BLOCK) / f64::from(RATE));
    for _ in 0..total_blocks {
        // Pace like the keep-alive worker: keep one callback plus one block
        // queued, pumping the run loop while the device drains.
        let waiting_since = Instant::now();
        while writer.queued_frames() + BLOCK as usize > writer.target_frames(BLOCK as usize) {
            let deadline = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.001);
            objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&deadline);
            if waiting_since.elapsed() > Duration::from_millis(750) {
                stage("monitor: device stalled");
                break;
            }
        }
        // Keep the block rate on wall time; the editor pump above can overshoot
        // a 10.7 ms deadline, and the simulated device would then starve.
        let now = Instant::now();
        if next_publish > now {
            let deadline = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(
                (next_publish - now).as_secs_f64(),
            );
            objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&deadline);
        }
        next_publish += period;
        let elapsed = started.elapsed().as_secs_f64();
        while note_index < notes.len() && elapsed >= notes[note_index].0 {
            let (_, on, note, velocity) = notes[note_index];
            if on {
                instance.note_on(note, velocity, 0);
            } else {
                instance.note_off(note, 0);
            }
            note_index += 1;
        }
        let block = instance.process_block_with_transport(BLOCK, Some(transport.clone()));
        raw.extend_from_slice(&block);
        writer.publish(&block, true);
        queued_frames += writer.queued_frames();
        blocks_done += 1;
        if warmup_report.is_none() && started.elapsed() >= Duration::from_secs(2) {
            warmup_report = Some(writer.report());
        }
        if blocks_done == total_blocks as usize {
            steady_report = Some(writer.report());
        }
    }
    drain_stop.store(true, Ordering::Release);
    let _ = drain.join();

    if let Some(path) = screenshot
        && let Some(number) = gui.window_number()
    {
        let status = std::process::Command::new("screencapture")
            .arg("-x")
            .arg("-l")
            .arg(number.to_string())
            .arg(path)
            .status();
        stage(&format!("screenshot {} -> {status:?}", path.display()));
    }

    let heard = drained.lock().unwrap();
    if let Some(path) = wav {
        write_wav_f32(path, &heard, RATE);
        stage(&format!(
            "wrote {} ({} samples)",
            path.display(),
            heard.len()
        ));
    }
    let report = steady_report.unwrap_or_else(|| writer.report());
    let warmup = warmup_report.unwrap_or_default();
    stage(&format!(
        "monitor steady: underruns={} dropped={} (after a 2 s warm-up)",
        report.underruns.saturating_sub(warmup.underruns),
        report.dropped.saturating_sub(warmup.dropped)
    ));
    stage(&format!(
        "monitor: blocks={total_blocks} raw_peak={:.6} raw_rms={:.6} heard_peak={:.6} heard_rms={:.6} \
         latency_blocks={:.2} underruns={} dropped={} published={} played={}",
        peak(&raw),
        rms(&raw),
        peak(&heard),
        rms(&heard),
        queued_frames as f64 / total_blocks as f64 / f64::from(BLOCK) + 1.0,
        report.underruns,
        report.dropped,
        report.published_frames,
        report.played_frames
    ));
}

#[cfg(target_os = "macos")]
fn main() {
    let options = parse_args();
    if options.plugin.as_os_str().is_empty() {
        eprintln!(
            "usage: vst3_editor_probe --plugin <bundle> [--context] [--handler] \
             [--attach [--hidden]] [--variant <{}>] [--preset <preset>] \
             [--controller-state] [--controller-own-state] [--controller-only] \
             [--state-before-activate] [--getstate] [--process <n>] \
             [--hold <seconds>] [--hold-process <seconds>] \
             [--monitor <seconds> [--screenshot <png>] [--wav <wav>]]",
            Variant::all().join("|")
        );
        exit(2);
    }

    let preset_state = options.preset.as_ref().map(|path| {
        stage(&format!("reading preset {}", path.display()));
        let preset = ForeignPreset::load(path).expect("load preset");
        let state = preset.to_vst3_state().expect("preset state");
        stage(&format!("preset VST3 state: {} bytes", state.len()));
        state
    });

    #[cfg(target_os = "macos")]
    init_application();

    let host = ComWrapper::new(ProbeHost);
    let host_app = host.to_com_ptr::<IHostApplication>().expect("host pointer");
    let early_handler = if matches!(options.variant, Variant::Handler | Variant::Handler3) {
        let handler = host
            .to_com_ptr::<IComponentHandler>()
            .expect("component handler pointer");
        stage("setComponentHandler (before state transfer and view creation)");
        handler.as_ptr()
    } else {
        std::ptr::null_mut()
    };
    stage(&format!("loading {}", options.plugin.display()));
    let transfer_state = !options.no_state && !options.state_before_activate;
    let activate = !options.no_activate && !options.state_before_activate;
    stage(&format!(
        "load options: initial_state={transfer_state} activate={activate}"
    ));
    let mut instance = Vst3Instance::load_with_options(
        &options.plugin,
        48_000,
        512,
        if options.with_context {
            host_app.as_ptr() as *mut vst3::Steinberg::FUnknown
        } else {
            std::ptr::null_mut()
        },
        early_handler,
        transfer_state,
        activate,
    )
    .expect("load plugin");
    stage("loaded");
    stage(&format!("class info: {:?}", instance.class_info()));
    if let Some(path) = &options.dump_state {
        let bytes = instance.state_bytes().expect("component state");
        std::fs::write(path, &bytes).expect("write component state");
        stage(&format!("wrote {} state bytes", bytes.len()));
    }
    if let Some(bytes) = &preset_state {
        if options.controller_only {
            stage("skipping component state (--controller-only)");
        } else {
            stage("applying preset state to component");
            instance
                .set_component_state_bytes(bytes)
                .expect("apply component state");
            stage("component state applied");
        }
        if options.controller_own_state {
            stage("applying preset state through controller setState");
            instance
                .feed_controller_own_state(bytes)
                .expect("apply controller own state");
            stage("controller own state applied");
        }
        if options.controller_state {
            stage("applying preset state to controller");
            instance
                .feed_controller_component_state(bytes)
                .expect("apply controller state");
            stage("controller state applied");
        }
    }
    if options.state_before_activate {
        stage("activating after state");
        instance.set_active_for_test(true);
        instance.set_processing_for_test(true);
        stage("activated");
    }
    if options.process_blocks > 0 {
        stage(&format!(
            "processing {} blocks before editor creation",
            options.process_blocks
        ));
        for _ in 0..options.process_blocks {
            let _ = instance.process_block(512);
        }
        stage("processed");
    }
    if options.get_state {
        stage("controller getState");
        match instance.controller_state_bytes() {
            Ok(bytes) => stage(&format!("controller getState: {} bytes", bytes.len())),
            Err(error) => stage(&format!("controller getState failed: {error}")),
        }
    }
    if options.roundtrip {
        stage("controller getState/setState round trip");
        match instance.roundtrip_controller_state() {
            Ok(length) => stage(&format!("controller state round trip: {length} bytes")),
            Err(error) => stage(&format!("controller state round trip failed: {error}")),
        }
    }
    if options.restore_state || options.restore_both {
        let bytes = instance.state_bytes().expect("component state");
        stage(&format!("component state: {} bytes", bytes.len()));
        if options.restore_both {
            stage("applying component and controller state");
            instance.set_state_bytes(&bytes).expect("apply state");
        } else {
            stage("applying component state");
            instance
                .set_component_state_bytes(&bytes)
                .expect("apply state");
        }
        stage("state applied");
    }
    if options.with_context
        && let Some(connection_point) = host.to_com_ptr::<IConnectionPoint>()
    {
        // SAFETY: The outer host wrapper retains this connection-point object throughout the probe, including this synchronous connection call.
        unsafe { instance.connect_host_connection_point(connection_point.as_ptr()) };
    }
    // `--handler` installs a live handler after load, for comparison.
    if options.handler {
        let handler = host
            .to_com_ptr::<IComponentHandler>()
            .expect("component handler pointer");
        stage("setComponentHandler");
        // SAFETY: The outer host wrapper retains the handler object throughout the probe; the owned interface also remains live during registration.
        let result = unsafe { instance.set_component_handler_for_test(handler.as_ptr()) };
        stage(&format!("setComponentHandler -> 0x{result:x}"));
    }
    if !options.attach {
        let info = instance.editor_info().expect("editor_info");
        stage(&format!("editor_info: {info:?}"));
        stage("done");
        exit(0);
    }

    let gui = instance.gui_handle().expect("editor handle");
    if let Some(reason) = gui.blocked_reason() {
        stage(&format!("REFUSED: {reason}"));
        exit(3);
    }
    let title = "ocp editor probe";
    stage(&format!("attach variant {:?}", options.variant));
    let host_variant = host_variant(options.variant).unwrap_or_default();
    let opened = gui.open_with_variant(title, options.hidden, host_variant);
    match opened {
        Ok(size) => {
            stage(&format!("attached: {size:?}"));
            #[cfg(target_os = "macos")]
            let minimum_views = if options.variant == Variant::ChildWindow {
                // The host's child container is already one descendant.
                2
            } else {
                1
            };
            #[cfg(target_os = "macos")]
            match verify_window(title, minimum_views) {
                Ok(actual) => {
                    stage(&format!("verified window: {actual:?}"));
                    if options.hold_seconds > 0 {
                        hold_window(options.hold_seconds);
                    }
                    if options.hold_process_seconds > 0 {
                        hold_window_processing(&mut instance, options.hold_process_seconds);
                    }
                    if options.monitor_seconds > 0 {
                        monitor_editor(
                            &mut instance,
                            &gui,
                            options.monitor_seconds,
                            options.screenshot.as_deref(),
                            options.wav.as_deref(),
                        );
                    }
                    gui.close().expect("close editor");
                    stage("closed");
                    stage("PASS");
                }
                Err(error) => {
                    stage(&format!("FAIL: {error}"));
                    let _ = gui.close();
                    exit(5);
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = size;
                stage("PASS (attach verification is macOS-only)");
            }
        }
        Err(error) => {
            stage(&format!("FAIL: attach refused: {error}"));
            exit(4);
        }
    }
    drop(instance);
    exit(0);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "vst3_editor_probe requires macOS in-process editor hosting; use oc-vst3-editor-host on this platform"
    );
    exit(2);
}
