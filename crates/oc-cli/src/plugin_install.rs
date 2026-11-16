//! `ocp plugin install`: install one catalog entry in one command.
//!
//! The command resolves the entry with the same policy the GUI uses, verifies
//! the download against the pinned sha256, and unpacks it into the user's
//! plugin directory. A build the catalog does not pin is refused in the same
//! vocabulary `plugin_search` and `plugin_pull` report.

use std::fs;
use std::io::Read;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oc_market::install::{
    HostDirs, InstallArchive, InstallFormat, InstallKind, InstallOutcome, bundle_format, host_os,
    host_platform, path_is_writable, platform_os, select_install_target,
};
use oc_market::{Candidate, Mode, Refusal};
use serde::Serialize;

use crate::plugin_index::{Catalog, Entry, Install, load_public_key, validate_catalog};

/// Size cap for one artifact, matching the GUI store limit.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// The catalog compiled into the binary, used when no source is given.
const BUNDLED_CATALOG: &str = include_str!("../../oc-gui/frontend/src/data/plugin-catalog.json");

/// Everything `ocp plugin install` needs from the command line.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub id: String,
    /// Local catalog JSON. `index` wins when both are set.
    pub catalog: Option<PathBuf>,
    /// Signed index base URL.
    pub index: Option<String>,
    /// Pinned public key for `index`; defaults to the compiled key.
    pub public_key: Option<PathBuf>,
    pub platform: Option<String>,
    pub format: Option<String>,
    /// Install directory; defaults to the host's plugin directory.
    pub into: Option<PathBuf>,
    pub dry_run: bool,
    /// Launch the vendor installer for a build that cannot be unpacked.
    pub open: bool,
    pub overwrite: bool,
}

/// What the command did, or would do with `--dry-run`.
#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub format: String,
    pub kind: &'static str,
    pub mode: &'static str,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub target: String,
    pub installed: bool,
    pub opened: bool,
    pub dry_run: bool,
    pub path: Option<String>,
    pub note: Option<String>,
}

struct LoadedCatalog {
    catalog: Catalog,
    authority: Option<(
        oc_market::index::IndexStore,
        oc_market::index::VerifiedIndex,
    )>,
}
impl LoadedCatalog {
    fn publication_guard(&self) -> Result<Option<oc_market::index::PublicationGuard>, String> {
        self.authority
            .as_ref()
            .map(|(store, index)| store.authorize(index))
            .transpose()
            .map_err(|error| error.to_string())
    }
}
impl std::fmt::Debug for LoadedCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedCatalog")
            .field("catalog", &self.catalog)
            .field("remote", &self.authority.is_some())
            .finish()
    }
}
impl std::ops::Deref for LoadedCatalog {
    type Target = Catalog;
    fn deref(&self) -> &Catalog {
        &self.catalog
    }
}

/// Run one install and return the report the CLI prints.
pub fn install(options: &InstallOptions) -> Result<InstallReport, String> {
    let catalog = load_catalog(options)?;
    install_loaded(options, catalog)
}

fn install_loaded(
    options: &InstallOptions,
    catalog: LoadedCatalog,
) -> Result<InstallReport, String> {
    install_loaded_at(options, catalog, &HostDirs::from_env())
}

fn install_loaded_at(
    options: &InstallOptions,
    catalog: LoadedCatalog,
    dirs: &HostDirs,
) -> Result<InstallReport, String> {
    drop(catalog.publication_guard()?);
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == options.id)
        .ok_or_else(|| {
            Refusal::UnknownId {
                id: options.id.clone(),
            }
            .to_string()
        })?;
    let platform = options.platform.clone().unwrap_or_else(host_platform);
    platform_os(&platform)?;
    let format = match options.format.as_deref() {
        Some(value) => Some(InstallFormat::parse(value).ok_or_else(|| {
            format!(
                "Unsupported format '{value}'; expected vst3, vst2, clap, au, lv2, or standalone"
            )
        })?),
        None => None,
    };
    let plan = oc_market::plan(
        &entry.id,
        &entry.name,
        &candidates(entry),
        &platform,
        format,
    )
    .map_err(|refusal| refusal.to_string())?;
    let selected = oc_market::choose_index(&candidates(entry), &platform, format)
        .and_then(|index| entry.installs.get(index))
        .ok_or_else(|| "Resolved catalog build is missing".to_string())?;
    // A hand-off build has no bundle directory to select; the installer opens
    // in the vendor's own window, so the plan reports no target.
    // A managed separator runtime unpacks into the app's own runtime directory
    // with a descriptor the client validates; the headless CLI does not own it.
    if plan.kind == InstallKind::SeparatorRuntime {
        return Err(format!(
            "{} is a managed separator runtime; install it from the plugin browser in the app",
            entry.name
        ));
    }
    let target = match &options.into {
        Some(dir) => Some(dir.clone()),
        None => default_target(plan.kind).ok(),
    };
    let companion_paths = if selected.companion_content.is_some() {
        Some(
            oc_market::companion::CompanionPaths::resolve(
                selected,
                platform_os(&platform)?,
                dirs,
                target
                    .as_deref()
                    .ok_or("Companion build has no plugin directory")?,
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let mut report = InstallReport {
        id: entry.id.clone(),
        name: entry.name.clone(),
        platform: platform.clone(),
        format: plan.format.as_str().to_string(),
        kind: plan.kind.as_str(),
        mode: plan.mode.as_str(),
        url: plan.url.clone(),
        sha256: plan.sha256.clone(),
        bytes: plan.bytes.unwrap_or_default(),
        target: target
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        installed: false,
        opened: false,
        dry_run: options.dry_run,
        path: None,
        note: None,
    };
    if options.dry_run {
        return Ok(report);
    }
    if platform != host_platform() {
        return Err(Refusal::PlatformMismatch { platform }.to_string());
    }
    if plan.mode == Mode::HandOff && !options.open {
        return Err(format!(
            "{} ships an installer that must be opened; run `ocp plugin install {} --open`",
            entry.name, entry.id
        ));
    }
    let bytes = download(&plan.url)?;
    oc_market::verify_artifact(&bytes, &plan.sha256, plan.bytes)
        .map_err(|mismatch| mismatch.to_string())?;
    if let Some(paths) = companion_paths {
        let descriptor = selected
            .companion_content
            .as_ref()
            .ok_or("Missing companion content")?;
        let content = download(&descriptor.url)?;
        let prepared = oc_market::companion::prepare(oc_market::companion::Prepare {
            entry,
            install: selected,
            plugin: &bytes,
            content: &content,
            paths,
        })
        .map_err(|error| error.to_string())?;
        let _publication = catalog.publication_guard()?;
        match prepared
            .publish(options.overwrite)
            .map_err(|error| error.to_string())?
        {
            InstallOutcome::Installed { path, message } => {
                report.bytes = bytes.len() as u64;
                report.installed = true;
                report.path = Some(path);
                report.note = Some(message);
                return Ok(report);
            }
            InstallOutcome::NeedsConfirmation { path, .. } => {
                return Err(format!(
                    "'{path}' already exists; pass --overwrite to replace it"
                ));
            }
            _ => return Err("Companion publication did not install both artifacts".into()),
        }
    }
    let _publication = catalog.publication_guard()?;
    report.bytes = bytes.len() as u64;
    let target = target.ok_or_else(|| "this build has no install directory".to_string())?;
    match plan.mode {
        Mode::Extract => {
            let bundle = bundle_format(plan.kind)
                .ok_or_else(|| format!("{} has no bundle directory", entry.name))?;
            let outcome = if plan.archive == Some(InstallArchive::TarGz) {
                oc_market::extract_tar_bundles(
                    &bytes,
                    bundle,
                    host_os(),
                    &target,
                    options.overwrite,
                )?
            } else {
                oc_market::extract_bundles(&bytes, bundle, host_os(), &target, options.overwrite)?
            };
            match outcome {
                InstallOutcome::Installed { path, message } => {
                    report.installed = true;
                    report.path = Some(path);
                    report.note = Some(message);
                }
                InstallOutcome::NeedsConfirmation { path, .. } => {
                    return Err(format!(
                        "'{path}' already exists; pass --overwrite to replace it"
                    ));
                }
                InstallOutcome::WrappedInstaller { name, bytes } => {
                    if !options.open {
                        return Err(format!(
                            "{} ships a vendor installer inside the archive; run `ocp plugin install {} --open`",
                            entry.name, entry.id
                        ));
                    }
                    let path = write_named_installer(&bytes, &name)?.handoff(open_external)?;
                    report.opened = true;
                    report.path = Some(path.display().to_string());
                    report.note =
                        Some("Installer opened; finish the installation in its window.".into());
                }
                InstallOutcome::OpenedInstaller { .. } => {
                    return Err("the resolver planned an unpack, not an installer".into());
                }
            }
        }
        Mode::HandOff => {
            if plan.kind == InstallKind::InstallerArchive {
                let (name, inner) = oc_market::install::unwrap_installer(&bytes, plan.archive)
                    .map_err(|error| format!("{}: {error}", entry.name))?;
                let path = write_named_installer(&inner, &name)?.handoff(open_external)?;
                report.opened = true;
                report.path = Some(path.display().to_string());
                report.note =
                    Some("Installer opened; finish the installation in its window.".into());
                return Ok(report);
            }
            let path = write_installer(&bytes, &plan.url)?.handoff(open_external)?;
            report.opened = true;
            report.path = Some(path.display().to_string());
            report.note = Some("Installer opened; finish the installation in its window.".into());
        }
    }
    Ok(report)
}

fn candidates(entry: &Entry) -> Vec<Candidate> {
    entry.installs.iter().map(candidate).collect()
}

fn candidate(install: &Install) -> Candidate {
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

fn default_target(kind: InstallKind) -> Result<PathBuf, String> {
    let format = bundle_format(kind)
        .ok_or_else(|| "this build ships an installer, so it needs --open".to_string())?;
    Ok(select_install_target(
        host_os(),
        kind,
        format,
        &HostDirs::from_env(),
        path_is_writable,
    )?
    .dir)
}

/// Load the catalog from a signed index, a local file, or the bundled copy.
fn load_catalog(options: &InstallOptions) -> Result<LoadedCatalog, String> {
    if let Some(base) = &options.index {
        return fetch_index(base, options.public_key.as_deref(), options.dry_run);
    }
    let bytes = match &options.catalog {
        Some(path) => {
            fs::read(path).map_err(|error| format!("read catalog {}: {error}", path.display()))?
        }
        None => BUNDLED_CATALOG.as_bytes().to_vec(),
    };
    let catalog = oc_market::catalog_wire::parse(&bytes).map_err(|error| error.to_string())?;
    Ok(LoadedCatalog {
        catalog,
        authority: None,
    })
}

/// Fetch `index.json`, verify its detached signature, then fetch and check the
/// catalog it names. Nothing is trusted before the signature checks out.
fn fetch_index(
    base: &str,
    public_key_path: Option<&Path>,
    dry_run: bool,
) -> Result<LoadedCatalog, String> {
    let root = std::env::var_os("OCP_PLUGIN_INDEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| oc_audio::private_fs::config_dir().join("plugin-index"));
    fetch_index_at(IndexSourceOptions {
        base,
        public_key_path,
        dry_run,
        cache_root: &root,
    })
}

struct IndexSourceOptions<'a> {
    base: &'a str,
    public_key_path: Option<&'a Path>,
    dry_run: bool,
    cache_root: &'a Path,
}

fn fetch_index_at(options: IndexSourceOptions<'_>) -> Result<LoadedCatalog, String> {
    use oc_market::index::{FetchSession, IndexStore, Policy, Response};
    let IndexSourceOptions {
        base,
        public_key_path,
        dry_run,
        cache_root,
    } = options;
    let base = format!("{}/", base.trim_end_matches('/'));
    let public_key = match public_key_path {
        Some(path) => load_public_key(path).map_err(|error| format!("{error:#}"))?,
        None => require_index_key(oc_market::configured_public_key()?)?,
    };
    let policy = Policy {
        public_key,
        origins: if base == oc_market::index::PRIMARY_BASE_URL {
            vec![base, oc_market::index::FALLBACK_BASE_URL.into()]
        } else {
            vec![base.clone(), base]
        },
        app_version: Some(env!("CARGO_PKG_VERSION").into()),
    };
    let store = IndexStore::new(cache_root.to_path_buf(), policy.clone());
    let cached = store.load().map_err(|error| error.to_string())?;
    if let Some(cached) = &cached {
        validate_catalog(&cached.index.document).map_err(|error| format!("{error:#}"))?;
    }
    let mut session = FetchSession::new(
        policy,
        cached.map(|cached| cached.index),
        store.watermark().map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    while let Some(request) = session.request().map_err(|error| error.to_string())? {
        let response = (|| {
            let mut fetch = client
                .get(&request.url)
                .header(reqwest::header::CACHE_CONTROL, "no-cache")
                .header(
                    reqwest::header::ACCEPT,
                    "application/json, application/octet-stream",
                );
            if let Some(etag) = &request.if_none_match {
                fetch = fetch.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            let mut response = fetch.send().map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut bytes = Vec::new();
            response
                .by_ref()
                .take(request.max_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > request.max_bytes {
                return Err(format!(
                    "Index response exceeds {} bytes",
                    request.max_bytes
                ));
            }
            Ok(Response {
                status,
                body: bytes,
                etag,
            })
        })();
        session
            .accept(response)
            .map_err(|error| error.to_string())?;
    }
    let index = session.into_verified().map_err(|error| error.to_string())?;
    let catalog = (*index.document).clone();
    if dry_run {
        if let Some(highest) = store.watermark().map_err(|error| error.to_string())? {
            highest
                .check(&index.stamp().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        }
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        store
            .commit(&index, now)
            .map_err(|error| error.to_string())?;
    }
    Ok(LoadedCatalog {
        catalog,
        authority: Some((store, index)),
    })
}

fn require_index_key(configured: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    configured.ok_or_else(|| {
        "Remote plugin-index trust is not configured; supply --public-key for the chosen index"
            .to_string()
    })
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    if !oc_market::is_install_url(url) {
        return Err(format!("Refusing to download from '{url}'"));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|error| format!("Download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}", response.status()));
    }
    if let Some(length) = response.content_length()
        && length > MAX_DOWNLOAD_BYTES
    {
        return Err("Download is larger than the plugin store limit".into());
    }
    // A body without a length (a chunked response) is read in chunks and the
    // cap is enforced on the running total, so the whole body never has to fit
    // in memory before the limit is known.
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut chunk)
            .map_err(|error| format!("Failed to read download: {error}"))?;
        if read == 0 {
            break;
        }
        if bytes.len() as u64 + read as u64 > MAX_DOWNLOAD_BYTES {
            return Err("Download is larger than the plugin store limit".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn write_named_installer(
    bytes: &[u8],
    name: &str,
) -> Result<oc_market::temporary::PreparedInstaller, String> {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && !name.contains(':'))
        .ok_or_else(|| "installer name in the archive is unsafe".to_string())?;
    oc_market::temporary::prepare_installer_in(&std::env::temp_dir(), name, bytes)
        .map_err(|error| format!("Could not stage installer: {error}"))
}

fn write_installer(
    bytes: &[u8],
    url: &str,
) -> Result<oc_market::temporary::PreparedInstaller, String> {
    let name = url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\', ':']))
        .unwrap_or("installer");
    oc_market::temporary::prepare_installer_in(&std::env::temp_dir(), name, bytes)
        .map_err(|error| format!("Could not stage installer: {error}"))
}

fn open_external(path: &Path) -> Result<(), String> {
    let target = path.to_string_lossy().to_string();
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&target).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &target])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(&target).status()
    }
    .map_err(|error| format!("open installer: {error}"))?;
    if !status.success() {
        return Err(format!("open installer exited with {status}"));
    }
    Ok(())
}

/// Serialise the report for `--json`.
pub fn report_json(report: &InstallReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_index::{BuildOptions, build, keygen};
    use oc_market::loopback::FixtureServer;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-plugin-install-{tag}-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
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

    /// A stored (uncompressed) zip, which is all the extractor needs to read.
    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
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

    fn fixture_archive() -> Vec<u8> {
        stored_zip(&[
            ("Fixture.vst3/Contents/Info.plist", b"<plist/>"),
            ("Fixture.vst3/Contents/MacOS/Fixture", b"binary"),
        ])
    }

    fn catalog_json(url: &str, sha256: &str, bytes: u64) -> String {
        serde_json::json!({
            "schema": 2,
            "generated_at": "2026-11-01T00:00:00Z",
            "entries": [{
                "id": "fixture.reverb",
                "name": "Fixture Reverb",
                "vendor": "Fixture Audio",
                "tags": ["reverb"],
                "description": "A fixture reverb.",
                "installs": [{
                    "kind": "direct",
                    "platform": host_platform(),
                    "format": "vst3",
                    "archive": "zip",
                    "url": url,
                    "sha256": sha256,
                    "bytes": bytes,
                }],
            }]
        })
        .to_string()
    }

    /// Build a signed index over one entry pointing at the loopback artifact,
    /// then publish the manifest, signature, catalog and archive.
    struct Published {
        _dir: TempDir,
        server: FixtureServer,
        public_key: PathBuf,
    }

    fn published(sha256: &str, bytes: u64, archive: &[u8]) -> Published {
        published_at(sha256, bytes, archive, None)
    }
    fn published_at(
        sha256: &str,
        bytes: u64,
        archive: &[u8],
        artifact_url: Option<&str>,
    ) -> Published {
        let dir = TempDir::new("index");
        let server = FixtureServer::start();
        server.publish("/artifact.zip", archive.to_vec());
        let url = artifact_url
            .map(str::to_owned)
            .unwrap_or_else(|| server.url("/artifact.zip"));
        let catalog_path = dir.0.join("catalog.json");
        fs::write(&catalog_path, catalog_json(&url, sha256, bytes)).unwrap();
        let keys = dir.0.join("keys");
        keygen(&keys, false).unwrap();
        let out = dir.0.join("dist");
        build(&BuildOptions {
            catalog_path,
            out_dir: out.clone(),
            key_path: Some(keys.join("plugin-index.key")),
            catalog_version: Some("test".into()),
            min_app_version: "0.1.0".into(),
            generated_at: Some("2026-11-01T00:00:00Z".into()),
            level: 3,
            mirrors: Vec::new(),
        })
        .unwrap();
        server.publish("/index.json", fs::read(out.join("index.json")).unwrap());
        server.publish(
            "/index.json.sig",
            fs::read(out.join("index.json.sig")).unwrap(),
        );
        let catalog_file = fs::read_dir(&out)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("catalog-"))
            })
            .expect("built catalog");
        let name = catalog_file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        server.publish(&format!("/{name}"), fs::read(&catalog_file).unwrap());
        Published {
            _dir: dir,
            server,
            public_key: keys.join("plugin-index.pub"),
        }
    }

    fn options(published: &Published, into: &Path) -> InstallOptions {
        InstallOptions {
            id: "fixture.reverb".into(),
            catalog: None,
            index: Some(published.server.base()),
            public_key: Some(published.public_key.clone()),
            platform: None,
            format: None,
            into: Some(into.to_path_buf()),
            dry_run: false,
            open: false,
            overwrite: false,
        }
    }

    #[test]
    fn signed_index_compatibility_and_rollback_fail_before_artifact_fetch() {
        let archive = fixture_archive();
        let fixture = published(
            &oc_market::sha256_hex(&archive),
            archive.len() as u64,
            &archive,
        );
        let cache = TempDir::new("index-policy-cache");
        let original = fs::read(fixture._dir.0.join("dist/index.json")).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(
            &fs::read(fixture._dir.0.join("keys/plugin-index.key")).unwrap(),
        )
        .unwrap();
        let publish = |value: &serde_json::Value| {
            let bytes = serde_json::to_vec(value).unwrap();
            fixture.server.publish("/index.json", bytes.clone());
            fixture
                .server
                .publish("/index.json.sig", pair.sign(&bytes).as_ref().to_vec());
        };
        for (field, value, stage) in [
            (
                "min_app_version",
                serde_json::json!("99.0.0"),
                "min_app_version",
            ),
            (
                "generated_at",
                serde_json::json!("2026-02-30T00:00:00Z"),
                "schema",
            ),
            ("schema", serde_json::json!(0), "schema"),
        ] {
            let mut manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
            manifest[field] = value;
            publish(&manifest);
            let error = fetch_index_at(IndexSourceOptions {
                base: &fixture.server.base(),
                public_key_path: Some(&fixture.public_key),
                dry_run: false,
                cache_root: &cache.0,
            })
            .unwrap_err();
            assert!(error.starts_with(stage), "{error}");
            assert_eq!(fs::read_dir(&cache.0).unwrap().count(), 0);
        }
        let mut manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
        publish(&manifest);
        let _ = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &cache.0,
        })
        .unwrap();
        manifest["generated_at"] = "2026-11-02T00:00:00Z".into();
        manifest["catalog_version"] = "newer".into();
        publish(&manifest);
        let _ = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &cache.0,
        })
        .unwrap();
        publish(&serde_json::from_slice(&original).unwrap());
        let error = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &cache.0,
        })
        .unwrap_err();
        assert!(error.starts_with("rollback:"), "{error}");
        assert!(
            !fixture
                .server
                .requests()
                .iter()
                .any(|path| path == "/artifact.zip")
        );
    }

    #[test]
    fn remote_install_rechecks_publication_after_download_and_allows_exact_replay() {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        use std::sync::mpsc;
        for advances in [false, true] {
            let archive = fixture_archive();
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let artifact_url = format!("http://{}/artifact.zip", listener.local_addr().unwrap());
            let fixture = published_at(
                &oc_market::sha256_hex(&archive),
                archive.len() as u64,
                &archive,
                Some(&artifact_url),
            );
            let cache = TempDir::new("publication-cache");
            let target = TempDir::new("publication-target");
            let loaded = fetch_index_at(IndexSourceOptions {
                base: &fixture.server.base(),
                public_key_path: Some(&fixture.public_key),
                dry_run: false,
                cache_root: &cache.0,
            })
            .unwrap();
            let (store, mut publication) = loaded.authority.as_ref().unwrap().clone();
            if advances {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&publication.manifest_bytes).unwrap();
                value["generated_at"] = "2026-11-02T00:00:00Z".into();
                value["catalog_version"] = "newer".into();
                let bytes = serde_json::to_vec(&value).unwrap();
                let pair = ring::signature::Ed25519KeyPair::from_pkcs8(
                    &fs::read(fixture._dir.0.join("keys/plugin-index.key")).unwrap(),
                )
                .unwrap();
                publication.manifest = serde_json::from_value(value).unwrap();
                publication.signature = pair.sign(&bytes).as_ref().to_vec().into();
                publication.manifest_bytes = bytes.into();
            }
            let (began_tx, began_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive.len()
                )
                .unwrap();
                stream.flush().unwrap();
                began_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                stream.write_all(&archive).unwrap();
            });
            let options = options(&fixture, &target.0);
            let installer = std::thread::spawn(move || install_loaded(&options, loaded));
            began_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            store.commit(&publication, 100).unwrap();
            release_tx.send(()).unwrap();
            server.join().unwrap();
            let result = installer.join().unwrap();
            if advances {
                assert!(result.unwrap_err().contains("rollback"));
                assert_eq!(
                    fs::read_dir(&target.0).unwrap().count(),
                    0,
                    "stale download must not publish a bundle or receipt"
                );
            } else {
                assert!(
                    result.unwrap().installed,
                    "same signed metadata remains idempotent"
                );
            }
        }
    }

    #[test]
    fn remote_index_dry_run_verifies_but_never_writes_cache_or_watermark() {
        let archive = fixture_archive();
        let fixture = published(
            &oc_market::sha256_hex(&archive),
            archive.len() as u64,
            &archive,
        );
        let cache = TempDir::new("dry-run-index-cache");
        let catalog = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: true,
            cache_root: &cache.0,
        })
        .unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(fs::read_dir(&cache.0).unwrap().count(), 0);
        assert_eq!(
            fixture
                .server
                .requests()
                .iter()
                .filter(|path| path.as_str() == "/index.json")
                .count(),
            1
        );
        assert!(
            !fixture
                .server
                .requests()
                .iter()
                .any(|path| path == "/artifact.zip")
        );
        let _ = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &cache.0,
        })
        .unwrap();
        assert!(cache.0.join("scopes").is_dir());
    }

    #[test]
    fn local_catalog_loading_uses_shared_validation_codes_and_paths() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../oc-market/tests/fixtures/catalog-validation.json"
        ))
        .unwrap();
        let dir = TempDir::new("shared-catalog-validation");
        let path = dir.0.join("catalog.json");
        for case in fixture["invalid"].as_array().unwrap() {
            let bytes = serde_json::to_vec(&case["document"]).unwrap();
            fs::write(&path, &bytes).unwrap();
            let options = InstallOptions {
                id: "example.synth".into(),
                catalog: Some(path.clone()),
                index: None,
                public_key: None,
                platform: None,
                format: None,
                into: Some(dir.0.join("plugins")),
                dry_run: true,
                open: false,
                overwrite: false,
            };
            let expected = oc_market::catalog_wire::parse(&bytes).unwrap_err();
            assert_eq!(
                load_catalog(&options).unwrap_err(),
                expected.to_string(),
                "{}",
                case["name"]
            );
            assert!(!dir.0.join("plugins").exists());
        }
    }

    #[test]
    fn installer_adapters_isolate_fixed_filenames_and_clean_failed_handoffs() {
        let first = write_installer(b"first", "https://example.test/setup.exe").unwrap();
        let second = write_named_installer(b"second", "setup.exe").unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(fs::read(first.path()).unwrap(), b"first");
        assert_eq!(fs::read(second.path()).unwrap(), b"second");
        let first_directory = first.path().parent().unwrap().to_path_buf();
        let second_directory = second.path().parent().unwrap().to_path_buf();
        assert!(
            first
                .handoff(|_| Err::<(), _>("opener unavailable"))
                .is_err()
        );
        assert!(!first_directory.exists());
        assert_eq!(fs::read(second.path()).unwrap(), b"second");
        drop(second);
        assert!(!second_directory.exists());
    }

    #[test]
    fn remote_index_requires_an_explicit_trust_authority() {
        assert!(
            require_index_key(None)
                .unwrap_err()
                .contains("trust is not configured")
        );
        assert_eq!(require_index_key(Some(vec![7; 32])).unwrap(), vec![7; 32]);
    }

    #[cfg(target_os = "macos")]
    fn companion_archive(files: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, data, kind) in files {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            h[148..156].fill(b' ');
            h[156] = *kind;
            let sum: u64 = h.iter().map(|b| *b as u64).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            tar.extend_from_slice(&h);
            tar.extend_from_slice(data);
            tar.resize(tar.len() + (512 - data.len() % 512) % 512, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[cfg(target_os = "macos")]
    fn published_companion(content: &[u8], hash: &str) -> Published {
        published_companion_at(content, hash, None)
    }

    #[cfg(target_os = "macos")]
    fn published_companion_at(content: &[u8], hash: &str, url: Option<&str>) -> Published {
        let plugin = fixture_archive();
        let fixture = published(
            &oc_market::sha256_hex(&plugin),
            plugin.len() as u64,
            &plugin,
        );
        fixture.server.publish("/factory.tar.gz", content.to_vec());
        let path = fixture._dir.0.join("catalog.json");
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let content_url = match url {
            Some(path) if path.starts_with('/') => fixture.server.url(path),
            Some(url) => url.to_string(),
            None => fixture.server.url("/factory.tar.gz"),
        };
        document["entries"][0]["installs"][0]["companion_content"] = serde_json::json!({
            "target":"surge-xt-factory","archive":"tar-gz","url":content_url,
            "sha256":hash,"bytes":content.len(),"strip_prefix":"factory"
        });
        fs::write(&path, document.to_string()).unwrap();
        let out = fixture._dir.0.join("companion-dist");
        let report = build(&BuildOptions {
            catalog_path: path,
            out_dir: out.clone(),
            key_path: Some(fixture._dir.0.join("keys/plugin-index.key")),
            catalog_version: Some("companion-test".into()),
            min_app_version: "0.1.0".into(),
            generated_at: Some("2026-11-01T00:00:00Z".into()),
            level: 3,
            mirrors: Vec::new(),
        })
        .unwrap();
        fixture
            .server
            .publish("/index.json", fs::read(out.join("index.json")).unwrap());
        fixture.server.publish(
            "/index.json.sig",
            fs::read(out.join("index.json.sig")).unwrap(),
        );
        fixture.server.publish(
            &format!("/{}", report.catalog_path),
            fs::read(out.join(&report.catalog_path)).unwrap(),
        );
        fixture
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn companion_download_rechecks_authority_and_allows_exact_replay() {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        use std::sync::mpsc;
        for advances in [false, true] {
            let archive = companion_archive(&[
                ("factory/patches_factory/p", b"patch", b'0'),
                ("factory/wavetables/w", b"wave", b'0'),
                ("factory/skins/s", b"skin", b'0'),
            ]);
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let artifact_url = format!("http://{}/artifact.zip", listener.local_addr().unwrap());
            let fixture = published_companion_at(
                &archive,
                &oc_market::sha256_hex(&archive),
                Some(&artifact_url),
            );
            let cache = TempDir::new("publication-cache");
            let target = TempDir::new("publication-target");
            let loaded = fetch_index_at(IndexSourceOptions {
                base: &fixture.server.base(),
                public_key_path: Some(&fixture.public_key),
                dry_run: false,
                cache_root: &cache.0,
            })
            .unwrap();
            let (store, mut publication) = loaded.authority.as_ref().unwrap().clone();
            if advances {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&publication.manifest_bytes).unwrap();
                value["generated_at"] = "2026-11-02T00:00:00Z".into();
                value["catalog_version"] = "newer".into();
                let bytes = serde_json::to_vec(&value).unwrap();
                let pair = ring::signature::Ed25519KeyPair::from_pkcs8(
                    &fs::read(fixture._dir.0.join("keys/plugin-index.key")).unwrap(),
                )
                .unwrap();
                publication.manifest = serde_json::from_value(value).unwrap();
                publication.signature = pair.sign(&bytes).as_ref().to_vec().into();
                publication.manifest_bytes = bytes.into();
            }
            let (began_tx, began_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive.len()
                )
                .unwrap();
                stream.flush().unwrap();
                began_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                stream.write_all(&archive).unwrap();
            });
            let home = TempDir::new("companion-publication-home");
            let dirs = HostDirs {
                home: Some(home.0.clone()),
                common_program_files: None,
                local_app_data: None,
            };
            let options = options(&fixture, &target.0);
            let installer = std::thread::spawn(move || install_loaded_at(&options, loaded, &dirs));
            began_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            store.commit(&publication, 100).unwrap();
            release_tx.send(()).unwrap();
            server.join().unwrap();
            let result = installer.join().unwrap();
            if advances {
                assert!(!home.0.join("Library/Application Support/Surge XT").exists());
                assert!(result.unwrap_err().contains("rollback"));
                assert_eq!(
                    fs::read_dir(&target.0).unwrap().count(),
                    0,
                    "stale download must not publish a bundle or receipt"
                );
            } else {
                assert!(
                    result.unwrap().installed,
                    "same signed metadata remains idempotent"
                );
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn one_cli_install_fetches_and_publishes_primary_and_companion_from_signed_catalog() {
        let content = companion_archive(&[
            ("factory/patches_factory/p", b"patch", b'0'),
            ("factory/wavetables/w", b"wave", b'0'),
            ("factory/skins/s", b"skin", b'0'),
        ]);
        let fixture = published_companion(&content, &oc_market::sha256_hex(&content));
        let root =
            oc_market::temporary::directory_in(&std::env::temp_dir(), "cli-companion-").unwrap();
        let dirs = HostDirs {
            home: Some(root.path().join("home")),
            common_program_files: None,
            local_app_data: None,
        };
        let options = options(&fixture, &root.path().join("custom-plugins"));
        let loaded = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &root.path().join("cache"),
        })
        .unwrap();
        let report = install_loaded_at(&options, loaded, &dirs).unwrap();
        assert!(report.installed);
        assert!(
            root.path()
                .join("custom-plugins/Fixture.vst3/Contents/Info.plist")
                .is_file()
        );
        let content_dir = root
            .path()
            .join("home/Library/Application Support/Surge XT");
        assert_eq!(
            fs::read(content_dir.join("patches_factory/p")).unwrap(),
            b"patch"
        );
        assert!(content_dir.join(".opencompose-content.json").is_file());
        let requests = fixture.server.requests();
        assert!(requests.iter().any(|path| path == "/artifact.zip"));
        assert!(requests.iter().any(|path| path == "/factory.tar.gz"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn companion_integrity_failure_and_dry_run_never_publish_partial_cli_install() {
        let content = companion_archive(&[
            ("factory/patches_factory/p", b"patch", b'0'),
            ("factory/wavetables/w", b"wave", b'0'),
            ("factory/skins/s", b"skin", b'0'),
        ]);
        for dry_run in [true, false] {
            let fixture = published_companion(&content, &"a".repeat(64));
            let root =
                oc_market::temporary::directory_in(&std::env::temp_dir(), "cli-companion-fail-")
                    .unwrap();
            let dirs = HostDirs {
                home: Some(root.path().join("home")),
                common_program_files: None,
                local_app_data: None,
            };
            let mut options = options(&fixture, &root.path().join("custom-plugins"));
            options.dry_run = dry_run;
            let loaded = fetch_index_at(IndexSourceOptions {
                base: &fixture.server.base(),
                public_key_path: Some(&fixture.public_key),
                dry_run,
                cache_root: &root.path().join("cache"),
            })
            .unwrap();
            let result = install_loaded_at(&options, loaded, &dirs);
            if dry_run {
                assert!(!result.unwrap().installed);
                assert!(
                    !fixture
                        .server
                        .requests()
                        .iter()
                        .any(|path| path == "/artifact.zip" || path == "/factory.tar.gz")
                );
            } else {
                assert!(result.unwrap_err().contains("Integrity"));
                assert!(
                    fixture
                        .server
                        .requests()
                        .iter()
                        .any(|path| path == "/factory.tar.gz")
                );
            }
            assert!(!root.path().join("custom-plugins").exists());
            assert!(!root.path().join("home").exists());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn missing_companion_download_leaves_existing_cli_install_untouched() {
        let content = companion_archive(&[
            ("factory/patches_factory/p", b"patch", b'0'),
            ("factory/wavetables/w", b"wave", b'0'),
            ("factory/skins/s", b"skin", b'0'),
        ]);
        let fixture = published_companion_at(
            &content,
            &oc_market::sha256_hex(&content),
            Some("/missing.tar.gz"),
        );
        let root =
            oc_market::temporary::directory_in(&std::env::temp_dir(), "cli-companion-missing-")
                .unwrap();
        let dirs = HostDirs {
            home: Some(root.path().join("home")),
            common_program_files: None,
            local_app_data: None,
        };
        let content_dir = dirs
            .home
            .as_ref()
            .unwrap()
            .join("Library/Application Support/Surge XT");
        let plugin_dir = root.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("previous"), b"plugin").unwrap();
        fs::create_dir_all(&content_dir).unwrap();
        fs::write(content_dir.join("previous"), b"content").unwrap();
        let options = options(&fixture, &plugin_dir);
        let loaded = fetch_index_at(IndexSourceOptions {
            base: &fixture.server.base(),
            public_key_path: Some(&fixture.public_key),
            dry_run: false,
            cache_root: &root.path().join("cache"),
        })
        .unwrap();
        assert!(install_loaded_at(&options, loaded, &dirs).is_err());
        assert_eq!(fs::read(plugin_dir.join("previous")).unwrap(), b"plugin");
        assert_eq!(fs::read(content_dir.join("previous")).unwrap(), b"content");
        assert_eq!(fs::read_dir(plugin_dir).unwrap().count(), 1);
        assert_eq!(fs::read_dir(content_dir).unwrap().count(), 1);
        assert!(
            fixture
                .server
                .requests()
                .iter()
                .any(|path| path == "/missing.tar.gz")
        );
    }

    #[test]
    fn installs_a_pinned_artifact_from_a_signed_index() {
        let archive = fixture_archive();
        let fixture = published(
            &oc_market::sha256_hex(&archive),
            archive.len() as u64,
            &archive,
        );
        let target = TempDir::new("target");
        let report = install(&options(&fixture, &target.0)).expect("install succeeds");
        assert!(report.installed);
        assert_eq!(report.kind, "zip-vst3");
        assert_eq!(report.mode, "extract");
        assert_eq!(report.bytes, archive.len() as u64);
        assert_eq!(report.sha256, oc_market::sha256_hex(&archive));
        assert!(target.0.join("Fixture.vst3/Contents/Info.plist").is_file());
        assert!(
            target
                .0
                .join("Fixture.vst3/Contents/MacOS/Fixture")
                .is_file()
        );
    }

    #[test]
    fn a_hash_mismatch_is_refused_and_installs_nothing() {
        let archive = fixture_archive();
        let fixture = published(&"0".repeat(64), archive.len() as u64, &archive);
        let target = TempDir::new("target");
        let error = install(&options(&fixture, &target.0)).unwrap_err();
        assert!(error.contains("sha256"), "{error}");
        assert!(!target.0.join("Fixture.vst3").exists());
    }

    #[test]
    fn a_size_mismatch_is_refused_and_installs_nothing() {
        let archive = fixture_archive();
        let fixture = published(&oc_market::sha256_hex(&archive), 1, &archive);
        let target = TempDir::new("target");
        let error = install(&options(&fixture, &target.0)).unwrap_err();
        assert!(error.contains("Downloaded"), "{error}");
        assert!(error.contains("expected 1"), "{error}");
        assert!(!target.0.join("Fixture.vst3").exists());
    }

    #[test]
    fn a_response_without_a_length_is_cut_off_at_the_store_limit() {
        let server = FixtureServer::start();
        // One chunk past the cap, then more than a socket buffer can absorb, so
        // a client that buffers the body before checking would read all of it.
        let total = MAX_DOWNLOAD_BYTES as usize + 64 * 1024 * 1024;
        server.stream("/big.zip", vec![b'z'; 64 * 1024], total);

        let error = download(&server.url("/big.zip")).unwrap_err();
        assert_eq!(error, "Download is larger than the plugin store limit");
        let sent = server.written("/big.zip");
        assert!(
            sent as u64 >= MAX_DOWNLOAD_BYTES,
            "the cap was never reached: {sent} bytes"
        );
        assert!(
            sent < total,
            "the whole {total} byte body was read before the cap stopped it"
        );
    }

    #[test]
    fn an_unsigned_index_is_refused_before_any_download() {
        let server = FixtureServer::start();
        let trust = TempDir::new("unsigned-index-trust");
        let public_key = keygen(&trust.0, false).unwrap().public_key_path;
        server.publish_text(
            "/index.json",
            r#"{"schema":1,"generated_at":"2026-11-01T00:00:00Z","catalog_version":"x","min_app_version":"0.1.0","catalog":{"path":"catalog-x.json.zst","sha256":"00","bytes":1,"uncompressed_bytes":1,"encoding":"zstd","compression_level":1}}"#,
        );
        let target = TempDir::new("target");
        let error = install(&InstallOptions {
            id: "fixture.reverb".into(),
            catalog: None,
            index: Some(server.base()),
            public_key: Some(public_key),
            platform: None,
            format: None,
            into: Some(target.0.clone()),
            dry_run: false,
            open: false,
            overwrite: false,
        })
        .unwrap_err();
        assert!(error.contains("unsigned"), "{error}");
        assert_eq!(server.requests(), vec!["/index.json"]);
    }

    #[test]
    fn an_unknown_id_is_named() {
        let target = TempDir::new("target");
        let error = install(&InstallOptions {
            id: "nope.nope".into(),
            catalog: None,
            index: None,
            public_key: None,
            platform: None,
            format: None,
            into: Some(target.0.clone()),
            dry_run: true,
            open: false,
            overwrite: false,
        })
        .unwrap_err();
        assert_eq!(error, "Unknown plugin catalog id 'nope.nope'");
    }

    #[test]
    fn a_page_only_entry_is_refused_with_its_url() {
        let target = TempDir::new("target");
        let error = install(&InstallOptions {
            id: "u-he.zebralette".into(),
            catalog: None,
            index: None,
            public_key: None,
            platform: Some("macos-aarch64".into()),
            format: None,
            into: Some(target.0.clone()),
            dry_run: true,
            open: false,
            overwrite: false,
        })
        .unwrap_err();
        assert!(error.contains("no direct archive; open"), "{error}");
        assert!(error.contains("u-he.com"), "{error}");
    }

    #[test]
    fn an_unpinned_build_is_refused_by_name() {
        let dir = TempDir::new("unpinned");
        let catalog = dir.0.join("catalog.json");
        fs::write(
            &catalog,
            serde_json::json!({
                "schema": 2,
                "generated_at": "2026-11-01T00:00:00Z",
                "entries": [{
                    "id": "fixture.unpinned",
                    "name": "Fixture Unpinned",
                    "vendor": "Fixture Audio",
                    "tags": ["reverb"],
                    "description": "An unpinned fixture.",
                    "installs": [{
                        "kind": "direct",
                        "platform": host_platform(),
                        "format": "vst3",
                        "archive": "zip",
                        "url": "https://example.com/fixture.zip",
                        "bytes": 1024,
                    }],
                }]
            })
            .to_string(),
        )
        .unwrap();
        let error = install(&InstallOptions {
            id: "fixture.unpinned".into(),
            catalog: Some(catalog),
            index: None,
            public_key: None,
            platform: Some(host_platform()),
            format: None,
            into: Some(dir.0.join("target")),
            dry_run: true,
            open: false,
            overwrite: false,
        })
        .unwrap_err();
        assert!(
            error.starts_with("Fixture Unpinned has no pinned sha256"),
            "{error}"
        );
        assert!(error.contains("https://example.com/fixture.zip"), "{error}");
    }

    #[test]
    fn a_hand_off_build_needs_open_and_reports_no_target() {
        let target = TempDir::new("target");
        let options = InstallOptions {
            id: "airwindows.consolidated".into(),
            catalog: None,
            index: None,
            public_key: None,
            // The macOS build is the disk-image hand-off under test; a dry run
            // plans any platform, so the answer is the same on every host.
            platform: Some("macos-aarch64".into()),
            format: None,
            into: Some(target.0.clone()),
            dry_run: true,
            open: false,
            overwrite: false,
        };
        let report = install(&options).expect("plan resolves");
        assert_eq!(report.mode, "installer");
        assert_eq!(report.kind, "dmg");
    }
}
