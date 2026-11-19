use super::*;
use serde_json::{Value, json};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/catalog-validation.json")).unwrap()
}

#[test]
fn shared_catalog_boundary_fixtures_have_stable_codes_and_paths() {
    for case in fixtures()["invalid"].as_array().unwrap() {
        let error = parse(&serde_json::to_vec(&case["document"]).unwrap()).unwrap_err();
        assert_eq!(
            serde_json::to_value(error.code).unwrap(),
            case["code"],
            "{}: {error}",
            case["name"]
        );
        assert_eq!(error.path, case["path"], "{}: {error}", case["name"]);
    }
}

#[test]
fn all_platforms_formats_and_legacy_defaults_keep_their_wire_spelling() {
    let base = fixtures()["base"].clone();
    for schema in [1, 2] {
        for platform in [
            Platform::MacosAarch64,
            Platform::MacosX86_64,
            Platform::WindowsX86_64,
            Platform::LinuxX86_64,
        ] {
            for format in [
                Format::Vst3,
                Format::Vst2,
                Format::Clap,
                Format::Au,
                Format::Lv2,
                Format::Standalone,
            ] {
                let mut value = base.clone();
                value["schema"] = schema.into();
                let install = &mut value["entries"][0]["installs"][0];
                install["platform"] = platform.as_str().into();
                install["format"] = format.as_str().into();
                install.as_object_mut().unwrap().remove("plugin_id");
                if matches!(format, Format::Vst2 | Format::Vst3) {
                    install["plugin_id"] = format!("{}:example", format.as_str()).into();
                }
                if format == Format::Lv2 {
                    install["plugin_id"] = "urn:example:synth".into();
                }
                let catalog = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
                assert_eq!(catalog.entries[0].installs[0].platform, platform);
                assert_eq!(catalog.entries[0].installs[0].format, format);
                assert_eq!(catalog.entries[0].aliases, ["ExampleSynth"]);
                assert!(!catalog.entries[0].installs[0].wrapped);
                assert!(catalog.packs.is_empty());
                let encoded = serde_json::to_value(catalog).unwrap();
                assert_eq!(
                    encoded["entries"][0]["installs"][0]["platform"],
                    platform.as_str()
                );
                assert_eq!(
                    encoded["entries"][0]["installs"][0]["format"],
                    format.as_str()
                );
            }
        }
    }
}

#[test]
fn plugin_archive_wire_spellings_include_tar_xz_and_7z() {
    for (spelling, archive) in [
        ("zip", Archive::Zip),
        ("tar-gz", Archive::TarGz),
        ("tar-xz", Archive::TarXz),
        ("7z", Archive::SevenZ),
        ("installer", Archive::Installer),
    ] {
        let parsed: Archive = serde_json::from_value(json!(spelling)).unwrap();
        assert_eq!(parsed, archive);
        assert_eq!(serde_json::to_value(archive).unwrap(), json!(spelling));
    }
}

#[test]
fn typed_descriptors_and_unknown_pack_payloads_survive_round_trip() {
    let source = fixtures()["complete"].clone();
    let catalog = parse(&serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(catalog.entries[0].installs[0].companion_content.is_some());
    assert!(catalog.entries[0].installs[1].runtime.is_some());
    assert_eq!(catalog.packs[0].soundfont().unwrap().license.name, "MIT");
    let archive = catalog.packs[1]
        .sound_archive()
        .expect("the sound archive pack is typed");
    assert_eq!(archive.id, "freepats.upright-piano-kw-small");
    assert_eq!(archive.content, SoundContent::Soundfont);
    assert_eq!(archive.archive, SoundArchive::SevenZ);
    assert!(matches!(&catalog.packs[2], Pack::Unknown(_)));
    let encoded = serde_json::to_value(&catalog).unwrap();
    for field in ["aliases", "known_issues", "installs"] {
        // Omitted default fields may appear during serialization; signed descriptor values remain exact.
        if field != "installs" {
            assert_eq!(encoded["entries"][0][field], source["entries"][0][field]);
        }
    }
    assert_eq!(
        encoded["entries"][0]["installs"][0]["companion_content"],
        source["entries"][0]["installs"][0]["companion_content"]
    );
    assert_eq!(
        encoded["entries"][0]["installs"][1]["runtime"],
        source["entries"][0]["installs"][1]["runtime"]
    );
    assert_eq!(encoded["packs"], source["packs"]);
}

#[test]
fn unknown_sound_archive_values_stay_opaque_for_newer_clients() {
    for field in ["content", "archive"] {
        let mut value = fixtures()["complete"].clone();
        value["packs"][1][field] = "future".into();
        let catalog = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(&catalog.packs[1], Pack::Unknown(_)));
        assert_eq!(catalog.packs[1].id(), "freepats.upright-piano-kw-small");
    }
}

#[test]
fn unknown_pack_bytes_and_pack_counts_are_bounded() {
    let mut value = fixtures()["base"].clone();
    value["packs"] = json!([{"kind":"future","id":"bank","payload":"x".repeat(MAX_PACK_BYTES)}]);
    let error = parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert_eq!(error.code, CatalogErrorCode::Parse);
    assert_eq!(error.path, "packs[0]");
    value["packs"] = Value::Array(vec![json!({"kind":"future","id":"bank"}); MAX_PACKS + 1]);
    let error = parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert_eq!(error.code, CatalogErrorCode::Parse);
    assert_eq!(error.path, "$");
}

#[test]
fn install_take_paths_are_relative_and_match_the_install_format() {
    let mut value = fixtures()["base"].clone();
    value["entries"][0]["installs"][0]["take"] = json!(["Pkg/Plugin.vst3", "Extras/Helpers.vst3"]);
    let catalog = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        catalog.entries[0].installs[0].take.as_deref(),
        Some(
            &[
                "Pkg/Plugin.vst3".to_string(),
                "Extras/Helpers.vst3".to_string()
            ][..]
        )
    );
    let encoded = serde_json::to_value(&catalog).unwrap();
    assert_eq!(
        encoded["entries"][0]["installs"][0]["take"],
        value["entries"][0]["installs"][0]["take"]
    );
}

#[test]
fn install_take_paths_reject_traversal_absolute_names_and_wrong_formats() {
    let invalid = [
        json!([]),
        json!(["Pkg/Plugin.vst3", "../Plugin.vst3"]),
        json!(["/Pkg/Plugin.vst3"]),
        json!(["Pkg\\Plugin.vst3"]),
        json!(["C:/Pkg/Plugin.vst3"]),
        json!(["Pkg/../Plugin.vst3"]),
        json!(["Pkg//Plugin.vst3"]),
        json!(["Pkg/Plugin.clap"]),
        json!(["Pkg/Plugin.vst3", "Pkg/Plugin.vst3"]),
        json!([format!("Pkg/{}.vst3", "x".repeat(256))]),
        json!(vec!["Pkg/Plugin.vst3"; MAX_TAKE_PATHS + 1]),
    ];
    for take in invalid {
        let mut value = fixtures()["base"].clone();
        value["entries"][0]["installs"][0]["take"] = take;
        let error = parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(error.code, CatalogErrorCode::InvalidTake, "{error}");
        assert!(
            error.path.starts_with("entries[0].installs[0].take"),
            "{error}"
        );
    }
}

#[test]
fn install_take_paths_accept_the_vst2_library_spellings() {
    for take in ["Pkg/Plugin.vst", "Pkg/Plugin.dll", "Pkg/Plugin.so"] {
        let mut value = fixtures()["base"].clone();
        let install = &mut value["entries"][0]["installs"][0];
        install["format"] = "vst2".into();
        install["plugin_id"] = "vst2:plugin".into();
        install["take"] = json!([take]);
        parse(&serde_json::to_vec(&value).unwrap()).unwrap_or_else(|error| {
            panic!("{take}: {error}");
        });
    }
    let mut value = fixtures()["base"].clone();
    value["entries"][0]["installs"][0]["format"] = "standalone".into();
    value["entries"][0]["installs"][0]["take"] = json!(["Pkg/Plugin"]);
    let error = parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert_eq!(error.code, CatalogErrorCode::InvalidTake);
}
