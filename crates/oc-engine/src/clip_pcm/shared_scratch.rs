//! Cross-process admission for temporary clip audio.

use std::fs::{self, File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{ClipPreparationError, lock_spool_directory, open_cache_lock, spool_error};

const PREFIX: &str = "lease-";
const STAGED_PCM_PREFIX: &str = "pcm-";
const RESERVATION_STEP: u64 = 256 * 1024;

pub(super) struct SharedScratchLease {
    root: PathBuf,
    record: Option<tempfile::TempDir>,
    owner: Option<File>,
    amount: Option<File>,
    reserved: u64,
}

fn pool_path(dir: &Path, pool: &str) -> PathBuf {
    dir.join(format!(".clip-scratch-{pool}"))
}

impl SharedScratchLease {
    pub(super) fn new(dir: &Path, pool: &str, cap: u64) -> std::io::Result<Self> {
        let root = pool_path(dir, pool);
        fs::create_dir_all(&root)?;
        let _gate = lock_spool_directory(&root)?;
        let record = tempfile::Builder::new().prefix(PREFIX).tempdir_in(&root)?;
        let owner = open_cache_lock(&record.path().join("owner"), true)?;
        owner.lock()?;
        let mut amount = open_cache_lock(&record.path().join("amount"), true)?;
        amount.write_all(&cap.to_le_bytes())?;
        amount.write_all(&0_u64.to_le_bytes())?;
        Ok(Self {
            root,
            record: Some(record),
            owner: Some(owner),
            amount: Some(amount),
            reserved: 0,
        })
    }

    pub(super) fn stage_pcm(&self) -> std::io::Result<tempfile::NamedTempFile> {
        let record = self
            .record
            .as_ref()
            .expect("an active lease has a directory");
        tempfile::Builder::new()
            .prefix(STAGED_PCM_PREFIX)
            .tempfile_in(record.path())
    }

    pub(super) fn grow(
        &mut self,
        size: u64,
        cap: u64,
        incremental: bool,
    ) -> Result<(), ClipPreparationError> {
        if size <= self.reserved {
            return Ok(());
        }
        let _gate = lock_spool_directory(&self.root).map_err(spool_error)?;
        let own = self.record.as_ref().map(|record| record.path());
        let (used, shared_cap) = scan_pool(&self.root, own, cap).map_err(spool_error)?;
        let remaining = shared_cap.saturating_sub(used);
        if size > remaining {
            return Err(ClipPreparationError {
                message: format!(
                    "Temporary clip audio would exceed the disk budget of {shared_cap} bytes shared by OpenCompose processes"
                ),
                scratch_busy: size <= cap,
            });
        }
        // Incremental decodes write many small packets. Keep their filesystem
        // accounting amortized while charging unused capacity to the quota.
        let reserved = if incremental && shared_cap >= 128 * RESERVATION_STEP {
            size.div_ceil(RESERVATION_STEP)
                .saturating_mul(RESERVATION_STEP)
                .min(remaining)
        } else {
            size
        };
        let amount = self
            .amount
            .as_mut()
            .expect("an active lease has an amount file");
        amount.seek(SeekFrom::Start(8)).map_err(spool_error)?;
        amount
            .write_all(&reserved.to_le_bytes())
            .map_err(spool_error)?;
        self.reserved = reserved;
        Ok(())
    }
}

impl Drop for SharedScratchLease {
    fn drop(&mut self) {
        // Close the lock handle before the temporary directory is removed.
        let _gate = lock_spool_directory(&self.root).ok();
        drop(self.amount.take());
        drop(self.owner.take());
        drop(self.record.take());
    }
}

pub(super) fn shared_used(dir: &Path, pool: &str) -> std::io::Result<u64> {
    let root = pool_path(dir, pool);
    if !root.is_dir() {
        return Ok(0);
    }
    let _gate = lock_spool_directory(&root)?;
    Ok(scan_pool(&root, None, u64::MAX)?.0)
}

/// The pool lock serializes reads and updates of amounts. Only the separate
/// owner file stays locked for the lifetime of a reservation.
fn scan_pool(root: &Path, own: Option<&Path>, mut cap: u64) -> std::io::Result<(u64, u64)> {
    let mut used = 0_u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(PREFIX))
        {
            continue;
        }
        let path = entry.path();
        if own == Some(path.as_path()) {
            continue;
        }
        match open_cache_lock(&path.join("owner"), false) {
            Ok(owner) => match owner.try_lock() {
                Ok(()) => {
                    drop(owner);
                    remove_stale_record(&path);
                    continue;
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => return Err(error),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                remove_stale_record(&path);
                continue;
            }
            Err(error) => return Err(error),
        }
        let mut amount = File::open(path.join("amount"))?;
        let mut bytes = [0; 8];
        amount.read_exact(&mut bytes)?;
        cap = cap.min(u64::from_le_bytes(bytes));
        amount.read_exact(&mut bytes)?;
        used = used.saturating_add(u64::from_le_bytes(bytes));
    }
    Ok((used, cap))
}

fn remove_stale_record(path: &Path) {
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(STAGED_PCM_PREFIX))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let _ = fs::remove_file(path.join("amount"));
    let _ = fs::remove_file(path.join("owner"));
    let _ = fs::remove_dir(path);
}
