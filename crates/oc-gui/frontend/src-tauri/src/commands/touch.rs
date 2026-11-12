//! Live touch overrides for automated effect parameters.
//!
//! A touch is not a project edit: it only tells the live graph to ignore the
//! lane while the hand holds the parameter. Offline rendering never reads it.

use tauri::State;

use oc_dsl::ast;
use oc_engine::touch;

use crate::audio::PlaybackState;
use crate::state::{AppState, ParameterTargetStamp};

use super::shared::{Target, parse_target};

/// Start a touch on one automated effect parameter.
///
/// Returns `false` when no automation lane owns the parameter, or when the
/// transport is not streaming; the caller keeps the ordinary edit path then.
#[tauri::command]
pub fn begin_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    value: f64,
    state: State<AppState>,
) -> Result<bool, String> {
    let mut inner = state.lock_inner();
    begin_effect_param_touch_inner(&mut inner, &target, effect_index, &param_name, value)
}

pub(crate) fn begin_effect_param_touch_inner(
    inner: &mut crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    value: f64,
) -> Result<bool, String> {
    let Some(registry) = eligible_registry(inner, target, effect_index, param_name, value)? else {
        return Ok(false);
    };
    Ok(registry.begin_legacy(&touch::effect_key(target, effect_index, param_name), value))
}

fn eligible_registry(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    value: f64,
) -> Result<Option<std::sync::Arc<touch::TouchRegistry>>, String> {
    if !value.is_finite() || inner.audio.playback_state != PlaybackState::Playing {
        return Ok(None);
    }
    let Some(handle) = inner.audio.render_thread.as_ref() else {
        return Ok(None);
    };
    let parsed = parse_target(target)?;
    let effects = match &parsed {
        Target::Track(index) => {
            &inner
                .project
                .tracks
                .get(*index)
                .ok_or_else(|| format!("Track {index} out of range"))?
                .effects
        }
        Target::Bus(name) => {
            &inner
                .project
                .buses
                .iter()
                .find(|bus| &bus.name == name)
                .ok_or_else(|| format!("Bus '{name}' not found"))?
                .effects
        }
        Target::Master => {
            &inner
                .project
                .master
                .as_ref()
                .ok_or("No master section")?
                .effects
        }
    };
    let effect = effects
        .get(effect_index)
        .ok_or_else(|| format!("Effect index {effect_index} out of range"))?;
    let lanes = match &parsed {
        Target::Track(index) => inner
            .project
            .tracks
            .get(*index)
            .map(|track| track.automation().collect::<Vec<_>>())
            .unwrap_or_default(),
        Target::Bus(name) => inner
            .project
            .buses
            .iter()
            .find(|bus| &bus.name == name)
            .map(|bus| bus.automation.iter().collect::<Vec<_>>())
            .unwrap_or_default(),
        Target::Master => inner
            .project
            .master
            .as_ref()
            .map(|master| master.automation.iter().collect::<Vec<_>>())
            .unwrap_or_default(),
    };
    if !lane_drives(&lanes, effect, effect_index, param_name) {
        return Ok(None);
    }
    Ok(Some(handle.touch()))
}

/// Move the hand value of a held touch.
#[tauri::command]
pub fn update_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    value: f64,
    state: State<AppState>,
) {
    update_effect_param_touch_inner(
        &state.lock_inner(),
        &target,
        effect_index,
        &param_name,
        value,
    );
}

pub(crate) fn update_effect_param_touch_inner(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    value: f64,
) {
    if !value.is_finite() {
        return;
    }
    if let Some(registry) = inner
        .audio
        .render_thread
        .as_ref()
        .map(|handle| handle.touch())
    {
        registry.update(&touch::effect_key(target, effect_index, param_name), value);
    }
}

/// Release a held touch; the lane takes the parameter back.
#[tauri::command]
pub fn end_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    state: State<AppState>,
) {
    end_effect_param_touch_inner(&state.lock_inner(), &target, effect_index, &param_name);
}

pub(crate) fn end_effect_param_touch_inner(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
) {
    if let Some(registry) = inner
        .audio
        .render_thread
        .as_ref()
        .map(|handle| handle.touch())
    {
        registry.release(&touch::effect_key(target, effect_index, param_name));
    }
}

/// True when an `automation:` lane of the owner addresses this chain entry and
/// parameter. Both lane address forms count: `fx<N>` and the effect type name.
fn lane_drives(
    lanes: &[&ast::AutomationLane],
    effect: &ast::Effect,
    effect_index: usize,
    param_name: &str,
) -> bool {
    let by_position = format!("fx{}.{param_name}", effect_index + 1);
    let by_type = format!("{}.{param_name}", effect.effect_type.lane_name());
    lanes
        .iter()
        .any(|lane| lane.param == by_position || lane.param == by_type)
}

/// Opaque wire identity includes its document/topology admission capture.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EffectTouchToken {
    native: touch::TouchToken,
    stamp: ParameterTargetStamp,
}
impl TryFrom<String> for EffectTouchToken {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        if value.len() > 256 {
            return Err("Invalid effect touch token".into());
        }
        let (native, stamp) =
            serde_json::from_str(&value).map_err(|_| "Invalid effect touch token")?;
        Ok(Self { native, stamp })
    }
}
impl From<EffectTouchToken> for String {
    fn from(value: EffectTouchToken) -> Self {
        serde_json::to_string(&(value.native, value.stamp))
            .expect("String-only touch token serialization")
    }
}

/// Begin a guarded effect parameter touch.
#[tauri::command]
pub fn begin_guarded_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    value: f64,
    expected_stamp: ParameterTargetStamp,
    state: State<AppState>,
) -> Result<Option<EffectTouchToken>, String> {
    begin_guarded_effect_param_touch_inner(
        &state.lock_inner(),
        &target,
        effect_index,
        &param_name,
        value,
        expected_stamp,
    )
}

pub(crate) fn begin_guarded_effect_param_touch_inner(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    value: f64,
    expected_stamp: ParameterTargetStamp,
) -> Result<Option<EffectTouchToken>, String> {
    super::shared::require_parameter_admission(inner, &expected_stamp)?;
    let Some(registry) = eligible_registry(inner, target, effect_index, param_name, value)? else {
        return Ok(None);
    };
    let native = registry
        .begin_owned(&touch::effect_key(target, effect_index, param_name), value)
        .map_err(|error| error.to_string())?;
    Ok(Some(EffectTouchToken {
        native,
        stamp: expected_stamp,
    }))
}

/// Update an owned effect parameter touch.
#[tauri::command]
pub fn update_owned_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    value: f64,
    token: EffectTouchToken,
    state: State<AppState>,
) -> Result<bool, String> {
    update_owned_effect_param_touch_inner(
        &state.lock_inner(),
        &target,
        effect_index,
        &param_name,
        value,
        &token,
    )
}

pub(crate) fn update_owned_effect_param_touch_inner(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    value: f64,
    token: &EffectTouchToken,
) -> Result<bool, String> {
    if super::shared::require_parameter_admission(inner, &token.stamp).is_err() {
        end_owned_effect_param_touch_inner(inner, target, effect_index, param_name, token);
        return Ok(false);
    }
    let registry = match eligible_registry(inner, target, effect_index, param_name, value) {
        Ok(Some(registry)) => registry,
        result => {
            end_owned_effect_param_touch_inner(inner, target, effect_index, param_name, token);
            return result.map(|_| false);
        }
    };
    Ok(registry.update_owned(
        &touch::effect_key(target, effect_index, param_name),
        &token.native,
        value,
    ))
}

/// End an owned effect parameter touch.
#[tauri::command]
pub fn end_owned_effect_param_touch(
    target: String,
    effect_index: usize,
    param_name: String,
    token: EffectTouchToken,
    state: State<AppState>,
) -> Result<bool, String> {
    Ok(end_owned_effect_param_touch_inner(
        &state.lock_inner(),
        &target,
        effect_index,
        &param_name,
        &token,
    ))
}

pub(crate) fn end_owned_effect_param_touch_inner(
    inner: &crate::state::AppStateInner,
    target: &str,
    effect_index: usize,
    param_name: &str,
    token: &EffectTouchToken,
) -> bool {
    inner.audio.render_thread.as_ref().is_some_and(|handle| {
        handle.touch().release_owned(
            &touch::effect_key(target, effect_index, param_name),
            &token.native,
        )
    })
}
