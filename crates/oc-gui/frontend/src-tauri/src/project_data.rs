//! What the app keeps for a project outside the project's own folder, and the
//! one identity every store uses for it.
//!
//! - A project, and a workspace folder, is found by its **path key**: the
//!   first 8 bytes of the SHA-256 of its canonical path, in hex. The edit
//!   history, the AI note, the conversations of a workspace and the recovery
//!   copies all key by it, so one path names the same data everywhere.
//! - A project file is recognised after a move outside the app by its
//!   **content SHA-256**, which the history's marker records each time the app
//!   reads or writes the file.
//! - [`relocate`] moves or copies everything keyed by one path to another;
//!   Move Project, Save As and an open that recognised a moved file all call
//!   it. [`stale`] removes what belongs to files that are gone.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub mod relocate;
pub mod stale;

pub use relocate::{Relocation, relocate};

/// Largest file [`content_sha256`] reads; a larger file is not a project the
/// parser accepts.
pub const MAX_FINGERPRINT_BYTES: u64 = 256 * 1024 * 1024;

/// The canonical form of `path`. A file that does not exist (not yet saved,
/// or moved away) resolves through its folder, so its key is the same before
/// and after it exists.
pub fn canonical_path(path: &Path) -> PathBuf {
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

/// The path key of a project file or a workspace folder.
pub fn path_key(path: &Path) -> String {
    let canonical = canonical_path(path);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `name` has the shape of a path key.
pub fn is_path_key(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Whether `value` is a SHA-256 in lowercase or uppercase hex.
pub fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The SHA-256 of the file at `path`, in hex; `None` when it cannot be read or
/// is larger than any project.
pub fn content_sha256(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FINGERPRINT_BYTES {
        return None;
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut file.take(MAX_FINGERPRINT_BYTES), &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// The SHA-256 of `bytes`, in hex; `None` when they exceed any project.
pub fn content_sha256_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() as u64 > MAX_FINGERPRINT_BYTES {
        return None;
    }
    Some(format!("{:x}", Sha256::digest(bytes)))
}

/// Whether the file at `path` is gone, rather than out of reach. A file on a
/// volume that is not mounted is out of reach: the nearest folder that still
/// exists is then a volume root or the folder volumes mount under, and data
/// for it is kept.
pub fn is_gone(path: &Path) -> bool {
    if path.exists() {
        return false;
    }
    let mut nearest = path.parent();
    while let Some(dir) = nearest {
        if dir.exists() {
            return !is_mount_parent(dir);
        }
        nearest = dir.parent();
    }
    false
}

fn is_mount_parent(dir: &Path) -> bool {
    if dir.parent().is_none() {
        return true;
    }
    let text = dir.to_string_lossy();
    let parent = dir
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned());
    matches!(text.as_ref(), "/Volumes" | "/media" | "/mnt" | "/run/media")
        || matches!(parent.as_deref(), Some("/media" | "/run/media"))
}

/// Total size of the files under `dir`, recursively; 0 when it is missing.
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_key_is_stable_before_and_after_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("song.ocp");
        let before = path_key(&file);
        std::fs::write(&file, "x").unwrap();
        assert_eq!(path_key(&file), before);
        assert!(is_path_key(&before));
    }

    #[test]
    fn a_file_is_gone_only_when_its_volume_is_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("song.ocp");
        assert!(is_gone(&file));
        assert!(is_gone(&dir.path().join("deleted/folder/song.ocp")));
        std::fs::write(&file, "x").unwrap();
        assert!(!is_gone(&file));
        assert!(!is_gone(Path::new("/Volumes/NotMounted-oc-test/song.ocp")));
        assert!(!is_gone(Path::new("/mnt/not-mounted-oc-test/song.ocp")));
    }

    #[test]
    fn the_content_hash_follows_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("song.ocp");
        std::fs::write(&file, "abc").unwrap();
        assert_eq!(
            content_sha256(&file).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(content_sha256(&dir.path().join("missing.ocp")), None);
    }
}
