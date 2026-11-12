use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

// The trim probe interprets a project, renders native instruments and mixes
// their effects. Include that code and its pinned dependencies in the cache
// identity, since development builds can share an application version.
const PROBE_CRATES: &[&str] = &[
    "oc-audio",
    "oc-dsl",
    "oc-dsp",
    "oc-engine",
    "oc-host",
    "oc-synth",
    "oc-synth-core",
    "oc-synth-fx",
    "oc-synth-import",
];

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest path"));
    let crates = manifest.parent().expect("workspace crates");
    let workspace = crates.parent().expect("workspace root");
    let mut fingerprint = DefaultHasher::new();
    for name in ["Cargo.toml", "Cargo.lock"] {
        fingerprint_input(workspace, &workspace.join(name), &mut fingerprint)?;
    }
    for name in PROBE_CRATES {
        let package = crates.join(name);
        for path in [package.join("Cargo.toml"), package.join("src")] {
            fingerprint_input(workspace, &path, &mut fingerprint)?;
        }
    }
    for key in [
        "TARGET",
        "OPT_LEVEL",
        "CARGO_CFG_TARGET_FEATURE",
        "CARGO_ENCODED_RUSTFLAGS",
    ] {
        env::var_os(key).hash(&mut fingerprint);
    }
    let mut features: Vec<_> = env::vars_os()
        .filter(|(key, _)| key.to_string_lossy().starts_with("CARGO_FEATURE_"))
        .collect();
    features.sort_unstable();
    features.hash(&mut fingerprint);
    let rustc = env::var_os("RUSTC").expect("Cargo compiler path");
    let version = Command::new(rustc).arg("-vV").output()?;
    if !version.status.success() {
        return Err(io::Error::other("cannot identify the probe compiler"));
    }
    version.stdout.hash(&mut fingerprint);
    println!(
        "cargo:rustc-env=OC_PRESET_TRIM_BUILD={:016x}",
        fingerprint.finish()
    );
    Ok(())
}

fn fingerprint_input(workspace: &Path, path: &Path, hash: &mut DefaultHasher) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
    path.strip_prefix(workspace)
        .expect("workspace path")
        .hash(hash);
    if path.is_dir() {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_unstable();
        for entry in entries {
            fingerprint_input(workspace, &entry, hash)?;
        }
    } else {
        fs::read(path)?.hash(hash);
    }
    Ok(())
}
