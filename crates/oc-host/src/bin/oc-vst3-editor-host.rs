//! Thin wrapper around the out-of-process VST3 editor host.
//!
//! The DAW's own binary also exposes this mode as `--editor-host`, so shipped
//! builds need no extra executable; this binary is the development and
//! command-line entry point.

fn main() {
    // SAFETY: this literal helper-mode main owns the platform UI loop;
    // run_cli retains and services this exact queue before native loading.
    let registration = unsafe {
        oc_host::application_main::ApplicationMainQueue::register_runtime_current(
            oc_host::application_main::MainRuntime::EditorHelper,
        )
    };
    let result = registration
        .map_err(|error| error.to_string())
        .and_then(oc_host::editor_host::run_cli);
    let code = match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            2
        }
    };
    oc_host::exit_process(code);
}
