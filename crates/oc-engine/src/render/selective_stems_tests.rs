#[test]
fn stem_publication_success_reuses_mix_bytes_and_preserves_unrelated_files() {
    let project = builtin_project(220);
    let export = super::StemsExport {
        mix: vec![0.1; 256],
        stems: Vec::new(),
        sample_rate: 8000,
        loudness: None,
    };
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("stems");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("unrelated"), b"keep").unwrap();
    let standalone = root.path().join("opening.wav");
    std::fs::write(&standalone, b"previous").unwrap();
    let receipt = super::write_stems_export_batch(
        &project,
        &dir,
        "small",
        &export,
        default_spec(8000),
        super::StemPublicationOptions {
            manifest: super::StemManifestOptions {
                analyze_levels: false,
            },
            mix_output: Some(&standalone),
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&standalone).unwrap(),
        std::fs::read(dir.join("small-mix.wav")).unwrap()
    );
    assert_eq!(std::fs::read(dir.join("unrelated")).unwrap(), b"keep");
    assert!(receipt.manifest.is_file());
    assert!(receipt.mix_report.pre_clamp_peak_dbfs < 0.0);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
    let internal_mix = dir.join("small-mix.wav");
    let previous_mix = std::fs::read(&internal_mix).unwrap();
    super::write_stems_export_batch(
        &project,
        &dir,
        "small",
        &export,
        default_spec(8000),
        super::StemPublicationOptions {
            manifest: super::StemManifestOptions {
                analyze_levels: false,
            },
            mix_output: Some(&internal_mix),
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(internal_mix).unwrap(), previous_mix);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
}

#[test]
fn stem_publication_late_headroom_failure_preserves_all_previous_outputs() {
    let project = builtin_project(220);
    let export = super::StemsExport {
        mix: vec![0.1; 256],
        stems: [("good", 0.2), ("bad", 3.0)]
            .into_iter()
            .map(|(name, value)| super::StemExportBuffer {
                name: name.into(),
                role: super::StemRole::Bus,
                samples: vec![value; 256],
                output: Some("master".into()),
                mix_sum_member: true,
            })
            .collect(),
        sample_rate: 8000,
        loudness: None,
    };
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("stems");
    std::fs::create_dir(&dir).unwrap();
    let previous = [
        ("small-mix.wav", b"old mix".as_slice()),
        ("small-bus-good.wav", b"old stem"),
        ("stems.json", b"old manifest"),
        ("unrelated.txt", b"keep"),
    ];
    for (name, bytes) in previous {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let standalone = root.path().join("opening.wav");
    std::fs::write(&standalone, b"old standalone").unwrap();
    for target in [&dir, &root.path().join("new-stems")] {
        let result = super::write_stems_export_batch(
            &project,
            target,
            "small",
            &export,
            default_spec(8000),
            super::StemPublicationOptions {
                manifest: super::StemManifestOptions {
                    analyze_levels: false,
                },
                mix_output: Some(&standalone),
            },
        );
        assert!(result.err().unwrap().contains("export refused"));
        assert_eq!(std::fs::read(&standalone).unwrap(), b"old standalone");
        for (name, bytes) in previous {
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes);
        }
        assert!(!root.path().join("new-stems").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), previous.len());
    }
}

#[test]
fn selective_stems_reject_unknown_bus_before_host_or_asset_preparation() {
    let mut project = builtin_project(220);
    project.presets[0].source = Some("vst2".into());
    project.presets[0].plugin = Some("definitely-unavailable-native-device".into());
    let result = super::render_with_selected_stems_with_options(
        &project,
        Path::new("missing.sf2"),
        &mut HashMap::new(),
        super::RenderOptions::new(8000, Default::default()),
        crate::graph::StemCaptureSelection {
            tracks: false,
            buses: Some(vec!["missing".into()]),
            pre_master: false,
        },
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "stem bus 'missing' does not exist"
    );
}

#[test]
fn selective_stems_render_only_requested_taps_and_optional_manifest_analysis() {
    let mut project = builtin_project(220);
    project.tracks[0].mix.output = Some("first".into());
    project.buses = serde_json::from_value(serde_json::json!([
        {"name":"first", "routes":["second"], "effects":[]},
        {"name":"second", "routes":["master"], "effects":[]},
    ]))
    .unwrap();
    let options = || super::RenderOptions::new(8000, Default::default());
    let full = super::render_with_stems_with_options(
        &project,
        Path::new("unused.sf2"),
        &mut HashMap::new(),
        options(),
    )
    .unwrap();
    let selected = super::render_with_selected_stems_with_options(
        &project,
        Path::new("unused.sf2"),
        &mut HashMap::new(),
        options(),
        crate::graph::StemCaptureSelection {
            tracks: false,
            buses: Some(vec!["second".into(), "second".into()]),
            pre_master: true,
        },
    )
    .unwrap();
    assert_eq!(selected.mix, full.mix);
    assert_eq!(selected.stems.len(), 2);
    let bus = selected
        .stems
        .iter()
        .find(|stem| stem.role == super::StemRole::Bus)
        .unwrap();
    assert_eq!(
        bus.samples,
        full.stems
            .iter()
            .find(|stem| stem.name == "second")
            .unwrap()
            .samples
    );
    assert!(bus.mix_sum_member);
    let pre_master = selected
        .stems
        .iter()
        .find(|stem| stem.role == super::StemRole::PreMaster)
        .unwrap();
    assert_eq!(pre_master.samples, selected.mix);
    assert!(!pre_master.mix_sum_member);
    let dir = tempfile::tempdir().unwrap();
    let path = super::write_stems_export_with_options(
        &project,
        dir.path(),
        "small",
        &selected,
        default_spec(8000),
        super::StemManifestOptions {
            analyze_levels: false,
        },
    )
    .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest["stems"].as_array().unwrap().len(), 2);
    assert!(manifest["mix"]["integrated_lufs"].is_null());
    assert!(manifest["mix"]["peak_dbfs"].is_null());
    assert!(manifest["stems"][0]["rms_dbfs"].is_null());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}
