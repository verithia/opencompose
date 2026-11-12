//! Offline render lock policy: snapshot, staleness and supersession.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::state::AppState;

const PROJECT: &str = r#"@ocp "0.2.0"

@project "offline render test"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 2:1]
    C4 q
"#;

fn render_state() -> Arc<AppState> {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(PROJECT).unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
    }
    Arc::new(state)
}

fn output_dir(tag: &str) -> PathBuf {
    oc_test_support::temp_dir("oc-offline-render", tag)
}

fn run_render(state: Arc<AppState>, output: PathBuf) -> thread::JoinHandle<Result<String, String>> {
    thread::spawn(move || {
        crate::commands::render::render_offline_with(
            &state,
            output.display().to_string(),
            None,
            None,
            None,
            crate::commands::render::render_offline_snapshot,
        )
    })
}

fn paused_render(
    state: Arc<AppState>,
    output: PathBuf,
) -> (
    thread::JoinHandle<Result<String, String>>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        crate::commands::render::render_offline_with(
            &state,
            output.display().to_string(),
            None,
            None,
            None,
            |snapshot| {
                let rendered = crate::commands::render::render_offline_snapshot(snapshot)?;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(rendered)
            },
        )
    });
    (handle, started_rx, release_tx)
}

#[test]
fn another_tool_call_is_served_while_a_render_is_in_flight() {
    let state = render_state();
    let dir = output_dir("inflight");
    let output = dir.join("mix.wav");
    let (render, started, release) = paused_render(Arc::clone(&state), output.clone());
    started
        .recv_timeout(Duration::from_secs(30))
        .expect("render never reached the in-flight point");

    // A real tool handler that needs the project lock must be served now.
    let (served_tx, served_rx) = mpsc::channel();
    let tool_state = Arc::clone(&state);
    let tool = thread::spawn(move || {
        let result = crate::commands::render_range(
            "/tmp/unused-offline-render.wav".into(),
            crate::commands::RangeRequest {
                start_s: Some(0.0),
                end_s: Some(1.0),
                tracks: Some(vec![99]),
                ..Default::default()
            },
            &tool_state,
        );
        served_tx.send(result).unwrap();
    });
    let served = served_rx.recv_timeout(Duration::from_secs(10));

    release.send(()).unwrap();
    tool.join().unwrap();
    let rendered = render.join().unwrap();

    let served = served.expect("a second tool call waited behind the in-flight render");
    assert!(served.unwrap_err().contains("out of range"));
    assert_eq!(rendered.unwrap(), output.display().to_string());
    assert!(output.is_file());
    assert!(state.range_cache.lock().unwrap().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_edit_during_a_render_is_not_cached_for_the_new_state() {
    let state = render_state();
    let dir = output_dir("stale");
    let output = dir.join("mix.wav");
    let (render, started, release) = paused_render(Arc::clone(&state), output.clone());
    started
        .recv_timeout(Duration::from_secs(30))
        .expect("render never reached the in-flight point");

    {
        let mut inner = state.lock_inner();
        inner.project.tracks[0].mix.volume_db = -6.0;
        inner.bump_revision();
    }
    release.send(()).unwrap();
    let result = render.join().unwrap();

    let error = result.unwrap_err();
    assert!(error.contains("project changed during render"), "{error}");
    assert!(!output.exists(), "a stale render must not publish its WAV");
    assert!(
        state.range_cache.lock().unwrap().is_none(),
        "a stale render must not become the cached render"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(leftovers.is_empty(), "the staged WAV was not removed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_superseded_render_does_not_publish_over_the_newer_one() {
    let state = render_state();
    let dir = output_dir("superseded");
    let old_output = dir.join("old.wav");
    let new_output = dir.join("new.wav");
    let (old_render, started, release) = paused_render(Arc::clone(&state), old_output.clone());
    started
        .recv_timeout(Duration::from_secs(30))
        .expect("render never reached the in-flight point");

    let newer = run_render(Arc::clone(&state), new_output.clone())
        .join()
        .unwrap();
    assert_eq!(newer.unwrap(), new_output.display().to_string());

    release.send(()).unwrap();
    let older = old_render.join().unwrap();

    let error = older.unwrap_err();
    assert!(error.contains("superseded"), "{error}");
    assert!(!old_output.exists());
    assert!(new_output.is_file());
    assert!(state.range_cache.lock().unwrap().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// Spawn the export the way an async Tauri command is spawned; `started` and
/// `release` park it mid-render and `done` reports the result.
fn export_off_thread(
    state: Arc<AppState>,
    output: PathBuf,
    started: mpsc::Sender<thread::ThreadId>,
    release: mpsc::Receiver<()>,
    done: mpsc::Sender<Result<String, String>>,
) {
    let output = output.display().to_string();
    tauri::async_runtime::spawn(async move {
        let result = crate::commands::render::render_offline_off_thread(
            &state,
            output,
            None,
            None,
            None,
            move |snapshot| {
                let rendered = crate::commands::render::render_offline_snapshot(snapshot)?;
                started.send(thread::current().id()).unwrap();
                release.recv().unwrap();
                Ok(rendered)
            },
        )
        .await;
        done.send(result).unwrap();
    });
}

#[test]
fn the_export_renders_off_the_command_thread() {
    let state = render_state();
    let dir = output_dir("off-thread");
    let output = dir.join("mix.wav");
    let caller = thread::current().id();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    export_off_thread(
        Arc::clone(&state),
        output.clone(),
        started_tx,
        release_rx,
        done_tx,
    );

    // The command future is awaited by the runtime, so this thread goes on.
    let render_thread = started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("render never reached the in-flight point");
    assert_ne!(
        render_thread, caller,
        "the render ran on the calling thread"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the export reported a result before the render finished"
    );

    // The app lock is free while the render runs: a tool call is served now.
    let served = crate::commands::render_range(
        "/tmp/unused-off-thread.wav".into(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            tracks: Some(vec![99]),
            ..Default::default()
        },
        &state,
    );
    assert!(served.unwrap_err().contains("out of range"));

    release_tx.send(()).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the export never resolved");
    assert_eq!(result.unwrap(), output.display().to_string());
    assert!(output.is_file());
    assert!(state.range_cache.lock().unwrap().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_second_export_supersedes_the_one_in_flight() {
    let state = render_state();
    let dir = output_dir("off-thread-superseded");
    let old_output = dir.join("old.wav");
    let new_output = dir.join("new.wav");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    export_off_thread(
        Arc::clone(&state),
        old_output.clone(),
        started_tx,
        release_rx,
        done_tx,
    );
    started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("render never reached the in-flight point");

    // The second export starts and finishes while the first one is parked.
    let newer = tauri::async_runtime::block_on(crate::commands::render::render_offline_off_thread(
        &state,
        new_output.display().to_string(),
        None,
        None,
        None,
        crate::commands::render::render_offline_snapshot,
    ))
    .unwrap();
    assert_eq!(newer, new_output.display().to_string());
    assert!(new_output.is_file());

    release_tx.send(()).unwrap();
    let older = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the first export never resolved");
    let error = older.unwrap_err();
    assert!(error.contains("superseded"), "{error}");
    assert!(!old_output.exists(), "a superseded export must not publish");
    assert!(new_output.is_file());
    assert!(state.range_cache.lock().unwrap().is_some());
    let mut leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    leftovers.sort();
    assert_eq!(leftovers, [std::ffi::OsString::from("new.wav")]);
    let _ = std::fs::remove_dir_all(dir);
}

/// Spawn the stems export the way the async command is spawned; `started` and
/// `release` park it mid-export and `done` reports the result.
fn export_stems_off_thread(
    state: Arc<AppState>,
    directory: PathBuf,
    started: mpsc::Sender<thread::ThreadId>,
    release: mpsc::Receiver<()>,
    done: mpsc::Sender<Result<serde_json::Value, String>>,
) {
    tauri::async_runtime::spawn(async move {
        let result = crate::commands::render::render_stems_off_thread(
            &state,
            directory.display().to_string(),
            None,
            move |snapshot, staging| {
                let summary = crate::commands::render::render_stems_snapshot(snapshot, staging)?;
                started.send(thread::current().id()).unwrap();
                release.recv().unwrap();
                Ok(summary)
            },
        )
        .await;
        done.send(result).unwrap();
    });
}

fn stems_in(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn the_stems_export_renders_off_the_command_thread() {
    let state = render_state();
    let dir = output_dir("stems-off-thread");
    let caller = thread::current().id();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    export_stems_off_thread(
        Arc::clone(&state),
        dir.clone(),
        started_tx,
        release_rx,
        done_tx,
    );

    let render_thread = started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("stems render never reached the in-flight point");
    assert_ne!(
        render_thread, caller,
        "the stems render ran on the calling thread"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the stems export reported a result before the render finished"
    );

    // The app lock is free while the stems render: a tool call is served now.
    let served = crate::commands::render_range(
        "/tmp/unused-stems-off-thread.wav".into(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            tracks: Some(vec![99]),
            ..Default::default()
        },
        &state,
    );
    assert!(served.unwrap_err().contains("out of range"));

    release_tx.send(()).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the stems export never resolved")
        .unwrap();
    assert_eq!(
        result["directory"],
        serde_json::json!(dir.display().to_string())
    );
    assert_eq!(
        result["manifest"],
        serde_json::json!(dir.join("stems.json").display().to_string())
    );
    assert!(result["stems"].as_u64().unwrap() >= 1);

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("stems.json")).unwrap()).unwrap();
    let mix_file = manifest["mix"]["file"].as_str().unwrap();
    assert!(
        dir.join(mix_file).is_file(),
        "the mix WAV was not published"
    );
    let stems = manifest["stems"].as_array().unwrap();
    assert_eq!(stems.len(), result["stems"].as_u64().unwrap() as usize);
    for stem in stems {
        let file = stem["file"].as_str().unwrap();
        assert!(dir.join(file).is_file(), "stem {file} was not published");
    }
    assert!(
        state.range_cache.lock().unwrap().is_none(),
        "the stems export must not seed the range cache"
    );
    let leftovers = stems_in(&dir);
    assert!(
        leftovers.iter().all(|name| !name.starts_with('.')),
        "the staged set was not cleaned up: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_second_stems_export_supersedes_the_first_and_leaves_no_files() {
    let state = render_state();
    let old_dir = output_dir("stems-superseded-old");
    let new_dir = output_dir("stems-superseded-new");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    export_stems_off_thread(
        Arc::clone(&state),
        old_dir.clone(),
        started_tx,
        release_rx,
        done_tx,
    );
    started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("stems render never reached the in-flight point");

    // The second stems export starts and finishes while the first is parked.
    let newer = tauri::async_runtime::block_on(crate::commands::render::render_stems_off_thread(
        &state,
        new_dir.display().to_string(),
        None,
        crate::commands::render::render_stems_snapshot,
    ))
    .unwrap();
    assert!(newer["stems"].as_u64().unwrap() >= 1);
    assert!(new_dir.join("stems.json").is_file());

    release_tx.send(()).unwrap();
    let older = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the first stems export never resolved");
    let error = older.unwrap_err();
    assert!(error.contains("superseded"), "{error}");
    assert!(
        stems_in(&old_dir).is_empty(),
        "a superseded stems export left files: {:?}",
        stems_in(&old_dir)
    );
    assert!(new_dir.join("stems.json").is_file());
    let _ = std::fs::remove_dir_all(old_dir);
    let _ = std::fs::remove_dir_all(new_dir);
}

#[test]
fn a_failed_stems_export_leaves_no_partial_set() {
    let state = render_state();
    let dir = output_dir("stems-render-failed");
    let result = tauri::async_runtime::block_on(crate::commands::render::render_stems_off_thread(
        &state,
        dir.display().to_string(),
        None,
        |_snapshot, staging| {
            // A real failure can land after part of the set is written.
            std::fs::write(staging.join("partial-mix.wav"), b"partial").unwrap();
            Err("stem render failed mid-way".into())
        },
    ));
    let error = result.unwrap_err();
    assert!(error.contains("mid-way"), "{error}");
    assert!(
        stems_in(&dir).is_empty(),
        "a failed stems export left files: {:?}",
        stems_in(&dir)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_stems_publish_failure_leaves_no_partial_set() {
    let state = render_state();
    let dir = output_dir("stems-publish-failed");
    // The manifest moves last; a directory in its place fails that last move
    // after every stem WAV already moved.
    std::fs::create_dir(dir.join("stems.json")).unwrap();
    let result = tauri::async_runtime::block_on(crate::commands::render::render_stems_off_thread(
        &state,
        dir.display().to_string(),
        None,
        crate::commands::render::render_stems_snapshot,
    ));
    let error = result.unwrap_err();
    assert!(error.contains("failed to publish"), "{error}");
    assert_eq!(
        stems_in(&dir),
        [String::from("stems.json")],
        "a failed publish left a partial stem set"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Audition: a listen-only 16 kHz mono clip of a rendered window
// ---------------------------------------------------------------------------

fn audition(
    state: &Arc<AppState>,
    path: Option<String>,
    request: crate::commands::RangeRequest,
) -> Result<crate::commands::AuditionRangeResult, String> {
    crate::commands::audition_range(
        path,
        request,
        crate::commands::AuditionChannel::Mono,
        crate::commands::AUDITION_SAMPLE_RATE,
        state,
    )
}

fn decoded(bytes: &[u8]) -> (hound::WavSpec, Vec<i16>) {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let spec = reader.spec();
    let samples = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    (spec, samples)
}

#[test]
fn audition_channel_isolates_a_side_and_stereo_keeps_both() {
    let state = render_state();
    let dir = output_dir("audition-channels");
    let request = || crate::commands::RangeRequest {
        start_bar: Some(1),
        end_bar: Some(3),
        ..Default::default()
    };
    let listened = |channel| {
        crate::commands::audition_range(
            Some(dir.join(format!("{channel:?}.wav")).display().to_string()),
            request(),
            channel,
            crate::commands::AUDITION_SAMPLE_RATE,
            &state,
        )
        .unwrap()
    };
    let left = listened(crate::commands::AuditionChannel::Left);
    let right = listened(crate::commands::AuditionChannel::Right);
    let stereo = listened(crate::commands::AuditionChannel::Stereo);
    assert_eq!(left.audio.channels, 1);
    assert_eq!(right.audio.channels, 1);
    assert_eq!(stereo.audio.channels, 2);
    // A stereo clip carries both sides, so it is about twice the payload of a
    // one-channel clip of the same window.
    let mono_len = left.audio.base64.len();
    let stereo_len = stereo.audio.base64.len();
    assert!(
        stereo_len > mono_len * 3 / 2,
        "stereo {stereo_len} bytes against mono {mono_len}"
    );
}

#[test]
fn saved_audition_preview_preserves_archived_bytes_and_rejects_outside_files() {
    use base64::Engine as _;
    let root = oc_audio::private_fs::test_root().join("saved-audition-preview");
    let archive = root.join("audition-media");
    std::fs::create_dir_all(&archive).unwrap();
    let bytes = oc_engine::wav::pcm_wav_bytes(&[0.25, -0.5, 0.125, 0.75], 24_000, 2).unwrap();
    let name = format!("{}.wav", "a".repeat(64));
    let path = archive.join(&name);
    std::fs::write(&path, &bytes).unwrap();
    let preview = crate::commands::read_saved_audition(&root, &path).unwrap();
    assert_eq!(preview.mime, "audio/wav");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(preview.base64)
            .unwrap(),
        bytes,
    );
    let outside = root.join(&name);
    std::fs::write(&outside, &bytes).unwrap();
    assert!(crate::commands::read_saved_audition(&root, &outside).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(crate::commands::read_saved_audition(&root, &archive.join(name)).is_err());
}

#[test]
fn saved_audition_render_grants_only_owned_bounded_archive_files() {
    use crate::ai::sessions::AUDITION_ARCHIVE_BUDGET;
    use crate::commands::saved_audition_render_path;

    let root = oc_audio::private_fs::test_root().join("saved-audition-full-render");
    let archive = root.join("audition-renders");
    let media = root.join("audition-media");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::create_dir_all(&media).unwrap();
    let name = format!("{}.wav", "a".repeat(64));
    let path = archive.join(&name);
    let file = std::fs::File::create(&path).unwrap();
    assert!(saved_audition_render_path(&root, &path).is_err());
    file.set_len(24 * 1024 * 1024).unwrap();
    assert_eq!(
        saved_audition_render_path(&root, &path).unwrap(),
        path.canonicalize().unwrap()
    );
    assert!(crate::commands::read_saved_audition(&root, &path).is_err());
    file.set_len(AUDITION_ARCHIVE_BUDGET + 1).unwrap();
    assert!(saved_audition_render_path(&root, &path).is_err());

    for rejected in [
        root.join(&name),
        media.join(&name),
        archive.join("notes.wav"),
        archive.join(format!("{}.txt", "a".repeat(64))),
    ] {
        std::fs::write(&rejected, [0]).unwrap();
        assert!(saved_audition_render_path(&root, &rejected).is_err());
    }
    #[cfg(unix)]
    {
        let alias = archive.join(format!("{}.wav", "b".repeat(64)));
        std::os::unix::fs::symlink(root.join(name), &alias).unwrap();
        assert!(saved_audition_render_path(&root, &alias).is_err());
    }
}

#[test]
fn audition_clip_source_reads_the_selected_source_seconds_and_channel() {
    use crate::commands::{AuditionChannel, audition_clip_source};
    use base64::Engine as _;
    let dir = oc_audio::private_fs::test_root().join("source-audition");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("raw.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..8000 {
        writer.write_sample(0_i16).unwrap();
        writer
            .write_sample(if frame < 4000 { 0_i16 } else { 4096 })
            .unwrap();
    }
    writer.finalize().unwrap();
    let mut padded = std::fs::read(&path).unwrap();
    padded.splice(12..12, *b"JUNK\x03\0\0\0abc\0");
    let riff_size = (padded.len() as u32 - 8).to_le_bytes();
    padded[4..8].copy_from_slice(&riff_size);
    std::fs::write(&path, &padded).unwrap();

    let right = audition_clip_source(&path, 2, 0.5, 1.0, AuditionChannel::Right, 8000).unwrap();
    assert_eq!(right.clip_index, Some(2));
    assert_eq!(right.start_s, 0.5);
    assert_eq!(right.end_s, 1.0);
    assert_eq!(right.source_duration_s, Some(1.0));
    assert!(right.truncated);
    let wav = base64::engine::general_purpose::STANDARD
        .decode(right.audio.base64)
        .unwrap();
    let (spec, pcm) = decoded(&wav);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_rate, 8000);
    assert_eq!(pcm.len(), 4000);
    assert!(pcm.iter().all(|sample| sample.abs() > 4000));
    let stereo = audition_clip_source(&path, 2, 0.5, 0.5, AuditionChannel::Stereo, 8000).unwrap();
    let wav = base64::engine::general_purpose::STANDARD
        .decode(stereo.audio.base64)
        .unwrap();
    let (spec, pcm) = decoded(&wav);
    assert_eq!(spec.channels, 2);
    assert_eq!(pcm, [0, 4095].repeat(4000));
    let left = audition_clip_source(&path, 2, 0.5, 0.5, AuditionChannel::Left, 8000).unwrap();
    let wav = base64::engine::general_purpose::STANDARD
        .decode(left.audio.base64)
        .unwrap();
    assert!(decoded(&wav).1.iter().all(|sample| sample.abs() <= 1));
    assert!(audition_clip_source(&path, 2, 1.0, 0.5, AuditionChannel::Mono, 8000).is_err());
    assert!(audition_clip_source(&path, 2, 0.0, 31.0, AuditionChannel::Mono, 8000).is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn an_implicit_audition_writes_a_wav_and_grid_to_owned_scratch() {
    let state = render_state();
    let result = audition(
        &state,
        None,
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(0.5),
            ..Default::default()
        },
    )
    .unwrap();
    let path = std::path::Path::new(&result.path);
    assert_eq!(
        path.parent().unwrap().file_name().unwrap(),
        "audition-scratch"
    );
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert!(crate::render_grid::read(path).is_some());
    assert_eq!(result.duration_s, 0.5);
}

#[test]
fn audition_range_returns_a_16k_mono_clip_and_the_full_render() {
    let state = render_state();
    let dir = output_dir("audition");
    let path = dir.join("audition.wav");
    let result = audition(
        &state,
        Some(path.display().to_string()),
        crate::commands::RangeRequest {
            start_bar: Some(1),
            end_bar: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.start_s, 0.0);
    assert_eq!(result.end_s, 4.0);
    assert_eq!(result.tracks, vec![0]);
    assert!(!result.truncated);
    assert_eq!(result.audio.mime, "audio/wav");
    assert_eq!(result.audio.sample_rate, 16_000);
    assert_eq!(result.audio.channels, 1);
    assert!((result.duration_s - 4.0).abs() < 0.01);
    assert_eq!(std::path::Path::new(&result.path), path);
    assert!(
        path.is_file(),
        "the full-rate render must still land at path"
    );

    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.audio.base64)
        .unwrap();
    let (spec, samples) = decoded(&bytes);
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);
    assert!((samples.len() as f64 / 16_000.0 - 4.0).abs() < 0.01);
    let peak = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 0, "the clip is silent");

    // The same window through render_range writes the same full-rate file: the
    // audition path is the renderer, not a second one.
    let plain = dir.join("plain.wav");
    crate::commands::render_range(
        plain.display().to_string(),
        crate::commands::RangeRequest {
            start_bar: Some(1),
            end_bar: Some(3),
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&plain).unwrap()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn listening_pair_remeasures_the_playable_wav_and_flags_a_silent_side() {
    use base64::Engine as _;
    use oc_engine::analysis::{AudioBuffer, integrated_lufs};
    let rate = 16_000;
    let tone = |gain: f64| AudioBuffer {
        sample_rate: rate,
        channels: vec![
            (0..rate)
                .map(|index| {
                    gain * (std::f64::consts::TAU * 440.0 * f64::from(index) / f64::from(rate))
                        .sin()
                })
                .collect(),
        ],
    };
    let pair = crate::commands::render::listening_pair(
        &tone(0.4),
        &tone(0.1),
        crate::commands::AuditionChannel::Mono,
        rate,
    )
    .unwrap();
    let json = serde_json::to_value(pair).unwrap();
    assert_eq!(json["status"], "matched");
    assert!((json["reference_gain_db"].as_f64().unwrap() + 12.0412).abs() < 0.02);
    assert_eq!(json["candidate_gain_db"], 0.0);
    for side in ["reference", "candidate"] {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(json[format!("{side}_audio")]["base64"].as_str().unwrap())
            .unwrap();
        let (spec, samples) = decoded(&bytes);
        assert_eq!(spec.channels, 1);
        assert_eq!(samples.len(), rate as usize);
        let decoded = AudioBuffer {
            sample_rate: spec.sample_rate,
            channels: vec![
                samples
                    .iter()
                    .map(|&value| f64::from(value) / 32768.0)
                    .collect(),
            ],
        };
        assert_eq!(
            integrated_lufs(&decoded),
            json[format!("{side}_lufs")].as_f64()
        );
    }
    let silence = crate::commands::render::listening_pair(
        &tone(0.1),
        &tone(0.0),
        crate::commands::AuditionChannel::Mono,
        rate,
    )
    .unwrap();
    let json = serde_json::to_value(silence).unwrap();
    assert_eq!(json["status"], "unmeasured");
    assert!(json["delta_lu"].is_null());
    assert_eq!(json["reference_gain_db"], 0.0);
}

#[test]
fn audio_file_preview_returns_a_playable_mono_clip_for_a_render() {
    let state = render_state();
    let dir = output_dir("audio-preview");
    let path = dir.join("preview.wav");
    let rendered = audition(
        &state,
        Some(path.display().to_string()),
        crate::commands::RangeRequest {
            start_bar: Some(1),
            end_bar: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    let preview =
        crate::commands::render::audio_file_preview_blocking(&rendered.path, None, None).unwrap();
    assert_eq!(preview.audio.mime, "audio/wav");
    assert_eq!(preview.audio.sample_rate, 16_000);
    assert_eq!(preview.audio.channels, 1);
    assert!((preview.clip_duration_s - 4.0).abs() < 0.01);
    assert!(!preview.truncated);

    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&preview.audio.base64)
        .unwrap();
    let (spec, samples) = decoded(&bytes);
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert!((samples.len() as f64 / 16_000.0 - 4.0).abs() < 0.01);
    let peak = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 0, "the preview is silent");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audio_file_preview_only_includes_the_opening_excerpt() {
    let dir = output_dir("audio-preview-prefix");
    let path = dir.join("long.wav");
    let rate = 8_000;
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..32 * rate {
        writer
            .write_sample(if frame < 30 * rate { 0_i16 } else { 12_000 })
            .unwrap();
    }
    writer.finalize().unwrap();

    let preview =
        crate::commands::render::audio_file_preview_blocking(path.to_str().unwrap(), None, None)
            .unwrap();
    assert_eq!(preview.clip_duration_s, 30.0);
    assert!(preview.truncated);
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(preview.audio.base64)
        .unwrap();
    let (spec, samples) = decoded(&bytes);
    assert_eq!(samples.len(), 30 * spec.sample_rate as usize);
    assert!(samples.iter().all(|&sample| sample == 0));

    let late = crate::commands::render::audio_file_preview_blocking(
        path.to_str().unwrap(),
        Some(31.0),
        Some(2.0),
    )
    .unwrap();
    assert_eq!(
        (late.start_s, late.end_s, late.clip_duration_s),
        (31.0, 32.0, 1.0)
    );
    assert!(late.truncated);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(late.audio.base64)
        .unwrap();
    let (_, samples) = decoded(&bytes);
    assert_eq!(samples.len(), 16_000);
    assert!(samples.iter().all(|&sample| sample > 7_000));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audition_range_truncates_a_long_window_to_thirty_seconds() {
    let state = render_state();
    let dir = output_dir("audition-truncated");
    let result = audition(
        &state,
        None,
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(40.0),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.truncated, "a 40 s window must report truncation");
    assert!(result.duration_s <= crate::commands::AUDITION_MAX_SECONDS);
    assert!(result.duration_s > 29.0, "{}", result.duration_s);
    assert!(result.audio.base64.len() <= crate::commands::AUDITION_MAX_BASE64_BYTES);

    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.audio.base64)
        .unwrap();
    let (spec, samples) = decoded(&bytes);
    assert_eq!(spec.sample_rate, 16_000);
    assert!(samples.len() as f64 / 16_000.0 <= 30.0 + 1e-9);
    assert!(std::path::Path::new(&result.path).is_file());
    let _ = std::fs::remove_file(&result.path);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audition_range_repeats_byte_identically() {
    let state = render_state();
    let dir = output_dir("audition-repeat");
    let path = dir.join("repeat.wav").display().to_string();
    let request = || crate::commands::RangeRequest {
        start_s: Some(0.0),
        end_s: Some(2.0),
        tracks: Some(vec![0]),
        ..Default::default()
    };
    let first = audition(&state, Some(path.clone()), request()).unwrap();
    let rendered = std::fs::read(&path).unwrap();
    let second = audition(&state, Some(path.clone()), request()).unwrap();
    assert_eq!(first.audio.base64, second.audio.base64);
    assert_eq!(rendered, std::fs::read(&path).unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn audition_range_refuses_the_same_windows_as_render_range() {
    let state = render_state();
    let dir = output_dir("audition-refused");
    let plain = dir.join("plain.wav").display().to_string();
    for request in [
        // both pairs
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            start_bar: Some(1),
            end_bar: Some(2),
            ..Default::default()
        },
        // neither pair
        crate::commands::RangeRequest::default(),
    ] {
        let audition_error = audition(&state, None, request.clone()).unwrap_err();
        let render_error =
            crate::commands::render_range(plain.clone(), request, &state).unwrap_err();
        assert_eq!(audition_error, render_error);
        assert!(
            audition_error.contains("provide start/end seconds or start_bar/end_bar, not both"),
            "{audition_error}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_range_keeps_its_shape_and_gains_no_audio() {
    let state = render_state();
    let dir = output_dir("render-unchanged");
    let path = dir.join("plain.wav");
    let rendered = crate::commands::render_range(
        path.display().to_string(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    let value = serde_json::to_value(&rendered).unwrap();
    assert!(value.get("audio").is_none(), "{value}");
    assert_eq!(value["sample_rate"], 44_100);
    assert_eq!(value["duration_s"], 1.0);
    assert_eq!(value["tracks"], serde_json::json!([0]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_range_reports_actual_wav_stems_with_a_flac_master() {
    let state = render_state();
    state.lock_inner().project.properties.export = Some(oc_dsl::ast::ExportSettings {
        container: oc_dsl::ast::ExportContainer::Flac,
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.flac");
    let rendered = crate::commands::render_range(
        path.display().to_string(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            stems: true,
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    assert!(std::fs::read(path).unwrap().starts_with(b"fLaC"));
    let [stem] = rendered.stems.as_slice() else {
        panic!("expected one stem");
    };
    assert!(stem.path.ends_with(".wav"));
    let wav = hound::WavReader::open(&stem.path).unwrap();
    assert_eq!(stem.frames, 44_100);
    assert_eq!(stem.frames, wav.duration() as usize);
    assert_eq!(stem.sample_rate, wav.spec().sample_rate);
    assert_eq!(stem.channels, wav.spec().channels);
}

#[test]
fn range_stem_loop_handoff_preserves_leading_silence_and_the_seam() {
    use std::fmt::Write as _;

    const SAMPLE_RATE: u32 = 48_000;
    const LOOP_FRAMES: usize = 384_000;
    const REST_FRAMES: usize = 96_000;
    let dir = tempfile::tempdir().unwrap();
    let header = "@ocp \"0.2.0\"\n@project \"Loop handoff\"\n  tempo: 120\n  time_signature: 4/4\n";
    let mut source = header.to_owned();
    for (name, hz) in [("pulse", 440), ("sustain", 220), ("delayed", 660)] {
        writeln!(
            source,
            "@preset {name}\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: {hz}, amplitude: 0.08}}\n@track {name} | preset: {name}, instrument: synth/lead_square"
        )
        .unwrap();
        if name == "pulse" {
            source.push_str("  mix: pan=-0.4\n");
        }
        for (cycle, bar) in [1, 5, 9].into_iter().enumerate() {
            let (start, end, end_beat, duration) = match name {
                "pulse" => (bar, bar + 1, 1, "q"),
                "sustain" => (bar + 3, bar + 4, 2, "w~q"),
                _ => (bar + 1, bar + 2, 1, "q"),
            };
            writeln!(
                source,
                "  @phrase cycle{cycle} [{start}:1 - {end}:{end_beat}]\n    C4 {duration}"
            )
            .unwrap();
        }
    }

    let make_state = |dsl: &str| {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = oc_dsl::parse(dsl).unwrap();
            inner.file_path = Some(dir.path().join("project.ocp"));
            inner.audio.sample_rate = SAMPLE_RATE;
        }
        state
    };
    let state = make_state(&source);
    let render = |state: &AppState, filename: &str, start, end, stems| {
        crate::commands::render_range(
            dir.path().join(filename).display().to_string(),
            crate::commands::RangeRequest {
                start_s: Some(start),
                end_s: Some(end),
                float_wav: true,
                stems,
                ..Default::default()
            },
            state,
        )
        .unwrap()
    };

    let export = render(&state, "loop.wav", 8.0, 16.0, true);
    assert_eq!(export.frames, LOOP_FRAMES);
    assert_eq!(export.stems.len(), 3);
    let mut receiving = header.to_owned();
    for stem in &export.stems {
        assert_eq!(
            (stem.frames, stem.sample_rate, stem.channels),
            (LOOP_FRAMES, SAMPLE_RATE, 2)
        );
        let mut wav = hound::WavReader::open(&stem.path).unwrap();
        assert_eq!(wav.duration() as usize, LOOP_FRAMES);
        if stem.name == "delayed" {
            let mut samples = wav.samples::<f32>().map(Result::unwrap);
            assert!(samples.by_ref().take(REST_FRAMES * 2).all(|x| x == 0.0));
            assert!(samples.take(480 * 2).any(|x| x.abs() > 0.01));
        }
        let filename = std::path::Path::new(&stem.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        writeln!(
            receiving,
            "@track {} | instrument: audio/clip\n  mix: pan_law=balance",
            stem.name
        )
        .unwrap();
        for (cycle, bar) in [1, 5, 9].into_iter().enumerate() {
            writeln!(
                receiving,
                "@audio_clip {}{cycle} [{bar}:1 - {}:1] | track: {}\n  file: \"{filename}\"",
                stem.name,
                bar + 4,
                stem.name
            )
            .unwrap();
        }
    }

    let receiving = make_state(&receiving);
    let reference = render(&state, "reference.wav", 15.75, 16.25, false);
    let repeated = render(&receiving, "repeated.wav", 15.75, 16.25, false);
    let read = |path: &str| {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    };
    let reference = read(&reference.path);
    let repeated = read(&repeated.path);
    assert_eq!(reference.len(), 24_000 * 2);
    assert_eq!(reference.len(), repeated.len());
    assert!(reference[..24_000].iter().any(|x| x.abs() > 0.01));
    assert!(reference[24_000..].iter().any(|x| x.abs() > 0.01));
    let (worst, max_error) = reference
        .iter()
        .zip(&repeated)
        .enumerate()
        .map(|(index, (a, b))| (index, (a - b).abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!(
        max_error < 1e-5,
        "loop seam error {max_error} at sample {worst}: expected {}, got {}",
        reference[worst],
        repeated[worst],
    );
}

/// A track whose preset is neither built-in nor a loaded plugin.
const SOUNDFONT_PROJECT: &str = r#"@ocp "0.2.0"

@project "soundfont refusal"
  tempo: 120
  time_signature: 4/4

@preset grand
  source: soundfont
  program: 0

@track keys | preset: grand, instrument: piano/acoustic_grand
  @phrase body [1:1 - 2:1]
    C4 q
"#;

#[test]
fn a_missing_output_directory_refuses_with_the_render_skill() {
    let state = render_state();
    let missing = std::env::temp_dir().join(format!("oc-refusal-{}", std::process::id()));
    let error = crate::commands::render_range(
        missing.join("range.wav").display().to_string(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        &state,
    )
    .unwrap_err();
    // The text this refusal has always carried, whole and unchanged.
    assert_eq!(
        error,
        format!("output directory '{}' does not exist", missing.display())
    );
    let payload = crate::ai::skill_hints::refusal(&error);
    assert_eq!(payload["error"], error);
    assert_eq!(payload["skill"], "fix-a-render-error");
}

#[test]
fn a_track_that_needs_a_soundfont_refuses_with_the_playable_skill() {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(SOUNDFONT_PROJECT).unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        // The refusal is about *no* playable source; a machine with a default
        // SoundFont installed would otherwise render and the test would pass or
        // fail depending on the host.
        inner.audio.soundfont_path = None;
    }
    let dir = output_dir("soundfont-refusal");
    let error = crate::commands::render_range(
        dir.join("range.wav").display().to_string(),
        crate::commands::RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        &state,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "Track 'keys' needs a SoundFont: declare preset.file in the project or supply a legacy \
         --soundfont fallback"
    );
    let payload = crate::ai::skill_hints::refusal(&error);
    assert_eq!(payload["error"], error);
    assert_eq!(payload["skill"], "playable-instruments");
    let _ = std::fs::remove_dir_all(dir);
}

fn export_block(depth: oc_dsl::ast::SampleFormat, dither: bool) -> oc_dsl::ast::ExportSettings {
    oc_dsl::ast::ExportSettings {
        container: oc_dsl::ast::ExportContainer::Wav,
        wav: oc_dsl::ast::WavSettings { depth, dither },
        flac: oc_dsl::ast::FlacSettings::default(),
        mp3: oc_dsl::ast::Mp3Settings::default(),
        sample_rate: None,
        resample: None,
    }
}

#[test]
fn the_dialog_choices_rank_above_the_project_and_float_dither_is_refused() {
    use crate::commands::render::{ExportOverrides, resolve_export_options};
    use oc_dsl::ast::SampleFormat;

    let mut project = oc_dsl::parse(PROJECT).unwrap();
    let defaults = resolve_export_options(&project, 48_000, &ExportOverrides::default()).unwrap();
    assert_eq!(defaults.container, oc_dsl::ast::ExportContainer::Wav);
    assert_eq!(defaults.wav.depth, SampleFormat::I16);
    assert!(!defaults.wav.dither);
    assert_eq!(defaults.sample_rate, 48_000);

    project.properties.export = Some(export_block(SampleFormat::I24, true));
    let project_choice =
        resolve_export_options(&project, 44_100, &ExportOverrides::default()).unwrap();
    assert_eq!(project_choice.wav.depth, SampleFormat::I24);
    assert!(project_choice.wav.dither);
    assert_eq!(project_choice.sample_rate, 44_100);

    let overridden = ExportOverrides {
        depth: Some(SampleFormat::F32),
        dither: Some(false),
        sample_rate: Some(96_000),
        ..Default::default()
    };
    let overridden = resolve_export_options(&project, 44_100, &overridden).unwrap();
    assert_eq!(overridden.wav.depth, SampleFormat::F32);
    assert!(!overridden.wav.dither);
    assert_eq!(overridden.sample_rate, 96_000, "the dialog's rate wins");

    let refused = ExportOverrides {
        depth: Some(SampleFormat::F32),
        dither: Some(true),
        ..Default::default()
    };
    let refused = resolve_export_options(&project, 44_100, &refused).unwrap_err();
    assert!(refused.contains("meaningless for f32"), "{refused}");

    let bad_rate = ExportOverrides {
        sample_rate: Some(1_000),
        ..Default::default()
    };
    let bad_rate = resolve_export_options(&project, 44_100, &bad_rate).unwrap_err();
    assert!(bad_rate.contains("8000-192000"), "{bad_rate}");
}

/// The dialog sends every field; a caller may name just one and keep the rest.
#[test]
fn an_export_request_that_names_one_option_keeps_the_rest() {
    use crate::commands::render::RenderAudioRequest;
    use oc_dsl::ast::SampleFormat;

    let request: RenderAudioRequest = serde_json::from_str(
        r#"{"scope":"mix","output_path":"/tmp/a.wav","options":{"depth":"i24"}}"#,
    )
    .expect("a partial override deserialises");
    assert_eq!(request.options.depth, Some(SampleFormat::I24));
    assert_eq!(request.options.sample_rate, None);
    assert_eq!(request.options.dither, None);
    assert!(request.options.normalise.is_none());
    assert!(request.pre_roll.is_none());

    let stems: RenderAudioRequest = serde_json::from_str(
        r#"{"scope":"stems","directory":"/tmp/stems","stem_name":"take 2","options":{}}"#,
    )
    .expect("a stems request deserialises");
    assert!(stems.output_path.is_none());
    assert_eq!(stems.directory.as_deref(), Some("/tmp/stems"));
}

/// The dialog's rate, resampling step and normalisation apply to the snapshot.
#[test]
fn the_dialog_resample_and_normalisation_apply_before_the_render() {
    use crate::commands::render::{ExportOverrides, capture_offline_snapshot};

    let state = render_state();
    {
        let mut inner = state.lock_inner();
        inner.project.master = Some(oc_dsl::ast::Master {
            effects: Vec::new(),
            gain_automation: Vec::new(),
            automation: Vec::new(),
            loudness_target: Some(-14.0),
            volume_db: 0.0,
            input_routes: Vec::new(),
        });
    }
    let overrides = ExportOverrides {
        resample: Some(3),
        normalise: Some(false),
        sample_rate: Some(96_000),
        ..Default::default()
    };
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    assert_eq!(snapshot.sample_rate, 96_000);
    assert_eq!(
        snapshot
            .project
            .properties
            .export
            .and_then(|export| export.resample),
        Some(3)
    );
    assert_eq!(
        snapshot
            .project
            .master
            .and_then(|master| master.loudness_target),
        None,
        "normalisation off clears the master target the renderer reads"
    );

    // The default keeps the target: normalisation is opt-out, not opt-in.
    let kept = capture_offline_snapshot(&state, None, &ExportOverrides::default()).unwrap();
    assert_eq!(
        kept.project
            .master
            .and_then(|master| master.loudness_target),
        Some(-14.0)
    );
}

/// The dialog's format, depth, dither and rate reach the published file.
#[test]
fn the_dialog_options_reach_the_written_file() {
    use crate::commands::render::{ExportOverrides, ExportScope, RenderAudioRequest};

    let state = render_state();
    let dir = output_dir("dialog-options");
    let output = dir.join("mix.wav");
    let request = RenderAudioRequest {
        scope: ExportScope::Mix,
        output_path: Some(output.display().to_string()),
        directory: None,
        stem_name: None,
        pre_roll: None,
        options: ExportOverrides {
            container: Some(oc_dsl::ast::ExportContainer::Wav),
            depth: Some(oc_dsl::ast::SampleFormat::I24),
            dither: Some(true),
            flac_level: None,
            mp3_bitrate: None,
            mp3_vbr: None,
            sample_rate: Some(48_000),
            resample: Some(2),
            normalise: Some(false),
        },
    };
    let result = tauri::async_runtime::block_on(crate::commands::render::render_mix_off_thread(
        &state, request, None,
    ))
    .expect("the export publishes");

    assert_eq!(result.scope, ExportScope::Mix);
    assert_eq!(result.path, output.display().to_string());
    assert_eq!(result.sample_rate, 48_000);
    assert_eq!(result.depth, oc_dsl::ast::SampleFormat::I24);
    assert!(result.dither);
    // The fixture is one bar at 120 BPM: two seconds plus the tone's tail.
    assert!(
        result.frames >= 96_000,
        "the render is shorter than the arrangement: {} frames",
        result.frames
    );
    assert_eq!(result.bytes, std::fs::metadata(&output).unwrap().len());
    assert!(!result.clipped);
    assert!(result.peak_dbfs < 0.0, "{result:?}");

    let reader = hound::WavReader::open(&output).unwrap();
    assert_eq!(reader.spec().sample_rate, 48_000);
    assert_eq!(reader.spec().bits_per_sample, 24);
    let _ = std::fs::remove_dir_all(dir);
}

/// The capability query publishes the encoder's own MP3 rate table.
#[test]
fn export_capabilities_publish_the_encoder_rate_table() {
    let capabilities = crate::commands::render::export_capabilities();
    assert_eq!(
        capabilities.mp3_sample_rates,
        oc_engine::encode::mp3::MP3_SAMPLE_RATES
    );
    assert_eq!(capabilities.mp3_sample_rates.len(), 7);
}

/// A hot mix through a float export reports the overload, not a clamp: the
/// same mix through an integer depth reports both.
#[test]
fn a_float_export_above_full_scale_is_not_reported_as_clipped() {
    use crate::commands::render::{ExportOverrides, ExportScope, RenderAudioRequest};
    use oc_dsl::ast::{ExportContainer, Master, SampleFormat};

    let state = render_state();
    {
        let mut inner = state.lock_inner();
        inner.project.master = Some(Master {
            effects: Vec::new(),
            gain_automation: Vec::new(),
            automation: Vec::new(),
            loudness_target: None,
            volume_db: 18.0,
            input_routes: Vec::new(),
        });
    }
    let dir = output_dir("float-hot");

    let render = |depth: SampleFormat, name: &str| {
        let output = dir.join(name);
        let request = RenderAudioRequest {
            scope: ExportScope::Mix,
            output_path: Some(output.display().to_string()),
            directory: None,
            stem_name: None,
            pre_roll: None,
            options: ExportOverrides {
                container: Some(ExportContainer::Wav),
                depth: Some(depth),
                dither: Some(false),
                flac_level: None,
                mp3_bitrate: None,
                mp3_vbr: None,
                sample_rate: Some(48_000),
                resample: Some(2),
                normalise: None,
            },
        };
        let result = tauri::async_runtime::block_on(
            crate::commands::render::render_mix_off_thread(&state, request, None),
        )
        .expect("the export publishes");
        (output, result)
    };

    let (float_output, float) = render(SampleFormat::F32, "mix-f32.wav");
    assert!(float.above_full_scale, "{float:?}");
    assert!(!float.clipped, "a float file clamps nothing: {float:?}");
    assert!(float.peak_dbfs > 0.0, "{float:?}");
    let reader = hound::WavReader::open(&float_output).unwrap();
    let peak = reader
        .into_samples::<f32>()
        .map(|sample| sample.unwrap().abs())
        .fold(0.0_f32, f32::max);
    assert!(peak > 1.0, "the float file keeps the overload, peak {peak}");

    let (_, integer) = render(SampleFormat::I16, "mix-i16.wav");
    assert!(integer.above_full_scale, "{integer:?}");
    assert!(integer.clipped, "the integer writer clamps: {integer:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A FLAC export writes FLAC bytes at the chosen depth and reports it.
#[test]
fn a_flac_export_writes_a_lossless_file_at_the_chosen_depth() {
    use crate::commands::render::{ExportOverrides, ExportScope, RenderAudioRequest};
    use oc_dsl::ast::{ExportContainer, SampleFormat};

    let state = render_state();
    let dir = output_dir("flac-options");
    let output = dir.join("mix.flac");
    let request = RenderAudioRequest {
        scope: ExportScope::Mix,
        output_path: Some(output.display().to_string()),
        directory: None,
        stem_name: None,
        pre_roll: None,
        options: ExportOverrides {
            container: Some(ExportContainer::Flac),
            depth: Some(SampleFormat::I24),
            dither: None,
            flac_level: Some(8),
            mp3_bitrate: None,
            mp3_vbr: None,
            sample_rate: Some(48_000),
            resample: Some(2),
            normalise: Some(false),
        },
    };
    let result = tauri::async_runtime::block_on(crate::commands::render::render_mix_off_thread(
        &state, request, None,
    ))
    .expect("the FLAC export publishes");

    assert_eq!(result.container, ExportContainer::Flac);
    assert_eq!(result.depth, SampleFormat::I24);
    assert!(!result.dither, "FLAC never dithers");
    assert_eq!(result.bytes, std::fs::metadata(&output).unwrap().len());
    let header = std::fs::read(&output).unwrap();
    assert_eq!(&header[..4], b"fLaC", "the file is a native FLAC stream");

    let decoded = oc_engine::audio_clip::decode(&output).expect("the FLAC decodes");
    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.frames(), result.frames);
    let _ = std::fs::remove_dir_all(dir);
}

/// An MP3 export writes layer III bytes at the chosen bitrate and reports it.
#[test]
fn an_mp3_export_writes_an_mp3_file_at_the_chosen_bitrate() {
    use crate::commands::render::{ExportOverrides, ExportScope, RenderAudioRequest};
    use oc_dsl::ast::{ExportContainer, SampleFormat};

    let state = render_state();
    let dir = output_dir("mp3-options");
    let output = dir.join("mix.mp3");
    let request = RenderAudioRequest {
        scope: ExportScope::Mix,
        output_path: Some(output.display().to_string()),
        directory: None,
        stem_name: None,
        pre_roll: None,
        options: ExportOverrides {
            container: Some(ExportContainer::Mp3),
            depth: None,
            dither: None,
            flac_level: None,
            mp3_bitrate: Some(192),
            mp3_vbr: None,
            sample_rate: Some(48_000),
            resample: Some(2),
            normalise: Some(false),
        },
    };
    let result = tauri::async_runtime::block_on(crate::commands::render::render_mix_off_thread(
        &state, request, None,
    ))
    .expect("the MP3 export publishes");

    assert_eq!(result.container, ExportContainer::Mp3);
    // Layer III takes 16-bit PCM and has no dither step.
    assert_eq!(result.depth, SampleFormat::I16);
    assert!(!result.dither);
    assert_eq!(result.bytes, std::fs::metadata(&output).unwrap().len());
    let bytes = std::fs::read(&output).unwrap();
    assert_eq!(bytes[0], 0xFF, "the file starts with a frame sync");
    assert_eq!(bytes[1] & 0xE0, 0xE0);
    // 192 kbps at 48 kHz is a 576-byte frame, and the file is whole frames.
    assert_eq!(bytes.len() % 576, 0, "{} bytes", bytes.len());

    let decoded = oc_engine::audio_clip::decode(&output).expect("the MP3 decodes");
    assert_eq!(decoded.sample_rate, 48_000);
    assert!(
        decoded.frames().abs_diff(result.frames) <= 1_152,
        "decoded {} frames against {} rendered",
        decoded.frames(),
        result.frames
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The MP3 options belong to MP3: another container refuses them, and MP3
/// refuses the WAV-only ones.
#[test]
fn mp3_options_are_refused_for_another_container_and_the_reverse() {
    use crate::commands::render::{ExportOverrides, resolve_export_options};
    use oc_dsl::ast::{ExportContainer, SampleFormat};

    let project = render_state().lock_inner().project.clone();
    let mp3_flags = ExportOverrides {
        container: Some(ExportContainer::Mp3),
        mp3_bitrate: Some(256),
        ..Default::default()
    };
    let spec = resolve_export_options(&project, 48_000, &mp3_flags).expect("the MP3 spec resolves");
    assert_eq!(spec.container, ExportContainer::Mp3);
    assert_eq!(spec.mp3.bitrate, 256);

    let wrong_container = ExportOverrides {
        container: Some(ExportContainer::Wav),
        mp3_bitrate: Some(256),
        ..Default::default()
    };
    let error = resolve_export_options(&project, 48_000, &wrong_container)
        .expect_err("the MP3 bitrate is an MP3 option");
    assert!(error.contains("only apply to an MP3 export"), "{error}");

    let depth = ExportOverrides {
        container: Some(ExportContainer::Mp3),
        depth: Some(SampleFormat::I24),
        ..Default::default()
    };
    let error =
        resolve_export_options(&project, 48_000, &depth).expect_err("MP3 has no sample depth");
    assert!(error.contains("no sample depth"), "{error}");

    let dither = ExportOverrides {
        container: Some(ExportContainer::Mp3),
        dither: Some(true),
        ..Default::default()
    };
    let error =
        resolve_export_options(&project, 48_000, &dither).expect_err("MP3 has no dither step");
    assert!(error.contains("no dither step"), "{error}");

    // The rate table is the encoder's: 96 kHz has no MPEG Layer III form.
    let rate = ExportOverrides {
        container: Some(ExportContainer::Mp3),
        sample_rate: Some(96_000),
        ..Default::default()
    };
    let error =
        resolve_export_options(&project, 48_000, &rate).expect_err("96 kHz is not an MP3 rate");
    assert!(error.contains("96000 Hz"), "{error}");
    assert!(
        error.contains("44100, 48000, 16000, 22050, 24000, 11025 or 12000"),
        "{error}"
    );
}

/// A cancelled FLAC export publishes nothing and leaves no staged file.
#[test]
fn a_cancelled_flac_export_leaves_no_file() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, stage_offline_render,
    };
    use oc_dsl::ast::{ExportContainer, SampleFormat};

    let state = render_state();
    let dir = output_dir("cancel-flac");
    let output = dir.join("mix.flac");
    let target = output.display().to_string();
    let overrides = ExportOverrides {
        container: Some(ExportContainer::Flac),
        depth: Some(SampleFormat::I24),
        ..Default::default()
    };

    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    let cancel = snapshot
        .cancel
        .clone()
        .expect("the export claims a cancel token");
    let staged = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        crate::commands::render::render_offline_snapshot,
    ) {
        Ok(staged) => staged,
        Err(error) => panic!("the staged FLAC render failed: {error}"),
    };
    assert!(!output.exists(), "nothing is published yet");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "encoding stages one FLAC temp file"
    );
    let pending = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap();
    assert!(
        pending
            .file_name()
            .to_string_lossy()
            .contains(".oc-render-"),
        "{:?}",
        pending.file_name()
    );
    assert_eq!(
        &std::fs::read(pending.path()).unwrap()[..4],
        b"fLaC",
        "the staged file is FLAC"
    );

    state.request_export_cancel();
    let checkpoint = crate::commands::render::cancel_checkpoint(cancel);
    let error = checkpoint("publishing").unwrap_err();
    assert!(
        error.contains("cancelled at the publishing checkpoint"),
        "{error}"
    );
    drop(staged);
    assert!(!output.exists(), "a cancelled FLAC export must not publish");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "the staged FLAC file must be gone"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Two exports in flight own separate cancel tokens: the dialog's Cancel names
/// the export that started last and leaves the earlier one running.
#[test]
fn two_exports_have_independent_cancel_tokens() {
    use crate::commands::render::{ExportOverrides, capture_offline_snapshot};

    let state = render_state();
    let first = capture_offline_snapshot(&state, None, &ExportOverrides::default())
        .unwrap()
        .cancel
        .expect("the export claims a token");
    let second = capture_offline_snapshot(&state, None, &ExportOverrides::default())
        .unwrap()
        .cancel
        .expect("the export claims a token");

    state.request_export_cancel();

    assert!(second.is_cancelled(), "the current export is cancelled");
    assert!(
        !first.is_cancelled(),
        "an earlier export keeps its own token and its staged file"
    );

    // A later export starts clean even though the previous request stands.
    let third = capture_offline_snapshot(&state, None, &ExportOverrides::default())
        .unwrap()
        .cancel
        .expect("the export claims a token");
    assert!(!third.is_cancelled(), "a new export starts uncancelled");
    assert!(
        second.is_cancelled(),
        "the cancelled export keeps its request"
    );
}

/// A cancel from the engine keeps the previous export and publishes nothing.
#[test]
fn a_cancelled_render_keeps_the_previous_export_and_the_cache_clean() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, stage_offline_render,
    };

    let state = render_state();
    let dir = output_dir("cancel-engine");
    let output = dir.join("mix.wav");
    std::fs::write(&output, b"previous export").unwrap();
    let target = output.display().to_string();
    let overrides = ExportOverrides::default();

    let mut snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    let token = snapshot
        .cancel
        .clone()
        .expect("the snapshot carries a token");
    // The engine reports its first completed source, then this callback stops
    // the render before the mix: the cancel arrives inside the render, not at a
    // wrapper checkpoint.
    snapshot.progress = Some(Box::new(move |_| token.cancel()));

    let error = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        crate::commands::render::render_offline_snapshot,
    ) {
        Ok(_) => panic!("a cancelled render staged a file"),
        Err(error) => error,
    };
    assert!(error.contains("cancelled"), "{error}");
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"previous export",
        "the previous export stays byte-identical"
    );
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "no staged file survives a cancelled render"
    );
    assert!(
        state.range_cache.lock().unwrap().is_none(),
        "a cancelled render must not enter the render cache"
    );
    assert!(
        !state.lock_inner().dirty,
        "a cancelled render must not mark the project dirty"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A destination that cannot be written fails before the render closure runs.
#[test]
fn a_destination_under_a_file_fails_before_rendering() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, stage_offline_render,
    };

    let state = render_state();
    let dir = output_dir("preflight-file-parent");
    let parent = dir.join("not-a-directory");
    std::fs::write(&parent, b"file").unwrap();
    let target = parent.join("mix.wav").display().to_string();
    let overrides = ExportOverrides::default();
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();

    let error = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        |_| panic!("the destination was rejected before any render work"),
    ) {
        Ok(_) => panic!("an impossible destination staged a file"),
        Err(error) => error,
    };
    assert!(error.contains("cannot write"), "{error}");
    assert_eq!(std::fs::read(&parent).unwrap(), b"file");
    let _ = std::fs::remove_dir_all(dir);
}

/// A read-only directory is rejected before the render closure runs.
#[cfg(unix)]
#[test]
fn a_read_only_destination_fails_before_rendering() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, stage_offline_render,
    };
    use std::os::unix::fs::PermissionsExt;

    let state = render_state();
    let dir = output_dir("preflight-read-only");
    let read_only = dir.join("read-only");
    std::fs::create_dir(&read_only).unwrap();
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o555)).unwrap();
    let target = read_only.join("mix.wav").display().to_string();
    let overrides = ExportOverrides::default();
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();

    let error = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        |_| panic!("the destination was rejected before any render work"),
    ) {
        Ok(_) => panic!("a read-only destination staged a file"),
        Err(error) => error,
    };
    assert!(error.contains("cannot write"), "{error}");
    // Leave the temp directory removable.
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// A failed publish keeps the target and removes the staged file.
#[test]
fn a_failed_publish_keeps_the_target_and_removes_the_staged_file() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, stage_offline_render,
    };

    let state = render_state();
    let dir = output_dir("publish-failure");
    let output = dir.join("mix.wav");
    let target = output.display().to_string();
    let overrides = ExportOverrides::default();
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    let staged = stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        crate::commands::render::render_offline_snapshot,
    )
    .expect("the render stages");

    // A directory in the target's place makes the rename fail on every
    // platform, which is the same code path as a locked file on Windows.
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("keep.txt"), b"keep").unwrap();
    let error = staged.publish(&state).unwrap_err();
    assert!(error.contains("failed to publish"), "{error}");
    assert!(output.is_dir(), "the target is untouched");
    assert_eq!(std::fs::read(output.join("keep.txt")).unwrap(), b"keep");
    let staged_names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().contains(".oc-render-"))
        .collect();
    assert!(staged_names.is_empty(), "the staged file is removed");
    let _ = std::fs::remove_dir_all(dir);
}

/// A cancelled export publishes nothing: no partial file, no staged temp file.
#[test]
fn a_cancelled_export_leaves_no_file_at_all() {
    use crate::commands::render::{
        ExportOverrides, cancel_checkpoint, capture_offline_snapshot, stage_offline_render,
    };

    let state = render_state();
    let dir = output_dir("cancel");
    let output = dir.join("mix.wav");
    let overrides = ExportOverrides::default();
    let target = output.display().to_string();

    // Cancelled before synthesis: the checkpoint refuses and the render never runs.
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    let cancel = snapshot
        .cancel
        .clone()
        .expect("the export claims a cancel token");
    state.request_export_cancel();
    let checkpoint = cancel_checkpoint(cancel);
    let error = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &checkpoint,
        |_| panic!("a cancelled export must not render"),
    ) {
        Ok(_) => panic!("a cancelled export staged a file"),
        Err(error) => error,
    };
    assert!(
        error.contains("cancelled at the start checkpoint"),
        "{error}"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    // Cancelled after the mix is encoded: the staged temp file drops with the
    // staged render, so the directory is empty again.
    let snapshot = capture_offline_snapshot(&state, None, &overrides).unwrap();
    let cancel = snapshot
        .cancel
        .clone()
        .expect("the export claims a cancel token");
    let staged = match stage_offline_render(
        snapshot,
        &target,
        &overrides,
        &|_, _| {},
        &|_| Ok(()),
        crate::commands::render::render_offline_snapshot,
    ) {
        Ok(staged) => staged,
        Err(error) => panic!("the staged render failed: {error}"),
    };
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "encoding stages one temp file"
    );
    let pending = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    assert!(
        pending.to_string_lossy().contains(".oc-render-"),
        "{pending:?}"
    );

    state.request_export_cancel();
    let checkpoint = cancel_checkpoint(cancel);
    let error = checkpoint("publishing").unwrap_err();
    assert!(
        error.contains("cancelled at the publishing checkpoint"),
        "{error}"
    );
    drop(staged);
    assert!(!output.exists(), "a cancelled export must not publish");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "the staged temp file must be gone"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A stem set takes the dialog's depth, rate and stem name.
#[test]
fn the_stem_set_takes_the_dialog_options_and_the_stem_name() {
    use crate::commands::render::{ExportOverrides, ExportScope, RenderAudioRequest};

    let state = render_state();
    let dir = output_dir("dialog-stems");
    let request = RenderAudioRequest {
        scope: ExportScope::Stems,
        output_path: None,
        directory: Some(dir.display().to_string()),
        stem_name: Some("Take 2".into()),
        pre_roll: None,
        options: ExportOverrides {
            depth: Some(oc_dsl::ast::SampleFormat::I24),
            dither: Some(false),
            sample_rate: Some(48_000),
            ..Default::default()
        },
    };
    let result = tauri::async_runtime::block_on(
        crate::commands::render::render_stem_set_off_thread(&state, request, None),
    )
    .expect("the stem set publishes");

    let mix = dir.join("take-2-mix.wav");
    assert_eq!(result.path, mix.display().to_string());
    assert_eq!(result.files.first().unwrap(), &mix.display().to_string());
    assert!(dir.join("stems.json").exists(), "the manifest is published");
    assert_eq!(result.stems, 1, "one track stem beside the mix");
    assert_eq!(result.sample_rate, 48_000);
    assert_eq!(result.depth, oc_dsl::ast::SampleFormat::I24);
    assert!(!result.dither);
    assert!(result.bytes > 0);

    let reader = hound::WavReader::open(&mix).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.spec().sample_rate, 48_000);
    let _ = std::fs::remove_dir_all(dir);
}

/// The GUI passes neither override, so the project block must reach the file.
#[test]
fn the_project_export_block_selects_the_written_depth() {
    let state = render_state();
    {
        let mut inner = state.lock_inner();
        inner.project.properties.export = Some(export_block(oc_dsl::ast::SampleFormat::I24, true));
        inner.dsl_source = oc_dsl::serialize(&inner.project);
    }
    let dir = output_dir("project-format");
    let output = dir.join("mix.wav");
    run_render(Arc::clone(&state), output.clone())
        .join()
        .unwrap()
        .expect("the export publishes");
    let reader = hound::WavReader::open(&output).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.spec().sample_format, hound::SampleFormat::Int);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn gui_export_uses_request_worker_override_without_changing_sample_settings() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, render_offline_snapshot,
    };
    let state = render_state();
    let overrides = ExportOverrides {
        sample_rate: Some(16_000),
        ..ExportOverrides::default()
    };
    let mut expected = None;
    for count in ["1", "2"] {
        let mut snapshot = capture_offline_snapshot(&state, Some(0.025), &overrides).unwrap();
        snapshot.worker_options =
            oc_engine::render_parallel::RenderWorkerOptions::from_pin(Some(count), || 8);
        assert_eq!(snapshot.sample_rate, 16_000);
        assert_eq!(snapshot.pre_roll, 0.025);
        let (audio, _) = render_offline_snapshot(snapshot).unwrap();
        assert!(audio.iter().any(|sample| *sample != 0.0));
        if let Some(previous) = &expected {
            assert_eq!(previous, &audio);
        } else {
            expected = Some(audio);
        }
    }
}

#[test]
fn gui_export_worker_override_preserves_cancellation() {
    use crate::commands::render::{
        ExportOverrides, capture_offline_snapshot, render_offline_snapshot,
    };
    let state = render_state();
    let mut snapshot = capture_offline_snapshot(&state, None, &ExportOverrides::default()).unwrap();
    snapshot.worker_options =
        oc_engine::render_parallel::RenderWorkerOptions::from_pin(Some("1"), || 8);
    snapshot.cancel.as_ref().unwrap().cancel();
    let error = match render_offline_snapshot(snapshot) {
        Ok(_) => panic!("cancelled GUI export must not publish audio"),
        Err(error) => error,
    };
    assert!(error.contains("cancelled"), "{error}");
}
