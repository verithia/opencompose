//! Streaming clip lanes: chunk-boundary placement and seek alignment.

use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::audio_clip;
use oc_engine::clip_lane::ClipLanes;
use oc_engine::derived_cache::DerivedCache;
use oc_engine::media_cache::MediaCache;

const RATE: u32 = 48_000;
const CHUNK: usize = 4096;

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-clip-lane", name)
}

/// Stereo ramp: each frame's value identifies it, so placement is checkable.
fn write_ramp(path: &Path, frames: usize, scale: f32) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..frames {
        writer.write_sample(frame as f32 * scale).unwrap();
        writer.write_sample(frame as f32 * scale * 0.5).unwrap();
    }
    writer.finalize().unwrap();
}

fn clip_project(clip: &str) -> Project {
    let text = format!(
        r#"@project "Clip lane"
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

fn ramp_media(root: &Path) -> PathBuf {
    let path = root.join("ramp.wav");
    write_ramp(&path, RATE as usize * 3, 1e-6);
    path
}

fn lanes(cache: &MediaCache, root: &Path, project: &Project) -> ClipLanes {
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    ClipLanes::load_with_caches(cache, &derived, project, RATE, Some(root)).unwrap()
}

#[test]
fn chunk_boundary_matches_a_single_offline_mix() {
    let root = temp_dir("chunk");
    ramp_media(&root);
    let project = clip_project(
        "  file: \"ramp.wav\"\n  offset: 0.05s\n  gain: -6dB\n  fade_in: 0.01s\n  fade_out: 0.02s\n  invert: true\n  crossfade: 10ms",
    );
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    let lane = lanes.get(0).unwrap();
    let start = lane.start_frame();
    assert_ne!(start % CHUNK as u64, 0, "clip must start inside a chunk");
    let total = start as usize + lane.length_frames() + 8192;

    let mut one_shot = vec![0.0_f32; total * 2];
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut one_shot,
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        Some(&root),
    )
    .unwrap();

    let mut chunked = vec![0.0_f32; total * 2];
    let mut position = 0;
    while position < total {
        let frames = (total - position).min(CHUNK);
        let mut block = vec![0.0_f32; frames * 2];
        lane.mix_into(position as u64, &mut block).unwrap();
        chunked[position * 2..(position + frames) * 2].copy_from_slice(&block);
        position += frames;
    }
    assert_eq!(chunked, one_shot, "chunked clip mix must be sample-exact");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_pitched_and_stretched_lane_matches_the_offline_mix() {
    let root = temp_dir("pitched");
    ramp_media(&root);
    // Pin the live rung to the render rung, so the pitch resample is one bake.
    let text = "@project \"Clip lane\"\n  tempo: 120\n  live_resample: 4\n@audio_clip hit [2:1 - 3:1] | track: tape\n  file: \"ramp.wav\"\n  stretch: 0.5\n  pitch: 5\n  fade_in: 0.01s\n  fade_out: 0.02s\n@track tape | instrument: audio/clip\n";
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    let lane = lanes.get(0).unwrap();
    // Three seconds of media transposed and halved last 1.5 s, inside the range.
    let plan = oc_engine::stretch::resolve(
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        RATE as usize * 3,
    )
    .unwrap();
    let expected = oc_engine::stretch::stretched_frames(
        plan.pitched_frames(RATE as usize * 3, RATE),
        plan.ratio,
    );
    assert_eq!(lane.length_frames(), expected);
    assert!(lane.length_frames() < RATE as usize * 2, "not range-bound");
    let start = lane.start_frame() as usize;
    let total = start + lane.length_frames() + 8192;

    let mut one_shot = vec![0.0_f32; total * 2];
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut one_shot,
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        Some(&root),
    )
    .unwrap();

    let mut chunked = vec![0.0_f32; total * 2];
    let mut position = 0;
    while position < total {
        let frames = (total - position).min(CHUNK);
        let mut block = vec![0.0_f32; frames * 2];
        lane.mix_into(position as u64, &mut block).unwrap();
        chunked[position * 2..(position + frames) * 2].copy_from_slice(&block);
        position += frames;
    }
    assert_eq!(
        chunked, one_shot,
        "the lane must bake the same pitched variant"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn off_grid_start_writes_exactly_at_its_frame() {
    let root = temp_dir("off-grid");
    ramp_media(&root);
    let project = clip_project("  file: \"ramp.wav\"\n  offset: 0.25s\n  gain: -3dB");
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    let lane = lanes.get(0).unwrap();
    let start = lane.start_frame() as usize;
    assert_ne!(start % CHUNK, 0);

    let mut track = vec![0.0_f32; (start + lane.length_frames() + 64) * 2];
    lane.mix_into(0, &mut track).unwrap();

    assert!(track[..start * 2].iter().all(|sample| *sample == 0.0));
    let gain = 10.0_f32.powf(-3.0 / 20.0);
    let source_offset = 12_000;
    for frame in 0..128 {
        let expected = (source_offset + frame) as f32 * 1e-6 * gain;
        assert!(
            (track[(start + frame) * 2] - expected).abs() < 1e-9,
            "frame {frame}"
        );
        assert!(
            (track[(start + frame) * 2 + 1] - expected * 0.5).abs() < 1e-9,
            "frame {frame}"
        );
    }
    let end = start + lane.length_frames();
    assert!(track[end * 2..].iter().all(|sample| *sample == 0.0));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn seek_into_a_clip_starts_at_the_right_source_sample() {
    let root = temp_dir("seek");
    ramp_media(&root);
    let project = clip_project("  file: \"ramp.wav\"\n  offset: 0.1s");
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    let lane = lanes.get(0).unwrap();
    let start = lane.start_frame() as usize;
    let seek_frame = start + 12_345;

    let mut whole = vec![0.0_f32; (start + lane.length_frames()) * 2];
    lane.mix_into(0, &mut whole).unwrap();
    let mut block = vec![0.0_f32; CHUNK * 2];
    lane.mix_into(seek_frame as u64, &mut block).unwrap();
    assert_eq!(
        block,
        whole[seek_frame * 2..(seek_frame + CHUNK) * 2],
        "seek must resume the same source sample"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A looped clip repeats its media region for the clip's whole length, and the
/// chunked stream matches the one-shot offline mix sample for sample.
#[test]
fn a_looped_clip_streams_identically_to_the_offline_mix() {
    let root = temp_dir("loop");
    ramp_media(&root);
    let project = clip_project("  file: \"ramp.wav\"\n  offset: 0.25s\n  loop: 0.5s");
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    let lane = lanes.get(0).unwrap();
    let start = lane.start_frame();
    // The clip range is one bar: two seconds at 120 BPM, longer than the loop.
    assert_eq!(lane.length_frames(), RATE as usize * 2);

    let total = start as usize + lane.length_frames() + 8192;
    let mut one_shot = vec![0.0_f32; total * 2];
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut one_shot,
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        Some(&root),
    )
    .unwrap();

    let mut chunked = vec![0.0_f32; total * 2];
    let mut position = 0;
    while position < total {
        let frames = (total - position).min(CHUNK);
        let mut block = vec![0.0_f32; frames * 2];
        lane.mix_into(position as u64, &mut block).unwrap();
        chunked[position * 2..(position + frames) * 2].copy_from_slice(&block);
        position += frames;
    }
    assert_eq!(chunked, one_shot, "a looped chunk mix must be sample-exact");

    // The media is a ramp whose value names its frame, so the loop rewinds it.
    let loop_frames = (0.5 * f64::from(RATE)) as usize;
    let offset_frames = (0.25 * f64::from(RATE)) as usize;
    let head = (start as usize + 10) * 2;
    let wrapped = (start as usize + loop_frames + 10) * 2;
    let expected = (offset_frames + 10) as f32 * 1e-6;
    assert!(
        (one_shot[head] - expected).abs() < 1e-9,
        "{}",
        one_shot[head]
    );
    assert_eq!(
        one_shot[head], one_shot[wrapped],
        "the loop repeats the head"
    );
    // One frame before the seam is still the far end of the region.
    let before_seam = (start as usize + loop_frames - 1) * 2;
    let region_end = (offset_frames + loop_frames - 1) as f32 * 1e-6;
    assert!(
        (one_shot[before_seam] - region_end).abs() < 1e-9,
        "{}",
        one_shot[before_seam]
    );
    // Past the loop the clip still plays: the tail is not silence.
    let tail = (start as usize + lane.length_frames() - 1) * 2;
    assert!(
        one_shot[tail].abs() > 0.0,
        "the looped clip must fill its range"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A muted clip is skipped exactly like an absent one: no mix samples, no
/// render length and no media requirement.
#[test]
fn a_muted_clip_renders_like_an_absent_clip() {
    let root = temp_dir("muted");
    ramp_media(&root);
    let project = clip_project(
        "  file: \"ramp.wav\"\n  offset: 0.05s\n  gain: -6dB\n  fade_in: 0.01s\n  muted: true",
    );
    let cache = MediaCache::new(None, 64 * 1024 * 1024, 0);
    let lanes = lanes(&cache, &root, &project);
    assert_eq!(lanes.len(), 1, "the muted take keeps its lane index");
    assert_eq!(lanes.get(0).unwrap().length_frames(), 0);
    assert!(lanes.identities().is_empty(), "no media is pinned");

    let mut absent = project.clone();
    absent.clear_audio_clips();
    assert_eq!(
        audio_clip::project_end_frame(&project, RATE).unwrap(),
        audio_clip::project_end_frame(&absent, RATE).unwrap(),
        "a muted clip extends nothing"
    );

    let length = 4 * RATE as usize;
    let mut muted_buffer = vec![0.0_f32; length * 2];
    let derived = DerivedCache::new(None, 64 * 1024 * 1024, 0);
    audio_clip::mix_into_track_with_caches(
        &cache,
        &derived,
        &mut muted_buffer,
        &project,
        project.audio_clip(0).unwrap(),
        RATE,
        Some(&root),
    )
    .unwrap();
    let absent_buffer = vec![0.0_f32; length * 2];
    assert_eq!(muted_buffer, absent_buffer, "the mix is sample-identical");

    let mut chunked = vec![0.0_f32; length * 2];
    let mut position = 0;
    while position < length {
        let frames = (length - position).min(CHUNK);
        let mut block = vec![0.0_f32; frames * 2];
        lanes
            .get(0)
            .unwrap()
            .mix_into(position as u64, &mut block)
            .unwrap();
        chunked[position * 2..(position + frames) * 2].copy_from_slice(&block);
        position += frames;
    }
    assert_eq!(chunked, absent_buffer, "the stream is sample-identical");

    // Missing media is not an error while the take is muted, and it is again
    // once the take is unmuted: that is what "skip like absent" means.
    let mut missing = project.clone();
    missing.audio_clip_mut(0).unwrap().file = "missing.wav".into();
    audio_clip::validate(&missing, Some(&root)).unwrap();
    missing.audio_clip_mut(0).unwrap().muted = false;
    assert!(audio_clip::validate(&missing, Some(&root)).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_clip_graphs_share_source_and_derived_pcm_budgets() {
    for stretch in [false, true] {
        let root = tempfile::tempdir().unwrap();
        for (index, file) in ["one.wav", "two.wav", "three.wav"].into_iter().enumerate() {
            write_ramp(&root.path().join(file), 4096, (index + 1) as f32 * 1e-6);
        }
        let source_bytes = 4096 * 8;
        let derived_bytes = source_bytes * 2;
        let source_cap = if stretch {
            1 << 20
        } else {
            source_bytes + 2048
        };
        let cache = MediaCache::new(
            Some(root.path().join("source")),
            source_cap,
            (source_bytes + 2048) as u64,
        );
        let derived = DerivedCache::new(
            Some(root.path().join("derived")),
            derived_bytes,
            (derived_bytes + 1024) as u64,
        );
        let project = |file| {
            let playback = if stretch {
                "\n  stretch: 2.0\n  mode: percussive"
            } else {
                ""
            };
            clip_project(&format!("  file: \"{file}\"{playback}"))
        };
        let load = |file| {
            ClipLanes::load_with_caches(&cache, &derived, &project(file), RATE, Some(root.path()))
        };
        let first = load("one.wav").unwrap();
        let shared = load("one.wav").unwrap();
        let second = load("two.wav").unwrap();
        let failure = load("three.wav")
            .err()
            .expect("RAM and live overflow disk are full");
        assert!(failure.contains("shared memory budget"), "{failure}");

        let usage = || {
            if stretch {
                derived.disk_usage()
            } else {
                cache.disk_usage()
            }
        };
        assert_eq!(
            usage().live_temporary_bytes,
            if stretch { derived_bytes } else { source_bytes } as u64
        );
        assert_eq!(usage().open_cached_bytes, 0);

        let reference = lanes(
            &MediaCache::new(None, 1 << 20, 0),
            root.path(),
            &project("two.wav"),
        );
        let mix = |lanes: &ClipLanes| {
            let lane = lanes.get(0).unwrap();
            let mut block = [0.0; 1024];
            lane.mix_into(lane.start_frame() + 117, &mut block).unwrap();
            block
        };
        assert_eq!(
            mix(&second),
            mix(&reference),
            "the overflow copy is sample-exact"
        );
        assert!(mix(&second).iter().any(|sample| *sample != 0.0));
        drop(first);
        assert!(
            load("three.wav").is_err(),
            "a shared graph still owns the first pin"
        );
        drop(shared);
        assert!(
            load("three.wav").is_ok(),
            "dropping the last graph releases its pin"
        );
    }
}

#[test]
fn native_spools_share_the_live_budget_and_survive_cache_clear() {
    for stretch in [false, true] {
        let root = tempfile::tempdir().unwrap();
        write_ramp(&root.path().join("one.wav"), 4096, 1e-6);
        write_ramp(&root.path().join("two.wav"), 4096, 2e-6);
        let source_dir = root.path().join("source");
        let derived_dir = root.path().join("derived");
        let cache = MediaCache::new(
            Some(source_dir.clone()),
            if stretch { 1 << 20 } else { 16 << 10 },
            48 << 10,
        );
        let derived = DerivedCache::new(Some(derived_dir.clone()), 16 << 10, 80 << 10);
        let (dir, suffix) = if stretch {
            (&derived_dir, "ocd")
        } else {
            (&source_dir, "ocm")
        };
        let load = |file| {
            let playback = if stretch {
                "\n  stretch: 2.0\n  mode: percussive"
            } else {
                ""
            };
            let project = clip_project(&format!("  file: \"{file}\"{playback}"));
            ClipLanes::load_with_caches(&cache, &derived, &project, RATE, Some(root.path()))
        };
        let first = load("one.wav").unwrap();
        let backing = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == suffix))
            .unwrap();
        let unrelated = dir.join(format!("unrelated.{suffix}"));
        std::fs::write(&unrelated, b"keep").unwrap();

        // Clear the indexed entries. A second graph must reopen the same backing.
        let clear = || {
            if stretch {
                derived.clear();
            } else {
                cache.clear();
            }
        };
        clear();
        let shared = load("one.wav").unwrap();
        let error = load("two.wav")
            .err()
            .expect("the native disk budget is full");
        assert!(error.contains("shared disk budget"), "{error}");
        assert!(backing.is_file(), "eviction removed a live backing");
        clear();
        assert!(backing.is_file(), "clear removed a live backing");
        assert!(unrelated.is_file());
        let usage = if stretch {
            derived.disk_usage()
        } else {
            cache.disk_usage()
        };
        let file_bytes = backing.metadata().unwrap().len();
        assert_eq!(usage.cached_bytes, file_bytes);
        assert_eq!(usage.open_cached_bytes, file_bytes);
        assert_eq!(usage.live_temporary_bytes, 0);

        let mut block = [0.0; 1024];
        let lane = shared.get(0).unwrap();
        lane.mix_into(lane.start_frame() + 117, &mut block).unwrap();
        assert!(block.iter().any(|sample| *sample != 0.0));

        drop((first, shared));
        clear();
        assert!(
            !backing.exists(),
            "clear must reclaim the file after the final release"
        );
        let two = load("two.wav").unwrap();
        drop(two);
        if stretch {
            let backing = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    *path != unrelated && path.extension().is_some_and(|ext| ext == suffix)
                })
                .unwrap();
            let mut bytes = std::fs::read(&backing).unwrap();
            bytes[0] ^= 0xff;
            std::fs::write(&backing, bytes).unwrap();
            let before = derived.stats().bakes;
            drop(load("two.wav").unwrap());
            drop(load("two.wav").unwrap());
            assert_eq!(
                derived.stats().bakes,
                before + 1,
                "repair should publish a usable backing"
            );
        }
    }
}
