//! Read-only snapshot input uses the same rendering and relative media owner as DSL text.

#[test]
fn project_snapshot_render_stems_and_bounce_preserve_text_audio_and_resource_roots() {
    let root = oc_audio::private_fs::test_root().join("cli-project-snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let mut writer = hound::WavWriter::create(
        root.join("voice.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..24_000 {
        writer.write_sample(0.25_f32).unwrap();
    }
    writer.finalize().unwrap();
    let text = "@project \"Snapshot input\"\n  tempo: 120\n@audio_clip voice [1:1 - 1:2] | track: tape\n  file: \"voice.wav\"\n@track tape | instrument: audio/clip\n";
    std::fs::write(root.join("source.ocp"), text).unwrap();
    let project = oc_dsl::parse(text).unwrap();
    std::fs::write(
        root.join("snapshot.ocpc"),
        oc_dsl::diff::compressed::encode_project(&project).unwrap(),
    )
    .unwrap();

    for command in ["render", "stems", "bounce"] {
        let mut rendered = Vec::new();
        for (label, input) in [("text", "source.ocp"), ("binary", "snapshot.ocpc")] {
            let output = root.join(format!("{command}-{label}.wav"));
            let mut child = crate::ocp();
            child.env(
                "OPENCOMPOSE_CONFIG_DIR",
                root.join(format!("profile-{command}-{label}")),
            );
            child
                .arg(if command == "bounce" {
                    "bounce"
                } else {
                    "render"
                })
                .arg(root.join(input))
                .arg("--output")
                .arg(&output);
            if command == "bounce" {
                child.args(["--track", "tape", "--from", "1:1", "--to", "1:2"]);
            } else {
                child.args(["--sample-rate", "48000", "--sample-format", "f32"]);
                if command == "stems" {
                    child
                        .arg("--stems")
                        .arg(root.join(format!("stems-{label}")));
                }
            }
            let result = child.output().unwrap();
            assert!(
                result.status.success(),
                "{command}/{label}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut reader = hound::WavReader::open(&output).unwrap();
            assert!(
                reader
                    .samples::<f32>()
                    .any(|sample| sample.unwrap().abs() > 0.1)
            );
            rendered.push(std::fs::read(output).unwrap());
        }
        assert_eq!(rendered[0], rendered[1], "{command} output differs");
    }
}

#[test]
fn project_snapshot_invalid_payload_leaves_existing_output_unchanged() {
    let root = oc_audio::private_fs::test_root().join("cli-invalid-project-snapshot");
    std::fs::create_dir_all(&root).unwrap();
    let input = root.join("invalid.ocpc");
    let output = root.join("out.wav");
    std::fs::write(&input, b"OCPC\x01\xff\xff\xff\xff").unwrap();
    std::fs::write(&output, b"existing output").unwrap();
    let result = crate::ocp()
        .env("OPENCOMPOSE_CONFIG_DIR", root.join("profile"))
        .arg("render")
        .arg(input)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("decoded size exceeds"));
    assert_eq!(std::fs::read(output).unwrap(), b"existing output");
}
