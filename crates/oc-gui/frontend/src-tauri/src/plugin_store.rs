//! Bundled free-plugin catalog and the generic installer used by the Store.
//!
//! The catalog is compiled into the binary. Every URL opened from the Store is
//! checked against it. Resolution, download verification and archive extraction
//! live in `oc-market`, so the Store, the agent tools and `ocp plugin install`
//! install the same build and refuse in the same words.

use crate::plugin_index::CatalogAuthority;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[path = "plugin_content.rs"]
mod plugin_content;
#[path = "separator_runtime.rs"]
pub mod separator_runtime;

use oc_market::catalog::{Candidate, Plan, Refusal};

pub use oc_market::install::{
    BundleFormat, HostDirs, HostOs, InstallArchive, InstallFormat, InstallKind, InstallOutcome,
    InstallSource, InstallTarget,
};
pub use oc_market::verify::{Mismatch as DownloadMismatch, hex, verify_download};
pub use oc_market::{
    bundle_format, bundle_kind, extract_bundles, extract_tar_bundles, host_os, host_platform,
    path_is_writable, sanitize_zip_path, select_install_target,
};

const CATALOG_JSON: &str = include_str!("../../src/data/plugin-catalog.json");
use oc_market::catalog_wire::{MAX_DOWNLOAD_BYTES, MAX_RUNTIME_DOWNLOAD_BYTES};

pub use oc_market::catalog_wire::{Install as CatalogInstall, PackEntry, PackLicense};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One installable plugin catalogue entry.
pub struct CatalogEntry {
    #[serde(flatten)]
    pub document: oc_market::catalog_wire::Entry,
    #[serde(default, skip_deserializing)]
    pub needs_admin: bool,
    /// Whether this host can install the entry at all. Computed per response;
    /// the Store button disables with `refusal` instead of failing on click.
    #[serde(default, skip_deserializing)]
    pub installable: bool,
    /// Whether a scanned bundle already satisfies the entry. Computed per
    /// response from the same matcher the manual check uses.
    #[serde(default, skip_deserializing)]
    pub installed: bool,
    /// `extract` (headless) or `installer` (a person finishes the install).
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub install_mode: Option<&'static str>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<PluginSearchRefusal>,
}

impl std::ops::Deref for CatalogEntry {
    type Target = oc_market::catalog_wire::Entry;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}
impl std::ops::DerefMut for CatalogEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}
impl From<oc_market::catalog_wire::Entry> for CatalogEntry {
    fn from(document: oc_market::catalog_wire::Entry) -> Self {
        Self {
            document,
            needs_admin: false,
            installable: false,
            installed: false,
            install_mode: None,
            refusal: None,
        }
    }
}

/// One parsed catalog revision: the plugin entries and the instrument packs.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub plugins: Vec<CatalogEntry>,
    pub packs: Vec<PackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// A plugin found installed on this host.
pub struct InstalledPlugin {
    pub name: String,
    pub format: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
/// Catalogue with installed plugin state.
pub struct PluginCatalog {
    pub platform: String,
    pub entries: Vec<CatalogEntry>,
    pub installed: Vec<InstalledPlugin>,
    pub source: String,
    pub generated_at: Option<String>,
    pub fetched_at: Option<u64>,
    pub stale_days: Option<u64>,
    pub last_error: Option<String>,
}

/// The compiled catalog and its timestamp; the final fallback for the Store.
pub fn bundled_catalog() -> Result<(Vec<CatalogEntry>, String), String> {
    let file = oc_market::catalog_wire::parse(CATALOG_JSON.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok((
        file.entries.into_iter().map(CatalogEntry::from).collect(),
        file.generated_at,
    ))
}

pub(crate) fn from_shared_catalog(file: oc_market::catalog_wire::Catalog) -> Catalog {
    Catalog {
        plugins: file.entries.into_iter().map(CatalogEntry::from).collect(),
        packs: file
            .packs
            .into_iter()
            .filter_map(|pack| match pack {
                oc_market::catalog_wire::Pack::Soundfont(pack) => Some(pack),
                oc_market::catalog_wire::Pack::Unknown(_) => None,
            })
            .collect(),
    }
}

/// Whether an entry can be installed from the panel.
pub fn is_panel_install(
    entries: &[CatalogEntry],
    source: &str,
    id: &str,
    platform: Option<&str>,
    format: Option<&str>,
) -> Result<bool, String> {
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("Unknown plugin catalog id '{id}'"))?;
    let host = host_platform();
    let install = resolve_pull_install(
        entry,
        platform.unwrap_or(&host),
        format.map(parse_install_format).transpose()?,
    )?;
    // Remote/cache entries have passed the signed-index verifier. Bundled
    // discovery entries may omit integrity pins and cannot authorize installs.
    if !matches!(source, "remote" | "cache")
        || platform.is_some_and(|platform| platform != host)
        || !install.sha256.as_deref().is_some_and(is_lowercase_sha256)
        || install.bytes.is_none_or(|bytes| bytes == 0)
    {
        return Ok(false);
    }
    Ok(matches!(
        effective_install_kind(install),
        Some(
            InstallKind::SeparatorRuntime
                | InstallKind::ZipVst3
                | InstallKind::ZipVst2
                | InstallKind::ZipClap
                | InstallKind::ZipLv2
        )
    ))
}

use oc_market::catalog_wire::is_lowercase_sha256;
#[cfg(test)]
use oc_market::catalog_wire::{is_https_url, is_valid_id};

/// Build the Store response for the current host, including install hints.
pub fn catalog_response(entries: &[CatalogEntry]) -> PluginCatalog {
    let mut entries = entries.to_vec();
    let platform = host_platform();
    // Drop companion-content bundles that are not ready before anything reads
    // the inventory, so the per-entry flag and the list agree.
    let mut installed = crate::plugin_inventory::installed();
    installed.retain(|plugin| {
        !entries.iter().any(|entry| {
            let name = normalize_plugin_name(&plugin.name);
            bundle_keys(entry).iter().any(|key| same_bundle(key, &name))
                && !plugin_ready(entry, plugin, &platform, &HostDirs::from_env())
        })
    });
    for entry in &mut entries {
        entry.needs_admin = entry_needs_admin(entry, &platform);
        entry.installed = installed_match(entry, &installed).is_some();
        // One resolver decides, so the Store and the agent tools agree.
        let status = oc_market::status(&entry.id, &entry.name, &candidates(entry), &platform, None);
        entry.installable = status.installable;
        entry.install_mode = status.mode.map(|mode| mode.as_str());
        entry.refusal = status.refusal.map(|refusal| PluginSearchRefusal {
            code: refusal.code(),
            message: refusal.to_string(),
            url: refusal.url().map(str::to_string),
        });
    }
    PluginCatalog {
        platform,
        entries,
        installed,
        source: "bundled".into(),
        generated_at: None,
        fetched_at: None,
        stale_days: None,
        last_error: None,
    }
}

// ============================================================================
// Resolution through the shared marketplace policy
// ============================================================================

fn candidate(install: &CatalogInstall) -> Candidate {
    Candidate {
        source: install.kind,
        platform: install.platform.to_string(),
        format: install.format,
        archive: install.archive,
        url: install.url.clone(),
        sha256: install.sha256.clone(),
        bytes: install.bytes,
        runtime: install.runtime.is_some(),
        wrapped: install.wrapped,
    }
}

fn candidates(entry: &CatalogEntry) -> Vec<Candidate> {
    entry.installs.iter().map(candidate).collect()
}

/// The install kind the Store would use. A managed separator runtime is
/// marked by the catalog's `runtime` descriptor, not by the archive.
fn effective_install_kind(install: &CatalogInstall) -> Option<InstallKind> {
    oc_market::install_kind(
        install.kind,
        install.archive,
        &install.url,
        install.format,
        install.runtime.is_some(),
        install.wrapped,
    )
}

/// One resolved entry: the shared plan plus the catalog object it came from.
/// The runtime and companion-content paths still need the raw install.
#[derive(Debug)]
struct Resolved<'a> {
    plan: Plan,
    install: &'a CatalogInstall,
}

/// The one resolution step for every surface: the GUI installer, the agent
/// tools and `ocp plugin install` all land here.
fn plan_for<'a>(
    entry: &'a CatalogEntry,
    platform: &str,
    format: Option<InstallFormat>,
) -> Result<Resolved<'a>, Refusal> {
    let candidates = candidates(entry);
    let plan = oc_market::plan(&entry.id, &entry.name, &candidates, platform, format)?;
    let index =
        oc_market::choose_index(&candidates, platform, format).ok_or_else(|| Refusal::NoBuild {
            name: entry.name.clone(),
            platform: platform.to_string(),
        })?;
    Ok(Resolved {
        plan,
        install: &entry.installs[index],
    })
}

/// The build `is_panel_install` asked about, chosen by the shared ranking.
fn resolve_pull_install<'a>(
    entry: &'a CatalogEntry,
    platform: &str,
    format: Option<InstallFormat>,
) -> Result<&'a CatalogInstall, String> {
    let candidates = candidates(entry);
    let index = oc_market::choose_index(&candidates, platform, format).ok_or_else(|| {
        format!(
            "{} has no install for {platform}{}",
            entry.name,
            format
                .map(|value| format!(" and {value:?}"))
                .unwrap_or_default()
        )
    })?;
    Ok(&entry.installs[index])
}

/// Store hint: would installing this build need administrator rights? It
/// follows the build kind, so an unpinned entry still reports the mechanism its
/// publisher will use once the artifact is pinned.
fn entry_needs_admin(entry: &CatalogEntry, platform: &str) -> bool {
    let candidates = candidates(entry);
    let Some(candidate) = oc_market::choose(&candidates, platform, None) else {
        return false;
    };
    let Some(kind) = oc_market::install_kind(
        candidate.source,
        candidate.archive,
        &candidate.url,
        candidate.format,
        candidate.runtime,
        candidate.wrapped,
    ) else {
        return false;
    };
    match kind {
        InstallKind::Pkg | InstallKind::Msi => true,
        InstallKind::ZipVst3
        | InstallKind::ZipVst2
        | InstallKind::ZipClap
        | InstallKind::ZipLv2 => {
            let Some(format) = bundle_format(kind) else {
                return false;
            };
            select_install_target(
                host_os(),
                kind,
                format,
                &HostDirs::from_env(),
                path_is_writable,
            )
            .map(|target| target.needs_admin)
            .unwrap_or(false)
        }
        _ => false,
    }
}

// ============================================================================
// URL allowlist
// ============================================================================

fn catalog_urls(entries: &[CatalogEntry]) -> HashSet<&str> {
    let mut urls = HashSet::new();
    for entry in entries {
        if let Some(homepage) = &entry.homepage {
            urls.insert(homepage.as_str());
        }
        for install in &entry.installs {
            urls.insert(install.url.as_str());
            if let Some(content) = &install.companion_content {
                urls.insert(content.url.as_str());
            }
        }
    }
    urls
}

fn url_allowed(entries: &[CatalogEntry], url: &str) -> bool {
    oc_market::is_install_url(url) && catalog_urls(entries).contains(url)
}

/// Open a URL only when it is listed in the active catalog.
pub fn open_allowed_url(entries: &[CatalogEntry], url: &str) -> Result<(), String> {
    if !url_allowed(entries, url) {
        return Err("Refusing to open a URL that is not part of the active plugin catalog".into());
    }
    open_external(url)
}

/// The vendor page a page-only entry sends a person to on this host.
///
/// The URL comes from the active catalog, never from the webview or an AI tool
/// call. An entry with a direct build is refused here: the Store has an install
/// path for it, so opening a page would only leave the build behind.
pub fn manual_page_url(entries: &[CatalogEntry], id: &str) -> Result<String, String> {
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("Unknown plugin catalog id '{id}'"))?;
    let platform = host_platform();
    let candidates = candidates(entry);
    let index = oc_market::choose_index(&candidates, &platform, None).ok_or_else(|| {
        Refusal::NoBuild {
            name: entry.name.clone(),
            platform,
        }
        .to_string()
    })?;
    let candidate = &candidates[index];
    if candidate.source != InstallSource::VendorPage {
        return Err(format!(
            "{} installs from its pinned artifact; use Install",
            entry.name
        ));
    }
    Ok(candidate.url.clone())
}

fn open_external(target: &str) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(target).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", target])
            .spawn()
    } else {
        Command::new("xdg-open").arg(target).spawn()
    };
    status
        .map(|_| ())
        .map_err(|error| format!("Failed to open '{target}': {error}"))
}

// ============================================================================
// Installed detection
// ============================================================================

/// Directories scanned for installed bundles on each platform.
pub fn plugin_scan_dirs(os: HostOs, dirs: &HostDirs) -> Vec<PathBuf> {
    let mut result = Vec::new();
    match os {
        HostOs::Macos => {
            if let Some(home) = &dirs.home {
                let base = home.join("Library/Audio/Plug-Ins");
                result.push(base.join("VST3"));
                result.push(base.join("VST"));
                result.push(base.join("CLAP"));
                result.push(base.join("LV2"));
                result.push(base.join("Components"));
            }
            result.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
            result.push(PathBuf::from("/Library/Audio/Plug-Ins/VST"));
            result.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
            result.push(PathBuf::from("/Library/Audio/Plug-Ins/LV2"));
            result.push(PathBuf::from("/Library/Audio/Plug-Ins/Components"));
        }
        HostOs::Windows => {
            if let Some(base) = &dirs.common_program_files {
                result.push(base.join("VST3"));
                result.push(base.join("VST2"));
                result.push(base.join("CLAP"));
                result.push(base.join("LV2"));
            }
            if let Some(base) = &dirs.local_app_data {
                result.push(base.join("Programs/Common/VST3"));
                result.push(base.join("Programs/Common/VST2"));
                result.push(base.join("Programs/Common/CLAP"));
                result.push(base.join("Programs/Common/LV2"));
            }
        }
        HostOs::Linux => {
            if let Some(home) = &dirs.home {
                result.push(home.join(".vst3"));
                result.push(home.join(".vst"));
                result.push(home.join(".clap"));
                result.push(home.join(".lv2"));
            }
            result.push(PathBuf::from("/usr/lib/vst3"));
            result.push(PathBuf::from("/usr/lib/vst"));
            result.push(PathBuf::from("/usr/lib/clap"));
            result.push(PathBuf::from("/usr/lib/lv2"));
            result.push(PathBuf::from("/usr/local/lib/vst3"));
            result.push(PathBuf::from("/usr/local/lib/vst"));
            result.push(PathBuf::from("/usr/local/lib/clap"));
            result.push(PathBuf::from("/usr/local/lib/lv2"));
        }
    }
    result
}

/// Scan explicit directories for VST3/VST2/CLAP/LV2/AU bundles; missing
/// directories are ignored so detection degrades gracefully.
pub fn scan_plugin_bundles(directories: &[PathBuf]) -> Vec<InstalledPlugin> {
    let mut found = Vec::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let format = match extension.to_ascii_lowercase().as_str() {
                "vst3" => "vst3",
                "vst" => "vst2",
                "clap" => "clap",
                "lv2" => "lv2",
                "component" => "au",
                _ => continue,
            };
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if stem.is_empty() {
                continue;
            }
            found.push(InstalledPlugin {
                name: stem.to_string(),
                format: format.to_string(),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    found
}

#[derive(Debug, Deserialize)]
struct ScanReceipt {
    #[serde(default)]
    plugins: Vec<ReceiptPlugin>,
}

#[derive(Debug, Deserialize)]
struct ReceiptPlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    path: String,
}

pub(crate) fn receipts_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OCP_PLUGIN_SCAN_RECEIPTS").map(PathBuf::from) {
        return Some(path);
    }
    // A test build keeps the receipts in the process scratch root.
    Some(crate::private_fs::config_dir().join("plugin-scan-receipts.json"))
}

fn scan_receipts(path: &Path) -> Vec<InstalledPlugin> {
    let Ok(data) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(receipts) = serde_json::from_slice::<Vec<ScanReceipt>>(&data) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for receipt in receipts {
        for plugin in receipt.plugins {
            if plugin.name.is_empty() {
                continue;
            }
            found.push(InstalledPlugin {
                name: plugin.name,
                format: plugin.format.to_ascii_lowercase(),
                path: plugin.path,
            });
        }
    }
    found
}

/// Walk the plugin folders, the scan receipts and the managed runtimes.
pub(crate) fn installed_plugins() -> Vec<InstalledPlugin> {
    let dirs = HostDirs::from_env();
    let mut found = scan_plugin_bundles(&plugin_scan_dirs(host_os(), &dirs));
    if let Some(path) = receipts_path() {
        found.extend(scan_receipts(&path));
    }
    found.extend(separator_runtime::installed());
    let mut seen = HashSet::new();
    found.retain(|plugin| seen.insert((plugin.name.clone(), plugin.path.clone())));
    found.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.format.cmp(&right.format))
    });
    found
}

/// The bundle names an entry may appear under: its name, its aliases, and the
/// slug of a pinned scanner id, all normalised.
fn bundle_keys(entry: &CatalogEntry) -> Vec<String> {
    std::iter::once(&entry.name)
        .chain(entry.aliases.iter())
        .map(|name| normalize_plugin_name(name))
        .chain(
            entry
                .installs
                .iter()
                .filter_map(|install| install.plugin_id.as_deref())
                .map(|id| normalize_plugin_name(scanner_slug(id))),
        )
        .filter(|key| !key.is_empty())
        .collect()
}

/// The last segment of a scanner id: `vst3:surge_xt` gives `surge_xt`, and an
/// LV2 URI gives its last path segment.
fn scanner_slug(id: &str) -> &str {
    id.trim_end_matches('/')
        .rsplit([':', '/'])
        .next()
        .unwrap_or(id)
}

/// A bundle name that carries a version suffix still names the same product, so
/// `TAL-Vocoder-2` matches `TAL Vocoder`. Shorter keys must be long enough that
/// a prefix cannot belong to another vendor's product.
fn same_bundle(key: &str, name: &str) -> bool {
    const MIN_PREFIX: usize = 6;
    if key == name {
        return true;
    }
    let (short, long) = if key.len() < name.len() {
        (key, name)
    } else {
        (name, key)
    };
    short.len() >= MIN_PREFIX && long.starts_with(short)
}

/// The scanned bundle that satisfies an entry, if one does.
///
/// This is the single matcher: the Store badge, the agent's `installed` flag and
/// the manual "check again" action all report the same answer.
pub fn installed_match<'a>(
    entry: &CatalogEntry,
    installed: &'a [InstalledPlugin],
) -> Option<&'a InstalledPlugin> {
    installed_match_at(entry, installed, &host_platform(), &HostDirs::from_env())
}

fn installed_match_at<'a>(
    entry: &CatalogEntry,
    installed: &'a [InstalledPlugin],
    platform: &str,
    dirs: &HostDirs,
) -> Option<&'a InstalledPlugin> {
    let keys = bundle_keys(entry);
    installed.iter().find(|plugin| {
        let name = normalize_plugin_name(&plugin.name);
        !name.is_empty()
            && keys.iter().any(|key| same_bundle(key, &name))
            && plugin_ready(entry, plugin, platform, dirs)
    })
}

fn plugin_ready(
    entry: &CatalogEntry,
    plugin: &InstalledPlugin,
    platform: &str,
    dirs: &HostDirs,
) -> bool {
    if !entry
        .installs
        .iter()
        .any(|install| install.platform == platform && install.companion_content.is_some())
    {
        return true;
    }
    let Some(format) = InstallFormat::parse(&plugin.format.to_ascii_lowercase()) else {
        return false;
    };
    let candidates = candidates(entry);
    oc_market::choose_index(&candidates, platform, Some(format))
        .and_then(|index| entry.installs.get(index))
        .is_some_and(|install| plugin_content::install_ready(install, dirs))
}

/// What a fresh walk of the plugin folders found for one catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledCheck {
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// The bundle that satisfied the entry, when one did.
    pub matched: Option<String>,
    /// Bundles the walk saw, so an empty scan is visible instead of silent.
    pub scanned: usize,
}

/// Re-scan the plugin folders and report whether `id` is installed now.
///
/// The walk reads the plugin directories and the scan receipts; it downloads
/// nothing and writes nothing.
pub fn installed_check(entries: &[CatalogEntry], id: &str) -> Result<InstalledCheck, String> {
    installed_check_in(entries, id, &installed_plugins())
}

/// [`installed_check`] against an inventory the caller supplies.
fn installed_check_in(
    entries: &[CatalogEntry],
    id: &str,
    installed: &[InstalledPlugin],
) -> Result<InstalledCheck, String> {
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("Unknown plugin catalog id '{id}'"))?;
    let matched = installed_match(entry, installed);
    Ok(InstalledCheck {
        id: entry.id.clone(),
        name: entry.name.clone(),
        installed: matched.is_some(),
        matched: matched.map(|plugin| plugin.name.clone()),
        scanned: installed.len(),
    })
}

// ============================================================================
// Install
// ============================================================================

/// Where a bundle install writes: an explicit directory when the caller names
/// one (tests and CI), otherwise the host's plugin directory.
fn resolve_target(
    kind: InstallKind,
    format: BundleFormat,
    into: Option<&Path>,
) -> Result<InstallTarget, String> {
    match into {
        Some(dir) => Ok(InstallTarget {
            dir: dir.to_path_buf(),
            needs_admin: false,
        }),
        None => select_install_target(
            host_os(),
            kind,
            format,
            &HostDirs::from_env(),
            path_is_writable,
        ),
    }
}

/// Install a catalog entry. The URL always comes from the active catalog, never
/// from the webview or an AI tool call.
pub async fn install(
    authority: &CatalogAuthority,
    entries: &[CatalogEntry],
    id: &str,
    overwrite: bool,
    into: Option<&Path>,
) -> Result<InstallOutcome, String> {
    authority.check()?;
    // A cancelled attempt leaves its id in the registry; this one starts clean.
    clear_cancel(id);
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| Refusal::UnknownId { id: id.to_string() }.to_string())?;
    let Resolved { plan, install } =
        plan_for(entry, &host_platform(), None).map_err(|refusal| refusal.to_string())?;
    if plan.kind == InstallKind::SeparatorRuntime {
        return separator_runtime::install(authority, entries, entry, install).await;
    }
    let bytes = download_verified(install, entries).await?;
    if cancelled(id) {
        return Err(format!("{id} was cancelled"));
    }
    if install.companion_content.is_some() {
        return plugin_content::install(
            authority, entries, entry, install, &bytes, overwrite, into,
        )
        .await;
    }
    let _publication = authority.publication_guard()?;
    match plan.kind {
        InstallKind::ZipVst3
        | InstallKind::ZipVst2
        | InstallKind::ZipClap
        | InstallKind::ZipLv2 => {
            let format = bundle_format(plan.kind)
                .ok_or_else(|| format!("{} has no supported bundle archive", entry.name))?;
            let outcome = if plan.archive == Some(InstallArchive::TarGz) {
                install_tar_bytes(&bytes, entry, host_os(), format, overwrite, into)?
            } else {
                install_zip_bytes(&bytes, entry, host_os(), format, overwrite, into)?
            };
            match outcome {
                InstallOutcome::WrappedInstaller { name, bytes } => {
                    open_wrapped_installer(entry, &name, &bytes)
                }
                other => Ok(other),
            }
        }
        InstallKind::Appimage => install_appimage_bytes(&bytes, entry, overwrite, into),
        InstallKind::InstallerArchive => {
            let (name, inner) = oc_market::install::unwrap_installer(&bytes, plan.archive)
                .map_err(|error| format!("{}: {error}", entry.name))?;
            open_wrapped_installer(entry, &name, &inner)
        }
        InstallKind::Pkg | InstallKind::Dmg | InstallKind::Msi | InstallKind::Exe => {
            open_installer(&bytes, entry, &plan.url)
        }
        InstallKind::Page | InstallKind::SeparatorRuntime => {
            unreachable!("the resolver plans a page or a runtime before this match")
        }
    }
}

async fn fetch_bytes(url: &str, entries: &[CatalogEntry]) -> Result<Vec<u8>, String> {
    let client = catalog_download_client(url, entries)?;
    let expected = entries.iter().flat_map(|e| &e.installs).find_map(|i| {
        if i.url == url {
            i.bytes
        } else {
            i.companion_content
                .as_ref()
                .filter(|c| c.url == url)
                .map(|c| c.bytes)
        }
    });
    fetch_resumable(&client, url, expected, MAX_DOWNLOAD_BYTES).await
}

fn catalog_download_client(url: &str, entries: &[CatalogEntry]) -> Result<reqwest::Client, String> {
    if !url_allowed(entries, url) {
        return Err("Refusing to download a URL outside the active plugin catalog".into());
    }
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))
}

fn download_error(error: reqwest::Error) -> String {
    use std::error::Error;
    let error = error.without_url();
    let mut text = error.to_string();
    let mut cause = error.source();
    for _ in 0..4 {
        let Some(next) = cause else {
            break;
        };
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

fn range_start(header: &str) -> Option<(u64, u64)> {
    let (range, total) = header.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    let total: u64 = total.parse().ok()?;
    (start <= end && end < total).then_some((start, total))
}

async fn fetch_resumable(
    client: &reqwest::Client,
    url: &str,
    expected: Option<u64>,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut target = DownloadTarget::Memory(Vec::new());
    fetch_resumable_into(client, url, expected, limit, &mut target).await?;
    let DownloadTarget::Memory(bytes) = target else {
        unreachable!()
    };
    Ok(bytes)
}

enum DownloadTarget {
    Memory(Vec<u8>),
    File {
        file: tokio::fs::File,
        len: u64,
        sha256: Box<Sha256>,
    },
}

impl DownloadTarget {
    fn len(&self) -> u64 {
        match self {
            Self::Memory(bytes) => bytes.len() as u64,
            Self::File { len, .. } => *len,
        }
    }

    async fn reset(&mut self) -> std::io::Result<()> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        match self {
            Self::Memory(bytes) => bytes.clear(),
            Self::File { file, len, sha256 } if *len != 0 => {
                file.flush().await?;
                file.set_len(0).await?;
                file.rewind().await?;
                *len = 0;
                **sha256 = Sha256::new();
            }
            Self::File { .. } => {}
        }
        Ok(())
    }

    async fn append(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::Memory(bytes) => bytes.extend_from_slice(chunk),
            Self::File { file, len, sha256 } => {
                file.write_all(chunk).await?;
                sha256.update(chunk);
                *len += chunk.len() as u64;
            }
        }
        Ok(())
    }
}

async fn fetch_resumable_into(
    client: &reqwest::Client,
    url: &str,
    expected: Option<u64>,
    limit: u64,
    target: &mut DownloadTarget,
) -> Result<(), String> {
    if expected.is_some_and(|n| n > limit) {
        return Err("Catalog archive exceeds download limit".into());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    let label = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut p| p.next_back().map(str::to_owned))
        })
        .unwrap_or_else(|| "archive".into());
    let mut last_error = "incomplete response".to_string();
    let mut attempts = 0;
    for attempt in 1..=4 {
        attempts = attempt;
        if attempt > 1 {
            tokio::time::sleep(Duration::from_millis(250 * (attempt - 1))).await;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            last_error = "15 minute total download limit reached".into();
            break;
        }
        let offset = target.len();
        let mut request = client
            .get(url)
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response =
            tokio::time::timeout(remaining.min(Duration::from_secs(45)), request.send()).await;
        let mut response = match response {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                last_error = download_error(e);
                continue;
            }
            Err(_) => {
                last_error = "connection/headers timed out after 45s".into();
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            last_error = format!("HTTP {status}");
            if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                continue;
            }
            return Err(format!(
                "Download {label} failed at {} bytes: {last_error}",
                target.len()
            ));
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let actual = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(range_start);
            if !actual.is_some_and(|(start, total)| {
                start == offset && total <= limit && expected.is_none_or(|n| n == total)
            }) {
                return Err(format!(
                    "Download {label} returned an invalid Content-Range at byte {offset}"
                ));
            }
        } else {
            target
                .reset()
                .await
                .map_err(|error| format!("Could not reset archive download: {error}"))?;
        }
        if response
            .content_length()
            .is_some_and(|n| n.saturating_add(target.len()) > limit)
        {
            return Err("Download is larger than the plugin store limit".into());
        }
        let mut complete = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                last_error = "15 minute total download limit reached".into();
                break;
            }
            match tokio::time::timeout(remaining.min(Duration::from_secs(45)), response.chunk())
                .await
            {
                Ok(Ok(Some(chunk))) => {
                    if target.len() + chunk.len() as u64 > limit {
                        return Err("Download exceeds install limit".into());
                    }
                    target
                        .append(&chunk)
                        .await
                        .map_err(|error| format!("Could not write archive download: {error}"))?;
                }
                Ok(Ok(None)) => {
                    complete = true;
                    break;
                }
                Ok(Err(e)) => {
                    last_error = download_error(e);
                    break;
                }
                Err(_) => {
                    last_error = "no download data received for 45 seconds".into();
                    break;
                }
            }
        }
        if complete && expected.is_none_or(|n| n == target.len()) {
            return Ok(());
        }
        if complete {
            last_error = format!(
                "incomplete body; expected {} bytes",
                expected.unwrap_or_default()
            );
        }
        log::warn!(
            "[plugin_store] Download {} interrupted at {} bytes (attempt {}/4): {}",
            label,
            target.len(),
            attempt,
            last_error
        );
    }
    Err(format!(
        "Download {label} failed after {attempts} attempts at {}{} bytes: {last_error}. Retry Install; no plugin files were changed.",
        target.len(),
        expected.map(|n| format!("/{n}")).unwrap_or_default()
    ))
}

fn install_zip_bytes(
    bytes: &[u8],
    entry: &CatalogEntry,
    os: HostOs,
    format: BundleFormat,
    overwrite: bool,
    into: Option<&Path>,
) -> Result<InstallOutcome, String> {
    let kind = bundle_kind(format);
    let target = resolve_target(kind, format, into)?;
    if target.needs_admin {
        return Err(format!(
            "Administrator rights are required to install into '{}'. Install manually or run OpenCompose elevated.",
            target.dir.display()
        ));
    }
    let mut outcome = extract_bundles(bytes, format, os, &target.dir, overwrite)?;
    if let InstallOutcome::Installed { path, message } = &mut outcome {
        *message = format!(
            "Installed {} into {}. Rescan the plugin browser to load it.",
            entry.name, path
        );
    }
    Ok(outcome)
}

fn install_tar_bytes(
    bytes: &[u8],
    entry: &CatalogEntry,
    os: HostOs,
    format: BundleFormat,
    overwrite: bool,
    into: Option<&Path>,
) -> Result<InstallOutcome, String> {
    let kind = bundle_kind(format);
    let target = resolve_target(kind, format, into)?;
    if target.needs_admin {
        return Err(format!(
            "Administrator rights are required to install into '{}'. Install manually or run OpenCompose elevated.",
            target.dir.display()
        ));
    }
    let mut outcome = extract_tar_bundles(bytes, format, os, &target.dir, overwrite)?;
    if let InstallOutcome::Installed { path, message } = &mut outcome {
        *message = format!(
            "Installed {} into {}. Rescan the plugin browser to load it.",
            entry.name, path
        );
    }
    Ok(outcome)
}

fn install_appimage_bytes(
    bytes: &[u8],
    entry: &CatalogEntry,
    overwrite: bool,
    into: Option<&Path>,
) -> Result<InstallOutcome, String> {
    let target = resolve_target(InstallKind::Appimage, BundleFormat::Vst3, into)?;
    fs::create_dir_all(&target.dir)
        .map_err(|error| format!("Failed to create '{}': {error}", target.dir.display()))?;
    let file_name = entry
        .id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let path = target.dir.join(format!("{file_name}.AppImage"));
    if path.exists() {
        if !overwrite {
            return Ok(InstallOutcome::NeedsConfirmation {
                path: path.display().to_string(),
                message: format!("{} already exists. Replace it?", path.display()),
            });
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Failed to inspect '{}': {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Refusing to overwrite an unexpected AppImage target".into());
        }
        fs::remove_file(&path)
            .map_err(|error| format!("Failed to replace '{}': {error}", path.display()))?;
    }
    fs::write(&path, bytes)
        .map_err(|error| format!("Failed to write '{}': {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
    }
    Ok(InstallOutcome::Installed {
        path: path.display().to_string(),
        message: format!("Installed {} to {}.", entry.name, path.display()),
    })
}

/// Write the installer an archive wrapped and open it, the same hand-off a bare
/// `.pkg`/`.exe` download gets.
fn open_wrapped_installer(
    _entry: &CatalogEntry,
    name: &str,
    bytes: &[u8],
) -> Result<InstallOutcome, String> {
    let name = sanitize_file_name(name)
        .ok_or_else(|| "Installer name in the archive is unsafe".to_string())?;
    let path = oc_market::temporary::prepare_installer_in(&std::env::temp_dir(), &name, bytes)
        .map_err(|error| format!("Could not stage installer: {error}"))?
        .handoff(|path| open_external(&path.to_string_lossy()))?;
    Ok(InstallOutcome::OpenedInstaller {
        path: path.display().to_string(),
        message: "Archive wrapped a vendor installer; it is now open. Finish the installation in its window.".into(),
    })
}

fn open_installer(
    bytes: &[u8],
    _entry: &CatalogEntry,
    url: &str,
) -> Result<InstallOutcome, String> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("installer");
    let name = sanitize_file_name(name)
        .ok_or_else(|| "Installer URL has an unsafe file name".to_string())?;
    let path = oc_market::temporary::prepare_installer_in(&std::env::temp_dir(), &name, bytes)
        .map_err(|error| format!("Could not stage installer: {error}"))?
        .handoff(|path| open_external(&path.to_string_lossy()))?;
    Ok(InstallOutcome::OpenedInstaller {
        path: path.display().to_string(),
        message: "Installer opened. Finish the installation in the installer window.".into(),
    })
}

fn sanitize_file_name(name: &str) -> Option<String> {
    let name = name.split(['?', '#']).next().unwrap_or(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|character| character.is_control())
    {
        return None;
    }
    Some(name.to_string())
}

// ============================================================================
// Agent search / pull
// ============================================================================

#[derive(Debug, Serialize)]
pub struct PluginSearchInstall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companion_content: Option<plugin_content::CompanionContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<separator_runtime::RuntimeDescriptor>,
    pub kind: InstallSource,
    pub platform: String,
    pub format: InstallFormat,
    pub archive: Option<InstallArchive>,
    pub url: String,
    pub bytes: Option<u64>,
    /// The pinned hash, or `null` when the publisher has not pinned the build.
    pub sha256: Option<String>,
}

/// Why `plugin_search` cannot promise an install, in the shared vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSearchRefusal {
    /// Stable machine code: `no_direct_archive`, `unpinned`, `no_build`, ...
    pub code: &'static str,
    pub message: String,
    /// The page a person can open by hand, when the refusal has one.
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginSearchEntry {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub tags: Vec<String>,
    pub installed: bool,
    /// True only when a pinned build can be installed on this platform.
    pub installable: bool,
    /// `extract` installs headlessly; `installer` opens a vendor installer.
    pub mode: Option<&'static str>,
    /// Present exactly when `installable` is false.
    pub refusal: Option<PluginSearchRefusal>,
    pub installs: Vec<PluginSearchInstall>,
}

#[derive(Debug, Serialize)]
pub struct PluginSearchResult {
    pub entries: Vec<PluginSearchEntry>,
    pub total: usize,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct PluginPullResult {
    pub id: String,
    pub name: String,
    pub dry_run: bool,
    pub kind: &'static str,
    /// `extract` (headless) or `installer` (hands off to a vendor installer).
    pub mode: &'static str,
    pub platform: String,
    pub format: InstallFormat,
    pub archive: Option<InstallArchive>,
    pub url: String,
    /// The pinned hash the download is checked against.
    pub sha256: String,
    pub target: Option<String>,
    pub needs_admin: bool,
    pub installed: bool,
    pub opened: bool,
    pub path: Option<String>,
    pub note: Option<String>,
    pub verified: bool,
}

fn platform_os(platform: &str) -> Result<HostOs, String> {
    match platform {
        "macos-aarch64" | "macos-x86_64" => Ok(HostOs::Macos),
        "windows-x86_64" => Ok(HostOs::Windows),
        "linux-x86_64" => Ok(HostOs::Linux),
        other => Err(format!(
            "Unsupported platform '{other}'; expected macos-aarch64, macos-x86_64, windows-x86_64, or linux-x86_64"
        )),
    }
}

fn parse_install_format(format: &str) -> Result<InstallFormat, String> {
    match format {
        "vst3" => Ok(InstallFormat::Vst3),
        "vst2" => Ok(InstallFormat::Vst2),
        "clap" => Ok(InstallFormat::Clap),
        "au" => Ok(InstallFormat::Au),
        "lv2" => Ok(InstallFormat::Lv2),
        "standalone" => Ok(InstallFormat::Standalone),
        other => Err(format!(
            "Unsupported format '{other}'; expected vst3, vst2, clap, au, lv2, or standalone"
        )),
    }
}

fn normalize_plugin_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn entry_installed(entry: &CatalogEntry, installed: &[InstalledPlugin]) -> bool {
    installed_match(entry, installed).is_some()
}

struct SearchFilters<'a> {
    query: Option<&'a str>,
    tags: Option<&'a [String]>,
    platform: &'a str,
    format: Option<InstallFormat>,
    installed_only: bool,
    limit: Option<usize>,
    source: &'a str,
}

fn search_entries(
    entries: &[CatalogEntry],
    filters: &SearchFilters<'_>,
    installed: &[InstalledPlugin],
) -> Result<PluginSearchResult, String> {
    platform_os(filters.platform)?;
    let needle = filters
        .query
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut matches = Vec::new();
    for entry in entries {
        let installs: Vec<_> = entry
            .installs
            .iter()
            .filter(|install| {
                install.platform == filters.platform
                    && filters.format.is_none_or(|value| install.format == value)
            })
            .collect();
        if installs.is_empty() {
            continue;
        }
        if filters.installed_only && !entry_installed(entry, installed) {
            continue;
        }
        if let Some(tags) = filters.tags
            && !tags.iter().all(|requested| {
                entry
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case(requested))
            })
        {
            continue;
        }
        if !needle.is_empty() {
            let haystack = [
                entry.name.as_str(),
                entry.vendor.as_str(),
                entry.description.as_deref().unwrap_or_default(),
                &entry.tags.join(" "),
            ]
            .join(" ")
            .to_ascii_lowercase();
            if !haystack.contains(&needle) {
                continue;
            }
        }
        // One resolver decides installability, so the search result can never
        // promise an install that `plugin_pull` would refuse.
        let status = oc_market::status(
            &entry.id,
            &entry.name,
            &candidates(entry),
            filters.platform,
            filters.format,
        );
        matches.push(PluginSearchEntry {
            id: entry.id.clone(),
            name: entry.name.clone(),
            vendor: entry.vendor.clone(),
            description: entry.description.clone(),
            homepage: entry.homepage.clone(),
            tags: entry.tags.clone(),
            installed: entry_installed(entry, installed),
            installable: status.installable,
            mode: status.mode.map(|mode| mode.as_str()),
            refusal: status.refusal.map(|refusal| PluginSearchRefusal {
                code: refusal.code(),
                message: refusal.to_string(),
                url: refusal.url().map(str::to_string),
            }),
            installs: installs
                .into_iter()
                .map(|install| PluginSearchInstall {
                    companion_content: install.companion_content.clone(),
                    runtime: install.runtime.clone(),
                    kind: install.kind,
                    platform: install.platform.to_string(),
                    format: install.format,
                    archive: install.archive,
                    url: install.url.clone(),
                    bytes: install.bytes,
                    sha256: install.sha256.clone(),
                })
                .collect(),
        });
    }
    let total = matches.len();
    let limit = filters.limit.unwrap_or(50).clamp(1, 100);
    matches.truncate(limit);
    Ok(PluginSearchResult {
        entries: matches,
        total,
        source: filters.source.to_string(),
    })
}

/// Read-only marketplace search used by MCP and the built-in AI panel.
#[allow(clippy::too_many_arguments)]
pub fn search(
    entries: &[CatalogEntry],
    source: &str,
    query: Option<&str>,
    tags: Option<&[String]>,
    platform: Option<&str>,
    format: Option<&str>,
    installed_only: bool,
    limit: Option<usize>,
) -> Result<PluginSearchResult, String> {
    let platform = platform.map(str::to_string).unwrap_or_else(host_platform);
    let format = format.map(parse_install_format).transpose()?;
    search_entries(
        entries,
        &SearchFilters {
            query,
            tags,
            platform: &platform,
            format,
            installed_only,
            limit,
            source,
        },
        &crate::plugin_inventory::installed(),
    )
}

fn planned_target(
    platform: &str,
    kind: InstallKind,
    format: InstallFormat,
) -> Option<(PathBuf, bool)> {
    let os = platform_os(platform).ok()?;
    if kind == InstallKind::Page {
        return None;
    }
    if matches!(
        kind,
        InstallKind::Pkg | InstallKind::Msi | InstallKind::Dmg | InstallKind::Exe
    ) {
        return None;
    }
    let bundle_format = match format {
        InstallFormat::Vst3 => BundleFormat::Vst3,
        InstallFormat::Vst2 => BundleFormat::Vst2,
        InstallFormat::Clap => BundleFormat::Clap,
        InstallFormat::Lv2 => BundleFormat::Lv2,
        InstallFormat::Au | InstallFormat::Standalone => return None,
    };
    let dirs = HostDirs::from_env();
    let home = dirs.home.clone();
    let local = dirs.local_app_data.clone();
    let writable = move |path: &Path| {
        home.as_ref().is_some_and(|value| path.starts_with(value))
            || local.as_ref().is_some_and(|value| path.starts_with(value))
    };
    select_install_target(os, kind, bundle_format, &dirs, writable)
        .ok()
        .map(|target| (target.dir, target.needs_admin))
}

/// Resolve (and with `dry_run: false` execute) a catalog install plan.
///
/// The plan comes from the shared resolver in `oc-market`, so a build this
/// tool installs is the build the CLI installs, and a refusal names the entry
/// and the reason in the same words.
pub async fn pull(
    authority: &CatalogAuthority,
    entries: &[CatalogEntry],
    id: &str,
    platform: Option<&str>,
    format: Option<&str>,
    dry_run: bool,
    into: Option<&Path>,
) -> Result<PluginPullResult, String> {
    authority.check()?;
    let entry = entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| Refusal::UnknownId { id: id.to_string() }.to_string())?;
    let platform = platform.map(str::to_string).unwrap_or_else(host_platform);
    platform_os(&platform)?;
    let format = format.map(parse_install_format).transpose()?;
    let Resolved { plan, install } =
        plan_for(entry, &platform, format).map_err(|refusal| refusal.to_string())?;
    let (target, needs_admin) = match into {
        Some(dir) => (Some(dir.display().to_string()), false),
        None => planned_target(&platform, plan.kind, plan.format)
            .map(|(dir, admin)| (Some(dir.display().to_string()), admin))
            .unwrap_or((
                None,
                matches!(plan.kind, InstallKind::Pkg | InstallKind::Msi),
            )),
    };
    let mut result = PluginPullResult {
        id: entry.id.clone(),
        name: entry.name.clone(),
        dry_run,
        kind: plan.kind.as_str(),
        mode: plan.mode.as_str(),
        platform,
        format: plan.format,
        archive: plan.archive,
        url: plan.url.clone(),
        sha256: plan.sha256.clone(),
        target,
        needs_admin,
        installed: false,
        opened: false,
        path: None,
        note: None,
        verified: true,
    };
    if dry_run {
        return Ok(result);
    }
    if result.platform != host_platform() {
        return Err(Refusal::PlatformMismatch {
            platform: result.platform.clone(),
        }
        .to_string());
    }
    match plan.kind {
        InstallKind::SeparatorRuntime => {
            apply_outcome(
                &mut result,
                separator_runtime::install(authority, entries, entry, install).await?,
            );
        }
        InstallKind::ZipVst3
        | InstallKind::ZipVst2
        | InstallKind::ZipClap
        | InstallKind::ZipLv2 => {
            let bytes = download_verified(install, entries).await?;
            if install.companion_content.is_some() {
                apply_outcome(
                    &mut result,
                    plugin_content::install(
                        authority, entries, entry, install, &bytes, false, into,
                    )
                    .await?,
                );
                return Ok(result);
            }
            let _publication = authority.publication_guard()?;
            let bundle_format = bundle_format(plan.kind)
                .ok_or_else(|| format!("{} has no supported bundle archive", entry.name))?;
            let outcome = if plan.archive == Some(InstallArchive::TarGz) {
                install_tar_bytes(&bytes, entry, host_os(), bundle_format, false, into)?
            } else {
                install_zip_bytes(&bytes, entry, host_os(), bundle_format, false, into)?
            };
            let outcome = match outcome {
                InstallOutcome::WrappedInstaller { name, bytes } => {
                    open_wrapped_installer(entry, &name, &bytes)?
                }
                other => other,
            };
            apply_outcome(&mut result, outcome);
        }
        InstallKind::Appimage => {
            let bytes = download_verified(install, entries).await?;
            let _publication = authority.publication_guard()?;
            apply_outcome(
                &mut result,
                install_appimage_bytes(&bytes, entry, false, into)?,
            );
        }
        InstallKind::InstallerArchive => {
            let bytes = download_verified(install, entries).await?;
            let _publication = authority.publication_guard()?;
            let (name, inner) = oc_market::install::unwrap_installer(&bytes, plan.archive)
                .map_err(|error| format!("{}: {error}", entry.name))?;
            apply_outcome(&mut result, open_wrapped_installer(entry, &name, &inner)?);
        }
        InstallKind::Pkg | InstallKind::Dmg | InstallKind::Msi | InstallKind::Exe => {
            let bytes = download_verified(install, entries).await?;
            let _publication = authority.publication_guard()?;
            match open_installer(&bytes, entry, &plan.url)? {
                InstallOutcome::OpenedInstaller { path, message } => {
                    result.opened = true;
                    result.path = Some(path);
                    result.note = Some(message);
                }
                _ => return Err("Installer did not open".into()),
            }
        }
        InstallKind::Page => unreachable!("the resolver never plans a page"),
    }
    Ok(result)
}

fn apply_outcome(result: &mut PluginPullResult, outcome: InstallOutcome) {
    match outcome {
        InstallOutcome::Installed { path, message } => {
            result.installed = true;
            result.path = Some(path);
            result.note = Some(message);
        }
        InstallOutcome::NeedsConfirmation { path, .. } => {
            result.path = Some(path);
            result.note =
                Some("Bundle already exists; replace it explicitly in the Store UI".into());
        }
        InstallOutcome::OpenedInstaller { path, message } => {
            result.opened = true;
            result.path = Some(path);
            result.note = Some(message);
        }
        InstallOutcome::WrappedInstaller { .. } => {
            result.note = Some("The archive wraps a vendor installer.".into());
        }
    }
}

/// Download one artifact and verify it against the catalog's pinned sha256.
/// A build the catalog does not pin is refused before anything is unpacked.
async fn download_verified(
    install: &CatalogInstall,
    entries: &[CatalogEntry],
) -> Result<Vec<u8>, String> {
    let Some(pinned) = install.sha256.as_deref() else {
        return Err(Refusal::Unpinned {
            name: install.url.clone(),
            url: install.url.clone(),
            bytes: install.bytes,
        }
        .to_string());
    };
    let bytes = fetch_bytes(&install.url, entries).await?;
    oc_market::verify_artifact(&bytes, pinned, install.bytes)
        .map_err(|mismatch| mismatch.to_string())?;
    Ok(bytes)
}

/// Ids the operator asked to stop while their download or install ran.
fn cancel_registry() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static CANCELLED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    CANCELLED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Ask a running install to stop at its next checkpoint.
pub fn request_cancel(id: &str) {
    cancel_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(id.to_string());
}

/// Forget a previous request, so a retry starts clean.
pub fn clear_cancel(id: &str) {
    cancel_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(id);
}

/// Whether this id asked to stop.
pub fn cancelled(id: &str) -> bool {
    cancel_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains(id)
}

/// Stream one URL into `part`, hashing as it goes. The caller owns the file's
/// lifetime: on any `Err` the partial file is still on disk for the caller to
/// remove (instrument packs use a drop guard).
///
/// `key` is the install id the operator can cancel: a cancelled download stops
/// at the next chunk and reports it as such, so the panel can say so.
pub async fn download_to_part(
    url: &str,
    part: &Path,
    key: &str,
    mut progress: impl FnMut(u64, u64),
) -> Result<(u64, String), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Download failed with HTTP {} from {url}",
            response.status()
        ));
    }
    let total = response.content_length().unwrap_or(0);
    if total > MAX_DOWNLOAD_BYTES {
        return Err("Download is larger than the plugin store limit".into());
    }
    let mut file = fs::File::create(part)
        .map_err(|error| format!("Failed to create '{}': {error}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed to read download: {error}"))?
    {
        if cancelled(key) {
            let _ = fs::remove_file(part);
            return Err(format!("{key} was cancelled"));
        }
        received += chunk.len() as u64;
        if received > MAX_DOWNLOAD_BYTES {
            return Err("Download is larger than the plugin store limit".into());
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|error| format!("Failed to write '{}': {error}", part.display()))?;
        progress(received, total);
    }
    file.sync_all()
        .map_err(|error| format!("Failed to flush '{}': {error}", part.display()))?;
    Ok((received, hex(&hasher.finalize())))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use oc_market::catalog_wire::SOUNDFONT_PACK_KIND;
    fn parse_remote_catalog_checked(
        bytes: &[u8],
    ) -> Result<Catalog, oc_market::catalog_wire::CatalogError> {
        oc_market::catalog_wire::parse(bytes).map(from_shared_catalog)
    }

    /// Parse the shared wire document; computed host status is added separately.
    fn parse_remote_catalog(bytes: &[u8]) -> Result<Catalog, String> {
        parse_remote_catalog_checked(bytes).map_err(|error| error.to_string())
    }

    use oc_market::install::sanitize_zip_path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-plugin-store-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn catalog() -> Vec<CatalogEntry> {
        bundled_catalog().expect("bundled catalog must parse").0
    }

    fn host_dirs() -> HostDirs {
        HostDirs {
            home: Some(PathBuf::from("/Users/test")),
            common_program_files: Some(PathBuf::from("C:/Program Files/Common Files")),
            local_app_data: Some(PathBuf::from("C:/Users/test/AppData/Local")),
        }
    }

    fn build_stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut directory = Vec::new();
        for (name, contents) in files {
            let offset = data.len() as u32;
            data.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            data.extend_from_slice(&20u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&(contents.len() as u32).to_le_bytes());
            data.extend_from_slice(&(contents.len() as u32).to_le_bytes());
            data.extend_from_slice(&(name.len() as u16).to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(name.as_bytes());
            data.extend_from_slice(contents);

            directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u32.to_le_bytes());
            directory.extend_from_slice(&(contents.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(contents.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u32.to_le_bytes());
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let directory_offset = data.len() as u32;
        let directory_size = directory.len() as u32;
        data.extend_from_slice(&directory);
        data.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&directory_size.to_le_bytes());
        data.extend_from_slice(&directory_offset.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data
    }

    // ---- Catalog schema ----

    #[test]
    fn catalog_entries_have_required_fields_and_unique_ids() {
        let entries = catalog();
        assert!(
            (25..=48).contains(&entries.len()),
            "catalog should contain 25-48 entries, found {}",
            entries.len()
        );
        let categories = [
            "synth",
            "sampler",
            "eq",
            "compressor",
            "reverb",
            "delay",
            "saturation",
            "metering",
            "drums",
            "vocal",
            "utility",
            "freeware",
            "freemium",
            "open-source",
        ];
        let mut ids = HashSet::new();
        for entry in &entries {
            assert!(
                ids.insert(entry.id.clone()),
                "duplicate catalog id {}",
                entry.id
            );
            assert!(is_valid_id(&entry.id), "invalid id {}", entry.id);
            assert!(!entry.name.is_empty(), "{} has no name", entry.id);
            assert!(!entry.vendor.is_empty(), "{} has no vendor", entry.id);
            assert!(!entry.installs.is_empty(), "{} has no installs", entry.id);
            assert!(
                entry
                    .license
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "{} has no license",
                entry.id
            );
            assert!(!entry.tags.is_empty(), "{} has no tags", entry.id);
            for tag in &entry.tags {
                assert!(
                    !tag.is_empty() && tag.len() <= 64,
                    "{} has a bad tag",
                    entry.id
                );
                assert!(
                    categories.contains(&tag.as_str()),
                    "{} has unknown tag {}",
                    entry.id,
                    tag
                );
            }
            if let Some(description) = &entry.description {
                assert!(
                    description
                        .chars()
                        .last()
                        .is_some_and(|character| matches!(character, '.' | '!' | '?')),
                    "{} description must be one sentence",
                    entry.id
                );
            }
            if let Some(homepage) = &entry.homepage {
                assert!(
                    is_https_url(homepage),
                    "{} homepage must be https",
                    entry.id
                );
            }
            let mut seen = HashSet::new();
            for install in &entry.installs {
                assert!(
                    is_https_url(&install.url),
                    "{} has a non-https URL",
                    entry.id
                );
                assert!(
                    seen.insert((install.kind, install.platform, install.format)),
                    "{} has duplicate install ({:?}, {}, {:?})",
                    entry.id,
                    install.kind,
                    install.platform,
                    install.format
                );
                if let Some(sha256) = &install.sha256 {
                    assert!(is_lowercase_sha256(sha256), "{} has a bad sha256", entry.id);
                }
                if let Some(bytes) = install.bytes {
                    assert!(bytes > 0, "{} has zero bytes", entry.id);
                }
            }
        }
    }

    #[test]
    fn catalog_has_every_required_plugin() {
        let entries = catalog();
        let ids: HashSet<_> = entries.iter().map(|entry| entry.id.as_str()).collect();
        for required in [
            "vital-audio.vital",
            "surge-synth-team.surge-xt",
            "airwindows.consolidated",
            "sfztools.sfizz",
            "decent-samples.decent-sampler",
            "spitfire-audio.labs",
            "valhalla-dsp.supermassive",
            "tal-software.tal-reverb-4",
            "tal-software.tal-chorus-lx",
            "tokyo-dawn.tdr-nova",
            "tokyo-dawn.tdr-kotelnikov",
            "youlean.loudness-meter-2",
            "voxengo.span",
            "melda.mfreefxbundle",
            "auburn-sounds.graillon",
            "melda.mautopitch",
            "tal-software.tal-vocoder",
            "distrho.cardinal",
            "chowdsp.chowtape-model",
            "asb2m10.dexed",
            "matt-tytel.helm",
            "discodsp.ob-xd",
            "u-he.zebralette",
            "u-he.podolski",
            "u-he.tyrelln6",
            "surge-synth-team.surge-xt-effects",
            "kilohearts.essentials",
            "analog-obsession.analog-obsession",
            "sonimus.soneq",
            "u-he.triple-cheese",
        ] {
            assert!(
                ids.contains(required),
                "missing required catalog entry {required}"
            );
        }
    }

    // ---- URL allowlist ----

    #[test]
    fn url_allowlist_accepts_catalog_urls_and_rejects_others() {
        let entries = catalog();
        let dexed = entries
            .iter()
            .find(|entry| entry.id == "asb2m10.dexed")
            .expect("dexed entry");
        assert!(url_allowed(&entries, dexed.homepage.as_deref().unwrap()));
        for install in &dexed.installs {
            assert!(url_allowed(&entries, &install.url));
        }
        assert!(!url_allowed(&entries, "https://example.com/dexed.zip"));
        assert!(!url_allowed(&entries, "http://github.com/asb2m10/dexed"));
        assert!(!url_allowed(&entries, "file:///etc/passwd"));
        assert!(!url_allowed(&entries, "javascript:alert(1)"));
        assert!(!url_allowed(&entries, ""));
        assert!(!url_allowed(
            &entries,
            "https://github.com/asb2m10/dexed/releases/download/v1.0.1/../../evil.zip"
        ));
    }

    // ---- Host install selection ----

    // The bundled catalog pins archives for Apple silicon only, and the Store
    // response always answers for the host platform.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn the_store_response_reports_installability() {
        let response = catalog_response(&catalog());
        let dexed = response
            .entries
            .iter()
            .find(|entry| entry.id == "asb2m10.dexed")
            .expect("dexed");
        assert!(dexed.installable, "dexed pins every artifact");
        assert_eq!(dexed.install_mode, Some("extract"));
        assert!(dexed.refusal.is_none());

        let podolski = response
            .entries
            .iter()
            .find(|entry| entry.id == "u-he.podolski")
            .expect("podolski");
        // u-he ships a pinned pkg, so the entry is installable but hands off to
        // the vendor installer.
        assert!(podolski.installable);
        assert_eq!(podolski.install_mode, Some("installer"));

        let page = response
            .entries
            .iter()
            .find(|entry| entry.id == "kilohearts.essentials")
            .expect("kilohearts");
        assert!(!page.installable);
        assert_eq!(page.refusal.as_ref().unwrap().code, "no_direct_archive");

        let missing = response
            .entries
            .iter()
            .find(|entry| entry.id == "lsp-plugins.lsp-plugins")
            .expect("lsp");
        assert!(!missing.installable);
        let refusal = missing.refusal.as_ref().expect("a refusal");
        assert_eq!(refusal.code, "no_build");
        assert!(
            refusal.message.contains(&response.platform),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn the_resolver_prefers_direct_builds_and_current_arch() {
        let entries = catalog();
        let dexed = entries
            .iter()
            .find(|entry| entry.id == "asb2m10.dexed")
            .expect("dexed entry");
        let dexed_installs = candidates(dexed);
        let arm = oc_market::choose(&dexed_installs, "macos-aarch64", None).expect("arm install");
        assert_eq!(arm.source, InstallSource::GithubRelease);
        assert_eq!(arm.format, InstallFormat::Vst3);
        assert_eq!(arm.archive, Some(InstallArchive::Zip));
        assert!(arm.url.contains("macOS"));
        let intel = oc_market::choose(&dexed_installs, "macos-x86_64", None).expect("intel");
        assert_eq!(intel.url, arm.url);
        let windows = oc_market::choose(&dexed_installs, "windows-x86_64", None).expect("windows");
        assert_eq!(windows.format, InstallFormat::Vst3);
        assert_eq!(windows.archive, Some(InstallArchive::Zip));
        // Dexed pins every artifact, so the resolver plans a verified install.
        let Resolved { plan, .. } = plan_for(dexed, "macos-aarch64", None).expect("pinned plan");
        assert_eq!(plan.kind, InstallKind::ZipVst3);
        assert_eq!(plan.mode, oc_market::Mode::Extract);
        assert_eq!(plan.sha256.len(), 64);
        // A page-only build is still refused, and the page stays visible.
        let page = entries
            .iter()
            .find(|entry| entry.id == "u-he.zebralette")
            .expect("page entry");
        let refusal = plan_for(page, "macos-aarch64", None).unwrap_err();
        assert_eq!(refusal.code(), "no_direct_archive");
    }

    #[test]
    fn installer_archives_hand_off_instead_of_extracting() {
        let entries = catalog();
        let cardinal = entries
            .iter()
            .find(|entry| entry.id == "distrho.cardinal")
            .expect("cardinal entry");
        let mac = oc_market::install_kind(
            InstallSource::GithubRelease,
            Some(InstallArchive::Pkg),
            "https://github.com/x/y/releases/download/1/a.pkg",
            InstallFormat::Vst3,
            false,
            false,
        );
        assert_eq!(mac, Some(InstallKind::Pkg));
        let windows = oc_market::install_kind(
            InstallSource::GithubRelease,
            Some(InstallArchive::Installer),
            "https://github.com/x/y/releases/download/1/a.exe",
            InstallFormat::Vst3,
            false,
            false,
        );
        assert_eq!(windows, Some(InstallKind::Exe));
        assert!(entry_needs_admin(cardinal, "macos-aarch64"));
        let page = entries
            .iter()
            .find(|entry| entry.id == "u-he.zebralette")
            .expect("page entry");
        assert!(plan_for(page, "macos-aarch64", None).is_err());
        assert!(!entry_needs_admin(page, "macos-aarch64"));
    }

    // ---- Target selection ----

    #[test]
    fn macos_target_prefers_user_then_system() {
        let dirs = host_dirs();
        let user = PathBuf::from("/Users/test/Library/Audio/Plug-Ins/VST3");
        let target = select_install_target(
            HostOs::Macos,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |path| path == user,
        )
        .unwrap();
        assert_eq!(target.dir, user);
        assert!(!target.needs_admin);

        let system = PathBuf::from("/Library/Audio/Plug-Ins/VST3");
        let target = select_install_target(
            HostOs::Macos,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |_| false,
        )
        .unwrap();
        assert_eq!(target.dir, system);
        assert!(target.needs_admin);

        let clap = PathBuf::from("/Users/test/Library/Audio/Plug-Ins/CLAP");
        let target = select_install_target(
            HostOs::Macos,
            InstallKind::ZipClap,
            BundleFormat::Clap,
            &dirs,
            |path| path == clap,
        )
        .unwrap();
        assert_eq!(target.dir, clap);
        assert!(!target.needs_admin);
    }

    #[test]
    fn windows_target_prefers_common_then_local() {
        let dirs = host_dirs();
        let common = PathBuf::from("C:/Program Files/Common Files/VST3");
        let target = select_install_target(
            HostOs::Windows,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |path| path == common,
        )
        .unwrap();
        assert_eq!(target.dir, common);
        assert!(!target.needs_admin);

        let local = PathBuf::from("C:/Users/test/AppData/Local/Programs/Common/VST3");
        let target = select_install_target(
            HostOs::Windows,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |path| path == local,
        )
        .unwrap();
        assert_eq!(target.dir, local);
        assert!(!target.needs_admin);

        let target = select_install_target(
            HostOs::Windows,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |_| false,
        )
        .unwrap();
        assert_eq!(target.dir, common);
        assert!(target.needs_admin);
    }

    #[test]
    fn linux_target_prefers_user_then_system() {
        let dirs = host_dirs();
        let user = PathBuf::from("/Users/test/.vst3");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |path| path == user,
        )
        .unwrap();
        assert_eq!(target.dir, user);
        assert!(!target.needs_admin);

        let system = PathBuf::from("/usr/lib/vst3");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::ZipVst3,
            BundleFormat::Vst3,
            &dirs,
            |_| false,
        )
        .unwrap();
        assert_eq!(target.dir, system);
        assert!(target.needs_admin);

        let clap_system = PathBuf::from("/usr/lib/clap");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::ZipClap,
            BundleFormat::Clap,
            &dirs,
            |_| false,
        )
        .unwrap();
        assert_eq!(target.dir, clap_system);
        assert!(target.needs_admin);

        let appimage = PathBuf::from("/Users/test/.local/bin");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::Appimage,
            BundleFormat::Vst3,
            &dirs,
            |_| false,
        )
        .unwrap();
        assert_eq!(target.dir, appimage);
        assert!(!target.needs_admin);
        assert!(
            select_install_target(
                HostOs::Macos,
                InstallKind::Appimage,
                BundleFormat::Vst3,
                &dirs,
                |_| true
            )
            .is_err()
        );
    }

    #[test]
    fn vst2_and_lv2_targets_use_platform_directories() {
        let dirs = host_dirs();
        let mac_vst = PathBuf::from("/Users/test/Library/Audio/Plug-Ins/VST");
        let target = select_install_target(
            HostOs::Macos,
            InstallKind::ZipVst2,
            BundleFormat::Vst2,
            &dirs,
            |path| path == mac_vst,
        )
        .unwrap();
        assert_eq!(target.dir, mac_vst);
        assert!(!target.needs_admin);

        let mac_lv2 = PathBuf::from("/Users/test/Library/Audio/Plug-Ins/LV2");
        let target = select_install_target(
            HostOs::Macos,
            InstallKind::ZipLv2,
            BundleFormat::Lv2,
            &dirs,
            |path| path == mac_lv2,
        )
        .unwrap();
        assert_eq!(target.dir, mac_lv2);

        let linux_vst = PathBuf::from("/Users/test/.vst");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::ZipVst2,
            BundleFormat::Vst2,
            &dirs,
            |path| path == linux_vst,
        )
        .unwrap();
        assert_eq!(target.dir, linux_vst);

        let linux_lv2 = PathBuf::from("/Users/test/.lv2");
        let target = select_install_target(
            HostOs::Linux,
            InstallKind::ZipLv2,
            BundleFormat::Lv2,
            &dirs,
            |path| path == linux_lv2,
        )
        .unwrap();
        assert_eq!(target.dir, linux_lv2);

        let windows_vst2 = PathBuf::from("C:/Program Files/Common Files/VST2");
        let target = select_install_target(
            HostOs::Windows,
            InstallKind::ZipVst2,
            BundleFormat::Vst2,
            &dirs,
            |path| path == windows_vst2,
        )
        .unwrap();
        assert_eq!(target.dir, windows_vst2);
        assert!(!target.needs_admin);
    }

    // ---- Zip sanitization / extraction ----

    #[test]
    fn zip_paths_reject_traversal_and_absolute_paths() {
        assert!(sanitize_zip_path("Dexed.vst3/Contents/Dexed").is_ok());
        assert!(sanitize_zip_path("../evil.vst3/payload").is_err());
        assert!(sanitize_zip_path("Dexed.vst3/../../evil").is_err());
        assert!(sanitize_zip_path("/etc/passwd").is_err());
        assert!(sanitize_zip_path("C:\\Windows\\evil.vst3").is_err());
        assert!(sanitize_zip_path("").is_err());
        assert!(sanitize_zip_path("ok/").is_ok());
    }

    #[test]
    fn zip_extraction_installs_only_the_requested_bundle() {
        let target = TempDir::new("extract");
        let archive = build_stored_zip(&[
            ("Dexed.vst3/Contents/x86_64-win/Dexed.vst3", b"binary"),
            ("Dexed.vst3/Contents/Resources/moduleinfo.json", b"{}"),
            ("LICENSE", b"gpl"),
        ]);
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(
            target
                .0
                .join("Dexed.vst3/Contents/x86_64-win/Dexed.vst3")
                .is_file()
        );
        assert!(!target.0.join("LICENSE").exists());
    }

    #[test]
    fn zip_extraction_strips_wrapper_directories() {
        let target = TempDir::new("wrapped");
        let archive = build_stored_zip(&[(
            "awcons-products/Airwindows Consolidated.vst3/Contents/x86_64-linux/plugin.so",
            b"binary",
        )]);
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(
            target
                .0
                .join("Airwindows Consolidated.vst3/Contents/x86_64-linux/plugin.so")
                .is_file()
        );
        assert!(!target.0.join("awcons-products").exists());
    }

    #[test]
    fn zip_extraction_rejects_traversal() {
        let target = TempDir::new("traversal");
        let archive = build_stored_zip(&[("../evil.vst3/payload.so", b"x")]);
        assert!(
            extract_bundles(
                &archive,
                BundleFormat::Vst3,
                HostOs::Macos,
                &target.0,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn zip_extraction_extracts_clap_files() {
        let target = TempDir::new("clap");
        let archive = build_stored_zip(&[("Dexed.clap", b"binary")]);
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Clap,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("Dexed.clap").is_file());
    }

    #[test]
    fn zip_extraction_installs_vst2_and_lv2_bundles() {
        let target = TempDir::new("vst2lv2");
        let archive = build_stored_zip(&[
            ("Obxd.vst/Contents/MacOS/Obxd", b"binary"),
            ("Calf.lv2/manifest.ttl", b"@prefix lv2:"),
        ]);
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst2,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("Obxd.vst/Contents/MacOS/Obxd").is_file());
        assert!(!target.0.join("Calf.lv2").exists());

        let outcome =
            extract_bundles(&archive, BundleFormat::Lv2, HostOs::Macos, &target.0, false).unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("Calf.lv2/manifest.ttl").is_file());

        let windows = build_stored_zip(&[("Obxd.dll", b"binary")]);
        let outcome = extract_bundles(
            &windows,
            BundleFormat::Vst2,
            HostOs::Windows,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("Obxd.dll").is_file());
    }

    #[test]
    fn vst2_extraction_ignores_libraries_inside_other_bundles() {
        let target = TempDir::new("vst2-nested");
        let archive = build_stored_zip(&[
            (
                "dragonfly-reverb-3.2.10/DragonflyHallReverb-vst.dll",
                b"vst2",
            ),
            (
                "dragonfly-reverb-3.2.10/DragonflyHallReverb.lv2/DragonflyHallReverb_dsp.dll",
                b"lv2",
            ),
        ]);
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst2,
            HostOs::Windows,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("DragonflyHallReverb-vst.dll").is_file());
        assert!(!target.0.join("DragonflyHallReverb_dsp.dll").exists());
    }

    #[test]
    fn zip_extraction_requires_confirmation_before_overwrite() {
        let target = TempDir::new("overwrite");
        let archive = build_stored_zip(&[("Dexed.vst3/Contents/Dexed", b"one")]);
        assert!(matches!(
            extract_bundles(
                &archive,
                BundleFormat::Vst3,
                HostOs::Macos,
                &target.0,
                false
            )
            .unwrap(),
            InstallOutcome::Installed { .. }
        ));
        assert!(matches!(
            extract_bundles(
                &archive,
                BundleFormat::Vst3,
                HostOs::Macos,
                &target.0,
                false
            )
            .unwrap(),
            InstallOutcome::NeedsConfirmation { .. }
        ));
        assert!(matches!(
            extract_bundles(&archive, BundleFormat::Vst3, HostOs::Macos, &target.0, true).unwrap(),
            InstallOutcome::Installed { .. }
        ));
    }

    // ---- Installed detection ----

    #[test]
    fn installed_scan_matches_bundle_names_and_degrades_when_empty() {
        let target = TempDir::new("installed");
        fs::create_dir(target.0.join("Dexed.vst3")).unwrap();
        fs::write(target.0.join("Surge XT.clap"), b"").unwrap();
        fs::write(target.0.join("Dexed.component"), b"").unwrap();
        let found = scan_plugin_bundles(&[target.0.clone(), target.0.join("missing")]);
        let names: HashSet<_> = found.iter().map(|plugin| plugin.name.as_str()).collect();
        assert!(names.contains("Dexed"));
        assert!(names.contains("Surge XT"));
        let formats: HashSet<_> = found
            .iter()
            .filter(|plugin| plugin.name == "Dexed")
            .map(|plugin| plugin.format.as_str())
            .collect();
        assert_eq!(formats, HashSet::from(["vst3", "au"]));
        assert!(scan_plugin_bundles(&[target.0.join("missing")]).is_empty());
    }

    #[test]
    fn receipts_are_optional_and_name_matched() {
        let target = TempDir::new("receipts");
        let receipt = serde_json::json!([{
            "schema": "opencompose.plugin_scan_receipt.v1",
            "path": "/plugins/Halcyon2.vst3",
            "format": "Vst3",
            "status": "passed",
            "scanned_at_unix": 1,
            "duration_ms": 1,
            "exit_code": 0,
            "error": null,
            "plugins": [{
                "id": "vst3:halcyon_2",
                "name": "Halcyon 2",
                "vendor": "Xfer",
                "format": "Vst3",
                "path": "/plugins/Halcyon2.vst3",
                "category": "Instrument",
            }]
        }]);
        let path = target.0.join("plugin-scan-receipts.json");
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let found = scan_receipts(&path);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Halcyon 2");
        assert_eq!(found[0].format, "vst3");
        assert!(scan_receipts(&target.0.join("missing.json")).is_empty());
    }

    #[test]
    fn search_filters_query_tags_platform_and_installed() {
        let entries = catalog();
        let installed = vec![InstalledPlugin {
            name: "Dexed".into(),
            format: "vst3".into(),
            path: "/Users/test/VST3/Dexed.vst3".into(),
        }];
        let all = search_entries(
            &entries,
            &SearchFilters {
                query: None,
                tags: None,
                platform: "macos-aarch64",
                format: None,
                installed_only: false,
                limit: None,
                source: "bundled",
            },
            &installed,
        )
        .unwrap();
        assert!(all.total > 20);
        assert!(all.entries.iter().all(|entry| {
            entry
                .installs
                .iter()
                .all(|install| install.platform == "macos-aarch64")
        }));

        let dexed = search_entries(
            &entries,
            &SearchFilters {
                query: Some("dexed"),
                tags: None,
                platform: "macos-aarch64",
                format: Some(InstallFormat::Vst3),
                installed_only: false,
                limit: None,
                source: "bundled",
            },
            &installed,
        )
        .unwrap();
        assert_eq!(dexed.total, 1);
        assert_eq!(dexed.entries[0].id, "asb2m10.dexed");
        assert!(dexed.entries[0].installed);

        let installed_only = search_entries(
            &entries,
            &SearchFilters {
                query: None,
                tags: None,
                platform: "macos-aarch64",
                format: None,
                installed_only: true,
                limit: None,
                source: "bundled",
            },
            &installed,
        )
        .unwrap();
        assert_eq!(installed_only.total, 1);

        let reverb_tags = vec!["reverb".into(), "freeware".into()];
        let reverbs = search_entries(
            &entries,
            &SearchFilters {
                query: None,
                tags: Some(&reverb_tags),
                platform: "windows-x86_64",
                format: None,
                installed_only: false,
                limit: Some(2),
                source: "bundled",
            },
            &installed,
        )
        .unwrap();
        assert_eq!(reverbs.total, 4);
        assert_eq!(reverbs.entries.len(), 2);
        assert!(reverbs.entries.iter().all(|entry| {
            entry.tags.contains(&"reverb".into()) && entry.tags.contains(&"freeware".into())
        }));

        assert!(
            search_entries(
                &entries,
                &SearchFilters {
                    query: None,
                    tags: None,
                    platform: "windows-arm64",
                    format: None,
                    installed_only: false,
                    limit: None,
                    source: "bundled",
                },
                &installed,
            )
            .is_err()
        );
        assert!(parse_install_format("aax").is_err());
    }

    // ---- Manual (page-only) installs ----

    fn entry<'a>(entries: &'a [CatalogEntry], id: &str) -> &'a CatalogEntry {
        entries.iter().find(|entry| entry.id == id).expect("entry")
    }

    fn bundle(name: &str, format: &str) -> InstalledPlugin {
        InstalledPlugin {
            name: name.into(),
            format: format.into(),
            path: format!("/Users/test/VST3/{name}.{format}"),
        }
    }

    #[test]
    fn the_installed_matcher_finds_a_bundle_by_name_and_alias() {
        let entries = catalog();
        let reverb = entry(&entries, "tal-software.tal-reverb-4");
        assert!(installed_match(reverb, &[bundle("Dexed", "vst3")]).is_none());
        let scanned = [bundle("TAL-Reverb-4", "vst3")];
        let found = installed_match(reverb, &scanned).expect("bundle");
        assert_eq!(found.name, "TAL-Reverb-4");
        // Punctuation and case do not change the identity.
        assert!(installed_match(reverb, &[bundle("tal reverb 4", "au")]).is_some());

        let labs = entry(&entries, "spitfire-audio.labs");
        assert!(installed_match(labs, &[bundle("LABS", "vst3")]).is_some());
        assert!(installed_match(labs, &[bundle("Spitfire LABS", "au")]).is_some());
        assert!(installed_match(labs, &[bundle("Surge XT", "vst3")]).is_none());
    }

    #[test]
    fn the_installed_matcher_accepts_a_version_suffix_and_a_scanner_id() {
        let entries = catalog();
        // The bundle carries the product version; the entry name does not.
        let vocoder = entry(&entries, "tal-software.tal-vocoder");
        assert!(installed_match(vocoder, &[bundle("TAL-Vocoder-2", "vst3")]).is_some());
        // A short name never matches by prefix, so another vendor's product
        // cannot pass for this one.
        let helm = entry(&entries, "matt-tytel.helm");
        assert!(installed_match(helm, &[bundle("Helm", "vst3")]).is_some());
        assert!(installed_match(helm, &[bundle("Helmet", "vst3")]).is_none());

        // An entry whose pinned scanner id names the bundle.
        let mut pinned = entry(&entries, "asb2m10.dexed").clone();
        pinned.name = "FM Synth".into();
        pinned.aliases = Vec::new();
        pinned.installs[0].plugin_id = Some("vst3:dexed".into());
        let scanned = [bundle("Dexed", "vst3")];
        let found = installed_match(&pinned, &scanned).expect("bundle");
        assert_eq!(found.name, "Dexed");
    }

    #[test]
    fn the_installed_check_reports_what_the_walk_found() {
        let entries = catalog();
        let empty: Vec<InstalledPlugin> = Vec::new();
        let missing =
            installed_check_in(&entries, "tal-software.tal-reverb-4", &empty).expect("check");
        assert_eq!(missing.id, "tal-software.tal-reverb-4");
        assert_eq!(missing.name, "TAL-Reverb-4");
        assert!(!missing.installed);
        assert!(missing.matched.is_none());
        assert_eq!(missing.scanned, 0);

        let installed = vec![bundle("Dexed", "vst3"), bundle("TAL-Reverb-4", "vst3")];
        let present =
            installed_check_in(&entries, "tal-software.tal-reverb-4", &installed).expect("check");
        assert!(present.installed);
        assert_eq!(present.matched.as_deref(), Some("TAL-Reverb-4"));
        assert_eq!(present.scanned, 2);

        assert!(installed_check_in(&entries, "nobody.plugin", &installed).is_err());
    }

    #[test]
    fn the_manual_page_is_the_https_page_of_a_page_only_entry() {
        let entries = catalog();
        let url = manual_page_url(&entries, "tal-software.tal-reverb-4").expect("page");
        assert_eq!(url, "https://tal-software.com/products/tal-reverb-4");
        assert!(url.starts_with("https://"));

        // An entry with a pinned artifact has an install path, not a page.
        let direct = manual_page_url(&entries, "asb2m10.dexed").unwrap_err();
        assert!(direct.contains("use Install"), "{direct}");
        assert!(manual_page_url(&entries, "nobody.plugin").is_err());

        // Every page the manual path can open is an https page.
        for entry in &entries {
            for install in &entry.installs {
                if install.kind == InstallSource::VendorPage {
                    assert!(
                        install.url.starts_with("https://"),
                        "{} opens {}",
                        entry.id,
                        install.url
                    );
                }
            }
        }
    }

    #[test]
    fn the_store_response_recomputes_installed_from_the_walk() {
        let mut entries = catalog();
        let mut ghost = entry(&entries, "tal-software.tal-reverb-4").clone();
        ghost.id = "test.ghost".into();
        ghost.name = "Ghost Plugin Xyzzy".into();
        ghost.aliases = Vec::new();
        // The flag a previous response carried never ships again.
        ghost.installed = true;
        entries.push(ghost);

        let response = catalog_response(&entries);
        let ghost = response
            .entries
            .iter()
            .find(|entry| entry.id == "test.ghost")
            .expect("ghost");
        assert!(!ghost.installed);
        assert_eq!(
            ghost.installed,
            installed_match(ghost, &response.installed).is_some()
        );
    }

    #[test]
    fn panel_install_accepts_pinned_signed_native_archives_only() {
        let mut entries = catalog();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.id == "asb2m10.dexed")
            .unwrap();
        let mut install = entry
            .installs
            .iter()
            .find(|install| install.format == InstallFormat::Vst3)
            .unwrap()
            .clone();
        install.platform = oc_market::catalog_wire::Platform::parse(&host_platform()).unwrap();
        install.kind = InstallSource::Direct;
        install.archive = Some(InstallArchive::Zip);
        install.sha256 = Some("a".repeat(64));
        install.bytes = Some(1024);
        entry.installs = vec![install];
        let id = "asb2m10.dexed";
        assert!(is_panel_install(&entries, "remote", id, None, None).unwrap());
        assert!(is_panel_install(&entries, "cache", id, None, None).unwrap());
        assert!(!is_panel_install(&entries, "bundled", id, None, None).unwrap());
        for format in [
            InstallFormat::Vst3,
            InstallFormat::Vst2,
            InstallFormat::Clap,
            InstallFormat::Lv2,
        ] {
            let selected = &mut entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .unwrap()
                .installs[0];
            selected.format = format;
            selected.archive = Some(InstallArchive::TarGz);
            assert!(is_panel_install(&entries, "remote", id, None, None).unwrap());
        }
        entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .unwrap()
            .installs[0]
            .format = InstallFormat::Vst3;

        for archive in [
            InstallArchive::Dmg,
            InstallArchive::Pkg,
            InstallArchive::Installer,
        ] {
            entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .unwrap()
                .installs[0]
                .archive = Some(archive);
            assert!(!is_panel_install(&entries, "remote", id, None, None).unwrap());
        }
        let selected = &mut entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .unwrap()
            .installs[0];
        selected.archive = Some(InstallArchive::Zip);
        selected.sha256 = None;
        assert!(!is_panel_install(&entries, "remote", id, None, None).unwrap());
        let selected = &mut entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .unwrap()
            .installs[0];
        selected.sha256 = Some("a".repeat(64));
        selected.bytes = None;
        assert!(!is_panel_install(&entries, "remote", id, None, None).unwrap());
    }

    #[test]
    fn pull_dry_run_returns_plan_without_network_or_writes() {
        let plan = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &catalog(),
            "asb2m10.dexed",
            Some("macos-aarch64"),
            Some("vst3"),
            true,
            None,
        ))
        .unwrap();
        assert!(plan.dry_run);
        assert_eq!(plan.kind, "zip-vst3");
        assert_eq!(plan.mode, "extract");
        assert!(!plan.installed);
        assert!(!plan.opened);
        assert!(plan.url.starts_with("https://github.com/"));
        assert!(plan.target.as_deref().unwrap().ends_with("VST3"));
        assert!(plan.verified);
        assert_eq!(plan.sha256.len(), 64);
    }

    #[test]
    fn pull_refuses_a_page_only_entry_and_names_the_page() {
        let error = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &catalog(),
            "u-he.zebralette",
            Some("macos-aarch64"),
            None,
            true,
            None,
        ))
        .unwrap_err();
        assert!(error.contains("no direct archive; open"), "{error}");
        assert!(error.contains("u-he.com"), "{error}");
    }

    #[test]
    fn pull_refuses_an_unpinned_artifact_by_name() {
        let catalog = fixture_catalog("https://example.com/fixture.zip", None, Some(1024));
        let entries = parse_remote_catalog(catalog.as_bytes()).expect("fixture catalog");
        let error = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &entries.plugins,
            "fixture.reverb",
            Some(&host_platform()),
            None,
            true,
            None,
        ))
        .unwrap_err();
        assert!(
            error.starts_with("Fixture Reverb has no pinned sha256"),
            "{error}"
        );
        assert!(error.contains("https://example.com/fixture.zip"), "{error}");
    }

    #[test]
    fn pull_rejects_unknown_id_and_non_catalog_urls() {
        assert!(
            tauri::async_runtime::block_on(pull(
                &CatalogAuthority::Bundled,
                &catalog(),
                "https://evil.example/dexed.zip",
                None,
                None,
                true,
                None
            ))
            .is_err()
        );
        assert!(
            tauri::async_runtime::block_on(pull(
                &CatalogAuthority::Bundled,
                &catalog(),
                "asb2m10.dexed",
                Some("macos-aarch64"),
                Some("aax"),
                true,
                None
            ))
            .is_err()
        );
    }

    #[test]
    fn remote_catalog_reuses_bundled_validation() {
        let bundled = fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/data/plugin-catalog.json"
        ))
        .unwrap();
        let catalog = parse_remote_catalog(&bundled).expect("bundled bytes are a valid catalog");
        assert!(!catalog.plugins.is_empty());
        assert!(
            catalog.packs.is_empty(),
            "the bundled catalog ships no instrument packs"
        );

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["schema"] = serde_json::json!(3);
        let error = parse_remote_catalog_checked(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(
            error.code,
            oc_market::catalog_wire::CatalogErrorCode::Schema
        );
        assert_eq!(error.path, "schema");

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["schema"] = serde_json::json!(1);
        parse_remote_catalog(&serde_json::to_vec(&value).unwrap())
            .expect("schema 1 catalogs stay readable");

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["entries"][0]["installs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "vendor_page",
                "platform": "macos-aarch64",
                "format": "vst2",
                "url": "https://example.com/plugin",
            }));
        parse_remote_catalog(&serde_json::to_vec(&value).unwrap())
            .expect("vst2 installs without a plugin_id validate");

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["entries"][0]["installs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "vendor_page",
                "platform": "macos-aarch64",
                "format": "lv2",
                "url": "https://example.com/plugin",
                "plugin_id": "https://example.com/lv2/plugin",
            }));
        parse_remote_catalog(&serde_json::to_vec(&value).unwrap())
            .expect("vst2 and lv2 installs validate");

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["entries"][0]["installs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "vendor_page",
                "platform": "macos-aarch64",
                "format": "vst2",
                "url": "https://example.com/plugin",
                "plugin_id": "vst2:Not A Slug",
            }));
        let error = parse_remote_catalog(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.contains("plugin_id"));

        let mut value: serde_json::Value = serde_json::from_slice(&bundled).unwrap();
        value["entries"][0]["installs"][0]["url"] = serde_json::json!("http://example.com/x.zip");
        let error = parse_remote_catalog_checked(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(
            error.code,
            oc_market::catalog_wire::CatalogErrorCode::InvalidUrl
        );
        assert_eq!(error.path, "entries[0].installs[0].url");
    }

    #[test]
    fn shared_catalog_validation_matches_publisher_codes_and_paths() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../oc-market/tests/fixtures/catalog-validation.json"
        )))
        .unwrap();
        for case in fixture["invalid"].as_array().unwrap() {
            let bytes = serde_json::to_vec(&case["document"]).unwrap();
            let expected = oc_market::catalog_wire::parse(&bytes).unwrap_err();
            let actual = parse_remote_catalog_checked(&bytes).unwrap_err();
            assert_eq!(actual, expected, "{}", case["name"]);
        }
        let source = &fixture["complete"];
        let catalog = parse_remote_catalog_checked(&serde_json::to_vec(source).unwrap()).unwrap();
        assert_eq!(
            catalog.packs.len(),
            1,
            "unknown kinds are forwarded by publishers and skipped by this GUI"
        );
        let encoded = serde_json::to_value(&catalog.plugins[0]).unwrap();
        assert_eq!(encoded["aliases"], source["entries"][0]["aliases"]);
        assert_eq!(
            encoded["installs"][0]["companion_content"],
            source["entries"][0]["installs"][0]["companion_content"]
        );
        assert_eq!(
            encoded["installs"][1]["runtime"],
            source["entries"][0]["installs"][1]["runtime"]
        );
        for field in ["needs_admin", "installable", "installed"] {
            assert_eq!(encoded[field], false);
        }
    }

    fn bundled_catalog_value() -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../src/data/plugin-catalog.json"
            ))
            .unwrap(),
        )
        .unwrap()
    }

    fn soundfont_pack() -> serde_json::Value {
        serde_json::json!({
            "kind": "soundfont",
            "id": "ms-basic",
            "name": "MS Basic",
            "version": "1.0.0",
            "description": "General MIDI sound set for the built-in SoundFont engine.",
            "url": "https://example.com/packs/ms-basic-1.0.0.sf3",
            "bytes": 49999999,
            "sha256": "0".repeat(64),
            "license": {
                "name": "MIT",
                "url": "https://example.com/MS%20Basic_License.md",
                "text": "MuseScore_General, MIT, derived from FluidR3Mono and FluidR3."
            }
        })
    }

    #[test]
    fn a_catalog_without_packs_still_parses() {
        let mut value = bundled_catalog_value();
        value.as_object_mut().unwrap().remove("packs");
        let catalog = parse_remote_catalog(&serde_json::to_vec(&value).unwrap())
            .expect("a catalog written before packs existed stays readable");
        assert!(!catalog.plugins.is_empty());
        assert!(catalog.packs.is_empty());
    }

    #[test]
    fn a_catalog_with_packs_parses_them_beside_the_plugins() {
        let mut value = bundled_catalog_value();
        value["packs"] = serde_json::json!([soundfont_pack()]);
        let catalog = parse_remote_catalog(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(catalog.plugins.len(), bundled_catalog().unwrap().0.len());
        assert_eq!(catalog.packs.len(), 1);
        let pack = &catalog.packs[0];
        assert_eq!(pack.id, "ms-basic");
        assert_eq!(pack.kind, SOUNDFONT_PACK_KIND);
        assert_eq!(pack.bytes, 49_999_999);
        assert_eq!(pack.license.name, "MIT");
    }

    #[test]
    fn an_unknown_pack_kind_is_skipped_rather_than_failing_the_catalog() {
        let mut value = bundled_catalog_value();
        value["packs"] = serde_json::json!([
            { "kind": "impulse-response", "id": "hall", "url": "https://example.com/hall.wav" },
            soundfont_pack()
        ]);
        let catalog = parse_remote_catalog(&serde_json::to_vec(&value).unwrap())
            .expect("a future pack kind must not break this build");
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].id, "ms-basic");
    }

    #[test]
    fn a_malformed_soundfont_pack_is_refused() {
        for (key, replacement, needle) in [
            ("sha256", serde_json::json!("0".repeat(63)), "sha256"),
            ("bytes", serde_json::json!(0), "size"),
            ("url", serde_json::json!("ms-basic.sf3"), "URL"),
            ("version", serde_json::json!(""), "version"),
        ] {
            let mut pack = soundfont_pack();
            pack[key] = replacement;
            let mut value = bundled_catalog_value();
            value["packs"] = serde_json::json!([pack]);
            let error = parse_remote_catalog(&serde_json::to_vec(&value).unwrap()).unwrap_err();
            assert!(error.contains(needle), "{key}: {error}");
        }
        let mut pack = soundfont_pack();
        pack["license"]["text"] = serde_json::json!("");
        let mut value = bundled_catalog_value();
        value["packs"] = serde_json::json!([pack]);
        let error = parse_remote_catalog_checked(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(error.code, oc_market::catalog_wire::CatalogErrorCode::Pack);
        assert_eq!(error.path, "packs[0].license.text");
    }

    #[test]
    fn duplicate_pack_ids_are_refused() {
        let mut value = bundled_catalog_value();
        value["packs"] = serde_json::json!([soundfont_pack(), soundfont_pack()]);
        let error = parse_remote_catalog_checked(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(
            error.code,
            oc_market::catalog_wire::CatalogErrorCode::DuplicateId
        );
        assert_eq!(error.path, "packs[1].id");
    }

    fn build_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut tar = Vec::new();
        for (name, contents) in files {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..107].copy_from_slice(b"0000644");
            let size = format!("{:011o}\0", contents.len());
            header[124..136].copy_from_slice(size.as_bytes());
            header[148..156].copy_from_slice(b"        ");
            header[156] = b'0';
            let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
            tar.extend_from_slice(&header);
            tar.extend_from_slice(contents);
            let padding = (512 - contents.len() % 512) % 512;
            tar.extend(std::iter::repeat_n(0u8, padding));
        }
        tar.extend(std::iter::repeat_n(0u8, 1024));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn tar_checksum_failure_keeps_the_existing_bundle_and_cleans_staging() {
        let target = TempDir::new("tar-checksum-atomic");
        let old = target.0.join("Fixture.vst3/Contents/old");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, b"keep").unwrap();
        let archive = build_tar_gz(&[
            ("Fixture.vst3/Contents/new", b"new"),
            ("LICENSE", b"license"),
        ]);
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(archive.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw[1024] ^= 1;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        let archive = encoder.finish().unwrap();
        let error =
            extract_tar_bundles(&archive, BundleFormat::Vst3, HostOs::Macos, &target.0, true)
                .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert_eq!(fs::read(old).unwrap(), b"keep");
        assert!(!target.0.join("Fixture.vst3/Contents/new").exists());
        assert_eq!(fs::read_dir(&target.0).unwrap().count(), 1);
    }

    #[test]
    fn tar_bundle_root_stripping_refuses_colliding_destinations() {
        let target = TempDir::new("tar-root-collision");
        let archive = build_tar_gz(&[
            ("release-a/Fixture.vst3/file", b"first"),
            ("release-b/Fixture.vst3/file", b"second"),
        ]);
        let error = extract_tar_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap_err();
        assert!(error.contains("Conflicting tar destination"), "{error}");
        assert_eq!(fs::read_dir(&target.0).unwrap().count(), 0);
    }

    #[test]
    fn gnu_tar_root_directory_and_dot_prefixed_bundle_paths_install() {
        for root in [".", "./"] {
            let target = TempDir::new("gnu-tar-root");
            let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut archive = tar::Builder::new(gzip);
            for (name, payload, kind) in [
                (root, &b""[..], tar::EntryType::Directory),
                (
                    "./Fixture.vst3/Contents/plugin",
                    &b"binary"[..],
                    tar::EntryType::Regular,
                ),
            ] {
                let mut header = tar::Header::new_gnu();
                header.as_gnu_mut().unwrap().set_atime(1234);
                header.as_gnu_mut().unwrap().set_ctime(5678);
                header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
                header.set_size(payload.len() as u64);
                header.set_mode(0o755);
                header.set_entry_type(kind);
                header.set_cksum();
                archive.append(&header, payload).unwrap();
            }
            let archive = archive.into_inner().unwrap().finish().unwrap();
            let outcome = extract_tar_bundles(
                &archive,
                BundleFormat::Vst3,
                HostOs::Macos,
                &target.0,
                false,
            )
            .unwrap();
            assert!(matches!(outcome, InstallOutcome::Installed { .. }));
            let plugin = target.0.join("Fixture.vst3/Contents/plugin");
            assert_eq!(fs::read(&plugin).unwrap(), b"binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(plugin).unwrap().permissions().mode() & 0o777,
                    0o755
                );
            }
            assert_eq!(fs::read_dir(&target.0).unwrap().count(), 1);
        }
    }

    #[test]
    fn tar_gz_extraction_is_path_safe_and_installs_the_bundle() {
        let target = TempDir::new("tar");
        let archive = build_tar_gz(&[
            ("Dexed.vst3/Contents/Dexed", b"binary"),
            ("LICENSE", b"gpl"),
        ]);
        let outcome = extract_tar_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            &target.0,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(target.0.join("Dexed.vst3/Contents/Dexed").is_file());
        assert!(!target.0.join("LICENSE").exists());

        let traversal = build_tar_gz(&[("../evil.vst3/payload", b"x")]);
        assert!(
            extract_tar_bundles(
                &traversal,
                BundleFormat::Vst3,
                HostOs::Macos,
                &target.0,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn scan_dirs_are_platform_specific() {
        let dirs = host_dirs();
        let mac = plugin_scan_dirs(HostOs::Macos, &dirs);
        assert!(mac.contains(&PathBuf::from("/Users/test/Library/Audio/Plug-Ins/VST3")));
        assert!(mac.contains(&PathBuf::from("/Library/Audio/Plug-Ins/CLAP")));

        let windows = plugin_scan_dirs(HostOs::Windows, &dirs);
        assert!(windows.contains(&PathBuf::from("C:/Program Files/Common Files/VST3")));
        assert!(windows.contains(&PathBuf::from(
            "C:/Users/test/AppData/Local/Programs/Common/CLAP"
        )));

        let linux = plugin_scan_dirs(HostOs::Linux, &dirs);
        assert!(linux.contains(&PathBuf::from("/Users/test/.vst3")));
        assert!(linux.contains(&PathBuf::from("/usr/lib/clap")));
    }
    // ---- One-click install from a loopback fixture ----

    fn fixture_catalog(url: &str, sha256: Option<&str>, bytes: Option<u64>) -> String {
        let mut install = serde_json::json!({
            "kind": "direct",
            "platform": host_platform(),
            "format": "vst3",
            "archive": "zip",
            "url": url,
        });
        if let Some(sha256) = sha256 {
            install["sha256"] = serde_json::Value::String(sha256.to_string());
        }
        if let Some(bytes) = bytes {
            install["bytes"] = serde_json::Value::Number(bytes.into());
        }
        serde_json::json!({
            "schema": 2,
            "generated_at": "2026-11-01T00:00:00Z",
            "entries": [{
                "id": "fixture.reverb",
                "name": "Fixture Reverb",
                "vendor": "Fixture Audio",
                "tags": ["reverb"],
                "description": "A fixture reverb.",
                "installs": [install],
            }]
        })
        .to_string()
    }

    #[test]
    fn pull_installs_a_pinned_archive_from_a_loopback_server() {
        let archive = build_stored_zip(&[
            ("Fixture.vst3/Contents/Info.plist", b"<plist/>"),
            ("Fixture.vst3/Contents/MacOS/Fixture", b"binary"),
        ]);
        let server = oc_market::loopback::FixtureServer::start();
        server.publish("/artifact.zip", archive.clone());
        let catalog = fixture_catalog(
            &server.url("/artifact.zip"),
            Some(&oc_market::sha256_hex(&archive)),
            Some(archive.len() as u64),
        );
        let entries = parse_remote_catalog(catalog.as_bytes()).expect("fixture catalog");
        let dir = std::env::temp_dir().join(format!(
            "ocp-pull-fixture-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &entries.plugins,
            "fixture.reverb",
            Some(&host_platform()),
            None,
            false,
            Some(&dir),
        ))
        .expect("install succeeds");
        assert!(result.installed);
        assert!(result.verified);
        assert_eq!(result.kind, "zip-vst3");
        assert_eq!(result.mode, "extract");
        assert_eq!(result.sha256, oc_market::sha256_hex(&archive));
        assert!(dir.join("Fixture.vst3/Contents/Info.plist").is_file());
        assert!(dir.join("Fixture.vst3/Contents/MacOS/Fixture").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_refuses_a_hash_mismatch_and_installs_nothing() {
        let archive = build_stored_zip(&[("Fixture.vst3/Contents/Info.plist", b"<plist/>")]);
        let server = oc_market::loopback::FixtureServer::start();
        server.publish("/artifact.zip", archive.clone());
        let catalog = fixture_catalog(
            &server.url("/artifact.zip"),
            Some(&"0".repeat(64)),
            Some(archive.len() as u64),
        );
        let entries = parse_remote_catalog(catalog.as_bytes()).expect("fixture catalog");
        let dir = std::env::temp_dir().join(format!(
            "ocp-pull-mismatch-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let error = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &entries.plugins,
            "fixture.reverb",
            Some(&host_platform()),
            None,
            false,
            Some(&dir),
        ))
        .unwrap_err();
        assert!(error.contains("sha256"), "{error}");
        assert!(!dir.join("Fixture.vst3").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_refuses_a_size_mismatch_and_installs_nothing() {
        let archive = build_stored_zip(&[("Fixture.vst3/Contents/Info.plist", b"<plist/>")]);
        let server = oc_market::loopback::FixtureServer::start();
        server.publish("/artifact.zip", archive.clone());
        let catalog = fixture_catalog(
            &server.url("/artifact.zip"),
            Some(&oc_market::sha256_hex(&archive)),
            Some(1),
        );
        let entries = parse_remote_catalog(catalog.as_bytes()).expect("fixture catalog");
        let dir = std::env::temp_dir().join(format!(
            "ocp-pull-size-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let error = tauri::async_runtime::block_on(pull(
            &CatalogAuthority::Bundled,
            &entries.plugins,
            "fixture.reverb",
            Some(&host_platform()),
            None,
            false,
            Some(&dir),
        ))
        .unwrap_err();
        // The resumable download stops as soon as the body cannot match the
        // declared size, and reports the declared size it wanted.
        assert!(error.contains("expected 1"), "{error}");
        assert!(error.contains("no plugin files were changed"), "{error}");
        assert!(!dir.join("Fixture.vst3").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_reports_installability_for_pinned_page_only_and_unpinned_entries() {
        let entries = catalog();
        // The bundled catalog pins its archives for Apple silicon.
        let platform = "macos-aarch64".to_string();
        let result = search_entries(
            &entries,
            &SearchFilters {
                query: Some("dexed"),
                tags: None,
                platform: &platform,
                format: None,
                installed_only: false,
                limit: None,
                source: "bundled",
            },
            &[],
        )
        .unwrap();
        let dexed = &result.entries[0];
        assert!(dexed.installable, "dexed pins every artifact");
        assert_eq!(dexed.mode, Some("extract"));
        assert!(dexed.refusal.is_none());

        let pages = search_entries(
            &entries,
            &SearchFilters {
                query: Some("zebralette"),
                tags: None,
                platform: &platform,
                format: None,
                installed_only: false,
                limit: None,
                source: "bundled",
            },
            &[],
        )
        .unwrap();
        let page = &pages.entries[0];
        assert!(!page.installable);
        let refusal = page.refusal.as_ref().expect("a refusal");
        assert_eq!(refusal.code, "no_direct_archive");
        assert!(refusal.message.contains("open"), "{}", refusal.message);
        assert!(refusal.message.contains("manually"), "{}", refusal.message);
        assert!(refusal.url.as_deref().unwrap().starts_with("https://"));

        // An unpinned build is refused whether or not the bundled catalog
        // happens to carry one, so the fixture supplies it.
        let catalog = fixture_catalog("https://example.com/fixture.zip", None, Some(1024));
        let unpinned_entries = parse_remote_catalog(catalog.as_bytes()).expect("fixture catalog");
        // The fixture catalog lists its install for the host platform.
        let platform = host_platform().to_string();
        let unpinned = search_entries(
            &unpinned_entries.plugins,
            &SearchFilters {
                query: Some("fixture"),
                tags: None,
                platform: &platform,
                format: None,
                installed_only: false,
                limit: None,
                source: "remote",
            },
            &[],
        )
        .unwrap();
        let fixture = unpinned
            .entries
            .iter()
            .find(|entry| entry.id == "fixture.reverb")
            .expect("fixture entry");
        assert!(!fixture.installable);
        let refusal = fixture.refusal.as_ref().expect("a refusal");
        assert_eq!(refusal.code, "unpinned");
        assert!(
            refusal
                .message
                .starts_with("Fixture Reverb has no pinned sha256"),
            "{}",
            refusal.message
        );
        assert!(
            fixture
                .installs
                .iter()
                .all(|install| install.sha256.is_none())
        );
    }
}

#[cfg(test)]
mod download_retry_tests {
    use super::*;
    fn spawn_server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                requests.push(String::from_utf8(request).unwrap());
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (format!("http://{address}/fixture.zip"), handle)
    }
    #[tokio::test]
    async fn download_retry_resumes_truncated_body_and_preserves_exact_bytes() {
        let (url,server)=spawn_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".into(),"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef".into()]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            fetch_resumable(&client, &url, Some(6), 100).await.unwrap(),
            b"abcdef"
        );
        let requests = server.join().unwrap();
        assert!(requests[1].to_ascii_lowercase().contains("range: bytes=3-"));
    }
    #[tokio::test]
    async fn download_retry_restarts_if_server_ignores_range() {
        let (url, server) = spawn_server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef".into(),
        ]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            fetch_resumable(&client, &url, Some(6), 100).await.unwrap(),
            b"abcdef"
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn download_retry_file_digest_tracks_resume_and_restart() {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

        for reply in [
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef",
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
        ] {
            let (url, server) = spawn_server(vec![
                "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".into(),
                reply.into(),
            ]);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut target = DownloadTarget::File {
                file: tokio::fs::File::from_std(tempfile::tempfile().unwrap()),
                len: 0,
                sha256: Box::new(Sha256::new()),
            };
            fetch_resumable_into(&client, &url, Some(6), 100, &mut target)
                .await
                .unwrap();
            let DownloadTarget::File {
                mut file,
                len,
                sha256,
            } = target
            else {
                unreachable!()
            };
            file.flush().await.unwrap();
            file.rewind().await.unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(len, 6);
            assert_eq!(bytes, b"abcdef");
            assert_eq!(sha256.finalize(), Sha256::digest(b"abcdef"));
            let requests = server.join().unwrap();
            assert!(requests[1].to_ascii_lowercase().contains("range: bytes=3-"));
        }
    }
    #[tokio::test]
    async fn download_retry_rejects_wrong_range_and_reports_transfer_detail() {
        let (url,server)=spawn_server(vec!["HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".into(),"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/6\r\nConnection: close\r\n\r\ndef".into()]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = fetch_resumable(&client, &url, Some(6), 100)
            .await
            .unwrap_err();
        assert!(
            error.contains("fixture.zip") && error.contains("Content-Range"),
            "{error}"
        );
        server.join().unwrap();
        let (url,server)=spawn_server(vec!["HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();4]);
        let error = fetch_resumable(&client, &url, Some(6), 100)
            .await
            .unwrap_err();
        assert!(
            error.contains("4 attempts") && error.contains("503"),
            "{error}"
        );
        server.join().unwrap();
    }
}
