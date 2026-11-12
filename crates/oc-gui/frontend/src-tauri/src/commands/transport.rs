//! Playback transport, previews, audio devices, and audio configuration.

use oc_dsl::ast::BarBeat;
use std::sync::atomic::Ordering;
use tauri::{Manager, State};

use crate::audio::{self, AudioConfig, AudioDeviceInfo, PlayDispatch, TransportSnapshot};
use crate::state::AppState;

use super::plugin_host::{collect_plugin_render_warnings, prepare_clap_playback};
use super::shared::run_blocking;

/// The record fields a snapshot reports: armed track, take, arm chip and punch.
fn with_record(
    mut snapshot: TransportSnapshot,
    inner: &crate::state::AppStateInner,
) -> TransportSnapshot {
    snapshot.record = crate::record::state_of(inner);
    snapshot.live_synth_sounding = inner.audio.live_synth_sounding();
    snapshot.native_monitoring = inner.audio.input_monitor_active()
        || inner.midi_live.monitoring_active()
        || inner
            .vst3_editor_keepalives
            .values()
            .any(|editor| !editor.window_closed());
    snapshot
}

/// Preview a preset — queues the audition and returns before rendering starts.
#[tauri::command]
pub async fn preview_preset(
    preset_name: String,
    start_paused: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    oc_diag::diag_info!("cmd: preview_preset preset={preset_name:?}");
    let worker = app.clone();
    with_native_start(app, move |_| {
        crate::audition::start(worker, preset_name, start_paused.unwrap_or(false))
    })
    .await
}

/// Turn one browser candidate's parameter batch into parsed engine values.
fn parse_candidate_params(
    params: Vec<(String, Option<String>)>,
) -> Result<Vec<(String, oc_dsl::ast::ParamValue)>, String> {
    let unknown = super::synth::unknown_synth_params(&params);
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown OpenSynth parameter(s): {}",
            unknown.join(", ")
        ));
    }
    let mut candidate = Vec::with_capacity(params.len());
    for (name, value) in params {
        let key = name.trim();
        if key.is_empty() {
            return Err("Factory patch parameter name cannot be empty".to_string());
        }
        if let Some(value) = value
            && !value.trim().is_empty()
        {
            candidate.push((key.to_owned(), super::shared::parse_param_value(&value)?));
        }
    }
    Ok(candidate)
}

/// Audition a factory patch candidate without loading it into the project.
#[tauri::command]
pub async fn audition_factory_patch(
    params: Vec<(String, Option<String>)>,
    effects: Option<Vec<oc_dsl::ast::Effect>>,
    label: Option<String>,
    start_paused: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    oc_diag::diag_info!(
        "cmd: audition_factory_patch label={label:?} params={}",
        params.len()
    );
    let candidate = parse_candidate_params(params)?;
    let worker = app.clone();
    with_native_start(app, move |_| {
        crate::audition::start_factory(
            worker,
            candidate,
            effects.unwrap_or_default(),
            label,
            start_paused.unwrap_or(false),
        )
    })
    .await
}

/// Render the same fixed segment for two candidates, align B to A and switch
/// between them. The whole render runs off the webview thread.
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri preserves the flat IPC argument keys consumed by existing clients"
)]
/// Compare two factory patch parameter sets.
#[tauri::command]
pub async fn compare_factory_patches(
    a_label: String,
    a_params: Vec<(String, Option<String>)>,
    a_effects: Option<Vec<oc_dsl::ast::Effect>>,
    b_label: String,
    b_params: Vec<(String, Option<String>)>,
    b_effects: Option<Vec<oc_dsl::ast::Effect>>,
    start_paused: Option<bool>,
    app: tauri::AppHandle,
) -> Result<crate::ab_audition::AbSnapshot, String> {
    oc_diag::diag_info!(
        "cmd: compare_factory_patches a={a_label:?} ({} params) b={b_label:?} ({} params)",
        a_params.len(),
        b_params.len()
    );
    let a = crate::ab_audition::AbCandidate {
        label: if a_label.trim().is_empty() {
            "A".into()
        } else {
            a_label
        },
        params: parse_candidate_params(a_params)?,
        effects: a_effects.unwrap_or_default(),
    };
    let b = crate::ab_audition::AbCandidate {
        label: if b_label.trim().is_empty() {
            "B".into()
        } else {
            b_label
        },
        params: parse_candidate_params(b_params)?,
        effects: b_effects.unwrap_or_default(),
    };
    with_native_start(app, move |state| {
        let reservation =
            crate::ab_audition::reserve(state, &a, &b, start_paused.unwrap_or(false))?;
        crate::ab_audition::compare_reserved(state, a, b, reservation)
    })
    .await
}

/// Flip the audition stream between the two rendered takes. When the previous
/// pass has already finished, the same pair starts again from the top.
#[tauri::command]
pub async fn switch_ab<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<crate::ab_audition::AbSnapshot, String> {
    oc_diag::diag_info!("cmd: switch_ab");
    with_native_start(app, crate::ab_audition::switch).await
}

/// Preview one pattern on the audition stream; the transport is left alone.
#[tauri::command]
pub async fn preview_pattern(
    pattern_name: String,
    start_paused: Option<bool>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    with_native_start(app, move |state| {
        preview_pattern_inner(pattern_name, start_paused.unwrap_or(false), state)
    })
    .await
}

fn preview_pattern_inner(
    pattern_name: String,
    start_paused: bool,
    state: &AppState,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let started_ns = crate::first_block::now_ns();
    let (generation, project, root, spec, soundfont, io) = {
        let mut inner = state.lock_inner();
        inner.audio.replace_private_audition(start_paused)?;
        let generation = state.audition.begin(&pattern_name);
        (
            generation,
            inner.project.clone(),
            inner
                .file_path
                .as_deref()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf),
            inner.audio.audio_io.spec(),
            inner.audio.soundfont_path.clone().unwrap_or_default(),
            inner.audio.audio_io.clone(),
        )
    };
    let result: Result<(), String> = (|| {
        let audition = audio::pattern_audition_project(&project, &pattern_name)?;
        let project = oc_engine::block_source::prepare_project(&audition, root.as_deref())?;
        if !state.audition.is_current(generation) {
            return Ok(());
        }
        #[cfg(test)]
        let simulated = state.audio_simulated();
        #[cfg(not(test))]
        let simulated = false;
        let stream = audio::AudioState::open_audition_render(
            &io,
            &spec,
            soundfont,
            project,
            simulated,
            started_ns,
            crate::first_block::StartPath::PatternPreview,
        )?;
        let mut inner = state.lock_inner();
        if !state.audition.is_current(generation) {
            return Ok(());
        }
        inner.audio.install_audition_render(stream)?;
        state.audition.complete(generation, started);
        Ok(())
    })();
    if let Err(error) = &result {
        state.audition.fail(generation, error.clone());
    }
    result
}

/// Silence any audition without changing the transport.
#[tauri::command]
pub fn stop_audition(state: State<AppState>) -> Result<(), String> {
    state.preview_focus.cancel_pending_start();
    let mut inner = state.lock_inner();
    state.audition.cancel_all();
    state.audition_pause_intent.store(false, Ordering::Release);
    inner.audio.stop_audition();
    Ok(())
}

/// Resume only a matching paused solo preview; normal Play restores the arrangement.
#[tauri::command]
pub async fn resume_track_preview<R: tauri::Runtime>(
    track_index: usize,
    app: tauri::AppHandle<R>,
) -> Result<TransportSnapshot, String> {
    with_native_start(app, move |state| {
        resume_track_preview_inner(track_index, state)
    })
    .await
}

fn resume_track_preview_inner(
    track_index: usize,
    state: &AppState,
) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    if track_index >= inner.project.tracks.len() {
        return Err(format!("Track {track_index} out of range"));
    }
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let inner = &mut *inner;
    inner.audio.resume_track_preview_for_project(
        track_index,
        &inner.project,
        &mut inner.plugin_instances,
    )?;
    inner.audio.transport_snapshot(&clock)
}

#[tauri::command]
pub fn pause_audition(state: State<AppState>) -> Result<TransportSnapshot, String> {
    state.preview_focus.cancel_pending_start();
    {
        let mut inner = state.lock_inner();
        state.audition_pause_intent.store(true, Ordering::Release);
        inner.audio.pause_private_audition();
    }
    get_transport_state(state)
}

#[tauri::command]
pub async fn resume_audition<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<TransportSnapshot, String> {
    with_native_start(app, |state| {
        {
            let mut inner = state.lock_inner();
            state.audition_pause_intent.store(false, Ordering::Release);
            inner.audio.resume_private_audition()?;
        }
        transport_state_inner(state)
    })
    .await
}

/// Preview a single track (solo render). Returns transport state with start position.
#[tauri::command]
pub async fn preview_track<R: tauri::Runtime>(
    track_index: usize,
    start_paused: Option<bool>,
    app: tauri::AppHandle<R>,
) -> Result<TransportSnapshot, String> {
    with_native_start(app, move |state| {
        preview_track_inner(track_index, start_paused.unwrap_or(false), state)
    })
    .await
}

pub(super) fn preview_track_inner(
    track_index: usize,
    start_paused: bool,
    state: &AppState,
) -> Result<TransportSnapshot, String> {
    {
        let inner = state.lock_inner();
        inner.audio.refuse_during_take("previewing a track")?;
        if track_index >= inner.project.tracks.len() {
            return Err(format!("Track {} out of range", track_index));
        }
    }
    let prepared = prepare_clap_playback(state)?;
    let mut inner = state.lock_inner();
    if !prepared.matches(&inner) {
        return Err("The project changed during native preview preparation; retry".into());
    }
    inner.audio.refuse_during_take("previewing a track")?;
    state.audition.cancel_all();
    if track_index >= inner.project.tracks.len() {
        return Err(format!("Track {} out of range", track_index));
    }

    let project = prepared.project;
    let warnings = prepared.warnings;
    let resource_root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    // Native editor state is already embedded in this immutable project.
    let mut instances = std::collections::HashMap::new();

    let result = inner.audio.preview_track_with_intent(
        &project,
        track_index,
        &mut instances,
        resource_root.as_deref(),
        start_paused,
    );
    result?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let mut snapshot = inner.audio.transport_snapshot(&clock)?;
    snapshot.did_render = true;
    snapshot.warnings = warnings;
    Ok(with_record(snapshot, &inner))
}

/// Solo the exact audio clip while preserving its source dependencies and mix.
/// Siblings stay in the snapshot for references such as vocal tuning guides.
pub(crate) fn isolated_audio_clip_preview_project(
    project: &oc_dsl::ast::Project,
    track_index: usize,
    start: BarBeat,
    end: BarBeat,
    clip_name: &str,
) -> Result<oc_dsl::ast::Project, String> {
    let track = project
        .tracks
        .get(track_index)
        .ok_or_else(|| format!("Track {track_index} out of range"))?;
    let mut matching = track.audio_clips().filter(|clip| {
        clip.name == clip_name && clip.time_range.start == start && clip.time_range.end == end
    });
    let clip = matching
        .next()
        .filter(|_| matching.next().is_none())
        .ok_or_else(|| {
            format!("Audio clip '{clip_name}' is no longer at the selected placement")
        })?;
    if track.mix.mute || clip.muted {
        return Err(format!("Audio clip '{clip_name}' or its track is muted"));
    }

    let mut solo = audio::solo_preview_project(project, track_index)?;
    let track = &mut solo.tracks[track_index];
    track.phrases.clear();
    track.patterns.clear();
    track.inline_patterns.clear();
    track.set_pattern_clips(std::iter::empty());
    track.performance = None;
    for clip in track.audio_clips_mut() {
        if clip.name != clip_name || clip.time_range.start != start || clip.time_range.end != end {
            clip.muted = true;
        }
    }
    // The menu names an authored placement. Section playback can move or copy
    // that placement, while its edits and clock are authored at this range.
    solo.sections.clear();
    Ok(solo)
}

/// Audition one timeline block's own range on the private stream.
///
/// An audio clip name restricts the selected track to that clip's media. The
/// worker streams the bounded mix while the arrangement cursor stays parked.
#[tauri::command]
pub async fn preview_block(
    track_index: usize,
    start: BarBeat,
    end: BarBeat,
    start_paused: Option<bool>,
    audio_clip_name: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    with_native_start(app, move |state| {
        preview_block_inner(
            track_index,
            start,
            end,
            start_paused.unwrap_or(false),
            audio_clip_name,
            state,
        )
    })
    .await
}

fn preview_block_inner(
    track_index: usize,
    start: BarBeat,
    end: BarBeat,
    start_paused: bool,
    audio_clip_name: Option<String>,
    state: &AppState,
) -> Result<Vec<String>, String> {
    let started = std::time::Instant::now();
    let started_ns = crate::first_block::now_ns();
    let label = audio_clip_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Track {}", track_index.saturating_add(1)));
    let (generation, snapshot) = {
        let mut inner = state.lock_inner();
        inner.audio.replace_private_audition(start_paused)?;
        let generation = state.audition.begin(&label);
        let snapshot = (|| -> Result<_, String> {
            let mut project = match audio_clip_name.as_deref() {
                Some(name) => isolated_audio_clip_preview_project(
                    &inner.project,
                    track_index,
                    start,
                    end,
                    name,
                )?,
                None => audio::solo_preview_project(&inner.project, track_index)?,
            };
            for preset in &mut project.presets {
                if preset.source.as_deref() == Some("clap")
                    && let Some(instance) = inner.plugin_instances.get(&preset.name)
                {
                    let bytes = instance.state_bytes()?;
                    preset.params.retain(|(key, _)| key != "clap_state_hex");
                    preset.params.push((
                        "clap_state_hex".into(),
                        oc_dsl::ast::ParamValue::String(
                            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                        ),
                    ));
                }
            }
            Ok((
                project,
                inner
                    .file_path
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .map(std::path::Path::to_path_buf),
                inner.audio.audio_io.spec(),
                inner.audio.soundfont_path.clone().unwrap_or_default(),
                inner.audio.audio_io.clone(),
            ))
        })();
        (generation, snapshot)
    };
    let result: Result<Vec<String>, String> = (|| {
        let (project, resource_root, spec, soundfont, io) = snapshot?;
        let warnings = collect_plugin_render_warnings(&project);
        let (start_frame, end_frame) = match audio_clip_name.as_deref() {
            Some(name) => {
                let clip = project.tracks[track_index]
                    .audio_clips()
                    .find(|clip| clip.name == name && !clip.muted)
                    .ok_or_else(|| format!("Audio clip '{name}' no longer exists"))?;
                oc_dsl::timing::ProjectClock::new(&project.properties)?
                    .audio_clip_range_frames(clip, spec.sample_rate)?
            }
            None => audio::block_preview_sample_range(
                &project,
                track_index,
                start,
                end,
                spec.sample_rate,
            )?,
        };
        let prepared =
            oc_engine::block_source::prepare_project(&project, resource_root.as_deref())?;
        if !state.audition.is_current(generation) {
            return Ok(warnings);
        }
        #[cfg(test)]
        let simulated = state.audio_simulated();
        #[cfg(not(test))]
        let simulated = false;
        let stream = audio::AudioState::open_audition_render_window(
            &io,
            &spec,
            soundfont,
            prepared,
            simulated,
            started_ns,
            crate::first_block::StartPath::ClipPreview,
            start_frame,
            Some(end_frame),
        )?;
        let mut inner = state.lock_inner();
        if !state.audition.is_current(generation) {
            return Ok(warnings);
        }
        inner.audio.install_audition_render(stream)?;
        state.audition.complete(generation, started);
        Ok(warnings)
    })();
    if let Err(error) = &result {
        state.audition.fail(generation, error.clone());
    }
    result
}

/// Play from current position (renders if needed, then starts streaming)
#[tauri::command]
pub async fn transport_play<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<TransportSnapshot, String> {
    with_native_start(app, |state| {
        let snapshot = play_with_take_policy(state, false)?;
        let inner = state.lock_inner();
        Ok(with_record(snapshot, &inner))
    })
    .await
}

/// Wait on the blocking pool so the preview window can acknowledge the pause.
pub(super) async fn with_native_start<R: tauri::Runtime, T: Send + 'static>(
    app: tauri::AppHandle<R>,
    start: impl FnOnce(&AppState) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let epoch = app.state::<AppState>().preview_focus.start_epoch();
    super::shared::run_blocking(move || {
        let state = app.state::<AppState>();
        let _handoff = state.preview_focus.start_native(&app, epoch)?;
        start(state.inner())
    })
    .await
}

/// The play path shared with recording, which must start the transport too.
pub(crate) fn play_inner(state: &AppState) -> Result<TransportSnapshot, String> {
    play_with_take_policy(state, true)
}

fn play_with_take_policy(
    state: &AppState,
    starting_take: bool,
) -> Result<TransportSnapshot, String> {
    if !starting_take {
        state
            .lock_inner()
            .audio
            .refuse_during_take("starting playback again")?;
    }
    let prepared = prepare_clap_playback(state)?;
    let mut inner = state.lock_inner();
    if !prepared.matches(&inner) {
        return Err("The project changed during native playback preparation; retry".into());
    }
    if !starting_take {
        inner.audio.refuse_during_take("starting playback again")?;
    }
    state.transport_pause_intent.store(false, Ordering::Release);
    state.audition.cancel_all();
    let mut warnings = collect_plugin_render_warnings(&inner.project);
    warnings.extend(prepared.warnings);
    let resource_root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let project = &prepared.project;
    let audio = &mut inner.audio;
    let mut plugin_instances = std::collections::HashMap::new();
    audio.resource_root = resource_root.clone();
    audio
        .render_cache
        .get_or_insert_with(oc_engine::render_cache::RenderCache::new)
        .set_resource_root(resource_root);
    let PlayDispatch::Started { did_render } = audio.play(project, &mut plugin_instances)?;
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties)?;
    let mut snapshot = audio.transport_snapshot(&clock)?;
    snapshot.did_render = did_render;
    snapshot.warnings = warnings;
    Ok(snapshot)
}

/// Pause — hold position, can resume with play
#[tauri::command]
pub fn transport_pause(state: State<AppState>) -> Result<TransportSnapshot, String> {
    pause_inner(state.inner())
}

pub(super) fn pause_inner(state: &AppState) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    inner.audio.refuse_during_take("pausing playback")?;
    state.preview_focus.cancel_pending_start();
    state.transport_pause_intent.store(true, Ordering::Release);

    inner.audio.pause();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let snapshot = inner.audio.transport_snapshot(&clock)?;
    Ok(with_record(snapshot, &inner))
}

/// Stop — finalise any take, then pause and reset to the beginning.
#[tauri::command]
pub fn transport_stop(state: State<AppState>) -> Result<TransportSnapshot, String> {
    state.preview_focus.cancel_pending_start();
    state.transport_pause_intent.store(true, Ordering::Release);
    stop_inner(state.inner())
}

/// Finish every take, then stop the transport.
///
/// The transport stops even when a take could not be committed: the recording
/// state is cleared first, so every later Stop is an ordinary one.
pub(crate) fn stop_inner(state: &AppState) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    state.audition.cancel_all();
    state.audition_pause_intent.store(false, Ordering::Release);

    let committed = inner.audio.recorder.recording() || inner.audio.midi_recorder.recording();
    let mut failure = None;
    if inner.audio.recorder.recording()
        && let Err(error) = crate::record::stop_inner(&mut inner)
    {
        failure = Some(error);
    }
    if inner.audio.midi_recorder.recording()
        && let Err(error) = crate::midi_record::stop_inner(&mut inner)
    {
        failure.get_or_insert(error);
    }
    inner.audio.stop();
    if let Some(error) = failure {
        return Err(error);
    }
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let mut snapshot = with_record(inner.audio.transport_snapshot(&clock)?, &inner);
    // A take that wrote a clip or notes must reach the editors now, not on the
    // next project load.
    if committed {
        snapshot.project = Some(super::shared::snapshot(&inner));
    }
    Ok(snapshot)
}

/// Arm one track for recording, or disarm with `null`.
#[tauri::command]
pub fn arm_track(
    track: Option<String>,
    state: State<AppState>,
) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    let armed = {
        let project_tracks: Vec<String> = inner
            .project
            .tracks
            .iter()
            .map(|t| t.name.clone())
            .collect();
        if let Some(name) = &track
            && !project_tracks.iter().any(|track| track == name)
        {
            return Err(crate::record::RecordRefusal::NotArmed.reason());
        }
        inner.audio.recorder.armed = track;
        inner.audio.recorder.report = None;
        inner.audio.midi_recorder.report = None;
        inner.audio.recorder.armed.clone()
    };
    let _ = armed;
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let snapshot = inner.audio.transport_snapshot(&clock)?;
    Ok(with_record(snapshot, &inner))
}

/// Start a take on the armed track, then start the transport with the
/// configured count-in.
#[tauri::command]
pub async fn start_recording<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<TransportSnapshot, String> {
    with_native_start(app, start_recording_inner).await
}

pub(super) fn start_recording_inner(state: &AppState) -> Result<TransportSnapshot, String> {
    let midi = {
        let mut inner = state.lock_inner();
        if inner.exit_ready {
            return Err("Cannot start recording while quitting.".into());
        }
        if inner.audio.audition_blocks_recording()
            || state.audition.snapshot().status == crate::audition::LOADING
        {
            return Err("Stop the audition before recording.".into());
        }
        // A take commits a clip or notes, so a preview must be finished first.
        // The press is refused before any capture is opened or armed.
        if let Some(reason) = super::shared::preview_refusal(&inner) {
            crate::record::say(&mut inner, Some(reason.clone()));
            return Err(reason);
        }
        let rate = inner.audio.sample_rate;
        let midi = crate::record::mode_of(&inner) == crate::record::RecordMode::Midi;
        // The take's own arming (an input stream open for audio) is part of
        // press-to-first-block, so the press is stamped before it.
        inner.audio.record_start_at(if midi {
            crate::first_block::StartPath::RecordMidi
        } else {
            crate::first_block::StartPath::RecordAudio
        });
        if midi {
            crate::midi_record::start_inner(&mut inner, rate, |selector, sink| {
                crate::midi_live::MidiRuntime::start_midir(selector, sink)
            })?;
        } else {
            crate::record::start_inner(&mut inner, rate)?;
        }
        // A ruler drag may have started before Record acquired the transport.
        inner.audio.unmute();
        midi
    };
    let snapshot = match play_inner(state) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            // A take whose transport never started must not stay armed: the
            // file is closed and removed, the capture stops and the count-in
            // is dropped.
            cancel_press(&mut state.lock_inner(), midi);
            return Err(error);
        }
    };
    // The take's head trim is resolved from the playhead by the transport poll,
    // once the transport has actually rolled.
    let inner = state.lock_inner();
    Ok(with_record(snapshot, &inner))
}

/// Drop a take whose transport never started.
pub(crate) fn cancel_press(inner: &mut crate::state::AppStateInner, midi: bool) {
    if midi {
        crate::midi_record::cancel_inner(inner);
    } else {
        crate::record::cancel_inner(inner);
    }
}

/// Stop the take and keep it as an audio clip.
#[tauri::command]
pub fn stop_recording(state: State<AppState>) -> Result<TransportSnapshot, String> {
    stop_inner(state.inner())
}

/// Seek to a beat position
#[tauri::command]
pub fn transport_seek(beat: f64, state: State<AppState>) -> Result<TransportSnapshot, String> {
    seek_inner(beat, state.inner())
}

pub(super) fn seek_inner(beat: f64, state: &AppState) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    inner.audio.refuse_during_take("moving the playhead")?;

    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    if !beat.is_finite() || beat < 0.0 {
        return Err("Seek position must be a non-negative finite beat".into());
    }
    inner.audio.seek(beat, &clock)?;
    let snapshot = inner.audio.transport_snapshot(&clock)?;
    Ok(with_record(snapshot, &inner))
}

/// Set or clear the session loop region, in beats. `None` for both bounds
/// clears the region and disarms Loop.
#[tauri::command]
pub fn set_loop_region(
    start_beats: Option<f64>,
    end_beats: Option<f64>,
    enabled: bool,
    state: State<AppState>,
) -> Result<audio::LoopState, String> {
    let mut inner = state.lock_inner();
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    inner
        .audio
        .set_loop_region(start_beats, end_beats, enabled, &clock)
}

/// Set or clear the punch range, in beats. A take keeps only the notes whose
/// start falls inside an enabled range.
#[tauri::command]
pub fn set_punch_region(
    start_beats: Option<f64>,
    end_beats: Option<f64>,
    enabled: bool,
    state: State<AppState>,
) -> Result<audio::PunchState, String> {
    state
        .lock_inner()
        .audio
        .set_punch_region(start_beats, end_beats, enabled)
}

/// What the render thread is preparing right now.
///
/// The status chip polls this while a build runs: `get_transport_state` only
/// covers playback, and a structural edit made while stopped still rebuilds
/// every instrument.
#[tauri::command]
pub fn project_build_progress() -> Option<crate::build_progress::BuildSnapshot> {
    crate::build_progress::snapshot()
}

#[tauri::command]
pub fn get_transport_state(state: State<AppState>) -> Result<TransportSnapshot, String> {
    transport_state_inner(state.inner())
}

pub(super) fn transport_state_inner(state: &AppState) -> Result<TransportSnapshot, String> {
    let mut inner = state.lock_inner();
    let revision = inner.revision;
    // The take is drained here: this is the 20 Hz poll the transport already
    // runs, and a capture error finalises the take instead of losing it.
    crate::record::poll_inner(&mut inner)?;
    crate::midi_record::poll_inner(&mut inner)?;
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let mut snapshot = inner.audio.transport_snapshot(&clock)?;
    let mut audition = state.audition.snapshot();
    inner.audio.fill_audition_snapshot(&mut audition);
    snapshot.audition = Some(audition);
    if inner.revision != revision {
        snapshot.project = Some(super::shared::snapshot(&inner));
    }
    let snapshot = with_record(snapshot, &inner);
    Ok(snapshot)
}

/// Mute audio output (during cursor scrub). Render keeps running for instant resume.
#[tauri::command]
pub fn transport_mute(state: State<AppState>) -> Result<(), String> {
    let mut inner = state.lock_inner();
    inner.audio.refuse_during_take("scrubbing the playhead")?;
    inner.audio.mute();
    Ok(())
}

/// Unmute audio output (after cursor scrub).
#[tauri::command]
pub fn transport_unmute(state: State<AppState>) -> Result<(), String> {
    let mut inner = state.lock_inner();
    inner.audio.unmute();
    Ok(())
}

/// Enumerate output devices again and return the audio-IO snapshot. The
/// preferences section's refresh button uses this.
#[tauri::command]
pub async fn list_audio_io(app: tauri::AppHandle) -> Result<audio::AudioIoView, String> {
    // Device enumeration blocks for hundreds of milliseconds under a slow
    // driver, so it runs off the webview thread.
    run_blocking(move || {
        let state = app.state::<AppState>();
        let mut inner = state.lock_inner();
        // Viewing the choices enumerates devices; it must not open the capture
        // stream, because that asks the OS for microphone access. Capture opens
        // on an explicit input choice, on monitoring, or when a take is armed.
        inner.audio.refresh_audio_io();
        Ok(inner.audio.audio_io_view())
    })
    .await
}

/// Current device, negotiated format, notices and measured latency.
#[tauri::command]
pub fn get_audio_io(state: State<AppState>) -> audio::AudioIoView {
    let mut inner = state.lock_inner();
    inner.audio.reap_idle_monitor_failure();
    if !inner.audio.recorder.recording() {
        let _ = inner.audio.take_capture_failure();
    }
    let _ = inner.audio.reap_live_synth_failure();
    // The section polls the last measured level without asking for microphone
    // access. Only explicit capture choices or recording open the input.
    inner.audio.audio_io_view()
}

/// Apply a full output selection; the returned snapshot is the opened state, so
/// a rejected selection snaps the picker back to it.
#[tauri::command]
pub async fn set_audio_io(
    selection: oc_audio::audio_io::AudioSelection,
    app: tauri::AppHandle,
) -> Result<audio::AudioIoView, String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        Ok(state.lock_inner().audio.set_audio_io(selection))
    })
    .await
}

/// Apply a full capture selection. The capture stream is opened on demand, so
/// the returned snapshot is both the opened input and the level meter state.
#[tauri::command]
pub async fn set_input_io(
    selection: oc_audio::audio_io::InputSelection,
    app: tauri::AppHandle,
) -> Result<audio::AudioIoView, String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        Ok(state.lock_inner().audio.set_input_io(selection))
    })
    .await
}

/// Change the click. The returned snapshot is what the render thread now uses.
#[tauri::command]
pub fn set_click(
    settings: oc_audio::click::ClickSettings,
    state: State<AppState>,
) -> crate::audio::ClickSettingsView {
    state.lock_inner().audio.set_click(settings)
}

/// Turn monitoring on or off without touching the device choice.
#[tauri::command]
pub fn set_input_monitor(
    monitor: bool,
    gain_db: Option<f64>,
    state: State<AppState>,
) -> audio::AudioIoView {
    let mut inner = state.lock_inner();
    let gain = gain_db.unwrap_or(inner.audio.audio_io.input_selection().input_monitor_gain_db);
    inner.audio.set_input_monitor(monitor, gain)
}

/// Output device list for the agent API; the shape predates the audio-IO owner.
#[tauri::command]
pub fn list_audio_devices(state: State<AppState>) -> Vec<AudioDeviceInfo> {
    let inner = state.lock_inner();
    let _ = inner.audio.audio_io.refresh();
    inner.audio.audio_io.status().devices
}

/// Audio configuration for the agent API.
#[tauri::command]
pub fn get_audio_config(state: State<AppState>) -> Result<AudioConfig, String> {
    let inner = state.lock_inner();
    Ok(inner.audio.config())
}
