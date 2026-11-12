//! Audio-clip media: symphonia decode, stereo downmix and band-limited resampling.
//! Runs on the offline/worker render path; the audio callback never calls this.

use crate::clip_pcm::ClipPreparationError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oc_dsl::ast::{AudioClip, GainEnvelopePoint, Project};
use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decoded stereo PCM at its native rate. Shared so cached entries are cloned cheaply.
pub struct DecodedAudio {
    pub sample_rate: u32,
    /// Interleaved left/right frames.
    pub samples: Arc<Vec<f32>>,
}

impl DecodedAudio {
    /// Stereo frame count.
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }
}

/// Matches the existing audio-region media bound.
pub(crate) const MAX_CLIP_SECONDS: u64 = 30 * 60;

/// Decode any symphonia-supported container to interleaved stereo at its native rate.
pub fn decode(path: &Path) -> Result<DecodedAudio, String> {
    let mut samples = Vec::new();
    let sample_rate = decode_packets(path, |block| {
        samples.extend_from_slice(block);
        Ok(())
    })?;
    Ok(DecodedAudio {
        sample_rate,
        samples: Arc::new(samples),
    })
}

/// Decode and downmix one packet at a time. The caller owns the PCM lifetime.
pub(crate) fn decode_packets(
    path: &Path,
    mut emit: impl FnMut(&[f32]) -> Result<(), ClipPreparationError>,
) -> Result<u32, ClipPreparationError> {
    let (sample_rate, _) = decode_until(path, |block, _, _| {
        emit(block)?;
        Ok(true)
    })?;
    Ok(sample_rate)
}

/// A decoded window of an audio clip.
pub struct DecodedPassage {
    pub audio: DecodedAudio,
    pub first_frame: usize,
    pub requested_frames: usize,
    /// The exact source frame count, when decoding reached the end.
    pub source_frames: Option<usize>,
}

/// Decode in packet order so codec delay and gapless trimming match a full
/// decode. Discard the prefix and stop once the selected PCM has been read.
pub fn decode_passage(path: &Path, start: f64, duration: f64) -> Result<DecodedPassage, String> {
    decode_passage_with_lead(path, start, duration, 0)
}

/// Read the same passage with up to `lead_frames` of preceding context.
pub fn decode_passage_with_lead(
    path: &Path,
    start: f64,
    duration: f64,
    lead_frames: usize,
) -> Result<DecodedPassage, String> {
    if !((0.0..MAX_CLIP_SECONDS as f64).contains(&start) && 0.0 < duration && duration <= 30.0) {
        return Err("Audio passage needs a start before 30 minutes and a duration greater than 0 and at most 30 seconds".into());
    }
    let mut samples = Vec::new();
    let mut selection = None;
    let (sample_rate, source_frames) = decode_until(path, |block, rate, offset| {
        let (first_rate, first, count, lead) = *selection.get_or_insert_with(|| {
            let first = (start * f64::from(rate)).round() as usize;
            let count = ((duration * f64::from(rate)).round() as usize).max(1);
            let from = first.saturating_sub(lead_frames);
            (rate, from, count, first - from)
        });
        if rate != first_rate || count > 30 * 192_000 {
            return Err("Audio passage changes sample rate or exceeds the sample budget".into());
        }
        let end = first + lead + count;
        let packet_end = offset + block.len() / 2;
        let from = first.max(offset);
        let to = end.min(packet_end);
        if from < to {
            samples.extend_from_slice(&block[2 * (from - offset)..2 * (to - offset)]);
        }
        Ok(packet_end <= end)
    })?;
    let (_, first_frame, count, lead) = selection.ok_or("No audio frames decoded")?;
    if samples.is_empty() {
        return Err("Audio passage starts at or beyond the end of the file".into());
    }
    Ok(DecodedPassage {
        audio: DecodedAudio {
            sample_rate,
            samples: Arc::new(samples),
        },
        first_frame,
        requested_frames: count + lead,
        source_frames,
    })
}

/// The packet callback returns whether more PCM is needed.
fn decode_until(
    path: &Path,
    mut emit: impl FnMut(&[f32], u32, usize) -> Result<bool, ClipPreparationError>,
) -> Result<(u32, Option<usize>), ClipPreparationError> {
    let file = std::fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("file not found: {}", path.display())
        } else {
            format!("cannot open {}: {error}", path.display())
        }
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("unsupported or invalid container: {error}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "container has no default audio track".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|error| format!("unsupported codec: {error}"))?;

    let mut sample_rate = codec_params.sample_rate.unwrap_or(0);
    let mut stereo = Vec::new();
    let mut frames = 0_u64;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(format!("read error: {error}").into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(message)) => {
                return Err(format!("malformed audio packet: {message}").into());
            }
            Err(error) => return Err(format!("decode error: {error}").into()),
        };
        let spec = *decoded.spec();
        if spec.rate == 0 || spec.channels.is_empty() {
            return Err("decoded stream has no sample rate or channels".into());
        }
        sample_rate = spec.rate;
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        stereo.clear();
        downmix(&mut stereo, buffer.samples(), spec.channels)?;
        let offset = frames;
        frames += (stereo.len() / 2) as u64;
        let limit = u64::from(sample_rate) * MAX_CLIP_SECONDS;
        if frames > limit {
            return Err(format!("clip exceeds the {MAX_CLIP_SECONDS}s decode bound").into());
        }
        if !emit(&stereo, sample_rate, offset as usize)? {
            return Ok((sample_rate, None));
        }
    }
    if frames == 0 || sample_rate == 0 {
        return Err("decoded no audio frames".into());
    }
    Ok((sample_rate, Some(frames as usize)))
}

/// Decode and resample to the project rate in one step.
pub fn load(
    path: &Path,
    target_rate: u32,
    quality: crate::resample::ResampleQuality,
) -> Result<DecodedAudio, String> {
    let decoded = decode(path)?;
    if decoded.sample_rate == target_rate {
        return Ok(decoded);
    }
    let samples =
        crate::resample::resample(&decoded.samples, decoded.sample_rate, target_rate, quality)?;
    Ok(DecodedAudio {
        sample_rate: target_rate,
        samples: Arc::new(samples),
    })
}

/// Resolve a project-relative media path; absolute paths remain untouched.
pub fn resolve_path(file: &str, root: Option<&Path>) -> Result<PathBuf, String> {
    if file.trim().is_empty() {
        return Err("empty media path".into());
    }
    let path = Path::new(file);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let root =
        root.ok_or_else(|| format!("relative media path '{file}' requires the project directory"))?;
    Ok(root.join(path))
}

/// Content identity of one clip's media: its length and modification time.
///
/// `None` when the file cannot be resolved, is missing or unreadable. The stamp
/// leaves out the resolved absolute path, so moving a project does not change
/// it, and reading it never decodes the file.
pub fn media_stamp(file: &str, root: Option<&Path>) -> Option<(u64, std::time::SystemTime)> {
    let path = resolve_path(file, root).ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let modified = metadata.modified().ok()?;
    Some((metadata.len(), modified))
}

/// Resolve one clip's media and refuse a file that is not on disk.
///
/// The refusal names the path that was tried, so a project separated from its
/// `media/` tree reports where the load looked.
fn media_path(clip: &AudioClip, root: Option<&Path>) -> Result<PathBuf, String> {
    let path = resolve_path(&clip.file, root)
        .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
    if !path.is_file() {
        return Err(format!(
            "audio clip '{}': file not found: {}",
            clip.name,
            path.display()
        ));
    }
    Ok(path)
}

/// Start frame and length in frames for a clip's musical range.
pub fn range_frames(
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
) -> Result<(u64, u64), String> {
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let (start, end) = clock.audio_clip_range_frames(clip, sample_rate)?;
    Ok((start, end - start))
}

/// Last frame touched by any audible audio clip, or zero when the project has
/// none. A muted clip extends nothing, exactly like a clip that is not there.
pub fn project_end_frame(project: &Project, sample_rate: u32) -> Result<u64, String> {
    let mut end = 0;
    for clip in project.audio_clips() {
        if clip.muted {
            continue;
        }
        let (start, length) = range_frames(project, clip, sample_rate)?;
        end = end.max(start.saturating_add(length));
    }
    Ok(end)
}

/// Duration before any clip processing.
pub fn source_seconds(clip: &AudioClip, root: Option<&Path>) -> Result<f64, String> {
    let path = media_path(clip, root)?;
    let entry = crate::media_cache::shared()
        .decoded(&path)
        .map_err(|error| {
            format!(
                "audio clip '{}': cannot decode {}: {error}",
                clip.name,
                path.display()
            )
        })?;
    Ok(entry.frames() as f64 / f64::from(entry.sample_rate()))
}

/// Playable length of one clip's stretched media, in seconds.
///
/// This is the span a slip offset moves through: `offset + range` may not pass
/// it, or the tail of the clip would be silent. Decoding goes through the
/// shared cache, so a second call is cheap.
pub fn content_seconds(
    project: &Project,
    clip: &AudioClip,
    root: Option<&Path>,
) -> Result<f64, String> {
    let path = media_path(clip, root)?;
    let entry = crate::media_cache::shared()
        .decoded(&path)
        .map_err(|error| {
            format!(
                "audio clip '{}': cannot decode {}: {error}",
                clip.name,
                path.display()
            )
        })?;
    let frames = entry.frames();
    let rate = entry.sample_rate();
    let plan = crate::stretch::resolve(project, clip, rate, frames)?;
    let output = if plan.is_bypass() {
        frames
    } else {
        plan.output_frames(frames, rate)
    };
    Ok(output as f64 / f64::from(rate))
}

/// Decode every audible clip once and confirm its media and stretch plan are
/// readable. A muted clip is skipped, like an absent one, so it may reference
/// media that is missing without failing a render.
pub fn validate(project: &Project, root: Option<&Path>) -> Result<(), String> {
    for clip in project.audio_clips() {
        if clip.muted {
            continue;
        }
        let path = media_path(clip, root)?;
        let entry = crate::media_cache::shared()
            .decoded(&path)
            .map_err(|error| {
                format!(
                    "audio clip '{}': cannot decode {}: {error}",
                    clip.name,
                    path.display()
                )
            })?;
        range_frames(project, clip, entry.sample_rate())
            .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
        // Retained settings must still be valid when processing is enabled again.
        let configured = clip.bypass_processing.then(|| {
            let mut configured = clip.clone();
            configured.bypass_processing = false;
            configured
        });
        let configured = configured.as_ref().unwrap_or(clip);
        crate::stretch::resolve(project, configured, entry.sample_rate(), entry.frames())?;
        validate_loop(project, configured, root)?;
    }
    Ok(())
}

/// Shortest loop the engine repeats: a shorter one is a stutter, not a loop.
pub const MIN_LOOP_SECONDS: f64 = 0.100;

/// Check a clip's loop against its media and its placement.
///
/// A loop region starts at the clip's offset, so it must end inside the media
/// and stay shorter than the clip; a loop over a clip shorter than the floor
/// would buzz on every frame.
fn validate_loop(project: &Project, clip: &AudioClip, root: Option<&Path>) -> Result<(), String> {
    if clip.loop_seconds <= 0.0 {
        return Ok(());
    }
    let content = content_seconds(project, clip, root)?;
    if content < MIN_LOOP_SECONDS {
        return Err(format!(
            "audio clip '{}': the media is {:.0} ms long, shorter than the {} ms a loop needs",
            clip.name,
            content * 1000.0,
            MIN_LOOP_SECONDS * 1000.0
        ));
    }
    if clip.loop_seconds < MIN_LOOP_SECONDS {
        return Err(format!(
            "audio clip '{}': a loop needs at least {} ms, not {:.0} ms",
            clip.name,
            MIN_LOOP_SECONDS * 1000.0,
            clip.loop_seconds * 1000.0
        ));
    }
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let (start, end) = clock.audio_clip_range_seconds(clip)?;
    let clip_seconds = end - start;
    if clip.loop_seconds >= clip_seconds {
        return Err(format!(
            "audio clip '{}': the loop ({:.3} s) must be shorter than the clip ({:.3} s)",
            clip.name, clip.loop_seconds, clip_seconds
        ));
    }
    let region_end = clip.offset_seconds + clip.loop_seconds;
    if region_end > content + 1e-9 {
        return Err(format!(
            "audio clip '{}': the loop ends at {region_end:.3} s but the media ends at {content:.3} s",
            clip.name
        ));
    }
    Ok(())
}

/// Project-rate media for one clip, decoded and resampled through the cache.
pub fn resampled_media(
    cache: &crate::media_cache::MediaCache,
    clip: &AudioClip,
    sample_rate: u32,
    quality: crate::resample::ResampleQuality,
    root: Option<&Path>,
) -> Result<(crate::media_cache::MediaKey, Arc<Vec<f32>>), String> {
    let path = media_path(clip, root)?;
    cache
        .resampled_with_key(&path, sample_rate, quality)
        .map_err(|error| {
            format!(
                "audio clip '{}': cannot decode {}: {error}",
                clip.name,
                path.display()
            )
        })
}

pub(crate) fn project_rate_pcm(
    cache: &crate::media_cache::MediaCache,
    clip: &AudioClip,
    sample_rate: u32,
    quality: crate::resample::ResampleQuality,
    root: Option<&Path>,
    cancellation: crate::clip_pcm::ClipCancellation<'_>,
) -> Result<(crate::media_cache::MediaKey, crate::clip_pcm::ClipPcm), ClipPreparationError> {
    let path = media_path(clip, root)?;
    cache
        .playback_with_key(&path, sample_rate, quality, cancellation)
        .map_err(|error| {
            error.context(format!(
                "audio clip '{}': cannot decode {}",
                clip.name,
                path.display()
            ))
        })
}

pub(crate) fn clip_playback_pcm(
    cache: &crate::media_cache::MediaCache,
    derived: &crate::derived_cache::DerivedCache,
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(crate::derived_cache::StretchKey, crate::clip_pcm::ClipPcm), String> {
    use crate::clip_pcm::ClipPcm;

    let quality = crate::resample::render_quality(project);
    let (media, samples) =
        project_rate_pcm(cache, clip, sample_rate, quality, root, Default::default())?;
    let plan = crate::stretch::resolve(project, clip, sample_rate, samples.frames())?;
    let key = crate::derived_cache::StretchKey::for_plan(&media, sample_rate, &plan, quality);
    if plan.is_bypass() {
        return Ok((key, samples));
    }
    let bytes = plan
        .output_frames(samples.frames(), sample_rate)
        .saturating_mul(8);
    let pcm = match samples {
        ClipPcm::Resident(source) if bytes <= derived.memory_cap() => {
            ClipPcm::Resident(derived.stretch(&media, source, sample_rate, &plan, quality)?)
        }
        _ => {
            let dir = derived.spool_dir(bytes).ok_or_else(|| {
                format!(
                    "audio clip '{}': insufficient disk capacity for processed media",
                    clip.name
                )
            })?;
            return Ok(derived.spool_stretch(
                dir,
                key,
                &samples,
                &plan,
                quality,
                Default::default(),
            )?);
        }
    };
    Ok((key, pcm))
}

/// Mix one clip into an interleaved stereo track buffer, sample-accurate at its start.
pub fn mix_into_track(
    buffer: &mut [f32],
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(), String> {
    mix_into_track_with_caches(
        crate::media_cache::shared(),
        crate::derived_cache::shared(),
        buffer,
        project,
        clip,
        sample_rate,
        root,
    )
}

/// Mix only the clip's overlap with a bounded track buffer. Its source
/// offset, loop phase and envelopes stay relative to the original placement.
#[cfg(feature = "plugin-host")]
pub(crate) fn mix_into_track_window(
    buffer: &mut [f32],
    buffer_start: u64,
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(), String> {
    if clip.muted {
        return Ok(());
    }
    let (start, frames) = range_frames(project, clip, sample_rate)
        .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
    let end = buffer_start.saturating_add((buffer.len() / 2) as u64);
    if start >= end || start.saturating_add(frames) <= buffer_start {
        return Ok(());
    }
    mix_into_track_at(
        crate::media_cache::shared(),
        crate::derived_cache::shared(),
        (buffer_start, buffer),
        project,
        clip,
        sample_rate,
        root,
    )
}

/// Cached mix path: decoded and stretched media are shared across clips and reloads.
pub fn mix_into_track_with_caches(
    cache: &crate::media_cache::MediaCache,
    derived: &crate::derived_cache::DerivedCache,
    buffer: &mut [f32],
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(), String> {
    mix_into_track_at(
        cache,
        derived,
        (0, buffer),
        project,
        clip,
        sample_rate,
        root,
    )
}

fn mix_into_track_at(
    cache: &crate::media_cache::MediaCache,
    derived: &crate::derived_cache::DerivedCache,
    destination: (u64, &mut [f32]),
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(), String> {
    let (buffer_start, buffer) = destination;
    if !buffer.len().is_multiple_of(2) {
        return Err("audio clip target must be interleaved stereo".into());
    }
    if clip.muted {
        return Ok(());
    }
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
    // A stored tuning block moves the audible window through the same bake
    // `ocp vocal apply` writes, so a project renders tuned without applying it.
    let harmony = crate::vocal_harmony::harmony_for_clip(project, clip);
    let tuned = crate::vocal_tune::render_window_with_key(project, clip, sample_rate, root)?;
    let mut mono_source = false;
    match &tuned {
        Some((_, window)) => {
            if harmony.is_some() {
                mono_source = crate::vocal_harmony::is_mono(window);
            }
            mix_window(
                (buffer_start, buffer),
                project,
                clip,
                sample_rate,
                window,
                0,
                None,
            )?;
        }
        None => {
            let (key, audio) = clip_playback_pcm(cache, derived, project, clip, sample_rate, root)?;
            let media_frames = audio.frames();
            let offset = (clip.offset_seconds * f64::from(sample_rate)).round() as usize;
            if offset >= media_frames {
                return Ok(());
            }
            if harmony.is_some() {
                mono_source = true;
                audio.visit(0, media_frames, |_, block| {
                    mono_source &= crate::vocal_harmony::is_mono(block);
                })?;
            }
            crate::clip_lane::ClipLane::from_samples(
                project,
                clip,
                0,
                sample_rate,
                key.media,
                audio,
            )?
            .mix_into(buffer_start, buffer)?;
        }
    }
    // The harmony is a second voice on the same track: it follows the tuned
    // lead and adds to the same buffer at its own level and pan.
    let Some(harmony) = harmony else {
        return Ok(());
    };
    let lead = tuned.as_ref().map(|(key, window)| (key, window.as_slice()));
    if let Some(window) =
        crate::vocal_harmony::render_harmony_window(project, clip, sample_rate, root, lead)?
    {
        let pan = crate::vocal_harmony::pan_gains(harmony.pan, mono_source);
        mix_window(
            (buffer_start, buffer),
            project,
            clip,
            sample_rate,
            &window,
            0,
            Some(pan),
        )?;
    }
    Ok(())
}

/// Add the audible overlap to a buffer with an absolute timeline origin.
///
/// `pan` is the extra per-channel balance of a second voice; `None` is unity.
fn mix_window(
    destination: (u64, &mut [f32]),
    project: &Project,
    clip: &AudioClip,
    sample_rate: u32,
    audio: &[f32],
    offset: usize,
    pan: Option<(f32, f32)>,
) -> Result<(), String> {
    let (start, length) = range_frames(project, clip, sample_rate)
        .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
    let (buffer_start, buffer) = destination;
    let length = usize::try_from(length).map_err(|_| "clip length overflow".to_string())?;
    let media_frames = audio.len() / 2;
    if offset >= media_frames {
        return Ok(());
    }
    let loop_frames = loop_frames(clip, sample_rate, media_frames, offset);
    // A looped clip fills its whole length; a single pass stops at the media end.
    let frames = if loop_frames > 0 {
        length
    } else {
        length.min(media_frames - offset)
    };
    let gain = 10.0_f32.powf(clip.gain_db as f32 / 20.0) * if clip.invert { -1.0 } else { 1.0 };
    let fade_in = (clip.fade_in_seconds * f64::from(sample_rate)).round() as usize;
    let fade_out = (clip.fade_out_seconds * f64::from(sample_rate)).round() as usize;
    let (left, right) = pan.unwrap_or((1.0, 1.0));
    let crossfade = (clip.crossfade_seconds * f64::from(sample_rate)).round() as usize;

    let buffer_end = buffer_start.saturating_add((buffer.len() / 2) as u64);
    let first = buffer_start.saturating_sub(start).min(frames as u64) as usize;
    let last = buffer_end.saturating_sub(start).min(frames as u64) as usize;
    for frame in first..last {
        let destination = (start + frame as u64 - buffer_start) as usize * 2;
        let envelope = clip_gain(
            gain,
            fade_in,
            fade_out,
            crossfade,
            frame,
            frames,
            &clip.gain_envelope,
        );
        let source = offset
            + if loop_frames > 0 {
                frame % loop_frames
            } else {
                frame
            };
        buffer[destination] += audio[source * 2] * envelope * left;
        buffer[destination + 1] += audio[source * 2 + 1] * envelope * right;
    }
    Ok(())
}

/// Frames in the loop region, or zero for a single pass.
///
/// The region starts at the clip's offset and is clamped to the media that is
/// left, so a hand-written loop cannot read past the file.
pub(crate) fn loop_frames(
    clip: &AudioClip,
    sample_rate: u32,
    media_frames: usize,
    offset: usize,
) -> usize {
    if !clip.loop_seconds.is_finite() || clip.loop_seconds <= 0.0 {
        return 0;
    }
    let frames = (clip.loop_seconds * f64::from(sample_rate)).round() as usize;
    frames.min(media_frames.saturating_sub(offset))
}

/// One frame of the clip envelope: gain, linear fades, the equal-power ends and
/// the optional point envelope.
///
/// The head and tail use a quarter-wave sine; coincident ends square to one.
pub(crate) fn clip_gain(
    gain: f32,
    fade_in: usize,
    fade_out: usize,
    crossfade: usize,
    frame: usize,
    frames: usize,
    points: &[GainEnvelopePoint],
) -> f32 {
    let mut envelope = gain;
    if fade_in > 0 && frame < fade_in {
        envelope *= frame as f32 / fade_in as f32;
    }
    if fade_out > 0 && frame + fade_out >= frames {
        envelope *= (frames - frame - 1) as f32 / fade_out as f32;
    }
    if crossfade > 0 {
        if frame < crossfade {
            let phase = (frame as f32 + 0.5) / crossfade as f32;
            envelope *= (phase * std::f32::consts::FRAC_PI_2).sin();
        }
        if frame + crossfade >= frames {
            let distance = (frames - frame - 1) as f32 + 0.5;
            let phase = distance / crossfade as f32;
            envelope *= (phase * std::f32::consts::FRAC_PI_2).sin();
        }
    }
    envelope * point_gain(points, frame, frames)
}

/// Linear gain of a clip's point envelope at `frame / frames`, clamped before
/// the first point and after the last. An empty envelope is unity.
fn point_gain(points: &[GainEnvelopePoint], frame: usize, frames: usize) -> f32 {
    let Some(first) = points.first() else {
        return 1.0;
    };
    let last = points.last().unwrap_or(first);
    let position = if frames == 0 {
        0.0
    } else {
        frame as f64 / frames as f64
    };
    let db = if position <= first.position {
        first.gain_db
    } else if position >= last.position {
        last.gain_db
    } else {
        points
            .windows(2)
            .find(|pair| position <= pair[1].position)
            .map_or(last.gain_db, |pair| {
                let span = pair[1].position - pair[0].position;
                let blend = if span > 0.0 {
                    (position - pair[0].position) / span
                } else {
                    0.0
                };
                pair[0].gain_db + (pair[1].gain_db - pair[0].gain_db) * blend
            })
    };
    10.0_f32.powf(db as f32 / 20.0)
}

fn downmix(output: &mut Vec<f32>, interleaved: &[f32], channels: Channels) -> Result<(), String> {
    let count = channels.count();
    if count == 0 {
        return Err("decoded stream has no channels".into());
    }
    let layout: Vec<Channels> = channels.iter().collect();
    output.reserve(interleaved.len() / count * 2);
    for frame in interleaved.chunks_exact(count) {
        if count == 1 {
            output.push(frame[0]);
            output.push(frame[0]);
            continue;
        }
        if count == 2 {
            output.push(frame[0]);
            output.push(frame[1]);
            continue;
        }
        let mut left = 0.0;
        let mut right = 0.0;
        for (index, sample) in frame.iter().enumerate() {
            let (left_weight, right_weight) = downmix_weights(layout[index]);
            left += sample * left_weight;
            right += sample * right_weight;
        }
        output.push(left);
        output.push(right);
    }
    Ok(())
}

fn downmix_weights(channel: Channels) -> (f32, f32) {
    const CENTER: f32 = 0.707_106_77;
    const LFE: f32 = 0.5;
    if channel.intersects(Channels::FRONT_LEFT) {
        (1.0, 0.0)
    } else if channel.intersects(Channels::FRONT_RIGHT) {
        (0.0, 1.0)
    } else if channel.intersects(Channels::LFE1 | Channels::LFE2) {
        (LFE, LFE)
    } else if channel
        .intersects(Channels::FRONT_CENTRE | Channels::REAR_CENTRE | Channels::TOP_CENTRE)
    {
        (CENTER, CENTER)
    } else if channel.intersects(
        Channels::REAR_LEFT
            | Channels::SIDE_LEFT
            | Channels::FRONT_LEFT_CENTRE
            | Channels::REAR_LEFT_CENTRE
            | Channels::TOP_FRONT_LEFT
            | Channels::TOP_REAR_LEFT
            | Channels::FRONT_LEFT_WIDE
            | Channels::FRONT_LEFT_HIGH,
    ) {
        (CENTER, 0.0)
    } else if channel.intersects(
        Channels::REAR_RIGHT
            | Channels::SIDE_RIGHT
            | Channels::FRONT_RIGHT_CENTRE
            | Channels::REAR_RIGHT_CENTRE
            | Channels::TOP_FRONT_RIGHT
            | Channels::TOP_REAR_RIGHT
            | Channels::FRONT_RIGHT_WIDE
            | Channels::FRONT_RIGHT_HIGH,
    ) {
        (0.0, CENTER)
    } else {
        (0.5, 0.5)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compressed_passages_match_full_pcm_and_report_eof_only_when_reached() {
        for fixture in ["sf3-tone-22050.ogg", "sampler/tone.flac"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture);
            let full = super::decode(&path).unwrap();
            let rate = f64::from(full.sample_rate);
            let first = full.frames() / 5;
            let count = full.frames() / 4;
            let middle =
                super::decode_passage(&path, first as f64 / rate, count as f64 / rate).unwrap();
            assert_eq!(middle.first_frame, first, "{fixture}");
            assert_eq!(middle.requested_frames, count, "{fixture}");
            assert_eq!(middle.source_frames, None, "{fixture}");
            assert_eq!(
                &*middle.audio.samples,
                &full.samples[first * 2..(first + count) * 2],
                "{fixture}"
            );

            let first = full.frames() - count;
            let with_lead = super::decode_passage_with_lead(
                &path,
                first as f64 / rate,
                count as f64 / rate,
                64,
            )
            .unwrap();
            assert_eq!(with_lead.first_frame, first - 64, "{fixture}");
            assert_eq!(with_lead.requested_frames, count + 64, "{fixture}");
            assert_eq!(
                &*with_lead.audio.samples,
                &full.samples[(first - 64) * 2..],
                "{fixture}"
            );
            let tail = super::decode_passage(&path, first as f64 / rate, 2.0 * count as f64 / rate)
                .unwrap();
            assert_eq!(tail.source_frames, Some(full.frames()), "{fixture}");
            assert_eq!(
                &*tail.audio.samples,
                &full.samples[first * 2..],
                "{fixture}"
            );
            assert!(super::decode_passage(&path, full.frames() as f64 / rate, 0.01).is_err());
        }
    }

    use super::*;
    use std::f64::consts::PI;

    use crate::resample::ResampleQuality;

    const RATE: u32 = 48_000;

    /// Fresh directory for one case; the caller removes it.
    fn temp_dir(tag: &str) -> PathBuf {
        oc_test_support::temp_dir("opencompose-clip-media", tag)
    }

    fn clip_with_file(file: &str) -> Project {
        let text = format!(
            "@project \"media path\"\n  tempo: 120\n@audio_clip hit [1:1 - 2:1] | track: tape\n  file: \"{file}\"\n@track tape | instrument: audio/clip\n"
        );
        oc_dsl::parse(&text).unwrap()
    }

    #[test]
    fn a_relative_media_path_resolves_against_the_project_directory() {
        let root = temp_dir("relative");
        std::fs::create_dir_all(root.join("media/samples")).unwrap();
        std::fs::write(root.join("media/samples/hit.wav"), b"wav").unwrap();
        let project = clip_with_file("media/samples/hit.wav");
        let clip = project.audio_clip(0).unwrap();
        assert_eq!(
            media_path(clip, Some(&root)).unwrap(),
            root.join("media/samples/hit.wav")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_absolute_media_path_resolves_without_a_project_directory() {
        let root = temp_dir("absolute");
        let file = root.join("hit.wav");
        std::fs::write(&file, b"wav").unwrap();
        let project = clip_with_file(&file.display().to_string());
        let clip = project.audio_clip(0).unwrap();
        assert_eq!(media_path(clip, None).unwrap(), file);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_media_file_is_refused_with_the_path_it_tried() {
        let root = temp_dir("missing");
        let project = clip_with_file("media/samples/gone.wav");
        let clip = project.audio_clip(0).unwrap();
        let error = media_path(clip, Some(&root)).unwrap_err();
        let tried = root.join("media/samples/gone.wav");
        assert!(error.contains(&tried.display().to_string()), "{error}");
        assert!(error.contains("audio clip 'hit'"), "{error}");
        assert!(error.contains("file not found"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_relative_media_path_without_a_project_directory_is_refused() {
        let project = clip_with_file("media/samples/hit.wav");
        let clip = project.audio_clip(0).unwrap();
        let error = media_path(clip, None).unwrap_err();
        assert!(error.contains("requires the project directory"), "{error}");
        assert!(error.contains("media/samples/hit.wav"), "{error}");
    }

    #[test]
    fn resampling_preserves_length_and_rms() {
        let source_rate = 44_100;
        let target_rate = 48_000;
        let frames = source_rate as usize / 2;
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let value =
                (2.0 * PI * 440.0 * frame as f64 / f64::from(source_rate)).sin() as f32 * 0.5;
            samples.push(value);
            samples.push(value);
        }
        let output = crate::resample::resample(
            &samples,
            source_rate,
            target_rate,
            ResampleQuality::DEFAULT_RENDER,
        )
        .unwrap();
        assert_eq!(
            output.len() / 2,
            (frames as f64 * f64::from(target_rate) / f64::from(source_rate)).round() as usize
        );
        let rms = |values: &[f32]| {
            let sum: f64 = values.iter().map(|value| f64::from(*value).powi(2)).sum();
            (sum / values.len() as f64).sqrt()
        };
        let before = rms(&samples);
        let after = rms(&output);
        assert!((before - after).abs() < 0.005, "{before} vs {after}");
    }

    #[test]
    fn downsampling_filters_above_nyquist_instead_of_aliasing() {
        let source_rate = 48_000;
        let target_rate = 44_100;
        let frames = source_rate as usize / 2;
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let value = (2.0 * PI * 23_000.0 * frame as f64 / f64::from(source_rate)).sin() as f32;
            samples.push(value);
            samples.push(value);
        }
        let output = crate::resample::resample(
            &samples,
            source_rate,
            target_rate,
            ResampleQuality::DEFAULT_RENDER,
        )
        .unwrap();
        let rms = |values: &[f32]| {
            let sum: f64 = values.iter().map(|value| f64::from(*value).powi(2)).sum();
            (sum / values.len() as f64).sqrt()
        };
        assert!(
            rms(&output) < 0.05,
            "aliased image leaked: {}",
            rms(&output)
        );
    }

    #[test]
    fn multi_channel_downmix_keeps_front_pair() {
        let mut output = Vec::new();
        downmix(
            &mut output,
            &[0.25, 0.5, 0.1, 0.0, 0.2, 0.3],
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::LFE1
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT,
        )
        .unwrap();
        assert_eq!(output.len(), 2);
        assert!((output[0] - (0.25 + 0.070_710_68 + 0.141_421_36)).abs() < 1e-5);
        assert!((output[1] - (0.5 + 0.070_710_68 + 0.212_132_04)).abs() < 1e-5);
    }

    #[test]
    fn absent_polarity_and_crossfade_keep_the_legacy_sum() {
        let text = r#"@project "Legacy"
  tempo: 120
@audio_clip clip [2:1 - 2:2] | track: tape
  file: "clip.wav"
  offset: 10ms
  gain: -3dB
  fade_in: 5ms
  fade_out: 10ms
@track tape | instrument: audio/clip
"#;
        let project = oc_dsl::parse(text).unwrap();
        let clip = project.audio_clip(0).unwrap();
        assert!(!clip.invert);
        assert_eq!(clip.crossfade_seconds, 0.0);

        let (start, length) = range_frames(&project, clip, RATE).unwrap();
        let offset = (clip.offset_seconds * f64::from(RATE)) as usize;
        let frames = length as usize;
        let media: Vec<f32> = (0..(frames + offset) * 2)
            .map(|index| ((index as f32 * 0.017).sin()) * 0.7)
            .collect();
        let mut mixed = vec![0.0_f32; (start as usize + frames) * 2];
        mix_window((0, &mut mixed), &project, clip, RATE, &media, offset, None).unwrap();

        let gain = 10.0_f32.powf(clip.gain_db as f32 / 20.0);
        let fade_in = (clip.fade_in_seconds * f64::from(RATE)) as usize;
        let fade_out = (clip.fade_out_seconds * f64::from(RATE)) as usize;
        let mut legacy = vec![0.0_f32; mixed.len()];
        for frame in 0..frames {
            let mut envelope = gain;
            if fade_in > 0 && frame < fade_in {
                envelope *= frame as f32 / fade_in as f32;
            }
            if fade_out > 0 && frame + fade_out >= frames {
                envelope *= (frames - frame - 1) as f32 / fade_out as f32;
            }
            let destination = (start as usize + frame) * 2;
            let source = (offset + frame) * 2;
            legacy[destination] += media[source] * envelope;
            legacy[destination + 1] += media[source + 1] * envelope;
        }
        let window_start = start + 31;
        let mut partial = vec![0.0; 100];
        mix_window(
            (window_start, &mut partial),
            &project,
            clip,
            RATE,
            &media,
            offset,
            None,
        )
        .unwrap();
        let first = window_start as usize * 2;
        assert_eq!(partial, legacy[first..first + partial.len()]);
        assert!(
            mixed
                .iter()
                .zip(&legacy)
                .all(|(left, right)| left.to_bits() == right.to_bits())
        );
    }

    /// Constant media turns every output sample into the clip's gain, so the
    /// shape of the envelope is readable straight from the buffer.
    fn render_envelope(envelope: Option<&str>) -> Vec<f32> {
        let line = envelope
            .map(|points| format!("  gain_envelope: {points}\n"))
            .unwrap_or_default();
        let text = format!(
            "@project \"Envelope\"\n  tempo: 120\n@audio_clip clip [1:1 - 2:1] | track: tape\n  file: \"clip.wav\"\n{line}@track tape | instrument: audio/clip\n"
        );
        let project = oc_dsl::parse(&text).unwrap();
        let clip = project.audio_clip(0).unwrap();
        let (start, length) = range_frames(&project, clip, RATE).unwrap();
        let frames = length as usize;
        let media = vec![0.5_f32; frames * 2];
        let mut mixed = vec![0.0_f32; (start as usize + frames) * 2];
        mix_window((0, &mut mixed), &project, clip, RATE, &media, 0, None).unwrap();
        let begin = start as usize * 2;
        mixed[begin..begin + frames * 2].to_vec()
    }

    #[test]
    fn a_gain_envelope_renders_quieter_in_the_middle_than_at_the_ends() {
        let mixed = render_envelope(Some("0:0dB, 0.5:-6dB, 1:0dB"));
        let frames = mixed.len() / 2;
        let head = mixed[0];
        let middle = mixed[(frames / 2) * 2];
        let tail = mixed[(frames - 1) * 2];
        assert!((head - 0.5).abs() < 1e-6, "head {head}");
        assert!(middle < head * 0.55, "middle {middle} vs head {head}");
        assert!(middle < tail * 0.55, "middle {middle} vs tail {tail}");
        // -6 dB is half amplitude, so the dip lands near half the clip gain.
        assert!(
            (middle - 0.5 * 10.0_f32.powf(-6.0 / 20.0)).abs() < 1e-3,
            "middle {middle}"
        );
        assert!((tail - 0.5).abs() < 1e-3, "tail {tail}");
    }

    #[test]
    fn an_empty_gain_envelope_leaves_the_static_gain_alone() {
        let mixed = render_envelope(None);
        assert!(
            mixed
                .iter()
                .all(|sample| sample.to_bits() == 0.5_f32.to_bits())
        );
        assert!(
            clip_gain(1.0, 0, 0, 0, 123, 1000, &[]).to_bits() == 1.0_f32.to_bits(),
            "an empty envelope is unity gain"
        );
    }
}
