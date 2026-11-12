//! Opens and closes the editor of an installed VST3 plugin.
//!
//! `OCP_TEST_PLUGINS=1 OCP_TEST_EDITOR_PLUGIN=<bundle> cargo test -p oc-host --test vst3_editor_probe`
//!
//! Runs as its own binary so AppKit work happens on the process main thread.
//! The editor window is transparent and never covers the desktop.

fn main() {
    #[cfg(target_os = "macos")]
    {
        macos::run();
    }
    #[cfg(not(target_os = "macos"))]
    println!("vst3 editor probe: skipped (macOS only)");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    use oc_host::vst3_instance::Vst3Instance;

    pub fn run() {
        if std::env::var_os("OCP_TEST_PLUGINS").is_none() {
            println!("vst3 editor probe: skipped (set OCP_TEST_PLUGINS=1)");
            return;
        }
        let Some(path) = std::env::var_os("OCP_TEST_EDITOR_PLUGIN").map(PathBuf::from) else {
            println!("vst3 editor probe: skipped (set OCP_TEST_EDITOR_PLUGIN)");
            return;
        };
        let instance = Vst3Instance::load(&path, 48_000, 512).expect("load plugin");
        let gui = instance.gui_handle().expect("editor handle");
        if let Some(reason) = gui.blocked_reason() {
            println!("vst3 editor probe: gated: {reason}");
            return;
        }
        let size = gui.open_hidden("ocp editor probe").expect("open editor");
        assert!(size.0 > 0 && size.1 > 0);
        println!("vst3 editor probe: {size:?}");
        assert!(gui.is_open());
        gui.close().expect("close editor");
        assert!(!gui.is_open());
        drop(gui);
        drop(instance);
        println!("vst3 editor probe: closed");
    }
}
