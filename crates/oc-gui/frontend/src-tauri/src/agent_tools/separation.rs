//! Typed arguments for source-separation tools.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

/// Which separation backend `separate_stems` may use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase", try_from = "String")]
pub enum SeparateStemsMode {
    #[default]
    Auto,
    External,
    Band,
}

impl TryFrom<String> for SeparateStemsMode {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        match value.as_str() {
            "auto" => Ok(Self::Auto),
            "external" => Ok(Self::External),
            "band" => Ok(Self::Band),
            value => Err(format!(
                "mode must be \"auto\", \"external\" or \"band\", not \"{value}\""
            )),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Split a rendered WAV into stems before transcribing: `mode` auto (default) runs demucs or spleeter when one is on PATH and otherwise falls back to the built-in band split, external refuses when neither is installed, band always uses the fallback. The band split is a 200 Hz low/high filter split plus the mid for a stereo source — an approximation, not a source separation — and `method` says which path ran. Feed each stem to `analyze_notes`.
pub struct SeparateStemsArgs {
    #[schemars(description = "Absolute WAV path, e.g. from render_range")]
    pub path: String,
    #[serde(default)]
    #[schemars(default, schema_with = "separation_mode_schema")]
    pub mode: SeparateStemsMode,
    #[schemars(
        description = "Optional installed separator catalog ID from plugin_search or plugin_pull. Requires that exact managed runtime and refuses instead of falling back. Cannot be combined with band mode."
    )]
    pub runtime_id: Option<String>,
    #[schemars(
        description = "Parent directory for a fresh run subfolder; use the returned stem paths. Defaults to a temporary directory."
    )]
    pub out_dir: Option<String>,
}

impl ToolArguments for SeparateStemsArgs {
    const ID: ToolId = ToolId::SeparateStems;
    const FIELD_ORDER: &'static [&'static str] = &["path", "mode", "runtime_id", "out_dir"];
}

/// The mode schema stays inline: this tool's wire contract predates references.
pub fn separation_mode_schema(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = <String as JsonSchema>::json_schema(generator).into_object();
    schema.enum_values = Some(vec![
        serde_json::Value::String("auto".into()),
        serde_json::Value::String("external".into()),
        serde_json::Value::String("band".into()),
    ]);
    schema.metadata().description = Some(
        "auto (default) prefers an installed CLI, external refuses when none is installed, band always uses the fallback"
            .into(),
    );
    schema.into()
}
