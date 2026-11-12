//! A long streaming clip must not allocate while it is being mixed.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_test_support::operations;
use std::path::PathBuf;

use oc_engine::clip_lane::ClipLanes;
use oc_engine::derived_cache::DerivedCache;
use oc_engine::media_cache::MediaCache;

const RATE: u32 = 8_000;
const CHUNK: usize = 4096;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-clip-alloc", name)
}

/// Two minutes of 8 kHz mono, so the clip cannot be mistaken for a short one.
fn write_long_wav(path: &std::path::Path) {
    let frames = RATE as usize * 120;
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..frames {
        writer
            .write_sample((frame as f32 * 0.001).sin() * 0.25)
            .unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
fn a_long_clip_mixes_without_allocating() {
    let root = temp_dir("long");
    let media = root.join("long.wav");
    write_long_wav(&media);
    let project = oc_dsl::parse(
        r#"@project "Long clip"
  tempo: 60
@audio_clip long [1:1 - 31:1] | track: tape
  file: "long.wav"
  gain: -3dB
@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();

    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = ClipLanes::load_with_caches(&cache, &derived, &project, RATE, Some(&root)).unwrap();
    let lane = lanes.get(0).unwrap();
    assert_eq!(lane.length_frames(), RATE as usize * 120);

    let mut output = vec![0.0_f32; CHUNK * 2];
    let mut tracks = vec![vec![0.0_f32; CHUNK * 2]];
    let allocations = operations(|| {
        let chunks = lane.length_frames() / CHUNK;
        for chunk in 0..chunks {
            output.fill(0.0);
            lane.mix_into((chunk * CHUNK) as u64, &mut output).unwrap();
            tracks[0].fill(0.0);
            lanes.mix_into((chunk * CHUNK) as u64, &mut tracks).unwrap();
        }
    });

    assert_eq!(allocations, 0, "clip mixing must not allocate");
    assert!(output.iter().any(|sample| sample.abs() > 0.001));
    std::fs::remove_dir_all(root).unwrap();
}
