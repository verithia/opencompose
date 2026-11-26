//! Versioned, integrity-checked envelopes for persisted plugin state, and the
//! explicit VST2-to-VST3 preset migration.
//!
//! A persisted state is `{ format, plugin_id, version, bytes, sha256 }`; the
//! payload stays the plugin's opaque chunk. Bare bytes written before envelopes
//! existed have no magic prefix and load unchanged.

use oc_dsl::ast::{ParamValue, Project};
use oc_dsl::media::{PluginStateBackend, normalized_plugin_state_parameter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use super::preset_parameters::host_parameter_key;
use super::resolution::{PluginQuery, identifier_key, plugin_matches_query};
use super::{PluginFormat, PluginInfo};

/// Magic plus envelope layout byte. Legacy state never starts with this.
const MAGIC: &[u8] = b"OCPSTATE\x01";
const HEADER_LIMIT: usize = 64 * 1024;
const SHA256_HEX_LEN: usize = 64;

/// Envelope version this build writes and the newest it will load.
///
/// Derived from the crate's major/minor version (`major * 100 + minor`): patch
/// releases stay compatible, while a change to the envelope layout or to load
/// semantics bumps the minor version. This is the only counter.
pub fn supported_plugin_state_version() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION").split('.');
    let major: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .expect("CARGO_PKG_VERSION major");
    let minor: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .expect("CARGO_PKG_VERSION minor");
    major.saturating_mul(100).saturating_add(minor)
}

/// The state's target: the format the host loads, the canonical plugin
/// identity, and the preset whose diagnostics name it.
#[derive(Debug, Clone, Copy)]
pub struct PluginStateContext<'a> {
    format: PluginFormat,
    /// Canonical id, normally `PluginInfo.id`; `None` skips the id check.
    plugin_id: Option<&'a str>,
    preset: &'a str,
}

impl<'a> PluginStateContext<'a> {
    /// State context for one plugin and preset.
    pub fn new(format: PluginFormat, plugin_id: Option<&'a str>, preset: &'a str) -> Self {
        Self {
            format,
            plugin_id,
            preset,
        }
    }
    /// Display label for the plugin format.
    pub fn format_label(&self) -> &'static str {
        plugin_format_label(self.format)
    }
}

/// JSON header followed by the opaque payload.
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeHeader {
    format: String,
    plugin_id: String,
    version: u32,
    sha256: String,
}

/// Wrap a plugin chunk in an envelope at the supported version.
pub fn encode_plugin_state(format: PluginFormat, plugin_id: &str, bytes: &[u8]) -> Vec<u8> {
    encode_plugin_state_versioned(format, plugin_id, supported_plugin_state_version(), bytes)
}

/// Wrap a chunk at an explicit version; tests use it to stand in for a newer build.
pub fn encode_plugin_state_versioned(
    format: PluginFormat,
    plugin_id: &str,
    version: u32,
    bytes: &[u8],
) -> Vec<u8> {
    let header = EnvelopeHeader {
        format: plugin_format_name(format).to_string(),
        plugin_id: plugin_id.to_string(),
        version,
        sha256: sha256_hex(bytes),
    };
    let header = serde_json::to_vec(&header).expect("a plugin state header serializes");
    let header_len = u32::try_from(header.len()).expect("a plugin state header fits u32");
    let mut encoded = Vec::with_capacity(MAGIC.len() + 4 + header.len() + bytes.len());
    encoded.extend_from_slice(MAGIC);
    encoded.extend_from_slice(&header_len.to_le_bytes());
    encoded.extend_from_slice(&header);
    encoded.extend_from_slice(bytes);
    encoded
}

/// Return the plugin chunk after checking the envelope, if there is one.
///
/// A bare chunk without the magic prefix is returned unchanged. A newer
/// version, a format or identity mismatch, and a digest mismatch are errors
/// that name `context.preset`; callers resolve state before writing anything.
pub fn decode_plugin_state(
    encoded: &[u8],
    context: &PluginStateContext<'_>,
) -> Result<Vec<u8>, String> {
    if !encoded.starts_with(MAGIC) {
        return Ok(encoded.to_vec());
    }
    let body = &encoded[MAGIC.len()..];
    let (length, rest) = body
        .split_at_checked(4)
        .ok_or_else(|| malformed(context, "the envelope has no header length"))?;
    let header_len = usize::try_from(u32::from_le_bytes(length.try_into().unwrap()))
        .expect("u32 fits usize on supported targets");
    if header_len > HEADER_LIMIT || rest.len() < header_len {
        return Err(malformed(
            context,
            "the envelope header length is out of range",
        ));
    }
    let (header, payload) = rest.split_at(header_len);
    let header: EnvelopeHeader = serde_json::from_slice(header).map_err(|error| {
        malformed(
            context,
            &format!("the envelope header is not valid JSON: {error}"),
        )
    })?;

    let supported = supported_plugin_state_version();
    if header.version > supported {
        return Err(format!(
            "plugin state for preset '{}' was saved by a newer build: state version {}, this build supports version {}; update OpenCompose to open this project, the project was not changed",
            context.preset, header.version, supported
        ));
    }
    let format = plugin_format_from_name(&header.format)
        .ok_or_else(|| malformed(context, &format!("unknown format '{}'", header.format)))?;
    if format != context.format {
        return Err(format!(
            "plugin state for preset '{}' was saved for {} but this preset loads it as {}",
            context.preset,
            plugin_format_label(format),
            context.format_label()
        ));
    }
    if let Some(expected) = context.plugin_id
        && header.plugin_id != expected
        && identifier_key(&header.plugin_id) != identifier_key(expected)
    {
        return Err(format!(
            "plugin state for preset '{}' belongs to plugin '{}' but this preset uses '{}'",
            context.preset, header.plugin_id, expected
        ));
    }
    if header.sha256.len() != SHA256_HEX_LEN
        || !header.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(malformed(
            context,
            "the envelope sha256 is not 64 hexadecimal characters",
        ));
    }
    let actual = sha256_hex(payload);
    if !actual.eq_ignore_ascii_case(&header.sha256) {
        return Err(format!(
            "plugin state for preset '{}' failed its integrity check: sha256 mismatch (envelope {}, payload {}); the project was not changed",
            context.preset, header.sha256, actual
        ));
    }
    Ok(payload.to_vec())
}

fn malformed(context: &PluginStateContext<'_>, reason: &str) -> String {
    format!(
        "plugin state for preset '{}' has a malformed envelope: {reason}",
        context.preset
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn plugin_format_name(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
        PluginFormat::Au => "au",
        PluginFormat::Lv2 => "lv2",
        PluginFormat::Vst2 => "vst2",
        PluginFormat::Internal => "internal",
    }
}

fn plugin_format_from_name(name: &str) -> Option<PluginFormat> {
    match name.to_ascii_lowercase().as_str() {
        "vst3" => Some(PluginFormat::Vst3),
        "clap" => Some(PluginFormat::Clap),
        "au" => Some(PluginFormat::Au),
        "lv2" => Some(PluginFormat::Lv2),
        "vst2" => Some(PluginFormat::Vst2),
        "internal" => Some(PluginFormat::Internal),
        _ => None,
    }
}

fn plugin_format_label(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Clap => "CLAP",
        PluginFormat::Au => "AU",
        PluginFormat::Lv2 => "LV2",
        PluginFormat::Vst2 => "VST2",
        PluginFormat::Internal => "built-in",
    }
}

/// One preset the VST2-to-VST3 migration would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst2ToVst3Migration {
    pub preset: String,
    pub vst2_plugin: String,
    pub vst3_plugin: String,
    /// Qualification metadata tied to the old binary.
    pub removed_params: Vec<String>,
}

/// A preset left alone, with the reason the report shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst2ToVst3Skip {
    pub preset: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Planned VST2-to-VST3 state migrations.
pub struct Vst2ToVst3MigrationPlan {
    pub migrations: Vec<Vst2ToVst3Migration>,
    pub skipped: Vec<Vst2ToVst3Skip>,
}

impl Vst2ToVst3MigrationPlan {
    /// Whether no migration is planned.
    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }
}

fn vst2_qualification_param(name: &str) -> bool {
    matches!(
        host_parameter_key(name).as_str(),
        "pluginversion" | "pluginarchitecture" | "pluginstateabi"
    )
}

fn has_vst2_state(params: &[(String, ParamValue)]) -> bool {
    params.iter().any(|(name, _)| {
        matches!(
            normalized_plugin_state_parameter(&host_parameter_key(name)),
            Some((None | Some(PluginStateBackend::Vst2), _))
        )
    })
}

fn vst2_track_state_uses(project: &Project) -> HashMap<&str, &str> {
    let mut uses = HashMap::new();
    for track in project
        .tracks
        .iter()
        .filter(|track| track.engine_params.as_deref().is_some_and(has_vst2_state))
    {
        let placed_patterns = track.pattern_clips().filter_map(|clip| {
            project
                .patterns
                .iter()
                .find(|pattern| pattern.name == clip.pattern)
        });
        let pattern_presets = placed_patterns
            .chain(&track.inline_patterns)
            .filter_map(|pattern| pattern.preset.as_deref());
        for preset in track.preset.as_deref().into_iter().chain(pattern_presets) {
            uses.insert(preset, track.name.as_str());
        }
    }
    uses
}

/// Protect the source and plugin declarations on which saved VST2 state depends.
fn vst2_state_owners(project: &Project) -> HashMap<&str, &str> {
    let mut owners = HashMap::new();
    let track_uses = vst2_track_state_uses(project);
    if track_uses.is_empty()
        && !project
            .presets
            .iter()
            .any(|preset| has_vst2_state(&preset.params))
    {
        return owners;
    }
    for preset in &project.presets {
        let track = track_uses.get(preset.name.as_str()).copied();
        let used_by = track.unwrap_or(&preset.name);
        if preset.extends.is_none() {
            if preset.source.as_deref() == Some("vst2")
                && (track.is_some() || has_vst2_state(&preset.params))
            {
                owners.insert(preset.name.as_str(), used_by);
            }
            continue;
        }
        let Ok(resolved) =
            oc_dsl::preset::resolve_preset_with_layers(&project.presets, &preset.name)
        else {
            continue;
        };
        if resolved.preset.source.as_deref() != Some("vst2")
            || (track.is_none() && !has_vst2_state(&resolved.preset.params))
        {
            continue;
        }
        for layer in [
            resolved.layers.iter().rfind(|layer| layer.source.is_some()),
            resolved.layers.iter().rfind(|layer| layer.plugin.is_some()),
        ]
        .into_iter()
        .flatten()
        {
            owners.insert(layer.name.as_str(), used_by);
        }
    }
    owners
}

/// Find VST2 presets whose plugin no longer scans but whose VST3 twin does.
///
/// Looks only at the inventories passed in, so it never scans or loads a
/// plugin. A preset with no VST3 twin is left out: the existing missing-plugin
/// diagnostic still covers it. Name matching alone cannot convert saved state.
pub fn plan_vst2_to_vst3_migration(
    project: &Project,
    installed_vst2: &[PluginInfo],
    installed_vst3: &[PluginInfo],
) -> Vst2ToVst3MigrationPlan {
    let mut plan = Vst2ToVst3MigrationPlan::default();
    let state_owners = vst2_state_owners(project);
    for preset in &project.presets {
        if preset.source.as_deref() != Some("vst2") {
            continue;
        }
        let Some(plugin_id) = preset.plugin.as_deref() else {
            plan.skipped.push(Vst2ToVst3Skip {
                preset: preset.name.clone(),
                reason: "the preset has no plugin id".to_string(),
            });
            continue;
        };
        let Some(query) = PluginQuery::parse(plugin_id) else {
            plan.skipped.push(Vst2ToVst3Skip {
                preset: preset.name.clone(),
                reason: format!("'{plugin_id}' is not a usable plugin id"),
            });
            continue;
        };
        if installed_vst2.iter().any(|plugin| {
            plugin.format == PluginFormat::Vst2 && plugin_matches_query(plugin, &query)
        }) {
            continue;
        }
        let key = identifier_key(plugin_id);
        let candidates: Vec<&PluginInfo> = installed_vst3
            .iter()
            .filter(|plugin| {
                plugin.format == PluginFormat::Vst3
                    && (identifier_key(&plugin.id) == key || identifier_key(&plugin.name) == key)
            })
            .collect();
        match candidates.as_slice() {
            [] => {}
            [_] if state_owners.contains_key(preset.name.as_str()) => {
                let dependent = state_owners[preset.name.as_str()];
                plan.skipped.push(Vst2ToVst3Skip {
                    preset: preset.name.clone(),
                    reason: format!(
                        "'{dependent}' uses saved VST2 state; a verified state conversion is required"
                    ),
                });
            }
            [twin] => plan.migrations.push(Vst2ToVst3Migration {
                preset: preset.name.clone(),
                vst2_plugin: plugin_id.to_string(),
                vst3_plugin: twin.id.clone(),
                removed_params: preset
                    .params
                    .iter()
                    .filter(|(name, _)| vst2_qualification_param(name))
                    .map(|(name, _)| name.clone())
                    .collect(),
            }),
            twins => plan.skipped.push(Vst2ToVst3Skip {
                preset: preset.name.clone(),
                reason: format!(
                    "{} VST3 plugins share the identity '{}'",
                    twins.len(),
                    plugin_id
                ),
            }),
        }
    }
    plan
}

/// Apply every planned migration; returns how many presets changed.
pub fn apply_vst2_to_vst3_migration(
    project: &mut Project,
    plan: &Vst2ToVst3MigrationPlan,
) -> Result<usize, String> {
    let state_owners = vst2_state_owners(project);
    let mut edits = Vec::with_capacity(plan.migrations.len());
    let mut edit_indices = HashSet::with_capacity(plan.migrations.len());
    for migration in &plan.migrations {
        let Some((index, preset)) = project
            .presets
            .iter()
            .enumerate()
            .find(|(_, preset)| preset.name == migration.preset)
        else {
            return Err(format!(
                "preset '{}' disappeared before the migration applied",
                migration.preset
            ));
        };
        if preset.source.as_deref() != Some("vst2")
            || preset.plugin.as_deref() != Some(migration.vst2_plugin.as_str())
            || state_owners.contains_key(preset.name.as_str())
            || !edit_indices.insert(index)
        {
            return Err(format!(
                "preset '{}' changed before the migration applied",
                migration.preset
            ));
        }
        edits.push((index, migration));
    }
    for (index, migration) in edits {
        let preset = &mut project.presets[index];
        preset.source = Some("vst3".to_string());
        preset.plugin = Some(migration.vst3_plugin.clone());
        preset
            .params
            .retain(|(name, _)| !vst2_qualification_param(name));
    }
    Ok(plan.migrations.len())
}

#[cfg(test)]
mod tests {
    use oc_dsl::ast::{ParamValue, PresetDef};

    use super::*;

    fn context<'a>(
        format: PluginFormat,
        plugin_id: &'a str,
        preset: &'a str,
    ) -> PluginStateContext<'a> {
        PluginStateContext::new(format, Some(plugin_id), preset)
    }

    fn envelope(version: u32, payload: &[u8]) -> Vec<u8> {
        encode_plugin_state_versioned(PluginFormat::Vst3, "vst3:halcyon_2", version, payload)
    }

    #[test]
    fn envelope_round_trips_bytes_format_and_identity() {
        let payload = b"opaque chunk \x00\x01\xff";
        let encoded = encode_plugin_state(PluginFormat::Vst3, "vst3:halcyon_2", payload);
        assert_ne!(encoded.as_slice(), payload);

        let decoded = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst3, "vst3:halcyon_2", "Halcyon Lead"),
        )
        .unwrap();

        assert_eq!(decoded, payload);
        assert_eq!(supported_plugin_state_version(), 1);
    }

    #[test]
    fn a_newer_envelope_is_refused_with_version_supported_and_preset() {
        let newer = supported_plugin_state_version() + 1;
        let encoded = envelope(newer, b"future");
        let error = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst3, "vst3:halcyon_2", "Halcyon Lead"),
        )
        .unwrap_err();

        assert!(error.contains(&format!("state version {newer}")), "{error}");
        assert!(
            error.contains(&format!(
                "supports version {}",
                supported_plugin_state_version()
            )),
            "{error}"
        );
        assert!(error.contains("newer build"), "{error}");
        assert!(error.contains("preset 'Halcyon Lead'"), "{error}");
    }

    #[test]
    fn a_payload_that_no_longer_matches_its_sha256_is_refused() {
        let mut encoded = envelope(supported_plugin_state_version(), b"opaque chunk");
        let last = encoded.len() - 1;
        encoded[last] ^= 0xff;

        let error = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst3, "vst3:halcyon_2", "Halcyon Lead"),
        )
        .unwrap_err();

        assert!(error.contains("sha256 mismatch"), "{error}");
        assert!(error.contains("preset 'Halcyon Lead'"), "{error}");
    }

    #[test]
    fn legacy_bare_bytes_load_unchanged() {
        let payload = b"bare vst2 bank";
        assert_eq!(
            decode_plugin_state(
                payload,
                &context(PluginFormat::Vst3, "vst3:halcyon_2", "Halcyon Lead")
            )
            .unwrap(),
            payload
        );
    }

    #[test]
    fn a_state_from_another_format_or_plugin_is_refused() {
        let encoded = envelope(supported_plugin_state_version(), b"chunk");

        let format_error = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst2, "vst2:halcyon_2", "Halcyon Lead"),
        )
        .unwrap_err();
        assert!(format_error.contains("saved for VST3"), "{format_error}");
        assert!(format_error.contains("loads it as VST2"), "{format_error}");

        let id_error = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst3, "vst3:other", "Halcyon Lead"),
        )
        .unwrap_err();
        assert!(
            id_error.contains("belongs to plugin 'vst3:halcyon_2'"),
            "{id_error}"
        );
        assert!(
            id_error.contains("this preset uses 'vst3:other'"),
            "{id_error}"
        );
    }

    #[test]
    fn a_truncated_header_is_a_readable_error() {
        let mut encoded = envelope(supported_plugin_state_version(), b"chunk");
        encoded.truncate(MAGIC.len() + 2);
        let error = decode_plugin_state(
            &encoded,
            &context(PluginFormat::Vst3, "vst3:halcyon_2", "Halcyon Lead"),
        )
        .unwrap_err();
        assert!(error.contains("malformed envelope"), "{error}");
        assert!(error.contains("preset 'Halcyon Lead'"), "{error}");
    }

    fn plugin(id: &str, name: &str, format: PluginFormat) -> PluginInfo {
        PluginInfo {
            id: id.to_string(),
            name: name.to_string(),
            vendor: String::new(),
            format,
            path: format!("/plugins/{name}"),
            category: super::super::PluginCategory::Instrument,
            num_params: 0,
            version: None,
            architectures: vec![],
            state_abi: None,
        }
    }

    fn vst2_preset() -> PresetDef {
        PresetDef {
            name: "Halcyon".to_string(),
            source: Some("vst2".to_string()),
            plugin: Some("vst2:halcyon_2".to_string()),
            program: None,
            patch: None,
            file: None,
            root_note: None,
            params: vec![
                (
                    "vst2_state_hex".to_string(),
                    ParamValue::String("deadbeef".to_string()),
                ),
                (
                    "plugin_state_abi".to_string(),
                    ParamValue::String("vst2:old".to_string()),
                ),
                ("gain".to_string(), ParamValue::Number(0.5)),
            ],
            effects: vec![],
            extends: None,
        }
    }

    fn project_with_preset(preset: PresetDef) -> Project {
        let mut project = oc_dsl::parse("@project \"Migration\"\n  tempo: 120\n").unwrap();
        project.presets.push(preset);
        project
    }

    #[test]
    fn a_name_match_preserves_inline_and_file_state() {
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];
        for key in ["vst2_state_hex", "Plugin-State-File", "StateHex"] {
            let mut preset = vst2_preset();
            preset.params[0].0 = key.to_owned();
            let project = project_with_preset(preset);

            let plan = plan_vst2_to_vst3_migration(&project, &[], &vst3);

            assert!(plan.is_empty());
            assert_eq!(plan.skipped.len(), 1);
            assert_eq!(plan.skipped[0].preset, "Halcyon");
            assert!(plan.skipped[0].reason.contains("uses saved VST2 state"));
        }
    }

    #[test]
    fn state_on_a_derived_preset_protects_its_inherited_plugin() {
        let mut parent = vst2_preset();
        parent.params.clear();
        let mut child = vst2_preset();
        child.name = "Layer".to_string();
        child.extends = Some(parent.name.clone());
        child.source = None;
        child.plugin = None;
        let mut project = project_with_preset(parent);
        project.presets.push(child);
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];

        let plan = plan_vst2_to_vst3_migration(&project, &[], &vst3);

        assert!(plan.is_empty());
        assert_eq!(plan.skipped[0].preset, "Halcyon");
        assert!(
            plan.skipped[0]
                .reason
                .contains("'Layer' uses saved VST2 state")
        );
    }

    #[test]
    fn track_state_protects_track_and_placed_pattern_presets() {
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
@preset TrackVoice
  source: vst2
  plugin: "vst2:halcyon_2"
@preset PatternVoice
  source: vst2
  plugin: "vst2:halcyon_2"
@pattern riff | preset: PatternVoice
  events:
    C4 q mf
@track Main | preset: TrackVoice, instrument: synth/lead
  engine_params: {state_file: "source.fxb"}
  clip: riff [1:1 - 2:1]
"#,
        )
        .unwrap();
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];

        let plan = plan_vst2_to_vst3_migration(&project, &[], &vst3);

        assert!(plan.is_empty());
        assert_eq!(plan.skipped.len(), 2);
        assert!(
            plan.skipped
                .iter()
                .all(|item| item.reason.contains("'Main' uses saved VST2 state"))
        );
    }

    #[test]
    fn a_vst2_that_still_scans_is_left_alone() {
        let project = project_with_preset(vst2_preset());
        let vst2 = [plugin("vst2:halcyon_2", "Halcyon 2", PluginFormat::Vst2)];
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];

        let plan = plan_vst2_to_vst3_migration(&project, &vst2, &vst3);

        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn no_vst3_twin_leaves_the_missing_plugin_diagnostic_alone() {
        let project = project_with_preset(vst2_preset());
        let plan = plan_vst2_to_vst3_migration(&project, &[], &[]);
        assert!(plan.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn migrating_without_state_replaces_the_plugin_and_keeps_other_params() {
        let mut preset = vst2_preset();
        preset.params.remove(0);
        let mut project = project_with_preset(preset);
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];
        let plan = plan_vst2_to_vst3_migration(&project, &[], &vst3);

        assert_eq!(plan.migrations[0].removed_params, ["plugin_state_abi"]);
        let applied = apply_vst2_to_vst3_migration(&mut project, &plan).unwrap();

        assert_eq!(applied, 1);
        let preset = &project.presets[0];
        assert_eq!(preset.source.as_deref(), Some("vst3"));
        assert_eq!(preset.plugin.as_deref(), Some("vst3:halcyon_2"));
        assert_eq!(
            preset.params,
            vec![("gain".to_string(), ParamValue::Number(0.5))]
        );
    }

    #[test]
    fn stale_stateful_preset_rejects_the_entire_batch() {
        let mut first = vst2_preset();
        let state = first.params.remove(0);
        let mut second = first.clone();
        second.name = "Second".to_string();
        let mut project = project_with_preset(first);
        project.presets.push(second);
        let vst3 = [plugin("vst3:halcyon_2", "Halcyon 2", PluginFormat::Vst3)];
        let plan = plan_vst2_to_vst3_migration(&project, &[], &vst3);
        assert_eq!(plan.migrations.len(), 2);
        project.presets[1].params.push(state);
        let before = project.clone();

        let error = apply_vst2_to_vst3_migration(&mut project, &plan).unwrap_err();

        assert!(error.contains("changed before the migration applied"));
        assert_eq!(project, before);
    }
}
