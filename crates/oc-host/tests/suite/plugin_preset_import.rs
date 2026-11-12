//! End-to-end `.ffp` import against installed plugin bundles.
//!
//! These tests skip (pass) when the bundle is absent, so CI without the
//! commercial plug-ins stays green.

use oc_host::plugin_preset;
use oc_host::vst3_instance::Vst3Instance;

fn installed(name: &str) -> std::path::PathBuf {
    let root = std::env::var_os("OCP_PLUGIN_VST3_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    root.join(name)
}

const BINARY_EQ_LOW_CUT: &[u8] = include_bytes!("../fixtures/plugin-preset/binary-eq-lowcut.ffp");
const TEXT_COMP_CLEAN: &[u8] = include_bytes!("../fixtures/plugin-preset/text-comp-clean.ffp");

fn load_installed(path: &std::path::Path) -> Option<Vst3Instance> {
    if !oc_host::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return None;
    }
    if !path.is_dir() {
        eprintln!("skipping: {} is not installed", path.display());
        return None;
    }
    match Vst3Instance::load(path, 48000, 512) {
        Ok(instance) => Some(instance),
        Err(error) => {
            eprintln!("skipping: failed to load {}: {error}", path.display());
            None
        }
    }
}

#[test]
fn imports_binary_low_cut_into_installed_eq() {
    let Some(mut instance) = load_installed(&installed("FabFilter Pro-Q 2.vst3")) else {
        return;
    };
    let preset = plugin_preset::parse_preset(BINARY_EQ_LOW_CUT).expect("fixture parses");
    assert_eq!(preset.plugin, "Pro-Q 2");

    let report =
        plugin_preset::apply_to_instance(&mut instance, &preset).expect("mapping succeeds");
    assert_eq!(report.parsed, 189);
    assert_eq!(report.mapped, 189, "{report:?}");
    assert_eq!(report.applied, 189, "{report:?}");
    assert!(report.unmapped.is_empty(), "{:?}", report.unmapped);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert!(report.mismatched.is_empty(), "{:?}", report.mismatched);

    // A plain 5.6439 formats as ~50 Hz and normalises to 0.201024; the same
    // conversion must happen on import.
    let normalized = instance
        .get_param_by_name("Band 1 Frequency")
        .expect("Band 1 Frequency exists");
    assert!(
        (normalized - 0.201_024).abs() < 5e-4,
        "documented normalised value 0.201024, got {normalized}"
    );
    let plain = instance
        .get_param_plain_by_name("Band 1 Frequency")
        .expect("plain readback");
    assert!(
        (plain - 50f64.log2()).abs() < 1e-3,
        "plain log2(50 Hz) round trip, got {plain}"
    );
    assert_eq!(instance.get_param_plain_by_name("Band 1 State"), Some(1.0));
    assert_eq!(instance.get_param_plain_by_name("Band 1 Shape"), Some(2.0));
    assert_eq!(instance.get_param_plain_by_name("Band 1 Slope"), Some(6.0));
    assert_eq!(instance.get_param_plain_by_name("Band 1 Q"), Some(0.5));
}

#[test]
fn imports_text_clean_into_installed_comp() {
    let Some(mut instance) = load_installed(&installed("FabFilter Pro-C 3.vst3")) else {
        return;
    };
    let preset = plugin_preset::parse_preset(TEXT_COMP_CLEAN).expect("fixture parses");
    assert_eq!(preset.plugin, "Pro-C 3");

    let report =
        plugin_preset::apply_to_instance(&mut instance, &preset).expect("mapping succeeds");
    assert_eq!(report.parsed, 9);
    assert_eq!(report.mapped, 9, "{report:?}");
    assert_eq!(report.applied, 9, "{report:?}");
    assert!(report.unmapped.is_empty(), "{:?}", report.unmapped);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert!(report.mismatched.is_empty(), "{:?}", report.mismatched);

    // Plain values read from the installed `Basic/Clean.ffp`.
    let plain = |name: &str| {
        instance
            .get_param_plain_by_name(name)
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    assert!((plain("Threshold") + 16.0).abs() < 1e-6);
    assert!((plain("Ratio") - 0.560_000_002_384_186).abs() < 1e-4);
    assert!((plain("Knee") - 7.345_943_450_927_73).abs() < 1e-4);
    assert!((plain("Attack") - 0.142_277_777_194_977).abs() < 1e-4);
    assert!((plain("Release") - 0.277_943_134_307_861).abs() < 1e-4);
    assert_eq!(plain("Auto Gain"), 1.0);
}
