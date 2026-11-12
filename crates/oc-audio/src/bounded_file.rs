//! Bounded regular-file I/O for shared processor-state resources. Resource paths
//! and format decoding belong to callers; this owner validates the opened handle.

/// Maximum encoded state resource accepted by application/editor readers.
pub const MAX_PLUGIN_STATE_BYTES: usize = 64 * 1024 * 1024;

use std::fs::{File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::Path;

/// A validated file and its captured metadata; cache lookups and reads therefore
/// refer to one opened resource even if its path is replaced afterwards.
pub struct BoundedRegularFile {
    file: File,
    metadata: Metadata,
    limit: usize,
}

impl BoundedRegularFile {
    /// Open a regular file, rejecting special files, and bind it to `limit`.
    pub fn open(path: &Path, limit: usize) -> Result<Self, String> {
        checked_read_limit(limit)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Open FIFOs/devices without waiting for a writer, then reject by handle.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                #[link_name = "GetFileType"]
                fn get_file_type(handle: *mut std::ffi::c_void) -> u32;
            }
            const FILE_TYPE_DISK: u32 = 1;
            // SAFETY: File owns the live handle throughout this query.
            if unsafe { get_file_type(file.as_raw_handle()) } != FILE_TYPE_DISK {
                return Err("plugin state must be a regular file".into());
            }
        }
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("plugin state must be a regular file".into());
        }
        if metadata.len()
            > u64::try_from(limit).map_err(|_| "plugin state limit is out of range")?
        {
            return Err(limit_error(limit));
        }
        Ok(Self {
            file,
            metadata,
            limit,
        })
    }

    /// Metadata captured when the file was opened.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Read the bound bytes, refusing a file that changed while reading.
    pub fn read(mut self) -> Result<Vec<u8>, String> {
        let bytes = read_state_bytes(&mut self.file, self.limit)?;
        let after = self.file.metadata().map_err(|error| error.to_string())?;
        if u64::try_from(bytes.len()).ok() != Some(self.metadata.len())
            || after.len() != self.metadata.len()
            || after.modified().ok() != self.metadata.modified().ok()
        {
            return Err("plugin state file changed while reading".into());
        }
        Ok(bytes)
    }
}

/// Open and read a bounded regular state file.
pub fn read_regular_state_file(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    BoundedRegularFile::open(path, limit)?.read()
}

fn checked_read_limit(limit: usize) -> Result<u64, String> {
    u64::try_from(limit)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| "plugin state limit is out of range".into())
}
fn limit_error(limit: usize) -> String {
    format!("plugin state exceeds the {limit}-byte limit")
}

/// Read at most limit+1, rejecting growth before extending the output. Empty
/// chunks are valid here; editor/application policy decides whether to accept them.
pub fn read_state_bytes(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut reader = reader.take(checked_read_limit(limit)?);
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => return Ok(output),
            Ok(count) => count,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("could not read state data: {error}")),
        };
        let wanted = output
            .len()
            .checked_add(count)
            .filter(|length| *length <= limit)
            .ok_or_else(|| limit_error(limit))?;
        if wanted > output.capacity() {
            let capacity = output
                .capacity()
                .max(8192)
                .saturating_mul(2)
                .min(limit)
                .max(wanted);
            output
                .try_reserve_exact(capacity - output.len())
                .map_err(|error| format!("could not allocate bounded plugin state: {error}"))?;
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_regular_file_checks_actual_size_and_detects_growth_after_open() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.bin");
        std::fs::write(&path, b"1234").unwrap();
        assert_eq!(read_regular_state_file(&path, 4).unwrap(), b"1234");
        assert!(BoundedRegularFile::open(&path, 3).is_err());
        assert!(BoundedRegularFile::open(root.path(), 4).is_err());
        let checked = BoundedRegularFile::open(&path, 4).unwrap();
        assert_eq!(checked.metadata().len(), 4);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"5")
            .unwrap();
        assert!(checked.read().unwrap_err().contains("4-byte limit"));
    }

    #[test]
    fn bounded_regular_file_refuses_in_budget_length_changes_after_validation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.bin");
        std::fs::write(&path, b"12").unwrap();
        let grows = BoundedRegularFile::open(&path, 8).unwrap();
        std::fs::write(&path, b"123").unwrap();
        assert!(grows.read().unwrap_err().contains("changed while reading"));
        let shrinks = BoundedRegularFile::open(&path, 8).unwrap();
        std::fs::write(&path, b"1").unwrap();
        assert!(
            shrinks
                .read()
                .unwrap_err()
                .contains("changed while reading")
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_regular_file_allows_regular_symlinks_and_keeps_opened_resource_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.bin");
        let link = root.path().join("state.link");
        std::fs::write(&path, b"old").unwrap();
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(read_regular_state_file(&link, 4).unwrap(), b"old");
        let checked = BoundedRegularFile::open(&path, 4).unwrap();
        std::fs::rename(&path, root.path().join("retired.bin")).unwrap();
        std::fs::write(&path, b"new").unwrap();
        assert_eq!(checked.read().unwrap(), b"old");
        assert_eq!(read_regular_state_file(&path, 4).unwrap(), b"new");
    }

    #[test]
    fn bounded_state_reader_accepts_exact_empty_and_refuses_growth() {
        assert_eq!(read_state_bytes(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_state_bytes(&b""[..], 4).unwrap().is_empty());
        assert!(
            read_state_bytes(std::io::repeat(1), 4)
                .unwrap_err()
                .contains("4-byte limit")
        );
        assert!(read_state_bytes(&b"1"[..], 0).is_err());
    }
    #[test]
    fn bounded_state_reader_preserves_original_read_failure() {
        struct Fails;
        impl Read for Fails {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture read fault"))
            }
        }
        assert!(
            read_state_bytes(Fails, 4)
                .unwrap_err()
                .contains("fixture read fault")
        );
        #[cfg(target_pointer_width = "64")]
        assert!(read_state_bytes(&b""[..], usize::MAX).is_err());
    }
}
