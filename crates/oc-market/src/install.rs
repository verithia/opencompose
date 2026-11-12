//! Install vocabulary, target selection, and archive extraction.
//!
//! Extraction is path-traversal safe: every entry is sanitised, symlinks are
//! refused, and nothing is written outside the staging directory.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;
use serde::{Deserialize, Serialize};

/// Largest single entry an install archive may expand to.
pub const MAX_ZIP_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
/// Largest total expansion of one install archive.
pub const MAX_ZIP_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Largest entry count one install archive may declare.
pub const MAX_ZIP_ENTRIES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The operating system family an install plan targets.
pub enum HostOs {
    Macos,
    Windows,
    Linux,
}

/// The host OS family, resolved at compile time.
pub fn host_os() -> HostOs {
    if cfg!(target_os = "macos") {
        HostOs::Macos
    } else if cfg!(target_os = "windows") {
        HostOs::Windows
    } else {
        HostOs::Linux
    }
}

/// Platform key used by the distribution catalog, e.g. `macos-aarch64`.
/// Host operating system for a catalog platform key.
pub fn platform_os(platform: &str) -> Result<HostOs, String> {
    match platform {
        "macos-aarch64" | "macos-x86_64" => Ok(HostOs::Macos),
        "windows-x86_64" => Ok(HostOs::Windows),
        "linux-x86_64" => Ok(HostOs::Linux),
        other => Err(format!(
            "Unsupported platform '{other}'; expected macos-aarch64, macos-x86_64, windows-x86_64, or linux-x86_64"
        )),
    }
}

/// The catalog platform key for the running host.
pub fn host_platform() -> String {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-aarch64".into()
        } else {
            "macos-x86_64".into()
        }
    } else if cfg!(target_os = "windows") {
        "windows-x86_64".into()
    } else {
        "linux-x86_64".into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Where an installer is fetched from.
pub enum InstallSource {
    Direct,
    GithubRelease,
    VendorPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
/// Container an install downloads.
pub enum InstallArchive {
    Zip,
    Dmg,
    Pkg,
    TarGz,
    Installer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Plugin or application format a package installs.
pub enum InstallFormat {
    Vst3,
    Vst2,
    Clap,
    Au,
    Lv2,
    Standalone,
}

impl InstallFormat {
    /// Wire spelling used by the catalog and by tool arguments.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vst3 => "vst3",
            Self::Vst2 => "vst2",
            Self::Clap => "clap",
            Self::Au => "au",
            Self::Lv2 => "lv2",
            Self::Standalone => "standalone",
        }
    }

    /// Parse the wire spelling; `None` for an unknown format.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "vst3" => Some(Self::Vst3),
            "vst2" => Some(Self::Vst2),
            "clap" => Some(Self::Clap),
            "au" => Some(Self::Au),
            "lv2" => Some(Self::Lv2),
            "standalone" => Some(Self::Standalone),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Specific packaging shape the installer handles.
pub enum InstallKind {
    Page,
    ZipVst3,
    ZipVst2,
    ZipClap,
    ZipLv2,
    Appimage,
    Pkg,
    Dmg,
    Msi,
    Exe,
    /// A managed separator runtime: a large archive the Store unpacks into its
    /// own directory and validates against a descriptor in the catalog.
    SeparatorRuntime,
    /// An archive that can only hold a vendor installer (AU and standalone
    /// builds have no bundle directory to place), so it is a hand-off.
    InstallerArchive,
}

impl InstallKind {
    /// Wire spelling used by `plugin_pull` and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "vendor_page",
            Self::ZipVst3 => "zip-vst3",
            Self::ZipVst2 => "zip-vst2",
            Self::ZipClap => "zip-clap",
            Self::ZipLv2 => "zip-lv2",
            Self::Appimage => "appimage",
            Self::Pkg => "pkg",
            Self::Dmg => "dmg",
            Self::Msi => "msi",
            Self::Exe => "exe",
            Self::SeparatorRuntime => "separator-runtime",
            Self::InstallerArchive => "installer-archive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Plugin bundle format inside a zip or tar archive.
pub enum BundleFormat {
    Vst3,
    Vst2,
    Clap,
    Lv2,
}

impl BundleFormat {
    /// Bundle extension as the host platform names it: VST2 is a `.vst`
    /// directory on macOS but a bare `.dll`/`.so` file elsewhere.
    fn extension(self, os: HostOs) -> &'static str {
        match (self, os) {
            (Self::Vst3, _) => "vst3",
            (Self::Vst2, HostOs::Macos) => "vst",
            (Self::Vst2, HostOs::Windows) => "dll",
            (Self::Vst2, HostOs::Linux) => "so",
            (Self::Clap, _) => "clap",
            (Self::Lv2, _) => "lv2",
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
/// What an install attempt did and whether it needs the operator.
pub enum InstallOutcome {
    Installed {
        path: String,
        message: String,
    },
    NeedsConfirmation {
        path: String,
        message: String,
    },
    OpenedInstaller {
        path: String,
        message: String,
    },
    /// The archive holds no bundle but exactly one vendor installer. The caller
    /// writes it out and opens it, the same hand-off a bare `.pkg` gets.
    WrappedInstaller {
        name: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Destination directory and whether writing there needs elevation.
pub struct InstallTarget {
    pub dir: PathBuf,
    pub needs_admin: bool,
}

#[derive(Debug, Clone, Default)]
/// Host directories an install plan may write to.
pub struct HostDirs {
    pub home: Option<PathBuf>,
    pub common_program_files: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl HostDirs {
    /// Read the host home and program directories from the environment.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let common_program_files = std::env::var_os("CommonProgramFiles")
            .or_else(|| std::env::var_os("COMMONPROGRAMFILES"))
            .map(PathBuf::from);
        let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        Self {
            home,
            common_program_files,
            local_app_data,
        }
    }
}

// ============================================================================
// Install target selection
// ============================================================================

fn target_name(format: BundleFormat, os: HostOs) -> &'static str {
    match format {
        BundleFormat::Vst3 => "VST3",
        BundleFormat::Vst2 if os == HostOs::Windows => "VST2",
        BundleFormat::Vst2 => "VST",
        BundleFormat::Clap => "CLAP",
        BundleFormat::Lv2 => "LV2",
    }
}

fn linux_bundle_dir(format: BundleFormat) -> &'static str {
    match format {
        BundleFormat::Vst3 => ".vst3",
        BundleFormat::Vst2 => ".vst",
        BundleFormat::Clap => ".clap",
        BundleFormat::Lv2 => ".lv2",
    }
}

fn linux_system_dir(format: BundleFormat) -> &'static str {
    match format {
        BundleFormat::Vst3 => "vst3",
        BundleFormat::Vst2 => "vst",
        BundleFormat::Clap => "clap",
        BundleFormat::Lv2 => "lv2",
    }
}

/// Bundle format for an install kind, when that kind unpacks a bundle.
pub fn bundle_format(kind: InstallKind) -> Option<BundleFormat> {
    match kind {
        InstallKind::ZipVst3 => Some(BundleFormat::Vst3),
        InstallKind::ZipVst2 => Some(BundleFormat::Vst2),
        InstallKind::ZipClap => Some(BundleFormat::Clap),
        InstallKind::ZipLv2 => Some(BundleFormat::Lv2),
        _ => None,
    }
}

/// The install kind that unpacks `format` into a bundle directory.
pub fn bundle_kind(format: BundleFormat) -> InstallKind {
    match format {
        BundleFormat::Vst3 => InstallKind::ZipVst3,
        BundleFormat::Vst2 => InstallKind::ZipVst2,
        BundleFormat::Clap => InstallKind::ZipClap,
        BundleFormat::Lv2 => InstallKind::ZipLv2,
    }
}

/// Pure target selection for a bundle install; writability is supplied so all
/// platforms can be covered by unit tests.
pub fn select_install_target(
    os: HostOs,
    kind: InstallKind,
    format: BundleFormat,
    dirs: &HostDirs,
    writable: impl Fn(&Path) -> bool,
) -> Result<InstallTarget, String> {
    if kind == InstallKind::Appimage {
        if os != HostOs::Linux {
            return Err("AppImage is only supported on Linux".into());
        }
        let home = dirs
            .home
            .as_ref()
            .ok_or_else(|| "Home directory is not available".to_string())?;
        return Ok(InstallTarget {
            dir: home.join(".local/bin"),
            needs_admin: false,
        });
    }

    if !matches!(
        kind,
        InstallKind::ZipVst3 | InstallKind::ZipVst2 | InstallKind::ZipClap | InstallKind::ZipLv2
    ) {
        return Err("This install kind is not a bundle archive".into());
    }

    let folder = target_name(format, os);
    match os {
        HostOs::Macos => {
            let home = dirs
                .home
                .as_ref()
                .ok_or_else(|| "Home directory is not available".to_string())?;
            let user = home.join("Library/Audio/Plug-Ins").join(folder);
            if writable(&user) {
                Ok(InstallTarget {
                    dir: user,
                    needs_admin: false,
                })
            } else {
                Ok(InstallTarget {
                    dir: PathBuf::from("/Library/Audio/Plug-Ins").join(folder),
                    needs_admin: true,
                })
            }
        }
        HostOs::Windows => {
            let common = dirs
                .common_program_files
                .as_ref()
                .map(|base| base.join(folder));
            let local = dirs
                .local_app_data
                .as_ref()
                .map(|base| base.join("Programs/Common").join(folder));
            if let Some(common) = &common
                && writable(common)
            {
                return Ok(InstallTarget {
                    dir: common.clone(),
                    needs_admin: false,
                });
            }
            if let Some(local) = &local
                && writable(local)
            {
                return Ok(InstallTarget {
                    dir: local.clone(),
                    needs_admin: false,
                });
            }
            let dir = common
                .or(local)
                .ok_or_else(|| "No Windows plugin directory is available".to_string())?;
            Ok(InstallTarget {
                dir,
                needs_admin: true,
            })
        }
        HostOs::Linux => {
            let home = dirs
                .home
                .as_ref()
                .ok_or_else(|| "Home directory is not available".to_string())?;
            let user = home.join(linux_bundle_dir(format));
            if writable(&user) {
                Ok(InstallTarget {
                    dir: user,
                    needs_admin: false,
                })
            } else {
                Ok(InstallTarget {
                    dir: PathBuf::from("/usr/lib").join(linux_system_dir(format)),
                    needs_admin: true,
                })
            }
        }
    }
}

/// Whether the nearest existing ancestor is a writable directory.
pub fn path_is_writable(path: &Path) -> bool {
    let mut probe = path;
    while !probe.exists() {
        let Some(parent) = probe.parent() else {
            return false;
        };
        probe = parent;
    }
    if !probe.is_dir() {
        return false;
    }
    crate::temporary::file_in(probe, ".ocp-write-probe-").is_ok()
}

// ============================================================================
// Zip extraction
// ============================================================================

struct ZipEntryMeta {
    name: String,
    method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    local_offset: u64,
    unix_mode: Option<u32>,
    is_dir: bool,
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| "Truncated zip archive".to_string())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "Truncated zip archive".to_string())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn eocd_offset(data: &[u8]) -> Result<usize, String> {
    if data.len() < 22 {
        return Err("Not a zip archive".into());
    }
    let start = data.len() - 22;
    let minimum = start.saturating_sub(65_535);
    for position in (minimum..=start).rev() {
        if data[position..position + 4] == [0x50, 0x4b, 0x05, 0x06] {
            let comment_length = read_u16(data, position + 20)? as usize;
            if position + 22 + comment_length == data.len() {
                return Ok(position);
            }
        }
    }
    Err("Not a zip archive".into())
}

fn parse_zip(data: &[u8]) -> Result<Vec<ZipEntryMeta>, String> {
    let eocd = eocd_offset(data)?;
    let entries = read_u16(data, eocd + 10)? as usize;
    let directory_size = read_u32(data, eocd + 12)? as usize;
    let directory_offset = read_u32(data, eocd + 16)? as usize;
    if entries == u16::MAX as usize
        || directory_size == u32::MAX as usize
        || directory_offset == u32::MAX as usize
    {
        return Err("Zip64 archives are not supported".into());
    }
    if entries > MAX_ZIP_ENTRIES {
        return Err("Zip archive has too many entries".into());
    }
    let directory_end = directory_offset
        .checked_add(directory_size)
        .ok_or_else(|| "Invalid zip directory".to_string())?;
    if directory_end > data.len() {
        return Err("Truncated zip directory".into());
    }

    let mut cursor = directory_offset;
    let mut result = Vec::with_capacity(entries);
    let mut total_size = 0u64;
    for _ in 0..entries {
        if read_u32(data, cursor)? != 0x0201_4b50 {
            return Err("Invalid zip central directory".into());
        }
        let made_by = read_u16(data, cursor + 4)?;
        let flags = read_u16(data, cursor + 8)?;
        let method = read_u16(data, cursor + 10)?;
        let compressed_size = read_u32(data, cursor + 20)? as u64;
        let uncompressed_size = read_u32(data, cursor + 24)? as u64;
        let name_length = read_u16(data, cursor + 28)? as usize;
        let extra_length = read_u16(data, cursor + 30)? as usize;
        let comment_length = read_u16(data, cursor + 32)? as usize;
        let external_attributes = read_u32(data, cursor + 38)?;
        let local_offset = read_u32(data, cursor + 42)? as u64;
        let name_start = cursor + 46;
        let name_end = name_start
            .checked_add(name_length)
            .ok_or_else(|| "Invalid zip entry name".to_string())?;
        let name = std::str::from_utf8(
            data.get(name_start..name_end)
                .ok_or_else(|| "Truncated zip entry name".to_string())?,
        )
        .map_err(|_| "Zip entry names must be valid UTF-8".to_string())?
        .to_string();
        if flags & 0x0001 != 0 {
            return Err("Encrypted zip entries are not supported".into());
        }
        if uncompressed_size > MAX_ZIP_ENTRY_BYTES {
            return Err("Zip entry is larger than the install limit".into());
        }
        total_size = total_size
            .checked_add(uncompressed_size)
            .ok_or_else(|| "Invalid zip archive size".to_string())?;
        if total_size > MAX_ZIP_TOTAL_BYTES {
            return Err("Zip archive expands beyond the install limit".into());
        }
        let host_os = (made_by >> 8) as u8;
        let unix_mode = if host_os == 3 {
            Some(external_attributes >> 16)
        } else {
            None
        };
        let is_dir =
            name.ends_with('/') || unix_mode.is_some_and(|mode| mode & 0o170000 == 0o040000);
        result.push(ZipEntryMeta {
            name,
            method,
            compressed_size,
            uncompressed_size,
            local_offset,
            unix_mode,
            is_dir,
        });
        cursor = name_end
            .checked_add(extra_length)
            .and_then(|value| value.checked_add(comment_length))
            .ok_or_else(|| "Invalid zip entry length".to_string())?;
    }
    Ok(result)
}

fn zip_entry_data(data: &[u8], meta: &ZipEntryMeta) -> Result<Vec<u8>, String> {
    let local = meta.local_offset as usize;
    if read_u32(data, local)? != 0x0403_4b50 {
        return Err("Invalid zip local header".into());
    }
    let name_length = read_u16(data, local + 26)? as usize;
    let extra_length = read_u16(data, local + 28)? as usize;
    let name_start = local + 30;
    let name_end = name_start
        .checked_add(name_length)
        .ok_or_else(|| "Invalid zip local name".to_string())?;
    if data.get(name_start..name_end) != Some(meta.name.as_bytes()) {
        return Err("Zip entry name mismatch".into());
    }
    let data_start = name_end
        .checked_add(extra_length)
        .ok_or_else(|| "Invalid zip local header".to_string())?;
    let data_end = data_start
        .checked_add(meta.compressed_size as usize)
        .ok_or_else(|| "Invalid zip entry size".to_string())?;
    let raw = data
        .get(data_start..data_end)
        .ok_or_else(|| "Truncated zip entry".to_string())?;
    let decoded = match meta.method {
        0 => raw.to_vec(),
        8 => {
            let mut decoded = Vec::with_capacity(
                (meta.uncompressed_size as usize).min(MAX_ZIP_ENTRY_BYTES as usize),
            );
            DeflateDecoder::new(raw)
                .take(MAX_ZIP_ENTRY_BYTES + 1)
                .read_to_end(&mut decoded)
                .map_err(|error| format!("Failed to extract zip entry: {error}"))?;
            decoded
        }
        _ => return Err("Unsupported zip compression method".into()),
    };
    if decoded.len() as u64 != meta.uncompressed_size {
        return Err("Zip entry size does not match its header".into());
    }
    Ok(decoded)
}

/// Resolves an install directory without requiring its missing suffix to exist.
/// Existing components are canonicalized before later parent components, so a
/// symlink followed by `..` has the filesystem's meaning rather than a lexical
/// approximation. No directories are created by resolution.
pub(crate) fn resolve_directory(path: &Path) -> std::io::Result<PathBuf> {
    use std::path::Component;

    let absolute = std::path::absolute(path)?;
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&resolved)?;
                        if !resolved.is_dir() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::NotADirectory,
                                "Install destination contains a non-directory component",
                            ));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
        }
    }
    Ok(resolved)
}

/// Reject absolute paths, traversal, backslashes, control characters, and
/// Windows drive prefixes. Returns a safe relative path.
pub fn sanitize_zip_path(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.len() > 4096 || name.contains('\0') {
        return Err("Invalid zip entry name".into());
    }
    if name.starts_with('/') || name.contains('\\') {
        return Err("Zip entry uses an absolute or Windows path".into());
    }
    let mut result = PathBuf::new();
    for component in name.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err("Zip entry escapes the install directory".into());
        }
        if component.len() > 255
            || component.contains(':')
            || component.chars().any(|character| character.is_control())
        {
            return Err("Zip entry contains an unsafe path component".into());
        }
        result.push(component);
    }
    if result.as_os_str().is_empty() {
        return Err("Zip entry has an empty path".into());
    }
    Ok(result)
}

fn bundle_relative_path(
    path: &Path,
    format: BundleFormat,
    os: HostOs,
) -> Option<(String, PathBuf)> {
    let suffix = format!(".{}", format.extension(os));
    let components: Vec<_> = path.iter().collect();
    let index = components.iter().position(|component| {
        component
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(&suffix)
    })?;
    if inside_bundle_directory(&components[..index]) {
        return None;
    }
    let root = components[index].to_string_lossy().to_string();
    let relative: PathBuf = components[index..].iter().collect();
    Some((root, relative))
}

/// VST2 on Windows and Linux is a bare library, so `.dll`/`.so` also names the
/// libraries inside a `.lv2` or `.vst3` directory. Do not lift those out as
/// VST2 plugins.
fn inside_bundle_directory(ancestors: &[&std::ffi::OsStr]) -> bool {
    ancestors.iter().any(|component| {
        let name = component.to_string_lossy().to_ascii_lowercase();
        [".vst3", ".vst", ".lv2", ".clap"]
            .iter()
            .any(|extension| name.ends_with(extension))
    })
}

/// Extract only the requested bundle type from a zip archive.
/// Scan an archive for exactly one vendor installer. `Au` and `Standalone`
/// builds use this because they have no bundle directory to place.
pub fn unwrap_installer(
    data: &[u8],
    archive: Option<InstallArchive>,
) -> Result<(String, Vec<u8>), String> {
    match archive {
        Some(InstallArchive::TarGz) => {
            let files = tar_entries(data)?;
            let mut installers = Vec::new();
            for file in &files {
                let name = &file.name;
                if file.kind == crate::tar::TarKind::Directory || !is_installer_file(name) {
                    continue;
                }
                let path = sanitize_zip_path(name)?;
                installers.push((
                    path.file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| name.clone()),
                    file.payload.clone(),
                ));
            }
            one_installer(installers)
        }
        _ => {
            let entries = parse_zip(data)?;
            let mut installers = Vec::new();
            for meta in &entries {
                if meta.is_dir || !is_installer_file(&meta.name) {
                    continue;
                }
                let path = sanitize_zip_path(&meta.name)?;
                installers.push((
                    path.file_name()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_else(|| meta.name.clone()),
                    zip_entry_data(data, meta)?,
                ));
            }
            one_installer(installers)
        }
    }
}

fn one_installer(mut installers: Vec<(String, Vec<u8>)>) -> Result<(String, Vec<u8>), String> {
    match installers.len() {
        1 => Ok(installers.pop().expect("one installer")),
        0 => Err("Archive holds no vendor installer".into()),
        count => Err(format!(
            "Archive holds {count} vendor installers; refusing to guess which one to run"
        )),
    }
}

/// Extensions a vendor installer ships with.
fn is_installer_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".pkg",
        ".mpkg",
        ".exe",
        ".msi",
        ".dmg",
        ".appimage",
        ".deb",
        ".rpm",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

/// Extract plugin bundles from a zip archive into the target directory.
pub fn extract_bundles(
    data: &[u8],
    format: BundleFormat,
    os: HostOs,
    target_dir: &Path,
    overwrite: bool,
) -> Result<InstallOutcome, String> {
    let entries = parse_zip(data)?;
    let mut bundles: Vec<String> = Vec::new();
    let mut grouped: Vec<Vec<usize>> = Vec::new();
    for (index, meta) in entries.iter().enumerate() {
        let path = sanitize_zip_path(&meta.name)?;
        if let Some((root, _)) = bundle_relative_path(&path, format, os) {
            if let Some(bundle_index) = bundles
                .iter()
                .position(|existing| existing.eq_ignore_ascii_case(&root))
            {
                if bundles[bundle_index] != root {
                    return Err("Zip archive contains conflicting bundle names".into());
                }
                grouped[bundle_index].push(index);
            } else {
                bundles.push(root);
                grouped.push(vec![index]);
            }
        }
    }
    if bundles.is_empty() {
        if let Ok((name, bytes)) = unwrap_installer(data, Some(InstallArchive::Zip)) {
            return Ok(InstallOutcome::WrappedInstaller { name, bytes });
        }
        return Err(format!(
            "Archive does not contain a .{} bundle",
            format.extension(os)
        ));
    }

    let mut confirm_path = None;
    for root in &bundles {
        let destination = target_dir.join(root);
        if destination.exists() && !overwrite {
            confirm_path = Some(destination);
            break;
        }
    }
    if let Some(path) = confirm_path {
        return Ok(InstallOutcome::NeedsConfirmation {
            path: path.display().to_string(),
            message: format!("{} already exists. Replace it?", path.display()),
        });
    }

    fs::create_dir_all(target_dir)
        .map_err(|error| format!("Failed to create '{}': {error}", target_dir.display()))?;
    let cleanup = crate::temporary::directory_in(target_dir, ".ocp-staging-").map_err(|error| {
        format!(
            "Failed to create staging directory in '{}': {error}",
            target_dir.display()
        )
    })?;
    let staging = cleanup.path();

    let mut extracted = 0u64;
    for indexes in &grouped {
        for index in indexes {
            let meta = &entries[*index];
            let path = sanitize_zip_path(&meta.name)?;
            let (_, relative) = bundle_relative_path(&path, format, os)
                .ok_or_else(|| "Zip bundle path disappeared".to_string())?;
            extracted = extracted
                .checked_add(write_zip_entry(staging, &relative, meta, data)? as u64)
                .ok_or_else(|| "Zip archive expands beyond the install limit".to_string())?;
            if extracted > MAX_ZIP_TOTAL_BYTES {
                return Err("Zip archive expands beyond the install limit".into());
            }
        }
    }

    for root in &bundles {
        let destination = target_dir.join(root);
        if destination.exists() {
            let metadata = fs::symlink_metadata(&destination).map_err(|error| {
                format!("Failed to inspect '{}': {error}", destination.display())
            })?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "Refusing to overwrite a symlink at '{}'",
                    destination.display()
                ));
            }
            if metadata.is_dir() {
                fs::remove_dir_all(&destination).map_err(|error| {
                    format!("Failed to replace '{}': {error}", destination.display())
                })?;
            } else {
                fs::remove_file(&destination).map_err(|error| {
                    format!("Failed to replace '{}': {error}", destination.display())
                })?;
            }
        }
        fs::rename(staging.join(root), &destination).map_err(|error| {
            format!(
                "Failed to move '{}' into place: {error}",
                destination.display()
            )
        })?;
    }

    drop(cleanup);
    let first = target_dir.join(&bundles[0]);
    Ok(InstallOutcome::Installed {
        path: first.display().to_string(),
        message: format!("Installed {} bundle(s).", bundles.len()),
    })
}

/// Removes a staging directory when it goes out of scope.
pub struct StagingCleanup(pub PathBuf);

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_zip_entry(
    staging: &Path,
    relative: &Path,
    meta: &ZipEntryMeta,
    data: &[u8],
) -> Result<usize, String> {
    if relative.is_absolute() {
        return Err("Invalid bundle path".into());
    }
    let destination = staging.join(relative);
    if !destination.starts_with(staging) {
        return Err("Zip entry escapes the staging directory".into());
    }
    if meta
        .unix_mode
        .is_some_and(|mode| mode & 0o170000 == 0o120000)
    {
        return Err("Refusing to extract a symlink from a zip archive".into());
    }
    if meta.is_dir {
        fs::create_dir_all(&destination)
            .map_err(|error| format!("Failed to create '{}': {error}", destination.display()))?;
        apply_mode(&destination, meta.unix_mode)?;
        return Ok(0);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create '{}': {error}", parent.display()))?;
    }
    let bytes = zip_entry_data(data, meta)?;
    let length = bytes.len();
    fs::write(&destination, bytes)
        .map_err(|error| format!("Failed to write '{}': {error}", destination.display()))?;
    apply_mode(&destination, meta.unix_mode)?;
    Ok(length)
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode
        && mode & 0o777 != 0
    {
        if mode & 0o170000 == 0o120000 {
            return Err("Refusing to extract a symlink from a zip archive".into());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).map_err(|error| {
            format!("Failed to set permissions on '{}': {error}", path.display())
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    let _ = (path, mode);
    Ok(())
}

struct ExtractedTarEntry {
    name: String,
    payload: Vec<u8>,
    kind: crate::tar::TarKind,
    mode: u32,
}

/// Read every regular file of a `.tar.gz` into memory, bounded by the install
/// limit. Keeping the whole listing lets extraction and unwrapping share one
/// parser.
fn tar_entries(data: &[u8]) -> Result<Vec<ExtractedTarEntry>, String> {
    use crate::tar::{TarKind, TarLimits, TarMetadata, visit_tar_gz};
    let limits = TarLimits {
        compressed_bytes: MAX_ZIP_TOTAL_BYTES,
        entries: MAX_ZIP_ENTRIES,
        entry_bytes: MAX_ZIP_ENTRY_BYTES,
        payload_bytes: MAX_ZIP_TOTAL_BYTES,
        expanded_bytes: MAX_ZIP_TOTAL_BYTES,
        path_bytes: 4096,
        path_depth: 64,
    };
    let mut files = Vec::new();
    visit_tar_gz(data, limits, TarMetadata::GnuLongName, |entry, payload| {
        if entry.kind == TarKind::Directory && matches!(entry.name.as_str(), "." | "./") {
            return Ok(());
        }
        let mut bytes = Vec::new();
        payload
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        files.push(ExtractedTarEntry {
            name: entry.name.clone(),
            payload: bytes,
            kind: entry.kind,
            mode: entry.mode,
        });
        Ok(())
    })?;
    if files.is_empty() {
        return Err("tar.gz archive has no entries".into());
    }
    Ok(files)
}

/// Extract plugin bundles from a tar archive into the target directory.
pub fn extract_tar_bundles(
    data: &[u8],
    format: BundleFormat,
    os: HostOs,
    target_dir: &Path,
    overwrite: bool,
) -> Result<InstallOutcome, String> {
    let files = tar_entries(data)?;
    let mut bundles: Vec<String> = Vec::new();
    let mut grouped: Vec<Vec<usize>> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let path = sanitize_zip_path(&file.name)?;
        if let Some((root, _)) = bundle_relative_path(&path, format, os) {
            if let Some(bundle_index) = bundles
                .iter()
                .position(|existing| existing.eq_ignore_ascii_case(&root))
            {
                if bundles[bundle_index] != root {
                    return Err("tar.gz archive contains conflicting bundle names".into());
                }
                grouped[bundle_index].push(index);
            } else {
                bundles.push(root);
                grouped.push(vec![index]);
            }
        }
    }
    if bundles.is_empty() {
        if let Ok((name, bytes)) = unwrap_installer(data, Some(InstallArchive::TarGz)) {
            return Ok(InstallOutcome::WrappedInstaller { name, bytes });
        }
        return Err(format!(
            "Archive does not contain a .{} bundle",
            format.extension(os)
        ));
    }
    if !overwrite && bundles.iter().any(|root| target_dir.join(root).exists()) {
        return Ok(InstallOutcome::NeedsConfirmation {
            path: target_dir.join(&bundles[0]).display().to_string(),
            message: "Bundle already exists. Replace it?".into(),
        });
    }
    fs::create_dir_all(target_dir)
        .map_err(|error| format!("Failed to create '{}': {error}", target_dir.display()))?;
    let cleanup = crate::temporary::directory_in(target_dir, ".ocp-staging-").map_err(|error| {
        format!(
            "Failed to create staging directory in '{}': {error}",
            target_dir.display()
        )
    })?;
    let staging = cleanup.path();
    for indexes in &grouped {
        for index in indexes {
            let file = &files[*index];
            let path = sanitize_zip_path(&file.name)?;
            let (_, relative) = bundle_relative_path(&path, format, os)
                .ok_or_else(|| "Tar bundle path disappeared".to_string())?;
            let destination = staging.join(&relative);
            if !destination.starts_with(staging) {
                return Err("Tar entry escapes the staging directory".into());
            }
            if file.kind == crate::tar::TarKind::Directory {
                fs::create_dir_all(&destination).map_err(|error| {
                    format!("Failed to create '{}': {error}", destination.display())
                })?;
            } else {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        format!("Failed to create '{}': {error}", parent.display())
                    })?;
                }
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|error| {
                        format!(
                            "Conflicting tar destination '{}': {error}",
                            destination.display()
                        )
                    })?;
                output.write_all(&file.payload).map_err(|error| {
                    format!("Failed to write '{}': {error}", destination.display())
                })?;
                apply_mode(&destination, Some(file.mode))?;
            }
        }
    }
    for root in &bundles {
        let destination = target_dir.join(root);
        if destination.exists() {
            let metadata = fs::symlink_metadata(&destination).map_err(|error| {
                format!("Failed to inspect '{}': {error}", destination.display())
            })?;
            if metadata.file_type().is_symlink() {
                return Err("Refusing to overwrite a symlink".into());
            }
            if metadata.is_dir() {
                fs::remove_dir_all(&destination).map_err(|error| {
                    format!("Failed to replace '{}': {error}", destination.display())
                })?;
            } else {
                fs::remove_file(&destination).map_err(|error| {
                    format!("Failed to replace '{}': {error}", destination.display())
                })?;
            }
        }
        fs::rename(staging.join(root), &destination)
            .map_err(|error| format!("Failed to move '{}': {error}", destination.display()))?;
    }
    drop(cleanup);
    Ok(InstallOutcome::Installed {
        path: target_dir.join(&bundles[0]).display().to_string(),
        message: format!("Installed {} bundle(s).", bundles.len()),
    })
}

/// NUL-terminated text field of a tar header.
pub fn tar_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// Octal number field of a tar header.
pub fn tar_octal(bytes: &[u8]) -> Result<u64, String> {
    crate::tar::octal(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn scratch(tag: &str) -> tempfile::TempDir {
        crate::temporary::directory_in(&std::env::temp_dir(), &format!("oc-market-{tag}-")).unwrap()
    }

    #[test]
    fn an_archive_wrapping_one_installer_is_handed_off() {
        let archive = stored_zip(&[
            ("Podolski/Podolski readme.rtf", b"readme"),
            ("Podolski/Podolski.pkg", b"pkg-bytes"),
        ]);
        let dir = scratch("wrap");
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            dir.path(),
            false,
        )
        .expect("wrapped installer");
        match outcome {
            InstallOutcome::WrappedInstaller { name, bytes } => {
                assert_eq!(name, "Podolski.pkg");
                assert_eq!(bytes, b"pkg-bytes");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(!dir.path().join("Podolski.pkg").exists());
    }

    #[test]
    fn two_installers_in_an_archive_stay_an_error() {
        let archive = stored_zip(&[("Mac/Setup.pkg", b"pkg"), ("Win/Setup.exe", b"exe")]);
        let dir = scratch("two");
        let error = extract_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            dir.path(),
            false,
        )
        .unwrap_err();
        assert!(error.contains("does not contain a .vst3 bundle"), "{error}");
    }

    #[test]
    fn a_plain_bundle_still_wins_over_installer_detection() {
        let archive = stored_zip(&[
            ("Fixture.vst3/Contents/Info.plist", b"<plist/>"),
            ("readme.txt", b"docs"),
        ]);
        let dir = scratch("bundle");
        let outcome = extract_bundles(
            &archive,
            BundleFormat::Vst3,
            HostOs::Macos,
            dir.path(),
            false,
        )
        .expect("bundle install");
        assert!(matches!(outcome, InstallOutcome::Installed { .. }));
        assert!(
            dir.path()
                .join("Fixture.vst3/Contents/Info.plist")
                .is_file()
        );
    }
}
