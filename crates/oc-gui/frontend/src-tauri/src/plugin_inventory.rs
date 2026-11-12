//! Disk cache for the plugin inventory.
//!
//! A cold inventory walks every installed bundle and inspects each one in a
//! helper process, which costs seconds. The cache stores the last result
//! together with the directory and receipt stamps it was taken from, so a warm
//! start validates those stamps with a handful of `stat` calls and serves the
//! stored list while a background refresh looks for changes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::plugin_store::{self, InstalledPlugin};
use crate::private_fs;

/// The cache file under `~/.opencompose`.
const CACHE_FILE: &str = "plugin-inventory.json";
/// Bumped whenever the stored shape changes; an older file is ignored.
const SCHEMA: &str = "opencompose_plugin_inventory_v1";
/// A larger file is junk, not a cache.
const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024;
/// Emitted with the entry count after a refresh replaces the inventory.
pub const UPDATED_EVENT: &str = "plugin_inventory_updated";

/// A directory the inventory reads; its mtime moves when a bundle is added or
/// removed, which is the only change the stored list cannot see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DirectoryStamp {
    path: String,
    modified_nanos: Option<u64>,
}

/// The scan-receipt file: rewriting it changes both its mtime and its length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReceiptStamp {
    path: String,
    modified_nanos: Option<u64>,
    bytes: u64,
}

/// One bundle binary: replacing it in place moves its mtime and size, while the
/// parent directory keeps its own mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BundleStamp {
    path: String,
    modified_nanos: Option<u64>,
    bytes: u64,
}

/// Everything a stored inventory depends on. Equal keys mean the stored lists
/// still describe the local machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InventoryKey {
    schema: String,
    /// A new build can add or rename built-in plugins.
    version: String,
    builtins: usize,
    directories: Vec<DirectoryStamp>,
    receipts: ReceiptStamp,
    runtimes: DirectoryStamp,
    /// Empty in a cache written before bundles were keyed; such a key never
    /// matches a live one, so the stored list is served and refreshed.
    #[serde(default)]
    bundles: Vec<BundleStamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedInventory {
    key: InventoryKey,
    plugins: Vec<oc_host::PluginInfo>,
    installed: Vec<InstalledPlugin>,
}

/// The lists the process currently serves, with the key they were taken from.
#[derive(Debug, Clone)]
struct Served {
    key: InventoryKey,
    installed: Vec<InstalledPlugin>,
}

/// What a refresh should build. `Cold` reuses the first scan so a launch with
/// no cache scans once; `Stale` always scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refresh {
    Cold,
    Stale,
}

static HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();
static REFRESHING: AtomicBool = AtomicBool::new(false);

fn served() -> &'static Mutex<Option<Served>> {
    static SERVED: OnceLock<Mutex<Option<Served>>> = OnceLock::new();
    SERVED.get_or_init(|| Mutex::new(None))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// The cache file; `OCP_PLUGIN_INVENTORY` overrides it for harnesses.
fn cache_path() -> PathBuf {
    std::env::var_os("OCP_PLUGIN_INVENTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| private_fs::config_dir().join(CACHE_FILE))
}

/// The stamps a cache written now would carry: one `stat` per plugin directory,
/// one for the receipt file, and one for the managed runtime root.
fn key_now() -> InventoryKey {
    let mut directories: Vec<String> = oc_host::plugin_search_paths();
    directories.extend(
        plugin_store::plugin_scan_dirs(
            plugin_store::host_os(),
            &plugin_store::HostDirs::from_env(),
        )
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned()),
    );
    directories.sort();
    directories.dedup();
    key_for(
        &directories,
        plugin_store::receipts_path().as_deref(),
        plugin_store::separator_runtime::root().as_deref(),
    )
}

fn key_for(
    directories: &[String],
    receipts: Option<&Path>,
    runtimes: Option<&Path>,
) -> InventoryKey {
    InventoryKey {
        schema: SCHEMA.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        builtins: oc_host::builtin::builtin_plugin_list().len(),
        directories: directories
            .iter()
            .map(|path| directory_stamp(Path::new(path)))
            .collect(),
        receipts: receipts.map(receipt_stamp).unwrap_or(ReceiptStamp {
            path: String::new(),
            modified_nanos: None,
            bytes: 0,
        }),
        runtimes: runtimes.map(directory_stamp).unwrap_or(DirectoryStamp {
            path: String::new(),
            modified_nanos: None,
        }),
        bundles: bundle_stamps(directories),
    }
}

/// The binary of every bundle the inventory would scan: one `stat` per bundle,
/// on top of the directory walk the inventory already does. Only the entries
/// that name a plugin bundle are stat'ed, so a folder of unrelated files stays
/// cheap.
fn bundle_stamps(directories: &[String]) -> Vec<BundleStamp> {
    let directories: Vec<PathBuf> = directories.iter().map(PathBuf::from).collect();
    let mut stamps: Vec<BundleStamp> = plugin_store::scan_plugin_bundles(&directories)
        .into_iter()
        .map(|bundle| bundle_stamp(Path::new(&bundle.path)))
        .collect();
    stamps.sort_by(|left, right| left.path.cmp(&right.path));
    stamps
}

/// A bundle's loadable binary, or the bundle path itself when no binary
/// resolves yet; the directory mtime still reports a broken install growing one.
fn bundle_stamp(bundle: &Path) -> BundleStamp {
    let binary = oc_host::bundle::binary_path(bundle).unwrap_or_else(|| bundle.to_path_buf());
    let meta = std::fs::metadata(&binary).ok();
    BundleStamp {
        path: binary.to_string_lossy().into_owned(),
        modified_nanos: meta
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as u64),
        bytes: meta.map(|meta| meta.len()).unwrap_or(0),
    }
}

fn modified_nanos(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
}

fn directory_stamp(path: &Path) -> DirectoryStamp {
    DirectoryStamp {
        path: path.to_string_lossy().into_owned(),
        modified_nanos: modified_nanos(path),
    }
}

fn receipt_stamp(path: &Path) -> ReceiptStamp {
    ReceiptStamp {
        path: path.to_string_lossy().into_owned(),
        modified_nanos: modified_nanos(path),
        bytes: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
    }
}

/// The stored inventory, or `None` when the file is missing, oversized,
/// unreadable, or from another schema. A corrupt file is ignored, not repaired:
/// the next scan replaces it.
fn read(path: &Path) -> Option<CachedInventory> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CACHE_BYTES {
        return None;
    }
    let cache: CachedInventory = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (cache.key.schema == SCHEMA).then_some(cache)
}

fn write(path: &Path, cache: &CachedInventory) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    private_fs::write_atomic(path, &bytes)
}

/// Serve the stored inventory through `install`, and ask for a refresh only
/// when the stored lists cannot answer for the local machine.
fn load_into(
    path: &Path,
    key: &InventoryKey,
    install: &mut dyn FnMut(&CachedInventory),
    refresh: &mut dyn FnMut(Refresh),
) {
    match read(path) {
        Some(cache) if cache.key == *key => install(&cache),
        Some(cache) => {
            // A moved key still describes the last scan; serving it keeps the
            // first paint off the scan path.
            install(&cache);
            refresh(Refresh::Stale);
        }
        None => refresh(Refresh::Cold),
    }
}

/// Load the cached inventory at startup and refresh it in the background when
/// the key moved. Returns before any scan: a launch with no cache starts one in
/// the background so the next launch is warm.
pub fn start(app: tauri::AppHandle) {
    let _ = HANDLE.set(app);
    load_into(
        &cache_path(),
        &key_now(),
        &mut |cache| {
            log::debug!(
                "plugin inventory: {} entries from cache",
                cache.plugins.len()
            );
            publish(cache);
        },
        &mut start_refresh,
    );
}

/// The inventory to show now. A moved key serves the stored list and refreshes
/// in the background, so the caller never waits for a scan.
pub fn plugins() -> Vec<oc_host::PluginInfo> {
    if !served_key_is_current() {
        start_refresh(Refresh::Stale);
    }
    oc_host::cached_plugins().to_vec()
}

/// Run the scan the explicit "Scan plugins" action asks for, persist it, and
/// tell the panels it landed. This scan retries bundles an earlier pass
/// quarantined, so a fixed install reappears without deleting the receipts.
pub fn rescan() -> Vec<oc_host::PluginInfo> {
    let (plugins, _) = oc_host::rescan_all_plugins();
    let plugins = oc_host::install_plugin_inventory(plugins).to_vec();
    store(&plugins);
    if let Some(app) = HANDLE.get() {
        let _ = app.emit(UPDATED_EVENT, plugins.len());
    }
    plugins
}

/// The Store's installed list. The stored list answers while the key holds; a
/// moved key walks the folders, which reads directory entries and no bundles,
/// and refreshes the inventory in the background.
pub fn installed() -> Vec<InstalledPlugin> {
    let key = key_now();
    let cached = lock(served())
        .as_ref()
        .filter(|served| served.key == key)
        .map(|served| served.installed.clone());
    match cached {
        Some(installed) => installed,
        None => {
            start_refresh(Refresh::Stale);
            plugin_store::installed_plugins()
        }
    }
}

fn served_key_is_current() -> bool {
    let key = key_now();
    lock(served())
        .as_ref()
        .is_some_and(|served| served.key == key)
}

fn publish(cache: &CachedInventory) {
    oc_host::install_plugin_inventory(cache.plugins.clone());
    *lock(served()) = Some(Served {
        key: cache.key.clone(),
        installed: cache.installed.clone(),
    });
}

/// Scan the bundles now and publish the result.
fn scan() -> Vec<oc_host::PluginInfo> {
    oc_host::install_plugin_inventory(oc_host::scan_all_plugins()).to_vec()
}

/// Persist `plugins` with the current key and the installed list it implies.
fn store(plugins: &[oc_host::PluginInfo]) {
    let key = key_now();
    let installed = plugin_store::installed_plugins();
    let cache = CachedInventory {
        key: key.clone(),
        plugins: plugins.to_vec(),
        installed: installed.clone(),
    };
    if let Err(error) = write(&cache_path(), &cache) {
        log::warn!("plugin inventory cache: {error}");
    }
    *lock(served()) = Some(Served { key, installed });
}

/// Scan in the background, then publish and persist the list.
fn start_refresh(mode: Refresh) {
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(app) = HANDLE.get().cloned() else {
        REFRESHING.store(false, Ordering::Release);
        return;
    };
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let plugins = match mode {
            // The first scan is published by `cached_plugins`; readers that
            // arrive while it runs wait for the same scan instead of starting
            // a second one.
            Refresh::Cold => oc_host::cached_plugins().to_vec(),
            Refresh::Stale => scan(),
        };
        store(&plugins);
        REFRESHING.store(false, Ordering::Release);
        log::debug!(
            "plugin inventory: refreshed {} entries in {:?}",
            plugins.len(),
            started.elapsed()
        );
        let _ = app.emit(UPDATED_EVENT, plugins.len());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ocp-plugin-inventory-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The plugin folders, receipt file and runtime root the key is built from.
    struct Fixture {
        root: TempDir,
        vst3: PathBuf,
        receipts: PathBuf,
        runtimes: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = TempDir::new(tag);
            let vst3 = root.join("VST3");
            std::fs::create_dir_all(&vst3).unwrap();
            let receipts = root.join("plugin-scan-receipts.json");
            std::fs::write(&receipts, b"[]").unwrap();
            let runtimes = root.join("runtimes");
            std::fs::create_dir_all(&runtimes).unwrap();
            Self {
                root,
                vst3,
                receipts,
                runtimes,
            }
        }

        fn key(&self) -> InventoryKey {
            key_for(
                &[self.vst3.to_string_lossy().into_owned()],
                Some(&self.receipts),
                Some(&self.runtimes),
            )
        }

        fn cache(&self, key: &InventoryKey) -> CachedInventory {
            CachedInventory {
                key: key.clone(),
                plugins: vec![plugin("Loud")],
                installed: vec![InstalledPlugin {
                    name: "Loud".into(),
                    format: "vst3".into(),
                    path: format!("{}/Loud.vst3", self.vst3.display()),
                }],
            }
        }

        fn cache_file(&self) -> PathBuf {
            self.root.join("plugin-inventory.json")
        }
    }

    fn plugin(name: &str) -> oc_host::PluginInfo {
        oc_host::PluginInfo {
            id: format!("vst3:{}", name.to_lowercase()),
            name: name.into(),
            vendor: String::new(),
            format: oc_host::PluginFormat::Vst3,
            path: format!("/plugins/{name}.vst3"),
            category: oc_host::PluginCategory::Effect,
            num_params: 0,
            version: None,
            architectures: Vec::new(),
            state_abi: None,
        }
    }

    /// Coarse filesystems stamp directory mtimes by the second.
    fn tick() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    /// Write a bundle whose binary sits where this platform's loader looks for
    /// it, and return that binary's path.
    fn write_bundle_binary(bundle: &Path, stem: &str, bytes: &[u8]) -> PathBuf {
        #[cfg(target_os = "macos")]
        let directory = bundle.join("Contents/MacOS");
        #[cfg(target_os = "linux")]
        let directory = bundle.join(format!("Contents/{}-linux", std::env::consts::ARCH));
        #[cfg(target_os = "windows")]
        let directory = bundle.join(format!("Contents/{}-win", std::env::consts::ARCH));
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        let directory = bundle.join("Contents");

        std::fs::create_dir_all(&directory).unwrap();
        let binary = directory.join(stem);
        std::fs::write(&binary, bytes).unwrap();
        binary
    }

    #[test]
    fn a_plugin_binary_rewritten_in_place_moves_the_key() {
        let fixture = Fixture::new("bundle-update");
        let bundle = fixture.vst3.join("Loud.vst3");
        let binary = write_bundle_binary(&bundle, "Loud", b"first build");
        assert_eq!(
            oc_host::bundle::binary_path(&bundle),
            Some(binary.clone()),
            "the fixture must sit where the bundle walk finds it"
        );
        let key = fixture.key();
        assert_eq!(key, fixture.key(), "an untouched bundle must keep the key");

        // A rebuild replaces the binary in place: same path, same length, a new
        // mtime. The parent directory's mtime does not move.
        tick();
        std::fs::write(&binary, b"other build").unwrap();
        assert_ne!(key, fixture.key(), "a replaced binary must move the key");

        // A build of a different length moves it as well.
        let key = fixture.key();
        tick();
        std::fs::write(&binary, b"a longer build").unwrap();
        assert_ne!(key, fixture.key(), "a resized binary must move the key");
    }

    #[test]
    fn the_cache_key_matches_until_a_plugin_folder_changes() {
        let fixture = Fixture::new("key");
        let key = fixture.key();
        assert_eq!(key, fixture.key(), "unchanged folders must share a key");

        tick();
        std::fs::write(fixture.vst3.join("Loud.vst3"), b"bundle").unwrap();
        assert_ne!(key, fixture.key(), "a new bundle must move the key");

        let key = fixture.key();
        tick();
        let mut receipts = std::fs::read(&fixture.receipts).unwrap();
        receipts.push(b'\n');
        std::fs::write(&fixture.receipts, receipts).unwrap();
        assert_ne!(
            key,
            fixture.key(),
            "a rewritten receipt file must move the key"
        );

        let key = fixture.key();
        tick();
        std::fs::create_dir(fixture.runtimes.join("spleeter")).unwrap();
        assert_ne!(
            key,
            fixture.key(),
            "a new managed runtime must move the key"
        );
    }

    #[test]
    fn a_stored_inventory_answers_without_walking_the_bundles() {
        let fixture = Fixture::new("load");
        let path = fixture.cache_file();
        let key = fixture.key();
        write(&path, &fixture.cache(&key)).unwrap();

        let mut walks = 0;
        let mut served = None;
        load_into(
            &path,
            &key,
            &mut |cache| served = Some(cache.clone()),
            &mut |_| walks += 1,
        );

        let served = served.expect("a matching cache must be served");
        assert_eq!(walks, 0, "a matching key must not walk the bundles");
        assert_eq!(served.plugins.len(), 1);
        assert_eq!(served.plugins[0].name, "Loud");
        assert_eq!(served.installed.len(), 1);
    }

    #[test]
    fn a_moved_key_serves_the_stored_list_and_refreshes_in_the_background() {
        let fixture = Fixture::new("stale");
        let path = fixture.cache_file();
        write(&path, &fixture.cache(&fixture.key())).unwrap();
        tick();
        std::fs::write(fixture.vst3.join("New.vst3"), b"bundle").unwrap();

        let mut served = None;
        let mut refresh = None;
        load_into(
            &path,
            &fixture.key(),
            &mut |cache| served = Some(cache.clone()),
            &mut |mode| refresh = Some(mode),
        );

        assert!(served.is_some(), "the stored list must still be served");
        assert_eq!(refresh, Some(Refresh::Stale));
    }

    #[test]
    fn a_corrupt_cache_is_ignored_and_rebuilt() {
        let fixture = Fixture::new("corrupt");
        let path = fixture.cache_file();
        std::fs::write(&path, b"{ not a cache").unwrap();

        let mut served = false;
        let mut refresh = None;
        load_into(&path, &fixture.key(), &mut |_| served = true, &mut |mode| {
            refresh = Some(mode)
        });

        assert!(!served, "a corrupt file must not be served");
        assert_eq!(refresh, Some(Refresh::Cold));
    }

    /// Measures cold scan versus warm cache read latency with installed plug-ins.
    ///
    /// ```sh
    /// mkdir -p /tmp/ocp-inventory
    /// export OCP_TEST_PLUGINS=1
    /// export OCP_PLUGIN_SCAN_HELPER=target/debug/opencompose-plugin-scan-helper
    /// export OCP_PLUGIN_SCAN_RECEIPTS=/tmp/ocp-inventory/receipts.json
    /// export OCP_PLUGIN_INVENTORY=/tmp/ocp-inventory/plugin-inventory.json
    /// cargo test -p oc-gui --lib -- plugin_inventory --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn review_plugin_inventory_cache_timings() {
        assert!(
            oc_host::external_plugin_tests_enabled(),
            "Set OCP_TEST_PLUGINS=1 to measure installed plug-ins"
        );
        let path = cache_path();
        let _ = std::fs::remove_file(&path);

        let started = std::time::Instant::now();
        let scanned = rescan();
        let scan = started.elapsed();

        let started = std::time::Instant::now();
        let key = key_now();
        let key_time = started.elapsed();

        let started = std::time::Instant::now();
        let cached = read(&path).expect("rescan must leave a readable cache");
        let read_time = started.elapsed();

        let started = std::time::Instant::now();
        oc_host::install_plugin_inventory(cached.plugins.clone());
        let install_time = started.elapsed();

        let started = std::time::Instant::now();
        let served = plugins();
        let serve_time = started.elapsed();

        eprintln!(
            "inventory: {} entries, cache {} bytes | scan {:?}, key {:?}, read {:?}, install {:?}, warm serve {:?}",
            scanned.len(),
            std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
            scan,
            key_time,
            read_time,
            install_time,
            serve_time
        );
        assert_eq!(cached.key, key);
        assert_eq!(served.len(), scanned.len());
    }
}
