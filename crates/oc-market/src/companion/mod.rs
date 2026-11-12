//! Verified companion data and plugin bundles published as one rollback operation.
//!
//! Separate destination directories cannot share an atomic filesystem rename.
//! Publication serializes cooperating readers/writers and restores prior files
//! on an error; it does not promise recovery from process or machine failure.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::catalog_wire::{Entry, Install, validate_companion};
use crate::{BundleFormat, HostDirs, HostOs, InstallFormat, InstallOutcome, verify_artifact};

mod extraction;
mod publication;
#[cfg(test)]
mod tests;

const RECEIPT: &str = ".opencompose-content.json";
const RECEIPT_BYTES: u64 = 16 * 1024;
const LOCK: &str = ".opencompose-surge-xt-factory.lock";
const REQUIRED: [&str; 3] = ["patches_factory", "wavetables", "skins"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Descriptor,
    UnsupportedHost,
    Integrity,
    Archive,
    Destination,
    Publication,
    Rollback,
}

#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}
impl Error {
    fn new(code: ErrorCode, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// Explicit host paths; the catalog never supplies a filesystem destination.
#[derive(Debug, Clone)]
pub struct CompanionPaths {
    plugin_dir: PathBuf,
    content_dir: PathBuf,
}
impl CompanionPaths {
    pub fn resolve(
        install: &Install,
        os: HostOs,
        dirs: &HostDirs,
        plugin_dir: &Path,
    ) -> Result<Self, Error> {
        let mut paths = Self {
            plugin_dir: plugin_dir.to_path_buf(),
            content_dir: content_directory(install, os, dirs)?,
        };
        paths.normalize()?;
        Ok(paths)
    }
    fn normalize(&mut self) -> Result<(), Error> {
        let plugin_dir =
            crate::install::resolve_directory(&self.plugin_dir).map_err(destination)?;
        let content_dir =
            crate::install::resolve_directory(&self.content_dir).map_err(destination)?;
        if plugin_dir.starts_with(&content_dir) {
            return Err(destination(
                "Plugin directory cannot be inside the companion data destination",
            ));
        }
        self.plugin_dir = plugin_dir;
        self.content_dir = content_dir;
        Ok(())
    }

    fn check_unchanged(&self) -> Result<(), Error> {
        let mut current = self.clone();
        current.normalize()?;
        if current.plugin_dir != self.plugin_dir || current.content_dir != self.content_dir {
            return Err(destination(
                "Companion destinations changed after preparation",
            ));
        }
        Ok(())
    }

    pub fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }
    pub fn content_dir(&self) -> &Path {
        &self.content_dir
    }
}

fn content_directory(install: &Install, os: HostOs, dirs: &HostDirs) -> Result<PathBuf, Error> {
    validate_companion(install, "install").map_err(|e| Error::new(ErrorCode::Descriptor, e))?;
    if install.companion_content.is_none() {
        return Err(Error::new(
            ErrorCode::Descriptor,
            "Missing companion content",
        ));
    }
    if os != HostOs::Macos || crate::platform_os(install.platform.as_str()).ok() != Some(os) {
        return Err(Error::new(
            ErrorCode::UnsupportedHost,
            "Companion factory content requires a macOS host",
        ));
    }
    let home = dirs
        .home
        .as_ref()
        .ok_or_else(|| destination("No user home directory"))?;
    crate::install::resolve_directory(&home.join("Library/Application Support/Surge XT"))
        .map_err(destination)
}

/// Resolves only the selected build's fixed content location. Readiness does
/// not depend on a plugin install directory or the process working directory.
pub fn ready_for_host(install: &Install, os: HostOs, dirs: &HostDirs) -> Result<bool, Error> {
    if install.companion_content.is_none() {
        return Ok(true);
    }
    ready_at(install, &content_directory(install, os, dirs)?)
}

/// Both buffers are authenticated against the selected catalog descriptor.
pub struct Prepare<'a> {
    pub entry: &'a Entry,
    pub install: &'a Install,
    pub plugin: &'a [u8],
    pub content: &'a [u8],
    pub paths: CompanionPaths,
}

pub struct PreparedCompanion {
    plugin_stage: TempDir,
    content_stage: TempDir,
    paths: CompanionPaths,
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    id: String,
    version: Option<String>,
    sha256: String,
}

pub fn prepare(request: Prepare<'_>) -> Result<PreparedCompanion, Error> {
    let Prepare {
        entry,
        install,
        plugin,
        content,
        mut paths,
    } = request;
    validate_companion(install, "install").map_err(|e| Error::new(ErrorCode::Descriptor, e))?;
    let descriptor = install
        .companion_content
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::Descriptor, "Missing companion content"))?;
    verify_artifact(
        plugin,
        install.sha256.as_deref().unwrap_or_default(),
        install.bytes,
    )
    .map_err(|e| Error::new(ErrorCode::Integrity, e))?;
    verify_artifact(content, &descriptor.sha256, Some(descriptor.bytes))
        .map_err(|e| Error::new(ErrorCode::Integrity, e))?;
    let format = match install.format {
        InstallFormat::Vst3 => BundleFormat::Vst3,
        InstallFormat::Clap => BundleFormat::Clap,
        _ => {
            return Err(Error::new(
                ErrorCode::Descriptor,
                "Unsupported companion plugin format",
            ));
        }
    };
    paths.normalize()?;
    fs::create_dir_all(&paths.plugin_dir).map_err(destination)?;
    fs::create_dir_all(
        paths
            .content_dir
            .parent()
            .ok_or_else(|| destination("Invalid content destination"))?,
    )
    .map_err(destination)?;
    // Directory creation can make previously absent aliases resolvable (for
    // example case-insensitive names); validate before allocating either stage.
    paths.normalize()?;
    let parent = paths
        .content_dir
        .parent()
        .ok_or_else(|| destination("Invalid content destination"))?;
    let plugin_stage = crate::temporary::directory_in(&paths.plugin_dir, ".ocp-content-plugins-")
        .map_err(destination)?;
    let content_stage =
        crate::temporary::directory_in(parent, ".ocp-factory-").map_err(destination)?;
    match crate::extract_bundles(plugin, format, HostOs::Macos, plugin_stage.path(), false)
        .map_err(|e| Error::new(ErrorCode::Archive, e))?
    {
        InstallOutcome::Installed { .. } => {}
        _ => {
            return Err(Error::new(
                ErrorCode::Archive,
                "Companion plugin archive must contain bundle directories",
            ));
        }
    }
    extraction::extract_content(content, content_stage.path(), &descriptor.strip_prefix)
        .map_err(|e| Error::new(ErrorCode::Archive, e))?;
    if !REQUIRED
        .iter()
        .all(|dir| content_stage.path().join(dir).is_dir())
    {
        return Err(Error::new(
            ErrorCode::Archive,
            "Companion factory content is missing required directories",
        ));
    }
    let receipt = Receipt {
        id: entry.id.clone(),
        version: entry.version.clone(),
        sha256: descriptor.sha256.clone(),
    };
    let bytes = serde_json::to_vec(&receipt).map_err(destination)?;
    fs::write(content_stage.path().join(RECEIPT), bytes).map_err(destination)?;
    Ok(PreparedCompanion {
        plugin_stage,
        content_stage,
        paths,
        name: entry.name.clone(),
    })
}

fn destination(error: impl ToString) -> Error {
    Error::new(ErrorCode::Destination, error)
}

fn scope_lock(content_dir: &Path, exclusive: bool) -> Result<Option<File>, Error> {
    let parent = content_dir
        .parent()
        .ok_or_else(|| destination("Invalid content destination"))?;
    let path = parent.join(LOCK);
    if fs::symlink_metadata(&path)
        .is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(destination("Companion lock must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    if exclusive {
        options.write(true).create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if !exclusive && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(destination(error)),
    };
    if exclusive {
        <File as FileExt>::lock(&file)
    } else {
        <File as FileExt>::lock_shared(&file)
    }
    .map_err(destination)?;
    Ok(Some(file))
}

/// Readiness follows the selected build, never the order of other formats.
/// Identical content is reusable across plugin versions.
pub fn ready(install: &Install, paths: &CompanionPaths) -> Result<bool, Error> {
    ready_at(install, &paths.content_dir)
}

fn ready_at(install: &Install, content_dir: &Path) -> Result<bool, Error> {
    validate_companion(install, "install").map_err(|e| Error::new(ErrorCode::Descriptor, e))?;
    let Some(content) = &install.companion_content else {
        return Ok(true);
    };
    if !content_dir.parent().is_some_and(Path::is_dir) {
        return Ok(false);
    }
    let lock = scope_lock(content_dir, false)?;
    let result = read_receipt(content_dir, &content.sha256);
    // Legacy installations have no scope lock. If the first cooperating writer
    // creates it during our read, re-read under its shared lock after publication.
    // Readers never create lock files or mutate the installation.
    if lock.is_none()
        && let Some(_lock) = scope_lock(content_dir, false)?
    {
        return read_receipt(content_dir, &content.sha256);
    }
    result
}

fn read_receipt(content_dir: &Path, sha256: &str) -> Result<bool, Error> {
    if !content_dir.is_dir() {
        return Ok(false);
    }
    let file = match File::open(content_dir.join(RECEIPT)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(destination(e)),
    };
    let mut bytes = Vec::new();
    file.take(RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(destination)?;
    if bytes.len() as u64 > RECEIPT_BYTES {
        return Ok(false);
    }
    let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes) else {
        return Ok(false);
    };
    Ok(receipt.sha256 == sha256 && REQUIRED.iter().all(|dir| content_dir.join(dir).is_dir()))
}
