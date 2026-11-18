//! Plugin folders the user adds on top of the platform's standard locations.
//!
//! The list is one small JSON file in the private config directory. Every
//! surface that searches for plugins (the inventory scan, the LV2 loader, the
//! Store's installed check and the first-run summary) reads it through
//! [`folders`], so a folder added once is searched everywhere.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Schema written into the folder file. Other versions are refused.
pub const SCHEMA: &str = "opencompose.plugin_folders.v1";
/// Overrides the folder file; harnesses and portable installs use it.
pub const FILE_ENV: &str = "OCP_PLUGIN_FOLDERS";
/// Each folder is walked on every scan, so the list stays short.
pub const MAX_FOLDERS: usize = 32;
/// A valid file of [`MAX_FOLDERS`] paths is far smaller than this.
const MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct FolderFile {
    schema: String,
    #[serde(default)]
    folders: Vec<PathBuf>,
}

/// Why a folder list could not be read or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderError {
    /// The path is not an existing directory.
    NotADirectory(PathBuf),
    /// The folder is already searched, either as a user or a standard folder.
    AlreadySearched(PathBuf),
    /// The folder is not in the user's list.
    NotListed(PathBuf),
    /// The list already holds [`MAX_FOLDERS`] entries.
    TooMany,
    /// The file exists but is not a folder list this version understands; it
    /// is never overwritten.
    Invalid(String),
    Io(String),
}

impl FolderError {
    /// Stable identifier for adapters; the UI chooses its own wording.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotADirectory(_) => "not_a_directory",
            Self::AlreadySearched(_) => "already_searched",
            Self::NotListed(_) => "not_listed",
            Self::TooMany => "too_many",
            Self::Invalid(_) => "invalid",
            Self::Io(_) => "io",
        }
    }
}

impl fmt::Display for FolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory(path) => write!(f, "'{}' is not a folder", path.display()),
            Self::AlreadySearched(path) => {
                write!(f, "'{}' is already searched for plugins", path.display())
            }
            Self::NotListed(path) => write!(f, "'{}' is not a plugin folder", path.display()),
            Self::TooMany => write!(f, "at most {MAX_FOLDERS} plugin folders can be added"),
            Self::Invalid(reason) => write!(f, "the plugin folder list is invalid: {reason}"),
            Self::Io(reason) => write!(f, "the plugin folder list could not be saved: {reason}"),
        }
    }
}

impl std::error::Error for FolderError {}

/// The folder file this process reads and writes.
pub fn file_path() -> PathBuf {
    std::env::var_os(FILE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| oc_audio::private_fs::config_dir().join("plugin-folders.json"))
}

/// The user's folders. A missing file is an empty list; an unreadable one is
/// logged and searched as empty, so a damaged file never blocks a scan.
pub fn folders() -> Vec<PathBuf> {
    match load(&file_path()) {
        Ok(folders) => folders,
        Err(error) => {
            log::warn!("{error}");
            Vec::new()
        }
    }
}

/// Add `folder` to the user's list and return the new list.
pub fn add(folder: &Path) -> Result<Vec<PathBuf>, FolderError> {
    add_to(&file_path(), folder, &standard_folders())
}

/// Remove `folder` from the user's list and return the new list.
pub fn remove(folder: &Path) -> Result<Vec<PathBuf>, FolderError> {
    remove_from(&file_path(), folder)
}

fn standard_folders() -> Vec<PathBuf> {
    super::standard_plugin_search_paths()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn load(file: &Path) -> Result<Vec<PathBuf>, FolderError> {
    let metadata = match std::fs::metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(FolderError::Io(error.to_string())),
    };
    if metadata.len() > MAX_FILE_BYTES {
        return Err(FolderError::Invalid(format!(
            "'{}' is larger than {MAX_FILE_BYTES} bytes",
            file.display()
        )));
    }
    let bytes = std::fs::read(file).map_err(|error| FolderError::Io(error.to_string()))?;
    let parsed: FolderFile = serde_json::from_slice(&bytes)
        .map_err(|error| FolderError::Invalid(format!("'{}': {error}", file.display())))?;
    if parsed.schema != SCHEMA {
        return Err(FolderError::Invalid(format!(
            "'{}' has schema '{}', expected '{SCHEMA}'",
            file.display(),
            parsed.schema
        )));
    }
    let mut folders = parsed.folders;
    folders.truncate(MAX_FOLDERS);
    Ok(folders)
}

fn save(file: &Path, folders: &[PathBuf]) -> Result<(), FolderError> {
    let bytes = serde_json::to_vec_pretty(&FolderFile {
        schema: SCHEMA.to_string(),
        folders: folders.to_vec(),
    })
    .map_err(|error| FolderError::Io(error.to_string()))?;
    oc_audio::private_fs::write_atomic(file, &bytes)
        .map_err(|error| FolderError::Io(error.to_string()))
}

/// Two spellings of one directory compare equal once both resolve.
fn same_folder(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (left.canonicalize(), right.canonicalize()),
            (Ok(left), Ok(right)) if left == right
        )
}

fn add_to(file: &Path, folder: &Path, standard: &[PathBuf]) -> Result<Vec<PathBuf>, FolderError> {
    let folder = folder
        .canonicalize()
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| FolderError::NotADirectory(folder.to_path_buf()))?;
    let mut folders = load(file)?;
    if folders
        .iter()
        .chain(standard)
        .any(|known| same_folder(known, &folder))
    {
        return Err(FolderError::AlreadySearched(folder));
    }
    if folders.len() >= MAX_FOLDERS {
        return Err(FolderError::TooMany);
    }
    folders.push(folder);
    save(file, &folders)?;
    Ok(folders)
}

fn remove_from(file: &Path, folder: &Path) -> Result<Vec<PathBuf>, FolderError> {
    let mut folders = load(file)?;
    let before = folders.len();
    // A folder that no longer exists cannot be canonicalized, so the stored
    // spelling is matched as well.
    folders.retain(|known| !same_folder(known, folder));
    if folders.len() == before {
        return Err(FolderError::NotListed(folder.to_path_buf()));
    }
    save(file, &folders)?;
    Ok(folders)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = oc_audio::private_fs::test_root()
                .join(format!("plugin-folders-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap()
        }

        fn file(&self) -> PathBuf {
            self.0.join("plugin-folders.json")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        let scratch = Scratch::new("missing");
        assert_eq!(load(&scratch.file()), Ok(Vec::new()));
    }

    #[test]
    fn an_added_folder_persists_and_is_removed_by_either_spelling() {
        let scratch = Scratch::new("add");
        let folder = scratch.dir("Plugins");
        let listed = add_to(&scratch.file(), &folder, &[]).unwrap();
        assert_eq!(listed, vec![folder.clone()]);
        assert_eq!(load(&scratch.file()).unwrap(), vec![folder.clone()]);

        let spelled = folder.join("..").join("Plugins");
        assert_eq!(remove_from(&scratch.file(), &spelled), Ok(Vec::new()));
        assert_eq!(load(&scratch.file()).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn duplicates_standard_folders_and_files_are_refused_without_a_write() {
        let scratch = Scratch::new("refuse");
        let folder = scratch.dir("Mine");
        let standard = scratch.dir("Standard");
        add_to(&scratch.file(), &folder, &[]).unwrap();
        let before = std::fs::read(scratch.file()).unwrap();

        assert!(matches!(
            add_to(&scratch.file(), &folder, &[]),
            Err(FolderError::AlreadySearched(_))
        ));
        assert!(matches!(
            add_to(&scratch.file(), &standard, std::slice::from_ref(&standard)),
            Err(FolderError::AlreadySearched(_))
        ));
        let file = scratch.0.join("not-a-folder.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            add_to(&scratch.file(), &file, &[]),
            Err(FolderError::NotADirectory(_))
        ));
        assert_eq!(std::fs::read(scratch.file()).unwrap(), before);
    }

    #[test]
    fn the_list_is_bounded() {
        let scratch = Scratch::new("bound");
        for index in 0..MAX_FOLDERS {
            add_to(&scratch.file(), &scratch.dir(&format!("f{index}")), &[]).unwrap();
        }
        assert_eq!(
            add_to(&scratch.file(), &scratch.dir("one-more"), &[]),
            Err(FolderError::TooMany)
        );
    }

    #[test]
    fn an_unknown_schema_is_never_overwritten() {
        let scratch = Scratch::new("schema");
        let original = br#"{"schema":"opencompose.plugin_folders.v9","folders":[]}"#;
        std::fs::write(scratch.file(), original).unwrap();
        assert!(matches!(
            add_to(&scratch.file(), &scratch.dir("x"), &[]),
            Err(FolderError::Invalid(_))
        ));
        assert_eq!(std::fs::read(scratch.file()).unwrap(), original);
    }

    #[test]
    fn a_vanished_folder_can_still_be_removed() {
        let scratch = Scratch::new("vanished");
        let folder = scratch.dir("Gone");
        add_to(&scratch.file(), &folder, &[]).unwrap();
        std::fs::remove_dir(&folder).unwrap();
        assert_eq!(remove_from(&scratch.file(), &folder), Ok(Vec::new()));
    }
}
