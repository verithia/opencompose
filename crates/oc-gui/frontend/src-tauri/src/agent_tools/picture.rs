//! Typed arguments for editorial picture-clock tools.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ProjectRevision, ToolArguments};

fn inline_schema(value: Value) -> Schema {
    serde_json::from_value(value).expect("an inline tool schema is valid")
}

fn bar_beat_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "bar": {"type": "integer", "description": "One-based bar"},
            "beat": {"type": "integer", "description": "One-based beat within the bar"},
            "subdivision": {"type": "integer", "description": "Zero-based subdivision within the beat"}
        },
        "required": ["bar", "beat", "subdivision"],
        "additionalProperties": false
    })
}

/// The project command already owns this request shape; the wrapper only adds
/// the provider schema and tool identity.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct PictureTimecodeArgs(pub crate::commands::PictureTimecodeRequest);

impl JsonSchema for PictureTimecodeArgs {
    fn schema_name() -> String {
        "PictureTimecodeRequest".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": "object",
            "properties": {
                "rate": {"type": "string", "enum": ["24000/1001", "24", "25", "30000/1001", "30", "48000/1001", "48", "50", "60000/1001", "60"]},
                "origin": {
                    "type": "string",
                    "description": "Picture timecode at project zero, HH:MM:SS:FF or HH:MM:SS;FF"
                },
                "points": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 64,
                    "items": {
                        "type": "object",
                        "properties": {
                            "timecode": {"type": "string", "description": "Editorial frame label"},
                            "position": bar_beat_value(),
                            "seconds": {"type": "number", "description": "Signed project time in seconds"}
                        },
                        "minProperties": 1,
                        "maxProperties": 1,
                        "additionalProperties": false
                    }
                }
            },
            "required": ["points"]
        }))
    }
}

impl ToolArguments for PictureTimecodeArgs {
    const ID: ToolId = ToolId::PictureTimecode;
    const FIELD_ORDER: &'static [&'static str] = &["points"];
}

struct ClockSchema;

impl JsonSchema for ClockSchema {
    fn schema_name() -> String {
        "PictureClock".into()
    }
    fn is_referenceable() -> bool {
        false
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        inline_schema(json!({
            "type": ["object", "null"],
            "properties": {
                "rate": {"type": "string", "enum": ["24000/1001", "24", "25", "30000/1001", "30", "48000/1001", "48", "50", "60000/1001", "60"]},
                "origin": {"type": "string", "description": "Timecode at project zero"}
            },
            "required": ["rate", "origin"],
            "additionalProperties": false
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
/// Save the editorial frame rate and timecode at project zero as one undoable edit. Use the current revision from project_get. Pass clock=null to clear the saved setting. Only save settings explicitly confirmed by the operator.
pub struct PictureClockSetArgs {
    #[schemars(with = "ClockSchema")]
    pub clock: Option<oc_dsl::timecode::PictureClock>,
    /// Current project revision from project_get
    pub expected_revision: ProjectRevision,
}

impl ToolArguments for PictureClockSetArgs {
    const ID: ToolId = ToolId::PictureClockSet;
    const FIELD_ORDER: &'static [&'static str] = &["clock", "expected_revision"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_picture_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [ToolId::PictureTimecode, ToolId::PictureClockSet] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_picture_requests_decode_points_and_an_optional_clock() {
        let timecode = PictureTimecodeArgs::decode(&json!({
            "points": [{"timecode": "01:00:00:00"}, {"seconds": -1.5}],
        }))
        .unwrap();
        let _ = timecode.0;

        let cleared = PictureClockSetArgs::decode(&json!({
            "clock": null,
            "expected_revision": 2,
        }))
        .unwrap();
        assert!(cleared.clock.is_none());
        assert_eq!(cleared.expected_revision.0, 2);

        let saved = PictureClockSetArgs::decode(&json!({
            "clock": {"rate": "24", "origin": "01:00:00:00"},
            "expected_revision": 2,
        }))
        .unwrap();
        assert_eq!(saved.clock.unwrap().rate.as_str(), "24");

        let high = PictureClockSetArgs::decode(&json!({
            "clock": {"rate": "60000/1001", "origin": "01:00:00;00"},
            "expected_revision": 2,
        }))
        .unwrap();
        assert_eq!(high.clock.unwrap().rate.as_str(), "60000/1001");
    }
}
