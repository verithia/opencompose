//! Complete UI-owned preparation and rendering of one cached VST2 source group.
use super::*;
use oc_host::vst2::{Vst2OwnedAuthoredPlan, Vst2RuntimePreparationPlan};

pub(crate) struct Vst2GroupRequest<'a> {
    pub plugin: Vst2OwnedAuthoredPlan,
    pub preset: &'a PresetDef,
    pub tracks: &'a [usize],
    pub events: &'a [&'a [&'a MidiEvent]],
    pub performance: Option<&'a [oc_synth::performance::ScheduledCommand]>,
    pub total_frames: usize,
    pub transport: PluginTransport,
    pub pre_roll_seconds: f64,
    pub cancel: Option<&'a crate::render_cancel::CancelToken>,
}
pub(crate) struct Vst2GroupOutput {
    pub buffers: Vec<Vec<f32>>,
    pub latency: u32,
}
pub(crate) type Vst2GroupRenderer =
    for<'a> fn(Vst2GroupRequest<'a>) -> Result<Vst2GroupOutput, String>;

/// Assemble original authored routing/state before the external execution
/// boundary. Cache admission and native execution consume this same plan.
pub(super) fn prepare_plugin(
    definitions: &[PresetDef],
    project: &Project,
    preset: &PresetDef,
    tracks: &[usize],
    sample_rate: u32,
    resource_root: Option<&Path>,
) -> Result<Vst2OwnedAuthoredPlan, String> {
    let first = *tracks.first().ok_or("VST2 source group has no tracks")?;
    let plan = super::vst2_prepare::instrument_request(
        definitions,
        preset,
        &project.tracks[first],
        sample_rate,
        resource_root,
    )?;
    for &track in &tracks[1..] {
        let other = super::vst2_prepare::instrument_request(
            definitions,
            preset,
            &project.tracks[track],
            sample_rate,
            resource_root,
        )?;
        require_shared_initialization(&plan, &other).map_err(|error| {
            format!(
                "VST2 preset '{}' shared source on track '{}': {error}",
                preset.name, project.tracks[track].name
            )
        })?;
    }
    Ok(plan)
}

pub(super) fn render(request: Vst2GroupRequest<'_>) -> Result<Vst2GroupOutput, String> {
    let schedule = super::vst2_runtime::compile(
        request.events,
        request.performance,
        request.total_frames,
        &request.transport,
    )?;
    if crate::render_cancel::stopped(request.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    let mut instance = Vst2RuntimePreparationPlan {
        plugin: request.plugin,
        parameter_names: schedule.names.clone(),
        max_parameter_points: schedule.max_parameter_points,
    }
    .execute_on_ui()
    .map_err(|error| {
        format!(
            "VST2 source preparation for preset '{}': {}",
            request.preset.name,
            crate::processing_failure::vst2_owned(&error, request.cancel)
        )
    })?;
    if crate::render_cancel::stopped(request.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    pre_roll_plugin(request.pre_roll_seconds);
    let latency = instance.snapshot.latency_samples;
    let buffers = super::vst2_runtime::render(
        &mut instance,
        &schedule,
        request.tracks.len(),
        request.total_frames,
        request.transport,
        request.cancel,
    )?;
    Ok(Vst2GroupOutput { buffers, latency })
}
fn require_shared_initialization(
    left: &Vst2OwnedAuthoredPlan,
    right: &Vst2OwnedAuthoredPlan,
) -> Result<(), &'static str> {
    let same_parameters = left.parameters.len() == right.parameters.len()
        && left
            .parameters
            .iter()
            .zip(&right.parameters)
            .all(|(left, right)| {
                left.name == right.name
                    && left.native_value == right.native_value
                    && left.domain == right.domain
                    && left.claim == right.claim
            });
    if left.path != right.path
        || left.sample_rate != right.sample_rate
        || left.max_frames != right.max_frames
        || left.state != right.state
        || !same_parameters
    {
        return Err(
            "shared native state or authored initialization differs; use independent source tracks",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
