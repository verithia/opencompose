//! Read-only automation read-out for the controls that display a driven value.

use serde::Serialize;
use tauri::State;

use oc_dsl::ast;
use oc_dsl::timing::ProjectClock;

use crate::audio::PlaybackState;
use crate::state::{AppState, AppStateInner};

use super::shared::{Target, parse_target};

/// The unit a driven value is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationDomain {
    /// A `pluginparam_` lane: the 0..1 position a patch or plugin takes.
    Normalized,
    /// An effect-parameter lane: the unit of the device parameter.
    Device,
}

/// What a control should display for one parameter.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AutomationReadout {
    /// A lane drives the parameter at the asked position.
    Driven {
        value: f64,
        domain: AutomationDomain,
        /// The lane's one-pole time constant; 0 keeps the raw curve values.
        smoothing_ms: f64,
    },
    /// No lane value applies here, so the control keeps its stored value.
    NotDriven { reason: String },
}

fn not_driven(reason: impl Into<String>) -> AutomationReadout {
    AutomationReadout::NotDriven {
        reason: reason.into(),
    }
}

/// The lane name as a reader knows it, decoding the patch encoding.
fn lane_label(lane_param: &str) -> String {
    oc_dsl::automation::decode_plugin_automation_param(lane_param)
        .unwrap_or_else(|| lane_param.to_string())
}

/// One automation owner and the state a read-out needs from it.
enum Owner<'a> {
    Track(&'a ast::Track),
    Bus(&'a ast::Bus),
    Master(&'a ast::Master),
}

impl<'a> Owner<'a> {
    fn lanes(&self) -> Vec<&'a ast::AutomationLane> {
        match self {
            Owner::Track(track) => track.automation().collect(),
            Owner::Bus(bus) => bus.automation.iter().collect(),
            Owner::Master(master) => master.automation.iter().collect(),
        }
    }

    fn effects(&self) -> &'a [ast::Effect] {
        match self {
            Owner::Track(track) => &track.effects,
            Owner::Bus(bus) => &bus.effects,
            Owner::Master(master) => &master.effects,
        }
    }

    fn track(&self) -> Option<&'a ast::Track> {
        match self {
            Owner::Track(track) => Some(track),
            _ => None,
        }
    }

    /// Why this owner is silent, when it is muted outright.
    fn mute_reason(&self) -> Option<String> {
        match self {
            Owner::Track(track) if track.mix.mute => {
                Some(format!("track '{}' is muted", track.name))
            }
            Owner::Bus(bus) if bus.mute => Some(format!("bus '{}' is muted", bus.name)),
            _ => None,
        }
    }
}

/// The owner a panel target names; `None` when the project has no such section.
fn resolve_owner<'a>(project: &'a ast::Project, target: &str) -> Result<Option<Owner<'a>>, String> {
    match parse_target(target)? {
        Target::Track(index) => project
            .tracks
            .get(index)
            .map(Owner::Track)
            .ok_or_else(|| format!("Track {index} out of range"))
            .map(Some),
        Target::Bus(name) => project
            .buses
            .iter()
            .find(|bus| bus.name == name)
            .map(Owner::Bus)
            .ok_or_else(|| format!("Bus '{name}' not found"))
            .map(Some),
        Target::Master => Ok(project.master.as_ref().map(Owner::Master)),
    }
}

/// Answer what drives one parameter at `position_beats`, or why nothing does.
///
/// `lane_index` keys the lane when an owner has several lanes on one
/// parameter; without it the parameter must name one lane.
pub(crate) fn automation_value_at_inner(
    inner: &AppStateInner,
    target: &str,
    lane_param: &str,
    lane_index: Option<usize>,
    position_beats: f64,
) -> Result<AutomationReadout, String> {
    if !position_beats.is_finite() || position_beats < 0.0 {
        return Err("Automation position must be a non-negative finite beat".into());
    }
    if target.starts_with("preset:") {
        return Ok(not_driven("a preset chain has no automation lanes"));
    }
    let project = &inner.project;
    let Some(owner) = resolve_owner(project, target)? else {
        return Ok(not_driven("the project has no such section"));
    };
    let lanes = owner.lanes();
    let lane = match super::fx::resolve_lane_index_in(&lanes, lane_param, lane_index) {
        Ok(index) => lanes[index],
        // The parameter exists but the key is not unique: say so rather than
        // picking one of its lanes.
        Err(reason) if lanes.iter().any(|lane| lane.param == lane_param) => {
            return Ok(not_driven(reason));
        }
        Err(_) => {
            return Ok(not_driven(format!(
                "no automation lane targets '{}'",
                lane_label(lane_param)
            )));
        }
    };
    if let Some(reason) = owner.mute_reason() {
        return Ok(not_driven(reason));
    }
    if inner.audio.playback_state != PlaybackState::Playing {
        return Ok(not_driven("the transport is stopped"));
    }

    let clock = ProjectClock::new(&project.properties)?;
    let sample_rate = inner.audio.sample_rate;
    let (value, domain) = if let Some((effect_target, parameter)) = lane.effect_lane() {
        let effects = owner.effects();
        let index = oc_dsl::automation::resolve_effect(effects, effect_target)
            .map_err(|error| format!("automation lane '{}': {error}", lane.param))?;
        let effect = &effects[index];
        let value =
            oc_dsl::automation::value_at(lane, &clock, sample_rate, position_beats, |value| {
                oc_engine::automation::effect_point_value(effect, parameter, value)
            })?;
        (value, AutomationDomain::Device)
    } else if let Some(param) = oc_dsl::automation::decode_plugin_automation_param(&lane.param) {
        let Some(track) = owner.track() else {
            return Ok(not_driven("only a track drives a patch parameter"));
        };
        let normalization = oc_engine::automation::patch_normalization(project, track);
        // A point that carries no number is skipped, exactly as the
        // interpreter skips it when it emits the lane's events.
        let mut numeric = lane.clone();
        numeric.points.retain(|point| {
            oc_engine::automation::patch_point_value(normalization, &param, &point.value).is_some()
        });
        if numeric.points.is_empty() {
            return Ok(not_driven("the lane has no numeric points"));
        }
        let value =
            oc_dsl::automation::value_at(&numeric, &clock, sample_rate, position_beats, |value| {
                oc_engine::automation::patch_point_value(normalization, &param, value)
                    .ok_or_else(|| format!("lane value {value:?} is not a number"))
            })?;
        (value, AutomationDomain::Normalized)
    } else {
        return Ok(not_driven(format!(
            "lane '{}' drives a MIDI controller, not a panel value",
            lane.param
        )));
    };

    let Some(value) = value else {
        return Ok(not_driven("the playhead is before the lane's first point"));
    };
    Ok(AutomationReadout::Driven {
        value,
        domain,
        smoothing_ms: lane.smoothing_ms,
    })
}

/// Read the value a lane drives into one parameter, or the reason it does not.
///
/// `target` is a panel target (`track:N`, `bus:name`, `master`);
/// `position_beats` is the transport position in quarter notes. `lane_index`
/// selects one lane when several share the parameter.
#[tauri::command]
pub fn automation_value_at(
    target: String,
    lane_param: String,
    lane_index: Option<usize>,
    position_beats: f64,
    state: State<AppState>,
) -> Result<AutomationReadout, String> {
    let inner = state.lock_inner();
    automation_value_at_inner(&inner, &target, &lane_param, lane_index, position_beats)
}
