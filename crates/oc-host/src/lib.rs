//! Plugin bridge — scanner and host for VST3/CLAP/AU plugins.
//!
//! ```text
//! scan_all_plugins()
//!   ├── scan CLAP dirs → load entry → enumerate descriptors
//!   ├── scan VST3 dirs → find bundles (isolated helper scan)
//!   └── scan AU dirs  → find .component (macOS only)
//!
//! Instantiation:
//!   load_clap_plugin(info) → ClapInstance → AudioEffect
//!   Vst3Instance::load(path) → explicit selected VST3 bundle only
//!   AuInstance::load(query) → AudioUnit instrument or effect (macOS)
//! ```
//!
//! Global plugin inventory — published once, replaced when a scan finishes.

use std::sync::{Mutex, OnceLock};

/// The published inventory. Readers keep a `'static` slice, so a replacement
/// publishes a new snapshot and leaves the previous one allocated: the
/// inventory is replaced only on a warm load, a background refresh, or an
/// explicit scan.
static PLUGIN_INVENTORY: OnceLock<Mutex<Option<&'static [PluginInfo]>>> = OnceLock::new();
/// Serialises the first scan: readers that arrive while it runs wait for it.
static INVENTORY_SCAN: Mutex<()> = Mutex::new(());
static RESOLVED_PLUGIN_CACHE: OnceLock<resolution_cache::ResolutionCache> = OnceLock::new();

/// Drop memoized lookups so a rescan can pick up installed or removed bundles.
pub fn clear_resolved_plugin_cache() {
    if let Some(cache) = RESOLVED_PLUGIN_CACHE.get() {
        cache.clear();
    }
}

fn inventory_slot() -> &'static Mutex<Option<&'static [PluginInfo]>> {
    PLUGIN_INVENTORY.get_or_init(|| Mutex::new(None))
}

/// Publish `plugins` as this process's inventory and return the stable slice
/// readers hold. Memoized lookups are dropped, so a lookup resolves against the
/// new list.
pub fn install_plugin_inventory(plugins: Vec<PluginInfo>) -> &'static [PluginInfo] {
    let snapshot: &'static [PluginInfo] = Box::leak(plugins.into_boxed_slice());
    *inventory_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
    clear_resolved_plugin_cache();
    snapshot
}

/// The published inventory, without waiting for a first scan.
pub fn published_plugins() -> Option<&'static [PluginInfo]> {
    *inventory_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// Get all plugins: the published inventory, or the first scan when nothing has
/// been published yet.
pub fn cached_plugins() -> &'static [PluginInfo] {
    if let Some(snapshot) = *inventory_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
    {
        return snapshot;
    }
    // Only one thread scans; readers that arrive during it wait for its result.
    let _scan = INVENTORY_SCAN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(snapshot) = *inventory_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
    {
        return snapshot;
    }
    install_plugin_inventory(scan_all_plugins())
}

/// Run the one-time inventory scan now; callers use this off the UI thread so a
/// transport press never pays for it.
pub fn warm_plugin_inventory() {
    let _ = cached_plugins();
}

pub mod application_main;
#[cfg(target_os = "macos")]
pub mod au_instance;
/// Built-in test plugins.
pub mod builtin;
pub mod bundle;
pub mod child_process;
mod clap_failure;
mod clap_guard;
pub mod clap_host;
pub mod clap_instance;
pub mod clap_main;
mod clap_module;
pub mod conformance;
pub mod editor_host;
pub mod editor_monitor;
mod ffi_guard;
pub mod host_app;
pub mod host_message;
pub mod known_folders;
/// LV2 plugin support.
pub mod lv2;
mod native_callback;
pub mod native_fault;
pub mod param_edit;
pub mod params;
pub mod plugin_folders;
pub mod plugin_preset;
pub mod plugin_state;
pub mod plugin_state_io;
pub mod ports;
pub mod preset_parameters;
pub mod programs;
mod resolution;
mod resolution_cache;
pub mod scanner;
/// VST2 plugin support.
pub mod vst2;
#[cfg(target_os = "macos")]
pub mod vst2_bridge;
mod vst3_guard;
pub mod vst3_instance;
#[cfg(target_os = "linux")]
pub mod x11;

/// Whether tests may instantiate third-party plugins.
///
/// Off by default: hosting a commercial plugin opens its own data folders (macOS
/// prompts for file access), can take seconds, and can crash the test process.
/// Set `OCP_TEST_PLUGINS=1` to run those tests.
pub fn external_plugin_tests_enabled() -> bool {
    std::env::var("OCP_TEST_PLUGINS").is_ok_and(|value| !value.is_empty() && value != "0")
}

/// Tests use built-ins or explicitly installed fixture inventories unless the
/// user opts into scanning and loading installed plug-ins.
fn discover_external_plugins() -> bool {
    !oc_audio::private_fs::running_under_test() || external_plugin_tests_enabled()
}

/// Set once this process has loaded a third-party plugin module.
static PLUGIN_MODULE_LOADED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record that a plugin module (VST3, VST2, CLAP, or LV2 binary) is loaded.
///
/// Called by the format loaders after `dlopen` succeeds. The flag exists for
/// [`exit_process`]: a hosted module owns process-global state and C++
/// destructors the host cannot reason about.
pub(crate) fn mark_plugin_module_loaded() {
    PLUGIN_MODULE_LOADED.store(true, std::sync::atomic::Ordering::Release);
}

/// Install an exit guard that keeps a hosted module's destructors from running.
///
/// [`exit_process`] is the explicit path, but a framework can call
/// `std::process::exit` itself after its event loop returns — Tauri does — and
/// that reaches `__cxa_finalize_ranges`, which runs the module's C++ static
/// destructors. Several modules die there: a Qt module tears down the
/// `NSApplication` it created, on whatever thread called `exit`.
///
/// `atexit` handlers share one list with `__cxa_atexit` destructors and run in
/// reverse registration order, so this guard takes effect only when it is
/// registered after the module built its state. Call it from the entry point
/// that owns the process (the GUI, the out-of-process editor host), and call it
/// again once a plugin is instantiated; an early registration is harmless but
/// runs behind the destructors it is meant to pre-empt.
///
/// The handler leaves through `_exit(0)` only when [`plugin_module_loaded`] is
/// true; otherwise it returns and the process shuts down normally. It forces
/// status 0, so callers that need their own status use [`exit_process`] or call
/// it before any plugin module is loaded.
///
/// A test executable's exit status is the test result, and `atexit` cannot see
/// it, so in a test process the handler returns and the status survives. A test
/// that exercises the guard itself starts its child with
/// [`EXIT_GUARD_UNDER_TEST_ENV`] set.
pub fn guard_plugin_teardown() {
    #[cfg(unix)]
    // SAFETY: `exit_guard` only flushes and calls `_exit`.
    unsafe {
        libc::atexit(exit_guard);
    }
}

#[cfg(unix)]
extern "C" fn exit_guard() {
    use std::io::Write;

    if !plugin_module_loaded() {
        return;
    }
    if oc_audio::private_fs::running_under_test() {
        if std::env::var_os(EXIT_GUARD_UNDER_TEST_ENV).is_none() {
            return;
        }
        oc_audio::private_fs::cleanup_test_root();
    }
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    log::logger().flush();
    // SAFETY: `_exit` is async-signal-safe, and skipping the module's static
    // destructors is the point of this handler.
    unsafe { libc::_exit(0) }
}

/// Arms [`guard_plugin_teardown`] in a test process, whose status it would
/// otherwise mask. Only a child that exits with status 0 on purpose sets it.
pub const EXIT_GUARD_UNDER_TEST_ENV: &str = "OCP_TEST_EXIT_GUARD";

/// Whether a third-party plugin module is loaded in this process.
pub fn plugin_module_loaded() -> bool {
    PLUGIN_MODULE_LOADED.load(std::sync::atomic::Ordering::Acquire)
}

/// End the process with `code`, skipping a hosted plugin module's teardown.
///
/// A hosted module registers C++ destructors through `__cxa_atexit`, and
/// `exit()` runs them from `__cxa_finalize_ranges`. Those destructors tear down
/// process-global UI state the module created — state that belongs to the
/// thread that instantiated the module, not necessarily the thread that runs
/// the exit. A host cannot repair a third-party destructor, so once a module is
/// loaded the process leaves through `_exit` after flushing: the module's
/// teardown must not run.
///
/// With no module loaded this is an ordinary exit, so plugin-free runs keep
/// their usual shutdown (including an AudioUnit, which the system loads and
/// whose teardown is safe).
///
/// Call it as the last statement of `main`, after every `Drop` that matters.
pub fn exit_process(code: i32) -> ! {
    use std::io::Write;

    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    log::logger().flush();
    if plugin_module_loaded() {
        #[cfg(unix)]
        // SAFETY: `_exit` is async-signal-safe and is the point of this call:
        // it skips the module's static destructors that `exit` would run.
        unsafe {
            libc::_exit(code)
        }
        #[cfg(not(unix))]
        std::process::exit(code)
    }
    std::process::exit(code)
}

// Types shared with the audio graph and the plugin instances.
pub use oc_audio::{PluginFormat, PluginTransport};
pub use plugin_state::{
    PluginStateContext, decode_plugin_state, encode_plugin_state, encode_plugin_state_versioned,
    supported_plugin_state_version,
};

use oc_dsl::ast::{ParamValue, Project};
use serde::{Deserialize, Serialize};

/// Metadata about a discovered plugin.
///
/// Channel layout is deliberately absent: only a loaded instance knows its
/// declared buses, and discovery does not query them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub format: PluginFormat,
    pub path: String,
    pub category: PluginCategory,
    /// Declared automatable parameter count; 0 when not reported.
    #[serde(default)]
    pub num_params: u32,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub architectures: Vec<String>,
    #[serde(default)]
    pub state_abi: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// How a discovered plugin is classified.
pub enum PluginCategory {
    Instrument,
    Effect,
    Analyzer,
    Other,
}

/// Every folder searched for plugins: the platform's standard locations, then
/// the folders the user added.
pub fn plugin_search_paths() -> Vec<String> {
    plugin_search_folders()
        .into_iter()
        .map(|folder| folder.path)
        .collect()
}

/// Where a searched plugin folder comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderOrigin {
    /// A platform convention every host searches.
    Standard,
    /// A conventional location outside the formats' standard lists.
    Convention,
    /// Named by an environment variable such as `CLAP_PATH`.
    Environment,
    /// Configured in another audio host's settings.
    HostSettings,
    /// Added by the user through [`plugin_folders`].
    User,
}

/// One searched plugin folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFolder {
    pub path: String,
    pub origin: FolderOrigin,
}

/// [`plugin_search_paths`] with each folder's origin. A folder reached more
/// than one way is listed once, under its first origin: standard, then the
/// extra locations of [`known_folders`], then the user's own.
pub fn plugin_search_folders() -> Vec<PluginFolder> {
    let mut folders: Vec<PluginFolder> = standard_plugin_search_paths()
        .into_iter()
        .map(|path| PluginFolder {
            path,
            origin: FolderOrigin::Standard,
        })
        .collect();
    let known = known_folders::plugin_folders().into_iter().map(|folder| {
        let origin = match folder.source {
            known_folders::KnownSource::Convention => FolderOrigin::Convention,
            known_folders::KnownSource::Environment => FolderOrigin::Environment,
            known_folders::KnownSource::HostSettings => FolderOrigin::HostSettings,
        };
        (folder.path, origin)
    });
    let user = plugin_folders::folders()
        .into_iter()
        .map(|path| (path, FolderOrigin::User));
    for (path, origin) in known.chain(user) {
        let path = display_path(&path);
        if !folders.iter().any(|listed| listed.path == path) {
            folders.push(PluginFolder { path, origin });
        }
    }
    folders
}

/// The platform's standard plugin folders.
pub fn standard_plugin_search_paths() -> Vec<String> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            paths.push(format!("{}/Library/Audio/Plug-Ins/VST3", home));
            paths.push("/Library/Audio/Plug-Ins/VST3".into());
            paths.push(format!("{}/Library/Audio/Plug-Ins/CLAP", home));
            paths.push("/Library/Audio/Plug-Ins/CLAP".into());
            paths.push(format!("{}/Library/Audio/Plug-Ins/Components", home));
            paths.push("/Library/Audio/Plug-Ins/Components".into());
            paths.push(format!("{}/Library/Audio/Plug-Ins/LV2", home));
            paths.push("/Library/Audio/Plug-Ins/LV2".into());
        }
    }

    #[cfg(target_os = "windows")]
    {
        // `%COMMONPROGRAMFILES%` is `C:\Program Files\Common Files` on a default
        // install, but it follows the system drive and the program-files
        // directory, so read it instead of assuming the default layout.
        let common = std::env::var("COMMONPROGRAMFILES")
            .unwrap_or_else(|_| "C:\\Program Files\\Common Files".to_string());
        paths.push(format!("{common}\\VST3"));
        paths.push(format!("{common}\\CLAP"));
        // The LV2 loader searches these as well, so they are counted here.
        paths.push(format!("{common}\\LV2"));
        if let Some(home) = std::env::var_os("LOCALAPPDATA") {
            let home = home.to_string_lossy();
            paths.push(format!("{}\\Programs\\Common\\VST3", home));
            paths.push(format!("{}\\Programs\\Common\\CLAP", home));
            paths.push(format!("{}\\LV2", home));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            paths.push(format!("{}/.vst3", home));
            paths.push(format!("{}/.clap", home));
            paths.push(format!("{}/.lv2", home));
        }
        paths.push("/usr/lib/vst3".into());
        paths.push("/usr/lib/clap".into());
        paths.push("/usr/local/lib/vst3".into());
        paths.push("/usr/local/lib/clap".into());
        paths.push("/usr/lib/lv2".into());
        paths.push("/usr/local/lib/lv2".into());
    }

    paths.extend(
        vst2::scan::search_paths()
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );

    paths
}

/// Scan all plugin directories for all formats.
///
/// Returns a combined list of discovered plugins. Quarantine receipts are
/// honoured, so opening a project never pays for a known-bad bundle.
pub fn scan_all_plugins() -> Vec<PluginInfo> {
    scan_all_plugins_with(false).0
}

/// Scan every bundle again, including the ones a receipt quarantined.
///
/// This is the explicit rescan action: a bundle that failed before a missing
/// dependency was installed gets one more chance, and one that still fails is
/// quarantined again after the same bounded helper run.
pub fn rescan_all_plugins() -> (Vec<PluginInfo>, scanner::ScanReport) {
    #[cfg(target_os = "macos")]
    vst2_bridge::allow_setup_retry();
    scan_all_plugins_with(true)
}

fn scan_all_plugins_with(force: bool) -> (Vec<PluginInfo>, scanner::ScanReport) {
    let mut all = Vec::new();

    // Built-in plugins (always available)
    all.extend(builtin::builtin_plugin_list());
    log::info!("Registered {} built-in plugins", all.len());
    if !discover_external_plugins() {
        return (all, scanner::ScanReport::default());
    }

    // Every third-party bundle is inspected in a disposable helper process.
    // A plugin crash, hang, or Objective-C collision cannot poison this host.
    let (isolated, report) = scanner::scan_discovered_plugins_isolated(force);
    log::info!("Found {} isolated third-party plugins", isolated.len());
    all.extend(isolated);

    // LV2 metadata lives in manifest.ttl, so the inventory scan is filesystem
    // only; the bundle binary is loaded when a project actually uses a plugin.
    let (lv2_plugins, lv2_errors) = lv2::scan_installed();
    for error in lv2_errors {
        log::warn!("{error}");
    }
    log::info!("Found {} LV2 plugins", lv2_plugins.len());
    all.extend(lv2_plugins.iter().map(lv2::plugin_info));

    // AudioUnit discovery only reads the bundle declarations. Loading arbitrary
    // components during a scan could execute plugin UI/runtime code.
    #[cfg(target_os = "macos")]
    for dir in plugin_search_paths() {
        au_instance::collect_au_bundles(std::path::Path::new(&dir), &mut all);
    }

    (all, report)
}

/// A bundle path as the string a receipt or an inventory entry stores.
///
/// A path that is not valid UTF-8 keeps its lossy form and is logged; dropping
/// it would leave the plugin missing from the inventory with no diagnostic.
pub(crate) fn display_path(path: &std::path::Path) -> String {
    match path.to_str() {
        Some(text) => text.to_string(),
        None => {
            let text = path.to_string_lossy().into_owned();
            log::warn!("Plugin bundle path is not valid UTF-8: {text}");
            text
        }
    }
}

/// Scan for plugin files without loading (fast, filesystem only).
///
/// The paths stay [`std::path::PathBuf`] until an inventory entry or a scan
/// receipt is serialized, so a bundle under a non-UTF-8 directory is still
/// listed.
pub fn discover_plugin_files() -> Vec<(std::path::PathBuf, PluginFormat)> {
    let mut results = Vec::new();
    for dir in plugin_search_paths() {
        collect_plugin_files(std::path::Path::new(&dir), MAX_BUNDLE_DEPTH, &mut results);
    }
    results
}

/// [`discover_plugin_files`] for one folder, with the same depth bound.
pub fn discover_plugin_files_in(dir: &std::path::Path) -> Vec<(std::path::PathBuf, PluginFormat)> {
    let mut results = Vec::new();
    collect_plugin_files(dir, MAX_BUNDLE_DEPTH, &mut results);
    results
}

/// How many directory levels below a search directory may hold a bundle.
///
/// A vendor folder above the bundle is the normal layout on Windows
/// (`<VST3>\<vendor>\<plugin>.vst3`), so the scan cannot stop at the first
/// level. A recognized bundle is never descended into, so this bound only
/// limits how deep an unrecognized folder tree is searched.
const MAX_BUNDLE_DEPTH: usize = 3;

fn collect_plugin_files(
    dir: &std::path::Path,
    depth: usize,
    results: &mut Vec<(std::path::PathBuf, PluginFormat)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let format = match ext {
            "vst3" => Some(PluginFormat::Vst3),
            "clap" => Some(PluginFormat::Clap),
            "component" => Some(PluginFormat::Au),
            "lv2" => Some(PluginFormat::Lv2),
            _ => vst2::scan::is_plugin_path(&path).then_some(PluginFormat::Vst2),
        };
        if let Some(format) = format {
            results.push((path, format));
            continue;
        }
        if depth > 0 && path.is_dir() {
            collect_plugin_files(&path, depth - 1, results);
        }
    }
}

/// Canonical plugin identity persisted in a state envelope.
///
/// This is the scanner id (`PluginInfo.id`), which is stable across sessions and
/// resolution forms; a plugin this session has not looked up falls back to the
/// declared identifier.
pub fn plugin_state_identity(identifier: &str, format: PluginFormat) -> String {
    let raw = identifier.trim();
    find_plugin_info(raw, format)
        .map(|info| info.id)
        .unwrap_or_else(|| raw.to_string())
}

/// Split an insert reference into an optional explicit format and its selector.
pub fn effect_plugin_selector(reference: &str) -> (Option<PluginFormat>, &str) {
    if let Some(selector) = reference
        .strip_prefix("au:")
        .or_else(|| reference.strip_prefix("au."))
    {
        return (Some(PluginFormat::Au), selector);
    }
    if (reference.contains('/') || reference.contains('\\'))
        && std::path::Path::new(reference)
            .extension()
            .is_some_and(|extension| extension == "component")
    {
        return (Some(PluginFormat::Au), reference);
    }
    let Some((prefix, selector)) = reference.split_once('.') else {
        return (None, reference);
    };
    let format = match prefix {
        "clap" => PluginFormat::Clap,
        "vst2" => PluginFormat::Vst2,
        "vst3" => PluginFormat::Vst3,
        "lv2" => PluginFormat::Lv2,
        _ => return (None, reference),
    };
    (Some(format), selector)
}

/// Resolve an insert selector using its explicit format or the legacy inventory.
pub fn find_effect_plugin(format: Option<PluginFormat>, selector: &str) -> Option<PluginInfo> {
    if let Some(format) = format {
        return find_plugin_info(selector, format);
    }
    cached_plugins()
        .iter()
        .find(|plugin| {
            matches!(
                plugin.format,
                PluginFormat::Clap | PluginFormat::Vst2 | PluginFormat::Vst3 | PluginFormat::Lv2
            ) && (plugin.id == selector || plugin.name == selector || plugin.path == selector)
        })
        .cloned()
}

/// Resolve a plugin id, display name, path, basename, or bundle identifier.
///
/// Exact identifiers win; normalized and bundle-identifier forms resolve only
/// when they name a single plugin.
pub fn find_plugin_info(plugin_id: &str, format: PluginFormat) -> Option<PluginInfo> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(fixture) = test_fixture_plugin_info(plugin_id, format) {
        return Some(fixture);
    }
    RESOLVED_PLUGIN_CACHE
        .get_or_init(Default::default)
        .resolve(plugin_id, format, || resolve_plugin_info(plugin_id, format))
}

#[cfg(any(test, feature = "test-support"))]
fn test_fixture_plugin_info(plugin_id: &str, format: PluginFormat) -> Option<PluginInfo> {
    if format != PluginFormat::Vst2 {
        return None;
    }
    let (id, name, category) = match plugin_id.trim().to_ascii_lowercase().as_str() {
        "test.fixture.vst2.tone" => (
            "test.fixture.vst2.tone",
            "OpenCompose Tone Fixture",
            PluginCategory::Instrument,
        ),
        "test.fixture.vst2.multi" => (
            "test.fixture.vst2.multi",
            "OpenCompose Multi Out Fixture",
            PluginCategory::Instrument,
        ),
        "test.fixture.vst2.gain" => (
            "test.fixture.vst2.gain",
            "OpenCompose Gain Fixture",
            PluginCategory::Effect,
        ),
        _ => return None,
    };
    Some(PluginInfo {
        id: id.into(),
        name: name.into(),
        vendor: "OpenCompose Tests".into(),
        format,
        path: vst2::test_support::fixture_path(name)?
            .to_string_lossy()
            .into_owned(),
        category,
        num_params: 0,
        version: None,
        architectures: Vec::new(),
        state_abi: None,
    })
}

fn resolve_plugin_info(plugin_id: &str, format: PluginFormat) -> Option<PluginInfo> {
    if !discover_external_plugins() {
        let query = resolution::PluginQuery::parse(plugin_id)?;
        return resolution::resolve_plugin(cached_plugins(), &query, format)
            .cloned()
            .or_else(|| {
                if format == PluginFormat::Lv2 {
                    lv2::find_registered_plugin(query.raw()).map(|plugin| plugin.info())
                } else {
                    None
                }
            });
    }
    if format == PluginFormat::Lv2 {
        return lv2::resolve_plugin(plugin_id).map(|plugin| plugin.info());
    }
    let query = resolution::PluginQuery::parse(plugin_id)?;
    // The browser inventory is a cache, not a hard boundary: a plugin installed
    // after the last scan must still resolve through the isolated scanner.
    if let Some(plugins) = *inventory_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        && let Some(found) = resolution::resolve_plugin(plugins, &query, format)
    {
        return Some(found.clone());
    }
    if format == PluginFormat::Internal {
        let builtins = builtin::builtin_plugin_list();
        return resolution::resolve_plugin(&builtins, &query, format).cloned();
    }
    // Opening a known bundle must not scan every unrelated installed plugin on
    // the playback startup path. Still use the crash-isolated/quarantined scanner.
    if let Some(info) = scanner::resolve_plugin_isolated(&query, format) {
        return Some(info);
    }
    // A refused, missing or differently formatted explicit path must not turn
    // into a scan of unrelated installed modules.
    if query.is_explicit_path() {
        return None;
    }
    // Class IDs unrelated to the bundle filename need the full inventory.
    resolution::resolve_plugin(cached_plugins(), &query, format).cloned()
}

fn normalize_preset_param_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether a preset declares plugin state for this format.
pub fn preset_has_declared_plugin_state(
    params: &[(String, ParamValue)],
    format: PluginFormat,
) -> bool {
    let format_prefix = match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Au => "au",
        PluginFormat::Lv2 => "lv2",
        PluginFormat::Vst2 => "vst2",
        PluginFormat::Internal => return false,
    };
    params.iter().any(|(name, value)| {
        let name = normalize_preset_param_name(name);
        let supported = name == "statefile"
            || name == "statehex"
            || name == "pluginstatefile"
            || name == "pluginstatehex"
            || name == format!("{format_prefix}statefile")
            || name == format!("{format_prefix}statehex");
        supported && matches!(value, ParamValue::String(value) if !value.trim().is_empty())
    })
}

/// Visit every preset referenced by a track, inline pattern or pattern.
fn for_each_referenced_preset<'a>(
    project: &'a Project,
    mut visit: impl FnMut(&'a oc_dsl::ast::PresetDef, &str),
) {
    let mut check_preset = |preset_name: Option<&str>, owner: &str| {
        let Some(preset_name) = preset_name else {
            return;
        };
        let Some(preset) = project.presets.iter().find(|p| p.name == preset_name) else {
            return;
        };
        visit(preset, owner);
    };

    for track in &project.tracks {
        check_preset(track.preset.as_deref(), &format!("track '{}'", track.name));
        for pattern in &track.inline_patterns {
            check_preset(
                pattern.preset.as_deref(),
                &format!("inline pattern on track '{}'", track.name),
            );
        }
    }

    for pattern in &project.patterns {
        check_preset(
            pattern.preset.as_deref(),
            &format!("pattern '{}'", pattern.name),
        );
    }
}

/// Return warnings for referenced plugin presets that must stop the render.
pub fn plugin_preset_render_warnings(project: &Project) -> Vec<String> {
    let mut warnings = Vec::new();
    for_each_referenced_preset(project, |preset, owner| {
        let Some(source) = preset.source.as_deref() else {
            return;
        };
        if source == "sample"
            && !preset
                .plugin
                .as_deref()
                .is_some_and(|plugin| plugin.eq_ignore_ascii_case("sfizz"))
        {
            warnings.push(format!(
                "Sample preset '{}' on {} is not renderable yet; source: sample currently requires plugin: sfizz, or use an explicit CLAP/VST2/VST3 sampler preset with captured plugin state",
                preset.name, owner
            ));
        } else if source == "clap" {
            let Some(plugin_id) = preset.plugin.as_deref() else {
                warnings.push(format!(
                    "CLAP preset '{}' on {} has no plugin id",
                    preset.name, owner
                ));
                return;
            };
            if find_plugin_info(plugin_id, PluginFormat::Clap).is_none() {
                warnings.push(format!(
                    "CLAP preset '{}' on {} was not found as a renderable CLAP plugin",
                    preset.name, owner
                ));
            }
        } else if source == "lv2" {
            let Some(plugin_id) = preset.plugin.as_deref() else {
                warnings.push(format!(
                    "LV2 preset '{}' on {} has no plugin id",
                    preset.name, owner
                ));
                return;
            };
            if find_plugin_info(plugin_id, PluginFormat::Lv2).is_none() {
                warnings.push(format!(
                    "LV2 preset '{}' on {} was not found as a renderable LV2 plugin",
                    preset.name, owner
                ));
            }
        } else if source == "vst2" {
            let Some(plugin_id) = preset.plugin.as_deref() else {
                warnings.push(format!(
                    "VST2 preset '{}' on {} has no plugin id",
                    preset.name, owner
                ));
                return;
            };
            if find_plugin_info(plugin_id, PluginFormat::Vst2).is_none() {
                warnings.push(format!(
                    "VST2 preset '{}' on {} was not found as a renderable VST2 plugin",
                    preset.name, owner
                ));
            }
        } else if source == "vst3" {
            let Some(plugin_id) = preset.plugin.as_deref() else {
                warnings.push(format!(
                    "VST3 preset '{}' on {} has no plugin id",
                    preset.name, owner
                ));
                return;
            };
            // In-process fake components are never installed as bundles.
            #[cfg(any(test, feature = "test-support"))]
            if vst3_instance::test_support::fake_plugin_bus_count(plugin_id).is_some() {
                return;
            }
            if find_plugin_info(plugin_id, PluginFormat::Vst3).is_none() {
                warnings.push(format!(
                    "VST3 preset '{}' on {} was not found as a renderable VST3 plugin",
                    preset.name, owner
                ));
            } else if plugin_id.to_ascii_lowercase().contains("sitala")
                && !preset_has_declared_plugin_state(&preset.params, PluginFormat::Vst3)
            {
                warnings.push(format!(
                    "Sitala VST3 preset '{}' on {} has no captured kit state; declare a non-empty vst3_state_file or vst3_state_hex so the sampler cannot silently render an empty kit",
                    preset.name, owner
                ));
            }
        }
    });
    warnings
}

/// Return warnings for referenced AudioUnit presets that will render silence.
///
/// A missing or unhostable unit keeps the scan-only path: the rest of the
/// project still renders, and the caller reports why this track is silent.
pub fn plugin_preset_render_soft_warnings(project: &Project) -> Vec<String> {
    let mut warnings = Vec::new();
    for_each_referenced_preset(project, |preset, owner| {
        if preset.source.as_deref() != Some("au") {
            return;
        }
        let Some(plugin) = preset.plugin.as_deref() else {
            warnings.push(format!(
                "AU preset '{}' on {} has no plugin id; its tracks render silence",
                preset.name, owner
            ));
            return;
        };
        #[cfg(target_os = "macos")]
        {
            if let Err(error) = au_instance::resolve_component(plugin) {
                warnings.push(format!(
                    "AU preset '{}' on {} cannot render in process: {}. Tracks that use it render silence",
                    preset.name, owner, error
                ));
            }
        }
        #[cfg(not(target_os = "macos"))]
        warnings.push(format!(
            "AU preset '{}' on {} names AudioUnit '{}', which needs macOS; its tracks render silence",
            preset.name, owner, plugin
        ));
    });
    warnings
}

/// The first render warning a plugin preset would produce, if any.
pub fn plugin_preset_render_error(project: &Project) -> Option<String> {
    plugin_preset_render_warnings(project).into_iter().next()
}

#[cfg(test)]
pub(crate) fn mapped_transport_fixture() -> PluginTransport {
    let project = oc_dsl::parse("@project\n  tempo: 120\n  time_signature: 7/8\n  tempo_map:\n    1:5 = 60 bpm\n  time_sig_map:\n    3:1 = 3/4\n").unwrap();
    PluginTransport::new(0, 48000, 120.0).with_clock(std::sync::Arc::new(
        oc_dsl::timing::ProjectClock::new(&project.properties).unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    /// A failing test binary exits with 101 through `std::process::exit`; the
    /// teardown guard must not turn that into a success.
    #[cfg(unix)]
    #[test]
    fn teardown_guard_keeps_a_test_process_exit_status() {
        const EXIT_STATUS_CHILD_ENV: &str = "OCP_EXIT_GUARD_STATUS_CHILD";
        if std::env::var_os(EXIT_STATUS_CHILD_ENV).is_some() {
            super::mark_plugin_module_loaded();
            super::guard_plugin_teardown();
            std::process::exit(101);
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::teardown_guard_keeps_a_test_process_exit_status",
                "--test-threads=1",
            ])
            .env(EXIT_STATUS_CHILD_ENV, "1")
            .env_remove(super::EXIT_GUARD_UNDER_TEST_ENV)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn the exit-status child");
        assert_eq!(status.code(), Some(101));
    }

    #[test]
    fn immutable_fixture_lookup_resolves_vst2_without_external_discovery() {
        for (id, category) in [
            ("test.fixture.vst2.tone", super::PluginCategory::Instrument),
            ("test.fixture.vst2.gain", super::PluginCategory::Effect),
        ] {
            let info = super::find_plugin_info(id, super::PluginFormat::Vst2).unwrap();
            assert_eq!(info.id, id);
            assert_eq!(info.category, category);
            assert!(std::path::Path::new(&info.path).exists());
            assert!(super::test_fixture_plugin_info(id, super::PluginFormat::Vst3).is_none());
        }
        assert!(
            super::test_fixture_plugin_info("unknown.fixture", super::PluginFormat::Vst2).is_none()
        );
    }

    use oc_dsl::ast::{InstrumentPath, MixState, PresetDef, Project, ProjectProperties, Track};

    #[test]
    fn mapped_transport_splits_boundaries_and_seeks_with_musical_bar_origin() {
        let root = super::mapped_transport_fixture();
        let before = root.at_sample(47993);
        assert_eq!(before.block_frames(128), 7);
        assert_eq!(before.current_tempo(), 120.0);
        let change = root.at_sample(48000);
        assert_eq!(change.current_tempo(), 60.0);
        assert_eq!(change.quarter_notes(), 2.0);
        assert_eq!(change.block_frames(128), 128);
        let meter = root.at_sample(288000); // quarter 7, two 7/8 bars elapsed
        assert_eq!(meter.quarter_notes(), 7.0);
        assert_eq!(meter.musical_position(), (3, 4, 7.0, 2));
        assert_eq!(root.at_sample(287999).block_frames(128), 1);
        assert_eq!(root.at_sample(456000).musical_position(), (3, 4, 10.0, 3));
        assert_eq!(root.at_sample(24000).quarter_notes(), 1.0); // backwards seek
        assert_eq!(root.sample_pos, 0); // snapshots do not mutate the shared origin
    }

    #[test]
    fn fractional_change_frame_matches_note_quantization() {
        let project =
            oc_dsl::parse("@project\n  tempo: 137\n  tempo_map:\n    1:2 = 60 bpm\n").unwrap();
        let clock =
            std::sync::Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
        let frame = clock.sample_at_quarter(1.0, 44100).unwrap();
        let root = super::PluginTransport::new(0, 44100, 137.0).with_clock(clock);
        assert_eq!(root.at_sample(frame - 1).block_frames(64), 1);
        assert_eq!(root.at_sample(frame).current_tempo(), 60.0);
        assert_eq!(root.at_sample(frame).quarter_notes(), 1.0);
        assert!(root.at_sample(frame + 1).quarter_notes() > 1.0);
    }

    #[test]
    fn plugin_discovery_registers_builtins_and_requires_opt_in_for_external_scans() {
        let (plugins, report) = super::rescan_all_plugins();
        assert!(plugins.len() >= 5, "Expected at least 5 builtin plugins");
        if !super::external_plugin_tests_enabled() {
            assert!(
                plugins
                    .iter()
                    .all(|plugin| plugin.format == super::PluginFormat::Internal)
            );
            assert_eq!(report.scanned, 0);
            assert_eq!(report.retried, 0);
        }
    }

    #[test]
    fn plugin_preset_warnings_only_include_referenced_presets() {
        let mut project = Project {
            name: Some("Test".to_string()),
            ocp_version: Some("0.2.0".to_string()),
            properties: ProjectProperties::default(),
            tracks: vec![],
            patterns: vec![],
            presets: vec![
                PresetDef {
                    name: "Unused".to_string(),
                    source: Some("vst3".to_string()),
                    plugin: Some("vst3:unused".to_string()),
                    program: None,
                    patch: None,
                    file: None,
                    root_note: None,
                    params: vec![],
                    effects: vec![],
                    extends: None,
                },
                PresetDef {
                    name: "Used".to_string(),
                    source: Some("clap".to_string()),
                    plugin: Some("clap.used".to_string()),
                    program: None,
                    patch: None,
                    file: None,
                    root_note: None,
                    params: vec![],
                    effects: vec![],
                    extends: None,
                },
            ],
            buses: vec![],
            master: None,
            sections: vec![],
            markers: Vec::new(),
            groups: Vec::new(),
        };
        project.tracks.push(Track {
            name: "Lead".to_string(),
            instrument: InstrumentPath::new("synth", "lead"),
            preset: Some("Used".to_string()),
            engine: None,
            mix: MixState::default(),
            effects: vec![],
            phrases: vec![],
            patterns: vec![],
            inline_patterns: vec![],
            clips: oc_dsl::ast::ClipPool::new(),
            gain_automation: vec![],
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        });

        let warnings = super::plugin_preset_render_warnings(&project);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("CLAP preset 'Used'"));
        assert!(warnings[0].contains("track 'Lead'"));
        assert!(super::plugin_preset_render_soft_warnings(&project).is_empty());
    }

    fn project_with_au_preset(plugin: &str) -> Project {
        let mut project = Project {
            name: Some("AU".to_string()),
            ocp_version: Some("0.2.0".to_string()),
            properties: ProjectProperties::default(),
            tracks: vec![],
            patterns: vec![],
            presets: vec![PresetDef {
                name: "Keys".to_string(),
                source: Some("au".to_string()),
                plugin: Some(plugin.to_string()),
                program: None,
                patch: None,
                file: None,
                root_note: None,
                params: vec![],
                effects: vec![],
                extends: None,
            }],
            buses: vec![],
            master: None,
            sections: vec![],
            markers: Vec::new(),
            groups: Vec::new(),
        };
        project.tracks.push(Track {
            name: "Keys".to_string(),
            instrument: InstrumentPath::new("keys", "piano"),
            preset: Some("Keys".to_string()),
            engine: None,
            mix: MixState::default(),
            effects: vec![],
            phrases: vec![],
            patterns: vec![],
            inline_patterns: vec![],
            clips: oc_dsl::ast::ClipPool::new(),
            gain_automation: vec![],
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        });
        project
    }

    #[test]
    fn unhostable_au_preset_warns_without_stopping_the_render() {
        let project = project_with_au_preset("au:aumu:zzzz:zzzz");
        assert!(
            super::plugin_preset_render_warnings(&project).is_empty(),
            "a missing AudioUnit must not abort the render"
        );
        let soft = super::plugin_preset_render_soft_warnings(&project);
        assert_eq!(soft.len(), 1);
        assert!(soft[0].contains("AU preset 'Keys'"));
        assert!(soft[0].contains("track 'Keys'"));
        assert!(soft[0].contains("render silence"));
        assert!(soft[0].contains("zzzz:zzzz"), "{}", soft[0]);
        assert_eq!(super::plugin_preset_render_error(&project), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_au_preset_is_not_reported_as_silent() {
        let Ok(_) = super::au_instance::resolve_component("Apple: DLSMusicDevice") else {
            eprintln!("skipping: 'Apple: DLSMusicDevice' is not registered");
            return;
        };
        let project = project_with_au_preset("Apple: DLSMusicDevice");
        assert!(super::plugin_preset_render_soft_warnings(&project).is_empty());
    }

    #[test]
    fn referenced_sample_preset_fails_closed_instead_of_using_the_soundfont_fallback() {
        let mut project = Project {
            name: Some("Sampler".to_string()),
            ocp_version: Some("0.2.0".to_string()),
            properties: ProjectProperties::default(),
            tracks: vec![],
            patterns: vec![],
            presets: vec![PresetDef {
                name: "Violin SFZ".to_string(),
                source: Some("sample".to_string()),
                plugin: Some("Sitala".to_string()),
                program: None,
                patch: None,
                file: Some("samples/violin.sfz".to_string()),
                root_note: None,
                params: vec![],
                effects: vec![],
                extends: None,
            }],
            buses: vec![],
            master: None,
            sections: vec![],
            markers: Vec::new(),
            groups: Vec::new(),
        };
        project.tracks.push(Track {
            name: "Strings".to_string(),
            instrument: InstrumentPath::new("strings", "violin"),
            preset: Some("Violin SFZ".to_string()),
            engine: None,
            mix: MixState::default(),
            effects: vec![],
            phrases: vec![],
            patterns: vec![],
            inline_patterns: vec![],
            clips: oc_dsl::ast::ClipPool::new(),
            gain_automation: vec![],
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        });

        let warnings = super::plugin_preset_render_warnings(&project);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not renderable yet"));
        assert!(warnings[0].contains("captured plugin state"));
    }

    #[test]
    fn declared_plugin_state_requires_supported_nonempty_string() {
        use oc_dsl::ast::ParamValue;

        assert!(!super::preset_has_declared_plugin_state(
            &[(
                "vst3_state_file".to_string(),
                ParamValue::String("  ".to_string())
            )],
            super::PluginFormat::Vst3,
        ));
        assert!(!super::preset_has_declared_plugin_state(
            &[("vst3_state_file".to_string(), ParamValue::Bool(true))],
            super::PluginFormat::Vst3,
        ));
        assert!(super::preset_has_declared_plugin_state(
            &[(
                "plugin.state-file".to_string(),
                ParamValue::String("states/drums.bin".to_string()),
            )],
            super::PluginFormat::Vst3,
        ));
        assert!(super::preset_has_declared_plugin_state(
            &[(
                "VST3 State Hex".to_string(),
                ParamValue::String("00ff".to_string()),
            )],
            super::PluginFormat::Vst3,
        ));
    }

    #[test]
    fn discovery_walks_vendor_directories_and_stops_at_bundles() {
        let root = std::env::temp_dir().join(format!("oc-discover-{}", std::process::id()));
        let nested = root.join("Vendor").join("Nested.vst3");
        std::fs::create_dir_all(nested.join("Contents").join("x86_64-win"))
            .expect("create nested bundle");
        // A binary named like a bundle inside a bundle must not be reported as
        // a second plugin: the walk stops at the bundle directory.
        std::fs::write(
            nested
                .join("Contents")
                .join("x86_64-win")
                .join("Nested.vst3"),
            b"x",
        )
        .expect("write nested binary");
        let flat = root.join("Flat.vst3");
        std::fs::create_dir_all(&flat).expect("create flat bundle");

        let mut found = Vec::new();
        super::collect_plugin_files(&root, super::MAX_BUNDLE_DEPTH, &mut found);
        std::fs::remove_dir_all(&root).ok();

        let mut names: Vec<String> = found
            .iter()
            .map(|(path, _)| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(names, ["Flat", "Nested"], "found {found:?}");
        assert!(
            found
                .iter()
                .all(|(_, format)| *format == super::PluginFormat::Vst3)
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
