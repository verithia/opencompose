//! Read-only live metering delegates to the same snapshot owner as the mixer.

use crate::state::AppState;
use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ArgumentError, ToolArguments, empty_arguments};
use serde_json::Value;

empty_arguments! {
    SnapshotArgs => MeterSnapshot,
}

/// Decode a metering call into its arguments.
pub fn decode(id: ToolId, args: &Value) -> Result<SnapshotArgs, ArgumentError> {
    if id != SnapshotArgs::ID {
        return Err(ArgumentError::wrong_tool(id));
    }
    SnapshotArgs::decode(args)
}

/// Execute a metering snapshot request.
pub fn execute(state: &AppState, _args: SnapshotArgs) -> Result<Value, String> {
    serde_json::to_value(crate::commands::meter_snapshot_for_state(state))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn live_meter_dispatch_is_unavailable_before_playback_and_never_starts_audio_or_edits() {
        let state = AppState::new();
        let before = serde_json::to_value(crate::commands::snapshot(&state.lock_inner())).unwrap();
        let epoch = state.lock_inner().content_epoch;
        let undo = state.lock_inner().undo.undo.len();
        let redo = state.lock_inner().undo.redo.len();
        let result = execute(&state, decode(ToolId::MeterSnapshot, &json!({})).unwrap()).unwrap();
        let expected =
            serde_json::to_value(crate::commands::meter_snapshot_for_state(&state)).unwrap();
        assert_eq!(result, expected);
        assert_eq!(result["available"], false);
        assert_eq!(result["playing"], false);
        assert!(result["master"].get("short_term_lufs").is_none());
        assert!(result["master"].get("correlation").is_none());
        assert!(result["tracks"].as_array().unwrap().is_empty());
        assert!(state.lock_inner().audio.render_thread.is_none());
        assert_eq!(
            serde_json::to_value(crate::commands::snapshot(&state.lock_inner())).unwrap(),
            before
        );
        assert_eq!(state.lock_inner().content_epoch, epoch);
        assert_eq!(state.lock_inner().undo.undo.len(), undo);
        assert_eq!(state.lock_inner().undo.redo.len(), redo);
    }

    #[test]
    fn live_meter_schema_policy_and_discovery_bind_the_actual_typed_dispatch() {
        use crate::tool_registry::{Approval, Handler, Panel, is_mutation_tool};
        let id = ToolId::parse("meter_snapshot").unwrap();
        assert_eq!(id, SnapshotArgs::ID);
        let metadata = id.metadata();
        assert_eq!(metadata.area, "analysis");
        assert_eq!(metadata.panel, Panel::Enabled);
        assert_eq!(metadata.handler, Handler::Backend);
        assert_eq!(metadata.approval, Approval::Automatic);
        assert!(!is_mutation_tool("meter_snapshot"));
        let spec = crate::tool_registry::panel_tool("meter_snapshot").unwrap();
        assert_eq!(
            spec.schema,
            crate::tool_registry::arguments::schema::<SnapshotArgs>()
        );
        crate::tool_registry::validate_args(&spec, &json!({})).unwrap();
        assert!(
            crate::tool_registry::validate_args(&spec, &json!({"start_playback": true})).is_err()
        );
        assert!(
            crate::tool_registry::mcp_tools()
                .iter()
                .any(|tool| tool["name"] == "meter_snapshot")
        );
        let ears = vec!["ears".to_owned()];
        assert!(
            crate::tool_registry::panel_tools_for(Some(&ears))
                .iter()
                .any(|tool| tool["name"] == "meter_snapshot")
        );
        let dsl = vec!["dsl-edit".to_owned()];
        assert!(
            !crate::tool_registry::panel_tools_for(Some(&dsl))
                .iter()
                .any(|tool| tool["name"] == "meter_snapshot")
        );
        assert!(decode(ToolId::TransportPlay, &json!({})).is_err());
        // No-argument external tools retain the shared legacy payload policy.
        assert!(decode(id, &Value::Null).is_ok());
    }
}
