//! CLI preset import, migration and class selection through the normal binary.

#[cfg(target_os = "macos")]
#[test]
fn explicit_audio_unit_lookup_uses_the_registry() {
    let scratch = tempfile::tempdir().unwrap();
    let receipts = scratch.path().join("scan.json");
    let output = super::ocp()
        .env("OPENCOMPOSE_CONFIG_DIR", scratch.path().join("profile"))
        .env("OCP_PLUGIN_SCAN_RECEIPTS", &receipts)
        .args(["resolve-plugin", "aufx:lpas:appl"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plugin: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plugin["id"], "au:aufx:lpas:appl");
    assert_eq!(plugin["format"], "Au");
    assert_eq!(plugin["category"], "Effect");
    assert!(!receipts.exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn vst3_parameter_query_and_preset_import_use_the_resolved_class() {
    use std::{fs, path::Path, process::Command};

    let scratch = tempfile::tempdir().unwrap();
    let bundle = scratch.path().join("ClassSelection.vst3");
    let directory = if cfg!(target_os = "macos") {
        bundle.join("Contents/MacOS")
    } else {
        bundle.join(format!("Contents/{}-linux", std::env::consts::ARCH))
    };
    fs::create_dir_all(&directory).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vst3_class_selection.cc");
    let compiler = std::env::var_os("CXX").unwrap_or_else(|| "c++".into());
    let output = Command::new(compiler)
        .args(["-std=c++17", "-shared", "-fPIC"])
        .arg(fixture)
        .arg("-o")
        .arg(directory.join("ClassSelection"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cli = || {
        let mut cli = super::ocp();
        cli.env("OPENCOMPOSE_CONFIG_DIR", scratch.path().join("profile"))
            .env("OCP_PLUGIN_SCAN_RECEIPTS", scratch.path().join("scan.json"))
            .env("OCP_PLUGIN_SCAN_HELPER", env!("CARGO_BIN_EXE_ocp"));
        cli
    };
    let resolved = cli().arg("resolve-plugin").arg(&bundle).output().unwrap();
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );

    let query = |class: &str| {
        let output = cli()
            .args(["plugin-params", class, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout).unwrap()
    };
    let primary = "vst3:fixture_primary";
    let alternate = "vst3:fixture_alternate";
    assert!(query(primary).is_empty());
    assert!(
        query(alternate)
            .iter()
            .any(|parameter| parameter["name"] == "Gain")
    );

    let preset = scratch.path().join("gain.ffp");
    fs::write(
        &preset,
        "[Preset]\nSignature=FC3p\nVersion=2\n\n[Parameters]\nGain=25\n",
    )
    .unwrap();
    let import = |class: &str| {
        let output = cli()
            .args(["plugin-preset", "import", class])
            .arg(&preset)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    assert_eq!(import(primary)["unmapped"], serde_json::json!(["Gain"]));
    let report = import(alternate);
    assert_eq!(report["mapped"], 1);
    assert_eq!(report["applied"], 1);
    assert_eq!(report["mismatched"], serde_json::json!([]));
}

#[test]
fn plugin_migration_does_not_publish_on_refusal_or_overwrite_an_output() {
    let scratch = tempfile::tempdir().unwrap();
    let input = scratch.path().join("invalid.fxb");
    let output = scratch.path().join("result.vstpreset");
    std::fs::write(&input, b"invalid").unwrap();
    let invoke = || {
        super::ocp()
            .args(["plugin", "migrate-vst2", "--source-name", "Example"])
            .arg(&input)
            .arg("--plugin")
            .arg(scratch.path().join("unused.vst3"))
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap()
    };
    let first = invoke();
    assert!(!first.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains("opaque VST2 FXB bank"));
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 1);

    std::fs::write(&output, b"keep me").unwrap();
    let second = invoke();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("output already exists"));
    assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
    assert_eq!(std::fs::read(&input).unwrap(), b"invalid");
}

#[test]
fn plugin_migration_reports_a_worker_refusal_without_publishing() {
    let scratch = tempfile::tempdir().unwrap();
    let input = scratch.path().join("source.fxb");
    let output = scratch.path().join("result.vstpreset");
    let mut bank = [0; 164];
    bank[..4].copy_from_slice(b"CcnK");
    bank[4..8].copy_from_slice(&156u32.to_be_bytes());
    bank[8..12].copy_from_slice(b"FBCh");
    bank[12..16].copy_from_slice(&2u32.to_be_bytes());
    bank[16..20].copy_from_slice(b"Tst1");
    bank[24..28].copy_from_slice(&1u32.to_be_bytes());
    bank[156..160].copy_from_slice(&4u32.to_be_bytes());
    std::fs::write(&input, bank).unwrap();
    let result = super::ocp()
        .args(["plugin", "migrate-vst2", "--source-name", "Example"])
        .arg(&input)
        .arg("--plugin")
        .arg(scratch.path().join("missing.vst3"))
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("bank's legacy class identity"));
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), bank);
}
