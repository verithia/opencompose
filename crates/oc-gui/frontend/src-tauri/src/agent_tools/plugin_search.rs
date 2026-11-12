//! Typed arguments for the plugin catalog search.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Search the bundled free-plugin catalog. Read-only; never downloads or installs. Platform defaults to this host; invalid platform/format values are rejected.
pub struct PluginSearchArgs {
    #[schemars(description = "Optional case-insensitive name, vendor, description, or tag search")]
    pub query: Option<String>,
    #[schemars(description = "Required tags (all must match)")]
    pub tags: Option<Vec<String>>,
    #[schemars(description = "macos-aarch64 | macos-x86_64 | windows-x86_64 | linux-x86_64")]
    pub platform: Option<String>,
    #[schemars(description = "vst3 | clap | au | standalone")]
    pub format: Option<String>,
    #[schemars(description = "Only return entries whose bundle is installed")]
    pub installed_only: Option<bool>,
    #[schemars(description = "Maximum entries to return (1-100, default 50)")]
    pub limit: Option<usize>,
}

impl ToolArguments for PluginSearchArgs {
    const ID: ToolId = ToolId::PluginSearch;
    const FIELD_ORDER: &'static [&'static str] = &[
        "query",
        "tags",
        "platform",
        "format",
        "installed_only",
        "limit",
    ];
}
