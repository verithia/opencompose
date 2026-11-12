//! Saving off the document lock: a plain save leaves playback alone, a Save As
//! re-points the resource root, and an edit that races the write stays unsaved.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::support::{TempDir, state_with_project};

use crate::commands::project::{
    needs_repoint, publish_save, save_document, save_file_inner, snapshot_for_save,
    warm_preload_if_current, write_save, write_save_with_library,
};

const SONG: &str = "@project \"Save\"\n  tempo: 120\n";

fn saved_state(label: &str) -> (TempDir, crate::state::AppState) {
    let dir = TempDir::new(label);
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(SONG, dir.path());
    (dir, state)
}

/// A save that changes nothing on disk must not touch the audio graph.
///
/// The observable is the audio state, not a mock: a preload would spawn a
/// render thread and clear the dirty flag, and `mark_dirty` would set it.
#[test]
fn a_plain_save_leaves_playback_alone() {
    let (dir, state) = saved_state("save-in-place");
    let path = dir.path().join("song.ocp");
    // The first save establishes where the document and its resources live.
    let first = save_document(None, &state).unwrap();
    assert_eq!(Path::new(&first.path), path);
    assert_eq!(
        state.lock_inner().audio.resource_root.as_deref(),
        Some(dir.path())
    );

    // The same document, written to the same directory a second time.
    {
        let mut inner = state.lock_inner();
        inner.audio.render_dirty = false;
    }
    let saved = save_document(None, &state).unwrap();
    assert!(!saved.repointed, "the resource directory did not move");
    let inner = state.lock_inner();
    assert!(
        inner.audio.render_thread.is_none(),
        "a plain save must not warm the graph"
    );
    assert!(
        !inner.audio.render_dirty,
        "a plain save must not invalidate the render cache"
    );
    assert_eq!(inner.audio.resource_root.as_deref(), Some(dir.path()));
    assert_eq!(inner.file_path.as_deref(), Some(path.as_path()));
    assert!(!inner.dirty, "the document that was written is clean");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), inner.dsl_source);
}

#[test]
fn save_as_adopts_library_patch_paths_and_keeps_them_on_the_next_save() {
    use oc_synth::synth::{SynthPatch, document::PatchDocument};

    let library = tempfile::tempdir().unwrap();
    let assets = library.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    let patch_name = "11111111111111111111111111111111.json";
    let patch = assets.join(patch_name);
    let sample = oc_engine::wav::mono_wav_bytes(&[0.25; 8_000], 8_000).unwrap();
    std::fs::write(assets.join("voice.wav"), &sample).unwrap();
    let document = PatchDocument {
        version: PatchDocument::PORTABLE_VERSION,
        name: "Voice".into(),
        author: "OpenCompose".into(),
        category: "Test".into(),
        use_policy: "Fixture".into(),
        patch: SynthPatch {
            sampler: Some(oc_synth::sampler::SamplerPatch {
                regions: vec![
                    serde_json::from_value(serde_json::json!({"sample":"voice.wav"})).unwrap(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    std::fs::write(&patch, document.to_json().unwrap()).unwrap();
    let (dir, state) = saved_state("save-as-library");
    let source = format!(
        "@project \"Portable\"\n  tempo: 120 // Keep this comment\n@preset voice\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{open_synth_state_file: \"{}\"}}\n@track lead | preset: voice, instrument: synth/lead_square\n  @phrase note [1:1 - 2:1]\n    C4 q\n",
        patch.display()
    );
    let render = |project: &oc_dsl::ast::Project, root: &Path| {
        let mut cache = oc_engine::render_cache::RenderCache::new();
        cache.set_resource_root(Some(root.to_path_buf()));
        cache
            .render_full(
                project,
                Path::new("unused.sf2"),
                8_000,
                &mut Default::default(),
            )
            .unwrap()
    };
    let reference = {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(&source).unwrap();
        inner.dsl_source = source;
        render(&inner.project, dir.path())
    };

    let moved = dir.path().join("portable");
    std::fs::create_dir(&moved).unwrap();
    let target = moved.join("song.ocp");
    let mut request = snapshot_for_save(Some(target.display().to_string()), &state).unwrap();
    write_save_with_library(&mut request, library.path()).unwrap();
    assert_eq!(publish_save(&state, &request), Some(true));
    drop(library);
    let save_again = save_document(None, &state).unwrap();
    assert_eq!(save_again.path, target.display().to_string());
    let saved = std::fs::read_to_string(&target).unwrap();
    assert!(saved.contains("tempo: 120 // Keep this comment"));
    assert!(saved.contains(&format!("media/patches/library/{patch_name}")));
    assert_eq!(
        std::fs::read(moved.join("media/patches/library/voice.wav")).unwrap(),
        sample
    );
    let reopened = oc_dsl::parse(&saved).unwrap();
    assert_eq!(reference, render(&reopened, &moved));
    assert_eq!(state.lock_inner().dsl_source, saved);
}

/// Save As into another directory moves the resources playback reads.
#[test]
fn save_as_points_playback_at_the_copied_resources() {
    let (dir, state) = saved_state("save-as-root");
    save_document(None, &state).unwrap();
    let moved = dir.path().join("copy");
    std::fs::create_dir_all(&moved).unwrap();
    let target = moved.join("song.ocp");

    let saved = save_document(Some(target.display().to_string()), &state).unwrap();

    assert!(
        saved.repointed,
        "a Save As into another directory re-points playback"
    );
    assert_eq!(Path::new(&saved.path), target);
    assert!(target.is_file(), "the project was written");
    let inner = state.lock_inner();
    assert_eq!(inner.audio.resource_root.as_deref(), Some(moved.as_path()));
    assert_eq!(inner.file_path.as_deref(), Some(target.as_path()));
    assert!(!inner.dirty);
}

#[test]
fn play_before_save_as_warm_up_loads_the_copied_media_once() {
    check_save_as_before_warm_up(None);
}

#[test]
fn mixer_and_effect_edits_during_save_as_warm_up_load_the_copied_media_once() {
    check_save_as_before_warm_up(Some(|inner| {
        inner.project.tracks[0].mix.volume_db = -3.0;
        inner.audio.apply_mixer_change(&inner.project);
    }));
    check_save_as_before_warm_up(Some(|inner| {
        inner.project.tracks[0].effects.push(oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Gain,
            params: vec![("factor".into(), oc_dsl::ast::ParamValue::Number(0.5))],
            bypass: false,
            sidechain_source: None,
        });
        inner.audio.apply_chain_change(&inner.project);
    }));
}

type GraphEdit = fn(&mut crate::state::AppStateInner);

fn check_save_as_before_warm_up(edit: Option<GraphEdit>) {
    let dir = TempDir::new("save-as-play");
    let original = dir.path().join("original");
    let copy = dir.path().join("copy");
    std::fs::create_dir_all(original.join("media/samples")).unwrap();
    std::fs::create_dir_all(&copy).unwrap();
    let media = Path::new("media/samples/hit.wav");
    let mut writer = hound::WavWriter::create(
        original.join(media),
        hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..16000 {
        let sample = (12000.0 * (std::f64::consts::TAU * 0.055 * f64::from(frame)).sin()) as i16;
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
    let state = state_with_project(
        r#"@project "Save As"
  tempo: 120
@audio_clip hit [1:1 - 2:1] | track: tape
  file: "media/samples/hit.wav"
@track tape | instrument: audio/clip
"#,
        &original,
    );
    {
        let mut inner = state.lock_inner();
        inner.audio.enable_simulated_output();
        inner.audio.sample_rate = 8000;
    }
    let saved = save_document(None, &state).unwrap();
    assert!(warm_preload_if_current(&state, saved.revision));
    let playback = {
        let inner = state.lock_inner();
        let worker = inner.audio.render_thread.as_ref().unwrap();
        assert!(worker.wait_ready(Duration::from_secs(3)));
        worker.playback.clone()
    };
    assert_eq!(playback.project_loads.load(Ordering::Acquire), 1);

    let target = copy.join("song.ocp");
    let saved = save_document(Some(target.display().to_string()), &state).unwrap();
    assert!(saved.repointed);
    assert!(copy.join(media).is_file());
    std::fs::rename(&original, dir.path().join("removed-source")).unwrap();

    if let Some(edit) = edit {
        edit(&mut state.lock_inner());
    }
    let pressed = crate::commands::transport::play_inner(&state).unwrap();
    assert_eq!(
        pressed.did_render,
        edit.is_none(),
        "either the intervening edit or Play must queue the copied graph"
    );
    assert!(!warm_preload_if_current(&state, saved.revision));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        {
            let inner = state.lock_inner();
            let worker = inner.audio.render_thread.as_ref().unwrap();
            assert_eq!(worker.load_error(), None);
            if inner.audio.simulated_output_heard_blocks() > 0 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "the copied clip never played");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(playback.project_loads.load(Ordering::Acquire), 2);
}

#[test]
fn needs_repoint_follows_the_directory_only() {
    let here = Path::new("/tmp/project");
    let elsewhere = Path::new("/tmp/project-copy");
    assert!(!needs_repoint(Some(here), Some(here)));
    assert!(!needs_repoint(None, None));
    assert!(needs_repoint(Some(here), Some(elsewhere)));
    assert!(needs_repoint(None, Some(here)));
    assert!(needs_repoint(Some(here), None));
}

/// The dangerous half: a write that races an edit must never mark it clean.
///
/// The edit is landed between the capture and the publication, exactly where a
/// slow Save As leaves the window open. No sleeping and no second thread: the
/// split helpers are called in the order the command calls them.
#[test]
fn an_edit_during_the_write_keeps_the_newer_document_dirty() {
    let (dir, state) = saved_state("save-race");
    let target = dir.path().join("raced.ocp");
    // An untitled document, so a publication would have to adopt the path too.
    {
        let mut inner = state.lock_inner();
        inner.file_path = None;
        inner.dirty = true;
    }

    // The save captures revision R and starts writing.
    let mut request = snapshot_for_save(Some(target.display().to_string()), &state).unwrap();

    // The user edits while the file is being written: revision R + 1.
    {
        let mut inner = state.lock_inner();
        inner.project.properties.tempo = 141.0;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.dirty = true;
        inner.bump_revision();
    }

    // The write finishes with the content it captured; publishing it is refused
    // because the document it describes is no longer the live one.
    write_save(&mut request).unwrap();
    assert!(
        publish_save(&state, &request).is_none(),
        "a save that raced an edit must not publish"
    );

    let inner = state.lock_inner();
    assert!(inner.dirty, "the newer document is still unsaved");
    assert_eq!(inner.project.properties.tempo, 141.0, "the edit survives");
    assert_eq!(
        inner.file_path, None,
        "the newer document keeps its own identity"
    );
    assert_eq!(inner.revision, request.revision + 1);

    // The file on disk is the captured revision — the one the save reports —
    // and the edit is only in memory.
    let written = std::fs::read_to_string(&target).unwrap();
    assert!(!written.contains("141"), "{written}");
    assert!(target.is_file());
}

/// The write must not need the document lock.
///
/// The copy half of a Save As reads and writes up to 256 MiB; a close or quit
/// that holds the lock must not be able to keep that write waiting, and the
/// write must not be able to keep the lock from them. The request owns
/// everything it needs, so a live guard on the main thread changes nothing.
#[test]
fn the_write_runs_without_the_document_lock() {
    let (dir, state) = saved_state("save-no-lock");
    let target = dir.path().join("lock-free.ocp");
    let mut request = snapshot_for_save(Some(target.display().to_string()), &state).unwrap();

    let guard = state.lock_inner();
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = write_save(&mut request);
        let _ = tx.send(result);
    });
    let result = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the write must not wait for the document lock");
    drop(guard);
    result.unwrap();
    writer.join().unwrap();
    assert!(target.is_file(), "the project was written");
}

/// A batch that opens while the save is writing pins the revision, so only the
/// transaction guard stops the publication from marking the batch clean.
#[test]
fn an_agent_batch_opened_during_the_write_keeps_the_document_dirty() {
    let (dir, state) = saved_state("save-batch-race");
    let target = dir.path().join("raced-batch.ocp");
    let mut request = snapshot_for_save(Some(target.display().to_string()), &state).unwrap();

    {
        let mut inner = state.lock_inner();
        inner
            .begin_transaction("turn".into(), "panel:t".into())
            .unwrap();
        inner.project.properties.tempo = 142.0;
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.dirty = true;
        // The batch owns the bump: the revision stays at the captured one.
        inner.bump_revision();
        assert_eq!(inner.revision, request.revision);
    }

    write_save(&mut request).unwrap();
    assert!(
        publish_save(&state, &request).is_none(),
        "a save that raced a batch must not publish"
    );

    let inner = state.lock_inner();
    assert!(inner.dirty, "the batch's document is still unsaved");
    assert_eq!(inner.project.properties.tempo, 142.0, "the edit survives");
    assert!(target.is_file());
}

/// An open agent batch owns the document's uncommitted edits.
#[test]
fn a_save_during_an_agent_batch_is_refused() {
    let (dir, state) = saved_state("save-batch");
    let path = dir.path().join("song.ocp");
    state
        .lock_inner()
        .begin_transaction("turn".into(), "panel:t".into())
        .unwrap();

    let refused = save_file_inner(Some(path.display().to_string()), &state).unwrap_err();
    assert!(refused.contains("transaction"), "{refused}");
    assert!(!path.exists(), "a refused save writes nothing");

    state.lock_inner().rollback_transaction().unwrap();
    assert!(
        save_file_inner(None, &state).is_ok(),
        "the batch no longer blocks"
    );
}
