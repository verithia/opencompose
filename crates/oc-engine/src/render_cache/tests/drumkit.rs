//! Drum-kit offline render tests.

use super::super::*;
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "opencompose-engine-drumkit-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_dc(path: &Path, frames: usize, amplitude: f32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    let value = (f64::from(amplitude) * f64::from(i16::MAX)) as i16;
    for _ in 0..frames {
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn first_onset_after(samples: &[f32], start: usize) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, value)| value.abs() > 0.005)
        .map(|(index, _)| index)
}

#[test]
fn kit_pattern_renders_hits_at_the_expected_frames() {
    let dir = TempDir::new("hits");
    let kick = dir.join("kick.wav");
    let snare = dir.join("snare.wav");
    write_dc(&kick, 12_000, 0.5);
    write_dc(&snare, 12_000, 0.25);
    let source = format!(
        "@project \"Kit hits\"\n  tempo: 120\n  time_signature: 4/4\n\
@kit basic\n\
  pad kick | note: C2, one_shot: true\n\
  pad snare | note: D2, one_shot: true\n\
  layer kick | low: 1, high: 127, sample: \"{}\"\n\
  layer snare | low: 1, high: 127, sample: \"{}\"\n\
@track drums | instrument: drumkit, preset: basic\n\
  @pattern beat [1:1 - 2:1]\n\
    kick 1:0 ff\n\
    snare 2:0 ff\n",
        kick.display(),
        snare.display()
    );
    let project = oc_dsl::parse(&source).expect("kit project parses");
    let mut cache = RenderCache::new();
    let mut plugins = std::collections::HashMap::new();
    let mix = cache
        .render_full(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .expect("kit renders");
    let mono: Vec<f32> = mix.chunks_exact(2).map(|frame| frame[0]).collect();
    assert_eq!(
        first_onset_after(&mono, 0),
        Some(0),
        "kick starts at frame 0"
    );
    // 120 BPM: the second beat is 0.5 s = 24 000 frames.
    assert_eq!(
        first_onset_after(&mono, 12_000),
        Some(24_000),
        "snare starts at beat 2"
    );
    // The mix applies the graph's centre-pan law, so only the ratio is fixed.
    let kick_level = mono[2_000];
    let snare_level = mono[25_000];
    assert!(kick_level > 0.2, "kick level {kick_level}");
    assert!(
        (kick_level / snare_level - 2.0).abs() < 0.1,
        "kick {kick_level} snare {snare_level}"
    );
}

#[test]
fn replacing_a_pad_sample_file_invalidates_the_render() {
    let dir = TempDir::new("reload");
    let sample = dir.join("kick.wav");
    write_dc(&sample, 4_800, 0.5);
    let source = format!(
        "@project \"Kit reload\"\n  tempo: 120\n\
@kit basic\n\
  pad kick | note: C2\n\
  layer kick | low: 1, high: 127, sample: \"{}\"\n\
@track drums | instrument: drumkit, preset: basic\n\
  @pattern beat [1:1 - 2:1]\n\
    kick 1:0 ff\n",
        sample.display()
    );
    let project = oc_dsl::parse(&source).unwrap();
    let mut cache = RenderCache::new();
    let mut plugins = std::collections::HashMap::new();
    let first = cache
        .render_full(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    let first_peak = first
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(first_peak > 0.2, "first peak {first_peak}");
    write_dc(&sample, 4_800, 0.1);
    let second = cache
        .render_incremental(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
        .unwrap();
    let second_peak = second
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(
        second_peak < first_peak * 0.5,
        "changed sample should be re-read: {first_peak} then {second_peak}"
    );
}

fn fnv1a(samples: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for sample in samples {
        for byte in sample.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[test]
fn sample_less_kit_renders_the_same_bytes_twice() {
    let source = "@project \"Synth kit\"\n  tempo: 120\n  time_signature: 4/4\n\
@kit basic\n\
  pad kick | note: C2\n\
  pad snare | note: D2\n\
  pad hat_closed | note: F#2\n\
  pad hat_open | note: Bb2\n\
@track drums | instrument: drumkit, preset: basic\n\
  @pattern beat [1:1 - 2:1]\n\
    kick 1:0 ff\n\
    hat_closed 1:1 mf\n\
    snare 2:0 ff\n\
    hat_open 2:1 mf\n";
    let project = oc_dsl::parse(source).expect("sample-less kit project parses");

    let render = || {
        let mut cache = RenderCache::new();
        let mut plugins = std::collections::HashMap::new();
        cache
            .render_full(&project, Path::new("unused.sf2"), 48_000, &mut plugins)
            .expect("sample-less kit renders")
    };
    let first = render();
    let second = render();
    let peak = first
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(peak > 0.05, "sample-less kit was silent (peak {peak})");
    assert_eq!(
        fnv1a(&first),
        fnv1a(&second),
        "two renders must be byte-identical"
    );
}
