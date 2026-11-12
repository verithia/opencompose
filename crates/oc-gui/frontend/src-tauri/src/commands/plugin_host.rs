//! Plugin discovery warm-up, instance loading, parameters, gestures, state, and editors.

mod owned_gestures;
pub use owned_gestures::*;

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, State};

use oc_dsl::ast;

use crate::state::AppState;
use crate::state::{UndoEdit, UndoScope, lock_vst3_preview};

use super::plugin_catalog::find_plugin_requirement;
use super::shared::{ProjectSnapshot, resync_with_undo, snapshot};
use super::synth::apply_preset_params;

// ============================================================================
// Plugin auto-loading
// ============================================================================

/// Plugin lookups the render paths resolve for this project.
pub(crate) fn plugin_lookup_requests(
    project: &ast::Project,
) -> Vec<(String, oc_host::PluginFormat)> {
    let mut requests = Vec::new();
    for preset in &project.presets {
        let format = match preset.source.as_deref() {
            Some("clap") => oc_host::PluginFormat::Clap,
            Some("vst3") => oc_host::PluginFormat::Vst3,
            Some("lv2") => oc_host::PluginFormat::Lv2,
            Some("vst2") => oc_host::PluginFormat::Vst2,
            _ => continue,
        };
        let Some(plugin) = preset.plugin.as_deref() else {
            continue;
        };
        if plugin.starts_with("builtin.") {
            continue;
        }
        requests.push((plugin.to_string(), format));
    }
    requests
}

pub(crate) fn project_uses_plugin_effects(project: &ast::Project) -> bool {
    let has_plugin_effect = |effects: &[ast::Effect]| {
        effects
            .iter()
            .any(|effect| matches!(effect.effect_type, ast::EffectType::Plugin(_)))
    };
    project
        .tracks
        .iter()
        .any(|track| has_plugin_effect(&track.effects))
        || project
            .buses
            .iter()
            .any(|bus| has_plugin_effect(&bus.effects))
        || project
            .master
            .as_ref()
            .is_some_and(|master| has_plugin_effect(&master.effects))
}

/// Whether the press path needs the full inventory rather than one lookup:
/// plugin effects are validated against it, CLAP instances are found in it.
pub(crate) fn project_needs_plugin_inventory(project: &ast::Project) -> bool {
    project_uses_plugin_effects(project)
        || project.presets.iter().any(|preset| {
            preset.source.as_deref() == Some("clap")
                && preset
                    .plugin
                    .as_deref()
                    .is_some_and(|plugin| !plugin.starts_with("builtin."))
        })
}

/// Resolve this project's plugin references off the UI thread, so the first
/// transport press does not scan the inventory or each bundle itself.
pub(crate) fn warm_transport_plugins(project: &ast::Project) {
    let requests = plugin_lookup_requests(project);
    let inventory = project_needs_plugin_inventory(project);
    if requests.is_empty() && !inventory {
        return;
    }
    std::thread::spawn(move || {
        if inventory {
            oc_host::warm_plugin_inventory();
        }
        for (plugin, format) in requests {
            let _ = oc_host::find_plugin_info(&plugin, format);
        }
    });
}

/// Ensure all CLAP presets in the project have loaded plugin instances.
/// Called before rendering so tracks actually play through their plugins.
/// Returns list of warning messages for presets that failed to load.
#[cfg(test)]
pub(crate) fn ensure_clap_instances_loaded(inner: &mut crate::state::AppStateInner) -> Vec<String> {
    let mut warnings = Vec::new();

    let presets: Vec<_> = inner
        .project
        .presets
        .iter()
        .filter(|p| p.source.as_deref() == Some("clap"))
        .filter(|p| {
            p.plugin
                .as_deref()
                .map(|id| !id.starts_with("builtin."))
                .unwrap_or(false)
        })
        .filter(|p| !inner.plugin_instances.contains_key(&p.name))
        .map(|p| (p.name.clone(), p.plugin.clone().unwrap_or_default()))
        .collect();

    // Only a CLAP project needs the inventory. Filling it for every project
    // made the first transport press scan all installed bundles.
    if presets.is_empty() {
        return warnings;
    }
    if inner.scanned_plugins.is_empty() {
        inner.scanned_plugins = oc_host::cached_plugins().to_vec();
    }

    for (preset_name, plugin_id) in presets {
        // Find plugin path
        let plugin_info = inner
            .presets
            .find_clap_plugin(&plugin_id)
            .or_else(|| {
                inner
                    .scanned_plugins
                    .iter()
                    .find(|p| p.id == plugin_id || p.name == plugin_id)
                    .cloned()
            })
            .or_else(|| {
                oc_host::cached_plugins()
                    .iter()
                    .find(|p| p.id == plugin_id || p.name == plugin_id)
                    .cloned()
            });

        let Some(info) = plugin_info else {
            let msg = format!(
                "Plugin '{}' not found — preset '{}' will use piano fallback",
                plugin_id, preset_name
            );
            warnings.push(msg);
            continue;
        };

        let path = std::path::Path::new(&info.path);
        match oc_host::clap_instance::ClapInstance::load(
            path,
            &info.id,
            inner.audio.sample_rate,
            512,
        ) {
            Ok(instance) => {
                log::debug!(
                    "[auto-load] Loaded CLAP '{}' for preset '{}'",
                    info.id,
                    preset_name
                );
                inner.plugin_instances.insert(preset_name, instance);
            }
            Err(e) => {
                let msg = format!(
                    "Failed to load '{}' for preset '{}': {}",
                    plugin_id, preset_name, e
                );
                warnings.push(msg);
            }
        }
    }

    warnings
}

pub(crate) fn collect_plugin_render_warnings(project: &ast::Project) -> Vec<String> {
    let mut warnings = Vec::new();
    warnings.extend(oc_host::plugin_preset_render_warnings(project));
    warnings.extend(oc_host::plugin_preset_render_soft_warnings(project));

    let mut check_effects = |owner: &str, effects: &[ast::Effect]| {
        for effect in effects {
            if let ast::EffectType::Plugin(plugin_id) = &effect.effect_type {
                let bare_id = plugin_id
                    .strip_prefix("clap.")
                    .or_else(|| plugin_id.strip_prefix("vst3."))
                    .or_else(|| plugin_id.strip_prefix("vst2."))
                    .unwrap_or(plugin_id);
                let is_renderable_plugin = oc_host::cached_plugins().iter().any(|plugin| {
                    matches!(
                        plugin.format,
                        oc_host::PluginFormat::Clap
                            | oc_host::PluginFormat::Vst3
                            | oc_host::PluginFormat::Vst2
                    ) && (plugin.id == bare_id || plugin.name == bare_id || plugin.path == bare_id)
                });
                if is_renderable_plugin {
                    continue;
                }
                warnings.push(format!(
                    "Plugin effect '{}' on {} was not found as a renderable CLAP, VST2 or VST3 plugin",
                    plugin_id, owner
                ));
            }
        }
    };

    for track in &project.tracks {
        check_effects(&format!("track '{}'", track.name), &track.effects);
    }
    for bus in &project.buses {
        check_effects(&format!("bus '{}'", bus.name), &bus.effects);
    }
    if let Some(master) = &project.master {
        check_effects("master", &master.effects);
    }

    warnings
}

/// Prepare editor-side CLAP owners without running native code under AppState.
pub(crate) fn ensure_clap_instances_ready(state: &AppState) -> Vec<String> {
    let missing: Vec<_> = {
        let inner = state.lock_inner();
        inner
            .project
            .presets
            .iter()
            .filter(|preset| {
                preset.source.as_deref() == Some("clap")
                    && preset
                        .plugin
                        .as_deref()
                        .is_some_and(|id| !id.starts_with("builtin."))
                    && !inner.plugin_instances.contains_key(&preset.name)
            })
            .map(|preset| preset.name.clone())
            .collect()
    };
    missing
        .into_iter()
        .filter_map(|name| {
            load_plugin_for_preset_blocking(state, &name)
                .err()
                .map(|error| format!("Failed to prepare CLAP preset '{name}': {error}"))
        })
        .collect()
}

/// The project and native editor state are captured in separate phases. Callers
/// verify the identity before publishing a transport start or render snapshot.
pub(crate) struct ClapPlaybackPreparation {
    pub(crate) project: ast::Project,
    pub(crate) revision: u64,
    pub(crate) content_epoch: u64,
    pub(crate) file_path: Option<std::path::PathBuf>,
    pub(crate) warnings: Vec<String>,
}
impl ClapPlaybackPreparation {
    pub(crate) fn matches(&self, inner: &crate::state::AppStateInner) -> bool {
        inner.revision == self.revision
            && inner.content_epoch == self.content_epoch
            && inner.file_path == self.file_path
    }
}
pub(crate) fn prepare_clap_playback(state: &AppState) -> Result<ClapPlaybackPreparation, String> {
    let warnings = ensure_clap_instances_ready(state);
    let (mut preparation, plans) = {
        let inner = state.lock_inner();
        let plans: Vec<_> = inner
            .plugin_instances
            .iter()
            .map(|(name, instance)| (name.clone(), instance.state_capture_plan()))
            .collect();
        (
            ClapPlaybackPreparation {
                project: inner.project.clone(),
                revision: inner.revision,
                content_epoch: inner.content_epoch,
                file_path: inner.file_path.clone(),
                warnings,
            },
            plans,
        )
    };
    for (name, plan) in plans {
        let state = plan.execute_on_main()?;
        let Some(preset) = preparation
            .project
            .presets
            .iter_mut()
            .find(|preset| preset.name == name)
        else {
            continue;
        };
        let hex: String = state.iter().map(|byte| format!("{byte:02x}")).collect();
        preset.params.retain(|(key, _)| key != "clap_state_hex");
        preset
            .params
            .insert(0, ("clap_state_hex".into(), ast::ParamValue::String(hex)));
    }
    Ok(preparation)
}

/// Inputs needed to instantiate a preset's plugin, captured under the app lock.
pub(crate) struct PluginLoadPlan {
    pub(crate) preset_name: String,
    pub(crate) preset: ast::PresetDef,
    pub(crate) format: oc_host::PluginFormat,
    pub(crate) plugin_id: String,
    pub(crate) plugin_info: Option<oc_host::PluginInfo>,
    pub(crate) sample_rate: u32,
    revision: u64,
    content_epoch: u64,
    file_path: Option<std::path::PathBuf>,
    load_epoch: u64,
}

// Keep format-specific prepared buffers out of the load result's inline storage.
pub(crate) enum LoadedPluginInstance {
    Clap(Box<oc_host::clap_instance::ClapInstance>),
    Vst3(Box<oc_host::vst3_instance::Vst3Instance>),
    Vst2(Box<oc_host::vst2::Vst2Instance>),
    Lv2(Box<oc_host::lv2::Lv2Instance>),
    #[cfg(target_os = "macos")]
    Au(Box<oc_host::au_instance::AuInstance>),
}

pub(crate) struct LoadedPlugin {
    pub(crate) instance: LoadedPluginInstance,
    pub(crate) params: Vec<oc_host::clap_instance::PluginParam>,
}

fn cached_plugin_params(
    inner: &crate::state::AppStateInner,
    preset_name: &str,
) -> Option<Result<Vec<oc_host::clap_instance::PluginParam>, String>> {
    if inner.plugin_instances.contains_key(preset_name) {
        return inner
            .plugin_param_cache
            .get(preset_name)
            .map(|params| Ok(params.to_vec()));
    }
    if let Some(instance) = inner.vst3_plugin_instances.get(preset_name) {
        return Some(Ok(vst3_params_for_ui(&lock_vst3_preview(instance))));
    }
    if let Some(instance) = inner.vst2_plugin_instances.get(preset_name) {
        return Some(Ok(vst2_params_for_ui(instance)));
    }
    if let Some(instance) = inner.lv2_plugin_instances.get(preset_name) {
        return Some(Ok(lv2_params_for_ui(instance)));
    }
    #[cfg(target_os = "macos")]
    if let Some(instance) = inner.au_plugin_instances.get(preset_name) {
        return Some(au_params_for_ui(instance));
    }
    None
}

pub(crate) fn plugin_load_plan(
    state: &AppState,
    preset_name: &str,
) -> Result<PluginLoadPlan, String> {
    let load_epoch = *state
        .project_load_epoch
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let inner = state.lock_inner();
    let preset = inner
        .project
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)
        .cloned()
        .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;

    let source = preset.source.as_deref().unwrap_or("");
    let plugin_id = preset.plugin.clone().unwrap_or_default();
    if !matches!(source, "clap" | "vst3" | "vst2" | "lv2" | "au")
        || plugin_id.starts_with("builtin.")
    {
        return Err(format!(
            "Preset '{}' is not a hosted plugin (source: {}, plugin: {})",
            preset_name, source, plugin_id
        ));
    }
    let format = match source {
        "clap" => oc_host::PluginFormat::Clap,
        "vst2" => oc_host::PluginFormat::Vst2,
        "lv2" => oc_host::PluginFormat::Lv2,
        "au" => oc_host::PluginFormat::Au,
        _ => oc_host::PluginFormat::Vst3,
    };
    let plugin_info = (source == "clap")
        .then(|| inner.presets.find_clap_plugin(&plugin_id))
        .flatten()
        .or_else(|| {
            inner
                .scanned_plugins
                .iter()
                .find(|plugin| {
                    plugin.format == format && (plugin.id == plugin_id || plugin.name == plugin_id)
                })
                .cloned()
        });
    Ok(PluginLoadPlan {
        preset_name: preset_name.to_owned(),
        preset,
        format,
        plugin_id,
        plugin_info,
        sample_rate: inner.audio.sample_rate,
        revision: inner.revision,
        content_epoch: inner.content_epoch,
        file_path: inner.file_path.clone(),
        load_epoch,
    })
}

/// Instantiate the planned plugin; slow and therefore never called under the app lock.
fn instantiate_plugin(plan: &PluginLoadPlan) -> Result<LoadedPlugin, String> {
    // An AudioUnit is addressed by its component name, not by a bundle path.
    #[cfg(target_os = "macos")]
    if plan.format == oc_host::PluginFormat::Au {
        let mut instance =
            oc_host::au_instance::AuInstance::load(&plan.plugin_id, plan.sample_rate, 512)
                .map_err(|error| error.to_string())?;
        let context = oc_host::PluginStateContext::new(
            oc_host::PluginFormat::Au,
            Some(plan.plugin_id.as_str()),
            &plan.preset_name,
        );
        oc_engine::render_cache::apply_au_preset_state_at(
            &mut instance,
            &plan.preset.params,
            &context,
            None,
        )
        .map_err(|error| error.to_string())?;
        oc_engine::render_cache::apply_au_preset_params(&mut instance, &plan.preset.params)?;
        let params = au_params_for_ui(&instance)?;
        return Ok(LoadedPlugin {
            instance: LoadedPluginInstance::Au(Box::new(instance)),
            params,
        });
    }
    let plugin_info = match &plan.plugin_info {
        Some(info) => info.clone(),
        None => oc_host::find_plugin_info(&plan.plugin_id, plan.format).ok_or_else(|| {
            format!(
                "{} plugin '{}' not found on system",
                plan.preset.source.as_deref().unwrap_or(""),
                plan.plugin_id
            )
        })?,
    };
    let path = std::path::Path::new(&plugin_info.path);
    log::debug!(
        "[load_plugin] loading {}: id='{}' path='{}' exists={}",
        plan.preset.source.as_deref().unwrap_or(""),
        plugin_info.id,
        path.display(),
        path.exists()
    );
    if plan.format == oc_host::PluginFormat::Vst2 {
        let mut instance = oc_host::vst2::Vst2Instance::load(path, plan.sample_rate, 512)?;
        let context = oc_host::PluginStateContext::new(
            oc_host::PluginFormat::Vst2,
            Some(plugin_info.id.as_str()),
            &plan.preset_name,
        );
        oc_engine::render_cache::apply_vst2_preset_state_at(
            &mut instance,
            &plan.preset.params,
            &context,
            None,
        )?;
        oc_engine::render_cache::apply_vst2_preset_params(&mut instance, &plan.preset.params)?;
        let params = vst2_params_for_ui(&instance);
        return Ok(LoadedPlugin {
            instance: LoadedPluginInstance::Vst2(Box::new(instance)),
            params,
        });
    }
    if plan.format == oc_host::PluginFormat::Lv2 {
        let mut instance =
            oc_host::lv2::Lv2Instance::load(path, &plugin_info.id, plan.sample_rate, 512)?;
        let context = oc_host::PluginStateContext::new(
            oc_host::PluginFormat::Lv2,
            Some(plugin_info.id.as_str()),
            &plan.preset_name,
        );
        oc_engine::render_cache::apply_lv2_preset_state_at(
            &mut instance,
            &plan.preset.params,
            &context,
            None,
        )?;
        oc_engine::render_cache::apply_lv2_preset_params(&mut instance, &plan.preset.params)?;
        let params = lv2_params_for_ui(&instance);
        return Ok(LoadedPlugin {
            instance: LoadedPluginInstance::Lv2(Box::new(instance)),
            params,
        });
    }
    if plan.format == oc_host::PluginFormat::Clap {
        let path = path.to_path_buf();
        let id = plugin_info.id.clone();
        let sample_rate = plan.sample_rate;
        return oc_host::clap_main::ClapMainExecutor::registered()?
            .dispatch_control(move || {
                let instance =
                    oc_host::clap_instance::ClapInstance::load(&path, &id, sample_rate, 512)?;
                let params = instance.get_params();
                Ok(LoadedPlugin {
                    instance: LoadedPluginInstance::Clap(Box::new(instance)),
                    params,
                })
            })?
            .wait();
    }
    // The host owns the main-thread contract: this dispatches and waits when the
    // caller is on a worker, and runs inline when it is already on the UI thread.
    let mut instance =
        oc_host::vst3_instance::Vst3Instance::load_on_main_thread(path, plan.sample_rate, 512)?;
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Vst3,
        Some(plugin_info.id.as_str()),
        &plan.preset_name,
    );
    oc_engine::render_cache::apply_vst3_preset_state_at(
        &mut instance,
        &plan.preset.params,
        &context,
        None,
    )?;
    oc_engine::render_cache::apply_vst3_preset_params(&mut instance, &plan.preset.params)?;
    // This instance owns the editor: it publishes editor edits into the preset
    // bus and its own processor follows them for state capture.
    instance.bind_preset_edits(&plan.preset_name);
    instance.publish_controller_values();
    let params = vst3_params_for_ui(&instance);
    Ok(LoadedPlugin {
        instance: LoadedPluginInstance::Vst3(Box::new(instance)),
        params,
    })
}

pub(crate) fn load_plugin_for_preset_with<F>(
    state: &AppState,
    preset_name: &str,
    instantiate: F,
) -> Result<Vec<oc_host::clap_instance::PluginParam>, String>
where
    F: FnOnce(&PluginLoadPlan) -> Result<LoadedPlugin, String>,
{
    let cached = {
        let inner = state.lock_inner();
        cached_plugin_params(&inner, preset_name)
    };
    if let Some(params) = cached {
        let params = params?;
        let mut inner = state.lock_inner();
        cache_vst3_params(&mut inner, preset_name, &params);
        return Ok(params);
    }
    // One instantiation at a time; concurrent requests for the same preset
    // must not each load a module.
    let _load = if oc_host::clap_main::ClapMainExecutor::registered()
        .is_ok_and(|executor| executor.is_current())
    {
        state
            .try_lock_plugin_load()
            .ok_or("A plugin is already loading; retry after the current load completes")?
    } else {
        state.lock_plugin_load()
    };
    let cached = {
        let inner = state.lock_inner();
        cached_plugin_params(&inner, preset_name)
    };
    if let Some(params) = cached {
        let params = params?;
        let mut inner = state.lock_inner();
        cache_vst3_params(&mut inner, preset_name, &params);
        return Ok(params);
    }
    let plan = plugin_load_plan(state, preset_name)?;
    let LoadedPlugin { instance, params } = instantiate(&plan)?;
    // A module registers its UI state's destructors while it is instantiated,
    // so the exit guard has to be registered again after the load to sit ahead
    // of them in the `atexit` list.
    oc_host::guard_plugin_teardown();

    let load_epoch = state
        .project_load_epoch
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut inner = state.lock_inner();
    if inner.exit_ready
        || *load_epoch != plan.load_epoch
        || inner.revision != plan.revision
        || inner.content_epoch != plan.content_epoch
        || inner.file_path != plan.file_path
        || !inner
            .project
            .presets
            .iter()
            .any(|preset| preset == &plan.preset)
    {
        return Err(format!(
            "Preset '{}' or its document changed while its plugin loaded",
            preset_name
        ));
    }
    let retired_clap = match instance {
        LoadedPluginInstance::Clap(instance) => {
            inner.open_clap_editors.remove(&plan.preset_name);
            inner.plugin_instances.insert(plan.preset_name, *instance)
        }
        LoadedPluginInstance::Vst3(instance) => {
            inner
                .vst3_plugin_instances
                .insert(plan.preset_name, Arc::new(Mutex::new(*instance)));
            None
        }
        LoadedPluginInstance::Vst2(instance) => {
            inner
                .vst2_plugin_instances
                .insert(plan.preset_name, *instance);
            None
        }
        LoadedPluginInstance::Lv2(instance) => {
            inner
                .lv2_plugin_instances
                .insert(plan.preset_name, *instance);
            None
        }
        #[cfg(target_os = "macos")]
        LoadedPluginInstance::Au(instance) => {
            inner
                .au_plugin_instances
                .insert(plan.preset_name, *instance);
            None
        }
    };
    // A load is the one enumeration a panel open costs; later polls reuse it.
    cache_vst3_params(&mut inner, preset_name, &params);
    drop(inner);
    drop(load_epoch);
    drop(retired_clap);
    Ok(params)
}

/// Remember an editor panel's inventory for the next poll.
fn cache_vst3_params(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    params: &[oc_host::clap_instance::PluginParam],
) {
    if inner.vst3_plugin_instances.contains_key(preset_name)
        || inner.plugin_instances.contains_key(preset_name)
    {
        inner.plugin_param_cache.store(preset_name, params.to_vec());
    }
}

/// Instantiate a preset's plugin and cache the instance. Blocking; call it on a worker.
pub(crate) fn load_plugin_for_preset_blocking(
    state: &AppState,
    preset_name: &str,
) -> Result<Vec<oc_host::clap_instance::PluginParam>, String> {
    load_plugin_for_preset_with(state, preset_name, instantiate_plugin)
}

/// Load a preset's plugin on the main thread, keeping callers that only need
/// the instance cached out of the instantiation path.
fn load_preset_on_main(app: &tauri::AppHandle, preset_name: &str) -> Result<(), String> {
    let name = preset_name.to_string();
    on_main(app, move |state| {
        load_plugin_for_preset_blocking(state, &name).map(|_| ())
    })
}

/// Run plugin work that can instantiate a module on the main thread.
///
/// A plugin that builds a Qt application while it is instantiated asserts when
/// it runs off the main dispatch queue, and the assert traps the process
/// instead of returning an error. Anything reachable from a worker thread goes
/// through here; work that is already on the main thread runs inline.
pub(crate) fn on_main<T, F>(app: &tauri::AppHandle, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> Result<T, String> + Send + 'static,
{
    if oc_host::clap_main::ClapMainExecutor::registered()
        .is_ok_and(|executor| executor.is_current())
    {
        return work(&app.state::<AppState>());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let _ = tx.send(work(&state));
    })
    .map_err(|error| format!("Plugin work could not reach the main thread: {error}"))?;
    rx.recv()
        .map_err(|_| "Plugin work ended without a result".to_string())?
}

/// Load a CLAP, VST2 or VST3 plugin instance for a preset. Returns its parameter list.
///
/// The instantiation runs on the **main thread**: a plugin that builds a Qt
/// application while it is instantiated asserts if it is not on the main
/// dispatch queue (some plugins do exactly that), and the failure is a process
/// trap rather than an error a caller could handle. Loading blocks the main
/// thread for the plugin's load time, which the panel shows as a busy strip.
#[tauri::command]
pub async fn load_plugin_for_preset(
    preset_name: String,
    app: tauri::AppHandle,
) -> Result<Vec<oc_host::clap_instance::PluginParam>, String> {
    // The panel only needs the parameter table. A cached table or an already
    // loaded instance answers in one step; anything else probes in a child
    // process, so instantiating a slow instrument does not freeze the window.
    // A failed probe falls back to the in-process load.
    enum Answer {
        Cached(Vec<oc_host::clap_instance::PluginParam>),
        Probe,
        Load,
    }
    let answer = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        match cached_plugin_params(&inner, &preset_name) {
            Some(Ok(params)) => Answer::Cached(params),
            Some(Err(error)) => return Err(error),
            None if !has_plugin_instance(&inner, &preset_name) => Answer::Probe,
            None => Answer::Load,
        }
    };
    match answer {
        Answer::Cached(params) => {
            let state = app.state::<AppState>();
            let mut inner = state.lock_inner();
            cache_vst3_params(&mut inner, &preset_name, &params);
            Ok(params)
        }
        Answer::Load => on_main(&app, move |state| {
            load_plugin_for_preset_blocking(state, &preset_name)
        }),
        Answer::Probe => {
            let handle = app.clone();
            let name = preset_name.clone();
            let probed = crate::commands::shared::run_blocking(move || {
                let state = handle.state::<AppState>();
                Ok(crate::probe::vst3_params(&state, &name))
            })
            .await?;
            match probed {
                Some((params, has_gui)) => {
                    let state = app.state::<AppState>();
                    let mut inner = state.lock_inner();
                    inner.plugin_gui_hints.insert(preset_name.clone(), has_gui);
                    cache_vst3_params(&mut inner, &preset_name, &params);
                    Ok(params)
                }
                None => on_main(&app, move |state| {
                    load_plugin_for_preset_blocking(state, &preset_name)
                }),
            }
        }
    }
}

/// Create the live instance a probe deliberately skipped.
///
/// The parameter table may have come from the out-of-process probe, so a panel
/// edit needs a real instance before its gesture can open. This is the one
/// place that loads it on the main thread; the caller shows its pending state
/// while the plugin's load runs.
#[tauri::command]
pub async fn ensure_plugin_instance(
    preset_name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let loaded = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        has_plugin_instance(&inner, &preset_name)
    };
    if loaded {
        return Ok(());
    }
    on_main(&app, move |state| {
        load_plugin_for_preset_blocking(state, &preset_name).map(|_| ())
    })
}

/// Stereo output pairs a preset's instrument declares (1 = main pair only).
/// Loads the plugin on demand so the strip can offer auxiliary sources.
#[tauri::command]
pub async fn preset_output_pairs(
    preset_name: String,
    load: Option<bool>,
    app: tauri::AppHandle,
) -> Result<usize, String> {
    let is_plugin = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        let Some(preset) = inner
            .project
            .presets
            .iter()
            .find(|preset| preset.name == preset_name)
        else {
            return Ok(1);
        };
        matches!(
            preset.source.as_deref(),
            Some("clap") | Some("vst3") | Some("vst2") | Some("lv2") | Some("au")
        ) && preset
            .plugin
            .as_deref()
            .is_some_and(|id| !id.starts_with("builtin."))
    };
    if !is_plugin {
        return Ok(1);
    }
    let needs_load = load.unwrap_or(true) && {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        !has_plugin_instance(&inner, &preset_name)
    };
    if needs_load {
        load_preset_on_main(&app, &preset_name)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        Ok::<usize, String>(
            inner
                .plugin_instances
                .get(&preset_name)
                .map(|instance| instance.output_count())
                .or_else(|| {
                    inner
                        .vst3_plugin_instances
                        .get(&preset_name)
                        .map(|instance| lock_vst3_preview(instance).output_count())
                })
                .or_else(|| {
                    inner
                        .vst2_plugin_instances
                        .get(&preset_name)
                        .map(|instance| instance.output_count())
                })
                .or_else(|| {
                    inner
                        .lv2_plugin_instances
                        .get(&preset_name)
                        .map(|instance| instance.output_count())
                })
                .unwrap_or(1),
        )
    })
    .await
    .map_err(|error| format!("Plugin output query failed: {error}"))?
}

/// One declared output port of a preset's instrument.
#[derive(serde::Serialize)]
pub struct PluginPortInfo {
    pub channels: u32,
    /// Stereo pair the port's first channel lands on, 0-based.
    pub first_pair: usize,
    pub pairs: usize,
}

fn port_infos(ports: Vec<oc_host::ports::AudioPort>) -> Vec<PluginPortInfo> {
    ports
        .into_iter()
        .map(|port| PluginPortInfo {
            channels: port.channels,
            first_pair: port.first_pair,
            pairs: port.pair_count(),
        })
        .collect()
}

/// Declared output ports a preset's instrument exposes, in declaration order.
/// An unloaded plugin reports one stereo port unless `load` is set.
#[tauri::command]
pub async fn preset_output_ports(
    preset_name: String,
    load: Option<bool>,
    app: tauri::AppHandle,
) -> Result<Vec<PluginPortInfo>, String> {
    let is_plugin = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        let Some(preset) = inner
            .project
            .presets
            .iter()
            .find(|preset| preset.name == preset_name)
        else {
            return Ok(port_infos(vec![oc_host::ports::AudioPort::STEREO]));
        };
        matches!(
            preset.source.as_deref(),
            Some("clap") | Some("vst3") | Some("vst2") | Some("lv2") | Some("au")
        ) && preset
            .plugin
            .as_deref()
            .is_some_and(|id| !id.starts_with("builtin."))
    };
    if !is_plugin {
        return Ok(port_infos(vec![oc_host::ports::AudioPort::STEREO]));
    }
    let needs_load = load.unwrap_or(true) && {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        !has_plugin_instance(&inner, &preset_name)
    };
    if needs_load {
        load_preset_on_main(&app, &preset_name)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        let ports = inner
            .plugin_instances
            .get(&preset_name)
            .map(|instance| instance.output_ports())
            .or_else(|| {
                inner
                    .vst3_plugin_instances
                    .get(&preset_name)
                    .map(|instance| lock_vst3_preview(instance).output_ports())
            })
            .or_else(|| {
                inner
                    .vst2_plugin_instances
                    .get(&preset_name)
                    .map(|instance| instance.output_ports())
            })
            .or_else(|| {
                inner
                    .lv2_plugin_instances
                    .get(&preset_name)
                    .map(|instance| instance.output_ports())
            })
            .unwrap_or_else(|| vec![oc_host::ports::AudioPort::STEREO]);
        Ok::<Vec<PluginPortInfo>, String>(port_infos(ports))
    })
    .await
    .map_err(|error| format!("Plugin port query failed: {error}"))?
}

fn vst2_params_for_ui(
    instance: &oc_host::vst2::Vst2Instance,
) -> Vec<oc_host::clap_instance::PluginParam> {
    instance
        .get_params()
        .into_iter()
        .map(|param| oc_host::clap_instance::PluginParam {
            id: param.index as u32,
            name: param.name,
            module: "VST2".to_string(),
            min: 0.0,
            max: 1.0,
            default: param.default,
            current: param.value,
            display: param.display,
            flags: 0,
        })
        .collect()
}

pub(crate) fn vst3_params_for_ui(
    instance: &oc_host::vst3_instance::Vst3Instance,
) -> Vec<oc_host::clap_instance::PluginParam> {
    instance
        .get_params_with_values()
        .into_iter()
        .map(|(param, current)| oc_host::clap_instance::PluginParam {
            id: param.id,
            name: param.name,
            module: "VST3".to_string(),
            min: 0.0,
            max: 1.0,
            default: param.default,
            current,
            display: format!("{current:.4}"),
            flags: 0,
        })
        .collect()
}

fn lv2_params_for_ui(
    instance: &oc_host::lv2::Lv2Instance,
) -> Vec<oc_host::clap_instance::PluginParam> {
    instance
        .params()
        .into_iter()
        .map(|param| oc_host::clap_instance::PluginParam {
            id: param.port,
            module: param.group.clone().unwrap_or_else(|| "LV2".to_string()),
            display: param_display(param.current, param.unit.as_deref()),
            name: param.name,
            min: param.min,
            max: param.max,
            default: param.default,
            current: param.current,
            flags: 0,
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn au_params_for_ui(
    instance: &oc_host::au_instance::AuInstance,
) -> Result<Vec<oc_host::clap_instance::PluginParam>, String> {
    let surface = instance
        .try_parameter_surface()
        .map_err(|error| error.to_string())?;
    let checked = match surface {
        oc_host::params::ParamSurface::Parameters { parameters } => parameters,
        oc_host::params::ParamSurface::Unavailable { .. } => Vec::new(),
    };
    let metadata = instance.get_params();
    if metadata.len() != checked.len() {
        return Err("The AU parameter inventory changed during refresh".into());
    }
    Ok(metadata
        .into_iter()
        .zip(checked)
        .map(|(param, checked)| oc_host::clap_instance::PluginParam {
            id: param.id,
            module: "AU".to_string(),
            display: checked.display,
            name: param.name,
            min: param.min,
            max: param.max,
            default: param.default,
            current: checked.current,
            flags: 0,
        })
        .collect())
}

/// Engineering value plus its unit, for the parameter read-out.
fn param_display(value: f64, unit: Option<&str>) -> String {
    match unit.map(str::trim).filter(|unit| !unit.is_empty()) {
        Some(unit) => format!("{value:.3} {unit}"),
        None => format!("{value:.3}"),
    }
}

/// Get parameters of an already-loaded plugin instance.
/// If the plugin requested a param flush (e.g. after GUI interaction),
/// performs the flush first so values are up-to-date.
#[tauri::command]
pub fn get_plugin_params(
    preset_name: String,
    state: State<AppState>,
) -> Result<Vec<oc_host::clap_instance::PluginParam>, String> {
    if let Some(params) = vst3_preview_params(&state, &preset_name) {
        return Ok(params);
    }
    let clap = {
        state
            .lock_inner()
            .plugin_instances
            .get(&preset_name)
            .map(|instance| instance.parameter_read_plan())
    };
    if let Some(plan) = clap {
        let params = plan.execute_on_main()?;
        let mut inner = state.lock_inner();
        if !inner
            .plugin_instances
            .get(&preset_name)
            .is_some_and(|instance| plan.is_for(instance))
        {
            return Err("The CLAP parameter owner changed during refresh".into());
        }
        inner.plugin_param_cache.store(&preset_name, params.clone());
        return Ok(params);
    }
    let inner = state.lock_inner();
    if let Some(instance) = inner.vst2_plugin_instances.get(&preset_name) {
        return Ok(vst2_params_for_ui(instance));
    }
    if let Some(instance) = inner.lv2_plugin_instances.get(&preset_name) {
        return Ok(lv2_params_for_ui(instance));
    }
    #[cfg(target_os = "macos")]
    if let Some(instance) = inner.au_plugin_instances.get(&preset_name) {
        return au_params_for_ui(instance);
    }
    if let Some(hint) = crate::device_catalog::builtin_parameter_hint(&inner.project, &preset_name)
    {
        return Err(hint);
    }
    Err(format!("Plugin for '{}' not loaded", preset_name))
}

/// Parameters of an editor preview instance, or `None` when the preset does not
/// hold a VST3 instance.
///
/// The inventory is cached and read again only when the plugin reports a
/// change; the values are refreshed on every call, which costs one controller
/// read per parameter instead of an info record and two string conversions. The
/// preview lock is held while the controller runs, never the document lock.
pub(crate) fn vst3_preview_params(
    state: &AppState,
    preset_name: &str,
) -> Option<Vec<oc_host::clap_instance::PluginParam>> {
    let (preview, cached) = {
        let inner = state.lock_inner();
        let preview = inner.vst3_plugin_instances.get(preset_name)?.clone();
        let cached = inner.plugin_param_cache.get(preset_name).map(<[_]>::to_vec);
        (preview, cached)
    };
    let mut instance = lock_vst3_preview(&preview);
    // A program change or a drag inside the plugin's own editor reports itself
    // through `restartComponent` and the edit bus; re-reading here is what
    // stops the inspector from showing the values from before the change.
    let changed = instance.absorb_editor_edits() > 0
        || !instance.poll_host_events().is_empty()
        || instance.take_document_dirty();
    if let Some(params) = cached
        && !changed
    {
        let ids: Vec<u32> = params.iter().map(|param| param.id).collect();
        let mut params = params;
        for (param, value) in params.iter_mut().zip(instance.param_values(&ids)) {
            if value.is_finite() {
                param.current = value;
                param.display = format!("{value:.4}");
            }
        }
        return Some(params);
    }
    let params = vst3_params_for_ui(&instance);
    drop(instance);
    // Keep the inventory for the next poll, unless the instance was replaced
    // while the controller was read.
    let mut inner = state.lock_inner();
    if inner
        .vst3_plugin_instances
        .get(preset_name)
        .is_some_and(|current| Arc::ptr_eq(current, &preview))
    {
        inner.plugin_param_cache.store(preset_name, params.clone());
    }
    Some(params)
}

/// Apply a `.ffp` preset to a loaded VST3 instance and persist the
/// resulting normalised values on the project preset so renders re-apply them.
#[tauri::command]
pub fn import_ffp_preset(
    preset_name: String,
    path: String,
    state: State<AppState>,
) -> Result<(ProjectSnapshot, oc_host::plugin_preset::PresetImportReport), String> {
    use oc_host::plugin_preset;
    // The file and the plugin work happen outside the document lock: reading a
    // hundred-megabyte preset or waiting on a plugin must not block the
    // transport poll that shares this lock.
    let bytes = std::fs::read(&path).map_err(|error| format!("Failed to read {path}: {error}"))?;
    let preset = plugin_preset::parse_preset(&bytes)
        .map_err(|error| format!("Failed to parse {path}: {error}"))?;
    let preview = {
        let inner = state.lock_inner();
        inner
            .vst3_plugin_instances
            .get(&preset_name)
            .cloned()
            .ok_or_else(|| format!("VST3 plugin for '{preset_name}' is not loaded"))?
    };
    let mut instance = lock_vst3_preview(&preview);
    let report = plugin_preset::apply_to_instance(&mut instance, &preset)
        .map_err(|error| error.to_string())?;
    instance.publish_controller_values();
    let targets: Vec<plugin_preset::PresetTarget> = instance
        .get_param_details()
        .iter()
        .map(plugin_preset::PresetTarget::from_detail)
        .collect();
    let mapping =
        plugin_preset::map_preset(&preset, &targets).map_err(|error| error.to_string())?;
    let mut params = Vec::with_capacity(mapping.mapped.len());
    for param in &mapping.mapped {
        if report.rejected.contains(&param.name) {
            continue;
        }
        if let Some(value) = instance.get_param_by_name(&param.name) {
            params.push((param.name.clone(), Some(format!("{value:.9}"))));
        }
    }
    drop(instance);
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    // The plugin's own inventory is the same; the cached values are not.
    inner.plugin_param_cache.invalidate(&preset_name);
    apply_preset_params(&mut inner.project, &preset_name, params)?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("preset import", UndoScope::Preset),
    )?;
    Ok((snapshot(&inner), report))
}

fn plugin_gesture_key(preset_name: &str, param_name: &str) -> String {
    format!("{preset_name}\0{param_name}")
}

pub(crate) fn plugin_automation_lane_name(param_name: &str) -> String {
    let mut encoded = String::from("pluginparam_");
    for byte in param_name.as_bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) fn upsert_plugin_automation_point(
    project: &mut ast::Project,
    track_index: usize,
    lane_param: &str,
    position: ast::BarBeat,
    normalized: f64,
) -> Result<(), String> {
    let track = project
        .tracks
        .get_mut(track_index)
        .ok_or_else(|| format!("Track index {track_index} out of range"))?;
    let existing = track.automation().position(|lane| lane.param == lane_param);
    let lane = if let Some(index) = existing {
        track
            .automation_lane_mut(index)
            .expect("the position named a lane")
    } else {
        track.push_automation(ast::AutomationLane {
            param: lane_param.to_string(),
            ppq: 0,
            smoothing_ms: 0.0,
            points: Vec::new(),
            ..Default::default()
        });
        let index = track.automation_count() - 1;
        track
            .automation_lane_mut(index)
            .expect("the lane was just appended")
    };
    let point = ast::AutomationPoint {
        position,
        value: ast::ParamValue::Number(normalized.clamp(0.0, 1.0)),
        curve: ast::Curve::Linear,
        tension: None,
    };
    if let Some(existing) = lane
        .points
        .iter_mut()
        .find(|point| point.position == position)
    {
        *existing = point;
    } else {
        lane.points.push(point);
        lane.points.sort_by_key(|point| {
            (
                point.position.bar,
                point.position.beat,
                point.position.subdivision,
            )
        });
    }
    Ok(())
}

/// Begin one explicit host automation transaction. Intermediate points update
/// the live plugin and project, but only `end` commits one undo entry.
#[tauri::command]
pub fn begin_plugin_param_gesture(
    preset_name: String,
    track_index: usize,
    param_name: String,
    state: State<AppState>,
) -> Result<(), String> {
    let mut inner = state.lock_inner();
    begin_plugin_param_gesture_inner(&mut inner, &preset_name, track_index, &param_name)
}

pub(crate) fn begin_plugin_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    track_index: usize,
    param_name: &str,
) -> Result<(), String> {
    inner.refuse_in_preview()?;
    if track_index >= inner.project.tracks.len() {
        return Err(format!("Track index {track_index} out of range"));
    }
    if !has_plugin_instance(inner, preset_name) {
        return Err(format!("Plugin for '{preset_name}' not loaded"));
    }
    owned_gestures::begin_legacy_plugin_session(inner, preset_name, track_index, param_name)
}

/// Append/replace a point in an active automation gesture. `value` uses the
/// plugin's exposed range; the recorded OCP lane stores a normalized value.
#[tauri::command]
pub fn update_plugin_param_gesture(
    preset_name: String,
    param_name: String,
    value: f64,
    position: ast::BarBeat,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    update_plugin_param_gesture_inner(&mut inner, &preset_name, &param_name, value, position)
}

pub(crate) fn update_plugin_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    param_name: &str,
    value: f64,
    position: ast::BarBeat,
) -> Result<ProjectSnapshot, String> {
    owned_gestures::apply_plugin_point(
        inner,
        preset_name,
        param_name,
        value,
        position,
        None,
        set_plugin_parameter,
    )
}

/// Write one gesture value into the live instance of whichever format hosts the
/// preset; returns the value the automation lane stores (0..1).
///
/// CLAP and VST3 report their own ranges, so a CLAP gesture carries the
/// parameter's engineering value while VST3, VST2, LV2 and AU carry 0..1.
fn set_plugin_parameter(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    param_name: &str,
    value: f64,
) -> Result<f64, String> {
    if let Some(instance) = inner.plugin_instances.get_mut(preset_name) {
        return instance.queue_control_parameter(param_name, value);
    }
    if let Some(preview) = inner.vst3_plugin_instances.get(preset_name).cloned() {
        let mut instance = lock_vst3_preview(&preview);
        let normalized = value.clamp(0.0, 1.0);
        if !instance.set_param_by_name(param_name, normalized) {
            return Err(format!("VST3 rejected parameter '{param_name}'"));
        }
        instance.publish_controller_value(param_name);
        return Ok(normalized);
    }
    if let Some(instance) = inner.vst2_plugin_instances.get_mut(preset_name) {
        let normalized = value.clamp(0.0, 1.0);
        if !instance.set_param_by_name(param_name, normalized) {
            return Err(format!("VST2 rejected parameter '{param_name}'"));
        }
        return Ok(normalized);
    }
    if let Some(instance) = inner.lv2_plugin_instances.get_mut(preset_name) {
        let normalized = value.clamp(0.0, 1.0);
        let Some(param) = instance
            .params()
            .into_iter()
            .find(|param| param.name == param_name)
        else {
            return Err(format!("Unknown LV2 parameter '{param_name}'"));
        };
        let actual = oc_host::params::denormalize(normalized, param.min, param.max);
        if !instance.set_param_by_name(param_name, actual) {
            return Err(format!("LV2 rejected parameter '{param_name}'"));
        }
        return Ok(normalized);
    }
    #[cfg(target_os = "macos")]
    if let Some(instance) = inner.au_plugin_instances.get_mut(preset_name) {
        let normalized = value.clamp(0.0, 1.0);
        let Some(param) = instance
            .get_params()
            .into_iter()
            .find(|param| param.name == param_name)
        else {
            return Err(format!("Unknown AU parameter '{param_name}'"));
        };
        let actual = oc_host::params::denormalize(normalized, param.min, param.max);
        if !instance.set_param_by_name(param_name, actual) {
            return Err(format!("AU rejected parameter '{param_name}'"));
        }
        return Ok(normalized);
    }
    Err(format!("Plugin for '{preset_name}' not loaded"))
}

/// Whether any live instance hosts this preset.
fn has_plugin_instance(inner: &crate::state::AppStateInner, preset_name: &str) -> bool {
    let hosted = inner.plugin_instances.contains_key(preset_name)
        || inner.vst3_plugin_instances.contains_key(preset_name)
        || inner.vst2_plugin_instances.contains_key(preset_name)
        || inner.lv2_plugin_instances.contains_key(preset_name);
    #[cfg(target_os = "macos")]
    let hosted = hosted || inner.au_plugin_instances.contains_key(preset_name);
    hosted
}

#[tauri::command]
pub fn end_plugin_param_gesture(
    preset_name: String,
    param_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    end_plugin_param_gesture_inner(&mut inner, &preset_name, &param_name)
}

pub(crate) fn end_plugin_param_gesture_inner(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    param_name: &str,
) -> Result<ProjectSnapshot, String> {
    owned_gestures::finish_plugin_recording(inner, preset_name, param_name, None)?;
    Ok(snapshot(inner))
}

/// Save the current state of a loaded CLAP plugin instance and store the path on the preset.
#[tauri::command]
pub fn save_plugin_state_file(
    preset_name: String,
    path: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    save_plugin_state_file_inner(&state, &preset_name, &path)
}

/// [`save_plugin_state_file`] without the Tauri state wrapper, for callers that
/// already hold `&AppState` (the agent surface).
pub(crate) fn save_plugin_state_file_inner(
    state: &AppState,
    preset_name: &str,
    path: &str,
) -> Result<ProjectSnapshot, String> {
    let (format, plugin_id, bytes) = capture_plugin_state_bytes(state, preset_name)?;
    let mut inner = state.lock_inner();
    write_plugin_state_file_and_update_preset(
        &mut inner,
        preset_name,
        path,
        &format,
        &plugin_id,
        bytes,
    )
}

/// Capture the current CLAP plugin state into the project for repeatable renders without sidecars.
#[tauri::command]
pub fn save_plugin_state_embedded(
    preset_name: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    save_plugin_state_embedded_inner(&state, &preset_name)
}

/// [`save_plugin_state_embedded`] without the Tauri state wrapper.
pub(crate) fn save_plugin_state_embedded_inner(
    state: &AppState,
    preset_name: &str,
) -> Result<ProjectSnapshot, String> {
    let (format, plugin_id, bytes) = capture_plugin_state_bytes(state, preset_name)?;
    let mut inner = state.lock_inner();
    embed_plugin_state_and_update_preset(&mut inner, preset_name, &format, &plugin_id, bytes)
}

/// Apply an inline plugin state value to a preset.
#[tauri::command]
pub fn apply_plugin_state(
    preset_name: String,
    state_hex: String,
    state: State<AppState>,
) -> Result<PluginStateApplied, String> {
    apply_plugin_state_value(&state, &preset_name, &state_hex)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturedPluginStateFormat {
    Clap,
    Vst3,
    Vst2,
    Au,
    Lv2,
}

impl CapturedPluginStateFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Clap => "CLAP",
            Self::Vst3 => "VST3",
            Self::Vst2 => "VST2",
            Self::Au => "AU",
            Self::Lv2 => "LV2",
        }
    }

    fn state_file_param(self) -> &'static str {
        match self {
            Self::Clap => "clap_state_file",
            Self::Vst3 => "vst3_state_file",
            Self::Vst2 => "vst2_state_file",
            Self::Au => "au_state_file",
            Self::Lv2 => "lv2_state_file",
        }
    }

    fn state_hex_param(self) -> &'static str {
        match self {
            Self::Clap => "clap_state_hex",
            Self::Vst3 => "vst3_state_hex",
            Self::Vst2 => "vst2_state_hex",
            Self::Au => "au_state_hex",
            Self::Lv2 => "lv2_state_hex",
        }
    }

    fn host_format(self) -> oc_host::PluginFormat {
        match self {
            Self::Clap => oc_host::PluginFormat::Clap,
            Self::Vst3 => oc_host::PluginFormat::Vst3,
            Self::Vst2 => oc_host::PluginFormat::Vst2,
            Self::Au => oc_host::PluginFormat::Au,
            Self::Lv2 => oc_host::PluginFormat::Lv2,
        }
    }
}

fn capture_plugin_state_bytes(
    state: &AppState,
    preset_name: &str,
) -> Result<(CapturedPluginStateFormat, String, Vec<u8>), String> {
    // Cloned so the live instance can be borrowed mutably for the save sync.
    let preset = {
        let inner = state.lock_inner();
        inner
            .project
            .presets
            .iter()
            .find(|p| p.name == preset_name)
            .cloned()
            .ok_or_else(|| format!("Preset '{}' not found", preset_name))?
    };

    match preset.source.as_deref() {
        Some("clap") => {
            let (plan, id, revision, content_epoch) = {
                let inner = state.lock_inner();
                let instance = inner
                    .plugin_instances
                    .get(preset_name)
                    .ok_or_else(|| format!("Plugin for '{}' not loaded", preset_name))?;
                (
                    instance.state_capture_plan(),
                    instance.plugin_id().to_owned(),
                    inner.revision,
                    inner.content_epoch,
                )
            };
            let bytes = plan.execute_on_main().map_err(|error| {
                format!("failed to capture CLAP state for '{preset_name}': {error}")
            })?;
            let inner = state.lock_inner();
            if inner.revision != revision
                || inner.content_epoch != content_epoch
                || !inner
                    .project
                    .presets
                    .iter()
                    .any(|current| current == &preset)
            {
                return Err("The project changed during CLAP state capture".into());
            }
            Ok((CapturedPluginStateFormat::Clap, id, bytes))
        }
        Some("vst3") => {
            let (preview, sample_rate) = {
                let inner = state.lock_inner();
                (
                    inner.vst3_plugin_instances.get(preset_name).cloned(),
                    inner.audio.sample_rate,
                )
            };
            if let Some(preview) = preview {
                let mut instance = lock_vst3_preview(&preview);
                // A preset save must carry what the editor shows, not the
                // processor state from before the drag.
                instance.absorb_editor_edits();
                instance.sync_processor_from_controller();
                let bytes = instance.state_bytes().map_err(|err| {
                    format!(
                        "failed to capture live VST3 state for '{}': {}",
                        preset_name, err
                    )
                })?;
                let identity = preset
                    .plugin
                    .as_deref()
                    .map(|id| oc_host::plugin_state_identity(id, oc_host::PluginFormat::Vst3))
                    .unwrap_or_default();
                return Ok((CapturedPluginStateFormat::Vst3, identity, bytes));
            }
            let plugin_id = preset
                .plugin
                .as_deref()
                .ok_or_else(|| format!("VST3 preset '{}' has no plugin id", preset_name))?;
            let info = oc_host::find_plugin_info(plugin_id, oc_host::PluginFormat::Vst3)
                .ok_or_else(|| format!("VST3 plugin '{}' not found on system", plugin_id))?;
            // Instantiation hops to the main thread; the app lock must not be
            // held across it, or the two can wait on each other.
            let mut instance = oc_host::vst3_instance::Vst3Instance::load_on_main_thread(
                std::path::Path::new(&info.path),
                sample_rate,
                512,
            )?;
            let context = oc_host::PluginStateContext::new(
                oc_host::PluginFormat::Vst3,
                Some(info.id.as_str()),
                preset_name,
            );
            oc_engine::render_cache::apply_vst3_preset_state_at(
                &mut instance,
                &preset.params,
                &context,
                None,
            )?;
            oc_engine::render_cache::apply_vst3_preset_params(&mut instance, &preset.params)?;
            // A separately hosted editor's edits live on the preset bus; fold
            // them in before the capture so the blob is not the stored preset.
            instance.bind_preset_edits(preset_name);
            instance.absorb_editor_edits();
            instance.sync_processor_from_controller();
            let bytes = instance.state_bytes().map_err(|err| {
                format!(
                    "failed to capture VST3 state for '{}': {}",
                    preset_name, err
                )
            })?;
            Ok((CapturedPluginStateFormat::Vst3, info.id, bytes))
        }
        Some("vst2") => {
            let sample_rate = {
                let mut inner = state.lock_inner();
                if let Some(instance) = inner.vst2_plugin_instances.get_mut(preset_name) {
                    let bytes = instance.state_bytes().map_err(|error| {
                        format!(
                            "failed to capture VST2 state for '{}': {}",
                            preset_name, error
                        )
                    })?;
                    let identity = preset
                        .plugin
                        .as_deref()
                        .map(|id| oc_host::plugin_state_identity(id, oc_host::PluginFormat::Vst2))
                        .unwrap_or_default();
                    return Ok((CapturedPluginStateFormat::Vst2, identity, bytes));
                }
                inner.audio.sample_rate
            };
            let plugin_id = preset
                .plugin
                .as_deref()
                .ok_or_else(|| format!("VST2 preset '{}' has no plugin id", preset_name))?;
            let info = oc_host::find_plugin_info(plugin_id, oc_host::PluginFormat::Vst2)
                .ok_or_else(|| format!("VST2 plugin '{}' not found on system", plugin_id))?;
            let mut instance = oc_host::vst2::Vst2Instance::load(
                std::path::Path::new(&info.path),
                sample_rate,
                512,
            )?;
            let context = oc_host::PluginStateContext::new(
                oc_host::PluginFormat::Vst2,
                Some(info.id.as_str()),
                preset_name,
            );
            oc_engine::render_cache::apply_vst2_preset_state_at(
                &mut instance,
                &preset.params,
                &context,
                None,
            )?;
            oc_engine::render_cache::apply_vst2_preset_params(&mut instance, &preset.params)?;
            let bytes = instance.state_bytes().map_err(|error| {
                format!(
                    "failed to capture VST2 state for '{}': {}",
                    preset_name, error
                )
            })?;
            Ok((CapturedPluginStateFormat::Vst2, info.id, bytes))
        }
        Some("lv2") => capture_lv2_state_bytes(state, &preset, preset_name),
        Some("au") => capture_au_state_bytes(state, &preset, preset_name),
        Some(source) => Err(format!(
            "Preset '{}' is not a CLAP, VST2, VST3, AU or LV2 plugin (source: {})",
            preset_name, source
        )),
        None => Err(format!(
            "Preset '{}' is not a CLAP, VST2, VST3, AU or LV2 plugin",
            preset_name
        )),
    }
}

/// The preset's plugin reference, which is also the identity written into the
/// state envelope; the load path verifies the envelope against it.
fn preset_plugin_reference(preset: &ast::PresetDef, preset_name: &str) -> Result<String, String> {
    preset
        .plugin
        .clone()
        .ok_or_else(|| format!("Preset '{}' has no plugin id", preset_name))
}

/// Capture an LV2 `state:interface` chunk from the live unit, or from a
/// temporary one when the panel never loaded it.
fn capture_lv2_state_bytes(
    state: &AppState,
    preset: &ast::PresetDef,
    preset_name: &str,
) -> Result<(CapturedPluginStateFormat, String, Vec<u8>), String> {
    let identity = preset_plugin_reference(preset, preset_name)?;
    {
        let inner = state.lock_inner();
        if let Some(instance) = inner.lv2_plugin_instances.get(preset_name) {
            let bytes = instance.state_bytes().map_err(|error| {
                format!("failed to capture LV2 state for '{preset_name}': {error}")
            })?;
            return Ok((CapturedPluginStateFormat::Lv2, identity, bytes));
        }
    }
    let plan = plugin_load_plan(state, preset_name)?;
    let LoadedPlugin { instance, .. } = instantiate_plugin(&plan)?;
    let LoadedPluginInstance::Lv2(instance) = instance else {
        return Err(format!(
            "Preset '{}' did not load as an LV2 plugin",
            preset_name
        ));
    };
    let bytes = instance
        .state_bytes()
        .map_err(|error| format!("failed to capture LV2 state for '{preset_name}': {error}"))?;
    Ok((CapturedPluginStateFormat::Lv2, identity, bytes))
}

/// Capture an AudioUnit `ClassInfo` from the live unit, or from a temporary one
/// when the panel never loaded it.
#[cfg(target_os = "macos")]
fn capture_au_state_bytes(
    state: &AppState,
    preset: &ast::PresetDef,
    preset_name: &str,
) -> Result<(CapturedPluginStateFormat, String, Vec<u8>), String> {
    let identity = preset_plugin_reference(preset, preset_name)?;
    {
        let inner = state.lock_inner();
        if let Some(instance) = inner.au_plugin_instances.get(preset_name) {
            let bytes = instance.state_bytes().map_err(|error| {
                format!("failed to capture AU state for '{preset_name}': {error}")
            })?;
            return Ok((CapturedPluginStateFormat::Au, identity, bytes));
        }
    }
    let plan = plugin_load_plan(state, preset_name)?;
    let LoadedPlugin { instance, .. } = instantiate_plugin(&plan)?;
    let LoadedPluginInstance::Au(instance) = instance else {
        return Err(format!(
            "Preset '{}' did not load as an AudioUnit",
            preset_name
        ));
    };
    let bytes = instance
        .state_bytes()
        .map_err(|error| format!("failed to capture AU state for '{preset_name}': {error}"))?;
    Ok((CapturedPluginStateFormat::Au, identity, bytes))
}

#[cfg(not(target_os = "macos"))]
fn capture_au_state_bytes(
    _state: &AppState,
    _preset: &ast::PresetDef,
    preset_name: &str,
) -> Result<(CapturedPluginStateFormat, String, Vec<u8>), String> {
    Err(format!(
        "Preset '{}' is an AU preset, and AudioUnit hosting is only available on macOS",
        preset_name
    ))
}

pub(crate) fn write_plugin_state_file_and_update_preset(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    path: &str,
    format: &CapturedPluginStateFormat,
    plugin_id: &str,
    bytes: Vec<u8>,
) -> Result<ProjectSnapshot, String> {
    let encoded = oc_host::encode_plugin_state(format.host_format(), plugin_id, &bytes);
    std::fs::write(path, encoded).map_err(|err| {
        format!(
            "failed to write {} state file '{}': {}",
            format.label(),
            path,
            err
        )
    })?;

    let old_project = inner.project.clone();
    let mut params = plugin_qualification_params(inner, preset_name);
    params.push((
        format.state_file_param().to_string(),
        Some(path.to_string()),
    ));
    apply_preset_params(&mut inner.project, preset_name, params)?;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("plugin state", UndoScope::PluginState),
    )?;
    Ok(snapshot(inner))
}

pub(crate) fn embed_plugin_state_and_update_preset(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
    format: &CapturedPluginStateFormat,
    plugin_id: &str,
    bytes: Vec<u8>,
) -> Result<ProjectSnapshot, String> {
    let old_project = inner.project.clone();
    let encoded = oc_host::encode_plugin_state(format.host_format(), plugin_id, &bytes);
    let mut params = plugin_qualification_params(inner, preset_name);
    params.extend([
        (
            format.state_hex_param().to_string(),
            Some(encode_hex(&encoded)),
        ),
        (format.state_file_param().to_string(), None),
    ]);
    apply_preset_params(&mut inner.project, preset_name, params)?;
    resync_with_undo(
        inner,
        old_project,
        UndoEdit::new("plugin state", UndoScope::PluginState),
    )?;
    Ok(snapshot(inner))
}

/// Default inline cap for a state value handed to an agent: 256 KiB encoded.
pub(crate) const PLUGIN_STATE_INLINE_BYTES: usize = 256 * 1024;
/// Hard ceiling a caller may raise the inline cap to. The agent HTTP body is
/// 8 MiB; hex is twice the encoded size, so a larger blob cannot be sent back.
pub(crate) const PLUGIN_STATE_INLINE_CEILING: usize = 3 * 1024 * 1024;

/// A plugin state captured as a value an agent can hand back later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStateReadout {
    pub preset: String,
    /// `vst3`, `clap`, `vst2`, `au` or `lv2`; the formats the state commands capture.
    pub format: String,
    /// Plugin identity the envelope names.
    pub plugin: String,
    /// Project parameter `plugins_apply_state` writes (`vst3_state_hex`, ...).
    pub param: String,
    /// Encoded envelope as hex, or null when it exceeds the inline cap.
    pub state_hex: Option<String>,
    /// Full encoded size in bytes; present even when not inlined.
    pub bytes: usize,
    /// SHA-256 of the encoded envelope, for an integrity check on apply.
    pub sha256: String,
    pub limit_bytes: usize,
    pub truncated: bool,
    /// Whether a live instance answered, rather than a temporary one.
    pub loaded: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStateApplied {
    pub preset: String,
    pub format: String,
    pub param: String,
    pub bytes: usize,
    pub sha256: String,
    pub snapshot: ProjectSnapshot,
}

/// Capture a preset's current plugin state as an inline value. Never changes
/// the project; a truncated read returns no fragment.
pub(crate) fn read_plugin_state_value(
    state: &AppState,
    preset_name: &str,
    max_bytes: Option<usize>,
) -> Result<PluginStateReadout, String> {
    let limit = match max_bytes {
        Some(0) => return Err("max_bytes must be at least 1".to_string()),
        Some(value) if value > PLUGIN_STATE_INLINE_CEILING => {
            return Err(format!(
                "max_bytes must be at most {PLUGIN_STATE_INLINE_CEILING} (hex of a larger blob does not fit one call)"
            ));
        }
        Some(value) => value,
        None => PLUGIN_STATE_INLINE_BYTES,
    };
    let loaded = {
        let inner = state.lock_inner();
        #[cfg(target_os = "macos")]
        let au_loaded = inner.au_plugin_instances.contains_key(preset_name);
        #[cfg(not(target_os = "macos"))]
        let au_loaded = false;
        inner.plugin_instances.contains_key(preset_name)
            || inner.vst3_plugin_instances.contains_key(preset_name)
            || inner.vst2_plugin_instances.contains_key(preset_name)
            || inner.lv2_plugin_instances.contains_key(preset_name)
            || au_loaded
    };
    let (captured, plugin_id, bytes) = capture_plugin_state_bytes(state, preset_name)?;
    let encoded = oc_host::encode_plugin_state(captured.host_format(), &plugin_id, &bytes);
    let digest = format!("{:x}", Sha256::digest(&encoded));
    let truncated = encoded.len() > limit;
    let note = if truncated {
        format!(
            "state is {} bytes, over the {} byte inline cap; no fragment is returned. Raise max_bytes (ceiling {}) or save it with plugins_save_state mode=file, then select it with plugins_select_program state_file.",
            encoded.len(),
            limit,
            PLUGIN_STATE_INLINE_CEILING
        )
    } else {
        "hand this state_hex back to plugins_apply_state; the envelope names its format, plugin identity and payload sha256".to_string()
    };
    Ok(PluginStateReadout {
        preset: preset_name.to_string(),
        format: oc_host::programs::format_label(captured.host_format()).to_string(),
        plugin: plugin_id,
        param: captured.state_hex_param().to_string(),
        state_hex: (!truncated).then(|| encode_hex(&encoded)),
        bytes: encoded.len(),
        sha256: digest,
        limit_bytes: limit,
        truncated,
        loaded,
        note,
    })
}

/// Write a state value into the preset's project state. The envelope is
/// verified first: a format, identity, version or sha256 mismatch is refused
/// by `decode_plugin_state` before anything is written.
pub(crate) fn apply_plugin_state_value(
    state: &AppState,
    preset_name: &str,
    state_hex: &str,
) -> Result<PluginStateApplied, String> {
    let plan = plugin_load_plan(state, preset_name)?;
    let encoded = decode_hex(state_hex)?;
    if encoded.is_empty() {
        return Err("state_hex is empty".to_string());
    }
    if encoded.len() > PLUGIN_STATE_INLINE_CEILING {
        return Err(format!(
            "state is {} bytes, over the {} byte inline ceiling; save it to a file and use plugins_select_program state_file",
            encoded.len(),
            PLUGIN_STATE_INLINE_CEILING
        ));
    }
    let identity = oc_host::plugin_state_identity(&plan.plugin_id, plan.format);
    let context =
        oc_host::PluginStateContext::new(plan.format, Some(identity.as_str()), preset_name);
    let payload = oc_host::decode_plugin_state(&encoded, &context)?;
    // Re-wrap so a bare legacy chunk carries this preset's identity.
    let canonical = oc_host::encode_plugin_state(plan.format, &identity, &payload);
    let digest = format!("{:x}", Sha256::digest(&canonical));
    let (file_key, hex_key) = plugin_state_keys(plan.format);
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_preset_params(
        &mut inner.project,
        preset_name,
        vec![
            (hex_key.to_string(), Some(encode_hex(&canonical))),
            (file_key.to_string(), None),
        ],
    )?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("plugin state", UndoScope::PluginState),
    )?;
    Ok(PluginStateApplied {
        preset: preset_name.to_string(),
        format: oc_host::programs::format_label(plan.format).to_string(),
        param: hex_key.to_string(),
        bytes: canonical.len(),
        sha256: digest,
        snapshot: snapshot(&inner),
    })
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    let hex: String = hex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if hex.is_empty() {
        return Ok(Vec::new());
    }
    if !hex.len().is_multiple_of(2) {
        return Err("invalid state hex: odd number of digits".to_string());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16)
            .map_err(|error| format!("invalid state hex at byte {}: {error}", i / 2))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

fn plugin_qualification_params(
    inner: &crate::state::AppStateInner,
    preset_name: &str,
) -> Vec<(String, Option<String>)> {
    let Some(preset) = inner
        .project
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)
    else {
        return Vec::new();
    };
    let Some(plugin_id) = preset.plugin.as_deref() else {
        return Vec::new();
    };
    let Some(plugin) = find_plugin_requirement(plugin_id, &inner.scanned_plugins)
        .or_else(|| find_plugin_requirement(plugin_id, oc_host::cached_plugins()))
    else {
        return Vec::new();
    };
    let architecture = plugin
        .architectures
        .iter()
        .find(|architecture| architecture.as_str() == std::env::consts::ARCH)
        .or_else(|| plugin.architectures.first());
    [
        ("plugin_version", plugin.version.as_deref()),
        ("plugin_architecture", architecture.map(String::as_str)),
        ("plugin_state_abi", plugin.state_abi.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name.to_string(), Some(value.to_string()))))
    .collect()
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Capture the live processor state for a separately hosted editor.
async fn capture_vst3_state(app: &tauri::AppHandle, preset_name: &str) -> Option<Vec<u8>> {
    let handle = app.clone();
    let name = preset_name.to_owned();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let preview = {
            let inner = state.lock_inner();
            inner.vst3_plugin_instances.get(&name).cloned()
        };
        let Some(preview) = preview else {
            let _ = tx.send(None);
            return;
        };
        let mut instance = lock_vst3_preview(&preview);
        instance.absorb_editor_edits();
        instance.sync_processor_from_controller();
        let bytes = instance.state_bytes().ok();
        let _ = tx.send(bytes);
    })
    .ok()?;
    rx.await.ok().flatten()
}

/// Whether this preset's VST3 editor has to run in the helper process.
///
/// macOS hosts an editor in-process unless the containment table gates it.
/// Everywhere else there is no in-process view, so the helper is the editor.
/// `OCP_VST3_EDITOR_OUT_OF_PROCESS` routes every editor through the helper,
/// which keeps the containment path reachable without a plugin that needs it.
fn editor_is_out_of_process(inner: &crate::state::AppStateInner, preset_name: &str) -> bool {
    if !inner.vst3_plugin_instances.contains_key(preset_name) {
        return false;
    }
    if std::env::var_os("OCP_VST3_EDITOR_OUT_OF_PROCESS").is_some() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        inner
            .vst3_plugin_instances
            .get(preset_name)
            .map(|preview| {
                let instance = lock_vst3_preview(preview);
                instance
                    .gui_handle()
                    .is_some_and(|gui| gui.blocked_reason().is_some())
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Open the native GUI window for a loaded CLAP or VST3 plugin.
///
/// VST2 editors are not implemented; a loaded VST2 instance returns an error.
/// GUI operations MUST run on the main thread (macOS Cocoa requirement).
#[tauri::command]
pub async fn open_plugin_gui(
    preset_name: String,
    app: tauri::AppHandle,
) -> Result<(u32, u32), String> {
    if app
        .state::<AppState>()
        .lock_inner()
        .vst2_plugin_instances
        .contains_key(&preset_name)
    {
        return Err("VST2 plugin editors are not supported in this build".into());
    }
    let state = app.state::<AppState>();
    let out_of_process = {
        let inner = state.lock_inner();
        if let Some(helper) = inner.editor_helpers.get(&preset_name)
            && helper.outcome().is_none()
        {
            return Err(format!("Plugin editor '{preset_name}' is already open"));
        }
        editor_is_out_of_process(&inner, &preset_name)
    };
    if out_of_process {
        let plan = plugin_load_plan(&state, &preset_name)?;
        let path = plan
            .plugin_info
            .as_ref()
            .map(|info| info.path.clone())
            .or_else(|| {
                oc_host::find_plugin_info(&plan.plugin_id, plan.format).map(|info| info.path)
            })
            .ok_or_else(|| format!("Plugin '{preset_name}' is not installed"))?;
        {
            let mut inner = state.lock_inner();
            if let Some(previous) = inner.editor_helpers.remove(&preset_name)
                && previous.outcome().is_none()
            {
                inner.editor_helpers.insert(preset_name.clone(), previous);
                return Err(format!("Plugin editor '{preset_name}' is already open"));
            }
        }
        let state_bytes = capture_vst3_state(&app, &preset_name).await;
        let (helper, size) = crate::editor_helper::spawn(
            std::path::Path::new(&path),
            &preset_name,
            state_bytes.as_deref(),
        )?;
        state
            .lock_inner()
            .editor_helpers
            .insert(preset_name.clone(), helper);
        return Ok(size);
    }
    let clap_gui = {
        let inner = state.lock_inner();
        inner
            .plugin_instances
            .get(&preset_name)
            .and_then(|instance| instance.gui_ptrs())
    };
    let handle = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        // Resolve pointers here, not before dispatch: a queued project replacement
        // may have destroyed the old instance in the meantime.
        let state = handle.state::<AppState>();
        let mut inner = state.lock_inner();
        let result = (|| {
            #[cfg(target_os = "macos")]
            if let Some(preview) = inner.vst3_plugin_instances.get(&preset_name).cloned() {
                let editor = {
                    let instance = lock_vst3_preview(&preview);
                    instance.try_gui_handle()?
                };
                if let Some(gui) = editor {
                    let size = gui.open(&preset_name)?;
                    // The editor's instance is not in the playback graph, so
                    // keep it processing or the plugin paints "audio processing
                    // disabled". The worker owns the instance from here on: it
                    // never takes the document lock, and it stops itself when
                    // the operator closes the window.
                    let mut watch = crate::plugin_keepalive::WindowWatch::new(handle.clone(), gui);
                    let refresh_app = handle.clone();
                    let setup = crate::plugin_keepalive::KeepAliveSetup {
                        io: Arc::clone(&inner.audio.audio_io),
                        gate: inner.audio.monitor_gate(),
                        preset: preset_name.clone(),
                        instance: preview,
                        clock: crate::plugin_keepalive::PreviewClock {
                            sample_rate: inner.audio.sample_rate.max(1),
                            tempo: inner.project.properties.tempo,
                        },
                        window_gone: Box::new(move || {
                            watch.poll();
                            watch.gone()
                        }),
                        refresh_clock: Box::new(move |clock| {
                            let state = refresh_app.state::<AppState>();
                            crate::plugin_keepalive::refresh_preview_clock(&state, clock);
                        }),
                    };
                    inner.vst3_editor_keepalives.insert(
                        preset_name.clone(),
                        crate::plugin_keepalive::EditorKeepAlive::spawn(setup),
                    );
                    return Ok(size);
                }
            }
            if inner.open_clap_editors.contains(&preset_name) {
                return Err(format!("Plugin editor '{preset_name}' is already open"));
            }
            let gui = clap_gui
                .ok_or_else(|| format!("Plugin '{preset_name}' is not loaded or has no GUI"))?;
            if !inner
                .plugin_instances
                .get(&preset_name)
                .is_some_and(|instance| gui.is_for(instance))
            {
                return Err("The CLAP editor request belongs to a replaced instance".into());
            }
            drop(inner);
            let size = gui.open()?;
            let mut inner = state.lock_inner();
            if !inner
                .plugin_instances
                .get(&preset_name)
                .is_some_and(|instance| gui.is_for(instance))
            {
                drop(inner);
                let _ = gui.close();
                return Err("The CLAP instance changed while its editor opened".into());
            }
            inner.open_clap_editors.insert(preset_name);
            Ok(size)
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| format!("Failed to dispatch editor open: {e}"))?;
    rx.await
        .map_err(|e| format!("Editor open channel error: {e}"))?
}

/// Close the native GUI window for a loaded CLAP plugin.
#[tauri::command]
pub async fn close_plugin_gui(preset_name: String, app: tauri::AppHandle) -> Result<(), String> {
    let clap_gui = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        inner
            .plugin_instances
            .get(&preset_name)
            .and_then(|instance| instance.gui_ptrs())
    };
    let handle = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let state = handle.state::<AppState>();
        let mut inner = state.lock_inner();
        if let Some(helper) = inner.editor_helpers.remove(&preset_name) {
            drop(inner);
            if helper.outcome().is_none() {
                helper.kill();
            }
            let _ = tx.send(Ok(()));
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(preview) = inner.vst3_plugin_instances.get(&preset_name).cloned() {
            drop(inner);
            let result = lock_vst3_preview(&preview)
                .try_gui_handle()
                .and_then(|gui| gui.map_or(Ok(()), |gui| gui.close()));
            if result.is_ok() {
                state
                    .lock_inner()
                    .vst3_editor_keepalives
                    .remove(&preset_name);
            }
            let _ = tx.send(result);
            return;
        }
        if let Some(gui) = &clap_gui
            && inner
                .plugin_instances
                .get(&preset_name)
                .is_some_and(|instance| gui.is_for(instance))
        {
            inner.open_clap_editors.remove(&preset_name);
        }
        drop(inner);
        let result = clap_gui.map_or(Ok(()), |gui| gui.close());
        let _ = tx.send(result);
    })
    .map_err(|error| format!("Failed to dispatch editor close: {error}"))?;
    rx.await
        .map_err(|error| format!("Main thread channel error: {error}"))?
}

/// Check if a loaded plugin has a GUI.
#[tauri::command]
pub fn plugin_has_gui(preset_name: String, state: State<AppState>) -> Result<bool, String> {
    let inner = state.lock_inner();
    #[cfg(target_os = "macos")]
    if let Some(preview) = inner.vst3_plugin_instances.get(&preset_name) {
        return Ok(lock_vst3_preview(preview).try_gui_handle()?.is_some());
    }
    // Outside macOS a VST3 editor has no in-process view; the helper hosts it.
    #[cfg(not(target_os = "macos"))]
    if inner.vst3_plugin_instances.contains_key(&preset_name) {
        return Ok(true);
    }
    if inner.vst2_plugin_instances.contains_key(&preset_name) {
        return Ok(false);
    }
    if inner.lv2_plugin_instances.contains_key(&preset_name) {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    if inner.au_plugin_instances.contains_key(&preset_name) {
        return Ok(false);
    }
    if let Some(hint) = inner.plugin_gui_hints.get(&preset_name) {
        return Ok(*hint);
    }
    let instance = inner
        .plugin_instances
        .get(&preset_name)
        .ok_or_else(|| format!("Plugin for '{}' not loaded", preset_name))?;
    Ok(instance.has_gui())
}

/// Report and release a helper that has exited.
///
/// A running helper keeps its slot and reports nothing, so the caller answers
/// "still open" and the UI keeps polling. A helper that stopped is removed
/// whether it crashed or closed its window.
pub(crate) fn take_helper_exit(
    inner: &mut crate::state::AppStateInner,
    preset_name: &str,
) -> Option<crate::editor_helper::HelperExit> {
    let exit = inner.editor_helpers.get(preset_name)?.outcome()?;
    inner.editor_helpers.remove(preset_name);
    Some(exit)
}

/// Why the plugin's native editor cannot be hosted, or `None` when it can.
#[tauri::command]
pub fn plugin_editor_status(
    preset_name: String,
    state: State<AppState>,
) -> Result<Option<String>, String> {
    let mut inner = state.lock_inner();
    if let Some(exit) = take_helper_exit(&mut inner, &preset_name) {
        // A window the operator closed is not a failure to report.
        return Ok((!exit.closed_cleanly).then_some(exit.message));
    }
    #[cfg(target_os = "macos")]
    if inner.vst3_plugin_instances.contains_key(&preset_name) {
        // A gated editor is hosted by the helper, which reports its own exit.
        return Ok(None);
    }
    if inner.vst2_plugin_instances.contains_key(&preset_name) {
        return Ok(Some(
            "VST2 plugin editors are not supported in this build".into(),
        ));
    }
    if inner.lv2_plugin_instances.contains_key(&preset_name) {
        return Ok(Some(
            "LV2 plugin editors are not supported in this build".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    if inner.au_plugin_instances.contains_key(&preset_name) {
        return Ok(Some(
            "AudioUnit editors are not supported in this build".into(),
        ));
    }
    if let Some(instance) = inner.plugin_instances.get(&preset_name) {
        return Ok(instance.failure().map(|fault| fault.describe_for("CLAP")));
    }
    let _ = &inner;
    Ok(None)
}

/// Check if the plugin GUI was closed externally (via native X button).
/// Returns true once, then resets the flag.
#[tauri::command]
pub fn poll_plugin_gui_closed(preset_name: String, state: State<AppState>) -> Result<bool, String> {
    plugin_gui_closed(&state, &preset_name)
}

/// Whether the operator closed the editor window from its own title bar.
///
/// AppKit closes a window without telling the host, so this is the only place
/// that notices. It releases the editor exactly as [`close_plugin_gui`] does —
/// the plugin's view is detached, its keep-alive is dropped and the next poll
/// reports the editor closed, so both ways of closing one behave alike.
pub(crate) fn plugin_gui_closed(state: &AppState, preset_name: &str) -> Result<bool, String> {
    {
        let inner = state.lock_inner();
        if let Some(helper) = inner.editor_helpers.get(preset_name) {
            return Ok(helper.outcome().is_some());
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(closed) = vst3_editor_closed(state, preset_name) {
        return Ok(closed);
    }
    let mut inner = state.lock_inner();
    if inner.vst3_plugin_instances.contains_key(preset_name)
        || inner.vst2_plugin_instances.contains_key(preset_name)
    {
        return Ok(false);
    }
    let instance = inner
        .plugin_instances
        .get(preset_name)
        .ok_or_else(|| format!("Plugin for '{}' not loaded", preset_name))?;
    let closed = instance.was_gui_closed();
    if closed {
        inner.open_clap_editors.remove(preset_name);
    }
    Ok(closed)
}

/// Whether the VST3 editor window is gone, releasing the editor when it is.
///
/// `None` when the preset holds no VST3 preview instance. Must run on the
/// AppKit main thread, which is where [`poll_plugin_gui_closed`] runs.
#[cfg(target_os = "macos")]
fn vst3_editor_closed(state: &AppState, preset_name: &str) -> Option<bool> {
    let (preview, editor_open, reported_closed) = {
        let inner = state.lock_inner();
        let preview = inner.vst3_plugin_instances.get(preset_name)?.clone();
        let keepalive = inner.vst3_editor_keepalives.get(preset_name);
        (
            preview,
            keepalive.is_some(),
            keepalive.is_some_and(|keepalive| keepalive.window_closed()),
        )
    };
    if !editor_open {
        // Never opened, or already reported closed.
        return Some(false);
    }
    let gone = reported_closed || {
        let instance = lock_vst3_preview(&preview);
        match instance.gui_handle() {
            Some(gui) => {
                let gone = gui.window_gone();
                if gone {
                    // Detach the view the closed window still holds.
                    let _ = gui.close();
                }
                gone
            }
            None => false,
        }
    };
    if gone {
        state
            .lock_inner()
            .vst3_editor_keepalives
            .remove(preset_name);
        log::info!("[editor-monitor] '{preset_name}' was closed from its window");
    }
    Some(gone)
}

/// Tauri event a plugin's host-level notifications are emitted on.
pub const PLUGIN_HOST_EVENT: &str = "plugin-host-event";

/// Host-level notifications a loaded plugin reported since the last poll.
///
/// A plugin sends these through `IComponentHandler::restartComponent`,
/// `setDirty` and `requestOpenEditor`. The host already re-reads the values, so
/// the UI uses this to refresh its own copy and to mark the project dirty.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PluginHostEvents {
    pub events: Vec<oc_host::vst3_instance::Vst3HostEvent>,
    pub dirty: bool,
    /// Editor name the plugin asked the host to open.
    pub open_editor: Option<String>,
    /// A parameter value or the inventory itself changed.
    pub params_changed: bool,
}

impl PluginHostEvents {
    fn is_empty(&self) -> bool {
        self.events.is_empty() && !self.dirty && self.open_editor.is_none() && !self.params_changed
    }
}

/// Drain a loaded VST3's host-level notifications and forward them to the UI.
///
/// This is the cheap poll the panel runs on a timer: it reads flags and applies
/// queued editor edits, and never enumerates the parameter list. A change it
/// reports invalidates the cached inventory so the next
/// [`get_plugin_params`] — and only that call — reads it again.
#[tauri::command]
pub fn poll_plugin_host_events(
    preset_name: String,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<PluginHostEvents, String> {
    let payload = {
        let preview = {
            let inner = state.lock_inner();
            inner.vst3_plugin_instances.get(&preset_name).cloned()
        };
        let Some(preview) = preview else {
            return Ok(PluginHostEvents::default());
        };
        let mut instance = lock_vst3_preview(&preview);
        let events = instance.poll_host_events();
        let dirty = instance.take_document_dirty();
        let open_editor = instance.take_editor_request();
        let params_changed = instance.absorb_editor_edits() > 0 || !events.is_empty() || dirty;
        PluginHostEvents {
            events,
            dirty,
            open_editor,
            params_changed,
        }
    };
    if payload.params_changed {
        state
            .lock_inner()
            .plugin_param_cache
            .invalidate(&preset_name);
    }
    if !payload.is_empty() {
        use tauri::Emitter;
        // Emitted outside the state lock: a listener must be able to call back.
        let _ = app.emit(PLUGIN_HOST_EVENT, payload.clone());
    }
    Ok(payload)
}

// ============================================================================
// Generated parameter panel
// ============================================================================

/// One preset's generated parameter panel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginParameterPanel {
    pub preset: String,
    /// Preset source: "vst2", "lv2", "au", "clap", "vst3", or an engine name.
    pub format: String,
    pub surface: oc_host::params::ParamSurface,
}

/// Parameter panel for a preset that has no in-app editor.
///
/// Instantiation runs on the blocking pool; the surface is read from the live
/// instance, so the values are the plugin's own.
#[tauri::command]
pub async fn plugin_parameter_panel(
    preset_name: String,
    app: tauri::AppHandle,
) -> Result<PluginParameterPanel, String> {
    on_main(&app, move |state| plugin_panel(state, &preset_name))
}

pub(crate) fn plugin_panel(
    state: &AppState,
    preset_name: &str,
) -> Result<PluginParameterPanel, String> {
    plugin_panel_with(state, preset_name, |plan| {
        enumerate_live_surface(state, plan)
    })
}

/// Build a panel with an injected enumerator; tests use this to stand in a
/// plugin exposing a known parameter list.
pub(crate) fn plugin_panel_with<F>(
    state: &AppState,
    preset_name: &str,
    enumerate: F,
) -> Result<PluginParameterPanel, String>
where
    F: FnOnce(&PluginLoadPlan) -> Result<oc_host::params::ParamSurface, String>,
{
    let preset = {
        let inner = state.lock_inner();
        inner
            .project
            .presets
            .iter()
            .find(|preset| preset.name == preset_name)
            .cloned()
            .ok_or_else(|| format!("Preset '{preset_name}' not found"))?
    };
    let format = preset.source.clone().unwrap_or_default();
    let builtin = preset
        .plugin
        .as_deref()
        .is_some_and(|id| id.starts_with("builtin."));
    let panel = |surface| PluginParameterPanel {
        preset: preset_name.to_owned(),
        format: format.clone(),
        surface,
    };
    if builtin {
        return Ok(panel(oc_host::params::unavailable_for_format(&format)));
    }
    match format.as_str() {
        "vst2" | "lv2" | "au" => {}
        // These keep their own editor surface; the generated panel is not theirs.
        "clap" | "vst3" => return Ok(panel(oc_host::params::unavailable_editor_format(&format))),
        _ => return Ok(panel(oc_host::params::unavailable_for_format(&format))),
    }
    let plan = match plugin_load_plan(state, preset_name) {
        Ok(plan) => plan,
        Err(error) => return Ok(panel(oc_host::params::query_failed(error))),
    };
    let surface = enumerate(&plan).unwrap_or_else(oc_host::params::query_failed);
    Ok(panel(surface))
}

/// Load the preset's plugin when needed, then read its parameter surface.
fn enumerate_live_surface(
    state: &AppState,
    plan: &PluginLoadPlan,
) -> Result<oc_host::params::ParamSurface, String> {
    load_plugin_for_preset_blocking(state, &plan.preset_name)?;
    let inner = state.lock_inner();
    let name = &plan.preset_name;
    if let Some(instance) = inner.lv2_plugin_instances.get(name) {
        return Ok(instance.parameter_surface());
    }
    if let Some(instance) = inner.vst2_plugin_instances.get(name) {
        return Ok(instance.parameter_surface());
    }
    #[cfg(target_os = "macos")]
    if let Some(instance) = inner.au_plugin_instances.get(name) {
        return instance
            .try_parameter_surface()
            .map_err(|error| error.to_string());
    }
    Err(format!("Plugin for '{name}' not loaded"))
}

/// What a VST2-to-VST3 migration would change, and whether it was applied.
#[derive(Debug, serde::Serialize)]
pub struct Vst2ToVst3MigrationReport {
    pub applied: bool,
    pub migrations: Vec<oc_host::plugin_state::Vst2ToVst3Migration>,
    pub skipped: Vec<oc_host::plugin_state::Vst2ToVst3Skip>,
}

/// Explicit opt-in migration: a VST2 preset whose plugin no longer scans moves
/// to the VST3 plugin with the same identity, dropping the incompatible VST2
/// state. A dry run reports the plan without touching the project.
#[tauri::command]
pub fn migrate_vst2_presets_to_vst3(
    dry_run: bool,
    state: State<AppState>,
) -> Result<(ProjectSnapshot, Vst2ToVst3MigrationReport), String> {
    let plugins = oc_host::cached_plugins();
    let vst2: Vec<_> = plugins
        .iter()
        .filter(|plugin| plugin.format == oc_host::PluginFormat::Vst2)
        .cloned()
        .collect();
    let vst3: Vec<_> = plugins
        .iter()
        .filter(|plugin| plugin.format == oc_host::PluginFormat::Vst3)
        .cloned()
        .collect();
    let mut inner = state.lock_inner();
    migrate_vst2_presets_to_vst3_inner(&mut inner, &vst2, &vst3, dry_run)
}

pub(crate) fn migrate_vst2_presets_to_vst3_inner(
    inner: &mut crate::state::AppStateInner,
    installed_vst2: &[oc_host::PluginInfo],
    installed_vst3: &[oc_host::PluginInfo],
    dry_run: bool,
) -> Result<(ProjectSnapshot, Vst2ToVst3MigrationReport), String> {
    let plan = oc_host::plugin_state::plan_vst2_to_vst3_migration(
        &inner.project,
        installed_vst2,
        installed_vst3,
    );
    let report = |applied: bool, plan: oc_host::plugin_state::Vst2ToVst3MigrationPlan| {
        Vst2ToVst3MigrationReport {
            applied,
            migrations: plan.migrations,
            skipped: plan.skipped,
        }
    };
    if dry_run || plan.is_empty() {
        return Ok((snapshot(inner), report(false, plan)));
    }
    let old_project = inner.project.clone();
    let changed = oc_host::plugin_state::apply_vst2_to_vst3_migration(&mut inner.project, &plan)?;
    if changed > 0 {
        resync_with_undo(
            inner,
            old_project,
            UndoEdit::new("VST2 to VST3 plugin migration", UndoScope::PluginState),
        )?;
    }
    let applied = changed > 0;
    Ok((snapshot(inner), report(applied, plan)))
}

// ---------------------------------------------------------------------------
// Programs / patches
// ---------------------------------------------------------------------------

/// What a caller can expect from a preset's format, plus the programs this
/// plugin actually reported. `enumerate` is the capability, not a promise that
/// this particular plugin exposes a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProgramListing {
    pub preset: String,
    pub format: String,
    pub loaded: bool,
    pub enumerate: String,
    pub select: Vec<String>,
    pub note: String,
    pub programs: Vec<oc_host::programs::NativeProgram>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProgramSelection {
    pub preset: String,
    pub format: String,
    /// `native_program_data` or `state_injection`.
    pub mechanism: String,
    pub program: Option<oc_host::programs::NativeProgram>,
    /// Project parameter written by the selection (`vst3_state_hex`, ...).
    pub state_param: String,
    pub state_bytes: usize,
    pub state_file: Option<String>,
    pub snapshot: ProjectSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramCapabilityEntry {
    pub format: String,
    pub enumerate: String,
    pub select: Vec<String>,
    pub note: String,
}

/// The per-format capability table as data. A caller reads this before asking
/// for programs so an empty list is never mistaken for a broken plugin.
#[tauri::command]
pub fn plugin_program_capabilities() -> Vec<ProgramCapabilityEntry> {
    program_capability_table()
}

pub(crate) fn program_capability_table() -> Vec<ProgramCapabilityEntry> {
    use oc_host::programs::{PROGRAM_CAPABILITIES, ProgramEnumeration};
    PROGRAM_CAPABILITIES
        .iter()
        .map(|row| ProgramCapabilityEntry {
            format: oc_host::programs::format_label(row.format).to_string(),
            enumerate: match row.enumerate {
                ProgramEnumeration::Native => "native",
                ProgramEnumeration::None => "none",
            }
            .to_string(),
            select: selection_labels(row.select),
            note: row.note.to_string(),
        })
        .collect()
}

fn selection_labels(selection: &[oc_host::programs::ProgramSelection]) -> Vec<String> {
    use oc_host::programs::ProgramSelection;
    selection
        .iter()
        .map(|mechanism| {
            match mechanism {
                ProgramSelection::NativeProgramData => "native_program_data",
                ProgramSelection::MidiProgramChange => "midi_program_change",
                ProgramSelection::StateInjection => "state_injection",
                ProgramSelection::None => "none",
            }
            .to_string()
        })
        .collect()
}

/// Enumerate a preset's programs without changing the project. A loaded
/// instance is preferred; otherwise a temporary one is built so the agent never
/// has to open an editor first.
/// Program lists are stable for the life of a plugin file, and listing one
/// without a loaded instance costs a full VST3 load. The cache holds the parsed
/// list for a couple of minutes and a selection drops it.
const PROGRAM_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(120);

type ProgramCache = std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, Vec<oc_host::programs::NativeProgram>)>,
>;

fn program_cache() -> &'static ProgramCache {
    static CACHE: std::sync::OnceLock<ProgramCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn program_cache_key(preset_name: &str, plan: &PluginLoadPlan) -> String {
    format!(
        "{}|{}|{}",
        preset_name,
        plan.plugin_id,
        plan.plugin_info
            .as_ref()
            .map(|info| info.path.as_str())
            .unwrap_or(""),
    )
}

fn cached_programs(key: &str) -> Option<Vec<oc_host::programs::NativeProgram>> {
    let cache = program_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let (when, programs) = cache.get(key)?;
    (when.elapsed() < PROGRAM_CACHE_TTL).then(|| programs.clone())
}

fn remember_programs(key: &str, programs: &[oc_host::programs::NativeProgram]) {
    let mut cache = program_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache.insert(
        key.to_string(),
        (std::time::Instant::now(), programs.to_vec()),
    );
}

fn forget_programs(preset_name: &str) {
    let mut cache = program_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache.retain(|key, _| !key.starts_with(&format!("{preset_name}|")));
}

pub(crate) fn plugin_program_listing(
    state: &AppState,
    preset_name: &str,
) -> Result<PluginProgramListing, String> {
    let plan = plugin_load_plan(state, preset_name)?;
    let capability = oc_host::programs::capability(plan.format);
    let mut loaded = false;
    let mut programs = Vec::new();
    let cache_key = program_cache_key(preset_name, &plan);
    if let Some(cached) = cached_programs(&cache_key) {
        programs = cached;
    } else if plan.format == oc_host::PluginFormat::Vst3 {
        {
            let preview = {
                let inner = state.lock_inner();
                inner.vst3_plugin_instances.get(preset_name).cloned()
            };
            if let Some(preview) = preview {
                let mut instance = lock_vst3_preview(&preview);
                instance.absorb_editor_edits();
                programs = instance.list_programs();
                loaded = true;
            }
        }
        if !loaded {
            let instance = temporary_vst3_instance(&plan)?;
            programs = instance.list_programs();
        }
    }
    if !programs.is_empty() {
        remember_programs(&cache_key, &programs);
    }
    Ok(PluginProgramListing {
        preset: preset_name.to_string(),
        format: oc_host::programs::format_label(plan.format).to_string(),
        loaded,
        enumerate: match capability.enumerate {
            oc_host::programs::ProgramEnumeration::Native => "native",
            oc_host::programs::ProgramEnumeration::None => "none",
        }
        .to_string(),
        select: selection_labels(capability.select),
        note: capability.note.to_string(),
        programs,
    })
}

/// Everything the selection command needs; kept separate from the Tauri
/// signature so the agent and the GUI share one validation path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProgramSelectRequest {
    pub index: Option<u32>,
    pub name: Option<String>,
    pub state_file: Option<String>,
    pub mode: Option<String>,
    pub path: Option<String>,
}

enum ProgramStateTarget {
    Embedded,
    File(String),
}

impl ProgramSelectRequest {
    fn target(&self) -> Result<ProgramStateTarget, String> {
        let mode = self.mode.as_deref().unwrap_or("embedded");
        match mode {
            "embedded" => Ok(ProgramStateTarget::Embedded),
            "file" => {
                let path = self
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| "mode 'file' requires a 'path'".to_string())?;
                Ok(ProgramStateTarget::File(path.to_string()))
            }
            other => Err(format!("mode must be 'embedded' or 'file', got '{other}'")),
        }
    }

    fn selected_program<'a>(
        &self,
        programs: &'a [oc_host::programs::NativeProgram],
    ) -> Result<&'a oc_host::programs::NativeProgram, String> {
        if self.index.is_some() && self.name.is_some() {
            return Err("give either 'index' or 'name', not both".to_string());
        }
        if let Some(index) = self.index {
            return programs
                .iter()
                .find(|program| program.index == index)
                .ok_or_else(|| {
                    format!(
                        "program index {index} is out of range ({})",
                        describe_programs(programs)
                    )
                });
        }
        if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return programs
                .iter()
                .find(|program| program.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    format!(
                        "no program named '{name}' ({})",
                        describe_programs(programs)
                    )
                });
        }
        Err("give 'index', 'name' or 'state_file'".to_string())
    }
}

fn describe_programs(programs: &[oc_host::programs::NativeProgram]) -> String {
    if programs.is_empty() {
        return "this plugin reported no programs".to_string();
    }
    let shown: Vec<String> = programs
        .iter()
        .take(20)
        .map(|program| format!("{} {}", program.index, program.name))
        .collect();
    format!("programs: {}", shown.join(", "))
}

fn temporary_vst3_instance(
    plan: &PluginLoadPlan,
) -> Result<oc_host::vst3_instance::Vst3Instance, String> {
    #[cfg(test)]
    {
        let id = plan
            .plugin_id
            .strip_prefix("vst3:")
            .unwrap_or(&plan.plugin_id);
        if let Some(instance) = oc_host::vst3_instance::test_support::fake_instance_for_id(id, 512)
        {
            return Ok(instance);
        }
    }
    let info = plan
        .plugin_info
        .clone()
        .or_else(|| oc_host::find_plugin_info(&plan.plugin_id, oc_host::PluginFormat::Vst3))
        .ok_or_else(|| format!("VST3 plugin '{}' not found on system", plan.plugin_id))?;
    let mut instance = oc_host::vst3_instance::Vst3Instance::load_on_main_thread(
        std::path::Path::new(&info.path),
        plan.sample_rate,
        512,
    )?;
    let context = oc_host::PluginStateContext::new(
        oc_host::PluginFormat::Vst3,
        Some(info.id.as_str()),
        &plan.preset_name,
    );
    oc_engine::render_cache::apply_vst3_preset_state_at(
        &mut instance,
        &plan.preset.params,
        &context,
        None,
    )?;
    oc_engine::render_cache::apply_vst3_preset_params(&mut instance, &plan.preset.params)?;
    Ok(instance)
}

fn select_from_vst3(
    instance: &mut oc_host::vst3_instance::Vst3Instance,
    request: &ProgramSelectRequest,
) -> Result<(oc_host::programs::NativeProgram, Vec<u8>), String> {
    let programs = instance.list_programs();
    let program = request.selected_program(&programs)?.clone();
    let data = instance
        .program_data(program.list_id, program.index)
        .ok_or_else(|| {
            format!(
                "plugin exposes no program data for '{}' (list {}); pass state_file to select it by state",
                program.name, program.list_id
            )
        })?;
    instance
        .set_state_bytes(&data)
        .map_err(|error| format!("failed to apply program '{}': {error}", program.name))?;
    instance.sync_processor_from_controller();
    let state = instance
        .state_bytes()
        .map_err(|error| format!("failed to capture program '{}': {error}", program.name))?;
    Ok((program, state))
}

fn plugin_state_keys(format: oc_host::PluginFormat) -> (&'static str, &'static str) {
    match format {
        oc_host::PluginFormat::Vst3 => ("vst3_state_file", "vst3_state_hex"),
        oc_host::PluginFormat::Clap => ("clap_state_file", "clap_state_hex"),
        oc_host::PluginFormat::Vst2 => ("vst2_state_file", "vst2_state_hex"),
        oc_host::PluginFormat::Au => ("au_state_file", "au_state_hex"),
        oc_host::PluginFormat::Lv2 => ("lv2_state_file", "lv2_state_hex"),
        oc_host::PluginFormat::Internal => ("state_file", "state_hex"),
    }
}

fn inject_program_state_file(
    state: &AppState,
    preset_name: &str,
    format: oc_host::PluginFormat,
    path: &str,
) -> Result<PluginProgramSelection, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("failed to read state file '{path}': {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("state file '{path}' is empty or not a file"));
    }
    let (file_key, hex_key) = plugin_state_keys(format);
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    apply_preset_params(
        &mut inner.project,
        preset_name,
        vec![
            (hex_key.to_string(), None),
            (file_key.to_string(), Some(path.to_string())),
        ],
    )?;
    resync_with_undo(
        &mut inner,
        old_project,
        UndoEdit::new("plugin program", UndoScope::PluginState),
    )?;
    Ok(PluginProgramSelection {
        preset: preset_name.to_string(),
        format: oc_host::programs::format_label(format).to_string(),
        mechanism: "state_injection".to_string(),
        program: None,
        state_param: file_key.to_string(),
        state_bytes: metadata.len() as usize,
        state_file: Some(path.to_string()),
        snapshot: snapshot(&inner),
    })
}

fn select_program_native(
    state: &AppState,
    preset_name: &str,
    plan: &PluginLoadPlan,
    request: &ProgramSelectRequest,
) -> Result<PluginProgramSelection, String> {
    let target = request.target()?;
    let (program, bytes) = {
        let preview = {
            let inner = state.lock_inner();
            inner.vst3_plugin_instances.get(preset_name).cloned()
        };
        if let Some(preview) = preview {
            let mut instance = lock_vst3_preview(&preview);
            select_from_vst3(&mut instance, request)?
        } else {
            let mut instance = temporary_vst3_instance(plan)?;
            select_from_vst3(&mut instance, request)?
        }
    };
    let format = CapturedPluginStateFormat::Vst3;
    let plugin_id = oc_host::plugin_state_identity(&plan.plugin_id, oc_host::PluginFormat::Vst3);
    let mut inner = state.lock_inner();
    let (snapshot, state_param, state_file) = match target {
        ProgramStateTarget::Embedded => {
            let snapshot = embed_plugin_state_and_update_preset(
                &mut inner,
                preset_name,
                &format,
                &plugin_id,
                bytes.clone(),
            )?;
            (snapshot, format.state_hex_param().to_string(), None)
        }
        ProgramStateTarget::File(path) => {
            let snapshot = write_plugin_state_file_and_update_preset(
                &mut inner,
                preset_name,
                &path,
                &format,
                &plugin_id,
                bytes.clone(),
            )?;
            (snapshot, format.state_file_param().to_string(), Some(path))
        }
    };
    Ok(PluginProgramSelection {
        preset: preset_name.to_string(),
        format: oc_host::programs::format_label(plan.format).to_string(),
        mechanism: "native_program_data".to_string(),
        program: Some(program),
        state_param,
        state_bytes: bytes.len(),
        state_file,
        snapshot,
    })
}

/// Select one program and write its state into the project. Native program
/// data is the primary path; `state_file` is the state-injection fallback for
/// formats and plugins that expose no program data.
pub(crate) fn select_plugin_program_request(
    state: &AppState,
    preset_name: &str,
    request: ProgramSelectRequest,
) -> Result<PluginProgramSelection, String> {
    let plan = plugin_load_plan(state, preset_name)?;
    // Selecting a program changes what the loaded instance reports, so the
    // cached list is stale from here on.
    forget_programs(preset_name);
    if let Some(path) = request
        .state_file
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        if request.index.is_some()
            || request.name.is_some()
            || request.mode.is_some()
            || request.path.is_some()
        {
            return Err("give 'state_file' alone, not with index, name, mode or path".to_string());
        }
        return inject_program_state_file(state, preset_name, plan.format, path);
    }
    if plan.format != oc_host::PluginFormat::Vst3 {
        return Err(format!(
            "{} has no native program list; pass state_file to select a program by state",
            oc_host::programs::format_label(plan.format)
        ));
    }
    select_program_native(state, preset_name, &plan, &request)
}

/// Tauri entry point for program enumeration.
#[tauri::command]
pub fn get_plugin_programs(
    preset_name: String,
    state: State<AppState>,
) -> Result<PluginProgramListing, String> {
    plugin_program_listing(&state, &preset_name)
}

/// Tauri entry point for program selection.
#[tauri::command]
pub fn select_plugin_program(
    preset_name: String,
    index: Option<u32>,
    name: Option<String>,
    state_file: Option<String>,
    mode: Option<String>,
    path: Option<String>,
    state: State<AppState>,
) -> Result<PluginProgramSelection, String> {
    select_plugin_program_request(
        &state,
        &preset_name,
        ProgramSelectRequest {
            index,
            name,
            state_file,
            mode,
            path,
        },
    )
}
