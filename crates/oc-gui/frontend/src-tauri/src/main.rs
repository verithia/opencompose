// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = oc_audio::private_fs::initialize_config_root_from_env() {
        eprintln!("Configuration profile: {error}");
        oc_host::exit_process(2);
    }

    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|arg| arg == oc_host::scanner::HELPER_MODE_ARG)
    {
        let code = match oc_host::scanner::run_helper_cli(&args) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                2
            }
        };
        oc_host::exit_process(code);
    }
    if args.iter().any(|arg| arg == oc_gui_lib::probe::MODE_ARG) {
        // A headless probe owns its main thread the way the CLI does: it loads
        // one plugin, writes the parameter table, and leaves. No window loop.
        // SAFETY: this literal child main owns native control preparation.
        let registration = unsafe {
            oc_host::application_main::ApplicationMainQueue::register_runtime_current(
                oc_host::application_main::MainRuntime::CommandLine,
            )
        };
        let code = match registration
            .map_err(|error| error.to_string())
            .and_then(|main_queue| {
                let _offline = oc_host::clap_main::ClapOfflineMainScope::enter()?;
                let result = oc_gui_lib::probe::run_cli(&args);
                let _ = main_queue
                    .executor()
                    .begin_shutdown()
                    .and_then(|_| main_queue.pump());
                result
            }) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                2
            }
        };
        oc_host::exit_process(code);
    }
    if args.iter().any(|arg| arg == oc_host::editor_host::MODE_ARG) {
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
    let diag = oc_gui_lib::parse_diag_options(&args[1..]).0;
    let log_directory = oc_audio::private_fs::explicit_config_dir().map(|root| root.join("logs"));
    if diag.any()
        && let Some(path) = oc_diag::init_with_options(oc_diag::InitOptions {
            program: "opencompose-gui",
            debug: diag.debug,
            perf: diag.perf,
            directory: log_directory.as_deref(),
        })
    {
        eprintln!("diagnostics log: {}", path.display());
    }
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|cwd| oc_gui_lib::parse_startup_path(&args[1..], &cwd));
    match path {
        Ok(path) => {
            oc_gui_lib::run_with_options(path, diag);
            // The event loop is done. Exit without running a hosted module's
            // teardown: it tears down UI state on the wrong thread and dies.
            oc_host::exit_process(0)
        }
        Err(error) => {
            eprintln!("{error}");
            oc_host::exit_process(2)
        }
    }
}
