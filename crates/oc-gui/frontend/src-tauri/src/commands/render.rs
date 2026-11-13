//! Offline renders, stem exports, and agent range renders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use oc_dsl::ast::{self, *};
use oc_dsl::timing::ProjectClock;
use oc_engine::encode::{EncodeReport, EncodeSpec};
use oc_engine::render_cache::RenderCache;
use oc_host::clap_instance::ClapInstance;

use crate::state::AppState;

use super::audition_scratch::ScratchRender;
use super::plugin_host::{collect_plugin_render_warnings, ensure_clap_instances_ready};

// ============================================================================
// Audio / Transport
// ============================================================================

/// Everything an offline render reads, captured under the project lock.
pub(crate) struct OfflineRenderSnapshot {
    pub(crate) project: Project,
    pub(crate) sample_rate: u32,
    pub(crate) worker_options: oc_engine::render_parallel::RenderWorkerOptions,
    pub(crate) soundfont_path: PathBuf,
    pub(crate) resource_root: Option<PathBuf>,
    pub(crate) plugin_instances: HashMap<String, ClapInstance>,
    pub(crate) pre_roll: f64,
    pub(crate) content_epoch: u64,
    pub(crate) generation: u64,
    /// The dialog's stop flag, polled by the engine per source chunk.
    pub(crate) cancel: Option<oc_engine::render_cancel::CancelToken>,
    /// Where the engine reports completed source tracks.
    pub(crate) progress: Option<oc_engine::render_cancel::ProgressCallback>,
    // MUST remain last: Rust drops fields in declaration order. Native owners
    // and progress closures retire before shutdown observes this job as finished.
    _native_job: crate::native_jobs::NativeJobGuard,
}

/// Per-call export choices layered over the project's `export:` block.
///
/// `None` keeps whatever the project declares, so a caller can override one
/// option without restating the rest. The dialog sends every field explicitly.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ExportOverrides {
    pub container: Option<ExportContainer>,
    pub depth: Option<SampleFormat>,
    pub dither: Option<bool>,
    /// FLAC compression level, 0 fastest to 8 smallest.
    pub flac_level: Option<u8>,
    /// MP3 CBR bitrate in kbps.
    pub mp3_bitrate: Option<u32>,
    /// MP3 VBR quality index, 0 best to 9 smallest.
    pub mp3_vbr: Option<u8>,
    pub sample_rate: Option<u32>,
    /// Render resampling step, 0 fastest to 7 best.
    pub resample: Option<u8>,
    /// `Some(false)` renders without the master loudness target.
    pub normalise: Option<bool>,
}

impl ExportOverrides {
    /// Fold the choices that live inside the project into a snapshot copy.
    fn apply_to(&self, project: &mut Project) -> Result<(), String> {
        if let Some(step) = self.resample {
            if step > 7 {
                return Err(format!(
                    "resampling step {step} is outside 0-7; use 0 for the fastest render or 7 for the best"
                ));
            }
            project.properties.export.get_or_insert_default().resample = Some(step);
        }
        if self.normalise == Some(false)
            && let Some(master) = &mut project.master
        {
            master.loudness_target = None;
        }
        Ok(())
    }
}

/// Project export settings with the call's overrides on top: the dialog's
/// choices reach the encoder through this one seam.
pub(crate) fn resolve_export_options(
    project: &Project,
    fallback_sample_rate: u32,
    overrides: &ExportOverrides,
) -> Result<EncodeSpec, String> {
    let sample_rate = overrides.sample_rate.unwrap_or(fallback_sample_rate);
    let mut spec = EncodeSpec::for_project(project, sample_rate);
    if let Some(container) = overrides.container {
        spec.container = container;
    }
    if let Some(depth) = overrides.depth {
        match spec.container {
            ExportContainer::Wav => spec.wav.depth = depth,
            ExportContainer::Flac => spec.flac.depth = depth,
            ExportContainer::Mp3 => {
                return Err(format!(
                    "MP3 has no sample depth: the encoder takes 16-bit PCM, so {depth} does not apply"
                ));
            }
        }
    }
    if let Some(level) = overrides.flac_level {
        if spec.container != ExportContainer::Flac {
            return Err(format!(
                "FLAC compression level {level} only applies to a FLAC export; this one writes {}",
                spec.container
            ));
        }
        if level > oc_dsl::ast::MAX_FLAC_LEVEL {
            return Err(format!(
                "FLAC compression level {level} is outside {}-{}; level 0 is fastest and 8 smallest",
                oc_dsl::ast::MIN_FLAC_LEVEL,
                oc_dsl::ast::MAX_FLAC_LEVEL
            ));
        }
        spec.flac.level = level;
    }
    if let Some(dither) = overrides.dither {
        spec.wav.dither = dither;
    }
    if (overrides.mp3_bitrate.is_some() || overrides.mp3_vbr.is_some())
        && spec.container != ExportContainer::Mp3
    {
        return Err(format!(
            "the MP3 bitrate and quality only apply to an MP3 export; this one writes {}",
            spec.container
        ));
    }
    if let Some(bitrate) = overrides.mp3_bitrate {
        spec.mp3.bitrate = bitrate;
        spec.mp3.vbr_quality = None;
    }
    if let Some(quality) = overrides.mp3_vbr {
        spec.mp3.vbr_quality = Some(quality);
    }
    spec.validate()?;
    Ok(spec)
}

/// What this build can encode. The dialog disables the rest instead of offering
/// a format that would fail at export.
#[derive(Debug, Clone, Serialize)]
pub struct ExportCapabilities {
    pub mp3: bool,
    /// MP3 sample rates this build writes, straight from the encoder table.
    pub mp3_sample_rates: Vec<u32>,
}

/// Report the containers and MP3 rate table this build writes.
#[tauri::command]
pub fn export_capabilities() -> ExportCapabilities {
    ExportCapabilities {
        mp3: cfg!(feature = "mp3"),
        mp3_sample_rates: oc_engine::encode::mp3::MP3_SAMPLE_RATES.to_vec(),
    }
}

/// Refuse a target whose extension names a different container.
fn ensure_output_extension(path: &Path, container: ExportContainer) -> Result<(), String> {
    let expected = container.extension();
    match path.extension().and_then(|value| value.to_str()) {
        Some(found) if found.eq_ignore_ascii_case(expected) => Ok(()),
        Some(found) => Err(format!(
            "output '{}' has the .{found} extension but this export writes .{expected}",
            path.display()
        )),
        None => Err(format!(
            "output '{}' has no extension; this export writes .{expected}",
            path.display()
        )),
    }
}

/// Which files one export publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportScope {
    /// One file holding the master mix.
    Mix,
    /// The mix plus one file per track and bus, under a directory.
    Stems,
}

/// One export request from the dialog.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderAudioRequest {
    pub scope: ExportScope,
    /// Target file for the mix scope.
    #[serde(default)]
    pub output_path: Option<String>,
    /// Publish directory for the stems scope.
    #[serde(default)]
    pub directory: Option<String>,
    /// Base name of the stem files; missing uses the song name.
    #[serde(default)]
    pub stem_name: Option<String>,
    #[serde(default)]
    pub pre_roll: Option<f64>,
    #[serde(default)]
    pub options: ExportOverrides,
}

/// Checkpoints a render passes, in order. The dialog names them instead of
/// pretending to know a frame count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPhase {
    Rendering,
    Encoding,
    Publishing,
    Done,
    Cancelled,
}

impl ExportPhase {
    fn stage(self) -> &'static str {
        match self {
            Self::Rendering => "rendering",
            Self::Encoding => "encoding",
            Self::Publishing => "publishing",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Event name the dialog listens to while an export runs.
pub const EXPORT_PROGRESS_EVENT: &str = "export-progress";

#[derive(Debug, Clone, Serialize)]
/// Progress event emitted during export.
pub struct ExportProgressEvent {
    pub generation: u64,
    pub phase: ExportPhase,
    /// The same stage as `phase`, named for progress consumers that read a stage.
    pub stage: String,
    /// Completed fraction of the current stage, `0.0..=1.0`.
    pub fraction: f32,
    pub elapsed_ms: u64,
}

/// What the dialog's finished page reports.
#[derive(Debug, Clone, Serialize)]
pub struct RenderAudioResult {
    pub scope: ExportScope,
    /// The mix file: the published file, or the mix of a stem set.
    pub path: String,
    pub files: Vec<String>,
    pub stems: usize,
    pub bytes: u64,
    pub frames: usize,
    pub sample_rate: u32,
    pub container: ExportContainer,
    pub depth: SampleFormat,
    pub dither: bool,
    pub peak_dbfs: f64,
    pub true_peak_dbfs: f64,
    /// True when the mix exceeded full scale, whether or not the file clamped.
    pub above_full_scale: bool,
    /// True when the written file clamped the overload.
    pub clipped: bool,
}

/// Fork the CLAP instances an offline pass needs, keyed by preset name.
///
/// A pass that renders one track or a window still runs the project's hosted
/// effects, so it owns its own instances and never touches the editor's.
pub(crate) struct OfflinePluginSnapshot {
    pub(crate) instances: HashMap<String, ClapInstance>,
    revision: u64,
    content_epoch: u64,
    file_path: Option<PathBuf>,
    // Last field. Callers returning worker snapshots MUST move this receipt
    // alongside instances; a partial move into a local is not a transfer.
    pub(crate) native_job: crate::native_jobs::NativeJobGuard,
}
impl OfflinePluginSnapshot {
    pub(crate) fn validate(&self, inner: &crate::state::AppStateInner) -> Result<(), String> {
        if inner.revision != self.revision
            || inner.content_epoch != self.content_epoch
            || inner.file_path != self.file_path
        {
            return Err(
                "The project changed during native plugin preparation; retry the render".into(),
            );
        }
        Ok(())
    }
}
pub(crate) fn fork_offline_plugin_instances(
    state: &AppState,
) -> Result<OfflinePluginSnapshot, String> {
    fork_offline_plugin_instances_with_cancel(
        state,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

fn fork_offline_plugin_instances_with_cancel(
    state: &AppState,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<OfflinePluginSnapshot, String> {
    let native_job = state.native_jobs.register(cancel)?;
    let warnings = ensure_clap_instances_ready(state);
    if !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    let (mut snapshot, plans) = {
        let inner = state.lock_inner();
        let warnings = collect_plugin_render_warnings(&inner.project);
        if !warnings.is_empty() {
            return Err(warnings.join("; "));
        }
        let mut plans = Vec::new();
        for preset in &inner.project.presets {
            if preset.source.as_deref() != Some("clap")
                || preset
                    .plugin
                    .as_deref()
                    .is_none_or(|id| id.starts_with("builtin."))
            {
                continue;
            }
            let instance = inner
                .plugin_instances
                .get(&preset.name)
                .ok_or_else(|| format!("CLAP preset '{}' has no loaded instance", preset.name))?;
            plans.push((preset.name.clone(), instance.offline_fork_plan()));
        }
        (
            OfflinePluginSnapshot {
                instances: HashMap::new(),
                revision: inner.revision,
                content_epoch: inner.content_epoch,
                file_path: inner.file_path.clone(),
                native_job,
            },
            plans,
        )
    };
    for (name, plan) in plans {
        snapshot.instances.insert(name, plan.execute_on_main()?);
    }
    snapshot.validate(&state.lock_inner())?;
    Ok(snapshot)
}

/// Snapshot inputs and claim this render's generation; never renders.
pub(crate) fn capture_offline_snapshot(
    state: &AppState,
    pre_roll: Option<f64>,
    overrides: &ExportOverrides,
) -> Result<OfflineRenderSnapshot, String> {
    // This owner precedes the document guard, so refusal drops the guard before
    // any native instance. Move instances only into the infallible final result.
    let plugins = fork_offline_plugin_instances_with_cancel(state, state.begin_export_cancel())?;
    let mut inner = state.lock_inner();
    plugins.validate(&inner)?;
    let mut project = inner.project.clone();
    overrides.apply_to(&mut project)?;
    let sample_rate = overrides.sample_rate.unwrap_or(inner.audio.sample_rate);
    oc_engine::encode::validate_sample_rate(sample_rate)?;
    let soundfont_path = inner.audio.soundfont_path.clone().unwrap_or_default();
    let resource_root = inner
        .file_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let content_epoch = inner.content_epoch;
    // Each render claims its own token; a Cancel for another export cannot stop
    // this one or drop its staged file.
    let cancel = plugins.native_job.cancel_token();
    let generation = inner.begin_offline_render();
    Ok(OfflineRenderSnapshot {
        worker_options: crate::gui_settings::load().worker_options(),
        project,
        sample_rate,
        soundfont_path,
        resource_root,
        plugin_instances: plugins.instances,
        pre_roll: pre_roll.unwrap_or(0.0),
        content_epoch,
        generation,
        cancel: Some(cancel),
        progress: None,
        _native_job: plugins.native_job,
    })
}

/// Output WAV staged beside the target; only a publish renames it into place.
struct PendingOutput {
    path: PathBuf,
    published: bool,
}

impl PendingOutput {
    fn new(output: &Path, generation: u64) -> Result<Self, String> {
        let name = output
            .file_name()
            .ok_or_else(|| format!("output path '{}' has no file name", output.display()))?;
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".oc-render-{generation}.tmp"));
        Ok(Self {
            path: output.with_file_name(temp_name),
            published: false,
        })
    }

    /// Prove the destination is writable and reserve the staging name before
    /// any render work starts.
    ///
    /// The empty file created here is the one the encoder later truncates; a
    /// cancelled or failed render drops it with `PendingOutput`.
    fn stage(output: &Path, generation: u64) -> Result<Self, String> {
        let pending = Self::new(output, generation)?;
        let parent = pending
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let metadata = std::fs::metadata(parent)
            .map_err(|error| format!("cannot write '{}': {error}", output.display()))?;
        if !metadata.is_dir() {
            return Err(format!(
                "cannot write '{}': '{}' is not a directory",
                output.display(),
                parent.display()
            ));
        }
        let open = || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending.path)
        };
        match open() {
            Ok(_) => Ok(pending),
            // A stale staging file from a crashed export is not a destination
            // problem; replace it and keep the preflight honest.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&pending.path)
                    .map_err(|error| format!("cannot write '{}': {error}", output.display()))?;
                open().map_err(|error| format!("cannot write '{}': {error}", output.display()))?;
                Ok(pending)
            }
            Err(error) => Err(format!("cannot write '{}': {error}", output.display())),
        }
    }

    fn publish(&mut self, output: &Path) -> Result<(), String> {
        move_into_place(&self.path, output)?;
        self.published = true;
        Ok(())
    }
}

/// Rename a staged file over `target`, replacing an existing file.
///
/// The rename is the publish: a target that already exists is replaced in one
/// step, so a failed publish never removes the previous file.
pub(crate) fn move_into_place(staged: &Path, target: &Path) -> Result<(), String> {
    fn failed(target: &Path, error: std::io::Error) -> String {
        format!("failed to publish '{}': {error}", target.display())
    }
    #[cfg(windows)]
    let published = replace_existing_file(staged, target);
    #[cfg(not(windows))]
    let published = std::fs::rename(staged, target);
    published.map_err(|error| failed(target, error))
}

/// Replace `target` with `staged` in one step on Windows.
///
/// `std::fs::rename` refuses an existing destination there, and deleting first
/// would lose the previous export when the second step fails.
#[cfg(windows)]
fn replace_existing_file(staged: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing: Vec<u16> = staged
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let new: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both buffers are NUL-terminated wide strings that outlive the call.
    let replaced = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Fail unless no edit or newer render won since the snapshot was taken.
fn check_publish_guard(
    inner: &crate::state::AppStateInner,
    content_epoch: u64,
    generation: u64,
) -> Result<(), String> {
    if inner.content_epoch != content_epoch {
        return Err(format!(
            "project changed during render (content key {content_epoch} -> {}); result not published",
            inner.content_epoch
        ));
    }
    if inner.render_generation != generation {
        return Err("render superseded by a newer render; result not published".into());
    }
    Ok(())
}

/// Publish the output and a clean cache only if no edit or newer render won.
fn publish_offline_output(
    state: &AppState,
    pending: &mut PendingOutput,
    output: &Path,
    content_epoch: u64,
    generation: u64,
    sample_rate: u32,
    cache: RenderCache,
) -> Result<(), String> {
    let inner = state.lock_inner();
    check_publish_guard(&inner, content_epoch, generation)?;
    pending.publish(output)?;
    if !cache.is_dirty() {
        state.store_range_cache_locked(content_epoch, sample_rate, cache);
    }
    Ok(())
}

pub(crate) fn render_offline_snapshot(
    snapshot: OfflineRenderSnapshot,
) -> Result<(Vec<f32>, RenderCache), String> {
    render_offline_snapshot_using_cache(snapshot, RenderCache::new())
}

/// The caller's fresh cache may carry explicitly prepared plugin metadata;
/// request assembly, rendering and cancellation remain the production pipeline.
pub(crate) fn render_offline_snapshot_using_cache(
    snapshot: OfflineRenderSnapshot,
    cache: RenderCache,
) -> Result<(Vec<f32>, RenderCache), String> {
    use oc_engine::render_cancel::RenderError;

    let mut plugin_instances = snapshot.plugin_instances;
    let minimum_duration = crate::project_extent::section_seconds(&snapshot.project)?;
    oc_engine::render::render_full_for_export_with_options_and_cache(
        &snapshot.project,
        &snapshot.soundfont_path,
        &mut plugin_instances,
        oc_engine::render::RenderOptions {
            resource_root: snapshot.resource_root.as_deref(),
            minimum_duration_seconds: minimum_duration,
            pre_roll_seconds: snapshot.pre_roll,
            cancel: snapshot.cancel,
            progress: snapshot.progress,
            ..oc_engine::render::RenderOptions::new(snapshot.sample_rate, snapshot.worker_options)
        },
        cache,
    )
    .map_err(|error| match error {
        RenderError::Cancelled => cancelled_message("rendering"),
        RenderError::Message(message) => message,
    })
}

/// A rendered mix staged beside its target, waiting for the publish guard.
pub(crate) struct StagedRender {
    output: PathBuf,
    pending: PendingOutput,
    content_epoch: u64,
    generation: u64,
    spec: EncodeSpec,
    cache: RenderCache,
    report: EncodeReport,
    frames: usize,
}

impl StagedRender {
    /// Publish only if no edit or newer render won; otherwise the staged WAV drops.
    pub(crate) fn publish(mut self, state: &AppState) -> Result<(), String> {
        publish_offline_output(
            state,
            &mut self.pending,
            &self.output,
            self.content_epoch,
            self.generation,
            self.spec.sample_rate,
            self.cache,
        )
    }
}

/// Report a phase and its completed fraction to the window, when one listens.
fn progress_reporter(
    app: Option<tauri::AppHandle>,
    generation: u64,
) -> Arc<dyn Fn(ExportPhase, f32) + std::marker::Send + std::marker::Sync> {
    let started = Instant::now();
    Arc::new(move |phase, fraction| {
        if let Some(app) = &app {
            let _ = app.emit(
                EXPORT_PROGRESS_EVENT,
                ExportProgressEvent {
                    generation,
                    phase,
                    stage: phase.stage().to_string(),
                    fraction: fraction.clamp(0.0, 1.0),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                },
            );
        }
    })
}

/// Map the engine's progress reports onto export events.
fn engine_progress(
    progress: Arc<dyn Fn(ExportPhase, f32) + std::marker::Send + std::marker::Sync>,
) -> oc_engine::render_cancel::ProgressCallback {
    Box::new(move |report| {
        let phase = match report.stage {
            oc_engine::render_cancel::RenderStage::Rendering
            | oc_engine::render_cancel::RenderStage::Mixing => ExportPhase::Rendering,
        };
        progress(phase, report.fraction);
    })
}

/// True when a backend error is the cancellation path rather than a failure.
pub(crate) fn is_cancelled(error: &str) -> bool {
    error.contains("cancelled") || error == oc_engine::render_cancel::CANCELLED_MESSAGE
}

/// The sentence the dialog shows when the render stopped on request.
fn cancelled_message(checkpoint: &str) -> String {
    format!("Export cancelled at the {checkpoint} checkpoint; nothing was published")
}

/// Refuse to publish once the dialog asked to stop, naming the checkpoint.
pub(crate) fn cancel_checkpoint(
    cancel: oc_engine::render_cancel::CancelToken,
) -> impl Fn(&str) -> Result<(), String> {
    move |phase| {
        if cancel.is_cancelled() {
            return Err(cancelled_message(phase));
        }
        Ok(())
    }
}

/// Render a snapshot and stage its file without touching `AppState`; runs off-lock.
pub(crate) fn stage_offline_render<F>(
    snapshot: OfflineRenderSnapshot,
    output_path: &str,
    overrides: &ExportOverrides,
    progress: &dyn Fn(ExportPhase, f32),
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
    render: F,
) -> Result<StagedRender, String>
where
    F: FnOnce(OfflineRenderSnapshot) -> Result<(Vec<f32>, RenderCache), String>,
{
    let content_epoch = snapshot.content_epoch;
    let generation = snapshot.generation;
    let cancel = snapshot.cancel.clone();
    let output = PathBuf::from(output_path);
    let spec = resolve_export_options(&snapshot.project, snapshot.sample_rate, overrides)?;
    ensure_output_extension(&output, spec.container)?;
    // Prove the destination before the render can spend minutes on a path the
    // filesystem will refuse; the staged file is removed if anything fails.
    let pending = PendingOutput::stage(&output, generation)?;
    // The checkpoint before synthesis is the cheap one: a cancel that arrives
    // while the request is queued never renders at all.
    checkpoint("start")?;
    progress(ExportPhase::Rendering, 0.0);
    let (audio, cache) = render(snapshot)?;
    let frames = audio.len() / usize::from(spec.channels.max(1));
    progress(ExportPhase::Encoding, 0.0);
    checkpoint("encoding")?;
    let report = oc_engine::encode::encode_to_file(&pending.path, &spec, &audio)
        .map_err(|error| format!("failed to write '{}': {error}", output.display()))?;
    // A stop that arrived while the encoder wrote drops the staged file.
    if cancel.as_ref().is_some_and(|token| token.is_cancelled()) {
        return Err(cancelled_message("encoding"));
    }
    Ok(StagedRender {
        output,
        pending,
        content_epoch,
        generation,
        spec,
        cache,
        report,
        frames,
    })
}

/// Render from a lock-free snapshot and publish under a content + generation guard.
#[cfg(test)]
pub(crate) fn render_offline_with<F>(
    state: &AppState,
    output_path: String,
    pre_roll: Option<f64>,
    format: Option<SampleFormat>,
    dither: Option<bool>,
    render: F,
) -> Result<String, String>
where
    F: FnOnce(OfflineRenderSnapshot) -> Result<(Vec<f32>, RenderCache), String>,
{
    let overrides = ExportOverrides {
        depth: format,
        dither,
        ..ExportOverrides::default()
    };
    let snapshot = capture_offline_snapshot(state, pre_roll, &overrides)?;
    stage_offline_render(
        snapshot,
        &output_path,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        render,
    )?
    .publish(state)?;
    Ok(output_path)
}

/// The finished page's summary of one published mix file.
fn mix_result(
    path: &Path,
    frames: usize,
    spec: EncodeSpec,
    report: EncodeReport,
) -> RenderAudioResult {
    let path = path.display().to_string();
    RenderAudioResult {
        scope: ExportScope::Mix,
        files: vec![path.clone()],
        bytes: file_bytes(&path),
        path,
        stems: 0,
        frames,
        sample_rate: spec.sample_rate,
        container: spec.container,
        depth: spec.depth(),
        dither: spec.dither(),
        peak_dbfs: report.pre_clamp_peak_dbfs,
        true_peak_dbfs: report.pre_clamp_true_peak_dbfs,
        above_full_scale: report.above_full_scale,
        clipped: report.clipped,
    }
}

/// Size of a published file; a missing file reports zero rather than failing
/// the export that just succeeded.
fn file_bytes(path: &str) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Run `work` on Tauri's blocking pool from a lock-free snapshot.
async fn snapshot_and_run<F, T>(
    state: &AppState,
    pre_roll: Option<f64>,
    overrides: &ExportOverrides,
    work: F,
) -> Result<T, String>
where
    F: FnOnce(OfflineRenderSnapshot) -> Result<T, String> + std::marker::Send + 'static,
    T: std::marker::Send + 'static,
{
    // A render is a long, crash-prone step: keep the state it started from.
    if let Err(error) = crate::autosave::save_now(state) {
        log::warn!("autosave before render: {error}");
    }
    let snapshot = capture_offline_snapshot(state, pre_roll, overrides)?;
    tauri::async_runtime::spawn_blocking(move || work(snapshot))
        .await
        .map_err(|error| format!("Render task failed: {error}"))?
}

/// Render on Tauri's blocking pool; the awaiting thread and app lock stay free.
pub(crate) async fn render_offline_off_thread<F>(
    state: &AppState,
    output_path: String,
    pre_roll: Option<f64>,
    format: Option<SampleFormat>,
    dither: Option<bool>,
    render: F,
) -> Result<String, String>
where
    F: FnOnce(OfflineRenderSnapshot) -> Result<(Vec<f32>, RenderCache), String>
        + std::marker::Send
        + 'static,
{
    let target = output_path.clone();
    let overrides = ExportOverrides {
        depth: format,
        dither,
        ..ExportOverrides::default()
    };
    let staged = snapshot_and_run(state, pre_roll, &overrides, move |snapshot| {
        stage_offline_render(
            snapshot,
            &target,
            &overrides,
            &|_, _| {},
            &|_| Ok(()),
            render,
        )
    })
    .await?;
    staged.publish(state)?;
    Ok(output_path)
}

/// Export the full mix. The invoke resolves once the WAV is published; a second
/// export started meanwhile supersedes this one.
#[tauri::command]
pub async fn render_offline(
    output_path: String,
    pre_roll: Option<f64>,
    sample_format: Option<SampleFormat>,
    dither: Option<bool>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let _timer = oc_diag::perf_counter("cmd.render_offline.ms").map(oc_diag::Timer::start);
    oc_diag::diag_info!(
        "cmd: render_offline output={output_path} pre_roll={pre_roll:?} format={sample_format:?} dither={dither:?}"
    );
    render_offline_off_thread(
        state.inner(),
        output_path,
        pre_roll,
        sample_format,
        dither,
        render_offline_snapshot,
    )
    .await
}

/// What the stems command reports once its set is published.
pub(crate) struct StemsSummary {
    stems: usize,
    frames: usize,
    loudness: Option<oc_engine::render::LoudnessReport>,
    /// What the mix did before any stem file was quantised.
    report: EncodeReport,
    /// Options every file in the set was written with.
    spec: EncodeSpec,
    /// Base names staged for publish, the mix file first.
    files: Vec<String>,
}

/// A complete stem set staged in a hidden directory inside the target; only a
/// publish moves its files out, and a dropped set removes every staged file.
struct StagedStems {
    directory: PathBuf,
    staging: PathBuf,
    content_epoch: u64,
    generation: u64,
    published: bool,
}

impl StagedStems {
    /// Move the whole set into the target under the content + generation guard.
    fn publish(mut self, state: &AppState) -> Result<PathBuf, String> {
        let inner = state.lock_inner();
        check_publish_guard(&inner, self.content_epoch, self.generation)?;
        move_staged_stems(&self.staging, &self.directory)?;
        self.published = true;
        let _ = std::fs::remove_dir_all(&self.staging);
        Ok(self.directory.join("stems.json"))
    }
}

impl Drop for StagedStems {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
    }
}

fn staging_stems_dir(directory: &Path, generation: u64) -> PathBuf {
    directory.join(format!(".stems.oc-render-{generation}.tmp"))
}

/// Render the mix and every stem into `staging`; nothing is published here.
pub(crate) fn render_stems_snapshot(
    snapshot: OfflineRenderSnapshot,
    staging: &Path,
) -> Result<StemsSummary, String> {
    render_stems_snapshot_with(snapshot, staging, None, &ExportOverrides::default())
}

/// The same render with the dialog's stem name and export choices.
pub(crate) fn render_stems_snapshot_with(
    snapshot: OfflineRenderSnapshot,
    staging: &Path,
    stem_name: Option<&str>,
    overrides: &ExportOverrides,
) -> Result<StemsSummary, String> {
    let song = stem_name
        .map(str::to_string)
        .or_else(|| snapshot.project.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "untitled".into());
    let spec = resolve_export_options(&snapshot.project, snapshot.sample_rate, overrides)?;
    let mut plugin_instances = snapshot.plugin_instances;
    let export = oc_engine::render::render_with_stems_with_options(
        &snapshot.project,
        &snapshot.soundfont_path,
        &mut plugin_instances,
        oc_engine::render::RenderOptions {
            resource_root: snapshot.resource_root.as_deref(),
            minimum_duration_seconds: crate::project_extent::section_seconds(&snapshot.project)?,
            pre_roll_seconds: snapshot.pre_roll,
            cancel: snapshot.cancel,
            progress: snapshot.progress,
            ..oc_engine::render::RenderOptions::new(snapshot.sample_rate, snapshot.worker_options)
        },
    )
    .map_err(|error| match error {
        oc_engine::render_cancel::RenderError::Cancelled => cancelled_message("rendering"),
        oc_engine::render_cancel::RenderError::Message(message) => message,
    })?;
    let report = oc_engine::encode::measure(&export.mix, spec.depth());
    oc_engine::render::write_stems_export(&snapshot.project, staging, &song, &export, spec)?;
    let mut files = staged_file_names(staging)?;
    // The mix comes first so the finished page can name it without a search.
    let mix_prefix = format!("{song}-mix.");
    files.sort_by_key(|name| !name.starts_with(&mix_prefix));
    Ok(StemsSummary {
        stems: export.stems.len(),
        frames: export.mix.len() / 2,
        loudness: export.loudness,
        report,
        spec,
        files,
    })
}

/// Base names staged for publish, in the order the directory lists them.
fn staged_file_names(staging: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(staging)
        .map_err(|error| format!("failed to read '{}': {error}", staging.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read '{}': {error}", staging.display()))?;
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Render a snapshot into a staged set inside `directory`; runs off-lock.
fn stage_stems_export<F>(
    snapshot: OfflineRenderSnapshot,
    directory: &Path,
    render: F,
) -> Result<(StagedStems, StemsSummary), String>
where
    F: FnOnce(OfflineRenderSnapshot, &Path) -> Result<StemsSummary, String>,
{
    stage_stems_export_with(snapshot, directory, &|_, _| {}, &|_| Ok(()), render)
}

/// The same staging with the dialog's progress and cancel checkpoints.
fn stage_stems_export_with<F>(
    snapshot: OfflineRenderSnapshot,
    directory: &Path,
    progress: &dyn Fn(ExportPhase, f32),
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
    render: F,
) -> Result<(StagedStems, StemsSummary), String>
where
    F: FnOnce(OfflineRenderSnapshot, &Path) -> Result<StemsSummary, String>,
{
    let content_epoch = snapshot.content_epoch;
    let generation = snapshot.generation;
    let staging = staging_stems_dir(directory, generation);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("failed to create '{}': {error}", staging.display()))?;
    checkpoint("start")?;
    progress(ExportPhase::Rendering, 0.0);
    let summary = match render(snapshot, &staging) {
        Ok(summary) => summary,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    progress(ExportPhase::Encoding, 0.0);
    checkpoint("encoding")?;
    let staged = StagedStems {
        directory: directory.to_path_buf(),
        staging,
        content_epoch,
        generation,
        published: false,
    };
    Ok((staged, summary))
}

/// Move the staged set in, manifest last; a failure removes what already moved.
fn move_staged_stems(staging: &Path, directory: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(staging)
        .map_err(|error| format!("failed to read '{}': {error}", staging.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read '{}': {error}", staging.display()))?;
        files.push(entry.path());
    }
    // The manifest goes last, so a published set is complete when it appears.
    files.sort_by_key(|path| path.ends_with("stems.json"));
    let mut moved = Vec::new();
    for staged in &files {
        let name = staged
            .file_name()
            .ok_or_else(|| format!("staged path '{}' has no file name", staged.display()))?;
        let target = directory.join(name);
        if let Err(error) = move_into_place(staged, &target) {
            for path in &moved {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        moved.push(target);
    }
    Ok(())
}

/// Export every track and bus stem off the command thread. The invoke resolves
/// once the whole set is published; a newer export supersedes this one.
#[tauri::command]
pub async fn render_stems(
    directory: String,
    pre_roll: Option<f64>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let _timer = oc_diag::perf_counter("cmd.render_stems.ms").map(oc_diag::Timer::start);
    oc_diag::diag_info!("cmd: render_stems dir={directory} pre_roll={pre_roll:?}");
    render_stems_off_thread(state.inner(), directory, pre_roll, render_stems_snapshot).await
}

pub(crate) async fn render_stems_off_thread<F>(
    state: &AppState,
    directory: String,
    pre_roll: Option<f64>,
    render: F,
) -> Result<serde_json::Value, String>
where
    F: FnOnce(OfflineRenderSnapshot, &Path) -> Result<StemsSummary, String>
        + std::marker::Send
        + 'static,
{
    let target = directory.clone();
    let (staged, summary) = snapshot_and_run(
        state,
        pre_roll,
        &ExportOverrides::default(),
        move |snapshot| stage_stems_export(snapshot, Path::new(&target), render),
    )
    .await?;
    let manifest = staged.publish(state)?;
    Ok(serde_json::json!({
        "directory": directory,
        "manifest": manifest,
        "stems": summary.stems,
        "frames": summary.frames,
        "loudness": summary.loudness,
    }))
}

/// Export the mix, or the mix plus stems, with the dialog's choices.
///
/// Progress arrives as `export-progress` events at the checkpoints a render
/// passes; Cancel stops at the next one and nothing is published.
#[tauri::command]
pub async fn render_audio(
    app: tauri::AppHandle,
    request: RenderAudioRequest,
    state: State<'_, AppState>,
) -> Result<RenderAudioResult, String> {
    let _timer = oc_diag::perf_counter("cmd.render_audio.ms").map(oc_diag::Timer::start);
    oc_diag::diag_info!(
        "cmd: render_audio scope={:?} output={:?} dir={:?} options={:?}",
        request.scope,
        request.output_path,
        request.directory,
        request.options
    );
    match request.scope {
        ExportScope::Mix => render_mix_off_thread(state.inner(), request, Some(app)).await,
        ExportScope::Stems => render_stem_set_off_thread(state.inner(), request, Some(app)).await,
    }
}

/// Render the master mix on the blocking pool, then publish it under the guards.
pub(crate) async fn render_mix_off_thread(
    state: &AppState,
    request: RenderAudioRequest,
    app: Option<tauri::AppHandle>,
) -> Result<RenderAudioResult, String> {
    let output_path = request.output_path.clone().ok_or_else(|| {
        "the mix scope needs an output path; choose where the file goes".to_string()
    })?;
    let overrides = request.options;
    let mut snapshot = capture_offline_snapshot(state, request.pre_roll, &overrides)?;
    let generation = snapshot.generation;
    let cancel = snapshot
        .cancel
        .clone()
        .ok_or_else(|| "the export did not claim a cancel token".to_string())?;
    let staged = {
        let app = app.clone();
        let target = output_path.clone();
        let cancel = cancel.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let progress = progress_reporter(app, generation);
            snapshot.progress = Some(engine_progress(Arc::clone(&progress)));
            let checkpoint = cancel_checkpoint(cancel);
            let result = stage_offline_render(
                snapshot,
                &target,
                &overrides,
                &*progress,
                &checkpoint,
                render_offline_snapshot,
            );
            if let Err(error) = &result
                && is_cancelled(error)
            {
                progress(ExportPhase::Cancelled, 0.0);
            }
            result
        })
        .await
        .map_err(|error| format!("Render task failed: {error}"))??
    };
    let progress = progress_reporter(app, generation);
    let checkpoint = cancel_checkpoint(cancel);
    let report = staged.report;
    let frames = staged.frames;
    let spec = staged.spec;
    progress(ExportPhase::Publishing, 0.0);
    if let Err(error) = checkpoint("publishing") {
        progress(ExportPhase::Cancelled, 0.0);
        return Err(error);
    }
    staged.publish(state)?;
    progress(ExportPhase::Done, 1.0);
    Ok(mix_result(
        &PathBuf::from(&output_path),
        frames,
        spec,
        report,
    ))
}

/// Render the mix and its stem set, then publish the whole set under the guards.
pub(crate) async fn render_stem_set_off_thread(
    state: &AppState,
    request: RenderAudioRequest,
    app: Option<tauri::AppHandle>,
) -> Result<RenderAudioResult, String> {
    let directory = request
        .directory
        .clone()
        .ok_or_else(|| "the stems scope needs a folder; choose where the set goes".to_string())?;
    let overrides = request.options;
    let stem_name = request.stem_name.clone();
    let mut snapshot = capture_offline_snapshot(state, request.pre_roll, &overrides)?;
    let generation = snapshot.generation;
    let cancel = snapshot
        .cancel
        .clone()
        .ok_or_else(|| "the export did not claim a cancel token".to_string())?;
    let (staged, summary) = {
        let app = app.clone();
        let target = directory.clone();
        let cancel = cancel.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let progress = progress_reporter(app, generation);
            snapshot.progress = Some(engine_progress(Arc::clone(&progress)));
            let checkpoint = cancel_checkpoint(cancel);
            let name = stem_name;
            let result = stage_stems_export_with(
                snapshot,
                Path::new(&target),
                &*progress,
                &checkpoint,
                move |snapshot, staging| {
                    render_stems_snapshot_with(snapshot, staging, name.as_deref(), &overrides)
                },
            );
            if let Err(error) = &result
                && is_cancelled(error)
            {
                progress(ExportPhase::Cancelled, 0.0);
            }
            result
        })
        .await
        .map_err(|error| format!("Render task failed: {error}"))??
    };
    let progress = progress_reporter(app, generation);
    let checkpoint = cancel_checkpoint(cancel);
    progress(ExportPhase::Publishing, 0.0);
    if let Err(error) = checkpoint("publishing") {
        progress(ExportPhase::Cancelled, 0.0);
        return Err(error);
    }
    staged.publish(state)?;
    progress(ExportPhase::Done, 1.0);

    let directory_path = Path::new(&directory);
    let mut files: Vec<String> = summary
        .files
        .iter()
        .filter(|name| name.as_str() != "stems.json")
        .map(|name| directory_path.join(name).display().to_string())
        .collect();
    let mix = files
        .iter()
        .position(|path| path.contains("-mix."))
        .map(|index| files.remove(index));
    let path = mix.clone().unwrap_or_else(|| directory.clone());
    if let Some(mix) = mix {
        files.insert(0, mix);
    }
    let bytes = files.iter().map(|file| file_bytes(file)).sum();
    Ok(RenderAudioResult {
        scope: ExportScope::Stems,
        path,
        files,
        stems: summary.stems,
        bytes,
        frames: summary.frames,
        sample_rate: summary.spec.sample_rate,
        container: summary.spec.container,
        depth: summary.spec.depth(),
        dither: summary.spec.dither(),
        peak_dbfs: summary.report.pre_clamp_peak_dbfs,
        true_peak_dbfs: summary.report.pre_clamp_true_peak_dbfs,
        above_full_scale: summary.report.above_full_scale,
        clipped: summary.report.clipped,
    })
}

/// True when `path` already holds a file the export would replace.
#[tauri::command]
pub fn export_target_exists(path: String) -> bool {
    Path::new(&path).exists()
}

/// Show a published file in the platform's file manager.
#[tauri::command]
pub fn reveal_export(path: String) -> Result<(), String> {
    crate::diagnostics::reveal_in_file_manager(&path)
}

/// Ask the running export to stop at its next checkpoint.
#[tauri::command]
pub fn cancel_export(state: State<'_, AppState>) {
    state.request_export_cancel();
}

/// Agent range render: a window in seconds or bars, plus an optional track
/// subset and per-track stems. The project lock is released before rendering
/// and CLAP instances are forked so the editor and UI playback keep their state.
#[derive(Debug, Serialize)]
pub struct RangeEncodingReport {
    pub peak_dbfs: Option<f64>,
    pub true_peak_dbfs: Option<f64>,
    pub above_full_scale: bool,
    pub clipped: bool,
}

impl From<oc_engine::encode::EncodeReport> for RangeEncodingReport {
    fn from(report: oc_engine::encode::EncodeReport) -> Self {
        let audible = report.pre_clamp_peak_dbfs.is_finite();
        Self {
            peak_dbfs: audible.then_some(report.pre_clamp_peak_dbfs),
            true_peak_dbfs: (audible && report.pre_clamp_true_peak_dbfs.is_finite())
                .then_some(report.pre_clamp_true_peak_dbfs),
            above_full_scale: report.above_full_scale,
            clipped: report.clipped,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RangeStemFile {
    pub track_index: usize,
    pub name: String,
    pub path: String,
    pub frames: usize,
    pub sample_rate: u32,
    pub channels: u16,
    pub pre_encode: RangeEncodingReport,
}

#[derive(Debug, Clone, Default)]
pub struct RangeRequest {
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub start_bar: Option<u32>,
    pub end_bar: Option<u32>,
    pub tracks: Option<Vec<usize>>,
    pub stems: bool,
    pub pre_roll: Option<f64>,
    pub float_wav: bool,
}

#[derive(Debug, Serialize)]
pub struct RangeRenderResult {
    pub path: String,
    pub start_s: f64,
    pub end_s: f64,
    pub start_bar: Option<u32>,
    pub end_bar: Option<u32>,
    pub sample_rate: u32,
    pub frames: usize,
    pub duration_s: f64,
    pub tracks: Vec<usize>,
    pub stems: Vec<RangeStemFile>,
    pub pre_encode: RangeEncodingReport,
    /// True when the request was answered from an already clean cached render
    /// without re-synthesising any source; false only for a cold cache.
    pub cache_hit: bool,
    /// How a `tracks` window was assembled; null for a full-mix window.
    pub window: Option<&'static str>,
    /// The bus whose return this file contains, when one was requested.
    pub bus: Option<String>,
}

/// Resolve a seconds pair or a notated bar pair (`end_bar` exclusive) through
/// the project clock; section repeats are not unrolled, so use seconds for those.
fn range_seconds(project: &ast::Project, request: &RangeRequest) -> Result<(f64, f64), String> {
    match (
        request.start_s,
        request.end_s,
        request.start_bar,
        request.end_bar,
    ) {
        (Some(start), Some(end), None, None) => Ok((start, end)),
        (None, None, Some(start_bar), Some(end_bar)) => {
            if start_bar == 0 || end_bar == 0 {
                return Err("bars are one-indexed".into());
            }
            if end_bar <= start_bar {
                return Err("end_bar must be after start_bar".into());
            }
            let clock = ProjectClock::new(&project.properties)?;
            let start = clock
                .seconds_at_quarter(clock.quarter_notes_at(&BarBeat::new(start_bar, 1, 0))?)?;
            let end =
                clock.seconds_at_quarter(clock.quarter_notes_at(&BarBeat::new(end_bar, 1, 0))?)?;
            Ok((start, end))
        }
        _ => Err("provide start/end seconds or start_bar/end_bar, not both".into()),
    }
}

pub fn render_range(
    output_path: String,
    request: RangeRequest,
    state: &AppState,
) -> Result<RangeRenderResult, String> {
    render_window(Some(output_path), request, None, state).map(|(result, _, _)| result)
}

/// The agent-facing range render: the narrow-window guard runs before the
/// window renders. `ocp render`, the panel's previews and a plain
/// `render_range` never reach this, so nothing a human drives is counted.
pub fn render_range_for_agent(
    output_path: String,
    request: RangeRequest,
    bus: Option<String>,
    state: &AppState,
) -> Result<RangeRenderResult, String> {
    let (revision, window_s, bar_s) = {
        let inner = state.lock_inner();
        let (start, end) = range_seconds(&inner.project, &request)?;
        (inner.revision, end - start, bar_seconds(&inner.project))
    };
    state.window_guard().admit(revision, window_s, bar_s)?;
    match bus.as_deref() {
        Some(bus) => {
            render_window(Some(output_path), request, Some(bus), state).map(|(result, _, _)| result)
        }
        None => render_range(output_path, request, state),
    }
}

/// One bar of the project at its opening tempo, for the narrow-window guard.
fn bar_seconds(project: &ast::Project) -> Option<f64> {
    let clock = ProjectClock::new(&project.properties).ok()?;
    let start = clock
        .seconds_at_quarter(clock.quarter_notes_at(&BarBeat::new(1, 1, 0)).ok()?)
        .ok()?;
    let end = clock
        .seconds_at_quarter(clock.quarter_notes_at(&BarBeat::new(2, 1, 0)).ok()?)
        .ok()?;
    (end > start).then_some(end - start)
}

/// The one range renderer both tools share: writes the master (and stems) and
/// returns the mix samples so an inline listening clip need not re-read them.
fn render_window(
    output_path: Option<String>,
    request: RangeRequest,
    bus: Option<&str>,
    state: &AppState,
) -> Result<(RangeRenderResult, Vec<f32>, Option<ScratchRender>), String> {
    if let (Some(start), Some(end)) = (request.start_s, request.end_s)
        && (!start.is_finite() || !end.is_finite() || start < 0.0)
    {
        return Err("range start/end must be finite and start >= 0".into());
    }
    if request.float_wav
        && let Some(path) = output_path.as_deref()
        && !Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return Err("float_wav requires a .wav output path".into());
    }
    if let Some(output) = output_path.as_deref().map(Path::new)
        && let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.is_dir()
    {
        return Err(format!(
            "output directory '{}' does not exist",
            parent.display()
        ));
    }

    let plugins = fork_offline_plugin_instances(state)?;
    let cancel = plugins.native_job.cancel_token();
    let (project, sample_rate, soundfont_path, resource_root, mut instances, epoch, start, end) = {
        let inner = state.lock_inner();
        plugins.validate(&inner)?;
        if let Some(tracks) = &request.tracks {
            if tracks.is_empty() {
                return Err("track subset must not be empty".into());
            }
            if let Some(index) = tracks
                .iter()
                .find(|index| **index >= inner.project.tracks.len())
            {
                return Err(format!("track {index} out of range"));
            }
        }
        let (start, end) = range_seconds(&inner.project, &request)?;
        let instances = plugins.instances;
        let resource_root = inner
            .file_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        (
            inner.project.clone(),
            inner.audio.sample_rate,
            inner.audio.soundfont_path.clone().unwrap_or_default(),
            resource_root,
            instances,
            inner.content_epoch,
            start,
            end,
        )
    };

    if !start.is_finite() || !end.is_finite() || start < 0.0 {
        return Err("range start/end must be finite and start >= 0".into());
    }
    if end <= start {
        return Err("range end must be after range start".into());
    }
    if end - start > 600.0 {
        return Err("range length must not exceed 600 seconds".into());
    }
    let start_sample = (start * f64::from(sample_rate)).round() as u64;
    let end_sample = (end * f64::from(sample_rate)).round() as u64;
    if end_sample <= start_sample {
        return Err("range is shorter than one sample at this sample rate".into());
    }
    let tracks = request.tracks.clone();

    let _render_guard = state.lock_range_render();
    let mut export_spec = EncodeSpec::for_project(&project, sample_rate);
    if request.float_wav {
        export_spec.container = ast::ExportContainer::Wav;
        export_spec.wav.depth = ast::SampleFormat::F32;
        export_spec.wav.dither = false;
    }
    let scratch_bytes = if output_path.is_none() {
        export_spec.container = ast::ExportContainer::Wav;
        let bytes = (end_sample - start_sample)
            .saturating_mul(u64::from(export_spec.channels) * 4)
            .saturating_add(4096);
        ScratchRender::check_size(bytes)?;
        Some(bytes)
    } else {
        None
    };
    // A bus tap renders the master path too, then writes one bus return. The
    // graph is never muted: a sidechain key on another track must keep
    // ducking the return that is being observed.
    let tapped_bus = bus;
    if tapped_bus.is_some() && request.tracks.is_some() {
        return Err(
            "a bus tap renders the whole graph; do not combine it with a track subset".into(),
        );
    }
    if let Some(bus) = tapped_bus
        && !project.buses.iter().any(|candidate| candidate.name == bus)
    {
        return Err(format!("no bus named '{bus}' in this project"));
    }
    let mut cache = state.take_range_cache(epoch, sample_rate);
    if tapped_bus.is_some() {
        cache.capture_export_stems();
    }
    cache.set_worker_options(crate::gui_settings::load().worker_options());
    cache.set_resource_root(resource_root);
    cache.set_cancel_token(cancel.clone());
    if let Some(pre_roll) = request.pre_roll {
        cache.set_pre_roll_seconds(pre_roll)?;
    }
    let rendered = cache.render_range(
        &project,
        &soundfont_path,
        sample_rate,
        &mut instances,
        oc_engine::render_cache::RangeRenderRequest {
            start_sample,
            end_sample,
            tracks: tracks.clone(),
            stems: request.stems,
        },
    )?;
    if cancel.is_cancelled() {
        return Err(oc_engine::render_cancel::CANCELLED_MESSAGE.into());
    }
    let tapped = match tapped_bus {
        Some(bus) => {
            let capture = cache
                .stems()
                .ok_or("the range render captured no bus stems")?;
            let buffer = capture
                .buses
                .iter()
                .find(|(name, _)| name == bus)
                .map(|(_, samples)| samples.clone())
                .ok_or_else(|| format!("bus '{bus}' produced no returned signal"))?;
            Some(buffer)
        }
        None => None,
    };
    cache.clear_render_controls();
    let scratch = scratch_bytes.map(ScratchRender::reserve).transpose()?;
    let output = output_path
        .as_deref()
        .map(Path::new)
        .or_else(|| scratch.as_ref().map(ScratchRender::path))
        .ok_or("Missing range output path")?;
    let audio = tapped.as_deref().unwrap_or(&rendered.audio);
    let pre_encode = oc_engine::encode::encode_to_file(output, &export_spec, audio)?.into();

    let mut stem_files = Vec::new();
    for (index, name, buffer) in &rendered.stems {
        if !rendered.tracks.contains(index) {
            continue;
        }
        let path = stem_path(output, *index, name);
        let mut stem_spec = export_spec.with_dither_seed(oc_engine::encode::seed_for_name(
            export_spec.wav.dither_seed,
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("stem"),
        ));
        stem_spec.container = ast::ExportContainer::Wav;
        let pre_encode = oc_engine::encode::encode_to_file(&path, &stem_spec, buffer)?.into();
        stem_files.push(RangeStemFile {
            track_index: *index,
            name: name.clone(),
            path: path.display().to_string(),
            frames: buffer.len() / usize::from(stem_spec.channels),
            sample_rate: stem_spec.sample_rate,
            channels: stem_spec.channels,
            pre_encode,
        });
    }
    // The analysis grid follows the WAV: `analyze_notes` gets a path, not the
    // project that produced it.
    let grid = crate::render_grid::RenderGrid::from_project(&project);
    crate::render_grid::write(output, &grid);
    for stem in &stem_files {
        crate::render_grid::write(std::path::Path::new(&stem.path), &grid);
    }
    if !cache.is_dirty() {
        state.store_range_cache(epoch, sample_rate, cache);
    }

    Ok((
        RangeRenderResult {
            path: output.display().to_string(),
            start_s: start_sample as f64 / f64::from(sample_rate),
            end_s: end_sample as f64 / f64::from(sample_rate),
            start_bar: request.start_bar,
            end_bar: request.end_bar,
            sample_rate,
            frames: rendered.frames,
            duration_s: rendered.frames as f64 / f64::from(sample_rate),
            tracks: rendered.tracks.clone(),
            stems: stem_files,
            pre_encode,
            cache_hit: rendered.cache_hit,
            window: rendered.window.map(|path| match path {
                oc_engine::render_cache::RangeWindowPath::Snapshot => "snapshot",
                oc_engine::render_cache::RangeWindowPath::Recomputed => "recomputed",
                oc_engine::render_cache::RangeWindowPath::Cold => "cold",
                oc_engine::render_cache::RangeWindowPath::Silent => "silent",
            }),
            bus: tapped_bus.map(str::to_owned),
        },
        rendered.audio,
        scratch,
    ))
}

// ============================================================================
// Audition: a listen-only clip for the agent
// ============================================================================

/// Inline listening clips are mono 16 kHz because that is what the provider's
/// audio pipeline keeps: its documentation states that multi-channel audio is
/// combined to a single channel and the resolution is downsampled, and that a
/// second of audio costs 32 tokens. A higher rate or a second channel would be
/// discarded upstream, so the clip is prepared the same way and only the bytes
/// that survive are sent. The full-rate render stays beside it in `path`.
pub const AUDITION_SAMPLE_RATE: u32 = 16_000;
/// A model listens to a phrase or a section, not a whole song.
pub const AUDITION_MAX_SECONDS: f64 = 30.0;
/// Base64 inflates by 4/3; 2 MiB keeps one clip inside the inline budget.
pub const AUDITION_MAX_BASE64_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct AuditionAudio {
    pub mime: &'static str,
    pub sample_rate: u32,
    pub channels: u32,
    pub base64: String,
}

#[derive(Debug, Serialize)]
pub struct AuditionRangeResult {
    pub path: String,
    pub start_s: f64,
    pub end_s: f64,
    /// Length of the returned clip, which is shorter than the window when the
    /// request was truncated.
    pub duration_s: f64,
    pub tracks: Vec<usize>,
    /// Full-rate saved window before encoding, even when inline audio is truncated.
    pub pre_encode: RangeEncodingReport,
    pub audio: AuditionAudio,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Render a window exactly like `render_range` and return a mono 16 kHz WAV of
/// it for the model to listen to. `path` is optional: without one the full-rate
/// render lands in bounded audition scratch storage.
#[cfg(test)]
pub fn audition_range(
    output_path: Option<String>,
    request: RangeRequest,
    channel: AuditionChannel,
    target_rate: u32,
    state: &AppState,
) -> Result<AuditionRangeResult, String> {
    audition_range_with_handoff(output_path, request, channel, target_rate, state, false)
}

fn audition_range_with_handoff(
    output_path: Option<String>,
    mut request: RangeRequest,
    channel: AuditionChannel,
    target_rate: u32,
    state: &AppState,
    archive_handoff: bool,
) -> Result<AuditionRangeResult, String> {
    request.stems = false;
    let (mut rendered, samples, scratch) = render_window(output_path, request, None, state)?;
    // A provider that takes one channel cannot hear a stereo clip, so the
    // request is narrowed to what the surface accepts and the result says so.
    let (clip, channels, truncated) =
        audition_clip(&samples, rendered.sample_rate, channel, target_rate)?;
    let bytes = oc_engine::wav::pcm_wav_bytes(&clip, target_rate, channels)?;
    if let Some(scratch) = scratch {
        rendered.path = scratch.publish(archive_handoff)?.display().to_string();
    }
    Ok(AuditionRangeResult {
        path: rendered.path,
        start_s: rendered.start_s,
        end_s: rendered.end_s,
        duration_s: clip.len() as f64 / (f64::from(target_rate) * f64::from(channels)),
        tracks: rendered.tracks,
        pre_encode: rendered.pre_encode,
        audio: AuditionAudio {
            mime: "audio/wav",
            sample_rate: target_rate,
            channels: u32::from(channels),
            base64: {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(bytes)
            },
        },
        truncated,
    })
}

#[derive(Debug, Serialize)]
pub struct AuditionClipSourceResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_index: Option<usize>,
    pub start_s: f64,
    pub end_s: f64,
    pub duration_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_duration_s: Option<f64>,
    pub audio: AuditionAudio,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// Read source seconds before any clip or mixer processing, without creating
/// a full-rate render or marking the current arrangement as listened to.
pub(crate) fn audition_clip_source(
    path: &std::path::Path,
    clip_index: usize,
    start: f64,
    duration: f64,
    channel: AuditionChannel,
    target_rate: u32,
) -> Result<AuditionClipSourceResult, String> {
    audition_source_window(
        path,
        Some(clip_index),
        start,
        duration,
        channel,
        target_rate,
    )
}

pub(crate) fn audition_source_window(
    path: &Path,
    clip_index: Option<usize>,
    start: f64,
    duration: f64,
    channel: AuditionChannel,
    target_rate: u32,
) -> Result<AuditionClipSourceResult, String> {
    if !start.is_finite()
        || start < 0.0
        || !duration.is_finite()
        || duration <= 0.0
        || duration > AUDITION_MAX_SECONDS
    {
        return Err("Source audition needs a nonnegative start and a duration greater than 0 and at most 30 seconds".into());
    }
    // Standard WAV archives can seek directly without populating the decoded
    // media cache with audio before or after the passage.
    let mut wav = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        .then(|| oc_engine::wav::open_reader(path).ok())
        .flatten()
        .filter(|reader| matches!(reader.spec().channels, 1 | 2) && reader.spec().sample_rate > 0);
    let (sample_rate, first, requested, samples, source_frames) =
        if wav.is_none() && clip_index.is_none() {
            let passage = oc_engine::audio_clip::decode_passage(path, start, duration)?;
            (
                passage.audio.sample_rate,
                passage.first_frame,
                passage.requested_frames,
                passage.audio.samples,
                passage.source_frames,
            )
        } else {
            let (sample_rate, frames, media) = match wav.as_ref() {
                Some(reader) => (reader.spec().sample_rate, reader.duration() as usize, None),
                None => {
                    let media = oc_engine::media_cache::shared().decoded(path)?;
                    (media.sample_rate(), media.frames(), Some(media))
                }
            };
            let rate = f64::from(sample_rate);
            let first = (start * rate).round() as usize;
            if first >= frames {
                return Err("Source audition starts at or beyond the end of the file".into());
            }
            let requested = ((duration * rate).round() as usize).max(1);
            let count = requested.min(frames - first);
            let samples = if let Some(media) = media {
                media.read_window(first..first + count)?
            } else {
                let reader = wav.as_mut().expect("a WAV reader when media is not cached");
                let samples = oc_engine::wav::read_stereo_window(reader, first..first + count)?;
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return Err(format!("{}: non-finite sample", path.display()));
                }
                samples
            };
            (
                sample_rate,
                first,
                requested,
                std::sync::Arc::new(samples),
                Some(frames),
            )
        };
    let rate = f64::from(sample_rate);
    let count = samples.len() / 2;
    let (clip, channels, truncated) = audition_clip(&samples, sample_rate, channel, target_rate)?;
    if clip.is_empty() {
        return Err("Source audition window is too short at the requested sample rate".into());
    }
    let duration_s = clip.len() as f64 / (f64::from(target_rate) * f64::from(channels));
    let bytes = oc_engine::wav::pcm_wav_bytes(&clip, target_rate, channels)?;
    let start_s = first as f64 / rate;
    Ok(AuditionClipSourceResult {
        clip_index,
        start_s,
        end_s: (start_s + duration_s).min((first + count) as f64 / rate),
        duration_s,
        source_duration_s: source_frames.map(|frames| frames as f64 / rate),
        audio: AuditionAudio {
            mime: "audio/wav",
            sample_rate: target_rate,
            channels: u32::from(channels),
            base64: {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(bytes)
            },
        },
        truncated: truncated || count < requested,
    })
}

/// The Playback tab reads the same source window as the agent and returns
/// stereo audio for an explicit play action in the webview.
#[tauri::command]
pub async fn preview_audio_clip_source(
    clip_index: usize,
    source_start_seconds: f64,
    duration_seconds: f64,
    state: tauri::State<'_, AppState>,
) -> Result<AuditionClipSourceResult, String> {
    if duration_seconds > 8.0 {
        return Err("Clip source preview is limited to eight seconds".into());
    }
    let path = crate::commands::audio_clip_source_path(state.inner(), clip_index)?;
    super::shared::run_blocking(move || {
        audition_clip_source(
            &path,
            clip_index,
            source_start_seconds,
            duration_seconds,
            AuditionChannel::Stereo,
            48_000,
        )
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct SavedAuditionPreview {
    pub mime: &'static str,
    pub base64: String,
}

#[tauri::command]
pub async fn preview_saved_audition(path: String) -> Result<SavedAuditionPreview, String> {
    super::shared::run_blocking(move || {
        let store = crate::ai::sessions::Store::config();
        read_saved_audition(store.root(), std::path::Path::new(&path))
    })
    .await
}

/// Grant the webview a playback URL for this one archived render.
#[tauri::command]
pub async fn open_saved_audition_render(path: String) -> Result<String, String> {
    super::shared::run_blocking(move || {
        let store = crate::ai::sessions::Store::config();
        let path = saved_audition_render_path(store.root(), Path::new(&path))?;
        crate::agent_api::audition_render_url(&path)
    })
    .await
}

pub(crate) fn saved_audition_render_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let (path, _) = archived_audition_file(root, path, "audition-renders")?;
    let length = path
        .metadata()
        .map_err(|error| format!("Cannot measure the saved render: {error}"))?
        .len();
    if length == 0 || length > crate::ai::sessions::AUDITION_ARCHIVE_BUDGET {
        return Err("The saved render is empty or exceeds the archive limit".into());
    }
    Ok(path)
}

fn archived_audition_file(
    root: &Path,
    path: &Path,
    subdir: &str,
) -> Result<(PathBuf, &'static str), String> {
    let archive = root
        .canonicalize()
        .map_err(|error| format!("Cannot open the audition archive: {error}"))?
        .join(subdir);
    let path = path
        .canonicalize()
        .map_err(|error| format!("Cannot open the saved audition: {error}"))?;
    let hashed_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if path.parent() != Some(archive.as_path()) || !hashed_name || !path.is_file() {
        return Err("The requested file is outside the saved audition archive".into());
    }
    let mime = match path.extension().and_then(|extension| extension.to_str()) {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        _ => return Err("Unsupported saved audition format".into()),
    };
    Ok((path, mime))
}

pub(crate) fn read_saved_audition(
    root: &std::path::Path,
    path: &std::path::Path,
) -> Result<SavedAuditionPreview, String> {
    use base64::Engine as _;
    use std::io::Read as _;
    let (path, mime) = archived_audition_file(root, path, "audition-media")?;
    let limit = (AUDITION_MAX_BASE64_BYTES / 4 * 3) as u64;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("Cannot read the saved audition: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err("The saved audition is empty or exceeds the preview limit".into());
    }
    Ok(SavedAuditionPreview {
        mime,
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// The agent-facing audition: always renders, and clears the loop counter —
/// listening is the remedy the refusal names.
pub fn audition_range_for_agent(
    output_path: Option<String>,
    request: RangeRequest,
    channel: AuditionChannel,
    target_rate: u32,
    state: &AppState,
    archive_handoff: bool,
) -> Result<AuditionRangeResult, String> {
    let result = audition_range_with_handoff(
        output_path,
        request,
        channel,
        target_rate,
        state,
        archive_handoff,
    )?;
    let revision = state.lock_inner().revision;
    state.window_guard().listened(revision);
    Ok(result)
}

/// File-relative boundaries and the exact clip sent to the transcript player.
#[derive(Debug, Serialize)]
pub struct AudioFilePreview {
    #[serde(flatten)]
    pub audio: AuditionAudio,
    pub start_s: f64,
    pub end_s: f64,
    pub clip_duration_s: f64,
    pub truncated: bool,
}

/// Open a bounded audition without a second render pass.
#[tauri::command]
pub async fn audio_file_preview(
    path: String,
    start_seconds: Option<f64>,
    duration_seconds: Option<f64>,
) -> Result<AudioFilePreview, String> {
    super::shared::run_blocking(move || {
        audio_file_preview_blocking(&path, start_seconds, duration_seconds)
    })
    .await
}

pub(crate) fn audio_file_preview_blocking(
    path: &str,
    start: Option<f64>,
    duration: Option<f64>,
) -> Result<AudioFilePreview, String> {
    let path = Path::new(path);
    let _scratch = super::audition_scratch::pin_reader(path)?;
    let result = audition_source_window(
        path,
        None,
        start.unwrap_or(0.0),
        duration.unwrap_or(AUDITION_MAX_SECONDS),
        AuditionChannel::Mono,
        AUDITION_SAMPLE_RATE,
    )?;
    let truncated = if duration.is_some() {
        result.truncated
    } else {
        // For an open-ended request, this means there is more source past
        // the preview cap. A short file already fits in one preview.
        result
            .source_duration_s
            .is_none_or(|total| total > result.end_s + 1.0 / f64::from(AUDITION_SAMPLE_RATE))
    };
    Ok(AudioFilePreview {
        audio: result.audio,
        start_s: result.start_s,
        end_s: result.end_s,
        clip_duration_s: result.duration_s,
        truncated,
    })
}

/// Which side of the render an audition clip carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuditionChannel {
    /// Equal-weight downmix of both sides.
    #[default]
    Mono,
    /// The left side alone, as a mono clip.
    Left,
    /// The right side alone, as a mono clip.
    Right,
    /// Both sides, interleaved.
    Stereo,
}

impl AuditionChannel {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "mono" => Ok(Self::Mono),
            "left" | "l" => Ok(Self::Left),
            "right" | "r" => Ok(Self::Right),
            "stereo" | "both" => Ok(Self::Stereo),
            other => Err(format!(
                "unknown audition channel '{other}'; accepted: mono, left, right, stereo"
            )),
        }
    }

    pub fn channels(self) -> u16 {
        match self {
            Self::Stereo => 2,
            _ => 1,
        }
    }
}

/// Build the clip the model listens to: the requested sides, band-limited to
/// `target_rate`, capped by the inline size budget. Returns the interleaved
/// samples, the channel count and whether the clip is shorter than the window.
fn audition_clip(
    interleaved: &[f32],
    sample_rate: u32,
    channel: AuditionChannel,
    target_rate: u32,
) -> Result<(Vec<f32>, u16, bool), String> {
    if sample_rate == 0 || target_rate == 0 {
        return Err("audition needs a positive sample rate".into());
    }
    let frames = interleaved.len() / 2;
    let source_limit =
        ((AUDITION_MAX_SECONDS * f64::from(sample_rate)).round() as usize).min(frames);
    let channels = channel.channels();
    // The resampler works on interleaved stereo, so a one-channel selection is
    // duplicated into both sides and folded back afterwards.
    let window = &interleaved[..source_limit * 2];
    let picked;
    let selected = match channel {
        AuditionChannel::Stereo => window,
        _ => {
            let mut mono = Vec::with_capacity(window.len());
            for frame in window.chunks_exact(2) {
                let sample = match channel {
                    AuditionChannel::Mono => 0.5 * (frame[0] + frame[1]),
                    AuditionChannel::Left => frame[0],
                    AuditionChannel::Right => frame[1],
                    AuditionChannel::Stereo => unreachable!(),
                };
                mono.extend([sample; 2]);
            }
            picked = mono;
            &picked
        }
    };
    let resampled = oc_engine::resample::resample(
        selected,
        sample_rate,
        target_rate,
        oc_engine::resample::ResampleQuality::DEFAULT_LIVE,
    )?;
    let mut clip: Vec<f32> = if channels == 1 {
        resampled.into_iter().step_by(2).collect()
    } else {
        resampled
    };
    // 16-bit samples are two bytes each; the header is 44 bytes.
    let bytes_per_frame = 2 * usize::from(channels);
    let by_bytes = (AUDITION_MAX_BASE64_BYTES / 4 * 3).saturating_sub(44) / bytes_per_frame;
    let cap = ((AUDITION_MAX_SECONDS * f64::from(target_rate)) as usize).min(by_bytes);
    clip.truncate(cap * usize::from(channels));
    let expected =
        (frames as f64 * f64::from(target_rate) / f64::from(sample_rate)).round() as usize;
    let truncated = clip.len() / usize::from(channels) < expected;
    Ok((clip, channels, truncated))
}

/// Eight seconds fits each encoded delivery at every supported audio rate.
pub(crate) const PAIR_MAX_SECONDS: f64 = 8.0;

#[derive(Serialize)]
pub(crate) struct ListeningPair {
    status: &'static str,
    duration_s: f64,
    reference_lufs: Option<f64>,
    candidate_lufs: Option<f64>,
    delta_lu: Option<f64>,
    reference_gain_db: f64,
    candidate_gain_db: f64,
    reference_audio: AuditionAudio,
    candidate_audio: AuditionAudio,
}

pub(crate) fn listening_pair(
    reference: &oc_engine::analysis::AudioBuffer,
    candidate: &oc_engine::analysis::AudioBuffer,
    channel: AuditionChannel,
    rate: u32,
) -> Result<ListeningPair, String> {
    use base64::Engine as _;
    use oc_engine::analysis::{AudioBuffer, integrated_lufs};

    let prepare = |buffer: &AudioBuffer| {
        let left = &buffer.channels[0];
        let right = buffer.channels.get(1).unwrap_or(left);
        let stereo: Vec<f32> = left
            .iter()
            .zip(right)
            .flat_map(|(&left, &right)| [left as f32, right as f32])
            .collect();
        audition_clip(&stereo, buffer.sample_rate, channel, rate).map(|(samples, _, _)| samples)
    };
    let mut takes = [prepare(reference)?, prepare(candidate)?];
    let channels = channel.channels();
    let width = usize::from(channels);
    let frames = takes[0].len().min(takes[1].len()) / width;
    if frames == 0 {
        return Err("The comparison excerpts contain no playable frames".into());
    }
    for take in &mut takes {
        take.truncate(frames * width);
    }
    let measure = |samples: &[f32]| {
        let mut channels = vec![Vec::with_capacity(frames); width];
        for (index, &sample) in samples.iter().enumerate() {
            channels[index % width].push(f64::from(sample));
        }
        integrated_lufs(&AudioBuffer {
            sample_rate: rate,
            channels,
        })
    };
    let mut gains = [0.0, 0.0];
    let attenuate = |samples: &mut [f32], gain_db: f64| {
        let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
        samples.iter_mut().for_each(|sample| *sample *= gain);
    };
    // Gating can change after attenuation, so remeasure before another pass.
    for _ in 0..3 {
        let (Some(a), Some(b)) = (measure(&takes[0]), measure(&takes[1])) else {
            break;
        };
        let delta = b - a;
        if delta.abs() < 0.05 {
            break;
        }
        let louder = usize::from(delta > 0.0);
        let trim = -delta.abs();
        attenuate(&mut takes[louder], trim);
        gains[louder] += trim;
    }
    // Preserve the relative gain when peaks need more headroom.
    let peak = takes
        .iter()
        .flatten()
        .fold(0.0_f64, |peak, &x| peak.max(f64::from(x.abs())));
    let ceiling = 10.0_f64.powf(-1.0 / 20.0);
    if peak > ceiling {
        let trim = 20.0 * (ceiling / peak).log10();
        for (take, gain) in takes.iter_mut().zip(&mut gains) {
            attenuate(take, trim);
            *gain += trim;
        }
    }

    let encode = |samples: &[f32]| -> Result<(AuditionAudio, Option<f64>), String> {
        let wav = oc_engine::wav::pcm_wav_bytes(samples, rate, channels)?;
        let mut reader =
            hound::WavReader::new(std::io::Cursor::new(&wav)).map_err(|error| error.to_string())?;
        let pcm: Result<Vec<f32>, _> = reader
            .samples::<i16>()
            .map(|sample| sample.map(|sample| f32::from(sample) / 32768.0))
            .collect();
        let lufs = measure(&pcm.map_err(|error| error.to_string())?);
        Ok((
            AuditionAudio {
                mime: "audio/wav",
                sample_rate: rate,
                channels: u32::from(channels),
                base64: base64::engine::general_purpose::STANDARD.encode(wav),
            },
            lufs,
        ))
    };
    let (reference_audio, reference_lufs) = encode(&takes[0])?;
    let (candidate_audio, candidate_lufs) = encode(&takes[1])?;
    let delta_lu = reference_lufs.zip(candidate_lufs).map(|(a, b)| b - a);
    let status = match delta_lu {
        Some(delta) if delta.abs() < 0.1 => "matched",
        Some(_) => "unmatched",
        None => "unmeasured",
    };
    Ok(ListeningPair {
        status,
        duration_s: frames as f64 / f64::from(rate),
        reference_lufs,
        candidate_lufs,
        delta_lu,
        reference_gain_db: gains[0],
        candidate_gain_db: gains[1],
        reference_audio,
        candidate_audio,
    })
}

pub(crate) fn stem_path(output: &std::path::Path, index: usize, name: &str) -> PathBuf {
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "render".into());
    let safe: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    output.with_file_name(format!("{stem}.track-{index:02}-{safe}.wav"))
}

#[cfg(test)]
mod native_job_lifetime_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn native_jobs_actual_snapshot_drops_progress_owners_before_completion_receipt() {
        struct ProgressOwner {
            registry: Arc<crate::native_jobs::NativeJobs>,
            observed: Arc<AtomicBool>,
        }
        impl Drop for ProgressOwner {
            fn drop(&mut self) {
                assert_eq!(
                    self.registry.active(),
                    1,
                    "receipt must still own the native job"
                );
                self.observed.store(true, Ordering::Release);
            }
        }
        let state = AppState::new();
        let mut snapshot =
            capture_offline_snapshot(&state, None, &ExportOverrides::default()).unwrap();
        let registry = Arc::clone(&state.native_jobs);
        assert_eq!(registry.active(), 1);
        let observed = Arc::new(AtomicBool::new(false));
        let owner = ProgressOwner {
            registry: Arc::clone(&registry),
            observed: Arc::clone(&observed),
        };
        snapshot.progress = Some(Box::new(move |_| {
            let _ = &owner;
        }));
        drop(snapshot);
        assert!(observed.load(Ordering::Acquire));
        assert_eq!(registry.active(), 0);
    }

    #[test]
    fn native_jobs_closed_registry_refuses_snapshot_before_native_preparation() {
        let state = AppState::new();
        state.native_jobs.close_and_cancel();
        assert!(capture_offline_snapshot(&state, None, &ExportOverrides::default()).is_err());
        assert!(fork_offline_plugin_instances(&state).is_err());
        assert_eq!(state.native_jobs.active(), 0);
    }
}
