#[test]
fn natural_playback_drains_effects_without_extending_the_timeline_or_preview() {
    use super::*;
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  effects:
    - saturator: drive=12%, mix=25%
@track synth | preset: tone, instrument: synth/lead
  @phrase p [1:1 - 2:1]
    C4 q mf
@master
  effects:
    - limiter: ceiling=-0.8dB
"#,
    )
    .unwrap();
    let rate = 8_000;
    let mut state = RenderState::new(rate);
    state.load_project(&project, PathBuf::new()).unwrap();
    let graph = state.graph.as_ref().unwrap();
    let latency = graph.master_chain.latency_samples() as usize;
    let pre_master = graph.pre_master_latency_samples() as u64;
    assert_eq!(
        state.playback_end(),
        state.total_frames + pre_master + 32 + latency as u64
    );
    let playback = streaming_playback(rtrb::RingBuffer::new(32).1);
    state.publish_ends(&playback);
    playback
        .play_cursor_frames
        .store(state.total_frames, Ordering::Release);
    assert_eq!(
        playback.total_frames.load(Ordering::Acquire),
        state.total_frames
    );
    assert!(!playback.has_played_out());
    playback
        .play_cursor_frames
        .store(state.playback_end(), Ordering::Release);
    assert!(playback.has_played_out());

    let mut streamed = Vec::new();
    while state.render_cursor < state.playback_end() {
        let block = state.render_chunk(511);
        assert!(!block.is_empty(), "{:?}", state.render_error);
        streamed.extend(block);
    }
    let offline = oc_engine::render_cache::RenderCache::new()
        .render_incremental(
            &project,
            std::path::Path::new(""),
            rate,
            &mut HashMap::new(),
        )
        .unwrap();
    let aligned = &streamed[latency * 2..];
    assert_eq!(aligned.len(), offline.len());
    let error = aligned
        .iter()
        .zip(&offline)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(error < 1e-5, "stream/offline maximum sample error {error}");

    state.play_end_frame = Some(2_000);
    state.seek_to(1_995).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    assert_eq!(state.render_chunk(32).len(), 10);
    assert!(state.render_chunk(32).is_empty());
}

#[test]
fn performance_stream_preserves_schedule_and_seek_history() {
    use super::*;
    let project = oc_dsl::parse(r#"@project
  tempo: 120
  tempo_map:
    1:2 = 90 bpm
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track played | preset: tone, instrument: synth/lead
  performance: { @0 ch0 note_on 60 90; @0.5 ch0 cc 64 127; @1 ch0 note_off 60 0; @1.5 ch0 cc 64 0; @1.5 ch1 note_on 67 70; @2 ch1 note_off 67 0; }
"#).unwrap();
    let mut state = RenderState::new(8000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert!(
        state.events.is_empty(),
        "score defaults must not shadow performance commands"
    );
    assert_eq!(state.performance_events[0].len(), 6);
    assert_eq!(state.performance_events[0][2].sample, 4000);
    let mut continuous = Vec::new();
    while continuous.len() < 18000 * 2 {
        continuous.extend(state.render_chunk(CHUNK_FRAMES));
    }
    assert!(continuous.iter().any(|sample| sample.abs() > 0.001));
    let mut seek = RenderState::new(8000);
    seek.load_project(&project, PathBuf::new()).unwrap();
    seek.play_end_frame = Some(16000);
    seek.seek_to(7000).unwrap();
    while seek.seek_target.is_some() {
        seek.preroll_chunk();
    }
    assert_eq!(seek.render_chunk(1024), continuous[14000..16048]);
    seek.seek_to(2000).unwrap();
    while seek.seek_target.is_some() {
        seek.preroll_chunk();
    }
    assert_eq!(
        seek.play_end_frame,
        Some(16000),
        "backward seek must preserve audition bounds"
    );
    assert_eq!(seek.render_chunk(1024), continuous[4000..6048]);
}

#[test]
fn muted_source_pruning_keeps_sidechain_and_duration() {
    let mut project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track key | preset: tone, instrument: synth/lead
  @phrase early [1:1 - 2:1]
    C4 w mf
  @phrase late [3:1 - 4:1]
    C4 w mf
@track bass | preset: tone, instrument: synth/lead
  @phrase p [1:1 - 2:1]
    C3 w mf
"#,
    )
    .unwrap();
    let mut state = super::RenderState::new(8000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let duration = state.total_frames;
    assert!(duration > 4 * 8000, "the late key sets the project end");

    project.tracks[0].mix.mute = true;
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    assert_eq!(state.source_tracks, vec![1]);
    assert_eq!(state.total_frames, duration);
    project.tracks[1].effects.push(oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Compression,
        params: vec![],
        bypass: false,
        sidechain_source: Some("key".into()),
    });
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    assert_eq!(state.source_tracks, vec![0, 1]);
    assert_eq!(state.total_frames, duration);
    let block = state.render_chunk(super::CHUNK_FRAMES);
    let offline = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        std::path::Path::new(""),
        8000,
        &mut std::collections::HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(&block, &offline[..block.len()]);
    project.tracks[1].effects.clear();
    project.tracks[1].mix.mute = true;
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    assert!(state.renderers.is_empty());
    assert_eq!(state.total_frames, duration);
    assert!(
        state
            .render_chunk(super::CHUNK_FRAMES)
            .iter()
            .all(|v| *v == 0.0)
    );
}

#[test]
fn dropping_busy_worker_does_not_wait_for_its_plugin_call() {
    use super::*;
    use std::time::Duration;
    let mut handle = RenderThreadHandle::spawn(8000);
    // Finish the real idle worker, then substitute a deliberately blocked
    // plugin-like call. Drop must not wait for that call to return.
    handle.cmd_tx.send(RenderCommand::Shutdown).unwrap();
    handle.thread.take().unwrap().join().unwrap();
    let shutdown = Arc::clone(&handle.shutdown);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    handle.thread = Some(std::thread::spawn(move || {
        release_rx.recv().unwrap();
        finished_tx.send(shutdown.load(Ordering::Acquire)).unwrap();
    }));
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(handle);
        dropped_tx.send(()).unwrap();
    });
    let nonblocking = dropped_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    // Always release/join before asserting, even if the regression returns.
    release_tx.send(()).unwrap();
    assert!(finished_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    dropper.join().unwrap();
    assert!(nonblocking, "UI drop waited for the blocked worker");
}

fn tone_project() -> Project {
    oc_dsl::parse("@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n").unwrap()
}

fn two_lane_project(second_muted: bool) -> Project {
    let mut project = oc_dsl::parse("@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n@track pad | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C3 w mf\n").unwrap();
    project.tracks[1].mix.mute = second_muted;
    project
}

/// Run the mixer edit and deliver the worker's lanes inline.
fn apply_mixer_inline(state: &mut RenderState, project: &Project) -> MixerOutcome {
    let outcome = state.apply_mixer(project, PathBuf::new()).unwrap();
    if let MixerOutcome::NeedsSources { tracks } = &outcome {
        let resolved = state.project.clone().unwrap();
        let lanes = build_lanes(
            &resolved,
            tracks,
            std::path::Path::new(""),
            state.sample_rate,
            &state.instrument_instantiations,
            state.root.as_deref(),
            state.worker_options,
        )
        .unwrap();
        state.splice_sources(lanes).unwrap();
    }
    outcome
}

fn clip_temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "opencompose-stream-clip-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Stereo ramp at the project rate; every frame has a distinct value.
fn clip_media(root: &std::path::Path) {
    clip_media_at(&root.join("clip.wav"));
}

/// The same ramp written at an explicit path.
fn clip_media_at(path: &std::path::Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..144_000 {
        writer.write_sample(frame as f32 * 1e-6).unwrap();
        writer.write_sample(frame as f32 * 1e-6 * 0.5).unwrap();
    }
    writer.finalize().unwrap();
}

/// One 2048-sample square frame; a loaded table sounds, a default one differs.
fn wavetable_media_at(path: &std::path::Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..2048 {
        let value = if frame < 1024 { 0.5 } else { -0.5 };
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

/// A mono sample long enough for the sampler to loop.
fn sampler_media_at(path: &std::path::Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..8_000 {
        let value = (std::f32::consts::TAU * 220.0 * frame as f32 / 8_000.0).sin() * 0.5;
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn clip_stream_project(root: &std::path::Path) -> Project {
    let text = r#"@project "clip stream"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "clip.wav"
  offset: 0.02s
  gain: -4dB
  fade_in: 0.05s
  fade_out: 0.1s
@track tape | instrument: audio/clip
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    oc_engine::block_source::prepare_project(&project, Some(root)).unwrap()
}

fn offline_render(project: &Project, root: &std::path::Path) -> Vec<f32> {
    oc_engine::render::render_to_buffer_with_plugins_at(
        project,
        std::path::Path::new(""),
        48_000,
        &mut std::collections::HashMap::new(),
        Some(root),
    )
    .unwrap()
}

/// Multi-output fake instrument: pair 0 prints 1.0/1.5, pair N prints
/// `N + 1` / `N + 1.5`. `mute_main` routes the main pair through a silent bus
/// so the master carries the auxiliary return alone.
fn fake_aux_project(plugin: &str, output: usize, mute_main: bool) -> Project {
    let mix = if mute_main {
        format!("output = sink, send(host.out{output}->drums=100%)")
    } else {
        format!("send(host.out{output}->drums=100%)")
    };
    let mut text = format!(
        "@project\n  tempo: 120\n@preset fake\n  source: vst3\n  plugin: \"{plugin}\"\n@track host | preset: fake, instrument: keys/kontakt\n  mix: {mix}\n  @phrase line [1:1 - 2:1]\n    C4 q\n@bus drums\n  routes: [master]\n"
    );
    if mute_main {
        text.push_str("@bus sink\n  effects:\n    - gain: factor=0.0\n  routes: [master]\n");
    }
    oc_dsl::parse(&text).unwrap()
}

fn offline_bus_stem(project: &Project, name: &str) -> Vec<f32> {
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache.set_vst3_tail_probe(|plan| {
        if plan.path.to_str() == Some("test.fake.two_bus_unity") {
            Ok(0)
        } else {
            Err(oc_host::vst3_instance::Vst3OwnedError::InvalidRequest(
                "Unexpected auxiliary stem fixture plugin",
            ))
        }
    });
    let (_, stems) = cache
        .render_export(project, std::path::Path::new(""), 48_000)
        .unwrap();
    stems
        .buses
        .into_iter()
        .find(|(bus, _)| bus == name)
        .map(|(_, samples)| samples)
        .expect("bus stem")
}

#[test]
fn streaming_captures_a_plugin_aux_pair_and_mixes_its_return() {
    use super::*;
    let project = fake_aux_project("test.fake.two_bus", 2, false);
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.source_outputs[0].aux_pairs(), &[1]);

    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    let aux = &state.aux_buffers[&(0, 1)];
    assert!((aux[0] - 2.0).abs() < 1e-6, "aux return {}", aux[0]);
    assert!((aux[1] - 2.5).abs() < 1e-6, "aux return {}", aux[1]);
    // Master carries the main pair (1.0/1.5) plus the aux return.
    assert!((mixed[0] - 3.0).abs() < 1e-6, "master {}", mixed[0]);
    assert!((mixed[1] - 4.0).abs() < 1e-6, "master {}", mixed[1]);
}

#[test]
fn streaming_reads_the_track_output_port() {
    use super::*;
    let text = "@project\n  tempo: 120\n@preset fake\n  source: vst3\n  plugin: \"test.fake.four_bus\"\n@track host | preset: fake, instrument: keys/kontakt, port: 2\n  @phrase line [1:1 - 2:1]\n    C4 q\n";
    let project = oc_dsl::parse(text).unwrap();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.source_outputs[0].main_pair(), 2);

    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    // Pair 2 prints 3.0 / 3.5 and reaches the master at unity.
    assert!((mixed[0] - 3.0).abs() < 1e-6, "master {}", mixed[0]);
    assert!((mixed[1] - 3.5).abs() < 1e-6, "master {}", mixed[1]);
}

#[test]
fn streaming_aux_return_respects_bus_mute_and_solo() {
    use super::*;
    let project = fake_aux_project("test.fake.two_bus", 2, false);
    let mut muted = project.clone();
    muted.buses[0].mute = true;
    let mut state = RenderState::new(48_000);
    state.load_project(&muted, PathBuf::new()).unwrap();
    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    assert!((mixed[0] - 1.0).abs() < 1e-6, "muted return {}", mixed[0]);
    assert!((mixed[1] - 1.5).abs() < 1e-6, "muted return {}", mixed[1]);

    // Soloing the return keeps its feed audible, exactly like offline.
    let mut soloed = project.clone();
    soloed.buses[0].solo = true;
    let mut state = RenderState::new(48_000);
    state.load_project(&soloed, PathBuf::new()).unwrap();
    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    assert!((mixed[0] - 3.0).abs() < 1e-6, "soloed return {}", mixed[0]);
    assert!((mixed[1] - 4.0).abs() < 1e-6, "soloed return {}", mixed[1]);
}

#[test]
fn streaming_aux_return_matches_the_offline_bus_level() {
    use super::*;
    oc_host::application_main::ApplicationMainExecutor::ensure_test_executor()
        .expect("test application executor");
    // The main pair is muted, so the master carries the aux return only.
    let project = fake_aux_project("test.fake.two_bus_unity", 2, true);
    let offline = offline_bus_stem(&project, "drums");
    assert!(
        (offline[0] - 2.0).abs() < 1e-6,
        "offline return {}",
        offline[0]
    );

    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    let aux = &state.aux_buffers[&(0, 1)];
    // Tolerance 1e-6: both renderers run the same send law and bus graph on
    // a constant plugin output, so only the block partition differs; f32
    // rounding of the unity gain is exact and 1e-6 leaves room for a future
    // non-constant fixture.
    let frames = aux.len().min(offline.len());
    let worst = (0..frames).fold(0.0_f32, |worst, index| {
        worst.max((aux[index] - offline[index]).abs())
    });
    assert!(worst <= 1e-6, "aux return differs by {worst}");
    assert!(
        (mixed[0] - 2.0).abs() < 1e-6,
        "streamed return {}",
        mixed[0]
    );
}

#[test]
fn streaming_delays_an_aux_pair_past_a_latent_lane_chain() {
    use super::*;
    // Lookahead compression on the preset lives in the lane chain. The aux
    // return bypasses that chain, so its samples must be delayed by the
    // chain latency to land where the graph's PDC assumes they arrive.
    let text = "@project\n  tempo: 120\n@preset fake\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n  effects:\n    - compression: lookahead=10ms\n@track host | preset: fake, instrument: keys/kontakt\n  mix: send(host.out2->drums=100%)\n  @phrase line [1:1 - 2:1]\n    C4 q\n@bus drums\n  routes: [master]\n";
    let project = oc_dsl::parse(text).unwrap();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    let latency = state.source_chains[0].latency_samples() as usize;
    assert!(latency > 0, "preset compressor reports no latency");
    assert_eq!(state.aux_delays[0][0].len(), latency * 2);

    let _ = state.render_chunk(super::CHUNK_FRAMES);
    let aux = &state.aux_buffers[&(0, 1)];
    assert!(
        aux[latency * 2 - 1] == 0.0,
        "aux return starts before the lane chain latency"
    );
    assert!(
        (aux[latency * 2] - 2.0).abs() < 1e-6,
        "{}",
        aux[latency * 2]
    );
    assert!(
        (aux[latency * 2 + 1] - 2.5).abs() < 1e-6,
        "{}",
        aux[latency * 2 + 1]
    );
}

#[test]
fn streaming_keeps_the_main_pair_when_the_plugin_declares_too_few_pairs() {
    use super::*;
    let project = fake_aux_project("test.fake.one_bus", 3, false);
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    // The unsupported send falls back to the previous main-only behaviour.
    assert!(state.source_outputs[0].aux_pairs().is_empty());
    assert!(state.aux_buffers.is_empty());
    let mixed = state.render_chunk(super::CHUNK_FRAMES);
    assert!((mixed[0] - 1.0).abs() < 1e-6, "main only {}", mixed[0]);
    assert!((mixed[1] - 1.5).abs() < 1e-6, "main only {}", mixed[1]);
}

#[test]
fn audio_clip_streams_identically_to_the_offline_render() {
    use super::*;
    let root = clip_temp_dir("identical");
    clip_media(&root);
    let project = clip_stream_project(&root);
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.renderers.len(), 1);
    assert!(
        state.renderers[0].is_silent(),
        "a clip-only track must not instantiate an instrument"
    );
    let lane_start = state.clip_lanes.get(0).unwrap().start_frame() as usize;
    assert_ne!(lane_start % CHUNK_FRAMES, 0, "clip starts inside a chunk");

    let mut streamed = Vec::new();
    while state.render_cursor < state.total_frames {
        streamed.extend(state.render_chunk(CHUNK_FRAMES));
    }
    let offline = offline_render(&project, &root);
    let frames = (streamed.len() / 2).min(offline.len() / 2);
    assert!(
        frames > lane_start + 48_000,
        "compared range must cover the clip"
    );
    for index in 0..frames * 2 {
        assert_eq!(
            streamed[index],
            offline[index],
            "sample {index} (frame {})",
            index / 2
        );
    }
    assert!(streamed[..lane_start * 2].iter().all(|s| *s == 0.0));
    assert!(
        streamed[(lane_start + 3_000) * 2].abs() > 0.0,
        "the clip must start once its fade-in is over"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A project read from a file resolves its clip against that file's
/// directory on the first load, when nothing else knows the path yet.
#[test]
fn a_relative_clip_media_path_resolves_on_the_first_project_load() {
    use super::*;
    let root = clip_temp_dir("relative-media");
    std::fs::create_dir_all(root.join("media/samples")).unwrap();
    clip_media_at(&root.join("media/samples/hit.wav"));
    let text = r#"@project "relative media"
  tempo: 120
@audio_clip hit [1:1 - 3:1] | track: tape
  file: "media/samples/hit.wav"
@track tape | instrument: audio/clip
"#;
    let project_file = root.join("song.ocp");
    std::fs::write(&project_file, text).unwrap();
    let project = oc_dsl::parse(&std::fs::read_to_string(&project_file).unwrap()).unwrap();

    let handle = RenderThreadHandle::spawn(48_000);
    handle.send(RenderCommand::ProjectChanged {
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        root: Some(root.clone()),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(10))
        .unwrap();

    wait_for_warm(&handle);

    let media = root.join("media/samples/hit.wav");
    assert_eq!(
        oc_engine::media_cache::shared().decodes_for(&media),
        1,
        "the clip decoded from the project file's directory"
    );
    assert!(
        handle.playback.total_frames.load(Ordering::Acquire) > 3 * 48_000,
        "the clip extends the streaming length"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Opening audio can play even when a later clip is unavailable.
#[test]
fn progressive_clip_playback_reports_a_missing_file_at_its_range() {
    let root = clip_temp_dir("missing-media");
    clip_media(&root);
    let project = oc_dsl::parse(
        r#"@project "missing media"
  tempo: 120
@audio_clip opening [1:1 - 2:1] | track: tape
  file: "clip.wav"
@audio_clip missing [3:1 - 4:1] | track: tape
  file: "media/samples/gone.wav"
@track tape | instrument: audio/clip
"#,
    )
    .unwrap();

    let handle = RenderThreadHandle::spawn(48_000);
    handle.send(RenderCommand::ProjectChanged {
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        root: Some(root.clone()),
        warm: false,
    });
    let timeout = std::time::Duration::from_secs(5);
    handle.wait_project_loaded(timeout).unwrap();
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let mut output = [0.0; 512];
    let started = std::time::Instant::now();
    loop {
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        if output.iter().any(|sample| *sample != 0.0) {
            break;
        }
        assert!(started.elapsed() < timeout, "{:?}", handle.load_error());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(handle.load_error().is_none());

    let missing_start = 4 * 48_000;
    handle.send(RenderCommand::Seek {
        frame: missing_start,
    });
    let started = std::time::Instant::now();
    let error = loop {
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        assert!(output.iter().all(|sample| *sample == 0.0));
        if let Some(error) = handle.load_error() {
            break error;
        }
        assert!(
            started.elapsed() < timeout,
            "missing media was not reported"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert!(
        error.contains(&root.join("media/samples/gone.wav").display().to_string()),
        "{error}"
    );
    assert!(!handle.playback.active.load(Ordering::Acquire));
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        missing_start
    );
    drop(handle);
    std::fs::remove_dir_all(root).unwrap();
}

/// Exercise a live seek while the progressive loader is blocked on a
/// different warp variant. Use the real callback and a separate offline
/// reference so the destination remains cold in the live cache.
#[test]
fn streaming_seeks_past_a_busy_warp_and_resumes_the_requested_samples() {
    use oc_engine::derived_cache::{self, StretchKey};
    use oc_engine::media_cache::{self, media_key};
    use oc_engine::stretch;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root = oc_audio::private_fs::test_root().join("seek-past-busy-warp");
    std::fs::create_dir_all(&root).unwrap();
    for name in ["opening.wav", "blocked.wav", "destination.wav"] {
        clip_media_at(&root.join(name));
    }
    std::fs::copy(root.join("destination.wav"), root.join("reference.wav")).unwrap();
    let project = oc_dsl::parse(
        r#"@project "Seek past busy warp"
  tempo: 120
  live_resample: 4
@audio_clip opening [1:1 - 2:1] | track: tape
  file: "opening.wav"
@audio_clip blocked [2:1 - 4:1] | track: tape
  file: "blocked.wav"
  stretch: 1.25
  mode: percussive
  warp: 0.2s -> 0.25s
@audio_clip destination [4:1 - 6:1] | track: tape
  file: "destination.wav"
  stretch: 1.25
  mode: percussive
  warp: 0.2s -> 0.25s
@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    let mut reference = project.clone();
    for clip in reference.audio_clips_mut() {
        if clip.name == "blocked" {
            clip.muted = true;
        } else if clip.name == "destination" {
            clip.file = "reference.wav".into();
        }
    }
    let expected = offline_render(&reference, &root);
    let timeout = Duration::from_secs(5);
    let blocked_file = root.join("blocked.wav");
    let destination_file = root.join("destination.wav");
    let blocked = project.audio_clips()[1];
    assert_eq!(blocked.name, "blocked");
    let plan = stretch::resolve(&project, blocked, 48_000, 144_000).unwrap();
    let key = StretchKey::new(
        media_key(&blocked_file).unwrap(),
        48_000,
        plan.ratio,
        plan.mode,
        stretch::ALGO_VERSION,
    )
    .with_warp(&plan.warp);
    let fade = crate::audio::RESUME_FADE_FRAMES as usize * 2;

    std::thread::scope(|scope| {
        let (entered, locked) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let holder = scope.spawn(move || {
            derived_cache::shared().get_or_bake(key, 8, || {
                entered.send(()).unwrap();
                let _ = held.recv();
                Err("Release the test bake without publishing samples".into())
            })
        });
        locked.recv_timeout(timeout).unwrap();
        let handle = RenderThreadHandle::spawn(48_000);
        let pull_at = |frame, output: &mut [f32]| {
            let started = Instant::now();
            loop {
                crate::audio::streaming_audio_callback(output, &handle.playback);
                let cursor = handle.playback.play_cursor_frames.load(Ordering::Acquire);
                if cursor == frame + (output.len() / 2) as u64 {
                    return;
                }
                assert!(
                    started.elapsed() < timeout,
                    "no block at {frame}: {:?}",
                    handle.load_error()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        handle.send(RenderCommand::ProjectChanged {
            project: Box::new(project),
            soundfont_path: PathBuf::new(),
            root: Some(root.clone()),
            warm: false,
        });
        handle.wait_project_loaded(timeout).unwrap();
        handle.send(RenderCommand::Play {
            start_frame: 0,
            resume: false,
            end_frame: None,
        });
        let mut opening = [0.0; 256];
        pull_at(0, &mut opening);
        assert_eq!(&opening[fade..], &expected[fade..opening.len()]);
        let started = Instant::now();
        while media_cache::shared().decodes_for(&blocked_file) == 0 {
            assert!(started.elapsed() < timeout, "{:?}", handle.load_error());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(media_cache::shared().decodes_for(&destination_file), 0);

        // Cross the warp marker over unequal callback blocks after the seek.
        let target = 6 * 48_000 + 12_000 - 207;
        handle.send(RenderCommand::Seek {
            frame: target as u64,
        });
        let mut landed = [0.0; 256];
        pull_at(target as u64, &mut landed);
        let offset = target * 2;
        assert_eq!(
            &landed[fade..],
            &expected[offset + fade..offset + landed.len()]
        );
        let mut across_marker = [0.0; 194];
        crate::audio::streaming_audio_callback(&mut across_marker, &handle.playback);
        let offset = offset + landed.len();
        assert_eq!(
            across_marker,
            expected[offset..offset + across_marker.len()]
        );
        assert_eq!(media_cache::shared().decodes_for(&destination_file), 1);
        assert!(
            !holder.is_finished(),
            "the destination must play before the held bake ends"
        );

        let paused_at = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        handle.send(RenderCommand::Pause);
        let mut paused = [1.0; 256];
        crate::audio::streaming_audio_callback(&mut paused, &handle.playback);
        assert!(paused.iter().all(|sample| *sample == 0.0));
        assert_eq!(
            handle.playback.play_cursor_frames.load(Ordering::Acquire),
            paused_at
        );

        let back = 913;
        handle.send(RenderCommand::Seek { frame: back });
        handle.send(RenderCommand::Play {
            start_frame: back,
            resume: false,
            end_frame: None,
        });
        let mut replay = [0.0; 256];
        pull_at(back, &mut replay);
        assert_eq!(
            &replay[fade..],
            &expected[back as usize * 2 + fade..back as usize * 2 + replay.len()]
        );
        assert_eq!(
            handle.playback.play_cursor_frames.load(Ordering::Acquire),
            back + 128
        );
        assert!(handle.load_error().is_none());

        drop(handle);
        release.send(()).unwrap();
        assert!(holder.join().unwrap().is_err());
    });
}

#[test]
#[ignore = "Requires a local 3.2-second drum-loop excerpt at 48 kHz"]
fn recorded_drum_warp_matches_offline_after_streaming_seeks() {
    use std::time::{Duration, Instant};

    let source = std::env::var_os("OCP_TEST_DRUM_WARP_SOURCE").expect("set the drum excerpt path");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::copy(source, root.join("drums.wav")).unwrap();
    std::fs::copy(root.join("drums.wav"), root.join("reference.wav")).unwrap();
    let project = oc_dsl::parse(
        r#"@project "Recorded drum warp"
  tempo: 120
  live_resample: 4
@audio_clip drums [1:1 - 5:1] | track: drum
  file: "drums.wav"
  stretch: 1.25
  mode: percussive
  warp: 0.4s -> 0.5s
  warp: 1.2s -> 1.52s
  warp: 2.4s -> 2.98s
  warp: 3.2s -> 4s
  loop: 4s
@track drum | instrument: audio/clip
"#,
    )
    .unwrap();
    let mut reference = project.clone();
    reference.audio_clip_mut(0).unwrap().file = "reference.wav".into();
    let expected = offline_render(&reference, root);

    let handle = RenderThreadHandle::spawn(48_000);
    handle.send(RenderCommand::ProjectChanged {
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        root: Some(root.to_path_buf()),
        warm: false,
    });
    let timeout = Duration::from_secs(10);
    handle.wait_project_loaded(timeout).unwrap();
    let playback = &handle.playback;
    let pull = |frame: usize, frames: usize, fade: usize| {
        let mut output = vec![0.0; frames * 2];
        let deadline = Instant::now() + timeout;
        loop {
            assert!(
                Instant::now() < deadline,
                "frame {frame}: {:?}",
                handle.load_error()
            );
            if playback.seek_generation.load(Ordering::Acquire)
                != playback.callback_generation.load(Ordering::Acquire)
                || ring_slots(&handle) >= output.len()
            {
                crate::audio::streaming_audio_callback(&mut output, playback);
                if playback.play_cursor_frames.load(Ordering::Acquire) == (frame + frames) as u64 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            &output[fade * 2..],
            &expected[(frame + fade) * 2..(frame + frames) * 2],
            "frame {frame}"
        );
    };

    let fade = crate::audio::RESUME_FADE_FRAMES as usize;
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    pull(0, 257, fade);

    let target = 4 * 48_000 - 207;
    handle.send(RenderCommand::Seek {
        frame: target as u64,
    });
    pull(target, 128, fade);
    pull(target + 128, 97, 0);
    pull(target + 225, 1024, 0);

    let paused_at = playback.play_cursor_frames.load(Ordering::Acquire);
    handle.send(RenderCommand::Pause);
    let mut paused = [1.0; 256];
    crate::audio::streaming_audio_callback(&mut paused, playback);
    assert_eq!(paused, [0.0; 256]);
    assert_eq!(
        playback.play_cursor_frames.load(Ordering::Acquire),
        paused_at
    );

    let back = 72_960 - 119;
    handle.send(RenderCommand::Seek { frame: back as u64 });
    handle.send(RenderCommand::Play {
        start_frame: back as u64,
        resume: false,
        end_frame: None,
    });
    pull(back, 128, fade);
    pull(back + 128, 97, 0);
    assert_eq!(playback.underruns.load(Ordering::Acquire), 0);
    assert!(handle.load_error().is_none());
}

#[test]
fn paused_seek_crosses_missing_media_and_playback_still_checks_the_destination() {
    let root = clip_temp_dir("seek-over-missing-media");
    clip_media(&root);
    let project = oc_dsl::parse(
        r#"@project "Seek across missing media"
  tempo: 120
@audio_clip missing [1:1 - 2:1] | track: tape
  file: "gone.wav"
@audio_clip valid [2:1 - 3:1] | track: tape
  file: "clip.wav"
@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    let mut state = RenderState::new(48_000);
    state
        .load_project_from(&project, PathBuf::new(), Some(root.clone()), 0)
        .unwrap();
    state.seek_to(96_513).unwrap();
    let start = std::time::Instant::now();
    while state.seek_target.is_some() {
        let advanced = state.preroll_chunk();
        assert!(state.render_error.is_none(), "{:?}", state.render_error);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        if advanced == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    assert_eq!(state.dsp_cursor, 96_513);
    let block = state.render_chunk(16);
    assert_eq!(block.len(), 32);
    assert!((block[0] - 513e-6).abs() < 1e-7, "{}", block[0]);

    // Even a preroll that ends in the missing clip must leave the next
    // audible range unrendered and subject to normal media validation.
    state.seek_to(513).unwrap();
    while state.seek_target.is_some() {
        assert!(state.preroll_chunk() > 0);
    }
    assert!(state.render_chunk(16).is_empty());
    assert!(state.render_error.as_deref().unwrap().contains("gone.wav"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

/// A project's relative wavetable has the same calibrated output on the first
/// live load and in offline renders, including with a prepared absolute path.
#[test]
fn a_relative_preset_wavetable_resolves_on_the_first_project_load() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("media/tables")).unwrap();
    wavetable_media_at(&root.join("media/tables/square.wav"));
    let text = r#"@project "relative wavetable"
  tempo: 120
@preset wave
  source: builtin
  plugin: "builtin.open_synth"
  params: { wavetable.file: "media/tables/square.wav", osc.a.waveform: 4, output.gain: 0.05 }
@track lead | preset: wave, instrument: synth/lead
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;
    let project_file = root.join("song.ocp");
    std::fs::write(&project_file, text).unwrap();
    let project = oc_dsl::parse(&std::fs::read_to_string(&project_file).unwrap()).unwrap();

    let (stream, _) = record_live_from(&project, Some(root), super::FIRST_CHUNK_FRAMES, None);
    assert!(
        stream.iter().any(|sample| sample.abs() > 1e-3),
        "the wavetable at media/tables/square.wav sounds"
    );
    let prepared = oc_engine::block_source::prepare_project(&project, Some(root)).unwrap();
    let params = oc_engine::preset::builtin_track_params(
        &prepared,
        prepared.presets.first(),
        &prepared.tracks[0],
    );
    assert!(params.iter().any(|(name, value)| {
        name == oc_engine::loudness::TRIM_PARAM && value.as_f64().is_some_and(|db| db > 1.0)
    }));
    let reference = offline_render(&prepared, root);
    let relative = offline_render(&project, root);
    assert_eq!(relative, reference, "relative and absolute offline renders");
    assert_eq!(
        stream.as_slice(),
        &reference[..stream.len()],
        "live callback and offline render"
    );
}

/// The same dispatch resolves a sampler's relative sample file.
#[test]
fn a_relative_preset_sampler_sample_resolves_on_the_first_project_load() {
    let root = clip_temp_dir("relative-sampler");
    std::fs::create_dir_all(root.join("media/samples")).unwrap();
    sampler_media_at(&root.join("media/samples/tone.wav"));
    let text = r#"@project "relative sampler"
  tempo: 120
@preset sampled
  source: builtin
  plugin: "builtin.sampler"
  params: { sampler.sample_file: "media/samples/tone.wav", sampler.root_note: 60 }
@track pad | preset: sampled, instrument: sampler
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;
    let project_file = root.join("song.ocp");
    std::fs::write(&project_file, text).unwrap();
    let project = oc_dsl::parse(&std::fs::read_to_string(&project_file).unwrap()).unwrap();

    let (stream, _) = record_live_from(&project, Some(&root), 24_000, None);
    assert!(
        stream.iter().any(|sample| sample.abs() > 1e-3),
        "the sample at media/samples/tone.wav sounds"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A genuinely missing preset asset still refuses the load and names the
/// resolved path it tried.
#[test]
fn a_missing_preset_asset_is_refused_with_the_path_it_tried() {
    use super::*;
    let root = clip_temp_dir("missing-wavetable");
    std::fs::create_dir_all(root.join("media/tables")).unwrap();
    let text = r#"@project "missing wavetable"
  tempo: 120
@preset wave
  source: builtin
  plugin: "builtin.open_synth"
  params: { wavetable.file: "media/tables/gone.wav" }
@track lead | preset: wave, instrument: synth/lead
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;
    let project_file = root.join("song.ocp");
    std::fs::write(&project_file, text).unwrap();
    let project = oc_dsl::parse(&std::fs::read_to_string(&project_file).unwrap()).unwrap();

    let handle = RenderThreadHandle::spawn(48_000);
    handle.send(RenderCommand::ProjectChanged {
        root: Some(root.clone()),
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    let error = handle
        .wait_project_loaded(std::time::Duration::from_secs(10))
        .expect_err("a missing wavetable must refuse the load");
    let tried = root.join("media/tables/gone.wav");
    assert!(error.contains(&tried.display().to_string()), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_looped_audio_clip_streams_identically_to_the_offline_render() {
    use super::*;
    let root = clip_temp_dir("looped");
    clip_media(&root);
    let text = r#"@project "clip loop stream"
  tempo: 120
@audio_clip hit [2:1 - 4:1] | track: tape
  file: "clip.wav"
  offset: 0.1s
  loop: 0.75s
  fade_in: 0.05s
  fade_out: 0.1s
@track tape | instrument: audio/clip
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    let project = oc_engine::block_source::prepare_project(&project, Some(&root)).unwrap();

    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.renderers.len(), 1);
    assert!(
        state.renderers[0].is_silent(),
        "a clip-only track must not instantiate an instrument"
    );
    let lane_start = state.clip_lanes.get(0).unwrap().start_frame() as usize;
    assert_ne!(lane_start % CHUNK_FRAMES, 0, "clip starts inside a chunk");

    let mut streamed = Vec::new();
    while state.render_cursor < state.total_frames {
        streamed.extend(state.render_chunk(CHUNK_FRAMES));
    }
    let offline = offline_render(&project, &root);
    let frames = (streamed.len() / 2).min(offline.len() / 2);
    assert!(
        frames > lane_start + 48_000,
        "compared range must cover the clip"
    );
    for index in 0..frames * 2 {
        assert_eq!(
            streamed[index],
            offline[index],
            "sample {index} (frame {})",
            index / 2
        );
    }
    // The loop repeats the media head: the frame one loop after the head is
    // the same source sample, and the clip fills its whole range.
    let loop_frames = (0.75 * 48_000.0) as usize;
    let head = (lane_start + 6_000) * 2;
    let wrapped = (lane_start + loop_frames + 6_000) * 2;
    assert_eq!(streamed[head], streamed[wrapped], "the loop repeats");
    // Inside the clip but before its fade-out, the loop is still playing.
    let tail = (lane_start + 4 * 48_000 - 5_000) * 2;
    assert!(
        streamed[tail].abs() > 0.0,
        "the looped clip fills its range"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_clip_mixes_with_the_instrument_on_its_track() {
    use super::*;
    let root = clip_temp_dir("instrument");
    clip_media(&root);
    let text = r#"@project "clip and synth"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 2, output.gain: 0.2}
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "clip.wav"
  gain: -6dB
@track tape | preset: tone, instrument: synth/lead
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    let project = oc_engine::block_source::prepare_project(&project, Some(&root)).unwrap();

    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.renderers.len(), 1);
    assert!(!state.renderers[0].is_silent());

    let mut streamed = Vec::new();
    while state.render_cursor < state.total_frames {
        streamed.extend(state.render_chunk(CHUNK_FRAMES));
    }
    let offline = offline_render(&project, &root);
    let frames = (streamed.len() / 2).min(offline.len() / 2);
    let lane_start = state.clip_lanes.get(0).unwrap().start_frame() as usize;
    assert!(frames > lane_start + 48_000);
    for index in 0..frames * 2 {
        assert_eq!(streamed[index], offline[index], "sample {index}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn dc_media(root: &std::path::Path) {
    let path = root.join("dc.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..192_000 {
        writer.write_sample(1.0f32).unwrap();
        writer.write_sample(1.0f32).unwrap();
    }
    writer.finalize().unwrap();
}

/// Unity DC through a 1 Hz `gain` lane, so every sample is the applied gain.
fn touch_clip_project(root: &std::path::Path) -> Project {
    let text = r#"@project "touch clip"
  tempo: 120
@audio_clip bed [1:1 - 3:1] | track: tape
  file: "dc.wav"
@track tape | instrument: audio/clip
  effects:
    - gain: factor=1
  automation: gain.factor {1:1=1, 1:2=0.5, 1:3=1, 1:4=0.5, 2:1=1, 2:2=0.5, 2:3=1, 2:4=0.5, 3:1=1}
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    oc_engine::block_source::prepare_project(&project, Some(root)).unwrap()
}

#[test]
fn a_live_touch_holds_the_hand_and_never_changes_the_offline_render() {
    use oc_engine::touch::{TouchRegistry, effect_key};
    let root = clip_temp_dir("touch");
    dc_media(&root);
    let project = touch_clip_project(&root);
    let key = effect_key("track:0", 0, "factor");
    let registry = Arc::new(TouchRegistry::new());

    // Lane-only live render.
    let mut reference = RenderState::new(48_000);
    reference.load_project(&project, PathBuf::new()).unwrap();
    let mut lane = Vec::new();
    while reference.render_cursor < reference.total_frames {
        lane.extend(reference.render_chunk(240));
    }

    // The same render with a touch held from 1 s to 3 s.
    let mut state = RenderState::new(48_000);
    state.touch = Some(Arc::clone(&registry));
    state.load_project(&project, PathBuf::new()).unwrap();
    let mut touched = Vec::new();
    while state.render_cursor < state.total_frames {
        match state.render_cursor {
            48_000 => registry.begin(&key, 0.1),
            144_000 => registry.release(&key),
            _ => {}
        }
        touched.extend(state.render_chunk(240));
    }

    // Before the touch the lane is in charge.
    assert_eq!(&touched[..48_000 * 2], &lane[..48_000 * 2]);
    // Two seconds of held touch stay at -20 dB on every sample.
    // The lane at unity is the clip through the centre pan law; -20 dB is
    // that value times 0.1 on every held sample.
    let hand = 0.1f32 * lane[0];
    assert!(
        touched[48_000 * 2..144_000 * 2]
            .iter()
            .all(|sample| *sample == hand),
        "the held touch must own the gain"
    );
    // The lane is back 50 ms after the release.
    let back = 144_000 * 2 + 2_400 * 2;
    assert_eq!(&touched[back..], &lane[back..]);

    // The offline render never reads the live touch state, even while a
    // touch is held: the lane is rendered as if untouched.
    registry.begin(&key, 0.1);
    let offline = offline_render(&project, &root);
    registry.release(&key);
    let frames = (lane.len() / 2).min(offline.len() / 2);
    assert_eq!(
        &lane[..frames * 2],
        &offline[..frames * 2],
        "a live lane render must equal the offline render"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stream_seek_and_replay_start_the_clip_once() {
    use super::*;
    let root = clip_temp_dir("seek");
    clip_media(&root);
    let project = clip_stream_project(&root);
    let offline = offline_render(&project, &root);
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    let lane_start = state.clip_lanes.get(0).unwrap().start_frame();
    let target = lane_start + 12_345;

    state.seek_to(target).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    assert_eq!(state.render_cursor, target);
    let block = state.render_chunk(CHUNK_FRAMES);
    let offset = target as usize * 2;
    assert_eq!(
        &block[..],
        &offline[offset..offset + CHUNK_FRAMES * 2],
        "seek must resume the clip at its source sample"
    );

    // Rewinding rebuilds the lanes from the cache and must not double-start.
    state.seek_to(0).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    let replay = state.render_chunk(CHUNK_FRAMES);
    let mut fresh = RenderState::new(48_000);
    fresh.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(replay, fresh.render_chunk(CHUNK_FRAMES));
    assert_eq!(replay, &offline[..CHUNK_FRAMES * 2]);
    std::fs::remove_dir_all(root).unwrap();
}

/// Four lanes of one instrument source at a fast tempo, so a test can play
/// to bar 8 without spending seconds of wall clock.
fn four_lane_project(source: &str) -> Project {
    let mut text =
        String::from("@project \"seek\"\n  tempo: 400\n  time_signature: 4/4\n@preset voice\n");
    text.push_str(source);
    for (index, name) in ["one", "two", "three", "four"].iter().enumerate() {
        // One whole note per bar keeps the arrangement sounding through
        // bar 8, where the backward seek starts.
        let notes = format!("    C{} w mf\n", 3 + index).repeat(8);
        text.push_str(&format!(
                "@track {name} | preset: voice, instrument: synth/lead\n  @phrase a [1:1:0 - 9:1:0]\n{notes}"
            ));
    }
    oc_dsl::parse(&text).unwrap()
}

fn four_vst3_project() -> Project {
    four_lane_project("  source: vst3\n  plugin: \"test.fake.one_bus\"\n")
}

/// Render until the DSP cursor passes bar 8, the position the review used.
fn render_to_bar_eight(state: &mut RenderState) -> u64 {
    let clock =
        oc_dsl::timing::ProjectClock::new(&state.project.clone().unwrap().properties).unwrap();
    let bar_eight = clock.sample_at_quarter(28.0, state.sample_rate).unwrap();
    while state.dsp_cursor < bar_eight {
        let block = state.render_chunk(CHUNK_FRAMES);
        assert!(!block.is_empty(), "playback stopped before bar 8");
    }
    bar_eight
}

#[test]
fn replacing_or_resetting_the_graph_invalidates_pending_source_builds() {
    use super::*;
    let project = oc_dsl::parse("@project \"Source epoch\"\n  tempo: 120\n").unwrap();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    let stale = state.source_epoch;
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.source_epoch, stale + 1);
    let (_, consumer) = rtrb::RingBuffer::new(16);
    let playback = streaming_playback(consumer);
    fail_playback(&mut state, &playback, "test reset".into());
    assert_eq!(state.source_epoch, stale + 2);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.source_epoch, stale + 3);
}

/// A backward seek keeps every instrument and starts the new position with
/// no voice held from before it. The first block must meet the 50 ms budget.
#[test]
fn a_backward_seek_keeps_the_instruments_and_starts_clean() {
    use super::*;
    let project = four_vst3_project();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    state.playing = true;
    render_to_bar_eight(&mut state);

    let instantiations = state.instrument_instantiations.load(Ordering::Acquire);
    let ids: Vec<u64> = state
        .renderers
        .iter()
        .map(SmartSource::instance_id)
        .collect();
    let epoch = state.source_epoch;

    let started = std::time::Instant::now();
    state.seek_to(0).unwrap();
    let first_block = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        state.instrument_instantiations.load(Ordering::Acquire),
        instantiations,
        "a backward seek must not instantiate an instrument"
    );
    assert_eq!(state.source_epoch, epoch, "the project was reloaded");
    assert_eq!(state.render_cursor, 0);
    assert_eq!(state.dsp_cursor, 0);
    assert!(
        state.seek_target.is_none(),
        "a seek to the origin has no gap to replay"
    );
    assert!(
        state.sounding.iter().all(|word| *word == 0),
        "the destination starts with every note released"
    );
    let block = state.render_chunk(super::FIRST_CHUNK_FRAMES);
    assert!(!block.is_empty(), "the seek produced no audio");
    assert_eq!(
        state
            .renderers
            .iter()
            .map(SmartSource::instance_id)
            .collect::<Vec<_>>(),
        ids
    );

    // The block itself is the first one the device would receive.
    let first_block = first_block.max(started.elapsed().as_secs_f64() * 1000.0);
    eprintln!("backward_seek_first_block_ms={first_block:.2}");
    assert!(
        first_block < crate::first_block::FIRST_BLOCK_BUDGET_MS,
        "first block after a backward seek took {first_block:.2} ms"
    );
}

/// A lane that cannot restore its own state is rebuilt on its own; the
/// other lanes keep their instances.
#[test]
fn a_backward_seek_rebuilds_only_a_lane_that_cannot_rewind() {
    use super::*;
    let project = four_vst3_project();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    state.playing = true;
    render_to_bar_eight(&mut state);

    let instantiations = state.instrument_instantiations.load(Ordering::Acquire);
    let ids: Vec<u64> = state
        .renderers
        .iter()
        .map(SmartSource::instance_id)
        .collect();
    state.renderers[2].mark_non_rewindable();

    state.seek_to(0).unwrap();
    assert_eq!(
        state.instrument_instantiations.load(Ordering::Acquire),
        instantiations + 1,
        "exactly the non-rewindable lane is rebuilt"
    );
    let after: Vec<u64> = state
        .renderers
        .iter()
        .map(SmartSource::instance_id)
        .collect();
    assert_ne!(after[2], ids[2], "the marked lane was not rebuilt");
    for lane in [0, 1, 3] {
        assert_eq!(after[lane], ids[lane], "lane {lane} was rebuilt");
    }
    assert!(state.sounding.iter().all(|word| *word == 0));
}

#[test]
fn playing_during_a_paused_seek_limits_preroll() {
    let project = tone_project();
    let mut state = RenderState::new(8000);
    state.load_project(&project, PathBuf::new()).unwrap();
    state.paused = true;
    state.seek_to(8_000).unwrap();
    assert_eq!(state.seek_target, Some(8_000));
    assert!(state.preroll_deadline.is_none());

    state.start_playing();
    let deadline = state
        .preroll_deadline
        .expect("play must bound pending preroll");
    assert!(deadline <= std::time::Instant::now() + super::SEEK_PREROLL_BUDGET);
    state.preroll_deadline = Some(std::time::Instant::now());
    assert!(state.preroll_expired());
    state.abandon_preroll();
    assert_eq!(state.render_cursor, 8_000);
    assert_eq!(state.dsp_cursor, 8_000);
    assert!(state.seek_target.is_none());
}

/// A forward seek keeps its history: no lane is rewound or rebuilt.
#[test]
fn a_forward_seek_keeps_the_fast_path() {
    use super::*;
    let project = four_vst3_project();
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    state.playing = true;
    render_to_bar_eight(&mut state);

    let instantiations = state.instrument_instantiations.load(Ordering::Acquire);
    let ids: Vec<u64> = state
        .renderers
        .iter()
        .map(SmartSource::instance_id)
        .collect();
    let cursor = state.render_cursor;
    let dsp_cursor = state.dsp_cursor;
    let target = cursor + 48_000;

    state.seek_to(target).unwrap();
    assert_eq!(
        state.render_cursor, cursor,
        "a forward seek rewound the lanes"
    );
    assert_eq!(
        state.dsp_cursor, dsp_cursor,
        "a forward seek dropped the DSP"
    );
    assert_eq!(
        state.instrument_instantiations.load(Ordering::Acquire),
        instantiations
    );
    assert_eq!(
        state
            .renderers
            .iter()
            .map(SmartSource::instance_id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(state.seek_target, Some(target));
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    assert_eq!(state.render_cursor, target);
}

#[test]
fn performance_hot_add_restores_held_notes_and_controller_history() {
    use super::*;
    let mut project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track held | preset: tone, instrument: synth/lead
  performance: { @0 ch0 note_on 60 90; @0.2 ch0 cc 64 127; @0.5 ch0 note_off 60 0; @2 ch0 cc 64 0; }
"#,
    )
    .unwrap();
    project.tracks[0].mix.mute = true;
    let mut state = RenderState::new(8000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert!(state.renderers.is_empty());
    project.tracks[0].mix.mute = false;
    assert!(matches!(
        state.apply_mixer(&project, PathBuf::new()).unwrap(),
        MixerOutcome::Reloaded
    ));
    assert_eq!(state.performance_events[0].len(), 4);
    state.seek_to(6000).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    let actual = state.render_chunk(CHUNK_FRAMES);
    let mut reference = RenderState::new(8000);
    reference.load_project(&project, PathBuf::new()).unwrap();
    reference.seek_to(6000).unwrap();
    while reference.seek_target.is_some() {
        reference.preroll_chunk();
    }
    assert_eq!(actual, reference.render_chunk(CHUNK_FRAMES));
    assert!(
        actual.iter().any(|sample| sample.abs() > 0.001),
        "sustain history must retain released-but-pedalled note"
    );
}

#[test]
fn unmuting_a_clip_only_track_keeps_the_clip() {
    use super::*;
    let root = clip_temp_dir("unmute");
    clip_media(&root);
    let mut project = clip_stream_project(&root);
    project.tracks[0].mix.mute = true;
    let mut state = RenderState::new(48_000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert!(state.renderers.is_empty(), "muted lane is pruned");

    let mut unmuted = project.clone();
    unmuted.tracks[0].mix.mute = false;
    let outcome = apply_mixer_inline(&mut state, &unmuted);
    assert!(matches!(outcome, MixerOutcome::NeedsSources { .. }));
    assert!(state.renderers[0].is_silent());

    // Seek to the clip and confirm the patched lane plays it exactly once.
    let lane_start = state.clip_lanes.get(0).unwrap().start_frame();
    state.seek_to(lane_start).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    let patched = state.render_chunk(CHUNK_FRAMES);

    let mut fresh = RenderState::new(48_000);
    fresh.load_project(&unmuted, PathBuf::new()).unwrap();
    fresh.seek_to(lane_start).unwrap();
    while fresh.seek_target.is_some() {
        fresh.preroll_chunk();
    }
    assert_eq!(patched, fresh.render_chunk(CHUNK_FRAMES));
    assert!(patched.iter().any(|sample| sample.abs() > 0.001));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pause_intent_silences_callback_even_if_queued_play_sets_active() {
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let mut initial = [0.0; 128];
    crate::audio::streaming_audio_callback(&mut initial, &handle.playback);
    assert!(handle.wait_ready(std::time::Duration::from_secs(5)));
    for command in [RenderCommand::Pause, RenderCommand::Stop] {
        handle.send(command);
        assert!(handle.playback.output_suspended.load(Ordering::Acquire));
        // A previously queued Play may finish after the newer pause intent.
        handle.playback.active.store(true, Ordering::Release);
        let before = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        let mut output = [1.0; 128];
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert_eq!(
            handle.playback.play_cursor_frames.load(Ordering::Acquire),
            before
        );
    }
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(!handle.playback.output_suspended.load(Ordering::Acquire));
}

#[test]
fn mixer_change_patches_graph_without_reloading_sources() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    let loads = handle.playback.project_loads.load(Ordering::Acquire);
    let instruments = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert_eq!(loads, 1);
    assert_eq!(instruments, 1);

    let mut muted = tone_project();
    muted.tracks[0].mix.mute = true;
    muted.tracks[0].mix.volume_db = -9.0;
    handle.send(RenderCommand::MixerChanged {
        requested_ns: crate::first_block::now_ns(),
        project: Box::new(muted),
        soundfont_path: PathBuf::new(),
    });
    std::thread::sleep(std::time::Duration::from_millis(50));

    assert_eq!(
        handle.playback.project_loads.load(Ordering::Acquire),
        loads,
        "a mixer edit must not reload the project"
    );
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments,
        "a mixer edit must not instantiate instruments"
    );
}

/// A tone through one gain insert, so a chain parameter is the only thing
/// between the source and the mix.
fn tone_through_gain(factor: f64) -> Project {
    oc_dsl::parse(&format!(
            "@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.4, waveform: 0}}\n@track lead | preset: tone, instrument: synth/lead\n  effects:\n    - gain: factor={factor}\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n"
        ))
        .unwrap()
}

fn wait_chain_applied(handle: &RenderThreadHandle, count: u64) {
    let start = std::time::Instant::now();
    while handle.playback.chain_applied.load(Ordering::Acquire) < count {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "the render thread never applied the chain document"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn chain_change_patches_the_graph_without_reloading_sources() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_through_gain(1.0)),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(5)));
    let loads = handle.playback.project_loads.load(Ordering::Acquire);
    let instruments = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    let generation = handle.playback.seek_generation.load(Ordering::Acquire);
    let cursor = handle.playback.play_cursor_frames.load(Ordering::Acquire);
    let total = handle.playback.total_frames.load(Ordering::Acquire);
    let voices = handle.playback.sounding_voices.load(Ordering::Acquire);
    assert_eq!(instruments, 1);
    assert_eq!(voices, 1, "the tone is sounding before the edit");

    handle.send(RenderCommand::ChainChanged {
        project: Box::new(tone_through_gain(0.25)),
        soundfont_path: PathBuf::new(),
    });
    wait_chain_applied(&handle, 1);

    assert_eq!(
        handle.playback.project_loads.load(Ordering::Acquire),
        loads,
        "a chain edit must not reload the project"
    );
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments,
        "a chain edit must not instantiate an instrument"
    );
    assert_eq!(
        handle.playback.seek_generation.load(Ordering::Acquire),
        generation,
        "a chain edit must not invalidate the ring"
    );
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        cursor,
        "a chain edit must not move the playhead"
    );
    assert_eq!(
        handle.playback.total_frames.load(Ordering::Acquire),
        total,
        "a chain edit must not change the project length"
    );
    assert_eq!(
        handle.playback.sounding_voices.load(Ordering::Acquire),
        voices,
        "a chain edit must not drop a sounding voice"
    );
    assert_eq!(handle.playback.chain_patches.load(Ordering::Acquire), 1);
}

#[test]
fn a_chain_change_while_stopped_is_heard_on_the_next_play() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_through_gain(1.0)),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&handle);

    // Stopped: the warm ring already holds audio rendered with factor 1.0.
    handle.send(RenderCommand::ChainChanged {
        project: Box::new(tone_through_gain(0.25)),
        soundfont_path: PathBuf::new(),
    });
    wait_chain_applied(&handle, 1);

    let out = play_and_drain(&handle);
    let peak = out
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    // 0.4 amplitude, a 0.25 gain and the centre pan law's 1/sqrt(2) per side.
    let expected = 0.4 * 0.25 / 2.0_f32.sqrt();
    assert!(
        (peak - expected).abs() < 0.005,
        "the next play must render with the patched chain: peak {peak}"
    );
}

#[test]
fn stream_that_never_acks_cannot_wedge_the_producer() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(
        handle.wait_ready(std::time::Duration::from_secs(2)),
        "producer waited forever for a callback that never came"
    );
}

fn wait_for_warm(handle: &RenderThreadHandle) {
    let start = std::time::Instant::now();
    while handle.playback.warm_state.load(Ordering::Acquire) != WARM_READY {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "warm-up timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Play a warmed handle and drain the ring once the producer has filled it.
fn play_and_drain(handle: &RenderThreadHandle) -> Vec<f32> {
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(5)));
    // No callback acks in this test; the producer publishes after ACK_TIMEOUT.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // SAFETY: this fixture opens no output stream; the test thread exclusively
    // consumes this live handle's ring while the worker owns its producer end.
    let consumer = unsafe { &mut *handle.playback.consumer_ptr() };
    let slots = consumer.slots();
    let mut out = vec![0.0; slots];
    if slots > 0
        && let Ok(chunk) = consumer.read_chunk(slots)
    {
        let (first, second) = chunk.as_slices();
        out[..first.len()].copy_from_slice(first);
        out[first.len()..].copy_from_slice(second);
        chunk.commit_all();
    }
    out
}

fn ring_slots(handle: &RenderThreadHandle) -> usize {
    // SAFETY: callers use a manual test-thread consumer without a device
    // callback; the live handle retains the ring and its distinct producer end.
    unsafe { (*handle.playback.consumer_ptr()).slots() }
}

fn two_track_project(tempo: f64) -> Project {
    oc_dsl::parse(&format!(
            "@project\n  tempo: {tempo}\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{osc.a.waveform: 2, output.gain: 0.2}}\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n@track pad | preset: tone, instrument: synth/lead\n  @phrase q [1:1 - 5:1]\n    C3 w mf\n"
        ))
        .unwrap()
}

#[test]
fn warm_up_builds_sources_without_starting_playback() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&handle);
    assert!(
        !handle.playback.active.load(Ordering::Acquire),
        "warm-up must not start playback"
    );
    assert_eq!(handle.playback.play_applied.load(Ordering::Acquire), 0);
    assert_eq!(
        ring_slots(&handle),
        0,
        "warm-up stages audio, it does not fill the ring before play"
    );
    let loads = handle.playback.project_loads.load(Ordering::Acquire);
    let instruments = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert_eq!(loads, 1);
    assert_eq!(instruments, 1);

    // The press publishes the staged chunks and continues without rebuilding.
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    assert!(handle.playback.active.load(Ordering::Acquire));
    assert_eq!(handle.playback.project_loads.load(Ordering::Acquire), loads);
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments
    );
    assert!(
        ring_slots(&handle) > 0,
        "the staged warm audio must reach the ring after play"
    );
}

#[test]
fn a_warm_opening_is_replaced_by_the_armed_count_in() {
    let handle = RenderThreadHandle::spawn(8_000);
    let project = tone_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let plan = oc_audio::click::count_in_plan(&clock, 1, 8_000, 0.0).unwrap();
    let frames = plan.frames;
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    wait_for_warm(&handle);
    let pb = &handle.playback;
    let mut output = [0.0; 512];
    crate::audio::streaming_audio_callback(&mut output, pb);
    let generation = pb.seek_generation.load(Ordering::Acquire);
    handle.send(RenderCommand::CountIn { plan });
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while pb.play_applied.load(Ordering::Acquire) == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(pb.seek_generation.load(Ordering::Acquire) > generation);
    assert_eq!(pb.count_in_remaining.load(Ordering::Acquire), frames);
}

#[test]
fn playing_from_a_seeked_frame_reuses_the_warm_sources() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&handle);
    let loads = handle.playback.project_loads.load(Ordering::Acquire);
    let instruments = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);

    // A forward seek is covered by the staged history: no rebuild.
    handle.send(RenderCommand::Play {
        start_frame: 4000,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        4000
    );
    assert_eq!(handle.playback.project_loads.load(Ordering::Acquire), loads);
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments
    );
}

#[test]
fn press_during_warm_up_does_not_rebuild_the_project() {
    use super::*;
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(tone_project()),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    // Press before the build has necessarily finished: Play queues behind it.
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    // Give a (wrong) second build a chance to surface.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(
        handle.playback.project_loads.load(Ordering::Acquire),
        1,
        "a press must wait for the in-flight build, not start another"
    );
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        1
    );
    assert!(handle.playback.active.load(Ordering::Acquire));
}

#[test]
fn a_project_change_replaces_the_warm_audio() {
    use super::*;
    let first = tone_project();
    let second = two_track_project(90.0);
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(first.clone()),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    // Replace while the first fill may still be running.
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(second.clone()),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&handle);
    assert_eq!(
        handle.playback.project_loads.load(Ordering::Acquire),
        2,
        "the replacement is a single rebuild"
    );
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        1 + 2,
        "one source for the first project, two for the second"
    );
    assert_eq!(
        ring_slots(&handle),
        0,
        "no previous project's audio may be left queued"
    );
    let kept = play_and_drain(&handle);
    assert!(kept.iter().any(|sample| sample.abs() > 0.001));

    // A fresh warm-up of the second project must sound identical; a stale
    // first-project prefix would break byte equality.
    let fresh = RenderThreadHandle::spawn(8000);
    fresh.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(second),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    fresh
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&fresh);
    assert_eq!(kept, play_and_drain(&fresh));

    let only_first = RenderThreadHandle::spawn(8000);
    only_first.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(first),
        soundfont_path: PathBuf::new(),
        warm: true,
    });
    only_first
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    wait_for_warm(&only_first);
    assert_ne!(
        kept,
        play_and_drain(&only_first),
        "the replacement must not keep the previous project's audio"
    );
}

#[test]
fn unmuting_a_pruned_track_defers_the_build_to_the_source_worker() {
    use super::*;
    let project = two_lane_project(true);
    let mut state = RenderState::new(8000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    assert_eq!(state.source_tracks, vec![0], "muted track is pruned");
    assert_eq!(state.renderers.len(), 1);
    let instantiations = state.instrument_instantiations.load(Ordering::Acquire);

    let mut unmuted = project.clone();
    unmuted.tracks[1].mix.mute = false;
    let outcome = state
        .apply_mixer(&unmuted, std::path::PathBuf::new())
        .unwrap();
    assert!(matches!(outcome, MixerOutcome::NeedsSources { ref tracks } if tracks == &[1]));
    assert_eq!(
        state.renderers.len(),
        1,
        "the render state must not build the hot lane"
    );
    assert_eq!(
        state.instrument_instantiations.load(Ordering::Acquire),
        instantiations,
        "no instrument may be instantiated on the render thread"
    );

    // A repeated mixer edit must not queue a second build for the same lane.
    let repeated = state
        .apply_mixer(&unmuted, std::path::PathBuf::new())
        .unwrap();
    assert!(matches!(repeated, MixerOutcome::Patched));

    // The worker's lanes are spliced in without rebuilding the other lane.
    let resolved = state.project.clone().unwrap();
    let lanes = build_lanes(
        &resolved,
        &[1],
        std::path::Path::new(""),
        8000,
        &state.instrument_instantiations,
        state.root.as_deref(),
        state.worker_options,
    )
    .unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].target, 1);
    state.splice_sources(lanes).unwrap();
    assert_eq!(state.source_tracks, vec![0, 1]);
    assert_eq!(state.renderers.len(), 2);
    assert_eq!(
        state.instrument_instantiations.load(Ordering::Acquire),
        instantiations + 1,
        "only the pruned track's source is built"
    );
    assert!(state.pending_sources.is_empty());

    let patched = state.render_chunk(super::CHUNK_FRAMES);
    let mut fresh = RenderState::new(8000);
    fresh
        .load_project(&unmuted, std::path::PathBuf::new())
        .unwrap();
    assert_eq!(patched, fresh.render_chunk(super::CHUNK_FRAMES));
    assert!(patched.iter().any(|sample| sample.abs() > 0.001));
}

#[test]
fn unmuting_a_pruned_track_does_not_underrun_the_callback() {
    use super::*;
    let sample_rate = 48_000;
    let handle = RenderThreadHandle::spawn(sample_rate);
    let project = oc_dsl::parse("@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 9:1]\n    C4 w mf\n@track pad | preset: tone, instrument: synth/lead\n  mix: mute=true\n  @phrase p [1:1 - 9:1]\n    C3 w mf\n").unwrap();
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project.clone()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(handle.playback.active_sources.load(Ordering::Acquire), 1);

    // Run the production callback at realtime before the press so the seek
    // generation is acknowledged without flushing already-queued audio.
    let stop = Arc::new(AtomicBool::new(false));
    let halted = Arc::clone(&stop);
    let playback = Arc::clone(&handle.playback);
    let consumer = std::thread::spawn(move || {
        let mut block = [0.0f32; 256];
        while !halted.load(Ordering::Acquire) {
            crate::audio::streaming_audio_callback(&mut block, &playback);
            std::thread::sleep(std::time::Duration::from_secs_f64(
                256.0 / sample_rate as f64,
            ));
        }
    });
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(10)));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        handle.playback.underruns.load(Ordering::Acquire),
        0,
        "steady playback underran before the unmute"
    );

    // 1.5 s is well past the 250 ms lookahead: building this lane on the
    // producer would drain the ring long before the splice.
    handle.test_build_delay_ms.store(1500, Ordering::Release);
    let mut unmuted = project.clone();
    unmuted.tracks[1].mix.mute = false;
    handle.send(RenderCommand::MixerChanged {
        requested_ns: crate::first_block::now_ns(),
        project: Box::new(unmuted),
        soundfont_path: PathBuf::new(),
    });
    std::thread::sleep(std::time::Duration::from_millis(900));
    assert_eq!(
        handle.playback.active_sources.load(Ordering::Acquire),
        1,
        "the pruned lane was built on the render thread"
    );
    assert_eq!(
        handle.playback.underruns.load(Ordering::Acquire),
        0,
        "the callback underran while the source worker built the lane"
    );

    let deadline = std::time::Instant::now();
    while handle.playback.active_sources.load(Ordering::Acquire) < 2 {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(15),
            "the source worker never delivered the lane"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        2,
        "only the pruned lane was built"
    );
    assert_eq!(
        handle.playback.underruns.load(Ordering::Acquire),
        0,
        "the callback underran after the splice"
    );

    stop.store(true, Ordering::Release);
    consumer.join().unwrap();
}

#[test]
fn patterns_on_one_track_have_independent_sources_and_sum_into_mixer() {
    use oc_dsl::ast::*;
    let mut project = oc_dsl::parse("@project\n@preset a\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.waveform: 0, output.gain: 0.2}\n@preset b\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.waveform: 2, output.gain: 0.2}\n@track t | preset: a, instrument: synth/lead\n  @phrase main [1:1 - 2:1]\n    @0 C4 q mf\n").unwrap();
    let phrase = project.tracks[0].phrases.remove(0);
    let instrument = project.tracks[0].instrument.clone();
    for name in ["a", "b"] {
        project.tracks[0].inline_patterns.push(PatternDef {
            name: name.into(),
            instrument: instrument.clone(),
            preset: Some(name.into()),
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: vec![],
            events: phrase.events.clone(),
            pedals: phrase.pedals.clone(),
            hairpins: Vec::new(),
            hits: vec![],
            color: None,
        });
    }
    let mut state = super::RenderState::new(8000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    assert_eq!(state.renderers.len(), 2);
    assert_eq!(state.source_tracks, vec![0, 0]);
    let both = state.render_chunk(super::CHUNK_FRAMES);
    let mut solo_outputs = Vec::new();
    for index in 0..2 {
        let mut solo = project.clone();
        solo.tracks[0].inline_patterns = vec![project.tracks[0].inline_patterns[index].clone()];
        let mut state = super::RenderState::new(8000);
        state
            .load_project(&solo, std::path::PathBuf::new())
            .unwrap();
        solo_outputs.push(state.render_chunk(super::CHUNK_FRAMES));
    }
    assert_ne!(solo_outputs[0], solo_outputs[1]);
    let error = both
        .iter()
        .zip(&solo_outputs[0])
        .zip(&solo_outputs[1])
        .map(|((a, b), c)| (a - b - c).abs())
        .fold(0.0_f32, f32::max);
    assert!(error < 1e-6, "source sum {error}");
}

#[test]
#[ignore = "requires installed Dexed CLAP"]
fn dexed_independent_block_source_smoke() {
    let source = "@project\n  tempo: 120\n@preset p\n  source: clap\n  plugin: \"com.digital-suburban.dexed\"\n@track lead | preset: p, instrument: synth/lead\n  @phrase main [1:1 - 2:1]\n    @0 C4 q mf\n";
    let source = if std::env::var("OCP_BLOCK_FORMAT").as_deref() == Ok("vst3") {
        source
            .replace("source: clap", "source: vst3")
            .replace("com.digital-suburban.dexed", "Dexed")
    } else {
        source.into()
    };
    let project = oc_dsl::parse(&source).unwrap();
    let started = std::time::Instant::now();
    let mut state = super::RenderState::new(48000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let first = state.render_chunk(super::CHUNK_FRAMES);
    eprintln!(
        "Dexed first block ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(first.len(), super::CHUNK_FRAMES * 2);
    assert!(first.iter().all(|s| s.is_finite()));
    assert!(first.iter().any(|s| s.abs() > 0.001));
    assert_eq!(state.render_cursor, super::CHUNK_FRAMES as u64);
    let mut silent = project.clone();
    silent.tracks[0].phrases.clear();
    let mut independent = super::RenderState::new(48000);
    independent
        .load_project(&silent, std::path::PathBuf::new())
        .unwrap();
    assert!(
        independent
            .render_chunk(super::CHUNK_FRAMES)
            .iter()
            .all(|s| s.abs() < 1e-6)
    );
}

#[test]
#[ignore = "requires OCP_STREAM_PROJECT; profiles actual source startup"]
fn review_project_startup_profile() {
    let path = std::path::PathBuf::from(std::env::var("OCP_STREAM_PROJECT").unwrap());
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let project = oc_engine::block_source::prepare_project(&project, path.parent()).unwrap();
    let mut timings = Vec::new();
    for trial in 0..3 {
        let mut state = super::RenderState::new(48000);
        let start = std::time::Instant::now();
        state
            .load_project(&project, std::path::PathBuf::new())
            .unwrap();
        let loaded = start.elapsed().as_secs_f64();
        let audio = state.render_chunk(super::CHUNK_FRAMES);
        let first = start.elapsed().as_secs_f64();
        assert_eq!(audio.len(), super::CHUNK_FRAMES * 2);
        assert!(audio.iter().all(|sample| sample.is_finite()));
        eprintln!(
            "startup_trial={trial} load_ms={:.3} block_ms={:.3} first_ms={:.3}",
            loaded * 1000.0,
            (first - loaded) * 1000.0,
            first * 1000.0
        );
        timings.push(first);
    }
    assert!(
        timings.iter().all(|seconds| *seconds < 2.0),
        "startup gate: {timings:?}"
    );
}

#[test]
#[ignore = "requires OCP_STREAM_PROJECT and OCP_TEST_PLUGINS=1 for installed plug-ins"]
fn native_review_stream_latency_and_replay() {
    let path = std::env::var("OCP_STREAM_PROJECT").unwrap();
    let path = std::path::Path::new(&path).canonicalize().unwrap();
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut project = oc_engine::block_source::prepare_project(&project, path.parent()).unwrap();
    if let Ok(track) = std::env::var("OCP_STREAM_TRACK") {
        project.tracks.retain(|t| t.name == track);
        assert_eq!(
            project.tracks.len(),
            1,
            "requested diagnostic track must exist"
        );
    }
    let start = std::time::Instant::now();
    let mut state = super::RenderState::new(48000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let mut audio = state.render_chunk(super::FIRST_CHUNK_FRAMES);
    let first_audio_seconds = start.elapsed().as_secs_f64();
    eprintln!(
        "first_audio_ms={:.2}, rendered_frames={}, total_frames={}",
        first_audio_seconds * 1000.0,
        state.render_cursor,
        state.total_frames
    );
    assert_eq!(state.render_cursor, super::FIRST_CHUNK_FRAMES as u64);
    while state.render_cursor < state.playback_end() {
        let block = state.render_chunk(super::CHUNK_FRAMES);
        assert!(
            !block.is_empty(),
            "render stalled: {:?}",
            state.render_error
        );
        audio.extend(block);
    }
    eprintln!(
        "render_seconds={:.3}, audio_seconds={:.3}",
        start.elapsed().as_secs_f64(),
        audio.len() as f64 / 96000.0
    );
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache
        .set_minimum_duration(crate::project_extent::section_seconds(&project).unwrap())
        .unwrap();
    // Offline CLAP rendering expects caller-owned instances, as the CLI does.
    let mut plugins = std::collections::HashMap::new();
    let resolved = oc_dsl::preset::resolve(&project).unwrap();
    for preset in &resolved.presets {
        if preset.source.as_deref() == Some("clap") {
            let id = preset.plugin.as_deref().unwrap();
            if id.starts_with("builtin.") {
                continue;
            }
            let info = oc_host::find_plugin_info(id, oc_host::PluginFormat::Clap).unwrap();
            let instance = oc_host::clap_instance::ClapInstance::load(
                std::path::Path::new(&info.path),
                &info.id,
                48000,
                512,
            )
            .unwrap();
            plugins.insert(preset.name.clone(), instance);
        }
    }
    let expected = cache
        .render_incremental(&project, std::path::Path::new(""), 48000, &mut plugins)
        .unwrap();
    let master_latency = state
        .graph
        .as_ref()
        .map_or(0, |graph| graph.master_chain.latency_samples());
    let aligned = &audio[master_latency as usize * 2..];
    assert_eq!(aligned.len(), expected.len());
    assert!(!audio.is_empty());
    assert!(
        audio
            .iter()
            .chain(&expected)
            .all(|sample| sample.is_finite()),
        "non-finite samples cannot pass a max-error comparison"
    );
    assert!(
        audio.iter().any(|sample| sample.abs() > 0.0001),
        "silent review project"
    );
    let error = aligned
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    eprintln!("offline_max_error={error}");
    if error >= 1e-5 && std::env::var_os("OCP_STREAM_REPEAT_DIAGNOSTIC").is_some() {
        let mut repeat_cache = oc_engine::render_cache::RenderCache::new();
        repeat_cache
            .set_minimum_duration(crate::project_extent::section_seconds(&project).unwrap())
            .unwrap();
        let repeated = repeat_cache
            .render_incremental(&project, std::path::Path::new(""), 48000, &mut plugins)
            .unwrap();
        let repeat_error = repeated
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        eprintln!("independent_offline_repeat_max_error={repeat_error}");
        if let Ok(directory) = std::env::var("OCP_STREAM_REPEAT_OUTPUT") {
            use std::io::Write;
            let directory = std::path::Path::new(&directory);
            for (name, samples) in [
                ("stream", aligned),
                ("offline_a", expected.as_slice()),
                ("offline_b", repeated.as_slice()),
            ] {
                assert!(samples.iter().all(|sample| sample.is_finite()));
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(directory.join(format!("{name}.f32le")))
                    .unwrap();
                let mut writer = std::io::BufWriter::new(file);
                for sample in samples {
                    writer.write_all(&sample.to_le_bytes()).unwrap();
                }
                writer.flush().unwrap();
            }
        }
    }
    assert!(error < 1e-5, "offline parity {error}");
    // Optional evidence for checking the actual delivered PCM16 artifact.
    // Match CLI quantization, but export samples rendered by the GUI path.
    if let Ok(path) = std::env::var("OCP_STREAM_PCM") {
        use std::io::Write;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        let mut writer = std::io::BufWriter::new(file);
        for &sample in &audio {
            assert!(sample.is_finite());
            let value = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
            writer.write_all(&value.to_le_bytes()).unwrap();
        }
        writer.flush().unwrap();
        eprintln!("delivered_pcm_samples={}", audio.len());
    }
    // Early-only seeks miss restoration of automation and released voices
    // accumulated later in a complete arrangement.
    for frame in [
        12345,
        state.total_frames / 2,
        state.total_frames * 3 / 4,
        0,
        8193,
    ] {
        state.seek_to(frame).unwrap();
        while state.seek_target.is_some() {
            state.preroll_chunk();
        }
        let actual = state.render_chunk(super::CHUNK_FRAMES);
        let expected_samples =
            ((state.playback_end() - frame) as usize).min(super::CHUNK_FRAMES) * 2;
        assert_eq!(
            actual.len(),
            expected_samples,
            "seek returned incomplete audio"
        );
        assert!(actual.iter().all(|sample| sample.is_finite()));
        let error = actual
            .iter()
            .zip(&audio[frame as usize * 2..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        eprintln!("seek_frame={frame}, max_error={error}");
        assert!(error < 1e-5, "seek parity {error}");
    }
    assert!(
        first_audio_seconds < 2.0,
        "first audio latency {first_audio_seconds}s"
    );
}

#[test]
fn streaming_source_controls_do_not_duplicate_mixer_gain_and_pan() {
    let project = oc_dsl::parse("@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track t | preset: p, instrument: synth/lead\n  mix: vol=-9dB, pan=-0.5\n  @phrase p [1:1 - 2:1]\n    @0 C4 q v=90\n").unwrap();
    let mut state = super::RenderState::new(48000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    for controller in [7, 10] {
        let values: Vec<_> = state
            .events
            .iter()
            .filter_map(|event| match event.event {
                oc_engine::interpret::MidiMessage::ControlChange {
                    controller: cc,
                    value,
                    ..
                } if cc == controller => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![if controller == 7 { 100 } else { 64 }]);
    }
    assert_eq!(project.tracks[0].mix.volume_db, -9.0);
    assert_eq!(project.tracks[0].mix.pan, -0.5);
}

#[test]
fn native_first_block_is_bounded_and_matches_offline_without_soundfont() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.5, output.gain: 0.2}\n@track lead | preset: p, instrument: synth/lead\n  @phrase main [1:1 - 3:1]\n    @0 C4 q mf\n").unwrap();
    let started = std::time::Instant::now();
    let mut state = super::RenderState::new(8000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let first = state.render_chunk(super::CHUNK_FRAMES);
    assert_eq!(state.render_cursor, super::CHUNK_FRAMES as u64);
    assert!(state.total_frames > state.render_cursor);
    assert!(
        started.elapsed().as_secs_f64() < 2.0,
        "first block took {:?}",
        started.elapsed()
    );
    let events = oc_engine::interpret::try_interpret(
        &project,
        &oc_engine::interpret::InterpretSettings {
            sample_rate: 8000,
            ..Default::default()
        },
    )
    .unwrap();
    let refs = events.iter().collect::<Vec<_>>();
    // The streaming lane reads the same parameter list, so the reference
    // render carries the per-patch loudness trim too.
    let params = oc_engine::preset::builtin_track_params(
        &project,
        project.presets.first(),
        &project.tracks[0],
    );
    let mut expected = vec![oc_synth::synth::render_track_with_tempo(
        &refs,
        super::CHUNK_FRAMES,
        8000,
        Some(&params),
        120.0,
    )];
    let mut graph =
        oc_engine::graph::AudioGraph::try_from_project_with_source_latencies(&project, 8000, &[])
            .unwrap();
    let expected = graph.mix(&mut expected);
    let error = first
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(error < 1e-6, "block parity {error}");
    assert!(first.iter().any(|s| s.abs() > 0.001));
    let handle = super::RenderThreadHandle::spawn(8000);
    handle.send(super::RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: std::path::PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(2))
        .unwrap();
    handle.send(super::RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let deadline = std::time::Instant::now();
    while handle.playback.seek_generation.load(Ordering::Acquire) < 2 {
        assert!(deadline.elapsed().as_secs_f64() < 2.0);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let mut output = vec![0.0; 512];
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert_eq!(output, &first[..512]);
    handle.send(super::RenderCommand::Pause);
    let deadline = std::time::Instant::now();
    while handle.playback.active.load(Ordering::Acquire) {
        assert!(deadline.elapsed().as_secs_f64() < 2.0);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let generation = handle.playback.seek_generation.load(Ordering::Acquire);
    let position = handle.playback.play_cursor_frames.load(Ordering::Acquire);
    handle.send(super::RenderCommand::Play {
        start_frame: position,
        resume: true,
        end_frame: None,
    });
    let deadline = std::time::Instant::now();
    while !handle.playback.active.load(Ordering::Acquire) {
        assert!(deadline.elapsed().as_secs_f64() < 2.0);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        generation,
        handle.playback.seek_generation.load(Ordering::Acquire),
        "resume must retain queued audio"
    );
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert_eq!(output, &first[512..1024]);
}

fn seek_callback_fixture() -> oc_dsl::ast::Project {
    oc_dsl::parse("@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.waveform: 0, amp.attack: 0.001, amp.release: 0.01, output.gain: 0.4}\n@track t | preset: p, instrument: synth/lead_sine\n  @phrase p [1:1 - 5:1]\n    C3 w mf\n    C5 w mf\n    C3 w mf\n    C5 w mf\n").unwrap()
}

fn expected_seek_block(frame: u64) -> Vec<f32> {
    let mut state = super::RenderState::new(8000);
    state
        .load_project(&seek_callback_fixture(), std::path::PathBuf::new())
        .unwrap();
    state.seek_to(frame).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    state.render_chunk(128)
}

/// These cases check which position a press plays, so the seek preroll always
/// finishes: past its budget the destination starts with released voices, and
/// a slow runner would turn a timing difference into a different block.
fn exact_seek_preroll(handle: &super::RenderThreadHandle) {
    handle.send(super::RenderCommand::TestPrerollBudget(
        std::time::Duration::from_secs(3600),
    ));
}

fn consume_first_audible(handle: &super::RenderThreadHandle) -> Vec<f32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut out = vec![0.0; 256];
        crate::audio::streaming_audio_callback(&mut out, &handle.playback);
        if out.iter().any(|v| v.abs() > 1e-6) {
            return out;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stream never supplied requested audio"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn paused_seek_callback_case(from: u64, target: u64, warm: bool) {
    let handle = super::RenderThreadHandle::spawn(8000);
    exact_seek_preroll(&handle);
    handle.send(super::RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(seek_callback_fixture()),
        soundfont_path: Default::default(),
        warm,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    if warm {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while handle.playback.warm_state.load(Ordering::Acquire) != super::WARM_READY {
            crate::audio::streaming_audio_callback(&mut [0.0; 256], &handle.playback);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    handle.send(super::RenderCommand::Play {
        start_frame: from,
        resume: false,
        end_frame: None,
    });
    let _ = consume_first_audible(&handle);
    handle.send(super::RenderCommand::Pause);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while handle.playback.active.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Reproduce the real caller: UI requested position was stored before
    // Play(false), while the ring still contained the previous position.
    handle
        .playback
        .play_cursor_frames
        .store(target, Ordering::Release);
    handle.send(super::RenderCommand::Play {
        start_frame: target,
        resume: false,
        end_frame: None,
    });
    let out = consume_first_audible(&handle);
    assert_eq!(
        out,
        expected_seek_block(target),
        "callback consumed old-position samples after explicit paused seek"
    );
}

#[test]
fn immediate_play_after_pause_does_not_consume_stale_audio_before_worker_applies_seek() {
    let handle = super::RenderThreadHandle::spawn(8000);
    exact_seek_preroll(&handle);
    handle.send(super::RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(seek_callback_fixture()),
        soundfont_path: Default::default(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    handle.send(super::RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let _ = consume_first_audible(&handle);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    handle.send(super::RenderCommand::TestBarrier {
        entered: entered_tx,
        release: release_rx,
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    handle.send(super::RenderCommand::Pause);
    handle.send(super::RenderCommand::Play {
        start_frame: 16000,
        resume: false,
        end_frame: None,
    });
    let mut output = [0.0; 256];
    let cursor = handle.playback.play_cursor_frames.load(Ordering::Acquire);
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert!(
        output.iter().all(|v| *v == 0.0),
        "immediate Play leaked the previous ring before worker applied seek"
    );
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        cursor
    );
    release_tx.send(()).unwrap();
    assert_eq!(consume_first_audible(&handle), expected_seek_block(16000));
}

#[test]
fn paused_seek_zero_consumes_restarted_audio_not_old_ring() {
    paused_seek_callback_case(16000, 0, false);
}

#[test]
fn paused_seek_nonzero_consumes_requested_audio_not_old_ring() {
    paused_seek_callback_case(0, 16000, false);
}

#[test]
fn consumed_warm_ring_pause_seek_zero_restarts_the_opening() {
    paused_seek_callback_case(0, 0, true);
}

#[test]
fn stopped_warm_seek_later_consumes_requested_audio_not_opening_ring() {
    let handle = super::RenderThreadHandle::spawn(8000);
    exact_seek_preroll(&handle);
    handle.send(super::RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(seek_callback_fixture()),
        soundfont_path: Default::default(),
        warm: true,
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while handle.playback.warm_state.load(Ordering::Acquire) != super::WARM_READY {
        crate::audio::streaming_audio_callback(&mut [0.0; 256], &handle.playback);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    handle
        .playback
        .play_cursor_frames
        .store(16000, Ordering::Release);
    handle.send(super::RenderCommand::Play {
        start_frame: 16000,
        resume: false,
        end_frame: None,
    });
    assert_eq!(consume_first_audible(&handle), expected_seek_block(16000));
}

#[test]
fn native_curve_tail_export_and_seek_match() {
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset p
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.level: 0.5, output.gain: 0.2}
@track lead | preset: p, instrument: synth/lead
  @phrase main [1:1 - 3:1]
    @0 C4 q pitch_curve="0:0,2:50" pitch_curve_horizon=2
    @1 C4 q pitch_curve="0:0,2:-50" pitch_curve_horizon=2
"#,
    )
    .unwrap();
    let mut state = super::RenderState::new(8000);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let mut audio = Vec::new();
    while state.render_cursor < state.total_frames {
        let block = state.render_chunk(super::CHUNK_FRAMES);
        assert!(!block.is_empty(), "{:?}", state.render_error);
        audio.extend(block);
    }
    let mut cache = oc_engine::render_cache::RenderCache::new();
    let expected = cache
        .render_full(
            &project,
            std::path::Path::new(""),
            8000,
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
    assert_eq!(audio.len(), expected.len());
    let error = audio
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(error < 1e-6, "export parity {error}");
    for frame in [5000, 0, 9000] {
        state.seek_to(frame).unwrap();
        while state.seek_target.is_some() {
            state.preroll_chunk();
        }
        let actual = state.render_chunk(super::CHUNK_FRAMES);
        assert!(!actual.is_empty());
        let expected = &audio[frame as usize * 2..frame as usize * 2 + actual.len()];
        let error = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(error < 1e-6, "tail seek parity {error}");
    }
}

use super::*;

#[test]
fn streaming_preserves_silent_section_extent_and_resets_after_edit() {
    let mut project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
    crate::project_extent::tests::add_section(&mut project, 5);
    let mut state = RenderState::new(8000);
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.total_frames, 64000);
    state.seek_to(60000).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    assert_eq!(state.render_cursor, 60000);
    project.sections.clear();
    state.load_project(&project, PathBuf::new()).unwrap();
    assert_eq!(state.total_frames, 16000);
}

#[test]
#[ignore = "requires OCP_TEST_SOUNDFONT; renders real SoundFont audio without a device"]
fn soundfont_seek_and_restart_match_continuous_render() {
    let font =
        PathBuf::from(std::env::var_os("OCP_TEST_SOUNDFONT").expect("set OCP_TEST_SOUNDFONT"));
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
  tempo_map:
    1:3 = 60 bpm
@track a | instrument: piano/acoustic_grand
  automation: sustain {1:1=true, 1:4=false}
  automation: expression {1:1=100%, 1:3=40%, 2:1=80%}
  effects:
    - delay: time_l=125ms, time_r=75ms, feedback=30%, mix=20%
  @phrase a [1:1 - 3:1]
    C4 q mf
    E4 h f
    G4 h mp
@master
  gain_automation: linear [1:1 - 3:1] {1:1=0.4, 3:1=1}
"#,
    )
    .unwrap();
    let mut state = RenderState::new(8000);
    state.load_project(&project, font).unwrap();
    let mut continuous = Vec::new();
    while state.render_cursor < state.total_frames {
        continuous.extend(state.render_chunk(super::CHUNK_FRAMES));
    }
    assert!(continuous.iter().any(|sample| sample.abs() > 0.001));
    for frame in [12345, 4001, 0, 20000, 0] {
        state.seek_to(frame).unwrap();
        while state.seek_target.is_some() {
            let previous = state.render_cursor;
            state.preroll_chunk();
            assert!(state.render_cursor - previous <= CHUNK_FRAMES as u64);
        }
        let actual = state.render_chunk(super::CHUNK_FRAMES);
        let expected = &continuous[frame as usize * 2..frame as usize * 2 + actual.len()];
        let max_error = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_error < 1e-6, "seek {frame}: max error {max_error}");
    }
    // A newer seek cancels an unfinished preroll instead of completing it.
    state.seek_to(20000).unwrap();
    state.preroll_chunk();
    assert!(state.seek_target.is_some());
    state.seek_to(0).unwrap();
    assert!(state.seek_target.is_none());
    assert_eq!(state.render_cursor, 0);
    assert_eq!(state.track_event_cursors, vec![0]);

    // Exercise the real producer/callback generation handshake without a
    // physical audio device. Fresh audio must survive the stale-data flush.
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: state.soundfont_path.clone(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(2))
        .unwrap();
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let start = std::time::Instant::now();
    while !handle.playback.active.load(Ordering::Acquire)
        || handle.playback.seek_generation.load(Ordering::Acquire) < 2
    {
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!handle.playback.ready.load(Ordering::Acquire));
    let mut output = vec![1.0; 512];
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert!(output.iter().all(|v| *v == 0.0));
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert_eq!(output, continuous[..512]);
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        256
    );
    let previous_generation = handle.playback.seek_generation.load(Ordering::Acquire);
    handle.send(RenderCommand::Seek { frame: 4001 });
    let start = std::time::Instant::now();
    while handle.playback.seek_generation.load(Ordering::Acquire) == previous_generation {
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    assert!(output.iter().all(|v| *v == 0.0));
    assert!(handle.wait_ready(std::time::Duration::from_secs(2)));
    crate::audio::streaming_audio_callback(&mut output, &handle.playback);
    // The seek cut the audio, so the fresh block fades in over the resume
    // ramp and matches the continuous render from the end of it.
    let fade = crate::audio::RESUME_FADE_FRAMES as usize * 2;
    assert!(
        output[..fade].iter().all(|value| value.abs() < 0.05),
        "the resume ramp starts near silence"
    );
    assert_eq!(output[fade..], continuous[8002 + fade..8514]);
    assert_eq!(
        handle.playback.play_cursor_frames.load(Ordering::Acquire),
        4257
    );
}

#[test]
fn failed_load_is_reported_and_thread_accepts_a_corrected_project() {
    let handle = RenderThreadHandle::spawn(8000);
    let mut project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
    project.properties.tempo = 0.0;
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project.clone()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    let error = handle
        .wait_project_loaded(std::time::Duration::from_secs(2))
        .unwrap_err();
    assert!(!error.contains("timed out"), "{error}");
    assert!(!handle.playback.active.load(Ordering::Acquire));
    assert!(!handle.playback.project_loaded.load(Ordering::Acquire));
    project.properties.tempo = 120.0;
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(2))
        .unwrap();
}

#[test]
fn a_pattern_preset_that_does_not_exist_fails_the_load() {
    let mut state = RenderState::new(8000);
    let project = oc_dsl::parse(
            "@project\n@preset keys\n  source: builtin\n\
             @pattern riff | preset: missing_patch, instrument: synth/lead_square\n  events:\n    @0 C4 w mf\n\
             @track lead | preset: keys, instrument: synth/lead_square\n  clip: riff [1:1 - 2:1]\n",
        )
        .unwrap();
    let error = state
        .load_project(&project, PathBuf::new())
        .expect_err("the unknown pattern preset must refuse the load");
    assert!(error.contains("riff"), "{error}");
    assert!(error.contains("missing_patch"), "{error}");
    assert_eq!(state.num_tracks, 0, "a refused load commits nothing");
}

#[test]
fn failed_replacement_never_commits_partial_render_state() {
    let mut state = RenderState::new(8000);
    let empty = oc_dsl::parse("@project\n").unwrap();
    state.load_project(&empty, PathBuf::new()).unwrap();
    let duration = state.total_frames;
    let missing = oc_dsl::parse("@project\n@track a | instrument: sample/take\n").unwrap();
    assert!(
        state
            .load_project(&missing, PathBuf::from("/nonexistent/pgen-test.sf2"))
            .is_err()
    );
    assert_eq!(state.num_tracks, 0);
    assert_eq!(state.total_frames, duration);
    let effect = oc_dsl::parse(
        "@project\n@master\n  effects:\n    - plugin:vst3.pgen_missing_effect_for_test: mix=1\n",
    )
    .unwrap();
    let error = state.load_project(&effect, PathBuf::new()).unwrap_err();
    assert!(error.contains("refusing bypassed render"), "{error}");
    assert_eq!(state.total_frames, duration);
}

#[test]
fn streaming_graph_seek_uses_absolute_mapped_automation_time() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n  tempo_map:\n    1:3 = 60 bpm\n\
             @track a | instrument: piano/acoustic_grand\n\
             @master\n  gain_automation: linear [1:1 - 3:1] {1:1=0, 3:1=1}\n",
    )
    .unwrap();
    let clock = Arc::new(ProjectClock::new(&project.properties).unwrap());
    let transport = PluginTransport::new(0, 8, 120.0).with_clock(clock);
    let mut whole = AudioGraph::try_from_project_with_source_latencies(&project, 8, &[]).unwrap();
    let expected = whole.mix_with_transport(&mut [vec![1.0; 128]], Some(transport.clone()));
    let mut state = RenderState::new(8);
    state.graph =
        Some(AudioGraph::try_from_project_with_source_latencies(&project, 8, &[]).unwrap());
    state.transport = Some(transport);
    state.total_frames = 64;
    for frame in [24, 4, 40, 0] {
        state.render_cursor = frame;
        state.dsp_cursor = frame;
        state.graph.as_mut().unwrap().reset();
        let actual = state.mix_chunk(&mut [vec![1.0; 16]], 8);
        assert_eq!(
            actual,
            expected[frame as usize * 2..frame as usize * 2 + 16]
        );
    }
}

// ========================================================================
// Live playhead latency: measurement harness and regression
// ========================================================================

const MEASURE_RATE: u32 = 48_000;
const MEASURE_BLOCK: usize = 256;

/// A bare tone's sixteenth note at bar 1 beat 3, so the transient has a
/// known project sample and the chain's shift is the only unknown.
fn click_project(track_effects: &str, master_effects: &str) -> Project {
    let source = format!(
        "@ocp \"0.2.0\"\n@project \"playhead click\"\n  tempo: 120\n@preset click\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.4, waveform: 0}}\n@track click | preset: click, instrument: synth/lead\n{track_effects}  @phrase p [1:1 - 3:1]\n    rest h\n    C4 16th fff\n{master_effects}"
    );
    oc_dsl::parse(&source).unwrap()
}

/// The click's project frame and the latency the live graph reports.
fn click_frame_and_latency(project: &Project) -> (u64, u64) {
    let settings = InterpretSettings {
        sample_rate: MEASURE_RATE,
        ..Default::default()
    };
    let events = interpret::try_interpret(project, &settings).unwrap();
    let frame = events
        .iter()
        .find(|event| matches!(event.event, MidiMessage::NoteOn { .. }))
        .map_or(0, |event| event.time_samples);
    let resolved = oc_dsl::preset::resolve(project).unwrap();
    let graph =
        AudioGraph::try_from_project_with_source_latencies(&resolved, MEASURE_RATE, &[]).unwrap();
    let latency = u64::from(graph.pre_master_latency_samples())
        + u64::from(graph.master_chain.latency_samples());
    (frame, latency)
}

/// One delivered device block: the raw cursor and the compensated position
/// after the read.
struct BlockTally {
    cursor_after: u64,
    audible_after: u64,
}

/// Play through the real worker with the test thread as the device, and
/// return the delivered stream plus one tally per committed block.
fn record_live(project: &Project, frames: usize) -> (Vec<f32>, Vec<BlockTally>) {
    record_live_from(project, None, frames, None)
}

/// The same run with the metronome on or off.
fn record_live_clicked(
    project: &Project,
    frames: usize,
    click: ClickSettings,
) -> (Vec<f32>, Vec<BlockTally>) {
    record_live_from(project, None, frames, Some(click))
}

/// The same run for a project whose relative assets need a directory.
fn record_live_from(
    project: &Project,
    root: Option<&std::path::Path>,
    frames: usize,
    click: Option<ClickSettings>,
) -> (Vec<f32>, Vec<BlockTally>) {
    let handle = RenderThreadHandle::spawn(MEASURE_RATE);
    handle.send(RenderCommand::ProjectChanged {
        root: root.map(std::path::Path::to_path_buf),
        project: Box::new(project.clone()),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(20))
        .unwrap();
    if let Some(settings) = click {
        handle.send(RenderCommand::Click { settings });
    }
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let playback = Arc::clone(&handle.playback);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut stream = Vec::with_capacity(frames * 2);
    let mut tallies = Vec::new();
    let mut block = vec![0.0f32; MEASURE_BLOCK * 2];
    while stream.len() < frames * 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "live render produced {} of {} frames",
            stream.len() / 2,
            frames
        );
        let before = playback.play_cursor_frames.load(Ordering::Acquire);
        crate::audio::streaming_audio_callback(&mut block, &playback);
        let after = playback.play_cursor_frames.load(Ordering::Acquire);
        let committed = (((after - before) as usize) * 2).min(block.len());
        stream.extend_from_slice(&block[..committed]);
        if committed > 0 {
            tallies.push(BlockTally {
                cursor_after: after,
                audible_after: playback.audible_frames(),
            });
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    (stream, tallies)
}

/// Offset of the picture against the sound for one project: the frame the
/// click first appears in the delivered stream, the raw cursor and the
/// compensated position at the moment that audio is heard.
struct ClickOffset {
    click_frame: u64,
    reported_chain: u64,
    device_frames: u64,
    /// Measured onset shift of the delivered audio, in frames.
    audio_shift: i64,
    /// Picture lead of the raw cursor when the click is heard.
    raw_lead: i64,
    /// Picture lead of the compensated position at the same instant.
    compensated_lead: i64,
}

fn measure_click(project: &Project) -> ClickOffset {
    let (click_frame, reported_chain) = click_frame_and_latency(project);
    let device = MEASURE_BLOCK as u64;
    let (stream, tallies) = record_live(project, click_frame as usize + MEASURE_RATE as usize);
    let heard = stream
        .iter()
        .position(|sample| sample.abs() > 1e-4)
        .map(|index| index / 2)
        .expect("the click is in the delivered stream") as u64;
    // The device plays this audio one callback period after the handoff.
    let heard_frame = heard + device;
    let tally = tallies
        .iter()
        .find(|tally| tally.cursor_after >= heard_frame)
        .expect("a block is delivered after the click");
    ClickOffset {
        click_frame,
        reported_chain,
        device_frames: device,
        audio_shift: heard as i64 - click_frame as i64,
        raw_lead: tally.cursor_after as i64 - click_frame as i64,
        compensated_lead: tally.audible_after as i64 - click_frame as i64,
    }
}

#[test]
#[ignore = "measures the live cursor-to-sound offset; run with --ignored --nocapture"]
fn review_cursor_to_sound_latency() {
    let cases = [
        ("builtin", click_project("", "")),
        (
            "master_limiter_20ms",
            click_project("", "@master\n  effects:\n    - limiter: lookahead=20ms\n"),
        ),
        (
            "track_tuner",
            click_project("  effects:\n    - tuner: amount=100%\n", ""),
        ),
        (
            "master_tuner_resonance",
            click_project(
                "",
                "@master\n  effects:\n    - tuner: amount=100%\n    - resonance: depth=12dB, mix=50%\n",
            ),
        ),
    ];
    for (name, project) in cases {
        let offset = measure_click(&project);
        let rate = f64::from(MEASURE_RATE);
        eprintln!(
            "[playhead] case={name} click_frame={} audio_shift_frames={} (audio_shift_ms={:.2}) reported_chain_frames={} device_frames={} raw_lead_frames={} (raw_lead_ms={:.2}) compensated_lead_frames={} (compensated_lead_ms={:.2}) monitor_frames={}",
            offset.click_frame,
            offset.audio_shift,
            offset.audio_shift as f64 * 1000.0 / rate,
            offset.reported_chain,
            offset.device_frames,
            offset.raw_lead,
            offset.raw_lead as f64 * 1000.0 / rate,
            offset.compensated_lead,
            offset.compensated_lead as f64 * 1000.0 / rate,
            offset.reported_chain + offset.device_frames
        );
        // The sound is shifted by exactly the reported chain latency, to
        // within the effect's grain phase and the one-sample onset.
        assert!(
            (offset.audio_shift - offset.reported_chain as i64).abs() <= 16,
            "{name}: audio shifted {} against reported {}",
            offset.audio_shift,
            offset.reported_chain
        );
        // The compensated picture never leads by more than one block.
        assert!(
            offset.compensated_lead.abs() <= MEASURE_BLOCK as i64,
            "{name}: compensated picture leads by {} frames",
            offset.compensated_lead
        );
    }
}

/// Meters describe the audio the device is playing, never the chunk the
/// producer rendered a lookahead early.
#[test]
fn meters_are_held_until_the_device_reaches_them() {
    let project = click_project("", "");
    let mut state = RenderState::new(MEASURE_RATE);
    let hub = Arc::new(MeterHub::new());
    state.meter_hub = Some(Arc::clone(&hub));
    state.load_project(&project, PathBuf::new()).unwrap();
    // Render past the click at frame 48_000; its chunk starts at 45_056.
    while state.render_cursor < 48_000 + CHUNK_FRAMES as u64 {
        assert!(!state.render_chunk(CHUNK_FRAMES).is_empty());
    }
    let block = MEASURE_BLOCK as u64;
    assert!(
        hub.snapshot(true).master.node.output_peak_db.is_none(),
        "meters were published before the device reached the audio"
    );
    // A cursor at the start of the timeline has released only silence.
    state.release_meters(block, block);
    assert!(hub.snapshot(true).master.node.output_peak_db.is_none());
    // Once the cursor reaches that chunk, its peak lands.
    state.release_meters(45_056 + block, block);
    let peak = hub
        .snapshot(true)
        .master
        .node
        .output_peak_db
        .expect("the click chunk's peak was released");
    // 0.4 peak through the centre pan law is 0.4 / sqrt(2).
    assert!((peak + 10.97).abs() < 0.1, "peak {peak} dB");
}

/// A muted channel publishes zero on every tap and clears its clip latch,
/// while a channel that is not silenced keeps metering its own level.
#[test]
fn a_muted_channel_publishes_zero_and_clears_its_clip() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n\
             @track hot | instrument: keys/piano\n\
             @track quiet | instrument: keys/piano\n",
    )
    .unwrap();
    let mut state = RenderState::new(MEASURE_RATE);
    let hub = Arc::new(MeterHub::new());
    state.meter_hub = Some(Arc::clone(&hub));
    hub.set_layout(2, &[], 2);
    state.graph = Some(
        AudioGraph::try_from_project_with_source_latencies(&project, MEASURE_RATE, &[]).unwrap(),
    );
    state.num_tracks = 2;
    state.meter_input_peaks = vec![0.0, 0.0];

    let hot = || vec![2.0_f32; CHUNK_FRAMES * 2];
    let quiet = || vec![0.25_f32; CHUNK_FRAMES * 2];
    let mut tracks = vec![hot(), quiet()];
    state.mix_chunk(&mut tracks, CHUNK_FRAMES);
    state.release_meters(1_000_000, 1);
    let published = hub.snapshot(true);
    assert!(
        published.tracks[0].node.clipped,
        "a full-scale chunk latches a clip"
    );
    assert!(published.tracks[0].node.output_peak_db.is_some());

    // Mute the hot channel; its live input still arrives, but nothing of
    // it reaches the mix.
    state.graph.as_mut().unwrap().routing[0].mute = true;
    let mut tracks = vec![hot(), quiet()];
    state.mix_chunk(&mut tracks, CHUNK_FRAMES);
    state.release_meters(2_000_000, 1);
    let published = hub.snapshot(true);
    let muted = &published.tracks[0].node;
    assert_eq!(muted.input_peak_db, None, "a muted channel meters zero");
    assert_eq!(muted.output_peak_db, None, "a muted channel meters zero");
    assert_eq!(muted.output_rms_db, None);
    assert!(!muted.clipped, "a muted channel clears its clip latch");
    assert!(
        published.tracks[1].node.output_peak_db.is_some(),
        "an unmuted channel still meters its own level"
    );
}

/// The mono check folds the monitored chunk and nothing else: the graph's
/// own mix is unchanged, so a render or an export cannot see it.
#[test]
fn the_mono_check_folds_the_monitored_chunk() {
    let project =
        oc_dsl::parse("@project\n  tempo: 120\n@track a | instrument: piano/acoustic_grand\n")
            .unwrap();
    let clock = Arc::new(ProjectClock::new(&project.properties).unwrap());
    let build = || AudioGraph::try_from_project_with_source_latencies(&project, 8, &[]).unwrap();
    let mut state = RenderState::new(8);
    state.graph = Some(build());
    state.transport = Some(PluginTransport::new(0, 8, 120.0).with_clock(Arc::clone(&clock)));
    state.num_tracks = 1;
    state.meter_input_peaks = vec![0.0];
    let input = vec![0.8, 0.2, 0.8, 0.2];
    let stereo = state.mix_chunk(&mut [input.clone()], 2);
    assert!(
        stereo.chunks_exact(2).all(|frame| frame[0] != frame[1]),
        "the stereo mix reaches the monitor unfolded: {stereo:?}"
    );
    // A fresh graph, same input: only the monitor fold differs.
    state.graph = Some(build());
    state.transport = Some(PluginTransport::new(0, 8, 120.0).with_clock(clock));
    state.mono_check = true;
    let mono = state.mix_chunk(&mut [input], 2);
    for (frame, folded) in stereo.chunks_exact(2).zip(mono.chunks_exact(2)) {
        let expected = 0.5 * (frame[0] + frame[1]);
        assert_eq!(
            folded,
            [expected, expected],
            "the monitor must carry the mono sum"
        );
    }
}

#[test]
fn fold_to_mono_keeps_a_correlated_pair_and_cancels_an_inverted_one() {
    let mut correlated = vec![0.5, 0.5, 0.25, 0.25];
    fold_to_mono(&mut correlated);
    assert_eq!(correlated, vec![0.5, 0.5, 0.25, 0.25]);
    let mut inverted = vec![0.5, -0.5, 0.25, -0.25];
    fold_to_mono(&mut inverted);
    assert_eq!(inverted, vec![0.0, 0.0, 0.0, 0.0]);
    let mut panned = vec![1.0, 0.0];
    fold_to_mono(&mut panned);
    assert_eq!(panned, vec![0.5, 0.5], "a hard-panned source drops 6 dB");
}

/// A seek drops the loudness window, so a reading never averages audio from
/// two places in the timeline.
#[test]
fn a_seek_drops_the_loudness_window() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
    let rate = 8_000u32;
    let mut state = RenderState::new(rate);
    state.load_project(&project, PathBuf::new()).unwrap();
    let tone: Vec<f32> = (0..rate as usize * 4)
        .flat_map(|index| {
            let value = 0.5 * (std::f32::consts::TAU * 1_000.0 * index as f32 / rate as f32).sin();
            [value, value]
        })
        .collect();
    state.loudness.push_interleaved(&tone);
    state.correlation.push(&tone);
    assert!(state.loudness.lufs().is_some(), "the window holds audio");
    assert!(state.correlation.value().is_some());
    state.seek_to(0).unwrap();
    assert_eq!(state.loudness.lufs(), None);
    assert_eq!(state.correlation.value(), None);
}

/// The compensation is one device block plus the reported chain latency.
#[test]
fn monitor_latency_is_the_device_block_plus_the_chain() {
    assert_eq!(super::DEVICE_LATENCY_BLOCKS, 1);
    assert_eq!(super::monitor_latency_frames(256, 0), 256);
    assert_eq!(super::monitor_latency_frames(256, 4_224), 4_480);
    assert_eq!(super::monitor_latency_frames(0, 1_920), 1_920);
}

/// The reported position is the ring cursor less the monitor latency, and
/// it saturates instead of wrapping before the first monitor window.
#[test]
fn audible_position_trails_the_ring_by_the_monitor_latency() {
    let handle = RenderThreadHandle::spawn(MEASURE_RATE);
    let playback = &handle.playback;
    playback.device_buffer_frames.store(256, Ordering::Release);
    playback
        .chain_latency_frames
        .store(4_224, Ordering::Release);
    playback.play_cursor_frames.store(4_480, Ordering::Release);
    assert_eq!(playback.monitor_latency_frames(), 4_480);
    assert_eq!(playback.audible_frames(), 0);
    playback.play_cursor_frames.store(52_480, Ordering::Release);
    assert_eq!(playback.audible_frames(), 48_000);
    playback.chain_latency_frames.store(0, Ordering::Release);
    playback.play_cursor_frames.store(1_000, Ordering::Release);
    assert_eq!(playback.audible_frames(), 744);
}

/// End to end: the live playhead does not lead the click that names the
/// moment, and the master chain's latency is what the position carries.
#[test]
fn live_playhead_does_not_lead_the_click() {
    // The tuner and the spectral suppressor both report latency, so the
    // graph's number is the whole modelling question here.
    let project = click_project(
        "",
        "@master\n  effects:\n    - tuner: amount=100%\n    - resonance: depth=12dB, mix=50%\n",
    );
    let offset = measure_click(&project);
    assert_eq!(offset.reported_chain, 4_224, "reported chain latency");
    assert_eq!(offset.device_frames, MEASURE_BLOCK as u64);
    assert!(
        (offset.audio_shift - 4_224).abs() <= 16,
        "the tuner and suppressor must delay the click by their reported latency, got {}",
        offset.audio_shift
    );
    assert!(
        offset.raw_lead >= 4_224 + MEASURE_BLOCK as i64,
        "the raw cursor must lead by the chain plus one device block, got {}",
        offset.raw_lead
    );
    // Block-stepped cursor: the residual is bounded by one device block.
    assert!(
        offset.compensated_lead.abs() <= MEASURE_BLOCK as i64,
        "compensated picture is {} frames off the click",
        offset.compensated_lead
    );
}

// ========================================================================
// Click and count-in
// ========================================================================

fn click_state() -> Box<super::RenderState> {
    use oc_audio::click::{ClickSettings, CountIn};
    let mut state = Box::new(super::RenderState::new(48_000));
    state.clock =
        Some(ProjectClock::new(&oc_dsl::ast::ProjectProperties::default()).expect("clock"));
    state.click_settings = ClickSettings {
        enabled: true,
        level_db: 0.0,
        count_in: CountIn::Off,
    };
    state.num_tracks = 1;
    state
}

/// Frames of a block that hold any non-zero sample.
fn loud_frames(block: &[f32]) -> Vec<usize> {
    block
        .chunks(2)
        .enumerate()
        .filter(|(_, frame)| frame.iter().any(|sample| *sample != 0.0))
        .map(|(index, _)| index)
        .collect()
}

#[test]
fn the_click_lands_on_the_beat_and_nowhere_else() {
    let mut state = click_state();
    let mut chunks = vec![vec![0.0f32; 4_800 * 2]];
    let mixed = state.mix_chunk(&mut chunks, 4_800);
    let hits = loud_frames(&mixed);
    assert!(!hits.is_empty(), "a click sounds at the bar line");
    // The click's first sample is a zero-crossing, so the first audible
    // frame is the one right after it.
    assert!(
        hits[0] <= 3,
        "the accented click starts on frame 0: {hits:?}"
    );
    assert!(
        hits.iter().all(|frame| *frame < 1_440),
        "a 30 ms click fits in 1440 frames at 48 kHz: {hits:?}"
    );

    // The next block starts on beat 2, half a second later.
    state.dsp_cursor = 24_000;
    let mut chunks = vec![vec![0.0f32; 4_800 * 2]];
    let mixed = state.mix_chunk(&mut chunks, 4_800);
    let hits = loud_frames(&mixed);
    assert!(
        hits.first().copied().is_some_and(|frame| frame <= 3),
        "beat 2 is on its own frame: {hits:?}"
    );
}

#[test]
fn the_bar_line_is_accented_and_an_off_beat_is_not() {
    let mut state = click_state();
    let mut bar = vec![vec![0.0f32; 2_000 * 2]];
    let accented = state.mix_chunk(&mut bar, 2_000);
    state.dsp_cursor = 24_000;
    let mut beat = vec![vec![0.0f32; 2_000 * 2]];
    let plain = state.mix_chunk(&mut beat, 2_000);
    assert_ne!(
        accented, plain,
        "the bar line sounds different from an off beat"
    );
}

#[test]
fn the_click_is_silent_until_it_is_switched_on() {
    let mut state = click_state();
    state.click_settings.enabled = false;
    let mut chunks = vec![vec![0.0f32; 4_800 * 2]];
    let mixed = state.mix_chunk(&mut chunks, 4_800);
    assert!(
        mixed.iter().all(|sample| *sample == 0.0),
        "a disabled click adds nothing"
    );
}

/// The absolute frame of every click in a delivered stream: a click is a
/// loud run, and the beats of these projects are a quarter apart.
fn click_starts(stream: &[f32]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut silent = usize::MAX;
    for (frame, samples) in stream.chunks(2).enumerate() {
        if samples.iter().any(|sample| sample.abs() > 1e-4) {
            if silent > 1_000 {
                starts.push(frame);
            }
            silent = 0;
        } else {
            silent = silent.saturating_add(1);
        }
    }
    starts
}

fn click_on() -> ClickSettings {
    use super::*;
    ClickSettings {
        enabled: true,
        level_db: 0.0,
        count_in: oc_audio::click::CountIn::Off,
    }
}

#[test]
fn the_click_and_the_backing_share_the_master_chain_reference() {
    use super::*;
    // A master tuner reports 1 920 frames of latency at 48 kHz (40 ms).
    let project = click_project("", "@master\n  effects:\n    - tuner: amount=100%\n");
    let (_, chain) = click_frame_and_latency(&project);
    assert_eq!(chain, 1_920, "the master tuner is the chain under test");
    // The project's tone sits on bar 1 beat 3; compare the two onsets there.
    const BEAT: u64 = 48_000;
    let frames = BEAT as usize + MEASURE_RATE as usize / 2;
    let (with_click, _) = record_live_clicked(&project, frames, click_on());
    let (backing_only, _) = record_live_clicked(&project, frames, {
        ClickSettings {
            enabled: false,
            ..click_on()
        }
    });
    let beats = click_starts(&with_click);
    let first = *beats.first().expect("the first click") as i64;
    let beat_three = *beats
        .iter()
        .find(|frame| **frame >= BEAT as usize)
        .expect("the beat's click") as i64;
    let backing = *click_starts(&backing_only)
        .first()
        .expect("the backing tone") as i64;
    let offset = beat_three - backing;
    println!(
        "click latency: chain={chain} first_click={first} beat_click={beat_three} \
             backing={backing} offset={offset} frames"
    );
    // The click trails the timeline by the chain, exactly like the tone.
    assert!(
        (first - chain as i64).abs() <= 16,
        "the first click must trail the run by {chain}, got {first}"
    );
    let expected = (BEAT + chain) as i64;
    assert!(
        (beat_three - expected).abs() <= 16,
        "the beat's click must land on {expected}, got {beat_three}"
    );
    assert!(
        (backing - expected).abs() <= 16,
        "the backing tone must land on {expected}, got {backing}"
    );
    // The loopback of click and backing: no offset between the two.
    assert!(
        offset.abs() <= MEASURE_BLOCK as i64,
        "click and backing are {offset} frames apart"
    );
}

#[test]
fn a_click_aligned_midi_note_records_on_the_beat_through_a_chain() {
    use super::*;
    use crate::midi_record::{CapturedEvent, CapturedKind, Quantise, TakeTransform, transform};
    let project = click_project("", "@master\n  effects:\n    - tuner: amount=100%\n");
    let (_, chain) = click_frame_and_latency(&project);
    const BEAT: u64 = 48_000;
    let frames = BEAT as usize + MEASURE_RATE as usize / 2;
    let (stream, _) = record_live_clicked(&project, frames, click_on());
    let heard = *click_starts(&stream)
        .iter()
        .find(|frame| **frame >= BEAT as usize)
        .expect("the beat's click") as u64;
    // The stream starts at the roll, and the device presents a block one
    // callback later: the click is heard at `heard` plus that block.
    let device = MEASURE_BLOCK as u64;
    let pressed = heard + device;
    let events = [
        CapturedEvent {
            frame: pressed,
            kind: CapturedKind::NoteOn {
                key: 60,
                velocity: 100,
            },
        },
        CapturedEvent {
            frame: pressed + 12_000,
            kind: CapturedKind::NoteOff { key: 60 },
        },
    ];
    let clock = ProjectClock::new(&oc_dsl::ast::ProjectProperties::default()).expect("clock");
    // The take's head is the roll wait plus the monitor latency, which is
    // the device period and the chain: the reference the click rides.
    let take = transform(&TakeTransform {
        events: &events,
        clock: &clock,
        rate: MEASURE_RATE,
        record_start_frame: 0,
        stop_frame: pressed + 48_000,
        origin_quarter: 0.0,
        head_trim_frames: device + chain,
        quantise: Quantise::Off,
        punch: None,
    })
    .expect("transform");
    let beat = clock
        .position_at_sample(BEAT, MEASURE_RATE)
        .expect("clock")
        .quarter;
    let note = take.notes.first().expect("the note is in the take");
    let offset = (note.start_beat - beat) * 24_000.0;
    println!(
        "click-aligned take: chain={chain} heard={heard} pressed={pressed} \
             note_offset={offset:.1} frames"
    );
    assert!(
        offset.abs() <= MEASURE_BLOCK as f64,
        "the note must be within one block of beat {beat}, got {offset:.1} frames off"
    );
}

#[test]
fn the_click_keeps_its_frame_wherever_the_block_starts() {
    use super::*;
    // A seek and a loop wrap both re-base the chunk start: the click must
    // stay on its beat plus the reference delay in every case.
    const DELAY: u64 = 1_920;
    const BEAT: u64 = 24_000;
    for start in [BEAT + DELAY - 4_000, BEAT, BEAT + DELAY] {
        let mut state = click_state();
        state.chain_latency_frames.store(DELAY, Ordering::Release);
        state.dsp_cursor = start;
        let frames = 4_800;
        let mut chunks = vec![vec![0.0f32; frames * 2]];
        let mixed = state.mix_chunk(&mut chunks, frames);
        let hits = loud_frames(&mixed);
        let first = *hits.first().expect("the beat's click is in this block");
        let at = start + first as u64;
        assert!(
            (at as i64 - (BEAT + DELAY) as i64).abs() <= 2,
            "a block starting at {start} puts the click at {at}, not {}",
            BEAT + DELAY
        );
    }
}

#[test]
fn a_bypassed_master_chain_sounds_the_click_where_an_empty_one_does() {
    use super::*;
    let plain = click_project("", "");
    let bypassed = click_project(
        "",
        "@master\n  effects:\n    - tuner: amount=100%, bypass=true\n",
    );
    let mut plain_state = RenderState::new(MEASURE_RATE);
    plain_state.click_settings = click_on();
    plain_state
        .load_project(&plain, PathBuf::new())
        .expect("plain project");
    let mut bypass_state = RenderState::new(MEASURE_RATE);
    bypass_state.click_settings = click_on();
    bypass_state
        .load_project(&bypassed, PathBuf::new())
        .expect("bypassed project");
    assert_eq!(plain_state.chain_latency_frames.load(Ordering::Acquire), 0);
    assert_eq!(
        bypass_state.chain_latency_frames.load(Ordering::Acquire),
        0,
        "a bypassed effect reports no latency"
    );
    let plain_block = plain_state.render_chunk(4_800);
    let bypass_block = bypass_state.render_chunk(4_800);
    assert_eq!(
        plain_block, bypass_block,
        "a bypassed master effect must sound exactly like an absent one"
    );
    // With no chain the click stays on its own frame, as before.
    let hits = loud_frames(&plain_block);
    assert!(
        hits.first().copied().is_some_and(|frame| frame <= 3),
        "an empty chain must not defer the click: {hits:?}"
    );
}

#[test]
fn the_count_in_and_the_first_timeline_beat_stay_one_beat_apart() {
    use super::*;
    use oc_audio::click::count_in_plan;
    const DELAY: u64 = 1_920;
    let mut state = click_state();
    state.chain_latency_frames.store(DELAY, Ordering::Release);
    let clock = state.clock.clone().expect("clock");
    let plan = count_in_plan(&clock, 1, MEASURE_RATE, 0.0).expect("plan");
    state.count_in = Some(super::CountInState {
        plan: plan.clone(),
        emitted: 0,
    });
    let mut stream = Vec::new();
    while state.count_in.is_some() {
        stream.extend(state.render_count_in_chunk(4_096));
    }
    assert_eq!(
        (stream.len() / 2) as u64,
        plan.frames,
        "the count-in still lasts one bar"
    );
    state.dsp_cursor = 0;
    let mut chunks = vec![vec![0.0f32; 4_800 * 2]];
    let first = state.mix_chunk(&mut chunks, 4_800);
    stream.extend(first);
    let starts = click_starts(&stream);
    assert_eq!(starts.len(), 5, "four count-in beats and the downbeat");
    for pair in starts.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            24_000,
            "the count-in and the timeline must stay one beat apart: {starts:?}"
        );
    }
    assert!(
        (starts[0] as i64 - DELAY as i64).abs() <= 2,
        "the count-in rides the same reference delay: {starts:?}"
    );
}

#[test]
fn a_count_in_sounds_before_the_timeline_and_leaves_the_playhead_alone() {
    use oc_audio::click::{CountIn, count_in_plan};
    let mut state = click_state();
    // The transport click is off; a count-in still clicks.
    state.click_settings = oc_audio::click::ClickSettings {
        enabled: false,
        level_db: -6.0,
        count_in: CountIn::OneBar,
    };
    let clock = state.clock.clone().expect("clock");
    let plan = count_in_plan(&clock, 1, 48_000, 0.0).expect("plan");
    assert_eq!(plan.frames, 96_000);
    state.count_in = Some(super::CountInState {
        plan: plan.clone(),
        emitted: 0,
    });
    let mut emitted = 0u64;
    let mut hits = Vec::new();
    while state.count_in.is_some() {
        let chunk = state.render_count_in_chunk(CHUNK_FRAMES);
        assert!(!chunk.is_empty(), "the count-in keeps producing audio");
        hits.extend(
            loud_frames(&chunk)
                .into_iter()
                .map(|frame| frame as u64 + emitted),
        );
        emitted += (chunk.len() / 2) as u64;
    }
    assert_eq!(emitted, plan.frames, "the count-in lasts one bar");
    assert_eq!(
        state.dsp_cursor, 0,
        "the timeline does not advance during the count-in"
    );
    assert_eq!(state.render_cursor, 0);
    // Four beats, half a second apart, the first one accented.
    let starts: Vec<u64> = [0u64, 24_000, 48_000, 72_000]
        .into_iter()
        .filter(|beat| hits.iter().any(|hit| *hit >= *beat && *hit < beat + 1_440))
        .collect();
    assert_eq!(starts.len(), 4, "one click per count-in beat: {hits:?}");
    let first = *hits.first().expect("a count-in click");
    assert!(
        first <= 3,
        "the count-in starts on its first frame: {first}"
    );
}

/// The count-in is audible but not part of the timeline: the reported
/// playhead holds at the record start, and the end of the project is the
/// real end rather than one count-in early.
#[test]
fn the_count_in_holds_the_playhead_at_the_record_start() {
    use super::*;
    // One bar at 120 bpm, the case the reported jump was measured on: a
    // 256-frame device block and a record start of frame 0.
    const RECORD_START: u64 = 0;
    const COUNT_IN: u64 = 96_000;
    const TIMELINE: u64 = 1_024;
    const BLOCK: u64 = 256;
    let (mut producer, consumer) = rtrb::RingBuffer::new(((COUNT_IN + TIMELINE) * 2 + 8) as usize);
    let playback = super::streaming_playback(consumer);
    playback.active.store(true, Ordering::Release);
    playback.ready.store(true, Ordering::Release);
    playback
        .play_cursor_frames
        .store(RECORD_START, Ordering::Release);
    playback
        .count_in_start
        .store(RECORD_START, Ordering::Release);
    playback.count_in_frames.store(COUNT_IN, Ordering::Release);
    playback
        .count_in_remaining
        .store(COUNT_IN, Ordering::Release);
    playback
        .total_frames
        .store(RECORD_START + TIMELINE, Ordering::Release);
    // Count-in audio first, then the timeline. The samples do not matter:
    // only how the callback assigns them to the cursor does.
    let audio = vec![0.25f32; ((COUNT_IN + TIMELINE) * 2) as usize];
    super::write_ring_chunk(&mut producer, &audio);

    let mut output = [0.0f32; (BLOCK * 2) as usize];
    while playback.count_in_remaining.load(Ordering::Acquire) > 0 {
        crate::audio::streaming_audio_callback(&mut output, &playback);
        assert_eq!(
            playback.timeline_frames(),
            RECORD_START,
            "the count-in does not move the timeline"
        );
        assert_eq!(
            playback.audible_frames(),
            RECORD_START,
            "the picture holds at the record start while the count-in sounds"
        );
    }
    println!(
        "count-in end: cursor={} audible={}",
        playback.timeline_frames(),
        playback.audible_frames()
    );
    assert!(
        playback.timeline_frames() < playback.total_frames.load(Ordering::Acquire),
        "the count-in does not count towards the end of the project"
    );
    // The device plays one buffer late, so the first timeline buffer still
    // reports the record start; the second one moves the picture.
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert_eq!(playback.timeline_frames(), RECORD_START + BLOCK);
    assert_eq!(playback.audible_frames(), RECORD_START);
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert_eq!(playback.timeline_frames(), RECORD_START + 2 * BLOCK);
    assert_eq!(playback.audible_frames(), RECORD_START + BLOCK);
    while playback.timeline_frames() < playback.total_frames.load(Ordering::Acquire) {
        assert!(
            playback.timeline_frames() < playback.total_frames.load(Ordering::Acquire),
            "the end arrives only after the whole timeline"
        );
        crate::audio::streaming_audio_callback(&mut output, &playback);
    }
    assert_eq!(
        playback.timeline_frames(),
        playback.total_frames.load(Ordering::Acquire),
        "the end of the project is the real end"
    );
}

/// A count-in that starts the transport at a nonzero frame holds the
/// picture there, not one latency window before it.
#[test]
fn the_count_in_holds_a_nonzero_record_start() {
    use super::*;
    const RECORD_START: u64 = 48_000;
    const COUNT_IN: u64 = 1_024;
    const TIMELINE: u64 = 512;
    let (mut producer, consumer) = rtrb::RingBuffer::new(8_192);
    let playback = super::streaming_playback(consumer);
    playback.active.store(true, Ordering::Release);
    playback.ready.store(true, Ordering::Release);
    playback
        .play_cursor_frames
        .store(RECORD_START, Ordering::Release);
    playback
        .count_in_start
        .store(RECORD_START, Ordering::Release);
    playback.count_in_frames.store(COUNT_IN, Ordering::Release);
    playback
        .count_in_remaining
        .store(COUNT_IN, Ordering::Release);
    playback
        .total_frames
        .store(RECORD_START + TIMELINE, Ordering::Release);
    let audio = vec![0.25f32; ((COUNT_IN + TIMELINE) * 2) as usize];
    super::write_ring_chunk(&mut producer, &audio);

    let mut output = [0.0f32; 512];
    while playback.count_in_remaining.load(Ordering::Acquire) > 0 {
        crate::audio::streaming_audio_callback(&mut output, &playback);
        assert_eq!(playback.audible_frames(), RECORD_START);
    }
    assert_eq!(playback.timeline_frames(), RECORD_START);
    // The first timeline block is still one device block behind the
    // picture; the second one moves it relative to the record start.
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert_eq!(playback.timeline_frames(), RECORD_START + 256);
    assert_eq!(playback.audible_frames(), RECORD_START);
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert_eq!(playback.timeline_frames(), RECORD_START + 512);
    assert_eq!(playback.audible_frames(), RECORD_START + 256);
}

#[test]
fn take_progress_counts_the_device_frames_even_after_a_loop_wrap() {
    let (mut producer, consumer) = rtrb::RingBuffer::new(8192);
    let playback = super::streaming_playback(consumer);
    playback.publish_loop_seam(Some(LoopRegion {
        start: 1_000,
        end: 2_000,
    }));
    playback.play_cursor_frames.store(1_990, Ordering::Release);
    playback.count_in_remaining.store(8, Ordering::Release);
    playback.publish_play_run(1_990);
    playback.active.store(true, Ordering::Release);
    super::write_ring_chunk(&mut producer, &[0.25; 256]);
    let mut block = [0.0; 64];
    crate::audio::streaming_audio_callback(&mut block, &playback);
    assert_eq!(playback.timeline_frames(), 1_014);
    assert_eq!(playback.unwrapped_take_frame(), 1_990);
    crate::audio::streaming_audio_callback(&mut block, &playback);
    assert_eq!(playback.timeline_frames(), 1_046);
    assert_eq!(playback.unwrapped_take_frame(), 2_014);

    let retired = playback.play_applied.load(Ordering::Acquire);
    playback.publish_play_run(500);
    playback.count_take_device_frames(retired, 32);
    assert_eq!(playback.unwrapped_take_frame(), 0);
}

/// A long note is not retriggered or rebuilt at a streaming chunk
/// boundary: the chunked producer and the offline render agree sample for
/// sample across the whole note.
#[test]
fn streaming_chunks_match_the_offline_render_for_a_long_note() {
    let text = r#"@project
  tempo: 20
  time_signature: 4/4
@preset p
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.72,
    unison.voices: 7, unison.detune: 24ct, unison.width: 0.85,
    amp.attack: 0.02s, amp.decay: 0.4s, amp.sustain: 0.95,
    amp.release: 0.4s, output.gain: 0.5}
@track pad | preset: p, instrument: synth/lead
  @phrase n [1:1:0 - 2:1:0]
    C3 w f
"#;
    let project = oc_dsl::parse(text).unwrap();
    let sample_rate = 48_000u32;
    let mut state = super::RenderState::new(sample_rate);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    let end = state.playback_end();
    let mut streamed: Vec<f32> = Vec::new();
    let mut first = true;
    while state.render_cursor < end {
        let frames = if first {
            super::FIRST_CHUNK_FRAMES
        } else {
            super::CHUNK_FRAMES
        };
        let block = state.render_chunk(frames);
        if block.is_empty() {
            break;
        }
        streamed.extend_from_slice(&block);
        first = false;
    }
    let offline = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        std::path::Path::new(""),
        sample_rate,
        &mut std::collections::HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(streamed.len(), offline.len(), "rendered length");
    let worst = streamed
        .iter()
        .zip(&offline)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert_eq!(worst, 0.0, "streaming differs from the offline render");
}

/// A seek into the middle of a sounding note resumes that note exactly: the
/// preroll replays its note-on, so a loop wrap or a transport jump does not
/// retrigger it or drop it.
#[test]
fn a_seek_into_a_long_note_resumes_it_exactly() {
    let text = r#"@project
  tempo: 20
  time_signature: 4/4
@preset p
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.72,
    unison.voices: 7, unison.detune: 24ct, unison.width: 0.85,
    amp.attack: 0.02s, amp.decay: 0.4s, amp.sustain: 0.95,
    amp.release: 0.4s, output.gain: 0.5}
@track pad | preset: p, instrument: synth/lead
  @phrase n [1:1:0 - 2:1:0]
    C3 w f
"#;
    let project = oc_dsl::parse(text).unwrap();
    let sample_rate = 48_000u32;
    let offline = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        std::path::Path::new(""),
        sample_rate,
        &mut std::collections::HashMap::new(),
        None,
    )
    .unwrap();
    let mut state = super::RenderState::new(sample_rate);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    // Six seconds in: the note has been sounding since frame zero.
    let seek = 6 * sample_rate as u64;
    state.seek_to(seek).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    let block = state.render_chunk(super::CHUNK_FRAMES);
    let expected = &offline[seek as usize * 2..seek as usize * 2 + block.len()];
    let worst = block
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        block.iter().any(|value| value.abs() > 0.01),
        "the note sounds"
    );
    assert_eq!(
        worst, 0.0,
        "the resumed note differs from the offline render"
    );
}

/// A seek that spends its preroll budget releases a note that started
/// before the event cursor: the skipped range never names it, so only the
/// sounding set can stop it at the destination.
#[test]
fn an_expired_preroll_releases_a_note_that_started_before_the_cursor() {
    let text = r#"@project
  tempo: 20
  time_signature: 4/4
@preset p
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.72,
    amp.attack: 0.01s, amp.decay: 0.05s, amp.sustain: 0.95,
    amp.release: 0.2s, output.gain: 0.5}
@track pad | preset: p, instrument: synth/lead
  @phrase n [1:1:0 - 2:1:0]
    C3 w f
"#;
    let project = oc_dsl::parse(text).unwrap();
    let sample_rate = 48_000u32;
    let mut state = super::RenderState::new(sample_rate);
    state
        .load_project(&project, std::path::PathBuf::new())
        .unwrap();
    // Playing, so the seek below may spend its wall-clock budget.
    state.playing = true;
    let head = state.render_chunk(super::CHUNK_FRAMES);
    assert!(
        head.iter().any(|value| value.abs() > 0.05),
        "the note sounds before the seek"
    );
    // Eleven seconds in: the whole note (the bar is 12 s at this tempo) has
    // been held since frame zero, and its note-on is behind the cursor.
    let target = 11 * sample_rate as u64;
    state.seek_to(target).unwrap();
    assert!(
        state.seek_target.is_some(),
        "the seek has a gap to reconstruct"
    );
    state.preroll_chunk();
    std::thread::sleep(super::SEEK_PREROLL_BUDGET * 2);
    assert!(state.preroll_expired(), "the preroll must spend its budget");
    assert!(
        state.sounding.iter().any(|word| *word != 0),
        "the note is still held when the preroll expires"
    );
    state.abandon_preroll();
    assert!(
        state.sounding.iter().all(|word| *word == 0),
        "the destination starts with every note released"
    );
    // The destination has no note of its own, so a released voice decays
    // instead of holding at its sustain level.
    let peaks: Vec<f32> = (0..8)
        .map(|_| super::peak_of(&state.render_chunk(super::CHUNK_FRAMES)))
        .collect();
    assert!(peaks[0] > 0.01, "the release is heard: {peaks:?}");
    assert!(peaks[7] < 1e-4, "the note decays to silence: {peaks:?}");
}

/// The callback wraps on a seam it read as one pair: a writer publishing
/// back to back never leaves it pairing one publish's start with another's
/// end.
#[test]
fn a_loop_seam_read_is_always_one_published_pair() {
    use super::*;
    let pair = Arc::new(AtomicPair::new());
    pair.store(Some(LoopRegion { start: 0, end: 1 }));
    let writer = {
        let pair = Arc::clone(&pair);
        std::thread::spawn(move || {
            for index in 1..=20_000_u64 {
                pair.store(Some(LoopRegion {
                    start: index * 2,
                    end: index * 2 + 1,
                }));
            }
        })
    };
    let mut reads = 0;
    while !writer.is_finished() {
        if let Some(region) = pair.load() {
            assert_eq!(
                region.end,
                region.start + 1,
                "a torn pair was read: {region:?}"
            );
            assert_eq!(region.start % 2, 0, "a torn pair was read: {region:?}");
            reads += 1;
        }
    }
    writer.join().unwrap();
    assert!(reads > 0, "the reader saw a published pair");
    assert_eq!(
        pair.load(),
        Some(LoopRegion {
            start: 40_000,
            end: 40_001
        })
    );
    assert_eq!(
        pair.store(None),
        Some(LoopRegion {
            start: 40_000,
            end: 40_001
        }),
        "store reports the pair it replaced"
    );
    assert_eq!(pair.load(), None);
}

/// Only an armed region ends a pass: a drawn but disarmed region must not
/// fade the audio that plays through its end.
#[test]
fn a_drawn_but_disarmed_region_ends_no_pass() {
    use super::*;
    let (_producer, consumer) = rtrb::RingBuffer::new(64);
    let playback = super::streaming_playback(consumer);
    playback.set_loop_region(Some(LoopRegion {
        start: 0,
        end: 4_000,
    }));
    assert!(playback.loop_region().is_some());
    assert!(
        playback.armed_loop_region().is_none(),
        "a region is not a loop until it is armed"
    );
    playback.set_loop_enabled(true);
    assert_eq!(
        playback.armed_loop_region(),
        Some(LoopRegion {
            start: 0,
            end: 4_000
        })
    );
    playback.set_loop_enabled(false);
    assert!(playback.armed_loop_region().is_none());
    playback.set_loop_region(None);
    assert!(playback.loop_region().is_none());
}

/// A loop pass that ends at the region boundary fades out, so the wrap does
/// not cut the note mid-waveform; a chunk that does not end a pass, or a
/// bounded preview, keeps its samples.
#[test]
fn a_loop_pass_ends_at_silence() {
    use super::*;
    let region = LoopRegion {
        start: 0,
        end: 4_000,
    };
    let program = |frame: usize| 0.6 * (std::f32::consts::TAU * frame as f32 / 128.0).cos();
    let chunk = |frames: usize| -> Vec<f32> {
        (0..frames)
            .flat_map(|frame| [program(frame), program(frame)])
            .collect()
    };

    // The pass ends here: the tail fades and the head keeps its samples.
    let mut closes = chunk(512);
    let before = closes.clone();
    super::fade_closed_loop_pass(&mut closes, 4_000, 48_000, Some(region), false);
    let head = 512 - crate::audio::RESUME_FADE_FRAMES as usize;
    assert_eq!(
        &closes[..head * 2],
        &before[..head * 2],
        "the head is untouched"
    );
    let last = closes.len() - 2;
    assert!(
        closes[last].abs() < 0.1 * program(511).abs(),
        "the pass stepped into the wrap: {} of {}",
        closes[last].abs(),
        program(511).abs()
    );

    // Not at the boundary, no region, and a bounded preview: unchanged.
    for (cursor, region, bounded) in [
        (3_000u64, Some(region), false),
        (4_000, None, false),
        (4_000, Some(region), true),
    ] {
        let mut kept = chunk(512);
        let expected = kept.clone();
        super::fade_closed_loop_pass(&mut kept, cursor, 48_000, region, bounded);
        assert_eq!(kept, expected, "cursor {cursor} bounded {bounded}");
    }
}

/// A loop pass that ends inside a sustained note must not cost a callback of
/// silence at the wrap.
///
/// The tone is a 250 Hz sine on a whole 32-frame period at 8 kHz and the
/// region starts on a period boundary, so the pass that follows the seam
/// continues the same phase. The seam is therefore a level question, not a
/// phase question: it may dip through the fades, but it may not go silent.
#[test]
fn a_loop_that_ends_inside_a_note_wraps_without_a_silent_callback() {
    use super::*;
    let sample_rate = 8_000u32;
    let block = 32usize;
    let handle = RenderThreadHandle::spawn(sample_rate);
    let project = oc_dsl::parse(
        r#"@project
  tempo: 60
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 250, amplitude: 0.5, waveform: 0, output.gain: 1}
@track lead | preset: tone, instrument: synth/lead
  @phrase held [1:1 - 9:1]
    C4 w f
"#,
    )
    .unwrap();
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: std::path::PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    // 2.000 s to 2.512 s at 8 kHz: the region ends inside the held note, and
    // its distance from zero prices the preroll the wrap rebuilds.
    let (region_start, region_end) = (16_000u64, 20_096u64);
    handle.playback.set_loop_region(Some(LoopRegion {
        start: region_start,
        end: region_end,
    }));
    handle.playback.set_loop_enabled(true);
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(5)));

    // Drive the production callback and keep every block. The consumer is
    // paced at one block per block period, so a starve means the producer
    // really ran out of audio and not that the test outran it.
    let mut blocks: Vec<Vec<f32>> = Vec::new();
    let mut cursors: Vec<(u64, u64)> = Vec::new();
    let mut seam_block: Option<usize> = None;
    let mut output = vec![0.0f32; block * 2];
    let mut heard = false;
    let mut wraps = 0;
    let deadline = std::time::Instant::now();
    while wraps < 2 {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(20),
            "the loop never wrapped twice"
        );
        let before = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        output.fill(0.0);
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        let after = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        if heard {
            let (mut run, mut longest) = (0usize, 0usize);
            for sample in output.chunks_exact(2).map(|frame| frame[0]) {
                if sample == 0.0 {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
            assert!(
                longest < 8,
                "the seam starved the ring: the callback at frame {before} holds \
                     {longest} zero frames"
            );
        }
        heard |= output.iter().any(|value| value.abs() > 0.01);
        if heard && after < before {
            wraps += 1;
            seam_block.get_or_insert(blocks.len());
        }
        cursors.push((before, after));
        blocks.push(output.clone());
        std::thread::sleep(std::time::Duration::from_micros(
            (block as u64) * 1_000_000 / u64::from(sample_rate),
        ));
    }

    let first_audible = blocks
        .iter()
        .position(|block| block.iter().any(|value| value.abs() > 0.01))
        .expect("the tone sounds");
    let seam_block = seam_block.expect("the playhead wraps with the audio");
    assert!(seam_block > first_audible);
    let samples: Vec<f32> = blocks.concat();
    let frame_at = |frame: usize| samples[frame * 2];
    // The old pass ends and the new one starts here.
    let (before, _) = cursors[seam_block];
    let seam = seam_block * block + (region_end - before) as usize;

    // The program is one sine, so a step beyond its own slope is a click.
    let amplitude = (region_start as usize + 64..region_start as usize + 1_000)
        .map(|frame| frame_at(frame).abs())
        .fold(0.0f32, f32::max);
    let program_step = amplitude * std::f32::consts::TAU * 250.0 / sample_rate as f32;
    let step = (frame_at(seam) - frame_at(seam - 1)).abs();
    assert!(
        step < 1.5 * program_step,
        "the seam steps by {step}, past the program's own {program_step}"
    );

    // The seam may dip through the fades, but it may not be silent: a run of
    // zero frames before the tone restarts is the gap this fix removes.
    let (mut zero_run, mut longest_zero) = (0usize, 0usize);
    let (mut dip_run, mut longest_dip) = (0usize, 0usize);
    for frame in first_audible * block..samples.len() / 2 {
        let level = frame_at(frame).abs();
        if level == 0.0 {
            zero_run += 1;
            longest_zero = longest_zero.max(zero_run);
        } else {
            zero_run = 0;
        }
        if level < 0.01 {
            dip_run += 1;
            longest_dip = longest_dip.max(dip_run);
        } else {
            dip_run = 0;
        }
    }
    assert!(
        longest_zero <= 1,
        "the wrap leaves {longest_zero} frames at exactly zero"
    );
    assert!(
        longest_dip < 4 * block,
        "the seam dips for {longest_dip} frames, four callbacks of {block}"
    );

    // The region starts on a whole period, so the pass after the fade-in
    // must be the same sine at the same phase.
    let fade = crate::audio::RESUME_FADE_FRAMES as usize;
    let phase_error = (0..512usize)
        .map(|offset| {
            let frame = seam + fade + offset;
            let expected = amplitude
                * (std::f32::consts::TAU * 250.0 * frame as f32 / sample_rate as f32).sin();
            (frame_at(frame) - expected).abs()
        })
        .fold(0.0f32, f32::max);
    assert!(
        phase_error < 0.02 * amplitude,
        "the resumed pass lost the sine phase: {phase_error} of {amplitude}"
    );
    eprintln!(
        "[loop-seam] seam at frame {seam} of block {seam_block}: step {step:.5} \
             (program {program_step:.5}), longest zero run {longest_zero} frames, \
             dip {longest_dip} frames, phase error {phase_error:.5} of {amplitude:.3}"
    );
}

/// Disabling Loop must drop the head of the pass already queued. The ring
/// can hold the wrapped pass while the playhead has not crossed the seam,
/// so playing it would sound the old loop start as the cursor moves on.
///
/// The region holds one note at its start and the timeline past the region
/// is silent until a marker in bar 3, so any audio between the seam and the
/// marker can only be the discarded pass.
#[test]
fn disabling_loop_discards_the_queued_wrapped_pass() {
    use super::*;
    let sample_rate = 8_000u32;
    let block = 256usize;
    let handle = RenderThreadHandle::spawn(sample_rate);
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.7,
    amp.attack: 0.01s, amp.decay: 0.05s, amp.sustain: 0.9,
    amp.release: 0.05s, filter.cutoff: 3000Hz, filter.env.amount: 0,
    output.gain: 0.5}
@track lead | preset: tone, instrument: synth/lead
  @phrase head [1:1 - 1:2]
    C4 q f
  @phrase marker [3:1 - 3:2]
    C4 q f
"#,
    )
    .unwrap();
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: std::path::PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    // One bar at 120 bpm: the region wraps at 2 s, the marker is at 4 s.
    let region_end = 16_000u64;
    let marker = 32_000u64;
    // The note ends with the first beat, so the pass tail is silent.
    let quiet_from = 8_192u64;
    handle.playback.set_loop_region(Some(LoopRegion {
        start: 0,
        end: region_end,
    }));
    handle.playback.set_loop_enabled(true);
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(5)));

    let mut output = vec![0.0f32; block * 2];
    let period =
        std::time::Duration::from_micros((block as u64) * 1_000_000 / u64::from(sample_rate));
    let mut blocks: Vec<(u64, Vec<f32>)> = Vec::new();
    let deadline = std::time::Instant::now();
    // Drive until the producer has wrapped: the head of the wrapped pass is
    // queued, and stopping here keeps the playhead before the seam.
    loop {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(20),
            "the loop never wrapped"
        );
        let before = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        output.fill(0.0);
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        blocks.push((before, output.clone()));
        if handle.playback.loop_seam().is_some() {
            assert!(
                before < region_end,
                "the wrapped head was already heard at frame {before}"
            );
            // Let the producer write that head into the ring.
            for _ in 0..4 {
                std::thread::sleep(period);
                let before = handle.playback.play_cursor_frames.load(Ordering::Acquire);
                output.fill(0.0);
                crate::audio::streaming_audio_callback(&mut output, &handle.playback);
                blocks.push((before, output.clone()));
            }
            break;
        }
        std::thread::sleep(period);
    }
    let toggled = blocks.len();
    handle.playback.set_loop_enabled(false);

    // Keep playing until the marker: the forward timeline, not the loop.
    let horizon = marker + 4 * block as u64;
    loop {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(20),
            "the forward timeline never reached the marker"
        );
        let before = handle.playback.play_cursor_frames.load(Ordering::Acquire);
        output.fill(0.0);
        crate::audio::streaming_audio_callback(&mut output, &handle.playback);
        blocks.push((before, output.clone()));
        if handle.playback.play_cursor_frames.load(Ordering::Acquire) >= horizon {
            break;
        }
        std::thread::sleep(period);
    }

    // The note peaks near 0.2, so one percent of full scale separates it
    // from a residual tail the synthesizer leaves behind.
    let mut quiet = 0usize;
    let mut marker_peak = 0.0f32;
    for (before, samples) in &blocks[toggled..] {
        // One peak per callback, because a note that sounds crosses zero.
        let mut quiet_peak = 0.0f32;
        for (frame, pair) in samples.chunks_exact(2).enumerate() {
            let position = before + frame as u64;
            if (quiet_from..marker).contains(&position) {
                quiet += 1;
                quiet_peak = quiet_peak.max(pair[0].abs());
            } else if (marker..marker + 2_000).contains(&position) {
                marker_peak = marker_peak.max(pair[0].abs());
            }
        }
        assert!(
            quiet_peak < 0.01,
            "the wrapped pass plays at frame {before}: {quiet_peak}"
        );
    }
    assert!(quiet > 8_000, "the settle window is empty: {quiet}");
    assert!(
        marker_peak > 0.05,
        "the forward timeline resumes at the marker: {marker_peak}"
    );
}

/// A starve is audible only if the audio steps back in. The callback fades
/// the audio in after the gap, so the gap stays a dip.
#[test]
fn a_starve_fades_back_into_the_waveform() {
    use super::*;
    let (mut producer, consumer) = rtrb::RingBuffer::new(super::RING_BUFFER_FRAMES * 2);
    let playback = Arc::new(super::streaming_playback(consumer));
    playback.active.store(true, Ordering::Release);
    playback.ready.store(true, Ordering::Release);

    // A cosine peaking on frame zero: a restart mid-program is a step of a
    // full 0.6, while the program's own slope there is zero.
    let program = |frame: usize| 0.6 * (std::f32::consts::TAU * frame as f32 / 128.0).cos();
    let block: Vec<f32> = (0..512)
        .flat_map(|frame| [program(frame), program(frame)])
        .collect();

    super::write_ring_chunk(&mut producer, &block);
    let mut output = [0.0f32; 256];
    for _ in 0..4 {
        crate::audio::streaming_audio_callback(&mut output, &playback);
        assert!(output[0].abs() > 0.5, "the first blocks play in full");
    }
    assert_eq!(playback.resume_fade.load(Ordering::Relaxed), 0);

    // The ring is empty: this callback is the starve.
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert!(
        output.iter().all(|value| *value == 0.0),
        "the starve is silent"
    );
    assert_eq!(playback.underruns.load(Ordering::Relaxed), 1);
    assert_eq!(
        playback.resume_fade.load(Ordering::Relaxed),
        crate::audio::RESUME_FADE_FRAMES,
        "the next audio is marked for a fade-in"
    );

    // The audio returns at its peak; the fade keeps the first frame near zero
    // and reaches the program within the fade.
    super::write_ring_chunk(&mut producer, &block);
    crate::audio::streaming_audio_callback(&mut output, &playback);
    assert!(
        output[0].abs() < 0.01,
        "the callback stepped back into the waveform: {}",
        output[0]
    );
    let fade = crate::audio::RESUME_FADE_FRAMES as usize;
    let reached = output[(fade - 1) * 2].abs();
    let natural = program(fade - 1).abs();
    assert!(
        reached > 0.9 * natural,
        "the fade did not reach the program: {reached} of {natural}"
    );
    assert_eq!(playback.resume_fade.load(Ordering::Relaxed), 0);
}

/// Audio that runs short fades its tail out, so the gap after it is a dip
/// rather than a step from the last sample to silence.
#[test]
fn a_short_read_fades_out() {
    use super::*;
    let (mut producer, consumer) = rtrb::RingBuffer::new(super::RING_BUFFER_FRAMES * 2);
    let playback = Arc::new(super::streaming_playback(consumer));
    playback.active.store(true, Ordering::Release);
    playback.ready.store(true, Ordering::Release);

    let program = |frame: usize| 0.6 * (std::f32::consts::TAU * frame as f32 / 128.0).cos();
    // 72 frames for a 128-frame callback: the block runs short.
    let short: Vec<f32> = (0..72)
        .flat_map(|frame| [program(frame), program(frame)])
        .collect();
    super::write_ring_chunk(&mut producer, &short);

    let mut output = [0.0f32; 256];
    crate::audio::streaming_audio_callback(&mut output, &playback);
    let last = 71;
    let faded = output[last * 2].abs();
    assert!(
        faded < 0.1 * program(last).abs(),
        "the tail stepped into silence: {faded} of {}",
        program(last).abs()
    );
    assert!(
        output[(last + 1) * 2..].iter().all(|value| *value == 0.0),
        "the remainder is silent"
    );
    assert_eq!(
        playback.resume_fade.load(Ordering::Relaxed),
        crate::audio::RESUME_FADE_FRAMES,
        "the audio after the gap fades in"
    );
}

/// A pass that plays out is not a starve. Sixty-six seconds of a sixty-second
/// program: the starve in the middle counts once, the six seconds after the
/// end do not, and the fade the last block armed is cleared for the next
/// play.
#[test]
fn the_end_of_a_pass_is_not_an_underrun() {
    use super::*;
    // One callback period, in frames: the block fed and the block read.
    const BLOCK: u64 = 128;
    let rate = 48_000u32;
    let (mut producer, consumer) = rtrb::RingBuffer::new(super::RING_BUFFER_FRAMES * 2);
    let playback = Arc::new(super::streaming_playback(consumer));
    playback.active.store(true, Ordering::Release);
    playback.ready.store(true, Ordering::Release);
    let total = 60 * u64::from(rate);
    playback.total_frames.store(total, Ordering::Release);

    let block = [0.25f32; BLOCK as usize * 2];
    let mut output = [0.0f32; BLOCK as usize * 2];
    let mut fed = 0u64;
    let starve_at = 30 * u64::from(rate);
    while fed < total {
        if fed == starve_at {
            // The producer is late while audio still remains: a starve.
            crate::audio::streaming_audio_callback(&mut output, &playback);
            assert_eq!(
                playback.underruns.load(Ordering::Relaxed),
                1,
                "a starve mid-pass was not counted"
            );
        }
        super::write_ring_chunk(&mut producer, &block);
        crate::audio::streaming_audio_callback(&mut output, &playback);
        fed += BLOCK;
    }
    assert!(playback.has_played_out(), "the pass reached its end");

    // Six more seconds of callbacks against the empty ring.
    for _ in 0..(6 * u64::from(rate) / BLOCK) {
        crate::audio::streaming_audio_callback(&mut output, &playback);
        assert!(
            output.iter().all(|value| *value == 0.0),
            "the end of the program is silent"
        );
    }
    assert_eq!(
        playback.underruns.load(Ordering::Relaxed),
        1,
        "the end of the pass was counted as a starve"
    );
    assert_eq!(
        playback.resume_fade.load(Ordering::Relaxed),
        0,
        "the next play would fade in from a remembered starve"
    );
}

/// The producer parks with the output gate open once it has rendered the
/// whole pass, so the ring the callback is still draining is finished by
/// the same callbacks. The callbacks after it are the end of the program.
#[test]
fn a_playthrough_to_the_end_does_not_underrun() {
    use super::*;
    // One callback period, in frames.
    const BLOCK: usize = 128;
    let sample_rate = 8_000;
    let handle = RenderThreadHandle::spawn(sample_rate);
    let project = oc_dsl::parse(
            "@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 1:2]\n    @0 C4 q mf\n",
        )
        .unwrap();
    handle.send(RenderCommand::ProjectChanged {
        root: None,
        project: Box::new(project),
        soundfont_path: PathBuf::new(),
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(10))
        .unwrap();
    let total = handle.playback.total_frames.load(Ordering::Acquire);
    assert!(total > 0, "the loaded pass has audio to play");

    // The production callback at realtime, as a device would run it.
    let stop = Arc::new(AtomicBool::new(false));
    let halted = Arc::clone(&stop);
    let playback = Arc::clone(&handle.playback);
    let consumer = std::thread::spawn(move || {
        let mut block = [0.0f32; BLOCK * 2];
        while !halted.load(Ordering::Acquire) {
            crate::audio::streaming_audio_callback(&mut block, &playback);
            std::thread::sleep(std::time::Duration::from_secs_f64(
                BLOCK as f64 / sample_rate as f64,
            ));
        }
    });
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    assert!(handle.wait_ready(std::time::Duration::from_secs(10)));
    let deadline = std::time::Instant::now();
    while !handle.playback.has_played_out() {
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(15),
            "the pass never played out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    stop.store(true, Ordering::Release);
    consumer.join().unwrap();

    // Six seconds of callbacks after the end, with no producer left.
    let mut block = [0.0f32; BLOCK * 2];
    for _ in 0..(6 * sample_rate as usize / BLOCK) {
        crate::audio::streaming_audio_callback(&mut block, &handle.playback);
    }
    assert_eq!(
        handle.playback.underruns.load(Ordering::Acquire),
        0,
        "a normal playthrough to the end inflated the underrun counter"
    );
    assert!(
        handle.playback.active.load(Ordering::Acquire),
        "the parked producer keeps the gate open while the ring drains"
    );
}

#[test]
fn prepared_streaming_blocks_and_meter_recycling_do_not_allocate_from_first_block() {
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset lead
  source: builtin
  plugin: "builtin.open_synth"
@track synth | preset: lead, instrument: synth/lead
  effects:
    - compression: threshold=-18dB, ratio=3, attack=5ms, release=80ms
  @phrase p [1:1 - 8:1]
    C4 w mf
@master
  effects:
    - gain: factor=0.8
"#,
    )
    .unwrap();
    let mut state = RenderState::new(48_000);
    state.meter_hub = Some(Arc::new(MeterHub::new()));
    state.click_settings.enabled = true;
    state.load_project(&project, PathBuf::new()).unwrap();
    let mut reference = RenderState::new(48_000);
    reference.click_settings.enabled = true;
    reference.load_project(&project, PathBuf::new()).unwrap();
    let mut output = Vec::with_capacity(CHUNK_FRAMES * 2);
    for block in 0..40 {
        let frames = [FIRST_CHUNK_FRAMES, 31, CHUNK_FRAMES, 127][block % 4];
        let expected = reference.render_chunk(frames);
        let operations =
            crate::test_alloc::operations(|| state.render_chunk_into(frames, &mut output));
        assert_eq!(operations, 0, "streaming block {block}");
        assert_eq!(state.render_error, None);
        assert_eq!(output, expected, "streaming block {block}");
        assert_eq!(
            state.free_meter_frames.len() + state.pending_meters.len(),
            MAX_PENDING_METERS
        );
        if block >= 20 {
            assert_eq!(
                crate::test_alloc::operations(|| state.release_meters(u64::MAX, 1)),
                0
            );
        }
    }
}

#[test]
fn prepared_count_in_keeps_plan_storage_until_control_plane_replacement() {
    use oc_audio::click::{CountIn, count_in_plan};
    let mut state = click_state();
    let plan = count_in_plan(
        state.clock.as_ref().unwrap(),
        CountIn::OneBar.bars(),
        48_000,
        0.0,
    )
    .unwrap();
    let total = plan.frames;
    state.count_in = Some(super::CountInState { plan, emitted: 0 });
    let mut output = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut emitted = 0;
    while state.count_in.is_some() {
        assert_eq!(
            crate::test_alloc::operations(
                || state.render_count_in_chunk_into(CHUNK_FRAMES, &mut output)
            ),
            0
        );
        assert!(!output.is_empty());
        emitted += (output.len() / 2) as u64;
    }
    assert_eq!(emitted, total);
    assert!(state.completed_count_in.is_some());
    assert_eq!(state.dsp_cursor, 0);
}

#[test]
fn prepared_streaming_fault_discards_audio_and_preserves_detailed_once_only_report() {
    struct Failure {
        failed: std::sync::atomic::AtomicBool,
        reported: std::sync::atomic::AtomicBool,
    }
    impl oc_audio::AudioEffect for Failure {
        fn process(&mut self, buffer: &mut [f32], _: u32) {
            buffer.fill(99.0);
            self.failed.store(true, Ordering::Release);
        }
        fn processing_failed(&self) -> bool {
            self.failed.load(Ordering::Acquire)
        }
        fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
            if self.processing_failed() && !self.reported.swap(true, Ordering::AcqRel) {
                oc_audio::ProcessingFailureReport::Ready(
                    "CLAP process: injected native exception".into(),
                )
            } else {
                oc_audio::ProcessingFailureReport::Reported
            }
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "fault fixture"
        }
    }
    for source_insert in [false, true] {
        let mut state = RenderState::new(48_000);
        state.meter_hub = Some(Arc::new(MeterHub::new()));
        state.click_settings.enabled = true;
        state.load_project(&tone_project(), PathBuf::new()).unwrap();
        let failure = Box::new(Failure {
            failed: std::sync::atomic::AtomicBool::new(false),
            reported: std::sync::atomic::AtomicBool::new(false),
        });
        if source_insert {
            state.source_chains[0].effects.push(failure);
        } else {
            state
                .graph
                .as_mut()
                .unwrap()
                .master_chain
                .effects
                .push(failure);
        }
        state.prepare_streaming().unwrap();
        let mut output = Vec::with_capacity(CHUNK_FRAMES * 2);
        assert_eq!(
            crate::test_alloc::operations(|| state.render_chunk_into(128, &mut output)),
            0
        );
        assert!(output.is_empty());
        assert!(state.rendered_ahead.is_empty());
        assert!(state.pending_meters.is_empty());
        assert_eq!(state.render_cursor, 0);
        assert_eq!(state.dsp_cursor, 0);
        assert_eq!(
            state.take_processing_error().as_deref(),
            Some("CLAP process: injected native exception")
        );
        assert_eq!(state.take_processing_error(), None);
    }
}

#[test]
fn prepared_streaming_failure_reporting_distinguishes_pending_ready_and_claimed() {
    struct PublishingFailure(Arc<std::sync::atomic::AtomicUsize>);
    impl oc_audio::AudioEffect for PublishingFailure {
        fn process(&mut self, _: &mut [f32], _: u32) {
            panic!("failed processor must not run");
        }
        fn processing_failed(&self) -> bool {
            true
        }
        fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
            use oc_audio::ProcessingFailureReport as Report;
            match self.0.load(Ordering::Acquire) {
                0 => Report::Pending,
                1 => {
                    self.0.store(2, Ordering::Release);
                    Report::Ready("CLAP process: published original cause".into())
                }
                _ => Report::Reported,
            }
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "publishing fault fixture"
        }
    }
    let publication = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut state = RenderState::new(48_000);
    state.load_project(&tone_project(), PathBuf::new()).unwrap();
    state.source_chains[0]
        .effects
        .push(Box::new(PublishingFailure(Arc::clone(&publication))));
    state.processing_fault = true;
    assert_eq!(
        crate::test_alloc::operations(|| assert_eq!(state.take_processing_error(), None)),
        0
    );
    assert!(
        state.processing_fault,
        "an in-progress report stays pending"
    );
    let mut output = Vec::with_capacity(CHUNK_FRAMES * 2);
    assert_eq!(
        crate::test_alloc::operations(|| state.render_chunk_into(128, &mut output)),
        0
    );
    assert!(output.is_empty());
    publication.store(1, Ordering::Release);
    assert_eq!(
        state.take_processing_error().as_deref(),
        Some("CLAP process: published original cause")
    );
    assert!(!state.processing_fault);
    // Another consumer having claimed the immutable report is terminal and
    // distinguishable from publication in progress, so playback cannot hang.
    state.processing_fault = true;
    assert!(
        state
            .take_processing_error()
            .unwrap()
            .contains("already reported")
    );
    assert!(!state.processing_fault);
}

/// A seek into the middle of a hosted crescendo must arrive with the
/// expression the ramp had reached, not with the phrase's opening value.
///
/// Ignored by default: it parses the bundled 23 MB SF3 bank (about 20 s) and
/// that load starves the timing-sensitive render-thread tests that run beside
/// it. Run it with `cargo test -p oc-gui --lib -- --ignored a_seek_into`.
#[test]
#[ignore = "parses the bundled 23 MB SF3 bank"]
fn a_seek_into_a_hosted_swell_primes_the_expression_state() {
    use super::*;
    let project = oc_dsl::parse(
        r#"@project
  tempo: 120
@preset bank
  source: soundfont
  program: 19
@track strings | preset: bank, instrument: strings/violin
  @phrase swell [1:1 - 4:1]
    @0.0 cresc
    @0 C4 w ppp
    @4 C4 w
    @8 C4 w fff
"#,
    )
    .unwrap();
    let font = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources/soundfonts/FluidR3Mono_GM.sf3");
    let mut continuous = RenderState::new(8000);
    continuous.load_project(&project, font.clone()).unwrap();
    let mut all = Vec::new();
    while all.len() < 8 * 8000 * 2 {
        all.extend(continuous.render_chunk(CHUNK_FRAMES));
    }
    assert!(
        all[16_000..17_024].iter().any(|sample| sample.abs() > 1e-4),
        "the source has to be sounding where the seek lands"
    );

    let mut seek = RenderState::new(8000);
    seek.load_project(&project, font).unwrap();
    seek.seek_to(8_000).expect("seek");
    while seek.seek_target.is_some() {
        seek.preroll_chunk();
    }
    let sought = seek.render_chunk(1024);
    let reference = &all[16_000..17_024];
    let rms = |samples: &[f32]| {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    };
    let (sought_rms, reference_rms) = (rms(&sought), rms(reference));
    assert!(
        reference_rms > 1e-5,
        "the continuous take has to sound where the seek lands"
    );
    let opening_rms = rms(&all[0..1024]);
    assert!(
        sought_rms > opening_rms * 1.5,
        "the seek arrived at the opening level ({sought_rms} vs {opening_rms}) instead of the ramp"
    );
    // The resampler is chunk-relative, so the two takes are compared by level
    // rather than byte for byte; an unprimed seek differs by more than 2x.
    assert!(
        (sought_rms / reference_rms - 1.0).abs() < 0.2,
        "the seek arrived at {sought_rms} instead of {reference_rms}: the expression ramp was not primed"
    );
}

/// A note edit must re-interpret the project without instantiating a single
/// instrument again; a sound change must still rebuild.
#[test]
fn a_note_edit_reuses_every_instrument_and_a_sound_change_rebuilds_it() {
    use super::*;
    let text = r#"@project "reuse"
  tempo: 120
@track lead | instrument: synth/lead_square
  @phrase p [1:1 - 3:1]
    C4 q mf
    E4 q mf
"#;
    let mut project = oc_dsl::parse(text).unwrap();
    // The GUI injects the family default before a project reaches the thread.
    oc_dsl::instruments::apply_defaults(&mut project);
    let handle = RenderThreadHandle::spawn(48_000);
    let load = |project: &Project| {
        handle.send(RenderCommand::ProjectChanged {
            project: Box::new(project.clone()),
            soundfont_path: PathBuf::new(),
            root: None,
            warm: true,
        });
        handle
            .wait_project_loaded(std::time::Duration::from_secs(5))
            .unwrap();
    };
    load(&project);
    let before = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert!(before > 0, "the first load built the instrument");

    // Move the second note: content only, same instrument and preset.
    let mut edited = project.clone();
    edited.tracks[0].phrases[0].events[1].start_beat = 2.0;
    load(&edited);
    let after_note = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert_eq!(
        after_note, before,
        "a note edit instantiated the instrument again"
    );

    // Retune the preset: the source depends on it, so it is rebuilt.
    let mut retuned = edited.clone();
    retuned.presets[0]
        .params
        .push(("osc.a.level".into(), oc_dsl::ast::ParamValue::Number(0.5)));
    load(&retuned);
    let after_sound = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert!(
        after_sound > after_note,
        "a preset change must rebuild the instrument"
    );
}

/// Reusing a lane set must not survive a track-count change: the stale chains
/// have to go with the stale instruments.
#[test]
fn adding_a_track_after_a_reused_load_rebuilds_every_lane() {
    use super::*;
    let text = r#"@project "reuse then grow"
  tempo: 120
@track lead | instrument: synth/lead_square
  @phrase p [1:1 - 3:1]
    C4 q mf
"#;
    let mut project = oc_dsl::parse(text).unwrap();
    oc_dsl::instruments::apply_defaults(&mut project);
    let handle = RenderThreadHandle::spawn(48_000);
    let load = |project: &Project| {
        handle.send(RenderCommand::ProjectChanged {
            project: Box::new(project.clone()),
            soundfont_path: PathBuf::new(),
            root: None,
            warm: true,
        });
        handle
            .wait_project_loaded(std::time::Duration::from_secs(5))
            .unwrap();
    };
    load(&project);
    let first = handle
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);

    // A content-only edit reuses the lane.
    let mut edited = project.clone();
    edited.tracks[0].phrases[0].events[0].start_beat = 0.5;
    load(&edited);
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        first
    );

    // A second track changes the lane count, so both lanes are rebuilt.
    let mut grown = edited.clone();
    let mut extra = grown.tracks[0].clone();
    extra.name = "lead 2".into();
    extra.phrases.clear();
    extra.patterns.clear();
    extra.inline_patterns.clear();
    extra.clips = oc_dsl::ast::ClipPool::new();
    grown.tracks.push(extra);
    oc_dsl::instruments::apply_defaults(&mut grown);
    load(&grown);
    assert_eq!(
        handle
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        first + 2,
        "both lanes were rebuilt once"
    );
}
