//! Media cache: decoded samples, resampled buffers and peak pyramids keyed by
//! absolute path + mtime + size. Runs on the offline/worker path, never in the
//! audio callback; the UI asks for peaks through a background command.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};

use crate::audio_clip;
use crate::audio_peaks::{PeakPyramid, SharedPyramid};
pub use crate::clip_pcm::ClipCacheUsage;
use crate::clip_pcm::{
    CacheGeneration, CacheWorkGates, ClipCancellation, ClipPcm, ClipPreparationError,
    DiskCacheWork, PcmBuilder, PlaybackPin, PlaybackPins, ScratchDisk,
    accept_disk_cache_generation, cache_file_name, clear_disk_cache, disk_cache_generation,
    named_spools, open_named_spool,
};
use crate::resample::ResampleQuality;

/// In-memory cap for decoded + resampled samples and peaks.
pub const DEFAULT_MEMORY_BYTES: usize = 512 * 1024 * 1024;
/// On-disk cap for cached media files.
pub const DEFAULT_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const CACHE_MAGIC: &[u8; 4] = b"OCMC";
const CACHE_VERSION: u8 = 1;
/// magic, version, flags, access stamp, rate, frames, peak bytes.
const CACHE_HEADER_BYTES: usize = 30;
const ACCESS_OFFSET: u64 = 6;
const MAX_RESIDENT_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const RESAMPLE_BLOCK_FRAMES: usize = 16_384;

/// Identity of one media file version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaKey {
    /// Canonicalised absolute path.
    pub path: PathBuf,
    /// Modification time as (seconds, nanoseconds) since the Unix epoch.
    pub mtime: (u64, u32),
    /// File size in bytes.
    pub size: u64,
}

impl MediaKey {
    /// Stable string used to derive the on-disk file name.
    pub fn stamp(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.path.display(),
            self.mtime.0,
            self.mtime.1,
            self.size
        )
    }

    /// Cache file name for the media key.
    pub fn file_name(&self) -> String {
        format!("{:032x}.ocm", Md5::digest(self.stamp().as_bytes()))
    }
}

/// Resolve a media file to its cache key; fails when the file is missing.
pub fn media_key(path: &Path) -> Result<MediaKey, String> {
    let absolute = fs::canonicalize(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("file not found: {}", path.display())
        } else {
            format!("cannot stat {}: {error}", path.display())
        }
    })?;
    let metadata = fs::metadata(&absolute)
        .map_err(|error| format!("cannot stat {}: {error}", absolute.display()))?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("cannot read mtime of {}: {error}", absolute.display()))?;
    let since_epoch = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    Ok(MediaKey {
        path: absolute,
        mtime: (since_epoch.as_secs(), since_epoch.subsec_nanos()),
        size: metadata.len(),
    })
}

/// One cached file: decoded samples plus lazily built peaks and resamplers.
pub struct MediaEntry {
    pub key: MediaKey,
    sample_rate: u32,
    pcm: ClipPcm,
    peaks: SharedPyramid,
    resampled: Mutex<HashMap<(u32, ResampleQuality), ClipPcm>>,
}

impl MediaEntry {
    fn new(key: MediaKey, sample_rate: u32, pcm: ClipPcm, peaks: PeakPyramid) -> Self {
        Self {
            key,
            sample_rate,
            pcm,
            peaks: Arc::new(peaks),
            resampled: Mutex::new(HashMap::new()),
        }
    }

    /// Native sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Native frame count.
    pub fn frames(&self) -> usize {
        self.pcm.frames()
    }

    /// Read a bounded native-rate stereo window without materializing a spool.
    pub fn read_window(&self, frames: std::ops::Range<usize>) -> Result<Vec<f32>, String> {
        if frames.start > frames.end || frames.end > self.frames() {
            return Err("Audio source window is out of range".into());
        }
        self.pcm.window(frames)
    }

    /// Shared peak pyramid.
    pub fn pyramid(&self) -> SharedPyramid {
        self.peaks.clone()
    }

    fn memory_bytes(&self) -> usize {
        let resampled = self
            .resampled
            .lock()
            .map(|map| map.values().map(ClipPcm::resident_bytes).sum::<usize>())
            .unwrap_or(0);
        self.pcm.resident_bytes() + resampled + self.peaks.encoded_len()
    }

    fn spooled_bytes(&self) -> u64 {
        let bytes = |pcm: &ClipPcm| match pcm {
            ClipPcm::Resident(_) => 0,
            ClipPcm::Spooled(_) => pcm.frames() as u64 * 8,
        };
        let resampled = self
            .resampled
            .lock()
            .map(|map| map.values().map(bytes).sum::<u64>())
            .unwrap_or(0);
        bytes(&self.pcm) + resampled
    }
}

/// Concurrent processes can bake the same key. Publish only complete files
/// from private temporary names and keep the first successful writer.
pub(crate) fn persist_cache_file<G>(
    temp: std::io::Result<tempfile::NamedTempFile>,
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
    accept: impl FnOnce() -> Option<G>,
) -> std::io::Result<bool> {
    let mut temp = temp?;
    write(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    let Some(_commit) = accept() else {
        return Ok(false);
    };
    match temp.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

/// Cache counters, used by tests and diagnostics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheStats {
    pub decodes: u64,
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub disk_writes: u64,
    pub resamples: u64,
    pub evictions: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<MediaKey, Arc<MediaEntry>>,
    lru: VecDeque<MediaKey>,
    stats: CacheStats,
    decodes_by_path: HashMap<PathBuf, u64>,
}

/// Shared cache for decoded media. `dir` is the persistent store; `None` keeps
/// the cache in memory only.
pub struct MediaCache {
    generation: CacheGeneration,
    playback_pins: PlaybackPins,
    scratch: ScratchDisk,
    publishing: ScratchDisk,
    decode_gates: CacheWorkGates<MediaKey>,
    inner: Mutex<Inner>,
    dir: Option<PathBuf>,
    memory_cap: usize,
    disk_cap: u64,
}

impl MediaCache {
    /// Create a cache with memory and disk budgets.
    pub fn new(dir: Option<PathBuf>, memory_cap: usize, disk_cap: u64) -> Self {
        Self {
            generation: CacheGeneration::default(),
            playback_pins: PlaybackPins::default(),
            scratch: ScratchDisk::for_pool(disk_cap, "source"),
            publishing: ScratchDisk::for_pool(disk_cap, "source-publication"),
            decode_gates: CacheWorkGates::default(),
            inner: Mutex::new(Inner::default()),
            dir,
            memory_cap,
            disk_cap,
        }
    }

    /// Persistent files and temporary PCM retained by live playback.
    pub fn disk_usage(&self) -> ClipCacheUsage {
        crate::clip_pcm::cache_usage(
            self.dir.as_deref(),
            "ocm",
            &self.playback_pins,
            self.disk_cap,
            self.scratch
                .used_on_volume(self.dir.as_deref())
                .saturating_add(self.publishing.used_on_volume(self.dir.as_deref())),
        )
    }

    pub(crate) fn pin_playback(
        &self,
        pcm: &ClipPcm,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Option<Arc<PlaybackPin>>, ClipPreparationError> {
        self.playback_pins.pin(
            pcm,
            self.memory_cap,
            self.dir.as_deref(),
            &self.scratch,
            cancellation,
        )
    }

    /// Cache configured from the environment: directory, memory cap, disk cap.
    pub fn from_env() -> Self {
        let dir = std::env::var_os("OCP_MEDIA_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(default_cache_dir);
        let memory_cap = env_bytes("OCP_MEDIA_CACHE_MEMORY_BYTES")
            .map(|bytes| bytes as usize)
            .unwrap_or(DEFAULT_MEMORY_BYTES);
        let disk_cap = env_bytes("OCP_MEDIA_CACHE_BYTES").unwrap_or(DEFAULT_DISK_BYTES);
        Self::new(dir, memory_cap, disk_cap)
    }

    /// Cache directory.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Decoded samples for `path`, reusing memory or disk without decoding twice.
    pub fn decoded(&self, path: &Path) -> Result<Arc<MediaEntry>, String> {
        Ok(self.decode_at_generation(
            path,
            self.generation.current(),
            ClipCancellation::default(),
        )?)
    }

    fn decode_at_generation(
        &self,
        path: &Path,
        generation: u64,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Arc<MediaEntry>, ClipPreparationError> {
        cancellation.check()?;
        let key = media_key(path)?;
        if let Some(entry) = self.memory_hit(&key) {
            return Ok(entry);
        }
        // A different media version can decode while this one is still loading.
        let gate = self.decode_gates.for_key(&key);
        let _gate = cancellation.lock(&gate)?;
        if let Some(entry) = self.memory_hit(&key) {
            return Ok(entry);
        }
        let (cached, _disk_work) = DiskCacheWork::lookup_or_lock(
            (self.disk_cap > 0 && cfg!(target_endian = "little"))
                .then(|| self.disk_path(&key))
                .flatten(),
            cancellation,
            || self.read_disk(&key),
        )?;
        if let Some(entry) = cached {
            if let Some(_commit) = self.generation.accept(generation) {
                self.insert(key, entry.clone());
            }
            return Ok(entry);
        }
        let disk_generation = self
            .dir
            .as_deref()
            .and_then(|dir| disk_cache_generation(dir, "ocm").ok());
        let mut pcm = PcmBuilder::new(self.source_spill());
        let rate = audio_clip::decode_packets(&key.path, |block| {
            cancellation.check()?;
            pcm.append(block)
        })?;
        cancellation.check()?;
        let pcm = pcm.finish()?;
        let peaks = PeakPyramid::build_pcm(&pcm, rate, cancellation)?;
        let mut entry = MediaEntry::new(key.clone(), rate, pcm, peaks);
        if let Some(_commit) = self.generation.accept(generation) {
            let mut inner = self.lock_inner();
            inner.stats.decodes += 1;
            *inner.decodes_by_path.entry(key.path.clone()).or_insert(0) += 1;
        }
        self.write_disk(&mut entry, generation, disk_generation, cancellation);
        cancellation.check()?;
        let entry = Arc::new(entry);
        if let Some(_commit) = self.generation.accept(generation) {
            self.insert(key, entry.clone());
            self.evict_disk();
        }
        Ok(entry)
    }

    /// Peak payload for a file, encoded for the frontend.
    pub fn pyramid_bytes(&self, path: &Path) -> Result<Vec<u8>, String> {
        Ok(self.decoded(path)?.pyramid().to_bytes())
    }

    /// Samples at `rate`, cached per file, rate and quality rung.
    pub fn resampled(
        &self,
        path: &Path,
        rate: u32,
        quality: ResampleQuality,
    ) -> Result<Arc<Vec<f32>>, String> {
        Ok(self.resampled_with_key(path, rate, quality)?.1)
    }

    /// Samples at `rate` plus the file version they were decoded from.
    pub fn resampled_with_key(
        &self,
        path: &Path,
        rate: u32,
        quality: ResampleQuality,
    ) -> Result<(MediaKey, Arc<Vec<f32>>), String> {
        let (key, pcm) =
            self.playback_with_key(path, rate, quality, ClipCancellation::default())?;
        Ok((key, pcm.materialize()?))
    }

    fn source_spill(&self) -> Option<(&Path, usize, &ScratchDisk)> {
        let dir = self.dir.as_deref()?;
        (self.disk_cap > 0).then_some((
            dir,
            self.memory_cap.min(MAX_RESIDENT_SOURCE_BYTES),
            &self.scratch,
        ))
    }

    pub(crate) fn playback_with_key(
        &self,
        path: &Path,
        rate: u32,
        quality: ResampleQuality,
        cancellation: ClipCancellation<'_>,
    ) -> Result<(MediaKey, ClipPcm), ClipPreparationError> {
        if rate == 0 {
            return Err("resample rates must be positive".into());
        }
        let entry = self.decode_at_generation(path, self.generation.current(), cancellation)?;
        if rate == entry.sample_rate {
            return Ok((entry.key.clone(), entry.pcm.clone()));
        }
        // Keep simultaneous seeks or renders from converting the same media twice.
        let mut variants = cancellation.lock(&entry.resampled)?;
        if let Some(samples) = variants.get(&(rate, quality)) {
            return Ok((entry.key.clone(), samples.clone()));
        }
        let frames = crate::resample::converted_frames(entry.frames(), entry.sample_rate, rate);
        let source_spill = self.source_spill();
        let samples = if let ClipPcm::Resident(source) = &entry.pcm
            && source_spill.is_none_or(|(_, limit, _)| frames.saturating_mul(8) <= limit)
        {
            ClipPcm::Resident(Arc::new(crate::resample::resample_cancellable(
                source,
                entry.sample_rate,
                rate,
                quality,
                cancellation,
            )?))
        } else {
            let mut output = PcmBuilder::new(source_spill);
            for start in (0..frames).step_by(RESAMPLE_BLOCK_FRAMES) {
                cancellation.check()?;
                let end = (start + RESAMPLE_BLOCK_FRAMES).min(frames);
                output.append(&crate::resample::resample_pcm_window(
                    &entry.pcm,
                    entry.sample_rate,
                    rate,
                    quality,
                    start..end,
                    cancellation,
                )?)?;
            }
            output.finish()?
        };
        cancellation.check()?;
        variants.insert((rate, quality), samples.clone());
        drop(variants);
        let mut inner = self.lock_inner();
        inner.stats.resamples += 1;
        self.evict_memory(&mut inner);
        Ok((entry.key.clone(), samples))
    }

    /// Cache counters snapshot.
    pub fn stats(&self) -> CacheStats {
        self.lock_inner().stats.clone()
    }

    /// Decodes performed for one canonical path, for cache assertions.
    pub fn decodes_for(&self, path: &Path) -> u64 {
        let Ok(key) = media_key(path) else {
            return 0;
        };
        *self
            .lock_inner()
            .decodes_by_path
            .get(&key.path)
            .unwrap_or(&0)
    }

    /// Drop indexed entries and remove cache files with no active reader.
    pub fn clear(&self) {
        let _commit = self.generation.advance();
        let mut inner = self.lock_inner();
        inner.entries.clear();
        inner.lru.clear();
        inner.stats = CacheStats::default();
        inner.decodes_by_path.clear();
        drop(inner);
        if let Some(dir) = &self.dir
            && (self.disk_cap > 0 || dir.is_dir())
            && let Err(error) = clear_disk_cache(dir, "ocm")
        {
            log::warn!("clip cache: cannot clear {}: {error}", dir.display());
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn memory_hit(&self, key: &MediaKey) -> Option<Arc<MediaEntry>> {
        let mut inner = self.lock_inner();
        let entry = inner.entries.get(key).cloned()?;
        if let Some(index) = inner.lru.iter().position(|candidate| candidate == key) {
            inner.lru.remove(index);
        }
        inner.lru.push_back(key.clone());
        inner.stats.memory_hits += 1;
        Some(entry)
    }

    fn insert(&self, key: MediaKey, entry: Arc<MediaEntry>) {
        let mut inner = self.lock_inner();
        inner.entries.insert(key.clone(), entry);
        if let Some(index) = inner.lru.iter().position(|candidate| *candidate == key) {
            inner.lru.remove(index);
        }
        inner.lru.push_back(key);
        self.evict_memory(&mut inner);
    }

    fn evict_memory(&self, inner: &mut Inner) {
        let mut resident = inner
            .entries
            .values()
            .map(|entry| entry.memory_bytes())
            .sum::<usize>();
        let mut spooled = inner
            .entries
            .values()
            .map(|entry| entry.spooled_bytes())
            .sum::<u64>();
        while (resident > self.memory_cap || spooled > self.disk_cap) && inner.entries.len() > 1 {
            let Some(oldest) = inner.lru.pop_front() else {
                break;
            };
            if let Some(dropped) = inner.entries.remove(&oldest) {
                resident = resident.saturating_sub(dropped.memory_bytes());
                spooled = spooled.saturating_sub(dropped.spooled_bytes());
                inner.stats.evictions += 1;
            }
        }
    }

    fn disk_path(&self, key: &MediaKey) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        Some(dir.join(key.file_name()))
    }

    fn read_disk(&self, key: &MediaKey) -> Option<Arc<MediaEntry>> {
        if !cfg!(target_endian = "little") || self.disk_cap == 0 {
            return None;
        }
        let path = self.disk_path(key)?;
        if fs::metadata(&path).ok()?.len() > self.disk_cap {
            return None;
        }
        let limit = self.memory_cap.min(MAX_RESIDENT_SOURCE_BYTES);
        let (sample_rate, peaks, pcm) = Self::read_cache_file(&path, limit)?;
        let entry = Arc::new(MediaEntry::new(key.clone(), sample_rate, pcm, peaks));
        self.stamp_access(&path);
        self.lock_inner().stats.disk_hits += 1;
        Some(entry)
    }

    fn read_cache_file(path: &Path, resident_limit: usize) -> Option<(u32, PeakPyramid, ClipPcm)> {
        let mut named = named_spools();
        let (file, pin) = open_named_spool(path).ok()?;
        let loaded = Self::decode_cache_file(path, file, pin, resident_limit, &mut named);
        if loaded.is_none() {
            let _ = named.remove_unused(path);
        }
        loaded
    }

    fn decode_cache_file(
        path: &Path,
        file: File,
        pin: File,
        resident_limit: usize,
        named: &mut crate::clip_pcm::NamedSpools,
    ) -> Option<(u32, PeakPyramid, ClipPcm)> {
        let length = file.metadata().ok()?.len();
        let mut reader = BufReader::new(file);
        let mut header = [0_u8; CACHE_HEADER_BYTES];
        reader.read_exact(&mut header).ok()?;
        if &header[0..4] != CACHE_MAGIC || header[4] != CACHE_VERSION {
            return None;
        }
        let sample_rate = u32::from_le_bytes(header[14..18].try_into().ok()?);
        let frames = u64::from_le_bytes(header[18..26].try_into().ok()?) as usize;
        let peak_bytes = u32::from_le_bytes(header[26..30].try_into().ok()?) as usize;
        let pcm_bytes = frames.checked_mul(8)?;
        let offset = CACHE_HEADER_BYTES.checked_add(peak_bytes)?;
        if length != offset.checked_add(pcm_bytes)? as u64
            || peak_bytes
                > frames
                    .div_ceil(crate::audio_peaks::BASE_BUCKET_FRAMES as usize)
                    .saturating_mul(24)
                    .saturating_add(1024)
        {
            return None;
        }
        let mut peak_payload = vec![0_u8; peak_bytes];
        reader.read_exact(&mut peak_payload).ok()?;
        let peaks = PeakPyramid::from_bytes(&peak_payload).ok()?;
        if peaks.sample_rate != sample_rate || peaks.frames != frames as u64 {
            return None;
        }
        let pcm = if pcm_bytes > resident_limit {
            named.register(path, reader.into_inner(), pin, frames, offset as u64)
        } else {
            let mut samples = vec![0_f32; frames.checked_mul(2)?];
            reader
                .read_exact(bytemuck::cast_slice_mut(&mut samples))
                .ok()?;
            ClipPcm::Resident(Arc::new(samples))
        };
        Some((sample_rate, peaks, pcm))
    }

    fn write_disk(
        &self,
        entry: &mut MediaEntry,
        generation: u64,
        disk_generation: Option<u64>,
        cancellation: ClipCancellation<'_>,
    ) {
        let Some(disk_generation) = disk_generation else {
            return;
        };
        if self.disk_cap == 0
            || !cfg!(target_endian = "little")
            || self.generation.current() != generation
            || cancellation.check().is_err()
        {
            return;
        }
        let Some(path) = self.disk_path(&entry.key) else {
            return;
        };
        if path.exists() {
            return;
        }
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let peaks = entry.pyramid().to_bytes();
        let offset = CACHE_HEADER_BYTES + peaks.len();
        let spooled = matches!(entry.pcm, ClipPcm::Spooled(_));
        let byte_count = (entry.frames() as u64)
            .saturating_mul(8)
            .saturating_add(offset as u64);
        let Ok(publication) = self.publishing.reserve_on_volume(dir, byte_count) else {
            return;
        };
        let write = persist_cache_file(
            publication.stage_pcm(),
            &path,
            |file| {
                let mut header = Vec::with_capacity(CACHE_HEADER_BYTES);
                header.extend_from_slice(CACHE_MAGIC);
                header.push(CACHE_VERSION);
                header.push(0);
                header.extend_from_slice(&now_nanos().to_le_bytes());
                header.extend_from_slice(&entry.sample_rate.to_le_bytes());
                header.extend_from_slice(&(entry.frames() as u64).to_le_bytes());
                header.extend_from_slice(&(peaks.len() as u32).to_le_bytes());
                file.write_all(&header)?;
                file.write_all(&peaks)?;
                entry
                    .pcm
                    .try_visit(0, entry.frames(), |_, samples| {
                        for block in samples.chunks(4096) {
                            cancellation.check()?;
                            file.write_all(bytemuck::cast_slice(block))
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(())
                    })
                    .map_err(std::io::Error::other)
            },
            || {
                cancellation.check().ok()?;
                let local = self.generation.accept(generation)?;
                let directory =
                    accept_disk_cache_generation(dir, "ocm", disk_generation).ok()??;
                Some((local, directory))
            },
        );
        match write {
            Ok(true) => {
                if let Some(_commit) = self.generation.accept(generation) {
                    self.lock_inner().stats.disk_writes += 1;
                }
            }
            Ok(false) => {}
            Err(error) if !cancellation.is_cancelled() => {
                log::warn!("media cache: cannot write {}: {error}", path.display());
            }
            Err(_) => {}
        }
        if spooled
            && let Some((sample_rate, _, pcm)) = Self::read_cache_file(&path, 0)
            && sample_rate == entry.sample_rate
            && pcm.frames() == entry.frames()
        {
            entry.pcm = pcm;
        }
    }

    fn stamp_access(&self, path: &Path) {
        if let Ok(mut file) = OpenOptions::new().write(true).open(path) {
            let _ = file.seek(SeekFrom::Start(ACCESS_OFFSET));
            let _ = file.write_all(&now_nanos().to_le_bytes());
        }
    }

    /// Delete the oldest cache files until the on-disk total fits the cap.
    fn evict_disk(&self) {
        if self.disk_cap == 0 {
            return;
        }
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if !cache_file_name(&path, "ocm") || !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), path))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        if total <= self.disk_cap {
            return;
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        let mut removed = 0_u64;
        for (_, size, path) in files {
            if total <= self.disk_cap {
                break;
            }
            if named_spools().remove_unused(&path).unwrap_or(false) {
                total = total.saturating_sub(size);
                removed += 1;
            }
        }
        if removed > 0 {
            self.lock_inner().stats.evictions += removed;
        }
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

fn env_bytes(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// Platform cache directory used when the GUI does not supply one. A test build
/// never resolves the operator's home; it uses the process scratch root.
pub fn default_cache_dir() -> Option<PathBuf> {
    if oc_audio::private_fs::running_under_test() {
        return Some(oc_audio::private_fs::test_config_dir().join("media"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Caches/OpenCompose/media"))
    }
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        Some(local.join("OpenCompose/cache/media"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"));
        Some(cache.join("opencompose/media"))
    }
}

static SHARED: OnceLock<MediaCache> = OnceLock::new();

/// Process-wide cache used by the render path and the GUI peaks command.
pub fn shared() -> &'static MediaCache {
    SHARED.get_or_init(MediaCache::from_env)
}

/// Install an explicit process cache before first use, or accept an identical
/// initialization. A conflicting configuration is reported instead of ignored.
/// Construct the cache with `from_env` when environment overrides must apply.
pub fn init_shared_cache(cache: MediaCache) -> Result<&'static MediaCache, String> {
    let dir = cache.dir.clone();
    let memory_cap = cache.memory_cap;
    let disk_cap = cache.disk_cap;
    let shared = SHARED.get_or_init(|| cache);
    if shared.dir != dir || shared.memory_cap != memory_cap || shared.disk_cap != disk_cap {
        return Err(
            "shared media cache is already initialized with a different configuration".into(),
        );
    }
    Ok(shared)
}

/// Point the process cache at the app cache dir; a no-op once it is in use.
pub fn init_shared(dir: PathBuf, disk_cap: u64) -> Result<(), String> {
    if SHARED.get().is_some() || std::env::var_os("OCP_MEDIA_CACHE_DIR").is_some() {
        return Ok(());
    }
    let memory_cap = std::env::var("OCP_MEDIA_CACHE_MEMORY_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MEMORY_BYTES);
    let _ = SHARED.set(MediaCache::new(Some(dir), memory_cap, disk_cap));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_shared_cache_initialization_rejects_conflicting_settings() {
        let current = shared();
        let same = MediaCache::new(current.dir.clone(), current.memory_cap, current.disk_cap);
        assert!(std::ptr::eq(init_shared_cache(same).unwrap(), current));
        let conflict = MediaCache::new(
            current.dir.clone(),
            current.memory_cap ^ 1,
            current.disk_cap,
        );
        assert!(init_shared_cache(conflict).is_err());
        assert!(std::ptr::eq(shared(), current));
    }

    fn temp_dir(name: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-media-cache", name)
    }

    fn write_wav(path: &Path, frames: usize) {
        write_wav_at(path, 48_000, frames);
    }

    fn write_wav_at(path: &Path, sample_rate: u32, frames: usize) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let value = (frame as f32 * 0.01).sin() * 0.5;
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn clearing_media_cache_keeps_unowned_files_and_derived_audio() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("recording.wav");
        write_wav(&source, 1024);
        let derived = dir.path().join("6f4beed2ca7a45b349add260d0b1cb8f.ocd");
        fs::write(&derived, b"separate cache").unwrap();
        let cache = MediaCache::new(Some(dir.path().to_path_buf()), 1 << 20, 2 << 20);
        cache.decoded(&source).unwrap();
        let media_entry = dir.path().join(media_key(&source).unwrap().file_name());
        assert!(media_entry.is_file());
        cache.clear();
        assert!(!media_entry.exists());
        assert!(source.is_file());
        assert!(derived.is_file());
    }

    #[test]
    fn concurrent_cache_writers_publish_only_one_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("media.ocm");
        let barrier = std::sync::Barrier::new(2);
        let writes = std::thread::scope(|scope| {
            let handles = [0x19, 0xfa].map(|byte| {
                let barrier = &barrier;
                let target = &target;
                let dir = dir.path();
                scope.spawn(move || {
                    persist_cache_file(
                        tempfile::NamedTempFile::new_in(dir),
                        target,
                        |file| {
                            file.write_all(&[byte; 512])?;
                            barrier.wait();
                            file.write_all(&[byte; 512])
                        },
                        || Some(()),
                    )
                    .unwrap()
                })
            });
            handles.map(|handle| handle.join().unwrap())
        });
        assert_ne!(writes[0], writes[1]);
        let output = fs::read(target).unwrap();
        assert_eq!(output, vec![if writes[0] { 0x19 } else { 0xfa }; 1024]);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn second_load_hits_the_cache() {
        let dir = temp_dir("hit");
        let media = dir.join("tone.wav");
        write_wav(&media, 4096);
        let cache = MediaCache::new(Some(dir.join("cache")), 64 * 1024 * 1024, 64 * 1024 * 1024);
        let first = cache.decoded(&media).unwrap();
        let second = cache.decoded(&media).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.stats().decodes, 1);
        assert_eq!(cache.stats().memory_hits, 1);
        assert_eq!(cache.decodes_for(&media), 1);
        cache.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_mtime_misses_and_invalidates_only_that_entry() {
        let dir = temp_dir("mtime");
        let first_file = dir.join("a.wav");
        let second_file = dir.join("b.wav");
        write_wav(&first_file, 2048);
        write_wav(&second_file, 2048);
        let cache = MediaCache::new(Some(dir.join("cache")), 64 * 1024 * 1024, 64 * 1024 * 1024);
        cache.decoded(&first_file).unwrap();
        cache.decoded(&second_file).unwrap();
        assert_eq!(cache.stats().decodes, 2);

        std::thread::sleep(Duration::from_millis(30));
        write_wav(&first_file, 2048);
        assert_ne!(
            media_key(&first_file).unwrap().mtime,
            media_key(&second_file).unwrap().mtime
        );
        cache.decoded(&first_file).unwrap();
        assert_eq!(cache.stats().decodes, 3, "changed mtime must decode again");
        cache.decoded(&second_file).unwrap();
        assert_eq!(cache.stats().decodes, 3, "unchanged entry stays cached");
        cache.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disk_entry_survives_a_fresh_cache_instance() {
        let dir = temp_dir("disk");
        let media = dir.join("tone.wav");
        write_wav(&media, 48_000);
        let cache_dir = dir.join("cache");
        {
            let cache =
                MediaCache::new(Some(cache_dir.clone()), 64 * 1024 * 1024, 64 * 1024 * 1024);
            let entry = cache.decoded(&media).unwrap();
            let peaks = entry.pyramid();
            assert_eq!(peaks.frames, 48_000);
            assert_eq!(cache.stats().disk_writes, 1);
        }
        let reopened = MediaCache::new(Some(cache_dir), 64 * 1024 * 1024, 64 * 1024 * 1024);
        let entry = reopened.decoded(&media).unwrap();
        assert_eq!(entry.frames(), 48_000);
        assert_eq!(entry.sample_rate(), 48_000);
        assert_eq!(reopened.stats().disk_hits, 1);
        assert_eq!(reopened.stats().decodes, 0);
        reopened.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_decode_waiting_on_one_source_does_not_hold_up_another() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.wav");
        let second = dir.path().join("b.wav");
        write_wav(&first, 128);
        write_wav(&second, 256);
        let cache = Arc::new(MediaCache::new(None, 1 << 20, 0));
        let gate = cache.decode_gates.for_key(&media_key(&first).unwrap());
        let blocked = gate.lock().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn({
            let cache = cache.clone();
            move || {
                send.send(cache.decoded(&second).map(|entry| entry.frames()))
                    .unwrap()
            }
        });
        let result = receive.recv_timeout(Duration::from_secs(2));
        drop(blocked);
        worker.join().unwrap();
        assert_eq!(result.unwrap().unwrap(), 256);
    }

    #[test]
    fn clearing_while_decoding_keeps_the_result_private_to_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("source.wav");
        write_wav(&media, 4096);
        let cache = MediaCache::new(Some(dir.path().join("cache")), 16 << 10, 48 << 10);
        let generation = cache.generation.current();
        cache.clear();

        let old = cache
            .decode_at_generation(&media, generation, ClipCancellation::default())
            .unwrap();
        assert_eq!(old.frames(), 4096);
        assert!(cache.lock_inner().entries.is_empty());
        assert_eq!(cache.disk_usage().cached_bytes, 0);
        assert!(
            old.pcm
                .window(10..14)
                .unwrap()
                .iter()
                .any(|value| *value != 0.0)
        );
        drop(old);

        assert_eq!(cache.decoded(&media).unwrap().frames(), 4096);
        assert!(cache.disk_usage().cached_bytes > 0);
    }

    #[test]
    fn clearing_after_a_cache_write_starts_prevents_its_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cache.ocm");
        let other_cache = MediaCache::new(Some(dir.path().to_path_buf()), 1024, 1024);
        let generation = disk_cache_generation(dir.path(), "ocm").unwrap();
        let derived_generation = disk_cache_generation(dir.path(), "ocd").unwrap();
        let published = persist_cache_file(
            tempfile::NamedTempFile::new_in(dir.path()),
            &target,
            |file| {
                file.write_all(b"staged data")?;
                other_cache.clear();
                Ok(())
            },
            || accept_disk_cache_generation(dir.path(), "ocm", generation).unwrap(),
        )
        .unwrap();
        assert!(!published);
        assert!(!target.exists());
        assert_eq!(
            disk_cache_generation(dir.path(), "ocd").unwrap(),
            derived_generation
        );
        assert!(fs::read_dir(dir.path()).unwrap().flatten().all(|entry| {
            matches!(
                entry.file_name().to_str(),
                Some(".clip-spools.lock" | ".ocm.generation")
            )
        }));
    }

    #[test]
    fn scratch_is_released_after_a_source_decode_hits_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("source.wav");
        write_wav(&media, 4096);
        let cache = MediaCache::new(Some(dir.path().join("cache")), 16 << 10, 48 << 10);
        let blocked = cache.scratch.reserve(24 << 10).unwrap();
        let error = cache.decoded(&media).err().expect("scratch is full");
        assert!(error.contains("shared disk budget"), "{error}");
        assert_eq!(cache.stats().decodes, 0);
        assert_eq!(cache.disk_usage().temporary_bytes, 24 << 10);
        drop(blocked);

        let entry = cache.decoded(&media).unwrap();
        assert_eq!(entry.frames(), 4096);
        let usage = cache.disk_usage();
        assert_eq!(usage.temporary_bytes, 0);
        assert!(usage.cached_bytes > 4096 * 8);
    }

    #[test]
    fn spooled_decode_peaks_and_mixed_rate_windows_match_resident_media() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("source.wav");
        write_wav_at(&media, 44_100, 34_567);
        let resident = MediaCache::new(None, 16 << 20, 0);
        let expected = resident.decoded(&media).unwrap();
        let reference = resident
            .resampled(&media, 48_000, ResampleQuality::PRESERVED)
            .unwrap();
        let cache_dir = dir.path().join("cache");
        let spooled = MediaCache::new(Some(cache_dir.clone()), 32 << 10, 2 << 20);
        let entry = spooled.decoded(&media).unwrap();
        assert!(matches!(entry.pcm, ClipPcm::Spooled(_)));
        assert_eq!(*entry.pyramid(), *expected.pyramid());
        let (_, playback) = spooled
            .playback_with_key(
                &media,
                48_000,
                ResampleQuality::PRESERVED,
                ClipCancellation::default(),
            )
            .unwrap();
        assert!(matches!(playback, ClipPcm::Spooled(_)));
        for window in [
            0..512,
            16_350..16_550,
            32_730..33_100,
            playback.frames() - 512..playback.frames(),
        ] {
            assert_eq!(
                playback.window(window.clone()).unwrap(),
                reference[window.start * 2..window.end * 2]
            );
        }
        assert_eq!(spooled.stats().decodes, 1);
        assert_eq!(spooled.stats().resamples, 1);
        assert_eq!(playback.resident_bytes(), 0);

        assert_eq!(spooled.stats().disk_writes, 1);
        drop(spooled);
        let reopened = MediaCache::new(Some(cache_dir), 32 << 10, 2 << 20);
        let entry = reopened.decoded(&media).unwrap();
        assert_eq!(reopened.stats().disk_hits, 1);
        assert_eq!(reopened.stats().decodes, 0);
        assert!(matches!(entry.pcm, ClipPcm::Spooled(_)));
        assert_eq!(
            entry.pcm.window(123..789).unwrap(),
            expected.pcm.window(123..789).unwrap()
        );
    }

    #[test]
    fn eviction_removes_the_oldest_disk_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = MediaCache::new(Some(cache_dir), 64 * 1024 * 1024, 12 << 10);
        let paths = ["a.wav", "b.wav"].map(|name| dir.path().join(name));
        let mut cached = Vec::new();
        for path in paths {
            write_wav(&path, 1024);
            cache.decoded(&path).unwrap();
            let cache_path = cache.disk_path(&media_key(&path).unwrap()).unwrap();
            assert!(cache_path.exists());
            cached.push(cache_path);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!cached[0].exists(), "the oldest file must be evicted");
        assert!(cached[1].exists());
    }

    #[test]
    fn resampled_buffers_are_keyed_by_quality_step() {
        let dir = temp_dir("quality");
        let media = dir.join("tone.wav");
        write_wav_at(&media, 44_100, 44_100);
        let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
        let fast = cache
            .resampled_with_key(&media, 48_000, ResampleQuality::FASTEST)
            .unwrap()
            .1;
        let best = cache
            .resampled_with_key(&media, 48_000, ResampleQuality::BEST)
            .unwrap()
            .1;
        assert_eq!(fast.len(), best.len());
        assert_ne!(*fast, *best, "different rungs must decode independently");
        assert_eq!(cache.stats().resamples, 2);
        let again = cache
            .resampled_with_key(&media, 48_000, ResampleQuality::FASTEST)
            .unwrap()
            .1;
        assert!(Arc::ptr_eq(&fast, &again), "same rung must hit the cache");
        assert_eq!(cache.stats().resamples, 2);
        cache.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_media_is_not_cached() {
        let dir = temp_dir("missing");
        let cache = MediaCache::new(None, 1024, 0);
        let error = match cache.decoded(&dir.join("missing.wav")) {
            Ok(_) => panic!("missing media must fail"),
            Err(error) => error,
        };
        assert!(error.contains("file not found"), "{error}");
        assert_eq!(cache.stats().decodes, 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
