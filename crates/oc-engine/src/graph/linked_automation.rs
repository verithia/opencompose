//! Resolve control links before building the effect chains that consume them.
use super::effect_automation::ParamInput;
use super::live_controls::LiveControls;
use super::macro_preview::MacroSnapshots;
use oc_dsl::ast::{
    AutomationLane, AutomationValueMapping, ControlLinkSource, Effect, LinkFormula,
    LinkFormulaInput, Project, Track,
};
use oc_dsl::automation::{LaneCurve, effect_lane, resolve_effect};
use oc_dsl::timing::ProjectClock;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const MAX_PROJECT_LINK_LANES: usize = 4096;

pub(super) struct LinkedLane {
    pub slot: usize,
    pub parameter: String,
    pub input: ParamInput,
    pub formula: LinkFormula,
    pub value_mapping: Option<AutomationValueMapping>,
    pub smoothing_ms: u32,
    pub bounds: (f64, f64),
}

#[derive(Default)]
pub(super) struct LinkedAutomation<'a> {
    owners: HashMap<&'a str, Vec<LinkedLane>>,
}

#[derive(Default)]
pub(super) struct LinkedSources<'a> {
    pub midi: Option<&'a LiveControls>,
    pub macros: Option<&'a MacroSnapshots>,
    pub root: Option<&'a Path>,
}

impl<'a> LinkedAutomation<'a> {
    pub(super) fn new(
        project: &'a Project,
        sample_rate: u32,
        sources: LinkedSources<'_>,
    ) -> Result<Self, String> {
        Self::prepare(project, sample_rate, None, sources)
    }

    pub(super) fn for_track(
        project: &'a Project,
        sample_rate: u32,
        track: usize,
        root: Option<&Path>,
    ) -> Result<Self, String> {
        let owner = project
            .tracks
            .get(track)
            .ok_or_else(|| format!("Track {track} out of range"))?;
        Self::prepare(
            project,
            sample_rate,
            Some(owner.name.as_str()),
            LinkedSources {
                root,
                ..Default::default()
            },
        )
    }

    fn prepare(
        project: &'a Project,
        sample_rate: u32,
        only_owner: Option<&str>,
        sources: LinkedSources<'_>,
    ) -> Result<Self, String> {
        let mut prepared = Self::default();
        let links = &project.properties.control_links;
        if sources.midi.is_none_or(LiveControls::is_empty)
            && !links.iter().any(|link| {
                matches!(
                    link.source,
                    ControlLinkSource::Lane { .. } | ControlLinkSource::PresetMacro { .. }
                )
            })
        {
            return Ok(prepared);
        }

        let tracks: HashMap<_, _> = project
            .tracks
            .iter()
            .map(|track| (track.name.as_str(), track))
            .collect();
        let buses: HashMap<_, _> = project
            .buses
            .iter()
            .map(|bus| (bus.name.as_str(), bus))
            .collect();
        let clock = ProjectClock::new(&project.properties)?;
        let mut source_curves = HashMap::new();
        let mut source_macros = HashMap::<&str, [f64; 8]>::new();
        let mut assigned = HashSet::new();
        let mut lane_count = 0;
        for link in links {
            let reading = sources.midi.and_then(|live| live.snapshot(&link.source));
            let lane_source = match &link.source {
                ControlLinkSource::Lane { track, param } => Some((track.as_str(), param.as_str())),
                ControlLinkSource::PresetMacro { .. } => None,
                _ if reading.is_some() => None,
                _ => continue,
            };
            for target in &link.targets {
                if only_owner.is_some_and(|owner| owner != target.track) {
                    continue;
                }
                let Some((effect, parameter)) = effect_lane(&target.param) else {
                    continue;
                };
                if !link.source.is_midi() && target.formula.input() != LinkFormulaInput::Normalized
                {
                    return Err(format!(
                        "control link '{}': source has no raw MIDI value",
                        link.name
                    ));
                }
                let owner = target.track.as_str();
                let track_target = tracks.get(owner);
                let bus_target = buses.get(owner);
                if track_target.is_some() && bus_target.is_some() {
                    return Err(format!(
                        "control link '{}': owner '{owner}' is ambiguous",
                        link.name
                    ));
                }
                let slot = if let Some(track) = track_target {
                    resolve_target(&track.effects, track.automation(), effect, parameter)
                } else if let Some(bus) = bus_target {
                    resolve_target(&bus.effects, bus.automation.iter(), effect, parameter)
                } else {
                    Err(format!("unknown track or bus '{owner}'"))
                }
                .map_err(|error| format!("control link '{}': {error}", link.name))?;
                if !assigned.insert((owner, slot, parameter.to_ascii_lowercase())) {
                    return Err(format!(
                        "control link '{}': multiple links target '{owner}:{}'",
                        link.name, target.param
                    ));
                }

                let mut add_lane = |input, bounds| {
                    lane_count += 1;
                    if lane_count > MAX_PROJECT_LINK_LANES {
                        return Err(
                            "control links exceed the project's prepared lane budget".to_owned()
                        );
                    }
                    prepared.owners.entry(owner).or_default().push(LinkedLane {
                        slot,
                        parameter: parameter.into(),
                        input,
                        formula: target.formula,
                        value_mapping: target.value_mapping,
                        smoothing_ms: target.smoothing_ms,
                        bounds,
                    });
                    Ok(())
                };
                if let Some((track, param)) = lane_source {
                    let source = tracks.get(track).ok_or_else(|| {
                        format!(
                            "control link '{}': unknown source track '{track}'",
                            link.name
                        )
                    })?;
                    let curves = match source_curves.entry((track, param)) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => entry.insert(
                            build_curves(project, source, param, &clock, sample_rate).map_err(
                                |error| format!("control link '{}': {error}", link.name),
                            )?,
                        ),
                    };
                    for (curve, bounds) in curves {
                        add_lane(ParamInput::Timeline(Arc::clone(curve)), *bounds)?;
                    }
                } else if let Some(reading) = reading {
                    add_lane(ParamInput::MidiCc(Arc::clone(reading)), (0.0, 127.0))?;
                } else if let ControlLinkSource::PresetMacro { preset, index } = &link.source {
                    let slot = index
                        .checked_sub(1)
                        .filter(|slot| *slot < 8)
                        .ok_or_else(|| {
                            format!(
                                "control link '{}': macro index must be within 1..8",
                                link.name
                            )
                        })?;
                    let values = match source_macros.entry(preset) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => {
                            let preset = link
                                .source
                                .resolve_macro_preset(project)?
                                .expect("named preset macro");
                            let patch =
                                oc_synth::synth::load_project_patch(&preset.params, sources.root)?;
                            entry.insert(std::array::from_fn(|i| {
                                patch
                                    .macros
                                    .get(i)
                                    .copied()
                                    .unwrap_or_else(|| patch.extra_macros[i - patch.macros.len()])
                            }))
                        }
                    };
                    let value = values[usize::from(slot)];
                    let preview = sources
                        .macros
                        .and_then(|macros| macros.reading(preset, *index));
                    add_lane(
                        ParamInput::PresetValue {
                            saved: value,
                            preview,
                        },
                        (0.0, 1.0),
                    )?;
                }
            }
        }
        Ok(prepared)
    }

    pub(super) fn for_owner(&self, owner: &str) -> &[LinkedLane] {
        self.owners.get(owner).map_or(&[], Vec::as_slice)
    }
}

fn resolve_target<'a>(
    effects: &[Effect],
    direct: impl Iterator<Item = &'a AutomationLane>,
    target: &str,
    parameter: &str,
) -> Result<usize, String> {
    let slot = resolve_effect(effects, target)?;
    if direct
        .filter(|lane| !lane.muted && !lane.points.is_empty())
        .any(|lane| {
            lane.effect_lane().is_some_and(|(owner, used)| {
                used.eq_ignore_ascii_case(parameter) && resolve_effect(effects, owner) == Ok(slot)
            })
        })
    {
        return Err(format!(
            "'{target}.{parameter}' also has a direct automation lane"
        ));
    }
    Ok(slot)
}

type CurveWithBounds = (Arc<LaneCurve>, (f64, f64));

fn build_curves(
    project: &Project,
    track: &Track,
    parameter: &str,
    clock: &ProjectClock,
    rate: u32,
) -> Result<Vec<CurveWithBounds>, String> {
    let source_effect = effect_lane(parameter)
        .map(|(owner, key)| {
            resolve_effect(&track.effects, owner).map(|slot| {
                (
                    key,
                    crate::dsp::device_for_effect(&track.effects[slot].effect_type),
                )
            })
        })
        .transpose()?;
    let normalization = crate::automation::patch_normalization(project, track);
    track
        .automation()
        .filter(|lane| lane.param == parameter && !lane.muted && !lane.points.is_empty())
        .map(|lane| {
            let curve = LaneCurve::new(lane, clock, rate, |value| {
                if let Some((key, device)) = source_effect {
                    crate::automation::effect_curve_point_value(lane, key, value, device)
                } else {
                    crate::automation::patch_point_value(normalization, parameter, value)
                        .ok_or_else(|| format!("source lane '{parameter}' needs a numeric value"))
                }
            })?;
            let bounds = curve
                .value_bounds()
                .ok_or("source lane has no finite bounds")?;
            Ok((Arc::new(curve), bounds))
        })
        .collect()
}

pub(super) fn changed_owners<'a>(previous: &'a Project, next: &'a Project) -> HashSet<&'a str> {
    let mut affected = HashSet::new();
    let old = &previous.properties.control_links;
    let new = &next.properties.control_links;
    fn authored_links(
        links: &[oc_dsl::ast::ControlLink],
    ) -> impl Iterator<Item = &oc_dsl::ast::ControlLink> {
        links.iter().filter(|link| {
            matches!(
                link.source,
                ControlLinkSource::Lane { .. } | ControlLinkSource::PresetMacro { .. }
            )
        })
    }
    let link_edits = authored_links(old).ne(authored_links(new));
    let normalization_changed =
        previous.properties.automation_normalization != next.properties.automation_normalization;
    for link in authored_links(old).chain(authored_links(new)) {
        let source_changed = match &link.source {
            ControlLinkSource::Lane { track, param } => {
                normalization_changed || source_changed(previous, next, track, param)
            }
            ControlLinkSource::PresetMacro { .. } => {
                previous.presets != next.presets
                    && link.source.resolve_macro_preset(previous)
                        != link.source.resolve_macro_preset(next)
            }
            _ => false,
        };
        if !link_edits && !source_changed {
            continue;
        }
        affected.extend(
            link.targets
                .iter()
                .filter(|target| effect_lane(&target.param).is_some())
                .map(|target| target.track.as_str()),
        );
    }
    affected
}

fn source_changed(previous: &Project, next: &Project, track: &str, param: &str) -> bool {
    let (Some(before), Some(after)) = (
        previous.tracks.iter().find(|owner| owner.name == track),
        next.tracks.iter().find(|owner| owner.name == track),
    ) else {
        return true;
    };
    before
        .automation()
        .filter(|lane| lane.param == param)
        .ne(after.automation().filter(|lane| lane.param == param))
        || before.effects != after.effects
        || crate::automation::patch_normalization(previous, before)
            != crate::automation::patch_normalization(next, after)
}

#[cfg(test)]
mod tests {
    use super::super::{AudioGraph, ChainPatch, GraphLiveInputs, PresetMacroInput};
    use crate::control_link::MidiControlInput;
    use oc_audio::PluginTransport;
    use oc_dsl::ast::{ParamValue, Project};
    use std::collections::HashMap;
    use std::sync::Arc;

    const RATE: u32 = 48_000;
    const SOURCE: &str = r#"@project
  tempo: 120
@track feeder | instrument: audio/clip
  effects:
    - gain: factor=1
  automation: fx1.factor [ppq=48000] {1:1:0=0.25, 1:1:240=0.75}
@track left | instrument: audio/clip
  mix: pan_law=balance
  effects:
    - gain: factor=1
@track right | instrument: audio/clip
  mix: pan_law=balance, output=return
@bus return
  routes: [master]
  effects:
    - gain: factor=1
@control_link linked
  from: lane(feeder, "fx1.factor")
  -> track: left param: fx1.factor formula: linear
  -> track: return param: fx1.factor formula: inverted
"#;

    fn project() -> Project {
        let parsed = oc_dsl::parse(SOURCE).unwrap();
        oc_dsl::validate(&parsed).unwrap();
        parsed
    }

    fn render(
        graph: &mut AudioGraph,
        project: &Project,
        start: u64,
        block_frames: usize,
        block_count: usize,
    ) -> Vec<f32> {
        graph.prepare(block_frames).unwrap();
        let transport = PluginTransport::from_project(0, RATE, &project.properties).unwrap();
        let mut inputs = [
            vec![0.0; block_frames * 2],
            [0.125, 0.0].repeat(block_frames),
            [0.0, -0.0625].repeat(block_frames),
        ];
        let aux = HashMap::new();
        let mut result = Vec::with_capacity(block_frames * block_count * 2);
        let mut pcm = vec![0.0; block_frames * 2];
        for block in 0..block_count {
            inputs[1]
                .chunks_exact_mut(2)
                .for_each(|frame| frame.copy_from_slice(&[0.125, 0.0]));
            inputs[2]
                .chunks_exact_mut(2)
                .for_each(|frame| frame.copy_from_slice(&[0.0, -0.0625]));
            let time = transport.at_sample(start + (block * block_frames) as u64);
            let mut outcome = Ok(());
            assert_eq!(
                crate::test_alloc::operations(|| {
                    outcome = graph.process_into(&mut inputs, Some(time), &aux, &mut pcm);
                }),
                0
            );
            outcome.unwrap();
            result.extend_from_slice(&pcm);
        }
        result
    }

    #[test]
    fn lane_link_fans_out_to_track_and_bus_without_realtime_allocation() {
        let project = project();
        let make =
            || AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
        let reference = render(&mut make(), &project, 0, 128, 1);
        let mut streamed = make();
        let blocks = render(&mut streamed, &project, 0, 16, 8);
        assert_eq!(blocks, reference);
        for (sample, stereo) in blocks.chunks_exact(2).enumerate() {
            let gain = 0.25 + 0.5 * sample.min(120) as f32 / 120.0;
            assert!(
                (stereo[0] - 0.125 * gain).abs() < 1e-7,
                "{sample}: {stereo:?}"
            );
            assert!(
                (stereo[1] + 0.0625 * (1.0 - gain)).abs() < 1e-7,
                "{sample}: {stereo:?}"
            );
        }
        streamed.reset();
        assert_eq!(render(&mut streamed, &project, 48, 16, 2), blocks[96..160]);
    }

    #[test]
    fn midi_cc_links_update_track_and_bus_in_the_same_block() {
        let source = SOURCE
            .replace("lane(feeder, \"fx1.factor\")", "cc(74, 2)")
            .replace("formula: linear\n", "formula: midi_unit\n")
            .replace(
                "formula: inverted\n",
                "formula: midi_centered mapping(linear, scale=2)\n",
            );
        let project = oc_dsl::parse(&source).unwrap();
        oc_dsl::validate(&project).unwrap();
        let input = Arc::new(MidiControlInput::default());
        let mut graph = AudioGraph::try_from_project_with_live_inputs(
            &project,
            RATE,
            &[],
            None,
            GraphLiveInputs {
                midi_cc: Some(Arc::clone(&input)),
                ..Default::default()
            },
        )
        .unwrap();
        let authored = [0.125, -0.0625].repeat(16);
        assert_eq!(render(&mut graph, &project, 0, 16, 1), authored);
        input.publish(0, 74, 64);
        input.publish(1, 75, 64);
        input.publish(1, 74, 128);
        assert_eq!(render(&mut graph, &project, 16, 16, 1), authored);
        input.publish(1, 74, 64);
        let centered = [0.125 * 64.0 / 127.0, -0.125].repeat(16);
        assert_eq!(render(&mut graph, &project, 32, 16, 1), centered);
        input.publish(1, 74, 127);
        assert_eq!(
            render(&mut graph, &project, 48, 16, 1),
            [0.125, 0.0].repeat(16)
        );
        graph.reset();
        input.publish(1, 74, 64);
        assert_eq!(render(&mut graph, &project, 0, 16, 1), centered);

        let mut offline =
            AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
        assert_eq!(render(&mut offline, &project, 0, 16, 1), authored);
        let mut changed = project.clone();
        changed.properties.control_links[0].source = oc_dsl::ast::ControlLinkSource::MidiCc {
            channel: 2,
            controller: 75,
        };
        assert_eq!(
            graph.patch_chains(&project, &changed, None, false).unwrap(),
            ChainPatch::Patched { chains: 2 }
        );
        assert_eq!(render(&mut graph, &changed, 16, 16, 1), centered);
        input.publish(1, 75, 0);
        assert_eq!(
            render(&mut graph, &changed, 32, 16, 1),
            [0.0, 0.0].repeat(16)
        );
    }

    #[test]
    fn named_preset_macro_controls_an_editable_track_and_bus_mix() {
        let source = SOURCE
            .replace(
                "@track feeder",
                r#"@preset macro_base
  source: builtin
  plugin: "builtin.open_synth"
  params: {macro.8: 0.25}
@preset controls
  extends: macro_base
@track feeder"#,
            )
            .replace("lane(feeder, \"fx1.factor\")", "macro(8, controls)");
        let project = oc_dsl::parse(&source).unwrap();
        oc_dsl::validate(&project).unwrap();
        let make =
            || AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
        let initial = [0.03125, -0.046875].repeat(16);
        let mut graph = make();
        assert_eq!(render(&mut graph, &project, 0, 16, 1), initial);
        let mut edited = project.clone();
        edited.presets[0].params[0].1 = ParamValue::Number(0.75);
        assert_eq!(
            graph.patch_chains(&project, &edited, None, false).unwrap(),
            ChainPatch::Patched { chains: 2 },
        );
        let expected = [0.09375, -0.015625].repeat(16);
        assert_eq!(render(&mut graph, &edited, 16, 16, 1), expected);
        graph.reset();
        assert_eq!(render(&mut graph, &edited, 0, 16, 1), expected);

        let preview = Arc::new(PresetMacroInput::default());
        let mut live = AudioGraph::try_from_project_with_live_inputs(
            &project,
            RATE,
            &[],
            None,
            GraphLiveInputs {
                preset_macros: Some(Arc::clone(&preview)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(render(&mut live, &project, 0, 16, 1), initial);
        let first = preview.begin("controls", 8).unwrap().unwrap();
        assert!(!preview.update("controls", 8, &first, f64::NAN));
        assert!(preview.update("controls", 8, &first, 1.0));
        assert_eq!(
            render(&mut live, &project, 16, 16, 1),
            [0.125, 0.0].repeat(16)
        );
        assert_eq!(render(&mut make(), &project, 0, 16, 1), initial);
        let second = preview.begin("controls", 8).unwrap().unwrap();
        assert!(!preview.end("controls", 8, &first));
        assert!(preview.update("controls", 8, &second, 0.5));
        assert_eq!(
            live.patch_chains(&project, &edited, None, false).unwrap(),
            ChainPatch::Patched { chains: 2 },
        );
        assert_eq!(
            render(&mut live, &edited, 32, 16, 1),
            [0.0625, -0.03125].repeat(16)
        );
        assert!(preview.end("controls", 8, &second));
        assert_eq!(render(&mut live, &edited, 48, 16, 1), expected);
        assert!(live.commit_saved_preset_macro("controls", 8, 0.5));
        assert_eq!(
            render(&mut live, &edited, 64, 16, 1),
            [0.0625, -0.03125].repeat(16),
        );
        let mut committed = edited.clone();
        committed.presets[0].params[0].1 = ParamValue::Number(0.5);
        assert_eq!(
            live.patch_chains(&committed, &edited, None, false).unwrap(),
            ChainPatch::Patched { chains: 2 },
        );
        assert_eq!(render(&mut live, &edited, 80, 16, 1), expected);
    }

    #[test]
    fn source_edits_rebuild_receivers_and_competing_authored_writers_are_refused() {
        let previous = project();
        let mut next = previous.clone();
        next.tracks[0].automation_mut().next().unwrap().points[0].value = ParamValue::Number(0.5);
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&previous, RATE, &[]).unwrap();
        let original = render(&mut graph, &previous, 0, 16, 1);
        assert_eq!(
            graph.patch_chains(&previous, &next, None, false).unwrap(),
            ChainPatch::Patched { chains: 3 }
        );
        graph.reset();
        let edited = render(&mut graph, &next, 0, 16, 1);
        assert_eq!(&original[..2], &[0.03125, -0.046875]);
        assert_eq!(&edited[..2], &[0.0625, -0.03125]);

        let conflicting = SOURCE.replace(
            "@control_link linked",
            "  automation: gain.factor {1:1=0.25}\n@control_link linked",
        );
        let project = oc_dsl::parse(&conflicting).unwrap();
        assert!(AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).is_err());
    }

    #[test]
    fn source_and_destination_smoothing_compose_in_parameter_units() {
        let authored = SOURCE
            .replace("1:1:0=0.25", "1:1:0=0.25 curve=stairs")
            .replace("[ppq=48000]", "[ppq=48000] smoothing=2ms")
            .replace("formula: linear", "formula: exponential smoothing: 4ms");
        let project = oc_dsl::parse(&authored).unwrap();
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
        let pcm = render(&mut graph, &project, 0, 16, 16);
        let mut source = 0.25f64;
        let mut left = source * source;
        let source_alpha = crate::smoothing::one_pole_alpha(2.0, 1.0, f64::from(RATE));
        let left_alpha = crate::smoothing::one_pole_alpha(4.0, 1.0, f64::from(RATE));
        for (sample, stereo) in pcm.chunks_exact(2).enumerate() {
            let target = if sample < 120 { 0.25 } else { 0.75 };
            source += (target - source) * source_alpha;
            left += (source * source - left) * left_alpha;
            assert!(
                (f64::from(stereo[0]) - 0.125 * left).abs() < 1e-7,
                "{sample}"
            );
            assert!(
                (f64::from(stereo[1]) + 0.0625 * (1.0 - source)).abs() < 1e-7,
                "{sample}"
            );
        }
    }

    #[test]
    fn link_mapping_converts_the_formula_result_before_writing_effect_units() {
        let authored = SOURCE.replace(
            "formula: inverted",
            "formula: inverted mapping(linear, scale=2, offset=0.25)",
        );
        let project = oc_dsl::parse(&authored).unwrap();
        oc_dsl::validate(&project).unwrap();
        let mut graph =
            AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
        for (sample, stereo) in render(&mut graph, &project, 0, 16, 8)
            .chunks_exact(2)
            .enumerate()
        {
            let source = 0.25 + 0.5 * sample.min(120) as f32 / 120.0;
            let gain = 0.25 + 2.0 * (1.0 - source);
            assert!((stereo[1] + 0.0625 * gain).abs() < 1e-7, "{sample}");
        }

        let out_of_range = authored.replace("offset=0.25", "offset=1000");
        let project = oc_dsl::parse(&out_of_range).unwrap();
        let failure = AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[])
            .err()
            .unwrap();
        assert!(failure.contains("exceeds recipient range"), "{failure}");
    }
}
