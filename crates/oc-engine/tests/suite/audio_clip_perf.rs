//! Temporary timing harness for the 5-minute audio clip media path.
//!
//! Part of the `perf-tests` set, so a plain `cargo test` neither compiles nor
//! runs it. Run with `cargo test -p oc-engine --features perf-tests --profile perf`.
#![cfg(feature = "perf-tests")]

use std::path::PathBuf;
use std::time::Instant;

use oc_engine::audio_peaks::PeakPyramid;
use oc_engine::media_cache::MediaCache;
use oc_engine::resample::ResampleQuality;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("oc-clip-perf", name)
}

fn write_five_minute_wav(path: &PathBuf) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    let frames = 48_000 * 300;
    for frame in 0..frames {
        let t = frame as f32 / 48_000.0;
        let left = (t * 2.0 * std::f32::consts::PI * 220.0).sin() * 0.4
            + (t * 2.0 * std::f32::consts::PI * 331.0).sin() * 0.2;
        let right = (t * 2.0 * std::f32::consts::PI * 221.0).sin() * 0.35;
        writer.write_sample(left).unwrap();
        writer.write_sample(right).unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
fn measure_five_minute_file() {
    let dir = temp_dir("five");
    let media = dir.join("five.wav");
    let started = Instant::now();
    write_five_minute_wav(&media);
    println!("write wav: {:?}", started.elapsed());

    let cache = MediaCache::new(None, 2 * 1024 * 1024 * 1024, 0);
    let started = Instant::now();
    let entry = cache.decoded(&media).unwrap();
    println!("cold decode (incl peaks): {:?}", started.elapsed());
    let started = Instant::now();
    let payload = entry.pyramid().to_bytes();
    println!(
        "peak encode {} bytes: {:?}",
        payload.len(),
        started.elapsed()
    );
    let started = Instant::now();
    let _ = cache.resampled(&media, 48_000, ResampleQuality::DEFAULT_RENDER);
    println!("resample to 48k: {:?}", started.elapsed());
    let started = Instant::now();
    for _ in 0..10 {
        let _ = cache.decoded(&media).unwrap();
    }
    println!("memory hit (avg of 10): {:?}", started.elapsed() / 10);

    let disk_dir = dir.join("cache");
    let started = Instant::now();
    let disk = MediaCache::new(
        Some(disk_dir.clone()),
        2 * 1024 * 1024 * 1024,
        8 * 1024 * 1024 * 1024,
    );
    let _ = disk.decoded(&media).unwrap();
    println!("cold decode + disk write: {:?}", started.elapsed());
    let size: u64 = std::fs::read_dir(&disk_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    println!(
        "disk entry size: {:.1} MiB",
        size as f64 / (1024.0 * 1024.0)
    );

    let started = Instant::now();
    let reopened = MediaCache::new(
        Some(disk_dir),
        2 * 1024 * 1024 * 1024,
        8 * 1024 * 1024 * 1024,
    );
    let entry = reopened.decoded(&media).unwrap();
    println!("disk hit: {:?}", started.elapsed());
    let started = Instant::now();
    let bytes = entry.pyramid().to_bytes();
    println!(
        "cached peak encode {} bytes: {:?}",
        bytes.len(),
        started.elapsed()
    );

    let samples = entry.read_window(0..entry.frames()).unwrap();
    let started = Instant::now();
    let pyramid = PeakPyramid::build(&samples, 48_000).unwrap();
    println!(
        "peak build from cached samples ({} levels): {:?}",
        pyramid.levels.len(),
        started.elapsed()
    );
    println!("paths: {}", media.display());
    let _ = std::fs::remove_dir_all(dir);
}
