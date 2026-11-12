//! Mix, parallel-render and auxiliary-output tests.

use super::super::*;

#[test]
#[ignore = "performance benchmark; run with --ignored --nocapture"]
fn parallel_builtin_tracks_are_deterministic_and_faster() {
    use crate::interpret::{MidiEvent, MidiMessage};
    use oc_synth::synth::document::PatchDocument;
    use oc_synth::synth::{
        LfoSettings, LfoShape, ModDestination, ModRoute, ModSource, SynthFx, SynthFxType,
        SynthPatch, WarpMode,
    };

    let patch = SynthPatch {
        oscillator_a_level: 0.4,
        oscillator_b_level: 0.3,
        oscillator_c_level: 0.3,
        sub_level: 0.1,
        noise_level: 0.02,
        unison_voices: 4,
        extra_lfos: std::array::from_fn(|_| LfoSettings {
            rate: 2.0,
            shape: std::sync::Arc::new(LfoShape::sine()),
            sync: false,
            retrigger: false,
            division: 2,
            ..LfoSettings::default()
        }),
        mod_routes: vec![
            ModRoute {
                source: ModSource::Lfo,
                destination: ModDestination::FilterCutoff,
                amount: 0.1,
                ..ModRoute::default()
            };
            64
        ],
        oscillator_a_warp_mode: WarpMode::BendPos,
        oscillator_a_warp_amount: 0.7,
        oscillator_b_warp_mode: WarpMode::AsymPos,
        oscillator_b_warp_amount: 0.7,
        oscillator_c_warp_mode: WarpMode::RmOsc,
        oscillator_c_warp_amount: 0.7,
        fx: vec![
            SynthFx::new(SynthFxType::Drive),
            SynthFx::new(SynthFxType::Compressor),
            SynthFx::new(SynthFxType::Eq3),
            SynthFx::new(SynthFxType::Delay),
            SynthFx::new(SynthFxType::Reverb),
            SynthFx::new(SynthFxType::Chorus),
        ],
        ..Default::default()
    };
    let expected_latency = patch.fx_latency_samples(48_000) as u32;
    let params = PatchDocument {
        version: 1,
        name: "parallel-perf".into(),
        author: "test".into(),
        category: "test".into(),
        use_policy: "test".into(),
        patch,
    }
    .to_ocp_params()
    .expect("dense patch exports");

    let total_samples = 48_000 * 5;
    let mut events = Vec::new();
    for note in 48..64_u8 {
        events.push(MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity: 100,
            },
            track_index: 0,
        });
    }
    for note in 48..64_u8 {
        events.push(MidiEvent {
            time_samples: total_samples as u64 - 1,
            event: MidiMessage::NoteOff { channel: 0, note },
            track_index: 0,
        });
    }

    let track_count = std::env::var("SYNTH_PERF_TRACKS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8);
    let jobs: Vec<super::BuiltinTrackJob> = (0..track_count)
        .map(|index| super::BuiltinTrackJob {
            index,
            plugin_id: oc_synth::synth::PLUGIN_ID.to_string(),
            preset_name: "parallel synth".to_string(),
            params: params.clone(),
            events: events.clone(),
            performance: None,
        })
        .collect();
    let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).expect("clock");

    let start = std::time::Instant::now();
    let parallel = super::render_builtin_tracks_parallel(
        jobs.clone(),
        BuiltinRenderContext {
            total_samples,
            start_sample: 0,
            sample_rate: 48_000,
            tempo: 120.0,
            resource_root: None,
            clock: &clock,
            cancel: None,
            worker_options: crate::render_parallel::RenderWorkerOptions::default(),
        },
    )
    .expect("parallel render");
    let parallel_time = start.elapsed();

    let start = std::time::Instant::now();
    let mut sequential = Vec::new();
    for job in &jobs {
        let event_refs: Vec<&MidiEvent> = job.events.iter().collect();
        let buffer = super::render_builtin_project(
            &job.plugin_id,
            Some(&job.params),
            &event_refs,
            total_samples,
            0,
            48_000,
            120.0,
            None,
            Some(&clock),
            None,
        )
        .expect("sequential render");
        sequential.push((job.index, buffer));
    }
    let sequential_time = start.elapsed();

    for ((pa, pb, pl), (sa, sb)) in parallel.iter().zip(sequential.iter()) {
        assert_eq!(pa, sa);
        // The dense patch carries a delay and a shaper, so its rack reports
        // latency; every worker must report exactly that.
        assert_eq!(*pl, expected_latency);
        assert_eq!(pb.len(), sb.len());
        assert!(
            pb.iter()
                .zip(sb.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "parallel rendering must be deterministic"
        );
    }
    println!(
        "{track_count} dense tracks: parallel {parallel_time:?}, sequential {sequential_time:?}, speedup {:.2}x",
        sequential_time.as_secs_f64() / parallel_time.as_secs_f64()
    );
    assert!(
        parallel_time < sequential_time,
        "parallel render should beat sequential: {parallel_time:?} vs {sequential_time:?}"
    );
}
#[test]
fn two_bus_fake_plugin_routes_auxiliary_output_to_its_bus() {
    let project = oc_dsl::parse(
            "@project\n  tempo: 120\n@preset fake\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n@track host | preset: fake, instrument: keys/piano\n  effects:\n    - gain: factor=0.5\n  mix: send(host.out2->drums=100%)\n  @phrase line [1:1 - 2:1]\n    C4 q\n@bus drums\n  routes: [master]\n",
        )
        .unwrap();
    assert_eq!(
        project.tracks[0].mix.sends[0]
            .source
            .as_ref()
            .unwrap()
            .pair(),
        1
    );

    // Fixed source PCM isolates downstream graph/send behavior. Actual
    // UI-owned group acquisition and processing is covered by the CLI pipeline
    // and native-main multi-output audits, never by a surrogate UI test thread.
    let mut buffers = vec![[1.0, 1.5].repeat(1024)];
    let aux_buffers = HashMap::from([((0usize, 1usize), [2.0, 2.5].repeat(1024))]);

    assert_eq!(buffers[0][0], 1.0);
    assert_eq!(aux_buffers[&(0, 1)][0], 2.0);

    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[0]).unwrap();
    graph.process_track_effects(&mut buffers, None);
    assert!((buffers[0][0] - 0.5).abs() < 1e-6);

    let mut stems = crate::graph::StemCapture::default();
    let master =
        graph.mix_processed_with_stems_and_aux(&mut buffers, None, Some(&mut stems), &aux_buffers);
    // The fake plugin emits distinct L/R, so the stereo balance law is unity at centre.
    assert!((master[0] - 2.5).abs() < 1e-6);
    let drums = stems
        .buses
        .iter()
        .find(|(name, _)| name == "drums")
        .expect("drums bus stem");
    assert_eq!(drums.1[0], 2.0);
    let host = stems
        .tracks
        .iter()
        .find(|(name, _)| name == "host")
        .expect("host track stem");
    assert!((host.1[0] - 0.5).abs() < 1e-6);
}

#[test]
fn fake_plugin_aux_send_applies_level_and_stereo_balance() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset fake\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n@track host | preset: fake, instrument: keys/piano\n  mix: output = sink, send(host.out2->drums=+6dB, pan=-0.5)\n  @phrase line [1:1 - 2:1]\n    C4 q\n@bus sink\n  effects:\n    - gain: factor=0.0\n  routes: [master]\n@bus drums\n  routes: [master]\n",
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    // Fixed source PCM isolates downstream graph/send behavior. Actual
    // UI-owned group acquisition and processing is covered by the CLI pipeline
    // and native-main multi-output audits, never by a surrogate UI test thread.
    let mut buffers = vec![[1.0, 1.5].repeat(1024)];
    let aux_buffers = HashMap::from([((0usize, 1usize), [2.0, 2.5].repeat(1024))]);
    assert!(aux_buffers[&(0, 1)][0] > 0.0);

    let mut graph =
        AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[0]).unwrap();
    graph.process_track_effects(&mut buffers, None);
    let master = graph.mix_processed_with_stems_and_aux(&mut buffers, None, None, &aux_buffers);

    // Aux bus 1 carries 2.0/2.5 per frame. The balance law keeps the near side
    // at unity and attenuates the far side to 0.5; the send level is +6 dB,
    // while the main pair is muted by `sink`.
    let amount = 10.0_f64.powf(6.0 / 20.0);
    assert!((f64::from(master[0]) - 2.0 * 1.0 * amount).abs() < 1e-5);
    assert!((f64::from(master[1]) - 2.5 * 0.5 * amount).abs() < 1e-5);
}

#[test]
fn builtin_synth_fx_latency_is_reported_for_source_compensation() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { fx.1.type: 3.0, fx.1.mix: 0.5, fx.2.type: 6.0, fx.2.mix: 0.5 }\n@track keys | instrument: synth/virtual_analog, preset: p\n  @phrase line [1:1 - 2:1]\n    C4 q\n",
    )
    .unwrap();
    let track = &project.tracks[0];
    let preset = project
        .presets
        .iter()
        .find(|preset| Some(&preset.name) == track.preset.as_ref());
    let params = crate::preset::builtin_track_params(&project, preset, track);
    let (_, latency) = super::render_builtin_project_with_latency(
        oc_synth::synth::PLUGIN_ID,
        Some(&params),
        &[],
        4_800,
        0,
        48_000,
        120.0,
        None,
        None,
        None,
    )
    .expect("builtin render");
    // 62.5 ms delay at 48 kHz plus the 8x shaper's 97 samples.
    assert_eq!(latency, 3_000 + 97);
}

#[test]
fn builtin_synth_compressor_latency_is_reported_for_source_compensation() {
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { fx.1.type: 1.0 }\n@track keys | instrument: synth/virtual_analog, preset: p\n  @phrase line [1:1 - 2:1]\n    C4 q\n",
    )
    .unwrap();
    let track = &project.tracks[0];
    let preset = project
        .presets
        .iter()
        .find(|preset| Some(&preset.name) == track.preset.as_ref());
    let params = crate::preset::builtin_track_params(&project, preset, track);
    let (_, latency) = super::render_builtin_project_with_latency(
        oc_synth::synth::PLUGIN_ID,
        Some(&params),
        &[],
        4_800,
        0,
        48_000,
        120.0,
        None,
        None,
        None,
    )
    .expect("builtin render");
    // The three-band compander's 2 ms lookahead limiter at 48 kHz.
    assert_eq!(latency, 96);
}

#[test]
fn explicit_worker_snapshot_survives_cache_reuse_and_other_requests() {
    let serial = crate::render_parallel::RenderWorkerOptions::from_pin(Some("1"), || 8);
    let parallel = crate::render_parallel::RenderWorkerOptions::from_pin(Some("3"), || 8);
    let mut a = RenderCache::new();
    let mut b = RenderCache::new();
    a.set_worker_options(serial);
    b.set_worker_options(parallel);
    assert_eq!(a.worker_options, Some(serial));
    assert_eq!(b.worker_options, Some(parallel));
    a.clear_render_controls();
    a.mark_all_dirty();
    assert_eq!(a.worker_options, Some(serial));
}

#[test]
fn offline_failure_boundary_discards_failed_track_bus_master_and_post_fx_caches() {
    struct Failure(std::sync::atomic::AtomicBool);
    impl oc_audio::AudioEffect for Failure {
        fn process(&mut self, buffer: &mut [f32], _: u32) {
            buffer.fill(99.0);
            self.0.store(true, Ordering::Release);
        }
        fn processing_failed(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
            oc_audio::ProcessingFailureReport::Ready("native process: graph fixture cause".into())
        }
        fn set_param(&mut self, _: &str, _: f64) {}
        fn get_param(&self, _: &str) -> Option<f64> {
            None
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "offline fault fixture"
        }
    }
    let project = oc_dsl::parse("@project\n@track tone | instrument: synth/lead\n  mix: send(room=50%)\n@bus room\n  routes: [master]\n").unwrap();
    for slot in ["track", "bus", "master", "vst2"] {
        let mut cache = RenderCache::new();
        cache.sample_rate = 8_000;
        cache.track_buffers = vec![Some(vec![0.25; 256])];
        cache.track_dirty = vec![false];
        cache.master_buffer = Some(vec![0.25; 256]);
        cache.mix_dirty = false;
        cache.remember_post_fx_snapshot(0, vec![0.25; 256]);
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 8_000, &[]).unwrap();
        let effect: Box<dyn oc_audio::AudioEffect> = if slot == "vst2" {
            let path = oc_host::vst2::test_support::fixture_path("OpenCompose Gain Fixture")
                .expect("host fixture");
            let plugin = Vst2Instance::load(&path, 8_000, 128).unwrap();
            oc_host::vst2::test_support::notify_io_changed(&plugin);
            Box::new(plugin)
        } else {
            Box::new(Failure(std::sync::atomic::AtomicBool::new(false)))
        };
        match slot {
            "track" | "vst2" => graph.track_chains[0].effects.push(effect),
            "bus" => graph
                .bus_chains
                .get_mut("room")
                .unwrap()
                .chain
                .effects
                .push(effect),
            _ => graph.master_chain.effects.push(effect),
        }
        let mut capture = Some(crate::graph::StemCapture::default());
        let result = cache.mix_graph(
            &project,
            GraphMixOptions {
                sample_rate: 8_000,
                total_samples: 128,
                timeline_start: 0,
                selection: None,
                window: None,
                workers: crate::render_parallel::RenderWorkerOptions::default(),
            },
            &mut capture,
            graph,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("failed {slot} must not return publishable audio"),
        };
        if slot == "vst2" {
            assert!(
                error.contains("VST2") && error.contains("routing changed"),
                "{error}"
            );
        } else {
            assert!(error.contains("graph fixture cause"), "{error}");
        }
        assert!(cache.master_buffer().is_none());
        assert_eq!(cache.cached_count(), 0);
        assert_eq!(cache.post_fx_snapshot_bytes(), 0);
        assert!(cache.is_dirty());
        let capture = capture.unwrap();
        assert!(
            capture.tracks.is_empty()
                && capture.buses.is_empty()
                && capture.master_inputs.is_empty()
                && capture.pre_master.is_empty()
        );
    }
}

#[test]
fn offline_failure_boundary_vst2_source_refuses_warm_cache_stems_and_wav_publication() {
    // Native IOChanged acceptance runs on the actual-main CLI harness. This
    // injected external-source boundary proves cache/stem/file publication.
    fn source_boundary(
        request: super::super::vst2_group::Vst2GroupRequest<'_>,
    ) -> Result<super::super::vst2_group::Vst2GroupOutput, String> {
        if request.performance.is_some_and(|commands| {
            commands.iter().any(|command| {
                matches!(
                    command.message,
                    oc_dsl::performance::ChannelCommand::ControlChange {
                        controller: 119,
                        ..
                    }
                )
            })
        }) {
            return Err("VST2 source routing changed (injected external-source boundary)".into());
        }
        Ok(super::super::vst2_group::Vst2GroupOutput {
            buffers: request
                .tracks
                .iter()
                .map(|_| vec![0.25; request.total_frames * 2])
                .collect(),
            latency: 0,
        })
    }
    fn project(fail: bool) -> Project {
        let request = if fail { "@0 ch0 cc 119 127;" } else { "" };
        oc_dsl::parse(&format!("@project\n  tempo: 120\n@preset p\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@track tone | preset: p, instrument: synth/lead\n  performance: {{ @0 ch0 note_on 60 100; {request} @1 ch0 note_off 60 0; }}\n")).unwrap()
    }
    let healthy = project(false);
    let broken = project(true);
    let mut cache = RenderCache::new();
    // This test owns source processing and publication, not native UI metadata.
    cache.set_vst2_tail_probe(|_| Ok(None));
    cache.set_vst2_group_renderer(source_boundary);
    cache.capture_stems(true);
    let mut plugins = HashMap::new();
    let original = cache
        .render_full(&healthy, Path::new("unused.sf2"), 8_000, &mut plugins)
        .unwrap();
    assert!(original.iter().any(|sample| *sample != 0.0));
    assert!(cache.duration() > 0.0);
    let error = cache
        .render_incremental(&broken, Path::new("unused.sf2"), 8_000, &mut plugins)
        .unwrap_err();
    assert!(
        error.contains("VST2") && error.contains("routing changed"),
        "{error}"
    );
    assert!(cache.master_buffer().is_none());
    assert_eq!(cache.cached_count(), 0);
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);
    assert!(cache.stem_capture.as_ref().unwrap().tracks.is_empty());
    assert_eq!(cache.duration(), 0.0);
    assert!(cache.is_dirty());
    let recovered = cache
        .render_incremental(&healthy, Path::new("unused.sf2"), 8_000, &mut plugins)
        .unwrap();
    assert_eq!(recovered, original);
    assert!(cache.duration() > 0.0);

    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("failed.wav");
    let export = || {
        let mut cache = RenderCache::new();
        cache.set_vst2_tail_probe(|_| Ok(None));
        cache.set_vst2_group_renderer(source_boundary);
        crate::render::render_to_wav_with_options_and_cache(
            &broken,
            Path::new("unused.sf2"),
            &path,
            &mut HashMap::new(),
            crate::render::RenderOptions::new(8_000, Default::default()),
            crate::encode::EncodeSpec::for_project(&broken, 8_000),
            cache,
        )
    };
    let error = export().unwrap_err();
    assert!(
        error.contains("VST2") && error.contains("routing changed"),
        "{error}"
    );
    assert!(!path.exists(), "failed source must not create an export");
    std::fs::write(&path, b"existing export").unwrap();
    let error = export().unwrap_err();
    assert!(
        error.contains("VST2") && error.contains("routing changed"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"existing export");
}
