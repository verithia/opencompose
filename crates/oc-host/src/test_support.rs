//! Paths to native fixtures built for the current host test target.

/// The optimized CLAP module used by owner and command-line regressions.
/// Cross-compilation does not build a runnable host fixture.
pub fn clap_module_fixture() -> Option<&'static std::path::Path> {
    option_env!("OC_CLAP_MODULE_FIXTURE_PATH").map(std::path::Path::new)
}

/// The LV2 module shared by host and engine integration fixtures.
/// Cross-compilation does not produce a runnable host module.
pub fn lv2_module_fixture() -> Option<&'static std::path::Path> {
    option_env!("OC_LV2_FIXTURE_PATH").map(std::path::Path::new)
}
