//! Neutral render-control types and lightweight instrument profile mapping.
//!
//! This is the bridge between musical intent and host-specific plugin APIs. The
//! first implementation reads profile hints from preset params so we can build
//! the mapping layer without changing the DSL grammar.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast::{Articulation, ParamValue};

mod profile_keys;
mod profile_resources;
use profile_keys::ProfileKeys;
pub use profile_keys::{
    ProfileAssignmentCandidate, ProfileKeyKind, ProfileValueStatus, classify_profile_assignment,
};
pub use profile_resources::{
    ProfileAssignmentClaim, ProfileAssignmentError, checked_profile_assignment,
    checked_profile_assignments,
};

/// Host-independent event shape used by the realistic rendering backbone.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderControlEvent {
    pub time_samples: u64,
    pub track_index: usize,
    pub kind: RenderControlKind,
}

#[derive(Debug, Clone, PartialEq)]
/// One host-independent performance event.
pub enum RenderControlKind {
    Note {
        channel: u8,
        note: u8,
        velocity: u8,
        duration_samples: u64,
    },
    Control {
        channel: u8,
        controller: u8,
        value: u8,
    },
    KeySwitch {
        channel: u8,
        note: u8,
        velocity: u8,
        duration_samples: u64,
    },
    ParamAutomation {
        name: String,
        value: f64,
    },
    NoteExpression {
        channel: u8,
        note: u8,
        expression: String,
        value: f64,
    },
    HumanizeHint {
        timing_ms: f64,
        velocity: u8,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
/// Maps authored articulations, controls and drum keys to host events.
pub struct InstrumentProfile {
    articulation_profiles: HashMap<String, ArticulationProfile>,
    control_cc: HashMap<String, u8>,
    control_cc14: HashMap<String, u8>,
    control_params: HashMap<String, String>,
    cc_params: HashMap<u8, String>,
    control_note_expressions: HashMap<String, String>,
    control_noise_triggers: HashMap<String, ControlNoiseTrigger>,
    drum_notes: HashMap<String, u8>,
    drum_choke_groups: HashMap<String, String>,
    drum_choke_release_samples: HashMap<String, u64>,
    note_tunings_cents: HashMap<u8, f64>,
    /// Absolute stereo pair named by the legacy `plugin_output_pair` (0-based).
    output_pair: Option<usize>,
    /// Declared output port this track takes, 0-based.
    output_port: Option<usize>,
    /// Stereo pair inside `output_port` when the port is wider than stereo.
    output_port_pair: Option<usize>,
    mpe_enabled: bool,
    mpe_first_channel: u8,
    mpe_last_channel: u8,
    pub keyswitch_duration_samples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// A controllerless noise trigger keyed from a note.
pub struct ControlNoiseTrigger {
    pub on_note: Option<u8>,
    pub off_note: Option<u8>,
    pub velocity: u8,
    pub duration_samples: u64,
    pub threshold: f64,
    pub range_min: Option<f64>,
    pub range_max: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
/// How one articulation is selected and shaped.
pub struct ArticulationProfile {
    pub keyswitch: Option<u8>,
    pub round_robin_keyswitches: Vec<u8>,
    pub control: Option<ArticulationControl>,
    pub param: Option<ArticulationParam>,
    pub humanize: Option<ArticulationHumanize>,
    pub release_trigger: Option<ArticulationReleaseTrigger>,
    pub duration_scale: Option<f64>,
    pub overlap_samples: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A controller value applied while an articulation plays.
pub struct ArticulationControl {
    pub controller: u8,
    pub value: u8,
    pub reset_value: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
/// A parameter value applied while an articulation plays.
pub struct ArticulationParam {
    pub name: String,
    pub value: f64,
    pub reset_value: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Timing and velocity offsets applied to an articulation.
pub struct ArticulationHumanize {
    pub timing_samples: u64,
    pub velocity: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A release sample triggered after the note ends.
pub struct ArticulationReleaseTrigger {
    pub note: u8,
    pub velocity: u8,
    pub duration_samples: u64,
    pub sustain_aware: bool,
}

impl InstrumentProfile {
    /// Preset assignments precede the effective source track's overrides.
    pub fn from_track(
        presets: &[oc_dsl::ast::PresetDef],
        track: &oc_dsl::ast::Track,
        sample_rate: u32,
        resource_root: Option<&Path>,
    ) -> Self {
        let mut params = track
            .preset
            .as_deref()
            .and_then(|name| presets.iter().find(|preset| preset.name == name))
            .map(|preset| preset.params.clone())
            .unwrap_or_default();
        if let Some(overrides) = &track.engine_params {
            params.extend(overrides.iter().cloned());
        }
        let mut profile = Self::from_preset_params_at(&params, sample_rate, resource_root);
        profile.set_output_port(
            track.output_port.map(usize::from),
            track.output_pair.map(usize::from),
        );
        profile
    }

    /// Build a profile from a preset parameter list at the sample rate.
    pub fn from_preset_params(params: &[(String, ParamValue)], sample_rate: u32) -> Self {
        Self::from_preset_params_at(params, sample_rate, None)
    }

    /// Resolve profile resources against the same project root used by preparation.
    pub fn from_preset_params_at(
        params: &[(String, ParamValue)],
        sample_rate: u32,
        resource_root: Option<&Path>,
    ) -> Self {
        let mut resources = profile_resources::ProfileResources::new(resource_root);
        Self::from_preset_params_with_resources(params, sample_rate, &mut resources)
    }

    fn from_preset_params_with_resources(
        params: &[(String, ParamValue)],
        sample_rate: u32,
        resources: &mut profile_resources::ProfileResources,
    ) -> Self {
        let mut profile = params
            .iter()
            .find_map(|(name, value)| {
                if !is_profile_manifest_key(&normalize_key(name)) {
                    return None;
                }
                let ParamValue::String(path) = value else {
                    return None;
                };
                resources.manifest(path, sample_rate).ok()
            })
            .unwrap_or_else(|| Self::empty_with_sample_rate(sample_rate));

        let scala_root_note = params
            .iter()
            .find_map(|(name, value)| {
                ProfileKeys::new(&normalize_key(name))
                    .contains(ProfileKeyKind::ScalaRoot)
                    .then(|| param_value_as_midi_note(value))
                    .flatten()
            })
            .unwrap_or(60);

        for (name, value) in params {
            let normalized = normalize_key(name);
            let keys = ProfileKeys::new(&normalized);
            if is_profile_manifest_key(&normalized) {
                continue;
            }

            if keys.contains(ProfileKeyKind::ScalaFile) {
                if let ParamValue::String(path) = value
                    && let Ok(tunings) = resources.scala(path, scala_root_note)
                {
                    profile.note_tunings_cents.extend(tunings);
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::TunFile) {
                if let ParamValue::String(path) = value
                    && let Ok(tunings) = resources.tun(path)
                {
                    profile.note_tunings_cents.extend(tunings);
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::KeyswitchDuration) {
                if let Some(ms) = param_value_as_f64(value) {
                    profile.keyswitch_duration_samples =
                        (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64;
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::OutputPair) {
                if let Some(pair) = declared_output_pair(value) {
                    profile.output_pair = Some(pair);
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::MpeEnabled) {
                if let Some(enabled) = param_value_as_bool(value) {
                    profile.mpe_enabled = enabled;
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::MpeFirstChannel) {
                if let Some(channel) = param_value_as_u7(value) {
                    profile.mpe_first_channel = channel.clamp(1, 15);
                }
                continue;
            }

            if keys.contains(ProfileKeyKind::MpeLastChannel) {
                if let Some(channel) = param_value_as_u7(value) {
                    profile.mpe_last_channel = channel.clamp(1, 15);
                }
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtRoundRobin)
                && let Some(notes) = param_value_as_midi_note_list(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .round_robin_keyswitches = notes;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtKeyswitch)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile.articulation_profile_mut(articulation).keyswitch = Some(note);
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtCcValue)
                && let Some(cc_value) = param_value_as_u7(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .control
                    .get_or_insert(ArticulationControl {
                        controller: 0,
                        value: cc_value,
                        reset_value: None,
                    })
                    .value = cc_value;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtCcReset)
                && let Some(reset_value) = param_value_as_u7(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .control
                    .get_or_insert(ArticulationControl {
                        controller: 0,
                        value: 127,
                        reset_value: Some(reset_value),
                    })
                    .reset_value = Some(reset_value);
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtCc)
                && let Some(controller) = param_value_as_u7(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .control
                    .get_or_insert(ArticulationControl {
                        controller,
                        value: 127,
                        reset_value: None,
                    })
                    .controller = controller;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtParamValue)
                && let Some(param_value) = param_value_as_normalized(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .param
                    .get_or_insert_with(|| ArticulationParam {
                        name: String::new(),
                        value: param_value,
                        reset_value: None,
                    })
                    .value = param_value;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtParamReset)
                && let Some(reset_value) = param_value_as_normalized(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .param
                    .get_or_insert_with(|| ArticulationParam {
                        name: String::new(),
                        value: 1.0,
                        reset_value: Some(reset_value),
                    })
                    .reset_value = Some(reset_value);
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtParam)
                && let ParamValue::String(param_name) = value
            {
                profile
                    .articulation_profile_mut(articulation)
                    .param
                    .get_or_insert_with(|| ArticulationParam {
                        name: param_name.clone(),
                        value: 1.0,
                        reset_value: None,
                    })
                    .name = param_name.clone();
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtDurationScale)
                && let Some(scale) = param_value_as_f64(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .duration_scale = Some(scale.max(0.0));
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtHumanizeTiming)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .humanize
                    .get_or_insert(ArticulationHumanize {
                        timing_samples: 0,
                        velocity: 0,
                    })
                    .timing_samples = (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtHumanizeVelocity)
                && let Some(velocity) = param_value_as_u7(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .humanize
                    .get_or_insert(ArticulationHumanize {
                        timing_samples: 0,
                        velocity,
                    })
                    .velocity = velocity;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtReleaseNote)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .release_trigger
                    .get_or_insert(ArticulationReleaseTrigger {
                        note,
                        velocity: 1,
                        duration_samples: (sample_rate as f64 * 0.05) as u64,
                        sustain_aware: false,
                    })
                    .note = note;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtReleaseVelocity)
                && let Some(velocity) = param_value_as_u7(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .release_trigger
                    .get_or_insert(ArticulationReleaseTrigger {
                        note: 0,
                        velocity,
                        duration_samples: (sample_rate as f64 * 0.05) as u64,
                        sustain_aware: false,
                    })
                    .velocity = velocity.max(1);
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtReleaseDuration)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .release_trigger
                    .get_or_insert(ArticulationReleaseTrigger {
                        note: 0,
                        velocity: 1,
                        duration_samples: (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64,
                        sustain_aware: false,
                    })
                    .duration_samples = (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtReleaseSustainAware)
                && let Some(enabled) = param_value_as_bool(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .release_trigger
                    .get_or_insert(ArticulationReleaseTrigger {
                        note: 0,
                        velocity: 1,
                        duration_samples: (sample_rate as f64 * 0.05) as u64,
                        sustain_aware: enabled,
                    })
                    .sustain_aware = enabled;
                continue;
            }

            if let Some(articulation) = keys.target(ProfileKeyKind::ArtOverlap)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile
                    .articulation_profile_mut(articulation)
                    .overlap_samples = Some((sample_rate as f64 * ms.max(0.0) / 1000.0) as u64);
                continue;
            }

            if let Some(note_key) = keys.target(ProfileKeyKind::NoteTuning)
                && let Some(note) = parse_midi_note(note_key)
                && let Some(cents) = param_value_as_f64(value)
            {
                profile.note_tunings_cents.insert(note, cents);
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlCc14)
                && let Some(cc) = param_value_as_u7(value)
            {
                profile.control_cc14.insert(control.to_string(), cc.min(31));
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlCc)
                && let Some(cc) = param_value_as_u7(value)
            {
                profile.control_cc.insert(control.to_string(), cc);
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlParam)
                && let ParamValue::String(param_name) = value
            {
                profile
                    .control_params
                    .insert(control.to_string(), param_name.clone());
                continue;
            }

            if let Some(controller) = keys.controller()
                && let ParamValue::String(param_name) = value
            {
                profile.cc_params.insert(controller, param_name.clone());
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlNoteExpression)
                && let ParamValue::String(expression) = value
            {
                profile
                    .control_note_expressions
                    .insert(control.to_string(), expression.clone());
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlOnNote)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .on_note = Some(note);
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlOffNote)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .off_note = Some(note);
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlNoiseVelocity)
                && let Some(velocity) = param_value_as_u7(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .velocity = velocity;
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlNoiseDuration)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .duration_samples = (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64;
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlNoiseThreshold)
                && let Some(threshold) = param_value_as_normalized(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .threshold = threshold.clamp(0.0, 1.0);
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlRangeMin)
                && let Some(value) = param_value_as_normalized(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .range_min = Some(value.clamp(0.0, 1.0));
                continue;
            }

            if let Some(control) = keys.target(ProfileKeyKind::ControlRangeMax)
                && let Some(value) = param_value_as_normalized(value)
            {
                profile
                    .control_noise_trigger_mut(control, sample_rate)
                    .range_max = Some(value.clamp(0.0, 1.0));
                continue;
            }

            if let Some(drum) = keys.target(ProfileKeyKind::DrumNote)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile.drum_notes.insert(drum.to_string(), note);
                continue;
            }

            // A `@kit` pad carries the note a hit on the pad name plays.
            if let Some(pad) = keys.target(ProfileKeyKind::PadNote)
                && let Some(note) = param_value_as_midi_note(value)
            {
                profile.drum_notes.entry(pad.to_string()).or_insert(note);
                continue;
            }

            if let Some(drum) = keys.target(ProfileKeyKind::DrumChokeGroup)
                && let ParamValue::String(group) = value
            {
                profile
                    .drum_choke_groups
                    .insert(drum.to_string(), normalize_key(group));
                continue;
            }

            if let Some(drum) = keys.target(ProfileKeyKind::DrumChokeRelease)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile.drum_choke_release_samples.insert(
                    drum.to_string(),
                    (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64,
                );
                continue;
            }

            if let Some(group) = keys.target(ProfileKeyKind::ChokeRelease)
                && let Some(ms) = param_value_as_f64(value)
            {
                profile.drum_choke_release_samples.insert(
                    group.to_string(),
                    (sample_rate as f64 * ms.max(0.0) / 1000.0) as u64,
                );
            }
        }

        profile
    }

    /// Load a profile manifest from a file path.
    pub fn from_manifest_file(
        path: impl AsRef<Path>,
        sample_rate: u32,
    ) -> Result<Self, ProfileManifestError> {
        let mut resources = profile_resources::ProfileResources::new(None);
        resources
            .manifest_path(path.as_ref(), sample_rate)
            .map_err(ProfileManifestError::Resource)
    }

    /// Parse a JSON profile manifest, bounded by the input limit.
    pub fn from_manifest_json_str(
        source: &str,
        sample_rate: u32,
    ) -> Result<Self, ProfileManifestError> {
        profile_resources::check_source_limit(source).map_err(ProfileManifestError::Resource)?;
        let value: serde_json::Value = serde_json::from_str(source)?;
        let params = json_manifest_params(value)?;
        profile_resources::ProfileResources::new(None)
            .manifest_params(&params, sample_rate)
            .map_err(ProfileManifestError::Resource)
    }

    /// Parse a TOML profile manifest, bounded by the input limit.
    pub fn from_manifest_toml_str(
        source: &str,
        sample_rate: u32,
    ) -> Result<Self, ProfileManifestError> {
        profile_resources::check_source_limit(source).map_err(ProfileManifestError::Resource)?;
        let value: toml::Value = toml::from_str(source)?;
        let params = toml_manifest_params(value)?;
        profile_resources::ProfileResources::new(None)
            .manifest_params(&params, sample_rate)
            .map_err(ProfileManifestError::Resource)
    }

    /// Whether the profile maps nothing.
    pub fn is_empty(&self) -> bool {
        self.articulation_profiles.is_empty()
            && self.control_cc.is_empty()
            && self.control_cc14.is_empty()
            && self.control_params.is_empty()
            && self.cc_params.is_empty()
            && self.control_note_expressions.is_empty()
            && self.control_noise_triggers.is_empty()
            && self.drum_notes.is_empty()
            && self.drum_choke_groups.is_empty()
            && self.drum_choke_release_samples.is_empty()
            && self.output_pair.is_none()
            && self.output_port.is_none()
            && self.output_port_pair.is_none()
            && !self.mpe_enabled
    }

    /// Profile for an articulation, if declared.
    pub fn articulation_profile(&self, articulation: Articulation) -> Option<&ArticulationProfile> {
        self.articulation_profiles
            .get(&articulation_key(articulation))
    }

    /// Keyswitch note for an articulation.
    pub fn keyswitch_for_articulation(&self, articulation: Articulation) -> Option<u8> {
        self.articulation_profile(articulation)?.keyswitch
    }

    /// Keyswitch for one occurrence, cycling round-robin keyswitches.
    pub fn keyswitch_for_articulation_occurrence(
        &self,
        articulation: Articulation,
        occurrence: usize,
    ) -> Option<u8> {
        let profile = self.articulation_profile(articulation)?;
        if profile.round_robin_keyswitches.is_empty() {
            profile.keyswitch
        } else {
            Some(
                profile.round_robin_keyswitches[occurrence % profile.round_robin_keyswitches.len()],
            )
        }
    }

    /// Controller applied while the articulation plays.
    pub fn articulation_control(&self, articulation: Articulation) -> Option<ArticulationControl> {
        self.articulation_profile(articulation)?.control
    }

    /// Parameter applied while the articulation plays.
    pub fn articulation_param(&self, articulation: Articulation) -> Option<&ArticulationParam> {
        self.articulation_profile(articulation)?.param.as_ref()
    }

    /// Timing and velocity offsets for the articulation.
    pub fn articulation_humanize(
        &self,
        articulation: Articulation,
    ) -> Option<ArticulationHumanize> {
        self.articulation_profile(articulation)?.humanize
    }

    /// Release sample trigger for the articulation.
    pub fn articulation_release_trigger(
        &self,
        articulation: Articulation,
    ) -> Option<ArticulationReleaseTrigger> {
        self.articulation_profile(articulation)?.release_trigger
    }

    /// Duration scale for the articulation.
    pub fn duration_scale_for_articulation(&self, articulation: Articulation) -> Option<f64> {
        self.articulation_profile(articulation)?.duration_scale
    }

    /// Overlap between consecutive notes of the articulation.
    pub fn overlap_samples_for_articulation(&self, articulation: Articulation) -> Option<u64> {
        self.articulation_profile(articulation)?.overlap_samples
    }

    /// CC controller mapped to a control name.
    pub fn control_cc(&self, name: &str) -> Option<u8> {
        self.control_cc.get(&normalize_key(name)).copied()
    }

    /// 14-bit CC controller mapped to a control name.
    pub fn control_cc14(&self, name: &str) -> Option<u8> {
        self.control_cc14.get(&normalize_key(name)).copied()
    }

    /// Parameter mapped to a control name.
    pub fn control_param(&self, name: &str) -> Option<&str> {
        self.control_params
            .get(&normalize_key(name))
            .map(String::as_str)
    }

    /// Parameter mapped to a controller number.
    pub fn cc_param(&self, controller: u8) -> Option<&str> {
        self.cc_params.get(&controller).map(String::as_str)
    }

    /// Note expression mapped to a control name.
    pub fn control_note_expression(&self, name: &str) -> Option<&str> {
        self.control_note_expressions
            .get(&normalize_key(name))
            .map(String::as_str)
    }

    /// Noise trigger mapped to a control name.
    pub fn control_noise_trigger(&self, name: &str) -> Option<ControlNoiseTrigger> {
        self.control_noise_triggers
            .get(&normalize_key(name))
            .copied()
    }

    /// True when the instrument declares a pad grid, so a hit name addresses a
    /// pad and a General MIDI name is a fallback rather than the normal path.
    pub fn has_pad_grid(&self) -> bool {
        !self.drum_notes.is_empty()
    }

    /// True when some pad of the grid answers `note`, so a hit on it sounds.
    pub fn has_pad_note(&self, note: u8) -> bool {
        self.drum_notes.values().any(|held| *held == note)
    }

    /// Pad note for a drum name, with articulation override.
    pub fn drum_note(&self, name: &str, articulation: Option<&str>) -> Option<u8> {
        let normalized_name = normalize_key(name);
        if let Some(articulation) = articulation {
            let key = format!("{}{}", normalized_name, normalize_key(articulation));
            if let Some(note) = self.drum_notes.get(&key) {
                return Some(*note);
            }
        }
        self.drum_notes.get(&normalized_name).copied()
    }

    /// Choke group for a drum name, with articulation override.
    pub fn drum_choke_group(&self, name: &str, articulation: Option<&str>) -> Option<&str> {
        let normalized_name = normalize_key(name);
        if let Some(articulation) = articulation {
            let key = format!("{}{}", normalized_name, normalize_key(articulation));
            if let Some(group) = self.drum_choke_groups.get(&key) {
                return Some(group);
            }
        }
        self.drum_choke_groups
            .get(&normalized_name)
            .map(String::as_str)
    }

    /// Release time for a choke group.
    pub fn drum_choke_release_samples(
        &self,
        name: &str,
        articulation: Option<&str>,
        group: &str,
    ) -> u64 {
        let normalized_name = normalize_key(name);
        if let Some(articulation) = articulation {
            let key = format!("{}{}", normalized_name, normalize_key(articulation));
            if let Some(samples) = self.drum_choke_release_samples.get(&key) {
                return *samples;
            }
        }
        self.drum_choke_release_samples
            .get(&normalized_name)
            .or_else(|| self.drum_choke_release_samples.get(&normalize_key(group)))
            .copied()
            .unwrap_or(0)
    }

    /// Per-note tuning offset.
    pub fn note_tuning_cents(&self, note: u8) -> Option<f64> {
        self.note_tunings_cents.get(&note).copied()
    }

    /// Absolute stereo pair the legacy parameter names; 0 when unnamed.
    pub fn output_pair(&self) -> usize {
        self.output_pair.unwrap_or(0)
    }

    /// Declared output port, 0-based.
    pub fn output_port(&self) -> Option<usize> {
        self.output_port
    }

    /// Stereo pair inside the declared output port.
    pub fn output_port_pair(&self) -> Option<usize> {
        self.output_port_pair
    }

    /// True when the track asks for an output other than the first pair.
    ///
    /// A fallback to the first pair only loses something then: a track that
    /// asks for the main pair already gets it, so that fallback is not a
    /// routing problem worth a warning.
    pub fn requests_non_main_output(&self) -> bool {
        self.output_port.is_some_and(|port| port != 0)
            || self.output_port_pair.is_some_and(|pair| pair != 0)
            || (self.output_port.is_none() && self.output_pair.is_some_and(|pair| pair != 0))
    }

    /// Record the track's first-class `port:` / `pair:` routing.
    pub fn set_output_port(&mut self, port: Option<usize>, pair: Option<usize>) {
        self.output_port = port;
        self.output_port_pair = pair;
    }

    /// MPE channel for a note ordinal, cycling the member channels.
    pub fn mpe_channel_for_note(&self, note_ordinal: usize) -> Option<u8> {
        if !self.mpe_enabled {
            return None;
        }
        let first = self.mpe_first_channel.min(self.mpe_last_channel);
        let last = self.mpe_last_channel.max(first);
        let count = (last - first + 1) as usize;
        Some(first + (note_ordinal % count) as u8)
    }

    fn articulation_profile_mut(&mut self, articulation: &str) -> &mut ArticulationProfile {
        self.articulation_profiles
            .entry(articulation.to_string())
            .or_default()
    }

    fn control_noise_trigger_mut(
        &mut self,
        control: &str,
        sample_rate: u32,
    ) -> &mut ControlNoiseTrigger {
        self.control_noise_triggers
            .entry(control.to_string())
            .or_insert(ControlNoiseTrigger {
                on_note: None,
                off_note: None,
                velocity: 32,
                duration_samples: (sample_rate as f64 * 0.05) as u64,
                threshold: 0.5,
                range_min: None,
                range_max: None,
            })
    }

    pub(crate) fn empty_with_sample_rate(sample_rate: u32) -> Self {
        Self {
            articulation_profiles: HashMap::new(),
            control_cc: HashMap::new(),
            control_cc14: HashMap::new(),
            control_params: HashMap::new(),
            cc_params: HashMap::new(),
            control_note_expressions: HashMap::new(),
            control_noise_triggers: HashMap::new(),
            drum_notes: HashMap::new(),
            drum_choke_groups: HashMap::new(),
            drum_choke_release_samples: HashMap::new(),
            note_tunings_cents: HashMap::new(),
            output_pair: None,
            output_port: None,
            output_port_pair: None,
            mpe_enabled: false,
            mpe_first_channel: 1,
            mpe_last_channel: 15,
            keyswitch_duration_samples: (sample_rate as f64 * 0.05) as u64,
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Why a profile manifest could not be loaded.
pub enum ProfileManifestError {
    #[error("{0}")]
    Resource(#[source] ProfileAssignmentError),
    #[error("failed to read profile manifest: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse JSON profile manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to parse TOML profile manifest: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("profile manifest must be a flat key/value map")]
    InvalidShape,
}

/// Normalized profile key for an articulation.
pub fn articulation_key(articulation: Articulation) -> String {
    normalize_key(&format!("{:?}", articulation))
}

pub(crate) fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_profile_manifest_key(normalized: &str) -> bool {
    ProfileKeys::new(normalized).contains(ProfileKeyKind::Manifest)
}

fn json_manifest_params(
    value: serde_json::Value,
) -> Result<Vec<(String, ParamValue)>, ProfileManifestError> {
    let serde_json::Value::Object(map) = value else {
        return Err(ProfileManifestError::InvalidShape);
    };
    map.into_iter()
        .map(|(key, value)| Ok((key, json_value_to_param(value)?)))
        .collect()
}

fn json_value_to_param(value: serde_json::Value) -> Result<ParamValue, ProfileManifestError> {
    match value {
        serde_json::Value::Bool(value) => Ok(ParamValue::Bool(value)),
        serde_json::Value::Number(value) => value
            .as_f64()
            .map(ParamValue::Number)
            .ok_or(ProfileManifestError::InvalidShape),
        serde_json::Value::String(value) => Ok(ParamValue::String(value)),
        _ => Err(ProfileManifestError::InvalidShape),
    }
}

fn toml_manifest_params(
    value: toml::Value,
) -> Result<Vec<(String, ParamValue)>, ProfileManifestError> {
    let toml::Value::Table(map) = value else {
        return Err(ProfileManifestError::InvalidShape);
    };
    map.into_iter()
        .map(|(key, value)| Ok((key, toml_value_to_param(value)?)))
        .collect()
}

fn toml_value_to_param(value: toml::Value) -> Result<ParamValue, ProfileManifestError> {
    match value {
        toml::Value::Boolean(value) => Ok(ParamValue::Bool(value)),
        toml::Value::Integer(value) => Ok(ParamValue::Number(value as f64)),
        toml::Value::Float(value) => Ok(ParamValue::Number(value)),
        toml::Value::String(value) => Ok(ParamValue::String(value)),
        _ => Err(ProfileManifestError::InvalidShape),
    }
}

fn param_value_as_u7(value: &ParamValue) -> Option<u8> {
    param_value_as_f64(value).map(|value| value.clamp(0.0, 127.0) as u8)
}

fn param_value_as_midi_note(value: &ParamValue) -> Option<u8> {
    match value {
        ParamValue::String(value) => parse_midi_note(value),
        _ => param_value_as_u7(value),
    }
}

fn param_value_as_midi_note_list(value: &ParamValue) -> Option<Vec<u8>> {
    let ParamValue::String(value) = value else {
        return param_value_as_midi_note(value).map(|note| vec![note]);
    };
    let notes: Vec<_> = value
        .split([',', ' ', ';'])
        .filter(|part| !part.trim().is_empty())
        .map(parse_midi_note)
        .collect::<Option<_>>()?;
    (!notes.is_empty()).then_some(notes)
}

fn declared_output_pair(value: &ParamValue) -> Option<usize> {
    param_value_as_f64(value).map(|pair| pair.max(1.0).floor() as usize - 1)
}

/// Admit an exact declared output selection through the profile consumer's
/// existing mapping. Legacy zero and one both select its first pair.
pub fn checked_output_pair(value: &ParamValue) -> Result<usize, ProfileAssignmentError> {
    let number = param_value_as_f64(value).ok_or(ProfileAssignmentError::InvalidValue)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(ProfileAssignmentError::InvalidValue);
    }
    declared_output_pair(value).ok_or(ProfileAssignmentError::InvalidValue)
}

/// A one-pair backend can claim only its first checked output selection.
pub fn validate_main_output_pair(value: &ParamValue) -> Result<(), ProfileAssignmentError> {
    if checked_output_pair(value)? != 0 {
        return Err(ProfileAssignmentError::InvalidValue);
    }
    Ok(())
}

fn param_value_as_f64(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Number(n) => Some(*n),
        ParamValue::Db(n) => Some(*n),
        ParamValue::Percentage(n) => Some(*n * 127.0 / 100.0),
        ParamValue::Frequency(n) => Some(*n),
        ParamValue::Time(n) => Some(*n),
        ParamValue::Cents(n) => Some(*n),
        ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        ParamValue::String(s) => s.parse::<f64>().ok(),
        ParamValue::Array(_) => None,
    }
}

fn param_value_as_bool(value: &ParamValue) -> Option<bool> {
    match value {
        ParamValue::Bool(value) => Some(*value),
        ParamValue::String(value) => match normalize_key(value).as_str() {
            "true" | "yes" | "on" | "enabled" => Some(true),
            "false" | "no" | "off" | "disabled" => Some(false),
            _ => None,
        },
        _ => param_value_as_f64(value).map(|value| value > 0.0),
    }
}

fn param_value_as_normalized(value: &ParamValue) -> Option<f64> {
    match value {
        ParamValue::Percentage(n) => Some((*n / 100.0).clamp(0.0, 1.0)),
        _ => param_value_as_f64(value).map(|value| {
            if (0.0..=1.0).contains(&value) {
                value
            } else {
                (value / 127.0).clamp(0.0, 1.0)
            }
        }),
    }
}

fn parse_midi_note(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Ok(note) = value.parse::<u8>() {
        return Some(note.min(127));
    }

    let mut chars = value.chars();
    let note = chars.next()?.to_ascii_uppercase();
    let base: i16 = match note {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };

    let rest: String = chars.collect();
    let (accidental, octave) = if let Some(octave) = rest.strip_prefix('#') {
        (1, octave)
    } else if let Some(octave) = rest.strip_prefix('b') {
        (-1, octave)
    } else {
        (0, rest.as_str())
    };
    let octave = octave.parse::<i16>().ok()?;
    let midi = (octave + 1) * 12 + base + accidental;
    u8::try_from(midi).ok().filter(|note| *note <= 127)
}

fn scala_tunings_from_str(source: &str, root_note: u8) -> Result<HashMap<u8, f64>, String> {
    let entries: Vec<_> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('!'))
        .collect();
    if entries.len() < 2 {
        return Err("Scala tuning requires description and note count".to_string());
    }
    let count: usize = entries[1]
        .parse()
        .map_err(|_| "Scala tuning has invalid note count".to_string())?;
    if count == 0 || count > entries.len().saturating_sub(2) {
        return Err("Scala tuning has fewer intervals than declared".to_string());
    }

    let mut tunings = HashMap::new();
    tunings.insert(root_note, 0.0);
    for (degree, entry) in entries.iter().skip(2).take(count).enumerate() {
        let cents = parse_scala_interval_cents(entry)?;
        if !cents.is_finite() {
            return Err("Scala tuning interval must be finite".into());
        }
        let note = root_note as usize + degree + 1;
        if note <= 127 {
            // CLAP/VST3 tuning expression expects deviation from 12-TET in cents.
            let expected = (degree + 1) as f64 * 100.0;
            tunings.insert(note as u8, cents - expected);
        }
    }
    Ok(tunings)
}

fn tun_tunings_from_str(source: &str) -> Result<HashMap<u8, f64>, String> {
    let mut tunings = HashMap::new();
    for line in source.lines().map(str::trim) {
        if line.is_empty()
            || line.starts_with('!')
            || line.starts_with('#')
            || line.starts_with(';')
            || (line.starts_with('[') && line.ends_with(']'))
        {
            continue;
        }
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<_> = if line.contains('=') {
            line.splitn(2, '=').collect()
        } else if line.contains(':') {
            line.splitn(2, ':').collect()
        } else {
            line.split_whitespace().take(2).collect()
        };
        if parts.len() != 2 {
            continue;
        }

        let note_key = normalize_tun_note_key(parts[0]);
        let Some(note) = parse_midi_note(&note_key) else {
            continue;
        };
        let cents_text = parts[1].trim().trim_end_matches("cents").trim();
        let cents: f64 = cents_text
            .parse()
            .map_err(|_| format!("Invalid .tun cents value: {}", parts[1].trim()))?;
        if !cents.is_finite() {
            return Err("Tuning value must be finite".into());
        }
        tunings.insert(note, cents);
    }
    if tunings.is_empty() {
        return Err(".tun file did not contain note tuning entries".to_string());
    }
    Ok(tunings)
}

fn normalize_tun_note_key(value: &str) -> String {
    let value = value.trim();
    for prefix in ["note", "midi", "key"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(['_', '-', ' ']);
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    value.to_string()
}

fn parse_scala_interval_cents(value: &str) -> Result<f64, String> {
    if let Some((numerator, denominator)) = value.split_once('/') {
        let numerator: f64 = numerator
            .trim()
            .parse()
            .map_err(|_| format!("Invalid Scala ratio numerator: {}", value))?;
        let denominator: f64 = denominator
            .trim()
            .parse()
            .map_err(|_| format!("Invalid Scala ratio denominator: {}", value))?;
        if numerator <= 0.0 || denominator <= 0.0 {
            return Err(format!("Invalid Scala ratio: {}", value));
        }
        return Ok(1200.0 * (numerator / denominator).log2());
    }
    if value.contains('.') {
        return value
            .parse()
            .map_err(|_| format!("Invalid Scala cents value: {}", value));
    }
    let ratio: f64 = value
        .parse()
        .map_err(|_| format!("Invalid Scala interval: {}", value))?;
    if ratio <= 0.0 {
        return Err(format!("Invalid Scala interval: {}", value));
    }
    Ok(1200.0 * ratio.log2())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_reads_keyswitch_and_cc_params() {
        let profile = InstrumentProfile::from_preset_params(
            &[
                (
                    "articulation.legato.keyswitch".to_string(),
                    ParamValue::String("C0".to_string()),
                ),
                (
                    "articulation.legato.round_robin_keyswitches".to_string(),
                    ParamValue::String("C0 C#0 D0".to_string()),
                ),
                ("cc.dynamics".to_string(), ParamValue::Number(1.0)),
                (
                    "control.brightness.cc".to_string(),
                    ParamValue::Number(74.0),
                ),
                ("control.dynamics.cc14".to_string(), ParamValue::Number(1.0)),
                (
                    "param.dynamics".to_string(),
                    ParamValue::String("Expression".to_string()),
                ),
                (
                    "control.brightness.note_expression".to_string(),
                    ParamValue::String("brightness".to_string()),
                ),
                (
                    "control.sustain.on_note".to_string(),
                    ParamValue::String("C0".to_string()),
                ),
                (
                    "control.sustain.off_note".to_string(),
                    ParamValue::String("C#0".to_string()),
                ),
                (
                    "control.sustain.noise_velocity".to_string(),
                    ParamValue::Number(20.0),
                ),
                (
                    "control.sustain.noise_duration_ms".to_string(),
                    ParamValue::Number(25.0),
                ),
                (
                    "control.sustain.noise_threshold".to_string(),
                    ParamValue::Percentage(40.0),
                ),
                (
                    "control.sustain.range_min".to_string(),
                    ParamValue::Percentage(25.0),
                ),
                (
                    "control.sustain.range_max".to_string(),
                    ParamValue::Percentage(75.0),
                ),
                (
                    "cc.64.param".to_string(),
                    ParamValue::String("Sustain Pedal".to_string()),
                ),
                (
                    "articulation.legato.cc".to_string(),
                    ParamValue::Number(68.0),
                ),
                (
                    "articulation.legato.cc_value".to_string(),
                    ParamValue::Number(127.0),
                ),
                (
                    "articulation.legato.cc_reset".to_string(),
                    ParamValue::Number(0.0),
                ),
                (
                    "articulation.legato.duration_scale".to_string(),
                    ParamValue::Number(1.1),
                ),
                (
                    "articulation.legato.humanize_timing_ms".to_string(),
                    ParamValue::Number(8.0),
                ),
                (
                    "articulation.legato.humanize_velocity".to_string(),
                    ParamValue::Number(5.0),
                ),
                (
                    "articulation.legato.release_note".to_string(),
                    ParamValue::String("D0".to_string()),
                ),
                (
                    "articulation.legato.release_velocity".to_string(),
                    ParamValue::Number(12.0),
                ),
                (
                    "articulation.legato.release_duration_ms".to_string(),
                    ParamValue::Number(30.0),
                ),
                (
                    "articulation.legato.release_sustain_aware".to_string(),
                    ParamValue::Bool(true),
                ),
                (
                    "articulation.legato.param".to_string(),
                    ParamValue::String("Mode".to_string()),
                ),
                (
                    "articulation.legato.param_value".to_string(),
                    ParamValue::Percentage(50.0),
                ),
                (
                    "articulation.legato.param_reset".to_string(),
                    ParamValue::Number(0.0),
                ),
                (
                    "articulation.legato.overlap_ms".to_string(),
                    ParamValue::Number(25.0),
                ),
                (
                    "microtuning.C4.cents".to_string(),
                    ParamValue::Number(-12.5),
                ),
                ("tuning.61.cents".to_string(), ParamValue::Number(8.0)),
                ("drum.kick.note".to_string(), ParamValue::Number(24.0)),
                ("drum.hihat.open.note".to_string(), ParamValue::Number(26.0)),
                (
                    "drum.hihat.open.choke_group".to_string(),
                    ParamValue::String("hihat".to_string()),
                ),
                (
                    "choke.hihat.release_ms".to_string(),
                    ParamValue::Number(25.0),
                ),
                ("plugin_output_pair".to_string(), ParamValue::Number(2.0)),
                ("mpe_enabled".to_string(), ParamValue::Bool(true)),
                ("mpe_first_channel".to_string(), ParamValue::Number(2.0)),
                ("mpe_last_channel".to_string(), ParamValue::Number(4.0)),
            ],
            48_000,
        );

        assert_eq!(
            profile.keyswitch_for_articulation(Articulation::Legato),
            Some(12)
        );
        assert_eq!(
            profile.keyswitch_for_articulation_occurrence(Articulation::Legato, 0),
            Some(12)
        );
        assert_eq!(
            profile.keyswitch_for_articulation_occurrence(Articulation::Legato, 1),
            Some(13)
        );
        assert_eq!(
            profile.keyswitch_for_articulation_occurrence(Articulation::Legato, 2),
            Some(14)
        );
        assert_eq!(
            profile.keyswitch_for_articulation_occurrence(Articulation::Legato, 3),
            Some(12)
        );
        assert_eq!(profile.control_cc("dynamics"), Some(1));
        assert_eq!(profile.control_cc("brightness"), Some(74));
        assert_eq!(profile.control_cc14("dynamics"), Some(1));
        assert_eq!(profile.control_param("dynamics"), Some("Expression"));
        assert_eq!(profile.cc_param(64), Some("Sustain Pedal"));
        assert_eq!(
            profile.control_note_expression("brightness"),
            Some("brightness")
        );
        assert_eq!(
            profile.control_noise_trigger("sustain"),
            Some(ControlNoiseTrigger {
                on_note: Some(12),
                off_note: Some(13),
                velocity: 20,
                duration_samples: 1200,
                threshold: 0.4,
                range_min: Some(0.25),
                range_max: Some(0.75),
            })
        );
        assert_eq!(profile.drum_note("kick", None), Some(24));
        assert_eq!(profile.drum_note("hihat", Some("open")), Some(26));
        assert_eq!(profile.drum_note("hihat", Some("closed")), None);
        assert_eq!(
            profile.drum_choke_group("hihat", Some("open")),
            Some("hihat")
        );
        assert_eq!(profile.drum_choke_group("hihat", Some("closed")), None);
        assert_eq!(
            profile.drum_choke_release_samples("hihat", Some("open"), "hihat"),
            1200
        );
        assert_eq!(profile.note_tuning_cents(60), Some(-12.5));
        assert_eq!(profile.note_tuning_cents(61), Some(8.0));
        assert_eq!(profile.output_pair(), 1);
        assert_eq!(profile.mpe_channel_for_note(0), Some(2));
        assert_eq!(profile.mpe_channel_for_note(2), Some(4));
        assert_eq!(profile.mpe_channel_for_note(3), Some(2));
        assert_eq!(
            profile.articulation_control(Articulation::Legato),
            Some(ArticulationControl {
                controller: 68,
                value: 127,
                reset_value: Some(0),
            })
        );
        assert_eq!(
            profile.articulation_param(Articulation::Legato),
            Some(&ArticulationParam {
                name: "Mode".to_string(),
                value: 0.5,
                reset_value: Some(0.0),
            })
        );
        assert_eq!(
            profile.duration_scale_for_articulation(Articulation::Legato),
            Some(1.1)
        );
        assert_eq!(
            profile.articulation_humanize(Articulation::Legato),
            Some(ArticulationHumanize {
                timing_samples: 384,
                velocity: 5,
            })
        );
        assert_eq!(
            profile.articulation_release_trigger(Articulation::Legato),
            Some(ArticulationReleaseTrigger {
                note: 14,
                velocity: 12,
                duration_samples: 1440,
                sustain_aware: true,
            })
        );
        assert_eq!(
            profile.overlap_samples_for_articulation(Articulation::Legato),
            Some(1200)
        );
    }

    #[test]
    fn scala_tuning_maps_intervals_to_note_offsets() {
        let tunings = scala_tunings_from_str(
            r#"! test scale
five limit
3
100.0
5/4
2/1
"#,
            60,
        )
        .unwrap();

        assert_eq!(tunings.get(&60).copied(), Some(0.0));
        assert!((tunings[&61] - 0.0).abs() < 0.0001);
        assert!((tunings[&62] - 186.3137).abs() < 0.001);
        assert!((tunings[&63] - 900.0).abs() < 0.0001);
    }

    #[test]
    fn tun_tuning_reads_note_cents_entries() {
        let tunings = tun_tunings_from_str(
            r#"[Tuning]
note_60 = -12.5
C#4: 7.25 cents
62 3.0
; ignored
"#,
        )
        .unwrap();

        assert_eq!(tunings.get(&60).copied(), Some(-12.5));
        assert_eq!(tunings.get(&61).copied(), Some(7.25));
        assert_eq!(tunings.get(&62).copied(), Some(3.0));
    }

    #[test]
    fn profile_reads_json_and_toml_manifests() {
        let json_profile = InstrumentProfile::from_manifest_json_str(
            r#"{
                "articulation_staccato_round_robin_keyswitches": "C0 C#0 D0",
                "cc_dynamics": 1,
                "param_brightness": "Brightness"
            }"#,
            44_100,
        )
        .unwrap();
        let toml_profile = InstrumentProfile::from_manifest_toml_str(
            r#"
                articulation_staccato_round_robin_keyswitches = "C0 C#0 D0"
                cc_dynamics = 1
                param_brightness = "Brightness"
            "#,
            44_100,
        )
        .unwrap();

        for profile in [json_profile, toml_profile] {
            assert_eq!(profile.control_cc("dynamics"), Some(1));
            assert_eq!(profile.control_param("brightness"), Some("Brightness"));
            assert_eq!(
                profile.keyswitch_for_articulation_occurrence(Articulation::Staccato, 1),
                Some(13)
            );
        }
    }

    #[test]
    fn preset_params_load_manifest_and_override_values() {
        let path =
            std::env::temp_dir().join(format!("opencompose-profile-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{
                "articulation_legato_keyswitch": "C0",
                "cc_dynamics": 1
            }"#,
        )
        .unwrap();

        let profile = InstrumentProfile::from_preset_params(
            &[
                (
                    "profile_manifest".to_string(),
                    ParamValue::String(path.to_string_lossy().to_string()),
                ),
                ("cc_dynamics".to_string(), ParamValue::Number(2.0)),
            ],
            44_100,
        );
        let _ = std::fs::remove_file(path);

        assert_eq!(
            profile.keyswitch_for_articulation(Articulation::Legato),
            Some(12)
        );
        assert_eq!(profile.control_cc("dynamics"), Some(2));
    }
}
