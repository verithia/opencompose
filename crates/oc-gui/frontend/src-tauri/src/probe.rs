//! Out-of-process parameter probe for hosted plugins.
//!
//! Selecting a plugin preset instantiates the module on the webview thread,
//! which freezes the whole window for the plugin's load time (a large
//! instrument takes seconds, and a Qt-based one asserts if it is loaded off the
//! main queue, so it cannot simply move to a worker). The panel only needs the
//! parameter table, so it runs the load in a child process: the parent keeps
//! painting while the child loads, and the live instance is created on the
//! first edit, the first editor open, or when the child fails.
//!
//! The child mode is `--probe-params`: it loads one VST3, applies the preset's
//! saved values, writes the table as JSON, and exits.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Argument that selects the child mode.
pub const MODE_ARG: &str = "--probe-params";

/// How long a probe may take before the panel falls back to an in-process load.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// One probe request, written by the parent and read by the child.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// Plugin bundle to load.
    pub path: PathBuf,
    pub sample_rate: u32,
    pub preset_name: String,
    /// Values the project already saved, applied before the table is read.
    pub params: Vec<(String, oc_dsl::ast::ParamValue)>,
}

/// The child's answer. `ok: false` carries the reason the parent falls back.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub params: Vec<oc_host::clap_instance::PluginParam>,
    pub has_gui: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Child mode: load one plugin, report its parameters, exit.
pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mut request_path = None;
    let mut reply_path = None;
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            MODE_ARG => {}
            "--request" => {
                request_path = Some(PathBuf::from(rest.next().ok_or("--request value")?))
            }
            "--out" => reply_path = Some(PathBuf::from(rest.next().ok_or("--out value")?)),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let request_path = request_path.ok_or("usage: --probe-params --request <json> --out <json>")?;
    let reply_path = reply_path.ok_or("usage: --probe-params --request <json> --out <json>")?;
    let request: Request = serde_json::from_slice(
        &std::fs::read(&request_path)
            .map_err(|error| format!("cannot read probe request: {error}"))?,
    )
    .map_err(|error| format!("cannot parse probe request: {error}"))?;
    let reply = probe(&request);
    std::fs::write(
        &reply_path,
        serde_json::to_vec(&reply).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("cannot write probe reply: {error}"))?;
    Ok(())
}

fn probe(request: &Request) -> Reply {
    let reply = |ok: bool, params, has_gui: bool, error: Option<String>| Reply {
        ok,
        params,
        has_gui,
        error,
    };
    let loaded = oc_host::vst3_instance::Vst3Instance::load_on_main_thread(
        &request.path,
        request.sample_rate,
        512,
    );
    let mut instance = match loaded {
        Ok(instance) => instance,
        Err(error) => return reply(false, Vec::new(), false, Some(error)),
    };
    if let Err(error) =
        oc_engine::render_cache::apply_vst3_preset_params(&mut instance, &request.params)
    {
        return reply(false, Vec::new(), false, Some(error));
    }
    let has_gui = has_editor(&instance);
    reply(
        true,
        crate::commands::plugin_host::vst3_params_for_ui(&instance),
        has_gui,
        None,
    )
}

/// Ask a child process for a preset's parameter table.
///
/// `None` means "no answer": the caller loads the plugin in process, exactly as
/// it did before the probe existed.
pub(crate) fn vst3_params(
    state: &crate::state::AppState,
    preset_name: &str,
) -> Option<(Vec<oc_host::clap_instance::PluginParam>, bool)> {
    let plan = crate::commands::plugin_host::plugin_load_plan(state, preset_name).ok()?;
    if plan.format != oc_host::PluginFormat::Vst3 {
        return None;
    }
    let info = plan.plugin_info.as_ref()?;
    if !Path::new(&info.path).exists() {
        return None;
    }
    let dir = oc_audio::private_fs::config_dir().join("plugin-probe");
    std::fs::create_dir_all(&dir).ok()?;
    let tag = format!("{}-{}", std::process::id(), next_tag());
    let request_path = dir.join(format!("request-{tag}.json"));
    let reply_path = dir.join(format!("reply-{tag}.json"));
    let request = Request {
        path: PathBuf::from(&info.path),
        sample_rate: plan.sample_rate,
        preset_name: plan.preset_name.clone(),
        params: plan.preset.params.clone(),
    };
    std::fs::write(&request_path, serde_json::to_vec(&request).ok()?).ok()?;
    let reply = run_child(&request_path, &reply_path);
    let _ = std::fs::remove_file(&request_path);
    let _ = std::fs::remove_file(&reply_path);
    let reply = reply?;
    if reply.ok {
        Some((reply.params, reply.has_gui))
    } else {
        if let Some(error) = &reply.error {
            log::warn!("[probe] {preset_name}: {error}");
        }
        None
    }
}

/// Whether the loaded plugin offers an editor.
///
/// A VST3 view is only creatable in-process on macOS; elsewhere the editor runs
/// in the helper process, so a loaded plugin has one.
#[cfg(target_os = "macos")]
fn has_editor(instance: &oc_host::vst3_instance::Vst3Instance) -> bool {
    instance.gui_handle().is_some()
}

#[cfg(not(target_os = "macos"))]
fn has_editor(_: &oc_host::vst3_instance::Vst3Instance) -> bool {
    true
}

fn run_child(request_path: &Path, reply_path: &Path) -> Option<Reply> {
    let mut child = Command::new(std::env::current_exe().ok()?)
        .arg(MODE_ARG)
        .arg("--request")
        .arg(request_path)
        .arg("--out")
        .arg(reply_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // A plugin that never finishes loading must not hold the panel.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let bytes = std::fs::read(reply_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn next_tag() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_round_trips_through_json() {
        let reply = Reply {
            ok: false,
            params: Vec::new(),
            has_gui: false,
            error: Some("plugin not found".into()),
        };
        let bytes = serde_json::to_vec(&reply).unwrap();
        let parsed: Reply = serde_json::from_slice(&bytes).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("plugin not found"));
    }

    #[test]
    fn a_missing_reply_file_is_not_an_answer() {
        let missing = std::env::temp_dir().join("oc-probe-missing-reply.json");
        let _ = std::fs::remove_file(&missing);
        assert!(std::fs::read(&missing).is_err());
    }

    #[test]
    fn the_mode_argument_is_the_one_main_dispatches_on() {
        assert_eq!(MODE_ARG, "--probe-params");
    }

    #[test]
    fn a_request_without_paths_is_refused() {
        let args = vec![MODE_ARG.to_string()];
        let error = run_cli(&args).expect_err("a bare probe has nowhere to write");
        assert!(error.contains("--request"), "{error}");
    }

    #[test]
    fn an_unreadable_request_reports_the_file() {
        let args = vec![
            MODE_ARG.to_string(),
            "--request".into(),
            "/nonexistent/probe-request.json".into(),
            "--out".into(),
            "/tmp/probe-reply.json".into(),
        ];
        let error = run_cli(&args).expect_err("a missing request is an error");
        assert!(error.contains("cannot read probe request"), "{error}");
    }
}
