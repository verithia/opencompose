//! Main-thread preparation of request-owned CLAP render owners.
use super::*;

/// Native state, parameter metadata and declared latency/tail are prepared as
/// one owned operation. The map returns even on plugin rejection, preserving
/// the failed owner's first cause for the control-plane render boundary.
pub(super) fn prepare_clap_render_owners(
    project: &Project,
    instances: &mut HashMap<String, ClapInstance>,
    resource_root: Option<&Path>,
) -> Result<(), String> {
    if instances.is_empty() {
        return Ok(());
    }
    let project = project.clone();
    let root = resource_root.map(Path::to_path_buf);
    let mut owned = std::mem::take(instances);
    let work = move || {
        let result = (|| {
            for preset in project
                .presets
                .iter()
                .filter(|preset| preset.source.as_deref() == Some("clap"))
            {
                let Some(instance) = owned.get_mut(&preset.name) else {
                    continue;
                };
                let plugin_id = instance.plugin_id().to_owned();
                let context =
                    PluginStateContext::new(PluginFormat::Clap, Some(&plugin_id), &preset.name);
                apply_clap_preset_state_at(instance, &preset.params, &context, root.as_deref())?;
                apply_clap_preset_params(instance, &preset.params)?;
                instance.refresh_control_metadata()?;
            }
            Ok(())
        })();
        Ok((owned, result))
    };
    let prepared = match oc_host::clap_main::ClapMainExecutor::registered() {
        Ok(executor) => executor.dispatch_control(work)?.wait()?,
        // Standalone callers retain the direct same-main preparation path.
        Err(_) => work()?,
    };
    *instances = prepared.0;
    prepared.1
}
