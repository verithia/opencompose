//! Tauri commands for MIDI import/export, plus the progress/cancel registry.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use oc_dsl::ast::Project;
use oc_engine::midi::MidiError as CoreMidiError;

use crate::commands::{self, HandoffVersion, ProjectSnapshot};
use crate::midi_bridge::{
    self, ExportOptions, ExportReport, ImportOptions, ImportResult, ImportStage, ImportSummary,
};
use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

#[derive(Debug, Clone, Serialize)]
/// Progress of one MIDI import job.
pub struct MidiImportProgress {
    pub stage: ImportStage,
    pub fraction: f64,
}

/// Typed apply failure so the dialog can tell cancellation from an error.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MidiImportFailure {
    Cancelled,
    Failed { message: String },
}

impl MidiImportFailure {
    fn from_core(error: CoreMidiError) -> Self {
        match error {
            CoreMidiError::Cancelled => MidiImportFailure::Cancelled,
            other => MidiImportFailure::Failed {
                message: other.to_string(),
            },
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        MidiImportFailure::Failed {
            message: message.into(),
        }
    }
}

impl fmt::Display for MidiImportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiImportFailure::Cancelled => formatter.write_str("MIDI import cancelled"),
            MidiImportFailure::Failed { message } => formatter.write_str(message),
        }
    }
}

struct Job {
    cancel: Arc<AtomicBool>,
    progress: MidiImportProgress,
}

/// Registered imports keyed by the frontend job id.
#[derive(Default)]
pub struct MidiJobs {
    jobs: Mutex<HashMap<String, Job>>,
}

impl MidiJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn begin(&self, id: &str) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.lock().insert(
            id.to_string(),
            Job {
                cancel: Arc::clone(&cancel),
                progress: MidiImportProgress {
                    stage: ImportStage::Parse,
                    fraction: 0.0,
                },
            },
        );
        cancel
    }

    fn set(&self, id: &str, stage: ImportStage, fraction: f64) {
        if let Some(job) = self.lock().get_mut(id) {
            job.progress = MidiImportProgress {
                stage,
                fraction: fraction.clamp(0.0, 1.0),
            };
        }
    }

    fn progress(&self, id: &str) -> Option<MidiImportProgress> {
        self.lock().get(id).map(|job| job.progress.clone())
    }

    fn cancel(&self, id: &str) -> bool {
        match self.lock().get(id) {
            Some(job) => {
                job.cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    fn finish(&self, id: &str) {
        self.lock().remove(id);
    }
}

#[derive(Debug, Serialize)]
/// Result of applying a MIDI import.
pub struct MidiImportOutcome {
    pub summary: ImportSummary,
    pub snapshot: ProjectSnapshot,
}

fn project_base(app: &AppHandle) -> Project {
    app.state::<AppState>().lock_inner().project.clone()
}

/// The document an import plans against, read together with its revision.
fn import_base(app: &AppHandle) -> (Project, u64) {
    let state = app.state::<AppState>();
    let inner = state.lock_inner();
    (inner.project.clone(), inner.revision)
}

fn import_blocking(
    path: String,
    options: ImportOptions,
    base: Project,
    job: Option<&(AppHandle, String, Arc<AtomicBool>)>,
) -> Result<ImportResult, MidiImportFailure> {
    if let Some((app, id, _)) = job {
        app.state::<MidiJobs>().set(id, ImportStage::Parse, 0.0);
    }
    let idle = AtomicBool::new(false);
    let cancel = job.map(|(_, _, cancel)| cancel.as_ref()).unwrap_or(&idle);
    let data = midi_bridge::read_smf_file_cancellable(Path::new(&path), cancel)
        .map_err(MidiImportFailure::from_core)?;
    let name = Path::new(&path)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_owned);
    let mut report = |stage: ImportStage, fraction: f64| {
        if let Some((app, id, _)) = job {
            app.state::<MidiJobs>().set(id, stage, fraction);
        }
    };
    let result =
        midi_bridge::import_smf_cancellable(&data, name, &options, &base, cancel, &mut report)
            .map_err(MidiImportFailure::from_core)?;
    if let Some((app, id, _)) = job {
        app.state::<MidiJobs>().set(id, ImportStage::Commit, 1.0);
    }
    Ok(result)
}

/// Commit an imported project through the same undo path as UI edits.
///
/// `base_revision` is the revision the import planned against. A document that
/// moved while the file was parsed makes every mapping stale, so the import is
/// refused before anything is replaced.
pub(crate) fn commit_import(
    app: &AppHandle,
    result: ImportResult,
    base_revision: u64,
) -> Result<ProjectSnapshot, String> {
    let state = app.state::<AppState>();
    let mut inner = state.lock_inner();
    commit_import_inner(&mut inner, result, base_revision)
}

pub(crate) fn commit_import_inner(
    inner: &mut AppStateInner,
    result: ImportResult,
    base_revision: u64,
) -> Result<ProjectSnapshot, String> {
    // Refuse before the document is replaced, so a preview never sees a
    // half-committed import.
    inner.refuse_in_preview()?;
    if inner.revision != base_revision {
        return Err(format!(
            "The project moved from revision {base_revision} to revision {} while the MIDI file \
             was being read, so the import was refused. Nothing was applied.",
            inner.revision
        ));
    }
    let old = std::mem::replace(&mut inner.project, result.project);
    commands::resync_with_undo(inner, old, UndoEdit::new("MIDI import", UndoScope::Project))?;
    Ok(commands::snapshot(inner))
}

#[tauri::command]
/// Preview a MIDI file without mutating the project.
pub async fn midi_import_preview(
    path: String,
    options: ImportOptions,
    app: AppHandle,
) -> Result<ImportSummary, String> {
    let base = project_base(&app);
    let result = tauri::async_runtime::spawn_blocking(move || {
        import_blocking(path, options, base, None).map(|result| result.summary)
    })
    .await
    .map_err(|error| format!("MIDI preview worker failed: {error}"))?;
    result.map_err(|error| error.to_string())
}

#[tauri::command]
/// Apply a MIDI import as one undoable edit.
pub async fn midi_import_apply(
    path: String,
    options: ImportOptions,
    job_id: String,
    app: AppHandle,
) -> Result<MidiImportOutcome, MidiImportFailure> {
    let (base, base_revision) = import_base(&app);
    let cancel = app.state::<MidiJobs>().begin(&job_id);
    let cancel_flag = Arc::clone(&cancel);
    let job = (app.clone(), job_id.clone(), cancel);
    let result = match tauri::async_runtime::spawn_blocking(move || {
        import_blocking(path, options, base, Some(&job))
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            app.state::<MidiJobs>().finish(&job_id);
            return Err(MidiImportFailure::failed(format!(
                "MIDI import worker failed: {error}"
            )));
        }
    };
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            app.state::<MidiJobs>().finish(&job_id);
            return Err(error);
        }
    };
    if cancel_flag.load(Ordering::SeqCst) {
        app.state::<MidiJobs>().finish(&job_id);
        return Err(MidiImportFailure::Cancelled);
    }
    let summary = result.summary.clone();
    let snapshot = match commit_import(&app, result, base_revision) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            app.state::<MidiJobs>().finish(&job_id);
            return Err(MidiImportFailure::failed(error));
        }
    };
    app.state::<MidiJobs>().finish(&job_id);
    Ok(MidiImportOutcome { summary, snapshot })
}

#[tauri::command]
/// Read progress for a running import.
pub fn midi_import_progress(
    job_id: String,
    app: AppHandle,
) -> Result<Option<MidiImportProgress>, String> {
    Ok(app.state::<MidiJobs>().progress(&job_id))
}

#[tauri::command]
/// Cancel a running import.
pub fn midi_import_cancel(job_id: String, app: AppHandle) -> Result<bool, String> {
    Ok(app.state::<MidiJobs>().cancel(&job_id))
}

#[tauri::command]
/// Report what a MIDI export would write.
pub async fn midi_export_report(
    options: ExportOptions,
    app: AppHandle,
) -> Result<ExportReport, String> {
    let project = project_base(&app);
    tauri::async_runtime::spawn_blocking(move || {
        midi_bridge::export_smf(&project, &options).map(|result| result.report)
    })
    .await
    .map_err(|error| format!("MIDI export worker failed: {error}"))?
}

#[tauri::command]
/// Write a MIDI export to disk.
pub async fn midi_export_write(
    options: ExportOptions,
    path: String,
    app: AppHandle,
) -> Result<ExportReport, String> {
    let project = project_base(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let result = midi_bridge::export_smf(&project, &options)?;
        std::fs::write(&path, &result.bytes)
            .map_err(|error| format!("Failed to write {path}: {error}"))?;
        Ok(result.report)
    })
    .await
    .map_err(|error| format!("MIDI export worker failed: {error}"))?
}

// ============================================================================
// Agent helpers (MCP / HTTP surface)
// ============================================================================

pub(crate) fn preview_for_agent(
    app: &AppHandle,
    path: &str,
    options: ImportOptions,
) -> Result<ImportSummary, String> {
    let base = project_base(app);
    import_blocking(path.to_string(), options, base, None)
        .map(|result| result.summary)
        .map_err(|error| error.to_string())
}

pub(crate) fn apply_for_agent(
    app: &AppHandle,
    path: &str,
    options: ImportOptions,
) -> Result<(ImportSummary, ProjectSnapshot), String> {
    let (base, base_revision) = import_base(app);
    let result = import_blocking(path.to_string(), options, base, None)
        .map_err(|error| error.to_string())?;
    let summary = result.summary.clone();
    let snapshot = commit_import(app, result, base_revision)?;
    Ok((summary, snapshot))
}

pub(crate) fn export_for_agent(
    app: &AppHandle,
    path: &str,
    options: ExportOptions,
) -> Result<ExportReport, String> {
    let project = project_base(app);
    let result = midi_bridge::export_smf(&project, &options)?;
    std::fs::write(path, &result.bytes)
        .map_err(|error| format!("Failed to write {path}: {error}"))?;
    Ok(result.report)
}

/// Import one MIDI file explicitly staged by the operator. The raw key gates
/// and CC64 automation remain separate so the receiving instrument can play both.
pub(crate) fn import_handoff_for_agent(
    state: &AppState,
    path: &Path,
    expected_revision: u64,
    caller: &str,
) -> Result<(ImportSummary, u64), String> {
    let caller = Some(caller);
    let (version, base) = {
        let inner = state.lock_inner();
        let version = HandoffVersion::capture(&inner, expected_revision, caller)?;
        (version, inner.project.clone())
    };
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("mid") || extension.eq_ignore_ascii_case("midi")
        })
    {
        return Err("Select a staged .mid or .midi file in handoff/.".into());
    }
    let root = version.project_root()?;
    let staged = commands::staged_path(&root, path)?;
    let bytes = commands::read_handoff_midi(&staged)?;
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let options =
        midi_bridge::handoff_import_options(base.tracks.is_empty() && base.sections.is_empty());
    let mut result = midi_bridge::import_smf_cancellable(
        &bytes,
        name,
        &options,
        &base,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .map_err(|error| error.to_string())?;
    if options.import_clock {
        midi_bridge::preserve_non_midi_properties(&mut result.project, &base);
    }
    oc_dsl::validate(&result.project).map_err(|errors| commands::validation_text(&errors))?;
    let summary = result.summary.clone();

    let mut inner = state.lock_inner();
    version.verify(&inner, caller)?;
    if version.project_root()? != root {
        return Err("The project directory moved during the MIDI handoff.".into());
    }
    let revision = inner.revision;
    let snapshot = commit_import_inner(&mut inner, result, revision)?;
    Ok((summary, snapshot.revision))
}

#[derive(Serialize)]
pub(crate) struct MidiHandoffExport {
    path: String,
    revision: u64,
    report: ExportReport,
}

/// Encode a committed document snapshot and publish a new file in handoff/.
pub(crate) fn export_handoff_for_agent(
    state: &AppState,
    expected_revision: u64,
    options: ExportOptions,
) -> Result<MidiHandoffExport, String> {
    let require_committed = |inner: &AppStateInner| {
        inner.refuse_in_transaction().map_err(|_| {
            "Finish the active edit transaction before exporting MIDI. In the AI panel, export in a new turn."
                .to_string()
        })
    };
    let (version, project) = {
        let inner = state.lock_inner();
        require_committed(&inner)?;
        let version = HandoffVersion::capture(&inner, expected_revision, None)?;
        (version, inner.project.clone())
    };
    let root = version.project_root()?;
    let result = midi_bridge::export_smf(&project, &options)?;
    let inner = state.lock_inner();
    require_committed(&inner)?;
    version.verify(&inner, None)?;
    if version.project_root()? != root {
        return Err("The project directory moved during the MIDI handoff.".into());
    }
    let filename = write_handoff_midi(&root, &result.bytes)?;
    Ok(MidiHandoffExport {
        path: format!("handoff/{filename}"),
        revision: inner.revision,
        report: result.report,
    })
}

fn write_handoff_midi(root: &Path, bytes: &[u8]) -> Result<String, String> {
    let folder = commands::ensure_handoff_folder(root)?;

    let mut staged = tempfile::Builder::new()
        .prefix(".opencompose-midi-")
        .tempfile_in(&folder)
        .map_err(|error| format!("Cannot stage the MIDI export: {error}"))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("Cannot write the MIDI export: {error}"))?;
    if folder.canonicalize().ok().as_deref() != Some(folder.as_path()) {
        return Err("The project handoff folder moved during export.".into());
    }
    for suffix in 1..=1000 {
        let filename = if suffix == 1 {
            "project.mid".into()
        } else {
            format!("project-{suffix}.mid")
        };
        match staged.persist_noclobber(folder.join(&filename)) {
            Ok(_) => return Ok(filename),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                staged = error.file;
            }
            Err(error) => return Err(format!("Cannot publish the MIDI export: {}", error.error)),
        }
    }
    Err("The handoff folder already holds 1,000 MIDI exports.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_project(path: &Path, source: &str) -> AppState {
        let state = AppState::new();
        let project = oc_dsl::parse(source).unwrap();
        std::fs::write(path, source).unwrap();
        {
            let mut inner = state.lock_inner();
            inner.project = project;
            inner.dsl_source = source.to_owned();
            inner.file_path = Some(path.to_path_buf());
        }
        state
    }

    const SOURCE: &str = r#"@project "Take"
  tempo: 96
  time_signature: 3/4

@track keys | instrument: piano/acoustic_grand
  @phrase verse [1:1 - 2:1]
    C4 8th mf
    @0.25 pedal down
    @1.5 pedal up
"#;

    #[test]
    fn midi_handoff_preserves_raw_gates_and_sustain_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let project_path = temp.path().join("take.ocp");
        let source = saved_project(&project_path, SOURCE);
        let first = export_handoff_for_agent(&source, 0, ExportOptions::default()).unwrap();
        assert_eq!(first.revision, 0);
        assert_eq!(first.path, "handoff/project.mid");
        let original = std::fs::read(temp.path().join(&first.path)).unwrap();
        let second = export_handoff_for_agent(&source, 0, ExportOptions::default()).unwrap();
        assert_eq!(second.path, "handoff/project-2.mid");
        assert_eq!(
            std::fs::read(temp.path().join(&first.path)).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read_dir(temp.path().join("handoff"))
                .unwrap()
                .count(),
            2
        );
        assert_eq!(source.lock_inner().undo.undo.len(), 0);

        let destination = saved_project(
            &temp.path().join("destination.ocp"),
            "@project \"Destination\"\n",
        );
        destination.lock_inner().project.properties.live_resample = Some(3);
        let (summary, revision) =
            import_handoff_for_agent(&destination, Path::new(&first.path), 0, "panel").unwrap();
        assert_eq!(summary.sustain_expanded_notes, 0);
        assert_eq!(revision, 1);
        let inner = destination.lock_inner();
        assert_eq!(inner.project.properties.tempo, 96.0);
        assert_eq!(inner.project.properties.time_signature.numerator, 3);
        assert_eq!(inner.project.properties.live_resample, Some(3));
        assert_eq!(inner.undo.undo.len(), 1);
        let exported = midi_bridge::export_smf(&inner.project, &ExportOptions::default()).unwrap();
        let midi = midly::Smf::parse(&exported.bytes).unwrap();
        let mut tick = 0;
        let actual: Vec<_> = midi.tracks[1]
            .iter()
            .filter_map(|event| {
                use midly::{MidiMessage, TrackEventKind};
                tick += event.delta.as_int();
                match event.kind {
                    TrackEventKind::Midi {
                        message: MidiMessage::NoteOn { key, .. },
                        ..
                    } => Some((tick, "on", key.as_int())),
                    TrackEventKind::Midi {
                        message: MidiMessage::NoteOff { key, .. },
                        ..
                    } => Some((tick, "off", key.as_int())),
                    TrackEventKind::Midi {
                        message: MidiMessage::Controller { controller, value },
                        ..
                    } if controller.as_int() == 64 => Some((tick, "cc64", value.as_int())),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(
            actual,
            [
                (0, "on", 60),
                (120, "cc64", 127),
                (240, "off", 60),
                (720, "cc64", 0)
            ]
        );
    }

    #[test]
    fn midi_handoff_checks_revision_and_transaction_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let source = saved_project(&temp.path().join("take.ocp"), SOURCE);
        assert!(export_handoff_for_agent(&source, 1, ExportOptions::default()).is_err());
        assert!(!temp.path().join("handoff").exists());
        let export = export_handoff_for_agent(&source, 0, ExportOptions::default()).unwrap();

        let base = source
            .begin_transaction("MIDI test".into(), "panel".into())
            .unwrap();
        let staged = Path::new(&export.path);
        assert!(import_handoff_for_agent(&source, staged, base, "other").is_err());
        assert!(export_handoff_for_agent(&source, base, ExportOptions::default()).is_err());
        let (summary, revision) = import_handoff_for_agent(&source, staged, base, "panel").unwrap();
        assert_eq!(summary.sustain_expanded_notes, 0);
        assert_eq!(revision, base);
        assert_eq!(source.lock_inner().project.tracks.len(), 2);
        source.rollback_transaction().unwrap();
        assert_eq!(source.lock_inner().project.tracks.len(), 1);
        assert!(temp.path().join(staged).is_file());
        assert_eq!(
            std::fs::read_dir(temp.path().join("handoff"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn midi_handoff_refuses_paths_outside_the_saved_project_folder() {
        let temp = tempfile::tempdir().unwrap();
        let source = saved_project(&temp.path().join("take.ocp"), SOURCE);
        let export = export_handoff_for_agent(&source, 0, ExportOptions::default()).unwrap();
        let absolute = temp.path().join(&export.path);
        std::fs::copy(&absolute, temp.path().join("outside.mid")).unwrap();
        std::fs::create_dir(temp.path().join("handoff/folder.mid")).unwrap();
        for path in [
            absolute.as_path(),
            Path::new("handoff/../outside.mid"),
            Path::new("handoff/folder.mid"),
        ] {
            assert!(
                import_handoff_for_agent(&source, path, 0, "panel").is_err(),
                "{path:?}"
            );
        }
        #[cfg(unix)]
        {
            let folder = temp.path().join("handoff");
            let outside = temp.path().join("outside.mid");
            std::os::unix::fs::symlink(&outside, folder.join("escape.mid")).unwrap();
            assert!(
                import_handoff_for_agent(&source, Path::new("handoff/escape.mid"), 0, "panel")
                    .is_err()
            );
            let outside_dir = temp.path().join("elsewhere");
            std::fs::rename(&folder, &outside_dir).unwrap();
            std::os::unix::fs::symlink(&outside_dir, &folder).unwrap();
            assert!(export_handoff_for_agent(&source, 0, ExportOptions::default()).is_err());
            assert!(!outside_dir.join("project-2.mid").exists());
        }
        assert_eq!(source.lock_inner().project.tracks.len(), 1);
    }

    #[test]
    fn job_registry_tracks_progress_and_cancel() {
        let jobs = MidiJobs::default();
        let cancel = jobs.begin("job");
        let started = jobs.progress("job").unwrap();
        assert_eq!(started.stage, ImportStage::Parse);
        assert_eq!(started.fraction, 0.0);
        jobs.set("job", ImportStage::Notes, 0.42);
        let progress = jobs.progress("job").unwrap();
        assert_eq!(progress.stage, ImportStage::Notes);
        assert_eq!(progress.fraction, 0.42);
        jobs.set("job", ImportStage::Commit, 2.0);
        assert_eq!(jobs.progress("job").unwrap().fraction, 1.0);
        assert!(jobs.cancel("job"));
        assert!(cancel.load(Ordering::SeqCst));
        jobs.finish("job");
        assert!(jobs.progress("job").is_none());
        assert!(!jobs.cancel("job"));
    }

    #[test]
    fn cancellation_serializes_as_a_typed_outcome() {
        let cancelled = MidiImportFailure::from_core(CoreMidiError::Cancelled);
        assert!(matches!(cancelled, MidiImportFailure::Cancelled));
        assert_eq!(
            serde_json::to_value(&cancelled).unwrap(),
            serde_json::json!({ "kind": "cancelled" })
        );
        let failed = MidiImportFailure::from_core(CoreMidiError::InvalidMapping {
            source_track: 3,
            message: "bad mapping".into(),
        });
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(value["kind"], "failed");
        assert_eq!(
            value["message"],
            "invalid import mapping for source track 3: bad mapping"
        );
        assert_eq!(
            failed.to_string(),
            "invalid import mapping for source track 3: bad mapping"
        );
    }
}
