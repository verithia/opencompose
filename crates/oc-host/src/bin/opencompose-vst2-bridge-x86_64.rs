//! Translated worker for plugin discovery and processing.
#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = if args
        .get(1)
        .is_some_and(|arg| arg == oc_host::scanner::HELPER_MODE_ARG)
    {
        oc_host::scanner::run_intel_plugin_helper_cli(&args)
    } else {
        oc_host::vst2_bridge::run_helper().map_err(|error| error.to_string())
    };
    if let Err(error) = &result {
        eprintln!("{error}");
    }
    oc_host::exit_process(i32::from(result.is_err()));
}

#[cfg(not(target_os = "macos"))]
fn main() {
    std::process::exit(1);
}
