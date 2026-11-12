#![cfg(feature = "plugin-host")]
//! Audio-clip decoding, resampling, placement and error reporting.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::audio_clip;
use oc_engine::resample::ResampleQuality;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-audio-clip", name)
}

fn write_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[f32]) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

fn sine(path: &Path, sample_rate: u32, seconds: f64, frequency: f64, amplitude: f32) {
    let frames = (seconds * f64::from(sample_rate)).round() as usize;
    let samples = (0..frames)
        .map(|frame| {
            (2.0 * PI * frequency * frame as f64 / f64::from(sample_rate)).sin() as f32 * amplitude
        })
        .collect::<Vec<_>>();
    write_wav(path, sample_rate, 1, &samples);
}

fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum();
    (sum / samples.len() as f64).sqrt()
}

fn clip_project(clip: &str) -> Project {
    let text = format!(
        r#"@project "Clip render"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
{clip}
@track tape | instrument: audio/clip
"#
    );
    let project = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn render(root: &Path, project: &Project) -> Result<Vec<f32>, String> {
    oc_engine::render::render_to_buffer_with_plugins_at(
        project,
        Path::new(""),
        48_000,
        &mut HashMap::new(),
        Some(root),
    )
}

#[test]
fn decode_mono_wav_duplicates_to_stereo() {
    let root = temp_dir("mono");
    let path = root.join("mono.wav");
    let samples = (0..100)
        .map(|frame| frame as f32 / 200.0)
        .collect::<Vec<_>>();
    write_wav(&path, 48_000, 1, &samples);

    let decoded = audio_clip::decode(&path).unwrap();
    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.frames(), 100);
    for (frame, &sample) in samples.iter().enumerate().take(100) {
        assert_eq!(decoded.samples[frame * 2], sample);
        assert_eq!(decoded.samples[frame * 2 + 1], sample);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resampling_44k1_to_48k_preserves_length_and_rms() {
    let root = temp_dir("resample-sine");
    let path = root.join("sine.wav");
    sine(&path, 44_100, 1.0, 440.0, 0.5);
    let decoded = audio_clip::decode(&path).unwrap();
    let resampled = audio_clip::load(&path, 48_000, ResampleQuality::DEFAULT_RENDER).unwrap();
    assert_eq!(resampled.sample_rate, 48_000);
    let expected = (decoded.frames() as f64 * 48_000.0 / 44_100.0).round() as usize;
    assert!((resampled.frames() as i64 - expected as i64).abs() <= 1);
    let before = rms(&decoded.samples);
    let after = rms(&resampled.samples);
    assert!((before - after).abs() < 0.01, "{before} vs {after}");
    for frame in 0..resampled.frames().min(1000) {
        assert!((resampled.samples[frame * 2] - resampled.samples[frame * 2 + 1]).abs() < 1e-6);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resampling_noise_preserves_energy() {
    let root = temp_dir("resample-noise");
    let path = root.join("noise.wav");
    let mut state = 0x1234_5678_u32;
    let samples = (0..44_100)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state as f32 / u32::MAX as f32) * 0.5 - 0.25
        })
        .collect::<Vec<_>>();
    write_wav(&path, 44_100, 1, &samples);
    let decoded = audio_clip::decode(&path).unwrap();
    let resampled = audio_clip::load(&path, 48_000, ResampleQuality::DEFAULT_RENDER).unwrap();
    let before = rms(&decoded.samples);
    let after = rms(&resampled.samples);
    assert!(
        ((before - after) / before).abs() < 0.02,
        "{before} vs {after}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clip_lands_at_the_musical_offset_with_gain() {
    let root = temp_dir("placement");
    let path = root.join("hit.wav");
    write_wav(&path, 48_000, 1, &vec![0.5; 48_000]);
    let project = clip_project("  file: \"hit.wav\"\n  gain: -6dB");
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    let end = start + 48_000 * 2;
    assert!(rendered.len() >= end);
    assert!(rendered[..start].iter().all(|sample| *sample == 0.0));
    // Constant-power center pan applies 1/sqrt(2) per channel.
    let expected = 0.5 * 10.0_f32.powf(-6.0 / 20.0) * std::f32::consts::FRAC_1_SQRT_2;
    for frame in 0..1000 {
        let index = start + frame * 2;
        assert!((rendered[index] - expected).abs() < 1e-5);
        assert!((rendered[index + 1] - expected).abs() < 1e-5);
    }
    assert!(rendered[end..].iter().all(|sample| *sample == 0.0));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clip_respects_track_gain_and_bus_effects() {
    let root = temp_dir("routing");
    let path = root.join("hit.wav");
    write_wav(&path, 48_000, 1, &vec![0.5; 48_000]);
    let text = r#"@project "Clip routing"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "hit.wav"
@track tape | instrument: audio/clip
  mix: vol=-6dB, output=group
@bus group
  routes: [master]
  effects:
    - gain: factor=0.5
"#;
    let mut project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    let expected = 0.5 * 10.0_f32.powf(-6.0 / 20.0) * 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    for frame in 0..1000 {
        assert!((rendered[start + frame * 2] - expected).abs() < 1e-5);
    }

    project.tracks[0].mix.mute = true;
    let muted = render(&root, &project).unwrap();
    assert!(muted.iter().all(|sample| sample.abs() < 1e-6));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clip_offset_and_fades_are_sample_accurate() {
    let root = temp_dir("fades");
    let path = root.join("ramp.wav");
    write_wav(&path, 48_000, 1, &vec![1.0; 96_000]);
    let project =
        clip_project("  file: \"ramp.wav\"\n  offset: 0.25s\n  fade_in: 0.5s\n  fade_out: 0.5s");
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    // Offset 0.25s skips the first quarter second of the file.
    assert!(rendered[start].abs() < 1e-6);
    let half_fade = start + 12_000 * 2;
    let center = std::f32::consts::FRAC_1_SQRT_2;
    assert!((rendered[half_fade] - 0.5 * center).abs() < 1e-4);
    let fade_out_start = start + 60_000 * 2;
    assert!((rendered[fade_out_start] - center).abs() < 1e-3);
    let last = start + 84_000 * 2 - 2;
    assert!(rendered[last].abs() < 1e-4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_and_corrupt_media_name_the_clip_and_path() {
    let root = temp_dir("errors");
    let project = clip_project("  file: \"missing.wav\"");
    let error = render(&root, &project).unwrap_err();
    assert!(error.contains("audio clip 'hit'"), "{error}");
    assert!(error.contains("missing.wav"), "{error}");
    assert!(error.contains("file not found"), "{error}");

    let corrupt = root.join("corrupt.wav");
    std::fs::write(&corrupt, b"not a wave file").unwrap();
    let project = clip_project("  file: \"corrupt.wav\"");
    let error = audio_clip::validate(&project, Some(&root)).unwrap_err();
    assert!(error.contains("audio clip 'hit'"), "{error}");
    assert!(error.contains("corrupt.wav"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

/// A project file on disk resolves a clip's `media/...` path against the
/// directory that holds it. An absolute path needs no directory, and a file
/// that is not there is refused with the path the load tried.
#[test]
fn relative_media_paths_resolve_against_the_project_file() {
    let root = temp_dir("project-file");
    std::fs::create_dir_all(root.join("media/samples")).unwrap();
    let media = root.join("media/samples/hit.wav");
    write_wav(&media, 48_000, 1, &vec![0.5; 48_000]);
    let text = r#"@project "Relative media"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "media/samples/hit.wav"
@track tape | instrument: audio/clip
"#;
    let project_file = root.join("song.ocp");
    std::fs::write(&project_file, text).unwrap();
    let source = std::fs::read_to_string(&project_file).unwrap();
    let project = oc_dsl::parse(&source).unwrap();
    oc_dsl::validate(&project).unwrap();
    let directory = project_file.parent().unwrap();

    // The clip starts at 2:1, so the first second of the render is silence.
    let rendered = render(directory, &project).unwrap();
    let start = 2 * 48_000 * 2;
    assert!(rendered[..start].iter().all(|sample| *sample == 0.0));
    assert!(
        rendered[start + 480 * 2].abs() > 0.05,
        "the clip plays once its range starts"
    );

    // An absolute path resolves with no project directory at all.
    let absolute = text.replace("media/samples/hit.wav", &media.display().to_string());
    let project = oc_dsl::parse(&absolute).unwrap();
    let rendered = oc_engine::render::render_to_buffer_with_plugins_at(
        &project,
        Path::new(""),
        48_000,
        &mut HashMap::new(),
        None,
    )
    .unwrap();
    assert!(rendered[start + 480 * 2].abs() > 0.05);

    // A missing file keeps the refusal and names the resolved path.
    let missing = text.replace("media/samples/hit.wav", "media/samples/gone.wav");
    let project = oc_dsl::parse(&missing).unwrap();
    let error = render(directory, &project).unwrap_err();
    let tried = root.join("media/samples/gone.wav");
    assert!(error.contains(&tried.display().to_string()), "{error}");
    assert!(error.contains("file not found"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn validate_decodes_every_referenced_file() {
    let root = temp_dir("validate");
    let path = root.join("hit.wav");
    write_wav(&path, 44_100, 2, &vec![0.1; 882]);
    let project = clip_project("  file: \"hit.wav\"");
    audio_clip::validate(&project, Some(&root)).unwrap();

    let relative = project.clone();
    let error = audio_clip::validate(&relative, None).unwrap_err();
    assert!(error.contains("project directory"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_end_frame_uses_the_project_clock() {
    let root = temp_dir("extent");
    let project = clip_project("  file: \"hit.wav\"");
    // [2:1 - 3:1] at 120 BPM is two seconds starting at two seconds.
    assert_eq!(
        audio_clip::project_end_frame(&project, 48_000).unwrap(),
        4 * 48_000
    );

    let mut mapped = project.clone();
    mapped.properties.tempo = 60.0;
    assert_eq!(
        audio_clip::project_end_frame(&mapped, 48_000).unwrap(),
        8 * 48_000
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn constant_stereo(path: &Path, left: f32, right: f32, frames: usize) {
    let mut samples = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        samples.push(left);
        samples.push(right);
    }
    write_wav(path, 48_000, 2, &samples);
}

#[test]
fn stereo_clip_is_unity_per_channel_at_centre() {
    let root = temp_dir("stereo-balance");
    let path = root.join("stereo.wav");
    constant_stereo(&path, 0.4, 0.25, 48_000);
    let project = clip_project("  file: \"stereo.wav\"");
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    for frame in 0..1000 {
        let index = start + frame * 2;
        assert!((rendered[index] - 0.4).abs() < 1e-5, "frame {frame}");
        assert!((rendered[index + 1] - 0.25).abs() < 1e-5, "frame {frame}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mono_clip_keeps_constant_power_at_centre() {
    let root = temp_dir("mono-balance");
    let path = root.join("mono.wav");
    write_wav(&path, 48_000, 1, &vec![0.4; 48_000]);
    let project = clip_project("  file: \"mono.wav\"");
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    let expected = 0.4 * std::f32::consts::FRAC_1_SQRT_2;
    for frame in 0..1000 {
        let index = start + frame * 2;
        assert!((rendered[index] - expected).abs() < 1e-5, "frame {frame}");
        assert!(
            (rendered[index + 1] - expected).abs() < 1e-5,
            "frame {frame}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stereo_clip_hard_left_keeps_the_left_channel_at_unity() {
    let root = temp_dir("stereo-hard-left");
    let path = root.join("stereo.wav");
    constant_stereo(&path, 0.4, 0.25, 48_000);
    let text = r#"@project "Clip hard left"
  tempo: 120
@audio_clip hit [2:1 - 3:1] | track: tape
  file: "stereo.wav"
@track tape | instrument: audio/clip
  mix: vol=0dB, pan=-1
"#;
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    let rendered = render(&root, &project).unwrap();
    let start = 2 * 48_000 * 2;
    for frame in 0..1000 {
        let index = start + frame * 2;
        assert!((rendered[index] - 0.4).abs() < 1e-5, "frame {frame}");
        assert!(rendered[index + 1].abs() < 1e-5, "frame {frame}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn two_clips_referencing_one_file_share_a_single_decode() {
    let root = temp_dir("shared-decode");
    let path = root.join("hit.wav");
    write_wav(&path, 48_000, 1, &vec![0.5; 48_000]);
    let mut project = clip_project("  file: \"hit.wav\"");
    let mut second = project.audio_clip(0).unwrap().clone();
    second.name = "hit2".into();
    second.time_range.start = oc_dsl::ast::BarBeat {
        bar: 3,
        beat: 1,
        subdivision: 0,
    };
    second.time_range.end = oc_dsl::ast::BarBeat {
        bar: 4,
        beat: 1,
        subdivision: 0,
    };
    project
        .add_audio_clip(second)
        .expect("the second clip lands on its track");

    let cache = oc_engine::media_cache::MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = oc_engine::derived_cache::DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let mut buffer = vec![0.0_f32; 8 * 48_000 * 2];
    for clip in project.audio_clips() {
        audio_clip::mix_into_track_with_caches(
            &cache,
            &derived,
            &mut buffer,
            &project,
            clip,
            48_000,
            Some(&root),
        )
        .unwrap();
    }
    assert_eq!(cache.stats().decodes, 1);
    assert_eq!(cache.decodes_for(&path), 1);
    for frame in [2 * 48_000, 4 * 48_000] {
        assert!(
            buffer[frame * 2].abs() > 0.1,
            "clip at frame {frame} is silent"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
