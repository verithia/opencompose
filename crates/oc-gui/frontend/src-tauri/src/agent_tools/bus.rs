//! Typed arguments for mixer bus management.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::ToolArguments;

#[derive(Debug, Deserialize, JsonSchema)]
/// Create a bus.
pub struct BusesAddArgs {
    #[schemars(description = "Bus name")]
    pub name: String,
    #[schemars(description = "Route targets, e.g. [\"master\"]")]
    pub routes: Option<Vec<String>>,
}

impl ToolArguments for BusesAddArgs {
    const ID: ToolId = ToolId::BusesAdd;
    const FIELD_ORDER: &'static [&'static str] = &["name", "routes"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Replace a bus's routes.
pub struct BusesRoutesArgs {
    #[schemars(description = "Bus name")]
    pub name: String,
    pub routes: Vec<String>,
}

impl ToolArguments for BusesRoutesArgs {
    const ID: ToolId = ToolId::BusesRoutes;
    const FIELD_ORDER: &'static [&'static str] = &["name", "routes"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Update a bus return's fader, mute and solo.
pub struct BusesMixArgs {
    #[schemars(description = "Bus name")]
    pub name: String,
    #[schemars(description = "Return fader in dB, -120 to 24")]
    pub volume_db: Option<f64>,
    #[schemars(description = "Mute the bus return")]
    pub mute: Option<bool>,
    #[schemars(description = "Solo the bus return; its inputs stay audible")]
    pub solo: Option<bool>,
}

impl ToolArguments for BusesMixArgs {
    const ID: ToolId = ToolId::BusesMix;
    const FIELD_ORDER: &'static [&'static str] = &["name", "volume_db", "mute", "solo"];
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Remove an unused bus. A bus that still receives track outputs, sends or bus routes is rejected unless reroute is true; rerouting returns those track outputs to master and drops the sends/routes into it.
pub struct BusesRemoveArgs {
    #[schemars(description = "Bus name")]
    pub name: String,
    #[schemars(description = "Delete anyway, re-routing the bus's references")]
    pub reroute: Option<bool>,
}

impl ToolArguments for BusesRemoveArgs {
    const ID: ToolId = ToolId::BusesRemove;
    const FIELD_ORDER: &'static [&'static str] = &["name", "reroute"];
}
