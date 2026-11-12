//! Interleaved PCM storage read on render workers, never on the device callback.

mod shared_scratch;

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError as FileTryLockError};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError, Weak};

#[derive(Clone)]
pub(crate) enum ClipPcm {
    Resident(Arc<Vec<f32>>),
    Spooled(Arc<Spool>),
}

pub(crate) struct Spool {
    file: Mutex<File>,
    frames: usize,
    byte_offset: u64,
    scratch: Option<ScratchLease>,
    _named_pin: Option<File>,
}

/// Reuse file-backed PCM within a process. A shared sidecar lock keeps its
/// name available to other processes for as long as the reader lives.
#[derive(Default)]
pub(crate) struct NamedSpools(HashMap<PathBuf, Weak<Spool>>);

pub(crate) fn named_spools() -> MutexGuard<'static, NamedSpools> {
    static FILES: OnceLock<Mutex<NamedSpools>> = OnceLock::new();
    FILES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn backing_path(path: &Path) -> PathBuf {
    path.parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(path.file_name())
        .map_or_else(|| path.to_path_buf(), |(parent, name)| parent.join(name))
}

fn cache_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn open_cache_lock(path: &Path, create: bool) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)
}

/// Serialize acquiring and unlinking sidecars. This lock file stays in the
/// directory so all processes always acquire the same lock.
fn spool_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn lock_spool_directory(dir: &Path) -> std::io::Result<File> {
    let file = open_cache_lock(&dir.join(".clip-spools.lock"), true)?;
    file.lock()?;
    Ok(file)
}

// Call with the directory lock held. Lock a sidecar instead of the PCM so
// Windows readers do not block writes to the cache entry's access stamp.
pub(crate) fn pin_named_spool(path: &Path) -> std::io::Result<File> {
    let pin = open_cache_lock(&cache_sidecar_path(path, ".pin"), true)?;
    pin.lock_shared()?;
    Ok(pin)
}

pub(crate) fn open_named_spool(path: &Path) -> std::io::Result<(File, File)> {
    let _directory = lock_spool_directory(spool_parent(path))?;
    let file = File::open(path)?;
    Ok((file, pin_named_spool(path)?))
}

impl NamedSpools {
    pub(crate) fn get(&self, path: &Path) -> Option<ClipPcm> {
        self.0
            .get(&backing_path(path))?
            .upgrade()
            .map(ClipPcm::Spooled)
    }

    pub(crate) fn register(
        &mut self,
        path: &Path,
        file: File,
        pin: File,
        frames: usize,
        byte_offset: u64,
    ) -> ClipPcm {
        if let Some(pcm) = self.get(path) {
            return pcm;
        }
        self.0.retain(|_, spool| spool.strong_count() != 0);
        let pcm = ClipPcm::file_backing(file, frames, byte_offset, None, Some(pin));
        if let ClipPcm::Spooled(spool) = &pcm {
            self.0.insert(backing_path(path), Arc::downgrade(spool));
        }
        pcm
    }

    pub(crate) fn remove_unused(&mut self, path: &Path) -> std::io::Result<bool> {
        let _directory = lock_spool_directory(spool_parent(path))?;
        self.remove_unused_locked(path)
    }

    fn remove_unused_locked(&mut self, path: &Path) -> std::io::Result<bool> {
        let key = backing_path(path);
        if self
            .0
            .get(&key)
            .is_some_and(|spool| spool.strong_count() != 0)
        {
            return Ok(false);
        }
        self.0.remove(&key);
        let pin_path = cache_sidecar_path(path, ".pin");
        let pin = match open_cache_lock(&pin_path, false) {
            Ok(pin) => match pin.try_lock() {
                Ok(()) => Some(pin),
                Err(FileTryLockError::WouldBlock) => return Ok(false),
                Err(FileTryLockError::Error(error)) => return Err(error),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        std::fs::remove_file(path)?;
        if let Some(pin) = pin {
            drop(pin);
            let _ = std::fs::remove_file(pin_path);
        }
        Ok(true)
    }

    fn is_open(&self, path: &Path) -> bool {
        self.get(path).is_some()
            || open_cache_lock(&cache_sidecar_path(path, ".pin"), false)
                .is_ok_and(|pin| matches!(pin.try_lock(), Err(FileTryLockError::WouldBlock)))
    }
}

/// Hold one preparation key across cooperating processes. Work files are
/// removed under the directory lock so waiters cannot lock a replaced inode.
pub(crate) struct DiskCacheWork {
    path: PathBuf,
    file: Option<File>,
}

impl DiskCacheWork {
    pub(crate) fn lookup_or_lock<T>(
        target: Option<PathBuf>,
        cancellation: ClipCancellation<'_>,
        mut lookup: impl FnMut() -> Option<T>,
    ) -> Result<(Option<T>, Option<Self>), String> {
        if let Some(hit) = lookup() {
            return Ok((Some(hit), None));
        }
        let permit = Self::acquire(target, cancellation)?;
        // Another process may have published between the initial miss and
        // acquiring the permit.
        let hit = permit.as_ref().and_then(|_| lookup());
        Ok((hit, permit))
    }

    fn acquire(
        target: Option<PathBuf>,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Option<Self>, String> {
        let Some(target) = target else {
            return Ok(None);
        };
        let path = cache_sidecar_path(&target, ".work");
        let dir = spool_parent(&path);
        if std::fs::create_dir_all(dir).is_err() {
            return Ok(None);
        }
        loop {
            cancellation.check()?;
            let Ok(directory) = lock_spool_directory(dir) else {
                return Ok(None);
            };
            let Ok(file) = open_cache_lock(&path, true) else {
                return Ok(None);
            };
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Some(Self {
                        path,
                        file: Some(file),
                    }));
                }
                Err(FileTryLockError::Error(_)) => return Ok(None),
                Err(FileTryLockError::WouldBlock) => {
                    drop(file);
                    drop(directory);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
}

impl Drop for DiskCacheWork {
    fn drop(&mut self) {
        if let Ok(_directory) = lock_spool_directory(spool_parent(&self.path)) {
            // Windows needs the handle closed before removing the lock file.
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Atomically replaced by Clear so a reader sees a complete generation even
/// when it snapshots before the directory is created.
pub(crate) fn disk_cache_generation(dir: &Path, suffix: &str) -> std::io::Result<u64> {
    let path = dir.join(format!(".{suffix}.generation"));
    match std::fs::read(path) {
        Ok(bytes) => bytes.try_into().map(u64::from_le_bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid clip cache generation",
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Hold the directory lock until the staged PCM is published.
pub(crate) fn accept_disk_cache_generation(
    dir: &Path,
    suffix: &str,
    generation: u64,
) -> std::io::Result<Option<File>> {
    let directory = lock_spool_directory(dir)?;
    Ok((disk_cache_generation(dir, suffix)? == generation).then_some(directory))
}

pub(crate) fn clear_disk_cache(dir: &Path, suffix: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut named = named_spools();
    let _directory = lock_spool_directory(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if cache_file_name(&path, suffix) {
            let _ = named.remove_unused_locked(&path);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "work")
            && cache_file_name(&path.with_extension(""), suffix)
            && let Ok(file) = open_cache_lock(&path, false)
            && file.try_lock().is_ok()
        {
            drop(file);
            let _ = std::fs::remove_file(&path);
        }
    }
    // Reclaim space before staging the marker. Old publishers still wait
    // behind the directory lock until the new generation is visible.
    let generation = disk_cache_generation(dir, suffix)?.wrapping_add(1);
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(&generation.to_le_bytes())?;
    staged.persist(dir.join(format!(".{suffix}.generation")))?;
    Ok(())
}

pub(crate) fn cache_file_name(path: &Path, suffix: &str) -> bool {
    path.extension()
        .is_some_and(|extension| extension == suffix)
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.len() == 32 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

/// A busy key waits for its existing worker. Unrelated media can prepare
/// concurrently and idle keys do not keep a mutex alive.
pub(crate) struct CacheWorkGates<K>(Mutex<HashMap<K, Weak<Mutex<()>>>>);

impl<K> Default for CacheWorkGates<K> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<K: Clone + Eq + std::hash::Hash> CacheWorkGates<K> {
    pub(crate) fn for_key(&self, key: &K) -> Arc<Mutex<()>> {
        let mut gates = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
            return gate;
        }
        gates.retain(|_, gate| gate.strong_count() != 0);
        let gate = Arc::new(Mutex::new(()));
        gates.insert(key.clone(), Arc::downgrade(&gate));
        gate
    }
}

/// Serializes the commit step with a clear, while expensive work runs outside
/// the lock. Work started in an earlier generation can still serve its caller.
#[derive(Default)]
pub(crate) struct CacheGeneration(Mutex<u64>);

impl CacheGeneration {
    fn lock(&self) -> MutexGuard<'_, u64> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn current(&self) -> u64 {
        *self.lock()
    }

    pub(crate) fn accept(&self, generation: u64) -> Option<MutexGuard<'_, u64>> {
        let current = self.lock();
        (*current == generation).then_some(current)
    }

    pub(crate) fn advance(&self) -> MutexGuard<'_, u64> {
        let mut current = self.lock();
        *current = current.wrapping_add(1);
        current
    }
}

/// Cancellation checked only on the preparation worker.
#[derive(Clone, Copy, Default)]
pub(crate) struct ClipCancellation<'a> {
    graph_dropped: Option<&'a AtomicBool>,
    preempted: Option<&'a dyn Fn() -> bool>,
}

impl<'a> ClipCancellation<'a> {
    pub(crate) fn new(graph_dropped: &'a AtomicBool, preempted: &'a dyn Fn() -> bool) -> Self {
        Self {
            graph_dropped: Some(graph_dropped),
            preempted: Some(preempted),
        }
    }
    pub(crate) fn check(self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("Audio clip preparation cancelled".into());
        }
        Ok(())
    }

    pub(crate) fn is_cancelled(self) -> bool {
        self.graph_dropped
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
            || self.preempted.is_some_and(|preempted| preempted())
    }

    pub(crate) fn lock<T>(self, mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
        if self.graph_dropped.is_none() {
            return Ok(mutex.lock().unwrap_or_else(|error| error.into_inner()));
        }
        loop {
            self.check()?;
            match mutex.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClipPreparationError {
    message: String,
    pub(crate) scratch_busy: bool,
}

impl ClipPreparationError {
    pub(crate) fn context(mut self, message: String) -> Self {
        self.message = format!("{message}: {}", self.message);
        self
    }
}

impl std::fmt::Display for ClipPreparationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl From<String> for ClipPreparationError {
    fn from(message: String) -> Self {
        Self {
            message,
            scratch_busy: false,
        }
    }
}

impl From<&str> for ClipPreparationError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl From<ClipPreparationError> for String {
    fn from(error: ClipPreparationError) -> Self {
        error.message
    }
}

// Leave room for project saves and unrelated processes as the clip cache grows.
const CLIP_VOLUME_HEADROOM_MIB: u64 = 256;
const CLIP_VOLUME_HEADROOM: u64 = CLIP_VOLUME_HEADROOM_MIB * 1024 * 1024;
// A successful probe can cover this many planned writes before we ask again.
const PCM_VOLUME_PROBE_INTERVAL: u64 = 1024 * 1024;

fn check_volume_growth(
    dir: &Path,
    bytes: u64,
    probe: impl FnOnce(&Path) -> std::io::Result<u64>,
) -> Result<u64, ClipPreparationError> {
    let available = probe(dir).map_err(|error| {
        format!(
            "Cannot check free space for clip audio in {}: {error}",
            dir.display()
        )
    })?;
    let writable = available.saturating_sub(CLIP_VOLUME_HEADROOM);
    if bytes > writable {
        return Err(format!(
            "Not enough free disk space for clip audio in {} ({bytes} bytes needed, {writable} bytes available after keeping {CLIP_VOLUME_HEADROOM_MIB} MiB free)",
            dir.display()
        ).into());
    }
    Ok(writable)
}

/// Temporary storage has its own cap because publishing decoded audio can
/// briefly require both the source spool and a separate cache file.
pub(crate) struct ScratchDisk {
    cap: u64,
    pool: &'static str,
    used: Arc<AtomicU64>,
    #[cfg(test)]
    volume_space: fn(&Path) -> std::io::Result<u64>,
}

impl ScratchDisk {
    #[cfg(test)]
    pub(crate) fn new(cap: u64) -> Self {
        Self::for_pool(cap, "test")
    }

    pub(crate) fn for_pool(cap: u64, pool: &'static str) -> Self {
        Self {
            cap,
            pool,
            used: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            volume_space: |path| fs4::available_space(path),
        }
    }

    pub(crate) fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    pub(crate) fn used_on_volume(&self, dir: Option<&Path>) -> u64 {
        let shared = dir
            .and_then(|dir| shared_scratch::shared_used(dir, self.pool).ok())
            .unwrap_or(0);
        self.used().max(shared)
    }

    pub(crate) fn reserve_on_volume(
        &self,
        dir: &Path,
        bytes: u64,
    ) -> Result<ScratchLease, ClipPreparationError> {
        if bytes > self.cap {
            return Err(scratch_limit_error(self.cap, false));
        }
        std::fs::create_dir_all(dir).map_err(spool_error)?;
        let writable = self.check_growth(dir, bytes)?;
        let shared = shared_scratch::SharedScratchLease::new(dir, self.pool, self.cap)
            .map_err(spool_error)?;
        let mut lease = self.reserve_inner(bytes, Some(shared))?;
        lease.volume_credit = writable
            .saturating_sub(bytes)
            .min(PCM_VOLUME_PROBE_INTERVAL);
        Ok(lease)
    }

    pub(crate) fn check_growth(&self, dir: &Path, bytes: u64) -> Result<u64, ClipPreparationError> {
        #[cfg(test)]
        let probe = self.volume_space;
        #[cfg(not(test))]
        let probe = |path: &Path| fs4::available_space(path);
        check_volume_growth(dir, bytes, probe)
    }

    #[cfg(test)]
    pub(crate) fn reserve(&self, bytes: u64) -> Result<ScratchLease, ClipPreparationError> {
        self.reserve_inner(bytes, None)
    }

    fn reserve_inner(
        &self,
        bytes: u64,
        shared: Option<shared_scratch::SharedScratchLease>,
    ) -> Result<ScratchLease, ClipPreparationError> {
        let mut lease = ScratchLease {
            used: self.used.clone(),
            cap: self.cap,
            bytes: 0,
            volume_credit: 0,
            shared,
        };
        lease.grow(bytes)?;
        Ok(lease)
    }
}

fn scratch_limit_error(cap: u64, scratch_busy: bool) -> ClipPreparationError {
    ClipPreparationError {
        message: format!("Temporary clip audio would exceed the shared disk budget of {cap} bytes"),
        scratch_busy,
    }
}

pub(crate) struct ScratchLease {
    used: Arc<AtomicU64>,
    cap: u64,
    bytes: u64,
    volume_credit: u64,
    shared: Option<shared_scratch::SharedScratchLease>,
}

impl ScratchLease {
    pub(crate) fn stage_pcm(&self) -> std::io::Result<tempfile::NamedTempFile> {
        self.shared
            .as_ref()
            .expect("staged PCM must have a filesystem reservation")
            .stage_pcm()
    }

    fn grow_spool(
        &mut self,
        scratch: &ScratchDisk,
        dir: &Path,
        size: u64,
        buffered: u64,
    ) -> Result<(), ClipPreparationError> {
        let extra = size.saturating_sub(self.bytes);
        if extra > self.volume_credit {
            // Buffered bytes are already charged to the quota, but may not
            // have reached the filesystem's available-space counters yet.
            let writable = scratch.check_growth(dir, extra.saturating_add(buffered))?;
            self.volume_credit = (writable - buffered).min(extra.max(PCM_VOLUME_PROBE_INTERVAL));
        }
        self.grow(size)?;
        self.volume_credit -= extra;
        Ok(())
    }

    fn grow(&mut self, size: u64) -> Result<(), ClipPreparationError> {
        let extra = size.saturating_sub(self.bytes);
        if extra == 0 {
            return Ok(());
        }
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(extra).filter(|&total| total <= self.cap)
            })
            .map_err(|_| scratch_limit_error(self.cap, size <= self.cap))?;
        if let Some(shared) = self.shared.as_mut()
            && let Err(error) = shared.grow(size, self.cap, self.bytes > 0)
        {
            self.used.fetch_sub(extra, Ordering::Relaxed);
            return Err(error);
        }
        self.bytes += extra;
        Ok(())
    }
}

impl Drop for ScratchLease {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Disk usage for one cache. Open named files are a subset of cached bytes.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ClipCacheUsage {
    pub dir: Option<String>,
    pub cached_bytes: u64,
    pub open_cached_bytes: u64,
    pub live_temporary_bytes: u64,
    pub temporary_bytes: u64,
    pub budget_bytes: u64,
}

pub(crate) fn cache_usage(
    dir: Option<&Path>,
    suffix: &str,
    pins: &PlaybackPins,
    budget_bytes: u64,
    temporary_bytes: u64,
) -> ClipCacheUsage {
    let live_temporary_bytes = pins.temporary_bytes();
    let mut usage = ClipCacheUsage {
        dir: dir.map(|dir| dir.display().to_string()),
        cached_bytes: 0,
        open_cached_bytes: 0,
        live_temporary_bytes,
        temporary_bytes: temporary_bytes.max(live_temporary_bytes),
        budget_bytes,
    };
    if let Some(dir) = dir
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if !cache_file_name(&path, suffix) || !entry.file_type().is_ok_and(|ty| ty.is_file()) {
                continue;
            }
            let named = named_spools();
            if let Ok(metadata) = entry.metadata() {
                usage.cached_bytes = usage.cached_bytes.saturating_add(metadata.len());
                if named.is_open(&path) {
                    usage.open_cached_bytes =
                        usage.open_cached_bytes.saturating_add(metadata.len());
                }
            }
        }
    }
    usage
}

/// Live graphs can keep a buffer after the LRU evicts it. Charge each
/// allocation once across graphs, for as long as a placement owns its pin.
#[derive(Default)]
pub(crate) struct PlaybackPins(Mutex<HashMap<usize, Weak<PlaybackPin>>>);

pub(crate) struct PlaybackPin {
    pub(crate) pcm: ClipPcm,
    // Keep the allocation identity stable even when the pin holds a disk copy.
    _source: Option<Weak<Vec<f32>>>,
    bytes: usize,
}

impl PlaybackPins {
    fn temporary_bytes(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .filter_map(Weak::upgrade)
            .filter_map(|pin| match &pin.pcm {
                ClipPcm::Spooled(spool) if spool.scratch.is_some() => {
                    Some((pin.bytes as u64).saturating_add(spool.byte_offset))
                }
                _ => None,
            })
            .fold(0, u64::saturating_add)
    }

    pub(crate) fn pin(
        &self,
        pcm: &ClipPcm,
        memory_cap: usize,
        dir: Option<&Path>,
        scratch: &ScratchDisk,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Option<Arc<PlaybackPin>>, ClipPreparationError> {
        let disk_cap = scratch.cap;
        let key = match pcm {
            ClipPcm::Resident(samples) => Arc::as_ptr(samples) as usize,
            ClipPcm::Spooled(spool) => Arc::as_ptr(spool) as usize,
        };
        let mut pins = cancellation.lock(&self.0)?;
        if let Some(pin) = pins.get(&key).and_then(Weak::upgrade) {
            return Ok(Some(pin));
        }
        let mut resident = 0_usize;
        let mut disk = 0_u64;
        pins.retain(|_, weak| {
            let Some(pin) = weak.upgrade() else {
                return false;
            };
            match &pin.pcm {
                ClipPcm::Resident(_) => resident = resident.saturating_add(pin.bytes),
                ClipPcm::Spooled(_) => disk = disk.saturating_add(pin.bytes as u64),
            }
            true
        });

        let bytes = pcm.frames().saturating_mul(8);
        let (pcm, source) = match pcm {
            ClipPcm::Spooled(_) => {
                if disk.saturating_add(bytes as u64) > disk_cap {
                    return Err(format!(
                        "Live clip PCM exceeds the shared disk budget of {disk_cap} bytes"
                    )
                    .into());
                }
                (pcm.clone(), None)
            }
            ClipPcm::Resident(samples) => {
                let stored = if resident.saturating_add(bytes) <= memory_cap {
                    pcm.clone()
                } else {
                    let dir = dir.filter(|_| disk.saturating_add(bytes as u64) <= disk_cap)
                        .ok_or_else(|| format!(
                            "Live clip PCM exceeds the shared memory budget of {memory_cap} bytes and has insufficient spool capacity"
                        ))?;
                    let mut spool = PcmBuilder::new(Some((dir, 0, scratch)));
                    for block in samples.chunks(16_384) {
                        cancellation.check()?;
                        spool.append(block)?;
                    }
                    spool.finish()?
                };
                (stored, Some(Arc::downgrade(samples)))
            }
        };
        let pin = Arc::new(PlaybackPin {
            pcm,
            _source: source,
            bytes,
        });
        pins.insert(key, Arc::downgrade(&pin));
        Ok(Some(pin))
    }
}

/// Holds only a short prefix in memory, then appends decoded packets to disk.
pub(crate) struct PcmBuilder<'a> {
    memory: Vec<f32>,
    disk: Option<(BufWriter<File>, ScratchLease)>,
    spill: Option<(&'a Path, usize, &'a ScratchDisk)>,
    frames: usize,
}

impl<'a> PcmBuilder<'a> {
    pub(crate) fn new(spill: Option<(&'a Path, usize, &'a ScratchDisk)>) -> Self {
        Self {
            memory: Vec::new(),
            disk: None,
            spill,
            frames: 0,
        }
    }

    pub(crate) fn append(&mut self, samples: &[f32]) -> Result<(), ClipPreparationError> {
        let size = (self.frames + samples.len() / 2) as u64 * 8;
        if let Some((dir, memory_limit, scratch)) = self.spill
            && size > memory_limit as u64
        {
            if let Some((writer, lease)) = self.disk.as_mut() {
                let buffered = writer.buffer().len() as u64;
                lease.grow_spool(scratch, dir, size, buffered)?;
            } else {
                let lease = scratch.reserve_on_volume(dir, size)?;
                let mut writer = BufWriter::new(tempfile::tempfile_in(dir).map_err(spool_error)?);
                writer
                    .write_all(bytemuck::cast_slice(&self.memory))
                    .map_err(spool_error)?;
                self.memory = Vec::new();
                self.disk = Some((writer, lease));
            }
        }
        if let Some((writer, _)) = self.disk.as_mut() {
            writer
                .write_all(bytemuck::cast_slice(samples))
                .map_err(spool_error)?;
        } else {
            self.memory.extend_from_slice(samples);
        }
        self.frames += samples.len() / 2;
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<ClipPcm, String> {
        match self.disk {
            Some((writer, lease)) => {
                let file = writer
                    .into_inner()
                    .map_err(|failure| spool_error(failure.into_error()))?;
                Ok(ClipPcm::temporary_file(file, self.frames, 0, lease))
            }
            None => Ok(ClipPcm::Resident(Arc::new(self.memory))),
        }
    }
}

fn spool_error(error: std::io::Error) -> String {
    format!("Cannot spool clip audio: {error}")
}

impl ClipPcm {
    pub(crate) fn temporary_file(
        file: File,
        frames: usize,
        byte_offset: u64,
        lease: ScratchLease,
    ) -> Self {
        Self::file_backing(file, frames, byte_offset, Some(lease), None)
    }

    fn file_backing(
        file: File,
        frames: usize,
        byte_offset: u64,
        scratch: Option<ScratchLease>,
        named_pin: Option<File>,
    ) -> Self {
        Self::Spooled(Arc::new(Spool {
            file: Mutex::new(file),
            frames,
            byte_offset,
            scratch,
            _named_pin: named_pin,
        }))
    }

    pub(crate) fn resident_bytes(&self) -> usize {
        match self {
            Self::Resident(samples) => samples.len() * size_of::<f32>(),
            Self::Spooled(_) => 0,
        }
    }

    pub(crate) fn window(&self, frames: std::ops::Range<usize>) -> Result<Vec<f32>, String> {
        let mut output = Vec::with_capacity(frames.len() * 2);
        self.visit(frames.start, frames.len(), |_, samples| {
            output.extend_from_slice(samples)
        })?;
        Ok(output)
    }

    pub(crate) fn materialize(&self) -> Result<Arc<Vec<f32>>, String> {
        match self {
            Self::Resident(samples) => Ok(Arc::clone(samples)),
            Self::Spooled(_) => Ok(Arc::new(self.window(0..self.frames())?)),
        }
    }

    pub(crate) fn frames(&self) -> usize {
        match self {
            Self::Resident(samples) => samples.len() / 2,
            Self::Spooled(spool) => spool.frames,
        }
    }

    pub(crate) fn visit(
        &self,
        source: usize,
        frames: usize,
        mut visit: impl FnMut(usize, &[f32]),
    ) -> Result<(), String> {
        self.try_visit(source, frames, |offset, samples| {
            visit(offset, samples);
            Ok(())
        })
    }

    pub(crate) fn try_visit(
        &self,
        source: usize,
        frames: usize,
        mut visit: impl FnMut(usize, &[f32]) -> Result<(), String>,
    ) -> Result<(), String> {
        if frames == 0 {
            return Ok(());
        }
        match self {
            Self::Resident(samples) => visit(0, &samples[source * 2..(source + frames) * 2])?,
            Self::Spooled(spool) => {
                let mut file = spool
                    .file
                    .lock()
                    .map_err(|_| "Clip spool reader lock failed")?;
                let error = |error: std::io::Error| format!("Cannot read clip audio: {error}");
                file.seek(SeekFrom::Start(spool.byte_offset + source as u64 * 8))
                    .map_err(error)?;
                let mut scratch = [0.0_f32; 4096];
                for start in (0..frames).step_by(scratch.len() / 2) {
                    let count = (frames - start).min(scratch.len() / 2);
                    let samples = &mut scratch[..count * 2];
                    file.read_exact(bytemuck::cast_slice_mut(samples))
                        .map_err(error)?;
                    visit(start, samples)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn visit_placement(
        &self,
        offset: usize,
        loop_frames: usize,
        range: std::ops::Range<usize>,
        mut visit: impl FnMut(usize, &[f32]),
    ) -> Result<(), String> {
        if loop_frames == 0 {
            return self.visit(offset + range.start, range.len(), |start, samples| {
                visit(range.start + start, samples);
            });
        }
        let mut frame = range.start;
        while frame < range.end {
            let within_loop = frame % loop_frames;
            let count = (loop_frames - within_loop).min(range.end - frame);
            self.visit(offset + within_loop, count, |start, samples| {
                visit(frame + start, samples)
            })?;
            frame += count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARED_CACHE_TEST_DIR: &str = "OCP_TEST_SHARED_CACHE_DIR";

    fn cross_process_cache_paths(root: &Path) -> [PathBuf; 2] {
        [
            root.join(format!("{}.ocm", "a".repeat(32))),
            root.join(format!("{}.ocd", "b".repeat(32))),
        ]
    }

    #[test]
    fn incremental_scratch_capacity_counts_toward_the_shared_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cap = 32 << 20;
        let first = ScratchDisk::new(cap);
        let second = ScratchDisk::new(cap);
        let mut lease = first.reserve_on_volume(dir.path(), 8).unwrap();
        lease.grow(16).unwrap();
        let reserved = first.used_on_volume(Some(dir.path()));
        assert!(reserved > 16);
        let error = second
            .reserve_on_volume(dir.path(), cap - reserved + 1)
            .err()
            .unwrap();
        assert!(error.scratch_busy);
        assert!(second.reserve_on_volume(dir.path(), cap - reserved).is_ok());
    }

    #[test]
    fn shared_scratch_budget_survives_contention_and_reclaims_a_dead_owner() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD_DIR: &str = "OCP_TEST_SHARED_SCRATCH_DIR";
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            let dir = PathBuf::from(dir);
            let scratch = ScratchDisk::for_pool(64, "source");
            let lease = scratch.reserve_on_volume(&dir, 48).unwrap();
            let mut staged = lease.stage_pcm().unwrap();
            staged.write_all(&[0; 48]).unwrap();
            std::fs::write(dir.join("ready"), []).unwrap();
            let _ = std::io::stdin().read(&mut [0]);
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let scratch = ScratchDisk::for_pool(128, "source");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "clip_pcm::tests::shared_scratch_budget_survives_contention_and_reclaims_a_dead_owner"])
            .env(CHILD_DIR, dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let _child_input = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "the scratch owner exited"
            );
            assert!(Instant::now() < deadline, "the scratch owner did not start");
            std::thread::sleep(Duration::from_millis(5));
        }

        let cache = crate::media_cache::MediaCache::new(Some(dir.path().to_path_buf()), 0, 128);
        assert_eq!(cache.disk_usage().temporary_bytes, 48);
        let mut lease = scratch.reserve_on_volume(dir.path(), 16).unwrap();
        assert!(lease.grow(17).unwrap_err().scratch_busy);
        assert_eq!(cache.disk_usage().temporary_bytes, 64);
        assert!(
            ScratchDisk::for_pool(128, "derived")
                .reserve_on_volume(dir.path(), 128)
                .is_ok()
        );

        child.kill().unwrap();
        child.wait().unwrap();
        lease.grow(128).unwrap();
        assert_eq!(cache.disk_usage().temporary_bytes, 128);
        drop(lease);
        assert_eq!(cache.disk_usage().temporary_bytes, 0);
        assert!(
            std::fs::read_dir(dir.path().join(".clip-scratch-source"))
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_type().unwrap().is_dir())
        );
    }

    #[test]
    fn waiting_for_disk_work_can_be_cancelled_without_releasing_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("variant.ocd");
        let owner = DiskCacheWork::acquire(Some(path.clone()), ClipCancellation::default())
            .unwrap()
            .unwrap();
        let dropped = AtomicBool::new(false);
        let checks = std::cell::Cell::new(0);
        let preempt = || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        };
        let blocked = DiskCacheWork::acquire(
            Some(path.clone()),
            ClipCancellation::new(&dropped, &preempt),
        );
        assert!(blocked.is_err());
        let work = cache_sidecar_path(&path, ".work");
        assert!(work.exists());
        drop(owner);
        assert!(!work.exists());
    }

    #[test]
    #[ignore = "subprocess entry point"]
    fn hold_named_spools_in_child_process() {
        let root = PathBuf::from(std::env::var_os(SHARED_CACHE_TEST_DIR).unwrap());
        let spools = cross_process_cache_paths(&root).map(|path| {
            let (file, pin) = open_named_spool(&path).unwrap();
            named_spools().register(&path, file, pin, 1, 0)
        });
        std::fs::write(root.join("ready"), []).unwrap();
        let _ = std::io::stdin().read(&mut [0]);
        for spool in spools {
            assert_eq!(spool.window(0..1).unwrap(), vec![0.5, -0.5]);
        }
    }

    #[test]
    fn clearing_clip_caches_preserves_spools_open_in_another_process() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let root = tempfile::tempdir().unwrap();
        let paths = cross_process_cache_paths(root.path());
        for path in &paths {
            std::fs::write(path, bytemuck::cast_slice(&[0.5_f32, -0.5])).unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "clip_pcm::tests::hold_named_spools_in_child_process",
            ])
            .env(SHARED_CACHE_TEST_DIR, root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let child_input = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "the spool holder exited"
            );
            assert!(Instant::now() < deadline, "the spool holder did not start");
            std::thread::sleep(Duration::from_millis(5));
        }

        let media = crate::media_cache::MediaCache::new(Some(root.path().to_path_buf()), 0, 1024);
        let derived =
            crate::derived_cache::DerivedCache::new(Some(root.path().to_path_buf()), 0, 1024);
        assert_eq!(media.disk_usage().open_cached_bytes, 8);
        assert_eq!(derived.disk_usage().open_cached_bytes, 8);
        media.clear();
        derived.clear();
        assert!(paths.iter().all(|path| path.exists()));

        drop(child_input);
        assert!(child.wait().unwrap().success());
        media.clear();
        derived.clear();
        for path in &paths {
            assert!(!path.exists());
            assert!(!cache_sidecar_path(path, ".pin").exists());
        }
    }

    #[test]
    fn simultaneous_file_backed_bakes_share_one_reservation_and_reader() {
        use crate::derived_cache::{DerivedCache, StretchKey};
        use crate::media_cache::MediaKey;
        use crate::resample::ResampleQuality;
        use crate::stretch::StretchPlan;
        use oc_dsl::ast::StretchMode;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let scratch = ScratchDisk::new(64 << 10);
        let mut builder = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        builder.append(&[0.25; 8192]).unwrap();
        let source = builder.finish().unwrap();
        let ClipPcm::Spooled(spool) = &source else {
            panic!("source fixture must be disk-backed");
        };
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let media = MediaKey {
            path: dir.path().join("source.wav"),
            mtime: (0, 0),
            size: 0,
        };
        let plan = StretchPlan::stretched(2.0, StretchMode::Percussive);
        let quality = ResampleQuality::PRESERVED;
        let key = StretchKey::for_plan(&media, 48_000, &plan, quality);
        let run = || {
            cache
                .spool_stretch(
                    dir.path(),
                    key.clone(),
                    &source,
                    &plan,
                    quality,
                    Default::default(),
                )
                .unwrap()
                .1
        };

        let (one, two) = std::thread::scope(|scope| {
            let blocked = spool.file.lock().unwrap();
            let one = scope.spawn(run);
            let start = Instant::now();
            while cache.disk_usage().temporary_bytes == 0 {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "first bake did not reserve space"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let (started, ready) = std::sync::mpsc::channel();
            let (finished, result) = std::sync::mpsc::channel();
            let run_second = &run;
            let two = scope.spawn(move || {
                started.send(()).unwrap();
                let pcm = run_second();
                finished.send(()).unwrap();
                pcm
            });
            ready.recv().unwrap();
            assert!(
                matches!(
                    result.recv_timeout(Duration::from_millis(50)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ),
                "the second worker should wait for the first"
            );
            drop(blocked);
            (one.join().unwrap(), two.join().unwrap())
        });
        let (ClipPcm::Spooled(one), ClipPcm::Spooled(two)) = (one, two) else {
            panic!("expected file-backed bake results");
        };
        assert!(Arc::ptr_eq(&one, &two));
        assert_eq!(cache.stats().bakes, 1);
        assert_eq!(cache.disk_usage().temporary_bytes, 0);
    }

    #[test]
    fn clearing_during_a_file_backed_bake_keeps_only_the_temporary_output() {
        use crate::derived_cache::{DerivedCache, StretchKey};
        use crate::media_cache::MediaKey;
        use crate::resample::ResampleQuality;
        use crate::stretch::StretchPlan;
        use oc_dsl::ast::StretchMode;
        use std::time::{Duration, Instant};

        let source_dir = tempfile::tempdir().unwrap();
        let output_dir = tempfile::tempdir().unwrap();
        let scratch = ScratchDisk::new(64 << 10);
        let mut builder = PcmBuilder::new(Some((source_dir.path(), 0, &scratch)));
        builder.append(&[0.25; 8192]).unwrap();
        let source = builder.finish().unwrap();
        let ClipPcm::Spooled(spool) = &source else {
            panic!("source fixture must be disk-backed");
        };
        let cache = DerivedCache::new(Some(output_dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let media = MediaKey {
            path: source_dir.path().join("test.wav"),
            mtime: (0, 0),
            size: 0,
        };
        let plan = StretchPlan::stretched(2.0, StretchMode::Percussive);
        let quality = ResampleQuality::PRESERVED;
        let key = StretchKey::for_plan(&media, 48_000, &plan, quality);

        let (_, output) = std::thread::scope(|scope| {
            let blocked = spool.file.lock().unwrap();
            let bake = scope.spawn(|| {
                cache.spool_stretch(
                    output_dir.path(),
                    key,
                    &source,
                    &plan,
                    quality,
                    Default::default(),
                )
            });
            let start = Instant::now();
            while cache.disk_usage().temporary_bytes == 0 {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "bake did not start"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let other_cache =
                DerivedCache::new(Some(output_dir.path().to_path_buf()), 16 << 10, 80 << 10);
            other_cache.clear();
            drop(blocked);
            bake.join().unwrap().unwrap()
        });
        assert_eq!(output.frames(), 8192);
        assert!(
            output
                .window(256..512)
                .unwrap()
                .iter()
                .any(|value| *value != 0.0)
        );
        assert_eq!(cache.disk_usage().cached_bytes, 0);
        assert!(cache.disk_usage().temporary_bytes > 0);
        drop(output);
        assert_eq!(cache.disk_usage().temporary_bytes, 0);
    }

    #[test]
    #[ignore = "requires an isolated volume with less than 256 MiB free"]
    fn native_low_volume_refuses_spooling() {
        let root = std::env::var_os("OCP_TEST_LOW_SCRATCH_VOLUME")
            .expect("set OCP_TEST_LOW_SCRATCH_VOLUME to the isolated volume");
        let dir = tempfile::tempdir_in(root).unwrap();
        let available = fs4::available_space(dir.path()).unwrap();
        assert!(
            available < CLIP_VOLUME_HEADROOM,
            "test volume has {available} bytes free"
        );
        let scratch = ScratchDisk::new(1024);
        let mut builder = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        let error = builder.append(&[0.5; 2]).unwrap_err();
        assert!(
            error.to_string().contains("Not enough free disk space"),
            "{error}"
        );
        assert!(!error.scratch_busy);
        drop(builder);
        assert_eq!(scratch.used(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert_eq!(scratch.used_on_volume(Some(dir.path())), 0);
        println!("Spool refused on a volume with {available} bytes free");
    }

    #[test]
    fn spool_refreshes_free_space_after_spending_its_write_credit() {
        static PROBES: AtomicU64 = AtomicU64::new(0);
        let dir = tempfile::tempdir().unwrap();
        let mut scratch = ScratchDisk::new(2 * PCM_VOLUME_PROBE_INTERVAL);
        scratch.volume_space = |_| {
            let available = if PROBES.fetch_add(1, Ordering::Relaxed) == 0 {
                CLIP_VOLUME_HEADROOM + 2 * PCM_VOLUME_PROBE_INTERVAL
            } else {
                CLIP_VOLUME_HEADROOM
            };
            Ok(available)
        };
        let mut builder = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        builder.append(&[0.5; 2]).unwrap();
        let block = vec![0.25; (PCM_VOLUME_PROBE_INTERVAL / 4) as usize];
        builder.append(&block).unwrap();
        assert_eq!(PROBES.load(Ordering::Relaxed), 1);
        assert!(builder.append(&[0.25; 2]).is_err());
        assert_eq!(PROBES.load(Ordering::Relaxed), 2);
        assert_eq!(scratch.used(), PCM_VOLUME_PROBE_INTERVAL + 8);
    }

    #[test]
    fn low_volume_space_refuses_reservation_and_growth_without_leaking_quota() {
        static AVAILABLE: AtomicU64 = AtomicU64::new(CLIP_VOLUME_HEADROOM);
        let dir = tempfile::tempdir().unwrap();
        let mut scratch = ScratchDisk::new(128);
        scratch.volume_space = |_| Ok(AVAILABLE.load(Ordering::Relaxed));

        let error = scratch.reserve_on_volume(dir.path(), 8).err().unwrap();
        assert!(
            error.to_string().contains("Not enough free disk space"),
            "{error}"
        );
        assert!(!error.scratch_busy);
        assert_eq!(scratch.used(), 0);

        AVAILABLE.store(CLIP_VOLUME_HEADROOM + 32, Ordering::Relaxed);
        let mut builder = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        builder.append(&[0.5; 8]).unwrap();
        assert_eq!(scratch.used(), 32);
        assert!(builder.append(&[0.25; 2]).is_err());
        assert_eq!(scratch.used(), 32);
        let pcm = builder.finish().unwrap();
        assert_eq!(pcm.window(0..4).unwrap(), vec![0.5; 8]);
        drop(pcm);
        assert_eq!(scratch.used(), 0);
    }

    #[test]
    fn simultaneous_pcm_builders_share_scratch_and_release_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = ScratchDisk::new(48);
        let mut first = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        first.append(&[0.5; 8]).unwrap();
        let mut second = PcmBuilder::new(Some((dir.path(), 0, &scratch)));
        second.append(&[0.25; 4]).unwrap();
        assert_eq!(scratch.used(), 48);
        assert!(second.append(&[0.0; 2]).is_err());
        drop(second);
        assert_eq!(scratch.used(), 32);

        let pcm = first.finish().unwrap();
        let clone = pcm.clone();
        drop(pcm);
        assert_eq!(scratch.used(), 32);
        drop(clone);
        assert_eq!(scratch.used(), 0);

        let invalid = dir.path().join("file");
        std::fs::write(&invalid, b"not a directory").unwrap();
        let mut failed = PcmBuilder::new(Some((&invalid, 0, &scratch)));
        assert!(failed.append(&[0.0; 2]).is_err());
        assert_eq!(scratch.used(), 0);
    }
}
