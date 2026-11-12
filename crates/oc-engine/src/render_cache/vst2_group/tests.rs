use super::*;
use oc_host::preset_parameters::ParameterWriteDomain;
use oc_host::vst2::{Vst2AuthoredClaim, Vst2AuthoredInput};
fn plan() -> Vst2OwnedAuthoredPlan {
    Vst2OwnedAuthoredPlan {
        path: "controlled-native-source".into(),
        sample_rate: 48_000,
        max_frames: 512,
        state: Some(vec![1, 2]),
        parameters: vec![Vst2AuthoredInput {
            name: "Gain".into(),
            native_value: Some(0.25),
            claim: Vst2AuthoredClaim::NativeOnly,
            domain: ParameterWriteDomain::Normalized,
        }],
    }
}
#[test]
fn vst2_group_shared_initialization_refuses_state_and_track_parameter_loss() {
    let left = plan();
    let mut right = plan();
    assert!(require_shared_initialization(&left, &right).is_ok());
    right.parameters[0].native_value = Some(0.75);
    assert!(require_shared_initialization(&left, &right).is_err());
    right = plan();
    right.state = Some(vec![3]);
    assert!(require_shared_initialization(&left, &right).is_err());
    right = plan();
    right.parameters[0].name = "gain_alias".into();
    assert!(require_shared_initialization(&left, &right).is_err());
    right = plan();
    right.parameters[0].claim = Vst2AuthoredClaim::InstrumentOutputRouting;
    assert!(require_shared_initialization(&left, &right).is_err());
}
#[test]
fn vst2_group_split_pattern_lane_pair_validation_uses_lane_project_indices() {
    let original = oc_dsl::parse("@project\n@preset a\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@preset b\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@pattern p | preset: b, instrument: synth/lead\n  events:\n    @0 C4 q\n@pattern q | preset: a, instrument: synth/lead\n  events:\n    @0 D4 q\n@track t | preset: a, instrument: synth/lead\n  clip: p [1:1 - 2:1]\n  clip: q [2:1 - 3:1]\n").unwrap();
    let resolved = oc_dsl::preset::resolve(&original).unwrap();
    let lanes = crate::source_lanes::plan(&resolved).unwrap();
    let lane_project = lanes.tracks();
    assert!(lane_project.tracks.len() > original.tracks.len());
    for lane in 0..lane_project.tracks.len() {
        require_main_pair_only(lane_project, &[lane], "VST2", "b", 48_000, None).unwrap();
    }
}

fn shared_project(note: &str) -> Project {
    oc_dsl::parse(&format!("@project\n@preset shared\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@track a | preset: shared, instrument: synth/lead\n  engine_params: {{plugin_output_pair: 1}}\n  @phrase a [1:1 - 2:1]\n    {note} q\n@track b | preset: shared, instrument: synth/lead\n  engine_params: {{plugin_output_pair: 1}}\n  @phrase b [1:1 - 2:1]\n    E4 q\n")).unwrap()
}
#[test]
fn vst2_group_warm_single_track_edit_rerenders_every_shared_source_member() {
    // This owner regression injects external group execution, not native DSP.
    fn execute(request: Vst2GroupRequest<'_>) -> Result<Vst2GroupOutput, String> {
        assert_eq!(
            request.tracks.len(),
            2,
            "shared group must retain both source inputs"
        );
        let sum: u32 = request
            .events
            .iter()
            .flat_map(|events| events.iter())
            .filter_map(|event| match event.event {
                MidiMessage::NoteOn { note, .. } => Some(u32::from(note)),
                _ => None,
            })
            .sum();
        assert!(sum >= 120, "both tracks must contribute their notes");
        Ok(Vst2GroupOutput {
            buffers: request
                .tracks
                .iter()
                .map(|_| vec![sum as f32 / 1000.0; request.total_frames * 2])
                .collect(),
            latency: 0,
        })
    }
    let mut cache = RenderCache::new();
    cache.set_vst2_tail_probe(|_| Ok(None));
    cache.set_vst2_group_renderer(execute);
    let mut plugins = HashMap::new();
    cache
        .render_full(
            &shared_project("C4"),
            Path::new("unused.sf2"),
            8_000,
            &mut plugins,
        )
        .unwrap();
    let before_a = cache.get_track_buffer(0).unwrap()[0];
    let before_b = cache.get_track_buffer(1).unwrap()[0];
    assert_eq!(before_a, before_b);
    cache.mark_track_dirty(0);
    cache
        .render_incremental(
            &shared_project("D4"),
            Path::new("unused.sf2"),
            8_000,
            &mut plugins,
        )
        .unwrap();
    let after_a = cache.get_track_buffer(0).unwrap()[0];
    let after_b = cache.get_track_buffer(1).unwrap()[0];
    assert_ne!(after_a, before_a);
    assert_ne!(after_b, before_b);
    assert_eq!(after_a, after_b);
}
#[test]
fn vst2_group_subset_expands_explicit_shared_outputs_only() {
    let project = shared_project("C4");
    for index in [0, 1] {
        let mut selected = vec![index];
        expand_shared_output_groups(&project, &mut selected);
        assert_eq!(selected, [0, 1]);
    }
    let mut independent = project;
    for track in &mut independent.tracks {
        track.engine_params = None;
    }
    let mut selected = vec![0];
    expand_shared_output_groups(&independent, &mut selected);
    assert_eq!(selected, [0]);
    let mut dirty = vec![true, false];
    propagate_shared_plugin_dirty(&independent, &mut dirty);
    assert_eq!(dirty, [true, false]);
}

#[test]
fn vst2_group_cache_preflight_uses_original_alias_order_not_flattened_positions() {
    let original = oc_dsl::parse("@project\n@preset parent\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {plugin_output_pair: 1, output_pair: 2}\n@preset child\n  extends: parent\n  params: {plugin_output_pair: 0}\n@track t | preset: child, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 q\n").unwrap();
    fn execute(request: Vst2GroupRequest<'_>) -> Result<Vst2GroupOutput, String> {
        let rows: Vec<_> = request
            .plugin
            .parameters
            .iter()
            .filter(|row| matches!(row.name.as_str(), "plugin_output_pair" | "output_pair"))
            .collect();
        assert_eq!(
            rows.iter().map(|row| row.native_value).collect::<Vec<_>>(),
            [Some(1.0), Some(2.0), Some(0.0)]
        );
        assert!(
            rows.iter()
                .all(|row| row.claim == Vst2AuthoredClaim::InstrumentOutputRouting)
        );
        Ok(Vst2GroupOutput {
            buffers: vec![vec![0.25; request.total_frames * 2]],
            latency: 0,
        })
    }
    let mut cache = RenderCache::new();
    cache.set_vst2_tail_probe(|_| Ok(None));
    cache.set_vst2_group_renderer(execute);
    let audio = cache
        .render_full(
            &original,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(audio.iter().any(|value| *value != 0.0));
    assert!(cache.master_buffer().is_some());
}
