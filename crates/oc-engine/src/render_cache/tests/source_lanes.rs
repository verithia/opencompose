//! Shared source ownership through the production offline renderer.

use super::super::*;

const RATE: u32 = 8_000;

fn project() -> Project {
    oc_dsl::parse(
        r#"@project
  tempo: 120
@preset voice
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.05, waveform: 0}
@pattern first | preset: voice, instrument: synth/lead
  events:
    @0 C4 q mf
@pattern second | preset: voice, instrument: synth/lead
  events:
    @0 C4 q mf
@track sum | preset: voice, instrument: synth/lead
  clip: first [1:1 - 1:2]
  clip: second [1:1 - 1:2]
"#,
    )
    .unwrap()
}

fn effects(line: &str) -> Vec<oc_dsl::ast::Effect> {
    oc_dsl::parse(&format!(
        "@project\n@track fx | instrument: synth/lead\n  effects:\n    - {line}\n"
    ))
    .unwrap()
    .tracks
    .remove(0)
    .effects
}

fn render(project: &Project) -> Vec<f32> {
    RenderCache::new()
        .render_full(project, Path::new("unused.sf2"), RATE, &mut HashMap::new())
        .unwrap()
}

#[test]
fn preset_then_pattern_inserts_sum_before_the_parent_insert_exactly_once() {
    let mut project = project();
    let mut reference = project.clone();
    let first = reference.tracks[0].pattern_clip(0).unwrap().clone();
    reference.tracks[0]
        .clips
        .set_pattern_clips(std::iter::once(first));
    let raw = render(&reference);
    project.presets[0].effects = effects("gain: factor=0.4");
    project.patterns[0].effects = effects("gain: factor=0.25");
    project.patterns[1].effects = effects("gain: factor=0.75");
    project.tracks[0].effects = effects("gain: factor=0.5");
    let actual = render(&project);
    assert_eq!(actual.len(), raw.len());
    assert!(raw.iter().any(|sample| sample.abs() > 1e-4));
    // Two identical independent sources have gains .4*.25 and .4*.75,
    // followed by one parent gain .5. Allow only f32 operation ordering error.
    for (actual, raw) in actual.iter().zip(raw) {
        assert!((*actual - raw * 0.2).abs() < 1e-7);
    }
}

#[test]
fn offline_source_latency_aligns_both_pattern_lanes_audibly() {
    let mut project = project();
    let raw = render(&project);
    project.patterns[0].effects = effects("limiter: ceiling=-0.8dB");
    let plan = crate::source_lanes::plan(&project).unwrap();
    let latency = crate::source_lanes::effect_chain(&project, &plan.tracks().tracks[0], RATE)
        .unwrap()
        .latency_samples() as usize;
    assert!(latency > 0);
    let actual = render(&project);
    assert_eq!(actual.len(), raw.len() + latency * 2);
    assert!(actual[..latency * 2].iter().all(|sample| *sample == 0.0));
    assert!(
        actual[latency * 2..]
            .iter()
            .any(|sample| sample.abs() > 1e-4)
    );
    for (actual, expected) in actual[latency * 2..].iter().zip(raw) {
        assert!((*actual - expected).abs() < 1e-7);
    }
}

#[test]
fn source_insert_declared_tail_extends_the_render_horizon() {
    let mut project = project();
    let raw = render(&project);
    project.patterns[0].effects = effects("doubler: voices=2, delay=18ms, mix=60%");
    let plan = crate::source_lanes::plan(&project).unwrap();
    let tail = crate::source_lanes::effect_chain(&project, &plan.tracks().tracks[0], RATE)
        .unwrap()
        .tail_samples() as usize;
    assert!(tail > 0);
    let actual = render(&project);
    assert_eq!(actual.len(), raw.len() + tail * 2);
}

#[test]
fn source_content_and_inserts_invalidate_a_same_layout_cache() {
    let mut project = project();
    let mut cache = RenderCache::new();
    let mut plugins = HashMap::new();
    let first = cache
        .render_incremental(&project, Path::new("unused.sf2"), RATE, &mut plugins)
        .unwrap();
    let before = cache.render_counters();
    project.patterns[0].effects = effects("gain: factor=0.25");
    let changed = cache
        .render_incremental(&project, Path::new("unused.sf2"), RATE, &mut plugins)
        .unwrap();
    assert_ne!(first, changed);
    assert_eq!(
        cache.render_counters().source_renders - before.source_renders,
        2
    );
    let before = cache.render_counters();
    project.patterns[0].instrument = oc_dsl::ast::InstrumentPath::new("strings", "violin");
    cache
        .render_incremental(&project, Path::new("unused.sf2"), RATE, &mut plugins)
        .unwrap();
    assert_eq!(
        cache.render_counters().source_renders - before.source_renders,
        2
    );
    let before = cache.render_counters();
    project.presets[0]
        .params
        .iter_mut()
        .find(|(key, _)| key == "frequency")
        .unwrap()
        .1 = oc_dsl::ast::ParamValue::Number(660.0);
    let changed_preset = cache
        .render_incremental(&project, Path::new("unused.sf2"), RATE, &mut plugins)
        .unwrap();
    assert_ne!(changed, changed_preset);
    assert_eq!(
        cache.render_counters().source_renders - before.source_renders,
        2
    );
}

#[test]
fn overlapping_same_preset_notes_keep_the_second_placement_alive() {
    let mut project = project();
    project.patterns[0].events[0].duration.base = oc_dsl::ast::BaseDuration::Half;
    project.patterns[1].events[0].duration.base = oc_dsl::ast::BaseDuration::Half;
    // Shift the second two-beat note one beat later. Its shared-pitch note-off
    // must remain independent of the first placement's release at one second.
    project.tracks[0]
        .pattern_clip_mut(0)
        .unwrap()
        .time_range
        .end
        .beat = 3;
    project.tracks[0]
        .pattern_clip_mut(1)
        .unwrap()
        .time_range
        .start
        .beat = 2;
    project.tracks[0]
        .pattern_clip_mut(1)
        .unwrap()
        .time_range
        .end
        .beat = 4;
    let actual = render(&project);
    let from = (1.1 * f64::from(RATE)) as usize * 2;
    let to = (1.4 * f64::from(RATE)) as usize * 2;
    assert!(actual[from..to].iter().any(|sample| sample.abs() > 1e-4));
}

fn shared_output_project() -> Project {
    let mut project = project();
    project.presets[0].source = Some("vst3".into());
    project.presets[0].plugin = Some(oc_host::vst3_instance::test_support::FAKE_TWO_BUS_ID.into());
    project.presets[0].params.clear();
    project.tracks[0].output_port = Some(0);
    project.tracks[0].output_pair = Some(0);
    let mut second_output = project.tracks[0].clone();
    second_output.name = "second_output".into();
    second_output.output_port = Some(1);
    second_output.clips.set_pattern_clips(std::iter::empty());
    project.tracks.push(second_output);
    project
}

#[test]
fn shared_plugin_output_coalesces_one_owners_placements_and_fans_distinct_stems_once() {
    let mut project = shared_output_project();
    for pattern in &mut project.patterns {
        pattern.effects = effects("gain: factor=0.5");
    }
    let plan = crate::source_lanes::plan(&project).unwrap();
    assert_eq!(plan.targets, vec![0, 1]);
    assert_eq!(plan.tracks().tracks[0].pattern_clip_count(), 2);
    assert_eq!(
        super::super::helpers::group_tracks_by_preset(plan.tracks(), &[0, 1]),
        vec![vec![0, 1]]
    );
    // Runtime source_renders==2 and distinct .5/2.0 stem samples are asserted
    // by the same-named CLI actual-main pipeline test. A libtest worker cannot
    // own the application UI executor.
}

#[test]
fn shared_output_rejects_source_inserts_or_instruments_that_cannot_be_isolated() {
    for instrument_conflict in [false, true] {
        let mut project = shared_output_project();
        if instrument_conflict {
            project.patterns[1].instrument = oc_dsl::ast::InstrumentPath::new("strings", "violin");
        } else {
            project.patterns[1].effects = effects("gain: factor=0.25");
        }
        let error = RenderCache::new()
            .render_full(&project, Path::new("unused.sf2"), RATE, &mut HashMap::new())
            .unwrap_err();
        assert!(error.contains("Unsupported source chain"), "{error}");
        assert!(error.contains("sum") && error.contains("voice"), "{error}");
        assert!(
            error.contains("separate presets") && error.contains("parent track"),
            "{error}"
        );
    }
}
