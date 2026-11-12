//! Shared snapshots, undo-aware resync, and string-to-AST parsing helpers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use oc_dsl::ast::{self, *};

/// Directory the project's relative media paths resolve against.
pub fn project_root(inner: &crate::state::AppStateInner) -> Option<PathBuf> {
    inner
        .file_path
        .as_deref()
        .and_then(Path::parent)
        .map(|root| {
            if root.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                root.to_path_buf()
            }
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Project document and its UI state.
pub struct ProjectSnapshot {
    pub project: ast::Project,
    pub dsl_source: String,
    pub file_path: Option<String>,
    pub dirty: bool,
    pub track_count: usize,
    pub revision: u64,
    /// Ephemeral admission authority; absent only when no new identity can be reserved.
    #[serde(default)]
    pub parameter_target_stamp: Option<crate::state::ParameterTargetStamp>,
    /// Written arrangement length, known before the first playback.
    pub duration_seconds: f64,
    /// The recovery copy or previous version a restore applied, when this
    /// snapshot answers a restore.
    #[serde(default)]
    pub restored_from: Option<String>,
}

/// Check the observed target authority under the same lock as membership minting.
pub(crate) fn require_parameter_admission(
    inner: &crate::state::AppStateInner,
    expected: &crate::state::ParameterTargetStamp,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    inner
        .undo
        .require_parameter_target_stamp(expected)
        .map_err(|error| error.to_string())
}

/// Run a blocking command body on the blocking pool. The webview thread only
/// parses the request and the reply, so it keeps painting while the work runs.
pub(crate) async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    T: std::marker::Send + 'static,
    F: FnOnce() -> Result<T, String> + std::marker::Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Background task failed: {error}"))?
}

pub(crate) fn snapshot(inner: &crate::state::AppStateInner) -> ProjectSnapshot {
    ProjectSnapshot {
        track_count: inner.project.tracks.len(),
        duration_seconds: crate::project_extent::arrangement_seconds(&inner.project).unwrap_or(0.0),
        project: inner.project.clone(),
        dsl_source: inner.dsl_source.clone(),
        file_path: inner.file_path.as_ref().map(|p| p.display().to_string()),
        dirty: inner.dirty,
        revision: inner.revision,
        parameter_target_stamp: inner.undo.parameter_target_stamp().cloned(),
        restored_from: None,
    }
}

/// Which audio state the edit invalidates.
#[derive(Clone, Copy)]
enum AudioSync {
    /// Structure changed: the render thread must rebuild its sources.
    Full,
    /// Mixer-only: patch the live graph, never rebuild instruments.
    Mixer,
    /// FX-chain-only: patch the affected chains, never the instruments.
    Chain,
}

/// Re-serialize DSL from AST after any mutation that changes audio content.
/// Pushes to undo stack by diffing against the pre-mutation snapshot.
/// `old_project` is the project state BEFORE the mutation was applied; `edit`
/// carries the caller's name, scopes and merge request.
pub(crate) fn resync_with_undo(
    inner: &mut crate::state::AppStateInner,
    old_project: ast::Project,
    edit: crate::state::UndoEdit,
) -> Result<(), String> {
    resync(inner, old_project, edit, AudioSync::Full)
}

/// Publish a document edit only when the project it produces still validates.
///
/// `mutate` runs on a copy of the project. A refusal returns the validator's
/// own sentences and leaves the project, the revision, `dirty` and the undo
/// history exactly as they were; a success is one undo entry, like a direct
/// [`resync_with_undo`].
pub(crate) fn commit_validated_edit<F>(
    inner: &mut crate::state::AppStateInner,
    edit: crate::state::UndoEdit,
    mutate: F,
) -> Result<(), String>
where
    F: FnOnce(&mut ast::Project) -> Result<(), String>,
{
    let mut next = inner.project.clone();
    mutate(&mut next)?;
    oc_dsl::validate(&next).map_err(|errors| validation_text(&errors))?;
    let old = std::mem::replace(&mut inner.project, next);
    resync_with_undo(inner, old, edit)
}

/// The validator's own sentences, one per line.
pub(crate) fn validation_text(errors: &[oc_dsl::error::ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resync for effect-chain edits: only the affected chains are patched, so
/// instruments, sounding voices and the playhead stay as they were.
pub(crate) fn resync_chain_with_undo(
    inner: &mut crate::state::AppStateInner,
    old_project: ast::Project,
    edit: crate::state::UndoEdit,
) -> Result<(), String> {
    resync(inner, old_project, edit, AudioSync::Chain)
}

/// The refusal a document edit gets while a preview holds the document.
///
/// The sentence is [`crate::preview::READ_ONLY`], the same one the panel shows,
/// so a refused take and a refused command say the same thing.
pub(crate) fn preview_refusal(inner: &crate::state::AppStateInner) -> Option<String> {
    inner
        .preview
        .is_some()
        .then(|| crate::preview::READ_ONLY.to_string())
}

/// Resync for gain/pan/mute/solo edits: the instruments stay alive.
pub(crate) fn resync_mixer_with_undo(
    inner: &mut crate::state::AppStateInner,
    old_project: ast::Project,
    edit: crate::state::UndoEdit,
) -> Result<(), String> {
    resync(inner, old_project, edit, AudioSync::Mixer)
}

fn resync(
    inner: &mut crate::state::AppStateInner,
    old_project: ast::Project,
    edit: crate::state::UndoEdit,
    sync: AudioSync,
) -> Result<(), String> {
    // An open edit preview holds two documents. A mutation would land on the
    // side on screen and make the comparison meaningless, so every document
    // edit is refused here, in the command layer, not only in the panel. The
    // caller already edited the AST, so put the previewed side back exactly as
    // the preview stored it: a refused command changes nothing.
    if let Some(session) = inner.preview.as_ref() {
        let project = session.project_for(session.side).clone();
        let dsl_source = session.dsl_for(session.side).to_string();
        inner.project = project;
        inner.dsl_source = dsl_source;
        return Err(crate::preview::READ_ONLY.to_string());
    }
    inner.retire_touches_for_topology_change(&old_project);
    inner.audio.resource_root = inner
        .file_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    oc_dsl::instruments::apply_defaults(&mut inner.project);
    inner
        .audio
        .invalidate_inherited_live_synth(&old_project, &inner.project);
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner.dirty = true;
    match sync {
        // Eagerly pre-load the updated project into the render thread so playback
        // starts instantly when the user next presses play.
        AudioSync::Full => {
            inner.audio.mark_dirty();
            inner.audio.preload_project(&inner.project);
        }
        AudioSync::Mixer => inner.audio.apply_mixer_change(&inner.project),
        AudioSync::Chain => inner.audio.apply_chain_change(&inner.project),
    }
    // Split borrow: undo gets &mut, project gets & — no clone needed for `new`.
    // `old_project` is moved into push (avoids a clone for the merge base).
    let project_ref = &inner.project;
    let undo = &mut inner.undo;
    if let Err(reason) = undo.push(old_project, project_ref, edit) {
        // The edit is applied; only its history level is missing.
        log::error!("edit applied but not recorded in the undo history: {reason}");
    }
    inner.bump_revision();
    Ok(())
}

// ============================================================================
// Parsing helpers (string → AST enums)
// ============================================================================

pub(crate) fn parse_note_name(s: &str) -> Result<NoteName, String> {
    match s {
        "C" => Ok(NoteName::C),
        "D" => Ok(NoteName::D),
        "E" => Ok(NoteName::E),
        "F" => Ok(NoteName::F),
        "G" => Ok(NoteName::G),
        "A" => Ok(NoteName::A),
        "B" => Ok(NoteName::B),
        _ => Err(format!("Invalid note name: {}", s)),
    }
}

pub(crate) fn parse_accidental(s: &str) -> Result<Accidental, String> {
    match s {
        "" | "Natural" => Ok(Accidental::Natural),
        "#" | "Sharp" => Ok(Accidental::Sharp),
        "##" | "DoubleSharp" => Ok(Accidental::DoubleSharp),
        "b" | "Flat" => Ok(Accidental::Flat),
        "bb" | "DoubleFlat" => Ok(Accidental::DoubleFlat),
        _ => Err(format!("Invalid accidental: {}", s)),
    }
}

pub(crate) fn parse_base_duration(s: &str) -> Result<BaseDuration, String> {
    match s {
        "w" | "Whole" => Ok(BaseDuration::Whole),
        "h" | "Half" => Ok(BaseDuration::Half),
        "q" | "Quarter" => Ok(BaseDuration::Quarter),
        "8th" | "Eighth" => Ok(BaseDuration::Eighth),
        "16th" | "Sixteenth" => Ok(BaseDuration::Sixteenth),
        "32nd" | "ThirtySecond" => Ok(BaseDuration::ThirtySecond),
        "64th" | "SixtyFourth" => Ok(BaseDuration::SixtyFourth),
        _ => Err(format!("Invalid duration: {}", s)),
    }
}

pub(crate) enum Target {
    Track(usize),
    Bus(String),
    Master,
}

impl Target {
    /// The clip-pool owner that names the same section.
    pub(crate) fn automation_owner(&self) -> ast::AutomationOwner {
        match self {
            Target::Track(index) => ast::AutomationOwner::Track(*index),
            Target::Bus(name) => ast::AutomationOwner::Bus(name.clone()),
            Target::Master => ast::AutomationOwner::Master,
        }
    }
}

pub(crate) fn parse_target(s: &str) -> Result<Target, String> {
    if s == "master" {
        return Ok(Target::Master);
    }
    if let Some(idx_str) = s.strip_prefix("track:") {
        let idx: usize = idx_str
            .parse()
            .map_err(|_| format!("Invalid track index: {}", idx_str))?;
        return Ok(Target::Track(idx));
    }
    if let Some(name) = s.strip_prefix("bus:") {
        return Ok(Target::Bus(name.to_string()));
    }
    Err(format!(
        "Invalid target: {}. Use 'track:N', 'bus:name', or 'master'",
        s
    ))
}

pub(crate) fn parse_param_value(param_value: &str) -> Result<ast::ParamValue, String> {
    let trimmed = param_value.trim();
    let lower = trimmed.to_lowercase();

    if let Some(stripped) = trimmed.strip_suffix("dB")
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Db(n));
    }
    if let Some(stripped) = trimmed.strip_suffix('%')
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Percentage(n));
    }
    if let Some(stripped) = lower.strip_suffix("khz")
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Frequency(n * 1000.0));
    }
    if let Some(stripped) = lower.strip_suffix("hz")
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Frequency(n));
    }
    if let Some(stripped) = lower.strip_suffix("ms")
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Time(n / 1000.0));
    }
    if let Some(stripped) = lower.strip_suffix('s')
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Time(n));
    }
    if let Some(stripped) = lower.strip_suffix("ct")
        && let Ok(n) = stripped.trim().parse::<f64>()
    {
        return Ok(ast::ParamValue::Cents(n));
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        return Ok(ast::ParamValue::Number(n));
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let body = &trimmed[1..trimmed.len() - 1];
        let values = body
            .split(',')
            .map(|value| value.trim().parse::<f64>())
            .collect::<Result<Vec<_>, _>>();
        if let Ok(values) = values {
            return Ok(ast::ParamValue::Array(values));
        }
    }
    if lower == "true" {
        return Ok(ast::ParamValue::Bool(true));
    }
    if lower == "false" {
        return Ok(ast::ParamValue::Bool(false));
    }
    Ok(ast::ParamValue::String(param_value.to_string()))
}

pub(crate) fn parse_effect_type(s: &str) -> Result<ast::EffectType, String> {
    ast::EffectType::from_name(s).ok_or_else(|| format!("Unknown effect type: {s}"))
}

pub(crate) fn parse_dynamics(s: &str) -> Result<Dynamics, String> {
    match s {
        "ppp" | "PPP" => Ok(Dynamics::PPP),
        "pp" | "PP" => Ok(Dynamics::PP),
        "p" | "P" => Ok(Dynamics::P),
        "mp" | "MP" => Ok(Dynamics::MP),
        "mf" | "MF" => Ok(Dynamics::MF),
        "f" | "F" => Ok(Dynamics::F),
        "ff" | "FF" => Ok(Dynamics::FF),
        "fff" | "FFF" => Ok(Dynamics::FFF),
        _ => Err(format!("Invalid dynamics: {}", s)),
    }
}
