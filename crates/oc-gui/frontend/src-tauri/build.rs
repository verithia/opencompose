use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
fn main() {
    println!("cargo:rerun-if-env-changed=OCP_BUILD_REVISION");
    // Track only source identity, never a timestamp that busts every build cache.
    for name in [
        Some("HEAD".to_owned()),
        git(&["symbolic-ref", "-q", "HEAD"]),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let revision = env::var("OCP_BUILD_REVISION")
        .ok()
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .filter(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=OCP_BUILD_REVISION={revision}");
    println!(
        "cargo:rustc-env=OCP_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
    tauri_build::build();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // Tauri's icon/version resource is linked into application binaries.
        // Library test executables also import the v6-only TaskDialogIndirect.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
        // The application already embeds Tauri's manifest in its resource file.
        // Keep that resource as the binary's sole manifest owner.
        println!("cargo:rustc-link-arg-bin=oc-gui=/MANIFEST:NO");
    }
}
