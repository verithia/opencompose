//! Read-only typed project inspection, context, validation and DSL comparison.

use crate::commands;
use crate::state::AppState;
use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{
    ArgumentError, ToolArguments, empty_arguments, typed_arguments,
};
use serde_json::{Value, json};

empty_arguments! {
    GetArgs => ProjectGet,
    DslArgs => ProjectDsl,
}
typed_arguments! {
    /// Build an AI editing context for a DSL path (project, track:bass, ...); path grammar is described by `read_skill dsl-subtree-paths`.
    pub struct ContextArgs => ProjectContext {
        #[schemars(schema_with = "crate::tool_registry::arguments::subtree_path_schema")]
        pub path: String,
        /// Optional user instruction
        pub instruction: Option<String>,
    }
    /// Build a context for generating a new track, phrase, or preset.
    pub struct GenerateContextArgs => ProjectGenerateContext {
        /// track | phrase | preset
        pub what: String,
        /// Optional user instruction
        pub instruction: Option<String>,
    }
    /// Validate a DSL replacement without applying it, including that every preset `file:` it names exists. Path grammar is described by `read_skill dsl-subtree-paths`.
    pub struct ValidateArgs => ProjectValidate {
        #[schemars(schema_with = "crate::tool_registry::arguments::subtree_path_schema")]
        pub path: String,
        /// Replacement DSL
        pub dsl: String,
    }
    /// Extract a DSL subtree without changing the project; path grammar is described by `read_skill dsl-subtree-paths`.
    pub struct ExtractArgs => ProjectExtract {
        #[schemars(schema_with = "crate::tool_registry::arguments::subtree_path_schema")]
        pub path: String,
    }
    /// Line diff between two DSL strings.
    pub struct DiffArgs => ProjectDiff {
        /// Old DSL
        pub old_dsl: String,
        /// New DSL
        pub new_dsl: String,
    }
}

#[derive(Debug)]
/// Decoded request for project queries.
pub enum Request {
    Get(GetArgs),
    Dsl(DslArgs),
    Context(ContextArgs),
    GenerateContext(GenerateContextArgs),
    Validate(ValidateArgs),
    Extract(ExtractArgs),
    Diff(DiffArgs),
}
impl Request {
    /// Decode a tool call into a request.
    pub fn decode(id: ToolId, args: &Value) -> Result<Self, ArgumentError> {
        match id {
            GetArgs::ID => GetArgs::decode(args).map(Self::Get),
            DslArgs::ID => DslArgs::decode(args).map(Self::Dsl),
            ContextArgs::ID => ContextArgs::decode(args).map(Self::Context),
            GenerateContextArgs::ID => GenerateContextArgs::decode(args).map(Self::GenerateContext),
            ValidateArgs::ID => ValidateArgs::decode(args).map(Self::Validate),
            ExtractArgs::ID => ExtractArgs::decode(args).map(Self::Extract),
            DiffArgs::ID => DiffArgs::decode(args).map(Self::Diff),
            _ => Err(ArgumentError::wrong_tool(id)),
        }
    }
}

/// Execute a decoded project query.
pub fn execute(state: &AppState, request: Request) -> Result<Value, String> {
    match request {
        Request::Get(_) => {
            let snapshot = { commands::snapshot(&state.lock_inner()) };
            serde_json::to_value(snapshot).map_err(|error| error.to_string())
        }
        Request::Dsl(_) => {
            let dsl = { state.lock_inner().dsl_source.clone() };
            Ok(json!({"dsl": dsl}))
        }
        Request::Context(args) => {
            let context = {
                let inner = state.lock_inner();
                oc_dsl::ai::build_context(
                    &inner.project,
                    &args.path,
                    &args.instruction.unwrap_or_default(),
                )?
            };
            serde_json::to_value(context).map_err(|error| error.to_string())
        }
        Request::GenerateContext(args) => {
            let context = {
                let inner = state.lock_inner();
                oc_dsl::ai::build_generate_context(
                    &inner.project,
                    &args.what,
                    &args.instruction.unwrap_or_default(),
                )?
            };
            serde_json::to_value(context).map_err(|error| error.to_string())
        }
        Request::Validate(args) => {
            let result = {
                let inner = state.lock_inner();
                commands::validate_edit_in_project(&inner, args.dsl, args.path)
            };
            Ok(validation_value(result))
        }
        Request::Extract(args) => {
            let extracted = {
                let inner = state.lock_inner();
                oc_dsl::ops::extract(&inner.project, &args.path)?
            };
            serde_json::to_value(extracted).map_err(|error| error.to_string())
        }
        Request::Diff(args) => serde_json::to_value(commands::diff_dsl(args.old_dsl, args.new_dsl))
            .map_err(|error| error.to_string()),
    }
}

/// A validation answer keeps `valid`/`parsed_dsl` for existing readers and
/// carries `ok` so a caller that branches on it cannot read invalid as success.
/// An invalid answer also carries `error` (the `errors` joined with `"; "`) and,
/// when one explains it, the `skill` id with the prose `hint` that reads it, so a
/// caller that reads `error` sees the failure and its remedy.
pub(crate) fn validation_value(result: oc_dsl::ai::ValidationResult) -> Value {
    let mut payload = json!({
        "ok": result.valid,
        "valid": result.valid,
        "errors": result.errors,
        "parsed_dsl": result.parsed_dsl,
    });
    if !result.valid {
        let message = result.errors.join("; ");
        payload["error"] = json!(message);
        if let Some(skill) = crate::ai::skill_hints::for_message(&message) {
            payload["skill"] = json!(skill);
            payload["hint"] = json!(crate::ai::skill_hints::hint_text(skill));
        }
    }
    payload
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::arguments::ArgumentErrorCode;

    fn query(state: &AppState, id: ToolId, args: Value) -> Value {
        execute(state, Request::decode(id, &args).unwrap()).unwrap()
    }

    #[test]
    fn project_queries_and_validation_leave_the_document_and_undo_unchanged() {
        let state = AppState::new();
        let before = serde_json::to_value(commands::snapshot(&state.lock_inner())).unwrap();
        let epoch = state.lock_inner().content_epoch;
        let undo = state.lock_inner().undo.undo.len();
        let redo = state.lock_inner().undo.redo.len();
        assert_eq!(query(&state, ToolId::ProjectGet, Value::Null), before);
        assert_eq!(
            query(&state, ToolId::ProjectDsl, json!({"ignored":true}))["dsl"],
            before["dsl_source"]
        );
        let context = query(
            &state,
            ToolId::ProjectContext,
            json!({"path":"project.properties", "instruction":"Inspect the tempo"}),
        );
        assert_eq!(context["user_instruction"], "Inspect the tempo");
        assert_eq!(context["edit_target"], "project.properties");
        assert!(context["dsl_context"].as_str().unwrap().contains("tempo"));
        let generated = query(
            &state,
            ToolId::ProjectGenerateContext,
            json!({"what":"track", "instruction":null}),
        );
        assert_eq!(generated["user_instruction"], "");
        let extracted = query(
            &state,
            ToolId::ProjectExtract,
            json!({"path":"project.properties"}),
        );
        assert!(extracted["dsl_text"].as_str().unwrap().contains("tempo"));
        let valid = query(
            &state,
            ToolId::ProjectValidate,
            json!({"path":"project.properties", "dsl":"tempo: 132"}),
        );
        assert_eq!(valid["ok"], true);
        assert_eq!(valid["valid"], true);
        let invalid = query(
            &state,
            ToolId::ProjectValidate,
            json!({"path":"doc", "dsl":"tempo: 132"}),
        );
        assert_eq!(invalid["ok"], false);
        assert_eq!(invalid["valid"], false);
        assert!(!invalid["errors"].as_array().unwrap().is_empty());
        assert!(
            invalid["error"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
        assert_eq!(
            serde_json::to_value(commands::snapshot(&state.lock_inner())).unwrap(),
            before
        );
        let inner = state.lock_inner();
        assert_eq!(inner.content_epoch, epoch);
        assert_eq!(inner.undo.undo.len(), undo);
        assert_eq!(inner.undo.redo.len(), redo);
    }

    #[test]
    fn typed_diff_returns_the_existing_dsl_comparison_shape() {
        let state = AppState::new();
        let old = "tempo: 120\n";
        let new = "tempo: 132\n";
        let diff = query(
            &state,
            ToolId::ProjectDiff,
            json!({"old_dsl":old, "new_dsl":new, "ignored":true}),
        );
        assert_eq!(diff["old_text"], old);
        assert_eq!(diff["new_text"], new);
        assert!(!diff["hunks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn typed_query_failures_keep_the_tool_and_field_identity() {
        for (id, value, field, code) in [
            (
                ToolId::ProjectValidate,
                json!({}),
                "path",
                ArgumentErrorCode::Missing,
            ),
            (
                ToolId::ProjectValidate,
                json!({"path":"project.properties"}),
                "dsl",
                ArgumentErrorCode::Missing,
            ),
            (
                ToolId::ProjectDiff,
                json!({}),
                "old_dsl",
                ArgumentErrorCode::Missing,
            ),
            (
                ToolId::ProjectDiff,
                json!({"old_dsl":"a", "new_dsl":null}),
                "new_dsl",
                ArgumentErrorCode::InvalidType,
            ),
            (
                ToolId::ProjectContext,
                json!({"path":"project", "instruction":42}),
                "instruction",
                ArgumentErrorCode::InvalidType,
            ),
        ] {
            let error = Request::decode(id, &value).unwrap_err();
            assert_eq!(error.tool, id);
            assert_eq!(error.field.as_deref(), Some(field));
            assert_eq!(error.code, code);
        }
        assert!(Request::decode(ToolId::TransportPlay, &json!({})).is_err());
    }

    #[test]
    fn derived_query_schema_preserves_field_order_and_path_metadata() {
        let validate = crate::tool_registry::panel_tool("project_validate").unwrap();
        assert_eq!(validate.schema["required"], json!(["path", "dsl"]));
        assert_eq!(
            validate.schema["properties"]["path"]["description"],
            crate::tool_registry::arguments::SUBTREE_PATH_DESCRIPTION
        );
        let diff = crate::tool_registry::panel_tool("project_diff").unwrap();
        assert_eq!(diff.schema["required"], json!(["old_dsl", "new_dsl"]));
        let context = crate::tool_registry::panel_tool("project_context").unwrap();
        let external =
            json!({"path":"project.properties", "instruction":null, "legacy_extension":true});
        assert!(
            ContextArgs::decode(&external)
                .unwrap()
                .instruction
                .is_none()
        );
        assert!(crate::tool_registry::validate_args(&context, &external).is_err());
        assert!(GetArgs::decode(&json!(["legacy payload"])).is_ok());
        assert!(DslArgs::decode(&Value::Null).is_ok());
    }
}
