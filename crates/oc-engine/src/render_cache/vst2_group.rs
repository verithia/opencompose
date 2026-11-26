//! Complete UI-owned preparation and rendering of one cached VST2 source group.
use super::*;
use oc_host::vst2::{Vst2OwnedAuthoredPlan, Vst2RuntimePreparationPlan};

pub(crate) struct Vst2GroupRequest<'a> {
    pub plugin: Vst2OwnedAuthoredPlan,
    pub preset: &'a PresetDef,
    pub project: &'a Project,
    pub tracks: &'a [usize],
    pub aux_pairs: &'a [&'a [usize]],
    pub events: &'a [&'a [&'a MidiEvent]],
    pub performance: Option<&'a [oc_synth::performance::ScheduledCommand]>,
    pub total_frames: usize,
    pub transport: PluginTransport,
    pub pre_roll_seconds: f64,
    pub cancel: Option<&'a crate::render_cancel::CancelToken>,
}
pub(crate) struct Vst2GroupOutput {
    pub buffers: Vec<Vec<f32>>,
    pub aux: AuxOutputBuffers,
    pub latency: u32,
}
pub(crate) type Vst2GroupRenderer =
    for<'a> fn(Vst2GroupRequest<'a>) -> Result<Vst2GroupOutput, String>;
pub(super) type Vst2GroupPreparer =
    for<'a> fn(Vst2GroupRequest<'a>) -> Result<super::vst2_batch::PreparedGroup, String>;

struct OutputPlan {
    pairs: Vec<usize>,
    tracks: Vec<usize>,
    aux: Vec<((usize, usize), usize)>,
}

#[derive(Clone, Copy)]
struct OutputRouting<'a> {
    project: &'a Project,
    tracks: &'a [usize],
    aux_pairs: &'a [&'a [usize]],
    preset: &'a str,
}

impl OutputPlan {
    fn new(request: OutputRouting<'_>, ports: &[AudioPort]) -> Result<Self, String> {
        let main_pairs = request
            .tracks
            .iter()
            .map(|&lane| {
                super::helpers::resolve_vst2_output_pair(
                    ports,
                    request.preset,
                    &request.project.tracks[lane],
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let capacity = ports.iter().map(AudioPort::pair_count).sum::<usize>();
        if let Some(pair) = request
            .aux_pairs
            .iter()
            .flat_map(|pairs| *pairs)
            .find(|&&pair| pair >= capacity)
        {
            return Err(format!(
                "VST2 preset '{}' declares {capacity} stereo pair(s); auxiliary output {} was requested",
                request.preset,
                pair + 1,
            ));
        }
        let (pairs, tracks, slots) =
            super::plugin_tracks::plan_output_pairs(&main_pairs, request.aux_pairs);
        let aux = request
            .tracks
            .iter()
            .zip(request.aux_pairs)
            .zip(slots)
            .flat_map(|((&lane, pairs), slots)| {
                pairs
                    .iter()
                    .copied()
                    .zip(slots)
                    .map(move |(pair, slot)| ((lane, pair), slot))
            })
            .collect();
        Ok(Self { pairs, tracks, aux })
    }

    fn assign(self, pcm: Vec<Vec<f32>>, latency: u32) -> Vst2GroupOutput {
        let mut captures = pcm
            .into_iter()
            .map(|buffer| (0, buffer))
            .collect::<Vec<_>>();
        for &slot in self
            .tracks
            .iter()
            .chain(self.aux.iter().map(|(_, slot)| slot))
        {
            captures[slot].0 += 1;
        }
        let mut take = |slot| {
            let (remaining, buffer): &mut (usize, Vec<f32>) = &mut captures[slot];
            *remaining -= 1;
            if *remaining == 0 {
                std::mem::take(buffer)
            } else {
                buffer.clone()
            }
        };
        let buffers = self.tracks.into_iter().map(&mut take).collect();
        let aux = self
            .aux
            .into_iter()
            .map(|(key, slot)| (key, take(slot)))
            .collect();
        Vst2GroupOutput {
            buffers,
            aux,
            latency,
        }
    }
}

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
    let cancel = request.cancel.cloned();
    let prepared = prepare(request)?;
    super::vst2_batch::render(vec![prepared], cancel.as_ref())?
        .pop()
        .ok_or_else(|| "VST2 source group returned no output".into())
}

pub(super) fn prepare(
    request: Vst2GroupRequest<'_>,
) -> Result<super::vst2_batch::PreparedGroup, String> {
    let _timer = oc_diag::perf_counter("cache.vst2.prepare.ms").map(oc_diag::Timer::start);
    let schedule = super::vst2_runtime::compile(
        request.events,
        request.performance,
        request.total_frames,
        &request.transport,
    )?;
    if crate::render_cancel::stopped(request.cancel) {
        return Err(crate::render_cancel::cancelled());
    }
    let routing = OutputRouting {
        project: request.project,
        tracks: request.tracks,
        aux_pairs: request.aux_pairs,
        preset: &request.preset.name,
    };
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if oc_host::vst2_bridge::needs_translation(&request.plugin.path) {
        let mut plugin = super::vst2_prepare::load_translated(&request.plugin)?;
        let output = OutputPlan::new(routing, &plugin.output_ports())?;
        let latency = plugin.latency_samples();
        plugin.begin_pre_roll(request.pre_roll_seconds)?;
        let deadline =
            super::offline_warmup::WarmupDeadline::after_preparation(request.pre_roll_seconds)?;
        let frames = request.total_frames;
        let transport = request.transport;
        return Ok(super::vst2_batch::PreparedGroup::new(
            deadline,
            Box::new(move |cancel| {
                let buffers = super::vst2_runtime::render_translated(
                    &mut plugin,
                    &schedule,
                    &output.pairs,
                    frames,
                    transport,
                    Some(cancel),
                )?;
                Ok(output.assign(buffers, latency))
            }),
        ));
    }
    // A prior quarantined CLOSE must stop admission before another native
    // program can grow the cohort's resident payload.
    super::vst2_batch::retire_native()?;
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
        drop(instance);
        super::vst2_batch::retire_native()?;
        return Err(crate::render_cancel::cancelled());
    }
    let deadline =
        super::offline_warmup::WarmupDeadline::after_preparation(request.pre_roll_seconds)?;
    let latency = instance.snapshot.latency_samples;
    let output = match OutputPlan::new(routing, &instance.snapshot.outputs) {
        Ok(output) => output,
        Err(error) => {
            drop(instance);
            super::vst2_batch::retire_native()?;
            return Err(error);
        }
    };
    let frames = request.total_frames;
    let transport = request.transport;
    Ok(super::vst2_batch::PreparedGroup::native(
        deadline,
        Box::new(move |cancel| {
            let buffers = super::vst2_runtime::render(
                &mut instance,
                &schedule,
                &output.pairs,
                frames,
                transport,
                Some(cancel),
            )?;
            Ok(output.assign(buffers, latency))
        }),
    ))
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
