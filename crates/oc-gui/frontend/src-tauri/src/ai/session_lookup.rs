//! Fallible saved-session lookup. Inventory errors remain distinguishable from
//! absent sessions, while a damaged unrelated candidate cannot hide a match.
use super::{META_FILE, SessionMeta, is_valid_id, is_valid_key};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(in crate::ai) enum Error {
    Io(io::Error),
    InvalidId,
    Missing,
    Corrupt(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::InvalidId => f.write_str("Invalid session id"),
            Self::Missing => f.write_str("No matching saved session"),
            Self::Corrupt(error) => write!(f, "Invalid session metadata: {error}"),
        }
    }
}
#[derive(Debug, Default)]
pub(in crate::ai) struct Workspaces {
    pub keys: Vec<String>,
    pub error: Option<Error>,
}
pub(in crate::ai) trait Io: std::fmt::Debug + Send + Sync {
    fn workspaces(&self, root: &Path) -> Workspaces;
    fn metadata(&self, path: &Path) -> Result<Option<SessionMeta>, Error>;
}
#[derive(Debug)]
pub(in crate::ai) struct Disk;
impl Io for Disk {
    fn workspaces(&self, root: &Path) -> Workspaces {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Workspaces::default(),
            Err(error) => {
                return Workspaces {
                    error: Some(Error::Io(error)),
                    ..Default::default()
                };
            }
        };
        let mut result = Workspaces::default();
        for entry in entries {
            match entry {
                Err(error) => {
                    result.error.get_or_insert(Error::Io(error));
                }
                Ok(entry) => {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !is_valid_key(&name) {
                        continue;
                    }
                    match entry.file_type() {
                        Ok(kind) if kind.is_dir() => result.keys.push(name),
                        Ok(_) => {}
                        Err(error) => {
                            result.error.get_or_insert(Error::Io(error));
                        }
                    }
                }
            }
        }
        result.keys.sort();
        result
    }
    fn metadata(&self, path: &Path) -> Result<Option<SessionMeta>, Error> {
        read_meta(path)
    }
}
pub(super) fn find(io: &dyn Io, root: &Path, id: &str) -> Result<(PathBuf, SessionMeta), Error> {
    if !is_valid_id(id) {
        return Err(Error::InvalidId);
    }
    let Workspaces { keys, mut error } = io.workspaces(root);
    for key in keys {
        let dir = root.join(key).join(id);
        match io.metadata(&dir.join(META_FILE)) {
            Ok(Some(meta)) if meta.id == id => return Ok((dir, meta)),
            Ok(Some(_)) => {
                error.get_or_insert(Error::Corrupt(
                    "Session identity does not match its directory".into(),
                ));
            }
            Ok(None) => {}
            Err(failure) => {
                if matches!(failure, Error::Io(_)) || error.is_none() {
                    error = Some(failure);
                }
            }
        }
    }
    Err(error.unwrap_or(Error::Missing))
}
pub(super) fn read_meta(path: &Path) -> Result<Option<SessionMeta>, Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(Error::Io(error)),
    };
    const MAX_BYTES: u64 = 1024 * 1024;
    let metadata = file.metadata().map_err(Error::Io)?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if reparse || !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(Error::Corrupt(
            "Metadata must be a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::Io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::Corrupt("Metadata exceeds its storage budget".into()));
    }
    let meta: SessionMeta =
        serde_json::from_slice(&bytes).map_err(|error| Error::Corrupt(error.to_string()))?;
    if !is_valid_id(&meta.id) {
        return Err(Error::Corrupt("Invalid session identity".into()));
    }
    Ok(Some(meta))
}
