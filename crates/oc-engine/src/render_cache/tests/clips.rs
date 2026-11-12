//! Audio-clip invalidation through the per-track clip fingerprint.

use super::*;
use oc_dsl::ast::Project;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("oc-clip-fingerprint", name)
}

fn write_constant_wav(path: &std::path::Path, frames: usize, value: f32) {
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
    for _ in 0..frames {
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn clip_project() -> Project {
    let text = r#"@project "Clip fingerprint"
  tempo: 120
@audio_clip hit [1:1 - 2:1] | track: tape
  file: "hit.wav"
@track tape | instrument: audio/clip
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

#[test]
fn changed_clip_re_renders_without_a_dirty_flag() {
    let root = temp_dir("rerender");
    write_constant_wav(&root.join("hit.wav"), 48_000, 0.5);
    let mut project = clip_project();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let mut plugins = HashMap::new();
    let first = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    let level = first[0].abs();
    assert!(level > 0.1, "the clip must be audible");

    // Simulate an edit path that forgot to set the dirty flag.
    project.audio_clip_mut(0).unwrap().gain_db = -12.0;
    let second = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    assert_ne!(
        first, second,
        "a changed clip must not serve the cached mix"
    );
    let expected = level * 10.0_f32.powf(-12.0 / 20.0);
    assert!(
        (second[0].abs() - expected).abs() < 1e-4,
        "{} vs {expected}",
        second[0]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn fingerprint_covers_every_stretch_field() {
    let project = clip_project();
    let track = &project.tracks[0];
    let base = track_clip_fingerprint(&project, track, 48_000, None);
    let mut edited = project.clone();
    edited.audio_clip_mut(0).unwrap().stretch = 1.25;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );

    let mut edited = project.clone();
    edited.audio_clip_mut(0).unwrap().stretch_mode = oc_dsl::ast::StretchMode::Percussive;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );

    let mut edited = project.clone();
    edited.audio_clip_mut(0).unwrap().tempo_follow = true;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );

    let mut edited = project.clone();
    edited.audio_clip_mut(0).unwrap().pitch = 12.0;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );

    let mut edited = project.clone();
    edited.audio_clip_mut(0).unwrap().offset_seconds = 0.25;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );

    // A tempo change moves the clip's frame placement.
    let mut edited = project.clone();
    edited.properties.tempo = 60.0;
    assert_ne!(
        base,
        track_clip_fingerprint(&edited, &edited.tracks[0], 48_000, None)
    );
}

#[test]
fn fingerprint_covers_the_harmony_voice() {
    let mut project = clip_project();
    let base = track_clip_fingerprint(&project, &project.tracks[0], 48_000, None);

    project
        .properties
        .vocal_harmonies
        .push(oc_dsl::ast::VocalHarmony {
            name: "third".into(),
            clip: Some("hit".into()),
            track: None,
            interval: 2,
            level_db: -6.0,
            pan: 0.0,
        });
    let with_voice = track_clip_fingerprint(&project, &project.tracks[0], 48_000, None);
    assert_ne!(base, with_voice, "a new voice must re-render the track");

    project.properties.vocal_harmonies[0].interval = 4;
    assert_ne!(
        with_voice,
        track_clip_fingerprint(&project, &project.tracks[0], 48_000, None),
        "a wider interval must re-render the track"
    );

    // The pan is a mix-time balance, so it does not change the bake identity.
    project.properties.vocal_harmonies[0].pan = -0.5;
    assert_eq!(
        track_clip_fingerprint(&project, &project.tracks[0], 48_000, None),
        {
            project.properties.vocal_harmonies[0].pan = 0.0;
            track_clip_fingerprint(&project, &project.tracks[0], 48_000, None)
        }
    );
}

#[test]
fn fingerprint_is_stable_for_an_unchanged_project() {
    let project = clip_project();
    let first = track_clip_fingerprint(&project, &project.tracks[0], 48_000, None);
    let second = track_clip_fingerprint(&project.clone(), &project.tracks[0], 48_000, None);
    assert_eq!(first, second);
}

#[test]
fn audio_clip_placement_invalidates_cache_and_extends_render() {
    let root = temp_dir("placement");
    write_constant_wav(&root.join("hit.wav"), 144_000, 0.5);
    let mut project = clip_project();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let mut plugins = HashMap::new();
    let first = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    project.audio_clip_mut(0).unwrap().placement = oc_dsl::ast::AudioClipPlacement {
        start_offset_seconds: 1.0 / 48_000.0,
        end_offset_seconds: 1.0 / 48_000.0,
    };
    assert_eq!(
        crate::audio_clip::project_end_frame(&project, 48_000).unwrap(),
        96_001
    );
    let second = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(&second[..2], &[0.0, 0.0]);
    assert!(second[2].abs() > 0.1);
    assert_eq!(second.len(), first.len() + 2);
    std::fs::remove_dir_all(root).unwrap();
}
