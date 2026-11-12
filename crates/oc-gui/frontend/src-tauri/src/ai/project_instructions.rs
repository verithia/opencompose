//! Per-project AI instructions under `~/.opencompose/ai/projects`.
//!
//! Layout:
//!
//! ```text
//! projects/
//!   index.json            key -> the project path it was derived from
//!   <key>.md              the note, exactly as written
//! ```
//!
//! The key is the first 8 bytes of the SHA-256 of the canonical project path,
//! so several `.ocp` files in one workspace keep separate notes. `index.json`
//! carries the readable path for the panel and for diagnosing a key; it is
//! written with the note, never on a read.
//!
//! The note is part of the system prompt, so its size is a prompt budget, not
//! just a storage limit: [`MAX_INSTRUCTION_CHARS`] is 4 000 characters, which is
//! [`MAX_INSTRUCTION_TOKENS`] at the panel's four-characters-per-token estimate.
//! A longer note is refused before anything is written; it is never truncated.
use crate::private_fs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Project instruction directory.
pub const DIR_NAME: &str = "projects";
/// Project instruction index file.
pub const INDEX_FILE: &str = "index.json";
/// Longest note a project may keep.
pub const MAX_INSTRUCTION_CHARS: usize = 4_000;
/// The note's share of the system prompt budget, at four characters per token.
pub const MAX_INSTRUCTION_TOKENS: usize = MAX_INSTRUCTION_CHARS / 4;
/// Index entries kept; the oldest `updated` is dropped first.
pub const MAX_INDEX_ENTRIES: usize = 256;

const VERSION: u32 = 1;

#[cfg(test)]
static TEST_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// One project's note. `project` is the canonical path the note belongs to, and
/// `text` is empty when the project has no note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInstructions {
    /// `None` when the request named no project and none is open.
    pub project: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// The cap the store enforces, so the panel shows the store's number.
    pub max_chars: usize,
}

impl ProjectInstructions {
    /// A project that has no path, and therefore no note.
    pub fn none() -> Self {
        Self {
            project: None,
            text: String::new(),
            updated: None,
            max_chars: MAX_INSTRUCTION_CHARS,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    projects: BTreeMap<String, IndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexEntry {
    path: String,
    updated: String,
}

/// The note store. `root` is the directory holding `index.json` and the notes.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The store under the user's private configuration directory.
    pub fn config() -> Self {
        #[cfg(test)]
        if let Ok(root) = TEST_ROOT.lock()
            && let Some(root) = root.as_ref()
        {
            return Self::new(root.clone());
        }
        Self::new(private_fs::config_dir().join("ai").join(DIR_NAME))
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The note for `project`, empty when it has none. A project that moved
    /// keeps its note when the index has exactly one note for the same file
    /// name at a path that is gone; two candidates are ambiguous and refused.
    pub fn load(&self, project: &Path) -> Result<ProjectInstructions, String> {
        let canonical = canonical_project(project).to_string_lossy().to_string();
        let index = self.read_index();
        let key = key_for(project);
        let (key, text) = match self.read_note(&key)? {
            Some(text) => (key, text),
            None => match self.related_note(project, &index, &canonical)? {
                Some(found) => found,
                None => (key, String::new()),
            },
        };
        let updated = index
            .projects
            .get(&key)
            .map(|entry| entry.updated.clone())
            .filter(|_| !text.is_empty());
        Ok(ProjectInstructions {
            project: Some(canonical),
            text,
            updated,
            max_chars: MAX_INSTRUCTION_CHARS,
        })
    }

    /// The note recorded under the same file name at another path, when that
    /// path is gone. One candidate is a move; several are not a reason to
    /// borrow a note, so the lookup refuses them.
    fn related_note(
        &self,
        project: &Path,
        index: &Index,
        canonical: &str,
    ) -> Result<Option<(String, String)>, String> {
        let Some(name) = project.file_name() else {
            return Ok(None);
        };
        let candidates: Vec<&String> = index
            .projects
            .iter()
            .filter(|(_, entry)| {
                entry.path != canonical
                    && Path::new(&entry.path).file_name() == Some(name)
                    && !Path::new(&entry.path).exists()
            })
            .map(|(key, _)| key)
            .filter(|key| self.note_path(key).exists())
            .collect();
        if candidates.len() != 1 {
            return Ok(None);
        }
        let key = candidates[0].clone();
        match self.read_note(&key)? {
            Some(text) => Ok(Some((key, text))),
            None => Ok(None),
        }
    }

    fn read_note(&self, key: &str) -> Result<Option<String>, String> {
        match std::fs::read_to_string(self.note_path(key)) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("project instructions: {error}")),
        }
    }

    /// Writes the note, or clears it when `text` is blank. Refuses a note above
    /// [`MAX_INSTRUCTION_CHARS`] before touching the disk.
    pub fn save(&self, project: &Path, text: &str) -> Result<ProjectInstructions, String> {
        let text = text.trim();
        let key = key_for(project);
        let path = self.note_path(&key);
        let canonical = canonical_project(project);
        let canonical = canonical.to_string_lossy().to_string();
        if text.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("project instructions: {error}")),
            }
            let mut index = self.read_index();
            if index.projects.remove(&key).is_some() {
                self.record_index(&mut index);
            }
            return Ok(ProjectInstructions {
                project: Some(canonical),
                text: String::new(),
                updated: None,
                max_chars: MAX_INSTRUCTION_CHARS,
            });
        }
        let chars = text.chars().count();
        if chars > MAX_INSTRUCTION_CHARS {
            return Err(format!(
                "project instructions are {chars} characters; the limit is {MAX_INSTRUCTION_CHARS} \
                 (about {MAX_INSTRUCTION_TOKENS} prompt tokens). Shorten the note; nothing was saved."
            ));
        }
        private_fs::write_atomic(&path, text.as_bytes())
            .map_err(|error| format!("project instructions: {error}"))?;
        let updated = crate::ai::sessions::now();
        let mut index = self.read_index();
        index.projects.insert(
            key,
            IndexEntry {
                path: canonical.clone(),
                updated: updated.clone(),
            },
        );
        self.record_index(&mut index);
        Ok(ProjectInstructions {
            project: Some(canonical),
            text: text.to_owned(),
            updated: Some(updated),
            max_chars: MAX_INSTRUCTION_CHARS,
        })
    }

    fn note_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.md"))
    }

    /// A missing or unreadable index is treated as empty: the notes stand on
    /// their own, so a damaged index loses the readable path, not the text.
    fn read_index(&self) -> Index {
        std::fs::read(self.root.join(INDEX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, index: &mut Index) -> Result<(), String> {
        index.v = VERSION;
        while index.projects.len() > MAX_INDEX_ENTRIES {
            let Some(oldest) = index
                .projects
                .iter()
                .min_by(|left, right| left.1.updated.cmp(&right.1.updated))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            index.projects.remove(&oldest);
        }
        let bytes = serde_json::to_vec_pretty(&*index).map_err(|error| error.to_string())?;
        private_fs::write_atomic(&self.root.join(INDEX_FILE), &bytes)
            .map_err(|error| format!("project instructions: {error}"))
    }

    /// The note is what the prompt reads and the index only names its path, so a
    /// failed index write is reported and does not fail the save.
    fn record_index(&self, index: &mut Index) {
        if let Err(error) = self.write_index(index) {
            log::warn!("[ai] project instructions index: {error}");
        }
    }
}

/// The key a project's note lives under: the first 8 bytes of the SHA-256 of its
/// canonical path, in hex.
pub fn key_for(project: &Path) -> String {
    let canonical = canonical_project(project);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let mut key = String::with_capacity(16);
    for byte in &digest[..8] {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// The project path as the store keys it. A file that does not exist yet keeps
/// its name under a canonical directory, so a note written before the first
/// save still matches the note read after it.
fn canonical_project(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Points the store at a fixture directory for the duration of one test.
#[cfg(test)]
pub fn test_root(root: &Path) -> TestRootGuard {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut slot) = TEST_ROOT.lock() {
        *slot = Some(root.to_path_buf());
    }
    TestRootGuard { _lock: lock }
}

#[cfg(test)]
pub struct TestRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
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
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(label: &str) -> (Fixture, Store) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "oc-project-instructions-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(dir.join("projects"));
        (Fixture { dir }, store)
    }

    /// A project file that exists, so the key is the canonical file path.
    fn project(fixture: &Fixture, name: &str) -> PathBuf {
        let path = fixture.dir.join(name);
        std::fs::write(&path, "@project\n").unwrap();
        path
    }

    #[test]
    fn two_projects_keep_their_own_note() {
        let (fixture, store) = fixture("separate");
        let a = project(&fixture, "a.ocp");
        let b = project(&fixture, "b.ocp");
        store.save(&a, "always print stems at -14 LUFS").unwrap();
        store.save(&b, "this song is in D minor").unwrap();
        assert_eq!(
            store.load(&a).unwrap().text,
            "always print stems at -14 LUFS"
        );
        assert_eq!(store.load(&b).unwrap().text, "this song is in D minor");
        assert_ne!(key_for(&a), key_for(&b));
        // The note of one project is not the note of any other project.
        assert!(!store.load(&b).unwrap().text.contains("-14 LUFS"));
    }

    #[test]
    fn a_cleared_note_leaves_no_file_and_no_index_entry() {
        let (fixture, store) = fixture("cleared");
        let a = project(&fixture, "a.ocp");
        store.save(&a, "keep the low end mono").unwrap();
        let cleared = store.save(&a, "  \n\t ").unwrap();
        assert!(cleared.text.is_empty());
        assert!(cleared.updated.is_none());
        assert!(!store.note_path(&key_for(&a)).exists());
        let index = store.read_index();
        assert!(index.projects.is_empty());
        assert!(store.load(&a).unwrap().text.is_empty());
    }

    #[test]
    fn a_moved_project_keeps_its_note() {
        let (fixture, store) = fixture("moved");
        let old = project(&fixture, "song.ocp");
        store.save(&old, "half-time feel").unwrap();
        let moved_dir = fixture.dir.join("moved");
        std::fs::create_dir_all(&moved_dir).unwrap();
        let moved = moved_dir.join("song.ocp");
        std::fs::rename(&old, &moved).unwrap();

        let loaded = store.load(&moved).unwrap();
        assert_eq!(loaded.text, "half-time feel");
        // The note belongs to the project that is open now, not the old path.
        assert_eq!(
            loaded.project.as_deref(),
            Some(canonical_project(&moved).to_string_lossy().as_ref())
        );
        assert!(loaded.updated.is_some());
    }

    #[test]
    fn two_indexed_notes_for_one_file_name_are_ambiguous_and_refused() {
        let (fixture, store) = fixture("ambiguous");
        for dir in ["first", "second"] {
            let path = fixture.dir.join(dir);
            std::fs::create_dir_all(&path).unwrap();
            let project = path.join("song.ocp");
            std::fs::write(&project, "@project\n").unwrap();
            store.save(&project, &format!("note from {dir}")).unwrap();
            // Both recorded paths are gone, as they would be after two moves.
            std::fs::remove_file(&project).unwrap();
        }
        let third = fixture.dir.join("third");
        std::fs::create_dir_all(&third).unwrap();
        let moved = third.join("song.ocp");
        std::fs::write(&moved, "@project\n").unwrap();

        // Two candidates are not a reason to borrow one project's note.
        assert!(store.load(&moved).unwrap().text.is_empty());
    }

    #[test]
    fn a_note_over_the_cap_is_refused_and_the_stored_one_survives() {
        let (fixture, store) = fixture("cap");
        let a = project(&fixture, "a.ocp");
        let at_cap = "x".repeat(MAX_INSTRUCTION_CHARS);
        store.save(&a, &at_cap).unwrap();
        let over = "x".repeat(MAX_INSTRUCTION_CHARS + 1);
        let error = store.save(&a, &over).unwrap_err();
        assert!(error.contains("4001"), "{error}");
        assert!(error.contains("4000"), "{error}");
        assert!(error.contains("nothing was saved"), "{error}");
        assert_eq!(store.load(&a).unwrap().text, at_cap);
    }

    #[test]
    fn the_note_and_its_index_survive_a_new_store() {
        let (fixture, store) = fixture("restart");
        let a = project(&fixture, "a.ocp");
        store.save(&a, "do not modulate").unwrap();
        let reopened = Store::new(store.root().to_path_buf());
        let loaded = reopened.load(&a).unwrap();
        assert_eq!(loaded.text, "do not modulate");
        assert!(loaded.updated.is_some());
        let index = reopened.read_index();
        let entry = index.projects.get(&key_for(&a)).unwrap();
        assert_eq!(entry.path, canonical_project(&a).to_string_lossy());
    }

    #[test]
    fn the_note_is_stored_trimmed() {
        let (fixture, store) = fixture("trim");
        let a = project(&fixture, "a.ocp");
        store.save(&a, "\n  stems at -14 LUFS  \n\n").unwrap();
        assert_eq!(store.load(&a).unwrap().text, "stems at -14 LUFS");
    }

    #[test]
    fn the_same_project_written_two_ways_finds_the_same_note() {
        let (fixture, store) = fixture("canonical");
        let a = project(&fixture, "a.ocp");
        store.save(&a, "half-time feel").unwrap();
        // A path with a detour resolves to the same canonical file.
        std::fs::create_dir_all(fixture.dir.join("sub")).unwrap();
        let indirect = fixture.dir.join("sub").join("..").join("a.ocp");
        assert_eq!(key_for(&indirect), key_for(&a));
        assert_eq!(store.load(&indirect).unwrap().text, "half-time feel");
    }

    #[test]
    fn a_note_written_before_the_first_save_is_found_after_it() {
        let (fixture, store) = fixture("unsaved");
        let path = fixture.dir.join("later.ocp");
        store.save(&path, "no reverb on the lead").unwrap();
        assert_eq!(store.load(&path).unwrap().text, "no reverb on the lead");
        // Creating the file must not move the note to another key.
        std::fs::write(&path, "@project\n").unwrap();
        assert_eq!(key_for(&path), key_for(&path));
        assert_eq!(store.load(&path).unwrap().text, "no reverb on the lead");
    }

    #[test]
    fn the_command_refuses_a_write_with_no_project() {
        let error =
            crate::commands::instructions_set(None, None, "always print stems").unwrap_err();
        assert!(error.contains("save the project"), "{error}");
        let empty = crate::commands::instructions_get(None, None).unwrap();
        assert!(empty.text.is_empty());
        assert!(empty.project.is_none());
        assert_eq!(empty.max_chars, MAX_INSTRUCTION_CHARS);
    }

    #[test]
    fn the_command_reads_and_writes_the_open_project() {
        let (fixture, store) = fixture("command");
        let _guard = test_root(store.root());
        let open = project(&fixture, "open.ocp");
        let written = crate::commands::instructions_set(None, Some(&open), "48 kHz stems").unwrap();
        assert_eq!(written.text, "48 kHz stems");
        let read = crate::commands::instructions_get(None, Some(&open)).unwrap();
        assert_eq!(read.text, "48 kHz stems");
        assert_eq!(read.max_chars, MAX_INSTRUCTION_CHARS);
    }

    #[test]
    fn the_index_keeps_the_newest_entries() {
        let (fixture, store) = fixture("index");
        let mut index = Index::default();
        for step in 0..MAX_INDEX_ENTRIES + 3 {
            index.projects.insert(
                format!("{step:016x}"),
                IndexEntry {
                    path: fixture
                        .dir
                        .join(format!("{step}.ocp"))
                        .to_string_lossy()
                        .to_string(),
                    updated: format!("2026-10-30T00:00:{step:02}.000Z"),
                },
            );
        }
        store.write_index(&mut index).unwrap();
        let read = store.read_index();
        assert_eq!(read.projects.len(), MAX_INDEX_ENTRIES);
        // The three oldest went first.
        assert!(!read.projects.contains_key("0000000000000000"));
        assert!(
            read.projects
                .contains_key(&format!("{:016x}", MAX_INDEX_ENTRIES + 2))
        );
    }
}
