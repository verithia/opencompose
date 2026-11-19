//! Instrument archives in the managed `sounds` folder.
//!
//! A pack installs into `<dir>/<id>/`, is recorded in `<dir>/packs.json`, and is
//! staged and verified before it replaces anything. A failed download,
//! extraction, content check or registry write leaves the previous install and
//! the registry unchanged.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::plugin_store::{self, DownloadMismatch, PackLicense};
use oc_market::catalog_wire::{SoundArchive, SoundArchivePack, SoundContent};

/// Schema written into `<dir>/packs.json`. Unknown versions are refused.
pub const REGISTRY_SCHEMA: &str = "opencompose.sound_packs.v1";
/// Installed-pack registry file name.
pub const REGISTRY_FILE: &str = "packs.json";
/// Overrides the managed sounds folder; tests and portable installs use it.
pub const DIR_ENV: &str = "OCP_SOUND_PACKS_DIR";

/// Default folder: `~/.opencompose/sounds/`.
pub fn dir() -> PathBuf {
    if let Some(path) = std::env::var_os(DIR_ENV) {
        return PathBuf::from(path);
    }
    crate::private_fs::config_dir().join("sounds")
}

fn pack_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(id)
}

fn part_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!(".{id}.part"))
}

/// Path of the registry file.
pub fn registry_file(dir: &Path) -> PathBuf {
    dir.join(REGISTRY_FILE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Stage a sound-pack install reports to the UI.
pub enum InstallPhase {
    Download,
    Verify,
    Extract,
}

/// One installed pack as the registry stores it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledSoundPack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub content: SoundContent,
    #[serde(default = "default_archive")]
    pub archive: SoundArchive,
    pub bytes: u64,
    pub sha256: String,
    pub license: PackLicense,
    pub path: String,
    pub installed_at: u64,
    pub unpacked_bytes: u64,
    pub files: u64,
}

fn default_archive() -> SoundArchive {
    SoundArchive::File
}

/// One row of the user-facing list: a catalog pack, an installed pack, or both.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SoundPackItem {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub homepage: String,
    pub version: String,
    pub description: String,
    pub content: SoundContent,
    pub archive: SoundArchive,
    pub bytes: u64,
    pub unpacked_bytes: u64,
    pub license: PackLicense,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub path: Option<String>,
    /// `index` when the signed catalog offers it, `local` when only the disk
    /// still has it.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
/// Installed and catalog sound packs.
pub struct SoundPackListing {
    pub dir: String,
    pub items: Vec<SoundPackItem>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    schema: String,
    #[serde(default)]
    packs: Vec<InstalledSoundPack>,
}

fn load(dir: &Path) -> Result<Vec<InstalledSoundPack>, String> {
    let path = registry_file(dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "failed to read sound pack registry '{}': {error}",
                path.display()
            ));
        }
    };
    let registry: Registry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid sound pack registry '{}': {error}", path.display()))?;
    if registry.schema != REGISTRY_SCHEMA {
        return Err(format!(
            "unsupported sound pack registry schema '{}' in '{}'",
            registry.schema,
            path.display()
        ));
    }
    Ok(registry.packs)
}

fn save(dir: &Path, packs: &[InstalledSoundPack]) -> Result<(), String> {
    let registry = Registry {
        schema: REGISTRY_SCHEMA.to_string(),
        packs: packs.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&registry)
        .map_err(|error| format!("failed to serialize the sound pack registry: {error}"))?;
    let path = registry_file(dir);
    crate::private_fs::write_atomic(&path, &bytes)
        .map_err(|error| format!("failed to write '{}': {error}", path.display()))
}

/// Packs the registry knows and the disk still has.
pub fn installed(dir: &Path) -> Result<Vec<InstalledSoundPack>, String> {
    Ok(load(dir)?
        .into_iter()
        .filter(|pack| Path::new(&pack.path).is_dir())
        .collect())
}

/// Everything the index offers, joined with what is installed locally.
pub fn listing(dir: &Path, packs: &[SoundArchivePack]) -> Result<SoundPackListing, String> {
    let on_disk = installed(dir)?;
    let mut items: Vec<SoundPackItem> = Vec::new();
    for pack in packs {
        let local = on_disk.iter().find(|row| row.id == pack.id);
        items.push(SoundPackItem {
            id: pack.id.clone(),
            name: pack.name.clone(),
            publisher: pack.publisher.clone(),
            homepage: pack.homepage.clone(),
            version: pack.version.clone(),
            description: pack.description.clone(),
            content: pack.content,
            archive: pack.archive,
            bytes: pack.bytes,
            unpacked_bytes: pack.unpacked_bytes,
            license: pack.license.clone(),
            installed: local.is_some(),
            installed_version: local.map(|row| row.version.clone()),
            path: local.map(|row| row.path.clone()),
            source: "index".into(),
        });
    }
    for row in &on_disk {
        if items.iter().any(|item| item.id == row.id) {
            continue;
        }
        items.push(SoundPackItem {
            id: row.id.clone(),
            name: row.name.clone(),
            publisher: row.publisher.clone(),
            homepage: String::new(),
            version: row.version.clone(),
            description: String::new(),
            content: row.content,
            archive: row.archive,
            bytes: row.bytes,
            unpacked_bytes: row.unpacked_bytes,
            license: row.license.clone(),
            installed: true,
            installed_version: Some(row.version.clone()),
            path: Some(row.path.clone()),
            source: "local".into(),
        });
    }
    Ok(SoundPackListing {
        dir: dir.display().to_string(),
        items,
    })
}

/// Removes the partial file unless a successful publication consumed it.
struct PartFile {
    path: PathBuf,
}

impl Drop for PartFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A path moved aside until the new install and registry commit succeed.
struct Aside {
    path: PathBuf,
    _hold: Option<tempfile::TempDir>,
}

impl Aside {
    fn take(parent: &Path, path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let hold = oc_market::temporary::directory_in(parent, ".backup-").map_err(|error| {
            format!(
                "failed to reserve a backup in '{}': {error}",
                parent.display()
            )
        })?;
        let aside = hold.path().join("install");
        fs::rename(path, &aside)
            .map_err(|error| format!("failed to move '{}' aside: {error}", path.display()))?;
        Ok(Some(Self {
            path: aside,
            _hold: Some(hold),
        }))
    }

    fn restore(self, original: &Path) -> Result<(), String> {
        fs::rename(&self.path, original)
            .map_err(|error| format!("failed to restore '{}': {error}", original.display()))
    }
}

/// Download, verify, extract, validate, publish and record one catalog pack.
/// The pack URL always comes from the active index; the caller never supplies
/// a URL or a file name.
pub async fn install(
    authority: &crate::plugin_index::CatalogAuthority,
    dir: &Path,
    packs: &[SoundArchivePack],
    id: &str,
    mut progress: impl FnMut(InstallPhase, u64, u64),
) -> Result<InstalledSoundPack, String> {
    authority.check()?;
    if !oc_market::catalog_wire::safe_component(id) {
        return Err(format!("Invalid instrument pack id '{id}'"));
    }
    let pack = packs
        .iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| format!("Unknown sound pack id '{id}'"))?;
    crate::private_fs::ensure_dir(dir)
        .map_err(|error| format!("failed to create '{}': {error}", dir.display()))?;
    let part_path = part_file(dir, id);
    let _part = PartFile {
        path: part_path.clone(),
    };
    plugin_store::clear_cancel(id);
    let (bytes, sha256) =
        plugin_store::download_to_part(&pack.url, &part_path, id, |done, total| {
            progress(InstallPhase::Download, done, total)
        })
        .await
        .map_err(|error| format!("Sound pack '{id}' was not downloaded: {error}"))?;
    progress(InstallPhase::Verify, 0, bytes);
    plugin_store::verify_download(bytes, &sha256, Some(pack.bytes), Some(&pack.sha256))
        .map_err(|mismatch| match mismatch {
            DownloadMismatch::Bytes { actual, expected } => format!(
                "Sound pack '{id}' is {actual} bytes, the index declares {expected}; nothing was installed"
            ),
            DownloadMismatch::Sha256 => format!(
                "Sound pack '{id}' failed sha256 verification; nothing was installed"
            ),
        })?;
    progress(InstallPhase::Verify, bytes, bytes);
    if plugin_store::cancelled(id) {
        return Err(format!("{id} was cancelled"));
    }

    let staging = oc_market::temporary::directory_in(dir, ".staging-")
        .map_err(|error| format!("failed to reserve staging in '{}': {error}", dir.display()))?;
    let staging_path = staging.path().to_path_buf();
    let archive = part_path.clone();
    let limit = oc_market::sound_archive::ExtractLimits::for_pack(pack.unpacked_bytes);
    let format = pack.archive;
    let content = pack.content;
    let file_name = matches!(format, SoundArchive::File)
        .then(|| file_name_from_url(&pack.url))
        .transpose()?;
    progress(InstallPhase::Extract, 0, pack.unpacked_bytes);
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let outcome =
            oc_market::sound_archive::extract(oc_market::sound_archive::ExtractRequest {
                format,
                archive: &archive,
                destination: &staging_path,
                single_file_name: file_name.as_deref(),
                limits: limit,
                progress: None,
            })?;
        validate_content(&staging_path, content)?;
        Ok::<_, String>(outcome)
    })
    .await
    .map_err(|error| format!("Sound pack '{id}' extraction task failed: {error}"))??;
    progress(
        InstallPhase::Extract,
        pack.unpacked_bytes,
        pack.unpacked_bytes,
    );
    if plugin_store::cancelled(id) {
        return Err(format!("{id} was cancelled"));
    }

    let mut registry: Vec<InstalledSoundPack> =
        load(dir)?.into_iter().filter(|row| row.id != id).collect();
    let _publication = authority.publication_guard()?;
    let target = pack_dir(dir, id);
    let aside = Aside::take(dir, &target)?;
    let staged_path = staging.path().to_path_buf();
    if let Err(error) = fs::rename(&staged_path, &target) {
        if let Some(aside) = aside {
            aside.restore(&target)?;
        }
        return Err(format!(
            "failed to move the staged pack into '{}': {error}",
            target.display()
        ));
    }
    let record = InstalledSoundPack {
        id: pack.id.clone(),
        name: pack.name.clone(),
        version: pack.version.clone(),
        publisher: pack.publisher.clone(),
        content: pack.content,
        archive: pack.archive,
        bytes: pack.bytes,
        sha256: pack.sha256.clone(),
        license: pack.license.clone(),
        path: target.display().to_string(),
        installed_at: unix_now(),
        unpacked_bytes: outcome.bytes,
        files: outcome.files,
    };
    registry.push(record.clone());
    registry.sort_by(|left, right| left.id.cmp(&right.id));
    if let Err(error) = save(dir, &registry) {
        let _ = fs::remove_dir_all(&target);
        if let Some(aside) = aside {
            aside.restore(&target)?;
        }
        return Err(error);
    }
    drop(staging);
    fs::remove_file(&part_path).ok();
    Ok(record)
}

/// Deletes the installed folder and its registry row. Refused when the pack is
/// not installed, so a typo cannot silently do nothing.
pub fn remove(dir: &Path, id: &str) -> Result<InstalledSoundPack, String> {
    if !oc_market::catalog_wire::safe_component(id) {
        return Err(format!("Invalid instrument pack id '{id}'"));
    }
    let mut registry = load(dir)?;
    let index = registry
        .iter()
        .position(|row| row.id == id)
        .ok_or_else(|| format!("Sound pack '{id}' is not installed"))?;
    let record = registry.remove(index);
    // The managed folder owns the destination, so a hand-edited registry path
    // cannot direct a removal outside it.
    let target = pack_dir(dir, id);
    let aside = Aside::take(dir, &target)?;
    if let Err(error) = save(dir, &registry) {
        if let Some(aside) = aside {
            aside.restore(&target)?;
        }
        return Err(error);
    }
    fs::remove_file(part_file(dir, id)).ok();
    Ok(record)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn file_name_from_url(url: &str) -> Result<String, String> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let name = path.rsplit('/').next().unwrap_or_default();
    let relative = oc_market::sanitize_zip_path(name)
        .map_err(|_| format!("Sound pack URL has no usable file name: {url}"))?;
    if relative.components().count() != 1 {
        return Err(format!("Sound pack URL has no usable file name: {url}"));
    }
    Ok(relative.to_string_lossy().into_owned())
}

fn validate_content(root: &Path, content: SoundContent) -> Result<(), String> {
    let files = collect_files(root)?;
    let matches = |extension: &str| {
        files.iter().any(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
    };
    let found = match content {
        SoundContent::Soundfont => files.iter().any(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "sf2" | "sf3"))
                && crate::soundfont_store::has_soundfont_header(path)
        }),
        SoundContent::Sfz => matches("sfz"),
        SoundContent::Samples => ["wav", "aif", "aiff", "flac", "ogg", "mp3"]
            .iter()
            .any(|extension| matches(extension)),
    };
    if found {
        Ok(())
    } else {
        Err(format!(
            "Sound pack does not contain the declared {} content",
            content.as_str()
        ))
    }
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((path, depth)) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to read '{}': {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err("Sound pack contains a link or device".into());
        }
        if metadata.is_dir() {
            if depth >= 64 {
                return Err("Sound pack directory depth limit exceeded".into());
            }
            for entry in fs::read_dir(&path)
                .map_err(|error| format!("failed to read '{}': {error}", path.display()))?
            {
                seen = seen.saturating_add(1);
                if seen > oc_market::sound_archive::MAX_ARCHIVE_ENTRIES {
                    return Err("Sound pack file count limit exceeded".into());
                }
                let entry = entry
                    .map_err(|error| format!("failed to read '{}': {error}", path.display()))?;
                pending.push((entry.path(), depth + 1));
            }
        } else if metadata.is_file() {
            files.push(path);
        } else {
            return Err("Sound pack contains a link or device".into());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::JoinHandle;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-sound-packs-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn serve(status: &'static str, body: Vec<u8>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pack.zip", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = std::io::Read::read(&mut socket, &mut request);
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).unwrap();
            socket.write_all(&body).unwrap();
            socket.flush().unwrap();
        });
        (url, handle)
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        plugin_store::hex(&Sha256::digest(bytes))
    }

    fn zip_bytes() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("Piano/piano.sf2", options).unwrap();
        writer.write_all(b"RIFF\0\0\0\0sfbk").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn pack(
        url: &str,
        bytes: &[u8],
        archive: SoundArchive,
        content: SoundContent,
    ) -> SoundArchivePack {
        SoundArchivePack {
            kind: "sound_archive".into(),
            id: "piano".into(),
            name: "Piano".into(),
            publisher: "Publisher".into(),
            homepage: "https://example.test/piano".into(),
            version: "1.0.0".into(),
            description: "A compact piano.".into(),
            content,
            archive,
            url: url.into(),
            bytes: bytes.len() as u64,
            sha256: sha256(bytes),
            unpacked_bytes: 64,
            license: PackLicense {
                name: "CC0-1.0".into(),
                url: "https://example.test/license".into(),
                text: "CC0.".into(),
            },
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tauri::async_runtime::block_on(future)
    }

    fn install(dir: &Path, packs: &[SoundArchivePack]) -> Result<InstalledSoundPack, String> {
        block_on(super::install(
            &crate::plugin_index::CatalogAuthority::Bundled,
            dir,
            packs,
            "piano",
            |_, _, _| {},
        ))
    }

    #[test]
    fn a_zip_pack_installs_and_lists_beside_the_index() {
        let dir = TempDir::new("install");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let packs = vec![pack(
            &url,
            &body,
            SoundArchive::Zip,
            SoundContent::Soundfont,
        )];
        let record = install(&dir.0, &packs).unwrap();
        server.join().unwrap();
        assert_eq!(record.id, "piano");
        assert!(Path::new(&record.path).join("Piano/piano.sf2").is_file());
        assert!(!part_file(&dir.0, "piano").exists());
        let listing = listing(&dir.0, &packs).unwrap();
        assert_eq!(listing.items.len(), 1);
        assert!(listing.items[0].installed);
        assert_eq!(listing.items[0].source, "index");
        assert_eq!(listing.items[0].installed_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn a_sha_mismatch_leaves_nothing_behind() {
        let dir = TempDir::new("sha");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let mut packs = vec![pack(
            &url,
            &body,
            SoundArchive::Zip,
            SoundContent::Soundfont,
        )];
        packs[0].sha256 = "0".repeat(64);
        let error = install(&dir.0, &packs).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("sha256"), "{error}");
        assert!(!pack_dir(&dir.0, "piano").exists());
        assert!(!part_file(&dir.0, "piano").exists());
        assert!(load(&dir.0).unwrap().is_empty());
    }

    #[test]
    fn an_extraction_failure_keeps_the_previous_install_and_registry() {
        let dir = TempDir::new("keep");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let packs = vec![pack(
            &url,
            &body,
            SoundArchive::Zip,
            SoundContent::Soundfont,
        )];
        let first = install(&dir.0, &packs).unwrap();
        server.join().unwrap();

        let bad = b"not a zip".to_vec();
        let (url, server) = serve("200 OK", bad.clone());
        let bad_packs = vec![pack(&url, &bad, SoundArchive::Zip, SoundContent::Soundfont)];
        let error = install(&dir.0, &bad_packs).unwrap_err();
        server.join().unwrap();
        assert!(!error.is_empty());
        assert!(Path::new(&first.path).join("Piano/piano.sf2").is_file());
        let registry = load(&dir.0).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].path, first.path);
    }

    #[test]
    fn a_content_check_failure_leaves_nothing_behind() {
        let dir = TempDir::new("content");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let packs = vec![pack(&url, &body, SoundArchive::Zip, SoundContent::Samples)];
        let error = install(&dir.0, &packs).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("samples"), "{error}");
        assert!(!pack_dir(&dir.0, "piano").exists());
        assert!(load(&dir.0).unwrap().is_empty());
    }

    #[test]
    fn remove_refuses_an_absent_pack_and_deletes_an_installed_one() {
        let dir = TempDir::new("remove");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let packs = vec![pack(
            &url,
            &body,
            SoundArchive::Zip,
            SoundContent::Soundfont,
        )];
        let record = install(&dir.0, &packs).unwrap();
        server.join().unwrap();
        assert!(remove(&dir.0, "piano").is_ok());
        assert!(!Path::new(&record.path).exists());
        assert!(load(&dir.0).unwrap().is_empty());
        assert!(remove(&dir.0, "piano").is_err());
    }

    #[test]
    fn a_local_pack_still_appears_when_the_index_drops_it() {
        let dir = TempDir::new("local");
        let body = zip_bytes();
        let (url, server) = serve("200 OK", body.clone());
        let packs = vec![pack(
            &url,
            &body,
            SoundArchive::Zip,
            SoundContent::Soundfont,
        )];
        install(&dir.0, &packs).unwrap();
        server.join().unwrap();
        let listing = listing(&dir.0, &[]).unwrap();
        assert_eq!(listing.items.len(), 1);
        assert_eq!(listing.items[0].source, "local");
        assert_eq!(listing.items[0].name, "Piano");
        assert_eq!(listing.items[0].content, SoundContent::Soundfont);
    }
}
