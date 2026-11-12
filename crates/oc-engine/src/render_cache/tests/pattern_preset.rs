//! A pattern's preset overrides its hosting track's preset, for its own clips.
//!
//! The renderer splits a track into one source lane per effective preset and
//! sums the lanes before the track's effects. These tests pin the audible
//! consequences: each clip keeps its own instrument, a pattern without a
//! preset follows the track, a missing preset is named, and the cache key
//! carries the pattern's preset.

use super::super::*;
use std::collections::HashMap;
use std::path::Path;

const RATE: u32 = 48_000;

fn preset(name: &str, frequency: u32) -> String {
    format!(
        "@preset {name}\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: {frequency}, amplitude: 0.2, waveform: 0}}\n"
    )
}

/// Two tracks: `shared` hosts a clip whose pattern preset is the variable, and
/// `bed` hosts a pattern without a preset, so it must follow its track.
fn project(second_pattern_preset: Option<&str>) -> Project {
    let lo = match second_pattern_preset {
        Some(preset) => format!("@pattern lo | preset: {preset}, instrument: synth/lead_square\n"),
        None => "@pattern lo | instrument: synth/lead_square\n".to_string(),
    };
    let source = format!(
        "@ocp \"0.2.0\"\n@project \"pattern preset\"\n  tempo: 120\n  time_signature: 4/4\n{}{}{}{}@pattern hi | preset: bright, instrument: synth/lead_square\n  events:\n    @0 C4 w mf\n{lo}  events:\n    @0 C4 w mf\n@pattern bed | instrument: synth/pad\n  events:\n    @0 E3 w mp\n@track shared | preset: bright, instrument: synth/lead_square\n  clip: hi [1:1 - 3:1]\n  clip: lo [3:1 - 5:1]\n@track bed | preset: keys, instrument: synth/pad\n  clip: bed [5:1 - 7:1]\n",
        preset("bright", 880),
        preset("dark", 220),
        preset("mid", 440),
        preset("keys", 660),
    );
    oc_dsl::parse(&source).expect("the pattern preset fixture parses")
}

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(13.0).unwrap();
    cache
        .render_full(project, Path::new("unused.sf2"), RATE, &mut HashMap::new())
        .expect("the project renders")
}

/// Samples of one stereo window. Bars 1-2 hold the first clip, bars 3-4 the
/// second, and bars 5-7 the untouched track; 120 BPM puts a bar at 2 s.
fn window(audio: &[f32], from_seconds: f64, to_seconds: f64) -> &[f32] {
    let from = ((from_seconds * f64::from(RATE)) as usize * 2).min(audio.len());
    let to = ((to_seconds * f64::from(RATE)) as usize * 2).min(audio.len());
    &audio[from..to]
}

const FIRST: (f64, f64) = (0.1, 1.9);
const SECOND: (f64, f64) = (4.1, 5.9);
/// Only the `bed` track plays here, so this is the untouched-track window.
const BED: (f64, f64) = (8.2, 11.8);

#[test]
fn each_clip_renders_with_its_patterns_preset() {
    let overridden = render(&project(Some("dark")));
    // Same two bars of both clips, but the second pattern plays the track's
    // preset instead of its own.
    let same = render(&project(Some("bright")));
    let third = render(&project(Some("mid")));

    assert_eq!(
        window(&overridden, FIRST.0, FIRST.1),
        window(&same, FIRST.0, FIRST.1),
        "the first clip must not move when the second pattern's preset changes"
    );
    assert_eq!(
        window(&overridden, FIRST.0, FIRST.1),
        window(&third, FIRST.0, FIRST.1),
        "the first clip keeps its own preset"
    );
    assert_ne!(
        window(&overridden, SECOND.0, SECOND.1),
        window(&same, SECOND.0, SECOND.1),
        "the second clip must play its pattern's preset, not the track's"
    );
    assert_ne!(
        window(&overridden, SECOND.0, SECOND.1),
        window(&third, SECOND.0, SECOND.1),
        "the second clip follows the pattern, not the previous pattern's preset"
    );
    assert!(
        window(&overridden, SECOND.0, SECOND.1)
            .iter()
            .any(|sample| *sample != 0.0),
        "the overridden clip is audible"
    );
}

#[test]
fn a_pattern_without_a_preset_follows_the_track() {
    let follows = render(&project(None));
    let same = render(&project(Some("bright")));
    assert_eq!(
        window(&follows, SECOND.0, SECOND.1),
        window(&same, SECOND.0, SECOND.1),
        "a pattern without a preset must use the hosting track's preset"
    );
}

#[test]
fn a_track_without_an_override_is_untouched() {
    // `bed`'s pattern carries no preset; moving the other track's pattern
    // preset must not change its audio.
    let overridden = render(&project(Some("dark")));
    let same = render(&project(Some("bright")));
    assert!(
        window(&overridden, BED.0, BED.1)
            .iter()
            .any(|sample| *sample != 0.0),
        "the untouched track is audible in its window"
    );
    assert_eq!(
        window(&overridden, BED.0, BED.1),
        window(&same, BED.0, BED.1),
        "a track whose clips carry no pattern preset must not move"
    );
}

#[test]
fn an_unknown_pattern_preset_names_the_pattern_and_the_preset() {
    let mut project = project(Some("dark"));
    project.patterns[1].preset = Some("missing_patch".into());
    let error = RenderCache::new()
        .render_full(&project, Path::new("unused.sf2"), RATE, &mut HashMap::new())
        .expect_err("an unknown pattern preset must fail the render");
    assert!(error.contains("lo"), "{error}");
    assert!(error.contains("missing_patch"), "{error}");
}

#[test]
fn a_changed_pattern_preset_does_not_reuse_the_cached_track() {
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(13.0).unwrap();
    let mut plugins = HashMap::new();
    let first = project(Some("dark"));
    let rendered = cache
        .render_full(&first, Path::new("unused.sf2"), RATE, &mut plugins)
        .expect("the first render succeeds");
    let second_clip = window(&rendered, SECOND.0, SECOND.1).to_vec();

    // Same lane layout, a different preset on the same lane: only the cache key
    // can notice, because nothing marks a track dirty here.
    let second = project(Some("mid"));
    let before = cache.render_counters();
    let audio = cache
        .render_incremental(&second, Path::new("unused.sf2"), RATE, &mut plugins)
        .expect("the second render succeeds");
    let after = cache.render_counters();

    assert_ne!(
        window(&audio, SECOND.0, SECOND.1),
        second_clip.as_slice(),
        "the cached track must not serve the preset it was rendered with"
    );
    assert_eq!(
        window(&audio, FIRST.0, FIRST.1),
        window(&rendered, FIRST.0, FIRST.1),
        "the other clip keeps its own preset"
    );
    assert_eq!(
        after.source_renders - before.source_renders,
        2,
        "both of the shared track's lanes re-render and nothing else does"
    );
}
