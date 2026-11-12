//! Plugin preset state resolution tests.

use super::super::*;
use super::*;

#[test]
fn host_parameter_key_ignores_separator_style() {
    let key = oc_host::preset_parameters::host_parameter_key;
    assert_eq!(key("vst3_state_file"), "vst3statefile");
    assert_eq!(key("plugin.state-file"), "pluginstatefile");
}

#[test]
fn project_transport_uses_project_meter() {
    let mut project = empty_project();
    project.properties.tempo = 90.0;
    project.properties.time_signature = TimeSignature {
        numerator: 7,
        denominator: 8,
    };

    let transport = project_transport(&project, 48_000, 48_000);

    assert_eq!(transport.sample_pos, 48_000);
    assert_eq!(transport.sample_rate, 48_000);
    assert_eq!(transport.tempo, 90.0);
    assert_eq!(transport.time_sig_numerator, 7);
    assert_eq!(transport.time_sig_denominator, 8);
    assert_eq!(transport.quarter_notes(), 1.5);
}

#[test]
fn state_file_path_prefers_format_specific_alias() {
    let params = vec![
        (
            "state_file".to_string(),
            ParamValue::String("/tmp/generic.state".to_string()),
        ),
        (
            "plugin_state_file".to_string(),
            ParamValue::String("/tmp/plugin.state".to_string()),
        ),
        (
            "vst3_state_file".to_string(),
            ParamValue::String("/tmp/specific.vstpreset".to_string()),
        ),
    ];

    assert_eq!(
        resolve_state_file_path(&params, "vst3statefile"),
        Some("/tmp/specific.vstpreset")
    );
}

#[test]
fn state_file_path_falls_back_to_generic_aliases() {
    let params = vec![
        (
            "state_file".to_string(),
            ParamValue::String("/tmp/generic.state".to_string()),
        ),
        (
            "plugin_state_file".to_string(),
            ParamValue::String("/tmp/plugin.state".to_string()),
        ),
    ];

    assert_eq!(
        resolve_state_file_path(&params, "clapstatefile"),
        Some("/tmp/plugin.state")
    );
}

#[test]
fn state_hex_prefers_format_specific_alias() {
    let params = vec![
        (
            "state_hex".to_string(),
            ParamValue::String("0102".to_string()),
        ),
        (
            "plugin_state_hex".to_string(),
            ParamValue::String("0304".to_string()),
        ),
        (
            "clap_state_hex".to_string(),
            ParamValue::String("0a ff".to_string()),
        ),
    ];

    assert_eq!(resolve_state_hex(&params, "clapstatehex"), Some("0a ff"));
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Clap,
        Some("clap:channel"),
        "Channel",
    );
    assert_eq!(
        resolve_plugin_state_bytes(&params, &context, "clapstatehex", "clapstatefile", None)
            .unwrap(),
        Some(vec![0x0a, 0xff])
    );
}

#[test]
fn invalid_state_hex_reports_context() {
    let err = decode_plugin_state_hex("VST3", "0g").unwrap_err();
    assert!(err.contains("invalid VST3 state hex at byte 0"));
}

#[test]
fn read_plugin_state_file_reports_missing_path() {
    let err = read_plugin_state_file(
        "VST3",
        "/tmp/opencompose-missing-state-file.vstpreset",
        None,
    )
    .unwrap_err();

    assert!(err.contains("failed to read VST3 state file"));
    assert!(err.contains("/tmp/opencompose-missing-state-file.vstpreset"));
}

#[test]
fn state_file_resolves_relative_to_project_resource_root() {
    let root = std::env::temp_dir().join(format!("opencompose-state-root-{}", std::process::id()));
    std::fs::create_dir_all(root.join("states")).unwrap();
    std::fs::write(root.join("states/sampler.bin"), [1_u8, 2, 3]).unwrap();

    let bytes = read_plugin_state_file("VST3", "states/sampler.bin", Some(&root)).unwrap();

    assert_eq!(bytes, vec![1, 2, 3]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sfizz_state_encodes_canonical_project_relative_sfz_path() {
    let root = std::env::temp_dir().join(format!("opencompose-sfizz-root-{}", std::process::id()));
    std::fs::create_dir_all(root.join("instruments")).unwrap();
    let sfz = root.join("instruments/test.sfz");
    std::fs::write(&sfz, "<region> sample=*sine\n").unwrap();
    let preset = PresetDef {
        name: "Test sfizz".to_string(),
        source: Some("sample".to_string()),
        plugin: Some("sfizz".to_string()),
        program: None,
        patch: None,
        file: Some("instruments/test.sfz".to_string()),
        root_note: None,
        params: vec![],
        effects: vec![],
        extends: None,
    };

    let state = sfizz_vst3_state_bytes(&preset, Some(&root)).unwrap();
    assert_eq!(&state[0..8], &5_u64.to_le_bytes());
    let path_length = i32::from_le_bytes(state[8..12].try_into().unwrap()) as usize;
    let path = std::str::from_utf8(&state[12..12 + path_length - 1]).unwrap();
    assert_eq!(Path::new(path), sfz.canonicalize().unwrap());
    assert_eq!(state[12 + path_length - 1], 0);
    // Check the tuning fields against SfizzVstState::store's version-5 ABI,
    // not only the leading SFZ path. Empty Scala means no custom scale.
    let tuning = 12 + path_length + 16;
    assert_eq!(&state[tuning..tuning + 5], &[1, 0, 0, 0, 0]);
    assert_eq!(
        i32::from_le_bytes(state[tuning + 5..tuning + 9].try_into().unwrap()),
        60
    );
    assert_eq!(
        f32::from_le_bytes(state[tuning + 9..tuning + 13].try_into().unwrap()),
        440.0
    );
    assert_eq!(
        f32::from_le_bytes(state[tuning + 13..tuning + 17].try_into().unwrap()),
        0.0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires installed sfizz VST3; actual host tuning integration"]
fn sfizz_default_state_preserves_equal_temperament() {
    let root = std::env::temp_dir().join(format!("ocp-sfizz-tuning-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("sine.sfz"),
        "<region> sample=*sine ampeg_attack=0.01 ampeg_release=0.01\n",
    )
    .unwrap();
    for (pitch, expected) in [("A3", 220.0), ("A4", 440.0), ("A5", 880.0)] {
        let source = format!(
            "@ocp \"0.2.0\"\n@project \"Tuning diagnostic\"\n  tempo: 60\n@preset sine\n  source: sample\n  plugin: \"sfizz\"\n  file: \"sine.sfz\"\n@track probe | preset: sine\n  @phrase held [1:1 - 2:1]\n    @0 {pitch} h v=100\n"
        );
        let project = oc_dsl::parse(&source).unwrap();
        let mut cache = super::RenderCache::new();
        cache.set_resource_root(Some(root.clone()));
        let audio = cache
            .render_full(
                &project,
                Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new(),
            )
            .unwrap();
        assert!(audio.iter().all(|x| x.is_finite()));
        let mono: Vec<f64> = audio
            .chunks_exact(2)
            .skip(2000)
            .take(8000)
            .map(|s| (s[0] as f64 + s[1] as f64) * 0.5)
            .collect();
        assert!(mono.iter().any(|x| x.abs() > 0.001));
        let crossings: Vec<f64> = mono
            .windows(2)
            .enumerate()
            .filter(|&(_, s)| s[0] <= 0.0 && s[1] > 0.0)
            .map(|(i, s)| i as f64 - s[0] / (s[1] - s[0]))
            .collect();
        assert!(crossings.len() > 100);
        let measured =
            8000.0 * (crossings.len() - 1) as f64 / (crossings.last().unwrap() - crossings[0]);
        eprintln!("sfizz default-state {pitch}: {measured:.5} Hz, expected {expected}");
        assert!((measured - expected).abs() < 0.2);
    }
    std::fs::remove_file(root.join("sine.sfz")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
#[ignore = "requires OCP_TEST_PLUGINS=1, OCP_PRESET_LIBRARY and the installed VST3"]
fn vst3_imported_preset_state_renders() {
    let Some(root) = std::env::var_os("OCP_PRESET_LIBRARY").map(std::path::PathBuf::from) else {
        eprintln!("set OCP_PRESET_LIBRARY; skipping");
        return;
    };
    let preset = root.join("Factory/Keyboard/KY - Additive.SerumPreset");
    if !preset.exists() {
        eprintln!("factory preset not installed; skipping");
        return;
    }
    let project = oc_dsl::parse(&format!(
            "@project\n  tempo: 120\n@preset imported\n  source: vst3\n  plugin: \"vst3:serum_2\"\n  params: {{ serum_file: \"{}\" }}\n@track t | preset: imported\n  @phrase p [1:1 - 2:1]\n    C4 q v=100\n",
            preset.display()
        ))
        .unwrap();
    let mut cache = super::RenderCache::new();
    let audio = cache
        .render_full(
            &project,
            std::path::Path::new("/tmp/unused.sf2"),
            48_000,
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
    let peak = audio
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.001, "imported preset rendered silence");
}

fn vst3_context<'a>(preset: &'a str) -> oc_host::PluginStateContext<'a> {
    oc_host::PluginStateContext::new(oc_host::PluginFormat::Vst3, Some("vst3:halcyon_2"), preset)
}

fn state_file_params(path: &std::path::Path) -> Vec<(String, ParamValue)> {
    vec![(
        "vst3_state_file".to_string(),
        ParamValue::String(path.to_string_lossy().into_owned()),
    )]
}

#[test]
fn an_enveloped_state_file_loads_the_original_chunk() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-envelope-load-{}.vstpreset",
        std::process::id()
    ));
    let chunk = b"opaque vst3 chunk";
    std::fs::write(
        &path,
        oc_host::encode_plugin_state(oc_host::PluginFormat::Vst3, "vst3:halcyon_2", chunk),
    )
    .unwrap();

    let loaded = resolve_plugin_state_bytes(
        &state_file_params(&path),
        &vst3_context("Halcyon Lead"),
        "vst3statehex",
        "vst3statefile",
        None,
    )
    .unwrap();

    assert_eq!(loaded.as_deref(), Some(chunk.as_slice()));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_newer_enveloped_state_is_refused_before_the_plugin_sees_it() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-envelope-newer-{}.vstpreset",
        std::process::id()
    ));
    std::fs::write(
        &path,
        oc_host::encode_plugin_state_versioned(
            oc_host::PluginFormat::Vst3,
            "vst3:halcyon_2",
            oc_host::supported_plugin_state_version() + 1,
            b"future chunk",
        ),
    )
    .unwrap();

    let error = resolve_plugin_state_bytes(
        &state_file_params(&path),
        &vst3_context("Halcyon Lead"),
        "vst3statehex",
        "vst3statefile",
        None,
    )
    .unwrap_err();

    let newer = oc_host::supported_plugin_state_version() + 1;
    assert!(error.contains(&format!("state version {newer}")), "{error}");
    assert!(
        error.contains(&format!(
            "supports version {}",
            oc_host::supported_plugin_state_version()
        )),
        "{error}"
    );
    assert!(error.contains("preset 'Halcyon Lead'"), "{error}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_tampered_envelope_file_is_refused() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-envelope-tampered-{}.vstpreset",
        std::process::id()
    ));
    let mut encoded = oc_host::encode_plugin_state(
        oc_host::PluginFormat::Vst3,
        "vst3:halcyon_2",
        b"original chunk",
    );
    let last = encoded.len() - 1;
    encoded[last] ^= 0xff;
    std::fs::write(&path, encoded).unwrap();

    let error = resolve_plugin_state_bytes(
        &state_file_params(&path),
        &vst3_context("Halcyon Lead"),
        "vst3statehex",
        "vst3statefile",
        None,
    )
    .unwrap_err();

    assert!(error.contains("sha256 mismatch"), "{error}");
    assert!(error.contains("preset 'Halcyon Lead'"), "{error}");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_legacy_bare_state_file_still_loads_unchanged() {
    let path = std::env::temp_dir().join(format!(
        "opencompose-legacy-state-{}.vstpreset",
        std::process::id()
    ));
    std::fs::write(&path, [9_u8, 8, 7]).unwrap();

    let loaded = resolve_plugin_state_bytes(
        &state_file_params(&path),
        &vst3_context("Halcyon Lead"),
        "vst3statehex",
        "vst3statefile",
        None,
    )
    .unwrap();

    assert_eq!(loaded, Some(vec![9, 8, 7]));
    std::fs::remove_file(path).unwrap();
}

fn generated_import_container(product_version: &str) -> Vec<u8> {
    let metadata =
        serde_json::to_vec(&serde_json::json!({ "productVersion": product_version })).unwrap();
    let mut bytes = oc_synth::synth::foreign_preset::MAGIC.to_vec();
    bytes.extend_from_slice(&u32::try_from(metadata.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    // Generated Zstandard single-segment frame: one final raw block containing
    // the CBOR empty map. No external preset or plugin is needed.
    bytes.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x01, 0x09, 0, 0, 0xa0]);
    bytes
}

#[test]
fn materialized_vst3_state_keeps_import_precedence_roots_and_versioned_cache() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("generated.preset");
    let source = generated_import_container("1");
    std::fs::write(&path, &source).unwrap();
    let expected = oc_synth::synth::foreign_preset::ForeignPreset::parse(&source)
        .unwrap()
        .to_vst3_state()
        .unwrap();
    let context = vst3_context("Generated fixture");
    let params = vec![
        (
            "serum_file".into(),
            ParamValue::String("generated.preset".into()),
        ),
        (
            "vst3_state_hex".into(),
            ParamValue::String("not hex".into()),
        ),
    ];
    let first = materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
        .unwrap()
        .unwrap();
    let again = materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
        .unwrap()
        .unwrap();
    assert_eq!(first.as_slice(), expected);
    assert!(std::sync::Arc::ptr_eq(&first, &again));
    let replacement = generated_import_container("2");
    assert_eq!(source.len(), replacement.len());
    std::fs::write(&path, replacement).unwrap();
    // Set an unambiguous second-level version even on a coarse filesystem.
    std::fs::File::open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2),
            ),
        )
        .unwrap();
    let changed = materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
        .unwrap()
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &changed));
    assert_ne!(first.as_slice(), changed.as_slice());
}

#[test]
fn materialized_vst3_state_preserves_hex_precedence_and_envelope_integrity() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("bare.state"), [9, 9]).unwrap();
    let context = vst3_context("Materialization fixture");
    let params = vec![
        (
            "vst3_state_file".into(),
            ParamValue::String("bare.state".into()),
        ),
        ("vst3_state_hex".into(), ParamValue::String("0a ff".into())),
    ];
    assert_eq!(
        materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
            .unwrap()
            .unwrap()
            .as_slice(),
        [10, 255]
    );
    let encoded = oc_host::encode_plugin_state(
        oc_host::PluginFormat::Vst3,
        "vst3:halcyon_2",
        b"fixture chunk",
    );
    std::fs::write(root.path().join("checked.state"), &encoded).unwrap();
    let params = vec![(
        "vst3_state_file".into(),
        ParamValue::String("checked.state".into()),
    )];
    assert_eq!(
        materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
            .unwrap()
            .unwrap()
            .as_slice(),
        b"fixture chunk"
    );
    let mut corrupt = encoded.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(root.path().join("checked.state"), corrupt).unwrap();
    let error = materialize_vst3_preset_state_at(&params, &context, Some(root.path())).unwrap_err();
    assert!(error.contains("Materialization fixture"));
    assert!(error.contains("sha256 mismatch"));
    std::fs::write(root.path().join("checked.state"), encoded).unwrap();
    let other = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Vst3,
        Some("vst3:other"),
        "Other fixture",
    );
    assert!(
        materialize_vst3_preset_state_at(&params, &other, Some(root.path()))
            .unwrap_err()
            .contains("belongs to plugin")
    );
}

#[test]
fn materialized_vst3_state_bounds_files_before_read_and_keeps_empty_raw_compatibility() {
    let root = tempfile::tempdir().unwrap();
    let context = vst3_context("Bounded fixture");
    let path = root.path().join("large.state");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(u64::try_from(oc_host::plugin_state_io::MAX_PLUGIN_STATE_BYTES).unwrap() + 1)
        .unwrap();
    let params = vec![(
        "vst3_state_file".into(),
        ParamValue::String("large.state".into()),
    )];
    assert!(
        materialize_vst3_preset_state_at(&params, &context, Some(root.path()))
            .unwrap_err()
            .contains("byte limit")
    );
    let imported = vec![(
        "serum_file".into(),
        ParamValue::String("large.state".into()),
    )];
    assert!(
        materialize_vst3_preset_state_at(&imported, &context, Some(root.path()))
            .unwrap_err()
            .contains("byte limit")
    );
    std::fs::write(root.path().join("empty.state"), []).unwrap();
    let empty = vec![(
        "vst3_state_file".into(),
        ParamValue::String("empty.state".into()),
    )];
    assert!(
        materialize_vst3_preset_state_at(&empty, &context, Some(root.path()))
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        materialize_vst3_preset_state_at(&[], &context, Some(root.path()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn host_state_alias_resolution_does_not_strip_native_plugin_suffixes() {
    let params = vec![
        (
            "state_file.dll".into(),
            ParamValue::String("must-not-be-opened.state".into()),
        ),
        (
            "StateHex.vst3".into(),
            ParamValue::String("not hexadecimal".into()),
        ),
    ];
    assert_eq!(resolve_state_file_path(&params, "vst2statefile"), None);
    assert_eq!(resolve_state_hex(&params, "vst2statehex"), None);
    let context = PluginStateContext::new(
        PluginFormat::Vst2,
        Some("test.fixture.vst2.tone"),
        "Suffix fixture",
    );
    assert!(
        materialize_vst2_preset_state_at(&params, &context, None)
            .unwrap()
            .is_none()
    );
    let mut real = params;
    real.push(("VST2.State-Hex".into(), ParamValue::String("0a ff".into())));
    assert_eq!(
        materialize_vst2_preset_state_at(&real, &context, None)
            .unwrap()
            .as_deref(),
        Some([10, 255].as_slice())
    );
    real.last_mut().unwrap().1 = ParamValue::String("invalid hex".into());
    assert!(materialize_vst2_preset_state_at(&real, &context, None).is_err());
}
