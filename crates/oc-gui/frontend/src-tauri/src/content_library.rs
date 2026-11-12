//! Content-library registration: the backend half of the browser scan.
//!
//! A catalogue is one JSON file listing library roots and the instrument files
//! found under them. Registering, removing and rescanning are ordinary backend
//! calls; the GUI (when it has a browser) and the agent surface are both thin
//! front ends over these functions. No third-party database is read or written.
//!
//! Every write is preceded by a backup and verified by re-reading the file. A
//! failed or short write restores the previous catalogue, so a crash mid-write
//! can never leave an unloadable catalogue behind.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Schema written into every catalogue. Unknown versions are refused, not
/// guessed at.
pub const CATALOGUE_SCHEMA: &str = "opencompose.content_library.v1";

/// Sampler asset manifest the existing asset loader already understands.
const SAMPLER_ASSET_SCHEMA: &str = "opencompose.pgen.sampler_asset.v1";

/// Extensions this scan treats as a sampled instrument.
const INSTRUMENT_EXTENSIONS: [&str; 3] = ["sf2", "sf3", "sfz"];

/// Directory depth and entry caps keep a mistaken root (for example `$HOME`)
/// from turning a rescan into a filesystem walk.
const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 20_000;

/// Serialise read-modify-write cycles in this process so two concurrent agent
/// calls cannot lose one registration. Cross-process writers are the user's
/// responsibility; the backup file is the recovery point.
static CATALOGUE_WRITE_LOCK: Mutex<()> = Mutex::new(());

fn write_guard() -> MutexGuard<'static, ()> {
    CATALOGUE_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One instrument file found in a content library.
pub struct ContentInstrument {
    pub name: String,
    pub path: String,
    /// `sf2`, `sf3`, `sfz` or `sampler_asset`.
    pub format: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// A registered content library root.
pub struct ContentLibrary {
    /// Stable id derived from the canonical path.
    pub id: String,
    pub name: String,
    pub path: String,
    /// `file` or `directory`.
    pub kind: String,
    pub added_at: u64,
    pub scanned_at: u64,
    /// The root disappeared after registration; the row is kept so a rescan
    /// can restore it without losing its identity.
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub instruments: Vec<ContentInstrument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Persisted content-library catalogue.
pub struct ContentCatalogue {
    pub schema: String,
    #[serde(default)]
    pub libraries: Vec<ContentLibrary>,
}

impl ContentCatalogue {
    fn empty() -> Self {
        Self {
            schema: CATALOGUE_SCHEMA.to_string(),
            libraries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Catalogue contents for the UI.
pub struct CatalogueListing {
    pub catalogue_path: String,
    pub schema: String,
    pub libraries: Vec<ContentLibrary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result of registering a library.
pub struct RegisterOutcome {
    pub catalogue_path: String,
    /// The backup taken before the write; `None` on the first registration.
    pub backup_path: Option<String>,
    pub library: ContentLibrary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result of removing a library.
pub struct RemoveOutcome {
    pub catalogue_path: String,
    pub backup_path: Option<String>,
    pub removed: ContentLibrary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result of rescanning libraries.
pub struct RescanOutcome {
    pub catalogue_path: String,
    pub backup_path: Option<String>,
    pub libraries: Vec<ContentLibrary>,
    /// Non-fatal problems, one per library (for example a vanished root).
    pub warnings: Vec<String>,
}

/// Default catalogue file. `OPENCOMPOSE_CONTENT_CATALOGUE` overrides it so a
/// test or a second profile never touches the operator's catalogue.
pub fn default_catalogue_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OPENCOMPOSE_CONTENT_CATALOGUE") {
        return PathBuf::from(path);
    }
    crate::private_fs::config_dir().join("content-libraries.json")
}

/// Resolve the caller's optional path, falling back to the default.
pub fn resolve_catalogue_path(path: Option<&str>) -> PathBuf {
    match path.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => default_catalogue_path(),
    }
}

/// Read a catalogue. A missing file is an empty catalogue; anything else that
/// does not parse is an error, never silently replaced.
pub fn load(path: &Path) -> Result<ContentCatalogue, String> {
    match fs::read(path) {
        Ok(bytes) => parse(&bytes, path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ContentCatalogue::empty()),
        Err(error) => Err(format!(
            "failed to read content catalogue '{}': {error}",
            path.display()
        )),
    }
}

fn parse(bytes: &[u8], path: &Path) -> Result<ContentCatalogue, String> {
    let catalogue: ContentCatalogue = serde_json::from_slice(bytes).map_err(|error| {
        format!(
            "content catalogue '{}' is not valid JSON: {error}",
            path.display()
        )
    })?;
    if catalogue.schema != CATALOGUE_SCHEMA {
        return Err(format!(
            "content catalogue '{}' has schema '{}', expected '{}'",
            path.display(),
            catalogue.schema,
            CATALOGUE_SCHEMA
        ));
    }
    Ok(catalogue)
}

/// List a catalogue without changing it.
pub fn list(path: &Path) -> Result<CatalogueListing, String> {
    let catalogue = load(path)?;
    Ok(CatalogueListing {
        catalogue_path: path.display().to_string(),
        schema: catalogue.schema,
        libraries: catalogue.libraries,
    })
}

/// Register one library root. The root is scanned first; a root with no
/// recognisable instrument files is refused before anything is written.
pub fn register(
    catalogue_path: &Path,
    root: &Path,
    name: Option<&str>,
) -> Result<RegisterOutcome, String> {
    let _guard = write_guard();
    let root = canonical_root(root)?;
    let mut catalogue = load(catalogue_path)?;
    if catalogue
        .libraries
        .iter()
        .any(|library| same_path(&library.path, &root))
    {
        return Err(format!(
            "'{}' is already registered in '{}'",
            root.display(),
            catalogue_path.display()
        ));
    }
    let instruments = scan_root(&root)?;
    if instruments.is_empty() {
        return Err(format!(
            "no instrument files ({} ) found under '{}'",
            INSTRUMENT_EXTENSIONS.join(", "),
            root.display()
        ));
    }
    let now = now_seconds();
    let library = ContentLibrary {
        id: library_id(&root),
        name: library_name(&root, name),
        path: root.display().to_string(),
        kind: if root.is_file() { "file" } else { "directory" }.to_string(),
        added_at: now,
        scanned_at: now,
        missing: false,
        instruments,
    };
    catalogue.libraries.push(library.clone());
    catalogue
        .libraries
        .sort_by_key(|library| library.name.to_lowercase());
    let backup = commit(catalogue_path, &catalogue)?;
    Ok(RegisterOutcome {
        catalogue_path: catalogue_path.display().to_string(),
        backup_path: backup.map(|path| path.display().to_string()),
        library,
    })
}

/// Remove a library row by id. The files on disk are never touched.
pub fn remove(catalogue_path: &Path, id: &str) -> Result<RemoveOutcome, String> {
    let _guard = write_guard();
    let mut catalogue = load(catalogue_path)?;
    let index = catalogue
        .libraries
        .iter()
        .position(|library| library.id == id)
        .ok_or_else(|| format!("unknown content library '{id}'"))?;
    let removed = catalogue.libraries.remove(index);
    let backup = commit(catalogue_path, &catalogue)?;
    Ok(RemoveOutcome {
        catalogue_path: catalogue_path.display().to_string(),
        backup_path: backup.map(|path| path.display().to_string()),
        removed,
    })
}

/// Re-scan one library (by id) or every library. Rows whose root vanished are
/// kept and marked `missing`, so a later rescan restores them.
pub fn rescan(catalogue_path: &Path, id: Option<&str>) -> Result<RescanOutcome, String> {
    let _guard = write_guard();
    let mut catalogue = load(catalogue_path)?;
    let targets: Vec<usize> = match id {
        Some(id) => {
            let index = catalogue
                .libraries
                .iter()
                .position(|library| library.id == id)
                .ok_or_else(|| format!("unknown content library '{id}'"))?;
            vec![index]
        }
        None => (0..catalogue.libraries.len()).collect(),
    };
    let mut warnings = Vec::new();
    let now = now_seconds();
    for index in targets {
        let library = &mut catalogue.libraries[index];
        let root = PathBuf::from(&library.path);
        match scan_root(&root) {
            Ok(instruments) => {
                library.instruments = instruments;
                library.missing = false;
                library.scanned_at = now;
            }
            Err(error) => {
                library.instruments.clear();
                library.missing = true;
                library.scanned_at = now;
                warnings.push(format!("'{}': {error}", library.path));
            }
        }
    }
    let backup = commit(catalogue_path, &catalogue)?;
    Ok(RescanOutcome {
        catalogue_path: catalogue_path.display().to_string(),
        backup_path: backup.map(|path| path.display().to_string()),
        libraries: catalogue.libraries,
        warnings,
    })
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    if !root.exists() {
        return Err(format!(
            "content library root '{}' does not exist",
            root.display()
        ));
    }
    root.canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", root.display()))
}

fn same_path(left: &str, right: &Path) -> bool {
    let left = Path::new(left);
    left == right
        || left
            .canonicalize()
            .map(|left| left.as_path() == right)
            .unwrap_or(false)
}

fn library_id(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(root.display().to_string().as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("lib-{hex}")
}

fn library_name(root: &Path, requested: Option<&str>) -> String {
    if let Some(name) = requested.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Content library")
        .to_string()
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Instrument files under one root. A file root is validated on its own; a
/// directory root is walked with depth/entry caps.
fn scan_root(root: &Path) -> Result<Vec<ContentInstrument>, String> {
    if root.is_file() {
        let instrument = scan_file(root)?;
        return Ok(vec![instrument]);
    }
    if !root.is_dir() {
        return Err(format!("'{}' is not a file or directory", root.display()));
    }
    let mut found = Vec::new();
    let mut budget = MAX_ENTRIES;
    walk(root, 0, &mut budget, &mut found)?;
    // A sampler asset's entrypoint is also a plain .sfz; keep the asset row,
    // which carries the stable name, and drop the duplicate path.
    let mut instruments: std::collections::BTreeMap<String, ContentInstrument> =
        std::collections::BTreeMap::new();
    for instrument in found {
        match instruments.get(&instrument.path) {
            Some(existing) if existing.format == "sampler_asset" => {}
            _ => {
                instruments.insert(instrument.path.clone(), instrument);
            }
        }
    }
    let mut instruments: Vec<ContentInstrument> = instruments.into_values().collect();
    instruments.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(instruments)
}

fn walk(
    root: &Path,
    depth: usize,
    budget: &mut usize,
    instruments: &mut Vec<ContentInstrument>,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    if *budget == 0 {
        return Ok(());
    }
    let entries = fs::read_dir(root)
        .map_err(|error| format!("failed to read '{}': {error}", root.display()))?;
    for entry in entries {
        if *budget == 0 {
            return Ok(());
        }
        *budget -= 1;
        let entry =
            entry.map_err(|error| format!("failed to read '{}': {error}", root.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect '{}': {error}", path.display()))?;
        if file_type.is_dir() {
            walk(&path, depth + 1, budget, instruments)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if name.eq_ignore_ascii_case("manifest.json") {
            if let Some(asset) = sampler_asset(&path)? {
                instruments.push(asset);
            }
            continue;
        }
        if is_instrument_file(&path) {
            instruments.push(scan_file(&path)?);
        }
    }
    Ok(())
}

fn is_instrument_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            INSTRUMENT_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn scan_file(path: &Path) -> Result<ContentInstrument, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !INSTRUMENT_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
    {
        return Err(format!(
            "'{}' is not an instrument file ({})",
            path.display(),
            INSTRUMENT_EXTENSIONS.join(", ")
        ));
    }
    let size_bytes = fs::metadata(path)
        .map_err(|error| format!("failed to stat '{}': {error}", path.display()))?
        .len();
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Instrument")
        .to_string();
    Ok(ContentInstrument {
        name,
        path: path.display().to_string(),
        format: extension,
        size_bytes,
    })
}

/// A sampler asset pack names its entrypoint in `manifest.json`; that is the
/// instrument, and the manifest's `asset_id` is its stable name.
fn sampler_asset(manifest_path: &Path) -> Result<Option<ContentInstrument>, String> {
    let bytes = fs::read(manifest_path)
        .map_err(|error| format!("failed to read '{}': {error}", manifest_path.display()))?;
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        // A stray manifest.json is not ours; ignore it rather than failing the
        // whole library scan.
        Err(_) => return Ok(None),
    };
    if value.get("schema").and_then(|schema| schema.as_str()) != Some(SAMPLER_ASSET_SCHEMA) {
        return Ok(None);
    }
    let Some(entrypoint) = value.get("entrypoint").and_then(|entry| entry.as_str()) else {
        return Ok(None);
    };
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    let entrypoint = root.join(entrypoint);
    let size_bytes = fs::metadata(&entrypoint)
        .map_err(|error| format!("sampler entrypoint '{}': {error}", entrypoint.display()))?
        .len();
    let name = value
        .get("asset_id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            entrypoint
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Sampler asset")
                .to_string()
        });
    Ok(Some(ContentInstrument {
        name,
        path: entrypoint.display().to_string(),
        format: "sampler_asset".to_string(),
        size_bytes,
    }))
}

/// Write the catalogue with a backup and verification. Returns the backup path
/// (`None` when there was no previous file).
pub fn commit(path: &Path, catalogue: &ContentCatalogue) -> Result<Option<PathBuf>, String> {
    commit_with(path, catalogue, &mut |path, bytes| {
        crate::private_fs::write_atomic(path, bytes)
    })
}

/// Testable core of [`commit`]. `write` receives the final path and bytes; the
/// rollback path never uses it, so a failing writer cannot break restoration.
fn commit_with(
    path: &Path,
    catalogue: &ContentCatalogue,
    write: &mut dyn FnMut(&Path, &[u8]) -> io::Result<()>,
) -> Result<Option<PathBuf>, String> {
    let mut body = serde_json::to_vec_pretty(catalogue)
        .map_err(|error| format!("failed to serialize content catalogue: {error}"))?;
    body.push(b'\n');
    // Refuse to write a body we could not read back, before touching the file.
    parse(&body, path)?;

    let previous = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "failed to read existing catalogue '{}': {error}",
                path.display()
            ));
        }
    };
    let backup_path = backup_path(path);
    if let Some(previous) = &previous {
        fs::write(&backup_path, previous).map_err(|error| {
            format!(
                "failed to back up '{}' to '{}': {error}",
                path.display(),
                backup_path.display()
            )
        })?;
    }

    if let Err(error) = write(path, &body) {
        restore(path, previous.as_deref())?;
        return Err(format!(
            "failed to write content catalogue '{}': {error} (previous catalogue restored)",
            path.display()
        ));
    }

    let written = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            restore(path, previous.as_deref())?;
            return Err(format!(
                "content catalogue '{}' could not be read back after writing: {error} (previous catalogue restored)",
                path.display()
            ));
        }
    };
    if written != body {
        restore(path, previous.as_deref())?;
        return Err(format!(
            "content catalogue '{}' was not written completely (previous catalogue restored)",
            path.display()
        ));
    }
    Ok(previous.map(|_| backup_path))
}

fn restore(path: &Path, previous: Option<&[u8]>) -> Result<(), String> {
    match previous {
        Some(bytes) => {
            crate::private_fs::write_atomic(path, bytes).map_err(|error| {
                format!(
                    "failed to restore content catalogue '{}': {error}",
                    path.display()
                )
            })?;
            Ok(())
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "failed to remove partial content catalogue '{}': {error}",
                path.display()
            )),
        },
    }
}

/// The backup file sits beside the catalogue with a `.bak` suffix, so a reader
/// can find it without a database or index.
pub fn backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "oc-content-library-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn fake_library(dir: &Path, files: &[&str]) {
        for file in files {
            write_file(&dir.join(file), b"fake instrument body");
        }
    }

    #[test]
    fn registered_library_discovers_sf3_and_retains_its_loadable_path() {
        let temp = TempDir::new("register-sf3");
        let font = temp.path().join("piano.sf3");
        fs::write(&font, b"RIFF\0\0\0\0sfbk").unwrap();
        let catalogue = temp.path().join("catalogue.json");
        let outcome = register(&catalogue, &font, None).unwrap();
        assert_eq!(outcome.library.instruments.len(), 1);
        assert_eq!(outcome.library.instruments[0].format, "sf3");
        assert_eq!(
            outcome.library.instruments[0].path,
            font.canonicalize().unwrap().display().to_string()
        );
    }

    #[test]
    fn registering_a_library_writes_a_readable_catalogue() {
        let temp = TempDir::new("register");
        let library = temp.path().join("strings");
        fake_library(&library, &["violins.sf2", "cellos.sf2"]);
        let catalogue = temp.path().join("catalogue.json");

        let outcome = register(&catalogue, &library, Some("Strings")).unwrap();
        assert_eq!(outcome.library.name, "Strings");
        assert_eq!(outcome.library.instruments.len(), 2);
        assert!(outcome.backup_path.is_none(), "first write has no backup");

        let listing = list(&catalogue).unwrap();
        assert_eq!(listing.libraries.len(), 1);
        assert_eq!(listing.libraries[0].instruments[0].format, "sf2");
        assert!(listing.libraries[0].id.starts_with("lib-"));
        // The catalogue is plain JSON and names the schema.
        let text = fs::read_to_string(&catalogue).unwrap();
        assert!(text.contains(CATALOGUE_SCHEMA));
    }

    #[test]
    fn a_second_write_backs_up_the_previous_catalogue() {
        let temp = TempDir::new("backup");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fake_library(&first, &["a.sfz"]);
        fake_library(&second, &["b.sfz"]);
        let catalogue = temp.path().join("catalogue.json");

        register(&catalogue, &first, None).unwrap();
        let before = fs::read(&catalogue).unwrap();
        let outcome = register(&catalogue, &second, None).unwrap();
        let backup = outcome.backup_path.expect("second write must back up");
        assert_eq!(fs::read(&backup).unwrap(), before);
        assert_eq!(list(&catalogue).unwrap().libraries.len(), 2);
    }

    #[test]
    fn duplicate_registration_is_refused_without_writing() {
        let temp = TempDir::new("duplicate");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        register(&catalogue, &library, None).unwrap();
        let before = fs::read(&catalogue).unwrap();

        let error = register(&catalogue, &library, None).unwrap_err();
        assert!(error.contains("already registered"), "{error}");
        assert_eq!(fs::read(&catalogue).unwrap(), before);
    }

    #[test]
    fn an_empty_root_is_refused_before_the_first_write() {
        let temp = TempDir::new("empty");
        let library = temp.path().join("empty");
        fs::create_dir_all(&library).unwrap();
        let catalogue = temp.path().join("catalogue.json");

        let error = register(&catalogue, &library, None).unwrap_err();
        assert!(error.contains("no instrument files"), "{error}");
        assert!(!catalogue.exists(), "nothing may be written on refusal");
        assert!(!backup_path(&catalogue).exists());
    }

    #[test]
    fn a_missing_root_is_refused() {
        let temp = TempDir::new("missing-root");
        let catalogue = temp.path().join("catalogue.json");
        let error = register(&catalogue, &temp.path().join("nope"), None).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        assert!(
            error.contains("nope"),
            "the refusal names the path the caller gave: {error}"
        );
        assert!(!catalogue.exists());
    }

    #[test]
    fn a_regular_file_that_is_not_an_instrument_is_refused() {
        let temp = TempDir::new("not-instrument");
        let catalogue = temp.path().join("catalogue.json");
        let file = temp.path().join("notes.txt");
        write_file(&file, b"not an instrument");
        let error = register(&catalogue, &file, None).unwrap_err();
        assert!(error.contains("not an instrument file"), "{error}");
        assert!(error.contains("notes.txt"), "{error}");
        assert!(!catalogue.exists(), "nothing may be written on refusal");
    }

    #[test]
    fn removing_a_library_keeps_the_files_and_backs_up() {
        let temp = TempDir::new("remove");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        let registered = register(&catalogue, &library, None).unwrap();
        let id = registered.library.id.clone();

        let outcome = remove(&catalogue, &id).unwrap();
        assert_eq!(outcome.removed.id, id);
        assert!(outcome.backup_path.is_some());
        assert!(library.join("a.sf2").exists(), "remove never deletes files");
        assert!(list(&catalogue).unwrap().libraries.is_empty());
        assert!(remove(&catalogue, &id).unwrap_err().contains("unknown"));
    }

    #[test]
    fn rescanning_picks_up_new_instruments() {
        let temp = TempDir::new("rescan");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        register(&catalogue, &library, None).unwrap();

        write_file(&library.join("nested/b.sfz"), b"new body");
        let outcome = rescan(&catalogue, None).unwrap();
        assert_eq!(outcome.libraries[0].instruments.len(), 2);
        assert!(!outcome.libraries[0].missing);
    }

    #[test]
    fn rescanning_a_vanished_root_marks_it_missing_instead_of_deleting_it() {
        let temp = TempDir::new("rescan-missing");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        register(&catalogue, &library, None).unwrap();
        fs::remove_dir_all(&library).unwrap();

        let outcome = rescan(&catalogue, None).unwrap();
        assert_eq!(outcome.libraries.len(), 1, "the row must survive");
        assert!(outcome.libraries[0].missing);
        assert!(outcome.libraries[0].instruments.is_empty());
        assert_eq!(outcome.warnings.len(), 1);
    }

    #[test]
    fn a_corrupt_catalogue_is_reported_not_overwritten() {
        let temp = TempDir::new("corrupt");
        let catalogue = temp.path().join("catalogue.json");
        write_file(&catalogue, b"{ not json");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);

        let error = register(&catalogue, &library, None).unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
        assert_eq!(fs::read(&catalogue).unwrap(), b"{ not json");
    }

    #[test]
    fn a_short_write_rolls_back_to_the_previous_catalogue() {
        let temp = TempDir::new("rollback");
        let library = temp.path().join("library");
        fake_library(&library, &["a.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        register(&catalogue, &library, None).unwrap();
        let previous = fs::read(&catalogue).unwrap();
        let mut catalogue_value = load(&catalogue).unwrap();
        catalogue_value.libraries[0].name = "Renamed".into();

        let error = commit_with(&catalogue, &catalogue_value, &mut |path, bytes| {
            // Write half the body, then fail the way a full disk would.
            fs::write(path, &bytes[..bytes.len() / 2])?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();

        assert!(error.contains("previous catalogue restored"), "{error}");
        assert_eq!(fs::read(&catalogue).unwrap(), previous);
        // The restored file is still the old, valid catalogue.
        assert_eq!(load(&catalogue).unwrap().libraries[0].name, "library");
    }

    #[test]
    fn a_failed_backup_aborts_before_writing() {
        let temp = TempDir::new("backup-failure");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fake_library(&first, &["a.sf2"]);
        fake_library(&second, &["b.sf2"]);
        let catalogue = temp.path().join("catalogue.json");
        register(&catalogue, &first, None).unwrap();
        let previous = fs::read(&catalogue).unwrap();
        // Make the backup destination un-writable by turning it into a directory.
        fs::create_dir_all(backup_path(&catalogue)).unwrap();

        let error = register(&catalogue, &second, None).unwrap_err();
        assert!(error.contains("back up"), "{error}");
        assert_eq!(fs::read(&catalogue).unwrap(), previous);
    }

    #[test]
    fn a_sampler_asset_manifest_is_one_named_instrument() {
        let temp = TempDir::new("asset");
        let pack = temp.path().join("pack");
        fs::create_dir_all(&pack).unwrap();
        write_file(&pack.join("entry.sfz"), b"<region>");
        write_file(
            &pack.join("manifest.json"),
            format!(
                r#"{{"schema":"{SAMPLER_ASSET_SCHEMA}","asset_id":"pluck","entrypoint":"entry.sfz","relocation_verified":true}}"#
            )
            .as_bytes(),
        );
        let catalogue = temp.path().join("catalogue.json");

        let outcome = register(&catalogue, &pack, None).unwrap();
        let instruments = &outcome.library.instruments;
        assert_eq!(instruments.len(), 1, "one path, one row");
        assert_eq!(instruments[0].name, "pluck");
        assert_eq!(instruments[0].format, "sampler_asset");
    }

    #[test]
    fn a_single_instrument_file_can_be_registered() {
        let temp = TempDir::new("single-file");
        let file = temp.path().join("grand.sf2");
        write_file(&file, b"body");
        let catalogue = temp.path().join("catalogue.json");
        let outcome = register(&catalogue, &file, Some("Grand")).unwrap();
        assert_eq!(outcome.library.kind, "file");
        assert_eq!(outcome.library.instruments.len(), 1);
        assert_eq!(outcome.library.instruments[0].name, "grand");
    }

    #[test]
    fn an_explicit_catalogue_path_wins_over_the_default() {
        let temp = TempDir::new("explicit");
        let path = temp.path().join("custom.json");
        assert_eq!(resolve_catalogue_path(Some(path.to_str().unwrap())), path);
        assert_eq!(resolve_catalogue_path(Some("  ")), default_catalogue_path());
        assert_eq!(resolve_catalogue_path(None), default_catalogue_path());
    }
}

// ---------------------------------------------------------------------------
// Tauri commands — the GUI front end, one of several callers
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_content_libraries(catalogue: Option<String>) -> Result<CatalogueListing, String> {
    list(&resolve_catalogue_path(catalogue.as_deref()))
}

#[tauri::command]
pub fn register_content_library(
    path: String,
    name: Option<String>,
    catalogue: Option<String>,
) -> Result<RegisterOutcome, String> {
    register(
        &resolve_catalogue_path(catalogue.as_deref()),
        Path::new(&path),
        name.as_deref(),
    )
}

#[tauri::command]
pub fn remove_content_library(
    id: String,
    catalogue: Option<String>,
) -> Result<RemoveOutcome, String> {
    remove(&resolve_catalogue_path(catalogue.as_deref()), &id)
}

#[tauri::command]
pub fn rescan_content_libraries(
    id: Option<String>,
    catalogue: Option<String>,
) -> Result<RescanOutcome, String> {
    rescan(&resolve_catalogue_path(catalogue.as_deref()), id.as_deref())
}
