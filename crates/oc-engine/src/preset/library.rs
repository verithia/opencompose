//! On-disk user patch library.
//!
//! One JSON file per patch under `~/.opencompose/presets` (override with
//! `OCP_PRESET_LIBRARY_DIR`). Writes go through a temporary file in the same
//! directory and are published with a rename, so a crash leaves either the old
//! file or the new one. A file that does not parse is skipped with a warning;
//! one bad preset never hides the rest of the library.
//!
//! Favourite and last-used are browsing preferences, not project data. A user
//! library patch carries them in its own file; factory patches have no file, so
//! their preferences live under `factory:<name>` in `preferences.json` beside
//! the library directory. That store never becomes a second library.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use oc_dsl::ast::{Effect, ParamValue};
use serde::{Deserialize, Serialize};

use super::{PresetTier, SoundPreset};

/// Extension every library entry uses.
pub const LIBRARY_FILE_EXTENSION: &str = "ocp-preset";

/// Preference store that sits beside the library directory.
pub const PREFERENCES_FILE_NAME: &str = "preferences.json";

/// Factory rows have no file of their own, so the store keys them explicitly.
pub const FACTORY_PREFERENCE_PREFIX: &str = "factory:";

/// Browsing preferences for one patch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchUsage {
    #[serde(default)]
    pub favorite: bool,
    /// Unix seconds; `None` until a load records it.
    #[serde(default)]
    pub last_used_at: Option<u64>,
}

/// Wall clock in Unix seconds, the unit the records store.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Preference store for patches that are not user-library files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetPreferences {
    #[serde(default = "preferences_version")]
    pub version: u32,
    #[serde(default)]
    pub patches: BTreeMap<String, PatchUsage>,
    #[serde(skip)]
    path: PathBuf,
}

impl Default for PresetPreferences {
    fn default() -> Self {
        Self::at(Self::default_path())
    }
}

impl PresetPreferences {
    /// `~/.opencompose/preferences.json`, or the path named by
    /// `OCP_PRESET_PREFERENCES`. It sits beside the library directory so a
    /// library override moves the preferences with it.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("OCP_PRESET_PREFERENCES")
            && !path.is_empty()
        {
            return PathBuf::from(path);
        }
        Self::path_beside(&PresetLibrary::default_dir())
    }

    /// Preferences file beside a library directory.
    pub fn path_beside(library_dir: &Path) -> PathBuf {
        library_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(PREFERENCES_FILE_NAME)
    }

    /// A preference store at an explicit path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            version: preferences_version(),
            patches: BTreeMap::new(),
            path: path.into(),
        }
    }

    /// Effective preference file path.
    pub fn path(&self) -> PathBuf {
        self.path_or_default()
    }

    /// Read the store. A missing or unreadable file is an empty store, never a
    /// failed command, so browsing still works with a damaged preference file.
    pub fn load(&self) -> Self {
        let path = self.path_or_default();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::at(path);
        };
        match serde_json::from_str::<Self>(&content) {
            Ok(mut stored) => {
                stored.path = path;
                stored
            }
            Err(error) => {
                log::warn!(
                    "Ignoring unreadable preference file {}: {error}",
                    path.display()
                );
                Self::at(path)
            }
        }
    }

    /// Write the preference store atomically.
    pub fn save(&self) -> Result<(), String> {
        let path = self.path_or_default();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create preference dir: {error}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| format!("Failed to encode preferences: {error}"))?;
        write_atomically(&path, &json)
    }

    /// Usage record for a factory preset.
    pub fn factory_usage(&self, name: &str) -> PatchUsage {
        self.patches
            .get(&factory_key(name))
            .copied()
            .unwrap_or_default()
    }

    /// Set the favorite flag for a factory preset.
    pub fn set_factory_favorite(&mut self, name: &str, favorite: bool) {
        self.patches.entry(factory_key(name)).or_default().favorite = favorite;
    }

    /// Record when a factory preset was last used.
    pub fn mark_factory_used(&mut self, name: &str, at: u64) {
        self.patches
            .entry(factory_key(name))
            .or_default()
            .last_used_at = Some(at);
    }

    fn path_or_default(&self) -> PathBuf {
        if self.path.as_os_str().is_empty() {
            Self::default_path()
        } else {
            self.path.clone()
        }
    }
}

fn preferences_version() -> u32 {
    1
}

fn factory_key(name: &str) -> String {
    format!("{FACTORY_PREFERENCE_PREFIX}{name}")
}

/// Whether a save created a file or replaced the matching one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Created,
    Replaced,
}

/// One scan of the library directory.
#[derive(Debug, Clone, Default)]
pub struct LibraryScan {
    pub presets: Vec<SoundPreset>,
    /// Browsing preferences keyed by preset name, parallel to `presets`.
    pub usage: BTreeMap<String, PatchUsage>,
    /// Reasons a file was skipped, in file-name order.
    pub warnings: Vec<String>,
}

/// What a scan depends on: each candidate file's path, size and modification
/// time. Sorting keeps the comparison independent of directory order.
fn directory_fingerprint(dir: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut fingerprint: Vec<(PathBuf, u64, Option<std::time::SystemTime>)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case(LIBRARY_FILE_EXTENSION))
        })
        .map(|path| {
            let metadata = std::fs::metadata(&path).ok();
            let length = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
            let modified = metadata.and_then(|meta| meta.modified().ok());
            (path, length, modified)
        })
        .collect();
    fingerprint.sort();
    fingerprint
}

type ScanCache = Option<(
    PathBuf,
    Vec<(PathBuf, u64, Option<std::time::SystemTime>)>,
    LibraryScan,
)>;

fn scan_cache() -> &'static std::sync::Mutex<ScanCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<ScanCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

fn cached_scan(
    dir: &Path,
    fingerprint: &[(PathBuf, u64, Option<std::time::SystemTime>)],
) -> Option<LibraryScan> {
    let cache = scan_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let (cached_dir, cached_fingerprint, scan) = cache.as_ref()?;
    (cached_dir == dir && cached_fingerprint == fingerprint).then(|| scan.clone())
}

fn remember_scan(
    dir: &Path,
    fingerprint: Vec<(PathBuf, u64, Option<std::time::SystemTime>)>,
    scan: &LibraryScan,
) {
    let mut cache = scan_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    *cache = Some((dir.to_path_buf(), fingerprint, scan.clone()));
}

/// Directory-backed patch library.
#[derive(Debug, Clone)]
pub struct PresetLibrary {
    dir: PathBuf,
}

impl Default for PresetLibrary {
    fn default() -> Self {
        Self::at(Self::default_dir())
    }
}

impl PresetLibrary {
    /// User library directory: `~/.opencompose/presets`, or the path named by
    /// `OCP_PRESET_LIBRARY_DIR` when that is set.
    ///
    /// A test build never resolves the operator's home: without the override it
    /// answers a process-keyed directory under the system temp directory.
    pub fn default_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("OCP_PRESET_LIBRARY_DIR")
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }
        oc_audio::private_fs::config_dir().join("presets")
    }

    /// A library rooted at an explicit directory.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Library directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Read every entry. Corrupt files and duplicate names are reported as
    /// warnings and skipped instead of failing the whole scan.
    ///
    /// An unchanged directory answers the previous scan: the panel lists the
    /// library on every open, and re-parsing every file for a listing that has
    /// not moved is what made a large or network-mounted library stall.
    pub fn scan(&self) -> LibraryScan {
        let fingerprint = directory_fingerprint(&self.dir);
        if let Some(cached) = cached_scan(&self.dir, &fingerprint) {
            return cached;
        }
        let scan = self.scan_uncached();
        remember_scan(&self.dir, fingerprint, &scan);
        scan
    }

    fn scan_uncached(&self) -> LibraryScan {
        let mut scan = LibraryScan::default();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return scan;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(LIBRARY_FILE_EXTENSION))
            })
            .collect();
        paths.sort();

        let mut names: Vec<String> = Vec::new();
        for path in paths {
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => {
                    scan.warnings
                        .push(format!("Skipped {}: {error}", path.display()));
                    continue;
                }
            };
            let stored = match serde_json::from_str::<StoredPreset>(&content) {
                Ok(stored) => stored,
                Err(error) => {
                    log::warn!(
                        "Skipping unreadable preset file {}: {error}",
                        path.display()
                    );
                    scan.warnings
                        .push(format!("Skipped {}: {error}", path.display()));
                    continue;
                }
            };
            let mut preset = stored.preset;
            if preset.name.trim().is_empty() {
                scan.warnings
                    .push(format!("Skipped {}: empty preset name", path.display()));
                continue;
            }
            if names.iter().any(|name| name == &preset.name) {
                log::warn!(
                    "Skipping duplicate library preset '{}' in {}",
                    preset.name,
                    path.display()
                );
                scan.warnings.push(format!(
                    "Skipped {}: duplicate preset '{}'",
                    path.display(),
                    preset.name
                ));
                continue;
            }
            if let Some(state) = stored.library_state {
                let Some(path) = self.collected_state(&state) else {
                    scan.warnings.push(format!(
                        "Skipped {}: invalid or missing collected patch state",
                        path.display()
                    ));
                    continue;
                };
                preset
                    .params
                    .push(("open_synth_state_file".into(), ParamValue::String(path)));
            }
            names.push(preset.name.clone());
            if oc_synth::synth::has_relative_project_patch_assets(&preset.params) {
                scan.warnings.push(format!(
                    "Library patch '{}' uses a project-relative asset. Save it again from its source project to collect its assets.",
                    preset.name
                ));
            }
            preset.tier = PresetTier::UserLibrary;
            preset.id = library_id(&preset.name);
            scan.usage.insert(
                preset.name.clone(),
                PatchUsage {
                    favorite: stored.favorite,
                    last_used_at: stored.last_used_at,
                },
            );
            scan.presets.push(preset);
        }
        scan.presets
            .sort_by_key(|preset| preset.name.to_lowercase());
        scan
    }

    fn collected_state(&self, name: &str) -> Option<String> {
        let digest = name.strip_suffix(".json")?;
        if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let root = self.dir.join("assets");
        if !std::fs::symlink_metadata(&root).ok()?.is_dir() {
            return None;
        }
        let path = std::path::absolute(root.join(name)).ok()?;
        std::fs::symlink_metadata(&path)
            .ok()?
            .is_file()
            .then_some(())?;
        Some(path.to_str()?.to_owned())
    }

    /// Write one preset. Saving the same name again replaces its file; a
    /// different name that sanitises to the same file name gets a stable hash
    /// suffix instead of overwriting the other patch. An overwrite keeps the
    /// favourite flag and last-used stamp the user already set.
    pub fn save(&self, preset: &SoundPreset) -> Result<SaveOutcome, String> {
        let _lock = self.write_lock()?;
        self.save_unlocked(preset, None)
    }

    /// Save a project-local patch, optionally overwriting.
    pub fn save_project_patch(
        &self,
        preset: &SoundPreset,
        project_dir: Option<&Path>,
        overwrite: bool,
    ) -> Result<SaveOutcome, String> {
        let _lock = self.write_lock()?;
        if !overwrite && self.find_file(preset.name.trim()).is_some() {
            return Err(format!(
                "A library patch named '{}' already exists",
                preset.name
            ));
        }
        self.save_project_patch_unlocked(preset, project_dir)
    }

    /// Import a shared patch without replacing the recipient's library entries.
    pub fn import_project_patch(&self, mut preset: SoundPreset) -> Result<LibraryEntry, String> {
        let _lock = self.write_lock()?;
        let base = preset.name.trim().to_owned();
        if base.is_empty() {
            return Err("Preset name cannot be empty".into());
        }
        preset.name = base.clone();
        for index in 2.. {
            if self.find_file(&preset.name).is_none() {
                break;
            }
            preset.name = format!("{base} ({index})");
        }
        self.save_project_patch_unlocked(&preset, None)?;
        let scan = self.scan();
        scan.presets
            .iter()
            .find(|entry| entry.name == preset.name)
            .map(|entry| LibraryEntry::from_scan(entry, &scan))
            .ok_or_else(|| "Could not read the imported library patch".into())
    }

    fn save_project_patch_unlocked(
        &self,
        preset: &SoundPreset,
        project_dir: Option<&Path>,
    ) -> Result<SaveOutcome, String> {
        let mut snapshot = preset.clone();
        let mut assets = super::library_assets::Assets::new(&self.dir);
        let library_state = assets.snapshot(&mut snapshot, project_dir)?;
        let outcome = self.save_unlocked(&snapshot, library_state)?;
        assets.retain();
        Ok(outcome)
    }

    fn write_lock(&self) -> Result<std::fs::File, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("Failed to create library dir: {error}"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(".write.lock"))
            .map_err(|error| format!("Failed to open preset library lock: {error}"))?;
        fs4::FileExt::lock(&file)
            .map_err(|error| format!("Failed to lock preset library: {error}"))?;
        Ok(file)
    }

    fn save_unlocked(
        &self,
        preset: &SoundPreset,
        library_state: Option<String>,
    ) -> Result<SaveOutcome, String> {
        let name = preset.name.trim();
        if name.is_empty() {
            return Err("Preset name cannot be empty".into());
        }
        let mut path = self.file_for(name);
        let mut outcome = SaveOutcome::Created;
        if path.exists() {
            if same_preset_file(&path, name) {
                outcome = SaveOutcome::Replaced;
            } else {
                path = self.collision_file_for(name);
                if path.exists() && same_preset_file(&path, name) {
                    outcome = SaveOutcome::Replaced;
                }
            }
        }
        let usage = if same_preset_file(&path, name) {
            read_stored(&path)
                .map(|stored| PatchUsage {
                    favorite: stored.favorite,
                    last_used_at: stored.last_used_at,
                })
                .unwrap_or_default()
        } else {
            PatchUsage::default()
        };

        let mut stored = preset.clone();
        stored.name = name.to_string();
        stored.tier = PresetTier::UserLibrary;
        stored.id = library_id(name);
        let record = StoredPreset {
            preset: stored,
            library_state,
            favorite: usage.favorite,
            last_used_at: usage.last_used_at,
        };
        let json = serde_json::to_string_pretty(&record)
            .map_err(|error| format!("Failed to encode preset: {error}"))?;
        write_atomically(&path, &json)?;
        log::info!(
            "Saved preset '{}' to library: {}",
            record.preset.name,
            path.display()
        );
        Ok(outcome)
    }

    /// Set the favourite flag on the file holding `name`. Returns whether the
    /// library has that patch.
    pub fn set_favorite(&self, name: &str, favorite: bool) -> Result<bool, String> {
        self.update_usage(name, |usage| usage.favorite = favorite)
    }

    /// Record that `name` was loaded at `at` (Unix seconds). Returns whether
    /// the library has that patch.
    pub fn mark_used(&self, name: &str, at: u64) -> Result<bool, String> {
        self.update_usage(name, |usage| usage.last_used_at = Some(at))
    }

    fn update_usage(
        &self,
        name: &str,
        update: impl FnOnce(&mut PatchUsage),
    ) -> Result<bool, String> {
        if !self.dir.is_dir() {
            return Ok(false);
        }
        let _lock = self.write_lock()?;
        let Some(path) = self.find_file(name) else {
            return Ok(false);
        };
        let mut record =
            read_stored(&path).ok_or_else(|| format!("Failed to read {}", path.display()))?;
        let mut usage = PatchUsage {
            favorite: record.favorite,
            last_used_at: record.last_used_at,
        };
        update(&mut usage);
        record.favorite = usage.favorite;
        record.last_used_at = usage.last_used_at;
        let json = serde_json::to_string_pretty(&record)
            .map_err(|error| format!("Failed to encode preset: {error}"))?;
        write_atomically(&path, &json)?;
        Ok(true)
    }

    fn find_file(&self, name: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.dir).ok()?;
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(LIBRARY_FILE_EXTENSION))
            })
            .collect();
        paths.sort();
        paths.into_iter().find(|path| same_preset_file(path, name))
    }

    /// Remove every file that holds `name`. Returns whether anything matched.
    pub fn remove(&self, name: &str) -> Result<bool, String> {
        if !self.dir.is_dir() {
            return Ok(false);
        }
        let _lock = self.write_lock()?;
        let entries = std::fs::read_dir(&self.dir).map_err(|error| error.to_string())?;
        let mut removed = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str())
                != Some(LIBRARY_FILE_EXTENSION)
            {
                continue;
            }
            if same_preset_file(&path, name) {
                std::fs::remove_file(&path)
                    .map_err(|error| format!("Failed to remove {}: {error}", path.display()))?;
                removed = true;
            }
        }
        Ok(removed)
    }

    /// File used for a first save of `name`.
    pub fn file_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.{LIBRARY_FILE_EXTENSION}",
            sanitize_filename(name)
        ))
    }

    fn collision_file_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!(
            "{}-{:08x}.{LIBRARY_FILE_EXTENSION}",
            sanitize_filename(name),
            name_hash(name)
        ))
    }
}

/// A preset loaded from the library, as sent to the GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub name: String,
    pub category: String,
    pub tags: Vec<String>,
    pub patch: Option<String>,
    pub params: Vec<(String, ParamValue)>,
    pub effects: Vec<Effect>,
    /// Browsing preferences, not project data.
    pub favorite: bool,
    pub last_used_at: Option<u64>,
}

impl From<&SoundPreset> for LibraryEntry {
    fn from(preset: &SoundPreset) -> Self {
        Self {
            name: preset.name.clone(),
            category: preset.category.label().to_string(),
            tags: preset.tags.clone(),
            patch: preset.patch.clone(),
            params: preset.params.clone(),
            effects: preset.effects.clone(),
            favorite: false,
            last_used_at: None,
        }
    }
}

impl LibraryEntry {
    /// Build an entry with the preferences the scan read from the same file.
    pub fn from_scan(preset: &SoundPreset, scan: &LibraryScan) -> Self {
        let mut entry = Self::from(preset);
        if let Some(usage) = scan.usage.get(&preset.name) {
            entry.favorite = usage.favorite;
            entry.last_used_at = usage.last_used_at;
        }
        entry
    }
}

/// Browser order: favourites first by name, then most recently used, then the
/// order the entries arrived in. `sort_by` is stable, so ties keep that order.
pub fn sort_for_browser(entries: &mut [LibraryEntry]) {
    entries.sort_by(|left, right| match (left.favorite, right.favorite) {
        (true, true) => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => right.last_used_at.cmp(&left.last_used_at),
    });
}

/// On-disk record: the preset plus the two preference fields. Both are
/// optional so files written before the fields existed still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPreset {
    #[serde(flatten)]
    preset: SoundPreset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    library_state: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_at: Option<u64>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn read_stored(path: &Path) -> Option<StoredPreset> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn library_id(name: &str) -> String {
    format!("library:{name}")
}

fn same_preset_file(path: &Path, name: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<SoundPreset>(&content)
        .map(|preset| preset.name == name)
        .unwrap_or(false)
}

fn write_atomically(path: &Path, contents: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("Preset path has no parent directory".into());
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("preset");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    if let Err(error) = std::fs::write(&temporary, contents) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Failed to write preset: {error}"));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Failed to publish preset: {error}"));
    }
    Ok(())
}

/// Keep names readable as file names; everything else becomes `_`.
pub fn sanitize_filename(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric()
                || character == '-'
                || character == '_'
                || character == ' '
            {
                character
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim();
    if slug.is_empty() {
        "patch".into()
    } else {
        slug.to_string()
    }
}

/// FNV-1a, truncated: only used to separate names that share a file slug.
fn name_hash(name: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{PresetBackend, PresetCategory};

    #[test]
    fn a_test_build_keeps_the_default_store_out_of_the_operator_home() {
        if std::env::var_os("OCP_PRESET_LIBRARY_DIR").is_some() {
            return;
        }
        let dir = PresetLibrary::default_dir();
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "the test build resolved the operator's library: {}",
            dir.display()
        );
        let preferences = PresetPreferences::default_path();
        assert!(
            preferences.starts_with(std::env::temp_dir()),
            "the test build resolved the operator's preferences: {}",
            preferences.display()
        );
        assert_eq!(preferences, PresetPreferences::path_beside(&dir));
    }

    struct TempLibrary {
        dir: PathBuf,
        library: PresetLibrary,
    }

    impl TempLibrary {
        fn new(label: &str) -> Self {
            let dir = unique_dir(label);
            let library = PresetLibrary::at(dir.clone());
            Self { dir, library }
        }
    }

    impl Drop for TempLibrary {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A directory that is not the library itself, for stores that sit beside
    /// it (the factory preferences).
    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let path = unique_dir(label);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_dir(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ocp-preset-library-{label}-{}-{stamp}-{unique}",
            std::process::id()
        ))
    }

    fn synth_preset(name: &str) -> SoundPreset {
        SoundPreset {
            id: String::new(),
            name: name.to_string(),
            category: PresetCategory::Synth,
            tags: vec!["user".into()],
            backend: PresetBackend::BuiltinPlugin {
                plugin_id: "builtin.open_synth".into(),
            },
            tier: PresetTier::Builtin,
            patch: Some("Glass Pad".into()),
            params: vec![
                ("osc.a.level".into(), ParamValue::Number(0.5)),
                ("filter.cutoff".into(), ParamValue::Number(4200.0)),
            ],
            effects: vec![],
        }
    }

    #[test]
    fn round_trip_keeps_params_and_provenance() {
        let temp = TempLibrary::new("round-trip");
        let library = temp.library.clone();
        assert_eq!(
            library.save(&synth_preset("Dust Pad")).unwrap(),
            SaveOutcome::Created
        );

        let scan = library.scan();
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
        assert_eq!(scan.presets.len(), 1);
        let preset = &scan.presets[0];
        assert_eq!(preset.name, "Dust Pad");
        assert_eq!(preset.tier, PresetTier::UserLibrary);
        assert_eq!(preset.id, "library:Dust Pad");
        assert_eq!(preset.patch.as_deref(), Some("Glass Pad"));
        assert_eq!(preset.params, synth_preset("Dust Pad").params);
    }

    #[test]
    fn an_unchanged_library_answers_the_previous_scan() {
        let temp = TempLibrary::new("scan-cache");
        let library = temp.library.clone();
        library.save(&synth_preset("Dust Pad")).unwrap();

        let first = library.scan();
        assert_eq!(first.presets.len(), 1);
        // A second scan of an untouched directory reuses the parse.
        let second = library.scan();
        assert_eq!(second.presets.len(), 1);

        // A new file changes the fingerprint, so the next scan reads it.
        library.save(&synth_preset("Glass Pad")).unwrap();
        let third = library.scan();
        assert_eq!(third.presets.len(), 2);
        assert!(
            third
                .presets
                .iter()
                .any(|preset| preset.name == "Glass Pad")
        );

        // A rewritten file is read again even when its name did not change.
        let path = library.find_file("Glass Pad").expect("the saved file");
        let mut stored = std::fs::read_to_string(&path).unwrap();
        stored = stored.replace("Glass Pad", "Glass Pad 2");
        std::fs::write(&path, stored).unwrap();
        let fourth = library.scan();
        assert!(
            fourth
                .presets
                .iter()
                .any(|preset| preset.name == "Glass Pad 2")
        );
    }

    #[test]
    fn packaged_samples_are_deduplicated_and_failed_overwrites_roll_back() {
        use oc_synth::sampler::{SampleRegion, SamplerPatch};
        use oc_synth::synth::{SynthPatch, document::PatchDocument};

        let temp = TempLibrary::new("collect");
        let root = tempfile::tempdir().unwrap();
        let sample = root.path().join("source.wav");
        let bytes = crate::wav::mono_wav_bytes(&[0.2; 64], 48_000).unwrap();
        std::fs::write(&sample, &bytes).unwrap();
        let source_doc = root.path().join("state.json");
        let mut document = PatchDocument {
            version: 1,
            name: "Original".into(),
            author: "Fixture".into(),
            category: "Synth".into(),
            use_policy: "Fixture".into(),
            patch: SynthPatch {
                sampler: Some(SamplerPatch {
                    regions: vec![
                        serde_json::from_value::<SampleRegion>(
                            serde_json::json!({"sample":sample}),
                        )
                        .unwrap(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        let write =
            |doc: &PatchDocument| std::fs::write(&source_doc, doc.to_json().unwrap()).unwrap();
        write(&document);
        let mut preset = synth_preset("Packaged");
        preset.params.push((
            "open_synth_state_file".into(),
            ParamValue::String("state.json".into()),
        ));
        let library = &temp.library;
        library
            .save_project_patch(&preset, Some(root.path()), false)
            .unwrap();
        let asset_dir = library.dir().join("assets");
        let assets = || std::fs::read_dir(&asset_dir).unwrap().count();
        assert_eq!(assets(), 2);
        library
            .save_project_patch(&preset, Some(root.path()), true)
            .unwrap();
        assert_eq!(assets(), 2);

        std::fs::write(
            &sample,
            crate::wav::mono_wav_bytes(&[0.4; 64], 48_000).unwrap(),
        )
        .unwrap();
        document.patch.sampler.as_mut().unwrap().regions.push(
            serde_json::from_value(serde_json::json!({"sample":root.path().join("missing.wav")}))
                .unwrap(),
        );
        write(&document);
        assert!(
            library
                .save_project_patch(&preset, Some(root.path()), true)
                .is_err()
        );
        assert_eq!(assets(), 2);
        drop(root);
        library.set_favorite("Packaged", true).unwrap();
        let relocated = tempfile::tempdir().unwrap();
        let relocated_dir = relocated.path().join("presets");
        std::fs::rename(library.dir(), &relocated_dir).unwrap();
        let moved = PresetLibrary::at(&relocated_dir).scan();
        assert!(moved.warnings.is_empty(), "{:?}", moved.warnings);
        assert!(moved.usage["Packaged"].favorite);
        let saved = moved.presets.into_iter().next().unwrap();
        let loaded = oc_synth::synth::load_project_patch(&saved.params, None).unwrap();
        let collected = &loaded.sampler.unwrap().regions[0].sample;
        assert_eq!(
            collected.parent(),
            Some(relocated_dir.join("assets").as_path())
        );
        assert_eq!(std::fs::read(collected).unwrap(), bytes);
    }

    #[test]
    fn saving_the_same_name_replaces_its_file() {
        let temp = TempLibrary::new("replace");
        let library = temp.library.clone();
        library.save(&synth_preset("Dust Pad")).unwrap();
        let mut changed = synth_preset("Dust Pad");
        changed.params = vec![("output.gain".into(), ParamValue::Number(0.9))];
        assert_eq!(library.save(&changed).unwrap(), SaveOutcome::Replaced);
        let scan = library.scan();
        assert_eq!(scan.presets.len(), 1);
        assert_eq!(scan.presets[0].params, changed.params);
    }

    #[test]
    fn names_that_share_a_slug_do_not_overwrite_each_other() {
        let temp = TempLibrary::new("slug");
        let library = temp.library.clone();
        library.save(&synth_preset("A/B")).unwrap();
        library.save(&synth_preset("A_B")).unwrap();
        let mut names: Vec<String> = library
            .scan()
            .presets
            .into_iter()
            .map(|preset| preset.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["A/B".to_string(), "A_B".to_string()]);
    }

    #[test]
    fn a_corrupt_file_is_skipped_with_a_warning() {
        let temp = TempLibrary::new("corrupt");
        let library = temp.library.clone();
        library.save(&synth_preset("Good")).unwrap();
        std::fs::write(temp.dir.join("broken.ocp-preset"), "{ not json").unwrap();
        let scan = library.scan();
        assert_eq!(scan.presets.len(), 1);
        assert_eq!(scan.presets[0].name, "Good");
        assert_eq!(scan.warnings.len(), 1);
        assert!(scan.warnings[0].contains("broken.ocp-preset"));
    }

    #[test]
    fn duplicate_names_on_disk_keep_the_first_file_only() {
        let temp = TempLibrary::new("duplicate");
        let library = temp.library.clone();
        library.save(&synth_preset("Twice")).unwrap();
        let second = temp.dir.join("twice-copy.ocp-preset");
        std::fs::write(
            &second,
            serde_json::to_string(&synth_preset("Twice")).unwrap(),
        )
        .unwrap();
        let scan = library.scan();
        assert_eq!(scan.presets.len(), 1);
        assert_eq!(scan.warnings.len(), 1);
        assert!(scan.warnings[0].contains("duplicate"));
    }

    #[test]
    fn remove_takes_the_entry_out_of_the_scan() {
        let temp = TempLibrary::new("remove");
        let library = temp.library.clone();
        library.save(&synth_preset("Gone")).unwrap();
        assert!(library.remove("Gone").unwrap());
        assert!(!library.remove("Gone").unwrap());
        assert!(library.scan().presets.is_empty());
    }

    /// Names in the order the browser shows them.
    fn ordered_names(library: &PresetLibrary) -> Vec<String> {
        let scan = library.scan();
        let mut entries: Vec<LibraryEntry> = scan
            .presets
            .iter()
            .map(|preset| LibraryEntry::from_scan(preset, &scan))
            .collect();
        sort_for_browser(&mut entries);
        entries.into_iter().map(|entry| entry.name).collect()
    }

    #[test]
    fn favourites_and_recent_order_survive_a_reread() {
        let temp = TempLibrary::new("usage-order");
        let library = temp.library.clone();
        for name in ["Zeta", "Alpha", "Beta", "Gamma"] {
            library.save(&synth_preset(name)).unwrap();
        }
        assert_eq!(ordered_names(&library), ["Alpha", "Beta", "Gamma", "Zeta"]);

        assert!(library.set_favorite("Zeta", true).unwrap());
        assert_eq!(ordered_names(&library), ["Zeta", "Alpha", "Beta", "Gamma"]);

        assert!(library.mark_used("Gamma", 100).unwrap());
        assert_eq!(ordered_names(&library), ["Zeta", "Gamma", "Alpha", "Beta"]);
        assert!(library.mark_used("Beta", 200).unwrap());
        assert_eq!(ordered_names(&library), ["Zeta", "Beta", "Gamma", "Alpha"]);

        // A fresh handle sees what the files hold, not what this process cached.
        let reread = PresetLibrary::at(temp.dir.clone());
        let scan = reread.scan();
        assert_eq!(
            scan.usage.get("Zeta"),
            Some(&PatchUsage {
                favorite: true,
                last_used_at: None,
            })
        );
        assert_eq!(
            scan.usage.get("Beta"),
            Some(&PatchUsage {
                favorite: false,
                last_used_at: Some(200),
            })
        );
        assert_eq!(scan.usage.get("Alpha"), Some(&PatchUsage::default()));
        assert_eq!(ordered_names(&reread), ["Zeta", "Beta", "Gamma", "Alpha"]);
    }

    #[test]
    fn a_file_without_the_preference_fields_still_loads() {
        let temp = TempLibrary::new("legacy");
        let legacy = serde_json::to_string(&synth_preset("Old Patch")).unwrap();
        std::fs::create_dir_all(&temp.dir).unwrap();
        std::fs::write(temp.dir.join("old.ocp-preset"), legacy).unwrap();
        let scan = temp.library.scan();
        assert!(scan.warnings.is_empty(), "{:?}", scan.warnings);
        assert_eq!(scan.presets.len(), 1);
        assert_eq!(scan.presets[0].name, "Old Patch");
        assert_eq!(scan.usage.get("Old Patch"), Some(&PatchUsage::default()));
        let entry = LibraryEntry::from_scan(&scan.presets[0], &scan);
        assert!(!entry.favorite);
        assert_eq!(entry.last_used_at, None);
    }

    #[test]
    fn saving_over_a_favourite_keeps_the_preferences() {
        let temp = TempLibrary::new("save-keeps-usage");
        let library = temp.library.clone();
        library.save(&synth_preset("Kept")).unwrap();
        library.set_favorite("Kept", true).unwrap();
        library.mark_used("Kept", 12).unwrap();

        let mut changed = synth_preset("Kept");
        changed.params = vec![("output.gain".into(), ParamValue::Number(0.9))];
        assert_eq!(library.save(&changed).unwrap(), SaveOutcome::Replaced);

        let scan = library.scan();
        assert_eq!(scan.presets[0].params, changed.params);
        assert_eq!(
            scan.usage.get("Kept"),
            Some(&PatchUsage {
                favorite: true,
                last_used_at: Some(12),
            })
        );
    }

    #[test]
    fn unknown_preset_names_are_not_recorded() {
        let temp = TempLibrary::new("missing");
        let library = temp.library.clone();
        assert!(!library.set_favorite("Nope", true).unwrap());
        assert!(!library.mark_used("Nope", 1).unwrap());
        assert!(library.scan().presets.is_empty());
    }

    #[test]
    fn factory_preferences_round_trip_beside_the_library() {
        let root = TempRoot::new("factory-preferences");
        let presets = root.path().join("presets");
        let path = PresetPreferences::path_beside(&presets);
        assert_eq!(path, root.path().join(PREFERENCES_FILE_NAME));

        let mut preferences = PresetPreferences::at(path.clone());
        assert_eq!(
            preferences.factory_usage("Glass Pad"),
            PatchUsage::default()
        );
        preferences.set_factory_favorite("Glass Pad", true);
        preferences.mark_factory_used("Glass Pad", 77);
        preferences.mark_factory_used("Init", 5);
        preferences.save().unwrap();

        let loaded = PresetPreferences::at(path.clone()).load();
        assert_eq!(
            loaded.factory_usage("Glass Pad"),
            PatchUsage {
                favorite: true,
                last_used_at: Some(77),
            }
        );
        assert_eq!(loaded.factory_usage("Init").last_used_at, Some(5));
        // The rename publishes the file, so no temporary sibling is left behind.
        let leftovers = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_damaged_preference_file_reads_as_empty() {
        let root = TempRoot::new("bad-preferences");
        let path = root.path().join(PREFERENCES_FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = PresetPreferences::at(path).load();
        assert!(loaded.patches.is_empty());
    }
}
