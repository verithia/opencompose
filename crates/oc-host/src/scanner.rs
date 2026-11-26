//! Out-of-process plugin scanning.
//!
//! A scan runs in a helper process per bundle, so a plugin that crashes on load
//! is recorded as a receipt instead of ending the application. Receipts make a
//! repeat scan cheap and let a quarantined bundle stay skipped.

mod dispatch;
mod parameter_inventory;
pub(crate) mod parent_lifetime;
pub use parameter_inventory::{PluginParameterInventory, inspect_plugin_parameters};

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A timeout quarantines a bundle for a day, not forever: the stall may have
/// been the machine, not the plugin.
const TIMEOUT_QUARANTINE_SECS: u64 = 24 * 60 * 60;

use serde::{Deserialize, Serialize};

use super::resolution::{self, PluginQuery};
use super::{PluginFormat, PluginInfo, discover_plugin_files, display_path};

const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// Schema of every receipt this host writes.
const RECEIPT_SCHEMA: &str = "opencompose_plugin_scan_receipt_v1";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
/// A helper's JSON inventory is far smaller than this; the bound keeps a
/// broken or malicious helper from allocating without limit.
const MAX_HELPER_STDOUT_BYTES: usize = 8 * 1024 * 1024;
/// Diagnostics are read for the receipt, not for the user's console.
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;
/// CLI flag that runs the scanner as a helper.
pub const HELPER_MODE_ARG: &str = "--opencompose-plugin-scan-helper";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ScanRuntime {
    #[default]
    Native,
    IntelVst2StereoInsert,
    IntelVst2Playback,
    IntelVst3Playback,
}

impl ScanRuntime {
    fn for_bundle(path: &Path, format: PluginFormat) -> Self {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        if format == PluginFormat::Vst2 && crate::vst2_bridge::needs_translation(path) {
            return Self::IntelVst2Playback;
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        if format == PluginFormat::Vst3 && crate::vst2_bridge::needs_vst3_translation(path) {
            return Self::IntelVst3Playback;
        }
        let _ = (path, format);
        Self::Native
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Outcome of one isolated plugin scan.
pub enum ScanStatus {
    Passed,
    Failed,
    Crashed,
    TimedOut,
    HelperMissing,
}

/// The loadable binary a receipt describes. A bundle rebuilt in place keeps its
/// path but not this identity, so its old receipt must not quarantine it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BundleIdentity {
    pub path: String,
    pub modified_nanos: Option<u64>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Cached result of scanning one plugin bundle.
pub struct PluginScanReceipt {
    pub schema: String,
    pub path: String,
    pub format: PluginFormat,
    pub status: ScanStatus,
    pub scanned_at_unix: u64,
    pub duration_ms: u128,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    /// Absent in a receipt written before binaries were keyed; such a receipt
    /// never matches a live bundle, so the bundle is retried once.
    #[serde(default)]
    pub binary: Option<BundleIdentity>,
    /// The host version that wrote the receipt. Only a receipt from this
    /// version is reused, because the helper's classification can change.
    #[serde(default)]
    pub host_version: Option<String>,
    #[serde(default)]
    runtime: ScanRuntime,
    pub plugins: Vec<PluginInfo>,
}

/// Counts from one scan pass over the discovered bundles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Bundles inspected in a helper process.
    pub scanned: usize,
    /// Unchanged bundles whose passing receipt answered without a helper run.
    pub reused: usize,
    /// Quarantined bundles an explicit rescan retried.
    pub retried: usize,
    /// Bundles whose probe failed, crashed, or timed out during this pass.
    pub failed: usize,
}

/// How far the scan pass running now has come. Bundles answered by a reusable
/// receipt count as done from the start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScanProgress {
    pub done: usize,
    pub total: usize,
    /// File name of the bundle a helper started most recently.
    pub current: Option<String>,
}

fn progress_slot() -> &'static std::sync::Mutex<Option<ScanProgress>> {
    static PROGRESS: std::sync::Mutex<Option<ScanProgress>> = std::sync::Mutex::new(None);
    &PROGRESS
}

/// Progress of the inventory scan running in this process, or `None` when no
/// pass is running. Any caller can poll it, whoever started the scan.
pub fn scan_progress() -> Option<ScanProgress> {
    progress_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn publish_progress(progress: Option<ScanProgress>) {
    *progress_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = progress;
}

/// Clears the published progress when the pass ends, also by unwinding.
struct ProgressPass;

impl Drop for ProgressPass {
    fn drop(&mut self) {
        publish_progress(None);
    }
}

/// Scan every discovered CLAP/VST2/VST3 bundle once.
///
/// `force` is the explicit rescan: a quarantined bundle is retried instead of
/// skipped. Each bundle still runs at most once per pass, so one that keeps
/// crashing costs one bounded helper run.
pub fn scan_discovered_plugins_isolated(force: bool) -> (Vec<PluginInfo>, ScanReport) {
    let request = dispatch::request();
    let receipt_file = receipts_path();
    let receipts = receipt_file
        .as_deref()
        .map(read_receipts)
        .unwrap_or_default();
    let _pass = ProgressPass;
    scan_files_isolated(
        discover_plugin_files(),
        force || rescan_requested(),
        &receipts,
        receipt_file.as_deref(),
        &|path, format| {
            request.scan(path, format, || {
                scan_bundle_isolated(Path::new(path), format, scan_timeout())
            })
        },
        &|progress| publish_progress(Some(progress)),
    )
}

/// Diagnostic tools must respect the same persisted quarantine as discovery.
/// This is a read-only check, not a request to rescan a crashing plugin.
pub fn is_bundle_quarantined(path: &Path) -> bool {
    let path = path.to_string_lossy();
    quarantining_receipt(&load_receipts(), &path, now_unix()).is_some()
}

fn quarantining_receipt<'a>(
    receipts: &'a [PluginScanReceipt],
    path: &str,
    now_unix: u64,
) -> Option<&'a PluginScanReceipt> {
    receipts
        .iter()
        .find(|receipt| receipt.path == path && quarantines(receipt, now_unix))
}

fn rescan_requested() -> bool {
    std::env::var_os("OCP_PLUGIN_RESCAN_QUARANTINED").is_some()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Whether a receipt still keeps the bundle out of a scan or playback. A
/// receipt describes one binary, so a rebuilt bundle at the same path is not
/// covered by it.
fn quarantines(receipt: &PluginScanReceipt, now_unix: u64) -> bool {
    let status_quarantines = match receipt.status {
        ScanStatus::Crashed | ScanStatus::Failed => true,
        ScanStatus::TimedOut => {
            now_unix.saturating_sub(receipt.scanned_at_unix) < TIMEOUT_QUARANTINE_SECS
        }
        _ => false,
    };
    status_quarantines
        && receipt.runtime == ScanRuntime::for_bundle(Path::new(&receipt.path), receipt.format)
        && receipt.binary.as_ref() == Some(&bundle_identity(Path::new(&receipt.path)))
}

pub(super) fn bundle_identity(bundle: &Path) -> BundleIdentity {
    let binary = super::bundle::binary_path(bundle).unwrap_or_else(|| bundle.to_path_buf());
    let metadata = fs::metadata(&binary).ok();
    BundleIdentity {
        path: binary.to_string_lossy().into_owned(),
        modified_nanos: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as u64),
        bytes: metadata.map(|metadata| metadata.len()).unwrap_or(0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BundleMatch {
    Path,
    Name,
    Normalized,
    Identifier,
}

fn bundle_match(
    path: &str,
    actual: PluginFormat,
    query: &PluginQuery,
    requested: PluginFormat,
) -> Option<BundleMatch> {
    if actual != requested
        || !matches!(
            actual,
            PluginFormat::Clap | PluginFormat::Vst3 | PluginFormat::Vst2
        )
    {
        return None;
    }
    let path_ref = Path::new(path);
    if path == query.raw() || resolution::canonical_path_matches(path_ref, query) {
        return Some(BundleMatch::Path);
    }
    let name = path_ref.file_name().and_then(|name| name.to_str());
    let stem = path_ref.file_stem().and_then(|stem| stem.to_str());
    if name == Some(query.raw()) || stem == Some(query.raw()) {
        return Some(BundleMatch::Name);
    }
    if resolution::path_matches_query(path_ref, query) {
        return Some(BundleMatch::Normalized);
    }
    resolution::bundle_identifier_matches(path_ref, query).then_some(BundleMatch::Identifier)
}

// The bundle is identified exactly, but its descriptor may be named differently.
fn select_plugin(
    receipt: &PluginScanReceipt,
    query: &PluginQuery,
    format: PluginFormat,
    path: &str,
    allow_default_class: bool,
) -> Option<PluginInfo> {
    if receipt.status != ScanStatus::Passed {
        return None;
    }
    let candidates = receipt
        .plugins
        .iter()
        .filter(|info| info.format == format && info.path == path);
    // An exact class ID or name takes precedence over a normalized sibling.
    if let Some(info) = resolution::resolve_candidates(candidates.clone(), query) {
        return Some(info.clone());
    }
    // A path or bundle id identifies the bundle; pick its default class.
    if !allow_default_class && !resolution::path_matches_query(Path::new(path), query) {
        return None;
    }
    resolution::preferred(candidates).cloned()
}

/// Scan `files` once each and record their receipts.
///
/// A bundle whose binary still matches a passing receipt from this host version
/// reuses that receipt's plugins without a helper run, so a repeat scan only
/// pays for new or rebuilt bundles. The rest run in up to [`scan_workers`]
/// helpers at once; each helper is its own process, so a slow plugin delays
/// only its own slot.
///
/// A quarantined bundle is skipped unless `force`; a forced pass also ignores
/// passing receipts, retries a quarantined bundle once and records the new
/// receipt, so a bundle that still fails is quarantined again instead of being
/// retried in a loop.
///
/// `progress` hears the pass's size first, then each helper start and finish.
fn scan_files_isolated<R: Into<dispatch::Attempt>>(
    files: Vec<(PathBuf, PluginFormat)>,
    force: bool,
    receipts: &[PluginScanReceipt],
    receipt_file: Option<&Path>,
    scan: &(dyn Fn(&str, PluginFormat) -> R + Sync),
    progress: &(dyn Fn(ScanProgress) + Sync),
) -> (Vec<PluginInfo>, ScanReport) {
    let now = now_unix();
    let mut report = ScanReport::default();
    let mut seen = std::collections::HashSet::new();
    // One slot per bundle in discovery order, so the inventory order does not
    // depend on which helper finishes first.
    let mut slots: Vec<Vec<PluginInfo>> = Vec::new();
    let mut pending: Vec<(usize, String, PluginFormat, bool)> = Vec::new();
    for (path, format) in files {
        if !matches!(
            format,
            PluginFormat::Clap | PluginFormat::Vst3 | PluginFormat::Vst2
        ) {
            continue;
        }
        // The receipt and the inventory store a string path: a bundle under a
        // directory that is not valid UTF-8 keeps a lossy form, never an empty
        // one, so it stays scannable and visible.
        let path = display_path(&path);
        // Overlapping search directories can name one bundle twice; a rescan
        // must still run it at most once.
        if !seen.insert(path.clone()) {
            continue;
        }
        let quarantined = quarantining_receipt(receipts, &path, now).is_some();
        if quarantined && !force {
            log::warn!("Skipping quarantined plugin bundle: {path}");
            continue;
        }
        if !force && let Some(receipt) = reusable_receipt(receipts, &path, format) {
            report.reused += 1;
            slots.push(receipt.plugins.clone());
            continue;
        }
        pending.push((slots.len(), path, format, quarantined));
        slots.push(Vec::new());
    }

    let tracker = ProgressTracker {
        state: std::sync::Mutex::new(ScanProgress {
            done: report.reused,
            total: report.reused + pending.len(),
            current: None,
        }),
        sink: progress,
    };
    tracker.publish(|_| {});
    let scanned = run_pending(&pending, scan_workers(pending.len()), scan, &tracker);
    let mut fresh = Vec::with_capacity(scanned.len());
    let mut shared = 0;
    for (index, dispatch::Attempt { receipt, launched }) in scanned {
        let (slot, _, _, quarantined) = pending[index];
        if launched {
            report.scanned += 1;
            report.retried += usize::from(quarantined);
        } else {
            shared += 1;
        }
        if matches!(
            receipt.status,
            ScanStatus::Crashed | ScanStatus::Failed | ScanStatus::TimedOut
        ) {
            report.failed += 1;
        }
        slots[slot] = receipt.plugins.clone();
        if launched {
            fresh.push(receipt);
        }
    }
    if let Some(receipt_file) = receipt_file {
        record_receipts(receipt_file, &fresh);
    }
    log::info!(
        "plugin {}: {} scanned, {} reused, {shared} shared, {} quarantined retried, {} failed",
        if force { "rescan" } else { "scan" },
        report.scanned,
        report.reused,
        report.retried,
        report.failed
    );
    (slots.into_iter().flatten().collect(), report)
}

/// A passing receipt that still describes the bundle on disk.
fn reusable_receipt<'a>(
    receipts: &'a [PluginScanReceipt],
    path: &str,
    format: PluginFormat,
) -> Option<&'a PluginScanReceipt> {
    let receipt = receipts
        .iter()
        .find(|receipt| receipt.path == path && receipt.format == format)?;
    (receipt.status == ScanStatus::Passed
        && receipt.schema == RECEIPT_SCHEMA
        && receipt.host_version.as_deref() == Some(HOST_VERSION)
        && receipt.runtime == ScanRuntime::for_bundle(Path::new(path), format)
        && receipt.binary.as_ref() == Some(&bundle_identity(Path::new(path))))
    .then_some(receipt)
}

/// Helpers that run at once. Each one loads a plugin binary, which is mostly
/// disk and dynamic-linker work, so a few overlap well; more would compete
/// with the UI for the same cores and make a slow plugin time out.
const MAX_SCAN_WORKERS: usize = 4;

fn scan_workers(pending: usize) -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1)
        .min(MAX_SCAN_WORKERS)
        .min(pending)
        .max(1)
}

/// The running counts of one pass, shared by its helper threads.
struct ProgressTracker<'a> {
    state: std::sync::Mutex<ScanProgress>,
    sink: &'a (dyn Fn(ScanProgress) + Sync),
}

impl ProgressTracker<'_> {
    fn publish(&self, change: impl FnOnce(&mut ScanProgress)) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        change(&mut state);
        // Publish in the same order as the worker updates.
        (self.sink)(state.clone());
    }
}

fn bundle_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Run `scan` for every pending bundle on `workers` threads and return the
/// receipts with their slots, in slot order.
fn run_pending<R: Into<dispatch::Attempt>>(
    pending: &[(usize, String, PluginFormat, bool)],
    workers: usize,
    scan: &(dyn Fn(&str, PluginFormat) -> R + Sync),
    tracker: &ProgressTracker<'_>,
) -> Vec<(usize, dispatch::Attempt)> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(pending.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((_, path, format, _)) = pending.get(index) else {
                        break;
                    };
                    tracker.publish(|state| state.current = Some(bundle_name(path)));
                    let attempt = scan(path, *format).into();
                    tracker.publish(|state| state.done += 1);
                    done.lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push((index, attempt));
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|error| error.into_inner());
    done.sort_by_key(|(slot, _)| *slot);
    done
}

/// Resolve one bundle without repeating discovery for an unchanged binary.
fn scan_candidate(path: &str, format: PluginFormat) -> Option<PluginScanReceipt> {
    let request = dispatch::request();
    let force = rescan_requested();
    let receipts = if force { Vec::new() } else { load_receipts() };
    scan_candidate_with_receipts(path, format, &receipts, || {
        let dispatch::Attempt { receipt, launched } = request.scan(path, format, || {
            scan_bundle_isolated(Path::new(path), format, scan_timeout())
        });
        if launched {
            append_receipt(&receipt);
        }
        receipt
    })
}

fn scan_candidate_with_receipts(
    path: &str,
    format: PluginFormat,
    receipts: &[PluginScanReceipt],
    scan: impl FnOnce() -> PluginScanReceipt,
) -> Option<PluginScanReceipt> {
    if quarantining_receipt(receipts, path, now_unix()).is_some() {
        return None;
    }
    Some(
        reusable_receipt(receipts, path, format)
            .cloned()
            .unwrap_or_else(scan),
    )
}

fn resolve_bundle_from(
    query: &PluginQuery,
    format: PluginFormat,
    files: Vec<(String, PluginFormat)>,
    mut scan: impl FnMut(&str, PluginFormat) -> Option<PluginScanReceipt>,
) -> Option<PluginInfo> {
    let mut exact = Vec::new();
    let mut normalized = Vec::new();
    let mut identifiers = Vec::new();
    for (path, actual) in files {
        match bundle_match(&path, actual, query, format) {
            Some(BundleMatch::Path | BundleMatch::Name) => exact.push((path, actual)),
            Some(BundleMatch::Normalized) => normalized.push((path, actual)),
            Some(BundleMatch::Identifier) => identifiers.push((path, actual)),
            None => {}
        }
    }
    for (path, actual) in exact {
        let Some(receipt) = scan(&path, actual) else {
            continue;
        };
        if let Some(info) = select_plugin(&receipt, query, format, &path, true) {
            return Some(info);
        }
    }
    // Case/space/underscore variants resolve only when one bundle matches.
    if normalized.len() == 1 {
        let (path, actual) = &normalized[0];
        if let Some(receipt) = scan(path, *actual)
            && let Some(info) = select_plugin(&receipt, query, format, path, false)
        {
            return Some(info);
        }
    }
    if identifiers.len() == 1 {
        let (path, actual) = &identifiers[0];
        if let Some(receipt) = scan(path, *actual)
            && let Some(info) = select_plugin(&receipt, query, format, path, true)
        {
            return Some(info);
        }
    }
    None
}

/// Fast path for an explicit bundle path, basename, display name, or
/// CFBundleIdentifier. The full inventory remains the fallback for class IDs.
pub(super) fn resolve_plugin_isolated(
    query: &PluginQuery,
    format: PluginFormat,
) -> Option<PluginInfo> {
    if !matches!(
        format,
        PluginFormat::Clap | PluginFormat::Vst3 | PluginFormat::Vst2
    ) {
        return None;
    }
    if query.is_explicit_path() {
        return resolve_explicit_bundle(query, format, scan_candidate);
    }
    // Matching is against the identifiers a project stores, so discovery's
    // paths become strings here; a non-UTF-8 one keeps its lossy form.
    let mut files: Vec<(String, PluginFormat)> = discover_plugin_files()
        .into_iter()
        .filter(|(_, actual)| *actual == format)
        .map(|(path, actual)| (display_path(&path), actual))
        .collect();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);
    resolve_bundle_from(query, format, files, scan_candidate)
        .or_else(|| resolve_scanned_class(&load_receipts(), query, format))
}

fn resolve_scanned_class(
    receipts: &[PluginScanReceipt],
    query: &PluginQuery,
    format: PluginFormat,
) -> Option<PluginInfo> {
    let candidates: Vec<_> = receipts
        .iter()
        .filter(|receipt| {
            receipt.format == format
                && resolution::resolve_plugin(&receipt.plugins, query, format).is_some()
                && reusable_receipt(std::slice::from_ref(*receipt), &receipt.path, format).is_some()
        })
        .flat_map(|receipt| receipt.plugins.iter().cloned())
        .collect();
    resolution::resolve_plugin(&candidates, query, format).cloned()
}

fn resolve_explicit_bundle(
    query: &PluginQuery,
    format: PluginFormat,
    scan: impl FnOnce(&str, PluginFormat) -> Option<PluginScanReceipt>,
) -> Option<PluginInfo> {
    let path = display_path(query.direct_bundle_path(format)?);
    let receipt = scan(&path, format)?;
    select_plugin(&receipt, query, format, &path, true)
}

/// Refresh one explicitly selected bundle without visiting other quarantines.
pub fn rescan_bundle(path: &Path) -> Result<PluginScanReceipt, String> {
    if !path.is_absolute() || !path.exists() {
        return Err("Pass the absolute path of an installed plugin bundle".into());
    }
    let format = match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("vst" | "dll") => PluginFormat::Vst2,
        Some("vst3") => PluginFormat::Vst3,
        Some("clap") => PluginFormat::Clap,
        _ => return Err("Expected a VST2, VST3 or CLAP plugin bundle".into()),
    };
    let receipt = scan_bundle_isolated(path, format, scan_timeout());
    if receipt_is_cacheable(&receipt)
        && let Some(cache) = receipts_path()
    {
        merge_receipts(&cache, &[&receipt]).map_err(|error| error.to_string())?;
    }
    Ok(receipt)
}

/// Scan one bundle in a helper process with a timeout.
pub fn scan_bundle_isolated(
    path: &Path,
    format: PluginFormat,
    timeout: Duration,
) -> PluginScanReceipt {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let runtime = ScanRuntime::for_bundle(path, format);
        if matches!(
            runtime,
            ScanRuntime::IntelVst2Playback | ScanRuntime::IntelVst3Playback
        ) {
            let mut receipt = new_receipt(path, format);
            receipt.runtime = runtime;
            let Some(helper) = crate::vst2_bridge::helper_path() else {
                receipt.status = ScanStatus::HelperMissing;
                receipt.error = Some("The OpenCompose Intel plugin worker is missing".into());
                return receipt;
            };
            return scan_bundle_with_helper(receipt, &helper, path, format, timeout);
        }
    }
    scan_bundle_isolated_with_resolver(path, format, timeout, helper_path)
}

fn scan_bundle_isolated_with_resolver(
    path: &Path,
    format: PluginFormat,
    timeout: Duration,
    find_helper: impl FnOnce() -> Option<PathBuf>,
) -> PluginScanReceipt {
    let mut receipt = new_receipt(path, format);
    let Some(helper) = find_helper() else {
        receipt.status = ScanStatus::HelperMissing;
        receipt.error = Some("opencompose-plugin-scan-helper was not found".into());
        return receipt;
    };
    scan_bundle_with_helper(receipt, &helper, path, format, timeout)
}

fn new_receipt(path: &Path, format: PluginFormat) -> PluginScanReceipt {
    PluginScanReceipt {
        schema: RECEIPT_SCHEMA.into(),
        path: path.to_string_lossy().into_owned(),
        format,
        status: ScanStatus::Failed,
        scanned_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        duration_ms: 0,
        exit_code: None,
        error: None,
        binary: Some(bundle_identity(path)),
        host_version: Some(HOST_VERSION.to_string()),
        runtime: ScanRuntime::Native,
        plugins: Vec::new(),
    }
}

fn scan_bundle_with_helper(
    mut receipt: PluginScanReceipt,
    helper: &Path,
    path: &Path,
    format: PluginFormat,
    timeout: Duration,
) -> PluginScanReceipt {
    let started = Instant::now();
    let format_arg = match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Vst2 => "vst2",
        _ => {
            receipt.error = Some("isolated scanner supports only CLAP/VST2/VST3".into());
            return receipt;
        }
    };
    // A stalled plugin may be waiting on a modal dialog. Quarantine it
    // immediately so the same scan does not reopen the dialog.
    let outcome = match run_helper(helper, path, format_arg, timeout) {
        Ok(outcome) => outcome,
        Err(error) => {
            receipt.status = if matches!(error.stage, HelperIoStage::Spawn) {
                ScanStatus::HelperMissing
            } else {
                ScanStatus::Failed
            };
            receipt.exit_code = error.exit_code;
            receipt.error = Some(format!("{}: {error}", helper.display()));
            receipt.duration_ms = started.elapsed().as_millis();
            return receipt;
        }
    };
    match outcome {
        HelperOutcome::Finished {
            success,
            code,
            stdout,
            stderr,
            signal,
        } => {
            receipt.exit_code = code;
            if success {
                match serde_json::from_slice(&stdout) {
                    Ok(plugins) => {
                        receipt.plugins = plugins;
                        receipt.status = ScanStatus::Passed;
                    }
                    Err(error) => {
                        receipt.status = ScanStatus::Failed;
                        receipt.error = Some(format!(
                            "invalid scan helper JSON: {error}; stdout_bytes={}; stdout_prefix={:?}",
                            stdout.len(),
                            String::from_utf8_lossy(&stdout[..stdout.len().min(256)])
                        ));
                    }
                }
            } else {
                receipt.status = if code.is_none() {
                    ScanStatus::Crashed
                } else {
                    ScanStatus::Failed
                };
                receipt.error = Some(stderr).filter(|error| !error.is_empty());
                if receipt.error.is_none() {
                    receipt.error = Some(match signal {
                        Some(signal) => format!("helper terminated by signal {signal}"),
                        None => "helper exited without diagnostics".into(),
                    });
                }
                // A binary that rejects the helper flag is not a helper at
                // all (stale or foreign build beside the host executable).
                if receipt
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Unrecognized option"))
                {
                    receipt.status = ScanStatus::HelperMissing;
                }
            }
        }
        HelperOutcome::TimedOut { diagnostics } => {
            receipt.status = ScanStatus::TimedOut;
            let mut message = format!("scan exceeded {} ms", timeout.as_millis());
            if let Some(diagnostic) = diagnostic_line(&diagnostics) {
                message.push_str(&format!("; stderr: {diagnostic:?}"));
            }
            receipt.error = Some(message);
        }
    }
    receipt.duration_ms = started.elapsed().as_millis();
    receipt
}

enum HelperOutcome {
    Finished {
        success: bool,
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: String,
        signal: Option<i32>,
    },
    TimedOut {
        diagnostics: Vec<u8>,
    },
}

fn diagnostic_line(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(256).collect())
}

#[derive(Debug, Clone, Copy)]
enum HelperIoStage {
    Spawn,
    Poll,
    CollectOutput,
    Process(crate::child_process::FailureKind),
}

impl std::fmt::Display for HelperIoStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn => f.write_str("Spawn"),
            Self::Poll => f.write_str("Poll"),
            Self::CollectOutput => f.write_str("CollectOutput"),
            Self::Process(kind) => write!(f, "{kind:?}"),
        }
    }
}

#[derive(Debug)]
struct HelperIoError {
    stage: HelperIoStage,
    source: std::io::Error,
    exit_code: Option<i32>,
    cleanup_error: Option<std::io::Error>,
    diagnostics: Vec<u8>,
}

impl std::fmt::Display for HelperIoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scan helper {}: {} (kind={:?}, os_code={:?})",
            self.stage,
            self.source,
            self.source.kind(),
            self.source.raw_os_error()
        )?;
        if let Some(error) = &self.cleanup_error {
            write!(formatter, "; cleanup: {error}")?;
        }
        if let Some(line) = diagnostic_line(&self.diagnostics) {
            write!(formatter, "; stderr: {line:?}")?;
        }
        Ok(())
    }
}

fn collected_helper_output(
    status: std::process::ExitStatus,
    output: io::Result<(Vec<u8>, Vec<u8>)>,
) -> Result<HelperOutcome, HelperIoError> {
    let (stdout, stderr) = output.map_err(|source| HelperIoError {
        stage: HelperIoStage::CollectOutput,
        source,
        exit_code: status.code(),
        cleanup_error: None,
        diagnostics: Vec::new(),
    })?;
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    Ok(HelperOutcome::Finished {
        success: status.success(),
        code: status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        signal,
    })
}

fn run_helper(
    helper: &Path,
    path: &Path,
    format_arg: &str,
    timeout: Duration,
) -> Result<HelperOutcome, HelperIoError> {
    run_helper_query(helper, path, format_arg, None, timeout)
}

fn run_helper_query(
    helper: &Path,
    path: &Path,
    format_arg: &str,
    parameter_class: Option<&str>,
    timeout: Duration,
) -> Result<HelperOutcome, HelperIoError> {
    use crate::child_process::{self, CommandSpec, FailureKind, RunOptions, StdioMode};
    let mut command = CommandSpec::new(helper);
    command
        .arg(HELPER_MODE_ARG)
        .arg(parent_lifetime::WATCH_STDIN_ARG)
        .arg("--format")
        .arg(format_arg)
        .arg("--path")
        .arg(path);
    if let Some(class) = parameter_class {
        command.arg("--parameters-for").arg(class);
    }
    match child_process::run(
        command,
        StdioMode::Capture,
        RunOptions {
            stdout_limit: MAX_HELPER_STDOUT_BYTES,
            stderr_limit: MAX_HELPER_STDERR_BYTES,
            ..RunOptions::new(timeout)
        },
        None,
    ) {
        Ok(output) => collected_helper_output(
            output.status,
            Ok((output.captured.stdout, output.captured.stderr)),
        ),
        Err(error) if error.kind == FailureKind::Deadline && error.cleanup_error.is_none() => {
            Ok(HelperOutcome::TimedOut {
                diagnostics: error.captured.stderr,
            })
        }
        Err(error) => {
            let stage = match error.kind {
                FailureKind::Spawn => HelperIoStage::Spawn,
                FailureKind::Poll => HelperIoStage::Poll,
                FailureKind::StdoutRead | FailureKind::StderrRead => HelperIoStage::CollectOutput,
                kind => HelperIoStage::Process(kind),
            };
            Err(HelperIoError {
                stage,
                source: error.source.unwrap_or_else(|| {
                    std::io::Error::other(format!("child process {:?}", error.kind))
                }),
                exit_code: error.status.and_then(|status| status.code()),
                cleanup_error: error.cleanup_error,
                diagnostics: error.captured.stderr,
            })
        }
    }
}

fn scan_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("OCP_PLUGIN_SCAN_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
    )
}

fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OCP_PLUGIN_SCAN_HELPER").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) {
        "opencompose-plugin-scan-helper.exe"
    } else {
        "opencompose-plugin-scan-helper"
    };
    let sibling = exe.parent()?.join(name);
    if sibling.is_file() {
        return Some(sibling);
    }
    let debug_sibling = exe.parent()?.parent()?.join(name);
    if debug_sibling.is_file() {
        Some(debug_sibling)
    } else {
        // The packaged GUI executable doubles as the disposable helper. Its
        // main function intercepts HELPER_MODE_ARG before Tauri initializes.
        Some(exe)
    }
}

/// Run the scan helper from process arguments. The caller must exit the
/// dedicated one-bundle helper process after this returns.
pub fn run_helper_cli(args: &[String]) -> Result<(), String> {
    run_helper_cli_for_runtime(args, ScanRuntime::Native)
}

/// Inventory the plugin topologies accepted by the Intel render worker.
#[cfg(target_os = "macos")]
pub fn run_intel_plugin_helper_cli(args: &[String]) -> Result<(), String> {
    if !cfg!(target_arch = "x86_64") {
        return Err("Plugin bridge helper must be Intel".into());
    }
    let runtime = match args
        .windows(2)
        .find(|pair| pair[0] == "--format")
        .map(|pair| pair[1].as_str())
    {
        Some("vst2") => ScanRuntime::IntelVst2Playback,
        Some("vst3") => ScanRuntime::IntelVst3Playback,
        _ => return Err("The Intel worker scans VST2 and VST3 modules".into()),
    };
    run_helper_cli_for_runtime(args, runtime)
}

fn run_helper_cli_for_runtime(args: &[String], runtime: ScanRuntime) -> Result<(), String> {
    parent_lifetime::watch_if_requested(args)?;
    let format = args
        .windows(2)
        .find(|pair| pair[0] == "--format")
        .map(|pair| pair[1].as_str())
        .ok_or("missing --format")?;
    let path = PathBuf::from(
        args.windows(2)
            .find(|pair| pair[0] == "--path")
            .map(|pair| pair[1].as_str())
            .ok_or("missing --path")?,
    );
    if let Some(class) = args
        .windows(2)
        .find(|pair| pair[0] == "--parameters-for")
        .map(|pair| pair[1].as_str())
    {
        return parameter_inventory::emit(&path, format, class);
    }
    let mut plugins = match format {
        "vst2" => super::vst2::scan::scan_in_disposable_process(
            &path,
            runtime == ScanRuntime::IntelVst2Playback,
        )?,
        "clap" => super::clap_host::scan_clap_bundle(&path),
        "vst3" => super::vst3_instance::Vst3Instance::scan_bundle(&path)
            .into_iter()
            .filter(|class| class.category == "Audio Module Class")
            .map(|class| PluginInfo {
                id: format!("vst3:{}", class.name.replace(' ', "_").to_lowercase()),
                name: class.name,
                vendor: class.vendor,
                format: PluginFormat::Vst3,
                path: path.to_string_lossy().into_owned(),
                category: if class
                    .sub_categories
                    .to_ascii_lowercase()
                    .contains("instrument")
                {
                    super::PluginCategory::Instrument
                } else {
                    super::PluginCategory::Effect
                },
                num_params: 0,
                version: None,
                architectures: vec![],
                state_abi: None,
            })
            .collect(),
        _ => return Err(format!("unsupported format: {format}")),
    };
    let architectures = plugin_architectures(&path);
    let bundle_version = bundle_version(&path);
    let state_kind = if format == "vst2" {
        "bank-chunk-v1"
    } else {
        "component-state-v1"
    };
    for plugin in &mut plugins {
        if plugin.version.as_deref().is_none_or(str::is_empty) {
            plugin.version = bundle_version.clone();
        }
        plugin.architectures = architectures.clone();
        plugin.state_abi = Some(format!(
            "{}:{}:{}:{state_kind}",
            format,
            plugin.id,
            plugin.version.as_deref().unwrap_or("unknown")
        ));
    }
    println!(
        "{}",
        serde_json::to_string(&plugins).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn plugin_architectures(path: &Path) -> Vec<String> {
    let executable = if path.is_dir() {
        let macos = path.join("Contents/MacOS");
        fs::read_dir(macos)
            .ok()
            .and_then(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .find(|path| path.is_file())
            })
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let output = Command::new("file").arg(executable).output().ok();
    let text = output
        .as_ref()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_ascii_lowercase())
        .unwrap_or_default();
    let mut architectures = Vec::new();
    for (needle, label) in [
        ("arm64", "aarch64"),
        ("aarch64", "aarch64"),
        ("x86_64", "x86_64"),
        ("x86-64", "x86_64"),
        ("i386", "x86"),
        ("80386", "x86"),
    ] {
        if text.contains(needle) && !architectures.iter().any(|arch| arch == label) {
            architectures.push(label.to_string());
        }
    }
    if architectures.is_empty() {
        architectures.push(std::env::consts::ARCH.to_string());
    }
    architectures
}

#[cfg(target_os = "macos")]
fn bundle_version(path: &Path) -> Option<String> {
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        if let Ok(output) = Command::new("plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(path.join("Contents/Info.plist"))
            .output()
            && output.status.success()
        {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn bundle_version(_path: &Path) -> Option<String> {
    None
}

fn receipts_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OCP_PLUGIN_SCAN_RECEIPTS").map(PathBuf::from) {
        return Some(path);
    }
    // In a test binary this resolves the process scratch root, never the
    // operator's home.
    Some(oc_audio::private_fs::config_dir().join("plugin-scan-receipts.json"))
}

fn read_receipts(path: &Path) -> Vec<PluginScanReceipt> {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn load_receipts() -> Vec<PluginScanReceipt> {
    receipts_path()
        .as_deref()
        .map(read_receipts)
        .unwrap_or_default()
}

/// A missing helper says nothing about the plugin, so caching that receipt
/// would hide every plugin until the receipt file is deleted.
fn receipt_is_cacheable(receipt: &PluginScanReceipt) -> bool {
    !matches!(receipt.status, ScanStatus::HelperMissing)
}

fn append_receipt(receipt: &PluginScanReceipt) {
    if let Some(path) = receipts_path() {
        record_receipt(&path, receipt);
    }
}

/// Replace the receipt for this path and format, keeping one row per bundle.
fn record_receipt(path: &Path, receipt: &PluginScanReceipt) {
    record_receipts(path, std::slice::from_ref(receipt));
}

/// Replace the receipts for these bundles with one read and one write.
fn record_receipts(path: &Path, fresh: &[PluginScanReceipt]) {
    let fresh: Vec<&PluginScanReceipt> = fresh
        .iter()
        .filter(|receipt| receipt_is_cacheable(receipt))
        .collect();
    if fresh.is_empty() {
        return;
    }
    if let Err(error) = merge_receipts(path, &fresh) {
        log::warn!("Could not persist plugin scan receipts at {path:?}: {error}");
    }
}

fn merge_receipts(path: &Path, fresh: &[&PluginScanReceipt]) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    // Lock a stable file identity across read, merge and atomic replacement.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_added_extension("lock"))?;
    lock.lock()?;
    let mut receipts = read_receipts(path);
    let current: Vec<_> = fresh
        .iter()
        .copied()
        .filter(|receipt| {
            !receipts.iter().any(|old| {
                old.path == receipt.path
                    && old.format == receipt.format
                    && old.binary != receipt.binary
            }) || receipt.binary.as_ref() == Some(&bundle_identity(Path::new(&receipt.path)))
        })
        .collect();
    receipts.retain(|old| {
        !current
            .iter()
            .any(|receipt| old.path == receipt.path && old.format == receipt.format)
    });
    receipts.extend(current.into_iter().cloned());
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(&serde_json::to_vec_pretty(&receipts)?)?;
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCategory;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// A bundle with a real binary, plus helper scripts that record each run.
    #[cfg(unix)]
    struct QuarantineFixture {
        root: PathBuf,
        bundle: PathBuf,
    }

    #[cfg(unix)]
    impl QuarantineFixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "ocp-scan-quarantine-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let bundle = root.join("Halcyon.vst3");
            let fixture = Self { root, bundle };
            fixture.write_binary(b"first build");
            fixture
        }

        fn path(&self) -> String {
            self.bundle.to_string_lossy().into_owned()
        }

        fn write_binary(&self, bytes: &[u8]) {
            #[cfg(target_os = "macos")]
            let directory = self.bundle.join("Contents/MacOS");
            #[cfg(target_os = "linux")]
            let directory = self
                .bundle
                .join(format!("Contents/{}-linux", std::env::consts::ARCH));
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let directory = self.bundle.join("Contents");
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("Halcyon"), bytes).unwrap();
        }

        fn failed_receipt(&self) -> PluginScanReceipt {
            let mut receipt = new_receipt(&self.bundle, PluginFormat::Vst3);
            receipt.status = ScanStatus::Failed;
            receipt.error = Some("missing dependency".into());
            receipt
        }

        /// A helper that exits with `code`, recording each run in `runs`.
        fn helper(&self, name: &str, code: u8, runs: &Path) -> PathBuf {
            let helper = self.root.join(name);
            fs::write(
                &helper,
                format!("#!/bin/sh\necho ran >> '{}'\nexit {code}\n", runs.display()),
            )
            .unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            helper
        }

        /// A helper that prints one descriptor, recording each run in `runs`.
        fn successful_helper(&self, runs: &Path) -> PathBuf {
            let helper = self.root.join("helper-ok");
            let payload = self.root.join("plugins.json");
            let mut info = super::super::builtin::builtin_plugin_list().remove(0);
            info.id = "vst3:halcyon".into();
            info.name = "Halcyon".into();
            info.path = self.path();
            info.format = PluginFormat::Vst3;
            info.category = PluginCategory::Instrument;
            fs::write(&payload, serde_json::to_vec(&vec![info]).unwrap()).unwrap();
            fs::write(
                &helper,
                format!(
                    "#!/bin/sh\necho ran >> '{}'\ncat '{}'\n",
                    runs.display(),
                    payload.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            helper
        }
    }

    #[cfg(unix)]
    impl Drop for QuarantineFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn scan_with_helper(
        files: Vec<(String, PluginFormat)>,
        force: bool,
        receipt_file: &Path,
        helper: &Path,
    ) -> (Vec<PluginInfo>, ScanReport) {
        let receipts = read_receipts(receipt_file);
        scan_files_isolated(
            files
                .into_iter()
                .map(|(path, format)| (PathBuf::from(path), format))
                .collect(),
            force,
            &receipts,
            Some(receipt_file),
            &|path, format| {
                scan_bundle_with_helper(
                    new_receipt(Path::new(path), format),
                    helper,
                    Path::new(path),
                    format,
                    Duration::from_secs(5),
                )
            },
            &|_| {},
        )
    }

    #[cfg(unix)]
    #[test]
    fn an_explicit_rescan_retries_a_failed_bundle_once() {
        let fixture = QuarantineFixture::new("rescan");
        let path = fixture.path();
        let files = vec![(path.clone(), PluginFormat::Vst3)];
        let receipt_file = fixture.root.join("receipts.json");
        let runs_ok = fixture.root.join("runs-ok.txt");
        let runs_failed = fixture.root.join("runs-failed.txt");
        record_receipt(&receipt_file, &fixture.failed_receipt());

        let helper = fixture.successful_helper(&runs_ok);
        let (plugins, report) = scan_with_helper(files.clone(), false, &receipt_file, &helper);
        assert!(
            plugins.is_empty(),
            "an implicit scan must honour the receipt"
        );
        assert_eq!(report.retried, 0);
        assert!(
            !runs_ok.exists(),
            "a skipped bundle must not run the helper"
        );

        let (plugins, report) = scan_with_helper(files.clone(), true, &receipt_file, &helper);
        assert_eq!(
            plugins.len(),
            1,
            "report={report:?}; receipts={:?}; runs_ok={:?}",
            read_receipts(&receipt_file),
            fs::read_to_string(&runs_ok).map(|runs| runs.lines().count())
        );
        assert_eq!(report.retried, 1);
        assert_eq!(report.failed, 0);
        let receipts = read_receipts(&receipt_file);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].status, ScanStatus::Passed);

        // The dependency is still missing: one retry, one new receipt, no loop.
        record_receipt(&receipt_file, &fixture.failed_receipt());
        let failing = fixture.helper("helper-fail", 1, &runs_failed);
        let (plugins, report) = scan_with_helper(files, true, &receipt_file, &failing);
        assert!(plugins.is_empty());
        assert_eq!(report.retried, 1);
        assert_eq!(report.failed, 1);
        let receipts = read_receipts(&receipt_file);
        assert_eq!(
            receipts.len(),
            1,
            "a repeat failure must replace its receipt"
        );
        assert_eq!(receipts[0].status, ScanStatus::Failed);
        assert_eq!(
            fs::read_to_string(&runs_failed).unwrap().lines().count(),
            1,
            "one explicit rescan retries a failing bundle once"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_rebuilt_bundle_is_retried_without_an_explicit_rescan() {
        let fixture = QuarantineFixture::new("rebuilt");
        let path = fixture.path();
        let files = vec![(path.clone(), PluginFormat::Vst3)];
        let receipt_file = fixture.root.join("receipts.json");
        let runs = fixture.root.join("runs.txt");
        record_receipt(&receipt_file, &fixture.failed_receipt());

        // Same path, new binary: the receipt no longer describes this bundle.
        fixture.write_binary(b"a longer replacement build");

        let helper = fixture.successful_helper(&runs);
        let (plugins, report) = scan_with_helper(files, false, &receipt_file, &helper);
        assert_eq!(plugins.len(), 1);
        assert_eq!(report.retried, 0, "an invalid receipt is not a force retry");
        let receipts = read_receipts(&receipt_file);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].status, ScanStatus::Passed);
    }

    #[cfg(unix)]
    #[test]
    fn an_unchanged_bundle_reuses_its_passing_receipt_until_a_forced_rescan() {
        let fixture = QuarantineFixture::new("reuse");
        let files = vec![(fixture.path(), PluginFormat::Vst3)];
        let receipt_file = fixture.root.join("receipts.json");
        let runs = fixture.root.join("runs.txt");
        let helper = fixture.successful_helper(&runs);
        let run_count = || fs::read_to_string(&runs).map_or(0, |text| text.lines().count());

        let (first, report) = scan_with_helper(files.clone(), false, &receipt_file, &helper);
        assert_eq!((report.scanned, report.reused, run_count()), (1, 0, 1));

        let (second, report) = scan_with_helper(files.clone(), false, &receipt_file, &helper);
        assert_eq!(
            (report.scanned, report.reused, run_count()),
            (0, 1, 1),
            "an unchanged bundle must not start a helper"
        );
        assert_eq!(
            second.iter().map(|info| &info.id).collect::<Vec<_>>(),
            first.iter().map(|info| &info.id).collect::<Vec<_>>()
        );

        fixture.write_binary(b"a rebuilt binary of another size");
        let (_, report) = scan_with_helper(files.clone(), false, &receipt_file, &helper);
        assert_eq!(
            (report.scanned, run_count()),
            (1, 2),
            "a rebuilt bundle is scanned"
        );

        let (_, report) = scan_with_helper(files, true, &receipt_file, &helper);
        assert_eq!((report.scanned, report.reused, run_count()), (1, 0, 3));
    }

    #[test]
    fn candidate_lookup_reuses_a_receipt_and_scans_a_replaced_binary() {
        let fixture = QuarantineFixture::new("candidate-reuse");
        let path = fixture.path();
        let format = PluginFormat::Vst3;
        let receipt = passed_receipt(&path, format, Vec::new());
        let cached =
            scan_candidate_with_receipts(&path, format, std::slice::from_ref(&receipt), || {
                panic!("lookup must not scan an unchanged bundle")
            })
            .unwrap();
        assert_eq!(cached.binary, receipt.binary);

        assert!(
            scan_candidate_with_receipts(&path, format, &[fixture.failed_receipt()], || {
                panic!("lookup must not scan a quarantined bundle")
            })
            .is_none()
        );

        fixture.write_binary(b"a different size invalidates the old receipt");
        let fresh =
            scan_candidate_with_receipts(&path, format, std::slice::from_ref(&receipt), || {
                passed_receipt(&path, format, Vec::new())
            })
            .unwrap();
        assert_ne!(fresh.binary, receipt.binary);

        let forced =
            scan_candidate_with_receipts(&path, format, &[], || fixture.failed_receipt()).unwrap();
        assert_eq!(forced.status, ScanStatus::Failed);
    }

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn native_failure_does_not_quarantine_the_translated_vst2_runtime() {
        let fixture = QuarantineFixture::new("translated-vst2");
        fixture.write_binary(&[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1]);
        let mut receipt = fixture.failed_receipt();
        receipt.format = PluginFormat::Vst2;
        assert!(!quarantines(&receipt, now_unix()));
        receipt.runtime = ScanRuntime::IntelVst2Playback;
        assert!(quarantines(&receipt, now_unix()));
        receipt.status = ScanStatus::Passed;
        let path = fixture.path();
        assert!(
            reusable_receipt(std::slice::from_ref(&receipt), &path, PluginFormat::Vst2).is_some()
        );
        receipt.runtime = ScanRuntime::Native;
        assert!(reusable_receipt(&[receipt], &path, PluginFormat::Vst2).is_none());
    }

    #[test]
    fn a_receipt_from_another_host_version_is_not_reused() {
        let root = oc_audio::private_fs::test_root()
            .join(format!("ocp-scan-version-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("Old.clap");
        fs::write(&bundle, b"binary").unwrap();
        let path = bundle.to_string_lossy().into_owned();
        let mut receipt = passed_receipt(&path, PluginFormat::Clap, Vec::new());
        assert!(
            reusable_receipt(std::slice::from_ref(&receipt), &path, PluginFormat::Clap).is_some()
        );
        receipt.host_version = Some("0.0.0-older".into());
        assert!(
            reusable_receipt(std::slice::from_ref(&receipt), &path, PluginFormat::Clap).is_none()
        );
        receipt.host_version = None;
        assert!(
            reusable_receipt(std::slice::from_ref(&receipt), &path, PluginFormat::Clap).is_none()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parallel_helpers_keep_discovery_order_and_record_every_receipt() {
        let root = oc_audio::private_fs::test_root()
            .join(format!("ocp-scan-parallel-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let receipt_file = root.join("receipts.json");
        let files: Vec<(PathBuf, PluginFormat)> = (0..9)
            .map(|index| (root.join(format!("p{index}.clap")), PluginFormat::Clap))
            .collect();
        let active = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        let heard = std::sync::Mutex::new(Vec::<ScanProgress>::new());
        let (plugins, report) = scan_files_isolated(
            files.clone(),
            false,
            &[],
            Some(&receipt_file),
            &|path, format| {
                use std::sync::atomic::Ordering;
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Later bundles finish first, so ordering cannot come from timing.
                let index: u64 = Path::new(path)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.strip_prefix('p'))
                    .and_then(|index| index.parse().ok())
                    .unwrap();
                std::thread::sleep(Duration::from_millis(30 - 3 * index));
                active.fetch_sub(1, Ordering::SeqCst);
                passed_receipt(path, format, vec![descriptor(path, path, path)])
            },
            &|progress| heard.lock().unwrap().push(progress),
        );
        assert_eq!(report.scanned, 9);
        let heard = heard.into_inner().unwrap();
        assert_eq!(
            heard.first().map(|first| (first.done, first.total)),
            Some((0, 9))
        );
        assert!(
            heard
                .iter()
                .all(|progress| progress.total == 9 && progress.done <= 9)
        );
        assert!(
            heard.windows(2).all(|pair| pair[0].done <= pair[1].done),
            "done never moves backwards"
        );
        assert_eq!(heard.last().map(|last| last.done), Some(9));
        assert!(
            heard
                .iter()
                .any(|progress| progress.current.as_deref() == Some("p0.clap"))
        );
        let expected: Vec<String> = files
            .iter()
            .map(|(path, _)| path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            plugins
                .iter()
                .map(|info| info.id.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= MAX_SCAN_WORKERS);
        assert_eq!(read_receipts(&receipt_file).len(), 9);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn named_bundle_selection_is_exact_and_format_bound() {
        let path = "/plugins/sfizz.vst3";
        let query = |identifier: &str| PluginQuery::parse(identifier).unwrap();
        assert_eq!(
            bundle_match(
                path,
                PluginFormat::Vst3,
                &query("sfizz"),
                PluginFormat::Vst3
            ),
            Some(BundleMatch::Name)
        );
        assert_eq!(
            bundle_match(
                path,
                PluginFormat::Vst3,
                &query("sfizz.vst3"),
                PluginFormat::Vst3
            ),
            Some(BundleMatch::Name)
        );
        assert_eq!(
            bundle_match(
                path,
                PluginFormat::Vst3,
                &query("SFIZZ"),
                PluginFormat::Vst3
            ),
            Some(BundleMatch::Normalized)
        );
        assert_eq!(
            bundle_match(path, PluginFormat::Vst3, &query(path), PluginFormat::Vst3),
            Some(BundleMatch::Path)
        );
        for id in ["sfiz", "fizz", "class-guid", "/other/sfizz.vst3"] {
            assert_eq!(
                bundle_match(path, PluginFormat::Vst3, &query(id), PluginFormat::Vst3),
                None,
                "{id}"
            );
        }
        assert_eq!(
            bundle_match(
                path,
                PluginFormat::Vst3,
                &query("sfizz"),
                PluginFormat::Clap
            ),
            None
        );
        assert_eq!(
            bundle_match(
                "/plugins/sfizz.component",
                PluginFormat::Au,
                &query("sfizz"),
                PluginFormat::Au
            ),
            None
        );
    }

    fn descriptor(path: &str, id: &str, name: &str) -> PluginInfo {
        let mut info = super::super::builtin::builtin_plugin_list().remove(0);
        info.id = id.into();
        info.name = name.into();
        info.path = path.into();
        info.format = PluginFormat::Vst3;
        info
    }

    #[test]
    fn isolated_class_selection_matches_published_inventory_precedence() {
        let path = "/plugins/Halcyon.vst3";
        let instrument = descriptor(path, "vst3:halcyon_fx", "Halcyon FX");
        let mut effect = descriptor(path, "vst3:Halcyon_FX", "Halcyon Fx");
        effect.category = PluginCategory::Effect;
        let query = PluginQuery::parse(&effect.id).unwrap();
        let receipt = passed_receipt(path, PluginFormat::Vst3, vec![instrument, effect.clone()]);
        let selected = select_plugin(&receipt, &query, PluginFormat::Vst3, path, false).unwrap();
        assert_eq!(selected.id, effect.id);
        assert_eq!(selected.category, PluginCategory::Effect);
    }

    #[cfg(unix)]
    #[test]
    fn passed_receipts_resolve_an_audio_class_without_scanning_unrelated_bundles() {
        let fixture = QuarantineFixture::new("class-id");
        fixture.write_binary(b"current version");
        let path = fixture.path();
        let mut receipt = new_receipt(&fixture.bundle, PluginFormat::Vst3);
        receipt.status = ScanStatus::Passed;
        receipt.plugins = vec![
            descriptor(&path, "vst3:halcyon", "Halcyon"),
            descriptor(&path, "vst3:halcyon_fx", "Halcyon FX"),
        ];
        let query = PluginQuery::parse("vst3:halcyon_fx").unwrap();
        assert_eq!(
            resolve_scanned_class(std::slice::from_ref(&receipt), &query, PluginFormat::Vst3)
                .unwrap()
                .name,
            "Halcyon FX"
        );
        fixture.write_binary(b"new version with a different size");
        assert!(resolve_scanned_class(&[receipt], &query, PluginFormat::Vst3).is_none());
    }

    fn passed_receipt(
        path: &str,
        format: PluginFormat,
        plugins: Vec<PluginInfo>,
    ) -> PluginScanReceipt {
        let mut receipt = new_receipt(Path::new(path), format);
        receipt.status = ScanStatus::Passed;
        receipt.plugins = plugins;
        receipt
    }

    #[test]
    fn explicit_paths_probe_only_the_requested_compatible_bundle() {
        let root = oc_test_support::temp_dir("plugin-path", "isolated-resolution");
        let bundle = root.join("Example.vst3");
        fs::create_dir_all(&bundle).unwrap();
        let path = bundle.to_string_lossy();
        let query = PluginQuery::parse(&path).unwrap();
        assert!(query.is_explicit_path());
        assert!(
            resolve_explicit_bundle(&query, PluginFormat::Vst2, |_, _| {
                panic!("a differently formatted path must not probe any bundle")
            })
            .is_none()
        );
        let missing = root.join("Missing.vst3");
        let missing = PluginQuery::parse(missing.to_str().unwrap()).unwrap();
        assert!(
            resolve_explicit_bundle(&missing, PluginFormat::Vst3, |_, _| {
                panic!("a missing explicit path must not probe other bundles")
            })
            .is_none()
        );
        for passed in [false, true] {
            let mut calls = 0;
            let result = resolve_explicit_bundle(&query, PluginFormat::Vst3, |actual, format| {
                calls += 1;
                assert_eq!(Path::new(actual), bundle);
                assert_eq!(format, PluginFormat::Vst3);
                passed.then(|| {
                    passed_receipt(
                        actual,
                        format,
                        vec![descriptor(actual, "example", "Example")],
                    )
                })
            });
            assert_eq!(calls, 1);
            assert_eq!(result.is_some(), passed);
        }
    }

    #[test]
    fn named_lookup_skips_unrelated_bundles_and_requires_passed_descriptor() {
        let files = vec![
            ("/plugins/other.vst3".into(), PluginFormat::Vst3),
            ("/plugins/sfizz.clap".into(), PluginFormat::Clap),
            ("/plugins/sfizz.vst3".into(), PluginFormat::Vst3),
        ];
        let info = descriptor("/plugins/sfizz.vst3", "class-id", "sfizz");
        for status in [
            ScanStatus::Passed,
            ScanStatus::Failed,
            ScanStatus::Crashed,
            ScanStatus::TimedOut,
            ScanStatus::HelperMissing,
        ] {
            let mut calls = 0;
            let result = resolve_bundle_from(
                &PluginQuery::parse("sfizz").unwrap(),
                PluginFormat::Vst3,
                files.clone(),
                |path, format| {
                    calls += 1;
                    assert_eq!(path, "/plugins/sfizz.vst3");
                    assert_eq!(format, PluginFormat::Vst3);
                    let mut receipt = new_receipt(Path::new(path), format);
                    receipt.status = status.clone();
                    receipt.plugins = vec![info.clone()];
                    Some(receipt)
                },
            );
            assert_eq!(calls, 1);
            assert_eq!(result.is_some(), status == ScanStatus::Passed);
        }
        // Quarantined candidates yield no receipt and must never resolve.
        assert!(
            resolve_bundle_from(
                &PluginQuery::parse("sfizz").unwrap(),
                PluginFormat::Vst3,
                files.clone(),
                |_, _| None
            )
            .is_none()
        );
        // A class ID cannot be inferred from an unrelated filename.
        assert!(
            resolve_bundle_from(
                &PluginQuery::parse("class-id").unwrap(),
                PluginFormat::Vst3,
                files,
                |_, _| panic!(
                    "unknown class ID must defer to inventory, not scan arbitrary bundles"
                )
            )
            .is_none()
        );
    }

    #[test]
    fn display_name_and_identifier_variants_select_one_bundle() {
        let files = vec![("/plugins/Halcyon2.vst3".into(), PluginFormat::Vst3)];
        let info = descriptor("/plugins/Halcyon2.vst3", "vst3:halcyon_2", "Halcyon 2");
        for identifier in ["Halcyon 2", "halcyon_2", "vst3:halcyon_2", "Halcyon2"] {
            let query = PluginQuery::parse(identifier).unwrap();
            let found =
                resolve_bundle_from(&query, PluginFormat::Vst3, files.clone(), |path, format| {
                    Some(passed_receipt(path, format, vec![info.clone()]))
                });
            assert_eq!(
                found.map(|plugin| plugin.id),
                Some("vst3:halcyon_2".into()),
                "{identifier}"
            );
        }
    }

    #[test]
    fn ambiguous_normalized_names_do_not_select_a_bundle() {
        let files = vec![
            ("/plugins/a/Halcyon2.vst3".into(), PluginFormat::Vst3),
            ("/plugins/b/Halcyon2.vst3".into(), PluginFormat::Vst3),
        ];
        assert!(
            resolve_bundle_from(
                &PluginQuery::parse("halcyon_2").unwrap(),
                PluginFormat::Vst3,
                files,
                |_, _| panic!("ambiguous names must not scan an arbitrary bundle")
            )
            .is_none()
        );
    }

    #[test]
    fn bundle_identifier_selects_the_matching_bundle_from_its_info_plist() {
        let root = std::env::temp_dir().join(format!(
            "ocp-scan-bundle-id-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = root.join("Halcyon2.vst3");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.acme.halcyon2.VST3</string>
</dict>
</plist>
"#,
        )
        .unwrap();
        let path = bundle.to_string_lossy().into_owned();
        let info = descriptor(&path, "vst3:halcyon_2", "Halcyon 2");
        let found = resolve_bundle_from(
            &PluginQuery::parse("com.acme.halcyon2.VST3").unwrap(),
            PluginFormat::Vst3,
            vec![(path.clone(), PluginFormat::Vst3)],
            |path, format| Some(passed_receipt(path, format, vec![info.clone()])),
        );
        assert_eq!(found.map(|plugin| plugin.id), Some("vst3:halcyon_2".into()));
        assert!(
            resolve_bundle_from(
                &PluginQuery::parse("com.acme.halcyon").unwrap(),
                PluginFormat::Vst3,
                vec![(path, PluginFormat::Vst3)],
                |_, _| panic!("near-miss bundle ids must not scan")
            )
            .is_none()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn two_class_bundle_prefers_the_instrument_class() {
        let root = std::env::temp_dir().join(format!(
            "ocp-scan-two-class-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bundle = root.join("Halcyon2.vst3");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.acme.halcyon2.VST3</string>
</dict>
</plist>
"#,
        )
        .unwrap();
        let path = bundle.to_string_lossy().into_owned();
        let instrument = descriptor(&path, "vst3:halcyon_2", "Halcyon 2");
        let mut effect = descriptor(&path, "vst3:halcyon_2_fx", "Halcyon 2 FX");
        effect.category = PluginCategory::Effect;
        // Effect first: default order must not decide the result.
        let plugins = vec![effect, instrument];
        for identifier in [path.as_str(), "com.acme.halcyon2.VST3", "Halcyon2"] {
            let query = PluginQuery::parse(identifier).unwrap();
            let found = resolve_bundle_from(
                &query,
                PluginFormat::Vst3,
                vec![(path.clone(), PluginFormat::Vst3)],
                |path, format| Some(passed_receipt(path, format, plugins.clone())),
            );
            assert_eq!(
                found.map(|plugin| plugin.id),
                Some("vst3:halcyon_2".into()),
                "{identifier}"
            );
        }
        // A name matching only the effect class selects that class.
        let receipt = passed_receipt(&path, PluginFormat::Vst3, plugins.clone());
        let query = PluginQuery::parse("Halcyon 2 FX").unwrap();
        assert_eq!(
            select_plugin(&receipt, &query, PluginFormat::Vst3, &path, true)
                .map(|plugin| plugin.id),
            Some("vst3:halcyon_2_fx".into())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_helper_fails_closed_without_loading_bundle() {
        let receipt = scan_bundle_isolated_with_resolver(
            Path::new("/tmp/untrusted.clap"),
            PluginFormat::Clap,
            Duration::from_millis(10),
            || None,
        );
        assert_eq!(receipt.status, ScanStatus::HelperMissing);
        assert!(receipt.plugins.is_empty());
    }

    #[test]
    fn missing_executable_preserves_spawn_error_without_quarantining() {
        let helper = oc_audio::private_fs::test_config_dir()
            .join("missing-scanner-helper")
            .join("not-an-executable");
        let path = Path::new("missing-fixture.vst3");
        let error = match run_helper(&helper, path, "vst3", Duration::from_secs(5)) {
            Err(error) => error,
            Ok(_) => panic!("missing executable unexpectedly started"),
        };
        assert!(matches!(error.stage, HelperIoStage::Spawn));
        assert_eq!(error.source.kind(), std::io::ErrorKind::NotFound);
        let raw = error.source.raw_os_error();
        assert!(raw.is_some());
        let receipt = scan_bundle_with_helper(
            new_receipt(path, PluginFormat::Vst3),
            &helper,
            path,
            PluginFormat::Vst3,
            Duration::from_secs(5),
        );
        assert_eq!(receipt.status, ScanStatus::HelperMissing);
        assert!(receipt.plugins.is_empty());
        assert!(!receipt_is_cacheable(&receipt));
        let diagnostic = receipt.error.unwrap();
        assert!(diagnostic.contains("Spawn"));
        assert!(diagnostic.contains("NotFound"));
        assert!(diagnostic.contains(&format!("os_code={raw:?}")));
    }

    #[cfg(unix)]
    #[test]
    fn output_collection_error_does_not_become_a_successful_empty_scan() {
        use std::os::unix::process::ExitStatusExt;
        let error = match collected_helper_output(
            std::process::ExitStatus::from_raw(0),
            Err(std::io::Error::from_raw_os_error(libc::EIO)),
        ) {
            Err(error) => error,
            Ok(_) => panic!("failed output collection unexpectedly succeeded"),
        };
        assert!(matches!(error.stage, HelperIoStage::CollectOutput));
        assert_eq!(error.source.raw_os_error(), Some(libc::EIO));
        assert_eq!(error.exit_code, Some(0));
        assert!(error.to_string().contains("CollectOutput"));
    }

    #[cfg(unix)]
    #[test]
    fn successful_helper_requires_json_but_accepts_an_empty_inventory() {
        let fixture = QuarantineFixture::new("protocol");
        let runs = fixture.root.join("runs.txt");
        let helper = fixture.successful_helper(&runs);
        let payload = fixture.root.join("plugins.json");
        let receipt_file = fixture.root.join("receipts.json");
        for bytes in [b"not JSON".as_slice(), b"[{".as_slice(), b"".as_slice()] {
            fs::write(&payload, bytes).unwrap();
            let (plugins, report) = scan_with_helper(
                vec![(fixture.path(), PluginFormat::Vst3)],
                true,
                &receipt_file,
                &helper,
            );
            assert!(plugins.is_empty());
            assert_eq!(report.scanned, 1);
            assert_eq!(report.failed, 1);
            let receipts = read_receipts(&receipt_file);
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].status, ScanStatus::Failed);
            assert_eq!(receipts[0].exit_code, Some(0));
            let diagnostic = receipts[0].error.as_deref().unwrap();
            assert!(diagnostic.contains("invalid scan helper JSON"));
            assert!(diagnostic.contains(&format!("stdout_bytes={}", bytes.len())));
        }
        fs::write(&payload, b"[]").unwrap();
        let (plugins, report) = scan_with_helper(
            vec![(fixture.path(), PluginFormat::Vst3)],
            true,
            &receipt_file,
            &helper,
        );
        assert!(plugins.is_empty());
        assert_eq!(report.failed, 0);
        let receipts = read_receipts(&receipt_file);
        assert_eq!(receipts[0].status, ScanStatus::Passed);
        assert!(receipts[0].error.is_none());
        assert_eq!(fs::read_to_string(runs).unwrap().lines().count(), 4);
    }

    #[test]
    fn missing_helper_receipts_are_not_cached() {
        let mut receipt = new_receipt(Path::new("/plugins/x.vst3"), PluginFormat::Vst3);
        receipt.status = ScanStatus::HelperMissing;
        receipt.error = Some("opencompose-plugin-scan-helper was not found".into());
        assert!(!receipt_is_cacheable(&receipt));
        receipt.status = ScanStatus::Failed;
        assert!(receipt_is_cacheable(&receipt));
        receipt.status = ScanStatus::Passed;
        assert!(receipt_is_cacheable(&receipt));
    }

    #[cfg(unix)]
    #[test]
    fn helper_timeout_is_killed_and_quarantinable() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!("ocp-scan-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let helper = directory.join("slow-helper");
        fs::write(&helper, "#!/bin/sh\nsleep 2\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let base = new_receipt(Path::new("/tmp/untrusted.clap"), PluginFormat::Clap);
        let receipt = scan_bundle_with_helper(
            base,
            &helper,
            Path::new("/tmp/untrusted.clap"),
            PluginFormat::Clap,
            Duration::from_millis(50),
        );

        assert_eq!(receipt.status, ScanStatus::TimedOut);
        assert!(receipt.duration_ms < 1_000);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_stdout_cannot_stall_the_scan() {
        use std::os::unix::fs::PermissionsExt;

        let directory = oc_test_support::temp_dir("scanner", "descendant");
        let helper = directory.join("holder");
        fs::write(&helper, "#!/bin/sh\nsleep 30 &\nprintf '[]'\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let outcome = run_helper(
            &helper,
            Path::new("/tmp/holder.vst3"),
            "vst3",
            Duration::from_millis(250),
        )
        .unwrap();
        match outcome {
            HelperOutcome::Finished {
                success,
                stdout,
                stderr,
                code,
                ..
            } => {
                assert!(success, "helper failed: {code:?}; {stderr}");
                assert_eq!(stdout, b"[]");
            }
            // The short deadline may expire before the shell exits on a busy
            // host. Both outcomes must retire the child without waiting on EOF.
            HelperOutcome::TimedOut { .. } => {}
        }
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_helper_stdout_is_refused_without_allocating_without_bound() {
        use std::os::unix::fs::PermissionsExt;

        let directory = oc_test_support::temp_dir("scanner", "oversized");
        let helper = directory.join("verbose");
        fs::write(
            &helper,
            "#!/bin/sh\nhead -c 9000000 /dev/zero | tr '\\0' a\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let error = match run_helper(
            &helper,
            Path::new("/tmp/verbose.vst3"),
            "vst3",
            Duration::from_secs(5),
        ) {
            Err(error) => error,
            Ok(_) => panic!("oversized helper output unexpectedly succeeded"),
        };
        assert!(matches!(
            error.stage,
            HelperIoStage::Process(crate::child_process::FailureKind::OutputLimit)
        ));
        assert!(error.to_string().contains("exceeds"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn stalled_helper_is_quarantined_after_one_launch() {
        use std::os::unix::fs::PermissionsExt;

        let directory = oc_test_support::temp_dir("scanner", "single-launch");
        let helper = directory.join("stalled-helper");
        fs::write(
            &helper,
            "#!/bin/sh\nprintf 'launch\\n' >> \"$0.runs\"\nsleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let bundle = directory.join("stalled.clap");
        let receipt = scan_bundle_with_helper(
            new_receipt(&bundle, PluginFormat::Clap),
            &helper,
            &bundle,
            PluginFormat::Clap,
            Duration::from_secs(5),
        );

        assert_eq!(receipt.status, ScanStatus::TimedOut);
        assert!(quarantines(&receipt, now_unix()));
        assert_eq!(
            fs::read_to_string(directory.join("stalled-helper.runs")).unwrap(),
            "launch\n"
        );
    }

    #[test]
    fn a_timeout_stops_quarantining_after_a_day() {
        let mut receipt = new_receipt(Path::new("/plugins/x.vst3"), PluginFormat::Vst3);
        receipt.status = ScanStatus::TimedOut;
        receipt.scanned_at_unix = 1_000_000;
        assert!(quarantines(&receipt, 1_000_000 + 60));
        assert!(quarantines(
            &receipt,
            1_000_000 + TIMEOUT_QUARANTINE_SECS - 1
        ));
        assert!(!quarantines(&receipt, 1_000_000 + TIMEOUT_QUARANTINE_SECS));
    }

    #[test]
    fn crashes_and_failures_stay_quarantined() {
        let mut receipt = new_receipt(Path::new("/plugins/x.vst3"), PluginFormat::Vst3);
        receipt.scanned_at_unix = 0;
        for status in [ScanStatus::Crashed, ScanStatus::Failed] {
            receipt.status = status.clone();
            assert!(quarantines(&receipt, u32::MAX as u64), "{status:?}");
        }
        receipt.status = ScanStatus::Passed;
        assert!(!quarantines(&receipt, u32::MAX as u64));
    }

    #[cfg(unix)]
    #[test]
    fn helper_nonzero_exit_is_recorded_without_host_failure() {
        let base = new_receipt(Path::new("/tmp/untrusted.clap"), PluginFormat::Clap);
        let receipt = scan_bundle_with_helper(
            base,
            Path::new("/usr/bin/false"),
            Path::new("/tmp/untrusted.clap"),
            PluginFormat::Clap,
            Duration::from_secs(5),
        );

        assert_eq!(receipt.status, ScanStatus::Failed);
        assert_eq!(receipt.exit_code, Some(1));
        assert!(receipt.error.is_some());
    }

    /// A temporary directory holding one bundle directory.
    fn bundle_root(tag: &str, bundle: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "ocp-scan-{tag}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let path = base.join(bundle);
        fs::create_dir_all(&path).expect("create the bundle");
        (base, path)
    }

    /// A bundle path whose bytes are not valid UTF-8.
    ///
    /// APFS refuses to create such a name, so the path is built in memory; a
    /// mounted volume can still hand one to the scanner.
    #[cfg(unix)]
    fn non_utf8_bundle_path() -> PathBuf {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(
            b"/plugins/vend\xffor/Legacy.vst3".to_vec(),
        ))
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_path_is_rendered_lossily() {
        let path = non_utf8_bundle_path();
        assert!(path.to_str().is_none(), "the fixture must not be UTF-8");
        let display = super::super::display_path(&path);
        assert!(!display.is_empty(), "a path never becomes an empty id");
        assert!(display.ends_with("Legacy.vst3"), "{display}");
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_bundle_reaches_the_scanner_receipt_with_a_path() {
        let path = non_utf8_bundle_path();
        let (plugins, report) = scan_files_isolated(
            vec![(path.clone(), PluginFormat::Vst3)],
            false,
            &[],
            None,
            &|path, format| {
                let mut receipt = new_receipt(Path::new(path), format);
                receipt.status = ScanStatus::Passed;
                let mut info = super::super::builtin::builtin_plugin_list().remove(0);
                info.id = path.to_string();
                info.path = path.to_string();
                info.format = format;
                receipt.plugins.push(info);
                receipt
            },
            &|_| {},
        );

        assert_eq!(report.scanned, 1, "the bundle must reach the scanner");
        assert_eq!(plugins.len(), 1);
        assert!(!plugins[0].id.is_empty(), "an inventory id is never empty");
        assert!(
            !plugins[0].path.is_empty(),
            "an inventory path is never empty"
        );
        let lossy = path.to_string_lossy().into_owned();
        assert_eq!(plugins[0].path, lossy, "the receipt keeps the lossy path");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_au_pass_lists_a_component_with_a_path() {
        let (base, bundle) = bundle_root("au", "Legacy.component");
        let mut listed = Vec::new();
        crate::au_instance::collect_au_bundles(&base, &mut listed);
        let _ = fs::remove_dir_all(&base);

        assert_eq!(listed.len(), 1);
        assert!(!listed[0].id.is_empty(), "an AU id is never empty");
        assert!(!listed[0].path.is_empty(), "an AU path is never empty");
        assert_eq!(listed[0].id, listed[0].path);
        assert_eq!(Path::new(&listed[0].path), bundle.as_path());
        assert_eq!(listed[0].name, "Legacy");
        assert_eq!(listed[0].format, PluginFormat::Au);
        assert_eq!(listed[0].category, crate::PluginCategory::Other);
    }

    /// Only Linux can name a directory with bytes that are not valid UTF-8;
    /// APFS and NTFS reject the name, so the walk is unchanged there.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_bundle_under_a_non_utf8_directory_is_discovered() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let base = std::env::temp_dir().join(format!(
            "ocp-scan-nonutf8-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let root = base.join(OsString::from_vec(b"vend\xffor".to_vec()));
        let bundle = root.join("Legacy.vst3");
        fs::create_dir_all(&bundle).expect("create the non-UTF-8 directory");

        let mut found = Vec::new();
        super::super::collect_plugin_files(&root, super::super::MAX_BUNDLE_DEPTH, &mut found);
        let _ = fs::remove_dir_all(&base);

        assert_eq!(found.len(), 1, "found {found:?}");
        assert_eq!(found[0].0, bundle, "discovery must keep the real path");
        assert_eq!(found[0].1, PluginFormat::Vst3);
        assert!(bundle.to_str().is_none());
        assert!(!super::super::display_path(&found[0].0).is_empty());
    }
}
