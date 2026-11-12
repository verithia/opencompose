//! Bounce and consolidate: render a track, or a selection of its audio clips,
//! into the project's media folder and put the result back as one audio clip.
//!
//! A bounce is the track's post-insert signal over `[from, to)`, so the clip it
//! publishes carries the insert chain and the track keeps its fader, sends and
//! routing. A consolidate renders the selected audio clips at clip level and
//! keeps the track's inserts, because the clip stays on that track.
//!
//! The render runs on a snapshot with the project lock released, and the
//! project is mutated only once the media file is in place. One undo entry
//! covers the whole edit; every refusal leaves the project and the media folder
//! as they were.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use oc_dsl::ast::{self, AudioClip, BarBeat, Project, TimeRange};
use oc_dsl::timing::ProjectClock;
use oc_host::clap_instance::ClapInstance;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

use super::render::{fork_offline_plugin_instances, move_into_place};
use super::sections::{parse_position, position_text};
use super::shared::{
    ProjectSnapshot, project_root, resync_with_undo, snapshot as project_snapshot,
};

/// Folder a bounce writes into, relative to the project file.
pub const MEDIA_DIR: &str = "media";

/// Where a bounce puts its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BounceMode {
    /// Add `<track> (bounced)` with the clip and mute the source track.
    NewTrack,
    /// Replace the source track's content in the range and bypass its inserts.
    InPlace,
}

/// How a bounce treats the sound that outlasts its range end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BounceTail {
    /// Render past the range end by the chain's delay, its ring-out and the
    /// note release.
    #[default]
    IncludeRelease,
    /// End the media exactly at the range end and cut any release.
    CutExactly,
}

/// What one bounce published, plus the line the status bar reads.
#[derive(Debug, Serialize)]
pub struct BounceResult {
    pub project: ProjectSnapshot,
    pub report: String,
    pub clip: String,
    pub track: String,
    /// Destination track that holds the bounced clip.
    pub target_track: String,
    /// Media path relative to the project file.
    pub media: String,
    /// Absolute path of the media file.
    pub path: String,
    pub mode: BounceMode,
    pub frames: usize,
    pub sample_rate: u32,
    /// Insert effects the bounce baked.
    pub baked: Vec<String>,
}

/// What one consolidate published, plus the line the status bar reads.
#[derive(Debug, Serialize)]
pub struct ConsolidateResult {
    pub project: ProjectSnapshot,
    pub report: String,
    pub clip: String,
    pub track: String,
    pub media: String,
    pub path: String,
    /// Audio clips the new clip replaced.
    pub clips: usize,
    pub frames: usize,
    pub sample_rate: u32,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A media file that exists under its final name only once published.
pub(crate) struct PendingMedia {
    path: PathBuf,
    published: bool,
}

impl PendingMedia {
    fn new(dir: &Path, name: &str) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
        let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            path: dir.join(format!(".{name}.{}-{serial}.tmp", std::process::id())),
            published: false,
        })
    }

    fn write(&self, samples: &[f32], sample_rate: u32) -> Result<(), String> {
        oc_engine::wav::write_stereo_f32_wav(&self.path, samples, sample_rate)
    }

    fn publish(&mut self, target: &Path) -> Result<(), String> {
        move_into_place(&self.path, target)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for PendingMedia {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Everything a bounce reads, captured under the project lock.
pub(crate) struct BounceSnapshot {
    /// Project the render reads; still holds the inserts the bounce bakes.
    pub(crate) project: Project,
    /// Project the bounce publishes.
    pub(crate) next: Project,
    pub(crate) track: String,
    pub(crate) target_track: String,
    pub(crate) clip: String,
    pub(crate) media: String,
    pub(crate) path: PathBuf,
    pub(crate) mode: BounceMode,
    pub(crate) from: BarBeat,
    /// Position the request asked to end at.
    pub(crate) to: BarBeat,
    /// Position the media and the bounced clip end at.
    pub(crate) clip_end: BarBeat,
    pub(crate) from_beat: f64,
    /// Beat the media ends at: `to_beat` plus the rendered tail.
    pub(crate) render_to_beat: f64,
    /// Seconds the media runs past `to_beat`.
    pub(crate) tail_seconds: f64,
    /// True when the range end falls before a note has finished.
    pub(crate) cuts_note: bool,
    /// Insert effects the bounce baked, by device name.
    pub(crate) baked: Vec<String>,
    /// True when a post-insert gain lane was baked with the chain.
    pub(crate) gain_envelope: bool,
    pub(crate) sample_rate: u32,
    pub(crate) soundfont_path: PathBuf,
    pub(crate) resource_root: Option<PathBuf>,
    pub(crate) plugin_instances: HashMap<String, ClapInstance>,
    pub(crate) content_epoch: u64,
    pub(crate) cancel: oc_engine::render_cancel::CancelToken,
    // MUST remain last: native instance owners drop before this worker receipt.
    pub(crate) _native_job: crate::native_jobs::NativeJobGuard,
}

/// A rendered bounce whose media is staged next to its final name.
pub(crate) struct RenderedBounce {
    pub(crate) snapshot: BounceSnapshot,
    pub(crate) media: PendingMedia,
    pub(crate) frames: usize,
}

/// Everything a consolidate reads, captured under the project lock.
pub(crate) struct ConsolidateSnapshot {
    /// Project the render reads: only the selected clips, no track inserts.
    pub(crate) project: Project,
    /// Project the consolidate publishes.
    pub(crate) next: Project,
    pub(crate) track: String,
    pub(crate) clip: String,
    pub(crate) media: String,
    pub(crate) path: PathBuf,
    pub(crate) from: BarBeat,
    pub(crate) to: BarBeat,
    pub(crate) from_frame: u64,
    pub(crate) to_frame: u64,
    pub(crate) replaced: usize,
    pub(crate) sample_rate: u32,
    pub(crate) soundfont_path: PathBuf,
    pub(crate) resource_root: Option<PathBuf>,
    pub(crate) plugin_instances: HashMap<String, ClapInstance>,
    pub(crate) content_epoch: u64,
    pub(crate) cancel: oc_engine::render_cancel::CancelToken,
    // MUST remain last: native instance owners drop before this worker receipt.
    pub(crate) _native_job: crate::native_jobs::NativeJobGuard,
}

/// A rendered consolidate whose media is staged next to its final name.
pub(crate) struct RenderedConsolidate {
    pub(crate) snapshot: ConsolidateSnapshot,
    pub(crate) media: PendingMedia,
    pub(crate) frames: usize,
}

// ============================================================================
// Commands
// ============================================================================

/// Render one track to an audio clip on a new track or in place.
#[tauri::command]
pub async fn bounce_track_to_audio(
    track: String,
    from: String,
    to: String,
    mode: BounceMode,
    tail: Option<BounceTail>,
    state: State<'_, AppState>,
) -> Result<BounceResult, String> {
    bounce_track_off_thread(
        state.inner(),
        track,
        from,
        to,
        mode,
        tail.unwrap_or_default(),
    )
    .await
}

/// The bounce runs off the project lock: snapshot, render, then publish.
pub(crate) async fn bounce_track_off_thread(
    state: &AppState,
    track: String,
    from: String,
    to: String,
    mode: BounceMode,
    tail: BounceTail,
) -> Result<BounceResult, String> {
    let snapshot = capture_bounce_snapshot_with(state, &track, &from, &to, mode, tail)?;
    let rendered = tauri::async_runtime::spawn_blocking(move || render_bounce(snapshot))
        .await
        .map_err(|error| format!("Bounce task failed: {error}"))??;
    let mut inner = state.lock_inner();
    publish_bounce(&mut inner, rendered)
}

/// Render the selected audio clips of one track into one clip.
#[tauri::command]
pub async fn consolidate_selection(
    track: String,
    clip_indices: Vec<usize>,
    state: State<'_, AppState>,
) -> Result<ConsolidateResult, String> {
    consolidate_selection_off_thread(state.inner(), track, clip_indices).await
}

/// The consolidate runs off the project lock: snapshot, render, then publish.
pub(crate) async fn consolidate_selection_off_thread(
    state: &AppState,
    track: String,
    clip_indices: Vec<usize>,
) -> Result<ConsolidateResult, String> {
    let snapshot = capture_consolidate_snapshot(state, &track, clip_indices)?;
    let rendered = tauri::async_runtime::spawn_blocking(move || render_consolidate(snapshot))
        .await
        .map_err(|error| format!("Consolidate task failed: {error}"))??;
    let mut inner = state.lock_inner();
    publish_consolidate(&mut inner, rendered)
}

// ============================================================================
// Snapshot
// ============================================================================

/// Check the request and build the project the bounce publishes; never renders.
#[cfg(test)]
pub(crate) fn capture_bounce_snapshot(
    state: &AppState,
    track: &str,
    from: &str,
    to: &str,
    mode: BounceMode,
) -> Result<BounceSnapshot, String> {
    capture_bounce_snapshot_with(state, track, from, to, mode, BounceTail::default())
}

/// The same snapshot with the caller's tail choice.
pub(crate) fn capture_bounce_snapshot_with(
    state: &AppState,
    track: &str,
    from: &str,
    to: &str,
    mode: BounceMode,
    tail: BounceTail,
) -> Result<BounceSnapshot, String> {
    let inner = state.lock_inner();
    let root =
        project_root(&inner).ok_or("Save the project first: a bounce needs a media folder.")?;
    let from = parse_position(from)?;
    let to = parse_position(to)?;
    let clock = ProjectClock::new(&inner.project.properties)?;
    let from_beat = clock.quarter_notes_at(&from)?;
    let to_beat = clock.quarter_notes_at(&to)?;
    if to_beat <= from_beat {
        return Err(format!(
            "the bounce range ends at or before its start: {} to {}",
            position_text(&from),
            position_text(&to)
        ));
    }
    let track_index = inner
        .project
        .tracks
        .iter()
        .position(|candidate| candidate.name == track)
        .ok_or_else(|| format!("No track named '{track}'."))?;
    let sample_rate = inner.audio.sample_rate;
    oc_engine::encode::validate_sample_rate(sample_rate)?;
    let identity = (inner.revision, inner.content_epoch, inner.file_path.clone());
    drop(inner);
    // Keep native ownership in this earlier local until final construction:
    // every later document guard drops first if validation refuses the request.
    let plugins = fork_offline_plugin_instances(state)?;
    let inner = state.lock_inner();
    plugins.validate(&inner)?;
    if (inner.revision, inner.content_epoch, inner.file_path.clone()) != identity {
        return Err("The project changed during bounce preparation; retry".into());
    }
    let soundfont_path = inner.audio.soundfont_path.clone().unwrap_or_default();
    let media_dir = root.join(MEDIA_DIR);
    let (name, path) = unique_media_path(&media_dir, "bounce", track);

    // The media has to carry what the range end cuts: the insert chain's delay
    // and ring-out plus the release of the last note. The tail lands on the
    // notated grid, so the clip and the media end on the same position.
    let tail_project = inner.project.clone();
    drop(inner);
    let tail_seconds = match tail {
        BounceTail::CutExactly => 0.0,
        BounceTail::IncludeRelease => oc_engine::render::bounce_tail_seconds(
            &tail_project,
            track_index,
            sample_rate,
            &plugins.instances,
            Some(&root),
        )?,
    };
    let inner = state.lock_inner();
    if (inner.revision, inner.content_epoch, inner.file_path.clone()) != identity {
        return Err("The project changed during bounce tail preparation; retry".into());
    }
    let render_to_beat =
        clock.quarter_at_seconds(clock.seconds_at_quarter(to_beat)? + tail_seconds.max(0.0))?;
    let render_to_beat = render_to_beat.max(to_beat);
    let clip_end = covering_position(&clock, render_to_beat)?;
    let render_to_beat = clock.quarter_notes_at(&clip_end)?;
    let tail_seconds =
        clock.seconds_at_quarter(render_to_beat)? - clock.seconds_at_quarter(to_beat)?;
    let cuts_note = note_end_beat(&inner.project, track_index) > to_beat + 1e-9;

    let mut next = inner.project.clone();
    let baked = active_effects(&next, track_index);
    let gain_envelope = next
        .tracks
        .get(track_index)
        .is_some_and(|track| !track.gain_automation.is_empty());
    let target_track;
    match mode {
        BounceMode::NewTrack => {
            target_track = unique_track_name(&next, &format!("{track} (bounced)"));
            let source = next
                .tracks
                .get(track_index)
                .ok_or_else(|| format!("No track named '{track}'."))?;
            let mut bounced = empty_track(&target_track);
            bounced.mix = source.mix.clone();
            bounced.mix.mute = false;
            bounced.mix.solo = false;
            next.tracks.push(bounced);
            let source = next
                .tracks
                .get_mut(track_index)
                .ok_or_else(|| format!("No track named '{track}'."))?;
            source.mix.mute = true;
            source.mix.solo = false;
        }
        BounceMode::InPlace => {
            target_track = track.to_string();
            if next.tracks[track_index]
                .performance
                .as_ref()
                .is_some_and(|performance| !performance.events.is_empty())
            {
                return Err("In-place bounce cannot replace performed note and controller events safely. Bounce onto a new track instead.".into());
            }
            if let Some(chain) = insert_chain(&next, track_index)
                && let Some(span) = content_spans(&next, track_index)
                    .into_iter()
                    .find(|span| span.start < from_beat || span.end > render_to_beat)
            {
                return Err(format!(
                    "{} lies outside {} to {}; bypassing the track's {} would change it. Bounce the whole track, or move that content first.",
                    span.label,
                    position_text(&from),
                    position_text(&clip_end),
                    chain,
                ));
            }
            if let Some(span) = content_spans(&next, track_index).into_iter().find(|span| {
                span.start < render_to_beat
                    && span.end > from_beat
                    && (span.start < from_beat || span.end > render_to_beat)
            }) {
                return Err(format!(
                    "{} crosses the in-place bounce boundary; replacing it would discard content outside {} to {}. Select whole blocks or bounce onto a new track.",
                    span.label,
                    position_text(&from),
                    position_text(&clip_end),
                ));
            }
            let track = next
                .tracks
                .get_mut(track_index)
                .ok_or_else(|| format!("No track named '{track}'."))?;
            for effect in &mut track.effects {
                effect.bypass = true;
            }
            track.gain_automation.clear();
            // The bounce clip covers the tail too, so the source must be
            // cleared to the media end; otherwise its release doubles it.
            remove_overlapping_blocks(&mut next, track_index, from_beat, render_to_beat)?;
        }
    }

    let clip = unique_clip_name(&next, &format!("{track} bounce"));
    next.add_audio_clip(bounce_clip(&clip, &target_track, &name, from, clip_end))
        .map_err(|error| format!("The bounce clip could not be placed: {error}"))?;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid bounce: {errors:?}"))?;

    Ok(BounceSnapshot {
        project: inner.project.clone(),
        next,
        track: track.to_string(),
        target_track,
        clip,
        media: format!("{MEDIA_DIR}/{name}"),
        path,
        mode,
        from,
        to,
        clip_end,
        from_beat,
        render_to_beat,
        tail_seconds,
        cuts_note,
        baked,
        gain_envelope,
        sample_rate,
        soundfont_path,
        resource_root: Some(root),
        plugin_instances: plugins.instances,
        content_epoch: inner.content_epoch,
        cancel: plugins.native_job.cancel_token(),
        _native_job: plugins.native_job,
    })
}

/// Check the selection and build the project the consolidate publishes.
pub(crate) fn capture_consolidate_snapshot(
    state: &AppState,
    track: &str,
    clip_indices: Vec<usize>,
) -> Result<ConsolidateSnapshot, String> {
    let inner = state.lock_inner();
    let root = project_root(&inner)
        .ok_or("Save the project first: a consolidate needs a media folder.")?;
    if clip_indices.is_empty() {
        return Err("Consolidate needs at least one audio clip.".into());
    }
    let track_index = inner
        .project
        .tracks
        .iter()
        .position(|candidate| candidate.name == track)
        .ok_or_else(|| format!("No track named '{track}'."))?;
    let clock = ProjectClock::new(&inner.project.properties)?;

    let mut indices = clip_indices;
    indices.sort_unstable();
    indices.dedup();
    let sample_rate = inner.audio.sample_rate;
    oc_engine::encode::validate_sample_rate(sample_rate)?;
    let (mut from_seconds, mut to_seconds) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut from_frame, mut to_frame) = (u64::MAX, 0);
    for &index in &indices {
        let clip = inner
            .project
            .audio_clip(index)
            .ok_or_else(|| format!("Audio clip {index} out of range."))?;
        if clip.track != track {
            return Err(format!(
                "Audio clip '{}' belongs to track '{}'; consolidate works on one track.",
                clip.name, clip.track
            ));
        }
        let (start, end) = clock.audio_clip_range_seconds(clip)?;
        from_seconds = from_seconds.min(start);
        to_seconds = to_seconds.max(end);
        let (start, end) = clock.audio_clip_range_frames(clip, sample_rate)?;
        from_frame = from_frame.min(start);
        to_frame = to_frame.max(end);
    }
    let from_beat = clock.quarter_at_seconds(from_seconds)?;
    let to_beat = clock.quarter_at_seconds(to_seconds)?;
    let from = clock.bar_beat_floor_at_quarter(from_beat)?;
    let to = covering_position(&clock, to_beat)?;

    // Anything else the track would sound inside the span must not be baked
    // and left in place, so refuse instead of doubling it.
    if let Some(span) = pattern_spans(&inner.project, track_index)
        .into_iter()
        .find(|span| span.start < to_beat && span.end > from_beat)
    {
        return Err(format!(
            "{} overlaps {} to {}; consolidate audio clips only.",
            span.label,
            position_text(&from),
            position_text(&to)
        ));
    }
    if let Some(span) = audio_spans(&inner.project, track, &indices)
        .into_iter()
        .find(|(_, span)| span.start < to_beat && span.end > from_beat)
        .map(|(_, span)| span)
    {
        return Err(format!(
            "{} overlaps {} to {} but is not in the selection.",
            span.label,
            position_text(&from),
            position_text(&to)
        ));
    }

    let selected: Vec<AudioClip> = indices
        .iter()
        .filter_map(|index| inner.project.audio_clip(*index))
        .cloned()
        .collect();
    // The render reads the clips at clip level: no instrument events, no
    // insert chain and no other clip on the track. The track keeps its chain,
    // so the published clip runs through it exactly once.
    let mut render_project = inner.project.clone();
    render_project
        .set_audio_clips(selected)
        .map_err(|error| format!("Consolidate selection: {error}"))?;
    let render_track = render_project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("No track named '{track}'."))?;
    render_track.instrument = ast::InstrumentPath::new("audio", "clip");
    render_track.preset = None;
    render_track.effects.clear();
    render_track.gain_automation.clear();
    render_track.set_automation_lanes(std::iter::empty());
    // The selected audio clips stay in the pool; only pattern content goes.
    render_track.set_pattern_clips(std::iter::empty());
    render_track.phrases.clear();
    render_track.patterns.clear();
    render_track.inline_patterns.clear();
    render_track.performance = None;

    let identity = (inner.revision, inner.content_epoch, inner.file_path.clone());
    drop(inner);
    // Keep native ownership in this earlier local until final construction:
    // every later document guard drops first if validation refuses the request.
    let plugins = fork_offline_plugin_instances(state)?;
    let inner = state.lock_inner();
    plugins.validate(&inner)?;
    if (inner.revision, inner.content_epoch, inner.file_path.clone()) != identity {
        return Err("The project changed during bounce preparation; retry".into());
    }
    let soundfont_path = inner.audio.soundfont_path.clone().unwrap_or_default();
    let media_dir = root.join(MEDIA_DIR);
    let (name, path) = unique_media_path(&media_dir, "consolidate", track);

    let clip_name = unique_clip_name(&inner.project, &format!("{track} consolidate"));
    let insert_at = (0..indices[0])
        .filter(|index| !indices.contains(index))
        .count();
    let mut next = inner.project.clone();
    let mut kept = Vec::with_capacity(next.audio_clip_count());
    for (index, clip) in next.audio_clips().into_iter().enumerate() {
        if !indices.contains(&index) {
            kept.push(clip.clone());
        }
    }
    let insert_at = insert_at.min(kept.len());
    let mut consolidated = bounce_clip(&clip_name, track, &name, from, to);
    consolidated.placement = ast::AudioClipPlacement {
        start_offset_seconds: from_seconds
            - clock.seconds_at_quarter(clock.quarter_notes_at(&from)?)?,
        end_offset_seconds: to_seconds - clock.seconds_at_quarter(clock.quarter_notes_at(&to)?)?,
    };
    // New anchors can change whether a boundary uses legacy floor or explicit
    // rounding. Keep the exact frame interval the selected clips actually sound.
    preserve_consolidated_frames(&clock, &mut consolidated, sample_rate, from_frame, to_frame)?;
    kept.insert(insert_at, consolidated);
    next.set_audio_clips(kept)
        .map_err(|error| format!("Invalid consolidate: {error}"))?;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid consolidate: {errors:?}"))?;

    Ok(ConsolidateSnapshot {
        project: render_project,
        next,
        track: track.to_string(),
        clip: clip_name,
        media: format!("{MEDIA_DIR}/{name}"),
        path,
        from,
        to,
        from_frame,
        to_frame,
        replaced: indices.len(),
        sample_rate,
        soundfont_path,
        resource_root: Some(root),
        plugin_instances: plugins.instances,
        content_epoch: inner.content_epoch,
        cancel: plugins.native_job.cancel_token(),
        _native_job: plugins.native_job,
    })
}

/// Preserve the rendered interval if new notated anchors change endpoint rounding.
fn preserve_consolidated_frames(
    clock: &ProjectClock,
    clip: &mut AudioClip,
    sample_rate: u32,
    start: u64,
    end: u64,
) -> Result<(), String> {
    let (actual_start, actual_end) = clock.audio_clip_range_frames(clip, sample_rate)?;
    if actual_start != start {
        clip.placement.start_offset_seconds = start as f64 / f64::from(sample_rate)
            - clock.seconds_at_quarter(clock.quarter_notes_at(&clip.time_range.start)?)?;
    }
    if actual_end != end {
        clip.placement.end_offset_seconds = end as f64 / f64::from(sample_rate)
            - clock.seconds_at_quarter(clock.quarter_notes_at(&clip.time_range.end)?)?;
    }
    if clock.audio_clip_range_frames(clip, sample_rate)? != (start, end) {
        return Err("The consolidated clip cannot preserve the selected sample boundaries.".into());
    }
    Ok(())
}

/// The audio clip a bounce or a consolidate publishes.
fn bounce_clip(name: &str, track: &str, file: &str, from: BarBeat, to: BarBeat) -> AudioClip {
    AudioClip {
        name: name.to_string(),
        track: track.to_string(),
        file: format!("{MEDIA_DIR}/{file}"),
        time_range: TimeRange {
            start: from,
            end: to,
        },
        placement: ast::AudioClipPlacement::default(),
        offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
        invert: false,
        crossfade_seconds: 0.0,
        stretch: 1.0,
        stretch_mode: ast::StretchMode::default(),
        tempo_follow: false,
        pitch: 0.0,
        formant: 0.0,
        bypass_processing: false,
        preserve_formants: false,
        warp_markers: Vec::new(),
        loop_seconds: 0.0,
        take_group: None,
        take: None,
        muted: false,
        gain_envelope: Vec::new(),
    }
}

// ============================================================================
// Render (no project lock)
// ============================================================================

/// Render the snapshot's range and stage the media file; runs off-lock.
pub(crate) fn render_bounce(mut snapshot: BounceSnapshot) -> Result<RenderedBounce, String> {
    if snapshot.cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    let (media, frames) = stage_media(
        &snapshot.project,
        &snapshot.soundfont_path,
        snapshot.sample_rate,
        &mut snapshot.plugin_instances,
        snapshot.resource_root.as_deref(),
        &snapshot.track,
        snapshot.from_beat,
        snapshot.render_to_beat,
        &snapshot.path,
        snapshot.cancel.clone(),
    )?;
    // Publish runs with AppState held. Retire native audio owners here on the
    // worker; their main-thread finalizers queue before the receipt can finish.
    snapshot.plugin_instances.clear();
    Ok(RenderedBounce {
        snapshot,
        media,
        frames,
    })
}

/// Render the snapshot's range and stage the media file; runs off-lock.
pub(crate) fn render_consolidate(
    mut snapshot: ConsolidateSnapshot,
) -> Result<RenderedConsolidate, String> {
    if snapshot.cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    let (media, frames) = stage_media_frames(
        &snapshot.project,
        &snapshot.soundfont_path,
        snapshot.sample_rate,
        &mut snapshot.plugin_instances,
        snapshot.resource_root.as_deref(),
        &snapshot.track,
        snapshot.from_frame,
        snapshot.to_frame,
        &snapshot.path,
        snapshot.cancel.clone(),
    )?;
    // The publication snapshot carries metadata and the receipt, not native
    // audio owners that could run stop_processing under the document lock.
    snapshot.plugin_instances.clear();
    Ok(RenderedConsolidate {
        snapshot,
        media,
        frames,
    })
}

/// One track's tap over the snapshot's range, written to the staged media file.
#[allow(clippy::too_many_arguments)]
fn stage_media(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    track: &str,
    from_beat: f64,
    to_beat: f64,
    path: &Path,
    cancel: oc_engine::render_cancel::CancelToken,
) -> Result<(PendingMedia, usize), String> {
    let clock = ProjectClock::new(&project.properties)?;
    let from_frame = clock.sample_at_quarter(from_beat, sample_rate)?;
    let to_frame = clock.sample_at_quarter(to_beat, sample_rate)?;
    stage_media_frames(
        project,
        soundfont_path,
        sample_rate,
        plugin_instances,
        resource_root,
        track,
        from_frame,
        to_frame,
        path,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_media_frames(
    project: &Project,
    soundfont_path: &Path,
    sample_rate: u32,
    plugin_instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
    track: &str,
    from_frame: u64,
    to_frame: u64,
    path: &Path,
    cancel: oc_engine::render_cancel::CancelToken,
) -> Result<(PendingMedia, usize), String> {
    let index = project
        .tracks
        .iter()
        .position(|candidate| candidate.name == track)
        .ok_or_else(|| format!("No track named '{track}'."))?;
    let samples = oc_engine::render::render_track_post_fx_with_options(
        project,
        soundfont_path,
        plugin_instances,
        oc_engine::render::TrackRenderRange {
            track: index,
            start_sample: from_frame,
            end_sample: to_frame,
        },
        oc_engine::render::RenderOptions {
            resource_root,
            cancel: Some(cancel.clone()),
            ..oc_engine::render::RenderOptions::new(
                sample_rate,
                crate::gui_settings::load().worker_options(),
            )
        },
    )?;
    if cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    let frames = samples.len() / 2;
    let (dir, name) = media_target(path)?;
    let media = PendingMedia::new(&dir, &name)?;
    media.write(&samples, sample_rate)?;
    Ok((media, frames))
}

/// Folder and file name the staged media publishes under.
fn media_target(path: &Path) -> Result<(PathBuf, String), String> {
    let dir = path
        .parent()
        .ok_or("the bounce media path has no parent")?
        .to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the bounce media path has no file name")?
        .to_string();
    Ok((dir, name))
}

// ============================================================================
// Publish
// ============================================================================

/// Put the media file in place and replace the project in one undo entry.
pub(crate) fn publish_bounce(
    inner: &mut AppStateInner,
    rendered: RenderedBounce,
) -> Result<BounceResult, String> {
    let RenderedBounce {
        snapshot,
        mut media,
        frames,
    } = rendered;
    if snapshot.cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    if inner.content_epoch != snapshot.content_epoch {
        return Err(format!(
            "the project changed during the bounce; nothing was published and {} was removed.",
            snapshot.media
        ));
    }
    media.publish(&snapshot.path)?;
    let report = bounce_report(&snapshot, frames);
    let old = std::mem::replace(&mut inner.project, snapshot.next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("bounce track", UndoScope::Project),
    )?;
    Ok(BounceResult {
        project: project_snapshot(inner),
        report,
        clip: snapshot.clip,
        track: snapshot.track,
        target_track: snapshot.target_track,
        media: snapshot.media,
        path: snapshot.path.display().to_string(),
        mode: snapshot.mode,
        frames,
        sample_rate: snapshot.sample_rate,
        baked: snapshot.baked,
    })
}

/// Put the media file in place and replace the project in one undo entry.
pub(crate) fn publish_consolidate(
    inner: &mut AppStateInner,
    rendered: RenderedConsolidate,
) -> Result<ConsolidateResult, String> {
    let RenderedConsolidate {
        snapshot,
        mut media,
        frames,
    } = rendered;
    if snapshot.cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    if inner.content_epoch != snapshot.content_epoch {
        return Err(format!(
            "the project changed during the consolidate; nothing was published and {} was removed.",
            snapshot.media
        ));
    }
    media.publish(&snapshot.path)?;
    let report = format!(
        "Consolidated {} audio clip{} on '{}' into {} ({:.2}s, {} frames at {} Hz); one clip replaces them over {} to {}.",
        snapshot.replaced,
        if snapshot.replaced == 1 { "" } else { "s" },
        snapshot.track,
        snapshot.media,
        frames as f64 / f64::from(snapshot.sample_rate),
        frames,
        snapshot.sample_rate,
        position_text(&snapshot.from),
        position_text(&snapshot.to),
    );
    let old = std::mem::replace(&mut inner.project, snapshot.next);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("consolidate clips", UndoScope::Project),
    )?;
    Ok(ConsolidateResult {
        project: project_snapshot(inner),
        report,
        clip: snapshot.clip,
        track: snapshot.track,
        media: snapshot.media,
        path: snapshot.path.display().to_string(),
        clips: snapshot.replaced,
        frames,
        sample_rate: snapshot.sample_rate,
    })
}

/// The one status line a bounce reports; it names what was baked and bypassed.
fn bounce_report(snapshot: &BounceSnapshot, frames: usize) -> String {
    let seconds = frames as f64 / f64::from(snapshot.sample_rate);
    let mut baked = snapshot.baked.clone();
    if snapshot.gain_envelope {
        baked.push("gain envelope".to_string());
    }
    let chain = (!baked.is_empty()).then(|| format!("insert chain ({})", baked.join(", ")));
    let tail = if snapshot.tail_seconds > 0.0 {
        format!(
            " The media runs {:.2}s past {} to {} for the insert delay, the ring-out and the release, so the clip carries the tail.",
            snapshot.tail_seconds,
            position_text(&snapshot.to),
            position_text(&snapshot.clip_end),
        )
    } else if snapshot.cuts_note {
        format!(
            " It ends exactly at {} and cuts the release of a sounding note.",
            position_text(&snapshot.to),
        )
    } else {
        String::new()
    };
    match snapshot.mode {
        BounceMode::NewTrack => format!(
            "Bounced '{}' to {} ({:.2}s, {} frames at {} Hz) onto '{}'; the source track is muted. {}{}",
            snapshot.track,
            snapshot.media,
            seconds,
            frames,
            snapshot.sample_rate,
            snapshot.target_track,
            match &chain {
                Some(chain) => format!("Baked {chain}."),
                None => "No insert chain was baked.".to_string(),
            },
            tail,
        ),
        BounceMode::InPlace => format!(
            "Bounced '{}' to {} in place over {} to {} ({:.2}s, {} frames at {} Hz). {}{}",
            snapshot.track,
            snapshot.media,
            position_text(&snapshot.from),
            position_text(&snapshot.to),
            seconds,
            frames,
            snapshot.sample_rate,
            match &chain {
                Some(chain) => format!("Bypassed {chain}; the bounce carries it."),
                None => "No insert chain was bypassed.".to_string(),
            },
            tail,
        ),
    }
}

/// Device names of the track's active insert effects.
fn active_effects(project: &Project, track_index: usize) -> Vec<String> {
    project
        .tracks
        .get(track_index)
        .map(|track| {
            track
                .effects
                .iter()
                .filter(|effect| !effect.bypass)
                .map(effect_name)
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================================
// Content spans
// ============================================================================

/// The notated position that covers `quarter`: `quarter` itself when it sits on
/// the grid, else the next subdivision. A bounce end and its media have to land
/// on a position the document can write.
pub(super) fn covering_position(clock: &ProjectClock, quarter: f64) -> Result<BarBeat, String> {
    if let Ok(position) = clock.bar_beat_at_quarter(quarter) {
        return Ok(position);
    }
    let floor = clock.bar_beat_floor_at_quarter(quarter)?;
    let step = 1.0 / f64::from(clock.meter_at_bar(floor.bar)?.denominator);
    clock.bar_beat_at_quarter(clock.quarter_notes_at(&floor)? + step)
}

/// Quarter position where the track's notes stop sounding.
///
/// Notes, not blocks: a note may start before a phrase ends and hold past it,
/// and its release outlasts both. Audio clips do not release, so they are not
/// part of this bound.
fn note_end_beat(project: &Project, track_index: usize) -> f64 {
    let Ok(clock) = ProjectClock::new(&project.properties) else {
        return 0.0;
    };
    let Some(track) = project.tracks.get(track_index) else {
        return 0.0;
    };
    let mut end = 0.0_f64;
    for phrase in &track.phrases {
        let Some(range) = &phrase.time_range else {
            continue;
        };
        let (Ok(start), Some(span)) = (
            clock.quarter_notes_at(&range.start),
            note_span(&phrase.events),
        ) else {
            continue;
        };
        end = end.max(start + span);
    }
    for clip in track.pattern_clips() {
        let Ok(start) = clock.quarter_notes_at(&clip.time_range.start) else {
            continue;
        };
        let span = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
            .map(|pattern| pattern.content_quarters())
            .unwrap_or(0.0);
        end = end.max(start + span);
    }
    if let Some(performance) = &track.performance {
        for event in &performance.events {
            end = end.max(event.quarter);
        }
    }
    end
}

/// Quarter notes the written notes of one block span, or `None` without notes.
fn note_span(events: &[ast::NoteEvent]) -> Option<f64> {
    events
        .iter()
        .map(|event| event.start_beat + event.duration.to_beats())
        .reduce(f64::max)
}

/// One placed piece of track content and its quarter-note span.
struct BlockSpan {
    start: f64,
    end: f64,
    label: String,
}

/// Every placed block on the track except its audio clips.
fn pattern_spans(project: &Project, track_index: usize) -> Vec<BlockSpan> {
    let Ok(clock) = ProjectClock::new(&project.properties) else {
        return Vec::new();
    };
    let Some(track) = project.tracks.get(track_index) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for clip in track.pattern_clips() {
        if let (Ok(start), Ok(end)) = (
            clock.quarter_notes_at(&clip.time_range.start),
            clock.quarter_notes_at(&clip.time_range.end),
        ) {
            spans.push(BlockSpan {
                start,
                end,
                label: format!("clip '{}'", clip.pattern),
            });
        }
    }
    for phrase in &track.phrases {
        let Some(range) = &phrase.time_range else {
            continue;
        };
        if let (Ok(start), Ok(end)) = (
            clock.quarter_notes_at(&range.start),
            clock.quarter_notes_at(&range.end),
        ) {
            spans.push(BlockSpan {
                start,
                end,
                label: format!("phrase '{}'", phrase.name),
            });
        }
    }
    for pattern in &track.patterns {
        let Some(range) = &pattern.time_range else {
            continue;
        };
        let mut end = range.start;
        end.bar = end.bar.saturating_add(pattern.repeat);
        if let (Ok(start), Ok(end)) = (
            clock.quarter_notes_at(&range.start),
            clock.quarter_notes_at(&end),
        ) {
            spans.push(BlockSpan {
                start,
                end,
                label: format!("pattern '{}'", pattern.name),
            });
        }
    }
    if let Some(performance) = &track.performance
        && !performance.events.is_empty()
    {
        let start = performance
            .events
            .iter()
            .map(|event| event.quarter)
            .fold(f64::INFINITY, f64::min);
        let end = performance
            .events
            .iter()
            .map(|event| event.quarter)
            .fold(f64::NEG_INFINITY, f64::max);
        spans.push(BlockSpan {
            start,
            end,
            label: format!("performance on '{}'", track.name),
        });
    }
    spans
}

/// Every audio clip on the track whose index is not in `skip`.
fn audio_spans(project: &Project, track: &str, skip: &[usize]) -> Vec<(usize, BlockSpan)> {
    let Ok(clock) = ProjectClock::new(&project.properties) else {
        return Vec::new();
    };
    project
        .audio_clips()
        .iter()
        .enumerate()
        .filter(|(index, clip)| clip.track == track && !skip.contains(index))
        .filter_map(|(index, clip)| {
            let (start, end) = clock.audio_clip_range_seconds(clip).ok()?;
            let start = clock.quarter_at_seconds(start).ok()?;
            let end = clock.quarter_at_seconds(end).ok()?;
            Some((
                index,
                BlockSpan {
                    start,
                    end,
                    label: format!("audio clip '{}'", clip.name),
                },
            ))
        })
        .collect()
}

/// Every placed piece of track content: blocks and the track's audio clips.
fn content_spans(project: &Project, track_index: usize) -> Vec<BlockSpan> {
    let Some(track) = project.tracks.get(track_index) else {
        return Vec::new();
    };
    let mut spans = pattern_spans(project, track_index);
    spans.extend(
        audio_spans(project, &track.name, &[])
            .into_iter()
            .map(|(_, span)| span),
    );
    spans
}

/// Device name a report or a refusal uses for one effect.
fn effect_name(effect: &ast::Effect) -> String {
    match effect.effect_type.device_id() {
        Some(id) => id.to_string(),
        None => match &effect.effect_type {
            ast::EffectType::Plugin(plugin) => plugin.clone(),
            _ => "plugin".to_string(),
        },
    }
}

/// The insert chain a bounce bakes, described for a refusal.
fn insert_chain(project: &Project, track_index: usize) -> Option<String> {
    let track = project.tracks.get(track_index)?;
    let mut parts: Vec<String> = track
        .effects
        .iter()
        .filter(|effect| !effect.bypass)
        .map(effect_name)
        .collect();
    if !track.gain_automation.is_empty() {
        parts.push("gain envelope".to_string());
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("insert chain ({})", parts.join(", ")))
}

/// Remove every block of the track that overlaps `[from, to)`.
fn remove_overlapping_blocks(
    project: &mut Project,
    track_index: usize,
    from: f64,
    to: f64,
) -> Result<(), String> {
    let clock = ProjectClock::new(&project.properties)?;
    let name = project
        .tracks
        .get(track_index)
        .map(|track| track.name.clone())
        .ok_or_else(|| format!("track {track_index} out of range"))?;
    let keep = |start: f64, end: f64| end <= from || start >= to;
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("track {track_index} out of range"))?;
    let mut clips = Vec::with_capacity(track.pattern_clip_count());
    for clip in track.pattern_clips() {
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        let end = clock.quarter_notes_at(&clip.time_range.end)?;
        if keep(start, end) {
            clips.push(clip.clone());
        }
    }
    track.set_pattern_clips(clips);
    let mut phrases = Vec::with_capacity(track.phrases.len());
    for phrase in track.phrases.drain(..) {
        let keep_phrase = match &phrase.time_range {
            Some(range) => keep(
                clock.quarter_notes_at(&range.start)?,
                clock.quarter_notes_at(&range.end)?,
            ),
            None => true,
        };
        if keep_phrase {
            phrases.push(phrase);
        }
    }
    track.phrases = phrases;
    let mut patterns = Vec::with_capacity(track.patterns.len());
    for pattern in track.patterns.drain(..) {
        let keep_pattern = match &pattern.time_range {
            Some(range) => {
                let mut end = range.start;
                end.bar = end.bar.saturating_add(pattern.repeat);
                keep(
                    clock.quarter_notes_at(&range.start)?,
                    clock.quarter_notes_at(&end)?,
                )
            }
            None => true,
        };
        if keep_pattern {
            patterns.push(pattern);
        }
    }
    track.patterns = patterns;
    let mut audio = Vec::with_capacity(project.audio_clip_count());
    let clips: Vec<AudioClip> = project.audio_clips().into_iter().cloned().collect();
    for clip in clips {
        let (start, end) = clock.audio_clip_range_seconds(&clip)?;
        let keep_clip = clip.track != name
            || keep(
                clock.quarter_at_seconds(start)?,
                clock.quarter_at_seconds(end)?,
            );
        if keep_clip {
            audio.push(clip);
        }
    }
    project
        .set_audio_clips(audio)
        .map_err(|error| format!("Invalid bounce: {error}"))?;
    Ok(())
}

// ============================================================================
// Naming
// ============================================================================

/// A file-safe track slug, matching the take writer.
fn track_slug(track: &str) -> String {
    let slug: String = track
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_ascii_lowercase();
    if slug.is_empty() {
        "track".to_string()
    } else {
        slug
    }
}

/// `media/<prefix>-<slug>-<n>.wav` with the first free `n`.
fn unique_media_path(dir: &Path, prefix: &str, track: &str) -> (String, PathBuf) {
    let slug = track_slug(track);
    for index in 1.. {
        let name = format!("{prefix}-{slug}-{index}.wav");
        let path = dir.join(&name);
        if !path.exists() {
            return (name, path);
        }
    }
    unreachable!("the media counter always finds a free name")
}

/// A clip name no other audio clip uses.
fn unique_clip_name(project: &Project, base: &str) -> String {
    if !project.audio_clips().iter().any(|clip| clip.name == base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base} {suffix}");
        if !project
            .audio_clips()
            .iter()
            .any(|clip| clip.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("the clip counter always finds a free name")
}

/// A track name no other track uses.
fn unique_track_name(project: &Project, base: &str) -> String {
    if !project.tracks.iter().any(|track| track.name == base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base} {suffix}");
        if !project.tracks.iter().any(|track| track.name == candidate) {
            return candidate;
        }
    }
    unreachable!("the track counter always finds a free name")
}

/// A clip-only track that carries no instrument, clips or inserts.
fn empty_track(name: &str) -> ast::Track {
    ast::Track {
        name: name.to_string(),
        instrument: ast::InstrumentPath::new("audio", "clip"),
        preset: None,
        engine: None,
        mix: ast::MixState::default(),
        effects: Vec::new(),
        phrases: Vec::new(),
        patterns: Vec::new(),
        inline_patterns: Vec::new(),
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: Vec::new(),
    }
}
