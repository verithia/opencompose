//! Instrument packs on disk: one file per pack under the user's soundfont
//! folder plus a small registry, so the app knows what it has without a rescan.
//!
//! An install streams into `<id>.sf3.part`, checks the catalog's size and
//! sha256, then renames into place; every failure removes the partial file.
//! The pack list itself comes from the signed plugin index (`packs`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::plugin_store::{self, DownloadMismatch, PackEntry, PackLicense};

/// Schema written into `<dir>/index.json`. Unknown versions are refused.
pub const REGISTRY_SCHEMA: &str = "opencompose.soundfonts.v1";
/// Installed-pack registry file name.
pub const REGISTRY_FILE: &str = "index.json";
/// Installed packs are `<id>.sf3`; a preset's `file:` names that path.
pub const PACK_EXTENSION: &str = "sf3";
/// Overrides the soundfont folder; tests and portable installs use it.
pub const DIR_ENV: &str = "OCP_SOUNDFONTS_DIR";

/// Default folder: `~/.opencompose/soundfonts/`.
pub fn dir() -> PathBuf {
    if let Some(path) = std::env::var_os(DIR_ENV) {
        return PathBuf::from(path);
    }
    crate::private_fs::config_dir().join("soundfonts")
}

/// Path of an installed pack file.
pub fn pack_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.{PACK_EXTENSION}"))
}

fn part_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.{PACK_EXTENSION}.part"))
}

/// Path of the registry file.
pub fn registry_file(dir: &Path) -> PathBuf {
    dir.join(REGISTRY_FILE)
}

/// One pack the registry records as installed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub bytes: u64,
    pub sha256: String,
    pub license: PackLicense,
    /// Absolute path of the installed `.sf3`; a preset `file:` uses it.
    pub path: String,
    pub installed_at: u64,
}

/// One row of the user-facing list: an index pack, an installed pack, or both.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SoundfontItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub bytes: u64,
    pub license: PackLicense,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub installed_bytes: Option<u64>,
    pub path: Option<String>,
    /// `index` when the signed catalog offers it, `local` when only the disk
    /// still has it.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
/// Installed and discovered soundfonts.
pub struct SoundfontListing {
    pub dir: String,
    pub items: Vec<SoundfontItem>,
    /// Read-only local files; these are not managed/downloaded packs.
    pub discovered: Vec<DiscoveredSoundfont>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    schema: String,
    #[serde(default)]
    packs: Vec<InstalledPack>,
}

/// Reads the registry. A missing file is an empty registry; a file that does
/// not parse is an error, never silently replaced.
fn load(dir: &Path) -> Result<Vec<InstalledPack>, String> {
    let path = registry_file(dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "failed to read soundfont registry '{}': {error}",
                path.display()
            ));
        }
    };
    let registry: Registry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid soundfont registry '{}': {error}", path.display()))?;
    if registry.schema != REGISTRY_SCHEMA {
        return Err(format!(
            "unsupported soundfont registry schema '{}' in '{}'",
            registry.schema,
            path.display()
        ));
    }
    Ok(registry.packs)
}

fn save(dir: &Path, packs: &[InstalledPack]) -> Result<(), String> {
    let registry = Registry {
        schema: REGISTRY_SCHEMA.to_string(),
        packs: packs.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&registry)
        .map_err(|error| format!("failed to serialize the soundfont registry: {error}"))?;
    let path = registry_file(dir);
    crate::private_fs::write_atomic(&path, &bytes)
        .map_err(|error| format!("failed to write '{}': {error}", path.display()))
}

/// Packs the registry knows and the disk still has.
pub fn installed(dir: &Path) -> Result<Vec<InstalledPack>, String> {
    Ok(load(dir)?
        .into_iter()
        .filter(|pack| Path::new(&pack.path).is_file())
        .collect())
}

/// Everything the index offers, joined with what is installed locally.
pub fn listing(dir: &Path, packs: &[PackEntry]) -> Result<SoundfontListing, String> {
    let on_disk = installed(dir)?;
    let mut items: Vec<SoundfontItem> = Vec::new();
    for pack in packs {
        let local = on_disk.iter().find(|row| row.id == pack.id);
        items.push(SoundfontItem {
            id: pack.id.clone(),
            name: pack.name.clone(),
            version: pack.version.clone(),
            description: pack.description.clone(),
            bytes: pack.bytes,
            license: pack.license.clone(),
            installed: local.is_some(),
            installed_version: local.map(|row| row.version.clone()),
            installed_bytes: local.map(|row| row.bytes),
            path: local.map(|row| row.path.clone()),
            source: "index".into(),
        });
    }
    for row in &on_disk {
        if items.iter().any(|item| item.id == row.id) {
            continue;
        }
        items.push(SoundfontItem {
            id: row.id.clone(),
            name: row.name.clone(),
            version: row.version.clone(),
            description: String::new(),
            bytes: row.bytes,
            license: row.license.clone(),
            installed: true,
            installed_version: Some(row.version.clone()),
            installed_bytes: Some(row.bytes),
            path: Some(row.path.clone()),
            source: "local".into(),
        });
    }
    Ok(SoundfontListing {
        dir: dir.display().to_string(),
        items,
        discovered: discover(dir),
    })
}

/// An existing SF2/SF3 candidate for `presets_add(source="soundfont", file=path)`.
/// Discovery checks the container signature; explicit inspection validates the full bank.
/// Discovery never registers, installs or deletes the file, nor claims its license.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoveredSoundfont {
    pub name: String,
    pub path: String,
    pub format: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
/// One program in a SoundFont.
pub struct SoundfontProgram {
    pub name: String,
    pub bank: u16,
    pub program: u16,
    pub zones: usize,
}

#[derive(Debug, Clone, Serialize)]
/// Result of inspecting a SoundFont file.
pub struct SoundfontInspection {
    /// True means the engine parser succeeded; warnings below are non-fatal.
    pub validated: bool,
    pub path: String,
    pub name: Option<String>,
    pub compressed: bool,
    pub total_programs: usize,
    pub matching_programs: usize,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    pub programs: Vec<SoundfontProgram>,
    pub warning_count: usize,
    pub warnings_omitted: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct SoundfontMetadata {
    path: String,
    name: Option<String>,
    compressed: bool,
    programs: Vec<SoundfontProgram>,
    warning_count: usize,
    warnings: Vec<String>,
}

// Cache metadata only, never the decoded sample bank. Paging should not decode the bank again.
type InspectionCache = Option<(
    PathBuf,
    u64,
    Option<SystemTime>,
    std::time::Instant,
    SoundfontMetadata,
)>;
static INSPECTION_CACHE: std::sync::Mutex<InspectionCache> = std::sync::Mutex::new(None);
const INSPECTION_PAGE: usize = 32;
const INSPECTION_MAX_PAGE: usize = 64;
const INSPECTION_WARNINGS: usize = 8;

fn font_metadata(path: String, font: oc_engine::sf2::SoundFont) -> SoundfontMetadata {
    let warning_count = font.warnings.len();
    // Thousands of per-zone parser warnings can repeat the same issue. Keep a few
    // examples and the original count rather than flooding the model's context.
    let mut warnings = Vec::new();
    for warning in font.warnings {
        let bounded: String = warning.chars().take(240).collect();
        if !warnings.contains(&bounded) {
            warnings.push(bounded);
            if warnings.len() == INSPECTION_WARNINGS {
                break;
            }
        }
    }
    SoundfontMetadata {
        path,
        name: font.name.map(|name| name.chars().take(128).collect()),
        compressed: font.compressed,
        programs: font
            .presets
            .into_iter()
            .map(|preset| SoundfontProgram {
                name: preset.name.chars().take(128).collect(),
                bank: preset.bank,
                program: preset.program,
                zones: preset.zones.len(),
            })
            .collect(),
        warning_count,
        warnings,
    }
}

fn inspection_page(
    metadata: &SoundfontMetadata,
    bank: Option<u16>,
    query: Option<&str>,
    offset: usize,
    limit: usize,
) -> SoundfontInspection {
    let limit = limit.clamp(1, INSPECTION_MAX_PAGE);
    let query = query.unwrap_or("").trim().to_lowercase();
    let matching: Vec<_> = metadata
        .programs
        .iter()
        .filter(|program| {
            bank.is_none_or(|bank| program.bank == bank)
                && program.name.to_lowercase().contains(&query)
        })
        .collect();
    let end = offset.saturating_add(limit).min(matching.len());
    SoundfontInspection {
        validated: true,
        path: metadata.path.clone(),
        name: metadata.name.clone(),
        compressed: metadata.compressed,
        total_programs: metadata.programs.len(),
        matching_programs: matching.len(),
        offset,
        limit,
        next_offset: (end < matching.len()).then_some(end),
        programs: matching
            .into_iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect(),
        warning_count: metadata.warning_count,
        warnings_omitted: metadata
            .warning_count
            .saturating_sub(metadata.warnings.len()),
        warnings: metadata.warnings.clone(),
    }
}

/// Explicit inspection validates with the engine parser, without opening an audio device.
/// Results contain bounded metadata pages only; the expensive decoded sample bank is dropped.
pub fn inspect_page(
    path: &Path,
    bank: Option<u16>,
    query: Option<&str>,
    offset: usize,
    limit: Option<usize>,
) -> Result<SoundfontInspection, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("SoundFont inspection requires a regular file, not a symlink".into());
    }
    if metadata.len() > 128 * 1024 * 1024 {
        return Err("SoundFont inspection is limited to 128 MiB files".into());
    }
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    let modified = metadata.modified().ok();
    let mut cache = INSPECTION_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some((prior, bytes, timestamp, when, font)) = cache.as_ref()
        && prior == &path
        && *bytes == metadata.len()
        && *timestamp == modified
        && when.elapsed() < std::time::Duration::from_secs(60)
    {
        return Ok(inspection_page(
            font,
            bank,
            query,
            offset,
            limit.unwrap_or(INSPECTION_PAGE),
        ));
    }
    let font = oc_engine::sf2::SoundFont::load(&path).map_err(|error| error.to_string())?;
    let font = font_metadata(path.display().to_string(), font);
    let page = inspection_page(&font, bank, query, offset, limit.unwrap_or(INSPECTION_PAGE));
    *cache = Some((
        path,
        metadata.len(),
        modified,
        std::time::Instant::now(),
        font,
    ));
    Ok(page)
}

#[cfg(test)]
fn inspect(path: &Path) -> Result<SoundfontInspection, String> {
    inspect_page(path, None, None, 0, None)
}

const DISCOVERY_ENTRIES: usize = 20_000;
const DISCOVERY_DEPTH: usize = 6;
type DiscoveryCache = Option<(Vec<PathBuf>, std::time::Instant, Vec<DiscoveredSoundfont>)>;
static DISCOVERY_CACHE: std::sync::Mutex<DiscoveryCache> = std::sync::Mutex::new(None);

/// Small, known instrument locations only; never recurse through the whole home directory.
fn discovery_roots(dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![dir.to_path_buf()];
    if let Some(file) = std::env::var_os("OCP_SOUNDFONT") {
        roots.push(PathBuf::from(file));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for relative in [
            "Documents/SoundFonts",
            "Documents/soundfonts",
            "Music/SoundFonts",
            "Library/Audio/Sounds/Banks",
        ] {
            roots.push(home.join(relative));
        }
    }
    for path in [
        "/Library/Audio/Sounds/Banks",
        "/usr/share/soundfonts",
        "/usr/share/sounds/sf2",
        "/usr/local/share/soundfonts",
    ] {
        roots.push(PathBuf::from(path));
    }
    if let Ok(catalogue) =
        crate::content_library::load(&crate::content_library::default_catalogue_path())
    {
        roots.extend(
            catalogue
                .libraries
                .into_iter()
                .map(|library| PathBuf::from(library.path)),
        );
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        roots.push(parent.join("soundfonts"));
        roots.push(parent.join("../Resources/soundfonts"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("soundfonts"));
    }
    roots.dedup();
    roots
}

/// Cached for 15 seconds to avoid rescanning on every agent context request.
/// Roots form part of the cache key, so profile/catalogue changes take effect immediately.
pub fn discover(dir: &Path) -> Vec<DiscoveredSoundfont> {
    let roots = discovery_roots(dir);
    let mut cache = DISCOVERY_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some((previous, when, files)) = cache.as_ref()
        && previous == &roots
        && when.elapsed() < std::time::Duration::from_secs(15)
    {
        return files.clone();
    }
    let files = discover_roots(&roots, DISCOVERY_ENTRIES, DISCOVERY_DEPTH);
    *cache = Some((roots, std::time::Instant::now(), files.clone()));
    files
}

fn discover_roots(
    roots: &[PathBuf],
    max_entries: usize,
    max_depth: usize,
) -> Vec<DiscoveredSoundfont> {
    use std::io::Read;
    let mut remaining = max_entries;
    let mut pending: std::collections::VecDeque<_> =
        roots.iter().cloned().map(|root| (root, 0)).collect();
    let mut files = std::collections::BTreeMap::new();
    while let Some((path, depth)) = pending.pop_front() {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if depth >= max_depth {
                continue;
            }
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            // Bound allocation as well as traversal, including directory entries we skip.
            for entry in entries
                .take(remaining.saturating_sub(pending.len()))
                .flatten()
            {
                let child = entry.path();
                if matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "node_modules" | "target" | ".venv")
                ) {
                    continue;
                }
                pending.push_back((child, depth + 1));
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let format = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(format.as_str(), "sf2" | "sf3") {
            continue;
        }
        // Extension alone is not evidence of a SoundFont; DLS is a different RIFF form.
        let mut header = [0_u8; 12];
        if fs::File::open(&path)
            .and_then(|mut file| file.read_exact(&mut header))
            .is_err()
            || &header[..4] != b"RIFF"
            || &header[8..] != b"sfbk"
        {
            continue;
        }
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        files.insert(
            path.clone(),
            DiscoveredSoundfont {
                name: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: path.display().to_string(),
                format,
                bytes: metadata.len(),
            },
        );
    }
    files.into_values().collect()
}

/// Removes the partial file unless the install disarmed it.
struct PartFile {
    path: PathBuf,
    keep: bool,
}

impl PartFile {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: true }
    }

    fn disarm(&mut self) {
        self.keep = false;
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Download, verify, rename, record. The pack URL always comes from the active
/// index; the caller never supplies one.
pub async fn install(
    authority: &crate::plugin_index::CatalogAuthority,
    dir: &Path,
    packs: &[PackEntry],
    id: &str,
    progress: impl FnMut(u64, u64),
) -> Result<InstalledPack, String> {
    authority.check()?;
    let pack = packs
        .iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| format!("Unknown instrument pack id '{id}'"))?;
    crate::private_fs::ensure_dir(dir)
        .map_err(|error| format!("failed to create '{}': {error}", dir.display()))?;
    let part_path = part_file(dir, id);
    let mut part = PartFile::new(part_path.clone());
    // A cancelled attempt leaves its id in the registry; this one starts clean.
    plugin_store::clear_cancel(id);
    let (bytes, sha256) = plugin_store::download_to_part(&pack.url, &part_path, id, progress)
        .await
        .map_err(|error| format!("Instrument pack '{id}' was not downloaded: {error}"))?;
    plugin_store::verify_download(bytes, &sha256, Some(pack.bytes), Some(&pack.sha256)).map_err(
        |mismatch| match mismatch {
            DownloadMismatch::Bytes { actual, expected } => format!(
                "Instrument pack '{id}' is {actual} bytes, the index declares {expected}; nothing was installed"
            ),
            DownloadMismatch::Sha256 => format!(
                "Instrument pack '{id}' failed sha256 verification; nothing was installed"
            ),
        },
    )?;
    if plugin_store::cancelled(id) {
        return Err(format!("{id} was cancelled"));
    }
    let _publication = authority.publication_guard()?;
    let target = pack_file(dir, id);
    fs::rename(&part_path, &target).map_err(|error| {
        format!(
            "failed to move '{}' to '{}': {error}",
            part_path.display(),
            target.display()
        )
    })?;
    part.disarm();
    // A pack keeps its path across reinstalls, so the bank parsed from the old
    // file must go now rather than when the next load happens to notice.
    oc_engine::sf2::SoundFont::invalidate_shared(&target);
    let record = InstalledPack {
        id: pack.id.clone(),
        name: pack.name.clone(),
        version: pack.version.clone(),
        bytes: pack.bytes,
        sha256: pack.sha256.clone(),
        license: pack.license.clone(),
        path: target.display().to_string(),
        installed_at: unix_now(),
    };
    let mut registry: Vec<InstalledPack> =
        load(dir)?.into_iter().filter(|row| row.id != id).collect();
    registry.push(record.clone());
    registry.sort_by(|left, right| left.id.cmp(&right.id));
    save(dir, &registry)?;
    Ok(record)
}

/// Deletes the pack file and its registry row. Refused when the pack is not
/// installed, so a typo cannot silently do nothing.
pub fn remove(dir: &Path, id: &str) -> Result<InstalledPack, String> {
    let mut registry = load(dir)?;
    let index = registry
        .iter()
        .position(|row| row.id == id)
        .ok_or_else(|| format!("Instrument pack '{id}' is not installed"))?;
    let record = registry.remove(index);
    // Drop the parse before the file goes: the path still resolves, which is
    // what the cache key needs to name the entry.
    oc_engine::sf2::SoundFont::invalidate_shared(Path::new(&record.path));
    match fs::remove_file(&record.path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("failed to remove '{}': {error}", record.path));
        }
    }
    let _ = fs::remove_file(part_file(dir, id));
    save(dir, &registry)?;
    Ok(record)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::JoinHandle;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-soundfonts-{tag}-{}-{}",
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

    #[test]
    fn inspection_pages_filter_programs_and_bound_payload() {
        let font = SoundfontMetadata {
            path: "/fonts/test.sf3".into(),
            name: Some("Test".into()),
            compressed: true,
            programs: (0..200)
                .map(|index| SoundfontProgram {
                    name: if index % 2 == 0 {
                        "Grand Piano".into()
                    } else {
                        "Strings".into()
                    },
                    bank: index / 100,
                    program: index % 100,
                    zones: 4,
                })
                .collect(),
            warning_count: 0,
            warnings: vec![],
        };
        let first = inspection_page(&font, Some(0), Some("PIANO"), 0, 32);
        assert!(first.validated);
        assert_eq!(first.total_programs, 200);
        assert_eq!(first.matching_programs, 50);
        assert_eq!(first.programs.len(), 32);
        assert_eq!(first.next_offset, Some(32));
        assert!(
            first
                .programs
                .iter()
                .all(|program| program.bank == 0 && program.name == "Grand Piano")
        );
        let second = inspection_page(&font, Some(0), Some("piano"), 32, 32);
        assert_eq!(second.programs.len(), 18);
        assert_eq!(second.next_offset, None);
        assert_eq!(
            inspection_page(&font, None, None, 0, usize::MAX)
                .programs
                .len(),
            64
        );
        assert!(
            inspection_page(&font, None, None, usize::MAX, 32)
                .programs
                .is_empty()
        );
        assert_eq!(
            inspection_page(&font, None, None, usize::MAX, 32).next_offset,
            None
        );
        let value = serde_json::to_value(first).unwrap();
        assert!(value.get("samples").is_none());
        assert!(serde_json::to_vec(&value).unwrap().len() < 8_000);
    }

    #[test]
    fn inspection_summarizes_thousands_of_nonfatal_warnings() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../oc-engine/tests/fixtures/minimal-test.sf2");
        let mut font = oc_engine::sf2::SoundFont::load(&path).unwrap();
        font.warnings = (0..10_000)
            .map(|index| format!("warning {index}: {}", "x".repeat(1_000)))
            .collect();
        let metadata = font_metadata("fixture.sf2".into(), font);
        let page = inspection_page(&metadata, None, None, 0, 32);
        assert!(page.validated);
        assert_eq!(page.warning_count, 10_000);
        assert_eq!(page.warnings.len(), 8);
        assert_eq!(page.warnings_omitted, 9_992);
        assert!(
            page.warnings
                .iter()
                .all(|warning| warning.chars().count() <= 240)
        );
        assert!(serde_json::to_vec(&page).unwrap().len() < 8_000);
    }

    #[test]
    fn inspection_lists_fixture_programs_through_the_engine_parser() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../oc-engine/tests/fixtures/minimal-test.sf2");
        let inspection = inspect(&path).unwrap();
        assert!(!inspection.programs.is_empty());
        assert!(
            inspection
                .programs
                .iter()
                .any(|program| program.bank == 0 && program.program == 0 && program.zones > 0)
        );
    }

    #[test]
    fn discovered_font_can_be_added_assigned_and_loaded_without_playback() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../oc-engine/tests/fixtures/minimal-test.sf2")
            .canonicalize()
            .unwrap();
        let discovered = discover_roots(std::slice::from_ref(&path), 10, 1);
        assert_eq!(discovered.len(), 1);
        let state = crate::state::AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project =
                oc_dsl::parse("@project\n@track keys | instrument: piano/acoustic_grand\n")
                    .unwrap();
            crate::commands::add_preset_inner(
                &mut inner,
                "Local piano".into(),
                "soundfont".into(),
                Some(0),
                None,
                None,
                Some(discovered[0].path.clone()),
            )
            .unwrap();
        }
        let snapshot =
            crate::commands::set_track_preset_blocking(&state, 0, Some("Local piano".into()))
                .unwrap();
        assert_eq!(
            snapshot.project.tracks[0].preset.as_deref(),
            Some("Local piano")
        );
        let preset = snapshot
            .project
            .presets
            .iter()
            .find(|preset| preset.name == "Local piano")
            .unwrap();
        assert_eq!(preset.file.as_deref(), Some(discovered[0].path.as_str()));
        assert_eq!(preset.program, Some(0));
        // No note, render or output stream: construct the same source used by playback.
        let source = oc_engine::block_source::BlockSource::new(
            Some(preset),
            &preset.params,
            Path::new("missing-default.sf2"),
            48_000,
            120.0,
            None,
        )
        .unwrap();
        assert!(matches!(
            source,
            oc_engine::block_source::BlockSource::SoundFont(_)
        ));
    }

    #[test]
    #[ignore = "requires an explicitly supplied local font; parser only, no audio playback"]
    fn inspect_local_soundfont_parser_only() {
        let path =
            PathBuf::from(std::env::var_os("OCP_TEST_SOUNDFONT").expect("set OCP_TEST_SOUNDFONT"));
        let font = oc_engine::sf2::SoundFont::load(&path).unwrap();
        let raw_programs: Vec<_> = font.presets.iter().map(|preset| serde_json::json!({
            "name": preset.name, "bank": preset.bank, "program": preset.program, "zones": preset.zones.len(),
        })).collect();
        let before_bytes = serde_json::to_vec(
            &serde_json::json!({"programs": raw_programs, "warnings": font.warnings}),
        )
        .unwrap()
        .len();
        let metadata = font_metadata(path.display().to_string(), font);
        let inspection = inspection_page(&metadata, Some(0), Some("piano"), 0, INSPECTION_PAGE);
        assert!(
            inspection
                .programs
                .iter()
                .any(|program| program.bank == 0 && program.program == 0 && program.zones > 0)
        );
        let after_bytes = serde_json::to_vec(&serde_json::json!({"inspection": inspection}))
            .unwrap()
            .len();
        assert!(after_bytes < 12_000);
        println!(
            "SoundFont {:?}: {} total programs; {} matches; warnings={}; original JSON={} bytes; compact filtered JSON={} bytes; program0={:?}",
            inspection.name,
            inspection.total_programs,
            inspection.matching_programs,
            inspection.warning_count,
            before_bytes,
            after_bytes,
            inspection
                .programs
                .iter()
                .find(|program| program.bank == 0 && program.program == 0)
                .map(|program| &program.name)
        );
    }

    #[test]
    fn discovery_finds_unregistered_sf2_sf3_without_mutating_the_registry() {
        let temp = TempDir::new("discovery");
        let nested = temp.0.join("bank");
        fs::create_dir(&nested).unwrap();
        fs::write(temp.0.join("piano.sf2"), b"RIFF\0\0\0\0sfbk").unwrap();
        fs::write(nested.join("strings.SF3"), b"RIFF\0\0\0\0sfbk").unwrap();
        fs::write(temp.0.join("system.dls"), b"RIFF\0\0\0\0DLS ").unwrap();
        fs::write(temp.0.join("wrong.sf2"), b"RIFF\0\0\0\0DLS ").unwrap();
        fs::write(temp.0.join("invalid.sf3"), b"not a font").unwrap();
        let files = discover_roots(&[temp.0.clone(), nested], 100, 6);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|file| file.format == "sf2"));
        assert!(files.iter().any(|file| file.format == "sf3"));
        assert!(files.iter().all(|file| Path::new(&file.path).is_absolute()));
        assert!(!registry_file(&temp.0).exists());
        assert!(installed(&temp.0).unwrap().is_empty());
        let listing = listing(&temp.0, &[]).unwrap();
        assert!(listing.items.is_empty());
        assert!(listing.discovered.iter().any(|file| {
            file.path
                == temp
                    .0
                    .join("piano.sf2")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
        }));
        assert!(remove(&temp.0, "piano").is_err());
        assert!(temp.0.join("piano.sf2").exists());
    }

    #[test]
    fn discovery_respects_depth_and_entry_limits() {
        let temp = TempDir::new("discovery-limits");
        let deep = temp.0.join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("deep.sf2"), b"RIFF\0\0\0\0sfbk").unwrap();
        assert!(discover_roots(std::slice::from_ref(&temp.0), 100, 2).is_empty());
        assert!(discover_roots(std::slice::from_ref(&temp.0), 1, 8).is_empty());
        assert_eq!(
            discover_roots(std::slice::from_ref(&temp.0), 100, 8).len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new("discovery-links");
        let external = TempDir::new("discovery-external");
        let file = external.0.join("outside.sf2");
        fs::write(&file, b"RIFF\0\0\0\0sfbk").unwrap();
        symlink(&file, temp.0.join("alias.sf2")).unwrap();
        symlink(&external.0, temp.0.join("directory")).unwrap();
        assert!(discover_roots(std::slice::from_ref(&temp.0), 100, 6).is_empty());
        assert!(discover_roots(&[temp.0.join("directory")], 100, 6).is_empty());
    }

    /// One-shot HTTP server: answers the first request with `status` and up to
    /// `body` bytes, then closes. `declared` overrides Content-Length so a
    /// truncated response can be scripted.
    fn serve(
        status: &'static str,
        body: Vec<u8>,
        declared: Option<usize>,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pack.sf3", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request);
            let length = declared.unwrap_or(body.len());
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(header.as_bytes());
            let _ = socket.write_all(&body);
            let _ = socket.flush();
        });
        (url, handle)
    }

    fn pack(url: &str, bytes: &[u8], sha256: &str) -> PackEntry {
        PackEntry {
            kind: "soundfont".into(),
            id: "ms-basic".into(),
            name: "MS Basic".into(),
            version: "1.0.0".into(),
            description: "General MIDI sound set.".into(),
            url: url.into(),
            bytes: bytes.len() as u64,
            sha256: sha256.into(),
            license: PackLicense {
                name: "MIT".into(),
                url: "https://example.com/MS%20Basic_License.md".into(),
                text: "MuseScore_General, MIT, derived from FluidR3Mono and FluidR3.".into(),
            },
        }
    }

    fn payload() -> Vec<u8> {
        let mut bytes = Vec::new();
        for index in 0..2048u32 {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        bytes
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        plugin_store::hex(&Sha256::digest(bytes))
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tauri::async_runtime::block_on(future)
    }

    fn install(dir: &Path, packs: &[PackEntry]) -> Result<InstalledPack, String> {
        block_on(super::install(
            &crate::plugin_index::CatalogAuthority::Bundled,
            dir,
            packs,
            "ms-basic",
            |_, _| {},
        ))
    }

    #[test]
    fn an_installed_pack_is_verified_written_and_recorded() {
        let dir = TempDir::new("install");
        let bytes = payload();
        let hash = sha256(&bytes);
        let (url, server) = serve("200 OK", bytes.clone(), None);
        let packs = vec![pack(&url, &bytes, &hash)];
        let record = install(&dir.0, &packs).expect("the pack installs");
        server.join().unwrap();

        let target = pack_file(&dir.0, "ms-basic");
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(record.path, target.display().to_string());
        assert_eq!(record.version, "1.0.0");
        assert!(!part_file(&dir.0, "ms-basic").exists());
        let registry = installed(&dir.0).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].id, "ms-basic");
        assert_eq!(registry[0].sha256, hash);
        assert_eq!(registry[0].bytes, bytes.len() as u64);
        assert_eq!(registry[0].license.name, "MIT");
        assert!(registry[0].installed_at > 0);
    }

    #[test]
    fn a_sha256_mismatch_is_refused_and_leaves_no_file() {
        let dir = TempDir::new("sha");
        let bytes = payload();
        let (url, server) = serve("200 OK", bytes.clone(), None);
        let packs = vec![pack(&url, &bytes, &"1".repeat(64))];
        let error = install(&dir.0, &packs).unwrap_err();
        server.join().unwrap();

        assert_eq!(
            error,
            "Instrument pack 'ms-basic' failed sha256 verification; nothing was installed"
        );
        assert!(!pack_file(&dir.0, "ms-basic").exists());
        assert!(!part_file(&dir.0, "ms-basic").exists());
        assert!(installed(&dir.0).unwrap().is_empty());
        let entries: Vec<_> = fs::read_dir(&dir.0).unwrap().flatten().collect();
        assert!(
            entries.is_empty(),
            "nothing may be left behind: {entries:?}"
        );
    }

    #[test]
    fn a_missing_pack_is_refused_with_its_id() {
        let dir = TempDir::new("missing");
        let bytes = payload();
        let (url, server) = serve("404 Not Found", Vec::new(), None);
        let packs = vec![pack(&url, &bytes, &sha256(&bytes))];
        let error = install(&dir.0, &packs).unwrap_err();
        server.join().unwrap();

        assert!(
            error.contains("Instrument pack 'ms-basic' was not downloaded"),
            "{error}"
        );
        assert!(error.contains("HTTP 404"), "{error}");
        assert!(!pack_file(&dir.0, "ms-basic").exists());
        assert!(!part_file(&dir.0, "ms-basic").exists());
    }

    #[test]
    fn an_aborted_download_leaves_no_part_file() {
        let dir = TempDir::new("aborted");
        let bytes = payload();
        // Content-Length promises far more than the body delivers, so the
        // read fails part-way and the install must clean up.
        let (url, server) = serve("200 OK", bytes[..512].to_vec(), Some(bytes.len()));
        let packs = vec![pack(&url, &bytes, &sha256(&bytes))];
        let error = install(&dir.0, &packs).unwrap_err();
        server.join().unwrap();

        assert!(
            error.contains("Instrument pack 'ms-basic' was not downloaded"),
            "{error}"
        );
        assert!(!part_file(&dir.0, "ms-basic").exists());
        assert!(!pack_file(&dir.0, "ms-basic").exists());
    }

    #[test]
    fn an_unknown_pack_id_is_refused() {
        let dir = TempDir::new("unknown");
        let bytes = payload();
        let packs = vec![pack(
            "https://example.com/pack.sf3",
            &bytes,
            &sha256(&bytes),
        )];
        let error = block_on(super::install(
            &crate::plugin_index::CatalogAuthority::Bundled,
            &dir.0,
            &packs,
            "nope",
            |_, _| {},
        ))
        .unwrap_err();
        assert_eq!(error, "Unknown instrument pack id 'nope'");
    }

    #[test]
    fn the_listing_reports_installed_and_available_packs() {
        let dir = TempDir::new("listing");
        let bytes = payload();
        let hash = sha256(&bytes);
        let (url, server) = serve("200 OK", bytes.clone(), None);
        let mut packs = vec![pack(&url, &bytes, &hash)];
        install(&dir.0, &packs).unwrap();
        server.join().unwrap();
        packs.push(PackEntry {
            kind: "soundfont".into(),
            id: "gs-extra".into(),
            name: "GS Extra".into(),
            version: "2.0.0".into(),
            description: "A second pack that is not installed.".into(),
            url: "https://example.com/gs-extra.sf3".into(),
            bytes: 1024,
            sha256: "2".repeat(64),
            license: PackLicense {
                name: "CC0".into(),
                url: "https://example.com/cc0".into(),
                text: "Public domain.".into(),
            },
        });

        let listing = listing(&dir.0, &packs).unwrap();
        assert_eq!(listing.dir, dir.0.display().to_string());
        assert_eq!(listing.items.len(), 2);
        let installed_item = listing
            .items
            .iter()
            .find(|item| item.id == "ms-basic")
            .unwrap();
        assert!(installed_item.installed);
        assert_eq!(installed_item.installed_version.as_deref(), Some("1.0.0"));
        assert_eq!(installed_item.source, "index");
        assert_eq!(
            installed_item.path.as_deref(),
            Some(pack_file(&dir.0, "ms-basic").to_string_lossy().as_ref())
        );
        let available = listing
            .items
            .iter()
            .find(|item| item.id == "gs-extra")
            .unwrap();
        assert!(!available.installed);
        assert!(available.installed_version.is_none());
        assert_eq!(available.bytes, 1024);
    }

    #[test]
    fn a_locally_installed_pack_the_index_dropped_still_lists() {
        let dir = TempDir::new("local");
        let bytes = payload();
        let hash = sha256(&bytes);
        let (url, server) = serve("200 OK", bytes.clone(), None);
        let packs = vec![pack(&url, &bytes, &hash)];
        install(&dir.0, &packs).unwrap();
        server.join().unwrap();

        let listing = listing(&dir.0, &[]).unwrap();
        assert_eq!(listing.items.len(), 1);
        assert_eq!(listing.items[0].source, "local");
        assert!(listing.items[0].installed);
        assert_eq!(listing.items[0].name, "MS Basic");
    }

    #[test]
    fn removing_a_pack_deletes_the_file_and_its_registry_row() {
        let dir = TempDir::new("remove");
        let bytes = payload();
        let hash = sha256(&bytes);
        let (url, server) = serve("200 OK", bytes.clone(), None);
        let packs = vec![pack(&url, &bytes, &hash)];
        install(&dir.0, &packs).unwrap();
        server.join().unwrap();

        let removed = remove(&dir.0, "ms-basic").expect("the pack is installed");
        assert_eq!(removed.id, "ms-basic");
        assert!(!pack_file(&dir.0, "ms-basic").exists());
        assert!(installed(&dir.0).unwrap().is_empty());
        assert_eq!(
            remove(&dir.0, "ms-basic").unwrap_err(),
            "Instrument pack 'ms-basic' is not installed"
        );
    }
}
