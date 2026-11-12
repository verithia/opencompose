//! Control-plane SF3 preparation with the complete bank's logical addresses.
use std::collections::VecDeque;
use std::sync::Mutex;

use super::*;

mod input;

const MAX_LOGICAL_BYTES: usize = 512 * 1024 * 1024;

pub(in crate::sf2) struct PreparedFont {
    pub font: Arc<SoundFont>,
    /// None is the unchanged complete-bank fallback, not an empty admission set.
    pub admitted: Option<Box<[bool]>>,
}

/// Prepared banks reused when a rebuild admits the same programs. Each entry
/// holds a whole logical waveform, so only the newest few stay resident.
const PREPARED_RESIDENT: usize = 2;

struct PreparedEntry {
    key: PathBuf,
    identity: BankIdentity,
    admitted: Vec<bool>,
    font: Arc<SoundFont>,
}

fn prepared_banks() -> &'static Mutex<VecDeque<PreparedEntry>> {
    static BANKS: std::sync::OnceLock<Mutex<VecDeque<PreparedEntry>>> = std::sync::OnceLock::new();
    BANKS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Reuse the bank a previous load prepared for the same programs.
///
/// A rebuild usually admits exactly the presets it admitted before, so the
/// admission table is the cache key: a hit skips the file read, the Ogg page
/// index and the Vorbis decode, which is the expensive part of a SoundFont
/// load. A different program set needs a different decoded subset, so it falls
/// through to a fresh preparation.
fn reuse_prepared(
    path: &Path,
    select: &impl Fn(Arc<SoundFont>) -> Result<Vec<bool>, String>,
) -> Result<Option<PreparedFont>, String> {
    let key = cache_key(path);
    let Some(identity) = file_identity(&key) else {
        return Ok(None);
    };
    let candidate = {
        let cache = prepared_banks()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache
            .iter()
            .find(|entry| entry.key == key && entry.identity == identity)
            .map(|entry| (Arc::clone(&entry.font), entry.admitted.clone()))
    };
    let Some((font, cached)) = candidate else {
        return Ok(None);
    };
    let admitted = select(Arc::clone(&font))?;
    if admitted != cached {
        return Ok(None);
    }
    Ok(Some(PreparedFont {
        font,
        admitted: Some(admitted.into_boxed_slice()),
    }))
}

fn remember_prepared(
    key: PathBuf,
    identity: BankIdentity,
    admitted: &[bool],
    font: &Arc<SoundFont>,
) {
    let mut cache = prepared_banks()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache.retain(|entry| {
        !(entry.key == key && entry.identity == identity && entry.admitted == admitted)
    });
    cache.push_back(PreparedEntry {
        key,
        identity,
        admitted: admitted.to_vec(),
        font: Arc::clone(font),
    });
    while cache.len() > PREPARED_RESIDENT {
        cache.pop_front();
    }
}

pub(in crate::sf2) fn prepare_streaming(
    path: &Path,
    workers: crate::render_parallel::RenderWorkerOptions,
    select: impl Fn(Arc<SoundFont>) -> Result<Vec<bool>, String>,
) -> Result<PreparedFont, String> {
    if let Some(font) = SoundFont::ready_shared(path) {
        font.check_preparation_limit(MAX_LOGICAL_BYTES)
            .map_err(|error| error.to_string())?;
        return Ok(PreparedFont {
            font,
            admitted: None,
        });
    }
    if let Some(prepared) = reuse_prepared(path, &select)? {
        return Ok(prepared);
    }
    let started = std::time::Instant::now();
    let input =
        input::BankSnapshot::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    log::debug!(
        "phase=read milliseconds={:.3}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    crate::sf2::progress::report("read", 0, 0);
    let indexed_at = std::time::Instant::now();
    let raw = RawBank::parse(&input.bytes).map_err(|error| error.to_string())?;
    let indexed = match index(raw, workers, MAX_LOGICAL_BYTES) {
        Ok(indexed) => indexed,
        Err(IndexFailure::Unindexable(reason)) => return complete_fallback(&input, reason),
        Err(error @ IndexFailure::LogicalLimit { .. }) => return Err(error.to_string()),
    };
    let Indexed {
        mut font,
        original,
        smpl,
    } = indexed.allocate()?;
    log::debug!(
        "phase=index milliseconds={:.3}",
        indexed_at.elapsed().as_secs_f64() * 1000.0
    );
    crate::sf2::progress::report("index", 0, 0);
    let selected_at = std::time::Instant::now();
    let admitted = select(Arc::clone(&font))?;
    log::debug!(
        "phase=program_plan milliseconds={:.3}",
        selected_at.elapsed().as_secs_f64() * 1000.0
    );
    if admitted.len() != font.presets.len() {
        return Err("SoundFont preparation returned an invalid preset admission table".into());
    }
    let mut ranges: Vec<(u64, u64)> = font
        .presets
        .iter()
        .zip(&admitted)
        .filter(|(_, admitted)| **admitted)
        .flat_map(|(preset, _)| preset.zones.iter())
        // Linear interpolation can read one point beyond the zone's final
        // played point. Preserve that neighbor even across sample boundaries.
        .map(|zone| {
            (
                zone.start,
                zone.end
                    .saturating_add(1)
                    .min(font.sample_bank.frames() as u64),
            )
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let decoded_at = std::time::Instant::now();
    let mut selected = Vec::new();
    let mut range_index = 0;
    for (index, header) in font.sample_headers.iter().enumerate() {
        while range_index < merged.len() && merged[range_index].1 <= u64::from(header.start) {
            range_index += 1;
        }
        if range_index == merged.len() {
            break;
        }
        if merged[range_index].0 < u64::from(header.end) {
            selected.push(index);
        }
    }
    // Results retain source order independent of completion order. Each producer
    // is capped before allocation/append at its indexed span; all retained PCM
    // buffers together are bounded by the selected logical waveform's size.
    let decoded = parallel_map(selected.len(), workers, |job| {
        let index = selected[job];
        let header = &font.sample_headers[index];
        let sample = decode_sample(
            smpl,
            &original[index],
            Some((header.end - header.start) as usize),
        );
        crate::sf2::progress::report("decode", job + 1, selected.len());
        sample
    })?;
    let ready = Arc::get_mut(&mut font)
        .ok_or("SoundFont preparation retained an unexpected mutable-bank observer")?;
    let SampleBank::Points16(points) = &mut ready.sample_bank else {
        return Err("indexed SF3 preparation requires 16-bit sample points".into());
    };
    let decoded_count = selected.len();
    for (index, sample) in selected.into_iter().zip(decoded) {
        let header = &ready.sample_headers[index];
        let sample = match sample {
            Ok(sample) => sample,
            // The index is a duration guess from the bank header. A stream
            // that decodes past it proves the guess wrong, so take the
            // complete parse instead of refusing the whole bank.
            Err(SampleDecodeError::Limit) => {
                return complete_fallback(
                    &input,
                    format!("sample '{}' streams past its indexed span", header.name),
                );
            }
            Err(reason) => return Err(format!("sample '{}': {reason}", header.name)),
        };
        if sample.points.len() != (header.end - header.start) as usize {
            // A duration guess must never move neighboring samples or leave an
            // admitted zone backed by placeholders. Use the original full path.
            return complete_fallback(
                &input,
                format!(
                    "sample '{}' decoded {} points, indexed {}",
                    header.name,
                    sample.points.len(),
                    header.end - header.start,
                ),
            );
        }
        points[header.start as usize..header.end as usize].copy_from_slice(&sample.points);
        if let Some(warning) = sample.warning {
            ready.warnings.push(warning);
        }
    }
    log::debug!(
        "phase=sample_decode milliseconds={:.3}",
        decoded_at.elapsed().as_secs_f64() * 1000.0
    );
    log::debug!(
        "[soundfont] streaming preparation decoded {decoded_count}/{} samples; logical_bytes={}",
        original.len(),
        ready.sample_bank.frames() * size_of::<i16>(),
    );
    ready.warnings.push(format!(
        "prepared {decoded_count} of {} SF3 samples for admitted streaming programs",
        original.len(),
    ));
    // Keep this preparation for the next rebuild that admits the same programs.
    remember_prepared(input.key.clone(), input.identity.clone(), &admitted, &font);
    Ok(PreparedFont {
        font,
        admitted: Some(admitted.into_boxed_slice()),
    })
}

fn complete_fallback(
    input: &input::BankSnapshot,
    reason: impl std::fmt::Display,
) -> Result<PreparedFont, String> {
    log::debug!("[soundfont] using complete bank preparation: {reason}");
    Ok(PreparedFont {
        font: input.full_font(MAX_LOGICAL_BYTES)?,
        admitted: None,
    })
}

struct Indexed<'a> {
    font: Arc<SoundFont>,
    original: Vec<SampleHeader>,
    smpl: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
enum IndexFailure {
    #[error("{0}")]
    Unindexable(String),
    #[error("SF3 logical waveform exceeds the {limit}-byte streaming preparation limit")]
    LogicalLimit { limit: usize },
}
impl From<String> for IndexFailure {
    fn from(reason: String) -> Self {
        Self::Unindexable(reason)
    }
}
impl From<&str> for IndexFailure {
    fn from(reason: &str) -> Self {
        Self::Unindexable(reason.into())
    }
}

fn index(
    raw: RawBank<'_>,
    workers: crate::render_parallel::RenderWorkerOptions,
    limit: usize,
) -> Result<IndexPlan<'_>, IndexFailure> {
    let mut smpl = None;
    for chunk in ChunkReader::list(raw.sdta.data, b"sdta").map_err(|error| error.to_string())? {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if &chunk.id == b"smpl" {
            smpl = Some(chunk.data);
        }
    }
    let smpl = smpl.ok_or("bank has no sample data")?;
    let original = raw.region.sample_headers.clone();
    if !original.iter().any(|header| {
        smpl.get(header.start as usize..)
            .is_some_and(vorbis::is_ogg)
    }) {
        return Err("uncompressed banks use the complete shared loader".into());
    }
    let counts = parallel_map(original.len(), workers, |index| {
        let header = &original[index];
        let region = smpl
            .get(header.start as usize..header.end as usize)
            .ok_or_else(|| {
                format!(
                    "sample '{}' has invalid compressed byte bounds",
                    header.name
                )
            })?;
        if vorbis::is_ogg(region) {
            vorbis::indexed_frames(region)
                .map_err(|reason| format!("sample '{}': {reason}", header.name))
        } else {
            Ok(region.len() / 2)
        }
    })?;
    let mut rewritten = Vec::with_capacity(original.len());
    let mut frames = 0_usize;
    for (header, count) in original.iter().zip(counts) {
        let count = count?;
        let start = frames;
        frames = frames
            .checked_add(count)
            .ok_or(IndexFailure::LogicalLimit { limit })?;
        if frames > u32::MAX as usize || frames > limit / size_of::<i16>() {
            return Err(IndexFailure::LogicalLimit { limit });
        }
        rewritten.push(SampleHeader {
            start: start as u32,
            end: frames as u32,
            loop_start: start.saturating_add(header.loop_start as usize).min(frames) as u32,
            loop_end: start.saturating_add(header.loop_end as usize).min(frames) as u32,
            ..header.clone()
        });
    }
    if frames == 0 {
        return Err("SF3 logical waveform has no points".into());
    }
    Ok(IndexPlan {
        raw,
        original,
        rewritten,
        smpl,
        frames,
    })
}

struct IndexPlan<'a> {
    raw: RawBank<'a>,
    original: Vec<SampleHeader>,
    rewritten: Vec<SampleHeader>,
    smpl: &'a [u8],
    frames: usize,
}

impl<'a> IndexPlan<'a> {
    fn allocate(self) -> Result<Indexed<'a>, String> {
        let Self {
            raw,
            original,
            rewritten,
            smpl,
            frames,
        } = self;
        let layout = std::alloc::Layout::array::<i16>(frames).map_err(|error| error.to_string())?;
        // SAFETY: layout is a nonzero, bounded i16 array allocation. Zero is a valid
        // i16 representation. Ownership transfers immediately to Vec with identical
        // size/alignment/capacity; no Rust loop touches pages for unrelated samples.
        let points = unsafe {
            let pointer = std::alloc::alloc_zeroed(layout).cast::<i16>();
            if pointer.is_null() {
                return Err("cannot allocate the SF3 logical streaming waveform".into());
            }
            Vec::from_raw_parts(pointer, frames, frames)
        };
        let font = raw
            .finish(SampleBank::Points16(points), rewritten, true)
            .map_err(|error| error.to_string())?;
        Ok(Indexed {
            font: Arc::new(font),
            original,
            smpl,
        })
    }
}

/// Control-plane preparation only. Worker count comes from the owning request,
/// capped at eight, and no worker outlives this finite source-ordered operation.
fn parallel_map<T: Send>(
    count: usize,
    options: crate::render_parallel::RenderWorkerOptions,
    operation: impl Fn(usize) -> T + Sync,
) -> Result<Vec<T>, String> {
    let workers = options.workers().min(8).min(count);
    if workers < 2 {
        return Ok((0..count).map(operation).collect());
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        let mut failure = None;
        for _ in 0..workers {
            let next = &next;
            let operation = &operation;
            match std::thread::Builder::new()
                .name("sf3-prepare".into())
                .spawn_scoped(scope, move || {
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if index >= count {
                            break;
                        }
                        results.push((index, operation(index)));
                    }
                    results
                }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    failure = Some(format!("cannot start SF3 preparation worker: {error}"));
                    break;
                }
            }
        }
        let mut ordered = Vec::with_capacity(count);
        // Join every admitted worker even after a spawn/join failure. No
        // detached scoped handle can rethrow during scope's implicit cleanup.
        for handle in handles {
            match handle.join() {
                Ok(results) => ordered.extend(results),
                Err(_) => {
                    failure.get_or_insert_with(|| "SF3 preparation worker panicked".into());
                }
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        ordered.sort_unstable_by_key(|(index, _)| *index);
        Ok(ordered.into_iter().map(|(_, value)| value).collect())
    })
}
