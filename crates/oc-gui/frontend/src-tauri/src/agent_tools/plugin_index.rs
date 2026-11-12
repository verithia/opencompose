//! Typed arguments for refreshing the plugin catalog index.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Refresh the signed remote plugin index and return its status. The index is fetched only when the plugin browser opens or a caller asks for it; a check inside the 6-hour freshness interval answers from the cache, and a refresh is limited to once per 5 minutes.
pub struct PluginIndexRefreshArgs {
    #[schemars(
        description = "Bypass the 6-hour freshness interval; the 5-minute manual floor still applies"
    )]
    pub force: Option<bool>,
}

impl ToolArguments for PluginIndexRefreshArgs {
    const ID: ToolId = ToolId::PluginIndexRefresh;
    const FIELD_ORDER: &'static [&'static str] = &["force"];
}
