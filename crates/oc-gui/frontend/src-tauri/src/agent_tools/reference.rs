//! Read-only production references; no project state or GUI is required.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{ArgumentError, ToolArguments, empty_arguments};

empty_arguments! { ListSkillsArgs => ListSkills }

#[derive(Debug, Deserialize, JsonSchema)]
/// Read one skill body by exact id from list_skills. For id production-curriculum, pass topic to get a genre, scene, or technique exercise, or omit it to browse fields. Returns Markdown and referenced panel tools.
pub struct ReadSkillArgs {
    /// Exact skill id from list_skills
    pub id: String,
    /// Optional for production-curriculum only. Field, genre, scene, or production technique
    pub topic: Option<String>,
}
impl ToolArguments for ReadSkillArgs {
    const ID: ToolId = ToolId::ReadSkill;
}

#[derive(Debug)]
/// Decoded request for reference-material tools.
pub enum Request {
    List(ListSkillsArgs),
    Read(ReadSkillArgs),
}
impl Request {
    /// Decode a tool call into a request.
    pub fn decode(id: ToolId, args: &Value) -> Option<Result<Self, ArgumentError>> {
        match id {
            ListSkillsArgs::ID => Some(ListSkillsArgs::decode(args).map(Self::List)),
            ReadSkillArgs::ID => Some(ReadSkillArgs::decode(args).map(Self::Read)),
            _ => None,
        }
    }
}
/// Execute a decoded reference request.
pub fn execute(request: Request) -> Result<Value, String> {
    match request {
        Request::List(_) => Ok(crate::ai::skills::list_value()),
        Request::Read(args) => crate::ai::skills::read_value(&args.id, args.topic.as_deref()),
    }
}
/// Decode and execute one reference call.
pub fn dispatch(id: ToolId, args: &Value) -> Option<Result<Value, String>> {
    Request::decode(id, args)
        .map(|request| request.map_err(|error| error.to_string()).and_then(execute))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_reference_requests_preserve_external_optional_and_unknown_fields() {
        let args = ReadSkillArgs::decode(
            &json!({"id":"dsl-subtree-paths", "topic":null, "legacy_extension":true}),
        )
        .unwrap();
        assert!(args.topic.is_none());
        let body = execute(Request::Read(args)).unwrap();
        assert_eq!(body["id"], "dsl-subtree-paths");
        assert!(body["body"].as_str().is_some_and(|body| !body.is_empty()));
        let missing = ReadSkillArgs::decode(&json!({})).unwrap_err();
        assert_eq!(missing.tool, ToolId::ReadSkill);
        assert_eq!(
            missing.code,
            crate::tool_registry::arguments::ArgumentErrorCode::Missing
        );
        assert_eq!(missing.field.as_deref(), Some("id"));
        assert_eq!(missing.to_string(), "missing argument 'id'");
        let invalid = ReadSkillArgs::decode(&json!({"id":42})).unwrap_err();
        assert_eq!(
            invalid.code,
            crate::tool_registry::arguments::ArgumentErrorCode::InvalidType
        );
        assert_eq!(invalid.field.as_deref(), Some("id"));
        assert!(invalid.to_string().starts_with("argument 'id': "));
        assert!(ReadSkillArgs::decode(&json!({"id":42})).is_err());
        assert!(ReadSkillArgs::decode(&json!({"id":"dsl-subtree-paths", "topic":42})).is_err());
        let topic = dispatch(
            ToolId::ReadSkill,
            &json!({"id":"production-curriculum", "topic":"Singeli"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(topic["title"], "Singeli");
        assert!(
            dispatch(
                ToolId::ReadSkill,
                &json!({"id":"writing-notes", "topic":"Singeli"})
            )
            .unwrap()
            .is_err()
        );
    }

    #[test]
    fn no_argument_reference_requests_keep_legacy_payload_compatibility() {
        for payload in [Value::Null, json!([]), json!({"ignored":true})] {
            assert!(
                dispatch(ToolId::ListSkills, &payload)
                    .unwrap()
                    .unwrap()
                    .is_array()
            );
        }
        assert!(dispatch(ToolId::TransportPlay, &json!({})).is_none());
    }

    #[test]
    fn panel_schema_stays_strict_while_external_decoder_keeps_legacy_extensions() {
        let spec = crate::tool_registry::panel_tool("read_skill").unwrap();
        for invalid in [
            json!({"id":"dsl-subtree-paths", "topic":null}),
            json!({"id":"dsl-subtree-paths", "legacy_extension":true}),
        ] {
            assert!(crate::tool_registry::validate_args(&spec, &invalid).is_err());
            assert!(ReadSkillArgs::decode(&invalid).is_ok());
        }
        assert_eq!(
            spec.schema,
            crate::tool_registry::arguments::schema::<ReadSkillArgs>()
        );
    }
}
