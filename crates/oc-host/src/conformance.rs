//! VST3 conformance battery.
//!
//! One child process hosts one bundle and runs a fixed battery of host-contract
//! steps. The child streams one JSON line per finished step, so a crash or a
//! hang still leaves the completed steps for the parent to collect. The parent
//! spawns one child per bundle; a module that aborts cannot take the matrix down.
//!
//! The battery reasons about the host contract, not about a particular plugin:
//! every step reports what the plugin declared, how long the call took and the
//! error text when the contract was not met.
//!
//! # Child protocol
//!
//! The child writes JSON Lines to stdout:
//!
//! ```text
//! {"step":{"step":"load","status":"pass","ms":41,"detail":{...}}}
//! {"fatal":"hang","step":"editor"}
//! {"done":true,"name":"Example Synth","vendor":"Example Vendor"}
//! ```
//!
//! A fatal line comes from the watchdog (hang) or from a `SIGABRT`/`SIGBUS`/
//! `SIGSEGV` handler (crash). Both leave with status 70 and skip the hosted
//! module's static destructors.
//!
//! # Threading
//!
//! The battery runs on the thread that calls [`run_child`]; the editor step
//! additionally requires that thread to be the AppKit main thread. The watchdog
//! is the only other thread, and it only reads a deadline and exits the process.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io::Write;
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, ITimerHandler,
    ITimerHandlerTrait,
};
use vst3::Steinberg::Vst::{
    IComponent, IEditController, IEditControllerTrait, IProgramListData, IUnitInfo, IUnitInfoTrait,
    ProgramListInfo,
};
#[cfg(target_os = "macos")]
use vst3::Steinberg::kPlatformTypeNSView;
use vst3::Steinberg::{
    IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    IPlugViewTrait, ViewRect, int32, int64, kInvalidArgument, kNotImplemented, kResultFalse,
    kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use crate::ports::AudioPort;
use crate::vst3_instance::{Vst3Instance, Vst3Param};

/// The fixed battery, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Load,
    Buses,
    State,
    Presets,
    Params,
    Editor,
    Process,
    Teardown,
}

/// Every step name, indexed by [`Step::index`].
pub const STEP_NAMES: [&str; 8] = [
    "load", "buses", "state", "presets", "params", "editor", "process", "teardown",
];

impl Step {
    /// The battery in execution order.
    pub const ALL: [Step; 8] = [
        Step::Load,
        Step::Buses,
        Step::State,
        Step::Presets,
        Step::Params,
        Step::Editor,
        Step::Process,
        Step::Teardown,
    ];

    /// Stable JSON and table name.
    pub fn name(self) -> &'static str {
        STEP_NAMES[self.index()]
    }

    /// Position in [`STEP_NAMES`].
    pub fn index(self) -> usize {
        match self {
            Step::Load => 0,
            Step::Buses => 1,
            Step::State => 2,
            Step::Presets => 3,
            Step::Params => 4,
            Step::Editor => 5,
            Step::Process => 6,
            Step::Teardown => 7,
        }
    }
}

/// Outcome of one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pass,
    Fail,
    Skipped,
    /// An instrument that produced no output after a note.
    Silent,
    /// The step did not return inside the watchdog deadline.
    Hang,
    /// The process died in this step.
    Crash,
    /// The parent killed a child that outlived its hard deadline.
    Timeout,
}

impl StepStatus {
    /// Whether this status counts as a passing step.
    pub fn is_pass(self) -> bool {
        matches!(self, StepStatus::Pass)
    }

    /// Whether this status blocks a plugin from passing every step.
    ///
    /// `Silent` is informational: an empty sampler correctly renders silence.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            StepStatus::Fail | StepStatus::Hang | StepStatus::Crash | StepStatus::Timeout
        )
    }

    /// Stable JSON and table label.
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pass => "pass",
            StepStatus::Fail => "fail",
            StepStatus::Skipped => "skipped",
            StepStatus::Silent => "silent",
            StepStatus::Hang => "hang",
            StepStatus::Crash => "crash",
            StepStatus::Timeout => "timeout",
        }
    }

    /// Short label for the matrix table.
    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Pass => "PASS",
            StepStatus::Fail => "FAIL",
            StepStatus::Skipped => "SKIP",
            StepStatus::Silent => "SILENT",
            StepStatus::Hang => "HANG",
            StepStatus::Crash => "CRASH",
            StepStatus::Timeout => "TIMEOUT",
        }
    }
}

/// One step's report as it appears in the child protocol and the matrix JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepReport {
    pub step: String,
    pub status: StepStatus,
    pub ms: u64,
    #[serde(default)]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl StepReport {
    fn new(step: Step, status: StepStatus, ms: u64, detail: Value, error: Option<String>) -> Self {
        Self {
            step: step.name().to_string(),
            status,
            ms,
            detail,
            error,
        }
    }

    fn skipped(step: Step, reason: &str) -> Self {
        Self::new(
            step,
            StepStatus::Skipped,
            0,
            json!({}),
            Some(reason.to_string()),
        )
    }
}

/// One plugin's row in the matrix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginReport {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub category: String,
    pub steps: Vec<StepReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_stderr: Option<String>,
    /// Other installations of the same bundle name that this row stands for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<String>,
}

impl PluginReport {
    /// A row with every step still marked skipped, for a child that died.
    pub fn unreached(path: &Path, name: &str) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            name: name.to_string(),
            vendor: String::new(),
            version: None,
            category: String::new(),
            steps: Step::ALL
                .iter()
                .map(|step| StepReport::skipped(*step, "not reached"))
                .collect(),
            exit_code: None,
            child_stderr: None,
            duplicates: Vec::new(),
        }
    }

    /// Steps that did not pass.
    pub fn failures(&self) -> impl Iterator<Item = &StepReport> {
        self.steps.iter().filter(|step| step.status.is_failure())
    }

    /// Whether every executed step passed; a skipped step is not a failure.
    pub fn passes_every_step(&self) -> bool {
        !self.steps.iter().any(|step| step.status.is_failure())
    }
}

fn step_order(name: &str) -> usize {
    Step::ALL
        .iter()
        .position(|step| step.name() == name)
        .unwrap_or(usize::MAX)
}

/// One error text shared by several plugins in the same step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureGroup {
    pub step: String,
    pub error: String,
    pub count: usize,
    pub plugins: Vec<String>,
}

/// Whole-matrix counters.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MatrixSummary {
    pub plugins: usize,
    pub passed_all: usize,
    pub status_counts: BTreeMap<String, usize>,
    pub step_failures: BTreeMap<String, usize>,
    pub failures: Vec<FailureGroup>,
    pub top_failure_kinds: Vec<FailureGroup>,
}

/// The matrix document written by `--format json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatrixReport {
    pub schema: String,
    pub host: String,
    pub host_version: String,
    pub generated_at_unix: u64,
    pub timeout_ms: u64,
    pub editor: bool,
    pub plugins: Vec<PluginReport>,
    pub summary: MatrixSummary,
}

impl MatrixReport {
    /// Build the document and its counters from the collected rows.
    pub fn new(plugins: Vec<PluginReport>, timeout_ms: u64, editor: bool) -> Self {
        let mut summary = MatrixSummary {
            plugins: plugins.len(),
            ..MatrixSummary::default()
        };
        let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for plugin in &plugins {
            if plugin.passes_every_step() {
                summary.passed_all += 1;
            }
            for step in &plugin.steps {
                *summary
                    .status_counts
                    .entry(step.status.as_str().to_string())
                    .or_default() += 1;
                if !step.status.is_failure() {
                    continue;
                }
                *summary.step_failures.entry(step.step.clone()).or_default() += 1;
                let error = step
                    .error
                    .clone()
                    .unwrap_or_else(|| step.status.as_str().to_string());
                groups
                    .entry((step.step.clone(), error))
                    .or_default()
                    .push(plugin.name.clone());
            }
        }
        let mut failures: Vec<FailureGroup> = groups
            .into_iter()
            .map(|((step, error), mut plugins)| {
                plugins.sort();
                plugins.dedup();
                FailureGroup {
                    step,
                    error,
                    count: plugins.len(),
                    plugins,
                }
            })
            .collect();
        failures.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| step_order(&left.step).cmp(&step_order(&right.step)))
                .then_with(|| left.error.cmp(&right.error))
        });
        summary.top_failure_kinds = failures.iter().take(3).cloned().collect();
        summary.failures = failures;
        Self {
            schema: "opencompose_vst3_conformance_v1".to_string(),
            host: "opencompose".to_string(),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0),
            timeout_ms,
            editor,
            plugins,
            summary,
        }
    }
}

/// What one step found.
pub struct StepOutcome {
    pub status: StepStatus,
    pub detail: Value,
    pub error: Option<String>,
}

impl StepOutcome {
    fn pass(detail: Value) -> Self {
        Self {
            status: StepStatus::Pass,
            detail,
            error: None,
        }
    }

    fn fail(error: impl Into<String>) -> Self {
        Self {
            status: StepStatus::Fail,
            detail: json!({}),
            error: Some(error.into()),
        }
    }

    fn fail_with(detail: Value, error: impl Into<String>) -> Self {
        Self {
            status: StepStatus::Fail,
            detail,
            error: Some(error.into()),
        }
    }

    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            status: StepStatus::Skipped,
            detail: json!({}),
            error: Some(reason.into()),
        }
    }

    fn silent(detail: Value) -> Self {
        Self {
            status: StepStatus::Silent,
            detail,
            error: Some("instrument produced no output after a note".to_string()),
        }
    }
}

/// One plugin under test. The runner owns timing and the watchdog; a driver
/// only performs the calls and reports what they returned.
pub trait ConformanceDriver {
    fn load(&mut self) -> StepOutcome;
    fn buses(&mut self) -> StepOutcome;
    fn state(&mut self) -> StepOutcome;
    fn presets(&mut self) -> StepOutcome;
    fn params(&mut self) -> StepOutcome;
    fn editor(&mut self) -> StepOutcome;
    fn process(&mut self) -> StepOutcome;
    fn teardown(&mut self) -> StepOutcome;
}

impl dyn ConformanceDriver + '_ {
    fn run(&mut self, step: Step) -> StepOutcome {
        match step {
            Step::Load => self.load(),
            Step::Buses => self.buses(),
            Step::State => self.state(),
            Step::Presets => self.presets(),
            Step::Params => self.params(),
            Step::Editor => self.editor(),
            Step::Process => self.process(),
            Step::Teardown => self.teardown(),
        }
    }
}

// ---------------------------------------------------------------------------
// Watchdog and child exit
// ---------------------------------------------------------------------------

/// Index of the step in flight, read by the fatal-signal handler.
static CURRENT_STEP: AtomicUsize = AtomicUsize::new(0);

/// One deadline shared by the watchdog thread and the battery thread.
struct WatchdogState {
    step: Option<Step>,
    deadline: Option<Instant>,
    generation: u64,
}

/// Exits the process when a battery step outlives the per-step deadline.
pub struct Watchdog {
    timeout: Duration,
    state: Arc<(Mutex<WatchdogState>, Condvar)>,
}

impl Watchdog {
    /// Start the watchdog thread. The deadline is set per step by [`Self::begin`].
    pub fn start(timeout: Duration) -> Arc<Self> {
        let state = Arc::new((
            Mutex::new(WatchdogState {
                step: None,
                deadline: None,
                generation: 0,
            }),
            Condvar::new(),
        ));
        let shared = Arc::clone(&state);
        std::thread::Builder::new()
            .name("conformance-watchdog".to_string())
            .spawn(move || {
                let (lock, wake) = &*shared;
                let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
                loop {
                    while state.deadline.is_none() {
                        state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                    }
                    let Some(deadline) = state.deadline else {
                        continue;
                    };
                    let generation = state.generation;
                    let now = Instant::now();
                    if now >= deadline {
                        let step = state.step.unwrap_or(Step::Load);
                        drop(state);
                        fatal_hang(step);
                    }
                    let (next, _) = wake
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|error| error.into_inner());
                    state = next;
                    if state.generation != generation {
                        continue;
                    }
                    if state
                        .deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        let step = state.step.unwrap_or(Step::Load);
                        drop(state);
                        fatal_hang(step);
                    }
                }
            })
            .ok();
        Arc::new(Self { timeout, state })
    }

    /// Arm the deadline for `step`.
    pub fn begin(&self, step: Step) {
        CURRENT_STEP.store(step.index(), Ordering::Release);
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.step = Some(step);
        state.deadline = Some(Instant::now() + self.timeout);
        wake.notify_all();
    }

    /// Disarm the deadline once a step returned.
    pub fn end(&self) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.deadline = None;
        wake.notify_all();
    }
}

fn emit_line(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
    let _ = stdout.flush();
}

/// Print the hang record and leave without running any module destructor.
fn fatal_hang(step: Step) -> ! {
    emit_line(&format!(
        "{{\"fatal\":\"hang\",\"step\":\"{}\"}}",
        step.name()
    ));
    leave(70)
}

fn leave(code: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` is async-signal-safe and skips the hosted module's static
    // destructors, which is the reason this path exists.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

/// Write to a raw file descriptor without allocation or buffering.
#[cfg(unix)]
fn write_fd(fd: i32, bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: the pointer and length describe a live byte slice.
        let written = unsafe {
            libc::write(
                fd,
                bytes[offset..].as_ptr() as *const c_void,
                bytes.len() - offset,
            )
        };
        if written <= 0 {
            return;
        }
        offset += written as usize;
    }
}

/// Install the fatal-signal handlers that turn a module abort into a crash step.
///
/// The handler writes the step name and leaves with `_exit(70)`. It uses only
/// async-signal-safe calls: `write` and `_exit`.
pub fn install_signal_handlers() {
    #[cfg(unix)]
    // SAFETY: fatal_signal has the C signal-handler ABI and only accesses
    // lock-free static state and async-signal-safe write/_exit operations.
    unsafe {
        for signal in [libc::SIGABRT, libc::SIGBUS, libc::SIGSEGV] {
            libc::signal(signal, fatal_signal as *const () as libc::sighandler_t);
        }
    }
}

#[cfg(unix)]
unsafe extern "C" fn fatal_signal(signal: libc::c_int) {
    let index = CURRENT_STEP
        .load(Ordering::Acquire)
        .min(STEP_NAMES.len() - 1);
    let step = STEP_NAMES[index].as_bytes();
    let name = match signal {
        libc::SIGABRT => b"SIGABRT".as_slice(),
        libc::SIGBUS => b"SIGBUS".as_slice(),
        libc::SIGSEGV => b"SIGSEGV".as_slice(),
        _ => b"signal".as_slice(),
    };
    write_fd(2, b"conformance child: ");
    write_fd(2, name);
    write_fd(2, b" in step ");
    write_fd(2, step);
    write_fd(2, b"\n");
    write_fd(1, b"{\"fatal\":\"crash\",\"signal\":\"");
    write_fd(1, name);
    write_fd(1, b"\",\"step\":\"");
    write_fd(1, step);
    write_fd(1, b"\"}\n");
    // SAFETY: `_exit` is async-signal-safe and deliberately skips the module's
    // static destructors.
    unsafe { libc::_exit(70) }
}

// ---------------------------------------------------------------------------
// Battery runner
// ---------------------------------------------------------------------------

/// Run the whole battery, timing each step and reporting it as it finishes.
///
/// A failed load skips the remaining steps: there is no instance to inspect.
pub fn run_battery(
    driver: &mut dyn ConformanceDriver,
    watchdog: Option<&Watchdog>,
    mut on_step: impl FnMut(&StepReport),
) -> Vec<StepReport> {
    let mut reports = Vec::with_capacity(Step::ALL.len());
    let mut load_failed = false;
    for step in Step::ALL {
        if load_failed {
            let report = StepReport::skipped(step, "load failed");
            on_step(&report);
            reports.push(report);
            continue;
        }
        if let Some(watchdog) = watchdog {
            watchdog.begin(step);
        } else {
            CURRENT_STEP.store(step.index(), Ordering::Release);
        }
        let started = Instant::now();
        let outcome = driver.run(step);
        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if let Some(watchdog) = watchdog {
            watchdog.end();
        }
        let report = StepReport::new(step, outcome.status, elapsed, outcome.detail, outcome.error);
        if step == Step::Load && report.status.is_failure() {
            load_failed = true;
        }
        on_step(&report);
        reports.push(report);
    }
    reports
}

// ---------------------------------------------------------------------------
// State stream
// ---------------------------------------------------------------------------

/// Minimal `IBStream` over a byte buffer, for `setComponentState`.
struct StateStream {
    data: Rc<RefCell<Vec<u8>>>,
    position: Cell<usize>,
}

impl StateStream {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data: Rc::new(RefCell::new(data)),
            position: Cell::new(0),
        }
    }
}

impl Class for StateStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for StateStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }
        let data = self.data.borrow();
        let position = self.position.get().min(data.len());
        let count = (num_bytes as usize).min(data.len().saturating_sub(position));
        if count > 0 {
            // SAFETY: the IBStream caller supplies a writable num_bytes region.
            // count is bounded by that size and the remaining private stream bytes;
            // the private Vec backing is not exposed as a caller buffer.
            unsafe {
                ptr::copy_nonoverlapping(data[position..].as_ptr(), buffer as *mut u8, count);
            }
        }
        self.position.set(position + count);
        if !num_bytes_read.is_null() {
            // SAFETY: the optional non-null IBStream result points to a writable
            // aligned int32 for this synchronous call.
            unsafe { ptr::write(num_bytes_read, count as int32) };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }
        let count = num_bytes as usize;
        let position = self.position.get();
        let mut data = self.data.borrow_mut();
        let Some(end) = position.checked_add(count) else {
            return -1;
        };
        if data.len() < end {
            data.resize(end, 0);
        }
        if count > 0 {
            // SAFETY: the IBStream caller supplies num_bytes readable bytes.
            // resize reserves the complete destination, and private stream storage
            // cannot alias the external caller buffer.
            unsafe {
                ptr::copy_nonoverlapping(buffer as *const u8, data[position..].as_mut_ptr(), count);
            }
        }
        self.position.set(position + count);
        if !num_bytes_written.is_null() {
            // SAFETY: the optional non-null IBStream output points to a writable
            // aligned int32 supplied for this synchronous call.
            unsafe { ptr::write(num_bytes_written, count as int32) };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let length = self.data.borrow().len() as int64;
        let base = match mode {
            mode if mode == IStreamSeekMode_::kIBSeekSet as int32 => 0,
            mode if mode == IStreamSeekMode_::kIBSeekCur as int32 => self.position.get() as int64,
            mode if mode == IStreamSeekMode_::kIBSeekEnd as int32 => length,
            _ => return -1,
        };
        let Some(new_position) = base.checked_add(pos) else {
            return -1;
        };
        if new_position < 0 {
            return -1;
        }
        self.position.set(new_position as usize);
        if !result.is_null() {
            // SAFETY: a non-null seek output is a caller-provided writable int64;
            // the validated scalar position retains no borrow of that storage.
            unsafe { ptr::write(result, new_position) };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            // SAFETY: a non-null tell output is a caller-provided writable int64
            // whose lifetime covers this synchronous callback.
            unsafe { ptr::write(pos, self.position.get() as int64) };
        }
        kResultOk
    }
}

fn char16_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|code| *code == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn controller_ref<'a>(instance: &'a Vst3Instance) -> Option<ComRef<'a, IEditController>> {
    let raw = instance.raw_interfaces();
    if raw.controller.is_null() {
        None
    } else {
        // SAFETY: the instance owns the controller for its whole lifetime, and
        // the caller keeps the instance alive while this borrow is used.
        Some(unsafe {
            ComRef::<IEditController>::from_raw_unchecked(raw.controller as *mut IEditController)
        })
    }
}

fn tresult_name(result: tresult) -> String {
    if result == kResultOk {
        "kResultOk".to_string()
    } else if result == kResultFalse {
        "kResultFalse".to_string()
    } else if result == kInvalidArgument {
        "kInvalidArgument".to_string()
    } else if result == kNotImplemented {
        "kNotImplemented".to_string()
    } else {
        format!("0x{result:x}")
    }
}

/// What the controller did with the component state.
enum ControllerState {
    Accepted,
    /// The VST3 interface is optional; a controller may decline to implement it.
    NotImplemented,
    Rejected(String),
}

impl ControllerState {
    fn label(&self) -> String {
        match self {
            ControllerState::Accepted => "accepted".to_string(),
            ControllerState::NotImplemented => "not implemented".to_string(),
            ControllerState::Rejected(error) => error.clone(),
        }
    }
}

fn feed_controller_state(instance: &Vst3Instance, bytes: &[u8]) -> Result<ControllerState, String> {
    let controller = controller_ref(instance).ok_or("no edit controller")?;
    let stream = StateStream::new(bytes.to_vec());
    let wrapper = ComWrapper::new(stream);
    let com_stream = wrapper
        .to_com_ptr::<IBStream>()
        .ok_or("could not create the state stream")?;
    // SAFETY: controller is a live borrowed interface and com_stream owns
    // its IBStream reference through this synchronous battery call.
    let result = unsafe { controller.setComponentState(com_stream.as_ptr()) };
    if result == kResultOk {
        Ok(ControllerState::Accepted)
    } else if result == kNotImplemented {
        Ok(ControllerState::NotImplemented)
    } else {
        Ok(ControllerState::Rejected(format!(
            "controller setComponentState returned {} (0x{result:x})",
            tresult_name(result)
        )))
    }
}

/// One parameter vector comparison, keyed by VST3 parameter id.
struct ParamComparison {
    count_before: usize,
    count_after: usize,
    compared: usize,
    missing: usize,
    extra: usize,
    mismatched: usize,
    max_abs_diff: f64,
}

fn compare_params(before: &[(Vst3Param, f64)], after: &[(Vst3Param, f64)]) -> ParamComparison {
    let mut comparison = ParamComparison {
        count_before: before.len(),
        count_after: after.len(),
        compared: 0,
        missing: 0,
        extra: 0,
        mismatched: 0,
        max_abs_diff: 0.0,
    };
    for (param, value) in before {
        match after.iter().find(|(other, _)| other.id == param.id) {
            Some((_, other)) => {
                let difference = (value - other).abs();
                comparison.max_abs_diff = comparison.max_abs_diff.max(difference);
                if difference > 1e-4 {
                    comparison.mismatched += 1;
                }
                comparison.compared += 1;
            }
            None => comparison.missing += 1,
        }
    }
    comparison.extra = after
        .iter()
        .filter(|(param, _)| !before.iter().any(|(other, _)| other.id == param.id))
        .count();
    comparison
}

/// Peak and RMS of the shared two-second-plus-note render.
struct ToneStats {
    instrument: bool,
    silence_blocks: u32,
    note_blocks: u32,
    silence_peak: f32,
    note_peak: f32,
    note_rms: f64,
}

impl ToneStats {
    fn detail(&self) -> Value {
        json!({
            "instrument": self.instrument,
            "sample_rate": 48_000,
            "block": 512,
            "silence_blocks": self.silence_blocks,
            "note_blocks": self.note_blocks,
            "silence_peak": self.silence_peak,
            "note_peak": self.note_peak,
            "note_rms": self.note_rms,
            "note_peak_db": finite_json(level_db(f64::from(self.note_peak))),
            "note_rms_db": finite_json(level_db(self.note_rms)),
        })
    }
}

fn render_test_tone(instance: &mut Vst3Instance, instrument: bool) -> ToneStats {
    const SAMPLE_RATE: u32 = 48_000;
    const BLOCK: u32 = 512;
    let silence_blocks = SAMPLE_RATE.div_ceil(BLOCK) * 2;
    let note_blocks = SAMPLE_RATE.div_ceil(BLOCK);
    let mut silence_peak = 0.0f32;
    let mut silence_squares = 0.0f64;
    let mut silence_samples = 0u64;
    for _ in 0..silence_blocks {
        let block = instance.process_block(BLOCK);
        accumulate(
            &block,
            &mut silence_peak,
            &mut silence_squares,
            &mut silence_samples,
        );
    }
    if instrument {
        instance.note_on(60, 0.8, 0);
    }
    let mut note_peak = 0.0f32;
    let mut note_squares = 0.0f64;
    let mut note_samples = 0u64;
    for _ in 0..note_blocks {
        let block = instance.process_block(BLOCK);
        accumulate(&block, &mut note_peak, &mut note_squares, &mut note_samples);
    }
    if instrument {
        // Leave no held note behind for the next step's silence window.
        instance.note_off(60, 0);
        let _ = instance.process_block(BLOCK);
    }
    let note_rms = if note_samples == 0 {
        0.0
    } else {
        (note_squares / note_samples as f64).sqrt()
    };
    ToneStats {
        instrument,
        silence_blocks,
        note_blocks,
        silence_peak,
        note_peak,
        note_rms,
    }
}

fn level_db(value: f64) -> f64 {
    if value > 0.0 {
        20.0 * value.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Both outputs at or below this level are silence, not a level difference.
///
/// -120 dBFS is below the noise floor of every plugin measured here. Without
/// this floor a denormal tail would make two numerically different silences
/// look like an audible difference.
const SILENCE_FLOOR: f64 = 1e-6;

/// Difference in dB; two silent outputs are equal, one silent output is infinite.
fn db_difference(left: f64, right: f64) -> f64 {
    if left <= SILENCE_FLOOR && right <= SILENCE_FLOOR {
        return 0.0;
    }
    if left == right {
        return 0.0;
    }
    if left <= 0.0 || right <= 0.0 {
        return f64::INFINITY;
    }
    (20.0 * (left / right).log10()).abs()
}

fn finite_json(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

/// Why the host cannot load this bundle before it calls `dlopen`.
fn bundle_arch_error(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let stem = path.file_stem()?.to_str()?;
        let binary = path.join("Contents/MacOS").join(stem);
        let bytes = std::fs::read(&binary).ok()?;
        if bytes.len() < 8 {
            return None;
        }
        const CPU_TYPE_X86_64: u32 = 0x0100_0007;
        const CPU_TYPE_ARM64: u32 = 0x0100_000c;
        const MH_MAGIC_64: u32 = 0xfeed_facf;
        const FAT_MAGIC: u32 = 0xcafe_babe;
        let host = if cfg!(target_arch = "aarch64") {
            CPU_TYPE_ARM64
        } else if cfg!(target_arch = "x86_64") {
            CPU_TYPE_X86_64
        } else {
            return None;
        };
        let read_be = |offset: usize| -> Option<u32> {
            bytes
                .get(offset..offset + 4)
                .map(|slice| u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
        };
        let read_le = |offset: usize| -> Option<u32> {
            bytes
                .get(offset..offset + 4)
                .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
        };
        let mut slices = Vec::new();
        if read_be(0) == Some(FAT_MAGIC) {
            let count = read_be(4)? as usize;
            for index in 0..count.min(64) {
                if let Some(cpu) = read_be(8 + index * 20) {
                    slices.push(cpu);
                }
            }
        } else if read_le(0) == Some(MH_MAGIC_64) {
            slices.push(read_le(4)?);
        } else {
            return None;
        }
        if slices.contains(&host) {
            return None;
        }
        let host_name = if host == CPU_TYPE_ARM64 {
            "arm64"
        } else {
            "x86_64"
        };
        let found = slices
            .iter()
            .map(|cpu| match *cpu {
                CPU_TYPE_ARM64 => "arm64",
                CPU_TYPE_X86_64 => "x86_64",
                _ => "other",
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "bundle has no {host_name} slice (Mach-O slices: {found})"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

// ---------------------------------------------------------------------------
// VST3 driver
// ---------------------------------------------------------------------------

/// The VST3 adapter for the battery.
pub struct Vst3Driver {
    path: std::path::PathBuf,
    no_editor: bool,
    instance: Option<Vst3Instance>,
    second: Option<Vst3Instance>,
    instrument: bool,
    name: String,
    vendor: String,
    version: Option<String>,
}

impl Vst3Driver {
    /// A driver that loads `path` at 48 kHz with 512-frame blocks.
    pub fn new(path: &Path, no_editor: bool) -> Self {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Unknown")
            .to_string();
        Self {
            path: path.to_path_buf(),
            no_editor,
            instance: None,
            second: None,
            instrument: false,
            name,
            vendor: String::new(),
            version: None,
        }
    }

    /// Class name reported by the loaded component, or the bundle name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Vendor reported by the factory, empty until a load succeeds.
    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    /// Factory version, empty until a load succeeds.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    fn instance(&self) -> Result<&Vst3Instance, String> {
        self.instance
            .as_ref()
            .ok_or_else(|| "no loaded instance".to_string())
    }

    fn instance_mut(&mut self) -> Result<&mut Vst3Instance, String> {
        self.instance
            .as_mut()
            .ok_or_else(|| "no loaded instance".to_string())
    }
}

fn arrangement(channels: u32) -> &'static str {
    match channels {
        0 => "empty",
        1 => "mono",
        2 => "stereo",
        _ => "wide",
    }
}

fn ports_json(ports: &[AudioPort]) -> Vec<Value> {
    ports
        .iter()
        .enumerate()
        .map(|(index, port)| {
            json!({
                "index": index,
                "channels": port.channels,
                "arrangement": arrangement(port.channels),
                "first_pair": port.first_pair,
            })
        })
        .collect()
}

fn accumulate(block: &[f32], peak: &mut f32, sum_squares: &mut f64, samples: &mut u64) {
    for sample in block {
        let magnitude = sample.abs();
        if magnitude > *peak {
            *peak = magnitude;
        }
        *sum_squares += f64::from(*sample) * f64::from(*sample);
        *samples += 1;
    }
}

impl ConformanceDriver for Vst3Driver {
    fn load(&mut self) -> StepOutcome {
        if let Some(error) = bundle_arch_error(&self.path) {
            return StepOutcome::fail_with(
                json!({
                    "architecture": error.clone(),
                    "path": self.path.to_string_lossy(),
                }),
                error,
            );
        }
        match Vst3Instance::load(&self.path, 48_000, 512) {
            Ok(instance) => {
                let info = instance.class_info();
                let class_name = info.name.clone();
                let fallback_instrument = instance.input_ports().is_empty();
                self.name = class_name.clone();
                self.vendor = info.vendor.clone();
                self.version = (!info.version.is_empty()).then(|| info.version.clone());
                // IComponent does not say whether a class is an instrument; the
                // factory subcategories do. An instrument may still declare an
                // audio input for sidechain or audio-in use.
                self.instrument = Vst3Instance::scan_bundle(&self.path)
                    .iter()
                    .find(|class| class.name == class_name)
                    .map(|class| class.is_instrument())
                    .unwrap_or(fallback_instrument);
                let has_controller = !instance.raw_interfaces().controller.is_null();
                let detail = json!({
                    "class_name": info.name,
                    "category": info.category,
                    "vendor": info.vendor,
                    "factory_version": info.version,
                    "controller": has_controller,
                    "instrument": self.instrument,
                    "sample_rate": 48_000,
                    "max_frames": 512,
                });
                self.instance = Some(instance);
                if !has_controller {
                    return StepOutcome::fail_with(detail, "no edit controller");
                }
                StepOutcome::pass(detail)
            }
            Err(error) => StepOutcome::fail(error),
        }
    }

    fn buses(&mut self) -> StepOutcome {
        let instance = match self.instance() {
            Ok(instance) => instance,
            Err(error) => return StepOutcome::fail(error),
        };
        let inputs = instance.input_ports();
        let outputs = instance.output_ports();
        let input_channels: u32 = inputs.iter().map(|port| port.channels).sum();
        let output_channels: u32 = outputs.iter().map(|port| port.channels).sum();
        let input_buses = inputs.len();
        let output_buses = outputs.len();
        let extra_output_buses = output_buses > 1 || output_channels > 2;
        let detail = json!({
            "input_buses": input_buses,
            "output_buses": output_buses,
            "input_channels": input_channels,
            "output_channels": output_channels,
            "inputs": ports_json(&inputs),
            "outputs": ports_json(&outputs),
            "extra_output_buses": extra_output_buses,
            "instrument": self.instrument,
            "has_audio_input": input_channels > 0,
        });
        if output_channels == 0 {
            return StepOutcome::fail_with(detail, "no audio output channels declared");
        }
        StepOutcome::pass(detail)
    }

    fn state(&mut self) -> StepOutcome {
        let path = self.path.clone();
        let bytes = match self.instance() {
            Ok(instance) => match instance.state_bytes() {
                Ok(bytes) => bytes,
                Err(error) => return StepOutcome::fail(error),
            },
            Err(error) => return StepOutcome::fail(error),
        };
        if bytes.is_empty() {
            return StepOutcome::fail("getState returned 0 bytes");
        }
        let mut second = match Vst3Instance::load(&path, 48_000, 512) {
            Ok(second) => second,
            Err(error) => return StepOutcome::fail(format!("second load failed: {error}")),
        };
        if let Err(error) = second.set_component_state_bytes(&bytes) {
            return StepOutcome::fail(format!("setState failed: {error}"));
        }
        let restored = match second.state_bytes() {
            Ok(restored) => restored,
            Err(error) => return StepOutcome::fail(format!("second getState failed: {error}")),
        };
        let bytes_identical = restored == bytes;
        let size_delta = restored.len() as i64 - bytes.len() as i64;
        let controller_state = feed_controller_state(&second, &bytes);
        let params_before = match self.instance() {
            Ok(instance) => instance.get_params_with_values(),
            Err(error) => return StepOutcome::fail(error),
        };
        let params_after = second.get_params_with_values();
        let params = compare_params(&params_before, &params_after);
        let instrument = self.instrument;
        let render_before = match self.instance_mut() {
            Ok(instance) => render_test_tone(instance, instrument),
            Err(error) => return StepOutcome::fail(error),
        };
        let render_after = render_test_tone(&mut second, instrument);
        let peak_db_diff = db_difference(
            f64::from(render_before.note_peak),
            f64::from(render_after.note_peak),
        );
        let rms_db_diff = db_difference(render_before.note_rms, render_after.note_rms);
        self.second = Some(second);
        let detail = json!({
            "state_bytes_identical": bytes_identical,
            "state_size_before": bytes.len(),
            "state_size_after": restored.len(),
            "state_size_delta": size_delta,
            "state_first_difference": first_difference(&bytes, &restored),
            "state_head_before": hex_head(&bytes),
            "state_head_after": hex_head(&restored),
            "controller_set_component_state": match &controller_state {
                Ok(state) => state.label(),
                Err(error) => error.clone(),
            },
            "param_count_before": params.count_before,
            "param_count_after": params.count_after,
            "param_compared": params.compared,
            "param_missing": params.missing,
            "param_extra": params.extra,
            "param_mismatched": params.mismatched,
            "param_max_abs_diff": params.max_abs_diff,
            "render_before": render_before.detail(),
            "render_after": render_after.detail(),
            "peak_db_diff": finite_json(peak_db_diff),
            "rms_db_diff": finite_json(rms_db_diff),
        });
        let mut failures = Vec::new();
        match &controller_state {
            Ok(ControllerState::Accepted | ControllerState::NotImplemented) => {}
            Ok(ControllerState::Rejected(error)) => failures.push(error.clone()),
            Err(error) => failures.push(format!("controller state unavailable: {error}")),
        }
        if params.count_before != params.count_after || params.missing > 0 || params.extra > 0 {
            failures.push(format!(
                "parameter inventory changed after reload: {} before, {} after, {} missing, {} extra",
                params.count_before, params.count_after, params.missing, params.extra
            ));
        }
        if params.max_abs_diff > 1e-4 {
            failures.push(format!(
                "parameter vector differs after reload: max |delta| = {:.6} over {} parameters ({} mismatched)",
                params.max_abs_diff, params.compared, params.mismatched
            ));
        }
        if peak_db_diff > 0.1 {
            failures.push(format!(
                "render peak differs after reload: {peak_db_diff:.3} dB (before {}, after {})",
                render_before.note_peak, render_after.note_peak
            ));
        }
        if rms_db_diff > 0.1 {
            failures.push(format!(
                "render RMS differs after reload: {rms_db_diff:.3} dB (before {}, after {})",
                render_before.note_rms, render_after.note_rms
            ));
        }
        if failures.is_empty() {
            StepOutcome::pass(detail)
        } else {
            StepOutcome::fail_with(detail, failures.join("; "))
        }
    }

    fn presets(&mut self) -> StepOutcome {
        let instance = match self.instance() {
            Ok(instance) => instance,
            Err(error) => return StepOutcome::fail(error),
        };
        let raw = instance.raw_interfaces();
        let controller = controller_ref(instance);
        // SAFETY: the instance owns the component for its whole lifetime.
        let component =
            unsafe { ComRef::<IComponent>::from_raw_unchecked(raw.component as *mut IComponent) };
        let unit_info = controller
            .and_then(|controller| controller.cast::<IUnitInfo>())
            .or_else(|| component.cast::<IUnitInfo>());
        let program_data = controller
            .and_then(|controller| controller.cast::<IProgramListData>())
            .or_else(|| component.cast::<IProgramListData>());
        let mut lists = Vec::new();
        let mut program_count = 0i32;
        if let Some(info) = &unit_info {
            // SAFETY: unit_info owns the queried live interface for this battery
            // step; the driver calls it serially before instance teardown.
            let list_count = unsafe { info.getProgramListCount() }.max(0);
            for index in 0..list_count {
                // SAFETY: ProgramListInfo contains only integer scalars and fixed
                // UTF-16 arrays, all of which admit an all-zero representation.
                let mut list: ProgramListInfo = unsafe { std::mem::zeroed() };
                // SAFETY: info remains owned and list is a writable, correctly aligned
                // ProgramListInfo; the index is within the reported list count.
                if unsafe { info.getProgramListInfo(index, &mut list) } == kResultOk {
                    program_count += list.programCount.max(0);
                    lists.push(json!({
                        "id": list.id,
                        "name": char16_string(&list.name),
                        "programs": list.programCount,
                    }));
                }
            }
        }
        StepOutcome::pass(json!({
            "unit_info": unit_info.is_some(),
            "program_data": program_data.is_some(),
            "program_count": program_count,
            "lists": lists,
        }))
    }

    fn params(&mut self) -> StepOutcome {
        let instance = match self.instance() {
            Ok(instance) => instance,
            Err(error) => return StepOutcome::fail(error),
        };
        let params = instance.get_params();
        if params.is_empty() {
            return StepOutcome::fail("the plugin exposes no parameters");
        }
        let first = &params[0];
        let Some(controller) = controller_ref(instance) else {
            return StepOutcome::fail("no edit controller for setParamNormalized");
        };
        // SAFETY: controller remains borrowed from the live instance, and
        // first.id comes from that instance's reported parameter inventory.
        let before = unsafe { controller.getParamNormalized(first.id) };
        let target = if before < 0.5 { 1.0 } else { 0.0 };
        // SAFETY: the same live controller and declared parameter remain in
        // scope; target is normalized and the serial battery owns this edit.
        let result = unsafe { controller.setParamNormalized(first.id, target) };
        // SAFETY: the parameter and controller remain live across this serial
        // readback; no lifecycle transition occurs between these calls.
        let after = unsafe { controller.getParamNormalized(first.id) };
        // Leave the plugin as it was found: the process step runs after this.
        // SAFETY: the same owned instance/controller and declared parameter
        // remain live; before is the native value captured in this step.
        let restore_result = unsafe { controller.setParamNormalized(first.id, before) };
        let tolerance = if first.step_count > 1 {
            1.0 / f64::from(first.step_count) + 1e-9
        } else {
            1e-6
        };
        let detail = json!({
            "count": params.len(),
            "first": {
                "id": first.id,
                "name": first.name,
                "step_count": first.step_count,
                "before": before,
                "target": target,
                "after": after,
                "set_result": result,
                "restore_result": restore_result,
            },
        });
        if result != kResultOk {
            return StepOutcome::fail_with(
                detail,
                format!("setParamNormalized failed (0x{result:x})"),
            );
        }
        if (after - target).abs() > tolerance {
            return StepOutcome::fail_with(
                detail,
                format!(
                    "setParamNormalized did not stick: target {target}, read back {after} (tolerance {tolerance})"
                ),
            );
        }
        StepOutcome::pass(detail)
    }

    fn editor(&mut self) -> StepOutcome {
        if self.no_editor {
            return StepOutcome::skipped("disabled by --no-editor");
        }
        let instance = match self.instance() {
            Ok(instance) => instance,
            Err(error) => return StepOutcome::fail(error),
        };
        if instance.raw_interfaces().controller.is_null() {
            return StepOutcome::skipped("no edit controller");
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            run_editor_step(instance)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            StepOutcome::skipped("editor hosting is not implemented on this platform")
        }
    }

    fn process(&mut self) -> StepOutcome {
        let instrument = self.instrument;
        let stats = match self.instance_mut() {
            Ok(instance) => render_test_tone(instance, instrument),
            Err(error) => return StepOutcome::fail(error),
        };
        let detail = stats.detail();
        if stats.instrument && stats.note_peak == 0.0 {
            return StepOutcome::silent(detail);
        }
        StepOutcome::pass(detail)
    }

    fn teardown(&mut self) -> StepOutcome {
        let mut destroyed = Vec::new();
        if let Some(second) = self.second.take() {
            drop(second);
            destroyed.push("state_instance");
        }
        if let Some(instance) = self.instance.take() {
            drop(instance);
            destroyed.push("main_instance");
        }
        StepOutcome::pass(json!({
            "order": ["state_instance", "main_instance"],
            "destroyed": destroyed,
        }))
    }
}

fn hex_head(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
}

// ---------------------------------------------------------------------------
// Editor step
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[derive(Default)]
struct FrameState {
    window: AtomicUsize,
    attaching: AtomicBool,
    calls: AtomicUsize,
    pending: Mutex<Option<(i32, i32)>>,
    /// X11 display connection paired with `window`; unused on macOS.
    #[cfg(target_os = "linux")]
    display: AtomicUsize,
    /// Handlers the plugin registered through `IRunLoop`.
    #[cfg(target_os = "linux")]
    run_loop: Mutex<RunLoopEntries>,
}

/// Plugin run-loop registrations. Handlers are COM interface pointers kept as
/// integers so the state stays shareable; the plugin owns them until it
/// unregisters.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct RunLoopEntries {
    timers: Vec<(usize, Instant)>,
    fds: Vec<(usize, i32)>,
}

/// Host-side `IPlugFrame` for the conformance window. It records every resize
/// and forwards it to the offscreen window, except while `attached` runs.
#[cfg(target_os = "macos")]
struct ConformancePlugFrame {
    state: Arc<FrameState>,
}

#[cfg(target_os = "macos")]
impl IPlugFrameTrait for ConformancePlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSWindow;
        use objc2_foundation::NSSize;

        if new_size.is_null() {
            return kResultOk;
        }
        // SAFETY: VST3 guarantees a valid `ViewRect` for the duration of the call.
        let rect = unsafe { &*new_size };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return kResultOk;
        }
        self.state.calls.fetch_add(1, Ordering::AcqRel);
        if self.state.attaching.load(Ordering::Acquire) {
            *self
                .state
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((width, height));
            return kResultOk;
        }
        let pointer = self.state.window.load(Ordering::Acquire);
        if pointer != 0 && MainThreadMarker::new().is_some() {
            // SAFETY: the handle keeps the window retained until `close`.
            let window = unsafe { &*(pointer as *const NSWindow) };
            window.setContentSize(NSSize::new(width as f64, height as f64));
        }
        if !view.is_null() {
            // SAFETY: the caller passes its own live view and rect.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            // SAFETY: the caller supplies its live view and writable ViewRect;
            // the borrowed COM reference and rect survive this synchronous resize.
            let _ = unsafe { view.onSize(new_size) };
        }
        kResultOk
    }
}

#[cfg(target_os = "macos")]
impl Class for ConformancePlugFrame {
    type Interfaces = (IPlugFrame,);
}

/// Host-side `IPlugFrame` for the X11 conformance window.
#[cfg(target_os = "linux")]
struct ConformancePlugFrame {
    state: Arc<FrameState>,
}

#[cfg(target_os = "linux")]
impl IPlugFrameTrait for ConformancePlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kResultOk;
        }
        // SAFETY: VST3 guarantees a valid `ViewRect` for the duration of the call.
        let rect = unsafe { &*new_size };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return kResultOk;
        }
        self.state.calls.fetch_add(1, Ordering::AcqRel);
        if self.state.attaching.load(Ordering::Acquire) {
            *self
                .state
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((width, height));
            return kResultOk;
        }
        let display = self.state.display.load(Ordering::Acquire);
        let window = self.state.window.load(Ordering::Acquire);
        if window != 0 {
            crate::x11::resize(display as *mut c_void, window as u64, width, height);
        }
        if !view.is_null() {
            // SAFETY: the caller passes its own live view and rect.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            let _ = unsafe { view.onSize(new_size) };
        }
        kResultOk
    }
}

#[cfg(target_os = "linux")]
impl Class for ConformancePlugFrame {
    // `IRunLoop` is part of the frame on Linux: a plugin queries it from the
    // frame and refuses to attach when the frame cannot serve one.
    type Interfaces = (IPlugFrame, IRunLoop);
}

#[cfg(target_os = "linux")]
impl IRunLoopTrait for ConformancePlugFrame {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        if handler.is_null() {
            return kInvalidArgument;
        }
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.fds.retain(|(known, _)| *known != handler as usize);
        entries.fds.push((handler as usize, fd));
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.fds.retain(|(known, _)| *known != handler as usize);
        kResultOk
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, milliseconds: u64) -> tresult {
        if handler.is_null() || milliseconds == 0 {
            return kInvalidArgument;
        }
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries
            .timers
            .retain(|(known, _)| *known != handler as usize);
        entries.timers.push((
            handler as usize,
            Instant::now() + Duration::from_millis(milliseconds),
        ));
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        let mut entries = self
            .state
            .run_loop
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries
            .timers
            .retain(|(known, _)| *known != handler as usize);
        kResultOk
    }
}

/// Host-side `IPlugFrame` for the Win32 conformance window. It records every
/// resize and forwards it to the hidden window with `SetWindowPos`.
#[cfg(target_os = "windows")]
struct ConformancePlugFrame {
    state: Arc<FrameState>,
}

#[cfg(target_os = "windows")]
impl IPlugFrameTrait for ConformancePlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
        };

        if new_size.is_null() {
            return kResultOk;
        }
        // SAFETY: VST3 guarantees a valid `ViewRect` for the duration of the call.
        let rect = unsafe { &*new_size };
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return kResultOk;
        }
        self.state.calls.fetch_add(1, Ordering::AcqRel);
        if self.state.attaching.load(Ordering::Acquire) {
            *self
                .state
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((width, height));
            return kResultOk;
        }
        let window = self.state.window.load(Ordering::Acquire) as HWND;
        if !window.is_null() {
            // SAFETY: the handle is this thread's hidden window and stays alive
            // until the editor step destroys it.
            unsafe {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    0,
                    0,
                    width,
                    height,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        if !view.is_null() {
            // SAFETY: the caller passes its own live view and rect.
            let view = unsafe { ComRef::<IPlugView>::from_raw_unchecked(view) };
            // SAFETY: `view` wraps the caller's live view and `new_size` is the caller's rect for this call.
            let _ = unsafe { view.onSize(new_size) };
        }
        kResultOk
    }
}

#[cfg(target_os = "windows")]
impl Class for ConformancePlugFrame {
    type Interfaces = (IPlugFrame,);
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn view_size(view: &ComPtr<IPlugView>) -> Option<(i32, i32)> {
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: view owns the live interface and rect is writable aligned
    // ViewRect storage retained through this synchronous call.
    if unsafe { view.getSize(&mut rect) } != kResultOk {
        return None;
    }
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(target_os = "macos")]
fn pump_run_loop(millis: u64) {
    use objc2_foundation::{NSDate, NSRunLoop};
    let limit = NSDate::dateWithTimeIntervalSinceNow(millis as f64 / 1000.0);
    NSRunLoop::currentRunLoop().runUntilDate(&limit);
}

/// Serve the plugin's run-loop registrations for `millis`, then return.
///
/// A plugin that asks for a repaint timer or watches a file descriptor needs
/// these callbacks to fire, so the attach step gives them a short window.
#[cfg(target_os = "linux")]
fn pump_run_loop(state: &FrameState, millis: u64) {
    let deadline = Instant::now() + Duration::from_millis(millis);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(10));
        let (due, fds) = {
            let mut entries = state
                .run_loop
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let now = Instant::now();
            let mut due = Vec::new();
            entries.timers.retain(|(handler, at)| {
                if now >= *at {
                    due.push(*handler);
                    false
                } else {
                    true
                }
            });
            (due, entries.fds.clone())
        };
        for handler in due {
            // SAFETY: the plugin registered this handler and has not
            // unregistered it, so it is alive for the call.
            let handler = unsafe {
                ComRef::<ITimerHandler>::from_raw_unchecked(handler as *mut ITimerHandler)
            };
            unsafe { handler.onTimer() };
        }
        if fds.is_empty() {
            std::thread::sleep(wait);
            continue;
        }
        let mut poll_fds: Vec<libc::pollfd> = fds
            .iter()
            .map(|(_, fd)| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `poll_fds` is a valid slice of the reported length.
        let ready = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                wait.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if ready <= 0 {
            continue;
        }
        for (index, entry) in poll_fds.iter().enumerate() {
            if entry.revents == 0 {
                continue;
            }
            let (handler, fd) = fds[index];
            // SAFETY: the plugin registered this handler and has not
            // unregistered it, so it is alive for the call.
            let handler = unsafe {
                ComRef::<IEventHandler>::from_raw_unchecked(handler as *mut IEventHandler)
            };
            unsafe { handler.onFDIsSet(fd) };
        }
    }
}

/// Drain this thread's Win32 message queue for `millis`, then return.
///
/// A plugin creates and repaints its child windows from the message loop of the
/// thread that attached it, so the attach step has to keep that loop turning.
#[cfg(target_os = "windows")]
fn pump_run_loop(millis: u64) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let deadline = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < deadline {
        // SAFETY: `message` is a valid `MSG` and this thread owns the windows.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: the queue belongs to the calling thread and `message` is live.
        while unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: `message` was filled by `PeekMessageW` for this thread's queue.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Create a view, attach it to a hidden Win32 window, resize it through the
/// host frame, then detach and destroy the window.
#[cfg(target_os = "windows")]
fn run_editor_step(instance: &Vst3Instance) -> StepOutcome {
    use std::os::windows::ffi::OsStrExt;
    use vst3::Steinberg::kPlatformTypeHWND;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Ole::OleInitialize;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetClientRect, RegisterClassW, UnregisterClassW, WNDCLASSW,
        WS_OVERLAPPEDWINDOW,
    };

    // A plugin view may create OLE controls of its own; OLE initialises COM in
    // apartment-threaded mode, and a second call on the same thread is a no-op
    // that returns `S_FALSE`.
    // SAFETY: the reserved argument is null; a repeated call on this thread is a counted no-op.
    unsafe { OleInitialize(ptr::null()) };

    let Some(controller) = controller_ref(instance) else {
        return StepOutcome::skipped("no edit controller");
    };
    // SAFETY: `controller` is the instance's live edit controller and the name is a NUL-terminated literal.
    let raw_view = unsafe { controller.createView(c"editor".as_ptr()) };
    // SAFETY: `raw_view` is the owned reference `createView` returned, or null, which `from_raw` maps to `None`.
    let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
        // A null view is how a plugin without an editor answers; that is a
        // capability the plugin lacks, not a host-contract violation.
        return StepOutcome::skipped("createView returned null: the plugin has no editor");
    };
    // SAFETY: `view` is a live `IPlugView` owned by this function.
    if unsafe { view.isPlatformTypeSupported(kPlatformTypeHWND) } != kResultOk {
        return StepOutcome::fail("the editor does not support the HWND platform");
    }
    let size_before = view_size(&view);
    let (width, height) = size_before.unwrap_or((800, 600));

    let wide = |text: &str| -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let class_name = wide("OpenComposeConformanceEditor");
    let window_name = wide("OpenCompose conformance");
    // SAFETY: the module handle of this process is valid for its whole life.
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    // SAFETY: `class` describes a window procedure that is live for the class.
    if unsafe { RegisterClassW(&class) } == 0 {
        return StepOutcome::fail(format!(
            "RegisterClassW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // The window is never shown: no `WS_VISIBLE`, and it opens offscreen. Some
    // editors refuse a zero-size parent, so it starts at the view's own size.
    // SAFETY: the class was registered above with `module`, and both name buffers are NUL-terminated and outlive the call.
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            -10_000,
            -10_000,
            width,
            height,
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    if window.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: the class was registered above with this name and module, and no window was created from it.
        unsafe { UnregisterClassW(class_name.as_ptr(), module) };
        return StepOutcome::fail(format!(
            "CreateWindowExW failed, so this session cannot host an editor: {error}"
        ));
    }

    let state = Arc::new(FrameState::default());
    let wrapper = ComWrapper::new(ConformancePlugFrame {
        state: Arc::clone(&state),
    });
    let Some(frame) = wrapper.to_com_ptr::<IPlugFrame>() else {
        // SAFETY: `window` was created above on this thread and no view is attached to it, and the class was registered here with this name and module.
        unsafe {
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), module);
        }
        return StepOutcome::fail("could not create the host IPlugFrame");
    };
    // SAFETY: `view` is live and `frame` stays alive until the frame is cleared below.
    if unsafe { view.setFrame(frame.as_ptr()) } != kResultOk {
        // SAFETY: `window` was created above on this thread and no view is attached to it, and the class was registered here with this name and module.
        unsafe {
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), module);
        }
        return StepOutcome::fail("the editor rejected the host plug frame");
    }
    // SAFETY: `view` is a live `IPlugView` owned by this function.
    let can_resize_before = unsafe { view.canResize() } == kResultOk;
    state.window.store(window as usize, Ordering::Release);
    state.attaching.store(true, Ordering::Release);
    // SAFETY: `window` is a live HWND created by this thread, and the host frame is set.
    let attached = unsafe { view.attached(window, kPlatformTypeHWND) };
    state.attaching.store(false, Ordering::Release);
    if attached != kResultOk {
        state.window.store(0, Ordering::Release);
        // SAFETY: `window` was created above on this thread and no view is attached to it, and the class was registered here with this name and module.
        unsafe {
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), module);
        }
        return StepOutcome::fail(format!("attached failed (0x{attached:x})"));
    }
    if let Some((pending_width, pending_height)) = state
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        // SAFETY: `window` is the live HWND created above on this thread.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
                window,
                ptr::null_mut(),
                0,
                0,
                pending_width,
                pending_height,
                windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                    | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
            );
        }
        let mut pending = ViewRect {
            left: 0,
            top: 0,
            right: pending_width,
            bottom: pending_height,
        };
        // SAFETY: `view` is attached and `pending` is a local rect that outlives the call.
        let _ = unsafe { view.onSize(&mut pending) };
    }
    pump_run_loop(250);
    let size_after = view_size(&view);
    let Some((after_width, after_height)) = size_after else {
        state.window.store(0, Ordering::Release);
        // SAFETY: `view` is attached to `window`, which is still alive.
        let _ = unsafe { view.removed() };
        drop(view);
        // SAFETY: `window` was created above on this thread and no view is attached to it, and the class was registered here with this name and module.
        unsafe {
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), module);
        }
        return StepOutcome::fail("getSize returned no size after attach");
    };
    let target_width = after_width.saturating_add(64);
    let target_height = after_height.saturating_add(32);
    let mut target = ViewRect {
        left: 0,
        top: 0,
        right: target_width,
        bottom: target_height,
    };
    let calls_before = state.calls.load(Ordering::Acquire);
    // SAFETY: `frame` and `view` are live, and `target` is a local rect that outlives the call.
    let resize_result = unsafe { frame.resizeView(view.as_ptr(), &mut target) };
    let calls_after = state.calls.load(Ordering::Acquire);
    // Read the window back so the report says whether the host frame actually
    // moved it, not only that the plugin called back.
    // SAFETY: `RECT` is plain integers, so all zeroes is a valid value.
    let mut client: RECT = unsafe { std::mem::zeroed() };
    // SAFETY: `window` is the live HWND created above and `client` is a writable local.
    let client_ok = unsafe { GetClientRect(window, &mut client) } != 0;
    // SAFETY: `view` is a live `IPlugView` owned by this function.
    let can_resize_after = unsafe { view.canResize() } == kResultOk;
    // SAFETY: `view` is attached to `window`, which is still alive.
    let removed = unsafe { view.removed() };
    // SAFETY: `view` is live and detached; a null frame clears its host reference.
    let _ = unsafe { view.setFrame(ptr::null_mut()) };
    state.window.store(0, Ordering::Release);
    drop(view);
    // SAFETY: `window` was created above on this thread and no view is attached to it, and the class was registered here with this name and module.
    unsafe {
        DestroyWindow(window);
        UnregisterClassW(class_name.as_ptr(), module);
    }
    let detail = json!({
        "size_before_attach": size_before.map(|(width, height)| json!({"width": width, "height": height})),
        "size_after_attach": {"width": after_width, "height": after_height},
        "can_resize_before_attach": can_resize_before,
        "can_resize_after_attach": can_resize_after,
        "resize_request": {"width": target_width, "height": target_height},
        "resize_result": resize_result,
        "resize_callbacks": calls_after.saturating_sub(calls_before),
        "window_client_after_resize": client_ok.then(|| json!({
            "width": client.right.saturating_sub(client.left),
            "height": client.bottom.saturating_sub(client.top),
        })),
        "removed_result": removed,
        "platform": "HWND",
        "window_hidden": true,
        "window_offscreen": true,
    });
    if resize_result != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("host frame resizeView failed (0x{resize_result:x})"),
        );
    }
    if removed != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("IPlugView::removed failed (0x{removed:x})"),
        );
    }
    StepOutcome::pass(detail)
}

/// Default window procedure for the conformance editor window.
#[cfg(target_os = "windows")]
unsafe extern "system" fn window_proc(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    // SAFETY: the arguments are the ones Windows passed to this procedure.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(window, message, wparam, lparam)
    }
}

/// Create a view, attach it to an offscreen X11 window, resize it through the
/// host frame, then detach and destroy the window.
#[cfg(target_os = "linux")]
fn run_editor_step(instance: &Vst3Instance) -> StepOutcome {
    use vst3::Steinberg::kPlatformTypeX11EmbedWindowID;

    if !crate::x11::display_available() {
        let name = std::env::var("DISPLAY").unwrap_or_default();
        return StepOutcome::skipped(format!("no X display for editor hosting ('{name}')"));
    }
    let Some(controller) = controller_ref(instance) else {
        return StepOutcome::skipped("no edit controller");
    };
    let raw_view = unsafe { controller.createView(c"editor".as_ptr()) };
    let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
        // A null view is how a plugin without an editor answers; that is a
        // capability the plugin lacks, not a host-contract violation.
        return StepOutcome::skipped("createView returned null: the plugin has no editor");
    };
    // The editor must claim the X11 embedding platform before a host can
    // attach it; a Wayland-only editor reports the other platform type.
    if unsafe { view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) } != kResultOk {
        return StepOutcome::fail("the editor does not support the X11EmbedWindowID platform");
    }
    let size_before = view_size(&view);
    let (width, height) = size_before.unwrap_or((800, 600));
    let window = match crate::x11::EditorWindow::open("OpenCompose conformance", width, height) {
        Ok(window) => window,
        Err(error) => return StepOutcome::fail(error),
    };
    let state = Arc::new(FrameState::default());
    state
        .display
        .store(window.display() as usize, Ordering::Release);
    let wrapper = ComWrapper::new(ConformancePlugFrame {
        state: Arc::clone(&state),
    });
    let Some(frame) = wrapper.to_com_ptr::<IPlugFrame>() else {
        return StepOutcome::fail("could not create the host IPlugFrame");
    };
    if unsafe { view.setFrame(frame.as_ptr()) } != kResultOk {
        return StepOutcome::fail("the editor rejected the host plug frame");
    }
    let can_resize_before = unsafe { view.canResize() } == kResultOk;
    state.window.store(window.xid() as usize, Ordering::Release);
    state.attaching.store(true, Ordering::Release);
    let attached =
        unsafe { view.attached(window.xid() as *mut c_void, kPlatformTypeX11EmbedWindowID) };
    state.attaching.store(false, Ordering::Release);
    if attached != kResultOk {
        state.window.store(0, Ordering::Release);
        return StepOutcome::fail(format!("attached failed (0x{attached:x})"));
    }
    if let Some((pending_width, pending_height)) = state
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        window.resize(pending_width, pending_height);
        let mut pending = ViewRect {
            left: 0,
            top: 0,
            right: pending_width,
            bottom: pending_height,
        };
        let _ = unsafe { view.onSize(&mut pending) };
    }
    pump_run_loop(&state, 50);
    let size_after = view_size(&view);
    let Some((after_width, after_height)) = size_after else {
        state.window.store(0, Ordering::Release);
        let _ = unsafe { view.removed() };
        drop(view);
        return StepOutcome::fail("getSize returned no size after attach");
    };
    let target_width = after_width.saturating_add(64);
    let target_height = after_height.saturating_add(32);
    let mut target = ViewRect {
        left: 0,
        top: 0,
        right: target_width,
        bottom: target_height,
    };
    let calls_before = state.calls.load(Ordering::Acquire);
    let resize_result = unsafe { frame.resizeView(view.as_ptr(), &mut target) };
    let calls_after = state.calls.load(Ordering::Acquire);
    let can_resize_after = unsafe { view.canResize() } == kResultOk;
    let removed = unsafe { view.removed() };
    let _ = unsafe { view.setFrame(ptr::null_mut()) };
    state.window.store(0, Ordering::Release);
    drop(view);
    drop(window);
    let detail = json!({
        "size_before_attach": size_before.map(|(width, height)| json!({"width": width, "height": height})),
        "size_after_attach": {"width": after_width, "height": after_height},
        "can_resize_before_attach": can_resize_before,
        "can_resize_after_attach": can_resize_after,
        "resize_request": {"width": target_width, "height": target_height},
        "resize_result": resize_result,
        "resize_callbacks": calls_after.saturating_sub(calls_before),
        "removed_result": removed,
        "platform": "X11EmbedWindowID",
        "window_hidden": true,
        "window_offscreen": true,
    });
    if resize_result != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("host frame resizeView failed (0x{resize_result:x})"),
        );
    }
    if removed != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("IPlugView::removed failed (0x{removed:x})"),
        );
    }
    StepOutcome::pass(detail)
}

/// Create a view, attach it to a hidden offscreen window, resize it through the
/// host frame, then detach and close. AppKit main thread only.
#[cfg(target_os = "macos")]
fn run_editor_step(instance: &Vst3Instance) -> StepOutcome {
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return StepOutcome::fail("the editor step is not on the AppKit main thread");
    };
    let Some(controller) = controller_ref(instance) else {
        return StepOutcome::skipped("no edit controller");
    };
    // SAFETY: the driver retains this live controller on the AppKit main
    // thread, and the editor view name is a static NUL-terminated string.
    let raw_view = unsafe { controller.createView(c"editor".as_ptr()) };
    // SAFETY: createView returns either null or one owned IPlugView
    // reference; ComPtr adopts exactly that returned ownership.
    let Some(view) = (unsafe { ComPtr::<IPlugView>::from_raw(raw_view) }) else {
        // A null view is how a plugin without an editor answers; that is a
        // capability the plugin lacks, not a host-contract violation.
        return StepOutcome::skipped("createView returned null: the plugin has no editor");
    };
    // SAFETY: view owns its reference and the platform type points to the
    // SDK's static NUL-terminated NSView identifier.
    if unsafe { view.isPlatformTypeSupported(kPlatformTypeNSView) } != kResultOk {
        return StepOutcome::fail("the editor does not support the macOS NSView platform");
    }
    let size_before = view_size(&view);
    let (width, height) = size_before.unwrap_or((800, 600));
    let _app = NSApplication::sharedApplication(mtm);
    _app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let frame_rect = NSRect::new(
        NSPoint::new(-10_000.0, -10_000.0),
        NSSize::new(width as f64, height as f64),
    );
    // SAFETY: the window is created on the main thread, checked above.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame_rect,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the main-thread Retained owner, not close(), releases this
    // window; disabling release-on-close prevents a second native release.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setAlphaValue(0.0);
    window.setTitle(&NSString::from_str("OpenCompose conformance"));
    let Some(parent) = window.contentView() else {
        window.close();
        return StepOutcome::fail("the offscreen NSWindow has no content view");
    };
    let state = Arc::new(FrameState::default());
    let wrapper = ComWrapper::new(ConformancePlugFrame {
        state: Arc::clone(&state),
    });
    let Some(frame) = wrapper.to_com_ptr::<IPlugFrame>() else {
        window.close();
        return StepOutcome::fail("could not create the host IPlugFrame");
    };
    // SAFETY: view and frame both own their live references and frame is
    // retained throughout attachment, resize and detachment in this step.
    if unsafe { view.setFrame(frame.as_ptr()) } != kResultOk {
        window.close();
        return StepOutcome::fail("the editor rejected the host plug frame");
    }
    // SAFETY: the owned view remains live on the AppKit main thread; this
    // capability query does not transfer any native ownership.
    let can_resize_before = unsafe { view.canResize() } == kResultOk;
    state
        .window
        .store(Retained::as_ptr(&window) as usize, Ordering::Release);
    state.attaching.store(true, Ordering::Release);
    // SAFETY: parent owns the live NSView and the main-thread window,
    // view and host frame stay retained through removed(); platform is static.
    let attached = unsafe {
        view.attached(
            Retained::as_ptr(&parent) as *mut c_void,
            kPlatformTypeNSView,
        )
    };
    state.attaching.store(false, Ordering::Release);
    if attached != kResultOk {
        state.window.store(0, Ordering::Release);
        window.close();
        return StepOutcome::fail(format!("attached failed (0x{attached:x})"));
    }
    if let Some((pending_width, pending_height)) = state
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        window.setContentSize(NSSize::new(pending_width as f64, pending_height as f64));
        let mut pending = ViewRect {
            left: 0,
            top: 0,
            right: pending_width,
            bottom: pending_height,
        };
        // SAFETY: the attached view remains owned and pending is a writable
        // ViewRect with the positive dimensions admitted by the resize callback.
        let _ = unsafe { view.onSize(&mut pending) };
    }
    pump_run_loop(50);
    let size_after = view_size(&view);
    let Some((after_width, after_height)) = size_after else {
        state.window.store(0, Ordering::Release);
        // SAFETY: successful attached() is paired with removed() before either
        // the retained parent window or this owned view reference is released.
        let _ = unsafe { view.removed() };
        drop(view);
        window.close();
        return StepOutcome::fail("getSize returned no size after attach");
    };
    let target_width = after_width.saturating_add(64);
    let target_height = after_height.saturating_add(32);
    let mut target = ViewRect {
        left: 0,
        top: 0,
        right: target_width,
        bottom: target_height,
    };
    let calls_before = state.calls.load(Ordering::Acquire);
    // SAFETY: frame and view remain owned, target is writable aligned
    // ViewRect storage, and this synchronous call runs on the AppKit thread.
    let resize_result = unsafe { frame.resizeView(view.as_ptr(), &mut target) };
    let calls_after = state.calls.load(Ordering::Acquire);
    let content_after = window.contentView().map(|content| content.frame().size);
    // SAFETY: the same owned view is still attached and live; no reference
    // or native window has been released during the resize observation.
    let can_resize_after = unsafe { view.canResize() } == kResultOk;
    // SAFETY: the successful attachment still owns its parent and view;
    // removed() completes before either owner is released.
    let removed = unsafe { view.removed() };
    // SAFETY: the view remains owned; null explicitly disconnects the host
    // frame before that frame's retained COM reference leaves scope.
    let _ = unsafe { view.setFrame(ptr::null_mut()) };
    state.window.store(0, Ordering::Release);
    drop(view);
    window.close();
    pump_run_loop(10);
    let detail = json!({
        "size_before_attach": size_before.map(|(width, height)| json!({"width": width, "height": height})),
        "size_after_attach": {"width": after_width, "height": after_height},
        "can_resize_before_attach": can_resize_before,
        "can_resize_after_attach": can_resize_after,
        "resize_request": {"width": target_width, "height": target_height},
        "resize_result": resize_result,
        "resize_callbacks": calls_after.saturating_sub(calls_before),
        "window_content_after_resize": content_after.map(|size| json!({"width": size.width, "height": size.height})),
        "removed_result": removed,
        "window_hidden": true,
        "window_offscreen": true,
    });
    if resize_result != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("host frame resizeView failed (0x{resize_result:x})"),
        );
    }
    if removed != kResultOk {
        return StepOutcome::fail_with(
            detail,
            format!("IPlugView::removed failed (0x{removed:x})"),
        );
    }
    StepOutcome::pass(detail)
}

// ---------------------------------------------------------------------------
// Child entry
// ---------------------------------------------------------------------------

/// Run the battery for one bundle and stream the result as JSON Lines.
///
/// The caller is the CLI's child mode; it leaves through
/// [`crate::exit_process`] after this returns so the hosted module's static
/// destructors never run.
pub fn run_child(path: &Path, timeout_ms: u64, no_editor: bool) -> i32 {
    install_signal_handlers();
    let watchdog = Watchdog::start(Duration::from_millis(timeout_ms.max(1)));
    let mut driver = Vst3Driver::new(path, no_editor);
    run_battery(&mut driver, Some(&watchdog), |report| {
        emit_line(&json!({ "step": report }).to_string());
    });
    emit_line(
        &json!({
            "done": true,
            "path": path.to_string_lossy(),
            "name": driver.name(),
            "vendor": driver.vendor(),
            "version": driver.version(),
        })
        .to_string(),
    );
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A driver that passes every step except the one under test.
    struct FakeDriver {
        fail: Option<Step>,
        editor_ran: bool,
        process_ran: bool,
        teardown_ran: bool,
    }

    impl FakeDriver {
        fn new(fail: Option<Step>) -> Self {
            Self {
                fail,
                editor_ran: false,
                process_ran: false,
                teardown_ran: false,
            }
        }

        fn outcome(&self, step: Step, detail: Value) -> StepOutcome {
            if self.fail == Some(step) {
                StepOutcome::fail(format!("{} fake failure", step.name()))
            } else {
                StepOutcome::pass(detail)
            }
        }
    }

    impl ConformanceDriver for FakeDriver {
        fn load(&mut self) -> StepOutcome {
            self.outcome(Step::Load, json!({"class_name": "Fake"}))
        }
        fn buses(&mut self) -> StepOutcome {
            self.outcome(Step::Buses, json!({"output_buses": 1}))
        }
        fn state(&mut self) -> StepOutcome {
            self.outcome(Step::State, json!({"identical": true}))
        }
        fn presets(&mut self) -> StepOutcome {
            self.outcome(Step::Presets, json!({"program_count": 0}))
        }
        fn params(&mut self) -> StepOutcome {
            self.outcome(Step::Params, json!({"count": 1}))
        }
        fn editor(&mut self) -> StepOutcome {
            self.editor_ran = true;
            self.outcome(
                Step::Editor,
                json!({"size_after_attach": {"width": 100, "height": 50}}),
            )
        }
        fn process(&mut self) -> StepOutcome {
            self.process_ran = true;
            self.outcome(Step::Process, json!({"note_peak": 0.5}))
        }
        fn teardown(&mut self) -> StepOutcome {
            self.teardown_ran = true;
            self.outcome(Step::Teardown, json!({"destroyed": ["main_instance"]}))
        }
    }

    #[test]
    fn battery_reports_every_step_and_a_failure_does_not_stop_later_steps() {
        let mut driver = FakeDriver::new(Some(Step::Params));
        let reports = run_battery(&mut driver, None, |_| {});

        assert_eq!(reports.len(), Step::ALL.len());
        assert_eq!(
            reports
                .iter()
                .map(|report| report.step.as_str())
                .collect::<Vec<_>>(),
            STEP_NAMES
        );
        let names: BTreeMap<_, _> = reports
            .iter()
            .map(|report| (report.step.as_str(), report.status))
            .collect();
        assert_eq!(names["load"], StepStatus::Pass);
        assert_eq!(names["params"], StepStatus::Fail);
        assert_eq!(names["editor"], StepStatus::Pass);
        assert_eq!(names["process"], StepStatus::Pass);
        assert_eq!(names["teardown"], StepStatus::Pass);
        let params = reports
            .iter()
            .find(|report| report.step == "params")
            .expect("params step");
        assert_eq!(params.error.as_deref(), Some("params fake failure"));
        assert!(driver.editor_ran && driver.process_ran && driver.teardown_ran);
    }

    #[test]
    fn a_failed_load_skips_the_rest() {
        let mut driver = FakeDriver::new(Some(Step::Load));
        let reports = run_battery(&mut driver, None, |_| {});
        assert_eq!(reports[0].status, StepStatus::Fail);
        assert!(
            reports[1..]
                .iter()
                .all(|report| report.status == StepStatus::Skipped)
        );
        assert!(!driver.editor_ran && !driver.process_ran);
    }

    fn param(id: u32, name: &str) -> Vst3Param {
        Vst3Param {
            id,
            name: name.to_string(),
            default: 0.0,
            step_count: 0,
            unit: String::new(),
        }
    }

    #[test]
    fn parameter_comparison_matches_by_id_and_measures_the_largest_difference() {
        let before = vec![
            (param(1, "Cutoff"), 0.5),
            (param(2, "Resonance"), 0.25),
            (param(3, "Drive"), 0.75),
        ];
        let after = vec![
            (param(3, "Drive"), 0.75),
            (param(1, "Cutoff"), 0.50005),
            (param(2, "Resonance"), 0.75),
        ];
        let comparison = compare_params(&before, &after);
        assert_eq!(comparison.count_before, 3);
        assert_eq!(comparison.count_after, 3);
        assert_eq!(comparison.compared, 3);
        assert_eq!(comparison.missing, 0);
        assert_eq!(comparison.extra, 0);
        assert_eq!(comparison.mismatched, 1);
        assert!((comparison.max_abs_diff - 0.5).abs() < 1e-12);
    }

    #[test]
    fn silent_instrument_is_informational_not_a_failure() {
        let mut report = PluginReport::unreached(Path::new("/tmp/Fake.vst3"), "Fake");
        report.steps = Step::ALL
            .iter()
            .map(|step| {
                StepReport::new(
                    *step,
                    if *step == Step::Process {
                        StepStatus::Silent
                    } else {
                        StepStatus::Pass
                    },
                    0,
                    json!({}),
                    None,
                )
            })
            .collect();
        assert!(report.passes_every_step());
        assert_eq!(report.failures().count(), 0);
        assert_eq!(
            report.steps[Step::Process.index()].status,
            StepStatus::Silent
        );
        assert!(!StepStatus::Silent.is_failure());
    }

    #[test]
    fn silence_and_a_signal_differ_by_infinity_db() {
        assert_eq!(db_difference(0.0, 0.0), 0.0);
        assert!(db_difference(0.0, 0.5).is_infinite());
        assert!(db_difference(0.5, 0.0).is_infinite());
        assert_eq!(db_difference(1e-8, 2e-8), 0.0);
        assert!((db_difference(0.5, 0.5) - 0.0).abs() < 1e-12);
        assert!((db_difference(0.5, 1.0) - 6.0206).abs() < 0.001);
    }

    #[test]
    fn watchdog_reports_a_hanging_step_and_exits_seventy() {
        const CHILD: &str = "OCP_CONFORMANCE_WATCHDOG_CHILD";
        if std::env::var_os(CHILD).is_some() {
            install_signal_handlers();
            let watchdog = Watchdog::start(Duration::from_millis(150));
            watchdog.begin(Step::Editor);
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        let executable = std::env::current_exe().expect("test executable");
        let output = std::process::Command::new(executable)
            .arg("--exact")
            .arg("conformance::tests::watchdog_reports_a_hanging_step_and_exits_seventy")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .expect("spawn the hanging child");
        assert_eq!(
            output.status.code(),
            Some(70),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"fatal\":\"hang\""), "stdout: {stdout}");
        assert!(stdout.contains("\"step\":\"editor\""), "stdout: {stdout}");
    }
}
