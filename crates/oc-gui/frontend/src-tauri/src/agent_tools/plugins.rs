//! Typed arguments for the simple external-plugin tools.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Scan installed CLAP/VST2/VST3/AU plugins (metadata only). Set rescan=true to retry bundles an earlier scan quarantined.
pub struct PluginsScanArgs {
    #[schemars(
        description = "Set true to retry bundles an earlier scan quarantined; false reads the published inventory"
    )]
    pub rescan: Option<bool>,
}

impl ToolArguments for PluginsScanArgs {
    const ID: ToolId = ToolId::PluginsScan;
    const FIELD_ORDER: &'static [&'static str] = &["rescan"];
}

macro_rules! preset_tool {
    ($request:ident, $id:ident) => {
        #[derive(Debug, Deserialize, JsonSchema)]
        pub struct $request {
            #[schemars(description = "Preset name")]
            pub preset: String,
        }

        impl ToolArguments for $request {
            const ID: ToolId = ToolId::$id;
            const FIELD_ORDER: &'static [&'static str] = &["preset"];
        }
    };
}

preset_tool!(PluginsLoadArgs, PluginsLoad);
preset_tool!(PluginsParamsArgs, PluginsParams);
preset_tool!(PluginsOpenGuiArgs, PluginsOpenGui);
preset_tool!(PluginsCloseGuiArgs, PluginsCloseGui);
preset_tool!(PluginsHasGuiArgs, PluginsHasGui);

#[derive(Debug, Deserialize, JsonSchema)]
/// Capture plugin state into the project (embedded) or a sidecar file.
pub struct PluginsSaveStateArgs {
    #[schemars(description = "Preset name")]
    pub preset: String,
    #[schemars(description = "embedded | file")]
    pub mode: String,
    #[schemars(description = "Sidecar path when mode=file")]
    pub path: Option<String>,
}

impl ToolArguments for PluginsSaveStateArgs {
    const ID: ToolId = ToolId::PluginsSaveState;
    const FIELD_ORDER: &'static [&'static str] = &["preset", "mode", "path"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Return the per-format program capability table: which formats expose a native program list and which selection mechanisms are available. Data only; never loads a plugin.
pub struct PluginsProgramCapabilitiesArgs {}

impl ToolArguments for PluginsProgramCapabilitiesArgs {
    const ID: ToolId = ToolId::PluginsProgramCapabilities;
}

#[derive(Debug, Deserialize, JsonSchema)]
/// List a preset's programs. VST3 plugins expose their own list; other formats return an empty list with the capability that says so. Loads a temporary instance when none is live.
pub struct PluginsProgramsArgs {
    #[schemars(description = "Preset name")]
    pub preset: String,
}

impl ToolArguments for PluginsProgramsArgs {
    const ID: ToolId = ToolId::PluginsPrograms;
    const FIELD_ORDER: &'static [&'static str] = &["preset"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Read a preset's current CLAP/VST2/VST3/AU/LV2 state as a portable value: state_hex, bytes, sha256 and truncated. The blob is capped at max_bytes (default 262144, ceiling 3145728); over the cap state_hex is null and truncated is true, never a fragment. Hand the value to `plugins_apply_state`. Loads a temporary AU/VST3/VST2/LV2 instance when the preset is not loaded; a CLAP preset must be loaded first.
pub struct PluginsReadStateArgs {
    #[schemars(description = "Preset name")]
    pub preset: String,
    #[schemars(range(min = 1, max = 3145728))]
    #[schemars(description = "Optional inline cap in encoded bytes (default 262144)")]
    pub max_bytes: Option<u32>,
}

impl ToolArguments for PluginsReadStateArgs {
    const ID: ToolId = ToolId::PluginsReadState;
    const FIELD_ORDER: &'static [&'static str] = &["preset"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Write a plugin state value (from `plugins_read_state` or a saved envelope) into the preset. Verifies the envelope's format, plugin identity, version and SHA-256 before writing, clears any state_file and pushes one PluginState undo entry, so the next render uses the applied state.
pub struct PluginsApplyStateArgs {
    #[schemars(description = "Preset name")]
    pub preset: String,
    #[schemars(description = "Hex state envelope from plugins_read_state")]
    pub state_hex: String,
}

impl ToolArguments for PluginsApplyStateArgs {
    const ID: ToolId = ToolId::PluginsApplyState;
    const FIELD_ORDER: &'static [&'static str] = &["preset", "state_hex"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Select one program and write its state into the project (embedded by default, or a verified sidecar file). Uses the plugin's program data where available; pass state_file to select a program by state when it is not. Honours the project revision guard and the plugin-state undo scope.
pub struct PluginsSelectProgramArgs {
    #[schemars(description = "Preset name")]
    pub preset: String,
    #[schemars(description = "Program index from plugins_programs; use alone")]
    pub index: Option<u32>,
    #[schemars(description = "Program name from plugins_programs; use alone")]
    pub name: Option<String>,
    #[schemars(description = "State blob for the state-injection fallback; use alone")]
    pub state_file: Option<String>,
    #[schemars(description = "embedded | file (default embedded)")]
    pub mode: Option<String>,
    #[schemars(description = "Sidecar path when mode=file")]
    pub path: Option<String>,
}

impl ToolArguments for PluginsSelectProgramArgs {
    const ID: ToolId = ToolId::PluginsSelectProgram;
    const FIELD_ORDER: &'static [&'static str] =
        &["preset", "index", "name", "state_file", "mode", "path"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_plugin_state_cap_decodes_an_optional_size() {
        let capped =
            PluginsReadStateArgs::decode(&json!({"preset": "lead", "max_bytes": 1024})).unwrap();
        assert_eq!(capped.preset, "lead");
        assert_eq!(capped.max_bytes, Some(1024));
        let uncapped = PluginsReadStateArgs::decode(&json!({"preset": "lead"})).unwrap();
        assert!(uncapped.max_bytes.is_none());
        assert!(PluginsReadStateArgs::decode(&json!({"preset": 1})).is_err());
    }
}
