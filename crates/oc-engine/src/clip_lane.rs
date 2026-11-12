//! Streaming audio-clip lanes: preloaded placements mixed into their track.
//! Loading decodes and bakes through the media caches. Render workers mix
//! resident or disk-backed PCM into fixed-size playback chunks.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use oc_dsl::ast::{AudioClip, GainEnvelopePoint, Project};

use crate::clip_pcm::{ClipCancellation, ClipPcm, ClipPreparationError, PlaybackPin};
use crate::derived_cache::{DerivedCache, StretchKey};
use crate::media_cache::{MediaCache, MediaKey};
use crate::stretch;

/// One clip pinned to the media version read when the project was loaded.
pub struct ClipLane {
    name: String,
    /// Project track index whose buffer receives this clip.
    track: usize,
    /// Absolute start frame and playable length at the project rate.
    start: u64,
    frames: usize,
    /// Source frame skipped from the project-rate media.
    offset: usize,
    /// Frames repeated by the loop region; zero plays the media once.
    loop_frames: usize,
    gain: f32,
    fade_in: usize,
    fade_out: usize,
    crossfade: usize,
    /// Optional point envelope over the clip length.
    points: Vec<GainEnvelopePoint>,
    /// Project-rate interleaved stereo owned for the whole project load.
    samples: ClipPcm,
    playback_pin: Option<Arc<PlaybackPin>>,
    media: Option<MediaKey>,
    /// A muted take keeps its identity here but contributes no samples.
    muted: bool,
}

impl ClipLane {
    /// Prepare one placement over already decoded (and possibly baked) media.
    pub(crate) fn from_samples(
        project: &Project,
        clip: &AudioClip,
        track: usize,
        sample_rate: u32,
        media: MediaKey,
        samples: ClipPcm,
    ) -> Result<Self, String> {
        let (start, length) = crate::audio_clip::range_frames(project, clip, sample_rate)
            .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
        if !clip.gain_db.is_finite()
            || !clip.offset_seconds.is_finite()
            || !clip.fade_in_seconds.is_finite()
            || !clip.fade_out_seconds.is_finite()
            || !clip.crossfade_seconds.is_finite()
            || clip.crossfade_seconds < 0.0
            || !clip.loop_seconds.is_finite()
            || clip.loop_seconds < 0.0
        {
            return Err(format!(
                "audio clip '{}': non-finite or negative gain, offset, fade, crossfade or loop",
                clip.name
            ));
        }
        let media_frames = samples.frames();
        let offset = (clip.offset_seconds * f64::from(sample_rate)).round() as usize;
        let loop_frames = crate::audio_clip::loop_frames(clip, sample_rate, media_frames, offset);
        // A looped lane fills the clip's length; a single pass stops at the media end.
        let playable = if loop_frames > 0 {
            length
        } else {
            length.min(media_frames.saturating_sub(offset) as u64)
        };
        Ok(Self {
            name: clip.name.clone(),
            track,
            start,
            frames: playable as usize,
            offset,
            loop_frames,
            gain: 10.0_f32.powf(clip.gain_db as f32 / 20.0) * if clip.invert { -1.0 } else { 1.0 },
            fade_in: (clip.fade_in_seconds * f64::from(sample_rate)).round() as usize,
            fade_out: (clip.fade_out_seconds * f64::from(sample_rate)).round() as usize,
            crossfade: (clip.crossfade_seconds * f64::from(sample_rate)).round() as usize,
            points: clip.gain_envelope.clone(),
            samples,
            playback_pin: None,
            media: Some(media),
            muted: false,
        })
    }

    /// A muted clip's lane: it keeps its index and name but mixes nothing.
    fn muted(clip: &AudioClip, track: usize) -> Self {
        Self {
            name: clip.name.clone(),
            track,
            start: 0,
            frames: 0,
            offset: 0,
            loop_frames: 0,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            crossfade: 0,
            points: Vec::new(),
            samples: ClipPcm::Resident(Arc::new(Vec::new())),
            playback_pin: None,
            media: None,
            muted: true,
        }
    }

    /// First absolute frame this clip can write.
    pub fn start_frame(&self) -> u64 {
        self.start
    }

    /// Playable length in frames, after offset and media-end clipping.
    pub fn length_frames(&self) -> usize {
        self.frames
    }

    /// Add the clip's contribution to a chunk starting at `chunk_start`.
    ///
    /// The destination is interleaved stereo. Disk-backed variants are read
    /// by the render worker before its output reaches the device ring.
    pub fn mix_into(&self, chunk_start: u64, output: &mut [f32]) -> Result<(), String> {
        if self.muted || self.frames == 0 || !output.len().is_multiple_of(2) {
            return Ok(());
        }
        let chunk_end = chunk_start.saturating_add((output.len() / 2) as u64);
        let first = self.start.max(chunk_start);
        let last = self.start.saturating_add(self.frames as u64).min(chunk_end);
        if first >= last {
            return Ok(());
        }
        let range = (first - self.start) as usize..(last - self.start) as usize;
        self.samples
            .visit_placement(self.offset, self.loop_frames, range, |start, samples| {
                for (index, sample) in samples.chunks_exact(2).enumerate() {
                    let frame = start + index;
                    let envelope = crate::audio_clip::clip_gain(
                        self.gain,
                        self.fade_in,
                        self.fade_out,
                        self.crossfade,
                        frame,
                        self.frames,
                        &self.points,
                    );
                    let destination = ((self.start + frame as u64 - chunk_start) as usize) * 2;
                    output[destination] += sample[0] * envelope;
                    output[destination + 1] += sample[1] * envelope;
                }
            })
            .map_err(|error| format!("Audio clip '{}': {error}", self.name))
    }
}

/// Every clip lane for one loaded project, in document order.
#[derive(Default)]
pub struct ClipLanes {
    lanes: Vec<ClipLane>,
    loading: Option<PendingClips>,
}

struct PendingClips {
    ranges: Vec<Option<(u64, u64)>>,
    failures: Vec<(u64, u64, String)>,
    receiver: mpsc::Receiver<(usize, Result<ClipLane, String>)>,
    cursor: Arc<AtomicU64>,
    demanded_clip: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
}

struct ClipJob {
    index: usize,
    start: u64,
    end: u64,
    clip: AudioClip,
    retry_at: Instant,
    retry_until: Option<Instant>,
}

const NO_CLIP_DEMAND: usize = usize::MAX;

fn clip_priority(cursor: u64, start: u64, end: u64, index: usize) -> (bool, u64, usize) {
    (end <= cursor, start.saturating_sub(cursor), index)
}

impl Drop for PendingClips {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl ClipLanes {
    /// Resolve and preload every clip through the shared media caches.
    pub fn load(project: &Project, sample_rate: u32, root: Option<&Path>) -> Result<Self, String> {
        Self::load_with_caches(
            crate::media_cache::shared(),
            crate::derived_cache::shared(),
            project,
            sample_rate,
            root,
        )
    }

    /// Preload every clip. If unique processed variants exceed the memory
    /// budget, spill them to bounded temporary storage on the render worker.
    pub fn load_with_caches(
        cache: &MediaCache,
        derived: &DerivedCache,
        project: &Project,
        sample_rate: u32,
        root: Option<&Path>,
    ) -> Result<Self, String> {
        let lanes = Self::load_selection(
            cache,
            derived,
            project,
            sample_rate,
            root,
            &project.audio_clips(),
            ClipCancellation::default(),
        )?;
        Ok(Self {
            lanes,
            loading: None,
        })
    }

    /// Prepare media independently of the render worker. Chunks must pass
    /// `ready_for` before processing so pending media cannot advance DSP.
    pub fn load_progressively(
        project: &Project,
        sample_rate: u32,
        root: Option<&Path>,
        cursor: u64,
    ) -> Result<Self, String> {
        let root = root.map(Path::to_path_buf);
        Self::load_progressively_using(
            project,
            sample_rate,
            cursor,
            move |project, clip, cancellation| {
                Self::load_selection(
                    crate::media_cache::shared(),
                    crate::derived_cache::shared(),
                    project,
                    sample_rate,
                    root.as_deref(),
                    &[clip],
                    cancellation,
                )
                .map(|mut lanes| lanes.remove(0))
            },
        )
    }

    fn load_progressively_using(
        project: &Project,
        sample_rate: u32,
        cursor: u64,
        mut prepare: impl FnMut(
            &Project,
            &AudioClip,
            ClipCancellation<'_>,
        ) -> Result<ClipLane, ClipPreparationError>
        + Send
        + 'static,
    ) -> Result<Self, String> {
        let clips = project.audio_clips();
        let mut ranges = Vec::with_capacity(clips.len());
        let mut lanes = Vec::with_capacity(clips.len());
        let mut jobs = Vec::new();
        let queued_at = Instant::now();
        for (index, clip) in clips.into_iter().enumerate() {
            let track = Self::destination(project, clip)?;
            lanes.push(ClipLane::muted(clip, track));
            let range = if clip.muted {
                None
            } else {
                let (start, len) = crate::audio_clip::range_frames(project, clip, sample_rate)
                    .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
                let end = start.saturating_add(len);
                jobs.push(ClipJob {
                    index,
                    start,
                    end,
                    clip: clip.clone(),
                    retry_at: queued_at,
                    retry_until: None,
                });
                Some((start, end))
            };
            ranges.push(range);
        }
        if jobs.is_empty() {
            return Ok(Self {
                lanes,
                loading: None,
            });
        }

        let (sender, receiver) = mpsc::channel();
        let cursor = Arc::new(AtomicU64::new(cursor));
        let demanded_clip = Arc::new(AtomicUsize::new(NO_CLIP_DEMAND));
        let cancelled = Arc::new(AtomicBool::new(false));
        let loading = PendingClips {
            ranges,
            failures: Vec::new(),
            receiver,
            cursor: Arc::clone(&cursor),
            demanded_clip: Arc::clone(&demanded_clip),
            cancelled: Arc::clone(&cancelled),
        };
        let project = project.clone();
        std::thread::Builder::new()
            .name("clip-media".into())
            .spawn(move || {
                while !jobs.is_empty() && !cancelled.load(Ordering::Relaxed) {
                    let demand = demanded_clip.load(Ordering::Relaxed);
                    let cursor = cursor.load(Ordering::Relaxed);
                    let now = Instant::now();
                    let next = jobs
                        .iter()
                        .enumerate()
                        .filter(|(_, job)| job.retry_at <= now)
                        .min_by_key(|(_, job)| {
                            (
                                job.index != demand,
                                clip_priority(cursor, job.start, job.end, job.index),
                            )
                        })
                        .map(|(position, _)| position);
                    let Some(next) = next else {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    };
                    let mut job = jobs.swap_remove(next);
                    let interrupted = Cell::new(false);
                    let preempted = || {
                        if interrupted.get() {
                            return true;
                        }
                        let next_demand = demanded_clip.load(Ordering::Relaxed);
                        // A scratch retry that is cooling down can still yield to work
                        // on another clip. An unchanged demand cannot interrupt it.
                        let should_yield = next_demand != demand
                            && next_demand != NO_CLIP_DEMAND
                            && next_demand != job.index
                            && jobs.iter().any(|queued| {
                                queued.index == next_demand && queued.retry_at <= Instant::now()
                            });
                        interrupted.set(should_yield);
                        should_yield
                    };
                    let result = prepare(
                        &project,
                        &job.clip,
                        ClipCancellation::new(&cancelled, &preempted),
                    );
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    if result.is_err() && interrupted.get() {
                        jobs.push(job);
                        continue;
                    }
                    if result.as_ref().is_err_and(|error| error.scratch_busy) {
                        let now = Instant::now();
                        let until = *job.retry_until.get_or_insert(now + Duration::from_secs(2));
                        if now < until {
                            job.retry_at = (now + Duration::from_millis(100)).min(until);
                            jobs.push(job);
                            continue;
                        }
                    }
                    if sender
                        .send((job.index, result.map_err(String::from)))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Could not start audio clip preparation: {error}"))?;
        Ok(Self {
            lanes,
            loading: Some(loading),
        })
    }

    /// Install completed lanes and check whether this half-open DSP range
    /// can render. Failed media is reported when its range is reached.
    pub fn ready_for(&mut self, start: u64, end: u64) -> Result<bool, String> {
        self.prepare_range(start, end, false)
    }

    /// Advance seek history with failed clips silent. A pending clip still
    /// blocks, and a failed clip remains an error during audible playback.
    pub fn ready_for_seek_history(&mut self, start: u64, end: u64) -> Result<bool, String> {
        self.prepare_range(start, end, true)
    }

    fn prepare_range(&mut self, start: u64, end: u64, seek_history: bool) -> Result<bool, String> {
        let Some(loading) = &mut self.loading else {
            return Ok(true);
        };
        loop {
            match loading.receiver.try_recv() {
                Ok((index, lane)) => {
                    let Some((start, end)) = loading.ranges[index].take() else {
                        continue;
                    };
                    match lane {
                        Ok(lane) => self.lanes[index] = lane,
                        Err(error) => loading.failures.push((start, end, error)),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    for (index, range) in loading.ranges.iter_mut().enumerate() {
                        if let Some((start, end)) = range.take() {
                            let name = &self.lanes[index].name;
                            loading.failures.push((
                                start,
                                end,
                                format!(
                                    "Audio clip '{name}': media preparation ended unexpectedly"
                                ),
                            ));
                        }
                    }
                    if loading.failures.is_empty() {
                        self.loading = None;
                        return Ok(true);
                    }
                    break;
                }
            }
        }
        let intersects = |(from, to)| start < to && from < end;
        if !seek_history
            && let Some((_, _, error)) = loading
                .failures
                .iter()
                .find(|(from, to, _)| intersects((*from, *to)))
        {
            return Err(error.clone());
        }
        let demand = loading
            .ranges
            .iter()
            .enumerate()
            .filter_map(|(index, range)| range.map(|(from, to)| (index, from, to)))
            .filter(|&(_, from, to)| intersects((from, to)))
            .min_by_key(|&(index, from, to)| clip_priority(start, from, to, index))
            .map_or(NO_CLIP_DEMAND, |(index, _, _)| index);
        loading.cursor.store(start, Ordering::Relaxed);
        loading.demanded_clip.store(demand, Ordering::Relaxed);
        Ok(demand == NO_CLIP_DEMAND)
    }

    fn destination(project: &Project, clip: &AudioClip) -> Result<usize, String> {
        project
            .tracks
            .iter()
            .position(|track| track.name == clip.track)
            .ok_or_else(|| {
                format!(
                    "audio clip '{}': unknown destination track '{}'",
                    clip.name, clip.track
                )
            })
    }

    fn load_selection(
        cache: &MediaCache,
        derived: &DerivedCache,
        project: &Project,
        sample_rate: u32,
        root: Option<&Path>,
        clips: &[&AudioClip],
        cancellation: ClipCancellation<'_>,
    ) -> Result<Vec<ClipLane>, ClipPreparationError> {
        let quality = crate::resample::live_quality(project);
        enum Prepared {
            Muted {
                track: usize,
            },
            Loaded {
                track: usize,
                media: MediaKey,
                samples: ClipPcm,
                plan: stretch::StretchPlan,
                source_pin: Option<Arc<PlaybackPin>>,
            },
        }
        let mut prepared = Vec::with_capacity(clips.len());
        let mut required = 0_usize;
        let mut requires_disk = 0_usize;
        let mut unique_variants = HashSet::new();
        for clip in clips {
            cancellation.check()?;
            let track = Self::destination(project, clip)?;
            // A muted clip is skipped exactly like an absent one: no decode, no
            // bake and no memory reservation.
            if clip.muted {
                prepared.push(Prepared::Muted { track });
                continue;
            }
            let (media, samples) = crate::audio_clip::project_rate_pcm(
                cache,
                clip,
                sample_rate,
                quality,
                root,
                cancellation,
            )?;
            let plan = stretch::resolve(project, clip, sample_rate, samples.frames())?;
            if !plan.is_bypass()
                && unique_variants.insert(StretchKey::for_plan(&media, sample_rate, &plan, quality))
            {
                let frames = plan.output_frames(samples.frames(), sample_rate);
                let bytes = frames.saturating_mul(2 * size_of::<f32>());
                if matches!(samples, ClipPcm::Spooled(_)) {
                    requires_disk = requires_disk.saturating_add(bytes);
                } else {
                    required = required.saturating_add(bytes);
                }
            }
            let source_pin = if plan.is_bypass() {
                cache
                    .pin_playback(&samples, cancellation)
                    .map_err(|error| error.context(format!("Audio clip '{}'", clip.name)))?
            } else {
                None
            };
            let samples = source_pin.as_ref().map_or(samples, |pin| pin.pcm.clone());
            prepared.push(Prepared::Loaded {
                track,
                media,
                samples,
                plan,
                source_pin,
            });
        }
        let spill_resident = required > derived.memory_cap();
        let disk_bytes = requires_disk.saturating_add(if spill_resident { required } else { 0 });
        let spool_dir = if disk_bytes > 0 {
            Some(derived.spool_dir(disk_bytes).ok_or_else(|| format!(
                "audio clips need {:.1} MiB of spooled media, above the available disk capacity for the {:.1} MiB derived-media budget",
                disk_bytes as f64 / (1024.0 * 1024.0),
                derived.memory_cap() as f64 / (1024.0 * 1024.0)
            ))?)
        } else {
            None
        };
        let mut spooled: HashMap<StretchKey, ClipPcm> = HashMap::new();
        let mut lanes = Vec::with_capacity(clips.len());
        for (index, entry) in prepared.into_iter().enumerate() {
            cancellation.check()?;
            let clip = &clips[index];
            match entry {
                Prepared::Muted { track } => lanes.push(ClipLane::muted(clip, track)),
                Prepared::Loaded {
                    track,
                    media,
                    samples,
                    plan,
                    source_pin,
                } => {
                    let bypass = plan.is_bypass();
                    let samples = if bypass {
                        samples
                    } else {
                        match samples {
                            ClipPcm::Resident(source) if !spill_resident => {
                                ClipPcm::Resident(derived.stretch_cancellable(
                                    &media,
                                    source,
                                    sample_rate,
                                    &plan,
                                    quality,
                                    cancellation,
                                )?)
                            }
                            _ => {
                                let dir = spool_dir.ok_or("Missing clip spool directory")?;
                                let key = StretchKey::for_plan(&media, sample_rate, &plan, quality);
                                match spooled.entry(key) {
                                    std::collections::hash_map::Entry::Occupied(entry) => {
                                        entry.get().clone()
                                    }
                                    std::collections::hash_map::Entry::Vacant(entry) => {
                                        let (_, pcm) = derived
                                            .spool_stretch(
                                                dir,
                                                entry.key().clone(),
                                                &samples,
                                                &plan,
                                                quality,
                                                cancellation,
                                            )
                                            .map_err(|error| {
                                                error.context(format!("Audio clip '{}'", clip.name))
                                            })?;
                                        entry.insert(pcm).clone()
                                    }
                                }
                            }
                        }
                    };
                    let pin = if bypass {
                        source_pin
                    } else {
                        derived
                            .pin_playback(&samples, cancellation)
                            .map_err(|error| error.context(format!("Audio clip '{}'", clip.name)))?
                    };
                    let samples = pin.as_ref().map_or(samples, |pin| pin.pcm.clone());
                    let mut lane =
                        ClipLane::from_samples(project, clip, track, sample_rate, media, samples)?;
                    lane.playback_pin = pin;
                    lanes.push(lane);
                }
            }
        }
        Ok(lanes)
    }

    /// Whether no clip lanes are loaded.
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// Number of clip lanes.
    pub fn len(&self) -> usize {
        self.lanes.len()
    }

    /// Lane at an index.
    pub fn get(&self, index: usize) -> Option<&ClipLane> {
        self.lanes.get(index)
    }

    /// `(clip name, media version)` for diagnostics and reload logging.
    pub fn identities(&self) -> Vec<(String, MediaKey)> {
        self.lanes
            .iter()
            .filter_map(|lane| lane.media.clone().map(|media| (lane.name.clone(), media)))
            .collect()
    }

    /// Mix every lane into its track buffer for the chunk at `chunk_start`.
    pub fn mix_into(&self, chunk_start: u64, track_buffers: &mut [Vec<f32>]) -> Result<(), String> {
        for lane in &self.lanes {
            if let Some(buffer) = track_buffers.get_mut(lane.track) {
                lane.mix_into(chunk_start, buffer)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_pcm::ScratchDisk;

    #[test]
    fn dropping_graph_cancels_in_flight_preparation_and_skips_queued_clips() {
        let project = oc_dsl::parse(
            r#"@project "Cancellation"
  tempo: 120
@audio_clip first [1:1 - 2:1] | track: tape
  file: "clip.wav"
@audio_clip later [2:1 - 3:1] | track: tape
  file: "clip.wav"
@track tape | instrument: audio/clip
"#,
        )
        .unwrap();
        let (started, attempts) = mpsc::channel();
        let gate = Arc::new(std::sync::Mutex::new(()));
        let owner = gate.clone();
        let _held = owner.lock().unwrap();
        let (finished, outcome) = mpsc::channel();
        let timeout = Duration::from_secs(5);
        let graph =
            ClipLanes::load_progressively_using(&project, 8000, 0, move |_, clip, cancellation| {
                started.send(clip.name.clone()).unwrap();
                let result = cancellation.lock(&gate);
                finished.send(result.is_err()).unwrap();
                drop(result?);
                Err("Unexpected preparation after cancellation".into())
            })
            .unwrap();
        assert_eq!(attempts.recv_timeout(timeout).unwrap(), "first");
        drop(graph);
        assert!(outcome.recv_timeout(timeout).unwrap());
        assert_eq!(
            attempts.recv_timeout(timeout),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn seeking_prioritizes_the_pending_clip_and_requeues_interrupted_work() {
        let project = oc_dsl::parse(
            r#"@project "Seek"
  tempo: 120
@audio_clip first [1:1 - 2:1] | track: tape
  file: "first.wav"
@audio_clip middle [2:1 - 2:4] | track: tape
  file: "middle.wav"
@audio_clip target [3:1 - 4:1] | track: tape
  file: "target.wav"
@track tape | instrument: audio/clip
"#,
        )
        .unwrap();
        let (started, attempts) = mpsc::channel();
        let gate = Arc::new(std::sync::Mutex::new(()));
        let owner = Arc::clone(&gate);
        let held = owner.lock().unwrap();
        let timeout = Duration::from_secs(5);
        let mut lanes =
            ClipLanes::load_progressively_using(&project, 8000, 0, move |_, clip, cancellation| {
                started.send(clip.name.clone()).unwrap();
                let _held = (clip.name == "first")
                    .then(|| cancellation.lock(&gate))
                    .transpose()?;
                Ok(ClipLane::muted(clip, 0))
            })
            .unwrap();
        assert_eq!(attempts.recv_timeout(timeout).unwrap(), "first");
        assert!(!lanes.ready_for(0, 1).unwrap());

        // The seek lands just before the target. The first DSP block still
        // needs that clip, even though the playhead itself is outside it.
        assert!(!lanes.ready_for(31_999, 32_002).unwrap());
        assert_eq!(attempts.recv_timeout(timeout).unwrap(), "target");
        let started = Instant::now();
        while !lanes.ready_for(31_999, 32_002).unwrap() {
            assert!(
                started.elapsed() < timeout,
                "the seek target stayed blocked"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(!lanes.ready_for(0, 1).unwrap());
        loop {
            let name = attempts.recv_timeout(timeout).unwrap();
            if name == "first" {
                break;
            }
            assert_eq!(name, "middle");
        }
        drop(held);
        let started = Instant::now();
        while !lanes.ready_for(0, 1).unwrap() {
            assert!(
                started.elapsed() < timeout,
                "the interrupted clip did not retry"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn progressive_preparation_retries_scratch_contention_and_loads_other_clips_first() {
        let root = tempfile::tempdir().unwrap();
        let mut wav = hound::WavWriter::create(
            root.path().join("clip.wav"),
            hound::WavSpec {
                channels: 2,
                sample_rate: 8000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..800 {
            wav.write_sample(4096_i16).unwrap();
        }
        wav.finalize().unwrap();
        let project = oc_dsl::parse(
            r#"@project "Retry"
  tempo: 120
@audio_clip first [1:1 - 2:1] | track: tape
  file: "clip.wav"
@audio_clip later [2:1 - 3:1] | track: tape
  file: "clip.wav"
@audio_clip missing [3:1 - 4:1] | track: tape
  file: "missing.wav"
@track tape | instrument: audio/clip
"#,
        )
        .unwrap();
        let scratch = Arc::new(ScratchDisk::new(32));
        let blocked = scratch.reserve(32).unwrap();
        let too_large = scratch.reserve(33).err().unwrap();
        assert!(!too_large.scratch_busy);
        let (sender, attempted) = mpsc::channel();
        let cache = MediaCache::new(None, 1 << 20, 0);
        let derived = DerivedCache::new(None, 1 << 20, 0);
        let mut lanes = ClipLanes::load_progressively_using(
            &project,
            8000,
            0,
            move |project, clip, cancellation| {
                sender.send(clip.name.clone()).unwrap();
                let _lease = if clip.name == "first" {
                    Some(scratch.reserve(16)?)
                } else {
                    None
                };
                ClipLanes::load_selection(
                    &cache,
                    &derived,
                    project,
                    8000,
                    Some(root.path()),
                    &[clip],
                    cancellation,
                )
                .map(|mut lanes| lanes.remove(0))
            },
        )
        .unwrap();
        let timeout = Duration::from_secs(5);
        assert_eq!(attempted.recv_timeout(timeout).unwrap(), "first");
        loop {
            let name = attempted.recv_timeout(timeout).unwrap();
            if name == "later" {
                break;
            }
            assert_eq!(name, "first");
        }
        loop {
            let name = attempted.recv_timeout(timeout).unwrap();
            if name == "missing" {
                break;
            }
            assert_eq!(name, "first");
        }
        assert!(!lanes.ready_for(0, 1).unwrap());
        drop(blocked);

        let start = Instant::now();
        while !lanes.ready_for(0, 1).unwrap() {
            assert!(
                start.elapsed() < timeout,
                "retry did not publish the opening clip"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(lanes.ready_for(16_000, 16_001).unwrap());
        let missing = lanes.ready_for(32_000, 32_001).unwrap_err();
        assert!(missing.contains("missing.wav"), "{missing}");
        assert!(
            attempted.try_iter().all(|name| name == "first"),
            "permanent errors should not be retried"
        );
        let mut tracks = vec![vec![0.0; 16]];
        lanes.mix_into(0, &mut tracks).unwrap();
        assert!(tracks[0].iter().all(|sample| *sample != 0.0));
    }
}
