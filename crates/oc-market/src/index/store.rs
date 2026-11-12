//! Scoped, authenticated catalog snapshots and serialized rollback protection.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use super::{
    IndexError, MANIFEST_FILE, MAX_CATALOG_DOWNLOAD_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES,
    Policy, SIGNATURE_FILE, Stamp, VerifiedIndex, authenticate, verify_parts,
};

const STATE_FILE: &str = "state.json";
const MAX_STATE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Persisted freshness and signature metadata for one cached catalog.
pub struct CacheState {
    #[serde(default)]
    pub trust_scope: String,
    pub fetched_at: u64,
    pub checked_at: u64,
    pub etag: Option<String>,
    pub generated_at: String,
    pub catalog_version: String,
    pub manifest_sha256: String,
    pub source_base: String,
}

#[derive(Debug, Clone)]
/// A verified index and the times it was fetched and checked.
pub struct StoredIndex {
    pub index: VerifiedIndex,
    pub fetched_at: u64,
    pub checked_at: u64,
}

#[derive(Clone)]
/// The per-trust-scope cache that stores verified catalogs and watermarks.
pub struct IndexStore {
    root: PathBuf,
    policy: Policy,
}
/// Holds catalog freshness stable during local publication. Never retain this guard over network I/O.
#[derive(Debug)]
pub struct PublicationGuard {
    _lock: Option<File>,
}

impl IndexStore {
    /// Open the store under `root` for the policy trust scope.
    pub fn new(root: PathBuf, policy: Policy) -> Self {
        Self { root, policy }
    }
    /// Verify `index` again and lock its scope for publication.
    pub fn authorize(&self, index: &VerifiedIndex) -> Result<PublicationGuard, IndexError> {
        let verified = verify_parts(
            &self.policy,
            index.manifest_bytes.to_vec(),
            index.signature.to_vec(),
            index.compressed.to_vec(),
            index.source_base.clone(),
            index.etag.clone(),
        )?;
        let lock = self.read_lock()?;
        if let Some(highest) = self.watermark_unlocked()? {
            highest.check(&verified.stamp()?)?;
        }
        Ok(PublicationGuard { _lock: lock })
    }
    /// Directory holding the current head and watermark for this scope.
    pub fn scope_dir(&self) -> PathBuf {
        self.root.join("scopes").join(self.policy.scope())
    }
    fn head(&self) -> PathBuf {
        self.scope_dir().join("head")
    }
    fn watermark_head(&self) -> PathBuf {
        self.scope_dir().join("highest")
    }
    fn read_lock(&self) -> Result<Option<File>, IndexError> {
        let file = match OpenOptions::new()
            .read(true)
            .open(self.scope_dir().join("write.lock"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage(error)),
        };
        <File as FileExt>::lock_shared(&file).map_err(storage)?;
        Ok(Some(file))
    }
    /// The cached catalog directory, when a head is published.
    pub fn cache_dir(&self) -> Result<Option<PathBuf>, IndexError> {
        let _lock = self.read_lock()?;
        self.cache_dir_unlocked()
    }
    fn cache_dir_unlocked(&self) -> Result<Option<PathBuf>, IndexError> {
        Ok(
            read_pointer(&self.head())?
                .map(|digest| self.scope_dir().join("catalogs").join(digest)),
        )
    }
    fn signed_stamp(&self, pointer: &Path, category: &str) -> Result<Option<Stamp>, IndexError> {
        let Some(digest) = read_pointer(pointer)? else {
            return Ok(None);
        };
        let directory = self.scope_dir().join(category).join(&digest);
        let manifest = read_bounded_file(&directory.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        let signature = read_bounded_file(&directory.join(SIGNATURE_FILE), MAX_SIGNATURE_BYTES)?;
        if crate::sha256_hex(&manifest) != digest {
            return Err(IndexError::storage(
                "Signed index pointer does not match its manifest",
            ));
        }
        Stamp::authenticated(&manifest, &signature, &self.policy.public_key).map(Some)
    }
    /// The highest signed catalog this store has accepted.
    pub fn watermark(&self) -> Result<Option<Stamp>, IndexError> {
        let _lock = self.read_lock()?;
        self.watermark_unlocked()
    }
    fn watermark_unlocked(&self) -> Result<Option<Stamp>, IndexError> {
        let highest = self.signed_stamp(&self.watermark_head(), "watermarks")?;
        let published = match self.signed_stamp(&self.head(), "catalogs") {
            Ok(published) => published,
            Err(_) if highest.is_some() => None,
            Err(error) => return Err(error),
        };
        match (highest, published) {
            (Some(highest), Some(published)) => {
                if published.generated_at > highest.generated_at {
                    return Ok(Some(published));
                }
                if published.generated_at == highest.generated_at {
                    highest.check(&published)?;
                }
                Ok(Some(highest))
            }
            (None, None) => self
                .read_legacy()
                .and_then(|legacy| legacy.map(|stored| stored.index.stamp()).transpose()),
            (highest, published) => Ok(highest.or(published)),
        }
    }
    fn read_snapshot(&self, directory: &Path) -> Result<StoredIndex, IndexError> {
        let state: CacheState = serde_json::from_slice(&read_bounded_file(
            &directory.join(STATE_FILE),
            MAX_STATE_BYTES,
        )?)
        .map_err(|error| IndexError::storage(format!("state JSON: {error}")))?;
        if state.trust_scope != self.policy.scope() {
            return Err(IndexError::new(
                "trust",
                "Cached plugin index belongs to a different authority or origin; refresh with the configured trust",
            ));
        }
        let manifest_bytes = read_bounded_file(&directory.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
        let signature = read_bounded_file(&directory.join(SIGNATURE_FILE), MAX_SIGNATURE_BYTES)?;
        let manifest = authenticate(&manifest_bytes, &signature, &self.policy.public_key)?;
        self.policy.compatible(&manifest)?;
        if crate::sha256_hex(&manifest_bytes) != state.manifest_sha256 {
            return Err(IndexError::storage(
                "Cache state does not match its signed manifest",
            ));
        }
        let allowed_origin = self.policy.origins.contains(&state.source_base)
            || manifest
                .mirrors
                .iter()
                .any(|mirror| mirror.base_url == state.source_base);
        if !allowed_origin {
            return Err(IndexError::new(
                "trust",
                "Cached plugin index has an unconfigured source origin",
            ));
        }
        let compressed = read_bounded_file(
            &directory.join(&manifest.catalog.path),
            MAX_CATALOG_DOWNLOAD_BYTES,
        )?;
        let index = verify_parts(
            &self.policy,
            manifest_bytes,
            signature,
            compressed,
            state.source_base,
            state.etag,
        )?;
        Ok(StoredIndex {
            index,
            fetched_at: state.fetched_at,
            checked_at: state.checked_at,
        })
    }
    fn read_legacy(&self) -> Result<Option<StoredIndex>, IndexError> {
        let legacy = self.root.join("last-good");
        if !legacy.exists() {
            return Ok(None);
        }
        let state: CacheState = serde_json::from_slice(&read_bounded_file(
            &legacy.join(STATE_FILE),
            MAX_STATE_BYTES,
        )?)
        .map_err(|error| IndexError::storage(format!("legacy state JSON: {error}")))?;
        if state.trust_scope != self.policy.scope() {
            return Ok(None);
        }
        self.read_snapshot(&legacy).map(Some)
    }

    /// Read-only loads are also used by CLI dry-run and never acquire a write lock.
    pub fn load(&self) -> Result<Option<StoredIndex>, IndexError> {
        let _lock = self.read_lock()?;
        self.load_unlocked()
    }
    fn load_unlocked(&self) -> Result<Option<StoredIndex>, IndexError> {
        // A damaged watermark must never silently reset freshness protection.
        let _ = self.watermark_unlocked()?;
        match self.cache_dir_unlocked()? {
            Some(directory) => {
                let stored = self.read_snapshot(&directory)?;
                if directory.file_name().and_then(|name| name.to_str())
                    != Some(stored.index.digest().as_str())
                {
                    return Err(IndexError::storage(
                        "Catalog head does not match its signed manifest",
                    ));
                }
                Ok(Some(stored))
            }
            None => self.read_legacy(),
        }
    }
    /// Every writer rechecks the signed high-water mark under the same OS lock.
    /// The watermark commits before the catalog head, so an interrupted catalog
    /// publication cannot allow an older response from a concurrent client.
    pub fn commit(&self, index: &VerifiedIndex, now: u64) -> Result<(), IndexError> {
        let index = verify_parts(
            &self.policy,
            index.manifest_bytes.to_vec(),
            index.signature.to_vec(),
            index.compressed.to_vec(),
            index.source_base.clone(),
            index.etag.clone(),
        )?;
        let scope = self.scope_dir();
        fs::create_dir_all(&scope).map_err(storage)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(scope.join("write.lock"))
            .map_err(storage)?;
        <File as FileExt>::lock(&lock).map_err(storage)?;
        let old_head = read_pointer(&self.head())?;
        let old_previous = read_pointer(&scope.join("previous"))?;
        let incoming = index.stamp()?;
        if let Some(highest) = self.watermark_unlocked()? {
            highest.check(&incoming)?;
        }
        let digest = index.digest();
        let watermark_dir = scope.join("watermarks").join(&digest);
        publish_directory(
            &watermark_dir,
            &[
                (MANIFEST_FILE, &index.manifest_bytes),
                (SIGNATURE_FILE, &index.signature),
            ],
        )?;
        atomic_write(&self.watermark_head(), digest.as_bytes())?;
        // The signed watermark above owns freshness. A broken old catalog may
        // be repaired by a newly verified snapshot without resetting that floor.
        let previous = self
            .load_unlocked()
            .ok()
            .flatten()
            .filter(|old| old.index.digest() == digest);
        let state = CacheState {
            trust_scope: self.policy.scope(),
            fetched_at: previous.as_ref().map_or(now, |old| old.fetched_at),
            checked_at: now,
            etag: index.etag.clone(),
            generated_at: index.manifest.generated_at.clone(),
            catalog_version: index.manifest.catalog_version.clone(),
            manifest_sha256: digest.clone(),
            source_base: index.source_base.clone(),
        };
        let state_bytes =
            serde_json::to_vec(&state).map_err(|error| IndexError::storage(error.to_string()))?;
        let catalog_dir = scope.join("catalogs").join(&digest);
        publish_directory(
            &catalog_dir,
            &[
                (MANIFEST_FILE, &index.manifest_bytes),
                (SIGNATURE_FILE, &index.signature),
                (&index.manifest.catalog.path, &index.compressed),
                (STATE_FILE, &state_bytes),
            ],
        )?;
        // ETag/check time are mutable metadata; signed snapshot bytes are immutable.
        atomic_write(&catalog_dir.join(STATE_FILE), &state_bytes)?;
        let previous = old_head
            .filter(|old| *old != digest)
            .or(old_previous.filter(|old| *old != digest));
        if let Some(previous) = &previous {
            atomic_write(&scope.join("previous"), previous.as_bytes())?;
        }
        atomic_write(&self.head(), digest.as_bytes())?;
        self.collect_unlocked(&digest, previous.as_deref())?;
        Ok(())
    }
    fn collect_unlocked(&self, current: &str, previous: Option<&str>) -> Result<(), IndexError> {
        for (category, retained) in [
            ("catalogs", vec![Some(current), previous]),
            ("watermarks", vec![Some(current)]),
        ] {
            let directory = self.scope_dir().join(category);
            for entry in fs::read_dir(&directory).map_err(storage)? {
                let entry = entry.map_err(storage)?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let owned = name.starts_with(".snapshot-")
                    || (name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()));
                if owned
                    && !retained
                        .iter()
                        .flatten()
                        .any(|retained| *retained == name.as_ref())
                {
                    remove_cache_entry(&entry.path())?;
                }
            }
        }
        for entry in fs::read_dir(self.scope_dir()).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_string_lossy().starts_with(".index-") {
                remove_cache_entry(&entry.path())?;
            }
        }
        Ok(())
    }
}

fn remove_cache_entry(path: &Path) -> Result<(), IndexError> {
    let metadata = fs::symlink_metadata(path).map_err(storage)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(storage)
    } else {
        fs::remove_file(path).map_err(storage)
    }
}

fn storage(error: std::io::Error) -> IndexError {
    IndexError::storage(error.to_string())
}
/// Read at most `limit` bytes, refusing a larger file.
pub fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, IndexError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(storage)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() > limit {
        return Err(IndexError::storage(
            "Cached index object exceeds its byte limit",
        ));
    }
    Ok(bytes)
}
fn read_pointer(path: &Path) -> Result<Option<String>, IndexError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    let mut bytes = Vec::new();
    file.take(65).read_to_end(&mut bytes).map_err(storage)?;
    let digest =
        String::from_utf8(bytes).map_err(|_| IndexError::storage("Invalid catalog pointer"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IndexError::storage("Invalid catalog pointer"));
    }
    Ok(Some(digest))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), IndexError> {
    let parent = path
        .parent()
        .ok_or_else(|| IndexError::storage("Invalid index state path"))?;
    fs::create_dir_all(parent).map_err(storage)?;
    let mut temporary = crate::temporary::file_in(parent, ".index-").map_err(storage)?;
    temporary.write_all(bytes).map_err(storage)?;
    temporary.as_file().sync_all().map_err(storage)?;
    let temporary = temporary.into_temp_path();
    // Windows cannot replace an existing destination with std::fs::rename.
    #[cfg(windows)]
    replace_windows(&temporary, path)?;
    #[cfg(not(windows))]
    fs::rename(&temporary, path).map_err(storage)?;
    Ok(())
}
#[cfg(windows)]
fn replace_windows(source: &Path, target: &Path) -> Result<(), IndexError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both paths are owned, NUL-terminated UTF-16 buffers alive through
    // the synchronous call; MoveFileExW does not retain either pointer.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(storage(std::io::Error::last_os_error()));
    }
    Ok(())
}
fn publish_directory(path: &Path, files: &[(&str, &[u8])]) -> Result<(), IndexError> {
    if path.exists() {
        for (name, bytes) in files {
            if *name != STATE_FILE && read_bounded_file(&path.join(name), bytes.len())? != *bytes {
                return Err(IndexError::storage(
                    "Immutable signed index snapshot was modified",
                ));
            }
        }
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| IndexError::storage("Invalid index snapshot path"))?;
    fs::create_dir_all(parent).map_err(storage)?;
    let staging = crate::temporary::directory_in(parent, ".snapshot-").map_err(storage)?;
    for (name, bytes) in files {
        atomic_write(&staging.path().join(name), bytes)?;
    }
    fs::rename(staging.path(), path).map_err(storage)
}
