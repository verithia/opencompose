//! Recent workspaces under `~/.opencompose/workspaces.json`: the directories the
//! operator has opened, newest first, bounded to [`MAX_RECENTS`].
//!
//! A workspace is a directory, not one `.ocp`: every project file in it shares
//! one set of AI conversations (see `commands::sessions`). The registry only
//! remembers where those directories are, so the start surface can list them.
//! Writes go through [`private_fs::write_atomic`] like the other stores.
use crate::private_fs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Workspace registry file name.
pub const FILE_NAME: &str = "workspaces.json";
/// How many recent directories the registry keeps. Older ones are dropped from
/// the front, so the file stays small and the chooser stays scannable.
pub const MAX_RECENTS: usize = 12;

const VERSION: u32 = 1;

#[cfg(test)]
static TEST_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
#[cfg(test)]
static TEST_ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One remembered directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// The directory's base name, for a row that shows a name and a path.
    pub name: String,
    /// Absolute, canonical path.
    pub path: String,
    /// When it was last opened, newest first in the list.
    pub opened: String,
    /// Whether the directory is still there, computed by [`Registry::list`]. It
    /// is not stored: a remembered folder can be deleted at any time.
    #[serde(default = "present", skip_serializing_if = "absent")]
    pub exists: bool,
}

fn present() -> bool {
    true
}

fn absent(value: &bool) -> bool {
    *value
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default = "version")]
    v: u32,
    #[serde(default)]
    workspaces: Vec<WorkspaceEntry>,
}

fn version() -> u32 {
    VERSION
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            v: VERSION,
            workspaces: Vec::new(),
        }
    }
}

/// The registry file. `new` takes its path so a test never writes the
/// operator's `~/.opencompose`.
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The operator's registry: `~/.opencompose/workspaces.json`.
    pub fn config() -> Self {
        #[cfg(test)]
        if let Ok(root) = TEST_ROOT.lock()
            && let Some(root) = root.as_ref()
        {
            return Self::new(root.join(FILE_NAME));
        }
        Self::new(private_fs::config_dir().join(FILE_NAME))
    }

    /// Remembered directories, newest first, each saying whether it is still there.
    pub fn list(&self) -> Result<Vec<WorkspaceEntry>, String> {
        let mut entries = self.read().workspaces;
        for entry in &mut entries {
            entry.exists = Path::new(&entry.path).is_dir();
        }
        entries.sort_by(|left, right| {
            right
                .opened
                .cmp(&left.opened)
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(entries)
    }

    /// Puts `dir` at the front with the given timestamp, replacing an earlier
    /// entry for the same directory, and drops anything past [`MAX_RECENTS`].
    pub fn remember(&self, dir: &Path, at: &str) -> Result<WorkspaceEntry, String> {
        let path = canonical_dir(dir)?;
        let entry = WorkspaceEntry {
            name: display_name(&path),
            path: path.to_string_lossy().to_string(),
            opened: at.to_owned(),
            exists: true,
        };
        let mut file = self.read();
        file.workspaces.retain(|known| known.path != entry.path);
        file.workspaces.insert(0, entry.clone());
        file.workspaces.truncate(MAX_RECENTS);
        file.v = VERSION;
        self.write(&file)?;
        Ok(entry)
    }

    /// Removes one directory from the list. A directory that is already gone can
    /// still be forgotten, by the path the list holds. Refuses anything else.
    pub fn forget(&self, dir: &Path) -> Result<(), String> {
        let stored = canonical_dir(dir)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| dir.to_string_lossy().to_string());
        let mut file = self.read();
        let before = file.workspaces.len();
        file.workspaces.retain(|known| known.path != stored);
        if file.workspaces.len() == before {
            return Err(format!("no recent workspace at {stored}"));
        }
        self.write(&file)
    }

    /// A missing or unreadable file is an empty list: the registry is a
    /// convenience, and one bad byte must not block the start surface.
    fn read(&self) -> RegistryFile {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RegistryFile>(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, file: &RegistryFile) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(file).map_err(|error| error.to_string())?;
        private_fs::write_atomic(&self.path, &bytes).map_err(|error| format!("workspaces: {error}"))
    }
}

/// The absolute, canonical form of a directory that exists.
pub fn canonical_dir(dir: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(dir)
        .map_err(|error| format!("workspace {}: {error}", dir.display()))?;
    if !canonical.is_dir() {
        return Err(format!("workspace {}: not a directory", dir.display()));
    }
    Ok(canonical)
}

/// A directory's own name; the path itself when it has none (a filesystem root).
pub fn display_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| dir.to_string_lossy().to_string())
}

/// Points [`Registry::config`] at a temporary directory for one test, so a
/// command test never writes the operator's registry.
#[cfg(test)]
pub(crate) struct TestRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) fn test_root(root: PathBuf) -> TestRootGuard {
    let lock = TEST_ROOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_ROOT.lock() {
        *slot = Some(root);
    }
    TestRootGuard { _lock: lock }
}

#[cfg(test)]
impl Drop for TestRootGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_ROOT.lock() {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(label: &str) -> Fixture {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "opencompose-workspaces-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn directory(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recents_are_newest_first_and_reopening_moves_a_directory_to_the_front() {
        let fixture = fixture("order");
        let registry = Registry::new(fixture.root.join(FILE_NAME));
        let first = directory(&fixture.root, "first songs");
        let second = directory(&fixture.root, "second songs");

        registry
            .remember(&first, "2026-10-29T08:00:00.000Z")
            .unwrap();
        registry
            .remember(&second, "2026-10-29T09:00:00.000Z")
            .unwrap();
        let listed = registry.list().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["second songs", "first songs"]
        );

        // Opening the older one again is one entry, at the front, with a new time.
        registry
            .remember(&first, "2026-10-29T10:00:00.000Z")
            .unwrap();
        let listed = registry.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "first songs");
        assert_eq!(listed[0].opened, "2026-10-29T10:00:00.000Z");
        // The stored path is canonical and absolute.
        assert_eq!(
            listed[0].path,
            first.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn the_list_is_bounded_and_a_forgotten_directory_leaves_it() {
        let fixture = fixture("bound");
        let registry = Registry::new(fixture.root.join(FILE_NAME));
        for index in 0..(MAX_RECENTS + 4) {
            let dir = directory(&fixture.root, &format!("songs-{index:02}"));
            registry
                .remember(&dir, &format!("2026-10-29T08:{index:02}:00.000Z"))
                .unwrap();
        }
        let listed = registry.list().unwrap();
        assert_eq!(listed.len(), MAX_RECENTS);
        // The oldest entries fell off the back.
        assert_eq!(listed[0].name, format!("songs-{:02}", MAX_RECENTS + 3));
        assert!(!listed.iter().any(|entry| entry.name == "songs-00"));

        let gone = fixture.root.join("songs-05");
        registry.forget(&gone).unwrap();
        assert!(
            !registry
                .list()
                .unwrap()
                .iter()
                .any(|entry| entry.name == "songs-05")
        );
        // Forgetting twice is a refusal, not a rewrite.
        assert!(registry.forget(&gone).is_err());
    }

    #[test]
    fn a_missing_or_unreadable_registry_is_an_empty_list() {
        let fixture = fixture("empty");
        let path = fixture.root.join(FILE_NAME);
        let registry = Registry::new(&path);
        assert!(registry.list().unwrap().is_empty());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(registry.list().unwrap().is_empty());
        // Remembering over a corrupt file starts a clean one.
        let dir = directory(&fixture.root, "songs");
        registry.remember(&dir, "2026-10-29T08:00:00.000Z").unwrap();
        assert_eq!(registry.list().unwrap().len(), 1);
        let written: RegistryFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written.v, VERSION);
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_refused_by_name() {
        let fixture = fixture("file");
        let registry = Registry::new(fixture.root.join(FILE_NAME));
        let file = fixture.root.join("song.ocp");
        std::fs::write(&file, "@project").unwrap();
        let error = registry
            .remember(&file, "2026-10-29T08:00:00.000Z")
            .unwrap_err();
        assert!(error.contains("not a directory"), "{error}");
        assert!(registry.list().unwrap().is_empty());
    }
}
