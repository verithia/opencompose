//! Contract tests for the retired effect vocabulary accepted by the render path.

use std::path::{Path, PathBuf};

use oc_dsl::ast::{EffectType, ParamValue, Project};
use oc_engine::dsp::{ResolvedEffect, legacy_warnings, resolve_effect};

fn parse_project(effects: &str) -> Project {
    let source = format!(
        r#"@ocp "0.2.0"

@project "legacy"
  tempo: 120
  time_signature: 4/4

@track t | instrument: piano/acoustic_grand
  effects:
{effects}
"#
    );
    oc_dsl::parse(&source).expect("legacy project should parse")
}

fn resolve(effect: &str) -> ResolvedEffect {
    let project = parse_project(&format!("    - {effect}\n"));
    let effect = &project.tracks[0].effects[0];
    resolve_effect(&effect.effect_type, &effect.params)
}

fn param<'a>(resolved: &'a ResolvedEffect, name: &str) -> &'a ParamValue {
    resolved
        .params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing resolved parameter {name}"))
}

fn number(resolved: &ResolvedEffect, name: &str) -> f64 {
    param(resolved, name)
        .as_f64()
        .unwrap_or_else(|| panic!("resolved parameter {name} is not numeric"))
}

fn label<'a>(resolved: &'a ResolvedEffect, name: &str) -> &'a str {
    match param(resolved, name) {
        ParamValue::String(text) => text,
        other => panic!("resolved parameter {name} is not a label: {other:?}"),
    }
}

fn enabled(resolved: &ResolvedEffect, name: &str) -> bool {
    matches!(param(resolved, name), ParamValue::Bool(true))
}

#[test]
fn high_shelf_becomes_a_high_shelf_band() {
    let resolved = resolve("eq: high_shelf=-2dB@10kHz");
    assert_eq!(resolved.effect_type, EffectType::DynamicEq);
    assert_eq!(number(&resolved, "band4_freq"), 10_000.0);
    assert_eq!(number(&resolved, "band4_gain"), -2.0);
    assert_eq!(number(&resolved, "band4_q"), 0.707);
    assert_eq!(label(&resolved, "band4_type"), "high_shelf");
    assert!(enabled(&resolved, "band4_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn low_shelf_becomes_a_low_shelf_band() {
    let resolved = resolve("eq: low_shelf=+1dB@60Hz");
    assert_eq!(number(&resolved, "band1_freq"), 60.0);
    assert_eq!(number(&resolved, "band1_gain"), 1.0);
    assert_eq!(label(&resolved, "band1_type"), "low_shelf");
    assert!(enabled(&resolved, "band1_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn presence_becomes_a_bell_band() {
    let resolved = resolve("eq: presence=+1dB@3kHz");
    assert_eq!(number(&resolved, "band2_freq"), 3_000.0);
    assert_eq!(number(&resolved, "band2_gain"), 1.0);
    assert_eq!(label(&resolved, "band2_type"), "bell");
    assert!(enabled(&resolved, "band2_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn notch_becomes_a_notch_band() {
    let resolved = resolve("eq: notch=-2dB@400Hz");
    assert_eq!(number(&resolved, "band3_freq"), 400.0);
    assert_eq!(number(&resolved, "band3_gain"), -2.0);
    assert_eq!(number(&resolved, "band3_q"), 4.0);
    assert_eq!(label(&resolved, "band3_type"), "notch");
    assert!(enabled(&resolved, "band3_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn low_cut_becomes_the_dedicated_low_cut() {
    let resolved = resolve("eq: low_cut=30Hz");
    assert_eq!(number(&resolved, "low_cut_freq"), 30.0);
    assert!(enabled(&resolved, "low_cut_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn high_pass_is_the_same_low_cut() {
    let resolved = resolve("eq: high_pass=80Hz");
    assert_eq!(number(&resolved, "low_cut_freq"), 80.0);
    assert!(enabled(&resolved, "low_cut_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn high_cut_becomes_the_dedicated_high_cut() {
    let resolved = resolve("eq: high_cut=8kHz");
    assert_eq!(number(&resolved, "high_cut_freq"), 8_000.0);
    assert!(enabled(&resolved, "high_cut_enabled"));
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_hall_sets_the_character_controls() {
    let resolved = resolve("reverb: type=hall, decay=3.2s, mix=100%");
    assert_eq!(resolved.effect_type, EffectType::Reverb);
    assert!((number(&resolved, "damping") - 25.0).abs() < 1e-9);
    assert!((number(&resolved, "modulation") - 20.0).abs() < 1e-9);
    assert!((number(&resolved, "early_late") - 20.0).abs() < 1e-9);
    assert!((number(&resolved, "decay") - 3.2).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_plate_sets_the_character_controls() {
    let resolved = resolve("reverb: type=plate");
    assert!((number(&resolved, "damping") - 10.0).abs() < 1e-9);
    assert!((number(&resolved, "modulation") - 35.0).abs() < 1e-9);
    assert!((number(&resolved, "early_late") - 5.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_room_sets_the_character_controls() {
    let resolved = resolve("reverb: type=room");
    assert!((number(&resolved, "damping") - 45.0).abs() < 1e-9);
    assert!((number(&resolved, "modulation") - 10.0).abs() < 1e-9);
    assert!((number(&resolved, "early_late") - 60.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_size_small_sets_the_scale_controls() {
    let resolved = resolve("reverb: size=small");
    assert!((number(&resolved, "decay") - 0.7).abs() < 1e-9);
    assert!((number(&resolved, "predelay") - 0.008).abs() < 1e-9);
    assert!((number(&resolved, "stereo_width") - 90.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_size_medium_sets_the_scale_controls() {
    let resolved = resolve("reverb: size=medium");
    assert!((number(&resolved, "decay") - 1.8).abs() < 1e-9);
    assert!((number(&resolved, "predelay") - 0.020).abs() < 1e-9);
    assert!((number(&resolved, "stereo_width") - 100.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn reverb_size_large_sets_the_scale_controls() {
    let resolved = resolve("reverb: size=large");
    assert!((number(&resolved, "decay") - 3.0).abs() < 1e-9);
    assert!((number(&resolved, "predelay") - 0.030).abs() < 1e-9);
    assert!((number(&resolved, "stereo_width") - 115.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn explicit_eq_controls_win_over_the_shorthand() {
    let resolved = resolve("eq: high_shelf=-2dB@10kHz, high_freq=12kHz, high_gain=-5dB");
    assert_eq!(number(&resolved, "band4_freq"), 12_000.0);
    assert_eq!(number(&resolved, "band4_gain"), -5.0);
    assert_eq!(label(&resolved, "band4_type"), "high_shelf");
    assert_eq!(resolved.warnings.len(), 1);
}

#[test]
fn explicit_reverb_controls_win_over_the_shorthand() {
    let resolved = resolve("reverb: type=hall, size=large, damping=60%, decay=1.2s, predelay=8ms");
    assert!((number(&resolved, "damping") - 60.0).abs() < 1e-9);
    assert!((number(&resolved, "decay") - 1.2).abs() < 1e-9);
    assert!((number(&resolved, "predelay") - 0.008).abs() < 1e-9);
    assert!((number(&resolved, "modulation") - 20.0).abs() < 1e-9);
    assert!((number(&resolved, "stereo_width") - 115.0).abs() < 1e-9);
    assert_eq!(resolved.warnings.len(), 2);
}

#[test]
fn warnings_are_deduplicated_per_shorthand() {
    let project = parse_project(
        "    - eq: high_shelf=-2dB@10kHz\n    - eq: high_shelf=-1dB@12kHz\n    - eq: low_cut=30Hz\n    - reverb: type=cathedral\n    - reverb: size=huge\n",
    );
    let warnings = legacy_warnings(&project);
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("high_shelf"))
    );
    assert!(warnings.iter().any(|warning| warning.contains("low_cut")));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("cathedral") && warning.contains("ignored"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("huge") && warning.contains("ignored"))
    );
}

#[test]
fn unknown_words_are_reported_and_dropped() {
    let resolved = resolve("reverb: type=cathedral, decay=2s");
    assert_eq!(number(&resolved, "decay"), 2.0);
    assert_eq!(resolved.params.len(), 1);
    assert_eq!(resolved.warnings.len(), 1);
    assert!(resolved.warnings[0].contains("ignored"));
}

#[test]
fn the_legacy_syntax_round_trips_through_serialization() {
    let project = parse_project(
        "    - reverb: type=hall, size=large, decay=3.2s, mix=100%\n    - eq: high_shelf=-2dB@10kHz\n    - eq: low_cut=30Hz\n",
    );
    let text = oc_dsl::serialize(&project);
    assert!(text.contains("high_shelf=-2dB@10kHz"), "{text}");
    assert!(text.contains("type=hall"), "{text}");
    assert!(text.contains("size=large"), "{text}");
    assert!(text.contains("low_cut=30Hz"), "{text}");
    let reparsed = oc_dsl::parse(&text).expect("formatted legacy syntax must parse");
    assert_eq!(project.tracks[0].effects, reparsed.tracks[0].effects);
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
}

fn builtin_project(source: &str) -> Project {
    let project = oc_dsl::parse(source).expect("example should parse");
    oc_dsl::validate(&project).expect("example should validate");
    let mut project = oc_dsl::preset::resolve(&project).expect("presets should resolve");
    fn keep_builtin(effects: &mut Vec<oc_dsl::ast::Effect>) {
        effects.retain(|effect| !matches!(effect.effect_type, EffectType::Plugin(_)));
    }
    for track in &mut project.tracks {
        keep_builtin(&mut track.effects);
    }
    for preset in &mut project.presets {
        keep_builtin(&mut preset.effects);
    }
    for bus in &mut project.buses {
        keep_builtin(&mut bus.effects);
    }
    if let Some(master) = &mut project.master {
        keep_builtin(&mut master.effects);
    }
    project
}

const DEMOS: [&str; 5] = [
    "demo_ballad.ocp",
    "demo_orchestral.ocp",
    "demo_presets.ocp",
    "demo_electronic.ocp",
    "demo_full_featured.ocp",
];

#[test]
fn every_shipped_demo_passes_the_builtin_graph_preflight() {
    for demo in DEMOS {
        let path = examples_dir().join(demo);
        let source = std::fs::read_to_string(&path).expect("example should be readable");
        let project = builtin_project(&source);
        oc_engine::graph::AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
            .unwrap_or_else(|error| panic!("{demo} failed the built-in preflight: {error}"));
    }
}

#[test]
#[cfg(feature = "plugin-host")]
fn every_shipped_demo_renders_through_the_builtin_graph() {
    let soundfont = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2");
    for demo in DEMOS {
        let path = examples_dir().join(demo);
        let source = std::fs::read_to_string(&path).expect("example should be readable");
        let project = builtin_project(&source);
        let audio = oc_engine::render::render_to_buffer(&project, &soundfont, 22_050)
            .unwrap_or_else(|error| panic!("{demo} failed to render: {error}"));
        assert!(!audio.is_empty(), "{demo} rendered no samples");
        assert!(
            audio.iter().all(|sample| sample.is_finite()),
            "{demo} rendered non-finite samples"
        );
        assert!(
            audio.iter().any(|sample| sample.abs() > 1e-6),
            "{demo} rendered silence"
        );
    }
}
