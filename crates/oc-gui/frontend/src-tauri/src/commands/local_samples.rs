//! Local sample commands for the sample panel's first section.
//!
//! Search, preview and add reach the files in [`crate::samples::local`]; the
//! preview plays through the same audition stream as a market preview, and an
//! add copies the file into the project before it places the clip, so the
//! project never points at a file outside its folder.

use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};
use tauri::Manager;

use super::sample_market::{
    DocumentIdentity, SampleCommandError, decode_preview, place_clip, play_preview, preview_reply,
    run_sample,
};
use crate::samples::local::{
    self, LocalQuery, LocalSampleError, LocalSampleRefusal, LocalSearchArgs,
};
use crate::state::{AppState, AppStateInner};

/// Market previews and local previews share one result shape; a local one
/// names this source.
const LOCAL_SOURCE: &str = "local";

impl From<LocalSampleError> for SampleCommandError {
    fn from(error: LocalSampleError) -> Self {
        Self {
            code: error.refusal.map(LocalSampleRefusal::code),
            message: error.message,
        }
    }
}

/// `local_sample_search`: audio files in the app's sound folders whose path
/// holds every query word.
#[tauri::command]
pub async fn local_sample_search(args: LocalSearchArgs) -> Result<Value, SampleCommandError> {
    run_sample(move || {
        let query = LocalQuery::from_args(&args)?;
        let result = local::catalogue().search(&query);
        serde_json::to_value(result).map_err(|error| SampleCommandError::from(error.to_string()))
    })
    .await
}

/// Play one local file through the audition stream.
#[tauri::command]
pub async fn local_sample_preview(
    app: tauri::AppHandle,
    path: String,
    label: Option<String>,
    start_paused: Option<bool>,
) -> Result<Value, SampleCommandError> {
    let started = Instant::now();
    let label = label.unwrap_or_default();
    let generation = {
        let state = app.state::<AppState>();
        let mut inner = state.lock_inner();
        inner
            .audio
            .replace_private_audition(start_paused.unwrap_or(false))?;
        state.audition.begin(&label)
    };
    run_sample(move || {
        let state = app.state::<AppState>();
        let result = local::resolve(&path)
            .map_err(SampleCommandError::from)
            .and_then(|file| preview_file(&state, &file, &path, &label, generation, started));
        if let Err(error) = &result {
            state.audition.fail(generation, error.message.clone());
        }
        result
    })
    .await
}

fn preview_file(
    state: &AppState,
    file: &Path,
    id: &str,
    label: &str,
    generation: u64,
    started: Instant,
) -> Result<Value, SampleCommandError> {
    if !state.audition.is_current(generation) {
        return Ok(preview_reply("cancelled", LOCAL_SOURCE, id, label, 0.0));
    }
    let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
    let samples = decode_preview(file, rate)?;
    let seconds = (samples.len() / 2) as f64 / f64::from(rate);
    let mut inner = state.lock_inner();
    if !play_preview(state, &mut inner, generation, rate, samples)? {
        return Ok(preview_reply("cancelled", LOCAL_SOURCE, id, label, 0.0));
    }
    state.audition.complete(generation, started);
    Ok(preview_reply("ok", LOCAL_SOURCE, id, label, seconds))
}

/// Copy one local file into the project and place an `@audio_clip` at the
/// playhead, on the named track or on a new audio track.
#[tauri::command]
pub async fn local_sample_add(
    app: tauri::AppHandle,
    path: String,
    track: Option<String>,
) -> Result<Value, SampleCommandError> {
    run_sample(move || {
        let source = local::resolve(&path)?;
        let state = app.state::<AppState>();
        let identity = DocumentIdentity::capture(&state);
        let root = identity.project_root().ok_or_else(|| {
            LocalSampleError::refused(
                LocalSampleRefusal::ProjectUnsaved,
                "Save the project first: a sample needs a media folder.",
            )
        })?;
        let (snapshot, track_name, file) =
            add_file(&state, &identity, &source, &root, track.as_deref())?;
        super::arrangement::warm_audio_clip_peaks(&snapshot.project, Some(&root));
        Ok(json!({
            "status": "ok",
            "track": track_name,
            "file": file,
            "snapshot": serde_json::to_value(&snapshot).unwrap_or(Value::Null),
        }))
    })
    .await
}

/// Copy and place; a refused placement removes the copy it made.
fn add_file(
    state: &AppState,
    identity: &DocumentIdentity,
    source: &Path,
    root: &Path,
    track: Option<&str>,
) -> Result<(super::shared::ProjectSnapshot, String, String), SampleCommandError> {
    let copy = local::copy_into_project(source, root)?;
    let seconds = clip_seconds(&copy.path)?;
    let name = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut inner = state.lock_inner();
    let (snapshot, track) = place_copy(&mut inner, identity, &copy.path, &name, seconds, track)?;
    let file = copy.path.display().to_string();
    copy.keep();
    Ok((snapshot, track, file))
}

fn place_copy(
    inner: &mut AppStateInner,
    identity: &DocumentIdentity,
    path: &Path,
    name: &str,
    seconds: f64,
    track: Option<&str>,
) -> Result<(super::shared::ProjectSnapshot, String), String> {
    if DocumentIdentity::of(inner) != *identity {
        return Err("the project changed while the sample was copied; nothing was added".into());
    }
    place_clip(inner, path, name, seconds, track)
}

/// The header's length, or the decoded length when the header states none.
fn clip_seconds(path: &Path) -> Result<f64, String> {
    if let Some(seconds) = oc_engine::audio_clip::probe_seconds(path) {
        return Ok(seconds);
    }
    let decoded = oc_engine::audio_clip::decode(path)?;
    Ok(decoded.frames() as f64 / f64::from(decoded.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_state(dir: &Path) -> AppState {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.file_path = Some(dir.join("song.ocp"));
        drop(inner);
        state
    }

    fn wav(path: &Path, seconds: f64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rate = 8_000;
        let frames = (seconds * f64::from(rate)) as usize;
        std::fs::write(
            path,
            oc_engine::wav::pcm_wav_bytes(&vec![0.1; frames * 2], rate, 2).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn adding_a_local_sample_copies_it_and_places_a_project_relative_clip() {
        let library = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let source = library.path().join("Snare Tight.wav");
        wav(&source, 0.5);
        let state = saved_state(project.path());
        let identity = DocumentIdentity::capture(&state);

        let (snapshot, track, file) =
            add_file(&state, &identity, &source, project.path(), None).unwrap();
        assert_eq!(track, "Samples");
        let clip = snapshot.project.audio_clip(0).unwrap();
        assert_eq!(clip.name, "Snare Tight");
        assert_eq!(clip.file, "media/local-samples/Snare-Tight.wav");
        assert!(Path::new(&file).exists());
        assert!(source.exists(), "the user's file stays where it was");
        assert_eq!(state.lock_inner().undo.undo_entries().len(), 1);
    }

    #[test]
    fn a_refused_placement_leaves_no_copy_and_no_undo_entry() {
        let library = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let source = library.path().join("kick.wav");
        wav(&source, 0.2);
        let state = saved_state(project.path());
        let identity = DocumentIdentity::capture(&state);
        state.lock_inner().revision += 1;

        let error = add_file(&state, &identity, &source, project.path(), None).unwrap_err();
        assert!(
            error.message.contains("the project changed"),
            "{}",
            error.message
        );
        let inner = state.lock_inner();
        assert!(inner.project.audio_clips().is_empty());
        assert!(inner.undo.undo_entries().is_empty());
        let copies = std::fs::read_dir(project.path().join(local::PROJECT_DIR))
            .unwrap()
            .count();
        assert_eq!(copies, 0);
    }

    #[test]
    fn a_local_preview_plays_the_file_and_a_replaced_one_does_not() {
        let library = tempfile::tempdir().unwrap();
        let source = library.path().join("kick.wav");
        wav(&source, 0.5);
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.audio.enable_simulated_output();
            inner.audio.replace_private_audition(false).unwrap();
        }
        let id = source.display().to_string();
        let older = state.audition.begin("Older");
        let current = state.audition.begin("Kick");
        let stale = preview_file(&state, &source, &id, "Older", older, Instant::now()).unwrap();
        assert_eq!(stale["status"], "cancelled");

        let reply = preview_file(&state, &source, &id, "Kick", current, Instant::now()).unwrap();
        assert_eq!(reply["status"], "ok");
        assert_eq!(reply["provider"], LOCAL_SOURCE);
        assert!((reply["seconds"].as_f64().unwrap() - 0.5).abs() < 1e-3);
        assert!(state.lock_inner().audio.audition_active());
    }

    #[test]
    fn a_local_refusal_reaches_the_panel_as_its_code() {
        let error = SampleCommandError::from(LocalSampleError::refused(
            LocalSampleRefusal::OutsideFolders,
            "outside",
        ));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "outside_folders", "message": "outside" })
        );
    }
}
