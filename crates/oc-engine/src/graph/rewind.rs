//! Control-thread replacement of mixer inserts that cannot clear DSP history.

use super::{
    AudioEffect, AudioGraph, EffectChain, EffectChainBuild, PreparedGraph,
    build_effect_chain_selection,
};
use crate::touch::TouchRegistry;
use oc_dsl::ast::{AutomationLane, Effect, EffectType, InputRoute, ParamValue, Project};
use std::sync::Arc;

pub(super) const RECREATE_PARAM: &str = "host_recreate_on_rewind";

fn configured(effect: &Effect) -> Option<&ParamValue> {
    effect
        .params
        .iter()
        .find(|(name, _)| name == RECREATE_PARAM)
        .map(|(_, value)| value)
}

pub(super) fn validate_recreate(effect: &Effect, owner: &str, place: &str) -> Result<(), String> {
    let Some(value) = configured(effect) else {
        return Ok(());
    };
    if owner.is_empty() || !matches!(effect.effect_type, EffectType::Plugin(_)) {
        return Err(format!(
            "{RECREATE_PARAM} requires a hosted track, bus or master insert"
        ));
    }
    if !matches!(value, ParamValue::Bool(_)) {
        return Err(format!("{place}: {RECREATE_PARAM} expects true or false"));
    }
    Ok(())
}

struct MixerChain<'a> {
    effects: &'a [Effect],
    routes: &'a [InputRoute],
    lanes: Vec<&'a AutomationLane>,
    linked: &'a [super::linked_automation::LinkedLane],
    has_gain_envelope: bool,
    target: String,
    place: String,
}

type Replacements = Vec<(usize, Box<dyn AudioEffect>)>;

impl MixerChain<'_> {
    fn prepare_replacements(
        &self,
        graph: &AudioGraph,
        chain: &EffectChain,
        project: &Project,
        touch: Option<&Arc<TouchRegistry>>,
        max_frames: usize,
    ) -> Result<Replacements, String> {
        let mut replacements = Vec::new();
        for (index, effect) in self.effects.iter().enumerate() {
            if configured(effect) != Some(&ParamValue::Bool(true)) {
                continue;
            }
            if self.effects.len() + usize::from(self.has_gain_envelope) != chain.effects.len() {
                return Err(format!(
                    "Mixer chain changed before rewinding {}",
                    self.target
                ));
            }
            let mut built = build_effect_chain_selection(
                self.effects,
                self.routes,
                self.lanes.iter().copied(),
                EffectChainBuild {
                    project: Some(project),
                    sample_rate: graph.sample_rate,
                    checked: true,
                    properties: &project.properties,
                    owner: &self.target,
                    place: &self.place,
                    touch,
                    resource_root: graph.resource_root.as_deref(),
                    linked_lanes: self.linked,
                    hosted: None,
                },
                Some(index),
            )
            .map_err(|error| format!("Rewind {} insert {}: {error}", self.target, index + 1))?;
            built.prepare_block(max_frames).map_err(|error| {
                format!("Rewind {} insert {}: {error:?}", self.target, index + 1)
            })?;
            let replacement = built.effects.pop().expect("single selected insert");
            replacements.push((index, replacement));
        }
        Ok(replacements)
    }
}

fn replace_and_reset(chain: &mut EffectChain, replacements: Replacements) {
    if replacements.is_empty() {
        chain.reset();
        return;
    }
    chain
        .ramps
        .retain(|ramp| !replacements.iter().any(|(slot, _)| *slot == ramp.effect));
    let mut replacements = replacements.into_iter().peekable();
    for (slot, effect) in chain.effects.iter_mut().enumerate() {
        if replacements
            .peek()
            .is_some_and(|(selected, _)| *selected == slot)
        {
            let (_, replacement) = replacements.next().unwrap();
            effect.end_processing();
            *effect = replacement;
        } else {
            effect.reset();
        }
    }
    chain.finish_ramps_on_rewind();
}

fn needs_recreation(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|effect| configured(effect) == Some(&ParamValue::Bool(true)))
}

impl AudioGraph {
    /// Rewind a streaming mixer and recreate opted-in inserts on its render
    /// worker. The project supplies live edits and the authored state sidecars.
    /// All replacements must load and prepare before any insert is replaced.
    /// Return whether the caller must republish latency and tail information.
    pub fn rewind_mixer_for_seek(
        &mut self,
        project: &Project,
        touch: Option<&Arc<TouchRegistry>>,
        max_frames: usize,
    ) -> Result<bool, String> {
        let linked = if project
            .tracks
            .iter()
            .any(|track| needs_recreation(&track.effects))
            || project
                .buses
                .iter()
                .any(|bus| needs_recreation(&bus.effects))
        {
            super::linked_automation::LinkedAutomation::new(
                project,
                self.sample_rate,
                super::linked_automation::LinkedSources {
                    midi: self.live_controls.as_ref(),
                    macros: self.preset_macros.as_ref(),
                    root: self.resource_root.as_deref(),
                },
            )?
        } else {
            super::linked_automation::LinkedAutomation::default()
        };
        let mut tracks = Vec::new();
        let mut buses = Vec::new();
        for (index, track) in project.tracks.iter().enumerate() {
            if !needs_recreation(&track.effects) {
                continue;
            }
            let chain = self.track_chains.get(index).ok_or("Missing rewind track")?;
            let owner = MixerChain {
                effects: &track.effects,
                routes: &track.input_routes,
                lanes: track.automation().collect(),
                linked: linked.for_owner(&track.name),
                has_gain_envelope: !track.gain_automation.is_empty(),
                target: format!("track:{index}"),
                place: format!("track '{}'", track.name),
            };
            tracks.push((
                index,
                owner.prepare_replacements(self, chain, project, touch, max_frames)?,
            ));
        }
        for bus in &project.buses {
            if !needs_recreation(&bus.effects) {
                continue;
            }
            let chain = &self
                .bus_chains
                .get(&bus.name)
                .ok_or("Missing rewind bus")?
                .chain;
            let owner = MixerChain {
                effects: &bus.effects,
                routes: &bus.input_routes,
                lanes: bus.automation.iter().collect(),
                linked: linked.for_owner(&bus.name),
                has_gain_envelope: !bus.gain_automation.is_empty(),
                target: format!("bus:{}", bus.name),
                place: format!("bus '{}'", bus.name),
            };
            buses.push((
                bus.name.as_str(),
                owner.prepare_replacements(self, chain, project, touch, max_frames)?,
            ));
        }
        let master = match &project.master {
            Some(master) if needs_recreation(&master.effects) => {
                let owner = MixerChain {
                    effects: &master.effects,
                    routes: &master.input_routes,
                    lanes: master.automation.iter().collect(),
                    linked: &[],
                    has_gain_envelope: !master.gain_automation.is_empty(),
                    target: "master".into(),
                    place: "master".into(),
                };
                owner.prepare_replacements(self, &self.master_chain, project, touch, max_frames)?
            }
            _ => Vec::new(),
        };
        if tracks.is_empty() && buses.is_empty() && master.is_empty() {
            self.reset();
            return Ok(false);
        }
        let max_samples = max_frames
            .checked_mul(2)
            .ok_or("Invalid mixer block capacity")?;
        if self
            .prepared
            .as_ref()
            .is_none_or(|plan| plan.max_samples != max_samples)
        {
            return Err("Prepare the mixer before recreating inserts on rewind".into());
        }
        let previous_latency = std::mem::take(&mut self.latency_signature);
        let previous_master_latency = self.master_chain.latency_samples();
        let mut tracks = tracks.into_iter().peekable();
        for (index, chain) in self.track_chains.iter_mut().enumerate() {
            let replacements = if tracks
                .peek()
                .is_some_and(|(selected, _)| *selected == index)
            {
                tracks.next().unwrap().1
            } else {
                Vec::new()
            };
            replace_and_reset(chain, replacements);
        }
        for (name, bus) in &mut self.bus_chains {
            let replacements = buses
                .iter()
                .position(|(selected, _)| *selected == name)
                .map_or_else(Vec::new, |index| buses.swap_remove(index).1);
            replace_and_reset(&mut bus.chain, replacements);
        }
        replace_and_reset(&mut self.master_chain, master);
        self.reset_mixer_history();
        // A fresh sidechain wrapper may report the same total latency as its
        // predecessor before receiving its own detector alignment.
        self.rebuild_latency_compensation(true);
        if self.latency_signature != previous_latency
            || self.master_chain.latency_samples() != previous_master_latency
        {
            self.prepared = None;
            self.prepared =
                Some(PreparedGraph::new(self, max_samples).map_err(|error| error.to_string())?);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::PreparedGraphBlock;
    use std::collections::HashMap;

    #[test]
    fn rebuilding_one_slot_keeps_its_neighbors_and_uses_the_full_chain_addresses() {
        let project = oc_dsl::parse(
            "@project\n@track carrier | instrument: audio/clip\n  effects:\n    - gain: factor=0.5\n    - gain: factor=0.75\n    - gain: factor=0.25\n  automation: fx2.factor {1:1=0.625}\n",
        )
        .unwrap();
        let track = &project.tracks[0];
        let mut chain = build_effect_chain_selection(
            &track.effects,
            &track.input_routes,
            track.automation(),
            EffectChainBuild {
                project: Some(&project),
                sample_rate: 48_000,
                checked: true,
                properties: &project.properties,
                owner: "track:0",
                place: "track:0",
                touch: None,
                resource_root: None,
                linked_lanes: &[],
                hosted: None,
            },
            None,
        )
        .unwrap();
        let first = chain.effects[0].as_ref() as *const dyn AudioEffect;
        let last = chain.effects[2].as_ref() as *const dyn AudioEffect;
        let mut replacement = build_effect_chain_selection(
            &track.effects,
            &track.input_routes,
            track.automation(),
            EffectChainBuild {
                project: Some(&project),
                sample_rate: 48_000,
                checked: true,
                properties: &project.properties,
                owner: "track:0",
                place: "track:0",
                touch: None,
                resource_root: None,
                linked_lanes: &[],
                hosted: None,
            },
            Some(1),
        )
        .unwrap();
        assert_eq!(replacement.effects.len(), 1);
        replacement.prepare_block(16).unwrap();
        replace_and_reset(&mut chain, vec![(1, replacement.effects.pop().unwrap())]);
        assert!(std::ptr::eq(first, chain.effects[0].as_ref()));
        assert!(std::ptr::eq(last, chain.effects[2].as_ref()));
        let mut samples = [1.0; 32];
        chain.process(&mut samples, 48_000);
        assert_eq!(samples, [0.078125; 32]);
    }

    #[test]
    fn an_invalid_recreation_leaves_the_prepared_graph_running() {
        let mut project = oc_dsl::parse(
            "@project\n@track carrier | instrument: audio/clip\n  mix: pan_law=balance\n  effects:\n    - gain: factor=0.5\n",
        )
        .unwrap();
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        graph.prepare(16).unwrap();
        project.tracks[0].effects[0]
            .params
            .push((RECREATE_PARAM.into(), ParamValue::Bool(true)));
        let error = graph.rewind_mixer_for_seek(&project, None, 16).unwrap_err();
        assert!(
            error.contains("requires a hosted track, bus or master insert"),
            "{error}"
        );
        let tracks = &mut [vec![0.5; 32]];
        let output = &mut [0.; 32];
        graph
            .process_prepared(PreparedGraphBlock {
                tracks,
                transport: None,
                aux: &HashMap::new(),
                output,
            })
            .unwrap();
        assert_eq!(*output, [0.25; 32]);
    }

    #[test]
    fn unmarked_mixer_uses_the_normal_reset_path() {
        let project = oc_dsl::parse(
            "@project\n@track carrier | instrument: audio/clip\n  mix: pan_law=balance\n  effects:\n    - gain: factor=0.5\n",
        )
        .unwrap();
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[]).unwrap();
        graph.prepare(16).unwrap();
        assert!(!graph.rewind_mixer_for_seek(&project, None, 16).unwrap());
        let tracks = &mut [vec![0.5; 32]];
        let output = &mut [0.; 32];
        graph
            .process_prepared(PreparedGraphBlock {
                tracks,
                transport: None,
                aux: &HashMap::new(),
                output,
            })
            .unwrap();
        assert_eq!(*output, [0.25; 32]);
    }
}
