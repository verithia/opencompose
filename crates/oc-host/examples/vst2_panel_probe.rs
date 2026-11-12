//! Read-only parameter probe for a hosted plugin that has no editor.
//!
//! Loads one VST2 bundle (or, with `--au`, one AudioUnit), prints the panel's view
//! of every parameter, and exits. Nothing is rendered, no window is opened, and no
//! state is written.
//!
//! ```text
//! cargo run -p oc-host --features test-support --example vst2_panel_probe -- \
//!   "/Library/Audio/Plug-Ins/VST/<vendor> <product>.vst"
//! cargo run -p oc-host --features test-support --example vst2_panel_probe -- \
//!   --au "Apple: AUDynamicsProcessor"
//! ```

use std::path::PathBuf;

use oc_host::params::ParamSurface;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "macos")]
    if args.first().map(String::as_str) == Some("--au") {
        let Some(query) = args.get(1) else {
            eprintln!("usage: vst2_panel_probe --au <component name>");
            std::process::exit(2);
        };
        match oc_host::au_instance::AuInstance::load(query, 48_000, 512) {
            Ok(instance) => {
                let params = instance.get_params();
                println!("plugin='{}' params={}", instance.name(), params.len());
                print_surface(instance.parameter_surface());
            }
            Err(error) => {
                eprintln!("load failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let Some(path) = args.first().map(PathBuf::from) else {
        eprintln!("usage: vst2_panel_probe <bundle.vst>");
        std::process::exit(2);
    };
    let instance = match oc_host::vst2::Vst2Instance::load(&path, 48_000, 512) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("load failed: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "plugin='{}' vendor='{}' params={}",
        instance.name(),
        instance.vendor(),
        instance.param_count()
    );
    print_surface(instance.parameter_surface());
}

fn print_surface(surface: ParamSurface) {
    match surface {
        ParamSurface::Parameters { parameters } => {
            println!("surface=parameters count={}", parameters.len());
            for param in parameters {
                println!(
                    "[{:>3}] {:<34} display={:<14} unit={:<6} steps={:<5} normalized={:<8.4} automatable={:<5} bypassing={:<5} group={}",
                    param.index,
                    param.name,
                    if param.display.is_empty() {
                        "-"
                    } else {
                        &param.display
                    },
                    if param.unit.is_empty() {
                        "-"
                    } else {
                        &param.unit
                    },
                    param
                        .steps
                        .map_or("-".to_string(), |steps| steps.to_string()),
                    param.normalized,
                    param.automatable,
                    param.by_passing,
                    param.group.as_deref().unwrap_or("-"),
                );
            }
        }
        ParamSurface::Unavailable { issue } => println!("surface=unavailable issue={issue:?}"),
    }
}
