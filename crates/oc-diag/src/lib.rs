//! Diagnostics: opt-in debug logging and performance counters.
//!
//! Two cargo features decide what can be switched on, two runtime flags decide
//! whether it is:
//!
//! | feature     | runtime flag | effect |
//! |-------------|--------------|--------|
//! | `debug-log` | `--debug`    | debug/trace call sites exist, level raised to Debug |
//! | `perf-log`  | `--perf`     | counters and timers exist, a reporter thread prints them |
//!
//! With a feature off the corresponding call sites compile to nothing, so a
//! normal build pays neither code size nor a branch. Without either flag the
//! logger still installs, at Warn, so panics and errors reach the log file.
//!
//! The audio callback must never log or allocate: it holds `Option<&Counter>`
//! values obtained at device setup and only bumps atomics.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use log;

pub mod time;

static DEBUG: AtomicBool = AtomicBool::new(false);
static PERF: AtomicBool = AtomicBool::new(false);
static STARTED: OnceLock<Instant> = OnceLock::new();

fn started() -> Instant {
    *STARTED.get_or_init(Instant::now)
}

fn sink() -> &'static Mutex<Option<File>> {
    static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// True when the build has `debug-log` and `init` was told `--debug`.
pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// True when the build has `perf-log` and `init` was told `--perf`.
pub fn perf_enabled() -> bool {
    PERF.load(Ordering::Relaxed)
}

/// True when either switch is on; cheap guard for messages that build strings.
pub fn logging_enabled() -> bool {
    debug_enabled() || perf_enabled()
}

/// Default log directory, following the platform's log convention.
pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Logs/OpenCompose");
        }
    }
    std::env::temp_dir().join("OpenCompose")
}

/// Where the current session writes, once [`init`] has run.
pub fn log_path() -> Option<PathBuf> {
    FILE_PATH.get().cloned()
}

static FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Startup configuration for diagnostics. An explicit directory replaces only
/// the platform default; it does not change any process environment variable.
#[derive(Debug, Clone, Copy)]
pub struct InitOptions<'a> {
    pub program: &'a str,
    pub debug: bool,
    pub perf: bool,
    pub directory: Option<&'a Path>,
}

impl InitOptions<'_> {
    fn directory_or_else(&self, platform_default: impl FnOnce() -> PathBuf) -> PathBuf {
        self.directory
            .map_or_else(platform_default, Path::to_path_buf)
    }
}

/// Install the logger and set the two switches.
///
/// `program` becomes the file-name prefix. Returns the log file path when one
/// was created. Safe to call once; later calls only update the switches.
pub fn init(program: &str, debug: bool, perf: bool) -> Option<PathBuf> {
    init_with_options(InitOptions {
        program,
        debug,
        perf,
        directory: None,
    })
}

/// Install diagnostics with a caller-selected directory. The first created log
/// fixes the destination for this process; subsequent calls update switches
/// and return that same path. A failed explicit destination never falls back
/// to the platform directory.
pub fn init_with_options(options: InitOptions<'_>) -> Option<PathBuf> {
    let InitOptions {
        program,
        debug,
        perf,
        ..
    } = options;
    DEBUG.store(debug && cfg!(feature = "debug-log"), Ordering::Relaxed);
    PERF.store(perf && cfg!(feature = "perf-log"), Ordering::Relaxed);
    started();

    let path = FILE_PATH.get().cloned();
    if path.is_some() {
        return path;
    }
    // No flag, no file: a bare `ocp render` would otherwise litter the log
    // directory once per invocation. Warnings still reach stderr.
    if !debug_enabled() && !perf_enabled() {
        static LOGGER: FileLogger = FileLogger;
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Warn);
        return None;
    }
    let dir = options.directory_or_else(log_dir);
    let file = std::fs::create_dir_all(&dir).ok().and_then(|()| {
        let stamp = timestamp();
        let path = dir.join(format!("{program}-{stamp}-{}.log", std::process::id()));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|file| (path, file))
    });
    if let Some((path, file)) = file {
        if let Ok(mut guard) = sink().lock() {
            *guard = Some(file);
        }
        let _ = FILE_PATH.set(path.clone());
    }

    let level = if debug_enabled() {
        log::LevelFilter::Debug
    } else if perf_enabled() {
        log::LevelFilter::Info
    } else {
        log::LevelFilter::Warn
    };
    static LOGGER: FileLogger = FileLogger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);

    if let Some(path) = FILE_PATH.get() {
        log::info!(
            "diagnostics: program={program} debug={debug} perf={perf} features(debug-log={}, perf-log={}) log={}",
            cfg!(feature = "debug-log"),
            cfg!(feature = "perf-log"),
            path.display()
        );
    }
    FILE_PATH.get().cloned()
}

struct FileLogger;

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.level() > log::max_level() {
            return false;
        }
        // Debugging this application must not mean decoding another crate's
        // trace: a target outside `oc_*` is capped at warnings unless the
        // environment asks for it.
        if metadata.level() > log::Level::Warn
            && !metadata.target().starts_with("oc_")
            && !metadata.target().starts_with("opencompose")
        {
            return std::env::var_os("OCP_LOG_THIRD_PARTY").is_some();
        }
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let elapsed = started().elapsed().as_secs_f64();
        let line = format!(
            "[{:>9.3}] {:<5} {}: {}\n",
            elapsed,
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut guard) = sink().lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.write_all(line.as_bytes());
        }
        if record.level() <= log::Level::Warn || debug_enabled() {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = sink().lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.flush();
        }
    }
}

/// Local wall-clock stamp for the file name.
///
/// The log lines themselves carry seconds since start, which is what correlates
/// with a report; a local stamp only makes the file easy to find.
fn timestamp() -> String {
    if let Ok(output) = std::process::Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        && output.status.success()
    {
        let stamp = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stamp.len() == 15 {
            return stamp;
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day) = time::civil_from_days(secs / 86_400);
    let rem = secs % 86_400;
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ============================================================================
// Performance counters
// ============================================================================

/// A named counter. `total` sums, `max` keeps the largest observed value, and
/// `count` counts observations, so both a rate and a worst case survive a run.
#[derive(Debug)]
pub struct Counter {
    name: &'static str,
    unit: &'static str,
    total: AtomicU64,
    max: AtomicU64,
    count: AtomicU64,
}

impl Counter {
    /// Add one occurrence.
    pub fn add(&self, value: u64) {
        self.total.fetch_add(value, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        if value > self.max.load(Ordering::Relaxed) {
            self.max.fetch_max(value, Ordering::Relaxed);
        }
    }

    /// Record one duration in milliseconds.
    pub fn observe_ms(&self, ms: f64) {
        let micros = (ms.max(0.0) * 1000.0) as u64;
        self.add(micros);
    }

    /// The counter's registered name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// `(count, total, max)` in the counter's unit (milliseconds for durations).
    pub fn read(&self) -> (u64, u64, u64) {
        (
            self.count.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }
}

fn registry() -> &'static Mutex<BTreeMap<&'static str, &'static Counter>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<&'static str, &'static Counter>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Intern a counter by name.
///
/// The first call for a name allocates and takes a lock, so call it at setup and
/// keep the `&'static Counter`; never call it from the audio callback.
pub fn counter(name: &'static str) -> &'static Counter {
    counter_in(name, "ms")
}

/// Intern a counter whose unit is not milliseconds (events, frames, bytes).
pub fn counter_in(name: &'static str, unit: &'static str) -> &'static Counter {
    let mut guard = registry().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = guard.get(name) {
        return existing;
    }
    let counter: &'static Counter = Box::leak(Box::new(Counter {
        name,
        unit,
        total: AtomicU64::new(0),
        max: AtomicU64::new(0),
        count: AtomicU64::new(0),
    }));
    guard.insert(name, counter);
    counter
}

/// Register a counter only when `--perf` is on.
///
/// Returns `None` when the build has no `perf-log` or the flag is off, which is
/// what the audio callback stores so it can bump counters without allocating.
pub fn perf_counter(name: &'static str) -> Option<&'static Counter> {
    if perf_enabled() {
        Some(counter(name))
    } else {
        None
    }
}

/// Format every registered counter as one line per counter, for a log or a
/// report at the end of a run.
pub fn report() -> Vec<String> {
    let guard = registry().lock().unwrap_or_else(|error| error.into_inner());
    guard
        .values()
        .filter_map(|counter| {
            let (count, total, max) = counter.read();
            if count == 0 {
                return None;
            }
            // Durations are stored in microseconds and printed in milliseconds.
            let (mean, max, total, unit) = if counter.unit == "ms" {
                (
                    total as f64 / count as f64 / 1000.0,
                    max as f64 / 1000.0,
                    total as f64 / 1000.0,
                    "ms",
                )
            } else {
                (
                    total as f64 / count as f64,
                    max as f64,
                    total as f64,
                    counter.unit,
                )
            };
            Some(format!(
                "{:<34} n={:<8} mean={:>10.3}{unit} max={:>10.3}{unit} total={:>12.3}{unit}",
                counter.name, count, mean, max, total
            ))
        })
        .collect()
}

/// Log every counter once. Safe to call from a timer thread.
pub fn log_report(label: &str) {
    for line in report() {
        log::info!("perf[{label}] {line}");
    }
}

/// Start the periodic reporter, if `--perf` is on and the build has `perf-log`.
pub fn spawn_reporter(interval: Duration) -> Option<std::thread::JoinHandle<()>> {
    if !perf_enabled() {
        return None;
    }
    let handle = std::thread::Builder::new()
        .name("oc-diag-reporter".into())
        .spawn(move || {
            loop {
                std::thread::sleep(interval);
                log_report("tick");
            }
        })
        .ok()?;
    Some(handle)
}

/// Times a scope and feeds the result to a counter on drop.
pub struct Timer {
    counter: &'static Counter,
    start: Instant,
    label: Option<&'static str>,
}

impl Timer {
    /// Start timing one operation for `counter`; drop stops it.
    pub fn start(counter: &'static Counter) -> Self {
        Self {
            counter,
            start: Instant::now(),
            label: None,
        }
    }

    /// Also log the elapsed time at debug level when `--debug` is on.
    pub fn labelled(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        self.counter.observe_ms(elapsed.as_secs_f64() * 1000.0);
        #[cfg(feature = "debug-log")]
        if debug_enabled()
            && let Some(label) = self.label
        {
            log::debug!("{label}: {:.2} ms", elapsed.as_secs_f64() * 1000.0);
        }
    }
}

// ============================================================================
// Macros
// ============================================================================

/// True when this build compiled the `debug-log` call sites in.
#[cfg(feature = "debug-log")]
pub const DEBUG_COMPILED: bool = true;
/// False when the build left the debug call sites out; the branch folds away.
#[cfg(not(feature = "debug-log"))]
pub const DEBUG_COMPILED: bool = false;

/// True when this build compiled the `perf-log` counters in.
#[cfg(feature = "perf-log")]
pub const PERF_COMPILED: bool = true;
/// False when the build left the perf counters out.
#[cfg(not(feature = "perf-log"))]
pub const PERF_COMPILED: bool = false;

/// Debug message: exists only with `debug-log`, fires only with `--debug`.
///
/// Without the feature `DEBUG_COMPILED` is a `false` constant, so the branch and
/// the formatting arguments fold away.
#[macro_export]
macro_rules! diag_debug {
    ($($arg:tt)*) => {{
        if $crate::DEBUG_COMPILED && $crate::debug_enabled() {
            $crate::log::debug!($($arg)*);
        }
    }};
}

/// Info message that exists only when one of the switches is on, so the normal
/// run stays silent and its log file only carries warnings and errors.
#[macro_export]
macro_rules! diag_info {
    ($($arg:tt)*) => {{
        if $crate::DEBUG_COMPILED && $crate::logging_enabled() {
            $crate::log::info!($($arg)*);
        }
    }};
}

/// Warning: always compiled and always logged. Used for conditions the user may
/// have to act on even in a normal build.
#[macro_export]
macro_rules! diag_warn {
    ($($arg:tt)*) => {{
        $crate::log::warn!($($arg)*);
    }};
}

/// Register a perf counter, or `None` when `perf-log` or `--perf` are missing.
#[macro_export]
macro_rules! perf_counter {
    ($name:expr) => {
        $crate::perf_counter($name)
    };
}

/// Start a timer for a counter, or `None` when counters are not compiled in.
#[macro_export]
macro_rules! perf_timer {
    ($name:expr) => {
        $crate::perf_counter($name).map($crate::Timer::start)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_directory_override_uses_only_the_supplied_profile_path() {
        let directory = PathBuf::from("profile-é-声").join("logs");
        let options = InitOptions {
            program: "test",
            debug: false,
            perf: true,
            directory: Some(&directory),
        };
        assert_eq!(
            options.directory_or_else(|| panic!(
                "explicit profile must not resolve the operator log directory"
            )),
            directory
        );
    }

    #[test]
    fn logging_directory_unset_preserves_the_platform_default_resolver() {
        let directory = PathBuf::from("platform-default").join("logs");
        let options = InitOptions {
            program: "test",
            debug: true,
            perf: false,
            directory: None,
        };
        assert_eq!(options.directory_or_else(|| directory.clone()), directory);
    }

    #[test]
    fn counters_accumulate_total_max_and_count() {
        let counter = counter_in("test.counter.accumulate", "ms");
        counter.add(3);
        counter.add(11);
        counter.add(5);
        assert_eq!(counter.read(), (3, 19, 11));
    }

    #[test]
    fn timers_record_into_their_counter() {
        let counter = counter_in("test.counter.timer", "ms");
        {
            let _timer = Timer::start(counter);
            std::thread::sleep(Duration::from_millis(2));
        }
        let (count, total, _max) = counter.read();
        assert_eq!(count, 1);
        assert!(total >= 1_000, "total was {total} us");
    }

    #[test]
    fn report_skips_counters_that_never_fired() {
        let _ = counter_in("test.counter.silent", "ms");
        let lines = report();
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("test.counter.silent")),
            "{lines:?}"
        );
    }

    #[test]
    fn timestamps_are_clock_shaped() {
        let stamp = timestamp();
        assert!(stamp.len() == 15 || stamp.len() == 16, "{stamp}");
        assert_eq!(&stamp[8..9], "-");
    }

    #[test]
    fn calendar_conversion_matches_known_dates() {
        assert_eq!(time::civil_from_days(0), (1970, 1, 1));
        assert_eq!(time::civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(time::civil_from_days(19_723), (2024, 1, 1));
    }
}
