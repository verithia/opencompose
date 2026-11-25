use oc_synth_core::sampler::{Sampler, patch_from_sfz_text};

fn write_constant(root: &std::path::Path) {
    let path = root.join("constant.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..64 {
        writer.write_sample(0.25_f32).unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
fn curved_sampler_release_follows_shape_and_finishes_without_heap_operations() {
    let root = oc_test_support::temp_dir("oc-sampler-release", "curves");
    write_constant(&root);
    for shape in [-6.0_f64, 0.0, 6.0] {
        let patch = patch_from_sfz_text(
            &format!(
                "<group> ampeg_release=1 ampeg_release_shape={shape}\n<region> sample=constant.wav key=60 loop_mode=loop_continuous amp_veltrack=0"
            ),
            std::slice::from_ref(&root),
        );
        assert!(patch.diagnostics.is_empty(), "{:?}", patch.diagnostics);
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let mut quarter = 0.0_f32;
        let mut last = 0.0_f32;
        let operations = oc_test_support::operations(|| {
            sampler.note_on(0, 60, 100);
            for _ in 0..32 {
                sampler.next_stereo(8_000);
            }
            sampler.note_off(0, 60);
            for frame in 1..=8_001 {
                let value = sampler.next_stereo(8_000)[0];
                if frame == 2_000 {
                    quarter = value;
                }
                last = value;
            }
            sampler.reset();
        });
        assert_eq!(
            operations, 0,
            "release, render and reset must remain heap-free"
        );
        let expected = if shape == 0.0 {
            0.75
        } else {
            1.0 - (shape * 0.25).exp_m1() / shape.exp_m1()
        };
        assert!((f64::from(quarter) / 0.25 - expected).abs() < 1e-5);
        assert_eq!(last, 0.0);
        assert!(!sampler.is_active());
    }
}

#[test]
fn release_shape_inherits_overrides_and_rejects_invalid_patch_values() {
    let root = oc_test_support::temp_dir("oc-sampler-release", "validation");
    write_constant(&root);
    let patch = patch_from_sfz_text(
        "<global> ampeg_release_shape=-4\n<group> ampeg_release_shape=3\n<region> sample=constant.wav\n<region> sample=constant.wav ampeg_release_shape=-2",
        std::slice::from_ref(&root),
    );
    assert!(patch.diagnostics.is_empty());
    assert_eq!(patch.regions[0].ampeg.release_shape, 3.0);
    assert_eq!(patch.regions[1].ampeg.release_shape, -2.0);
    for value in [f64::NAN, f64::INFINITY, -21.0, 21.0] {
        let mut invalid = patch.clone();
        invalid.regions[0].ampeg.release_shape = value;
        invalid.regions[0].sample = root.join("missing.wav");
        let error = match Sampler::from_patch(&invalid) {
            Ok(_) => panic!("invalid shape must fail before decoding any sample"),
            Err(error) => error,
        };
        assert!(error.contains("release shape"));
    }
}
