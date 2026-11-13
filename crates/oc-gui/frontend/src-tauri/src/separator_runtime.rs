//! Managed, offline separator packages from the signed plugin index.
use super::*;

#[cfg(any(unix, test))]
use oc_market::catalog_wire::POSIX_EXECUTABLES;
pub use oc_market::catalog_wire::RuntimeDescriptor;
#[cfg(test)]
use oc_market::catalog_wire::{MODEL_FILES, WINDOWS_PYTHON, WINDOWS_REQUIRED};
use oc_market::catalog_wire::{WINDOWS_RUNNER, runtime_path, safe_component};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    id: String,
    name: String,
    version: String,
    platform: String,
    archive_sha256: String,
    runtime: RuntimeDescriptor,
}
const MAX_EXPANDED: u64 = 3 * 1024 * 1024 * 1024;
pub(super) fn validate_descriptor(
    entry: &CatalogEntry,
    install: &CatalogInstall,
) -> Result<(), String> {
    oc_market::catalog_wire::validate_runtime(&entry.document, install, "install")
        .map_err(|error| error.to_string())
}
pub(crate) fn root() -> Option<PathBuf> {
    // A test build keeps installed runtimes in the process scratch root.
    Some(crate::private_fs::config_dir().join("runtimes"))
}
fn destination(root: &Path, entry: &CatalogEntry, install: &CatalogInstall) -> PathBuf {
    root.join(&entry.id).join(format!(
        "{}-{}-{}",
        entry.version.as_deref().unwrap_or_default(),
        install.platform,
        &install.sha256.as_deref().unwrap_or_default()[..16]
    ))
}
fn read_receipt(dir: &Path) -> Option<Receipt> {
    let path = dir.join("receipt.json");
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 128 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
fn regular(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = sanitize_zip_path(relative)?;
    let mut p = root.to_path_buf();
    for c in relative.components() {
        p.push(c);
        if fs::symlink_metadata(&p)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Runtime contains a symlink".into());
        }
    }
    if !p.is_file() {
        return Err(format!("Missing runtime file {}", p.display()));
    }
    Ok(p)
}
fn verify_files(dir: &Path, r: &RuntimeDescriptor) -> Result<(), String> {
    for file in &r.required_files {
        let path = regular(dir, &file.path)?;
        let mut input = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = input.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        if hex(&hash.finalize()) != file.sha256 {
            return Err(format!("Runtime file failed sha256: {}", file.path));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in POSIX_EXECUTABLES
            .iter()
            .copied()
            .filter(|_| r.layout_version == 1)
        {
            if fs::metadata(dir.join(f))
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o111
                == 0
            {
                return Err(format!("Runtime executable is not executable: {f}"));
            }
        }
    }
    Ok(())
}
fn ready(dir: &Path, entry: &CatalogEntry, install: &CatalogInstall) -> bool {
    read_receipt(dir).is_some_and(|r| {
        r.id == entry.id
            && Some(r.version.as_str()) == entry.version.as_deref()
            && r.platform == install.platform
            && Some(r.archive_sha256.as_str()) == install.sha256.as_deref()
            && Some(&r.runtime) == install.runtime.as_ref()
            && verify_files(dir, &r.runtime).is_ok()
    })
}
pub(super) async fn install(
    authority: &CatalogAuthority,
    entries: &[CatalogEntry],
    entry: &CatalogEntry,
    install: &CatalogInstall,
) -> Result<InstallOutcome, String> {
    if install.platform != host_platform() {
        return Err("Separator runtime platform does not match host".into());
    }
    let root = root().ok_or("No user runtime directory")?;
    install_from_catalog(authority, &root, entries, entry, install).await
}

async fn install_from_catalog(
    authority: &CatalogAuthority,
    root: &Path,
    entries: &[CatalogEntry],
    entry: &CatalogEntry,
    install: &CatalogInstall,
) -> Result<InstallOutcome, String> {
    use tokio::io::AsyncWriteExt;

    authority.check()?;
    validate_descriptor(entry, install)?;
    let dest = destination(root, entry, install);
    let parent = dest.parent().ok_or("Invalid runtime destination")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Keep the lock file at a stable path so overlapping app processes and
    // callers agree on the same installation owner.
    let lock_path = parent.join(".install.lock");
    let lock = loop {
        match crate::private_fs::lock(&lock_path, false, true) {
            Ok(lock) => break lock,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(format!("Could not lock separator installation: {e}")),
        }
    };
    if ready(&dest, entry, install) {
        authority.check()?;
        return Ok(outcome(&dest));
    }
    if dest.exists() {
        return Err(invalid_destination());
    }

    let client = catalog_download_client(&install.url, entries)?;
    let file = tempfile::tempfile_in(parent)
        .map_err(|error| format!("Could not create separator download: {error}"))?;
    let mut target = DownloadTarget::File {
        file: tokio::fs::File::from_std(file),
        len: 0,
        sha256: Box::new(Sha256::new()),
    };
    fetch_resumable_into(
        &client,
        &install.url,
        install.bytes,
        MAX_RUNTIME_DOWNLOAD_BYTES,
        &mut target,
    )
    .await?;
    let DownloadTarget::File {
        mut file,
        len,
        sha256,
    } = target
    else {
        unreachable!()
    };
    file.flush()
        .await
        .map_err(|error| format!("Could not finish separator download: {error}"))?;
    verify_download(
        len,
        &hex(&sha256.finalize()),
        install.bytes,
        install.sha256.as_deref(),
    )
    .map_err(|error| error.to_string())?;
    let mut file = file.into_std().await;
    let entry = entry.clone();
    let install = install.clone();
    let authority = authority.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Seek;
        let _lock = lock;
        let _publication = authority.publication_guard()?;
        file.rewind()
            .map_err(|error| format!("Could not read separator download: {error}"))?;
        install_archive(&dest, &entry, &install, file)
    })
    .await
    .map_err(|error| format!("Separator installer stopped: {error}"))?
}
fn outcome(dest: &Path) -> InstallOutcome {
    InstallOutcome::Installed {
        path: dest.display().to_string(),
        message: "Separator runtime installed and ready for offline WAV separation.".into(),
    }
}
#[cfg(test)]
fn install_bytes(
    root: &Path,
    entry: &CatalogEntry,
    install: &CatalogInstall,
    bytes: &[u8],
) -> Result<InstallOutcome, String> {
    validate_descriptor(entry, install)?;
    verify_download(
        bytes.len() as u64,
        &hex(&Sha256::digest(bytes)),
        install.bytes,
        install.sha256.as_deref(),
    )
    .map_err(|_| "Runtime archive integrity mismatch")?;
    let dest = destination(root, entry, install);
    install_archive(&dest, entry, install, bytes)
}

fn invalid_destination() -> String {
    "Runtime destination exists but failed integrity verification; previous installation was preserved".into()
}

fn install_archive(
    dest: &Path,
    entry: &CatalogEntry,
    install: &CatalogInstall,
    archive: impl Read,
) -> Result<InstallOutcome, String> {
    if ready(dest, entry, install) {
        return Ok(outcome(dest));
    }
    let parent = dest.parent().ok_or("Invalid runtime destination")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging_guard = oc_market::temporary::directory_in(parent, ".staging-")
        .map_err(|error| error.to_string())?;
    let staging = staging_guard.path();
    extract(archive, staging, platform_os(&install.platform)?)?;
    let runtime = install
        .runtime
        .as_ref()
        .ok_or("Missing runtime descriptor")?;
    verify_files(staging, runtime)?;
    let receipt = Receipt {
        id: entry.id.clone(),
        name: entry.name.clone(),
        version: entry.version.clone().unwrap(),
        platform: install.platform.to_string(),
        archive_sha256: install.sha256.clone().unwrap(),
        runtime: runtime.clone(),
    };
    fs::write(
        staging.join("receipt.json"),
        serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if dest.exists() {
        return Err(invalid_destination());
    }
    fs::rename(staging, dest).map_err(|e| format!("Could not publish runtime: {e}"))?;
    Ok(outcome(dest))
}
fn extract(archive: impl Read, dest: &Path, os: HostOs) -> Result<(), String> {
    use oc_market::tar::{TarKind, TarLimits, TarMetadata, visit_tar_gz};
    let mut seen = HashSet::new();
    let limits = TarLimits {
        compressed_bytes: MAX_RUNTIME_DOWNLOAD_BYTES,
        entries: 100_000,
        entry_bytes: 1024 * 1024 * 1024,
        payload_bytes: MAX_EXPANDED,
        expanded_bytes: MAX_EXPANDED + 100_000 * 1024,
        path_bytes: 4096,
        path_depth: 64,
    };
    visit_tar_gz(archive, limits, TarMetadata::None, |entry, payload| {
        if entry.kind == TarKind::Directory && matches!(entry.name.as_str(), "." | "./") {
            return Ok(());
        }
        let path = runtime_path(&entry.name, os)?;
        let key = if os == HostOs::Windows {
            path.to_string_lossy().to_lowercase()
        } else {
            path.to_string_lossy().into_owned()
        };
        if key == "receipt.json" || !seen.insert(key) {
            return Err("Reserved or duplicate runtime archive path".into());
        }
        let target = dest.join(&path);
        if entry.kind == TarKind::Directory {
            fs::create_dir_all(&target).map_err(|error| error.to_string())?;
        } else {
            fs::create_dir_all(target.parent().ok_or("Invalid runtime path")?)
                .map_err(|error| error.to_string())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| error.to_string())?;
            if std::io::copy(payload, &mut output).map_err(|error| error.to_string())? != entry.size
            {
                return Err("Truncated runtime file".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode((entry.mode & 0o111) | 0o600),
                )
                .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    })
}

fn receipts(root: &Path, requested_id: Option<&str>) -> Vec<(PathBuf, Receipt)> {
    let mut found = Vec::new();
    if let Some(id) = requested_id {
        if safe_component(id) {
            let path = root.join(id);
            if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                collect_receipts(&path, &mut found);
            }
        }
    } else if let Ok(ids) = fs::read_dir(root) {
        for id in ids.take(128).flatten() {
            if id.file_type().is_ok_and(|kind| kind.is_dir()) {
                collect_receipts(&id.path(), &mut found);
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn collect_receipts(directory: &Path, found: &mut Vec<(PathBuf, Receipt)>) {
    let (Some(id), Ok(versions)) = (directory.file_name(), fs::read_dir(directory)) else {
        return;
    };
    for version in versions.take(32).flatten() {
        if !version.file_type().is_ok_and(|kind| kind.is_dir())
            || version.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let path = version.path();
        if let Some(receipt) = read_receipt(&path)
            && id == receipt.id.as_str()
            && receipt.platform == host_platform()
            && platform_os(&receipt.platform)
                .is_ok_and(|os| receipt.runtime.validate_layout(os).is_ok())
            && verify_files(&path, &receipt.runtime).is_ok()
        {
            found.push((path, receipt));
        }
    }
}
pub(super) fn installed() -> Vec<InstalledPlugin> {
    root()
        .map(|root| {
            receipts(&root, None)
                .into_iter()
                .map(|(p, r)| InstalledPlugin {
                    name: r.name,
                    format: "standalone".into(),
                    path: p.display().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}
pub(crate) struct ResolvedRuntime {
    pub id: String,
    pub launcher: PathBuf,
    pub version: String,
    pub python_runner: Option<PathBuf>,
}

impl ResolvedRuntime {
    fn from_receipt(dir: PathBuf, receipt: Receipt) -> Self {
        let launcher = dir.join(receipt.runtime.launcher.split('/').collect::<PathBuf>());
        Self {
            id: receipt.id,
            launcher,
            version: receipt.version,
            python_runner: (receipt.runtime.layout_version == 2)
                .then(|| dir.join(WINDOWS_RUNNER.split('/').collect::<PathBuf>())),
        }
    }

    pub(crate) fn command(&self) -> oc_host::child_process::CommandSpec {
        let mut command = oc_host::child_process::CommandSpec::new(&self.launcher);
        if let Some(runner) = &self.python_runner {
            command.args(["-I", "-B"]).arg(runner);
        }
        command
    }
}

pub(crate) fn resolve(id: Option<&str>) -> Option<ResolvedRuntime> {
    resolve_in(&root()?, id)
}

pub(crate) fn resolve_in(root: &Path, id: Option<&str>) -> Option<ResolvedRuntime> {
    receipts(root, id)
        .into_iter()
        .next_back()
        .map(|(dir, receipt)| ResolvedRuntime::from_receipt(dir, receipt))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive(files: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, data, kind) in files {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..108].copy_from_slice(b"0000755\0");
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
    fn platform_fixture(platform: &str) -> (CatalogEntry, Vec<u8>) {
        let (version, launcher, executables) = if platform == "windows-x86_64" {
            (2, WINDOWS_PYTHON, WINDOWS_REQUIRED)
        } else {
            (1, POSIX_EXECUTABLES[0], POSIX_EXECUTABLES)
        };
        let files: Vec<_> = executables
            .iter()
            .chain(MODEL_FILES)
            .map(|p| (*p, &b"fixture"[..], b'0'))
            .collect();
        let bytes = archive(&files);
        let required_files: Vec<_> = files.iter().map(|(path, data, _)| {
            serde_json::json!({"path": path, "sha256": hex(&Sha256::digest(data))})
        }).collect();
        let entry = serde_json::from_value(serde_json::json!({
            "id": "spleeter", "name": "Spleeter", "vendor": "Deezer",
            "version": "2.4.2", "license": "MIT", "tags": ["separator"],
            "installs": [{
                "kind": "direct", "platform": platform, "format": "standalone",
                "archive": "tar-gz", "url": "https://example.com/spleeter.tar.gz",
                "sha256": hex(&Sha256::digest(&bytes)), "bytes": bytes.len(),
                "runtime": {"provider": "spleeter", "layout_version": version,
                    "launcher": launcher, "required_files": required_files}
            }]
        }))
        .unwrap();
        (entry, bytes)
    }
    fn fixture() -> (CatalogEntry, Vec<u8>) {
        platform_fixture(&host_platform())
    }
    fn temp() -> (PathBuf, tempfile::TempDir) {
        let directory =
            oc_market::temporary::directory_in(&std::env::temp_dir(), "oc-runtime-").unwrap();
        (directory.path().to_path_buf(), directory)
    }
    #[test]
    fn separator_runtime_install_is_atomic_idempotent_and_detectable() {
        let (root, _cleanup) = temp();
        let (entry, bytes) = fixture();
        let install = &entry.installs[0];
        install_bytes(&root, &entry, install, &bytes).unwrap();
        let dest = destination(&root, &entry, install);
        assert!(ready(&dest, &entry, install));
        let first = fs::read(dest.join("receipt.json")).unwrap();
        install_bytes(&root, &entry, install, &bytes).unwrap();
        assert_eq!(first, fs::read(dest.join("receipt.json")).unwrap());
        assert_eq!(receipts(&root, None).len(), 1);
        fs::write(dest.join("models/2stems/model.index"), b"bad").unwrap();
        assert!(!ready(&dest, &entry, install));
        assert!(install_bytes(&root, &entry, install, &bytes).is_err());
        assert_eq!(
            fs::read(dest.join("models/2stems/model.index")).unwrap(),
            b"bad"
        );
    }
    #[test]
    fn separator_runtime_resolves_the_requested_catalog_id() {
        let (root, _cleanup) = temp();
        let (mut entry, bytes) = fixture();
        install_bytes(&root, &entry, &entry.installs[0], &bytes).unwrap();
        entry.id = "z.other".into();
        install_bytes(&root, &entry, &entry.installs[0], &bytes).unwrap();
        let selected = resolve_in(&root, Some("spleeter")).unwrap();
        assert_eq!(selected.id, "spleeter");
        assert!(selected.launcher.starts_with(root.join("spleeter")));
        assert_eq!(resolve_in(&root, None).unwrap().id, "z.other");
        assert!(resolve_in(&root, Some("missing")).is_none());
    }
    #[test]
    fn separator_runtime_accepts_root_directories_but_refuses_receipt_aliases() {
        for root in [".", "./"] {
            let (destination, _cleanup) = temp();
            let bytes = archive(&[(root, b"", b'5'), ("./bin/runner", b"binary", b'0')]);
            extract(bytes.as_slice(), &destination, HostOs::Linux).unwrap();
            assert_eq!(fs::read(destination.join("bin/runner")).unwrap(), b"binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(destination.join("bin/runner"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o711
                );
            }
            let (destination, _cleanup) = temp();
            let bytes = archive(&[(root, b"", b'5'), ("./receipt.json", b"untrusted", b'0')]);
            assert!(
                extract(bytes.as_slice(), &destination, HostOs::Windows)
                    .unwrap_err()
                    .contains("Reserved")
            );
            assert!(!destination.join("receipt.json").exists());
        }
    }

    #[test]
    fn separator_runtime_late_checksum_failure_never_publishes_a_runtime() {
        let (root, _cleanup) = temp();
        let (entry, archive) = fixture();
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(archive.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw[1024] ^= 1;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        let archive = encoder.finish().unwrap();
        fs::write(root.join("keep"), b"old").unwrap();
        let destination = root.join("installed");
        let error = install_archive(&destination, &entry, &entry.installs[0], archive.as_slice())
            .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(!destination.exists());
        assert_eq!(fs::read(root.join("keep")).unwrap(), b"old");
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    }

    #[test]
    fn separator_runtime_rejects_integrity_missing_models_and_unsafe_archives() {
        let (root, _cleanup) = temp();
        let (mut entry, bytes) = fixture();
        entry.installs[0]
            .runtime
            .as_mut()
            .unwrap()
            .required_files
            .pop();
        assert!(validate_descriptor(&entry, &entry.installs[0]).is_err());
        let (entry, bytes2) = fixture();
        assert!(
            install_bytes(&root, &entry, &entry.installs[0], &bytes[..bytes.len() - 1]).is_err()
        );
        for (path, kind) in [
            ("../escape", b'0'),
            ("/absolute", b'0'),
            ("linked", b'2'),
            ("hardlink", b'1'),
            ("fifo", b'6'),
            ("metadata", b'x'),
        ] {
            let (dir, _c) = temp();
            assert!(
                extract(
                    archive(&[(path, b"x", kind)]).as_slice(),
                    &dir,
                    HostOs::Linux
                )
                .is_err(),
                "{path}"
            );
        }
        let mut missing = entry.clone();
        missing.installs[0].runtime.as_mut().unwrap().required_files[0].sha256 = "0".repeat(64);
        assert!(install_bytes(&root, &missing, &missing.installs[0], &bytes2).is_err());
        assert!(receipts(&root, None).is_empty());
    }
    #[test]
    fn separator_runtime_windows_layout_and_command() {
        let (root, _cleanup) = temp();
        let root = root.join("directory with spaces");
        let (entry, bytes) = platform_fixture("windows-x86_64");
        let install = &entry.installs[0];
        install_bytes(&root, &entry, install, &bytes).unwrap();
        let dest = destination(&root, &entry, install);
        assert!(ready(&dest, &entry, install));
        let receipt = read_receipt(&dest).unwrap();
        let runtime = ResolvedRuntime::from_receipt(dest.clone(), receipt);
        let command = runtime.command();
        assert_eq!(Path::new(command.get_program()), dest.join(WINDOWS_PYTHON));
        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(args.len(), 3);
        assert_eq!(args[..2], ["-I", "-B"]);
        assert_eq!(Path::new(args[2]), dest.join(WINDOWS_RUNNER));

        let mut invalid = entry.clone();
        invalid.installs[0].runtime.as_mut().unwrap().layout_version = 1;
        assert!(validate_descriptor(&invalid, &invalid.installs[0]).is_err());
        let mut invalid = entry;
        invalid.installs[0]
            .runtime
            .as_mut()
            .unwrap()
            .required_files
            .retain(|f| f.path != WINDOWS_RUNNER);
        assert!(validate_descriptor(&invalid, &invalid.installs[0]).is_err());
    }

    #[test]
    fn separator_runtime_windows_archive_refuses_device_names_and_aliases() {
        for files in [
            vec![("python/NUL.dll", &b"x"[..], b'0')],
            vec![("python/runtime.dll.", &b"x"[..], b'0')],
            vec![("RECEIPT.JSON", &b"x"[..], b'0')],
            vec![
                ("python/file", &b"x"[..], b'0'),
                ("python/FILE", &b"x"[..], b'0'),
            ],
        ] {
            let (dir, _cleanup) = temp();
            assert!(extract(archive(&files).as_slice(), &dir, HostOs::Windows).is_err());
        }
    }

    #[tokio::test]
    async fn separator_runtime_concurrent_installs_share_one_download() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut entry, bytes) = fixture();
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        entry.installs[0].url = format!("http://{}/runtime.tar.gz", server.local_addr().unwrap());
        let serve = tokio::spawn(async move {
            let (mut conn, _) = server.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(conn.read_u8().await.unwrap());
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            conn.write_all(response.as_bytes()).await.unwrap();
            conn.write_all(&bytes).await.unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let entries = std::slice::from_ref(&entry);
        let install = &entry.installs[0];
        let (first, second) = tokio::join!(
            install_from_catalog(
                &CatalogAuthority::Bundled,
                root.path(),
                entries,
                &entry,
                install
            ),
            install_from_catalog(
                &CatalogAuthority::Bundled,
                root.path(),
                entries,
                &entry,
                install
            ),
        );
        assert!(matches!(first.unwrap(), InstallOutcome::Installed { .. }));
        assert!(matches!(second.unwrap(), InstallOutcome::Installed { .. }));
        serve.await.unwrap();
        assert!(ready(
            &destination(root.path(), &entry, install),
            &entry,
            install
        ));
        let contents = fs::read_dir(root.path().join(&entry.id)).unwrap().count();
        assert_eq!(contents, 2, "the installed runtime and stable lock file");
    }

    #[tokio::test]
    #[ignore = "Requires a local signed index with the Windows Spleeter archive"]
    async fn separator_runtime_installs_real_windows_archive() {
        use crate::plugin_index::{HttpFetch, IndexClient, IndexSource};

        let url =
            std::env::var("OPENCOMPOSE_INDEX_BASE_URL").expect("set the loopback test index URL");
        let url = reqwest::Url::parse(&url).unwrap();
        assert!(url.host_str().is_some_and(|host| host == "127.0.0.1"));
        assert!(std::env::var_os("OPENCOMPOSE_INDEX_PUBLIC_KEY").is_some());
        let index_dir = tempfile::tempdir().unwrap();
        let index = IndexClient::new(index_dir.path().to_path_buf()).unwrap();
        index
            .refresh(&HttpFetch::new().unwrap(), true)
            .await
            .unwrap();
        let view = index.view().unwrap();
        assert!(matches!(view.source, IndexSource::Remote));
        let id = "deezer.spleeter";
        let platform = "windows-x86_64";
        let plan = pull(
            &view.authority,
            &view.entries,
            id,
            Some(platform),
            None,
            true,
            None,
        )
        .await
        .unwrap();
        assert_eq!(plan.kind, "separator-runtime");

        if cfg!(windows) {
            let result = pull(&view.authority, &view.entries, id, None, None, false, None)
                .await
                .unwrap();
            assert!(result.installed && result.verified);
            let source = PathBuf::from(
                std::env::var_os("OCP_TEST_SEPARATOR_EXCERPT")
                    .expect("set the compressed audio excerpt path"),
            );
            let output = tempfile::tempdir().unwrap();
            let result = crate::agent_api::separate_stems_tool(&serde_json::json!({
                "path": source, "out_dir": output.path(), "runtime_id": id
            }))
            .unwrap();
            assert_eq!(result["method"], "external:spleeter");
            assert_eq!(result["runtime_id"], id);
            assert_eq!(result["input_conversion"]["format"], "wav");
            let stems = result["stems"].as_array().unwrap();
            assert_eq!(stems.len(), 2);
            for name in ["vocals", "accompaniment"] {
                let stem = stems.iter().find(|stem| stem["name"] == name).unwrap();
                let wav = oc_engine::analysis::read_wav(Path::new(stem["path"].as_str().unwrap()))
                    .unwrap();
                assert!(wav.duration_s() > 1.0);
                assert_eq!(wav.sample_rate, 44_100);
            }
            return;
        }

        // Run the same pinned download and extraction on other hosts. The
        // normal pull path correctly refuses to install for a foreign host.
        let entry = view.entries.iter().find(|entry| entry.id == id).unwrap();
        let install = entry
            .installs
            .iter()
            .find(|install| install.platform == platform)
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        install_from_catalog(&view.authority, root.path(), &view.entries, entry, install)
            .await
            .unwrap();
        let dest = destination(root.path(), entry, install);
        assert!(ready(&dest, entry, install));
        let receipt = read_receipt(&dest).unwrap();
        let runtime = ResolvedRuntime::from_receipt(dest, receipt);
        assert!(runtime.launcher.is_file());
        assert!(runtime.python_runner.is_some_and(|runner| runner.is_file()));
    }

    #[tokio::test]
    async fn separator_runtime_pull_dry_run_and_store_share_kind() {
        let (entry, _) = fixture();
        assert!(
            is_panel_install(
                std::slice::from_ref(&entry),
                "remote",
                "spleeter",
                None,
                None
            )
            .unwrap()
        );
        assert_eq!(
            effective_install_kind(&entry.installs[0]),
            Some(InstallKind::SeparatorRuntime)
        );
        let result = pull(
            &CatalogAuthority::Bundled,
            &[entry],
            "spleeter",
            None,
            None,
            true,
            None,
        )
        .await
        .unwrap();
        assert!(!result.installed);
        assert_eq!(result.kind, "separator-runtime");
    }
}
