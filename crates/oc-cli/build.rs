//! Linker settings the `ocp` binary needs on Windows.

fn main() {
    // A PE image reserves 1 MB for the main thread, while ELF and Mach-O reserve
    // 8 MB. `ocp` builds its whole clap command tree before `main` does any
    // work, and that needs more than 1 MB, so match the other platforms.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/STACK:8388608");
    }
}
