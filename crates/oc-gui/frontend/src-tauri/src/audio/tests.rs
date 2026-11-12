use std::sync::atomic::Ordering;

#[test]
fn the_bundled_gm_bank_resolves_from_the_macos_resource_directory() {
    let root = std::env::temp_dir().join(format!("ocp-bundled-font-{}", std::process::id()));
    let exe_dir = root.join("Contents/MacOS");
    let resource_dir = root.join("Contents/Resources/soundfonts");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&exe_dir).unwrap();
    std::fs::create_dir_all(&resource_dir).unwrap();
    let bank = resource_dir.join("FluidR3Mono_GM.sf3");
    std::fs::write(&bank, b"RIFF").unwrap();

    let expected = bank.canonicalize().unwrap();
    assert_eq!(
        super::default_soundfont_in(Some(&exe_dir), &root),
        Some(expected)
    );

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_ab_pair_reads_the_selected_take_at_the_same_cursor() {
    let playback = super::AudioPlayback {
        buffer: vec![0.25; 16],
        alt: Some(vec![0.75; 16]),
        cursor: std::sync::atomic::AtomicU64::new(0),
        playing: std::sync::atomic::AtomicBool::new(true),
        side: std::sync::atomic::AtomicBool::new(false),
        first_block: crate::first_block::FirstBlock::new(),
        stream_error: Default::default(),
    };
    let mut output = [0.0; 8];
    super::audio_callback(&mut output, &playback);
    assert_eq!(output, [0.25; 8]);
    playback.side.store(true, Ordering::Release);
    super::audio_callback(&mut output, &playback);
    assert_eq!(output, [0.75; 8], "the switch reads the other take");
    assert_eq!(
        playback.cursor.load(Ordering::Acquire),
        16,
        "switching did not restart the shared cursor"
    );
}

#[test]
fn the_monitor_gate_mutes_an_editor_preview_exactly_while_playing() {
    let mut audio = super::AudioState::new();
    let gate = audio.monitor_gate();
    assert!(
        gate.load(Ordering::Acquire),
        "a stopped transport is audible"
    );
    audio.set_playback_state(super::PlaybackState::Playing);
    assert!(!gate.load(Ordering::Acquire));
    audio.set_playback_state(super::PlaybackState::Paused);
    assert!(gate.load(Ordering::Acquire));
    audio.set_playback_state(super::PlaybackState::Playing);
    audio.set_playback_state(super::PlaybackState::Stopped);
    assert!(gate.load(Ordering::Acquire));
}

fn attach_live_synth(
    audio: &mut super::AudioState,
    preset: &str,
    producer: rtrb::Producer<crate::live_synth::Command>,
) {
    audio.live_synth = Some(preset.into());
    *audio.live_route.lock().unwrap() =
        Some(crate::live_synth::CommandProducer::test_producer(producer));
}

#[test]
fn editing_a_parent_invalidates_only_a_changed_inherited_keyboard_patch() {
    let project = oc_dsl::parse(
        "@project\n@preset parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.2, osc.a.level: 0.3}\n@preset child\n  extends: parent\n  params: {output.gain: 0.7}\n",
    )
    .unwrap();
    let mut audio = super::AudioState::new();
    let (producer, _consumer) = rtrb::RingBuffer::new(4);
    attach_live_synth(&mut audio, "child", producer);
    let mut edited = project.clone();
    edited.presets[0].params[0].1 = oc_dsl::ast::ParamValue::Number(0.5);
    audio.invalidate_inherited_live_synth(&project, &edited);
    assert_eq!(audio.live_synth.as_deref(), Some("child"));
    edited.presets[0].params[1].1 = oc_dsl::ast::ParamValue::Number(0.5);
    audio.invalidate_inherited_live_synth(&project, &edited);
    assert!(audio.live_synth.is_none());
}

#[test]
fn live_synth_stop_and_overflow_release_queue() {
    let mut audio = super::AudioState::new();
    let (mut producer, _consumer) = rtrb::RingBuffer::new(1);
    producer
        .push(crate::live_synth::Command::On(60, 100))
        .ok()
        .unwrap();
    attach_live_synth(&mut audio, "bass", producer);
    audio.invalidate_live_synth("pad");
    assert!(audio.live_synth.is_some());
    assert!(audio.live_synth_note_off("bass", 60).is_err());
    assert!(audio.live_synth.is_none());
    let (producer, _consumer) = rtrb::RingBuffer::new(1);
    attach_live_synth(&mut audio, "bass", producer);
    audio.stop();
    assert!(audio.live_synth.is_none());
}

#[test]
fn project_restore_ends_keyboard_audition() {
    let mut audio = super::AudioState::new();
    let (producer, _consumer) = rtrb::RingBuffer::new(8);
    attach_live_synth(&mut audio, "lead", producer);
    audio.end_live_synth();
    assert!(audio.live_synth.is_none());
    audio.end_live_synth(); // Safe without an active audition.
    assert!(audio.live_synth.is_none());
}

#[test]
fn repeated_keyboard_notes_reuse_patch_until_invalidated() {
    let mut audio = super::AudioState::new();
    let (producer, mut consumer) = rtrb::RingBuffer::new(8);
    attach_live_synth(&mut audio, "lead", producer);
    for note in [60, 64, 67] {
        audio
            .live_synth_note_on(
                "lead",
                || panic!("loaded an active patch again"),
                120.0,
                note,
                90,
            )
            .unwrap();
        assert!(
            matches!(consumer.pop().unwrap(), crate::live_synth::Command::On(n, 90) if n == note)
        );
    }
    // Failed replacement must not interrupt the previous playable instrument.
    assert!(
        audio
            .live_synth_note_on("other", || Err("missing resource".into()), 120.0, 60, 90)
            .is_err()
    );
    assert_eq!(audio.live_synth.as_deref(), Some("lead"));
    audio.end_live_synth();
    let error = audio
        .live_synth_note_on("lead", || Err("reload attempted".into()), 120.0, 60, 90)
        .unwrap_err();
    assert_eq!(error, "reload attempted");
    assert!(audio.live_synth.is_none());
}
#[test]
fn unsupported_performance_uses_checked_backend_admission_not_soundfont_preload() {
    let project = oc_dsl::parse("@project\n@track t | instrument: sample/take\n  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 37; }\n").unwrap();
    assert!(super::project_needs_plugin_aware_render(&project));
    let mut state = super::AudioState::new();
    state.preload_project(&project);
    assert!(state.render_thread.is_none());
    let error = state
        .render_for_playback(&project, &mut std::collections::HashMap::new())
        .unwrap_err();
    assert!(error.contains("explicit CLAP preset"), "{error}");
}

#[test]
#[ignore = "requires OCP_TEST_CLAP pointing to the local Dexed CLAP bundle"]
fn clap_performance_cached_playback_seek_and_dirty_replay() {
    use oc_host::clap_instance::ClapInstance;
    let path = std::env::var("OCP_TEST_CLAP").expect("OCP_TEST_CLAP");
    let project = oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    1:2 = 60 bpm\n@preset p\n  source: clap\n  plugin: \"com.digital-suburban.dexed\"\n@track t | preset: p, instrument: piano/acoustic_grand\n  performance: { @0.125 ch0 note_on 60 90; @0.75 ch0 bend 4096; @1.125 ch0 note_off 60 37; }\n").unwrap();
    assert!(super::project_needs_plugin_aware_render(&project));
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut state = super::AudioState::new();
    state.sample_rate = 48000;
    state.soundfont_path = None;
    state.preload_project(&project);
    assert!(
        state.render_thread.is_none(),
        "must not launch SoundFont substitution"
    );
    let mut instances = std::collections::HashMap::new();
    instances.insert(
        "p".into(),
        ClapInstance::load(
            std::path::Path::new(&path),
            "com.digital-suburban.dexed",
            48000,
            256,
        )
        .unwrap(),
    );
    let authored_state = instances["p"].state_bytes().unwrap();
    let reference = state.render_for_playback(&project, &mut instances).unwrap();
    assert_eq!(authored_state, instances["p"].state_bytes().unwrap());
    assert!(reference.iter().all(|s| s.is_finite()));
    assert!(reference.iter().any(|s| s.abs() > 0.001));
    state.playback = Some(std::sync::Arc::new(super::AudioPlayback {
        buffer: reference.clone(),
        alt: None,
        cursor: std::sync::atomic::AtomicU64::new(0),
        playing: std::sync::atomic::AtomicBool::new(true),
        side: std::sync::atomic::AtomicBool::new(false),
        first_block: crate::first_block::FirstBlock::new(),
        stream_error: Default::default(),
    }));
    for quarter in [1.125, 0.2, 0.0, 0.75, 1.25] {
        state.seek(quarter, &clock).unwrap();
        let start = clock.sample_at_quarter(quarter, 48000).unwrap() as usize * 2;
        let mut output = [0.0; 512];
        super::audio_callback(&mut output, state.playback.as_ref().unwrap());
        assert_eq!(output.as_slice(), &reference[start..start + 512]);
    }
    state.mark_dirty();
    let replay = state.render_for_playback(&project, &mut instances).unwrap();
    assert_eq!(reference.len(), replay.len());
    let error = reference
        .iter()
        .zip(&replay)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        error <= 1e-6,
        "dirty replay changed performance audio: {error}"
    );
    assert_eq!(authored_state, instances["p"].state_bytes().unwrap());
    state.pause();
    let mut output = [1.0; 512];
    super::audio_callback(&mut output, state.playback.as_ref().unwrap());
    assert!(output.iter().all(|s| *s == 0.0));
    state.stop();
    assert_eq!(
        state
            .playback
            .as_ref()
            .unwrap()
            .cursor
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    let mut edited = project.clone();
    edited.tracks[0].performance.as_mut().unwrap().events[1].message =
        oc_dsl::performance::ChannelCommand::PitchBend { value: -4096 };
    state.mark_dirty();
    let changed = state.render_for_playback(&edited, &mut instances).unwrap();
    assert!(
        reference
            .iter()
            .zip(&changed)
            .any(|(a, b)| (a - b).abs() > 0.01)
    );
    state.mark_dirty();
    let restored = state.render_for_playback(&project, &mut instances).unwrap();
    assert_eq!(
        reference, restored,
        "restoring authored performance must restore audio"
    );
    assert_eq!(authored_state, instances["p"].state_bytes().unwrap());
}

use super::*;
use oc_dsl::ast::{
    Effect, EffectType, InstrumentPath, Master, MixState, PatternDef, PresetDef, ProjectProperties,
    Track,
};

fn project() -> Project {
    Project {
        name: Some("Test".to_string()),
        ocp_version: Some("0.2.0".to_string()),
        properties: ProjectProperties::default(),
        presets: vec![],
        tracks: vec![],
        patterns: vec![],
        buses: vec![],
        master: None,
        sections: vec![],
        markers: Vec::new(),
        groups: Vec::new(),
    }
}

#[test]
fn solo_preview_preserves_routing_and_audible_insert_bus_master_processing() {
    let p = oc_dsl::parse(
        r#"@project
  tempo: 120
  tempo_map:
    1:3 = 60 bpm
@preset voice
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 250, amplitude: 0.2}
@track a | preset: voice, instrument: synth/lead_square
  mix: output=group
  effects:
    - gain: factor=0.5
  @phrase a [1:2 - 2:1]
    C4 h mf
@track b | preset: voice, instrument: synth/lead_square
  @phrase b [1:1 - 2:1]
    C5 w mf
@bus group
  routes: [master]
  effects:
    - gain: factor=0.5
@master
  effects:
    - gain: factor=0.5
"#,
    )
    .unwrap();
    let mut p = p;
    p.tracks[0].mix.mute = true;
    p.tracks[1].mix.solo = true;
    let solo = solo_preview_project(&p, 0).unwrap();
    assert!(!solo.tracks[0].mix.mute);
    assert!(solo.tracks.iter().all(|t| !t.mix.solo));
    assert!(p.tracks[0].mix.mute && p.tracks[1].mix.solo);
    assert_eq!(solo.tracks.len(), 2);
    assert!(solo.tracks[1].mix.mute);
    assert!(!p.tracks[1].mix.mute);
    let mut dry = solo.clone();
    dry.tracks[0].effects.clear();
    dry.buses[0].effects.clear();
    dry.master.as_mut().unwrap().effects.clear();
    let render = |project: &Project| {
        oc_engine::render::render_to_buffer_with_plugins(
            project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut std::collections::HashMap::new(),
        )
        .unwrap()
    };
    let wet = render(&solo);
    let dry = render(&dry);
    assert!(wet[..8000].iter().all(|s| *s == 0.0)); // actual playback retains leading half-second
    assert!(wet.iter().any(|s| s.abs() > 0.001));
    assert_eq!(wet, dry.iter().map(|v| v * 0.125).collect::<Vec<_>>());
    assert!(solo_preview_project(&p, 2).is_err());
}

#[test]
fn mapped_transport_seek_and_snapshot_share_render_clock() {
    let project = oc_dsl::parse("@project\n  tempo: 120\n  time_signature: 7/8\n  tempo_map:\n    1:5 = 60 bpm\n  time_sig_map:\n    3:1 = 3/4\n").unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.sample_rate = 8;
    audio.playback = Some(Arc::new(AudioPlayback {
        buffer: vec![0.0; 160],
        alt: None,
        cursor: AtomicU64::new(0),
        playing: AtomicBool::new(false),
        side: AtomicBool::new(false),
        first_block: crate::first_block::FirstBlock::new(),
        stream_error: Default::default(),
    }));
    audio.seek(7.5, &clock).unwrap();
    assert_eq!(
        audio
            .playback
            .as_ref()
            .unwrap()
            .cursor
            .load(Ordering::Acquire),
        104
    );
    let snap = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(snap.position_beats, 7.5);
    assert_eq!(snap.position_seconds, 6.5);
    audio.seek(1.5, &clock).unwrap();
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_seconds,
        0.75
    );
    assert!(audio.seek(f64::NAN, &clock).is_err());
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        1.5
    );
}

#[test]
fn seek_before_first_play_survives_without_a_render_worker() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.seek(64.0, &clock).unwrap();
    assert_eq!(audio.pending_seek_frame, Some(32 * 44100));
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        64.0
    );
    audio.mark_dirty();
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        64.0
    );
    audio.stop();
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        0.0
    );
}

#[test]
fn native_preset_audition_uses_instrument_with_note_off() {
    let audition = preset_audition_project(&native_test_project(), "native").unwrap();
    assert!(supports_block_sources(&audition));
    let rendered = oc_engine::render::render_to_buffer_with_plugins_at(
        &audition,
        std::path::Path::new(""),
        8000,
        &mut std::collections::HashMap::new(),
        None,
    )
    .unwrap();
    assert!(rendered.iter().all(|v| v.is_finite()));
    assert!(rendered.iter().any(|v| v.abs() > 0.001));
    assert!(preset_audition_project(&audition, "missing").is_err());
}

#[test]
fn builtin_sampler_and_soundfont_auditions_play_the_scale() {
    let project = oc_dsl::parse(
            "@project\n\
             @preset synth\n  source: builtin\n  plugin: \"builtin.open_synth\"\n\
             @preset kit\n  source: sample\n\
             @preset piano\n  source: soundfont\n  program: 0\n\
             @track lead | preset: synth, instrument: synth/lead_square\n  @phrase held [1:1:0 - 2:1:0]\n    C4 w mf\n",
        )
        .unwrap();
    let figure = crate::audition_figure::notes(false);
    assert!(figure.len() > 1);
    for preset in ["synth", "kit", "piano"] {
        let audition = preset_audition_project(&project, preset).unwrap();
        let events = &audition.tracks[0].phrases[0].events;
        assert_eq!(events.len(), figure.len(), "preset {preset}");
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].pitch.to_midi() < pair[1].pitch.to_midi()),
            "preset {preset} must audition an ascending figure"
        );
        assert_eq!(audition.properties.tempo, crate::audition_figure::TEMPO_BPM);
        assert_eq!(events[1].start_beat, crate::audition_figure::STEP_QUARTERS);
    }
}

#[test]
fn soundfont_program_audition_renders_the_scale_and_a_drum_groove() {
    let dir = oc_test_support::temp_dir("ocp-audition-sf2", "font");
    let font = dir.join("minimal-test.sf2");
    std::fs::write(
        &font,
        include_bytes!("../../../../../oc-engine/tests/fixtures/minimal-test.sf2"),
    )
    .unwrap();
    let sample_rate = 8000;
    let melodic = AudioState::render_program_audition(&font, sample_rate, 0, 0).unwrap();
    assert_eq!(
        melodic.len(),
        crate::audition_figure::total_frames(sample_rate, false) as usize * 2
    );
    assert!(
        melodic.iter().any(|sample| sample.abs() > 0.001),
        "the audition figure must sound"
    );
    // The GM drum channel gets the groove rather than pitches.
    let drums = AudioState::render_program_audition(&font, sample_rate, 0, 9).unwrap();
    assert_eq!(
        drums.len(),
        crate::audition_figure::total_frames(sample_rate, true) as usize * 2
    );
}

#[test]
fn note_taking_kinds_route_their_audition_through_the_figure() {
    fn assert_voice<T: FigureVoice>() {}
    // No hosted plugin binary is part of the test tree, so the note stream
    // these kinds receive is covered by the figure's sequencer test.
    assert_voice::<oc_host::clap_instance::ClapInstance>();
    assert_voice::<oc_host::vst3_instance::Vst3Instance>();
    assert_voice::<ProgramVoice<'static>>();
}

fn wait_for_audio(mut condition: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "audio lifecycle timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// A device rate change drops the render thread. The seam is rebuilt from
/// the same beats at the new rate, and the pass after it starts on audio
/// rather than at the frames the old rate left behind.
#[test]
fn a_device_rate_change_recomputes_the_loop_seam_from_the_same_beats() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.preload_project(&project);
    audio
        .set_loop_region(Some(1.0), Some(3.0), true, &clock)
        .unwrap();
    // Two quarters at 120 bpm are 1 s, here 8000 frames.
    assert_eq!(audio.active_loop_frames(), Some((4_000, 12_000)));

    // What `sync_sample_rate` does when the device reports a new rate: the
    // stream and the render thread go, the rate is adopted, and the next
    // load rebuilds.
    audio.pause();
    audio.drop_stream();
    audio.render_thread = None;
    audio.playback = None;
    audio.render_dirty = true;
    audio.sample_rate = 16_000;
    audio.preload_project(&project);

    // The same beats now occupy twice the frames, and the seam is pushed at
    // the new rate before any audio is heard.
    let playback = std::sync::Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    assert_eq!(
        playback.loop_region(),
        Some(crate::render_thread::LoopRegion {
            start: 8_000,
            end: 24_000
        })
    );
    // The rebuild finishes in the background and publishes its length.
    wait_for_audio(|| playback.total_frames.load(Ordering::Acquire) >= 24_000);
    assert_eq!(audio.active_loop_frames(), Some((8_000, 24_000)));
    assert_eq!(playback.loop_wrap(23_999, 1 << 40), None);
    assert_eq!(
        playback.loop_wrap(24_000, 1 << 40),
        Some(8_000),
        "the pass wraps at the new frame of the same beat"
    );

    // Roll the last 800 frames into the seam and keep listening: the block
    // after the wrap carries the tone, so the pass did not start on silence.
    audio.seek(2.9, &clock).unwrap();
    let mut plugins = std::collections::HashMap::new();
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| playback.loop_seam().is_some());
    assert_eq!(
        playback.loop_seam(),
        Some(crate::render_thread::LoopRegion {
            start: 8_000,
            end: 24_000
        }),
        "the wrapped pass describes the new seam"
    );
    let heard = audio.simulated_output_heard_blocks();
    wait_for_audio(|| audio.simulated_output_heard_blocks() > heard);
    let cursor = playback.play_cursor_frames.load(Ordering::Acquire);
    assert!(
        (8_000..24_000).contains(&cursor),
        "the playhead is inside the new pass, not the old frames: {cursor}"
    );
    audio.stop();
}

#[test]
fn simulated_device_transport_and_audition_lifecycle() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    let mut plugins = std::collections::HashMap::new();
    audio.seek(1.0, &clock).unwrap();
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 1.0);
    audio.pause();
    wait_for_audio(|| {
        !audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .active
            .load(Ordering::Acquire)
    });
    let paused = audio.transport_snapshot(&clock).unwrap().position_beats;
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > paused);
    audio.seek(0.25, &clock).unwrap();
    wait_for_audio(|| {
        let p = audio.transport_snapshot(&clock).unwrap().position_beats;
        p > 0.25 && p < 1.0
    });
    audio.pause();
    audio.seek(2.0, &clock).unwrap();
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        2.0
    );
    audio.mark_dirty();
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 2.0);
    audio.stop();
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        0.0
    );
    audio
        .preview_track(&project, 0, &mut plugins, None)
        .unwrap();
    assert!(
        audio.render_dirty,
        "main mix must be restored after audition"
    );
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 0.0);
    audio.seek(200.0, &clock).unwrap();
    audio
        .preview_track(&project, 0, &mut plugins, None)
        .unwrap();
    let restarted = audio.transport_snapshot(&clock).unwrap();
    assert!(restarted.playing && restarted.position_beats < 1.0);
    audio.stop();
    audio.play(&project, &mut plugins).unwrap();
    assert!(!audio.render_dirty);
    audio.reset_project();
    assert!(audio.simulated_output.is_none());
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        0.0
    );
}

#[test]
fn simulated_scrub_mute_advances_clock_and_acknowledges_seek() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    audio.mute();
    audio.seek(1.0, &clock).unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 1.0);
    audio.unmute();
    audio.reset_project();
}

#[test]
#[ignore = "requires OCP_AUDITION_PROJECT; real project / all presets and tracks with simulated device"]
fn review_project_audition_lifecycle() {
    let path = PathBuf::from(std::env::var_os("OCP_AUDITION_PROJECT").unwrap());
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.resource_root = path.parent().map(std::path::Path::to_path_buf);
    let mut plugins = std::collections::HashMap::new();
    for preset in &project.presets {
        let audition = preset_audition_project(&project, &preset.name).unwrap();
        audio.stop();
        let start = std::time::Instant::now();
        audio
            .preview_track(&audition, 0, &mut plugins, path.parent())
            .unwrap();
        assert!(
            audio
                .render_thread
                .as_ref()
                .unwrap()
                .playback
                .ready
                .load(Ordering::Acquire)
        );
        wait_for_audio(|| {
            audio
                .simulated_output
                .as_ref()
                .unwrap()
                .nonzero
                .load(Ordering::Acquire)
        });
        wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 0.0);
        eprintln!(
            "preset={} ready_ms={}",
            preset.name,
            start.elapsed().as_millis()
        );
    }
    for (index, track) in project.tracks.iter().enumerate() {
        audio.stop();
        audio.seek(64.0, &clock).unwrap();
        let start = std::time::Instant::now();
        audio
            .preview_track(&project, index, &mut plugins, path.parent())
            .unwrap();
        assert!(
            audio
                .render_thread
                .as_ref()
                .unwrap()
                .playback
                .ready
                .load(Ordering::Acquire)
        );
        wait_for_audio(|| {
            let position = audio.transport_snapshot(&clock).unwrap().position_beats;
            position > 0.0 && position < 4.0
        });
        wait_for_audio(|| {
            audio
                .simulated_output
                .as_ref()
                .unwrap()
                .nonzero
                .load(Ordering::Acquire)
        });
        eprintln!(
            "track={} restart_ready_ms={}",
            track.name,
            start.elapsed().as_millis()
        );
    }
    audio.stop();
    audio.seek(1.0, &clock).unwrap();
    audio.play(&project, &mut plugins).unwrap();
    assert!(!audio.render_dirty);
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 1.0);
    audio.reset_project();
}

#[test]
fn transport_intent_gate_silences_without_acquiring_project_lock() {
    struct Probe(std::sync::atomic::AtomicU64);
    impl RenderSink for Probe {
        fn fill(&self, output: &mut [f32]) {
            self.0.fetch_add(1, Ordering::Relaxed);
            output.fill(0.25);
        }
        fn failed(&self, _: String) {}
    }
    let state = crate::state::AppState::new();
    let _held = state.lock_inner();
    let probe = Arc::new(Probe(std::sync::atomic::AtomicU64::new(0)));
    let sink = IntentGatedSink {
        sink: probe.clone(),
        gate: state.transport_pause_intent.clone(),
    };
    state.transport_pause_intent.store(true, Ordering::Release);
    let mut output = [1.0; 64];
    sink.fill(&mut output);
    assert!(output.iter().all(|sample| *sample == 0.0));
    assert_eq!(probe.0.load(Ordering::Relaxed), 0);
    state.transport_pause_intent.store(false, Ordering::Release);
    sink.fill(&mut output);
    assert!(output.iter().all(|sample| *sample == 0.25));
    assert_eq!(probe.0.load(Ordering::Relaxed), 1);
}

#[test]
fn private_block_stream_retains_window_bound_after_pause() {
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    let project = native_test_project();
    let spec = audio.audio_io.spec();
    let start = u64::from(spec.sample_rate) / 4;
    let end = u64::from(spec.sample_rate);
    let prepared = AudioState::open_audition_render_window(
        &audio.audio_io,
        &spec,
        PathBuf::new(),
        project,
        true,
        now_ns(),
        StartPath::PresetAudition,
        start,
        Some(end),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    assert!(
        prepared
            .thread
            .playback
            .play_cursor_frames
            .load(Ordering::Acquire)
            <= start
    );
    audio.install_audition_render(prepared).unwrap();
    let playback = audio.audition_render.as_ref().unwrap().playback.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while playback.play_cursor_frames.load(Ordering::Acquire) <= start {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    audio.pause_private_audition();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let paused = playback.play_cursor_frames.load(Ordering::Acquire);
    std::thread::sleep(std::time::Duration::from_millis(15));
    assert_eq!(playback.play_cursor_frames.load(Ordering::Acquire), paused);
    audio.resume_private_audition().unwrap();
    while playback.play_cursor_frames.load(Ordering::Acquire) < end {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(playback.play_cursor_frames.load(Ordering::Acquire), end);
    assert!(audio.playback.is_none());
}

#[test]
fn a_suspended_block_preview_resumes_from_its_window_before_the_worker_cursor_updates() {
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    let spec = audio.audio_io.spec();
    let start = u64::from(spec.sample_rate) / 2;
    let end = u64::from(spec.sample_rate);
    let prepared = AudioState::open_audition_render_window(
        &audio.audio_io,
        &spec,
        PathBuf::new(),
        native_test_project(),
        true,
        now_ns(),
        StartPath::ClipPreview,
        start,
        Some(end),
    )
    .unwrap();
    audio.audition_pause_intent.store(true, Ordering::Release);
    audio.install_audition_render(prepared).unwrap();
    let playback = audio.audition_render.as_ref().unwrap().playback.clone();
    assert!(!audio.audition_active());
    assert!(playback.output_suspended.load(Ordering::Acquire));

    // A resume can arrive before the render worker publishes its first cursor.
    playback.play_cursor_frames.store(0, Ordering::Release);
    audio.audition_pause_intent.store(false, Ordering::Release);
    audio.resume_private_audition().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let frame = playback.play_cursor_frames.load(Ordering::Acquire);
        if frame != 0 {
            assert!(
                frame >= start,
                "block resumed outside its window at frame {frame}"
            );
            if frame > start {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(playback.play_cursor_frames.load(Ordering::Acquire) <= end);
}

#[test]
fn drum_pad_preview_streams_privately_and_keeps_arrangement_cursor() {
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    let project =
        oc_dsl::parse("@project\n@kit drums\n  pad kick | note: 36, voice: kick\n").unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    audio.seek(6.0, &clock).unwrap();
    let io = Arc::clone(&audio.audio_io);
    let spec = io.spec();
    let open_pad = |note| {
        AudioState::open_drum_pad_preview(
            &io,
            &spec,
            true,
            now_ns(),
            DrumPadPreview {
                params: &project.presets[0].params,
                root: None,
                note,
                velocity: 110,
            },
        )
    };
    audio.replace_private_audition(true).unwrap();
    audio
        .install_audition_render(open_pad(36).unwrap())
        .unwrap();
    let render = audio.audition_render.as_ref().unwrap();
    assert!(render.playback.output_suspended.load(Ordering::Acquire));
    assert!(audio.audition_playback.is_none() && audio.playback.is_none());
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        6.0
    );
    assert!(open_pad(99).is_err());
    assert!(!audio.audition_active());
    audio.resume_private_audition().unwrap();
    let playback = Arc::clone(&audio.audition_render.as_ref().unwrap().playback);
    wait_for_audio(|| playback.play_cursor_frames.load(Ordering::Acquire) > 0);
    audio.stop_audition();
}

fn native_test_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Native GUI regression"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@track synth | preset: native, instrument: synth/lead_square
  @phrase note [1:1:0 - 2:1:0]
    C4 w mf
"#,
    )
    .unwrap()
}

fn pattern_test_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Pattern GUI regression"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@pattern hook | preset: native, instrument: synth/lead_square
  events:
    C4 q mf
    E4 q mf
@track synth | preset: native, instrument: synth/lead_square
  clip: hook [1:1:0 - 2:1:0]
"#,
    )
    .unwrap()
}

fn late_clip_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Late clip GUI regression"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@pattern hook | preset: native, instrument: synth/lead_square
  events:
    C4 q mf
@track late | preset: native, instrument: synth/lead_square
  clip: hook [3:1:0 - 5:1:0]
"#,
    )
    .unwrap()
}

fn late_phrase_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Late phrase GUI regression"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@track late | preset: native, instrument: synth/lead_square
  @phrase late_p [5:1:0 - 6:1:0]
    C4 w mf
"#,
    )
    .unwrap()
}

#[test]
fn track_preview_range_covers_the_first_and_last_content() {
    let phrase_project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&phrase_project.properties).unwrap();
    assert_eq!(
        track_preview_range_quarters(&phrase_project, 0, &clock).unwrap(),
        Some((16.0, 20.0))
    );

    let clip_project = late_clip_project();
    assert_eq!(
        track_preview_range_quarters(&clip_project, 0, &clock).unwrap(),
        Some((8.0, 16.0))
    );
}

#[test]
fn track_preview_range_follows_section_repeats() {
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Repeated preview"
  tempo: 120
  time_signature: 4/4
@section body [1:1 - 3:1]
  repeat: 2
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@track synth | preset: native, instrument: synth/lead_square
  @phrase p [1:1:0 - 2:1:0]
    C4 w mf
"#,
    )
    .unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    // Each pass is 8 quarters; the phrase's 4 quarters land at 0 and 8.
    assert_eq!(
        track_preview_range_quarters(&project, 0, &clock).unwrap(),
        Some((0.0, 12.0))
    );
}

#[test]
fn track_preview_range_is_empty_without_content() {
    let mut project = native_test_project();
    project.tracks[0].phrases.clear();
    project.tracks[0].patterns.clear();
    project.tracks[0].clips.clear();
    project.tracks[0].performance = None;
    project.clear_audio_clips();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    assert_eq!(
        track_preview_range_quarters(&project, 0, &clock).unwrap(),
        None
    );
}

#[test]
fn play_after_an_effect_drain_restarts_the_arrangement_at_zero() {
    let mut project = late_phrase_project();
    project.presets[0].effects.push(oc_dsl::ast::Effect {
        effect_type: oc_dsl::ast::EffectType::Saturator,
        params: vec![],
        bypass: false,
        sidechain_source: None,
    });
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    audio.sample_rate = 8_000;
    let mut instances = std::collections::HashMap::new();
    audio.play(&project, &mut instances).unwrap();
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    wait_for_audio(|| playback.has_played_out());
    let ended = audio.transport_snapshot(&clock).unwrap();
    assert!(!ended.playing);
    assert_eq!(ended.position_seconds, ended.duration_seconds);
    assert!(playback.timeline_frames() > playback.total_frames.load(Ordering::Acquire));

    audio.play(&project, &mut instances).unwrap();
    let starting = audio.transport_snapshot(&clock).unwrap();
    assert!(starting.playing);
    assert!(starting.position_beats < 1.0);
    audio.reset_project();
}

#[test]
fn track_preview_installs_paused_and_resumes_at_the_requested_first_frame() {
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .preview_track_with_intent(
            &project,
            0,
            &mut std::collections::HashMap::new(),
            None,
            true,
        )
        .unwrap();
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    assert!(audio.transport_pause_intent.load(Ordering::Acquire));
    assert!(playback.output_suspended.load(Ordering::Acquire));
    let paused = audio.transport_snapshot(&clock).unwrap();
    assert!(!paused.playing);
    assert_eq!(paused.position_beats, 16.0);
    let first_frame = audio.pending_seek_frame.unwrap();

    // An unapplied worker cursor from the previous pass cannot supply the resume origin.
    playback.play_cursor_frames.store(0, Ordering::Release);
    audio.resume_track_preview(0).unwrap();
    assert!(!audio.transport_pause_intent.load(Ordering::Acquire));
    assert_eq!(audio.pending_start.unwrap().frame, first_frame);
    wait_for_audio(|| playback.play_cursor_frames.load(Ordering::Acquire) > first_frame);
    audio.reset_project();
}

#[test]
fn track_preview_rebuilds_at_the_same_time_after_an_output_rate_change() {
    use oc_audio::audio_io::FakeBackend;

    let backend = Arc::new(FakeBackend::new(
        vec![fake_device(
            "test",
            "Test output",
            &[8_000, 16_000],
            Some((128, 1024)),
            true,
        )],
        Some("test".into()),
    ));
    let mut audio = AudioState::with_backend(backend);
    audio.simulate_output = true;
    let selection = |rate| AudioSelection {
        sample_rate: Some(rate),
        ..AudioSelection::default()
    };
    assert!(
        audio
            .set_audio_io(selection(8_000))
            .status
            .failure
            .is_none()
    );
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut instances = std::collections::HashMap::new();

    for start_paused in [true, false] {
        audio
            .preview_track_with_intent(&project, 0, &mut instances, None, start_paused)
            .unwrap();
        let worker = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
        if !start_paused {
            wait_for_audio(|| worker.play_cursor_frames.load(Ordering::Acquire) > 64_000);
        }
        let old_frame = audio.pending_seek_frame.unwrap_or_else(|| {
            audio.pause();
            wait_for_audio(|| !worker.active.load(Ordering::Acquire));
            worker.play_cursor_frames.load(Ordering::Acquire)
        });
        let old_end = audio.track_preview.unwrap().1.unwrap();
        assert!(
            audio
                .set_audio_io(selection(16_000))
                .status
                .failure
                .is_none()
        );
        let paused = audio.transport_snapshot(&clock).unwrap();
        assert!(!paused.playing);
        assert!(paused.track_preview_rebuild);
        assert_eq!(paused.position_seconds, old_frame as f64 / 8_000.0);
        assert_eq!(paused.duration_seconds, old_end as f64 / 8_000.0);
        audio
            .resume_track_preview_for_project(0, &project, &mut instances)
            .unwrap();
        assert_eq!(audio.pending_start.unwrap().frame, old_frame * 2);
        assert_eq!(audio.track_preview, Some((0, Some(old_end * 2))));
        assert!(audio.transport_snapshot(&clock).unwrap().playing);
        audio.stop();
        assert!(
            audio
                .set_audio_io(selection(8_000))
                .status
                .failure
                .is_none()
        );
    }
    audio.reset_project();
}

#[test]
fn paused_arrangement_seek_repositions_the_renderer_before_play() {
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    audio.sample_rate = 8_000;
    let mut instances = std::collections::HashMap::new();
    audio.play(&project, &mut instances).unwrap();
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    wait_for_audio(|| playback.ready.load(Ordering::Acquire));
    audio.pause();

    audio.seek(16.0, &clock).unwrap();
    let frame = clock.sample_at_quarter(16.0, audio.sample_rate).unwrap();
    let requested = playback.position_requested.load(Ordering::Acquire);
    wait_for_audio(|| {
        playback.position_applied.load(Ordering::Acquire) == requested
            && playback.play_cursor_frames.load(Ordering::Acquire) == frame
    });
    assert!(!playback.active.load(Ordering::Acquire));
    assert_eq!(audio.pending_seek_frame, Some(frame));
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        16.0
    );

    audio.seek(0.0, &clock).unwrap();
    audio.play(&project, &mut instances).unwrap();
    wait_for_audio(|| playback.ready.load(Ordering::Acquire));
    assert!(audio.transport_snapshot(&clock).unwrap().position_beats < 1.0);
    audio.reset_project();
}

#[test]
fn pause_retains_the_latest_unacknowledged_seek_when_the_worker_cursor_is_stale() {
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    audio.sample_rate = 8_000;
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    let rt = audio.render_thread.as_ref().unwrap();
    let playback = Arc::clone(&rt.playback);
    let (entered, wait) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    rt.send(RenderCommand::TestBarrier {
        entered,
        release: blocked,
    });
    wait.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();

    audio.seek(17.0, &clock).unwrap();
    audio.seek(18.0, &clock).unwrap();
    // An older position command can replace the optimistically published cursor.
    playback.play_cursor_frames.store(68_000, Ordering::Release);
    audio.pause();
    let paused = audio.transport_snapshot(&clock).unwrap();
    release.send(()).unwrap();
    assert_eq!(paused.position_beats, 18.0);
    audio.adopt_sample_rate(16_000);
    assert_eq!(audio.pending_seek_frame, Some(144_000));
}

#[test]
fn track_preview_resume_keeps_graph_cursor_and_end_bound() {
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 16.1);
    audio.pause();
    let pb = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    wait_for_audio(|| !pb.active.load(Ordering::Acquire));
    let frame = pb.play_cursor_frames.load(Ordering::Acquire);
    let loads = pb.project_loads.load(Ordering::Acquire);
    let instruments = pb.instrument_instantiations.load(Ordering::Acquire);
    let duration = audio.transport_snapshot(&clock).unwrap().duration_seconds;
    let bound = audio.track_preview;
    assert!(audio.resume_track_preview(1).is_err());
    assert_eq!(audio.playback_state, PlaybackState::Paused);
    audio.resume_track_preview(0).unwrap();
    assert!(
        audio.render_dirty,
        "main transport must still restore the arrangement"
    );
    wait_for_audio(|| pb.play_cursor_frames.load(Ordering::Acquire) > frame);
    assert_eq!(pb.project_loads.load(Ordering::Acquire), loads);
    assert_eq!(
        pb.instrument_instantiations.load(Ordering::Acquire),
        instruments
    );
    assert_eq!(audio.track_preview, bound);
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().duration_seconds,
        duration
    );
    audio.stop();
    assert!(audio.resume_track_preview(0).is_err());
    audio.reset_project();
}

#[test]
fn track_preview_resume_rejects_invalidated_or_finished_identity() {
    let mut audio = AudioState::new();
    audio.playback_state = PlaybackState::Paused;
    assert!(audio.resume_track_preview(0).is_err());
    audio.track_preview = Some((0, Some(100)));
    audio.mark_dirty();
    assert!(audio.track_preview.is_none());
    audio.track_preview = Some((0, Some(100)));
    audio.apply_mixer_change(&native_test_project());
    assert!(audio.track_preview.is_none());
    audio.track_preview = Some((0, Some(100)));
    audio.stop();
    assert!(audio.track_preview.is_none());
}

#[test]
fn track_preview_resume_is_invalid_after_main_transport_play() {
    let project = native_test_project();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap();
    audio.pause();
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    audio.pause();
    assert!(audio.track_preview.is_none());
    assert!(audio.resume_track_preview(0).is_err());
    audio.reset_project();
}

#[test]
fn track_preview_starts_transport_at_the_first_content() {
    let project = late_phrase_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.seek(190.0, &clock).unwrap();
    audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap();
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert!(snapshot.playing);
    assert!(
        (snapshot.position_beats - 16.0).abs() < 0.001,
        "preview should start at bar 5, got {}",
        snapshot.position_beats
    );
    audio.reset_project();
}

#[test]
fn track_preview_stops_at_the_last_content_plus_the_tail() {
    // One clip at the start; the section leaves ~60 s of empty bars after it.
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Empty tail preview"
  tempo: 120
  time_signature: 4/4
@section arrangement [1:1 - 33:1]
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@pattern hook | preset: native, instrument: synth/lead_square
  events:
    C4 q mf
@track synth | preset: native, instrument: synth/lead_square
  clip: hook [1:1:0 - 2:1:0]
"#,
    )
    .unwrap();
    assert!(
        crate::project_extent::section_seconds(&project).unwrap() > 60.0,
        "the fixture must have a long empty tail"
    );
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = substituting_audio(8_000);
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap();
    // The clip ends at 2 s, so the bound adds the tail instead of the section.
    let expected = 2.0 + PREVIEW_TAIL_SECONDS;
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().duration_seconds > 0.0);
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    let applied = playback.play_applied.load(Ordering::Acquire);
    let view = audio.set_audio_io(AudioSelection {
        buffer_frames: Some(256),
        ..AudioSelection::default()
    });
    assert!(view.status.failure.is_none());
    wait_for_audio(|| playback.play_applied.load(Ordering::Acquire) > applied);
    let duration = audio.transport_snapshot(&clock).unwrap().duration_seconds;
    assert!(
        (duration - expected).abs() < 0.001,
        "preview lasted {duration} s instead of {expected} s"
    );
    wait_for_audio(|| !audio.transport_snapshot(&clock).unwrap().playing);
    let stalled = audio.transport_snapshot(&clock).unwrap().position_seconds;
    assert!(
        stalled <= expected + 0.001,
        "preview ran to {stalled} s past the bound"
    );
    assert!(
        stalled > expected - 1.0,
        "preview stalled at {stalled} s before the bound"
    );
    audio.reset_project();
}

#[test]
fn track_preview_of_a_late_clip_starts_and_ends_at_that_clip() {
    let project = late_clip_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.seek(190.0, &clock).unwrap();
    audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap();
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert!(snapshot.playing);
    assert!(
        (snapshot.position_beats - 8.0).abs() < 0.001,
        "preview should start at bar 3, got {}",
        snapshot.position_beats
    );
    // Bars 3-5 are 8 s at 120 bpm; the effect tail follows the clip end.
    let expected = 8.0 + PREVIEW_TAIL_SECONDS;
    wait_for_audio(|| {
        let duration = audio.transport_snapshot(&clock).unwrap().duration_seconds;
        (duration - expected).abs() < 0.001
    });
    audio.reset_project();
}

#[test]
fn block_preview_window_uses_the_passed_range_only() {
    let project = late_clip_project();
    let bar3 = BarBeat::new(3, 1, 0);
    let bar5 = BarBeat::new(5, 1, 0);
    let (start, end) = block_preview_sample_range(&project, 0, bar3, bar5, 8000).unwrap();
    // Bar 3 is quarter 8 (4 s at 120 bpm); bar 5 is quarter 16 (8 s).
    assert_eq!(start, 32_000);
    assert_eq!(end, 64_000 + (PREVIEW_TAIL_SECONDS * 8000.0) as u64);
    assert!(block_preview_sample_range(&project, 0, bar5, bar3, 8000).is_err());
    assert!(block_preview_sample_range(&project, 0, bar3, bar3, 8000).is_err());
    assert!(block_preview_sample_range(&project, 1, bar3, bar5, 8000).is_err());
}

#[test]
fn block_preview_window_covers_a_phrase_and_a_single_bar() {
    let project = late_phrase_project();
    let (start, end) = block_preview_sample_range(
        &project,
        0,
        BarBeat::new(5, 1, 0),
        BarBeat::new(6, 1, 0),
        8000,
    )
    .unwrap();
    // Bar 5 is quarter 16 (8 s at 120 bpm); bar 6 is quarter 20 (10 s), plus
    // the preview's ring-out allowance.
    assert_eq!(start, 64_000);
    assert_eq!(end, 80_000 + (PREVIEW_TAIL_SECONDS * 8000.0) as u64);
}

#[test]
fn block_preview_renders_starting_at_the_block() {
    let project = late_clip_project();
    let buffer = AudioState::render_block_window(
        &project,
        0,
        BarBeat::new(3, 1, 0)..BarBeat::new(5, 1, 0),
        8000,
        std::path::Path::new(""),
        &mut std::collections::HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(
        buffer.len(),
        (64_000 - 32_000 + (PREVIEW_TAIL_SECONDS * 8000.0) as usize) * 2
    );
    assert!(
        buffer[..2_000].iter().any(|sample| sample.abs() > 0.001),
        "the block window must open on the block's own notes"
    );
}

#[test]
fn block_preview_auditions_without_moving_the_transport_cursor() {
    let project = pattern_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.seek(2.0, &clock).unwrap();
    let before = audio.transport_snapshot(&clock).unwrap();

    // The command now renders off the UI thread and then plays the buffer;
    // the test exercises the same two steps.
    let mut instances = std::collections::HashMap::new();
    let buffer = AudioState::render_block_window(
        &project,
        0,
        BarBeat::new(1, 1, 0)..BarBeat::new(3, 1, 0),
        8000,
        Path::new(""),
        &mut instances,
        None,
    )
    .unwrap();
    audio.play_audition_buffer(buffer).unwrap();
    assert!(
        audio.audition_active(),
        "the audition stream should be active"
    );
    let during = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(during.position_beats, before.position_beats);
    assert!(!during.playing);

    wait_for_audio(|| audio.heard_audition_blocks() > 0);
    audio.stop_audition();
    assert!(!audio.audition_active());
    let after = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(after.position_beats, before.position_beats);
    assert!(!after.playing);
}

#[test]
fn pattern_audition_project_plays_only_the_selected_pattern() {
    let project = pattern_test_project();
    let audition = pattern_audition_project(&project, "hook").unwrap();
    assert_eq!(audition.tracks.len(), 1);
    assert_eq!(audition.tracks[0].inline_patterns.len(), 1);
    assert_eq!(audition.tracks[0].inline_patterns[0].name, "hook");
    assert!(audition.tracks[0].pattern_clip_count() == 0);
    assert!(audition.tracks[0].phrases.is_empty());
    assert!(audition.patterns.is_empty());
    assert!(pattern_audition_project(&project, "missing").is_err());
}

#[test]
fn pattern_audition_keeps_the_pattern_transposition() {
    let mut project = pattern_test_project();
    project.patterns[0].transpose = Some(-2);
    let audition = pattern_audition_project(&project, "hook").unwrap();
    assert_eq!(audition.tracks[0].inline_patterns[0].transpose, Some(-2));
    let events = oc_engine::interpret::try_interpret(
        &audition,
        &oc_engine::interpret::InterpretSettings {
            sample_rate: 8_000,
            ..Default::default()
        },
    )
    .unwrap();
    let notes = events
        .into_iter()
        .filter_map(|event| match event.event {
            oc_engine::interpret::MidiMessage::NoteOn { note, .. } => Some(note),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(notes, vec![58, 62], "C4 and E4 sound two semitones lower");
}

#[test]
fn pattern_preview_streams_without_moving_the_transport_cursor() {
    let project = pattern_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.seek(2.0, &clock).unwrap();
    let before = audio.transport_snapshot(&clock).unwrap();

    audio.preview_pattern(&project, "hook", None).unwrap();
    assert!(
        audio.audition_active(),
        "the audition stream should be active"
    );
    let during = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(during.position_beats, before.position_beats);
    assert!(!during.playing);

    wait_for_audio(|| audio.heard_audition_blocks() > 0);
    audio.stop_audition();
    assert!(!audio.audition_active());
    let after = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(after.position_beats, before.position_beats);
    assert!(!after.playing);
}

#[test]
fn opening_project_discards_previous_playback_and_render_cache() {
    let mut audio = AudioState::new();
    audio.playback = Some(Arc::new(AudioPlayback {
        buffer: vec![0.25; 128],
        alt: None,
        cursor: AtomicU64::new(64),
        playing: AtomicBool::new(true),
        side: AtomicBool::new(false),
        first_block: crate::first_block::FirstBlock::new(),
        stream_error: Default::default(),
    }));
    audio.render_cache = Some(RenderCache::new());
    audio.render_dirty = false;
    audio.reset_project();
    assert!(audio.playback.is_none());
    assert!(audio.render_cache.is_none());
    assert!(audio.render_thread.is_none());
    assert!(!audio.streaming_active);
    assert!(audio.render_dirty);
    assert!(matches!(audio.playback_state, PlaybackState::Stopped));
}

fn long_native_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Second native project"
  tempo: 90
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@track lead | preset: native, instrument: synth/lead_square
  @phrase lead [1:1:0 - 5:1:0]
    C4 w mf
@track pad | preset: native, instrument: synth/lead_square
  @phrase pad [1:1:0 - 5:1:0]
    C3 w mf
"#,
    )
    .unwrap()
}

#[test]
fn warm_up_opens_the_stream_and_prefills_the_ring() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.preload_project(&project);
    assert!(
        audio.simulated_output.is_some() && audio.streaming_active,
        "warm-up must start the consumer so the press only enables it"
    );

    // Every observation before readiness must say warming, never neutral.
    let mut prepared = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        let snapshot = audio.transport_snapshot(&clock).unwrap();
        if snapshot.prepared {
            assert!(!snapshot.warming);
            prepared = true;
            break;
        }
        assert!(
            snapshot.warming,
            "a project under warm-up must report warming"
        );
        assert!(!snapshot.playing);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(prepared, "warm-up never became ready");
    let rt = audio.render_thread.as_ref().unwrap();
    assert!(!rt.playback.active.load(Ordering::Acquire));
    assert!(
        rt.playback.ready.load(Ordering::Acquire),
        "warm-up publishes audio into the ring before any press"
    );
    assert_eq!(rt.playback.play_applied.load(Ordering::Acquire), 0);
    assert!(
        !audio
            .simulated_output
            .as_ref()
            .unwrap()
            .nonzero
            .load(Ordering::Acquire),
        "an idle warm stream outputs silence"
    );
    let instruments = rt
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert_eq!(instruments, 1);

    let playback = Arc::clone(&rt.playback);
    let generation = playback.seek_generation.load(Ordering::Acquire);
    audio.seek(0.0, &clock).unwrap();
    let requested = playback.position_requested.load(Ordering::Acquire);
    wait_for_audio(|| playback.position_applied.load(Ordering::Acquire) == requested);
    assert_eq!(playback.seek_generation.load(Ordering::Acquire), generation);
    assert!(playback.ready.load(Ordering::Acquire));

    // The first press only starts the consumer.
    let mut plugins = std::collections::HashMap::new();
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| {
        audio
            .simulated_output
            .as_ref()
            .unwrap()
            .nonzero
            .load(Ordering::Acquire)
    });
    let rt = audio.render_thread.as_ref().unwrap();
    assert_eq!(rt.playback.project_loads.load(Ordering::Acquire), 1);
    assert_eq!(
        rt.playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments,
        "the first press must not rebuild the warmed sources"
    );
    audio.reset_project();
}

#[test]
fn press_during_warm_up_reuses_the_in_flight_build() {
    let project = native_test_project();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.preload_project(&project);
    // Press immediately: the build may still be running.
    let mut plugins = std::collections::HashMap::new();
    audio.play(&project, &mut plugins).unwrap();
    wait_for_audio(|| {
        audio
            .simulated_output
            .as_ref()
            .unwrap()
            .nonzero
            .load(Ordering::Acquire)
    });
    let rt = audio.render_thread.as_ref().unwrap();
    assert_eq!(
        rt.playback.project_loads.load(Ordering::Acquire),
        1,
        "the press must not queue a second project build"
    );
    assert_eq!(
        rt.playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        1
    );
    assert!(audio.streaming_active);
    audio.reset_project();
}

#[test]
fn opening_another_project_replaces_the_warm_up() {
    let first = native_test_project();
    let second = long_native_project();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.preload_project(&first);
    audio
        .render_thread
        .as_ref()
        .unwrap()
        .wait_project_loaded(std::time::Duration::from_secs(5))
        .unwrap();
    let first_frames = audio
        .render_thread
        .as_ref()
        .unwrap()
        .playback
        .total_frames
        .load(Ordering::Acquire);

    // Opening another file drops the worker and its warm ring.
    audio.reset_project();
    assert!(audio.render_thread.is_none());
    assert!(!audio.streaming_active);
    audio.preload_project(&second);
    let clock = oc_dsl::timing::ProjectClock::new(&second.properties).unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().prepared);
    let rt = audio.render_thread.as_ref().unwrap();
    assert_eq!(
        rt.playback.project_loads.load(Ordering::Acquire),
        1,
        "the replacement starts a fresh worker"
    );
    assert_eq!(
        rt.playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        2
    );
    assert!(
        rt.playback.total_frames.load(Ordering::Acquire) > first_frames,
        "the published duration belongs to the second project"
    );
    audio.reset_project();
}

#[test]
fn native_gui_playback_buffer_and_export_need_no_soundfont() {
    let project = native_test_project();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.soundfont_path = None;
    audio.sample_rate = 8000;
    audio.preload_project(&project);
    assert!(audio.render_thread.is_some());
    assert!(!audio.render_dirty);
    audio
        .render_thread
        .as_ref()
        .unwrap()
        .wait_project_loaded(std::time::Duration::from_secs(2))
        .unwrap();
    let mut instances = std::collections::HashMap::new();
    let buffer = audio.render_for_playback(&project, &mut instances).unwrap();
    assert!(!buffer.is_empty());
    assert!(buffer.iter().all(|sample| sample.is_finite()));
    assert!(buffer.iter().any(|sample| sample.abs() > 0.0001));
    let output =
        std::env::temp_dir().join(format!("ocp-native-gui-export-{}.wav", std::process::id()));
    assert!(!output.exists());
    audio
        .render_project_to_wav(&project, &output, &mut instances, None)
        .unwrap();
    assert!(std::fs::metadata(&output).unwrap().len() > 44);
    std::fs::remove_file(output).unwrap();
}

#[test]
fn native_section_reopen_playback_and_export_preserve_final_rest() {
    let mut project = native_test_project();
    project.properties.tempo = 120.0;
    crate::project_extent::tests::add_section(&mut project, 5);
    // Serialize/reopen: the .ocp itself carries the extent, not a sidecar.
    let reopened = oc_dsl::parse(&oc_dsl::serialize(&project)).unwrap();
    let mut audio = AudioState::new();
    audio.sample_rate = 8000;
    audio.soundfont_path = None;
    let mut instances = std::collections::HashMap::new();
    let buffer = audio
        .render_for_playback(&reopened, &mut instances)
        .unwrap();
    assert_eq!(buffer.len(), 8 * 8000 * 2);
    let output =
        std::env::temp_dir().join(format!("ocp-section-export-{}.wav", std::process::id()));
    assert!(!output.exists());
    audio
        .render_project_to_wav(&reopened, &output, &mut instances, None)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&output).unwrap().len(),
        44 + buffer.len() as u64 * 2
    );
    std::fs::remove_file(output).unwrap();
    project.sections.clear();
    let short = audio.render_for_playback(&project, &mut instances).unwrap();
    assert!(short.len() < buffer.len());
    assert_eq!(&buffer[..short.len()], short);
}

#[test]
fn mixed_gui_playback_still_rejects_missing_soundfont() {
    let mut project = native_test_project();
    let mut gm = project.tracks[0].clone();
    gm.name = "GM needs actual SoundFont".into();
    gm.preset = None;
    project.tracks.push(gm);
    let mut audio = AudioState::new();
    audio.soundfont_path = None;
    audio.sample_rate = 8000;
    assert!(
        audio
            .render_for_playback(&project, &mut std::collections::HashMap::new())
            .is_err()
    );
}

#[test]
#[ignore = "explicit project/output paths required; no audio device is opened"]
fn native_project_gui_offline_smoke() {
    use std::io::Write;
    let input = PathBuf::from(std::env::var("OCP_GUI_SMOKE_PROJECT").unwrap());
    let output = PathBuf::from(std::env::var("OCP_GUI_SMOKE_OUTPUT").unwrap());
    let project = oc_dsl::parse(&std::fs::read_to_string(input).unwrap()).unwrap();
    oc_dsl::validate(&project).unwrap();
    let serialized = oc_dsl::serialize(&project);
    let reopened = oc_dsl::parse(&serialized).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );
    let mut audio = AudioState::new();
    audio.soundfont_path = std::env::var_os("OCP_GUI_SMOKE_SOUNDFONT").map(PathBuf::from);
    if let Some(path) = &audio.soundfont_path {
        assert!(path.is_file(), "explicit smoke SoundFont must exist");
    }
    audio.sample_rate = 48000;
    let mut instances = std::collections::HashMap::new();
    let buffer = audio
        .render_for_playback(&reopened, &mut instances)
        .unwrap();
    assert!(!buffer.is_empty() && buffer.iter().all(|sample| sample.is_finite()));
    assert!(buffer.iter().any(|sample| sample.abs() > 0.0001));
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("reopened.ocp"), serialized).unwrap();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(output.join("playback.f32le")).unwrap());
    for sample in &buffer {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
    file.flush().unwrap();
    audio
        .render_project_to_wav(&reopened, &output.join("export.wav"), &mut instances, None)
        .unwrap();
    println!(
        "GUI offline smoke: {} stereo frames; explicit SoundFont={}; no audio device",
        buffer.len() / 2,
        audio.soundfont_path.is_some()
    );
}

fn preset(name: &str, source: &str, plugin: Option<&str>) -> PresetDef {
    PresetDef {
        name: name.to_string(),
        source: Some(source.to_string()),
        plugin: plugin.map(str::to_string),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    }
}

fn track_with_preset(preset_name: Option<&str>) -> Track {
    Track {
        name: "Track".to_string(),
        instrument: InstrumentPath::new("synth", "lead"),
        preset: preset_name.map(str::to_string),
        engine: None,
        mix: MixState::default(),
        effects: vec![],
        phrases: vec![],
        patterns: vec![],
        inline_patterns: vec![],
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: vec![],
    }
}

fn pattern_with_preset(preset_name: Option<&str>) -> PatternDef {
    PatternDef {
        name: "Pattern".to_string(),
        instrument: InstrumentPath::new("synth", "lead"),
        preset: preset_name.map(str::to_string),
        engine: None,
        transpose: None,
        swing: None,
        bars: None,
        effects: vec![],
        events: vec![],
        pedals: vec![],
        hairpins: Vec::new(),
        hits: vec![],
        color: None,
    }
}

#[test]
fn mixed_soundfont_kit_and_audio_projects_use_block_sources() {
    let project = oc_dsl::parse(
        r#"@project
@preset piano
  source: soundfont
  file: "piano.sf3"
@kit drums
  pad kick | note: 36, voice: kick
@audio_clip vocals [1:1 - 3:1] | track: vocals
  file: "vocals.wav"
@track piano | preset: piano, instrument: piano/acoustic_grand
  @phrase p [1:1 - 3:1]
    C4 w
@track drums | preset: drums, instrument: drums/drumkit
  @pattern beat [1:1 - 3:1]
    kick 1:0 mf
@track vocals | instrument: audio/clip
"#,
    )
    .unwrap();
    assert!(project_needs_plugin_aware_render(&project));
    assert!(
        supports_block_sources(&project),
        "a supported kit must not force the whole arrangement to render before playback"
    );
    let mut unsupported = project.clone();
    let kit = unsupported
        .presets
        .iter_mut()
        .find(|p| p.name == "drums")
        .unwrap();
    kit.plugin = Some("builtin.unknown".into());
    assert!(!supports_block_sources(&unsupported));
}

#[test]
fn unsupported_preview_reports_error_instead_of_discarding_render_plan() {
    let project = oc_dsl::parse("@project\n@track t | instrument: sample/take\n  performance: { @0 ch0 note_on 60 90; @1 ch0 note_off 60 37; }\n").unwrap();
    let mut audio = AudioState::new();
    let error = audio
        .preview_track(&project, 0, &mut std::collections::HashMap::new(), None)
        .unwrap_err();
    assert!(error.contains("streaming source"), "{error}");
    assert!(!audio.streaming_active);
    assert!(audio.playback.is_none());
}

#[test]
fn audible_transport_never_falls_back_to_full_song_render() {
    let project = oc_dsl::parse("@project\n@preset p\n  source: builtin\n  plugin: \"builtin.unsupported\"\n@track t | preset: p, instrument: synth/lead\n").unwrap();
    let mut audio = AudioState::new();
    let error = audio
        .play(&project, &mut std::collections::HashMap::new())
        .err()
        .expect("unsupported source is refused");
    assert!(
        error.contains("Streaming playback") && error.contains("builtin.unsupported"),
        "{error}"
    );
    assert!(audio.render_cache.is_none() && audio.playback.is_none());
}

#[test]
fn qualified_performance_projects_use_block_sources() {
    for (source, plugin) in [
        ("builtin", "builtin.open_synth"),
        ("clap", "test.plugin"),
        ("vst2", "test.plugin"),
        ("vst3", "test.plugin"),
    ] {
        let text = format!(
            "@project\n@preset p\n  source: {source}\n  plugin: \"{plugin}\"\n@track t | preset: p, instrument: synth/lead_square\n  performance: {{ @0 ch0 note_on 60 90; @1 ch0 note_off 60 37; }}\n"
        );
        let project = oc_dsl::parse(&text).unwrap();
        assert!(project_needs_plugin_aware_render(&project));
        assert!(supports_block_sources(&project), "{source}");
    }
}

#[test]
fn existing_host_adapters_and_default_soundfont_use_block_sources() {
    for source in ["clap", "vst3", "vst2", "au", "lv2"] {
        let mut project = project();
        project
            .presets
            .push(preset("host", source, Some("test.plugin")));
        project.presets.push(preset("font", "soundfont", None));
        project.tracks.push(track_with_preset(Some("host")));
        project.tracks.push(track_with_preset(Some("font")));
        project.presets[0].params.push((
            "plugin_output_pair".into(),
            oc_dsl::ast::ParamValue::Number(2.0),
        ));
        assert!(supports_block_sources(&project), "{source}");
    }
}

#[test]
fn native_sampler_projects_use_block_sources() {
    let mut project = project();
    project.presets.push(preset(
        "sample",
        "builtin",
        Some(oc_synth::sampler::PLUGIN_ID),
    ));
    project.tracks.push(track_with_preset(Some("sample")));
    assert!(project_needs_plugin_aware_render(&project));
    assert!(supports_block_sources(&project));
}

#[test]
fn mixed_sfizz_and_native_projects_use_block_sources() {
    let mut project = project();
    project
        .presets
        .push(preset("keys", "sample", Some("sfizz")));
    project
        .presets
        .push(preset("bass", "builtin", Some("builtin.open_synth")));
    project.tracks.push(track_with_preset(Some("keys")));
    project.tracks.push(track_with_preset(Some("bass")));
    assert!(project_needs_plugin_aware_render(&project));
    assert!(supports_block_sources(&project));
    project.presets[0].plugin = Some("unknown-sampler".into());
    assert!(!supports_block_sources(&project));
}

#[test]
fn soundfont_project_uses_streaming_render() {
    let mut project = project();
    project.presets.push(preset("piano", "soundfont", None));
    project.tracks.push(track_with_preset(Some("piano")));

    assert!(!project_needs_plugin_aware_render(&project));
}

#[test]
fn clap_and_builtin_instrument_projects_use_plugin_aware_render() {
    let mut clap_project = project();
    clap_project.presets.push(preset(
        "lead",
        "clap",
        Some("org.surge-synth-team.surge-xt"),
    ));
    clap_project.tracks.push(track_with_preset(Some("lead")));

    let mut builtin_project = project();
    builtin_project
        .presets
        .push(preset("tone", "builtin", Some("builtin.test_tone")));
    builtin_project.tracks.push(track_with_preset(Some("tone")));

    assert!(project_needs_plugin_aware_render(&clap_project));
    assert!(project_needs_plugin_aware_render(&builtin_project));
}

#[test]
fn unsupported_plugin_instrument_projects_use_plugin_aware_render() {
    let mut project = project();
    project
        .presets
        .push(preset("lead", "vst3", Some("vst3:example")));
    project.tracks.push(track_with_preset(Some("lead")));

    assert!(project_needs_plugin_aware_render(&project));
}

#[test]
fn plugin_pattern_projects_use_plugin_aware_render() {
    let mut project = project();
    project.presets.push(preset(
        "lead",
        "clap",
        Some("org.surge-synth-team.surge-xt"),
    ));
    project.patterns.push(pattern_with_preset(Some("lead")));

    assert!(project_needs_plugin_aware_render(&project));
}

#[test]
fn plugin_effect_projects_use_plugin_aware_render() {
    let mut project = project();
    project.master = Some(Master {
        gain_automation: Vec::new(),
        automation: Vec::new(),
        effects: vec![Effect {
            effect_type: EffectType::Plugin("org.example.effect".to_string()),
            params: vec![],
            bypass: false,
            sidechain_source: None,
        }],
        loudness_target: None,
        volume_db: 0.0,
        input_routes: vec![],
    });

    assert!(project_needs_plugin_aware_render(&project));
}

/// A tone through one gain insert, so an effect parameter is the only
/// thing between the source and the mix.
fn chain_drag_project(factor: f64) -> Project {
    oc_dsl::parse(&format!(
            "@project\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.4}}\n@track lead | preset: tone, instrument: synth/lead\n  effects:\n    - gain: factor={factor}\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n"
        ))
        .unwrap()
}

/// A drag of 50 input events overruns the render thread: only the first
/// document may be in flight and only the newest may still be waiting.
#[test]
fn a_parameter_drag_hands_over_one_chain_document_at_a_time() {
    let project = chain_drag_project(1.0);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 0.0);
    let rt = audio.render_thread.as_ref().unwrap();
    let loads = rt.playback.project_loads.load(Ordering::Acquire);
    let instruments = rt
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);

    // Hold the render thread inside its command loop, so a document per
    // input event would really pile up.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    rt.send(RenderCommand::TestBarrier {
        entered: entered_tx,
        release: release_rx,
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();

    for step in 1..=50 {
        let mut edited = project.clone();
        let factor = 1.0 - f64::from(step) * 0.015;
        edited.tracks[0].effects[0].params =
            vec![("factor".into(), oc_dsl::ast::ParamValue::Number(factor))];
        audio.apply_chain_change(&edited);
    }
    assert_eq!(
        audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .chain_requests
            .load(Ordering::Acquire),
        1,
        "a drag may only hand over the document already in flight"
    );
    {
        let pending = audio.pending_chain.as_ref().expect("a document is waiting");
        assert_eq!(
            pending.tracks[0].effects[0]
                .params
                .iter()
                .find(|(name, _)| name == "factor")
                .map(|(_, value)| value.as_f64()),
            Some(Some(1.0 - 50.0 * 0.015)),
            "the waiting document must be the newest one"
        );
    }

    release_tx.send(()).unwrap();
    // The poll hands the waiting document over once the first one landed.
    wait_for_audio(|| {
        audio.transport_snapshot(&clock).unwrap();
        audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .chain_applied
            .load(Ordering::Acquire)
            >= 2
    });
    let rt = audio.render_thread.as_ref().unwrap();
    assert_eq!(
        rt.playback.chain_requests.load(Ordering::Acquire),
        2,
        "fifty input events must cost two documents, not fifty"
    );
    // Two documents reach the graph, so two patches: the first edit and the
    // newest one. The other 48 input events never touch it.
    assert_eq!(
        rt.playback.chain_patches.load(Ordering::Acquire),
        2,
        "fifty input events must not cost fifty chain patches"
    );
    assert_eq!(
        rt.playback.project_loads.load(Ordering::Acquire),
        loads,
        "a chain drag must not reload the project"
    );
    assert_eq!(
        rt.playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments,
        "a chain drag must not instantiate an instrument"
    );
    audio.reset_project();
}

#[test]
fn mute_while_playing_patches_graph_without_reloading() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 0.0);
    let rt = audio.render_thread.as_ref().unwrap();
    let loads = rt.playback.project_loads.load(Ordering::Acquire);
    let instruments = rt
        .playback
        .instrument_instantiations
        .load(Ordering::Acquire);
    assert_eq!(instruments, 1);

    let mut muted = project.clone();
    muted.tracks[0].mix.mute = true;
    let position = audio.transport_snapshot(&clock).unwrap().position_beats;
    audio.apply_mixer_change(&muted);
    assert!(!audio.render_dirty, "a mixer edit must not force a reload");

    wait_for_audio(|| {
        let snapshot = audio.transport_snapshot(&clock).unwrap();
        snapshot.playing && snapshot.position_beats > position
    });
    let rt = audio.render_thread.as_ref().unwrap();
    assert_eq!(rt.playback.project_loads.load(Ordering::Acquire), loads);
    assert_eq!(
        rt.playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments,
        "muting must not re-instantiate the instrument"
    );

    // Unmuting stays instrument-free too.
    audio.apply_mixer_change(&project);
    wait_for_audio(|| audio.transport_snapshot(&clock).unwrap().position_beats > 0.0);
    assert_eq!(
        audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .instrument_instantiations
            .load(Ordering::Acquire),
        instruments
    );
    audio.reset_project();
}

/// A tone whose first sixteenth lands on bar 1 beat 3, with the monitoring
/// chain under test spliced into one track or the master.
fn monitoring_project(track_effects: &str, master_effects: &str) -> Project {
    let source = format!(
        "@ocp \"0.2.0\"\n@project \"monitor latency\"\n  tempo: 120\n@preset click\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.4}}\n@track click | preset: click, instrument: synth/lead\n{track_effects}  @phrase p [1:1 - 3:1]\n    rest h\n    C4 16th fff\n{master_effects}"
    );
    oc_dsl::parse(&source).unwrap()
}

/// The snapshot's position is the engine cursor less one device block plus
/// the graph's reported chain: 0 with no monitoring effect, 40 ms for the
/// tuner (track or master) and 88 ms for the tuner plus the suppressor.
#[test]
fn transport_position_lags_the_engine_clock_by_the_monitor_latency() {
    const RATE: u32 = 48_000;
    // The simulated device callback hands over 256 interleaved samples.
    const BLOCK: u64 = 128;
    let cases = [
        ("none", "", "", 0_u64),
        (
            "track_tuner",
            "  effects:\n    - tuner: amount=100%\n",
            "",
            1_920,
        ),
        (
            "master_tuner",
            "",
            "@master\n  effects:\n    - tuner: amount=100%\n",
            1_920,
        ),
        (
            "master_tuner_resonance",
            "",
            "@master\n  effects:\n    - tuner: amount=100%\n    - resonance: depth=12dB, mix=50%\n",
            4_224,
        ),
    ];
    for (name, track_effects, master_effects, expected_chain) in cases {
        let project = monitoring_project(track_effects, master_effects);
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        let mut audio = AudioState::new();
        audio.simulate_output = true;
        audio.sample_rate = RATE;
        audio
            .play(&project, &mut std::collections::HashMap::new())
            .unwrap();
        wait_for_audio(|| {
            audio
                .render_thread
                .as_ref()
                .is_some_and(|rt| rt.playback.audible_frames() > 0)
        });
        // Compare on an instant when the engine cursor does not move
        // between the snapshot and the direct read.
        let started = std::time::Instant::now();
        let (cursor, snapshot, latency) = loop {
            let before = audio
                .render_thread
                .as_ref()
                .unwrap()
                .playback
                .play_cursor_frames
                .load(Ordering::Acquire);
            let snapshot = audio.transport_snapshot(&clock).unwrap();
            let playback = &audio.render_thread.as_ref().unwrap().playback;
            let after = playback.play_cursor_frames.load(Ordering::Acquire);
            if before == after {
                break (before, snapshot, playback.monitor_latency_frames());
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{name}: the cursor never settled"
            );
        };
        assert_eq!(
            latency,
            BLOCK + expected_chain,
            "{name}: device block plus chain"
        );
        assert_eq!(
            (snapshot.monitor_latency_seconds * f64::from(RATE)).round() as u64,
            latency,
            "{name}: exposed latency"
        );
        let reported = (snapshot.position_seconds * f64::from(RATE)).round() as u64;
        assert_eq!(
            reported,
            cursor - latency,
            "{name}: reported position against the engine clock"
        );
        assert!(
            (snapshot.position_beats - clock.position_at_sample(reported, RATE).unwrap().quarter)
                .abs()
                < 1e-9,
            "{name}: beats and seconds disagree"
        );
        audio.stop();
    }
}

#[test]
fn play_returns_promptly_and_reports_a_bounded_first_chunk_failure() {
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    // A preset without a plug-in ID fails on the worker without scanning the host.
    let broken = oc_dsl::parse(
            "@project\n@preset p\n  source: vst3\n@track t | preset: p, instrument: synth/lead\n  @phrase main [1:1 - 2:1]\n    C4 w mf\n",
        )
        .unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&broken.properties).unwrap();
    let worker = RenderThreadHandle::spawn(audio.sample_rate);
    let (entered, wait) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    worker.send(RenderCommand::TestBarrier {
        entered,
        release: blocked,
    });
    wait.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    audio.render_thread = Some(worker);
    let start = std::time::Instant::now();
    audio
        .play(&broken, &mut std::collections::HashMap::new())
        .unwrap();
    let elapsed = start.elapsed();
    // This guards against spinning on the worker, not against latency: the suite runs its
    // 500+ tests in parallel on 12 cores, and a 500 ms wall-clock bound fails there while
    // passing in isolation. The real contract is the ordering assertion below (play returns
    // while the first chunk is still pending); the cap only has to catch a hang.
    let bound = if cfg!(debug_assertions) {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_millis(500)
    };
    assert!(elapsed < bound, "play blocked for {elapsed:?}");
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert!(snapshot.starting, "first chunk is still pending");
    assert!(snapshot.playing);
    release.send(()).unwrap();

    // The worker's load failure becomes a UI-visible status, without a spin.
    let started = std::time::Instant::now();
    let failed = loop {
        let snapshot = audio.transport_snapshot(&clock).unwrap();
        if snapshot.status.is_some() {
            break snapshot;
        }
        // Same reasoning as the bound above: a starved worker thread in the parallel
        // suite can take seconds to report, and the contract is that it *is* reported.
        let report_bound = if cfg!(debug_assertions) {
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::from_secs(3)
        };
        assert!(
            started.elapsed() < report_bound,
            "load failure was never reported"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert!(!failed.playing);
    assert!(!failed.starting);
    let status = failed.status.unwrap();
    assert!(status.starts_with("Playback failed:"), "{status}");

    // The deadline itself is bounded: an aged request fails deterministically.
    audio.load_queued = false;
    audio.pending_start = Some(PendingStart {
        started: std::time::Instant::now()
            - FIRST_CHUNK_DEADLINE
            - std::time::Duration::from_secs(1),
        frame: 0,
        resume: false,
        before_applied: 0,
    });
    audio.transport_status = None;
    audio.poll_stream_state(false, 0, None);
    assert!(audio.transport_status.is_some());
    assert!(matches!(audio.playback_state, PlaybackState::Paused));
    audio.reset_project();
}

#[test]
fn pending_play_does_not_report_a_first_block_timeout_during_project_loading() {
    let mut audio = AudioState::new();
    audio.load_queued = true;
    audio.pending_start = Some(PendingStart {
        started: std::time::Instant::now()
            - FIRST_CHUNK_DEADLINE
            - std::time::Duration::from_secs(1),
        frame: 48_000,
        resume: false,
        before_applied: 0,
    });
    audio.poll_stream_state(false, 0, None);
    assert!(audio.pending_start.is_some());
    assert!(audio.transport_status.is_none());

    audio.poll_stream_state(false, 0, Some("Cannot decode media".into()));
    assert!(audio.pending_start.is_none());
    assert_eq!(
        audio.transport_status.as_deref(),
        Some("Playback failed: Cannot decode media")
    );
    assert_eq!(audio.pending_seek_frame, Some(48_000));
}

#[test]
fn a_worker_read_failure_after_start_pauses_and_explains_the_stop() {
    let mut audio = AudioState::new();
    audio.set_playback_state(PlaybackState::Playing);
    audio.poll_stream_state(
        true,
        1,
        Some("Cannot read clip audio: truncated spool".into()),
    );
    assert!(matches!(audio.playback_state, PlaybackState::Paused));
    assert_eq!(
        audio.transport_status.as_deref(),
        Some("Playback stopped: Cannot read clip audio: truncated spool"),
    );
}

#[test]
#[ignore = "requires OCP_STREAM_PROJECT; measures edit-to-first-chunk and mute latency"]
fn review_edit_to_first_chunk_latency() {
    let path = PathBuf::from(std::env::var_os("OCP_STREAM_PROJECT").unwrap());
    let base = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&base.properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 48000;
    audio.resource_root = path.parent().map(std::path::Path::to_path_buf);
    let mut plugins = std::collections::HashMap::new();
    let generation = |audio: &AudioState| {
        audio
            .render_thread
            .as_ref()
            .map_or(0, |rt| rt.playback.seek_generation.load(Ordering::Acquire))
    };
    // The worker bumps seek_generation when it applies Play; ready must then
    // be observed after that bump, never from the previous playback.
    let first_chunk_ms = |audio: &AudioState, before: u64| {
        let start = std::time::Instant::now();
        while generation(audio) == before
            || !audio
                .render_thread
                .as_ref()
                .unwrap()
                .playback
                .ready
                .load(Ordering::Acquire)
        {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "first chunk timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        start.elapsed().as_secs_f64() * 1000.0
    };
    let wait_until = |condition: &mut dyn FnMut() -> bool| {
        let start = std::time::Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "measurement timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        start.elapsed().as_secs_f64() * 1000.0
    };
    let counters = |audio: &AudioState| {
        let pb = &audio.render_thread.as_ref().unwrap().playback;
        (
            pb.project_loads.load(Ordering::Acquire),
            pb.instrument_instantiations.load(Ordering::Acquire),
        )
    };

    // Cold start: project never loaded, then press play.
    let before = generation(&audio);
    let start = std::time::Instant::now();
    audio.play(&base, &mut plugins).unwrap();
    let cold_play_ms = start.elapsed().as_secs_f64() * 1000.0;
    let cold_ready_ms = first_chunk_ms(&audio, before);
    let (loads, instruments) = counters(&audio);
    eprintln!(
        "cold play_return_ms={cold_play_ms:.1} ready_ms={cold_ready_ms:.1} loads={loads} instruments={instruments}"
    );

    // Mute while playing: the graph must patch, not reload.
    let mut edited = base.clone();
    edited.tracks[0].mix.mute = true;
    let position = audio.transport_snapshot(&clock).unwrap().position_beats;
    let start = std::time::Instant::now();
    audio.apply_mixer_change(&edited);
    let mute_heard_ms =
        wait_until(&mut || audio.transport_snapshot(&clock).unwrap().position_beats > position);
    let (loads, instruments) = counters(&audio);
    eprintln!(
        "mute_while_playing dispatch_ms={:.1} next_chunk_ms={mute_heard_ms:.1} loads={loads} instruments={instruments}",
        start.elapsed().as_secs_f64() * 1000.0
    );

    // Mute while paused, then play: resumes from the intact lookahead.
    audio.pause();
    edited.tracks[1].mix.mute = true;
    audio.apply_mixer_change(&edited);
    let resume_from = audio.transport_snapshot(&clock).unwrap().position_beats;
    let start = std::time::Instant::now();
    audio.play(&edited, &mut plugins).unwrap();
    let warm_play_ms = start.elapsed().as_secs_f64() * 1000.0;
    let warm_audible_ms =
        wait_until(&mut || audio.transport_snapshot(&clock).unwrap().position_beats > resume_from);
    let (loads, instruments) = counters(&audio);
    eprintln!(
        "mute_then_play play_return_ms={warm_play_ms:.1} audible_ms={warm_audible_ms:.1} loads={loads} instruments={instruments}"
    );

    // First unmute of a track that was pruned at load: only that lane is built.
    let mut hot = edited.clone();
    hot.tracks[1].mix.mute = false;
    let (loads, instruments) = counters(&audio);
    let start = std::time::Instant::now();
    audio.apply_mixer_change(&hot);
    let hot_wait_ms = wait_until(&mut || counters(&audio).1 > instruments);
    let (hot_loads, hot_instruments) = counters(&audio);
    eprintln!(
        "first_unmute dispatch_ms={:.1} built_ms={hot_wait_ms:.1} loads={hot_loads} instruments={hot_instruments} (was loads={loads} instruments={instruments})",
        start.elapsed().as_secs_f64() * 1000.0
    );

    // Structural edit: a reload, but the UI thread does not wait for it.
    audio.pause();
    let mut structural = edited.clone();
    structural.tracks[0].phrases.clear();
    audio.mark_dirty();
    audio.preload_project(&structural);
    let before = generation(&audio);
    let start = std::time::Instant::now();
    audio.play(&structural, &mut plugins).unwrap();
    let structural_play_ms = start.elapsed().as_secs_f64() * 1000.0;
    let structural_ready_ms = first_chunk_ms(&audio, before);
    let (loads, instruments) = counters(&audio);
    eprintln!(
        "structural_edit play_return_ms={structural_play_ms:.1} ready_ms={structural_ready_ms:.1} loads={loads} instruments={instruments}"
    );
    audio.stop();
}

#[test]
fn a_failed_warm_up_is_not_reported_as_warming() {
    let mut project = native_test_project();
    project.properties.tempo = 0.0;
    let clock = oc_dsl::timing::ProjectClock::new(&native_test_project().properties).unwrap();
    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 8000;
    audio.preload_project(&project);
    wait_for_audio(|| {
        audio
            .render_thread
            .as_ref()
            .and_then(|rt| rt.load_error())
            .is_some()
    });
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert!(!snapshot.warming, "a failed build is not a warm-up");
    assert!(!snapshot.prepared);
    audio.reset_project();
}

#[test]
#[ignore = "requires OCP_STREAM_PROJECT; measures open -> ready and press latency"]
fn review_open_to_first_sound_latency() {
    use crate::render_thread::WARM_READY;

    let path = PathBuf::from(std::env::var_os("OCP_STREAM_PROJECT").unwrap());
    let source = std::fs::read_to_string(&path).unwrap();
    let open_start = std::time::Instant::now();
    let project = oc_dsl::parse(&source).unwrap();
    eprintln!(
        "[measure] parse_ms={:.2}",
        open_start.elapsed().as_secs_f64() * 1000.0
    );
    let mut plugins = std::collections::HashMap::new();

    let mut audio = AudioState::new();
    audio.simulate_output = true;
    audio.sample_rate = 48000;
    audio.resource_root = path.parent().map(std::path::Path::to_path_buf);
    audio.reset_project();
    let open_start = std::time::Instant::now();
    audio.preload_project(&project);
    let open_ms = open_start.elapsed().as_secs_f64() * 1000.0;

    let wait_start = std::time::Instant::now();
    while wait_start.elapsed() < std::time::Duration::from_secs(10) {
        let ready = audio
            .render_thread
            .as_ref()
            .is_some_and(|rt| rt.playback.warm_state.load(Ordering::Acquire) == WARM_READY);
        if ready {
            break;
        }
        if audio
            .render_thread
            .as_ref()
            .is_some_and(|rt| rt.load_error().is_some())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let ready_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let (loaded, ring_ready, warm, stream_running, loads, instruments) = state_flags(&audio);
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    eprintln!(
        "[measure] open_dispatch_ms={open_ms:.2} open_to_ready_ms={ready_ms:.2} loaded={loaded} ring_ready={ring_ready} warm={warm} stream_running={stream_running} loads={loads} instruments={instruments} snapshot(warming={} prepared={} duration_s={:.2}) load_error={:?}",
        snapshot.warming,
        snapshot.prepared,
        snapshot.duration_seconds,
        audio.render_thread.as_ref().and_then(|rt| rt.load_error())
    );

    let press_start = std::time::Instant::now();
    audio.play(&project, &mut plugins).unwrap();
    let return_ms = press_start.elapsed().as_secs_f64() * 1000.0;
    let heard_before = heard_blocks(&audio);
    let after_press = state_flags(&audio);
    let audible_ms = wait_heard_blocks(&audio, heard_before);
    eprintln!(
        "[measure] press1_return_ms={return_ms:.2} press1_first_audible_ms={audible_ms:?} ring_ready_after={} stream_running_after={} loads={} instruments={}",
        after_press.1, after_press.3, after_press.4, after_press.5
    );

    // Second play press after a pause must reuse the live ring.
    audio.pause();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let cursor_before = audio.render_thread.as_ref().map_or(0, |rt| {
        rt.playback.play_cursor_frames.load(Ordering::Acquire)
    });
    let heard_before = heard_blocks(&audio);
    let press_start = std::time::Instant::now();
    audio.play(&project, &mut plugins).unwrap();
    let resume_return_ms = press_start.elapsed().as_secs_f64() * 1000.0;
    let resume_audible_ms = wait_heard_blocks(&audio, heard_before);
    let cursor_after = audio.render_thread.as_ref().map_or(0, |rt| {
        rt.playback.play_cursor_frames.load(Ordering::Acquire)
    });
    eprintln!(
        "[measure] press2_return_ms={resume_return_ms:.2} press2_first_audible_ms={resume_audible_ms:?} cursor_frames={cursor_before}->{cursor_after} loads={} instruments={}",
        state_flags(&audio).4,
        state_flags(&audio).5
    );
    audio.reset_project();

    // A press while the load is still in flight must return at once and show
    // a loading transport, then start when the worker is ready.
    let mut early = AudioState::new();
    early.simulate_output = true;
    early.sample_rate = 48000;
    early.resource_root = path.parent().map(std::path::Path::to_path_buf);
    early.reset_project();
    let open_start = std::time::Instant::now();
    early.preload_project(&project);
    let early_open_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let before = early.transport_snapshot(&clock).unwrap();
    let press_start = std::time::Instant::now();
    early.play(&project, &mut plugins).unwrap();
    let early_return_ms = press_start.elapsed().as_secs_f64() * 1000.0;
    let during = early.transport_snapshot(&clock).unwrap();
    let heard_before = heard_blocks(&early);
    let early_audible_ms = wait_heard_blocks(&early, heard_before);
    let settled = early.transport_snapshot(&clock).unwrap();
    let early_flags = state_flags(&early);
    eprintln!(
        "[measure] early_open_dispatch_ms={early_open_ms:.2} early_press_return_ms={early_return_ms:.2} early_first_audible_ms={early_audible_ms:?} before(warming={} prepared={}) after_press(warming={} prepared={} starting={} playing={}) loaded={} ring_ready={} stream_running={} loads={} instruments={}",
        before.warming,
        before.prepared,
        during.warming,
        during.prepared,
        during.starting,
        during.playing,
        early_flags.0,
        early_flags.1,
        early_flags.3,
        early_flags.4,
        early_flags.5
    );
    assert!(
        !settled.warming,
        "a started transport must stop reporting warming"
    );
    assert!(
        !settled.starting,
        "a started transport must clear the starting flag"
    );
    early.reset_project();
}

fn state_flags(audio: &AudioState) -> (bool, bool, u8, bool, u64, u64) {
    audio
        .render_thread
        .as_ref()
        .map_or((false, false, 0, audio.streaming_active, 0, 0), |rt| {
            (
                rt.playback.project_loaded.load(Ordering::Acquire),
                rt.playback.ready.load(Ordering::Acquire),
                rt.playback.warm_state.load(Ordering::Acquire),
                audio.streaming_active || audio.simulated_output.is_some(),
                rt.playback.project_loads.load(Ordering::Acquire),
                rt.playback
                    .instrument_instantiations
                    .load(Ordering::Acquire),
            )
        })
}

fn heard_blocks(audio: &AudioState) -> u64 {
    audio
        .simulated_output
        .as_ref()
        .map_or(0, |output| output.heard_blocks.load(Ordering::Acquire))
}

fn wait_heard_blocks(audio: &AudioState, before: u64) -> Option<f64> {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        if heard_blocks(audio) > before {
            return Some(start.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    None
}

// ========================================================================
// Device selection and the opened stream
// ========================================================================

fn fake_device(
    key: &str,
    name: &str,
    rates: &[u32],
    buffer: Option<(u32, u32)>,
    is_default: bool,
) -> super::OutputDevice {
    super::OutputDevice {
        key: key.to_string(),
        name: name.to_string(),
        is_default,
        channels: 2,
        sample_rates: rates.to_vec(),
        default_rate: rates[0],
        buffer_frames: buffer,
    }
}

fn fake_backend() -> std::sync::Arc<oc_audio::audio_io::FakeBackend> {
    std::sync::Arc::new(oc_audio::audio_io::FakeBackend::new(
        vec![
            fake_device(
                "coreaudio:a",
                "Speakers",
                &[8_000, 16_000],
                Some((128, 1_024)),
                true,
            ),
            fake_device(
                "coreaudio:b",
                "Interface",
                &[8_000, 16_000],
                Some((64, 512)),
                false,
            ),
        ],
        Some("coreaudio:a".to_string()),
    ))
}

fn audio_on(backend: &std::sync::Arc<oc_audio::audio_io::FakeBackend>) -> super::AudioState {
    super::AudioState::with_backend(backend.clone())
}

#[test]
fn a_device_switch_rebuilds_the_stream_that_was_open() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let project = native_test_project();
    audio.preload_project(&project);
    assert!(
        audio.audio_io.status().stream_open,
        "the warm stream is open"
    );

    let view = audio.set_audio_io(super::AudioSelection {
        output_device: Some("coreaudio:b".to_string()),
        sample_rate: None,
        buffer_frames: None,
        ..super::AudioSelection::default()
    });
    assert!(view.status.failure.is_none(), "{:?}", view.status.failure);
    assert_eq!(
        view.status
            .opened
            .as_ref()
            .map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:b"))
    );
    assert!(view.status.stream_open, "the swept stream came back");
    // One open is the probe, the second is the swept transport stream.
    let swept = backend
        .opens()
        .into_iter()
        .filter(|(key, _)| key.as_deref() == Some("coreaudio:b"))
        .count();
    assert_eq!(swept, 2, "the open stream moved to the new device");
}

#[test]
fn a_dead_output_stream_pauses_the_transport() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert_eq!(audio.playback_state, super::PlaybackState::Playing);

    backend.disconnect("the device was removed");
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert!(!snapshot.playing, "a dead stream cannot report Playing");
    assert_eq!(audio.playback_state, super::PlaybackState::Paused);
    assert!(
        snapshot
            .status
            .as_deref()
            .is_some_and(|status| status.contains("the device was removed")),
        "{:?}",
        snapshot.status
    );
    let status = audio.audio_io.status();
    assert!(!status.stream_open, "the dead stream is dropped");
    assert!(
        status.notices.iter().any(|notice| matches!(
            notice,
            oc_audio::audio_io::AudioNotice::StreamFailed { role, .. } if role == "main"
        )),
        "{:?}",
        status.notices
    );
}

#[test]
fn dead_private_auditions_release_output_and_preserve_the_arrangement_cursor() {
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let (controller, _) = crate::audition::AuditionController::new();
    let error = "the audition device was removed";
    for preview in ["stream", "pair", "buffer"] {
        let backend = fake_backend();
        let mut audio = audio_on(&backend);
        audio.seek(1.0, &clock).unwrap();
        let spec = audio.audio_io.spec();
        match preview {
            "buffer" => audio.play_audition_buffer(vec![0.1; 1_024]).unwrap(),
            _ => {
                let stream = if preview == "pair" {
                    AudioState::open_audition_pair_stream(
                        &audio.audio_io,
                        &spec,
                        PathBuf::new(),
                        project.clone(),
                        project.clone(),
                        false,
                    )
                } else {
                    AudioState::open_audition_render(
                        &audio.audio_io,
                        &spec,
                        PathBuf::new(),
                        project.clone(),
                        false,
                        crate::first_block::now_ns(),
                        crate::first_block::StartPath::PresetAudition,
                    )
                }
                .unwrap();
                audio.install_audition_render(stream).unwrap();
            }
        }
        assert!(audio.audition_active(), "{preview}");
        backend.disconnect(error);
        let transport = audio.transport_snapshot(&clock).unwrap();
        assert!(!transport.audition_active, "{preview}");
        assert!(!transport.playing, "{preview}");
        assert_eq!(transport.position_beats, 1.0, "{preview}");
        assert!(!audio.has_audition_pair());
        assert_eq!(audio.resume_private_audition().unwrap_err(), error);
        for _ in 0..2 {
            let mut audition = controller.snapshot();
            audio.fill_audition_snapshot(&mut audition);
            assert_eq!(audition.status, crate::audition::ERROR, "{preview}");
            assert_eq!(audition.error.as_deref(), Some(error));
        }
        audio.play_audition_buffer(vec![0.1; 1_024]).unwrap();
        assert!(audio.audition_active());
        let mut audition = controller.snapshot();
        audio.fill_audition_snapshot(&mut audition);
        assert!(audition.error.is_none(), "{preview}");
    }
}

#[test]
fn a_rate_change_drops_the_render_thread_and_stays_paused() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let project = native_test_project();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert_eq!(audio.playback_state, super::PlaybackState::Playing);
    assert_eq!(audio.sample_rate, 8_000);

    let view = audio.set_audio_io(super::AudioSelection {
        sample_rate: Some(16_000),
        ..super::AudioSelection::default()
    });
    assert_eq!(audio.sample_rate, 16_000);
    assert!(
        audio.render_thread.is_none(),
        "the ring is rebuilt at the rate"
    );
    assert_eq!(audio.playback_state, super::PlaybackState::Paused);
    assert_eq!(
        view.status.opened.as_ref().map(|opened| opened.sample_rate),
        Some(16_000)
    );
    assert_eq!(view.callback_frames, 0, "no stream reports a callback yet");
}

#[test]
fn a_refresh_that_changes_the_device_rate_pauses_and_rebuilds() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let project = native_test_project();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert_eq!(audio.sample_rate, 8_000);

    // The system default moves to a device with a different native rate.
    backend.set_devices(vec![fake_device(
        "coreaudio:c",
        "Studio Box",
        &[16_000],
        Some((64, 512)),
        true,
    )]);
    let view = audio.refresh_audio_io();
    assert_eq!(audio.sample_rate, 16_000);
    assert!(
        audio.render_thread.is_none(),
        "the ring is rebuilt at the rate"
    );
    assert_eq!(audio.playback_state, super::PlaybackState::Paused);
    assert_eq!(
        view.status
            .opened
            .as_ref()
            .map(|opened| opened.name.as_str()),
        Some("Studio Box"),
        "the stale device is forgotten"
    );
}

#[test]
fn a_failed_device_selection_keeps_the_stream_and_snaps_back() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let project = native_test_project();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    let pinned = audio.set_audio_io(super::AudioSelection {
        output_device: Some("coreaudio:a".to_string()),
        ..super::AudioSelection::default()
    });
    assert!(pinned.status.failure.is_none());
    backend.fail_open("Interface", "the device is in use");

    let view = audio.set_audio_io(super::AudioSelection {
        output_device: Some("coreaudio:b".to_string()),
        ..super::AudioSelection::default()
    });
    assert_eq!(
        view.status.selection.output_device.as_deref(),
        Some("coreaudio:a"),
        "the picker keeps the device that is open"
    );
    assert_eq!(
        view.status
            .opened
            .as_ref()
            .map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:a"))
    );
    assert!(matches!(
        view.status.failure,
        Some(oc_audio::audio_io::AudioFailure::OpenFailed { .. })
    ));
    assert!(view.status.stream_open, "the old stream was left alone");
    assert_eq!(audio.playback_state, super::PlaybackState::Paused);
}

#[test]
fn live_keyboard_parks_and_resumes_the_arrangement_cursor() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    audio.enable_simulated_output();
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let mut instances = std::collections::HashMap::new();
    audio.seek(1.0, &clock).unwrap();
    audio.play(&project, &mut instances).unwrap();
    let playback = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    wait_for_audio(|| playback.play_cursor_frames.load(Ordering::Acquire) > 4_000);

    assert!(
        audio
            .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 60, 100)
            .unwrap()
    );
    assert!(audio.live_synth_sounding());
    let parked = audio.transport_snapshot(&clock).unwrap();
    assert!(!parked.playing);
    assert!(parked.position_beats > 0.0);
    let parked_frame = playback.play_cursor_frames.load(Ordering::Acquire);
    audio.play(&project, &mut instances).unwrap();
    assert_eq!(audio.pending_start.unwrap().frame, parked_frame);
    assert!(audio.pending_start.unwrap().resume);
    assert!(!audio.live_synth_active());
}

#[test]
fn live_keyboard_keeps_a_paused_explicit_seek() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    audio.enable_simulated_output();
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    audio.pause();
    audio.seek(2.0, &clock).unwrap();

    audio
        .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 60, 100)
        .unwrap();
    let parked = audio.transport_snapshot(&clock).unwrap();
    assert!(!parked.playing);
    assert_eq!(parked.position_beats, 2.0);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert!(!audio.pending_start.unwrap().resume);
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_beats,
        2.0
    );
}

#[test]
fn a_failed_live_synth_reopen_keeps_the_original_note_queue() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    audio
        .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 60, 100)
        .unwrap();
    assert!(audio.live_synth_sounding());
    backend.fail_open("Interface", "the device is in use");
    let replacement =
        oc_audio::audio_io::StreamSpec::new(Some("coreaudio:b".into()), audio.sample_rate, None);
    assert!(audio.main_stream.reopen(&replacement).is_err());
    assert!(
        audio.live_synth_sounding(),
        "the original queue still owns the pending note"
    );
    audio.live_synth_note_off("lead", 60).unwrap();
    assert!(audio.live_synth_active());

    let working = oc_audio::audio_io::StreamSpec::new(None, audio.sample_rate, None);
    audio.main_stream.reopen(&working).unwrap();
    assert!(
        !audio.live_synth_sounding(),
        "the accepted stream installs a fresh queue"
    );
    audio
        .live_synth_note_on("lead", || panic!("the patch stays loaded"), 120.0, 64, 100)
        .unwrap();
    assert!(audio.live_synth_sounding());
}

#[test]
fn a_disconnected_live_synth_releases_preview_ownership_and_recovers_on_next_note() {
    let backend = fake_backend();
    let mut audio = audio_on(&backend);
    let clock = oc_dsl::timing::ProjectClock::new(&native_test_project().properties).unwrap();
    audio
        .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 60, 100)
        .unwrap();
    assert!(audio.live_synth_sounding());

    backend.disconnect("the device was removed");
    assert!(!audio.live_synth_active());
    assert!(
        !audio.live_synth_sounding(),
        "queued commands cannot own a dead stream"
    );
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(
        snapshot.status.as_deref(),
        Some("Synth audition stopped: the device was removed")
    );
    assert!(!audio.audio_io.status().stream_open);
    assert!(
        audio
            .audio_io
            .status()
            .notices
            .iter()
            .any(|notice| matches!(
                notice,
                oc_audio::audio_io::AudioNotice::StreamFailed { role, error }
                    if role == "main" && error == "the device was removed"
            ))
    );
    audio.live_synth_note_off("lead", 60).unwrap();
    audio
        .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 64, 100)
        .unwrap();
    assert!(audio.live_synth_active());
    assert!(audio.live_synth_sounding());
    assert!(audio.transport_snapshot(&clock).unwrap().status.is_none());
}

/// A backend whose stream answers a different rate than the format asks
/// for. Negotiation names one rate; the device reports another.
struct SubstitutingBackend {
    inner: oc_audio::audio_io::FakeBackend,
    opened_rate: u32,
}

impl oc_audio::audio_io::DeviceBackend for SubstitutingBackend {
    fn list_outputs(&self) -> Result<Vec<super::OutputDevice>, String> {
        self.inner.list_outputs()
    }

    fn default_output(&self) -> Option<String> {
        self.inner.default_output()
    }

    fn open_output(
        &self,
        spec: &super::StreamSpec,
        sink: Arc<dyn super::RenderSink>,
    ) -> Result<super::StreamSink, String> {
        let opened = self.inner.open_output(spec, sink)?;
        Ok(super::StreamSink::new(
            (),
            self.opened_rate,
            opened.buffer_frames,
            opened.buffer_fixed,
            None,
        ))
    }

    fn list_inputs(&self) -> Result<Vec<oc_audio::audio_io::InputDevice>, String> {
        self.inner.list_inputs()
    }

    fn default_input(&self) -> Option<String> {
        self.inner.default_input()
    }

    fn open_input(
        &self,
        spec: &oc_audio::audio_io::InputSpec,
        sink: Arc<dyn oc_audio::audio_io::InputSink>,
    ) -> Result<super::StreamSink, String> {
        self.inner.open_input(spec, sink)
    }
}

fn substituting_audio(opened_rate: u32) -> super::AudioState {
    let inner = oc_audio::audio_io::FakeBackend::new(
        vec![fake_device(
            "coreaudio:a",
            "Speakers",
            &[8_000, 16_000],
            Some((128, 1_024)),
            true,
        )],
        Some("coreaudio:a".to_string()),
    );
    super::AudioState::with_backend(Arc::new(SubstitutingBackend { inner, opened_rate }))
}

#[test]
fn buffered_audition_refuses_a_device_that_opens_at_another_rate() {
    let mut audio = substituting_audio(16_000);
    assert_eq!(audio.audio_io.spec().sample_rate, 8_000);
    let error = audio.play_audition_buffer(vec![0.1; 16_000]).unwrap_err();
    assert!(error.contains("different preview sample rate"));
    assert!(!audio.audition_active());
}

fn rate_substitution(view: &super::AudioIoView) -> Option<(u32, u32)> {
    view.status.notices.iter().find_map(|notice| match notice {
        oc_audio::audio_io::AudioNotice::RateSubstituted { requested, chosen } => {
            Some((*requested, *chosen))
        }
        _ => None,
    })
}

/// The device answers the rate the render thread already runs at. The
/// stream's answer decides, so nothing is rebuilt and the substitution is
/// named instead of silently assumed away.
#[test]
fn a_stream_that_answers_the_running_rate_keeps_the_render_thread() {
    let mut audio = substituting_audio(8_000);
    let project = native_test_project();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert_eq!(audio.playback_state, super::PlaybackState::Playing);
    assert_eq!(audio.sample_rate, 8_000);

    let view = audio.set_audio_io(super::AudioSelection {
        sample_rate: Some(16_000),
        ..super::AudioSelection::default()
    });
    assert_eq!(
        view.status.opened.as_ref().map(|opened| opened.sample_rate),
        Some(8_000),
        "the section reports the rate the stream opened"
    );
    assert_eq!(
        rate_substitution(&view),
        Some((16_000, 8_000)),
        "the notice names the rate asked for and the one that opened"
    );
    assert_eq!(
        audio.sample_rate, 8_000,
        "the render thread keeps the rate it was running at"
    );
    assert!(
        audio.render_thread.is_some(),
        "a stream that answered the running rate needs no rebuild"
    );
    assert_eq!(
        audio.playback_state,
        super::PlaybackState::Playing,
        "the transport kept rolling through the selection"
    );
}

/// The device answers a rate other than the running one. The render thread
/// is rebuilt at the rate the stream reports, which is what the notice says.
#[test]
fn a_stream_that_answers_a_new_rate_rebuilds_the_render_thread() {
    let mut audio = substituting_audio(16_000);
    let project = native_test_project();
    audio.preload_project(&project);
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert_eq!(audio.sample_rate, 8_000);

    let view = audio.set_audio_io(super::AudioSelection {
        sample_rate: None,
        ..super::AudioSelection::default()
    });
    assert_eq!(
        view.status.opened.as_ref().map(|opened| opened.sample_rate),
        Some(16_000)
    );
    assert_eq!(
        rate_substitution(&view),
        Some((8_000, 16_000)),
        "both rates are named"
    );
    assert_eq!(
        audio.sample_rate, 16_000,
        "the render rate follows the opened stream"
    );
    assert!(
        audio.render_thread.is_none(),
        "the ring is rebuilt at the answered rate"
    );
    assert_eq!(audio.playback_state, super::PlaybackState::Paused);
}

// ========================================================================
// Capture
// ========================================================================

fn capture_backend() -> std::sync::Arc<oc_audio::audio_io::FakeBackend> {
    use oc_audio::audio_io::{FakeBackend, InputDevice, OutputDevice};
    std::sync::Arc::new(
        FakeBackend::new(
            vec![OutputDevice {
                key: "coreaudio:out".into(),
                name: "Speakers".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![44_100, 48_000],
                default_rate: 48_000,
                buffer_frames: Some((64, 2_048)),
            }],
            Some("coreaudio:out".into()),
        )
        .with_inputs(
            vec![InputDevice {
                key: "coreaudio:mic".into(),
                name: "Built-in Microphone".into(),
                is_default: true,
                channels: 2,
                sample_rates: vec![44_100, 48_000],
                default_rate: 48_000,
            }],
            Some("coreaudio:mic".into()),
        ),
    )
}

/// A source that produces silence, so a monitor mix is only capture.
struct NoopSource;

impl oc_audio::audio_io::RenderSource for NoopSource {
    fn create(
        &self,
        _spec: &oc_audio::audio_io::StreamSpec,
    ) -> std::sync::Arc<dyn oc_audio::audio_io::RenderSink> {
        std::sync::Arc::new(NoopSink)
    }
}

struct NoopSink;

impl oc_audio::audio_io::RenderSink for NoopSink {
    fn fill(&self, output: &mut [f32]) {
        output.fill(0.0);
    }
}

fn monitor_block(port: &std::sync::Arc<crate::input::InputPort>, frames: usize) -> Vec<f32> {
    let source: std::sync::Arc<dyn oc_audio::audio_io::RenderSource> =
        std::sync::Arc::new(NoopSource);
    let wrapped = crate::input::MonitorSource::new(source, std::sync::Arc::clone(port));
    let spec = oc_audio::audio_io::StreamSpec::new(None, 48_000, None);
    let sink = wrapped.create(&spec);
    let mut output = vec![0.0; frames * 2];
    sink.fill(&mut output);
    output
}

#[test]
fn capture_opens_on_demand_and_names_the_device() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    assert!(
        !audio.input_view().open,
        "nothing captures before it is asked"
    );
    audio.ensure_input();
    let view = audio.input_view();
    assert!(view.open);
    assert_eq!(view.device.as_deref(), Some("Built-in Microphone"));
    assert_eq!(
        backend.input_opens().len(),
        1,
        "one capture stream, not a probe per poll"
    );
    // A second call reuses the open stream.
    audio.ensure_input();
    assert_eq!(backend.input_opens().len(), 1);
}

#[test]
fn selecting_an_output_with_capture_open_reopens_the_input() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    assert!(audio.input_view().open);
    let before = backend.input_opens().len();
    let view = audio.set_audio_io(super::AudioSelection::default());
    assert!(view.status.failure.is_none());
    assert!(audio.input_view().open);
    assert!(backend.input_opens().len() > before);
}

#[test]
fn monitoring_is_off_until_the_user_turns_it_on() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    assert!(!audio.input_monitor_active());
    backend.push_input(&[0.5, -0.5], 2);
    let silent = monitor_block(&audio.input_port, 1);
    assert_eq!(silent, vec![0.0, 0.0], "monitoring is off by default");

    audio.set_input_monitor(true, 0.0);
    assert!(audio.input_monitor_active());
    // The switch flushes the stale tail so nothing pre-switch is heard.
    let flushed = monitor_block(&audio.input_port, 1);
    assert_eq!(flushed, vec![0.0, 0.0]);
    backend.push_input(&[0.5, -0.5], 2);
    let heard = monitor_block(&audio.input_port, 1);
    assert_eq!(
        heard,
        vec![0.5, -0.5],
        "monitoring sums capture into the output"
    );
}

#[test]
fn a_cold_monitor_opens_only_its_output_and_leaves_the_cursor_parked() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    audio.seek_frame(2_400, &clock).unwrap();
    assert!(backend.input_opens().is_empty());
    assert!(backend.opens().is_empty());

    let view = audio.set_input_monitor(true, 0.0);
    assert!(view.input.open);
    assert!(view.input.monitor_error.is_none());
    assert_eq!(backend.input_opens().len(), 1);
    assert_eq!(backend.opens().len(), 1);
    assert!(audio.idle_monitor.is_some());
    assert!(!audio.streaming_active);
    assert!(audio.render_thread.is_none());
    backend.push_input(&[0.25, -0.5], 2);
    let mut out = [0.0; 2];
    backend.fill_output(&mut out);
    assert_eq!(out, [0.25, -0.5]);
    let snapshot = audio.transport_snapshot(&clock).unwrap();
    assert_eq!(snapshot.position_seconds, 0.05);
    assert!(!snapshot.playing);

    audio.set_input_monitor(false, 0.0);
    assert!(!audio.main_stream.is_open());
    backend.push_input(&[0.75, 0.75], 2);
    backend.fill_output(&mut out);
    assert_eq!(out, [0.0, 0.0]);
}

#[test]
fn a_take_ignores_the_previous_runs_cursor_until_the_new_roll() {
    let mut audio = AudioState::new();
    audio.render_thread = Some(RenderThreadHandle::spawn(audio.sample_rate));
    let pb = Arc::clone(&audio.render_thread.as_ref().unwrap().playback);
    pb.play_cursor_frames.store(96_000, Ordering::Release);
    audio.pending_seek_frame = Some(24_000);
    assert_eq!(audio.take_progress_frame(), 0);

    audio.pending_seek_frame = None;
    audio.pending_start = Some(super::PendingStart {
        started: std::time::Instant::now(),
        frame: 24_000,
        resume: false,
        before_applied: pb.play_applied.load(Ordering::Acquire),
    });
    assert_eq!(audio.take_progress_frame(), 0);

    pb.count_in_remaining.store(512, Ordering::Release);
    pb.play_applied.fetch_add(1, Ordering::Release);
    assert_eq!(audio.take_progress_frame(), 0);
    pb.count_in_remaining.store(0, Ordering::Release);
    pb.play_cursor_frames.store(24_512, Ordering::Release);
    assert_eq!(audio.take_progress_frame(), audio.playhead_frame());
    assert!(audio.take_progress_frame() > 24_000);
}

#[test]
fn monitor_output_hands_off_to_the_synth_and_then_the_transport() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.set_input_monitor(true, 0.0);
    audio
        .live_synth_note_on("lead", || Ok(Default::default()), 120.0, 60, 100)
        .unwrap();
    assert!(audio.live_synth_active());
    assert!(audio.idle_monitor.is_none());
    assert!(audio.input_monitor_active());
    audio.stop();
    assert!(!audio.live_synth_active());
    assert!(audio.idle_monitor.is_some());
    assert!(audio.input_monitor_active());

    let project = native_test_project();
    audio
        .play(&project, &mut std::collections::HashMap::new())
        .unwrap();
    assert!(audio.idle_monitor.is_none());
    assert!(audio.streaming_active);
    audio.pause();
    audio.stop();
    assert!(audio.input_monitor_active());
    audio.set_input_monitor(false, 0.0);
    assert!(
        audio.main_stream.is_open(),
        "the transport retains its warm output"
    );
    assert!(!audio.input_monitor_active());
}

#[test]
fn idle_monitor_failure_keeps_capture_and_recovers_on_refresh() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.set_input_monitor(true, 0.0);
    backend.disconnect("headphones disconnected");
    audio.reap_idle_monitor_failure();
    let view = audio.audio_io_view();
    assert_eq!(
        view.input.monitor_error.as_deref(),
        Some("headphones disconnected")
    );
    assert!(view.input.open);
    assert!(view.input.error.is_none());
    assert!(!audio.input_monitor_active());

    let view = audio.refresh_audio_io();
    assert!(view.input.monitor_error.is_none());
    assert!(audio.input_monitor_active());
}

#[test]
fn a_refused_playback_open_leaves_the_idle_monitor_audible() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.set_input_monitor(true, 0.0);
    backend.fail_open("Speakers", "output busy");

    let project = native_test_project();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    audio.seek_frame(2_400, &clock).unwrap();
    let result = audio.play(&project, &mut std::collections::HashMap::new());
    assert!(result.is_err());
    assert_eq!(
        audio.transport_snapshot(&clock).unwrap().position_seconds,
        0.05
    );
    assert!(audio.idle_monitor.is_some());
    assert!(!audio.streaming_active);
    backend.push_input(&[0.25, -0.5], 2);
    let mut out = [0.0; 2];
    backend.fill_output(&mut out);
    assert_eq!(out, [0.25, -0.5]);
}

#[test]
fn monitoring_uses_the_accepted_output_rate_and_drops_a_closed_outputs_tail() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    audio.set_input_monitor(true, 0.0);
    backend.push_input(&[0.8, -0.8], 2);

    let source = audio.main_source(Arc::new(NoopSource));
    let callback = source.create(&super::StreamSpec::new(None, 48_000, None));
    callback.opened_at(96_000);
    backend.push_input(&[0.25, -0.25, 0.5, -0.5], 2);
    let mut heard = [0.0; 6];
    callback.fill(&mut heard);
    assert_eq!(heard, [0.25, -0.25, 0.375, -0.375, 0.5, -0.5]);
}

#[test]
fn the_monitor_gain_scales_what_is_heard() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    audio.set_input_monitor(true, -6.0);
    let _ = monitor_block(&audio.input_port, 1);
    backend.push_input(&[0.5, -0.5], 2);
    let heard = monitor_block(&audio.input_port, 1);
    let expected = 0.5 * 10f32.powf(-6.0 / 20.0);
    assert!(
        (heard[0] - expected).abs() < 1e-6,
        "{heard:?} vs {expected}"
    );
}

#[test]
fn turning_monitoring_off_stops_the_capture_being_added() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    audio.set_input_monitor(true, 0.0);
    let _ = monitor_block(&audio.input_port, 1);
    audio.set_input_monitor(false, 0.0);
    assert!(!audio.input_monitor_active());
    backend.push_input(&[0.75, 0.75], 2);
    let silent = monitor_block(&audio.input_port, 1);
    assert_eq!(silent, vec![0.0, 0.0]);
}

#[test]
fn the_level_meter_reads_the_peak_and_latches_a_clip() {
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    backend.push_input(&[0.25, -0.5], 2);
    let view = audio.input_view();
    assert!((view.level_peak - 0.5).abs() < 1e-9);
    assert!(!view.clipped);
    assert_eq!(
        audio.input_view().level_peak,
        0.0,
        "reading the meter resets it, so it shows the last block"
    );
    backend.push_input(&[1.25, 0.0], 2);
    let view = audio.input_view();
    assert!(
        view.clipped,
        "a sample at or above full scale latches the clip"
    );
}

#[test]
fn choosing_the_other_channel_layout_reopens_capture() {
    use oc_audio::audio_io::{InputChannels, InputSelection};
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    let before = backend.input_opens().len();
    audio.set_input_io(InputSelection {
        input_channels: InputChannels::Stereo,
        ..InputSelection::default()
    });
    let opens = backend.input_opens();
    assert!(opens.len() > before, "a channel change reopens the stream");
    assert_eq!(
        opens.last().expect("an open").1.channels,
        2,
        "the stream is rebuilt at the new channel count"
    );
    assert_eq!(audio.input_view().channels, 2);
}

#[test]
fn a_failed_capture_open_is_reported_and_leaves_nothing_open() {
    let backend = capture_backend();
    backend.fail_input_open("Built-in Microphone", "the device is busy");
    let mut audio = super::AudioState::with_backend(backend);
    audio.ensure_input();
    let view = audio.input_view();
    assert!(!view.open);
    assert_eq!(
        view.error.as_deref(),
        Some("Built-in Microphone: the device is busy")
    );
}

#[test]
fn a_machine_without_capture_devices_reports_the_reason() {
    use oc_audio::audio_io::{FakeBackend, InputSelection, OutputDevice};
    let backend = std::sync::Arc::new(FakeBackend::new(
        vec![OutputDevice {
            key: "coreaudio:out".into(),
            name: "Speakers".into(),
            is_default: true,
            channels: 2,
            sample_rates: vec![48_000],
            default_rate: 48_000,
            buffer_frames: None,
        }],
        Some("coreaudio:out".into()),
    ));
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    assert!(!audio.input_view().open);
    assert_eq!(backend.input_opens().len(), 0, "nothing was opened");
    let view = audio.set_input_io(InputSelection::default());
    assert!(matches!(
        view.status.input_failure,
        Some(oc_audio::audio_io::AudioFailure::NoInputDevices)
    ));
}

#[test]
fn a_dead_capture_stream_is_reported_as_an_input_notice() {
    use oc_audio::audio_io::AudioNotice;
    let backend = capture_backend();
    let mut audio = super::AudioState::with_backend(backend.clone());
    audio.ensure_input();
    backend.disconnect_input("the microphone was unplugged");
    let error = audio.input_port.take_error().expect("a capture error");
    audio.audio_io.note_input_failure(&error);
    let view = audio.audio_io_view();
    assert!(
        view.status.input_notices.iter().any(|notice| matches!(
            notice,
            AudioNotice::InputFailed { error } if error == "the microphone was unplugged"
        )),
        "{:?}",
        view.status.input_notices
    );
}

#[test]
fn opensynth_preset_preview_repeated_resume_delivers_new_nonzero_pcm() {
    use crate::render_thread::{RenderCommand, StreamingPlayback};
    use oc_dsl::ast::ParamValue;
    use std::time::{Duration, Instant};

    fn acknowledge_worker(state: &crate::state::AppState) {
        let (entered, acknowledgment) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        state
            .lock_inner()
            .audio
            .audition_render
            .as_ref()
            .unwrap()
            .send(RenderCommand::TestBarrier {
                entered,
                release: released,
            });
        // Do not leave a blocked worker if an assertion unwinds.
        release.send(()).unwrap();
        acknowledgment.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    fn consume_new_audible_block(playback: &StreamingPlayback, previous: u64) -> f64 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut block = [0.0; 256];
            streaming_audio_callback(&mut block, playback);
            assert!(block.iter().all(|sample| sample.is_finite()));
            let energy: f64 = block.iter().map(|&sample| f64::from(sample).powi(2)).sum();
            if energy > 1e-10 {
                assert!(playback.play_cursor_frames.load(Ordering::Acquire) > previous);
                return energy;
            }
            assert!(
                Instant::now() < deadline,
                "no new nonzero PCM after preset resume; previous={previous}, cursor={}, active={}, suspended={}, requested={}, applied={}",
                playback.play_cursor_frames.load(Ordering::Acquire),
                playback.active.load(Ordering::Acquire),
                playback.output_suspended.load(Ordering::Acquire),
                playback.position_requested.load(Ordering::Acquire),
                playback.position_applied.load(Ordering::Acquire)
            );
            std::thread::yield_now();
        }
    }

    let mut project = native_test_project();
    project.presets[0].params.extend([
        ("osc.a.waveform".into(), ParamValue::Number(1.0)),
        ("amp.attack".into(), ParamValue::Number(0.0)),
        ("amp.sustain".into(), ParamValue::Number(1.0)),
        ("output.loudness_trim_db".into(), ParamValue::Number(0.0)),
    ]);
    let state = crate::state::AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = project;
        inner.audio.enable_simulated_output();
        inner.audio.replace_private_audition(true).unwrap();
    }
    let generation = state.audition.begin("native");
    crate::audition::run(&state, "native", generation, Instant::now());
    let playback = {
        let mut inner = state.lock_inner();
        // Join the initially suspended simulated sink before becoming the sole
        // ring consumer. All following samples use the production callback.
        drop(inner.audio.audition_render_simulated.take());
        Arc::clone(&inner.audio.audition_render.as_ref().unwrap().playback)
    };
    acknowledge_worker(&state);
    state.audition_pause_intent.store(false, Ordering::Release);
    state.lock_inner().audio.resume_private_audition().unwrap();
    acknowledge_worker(&state);
    consume_new_audible_block(&playback, 0);
    let instances = playback.instrument_instantiations.load(Ordering::Acquire);
    let loads = playback.project_loads.load(Ordering::Acquire);
    assert!(instances > 0);
    for _ in 0..2 {
        state.audition_pause_intent.store(true, Ordering::Release);
        state.lock_inner().audio.pause_private_audition();
        acknowledge_worker(&state);
        let paused_cursor = playback.play_cursor_frames.load(Ordering::Acquire);
        for _ in 0..3 {
            let mut block = [1.0; 256];
            streaming_audio_callback(&mut block, &playback);
            assert!(block.iter().all(|&sample| sample == 0.0));
            assert_eq!(
                playback.play_cursor_frames.load(Ordering::Acquire),
                paused_cursor
            );
        }
        state.audition_pause_intent.store(false, Ordering::Release);
        state.lock_inner().audio.resume_private_audition().unwrap();
        acknowledge_worker(&state);
        assert!(consume_new_audible_block(&playback, paused_cursor) > 1e-10);
        assert!(state.audition.is_current(generation));
        assert_eq!(
            playback.instrument_instantiations.load(Ordering::Acquire),
            instances
        );
        assert_eq!(playback.project_loads.load(Ordering::Acquire), loads);
        assert!(Arc::ptr_eq(
            &playback,
            &state
                .lock_inner()
                .audio
                .audition_render
                .as_ref()
                .unwrap()
                .playback
        ));
    }
    // Consume the real finite figure through the device callback, not by
    // moving its cursor or declaring the producer finished.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !playback.has_played_out() {
        let mut block = [0.0; 1024];
        streaming_audio_callback(&mut block, &playback);
        assert!(block.iter().all(|sample| sample.is_finite()));
        assert!(Instant::now() < deadline, "audition never played out");
        std::thread::yield_now();
    }
    {
        let mut inner = state.lock_inner();
        inner.audio.pause_private_audition();
        assert!(
            !inner.audio.audition_paused,
            "completed output is not a paused preview"
        );
        inner.audio.resume_private_audition().unwrap();
        let mut snapshot = state.audition.snapshot();
        inner.audio.fill_audition_snapshot(&mut snapshot);
        assert!(snapshot.finished);
        assert!(!snapshot.paused);
        inner.audio.replace_private_audition(true).unwrap();
    }
    let fresh_generation = state.audition.begin("native");
    crate::audition::run(&state, "native", fresh_generation, Instant::now());
    let fresh = {
        let mut inner = state.lock_inner();
        drop(inner.audio.audition_render_simulated.take());
        Arc::clone(&inner.audio.audition_render.as_ref().unwrap().playback)
    };
    assert_ne!(fresh_generation, generation);
    assert!(!Arc::ptr_eq(&fresh, &playback));
    acknowledge_worker(&state);
    state.audition_pause_intent.store(false, Ordering::Release);
    state.lock_inner().audio.resume_private_audition().unwrap();
    acknowledge_worker(&state);
    consume_new_audible_block(&fresh, 0);
    state.audition.cancel_all();
    state.lock_inner().audio.stop_audition();
}

#[test]
fn bounded_private_preview_finishes_at_its_run_end_not_a_pending_replay() {
    use crate::render_thread::RenderCommand;
    use std::time::{Duration, Instant};
    let mut project = native_test_project();
    project.presets[0].params.push((
        "output.loudness_trim_db".into(),
        oc_dsl::ast::ParamValue::Number(0.0),
    ));
    let mut audio = AudioState::new();
    audio.enable_simulated_output();
    audio.replace_private_audition(true).unwrap();
    let prepared = AudioState::open_audition_render_window(
        &audio.audio_io,
        &audio.audio_io.spec(),
        PathBuf::new(),
        project,
        true,
        crate::first_block::now_ns(),
        StartPath::ClipPreview,
        0,
        Some(512),
    )
    .unwrap();
    audio.install_audition_render(prepared).unwrap();
    drop(audio.audition_render_simulated.take());
    let playback = Arc::clone(&audio.audition_render.as_ref().unwrap().playback);
    audio.audition_pause_intent.store(false, Ordering::Release);
    audio.resume_private_audition().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !audio.private_audition_finished() {
        let mut block = [0.0; 256];
        streaming_audio_callback(&mut block, &playback);
        assert!(
            Instant::now() < deadline,
            "bounded preview failed to finish"
        );
        std::thread::yield_now();
    }
    assert_eq!(playback.run_output_end_frame(), 512);
    assert_eq!(playback.timeline_frames(), 512);
    audio.pause_private_audition();
    assert!(!audio.audition_paused);
    let render = audio.audition_render.as_ref().unwrap();
    let (entered, acknowledgment) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    render.send(RenderCommand::TestBarrier {
        entered,
        release: released,
    });
    acknowledgment.recv_timeout(Duration::from_secs(5)).unwrap();
    render.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: Some(512),
    });
    // A queued new run must not inherit completion from the previous callback cursor.
    let pending_finished = audio.private_audition_finished();
    release.send(()).unwrap();
    assert!(!pending_finished);
    audio.stop_audition();
}
