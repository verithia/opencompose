//! Typed arguments for project-preset tools.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ProjectRevision, ToolArguments};

#[derive(Debug, Deserialize, JsonSchema)]
/// Create a preset. For a local SoundFont use source=soundfont, file=<absolute SF2/SF3 path from soundfonts_list.discovered or content_libraries.discovered_soundfonts>, program=0 (or another program), then tracks_preset to assign it. Use source=clap|vst3|lv2 with plugin=<scanned plugin id> for instrument plugins.
pub struct PresetsAddArgs {
    #[schemars(description = "Preset name")]
    pub name: String,
    #[schemars(description = "clap | vst3 | lv2 | soundfont | builtin")]
    pub source: String,
    #[schemars(with = "Option<i64>")]
    #[schemars(description = "SoundFont program")]
    pub program: Option<u8>,
    #[schemars(description = "Plugin id from plugins_scan")]
    pub plugin: Option<String>,
    #[schemars(description = "Optional plugin patch/path")]
    pub patch: Option<String>,
    #[schemars(description = "Optional sample/preset file")]
    pub file: Option<String>,
}

impl ToolArguments for PresetsAddArgs {
    const ID: ToolId = ToolId::PresetsAdd;
    const FIELD_ORDER: &'static [&'static str] = &["name", "source"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Share an existing OpenSynth project preset as one self-contained .ocppatch bundle, including its inherited voice, samples and inserts. Requires a saved, committed project and its current revision. Finish an active transaction first; in the panel, export in a new turn. Writes a new numbered file under handoff/ without overwriting an earlier export, and returns its relative path and byte count. External insert sidechains must be removed before sharing.
pub struct PresetBundleExportArgs {
    #[schemars(description = "Existing project preset name")]
    pub name: String,
    /// Current project revision from project_get
    pub expected_revision: ProjectRevision,
}

impl ToolArguments for PresetBundleExportArgs {
    const ID: ToolId = ToolId::PresetBundleExport;
    const FIELD_ORDER: &'static [&'static str] = &["name", "expected_revision"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Load a staged .ocppatch file from the saved project's handoff/ folder into an existing OpenSynth project preset as one undoable edit. Replaces the whole voice and insert chain, detaches inheritance, and preserves the target's project name and track references. Also keeps a reusable library copy; Undo restores the project but retains that library copy. Duplicate library names get numbered copies. Requires current revision and transaction ownership. External insert sidechains are refused.
pub struct PresetBundleImportArgs {
    #[schemars(
        description = "Project-relative .ocppatch path under handoff/, not an absolute path"
    )]
    pub path: String,
    #[schemars(description = "Existing OpenSynth project preset to replace")]
    pub name: String,
    /// Current project revision from project_get
    pub expected_revision: ProjectRevision,
}

impl ToolArguments for PresetBundleImportArgs {
    const ID: ToolId = ToolId::PresetBundleImport;
    const FIELD_ORDER: &'static [&'static str] = &["path", "name", "expected_revision"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Remove a preset and clear it from tracks.
pub struct PresetsRemoveArgs {
    #[schemars(description = "Preset name")]
    pub name: String,
}

impl ToolArguments for PresetsRemoveArgs {
    const ID: ToolId = ToolId::PresetsRemove;
    const FIELD_ORDER: &'static [&'static str] = &["name"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Rename a project preset and rewrite every reference to it (tracks, patterns, patches that extend it) in the same undoable Preset edit, so nothing is left pointing at the old name. Use it when a patch is misnamed or before sharing it. An unknown name, an existing new name or an empty name is refused and nothing changes. Returns the new name, the tracks and patterns that now point at it, and the new revision.
pub struct PresetsRenameArgs {
    #[schemars(description = "Current preset name")]
    pub name: String,
    #[schemars(description = "New preset name")]
    pub new_name: String,
}

impl ToolArguments for PresetsRenameArgs {
    const ID: ToolId = ToolId::PresetsRename;
    const FIELD_ORDER: &'static [&'static str] = &["name", "new_name"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Copy a project preset under a new name as one undoable Preset edit. With reassign true (the default) the tracks and patterns that played the original play the copy, so the original stays free to edit or remove; with false they keep playing the original. An unknown preset, an existing new name or an empty name is refused and nothing changes. Returns the new name, whether the references moved, and the new revision.
pub struct PresetsDuplicateArgs {
    #[schemars(description = "Preset to copy")]
    pub name: String,
    #[schemars(description = "Name for the copy")]
    pub new_name: String,
    #[schemars(description = "Point referencing tracks and patterns at the copy (default true)")]
    pub reassign: Option<bool>,
}

impl ToolArguments for PresetsDuplicateArgs {
    const ID: ToolId = ToolId::PresetsDuplicate;
    const FIELD_ORDER: &'static [&'static str] = &["name", "new_name"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Set or remove one preset parameter (state_file, articulation_..., cc_..._param, ...).
pub struct PresetsSetParamArgs {
    #[schemars(description = "Preset name")]
    pub name: String,
    #[schemars(description = "Parameter name")]
    pub param: String,
    #[schemars(description = "Parameter value; null removes")]
    pub value: Option<String>,
}

impl ToolArguments for PresetsSetParamArgs {
    const ID: ToolId = ToolId::PresetsSetParam;
    const FIELD_ORDER: &'static [&'static str] = &["name", "param"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Set multiple preset parameters atomically.
pub struct PresetsSetParamsArgs {
    #[schemars(description = "Preset name")]
    pub name: String,
    #[schemars(
        description = "Object map {name: value}, an array of [name, value] pairs, or a JSON string of either, e.g. {\"amp.decay\": \"0.45s\"}"
    )]
    pub params: Value,
    #[schemars(description = "Replace the synth patch instead of merging")]
    pub replace_synth: Option<bool>,
}

impl ToolArguments for PresetsSetParamsArgs {
    const ID: ToolId = ToolId::PresetsSetParams;
    const FIELD_ORDER: &'static [&'static str] = &["name", "params"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn typed_preset_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [
            ToolId::PresetBundleExport,
            ToolId::PresetBundleImport,
            ToolId::PresetsAdd,
            ToolId::PresetsRemove,
            ToolId::PresetsRename,
            ToolId::PresetsDuplicate,
            ToolId::PresetsSetParam,
            ToolId::PresetsSetParams,
        ] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_preset_parameter_keeps_null_as_a_removal() {
        let removed = PresetsSetParamArgs::decode(&json!({
            "name": "lead",
            "param": "amp.decay",
            "value": null,
        }))
        .unwrap();
        assert!(removed.value.is_none());
        let set = PresetsSetParamArgs::decode(&json!({
            "name": "lead",
            "param": "amp.decay",
            "value": "0.45s",
        }))
        .unwrap();
        assert_eq!(set.value.as_deref(), Some("0.45s"));
        assert!(PresetsRemoveArgs::decode(&json!({})).is_err());
    }

    #[test]
    fn typed_preset_parameter_maps_accept_every_documented_shape() {
        let object = PresetsSetParamsArgs::decode(&json!({
            "name": "lead",
            "params": {"amp.decay": "0.45s"},
        }))
        .unwrap();
        assert!(object.params.is_object());
        assert!(object.replace_synth.is_none());

        let pairs = PresetsSetParamsArgs::decode(&json!({
            "name": "lead",
            "params": [["amp.decay", "0.45s"]],
            "replace_synth": true,
        }))
        .unwrap();
        assert!(pairs.params.is_array());
        assert_eq!(pairs.replace_synth, Some(true));

        assert!(PresetsSetParamsArgs::decode(&json!({"name": "lead"})).is_err());
    }
}
