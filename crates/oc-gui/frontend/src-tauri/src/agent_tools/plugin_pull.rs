//! Typed arguments for a plugin catalog install.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Resolve or execute a catalog install. dry_run defaults to true and performs no network or disk writes; pass dry_run=false to install. URLs always come from the verified catalog. The built-in panel permits signed-index, checksum/size-pinned native plugin archives (VST3, VST2, CLAP, LV2) and managed separator runtimes. Use plugin_search then one call with dry_run=false to install. Manual DMG/PKG/EXE installers are refused. Never run shell or pip commands.
pub struct PluginPullArgs {
    #[schemars(description = "Catalog entry id from plugin_search")]
    pub id: String,
    #[schemars(description = "Optional target platform; defaults to this host")]
    pub platform: Option<String>,
    #[schemars(description = "Optional preferred vst3/clap/au/standalone build")]
    pub format: Option<String>,
    #[schemars(description = "Default true. false is explicit consent to download and install")]
    pub dry_run: Option<bool>,
}

impl ToolArguments for PluginPullArgs {
    const ID: ToolId = ToolId::PluginPull;
    const FIELD_ORDER: &'static [&'static str] = &["id", "platform", "format", "dry_run"];
}
