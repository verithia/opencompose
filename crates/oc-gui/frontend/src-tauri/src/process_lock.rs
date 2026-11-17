//! An exclusive lock a resource keeps for as long as one process uses it.
//!
//! The lock is an advisory whole-file lock held on a file under the app's data
//! folder. `flock` conflicts per open file description, so a process that
//! already holds a key keeps one shared description and reference count; the
//! same process may acquire the key again until every guard is dropped.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs4::{FileExt, TryLockError};

/// Why a resource lock was not acquired.
#[derive(Debug)]
pub enum LockError {
    /// Another open file description holds the lock.
    InUse,
    /// The lock file could not be opened or locked.
    Io(io::Error),
}

impl LockError {
    pub fn is_in_use(&self) -> bool {
        matches!(self, Self::InUse)
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InUse => formatter.write_str("another process is using this resource"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LockError {}

struct Held {
    _file: File,
    holders: usize,
}

fn held() -> &'static Mutex<HashMap<PathBuf, Held>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, Held>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One process's hold on a lock file. The last guard to drop releases it.
pub struct ProcessLock {
    path: PathBuf,
}

impl ProcessLock {
    /// Open and exclusively lock `path`, without waiting.
    ///
    /// The open file description stays in the process registry until every
    /// guard for this path drops. A second guard for a path this process
    /// already holds therefore succeeds even though a separate open of the
    /// same file would conflict, which is how the panel and its own turn share
    /// a conversation.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref().to_path_buf();
        let mut held = held().lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = held.get_mut(&path) {
            entry.holders += 1;
            return Ok(Self { path });
        }
        if let Some(parent) = path.parent() {
            crate::private_fs::ensure_dir(parent).map_err(LockError::Io)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(LockError::Io)?;
        match <File as FileExt>::try_lock(&file) {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::InUse),
            Err(TryLockError::Error(error)) => return Err(LockError::Io(error)),
        }
        held.insert(
            path.clone(),
            Held {
                _file: file,
                holders: 1,
            },
        );
        Ok(Self { path })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let mut held = held().lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = held.get_mut(&self.path) {
            entry.holders = entry.holders.saturating_sub(1);
            if entry.holders == 0 {
                // Dropping the last description releases the lock.
                held.remove(&self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn other_process(path: &Path) -> File {
        // A separate open is a separate file description, which is what a
        // second process presents to `flock`; the same process can hold the
        // key again through the registry while this description stays locked.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap();
        <File as FileExt>::try_lock(&file).unwrap();
        file
    }

    #[test]
    fn a_second_holder_is_refused_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks/project.test.lock");
        let guard = ProcessLock::acquire(&path).unwrap();
        {
            let again = ProcessLock::acquire(&path).unwrap();
            assert!(again.path == path, "same process re-acquires");
        }
        drop(guard);
        let foreign = other_process(&path);
        assert!(matches!(ProcessLock::acquire(&path), Err(LockError::InUse)));
        drop(foreign);
        assert!(ProcessLock::acquire(&path).is_ok());
    }
}
