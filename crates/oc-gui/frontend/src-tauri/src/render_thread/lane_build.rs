//! Construction of one source lane: its instrument and its lane effect chain.
//!
//! A full load, a hot-added lane and a lane loaded in the background all build
//! a lane the same way, so they share this one constructor.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use oc_dsl::ast::{PresetDef, Project, Track};
use oc_engine::block_source::{BlockSource, SmartSource};
use oc_engine::graph::EffectChain;
use oc_engine::interpret::MidiMessage;
use oc_engine::render_parallel::RenderWorkerOptions;
use oc_synth::performance::ScheduledCommand;

use super::{LaneSpec, clip_only_lane};

/// What one lane is built from.
pub(crate) struct LaneInput<'a> {
    /// The resolved project the lane belongs to.
    pub project: &'a Project,
    /// The lane's own track, with its mix neutralised for source rendering.
    pub track: &'a Track,
    pub preset: Option<&'a PresetDef>,
    pub soundfont_path: &'a Path,
    pub sample_rate: u32,
    pub root: Option<&'a Path>,
    /// The lane's score, in time order.
    pub events: &'a [&'a MidiMessage],
    /// The lane carries notes or a performance, so it needs an instrument.
    pub has_musical: bool,
    /// Scheduled commands when the lane is a live performance.
    pub performance: Option<&'a [ScheduledCommand]>,
    pub workers: RenderWorkerOptions,
    pub instantiations: &'a AtomicU64,
}

/// A lane ready to render.
pub(crate) struct Lane {
    pub source: SmartSource,
    pub chain: EffectChain,
    pub spec: LaneSpec,
}

/// Build one lane's effect chain and instrument.
pub(crate) fn build_lane(input: LaneInput<'_>) -> Result<Lane, String> {
    let LaneInput {
        project,
        track,
        preset,
        soundfont_path,
        sample_rate,
        root,
        events,
        has_musical,
        performance,
        workers,
        instantiations,
    } = input;
    let params = oc_engine::preset::builtin_track_params_at(project, preset, track, root);
    let mut chain =
        oc_engine::source_lanes::effect_chain_with_resources(project, track, sample_rate, root)?;
    // Lane effects run on the streaming path, so their hosted plugins
    // participate in the idle gate.
    chain.set_streaming_idle_gate(true);
    let mut source = if clip_only_lane(project, track, has_musical) {
        BlockSource::Silent
    } else {
        instantiations.fetch_add(1, Ordering::Relaxed);
        match performance {
            Some(commands) => BlockSource::new_performance(
                preset,
                &params,
                soundfont_path,
                sample_rate,
                project.properties.tempo,
                commands,
                root,
            ),
            None => BlockSource::new_streaming(
                preset,
                &params,
                soundfont_path,
                sample_rate,
                project.properties.tempo,
                root,
                oc_engine::block_source::StreamingSourcePreparation { events, workers },
            ),
        }
        .map_err(|error| format!("Failed to create source for {}: {error}", track.name))?
    };
    // A lane is built at frame zero; one that joins later moves its clock
    // when it is admitted.
    source.install_hairpin_layers(preset, project, track, &project.properties, 0)?;
    Ok(Lane {
        source: SmartSource::new(source, sample_rate, track.mix.smart_disable),
        chain,
        spec: LaneSpec {
            preset: preset.cloned(),
            params,
            performance: performance.is_some(),
            smart_disable: track.mix.smart_disable,
        },
    })
}
