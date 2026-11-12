//! Derived-media cache: baked clip variants keyed by source media version,
//! stretch parameters and the algorithm version. Offline/worker path only.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use oc_dsl::ast::StretchMode;

use crate::clip_pcm::{
    CacheGeneration, CacheWorkGates, ClipCancellation, ClipPcm, ClipPreparationError,
    DiskCacheWork, PlaybackPin, PlaybackPins, ScratchDisk, accept_disk_cache_generation,
    cache_file_name, clear_disk_cache, disk_cache_generation, named_spools, open_named_spool,
    pin_named_spool,
};
use crate::media_cache::{MediaKey, persist_cache_file};
use crate::resample::ResampleQuality;
use crate::stretch::{self, StretchPlan};

/// Memory cap for resident derived variants; separate from the decoded media cap.
pub const DEFAULT_MEMORY_BYTES: usize = 256 * 1024 * 1024;
/// On-disk cap for derived variant files in the media cache directory.
pub const DEFAULT_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const CACHE_MAGIC: &[u8; 4] = b"OCMD";
const CACHE_VERSION: u8 = 1;
/// magic, version, flags, access stamp, rate, frames.
const CACHE_HEADER_BYTES: usize = 26;
const ACCESS_OFFSET: u64 = 6;

/// Identity of one baked variant.
///
/// The source `MediaKey` ties a variant to one file version; `ratio_bits`
/// keeps every distinct f64 ratio apart; `pitch_rate` keeps transpositions
/// apart even when two plans end up at the same ratio; `algo_version`
/// invalidates variants when the DSP changes instead of requiring a manual wipe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StretchKey {
    pub media: MediaKey,
    pub target_rate: u32,
    /// Ratio as raw bits, so equal plans share and any change misses.
    pub ratio_bits: u64,
    pub mode: StretchMode,
    /// Playback-speed resample rate; zero when the clip is not transposed.
    pub pitch_rate: u32,
    pub resample_step: u8,
    pub formant_bits: u32,
    pub warp: Vec<(usize, usize)>,
    pub algo_version: u32,
}

impl StretchKey {
    /// A derived-cache key for one stretch request.
    pub fn new(
        media: MediaKey,
        target_rate: u32,
        ratio: f64,
        mode: StretchMode,
        algo_version: u32,
    ) -> Self {
        Self {
            media,
            target_rate,
            ratio_bits: ratio.to_bits(),
            mode,
            pitch_rate: 0,
            resample_step: ResampleQuality::PRESERVED.step(),
            formant_bits: 0,
            warp: Vec::new(),
            algo_version,
        }
    }

    pub(crate) fn for_plan(
        media: &MediaKey,
        target_rate: u32,
        plan: &StretchPlan,
        quality: ResampleQuality,
    ) -> Self {
        let mut key = Self::new(
            media.clone(),
            target_rate,
            plan.ratio,
            plan.mode,
            stretch::ALGO_VERSION,
        )
        .with_pitch_rate(plan.pitch_rate)
        .with_formants(plan.formant_cents)
        .with_warp(&plan.warp);
        key.resample_step = quality.step();
        key
    }

    /// Record the pitch step; variants without pitch keep their previous name.
    pub fn with_pitch_rate(mut self, pitch_rate: u32) -> Self {
        self.pitch_rate = pitch_rate;
        self
    }

    /// Add a formant shift to the key.
    pub fn with_formants(mut self, cents: f32) -> Self {
        self.formant_bits = cents.to_bits();
        self
    }

    /// Add warp boundaries to the key.
    pub fn with_warp(mut self, boundaries: &[(usize, usize)]) -> Self {
        self.warp = boundaries.to_vec();
        self
    }

    /// Stable string used to derive the on-disk file name.
    pub fn stamp(&self) -> String {
        let base = format!(
            "{}|{}|{}|{:016x}|{}",
            self.media.stamp(),
            self.target_rate,
            self.algo_version,
            self.ratio_bits,
            self.mode.as_str()
        );
        let mut stamp = base;
        if self.pitch_rate != 0 {
            stamp.push_str(&format!("|p{}", self.pitch_rate));
        }
        if self.resample_step != ResampleQuality::PRESERVED.step() {
            stamp.push_str(&format!("|q{}", self.resample_step));
        }
        if self.formant_bits != 0 {
            stamp.push_str(&format!("|f{:08x}", self.formant_bits));
        }
        for (source, target) in &self.warp {
            stamp.push_str(&format!("|w{source}:{target}"));
        }
        stamp
    }

    /// On-disk file name for the key.
    pub fn file_name(&self) -> String {
        format!("{:032x}.ocd", Md5::digest(self.stamp().as_bytes()))
    }
}

/// Cache counters, used by tests and diagnostics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedStats {
    pub bakes: u64,
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub disk_writes: u64,
    pub evictions: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<StretchKey, Arc<Vec<f32>>>,
    lru: VecDeque<StretchKey>,
    bytes: usize,
    stats: DerivedStats,
}

/// Memory and disk cache for baked variants. `dir` is the persistent store;
/// `None` keeps entries in memory only.
pub struct DerivedCache {
    generation: CacheGeneration,
    bake_gates: CacheWorkGates<StretchKey>,
    playback_pins: PlaybackPins,
    scratch: ScratchDisk,
    inner: Mutex<Inner>,
    dir: Option<PathBuf>,
    memory_cap: usize,
    disk_cap: u64,
}

impl DerivedCache {
    /// Create a cache with memory and disk budgets.
    pub fn new(dir: Option<PathBuf>, memory_cap: usize, disk_cap: u64) -> Self {
        Self {
            generation: CacheGeneration::default(),
            bake_gates: CacheWorkGates::default(),
            playback_pins: PlaybackPins::default(),
            scratch: ScratchDisk::for_pool(disk_cap, "derived"),
            inner: Mutex::new(Inner::default()),
            dir,
            memory_cap,
            disk_cap,
        }
    }

    /// Persistent files and temporary PCM retained by live playback.
    pub fn disk_usage(&self) -> crate::media_cache::ClipCacheUsage {
        crate::clip_pcm::cache_usage(
            self.dir.as_deref(),
            "ocd",
            &self.playback_pins,
            self.disk_cap,
            self.scratch.used_on_volume(self.dir.as_deref()),
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
        let dir = std::env::var_os("OCP_DERIVED_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("OCP_MEDIA_CACHE_DIR").map(PathBuf::from))
            .or_else(crate::media_cache::default_cache_dir);
        let memory_cap = env_bytes("OCP_DERIVED_CACHE_MEMORY_BYTES")
            .map(|bytes| bytes as usize)
            .unwrap_or(DEFAULT_MEMORY_BYTES);
        let disk_cap = env_bytes("OCP_DERIVED_CACHE_BYTES").unwrap_or(DEFAULT_DISK_BYTES);
        Self::new(dir, memory_cap, disk_cap)
    }

    /// Persistent cache directory; `None` keeps generated variants in memory.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Memory cap in bytes.
    pub fn memory_cap(&self) -> usize {
        self.memory_cap
    }

    /// Resident bytes.
    pub fn memory_bytes(&self) -> usize {
        self.lock_inner().bytes
    }

    /// Cache counters snapshot.
    pub fn stats(&self) -> DerivedStats {
        self.lock_inner().stats.clone()
    }

    /// Resident samples for `key`, promoting the entry in the LRU order.
    pub fn lookup(&self, key: &StretchKey) -> Option<Arc<Vec<f32>>> {
        let mut inner = self.lock_inner();
        let samples = inner.entries.get(key).cloned()?;
        if let Some(index) = inner.lru.iter().position(|candidate| candidate == key) {
            inner.lru.remove(index);
        }
        inner.lru.push_back(key.clone());
        inner.stats.memory_hits += 1;
        Some(samples)
    }

    /// Return the variant for `key`, baking it at most once per key.
    ///
    /// `expected_bytes` is the size the caller is about to bake; it is refused
    /// before baking when the cache cannot hold the variant at all.
    pub fn get_or_bake<F>(
        &self,
        key: StretchKey,
        expected_bytes: usize,
        bake: F,
    ) -> Result<Arc<Vec<f32>>, String>
    where
        F: FnOnce() -> Result<Vec<f32>, String>,
    {
        self.get_or_bake_cancellable(key, expected_bytes, ClipCancellation::default(), bake)
    }

    fn get_or_bake_cancellable(
        &self,
        key: StretchKey,
        expected_bytes: usize,
        cancellation: ClipCancellation<'_>,
        bake: impl FnOnce() -> Result<Vec<f32>, String>,
    ) -> Result<Arc<Vec<f32>>, String> {
        cancellation.check()?;
        let generation = self.generation.current();
        if let Some(samples) = self.lookup(&key) {
            return Ok(samples);
        }
        if expected_bytes > self.memory_cap {
            return Err(format!(
                "derived variant needs {:.1} MiB, above the {:.1} MiB derived-media budget",
                mib(expected_bytes),
                mib(self.memory_cap)
            ));
        }
        let gate = self.bake_gates.for_key(&key);
        let _bake = cancellation.lock(&gate)?;
        if let Some(samples) = self.lookup(&key) {
            return Ok(samples);
        }
        let (cached, _disk_work) = DiskCacheWork::lookup_or_lock(
            (self.disk_cap > 0 && cfg!(target_endian = "little"))
                .then(|| self.disk_path(&key))
                .flatten(),
            cancellation,
            || self.read_disk(&key),
        )?;
        if let Some(samples) = cached {
            if let Some(_commit) = self.generation.accept(generation) {
                self.insert(key, samples.clone())?;
            }
            return Ok(samples);
        }
        let disk_generation = self
            .dir
            .as_deref()
            .and_then(|dir| disk_cache_generation(dir, "ocd").ok());
        let samples = Arc::new(bake()?);
        cancellation.check()?;
        if let Some(_commit) = self.generation.accept(generation) {
            self.lock_inner().stats.bakes += 1;
            self.insert(key.clone(), samples.clone())?;
        }
        self.write_disk(&key, &samples, generation, disk_generation, cancellation);
        cancellation.check()?;
        Ok(samples)
    }

    /// Bake the variant `plan` describes of project-rate `source`, or reuse a
    /// cached one. A plan without pitch bakes exactly the stretched source.
    pub fn stretch(
        &self,
        media: &MediaKey,
        source: Arc<Vec<f32>>,
        target_rate: u32,
        plan: &StretchPlan,
        quality: ResampleQuality,
    ) -> Result<Arc<Vec<f32>>, String> {
        self.stretch_cancellable(
            media,
            source,
            target_rate,
            plan,
            quality,
            ClipCancellation::default(),
        )
    }

    pub(crate) fn stretch_cancellable(
        &self,
        media: &MediaKey,
        source: Arc<Vec<f32>>,
        target_rate: u32,
        plan: &StretchPlan,
        quality: ResampleQuality,
        cancellation: ClipCancellation<'_>,
    ) -> Result<Arc<Vec<f32>>, String> {
        let key = StretchKey::for_plan(media, target_rate, plan, quality);
        let expected = plan
            .output_frames(source.len() / 2, target_rate)
            .saturating_mul(8);
        self.get_or_bake_cancellable(key, expected, cancellation, || {
            plan.bake_cancellable(&source, target_rate, quality, cancellation)
        })
    }

    pub(crate) fn spool_dir(&self, bytes: usize) -> Option<&Path> {
        if self.disk_cap > 0
            && (bytes as u64).saturating_add(CACHE_HEADER_BYTES as u64) <= self.disk_cap
        {
            self.dir.as_deref()
        } else {
            None
        }
    }

    pub(crate) fn spool_stretch(
        &self,
        dir: &Path,
        mut key: StretchKey,
        source: &crate::clip_pcm::ClipPcm,
        plan: &StretchPlan,
        quality: ResampleQuality,
        cancellation: ClipCancellation<'_>,
    ) -> Result<(StretchKey, crate::clip_pcm::ClipPcm), ClipPreparationError> {
        use crate::clip_pcm::ClipPcm;
        let generation = self.generation.current();
        let frames = plan.output_frames(source.frames(), key.target_rate);
        let continuous =
            matches!(source, ClipPcm::Resident(_)) && frames.saturating_mul(8) <= self.memory_cap;
        if !continuous {
            key.algo_version |= 1 << 31;
        }
        let gate = self.bake_gates.for_key(&key);
        let _bake = cancellation.lock(&gate)?;
        let path = dir.join(key.file_name());
        let (cached, _disk_work) = DiskCacheWork::lookup_or_lock(
            cfg!(target_endian = "little").then(|| path.clone()),
            cancellation,
            || {
                cfg!(target_endian = "little")
                    .then(|| self.open_spooled(&path, &key, frames))
                    .flatten()
            },
        )?;
        if let Some(pcm) = cached {
            self.stamp_access(&path);
            self.lock_inner().stats.disk_hits += 1;
            return Ok((key, pcm));
        }

        let disk_generation = disk_cache_generation(dir, "ocd").ok();
        let bytes = (frames as u64)
            .saturating_mul(8)
            .saturating_add(CACHE_HEADER_BYTES as u64);
        let lease = self.scratch.reserve_on_volume(dir, bytes)?;
        let error = |error: std::io::Error| format!("Cannot spool clip audio: {error}");
        let temp = lease.stage_pcm().map_err(error)?;
        let mut writer = BufWriter::new(temp);
        writer
            .write_all(&cache_header(&key, frames))
            .map_err(error)?;
        let mut written = 0;
        let mut emit = |block: &[f32]| {
            cancellation.check()?;
            let buffered = writer.buffer().len() as u64;
            self.scratch
                .check_growth(dir, buffered + std::mem::size_of_val(block) as u64)
                .map_err(String::from)?;
            writer
                .write_all(bytemuck::cast_slice(block))
                .map_err(error)?;
            written += block.len() / 2;
            Ok(())
        };
        match source {
            ClipPcm::Resident(samples) if continuous => {
                emit(&plan.bake_cancellable(samples, key.target_rate, quality, cancellation)?)?
            }
            _ => plan.bake_blocks(source, key.target_rate, quality, cancellation, emit)?,
        }
        cancellation.check()?;
        if written != frames {
            return Err(format!("Clip bake wrote {written} frames, expected {frames}").into());
        }
        let temp = writer
            .into_inner()
            .map_err(|failure| error(failure.into_error()))?;
        if let Some(_commit) = self.generation.accept(generation) {
            self.lock_inner().stats.bakes += 1;
        }

        if cfg!(target_endian = "little") {
            temp.as_file().sync_all().map_err(error)?;
        }
        cancellation.check()?;
        let file = if cfg!(target_endian = "little")
            && let Some(_commit) = self.generation.accept(generation)
        {
            let mut named = named_spools();
            if let Some(pcm) = named.get(&path) {
                return Ok((key, pcm));
            }
            let locked_pin = disk_generation
                .and_then(|generation| {
                    accept_disk_cache_generation(dir, "ocd", generation)
                        .ok()
                        .flatten()
                })
                .and_then(|directory| pin_named_spool(&path).ok().map(|pin| (directory, pin)));
            if let Some((directory, pin)) = locked_pin {
                match temp.persist_noclobber(&path) {
                    Ok(file) => {
                        let pcm =
                            named.register(&path, file, pin, frames, CACHE_HEADER_BYTES as u64);
                        drop(directory);
                        drop(named);
                        self.lock_inner().stats.disk_writes += 1;
                        self.evict_disk();
                        return Ok((key, pcm));
                    }
                    Err(failure) if failure.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        drop(pin);
                        drop(directory);
                        drop(named);
                        if let Some(pcm) = self.open_spooled(&path, &key, frames) {
                            return Ok((key, pcm));
                        }
                        failure.file.into_file()
                    }
                    Err(failure) => {
                        log::warn!(
                            "derived cache: cannot publish {}: {}",
                            path.display(),
                            failure.error
                        );
                        failure.file.into_file()
                    }
                }
            } else {
                temp.into_file()
            }
        } else {
            temp.into_file()
        };
        Ok((
            key,
            ClipPcm::temporary_file(file, frames, CACHE_HEADER_BYTES as u64, lease),
        ))
    }

    fn open_spooled(
        &self,
        path: &Path,
        key: &StretchKey,
        frames: usize,
    ) -> Option<crate::clip_pcm::ClipPcm> {
        let mut named = named_spools();
        if let Some(pcm) = named.get(path) {
            return Some(pcm);
        }
        let (mut file, pin) = open_named_spool(path).ok()?;
        let mut header = [0; CACHE_HEADER_BYTES];
        let length = file.metadata().ok()?.len();
        let expected = (frames as u64)
            .checked_mul(8)?
            .checked_add(CACHE_HEADER_BYTES as u64)?;
        let valid_header = match file.read_exact(&mut header) {
            Ok(()) => parse_header(&header) == Some((key.target_rate, frames as u64)),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
            Err(_) => return None,
        };
        if !valid_header || length != expected {
            drop(file);
            drop(pin);
            let _ = named.remove_unused(path);
            return None;
        }
        Some(named.register(path, file, pin, frames, CACHE_HEADER_BYTES as u64))
    }

    /// Insert a resident variant, evicting the oldest entries over the cap.
    fn insert(&self, key: StretchKey, samples: Arc<Vec<f32>>) -> Result<(), String> {
        let bytes = samples.len() * 4;
        let mut inner = self.lock_inner();
        if bytes > self.memory_cap {
            return Err(format!(
                "derived variant needs {:.1} MiB, above the {:.1} MiB derived-media budget",
                mib(bytes),
                mib(self.memory_cap)
            ));
        }
        if let Some(previous) = inner.entries.insert(key.clone(), samples) {
            inner.bytes = inner.bytes.saturating_sub(previous.len() * 4);
        }
        if let Some(index) = inner.lru.iter().position(|candidate| *candidate == key) {
            inner.lru.remove(index);
        }
        inner.lru.push_back(key);
        inner.bytes += bytes;
        while inner.bytes > self.memory_cap && inner.entries.len() > 1 {
            let Some(oldest) = inner.lru.pop_front() else {
                break;
            };
            if let Some(dropped) = inner.entries.remove(&oldest) {
                inner.bytes = inner.bytes.saturating_sub(dropped.len() * 4);
                inner.stats.evictions += 1;
            }
        }
        Ok(())
    }

    fn disk_path(&self, key: &StretchKey) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        Some(dir.join(key.file_name()))
    }

    fn read_disk(&self, key: &StretchKey) -> Option<Arc<Vec<f32>>> {
        if !cfg!(target_endian = "little") {
            return None;
        }
        let path = self.disk_path(key)?;
        if fs::metadata(&path).ok()?.len()
            > (self.memory_cap as u64).saturating_add(CACHE_HEADER_BYTES as u64)
        {
            return None;
        }
        let (sample_rate, samples) = match Self::read_cache_file(&path, self.memory_cap) {
            Some(loaded) => loaded,
            None => {
                if path.exists() {
                    let _ = named_spools().remove_unused(&path);
                }
                return None;
            }
        };
        if sample_rate != key.target_rate {
            let _ = named_spools().remove_unused(&path);
            return None;
        }
        self.stamp_access(&path);
        self.lock_inner().stats.disk_hits += 1;
        Some(Arc::new(samples))
    }

    fn read_cache_file(path: &Path, byte_limit: usize) -> Option<(u32, Vec<f32>)> {
        let file = File::open(path).ok()?;
        let file_length = file.metadata().ok()?.len();
        let mut reader = BufReader::new(file);
        let mut header = [0_u8; CACHE_HEADER_BYTES];
        reader.read_exact(&mut header).ok()?;
        let (sample_rate, frames) = parse_header(&header)?;
        let pcm_bytes = frames.checked_mul(2 * size_of::<f32>() as u64)?;
        if pcm_bytes > byte_limit as u64
            || file_length != (CACHE_HEADER_BYTES as u64).checked_add(pcm_bytes)?
        {
            return None;
        }
        let mut samples = vec![0_f32; usize::try_from(frames).ok()?.checked_mul(2)?];
        reader
            .read_exact(bytemuck::cast_slice_mut(&mut samples))
            .ok()?;
        Some((sample_rate, samples))
    }

    fn write_disk(
        &self,
        key: &StretchKey,
        samples: &[f32],
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
            || cancellation.is_cancelled()
        {
            return;
        }
        let Some(path) = self.disk_path(key) else {
            return;
        };
        if path.exists() {
            return;
        }
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let frames = samples.len() / 2;
        let bytes = (samples.len() as u64)
            .saturating_mul(4)
            .saturating_add(CACHE_HEADER_BYTES as u64);
        let Ok(publication) = self.scratch.reserve_on_volume(dir, bytes) else {
            return;
        };
        let write = persist_cache_file(
            publication.stage_pcm(),
            &path,
            |file| {
                file.write_all(&cache_header(key, frames))?;
                for block in samples.chunks(16_384) {
                    cancellation.check().map_err(std::io::Error::other)?;
                    file.write_all(bytemuck::cast_slice(block))?;
                }
                Ok(())
            },
            || {
                cancellation.check().ok()?;
                let local = self.generation.accept(generation)?;
                let directory =
                    accept_disk_cache_generation(dir, "ocd", disk_generation).ok()??;
                Some((local, directory))
            },
        );
        match write {
            Ok(true) => {
                if let Some(_commit) = self.generation.accept(generation) {
                    self.lock_inner().stats.disk_writes += 1;
                    self.evict_disk();
                }
            }
            Ok(false) => {}
            Err(error) if !cancellation.is_cancelled() => {
                log::warn!("derived cache: cannot write {}: {error}", path.display());
            }
            Err(_) => {}
        }
    }

    fn stamp_access(&self, path: &Path) {
        if let Ok(mut file) = OpenOptions::new().write(true).open(path) {
            let _ = file.seek(SeekFrom::Start(ACCESS_OFFSET));
            let _ = file.write_all(&now_nanos().to_le_bytes());
        }
    }

    /// Delete the oldest derived files until the on-disk total fits the cap.
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
                if !cache_file_name(&path, "ocd") || !entry.file_type().ok()?.is_file() {
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

    /// Drop resident entries and remove derived files with no active reader.
    pub fn clear(&self) {
        let _commit = self.generation.advance();
        let mut inner = self.lock_inner();
        inner.entries.clear();
        inner.lru.clear();
        inner.bytes = 0;
        inner.stats = DerivedStats::default();
        drop(inner);
        if let Some(dir) = &self.dir
            && (self.disk_cap > 0 || dir.is_dir())
            && let Err(error) = clear_disk_cache(dir, "ocd")
        {
            log::warn!("clip cache: cannot clear {}: {error}", dir.display());
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
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

fn cache_header(key: &StretchKey, frames: usize) -> [u8; CACHE_HEADER_BYTES] {
    let mut header = [0; CACHE_HEADER_BYTES];
    header[..4].copy_from_slice(CACHE_MAGIC);
    header[4] = CACHE_VERSION;
    header[6..14].copy_from_slice(&now_nanos().to_le_bytes());
    header[14..18].copy_from_slice(&key.target_rate.to_le_bytes());
    header[18..26].copy_from_slice(&(frames as u64).to_le_bytes());
    header
}

fn parse_header(header: &[u8; CACHE_HEADER_BYTES]) -> Option<(u32, u64)> {
    if &header[..4] != CACHE_MAGIC || header[4] != CACHE_VERSION {
        return None;
    }
    Some((
        u32::from_le_bytes(header[14..18].try_into().ok()?),
        u64::from_le_bytes(header[18..26].try_into().ok()?),
    ))
}

static SHARED: OnceLock<DerivedCache> = OnceLock::new();

/// Process-wide derived cache used by clip baking.
pub fn shared() -> &'static DerivedCache {
    SHARED.get_or_init(DerivedCache::from_env)
}

/// Install an explicit process cache before first use, or accept an identical
/// initialization. A conflicting configuration is reported instead of ignored.
/// Construct the cache with `from_env` when environment overrides must apply.
pub fn init_shared_cache(cache: DerivedCache) -> Result<&'static DerivedCache, String> {
    let dir = cache.dir.clone();
    let memory_cap = cache.memory_cap;
    let disk_cap = cache.disk_cap;
    let shared = SHARED.get_or_init(|| cache);
    if shared.dir != dir || shared.memory_cap != memory_cap || shared.disk_cap != disk_cap {
        return Err(
            "shared derived cache is already initialized with a different configuration".into(),
        );
    }
    Ok(shared)
}

/// Bytes a stretched variant of `media_frames` frames at `ratio` will occupy.
pub fn stretched_bytes(media_frames: usize, ratio: f64) -> usize {
    stretch::stretched_frames(media_frames, ratio) * 2 * std::mem::size_of::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_shared_cache_initialization_rejects_conflicting_settings() {
        let current = shared();
        let same = DerivedCache::new(current.dir.clone(), current.memory_cap, current.disk_cap);
        assert!(std::ptr::eq(init_shared_cache(same).unwrap(), current));
        let conflict = DerivedCache::new(
            current.dir.clone(),
            current.memory_cap ^ 1,
            current.disk_cap,
        );
        assert!(init_shared_cache(conflict).is_err());
        assert!(std::ptr::eq(shared(), current));
    }

    use std::time::Duration;

    fn temp_dir(name: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-derived-cache", name)
    }

    fn key(ratio: f64) -> StretchKey {
        StretchKey::new(
            MediaKey {
                path: PathBuf::from("/tmp/take.wav"),
                mtime: (1_700_000_000, 7),
                size: 4096,
            },
            48_000,
            ratio,
            StretchMode::Percussive,
            stretch::ALGO_VERSION,
        )
    }

    fn bake(frames: usize) -> Vec<f32> {
        vec![0.25; frames * 2]
    }

    #[test]
    fn cooperating_processes_bake_one_copy_and_leave_other_keys_runnable() {
        use std::process::{Command, Stdio};
        use std::time::Instant;

        const CHILD_DIR: &str = "OCP_TEST_SHARED_DERIVED_WORK";
        let variant = key(2.0);
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            let dir = PathBuf::from(dir);
            let cache = DerivedCache::new(Some(dir.clone()), 16 << 10, 80 << 10);
            let work_path = cache
                .disk_path(&variant)
                .unwrap()
                .with_extension("ocd.work");
            let work = OpenOptions::new()
                .read(true)
                .write(true)
                .open(work_path)
                .unwrap();
            assert!(matches!(
                work.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            drop(work);

            cache
                .get_or_bake(key(1.5), 8192, || Ok(bake(1024)))
                .unwrap();
            fs::write(dir.join("ready"), []).unwrap();
            let samples = cache
                .get_or_bake(variant, 8192, || panic!("duplicate bake"))
                .unwrap();
            assert_eq!(*samples, bake(1024));
            assert_eq!(cache.stats().disk_hits, 1);
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let mut worker = None;
        cache.get_or_bake(variant, 8192, || {
            worker = Some(Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "derived_cache::tests::cooperating_processes_bake_one_copy_and_leave_other_keys_runnable"])
                .env(CHILD_DIR, dir.path())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap());
            let child = worker.as_mut().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.path().join("ready").exists() {
                if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                    let mut child = worker.take().unwrap();
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!("the independent bake did not finish: {}", String::from_utf8_lossy(&output.stdout));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(bake(1024))
        }).unwrap();
        let child = worker.unwrap().wait_with_output().unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(fs::read_dir(dir.path()).unwrap().flatten().all(|entry| {
            entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "work")
        }));
    }

    #[test]
    fn a_clear_from_another_process_rejects_an_in_flight_bake() {
        const CHILD_DIR: &str = "OCP_TEST_CLEAR_DERIVED_DIR";
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            let cache = DerivedCache::new(Some(dir.into()), 16 << 10, 80 << 10);
            cache.clear();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let variant = key(2.0);
        let samples = cache
            .get_or_bake(variant.clone(), 8192, || {
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "derived_cache::tests::a_clear_from_another_process_rejects_an_in_flight_bake"])
                    .env(CHILD_DIR, dir.path())
                    .output()
                    .unwrap();
                assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stdout));
                Ok(bake(1024))
            })
            .unwrap();
        assert_eq!(*samples, bake(1024));
        assert_eq!(cache.disk_usage().cached_bytes, 0);
        let fresh = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        fresh.get_or_bake(variant, 8192, || Ok(bake(1024))).unwrap();
        assert!(fresh.disk_usage().cached_bytes > 0);
    }

    #[test]
    fn cancelled_derived_bake_releases_key_without_publishing() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let key = key(2.0);
        let cancelled = AtomicBool::new(false);
        let cancellation = ClipCancellation::new(&cancelled, &|| false);
        let result = cache.get_or_bake_cancellable(key.clone(), 8192, cancellation, || {
            cancelled.store(true, Ordering::Relaxed);
            Ok(bake(1024))
        });
        assert!(result.is_err());
        assert_eq!(cache.stats().bakes, 0);
        assert!(cache.lookup(&key).is_none());
        assert_eq!(cache.disk_usage().cached_bytes, 0);
        assert_eq!(cache.disk_usage().temporary_bytes, 0);
        assert_eq!(
            cache
                .get_or_bake(key, 8192, || Ok(bake(1024)))
                .unwrap()
                .len(),
            2048
        );
    }

    #[test]
    fn clearing_during_a_bake_keeps_its_result_out_of_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let key = key(2.0);
        let output = cache
            .get_or_bake(key.clone(), 8192, || {
                cache.clear();
                Ok(bake(1024))
            })
            .unwrap();
        assert_eq!(*output, bake(1024));
        assert_eq!(cache.memory_bytes(), 0);
        assert_eq!(cache.disk_usage().cached_bytes, 0);
        cache.get_or_bake(key, 8192, || Ok(bake(1024))).unwrap();
        assert!(cache.disk_usage().cached_bytes > 0);
    }

    #[test]
    fn scratch_is_reserved_before_a_derived_bake() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DerivedCache::new(Some(dir.path().to_path_buf()), 16 << 10, 80 << 10);
        let source = ClipPcm::Resident(Arc::new(bake(4096)));
        let plan = StretchPlan::stretched(2.0, StretchMode::Percussive);
        let blocked = cache.scratch.reserve(24 << 10).unwrap();
        let bake = || {
            cache.spool_stretch(
                dir.path(),
                key(2.0),
                &source,
                &plan,
                ResampleQuality::PRESERVED,
                ClipCancellation::default(),
            )
        };
        let error = bake().err().expect("scratch is full");
        assert!(error.scratch_busy, "{error}");
        assert_eq!(cache.stats().bakes, 0);
        assert_eq!(cache.disk_usage().temporary_bytes, 24 << 10);
        drop(blocked);
        let (_, pcm) = bake().unwrap();
        assert_eq!(pcm.frames(), 8192);
        let usage = cache.disk_usage();
        assert_eq!(usage.temporary_bytes, 0);
        assert_eq!(usage.cached_bytes, 8192 * 8 + CACHE_HEADER_BYTES as u64);
    }

    #[test]
    fn same_key_bakes_once() {
        let cache = DerivedCache::new(None, 64 * 1024 * 1024, 0);
        let first = cache
            .get_or_bake(key(1.5), 8 * 1024, || Ok(bake(1024)))
            .unwrap();
        let second = cache
            .get_or_bake(key(1.5), 8 * 1024, || panic!("must not bake twice"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.stats().bakes, 1);
        assert_eq!(cache.stats().memory_hits, 1);
        assert_eq!(cache.memory_bytes(), 1024 * 2 * 4);
    }

    #[test]
    fn ratio_mode_source_and_version_changes_miss() {
        let cache = DerivedCache::new(None, 64 * 1024 * 1024, 0);
        cache
            .get_or_bake(key(1.5), 8192, || Ok(bake(1024)))
            .unwrap();
        cache
            .get_or_bake(key(1.25), 8192, || Ok(bake(1024)))
            .unwrap();
        assert_eq!(cache.stats().bakes, 2, "a ratio change must bake again");

        let mut versioned = key(1.5);
        versioned.algo_version += 1;
        cache
            .get_or_bake(versioned, 8192, || Ok(bake(1024)))
            .unwrap();
        assert_eq!(
            cache.stats().bakes,
            3,
            "an algorithm version bump must not reuse the old variant"
        );

        let mut other_mode = key(1.5);
        other_mode.mode = StretchMode::Musical;
        cache
            .get_or_bake(other_mode, 8192, || Ok(bake(1024)))
            .unwrap();
        assert_eq!(cache.stats().bakes, 4);

        let mut changed_media = key(1.5);
        changed_media.media.mtime = (1_700_000_001, 7);
        cache
            .get_or_bake(changed_media, 8192, || Ok(bake(1024)))
            .unwrap();
        assert_eq!(
            cache.stats().bakes,
            5,
            "a changed source media version must bake again"
        );
    }

    #[test]
    fn disk_entry_survives_a_fresh_cache_instance() {
        let dir = temp_dir("disk");
        let cache_dir = dir.join("cache");
        let samples = {
            let cache = DerivedCache::new(Some(cache_dir.clone()), 64 * 1024 * 1024, 1 << 20);
            cache
                .get_or_bake(key(2.0), 8192, || Ok(bake(1024)))
                .unwrap()
        };
        assert_eq!(
            fs::read_dir(&cache_dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("ocd"))
                .count(),
            1
        );
        let reopened = DerivedCache::new(Some(cache_dir), 64 * 1024 * 1024, 1 << 20);
        let loaded = reopened
            .get_or_bake(key(2.0), 8192, || panic!("must read from disk"))
            .unwrap();
        assert_eq!(&*loaded, &*samples);
        assert_eq!(reopened.stats().disk_hits, 1);
        assert_eq!(reopened.stats().bakes, 0);
        reopened.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_disk_variant_is_refused_without_loading_or_discarding_it() {
        let dir = temp_dir("disk-budget");
        let key = key(2.0);
        let writer = DerivedCache::new(Some(dir.clone()), 8192, 1 << 20);
        writer
            .get_or_bake(key.clone(), 8192, || Ok(bake(1024)))
            .unwrap();
        let reader = DerivedCache::new(Some(dir.clone()), 4096, 1 << 20);
        assert!(
            reader
                .get_or_bake(key.clone(), 8192, || panic!("must not bake"))
                .is_err()
        );
        assert_eq!(reader.stats().disk_hits, 0);
        assert!(dir.join(key.file_name()).is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn variant_larger_than_the_budget_is_refused_before_baking() {
        let cache = DerivedCache::new(None, 4096, 0);
        let error = cache
            .get_or_bake(key(2.0), 8192, || panic!("must not bake over budget"))
            .unwrap_err();
        assert!(error.contains("derived-media budget"), "{error}");
        assert_eq!(cache.stats().bakes, 0);
        assert_eq!(cache.memory_bytes(), 0);
    }

    #[test]
    fn lru_keeps_resident_bytes_under_the_cap() {
        let cache = DerivedCache::new(None, 16 * 1024, 0);
        for ratio in [1.1, 1.2, 1.3] {
            cache
                .get_or_bake(key(ratio), 8192, || Ok(bake(1024)))
                .unwrap();
        }
        assert!(cache.memory_bytes() <= 16 * 1024);
        assert!(cache.stats().evictions >= 1);
        assert_eq!(cache.stats().bakes, 3);
    }

    #[test]
    fn cache_keeps_mixed_rate_bakes_separate_in_both_warm_orders() {
        let media = key(1.25).media;
        let source: Vec<f32> = (0..4_410)
            .flat_map(|frame| {
                let sample =
                    (frame as f64 * std::f64::consts::TAU * 19_000.0 / 44_100.0).sin() as f32;
                [sample, -sample]
            })
            .collect();
        let plan = StretchPlan::stretched(1.25, StretchMode::Percussive);
        let qualities = [ResampleQuality::FASTEST, ResampleQuality::BEST];
        let source_at = |quality| {
            Arc::new(crate::resample::resample(&source, 44_100, 48_000, quality).unwrap())
        };
        let reference =
            qualities.map(|quality| plan.bake(&source_at(quality), 48_000, quality).unwrap());
        assert_ne!(reference[0], reference[1]);
        for order in [[0, 1], [1, 0]] {
            let cache = DerivedCache::new(None, 1 << 20, 0);
            for index in order {
                let quality = qualities[index];
                let rendered = cache
                    .stretch(&media, source_at(quality), 48_000, &plan, quality)
                    .unwrap();
                assert_eq!(&*rendered, &reference[index]);
            }
            assert_eq!(cache.stats().bakes, 2);
        }
    }

    #[test]
    fn cached_and_fresh_stretch_are_bit_identical() {
        let cache = DerivedCache::new(None, 64 * 1024 * 1024, 0);
        let media = key(2.0).media;
        let mut source = vec![0.0_f32; 48_000 * 2];
        for frame in (0..48_000).step_by(4_800) {
            source[frame * 2] = 1.0;
            source[frame * 2 + 1] = 1.0;
        }
        let source = Arc::new(source);
        let plan = StretchPlan::stretched(2.0, StretchMode::Percussive);
        let render = ResampleQuality::DEFAULT_RENDER;
        let first = cache
            .stretch(&media, source.clone(), 48_000, &plan, render)
            .unwrap();
        assert_eq!(first.len() / 2, 96_000);
        let hit = cache
            .stretch(&media, source.clone(), 48_000, &plan, render)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        cache.clear();
        let fresh = cache
            .stretch(&media, source, 48_000, &plan, render)
            .unwrap();
        assert_eq!(first, fresh, "a cache hit must match a fresh bake");
    }

    #[test]
    fn a_variant_from_the_previous_algorithm_version_is_not_served() {
        let dir = temp_dir("stale");
        let cache_dir = dir.join("cache");
        let media = key(2.0).media;
        let source: Arc<Vec<f32>> = Arc::new({
            let mut samples = vec![0.0_f32; 48_000 * 2];
            for frame in (0..48_000).step_by(4_800) {
                samples[frame * 2] = 1.0;
                samples[frame * 2 + 1] = 1.0;
            }
            samples
        });
        // Store a variant under the previous version, as an old bake left it.
        let stale = StretchKey::new(
            media.clone(),
            48_000,
            2.0,
            StretchMode::Musical,
            stretch::ALGO_VERSION - 1,
        );
        let old = {
            let cache = DerivedCache::new(Some(cache_dir.clone()), 64 * 1024 * 1024, 16 << 20);
            cache
                .get_or_bake(stale.clone(), 1 << 20, || stretch::wsola(&source, 2.0))
                .unwrap()
        };
        assert!(cache_dir.join(stale.file_name()).exists());

        // A cold cache asking for the current version must bake, not read the old file.
        let fresh = DerivedCache::new(Some(cache_dir.clone()), 64 * 1024 * 1024, 16 << 20);
        let samples = fresh
            .stretch(
                &media,
                source.clone(),
                48_000,
                &StretchPlan::stretched(2.0, StretchMode::Musical),
                ResampleQuality::DEFAULT_RENDER,
            )
            .unwrap();
        assert_eq!(
            fresh.stats().disk_hits,
            0,
            "a stale .ocd must not be served"
        );
        assert_eq!(fresh.stats().bakes, 1);
        assert_eq!(fresh.stats().disk_writes, 1);
        let current = StretchKey::new(
            media,
            48_000,
            2.0,
            StretchMode::Musical,
            stretch::ALGO_VERSION,
        );
        assert_ne!(current.file_name(), stale.file_name());
        assert!(cache_dir.join(current.file_name()).exists());
        assert!(cache_dir.join(stale.file_name()).exists());
        assert_ne!(
            &*samples, &*old,
            "the musical path must not reuse a WSOLA bake"
        );
        fresh.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_musical_variant_round_trips_through_the_disk() {
        let dir = temp_dir("musical");
        let cache_dir = dir.join("cache");
        let media = key(1.5).media;
        let mut source = vec![0.0_f32; 48_000 * 2];
        for frame in (0..48_000).step_by(2_400) {
            source[frame * 2] = 0.5;
            source[frame * 2 + 1] = 0.5;
        }
        let source = Arc::new(source);
        let baked = {
            let cache = DerivedCache::new(Some(cache_dir.clone()), 64 * 1024 * 1024, 1 << 20);
            cache
                .stretch(
                    &media,
                    source.clone(),
                    48_000,
                    &StretchPlan::stretched(1.5, StretchMode::Musical),
                    ResampleQuality::DEFAULT_RENDER,
                )
                .unwrap()
        };
        let reopened = DerivedCache::new(Some(cache_dir), 64 * 1024 * 1024, 1 << 20);
        let loaded = reopened
            .stretch(
                &media,
                source,
                48_000,
                &StretchPlan::stretched(1.5, StretchMode::Musical),
                ResampleQuality::DEFAULT_RENDER,
            )
            .unwrap();
        assert_eq!(reopened.stats().disk_hits, 1);
        assert_eq!(baked, loaded);
        reopened.clear();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disk_cap_evicts_old_derived_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let cache = DerivedCache::new(Some(cache_dir), 64 * 1024 * 1024, 12 << 10);
        let mut cached = Vec::new();
        for ratio in [1.1, 1.2] {
            let key = key(ratio);
            cache
                .get_or_bake(key.clone(), 8192, || Ok(bake(1024)))
                .unwrap();
            let cache_path = cache.disk_path(&key).unwrap();
            assert!(cache_path.exists());
            cached.push(cache_path);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!cached[0].exists(), "the oldest file must be evicted");
        assert!(cached[1].exists());
    }
}
