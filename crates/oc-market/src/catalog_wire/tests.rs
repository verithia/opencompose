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
fn typed_descriptors_and_unknown_pack_payloads_survive_round_trip() {
    let source = fixtures()["complete"].clone();
    let catalog = parse(&serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(catalog.entries[0].installs[0].companion_content.is_some());
    assert!(catalog.entries[0].installs[1].runtime.is_some());
    assert_eq!(catalog.packs[0].soundfont().unwrap().license.name, "MIT");
    assert!(matches!(&catalog.packs[1], Pack::Unknown(_)));
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
