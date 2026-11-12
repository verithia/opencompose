//! Coverage of installed plugin factory presets.
//!
//! The presets are read from the VST3 bundles at runtime and are never copied
//! into the repository. Tests skip (pass) when a bundle is absent.

use oc_host::plugin_preset::{self, PresetFormat};
use oc_host::vst3_instance::Vst3Instance;
use std::path::{Path, PathBuf};

struct Product {
    bundle: &'static str,
    signature: &'static str,
    version: u32,
    /// Exact payload count for binary files; ASCII presets are sparse.
    params: Option<usize>,
    /// Upper bound for sparse ASCII presets (all real parameters).
    max_params: usize,
    presets: usize,
    format: PresetFormat,
    /// One vendor needs its resource files next to the host binary.
    loadable: bool,
}

const PRODUCTS: &[Product] = &[
    Product {
        bundle: "FabFilter Pro-Q 2.vst3",
        signature: "FQ2p",
        version: 2,
        params: Some(189),
        max_params: 189,
        presets: 27,
        format: PresetFormat::Binary,
        loadable: true,
    },
    Product {
        bundle: "FabFilter Pro-R.vst3",
        signature: "FPRr",
        version: 4,
        params: Some(85),
        max_params: 85,
        presets: 163,
        format: PresetFormat::Binary,
        loadable: true,
    },
    Product {
        bundle: "FabFilter Pro-L 2.vst3",
        signature: "FL2p",
        version: 2,
        params: Some(31),
        max_params: 31,
        presets: 58,
        format: PresetFormat::Binary,
        loadable: false,
    },
    Product {
        bundle: "FabFilter Pro-MB.vst3",
        signature: "FPMb",
        version: 3,
        params: Some(150),
        max_params: 150,
        presets: 55,
        format: PresetFormat::Binary,
        loadable: true,
    },
    Product {
        bundle: "FabFilter Pro-DS.vst3",
        signature: "FPSr",
        version: 2,
        params: Some(20),
        max_params: 20,
        presets: 9,
        format: PresetFormat::Binary,
        loadable: true,
    },
    Product {
        bundle: "FabFilter Pro-C 3.vst3",
        signature: "FC3p",
        version: 2,
        params: None,
        max_params: 100,
        presets: 122,
        format: PresetFormat::Text,
        loadable: true,
    },
    Product {
        bundle: "FabFilter Pro-Q 4.vst3",
        signature: "FQ4p",
        version: 4,
        params: None,
        max_params: 600,
        presets: 171,
        format: PresetFormat::Text,
        loadable: true,
    },
];

fn factory_root(product: &Product) -> PathBuf {
    let base = std::env::var_os("OCP_PLUGIN_VST3_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    base.join(product.bundle)
        .join("Contents/Resources/FactoryPresets")
}

fn bundle_path(product: &Product) -> PathBuf {
    let base = std::env::var_os("OCP_PLUGIN_VST3_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    base.join(product.bundle)
}

fn collect_presets(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_presets(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "ffp") {
            out.push(path);
        }
    }
}

fn preset_files(product: &Product) -> Option<Vec<PathBuf>> {
    let root = factory_root(product);
    if !root.is_dir() {
        return None;
    }
    let mut files = Vec::new();
    collect_presets(&root, &mut files);
    files.sort();
    Some(files)
}

/// Parse every installed factory preset and check the documented counts.
#[test]
fn parses_every_installed_factory_preset() {
    let mut total = 0;
    let mut installed = Vec::new();
    let mut missing = Vec::new();
    for product in PRODUCTS {
        let Some(files) = preset_files(product) else {
            missing.push(product.signature);
            continue;
        };
        installed.push(product.signature);
        assert_eq!(
            files.len(),
            product.presets,
            "{} factory preset count",
            product.signature
        );
        for file in files {
            let bytes = std::fs::read(&file).expect("read preset");
            let preset = plugin_preset::parse_preset(&bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            assert_eq!(preset.signature, product.signature, "{}", file.display());
            assert_eq!(preset.version, product.version, "{}", file.display());
            assert_eq!(preset.format, product.format, "{}", file.display());
            assert!(!preset.values.is_empty(), "{}", file.display());
            // Binary payload counts are fixed; ASCII presets are sparse (69-100
            // for Pro-C 3, 576-600 for Pro-Q 4) so only bound them.
            match product.params {
                Some(params) => assert_eq!(
                    preset.values.len(),
                    params,
                    "{} parameter count",
                    file.display()
                ),
                None => assert!(
                    preset.values.len() <= product.max_params,
                    "{} has {} parameters",
                    file.display(),
                    preset.values.len()
                ),
            }
            total += 1;
        }
    }
    eprintln!(
        "parsed {total} vendor factory presets across {}/{} installed products; missing {missing:?}",
        installed.len(),
        PRODUCTS.len()
    );
    let expected: usize = PRODUCTS
        .iter()
        .filter(|product| installed.contains(&product.signature))
        .map(|product| product.presets)
        .sum();
    assert_eq!(total, expected);
}

/// Map every factory preset and report legacy keys the current plug-in lacks.
#[test]
fn maps_every_installed_factory_preset() {
    if !oc_host::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    let mut mapped_total = 0;
    let mut value_total = 0;
    let mut unmapped = std::collections::BTreeMap::<String, usize>::new();
    for product in PRODUCTS.iter().filter(|product| product.loadable) {
        let Some(files) = preset_files(product) else {
            continue;
        };
        let path = bundle_path(product);
        let Ok(instance) = Vst3Instance::load(&path, 48000, 512) else {
            eprintln!("skipping mapping for {}: load failed", product.signature);
            continue;
        };
        let targets: Vec<plugin_preset::PresetTarget> = instance
            .get_param_details()
            .iter()
            .map(plugin_preset::PresetTarget::from_detail)
            .collect();
        for file in files {
            let bytes = std::fs::read(&file).expect("read preset");
            let preset = plugin_preset::parse_preset(&bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            let mapping = plugin_preset::map_preset(&preset, &targets)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            assert_eq!(
                mapping.mapped.len() + mapping.unmapped.len(),
                preset.values.len(),
                "{}",
                file.display()
            );
            assert!(!mapping.mapped.is_empty(), "{}", file.display());
            mapped_total += mapping.mapped.len();
            value_total += preset.values.len();
            for name in mapping.unmapped {
                *unmapped.entry(name).or_default() += 1;
            }
        }
    }
    eprintln!(
        "mapped {mapped_total}/{value_total} vendor preset values; legacy unmapped keys: {unmapped:?}"
    );
    assert!(
        mapped_total * 10 >= value_total * 9,
        "mapped {mapped_total}/{value_total}"
    );
}

fn import_vendor(product: &Product, relative: &str, expected: &[(&str, f64)]) {
    if !oc_host::external_plugin_tests_enabled() {
        return;
    }
    let Some(files) = preset_files(product) else {
        return;
    };
    let file = files
        .iter()
        .find(|file| file.ends_with(relative))
        .unwrap_or_else(|| panic!("{relative} not found for {}", product.signature));
    let bytes = std::fs::read(file).expect("read preset");
    let preset = plugin_preset::parse_preset(&bytes).expect("parse preset");
    let mut instance = Vst3Instance::load(&bundle_path(product), 48000, 512)
        .unwrap_or_else(|error| panic!("load {}: {error}", product.signature));
    let report = plugin_preset::apply_to_instance(&mut instance, &preset).expect("import preset");
    assert_eq!(report.parsed, preset.values.len());
    assert_eq!(report.mapped, preset.values.len(), "{report:?}");
    assert_eq!(report.applied, preset.values.len(), "{report:?}");
    assert!(report.unmapped.is_empty(), "{:?}", report.unmapped);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert!(report.mismatched.is_empty(), "{:?}", report.mismatched);
    for (name, value) in expected {
        let plain = instance
            .get_param_plain_by_name(name)
            .unwrap_or_else(|| panic!("{name} missing"));
        assert!(
            (plain - value).abs() < 1e-4 * value.abs().max(1.0),
            "{name}: expected plain {value}, read back {plain}"
        );
    }
}

/// Apply one vendor preset per installed product and read parameters back.
#[test]
fn imports_vendor_factory_presets() {
    if !oc_host::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    import_vendor(
        &PRODUCTS[0],
        "Low Cut.ffp",
        &[
            ("Band 1 Frequency", 50f64.log2()),
            ("Band 1 Q", 0.5),
            ("Band 1 Shape", 2.0),
        ],
    );
    import_vendor(
        &PRODUCTS[1],
        "Medium Hall 1.ffp",
        &[("Space", 0.341_67), ("Mix", 50.0)],
    );
    import_vendor(
        &PRODUCTS[3],
        "1 Full Band.ffp",
        &[
            ("Band 1 Low Crossover", 30f64.log2()),
            ("Band 1 Range", -6.0),
        ],
    );
    import_vendor(
        &PRODUCTS[4],
        "Classic 10k Wide Band.ffp",
        &[("Mode", 1.0), ("Range", 6.0), ("Lookahead", 12.0)],
    );
    import_vendor(
        &PRODUCTS[5],
        "Basic/Clean.ffp",
        &[
            ("Threshold", -16.0),
            ("Ratio", 0.560_000_002_384_186),
            ("Knee", 7.345_943_450_927_73),
        ],
    );
    import_vendor(
        &PRODUCTS[6],
        "Default Setting.ffp",
        &[
            ("Band 1 Frequency", 9.965_784_072_875_98),
            ("Band 1 Q", 0.5),
            ("Band 1 Slope", 2.0),
        ],
    );
}

/// External Q3 file: proves binary signature dispatch for a product that
/// is not installed. The file is not committed; the test skips without it.
#[test]
fn parses_external_q3_preset_when_available() {
    let Some(dir) = std::env::var_os("OCP_TEST_FFP_DIR") else {
        return;
    };
    let path = Path::new(&dir).join("pq3-default.ffp");
    if !path.is_file() {
        return;
    }
    let preset =
        plugin_preset::parse_preset(&std::fs::read(path).expect("read pq3")).expect("parse pq3");
    assert_eq!(preset.format, PresetFormat::Binary);
    assert_eq!(preset.signature, "FQ3p");
    assert_eq!(preset.version, 4);
    assert_eq!(preset.plugin, "Pro-Q 3");
    assert_eq!(preset.values.len(), 334);
}

/// Third-party Q4 file: shows the ASCII path accepts a non-vendor author
/// and the same 576-parameter layout. Not committed; skips without the file.
#[test]
fn parses_third_party_q4_preset_when_available() {
    let Some(dir) = std::env::var_os("OCP_TEST_FFP_DIR") else {
        return;
    };
    let path = Path::new(&dir).join("pq4-aces.ffp");
    if !path.is_file() {
        return;
    }
    let preset =
        plugin_preset::parse_preset(&std::fs::read(path).expect("read pq4")).expect("parse pq4");
    assert_eq!(preset.format, PresetFormat::Text);
    assert_eq!(preset.signature, "FQ4p");
    assert_eq!(preset.author.as_deref(), Some("Fixture Author"));
    assert_eq!(preset.values.len(), 576);
}
