//! A plugin module instantiated on a worker thread must not crash the exit.
//!
//! A hosted module registers C++ destructors through `__cxa_atexit`, and
//! `exit()` runs them from `__cxa_finalize_ranges` on the process main thread.
//! A module that builds process-global UI state on its instantiation thread and
//! then tears it down from the main thread can die with SIGSEGV. A host cannot
//! repair a third-party destructor, so
//! [`oc_host::exit_process`] leaves through `_exit` once a module is loaded.
//!
//! The child here reproduces the shape that makes the teardown unsafe: it loads
//! the module from a spawned thread (as a libtest worker does) and then leaves
//! through the production exit path. The parent asserts the exact exit status;
//! a signal fails the test. For affected modules, reverting the exit guard
//! exposes the third-party teardown failure.
//!
//! Third-party plugin: requires `OCP_TEST_PLUGINS=1` and an explicit bundle
//! path in `OCP_TEST_VST3_PLUGIN`. No installed plugins are auto-discovered.

use std::path::PathBuf;

const CHILD_ENV: &str = "OCP_PLUGIN_MODULE_EXIT_CHILD";
const FRAMEWORK_EXIT_ENV: &str = "OCP_PLUGIN_FRAMEWORK_EXIT_CHILD";

fn configured_bundle() -> Option<PathBuf> {
    let path = std::env::var_os("OCP_TEST_VST3_PLUGIN").map(PathBuf::from)?;
    assert!(
        path.exists(),
        "OCP_TEST_VST3_PLUGIN must name an existing VST3 bundle"
    );
    assert!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vst3")),
        "OCP_TEST_VST3_PLUGIN must name a .vst3 bundle"
    );
    Some(path)
}

#[test]
fn plugin_module_teardown_does_not_run_at_exit() {
    if !oc_host::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let Some(bundle) = configured_bundle() else {
        eprintln!("skipping: set OCP_TEST_VST3_PLUGIN to an explicit VST3 bundle");
        return;
    };
    let exe = std::env::current_exe().expect("test executable path");
    let output = std::process::Command::new(exe)
        .args([
            "--nocapture",
            "plugin_module_teardown_does_not_run_at_exit_child",
        ])
        .env(CHILD_ENV, "1")
        .env("OCP_TEST_PLUGINS", "1")
        .env("OCP_TEST_VST3_PLUGIN", &bundle)
        .output()
        .expect("spawn the exit child");
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("plugin-module-exit: ok"),
        "the child stopped before reporting: {:?}",
        output.status
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "the module's teardown must not run at exit: {:?}",
        output.status
    );
}

#[test]
fn plugin_module_teardown_does_not_run_at_exit_child() {
    if !oc_host::external_plugin_tests_enabled() || std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let bundle = configured_bundle().expect("an explicitly selected VST3 bundle");
    std::thread::Builder::new()
        .name("plugin-loader".into())
        .spawn(move || {
            let mut instance =
                oc_host::vst3_instance::Vst3Instance::load(&bundle, 48_000, 512).expect("load");
            let _ = instance.process_block_with_transport(256, None);
            // Returning drops the instance on this thread, as a render does.
        })
        .expect("spawn the loader thread")
        .join()
        .expect("loader thread");
    println!("plugin-module-exit: ok");
    oc_host::exit_process(0);
}

/// The exit path a GUI framework owns.
///
/// Tauri calls `std::process::exit` after its event loop returns, so the host
/// never reaches [`oc_host::exit_process`]; the module's static destructors run
/// from `__cxa_finalize_ranges` instead. `guard_plugin_teardown` registers an
/// `atexit` handler after the module is instantiated, which runs ahead of those
/// destructors and leaves through `_exit`.
#[test]
fn framework_exit_does_not_run_teardown() {
    if !oc_host::external_plugin_tests_enabled() {
        eprintln!("skipping: set OCP_TEST_PLUGINS=1 to host third-party plugins");
        return;
    }
    if std::env::var_os(FRAMEWORK_EXIT_ENV).is_some() {
        return;
    }
    let Some(bundle) = configured_bundle() else {
        eprintln!("skipping: set OCP_TEST_VST3_PLUGIN to an explicit VST3 bundle");
        return;
    };
    let exe = std::env::current_exe().expect("test executable path");
    let output = std::process::Command::new(exe)
        .args(["--nocapture", "framework_exit_child"])
        .env(FRAMEWORK_EXIT_ENV, "1")
        .env("OCP_TEST_PLUGINS", "1")
        .env("OCP_TEST_VST3_PLUGIN", &bundle)
        .output()
        .expect("spawn the framework-exit child");
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("plugin-framework-exit: guard-installed"),
        "the child stopped before installing the guard: {:?}",
        output.status
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "the framework exit must not run the module's teardown: {:?}",
        output.status
    );
}

#[test]
fn framework_exit_child() {
    if !oc_host::external_plugin_tests_enabled() || std::env::var_os(FRAMEWORK_EXIT_ENV).is_none() {
        return;
    }
    let bundle = configured_bundle().expect("an explicitly selected VST3 bundle");
    let _instance =
        oc_host::vst3_instance::Vst3Instance::load(&bundle, 48_000, 512).expect("load the module");
    // Installed after the load, as the GUI does, so it sits ahead of the module's
    // own destructors in the `atexit` list.
    oc_host::guard_plugin_teardown();
    println!("plugin-framework-exit: guard-installed");
    // The framework's exit, not the host's.
    std::process::exit(0);
}
