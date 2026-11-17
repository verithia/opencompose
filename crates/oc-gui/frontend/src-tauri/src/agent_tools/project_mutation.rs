//! Typed project edits delegate to the shared command and undo owners.

use std::path::{Path, PathBuf};

use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde_json::{Value, json};
use tauri::State;

use super::project_query::validation_value;
use crate::commands;
use crate::state::AppState;
use crate::tool_registry::ToolId;
use crate::tool_registry::arguments::{
    ArgumentError, ProjectRevision, ToolArguments, empty_arguments, typed_arguments,
};

empty_arguments! {
    UndoArgs => Undo,
    RedoArgs => Redo,
}
typed_arguments! {
    /// Create a new empty project at an absolute .ocp path.
    pub struct ProjectNewArgs => ProjectNew {
        /// Absolute path to the new .ocp file
        #[schemars(schema_with = "project_path_schema")]
        pub path: String,
        /// Optional starter content; "starter" is the only template
        pub template: Option<String>,
    }
    /// Set the project tempo in BPM (20-999).
    pub struct SetTempoArgs => ProjectSetTempo {
        /// Tempo in BPM
        pub tempo: f64,
    }
    /// Set the project name.
    pub struct SetNameArgs => ProjectSetName {
        /// Project name
        pub name: String,
    }
    /// Open an .ocp project from disk.
    pub struct ProjectOpenArgs => ProjectOpen {
        /// Absolute path to an .ocp file
        pub path: String,
    }
    /// Save the project. Omit path to save to the current file.
    pub struct ProjectSaveArgs => ProjectSave {
        /// Optional .ocp path
        pub path: Option<String>,
    }
    /// Import up to 64 same-origin mono or stereo WAV stems and one optional MIDI file into a saved project with no tracks or sections. Supply explicit project-relative paths inside handoff/, where the operator staged the exports. WAVs start at 1:1 with source speed and leading silence intact. Stereo files use unity-gain balance at center, including dual-mono prints. MIDI sets the project clock and starts muted, with raw key gates and separate sustain lanes by channel. Use the current revision from project_get. Conflicting Broadcast WAV start timestamps are refused. Returns revision, import counts and the number of timestamped stems. Missing timestamps leave common origin unverified. Inspect with audio_clips_list, then verify alignment before editing.
    pub struct ImportAlignedStemsArgs => ImportAlignedStems {
        #[schemars(schema_with = "handoff_paths_schema")]
        pub paths: Vec<String>,
        /// Current project revision from project_get
        pub expected_revision: ProjectRevision,
    }
    /// Apply one complete DSL subtree replacement. Returns the new snapshot and a diff; path grammar is described by `read_skill dsl-subtree-paths`.
    pub struct EditArgs => ProjectEdit {
        #[schemars(schema_with = "crate::tool_registry::arguments::subtree_path_schema")]
        pub path: String,
        /// Complete replacement DSL
        pub dsl: String,
        /// Optional optimistic-concurrency guard; defaults to current DSL
        pub expected_dsl: Option<String>,
    }
}

/// Longest project path the tool accepts, matching the session binding bound.
pub const MAX_PROJECT_PATH_CHARS: usize = 4096;

/// The new project's path is absolute, ends in `.ocp` and is bounded.
fn project_path_schema(_: &mut SchemaGenerator) -> Schema {
    serde_json::from_value(json!({
        "type": "string",
        "description": "Absolute path to the new .ocp file",
        "maxLength": MAX_PROJECT_PATH_CHARS,
    }))
    .expect("the project path schema is valid")
}

/// Check the path a new project will occupy before the main thread sees it.
///
/// The tool creates a file, so an existing path is refused instead of
/// overwritten; the GUI's save picker owns its own confirmation.
pub(crate) fn validate_new_project_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.chars().count() > MAX_PROJECT_PATH_CHARS {
        return Err(format!(
            "project_new needs a path of at most {MAX_PROJECT_PATH_CHARS} characters"
        ));
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("project_new needs an absolute .ocp path".into());
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ocp"))
    {
        return Err("project_new needs a path ending in .ocp".into());
    }
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    Ok(path.to_path_buf())
}

/// The handoff path list keeps its item description and bounds inline.
fn handoff_paths_schema(_: &mut SchemaGenerator) -> Schema {
    serde_json::from_value(json!({
        "type": "array",
        "minItems": 1,
        "maxItems": 65,
        "items": {
            "type": "string",
            "description": "Project-relative file path such as handoff/Drums.wav"
        }
    }))
    .expect("the handoff path schema is valid")
}

#[derive(Debug)]
/// Decoded request for project mutations.
pub enum Request {
    SetTempo(SetTempoArgs),
    SetName(SetNameArgs),
    Edit(EditArgs),
    Undo(UndoArgs),
    Redo(RedoArgs),
}
impl Request {
    /// Decode a tool call into a request.
    pub fn decode(id: ToolId, args: &Value) -> Result<Self, ArgumentError> {
        match id {
            SetTempoArgs::ID => SetTempoArgs::decode(args).map(Self::SetTempo),
            SetNameArgs::ID => SetNameArgs::decode(args).map(Self::SetName),
            EditArgs::ID => EditArgs::decode(args).map(Self::Edit),
            UndoArgs::ID => UndoArgs::decode(args).map(Self::Undo),
            RedoArgs::ID => RedoArgs::decode(args).map(Self::Redo),
            _ => Err(ArgumentError::wrong_tool(id)),
        }
    }
}

/// The dispatcher enforces preview, batch ownership and the revision envelope
/// before entering the command owner, as it does for other project mutations.
pub fn execute(state: State<'_, AppState>, request: Request) -> Result<Value, String> {
    let snapshot = match request {
        Request::SetTempo(args) => commands::set_tempo(args.tempo, state)?,
        Request::SetName(args) => commands::set_project_name(args.name, state)?,
        Request::Undo(_) => commands::undo(state)?,
        Request::Redo(_) => commands::redo(state)?,
        Request::Edit(args) => {
            let before = commands::get_dsl_source(state.clone())?;
            let expected = args.expected_dsl.unwrap_or_else(|| before.clone());
            let check =
                commands::ai_validate_edit(args.dsl.clone(), args.path.clone(), state.clone())?;
            if !check.valid {
                return Ok(validation_value(check));
            }
            let snapshot = commands::ai_apply_edit(args.dsl, args.path, expected, state)?;
            let diff = commands::diff_dsl(before, snapshot.dsl_source.clone());
            return Ok(json!({"snapshot":snapshot, "diff":diff}));
        }
    };
    serde_json::to_value(snapshot).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::{
        Manager,
        test::{MockRuntime, mock_builder, mock_context, noop_assets},
    };

    fn app() -> tauri::App<MockRuntime> {
        mock_builder()
            .manage(AppState::new())
            .build(mock_context(noop_assets()))
            .unwrap()
    }
    fn run(app: &tauri::App<MockRuntime>, id: ToolId, value: Value) -> Result<Value, String> {
        let request = Request::decode(id, &value).map_err(|error| error.to_string())?;
        execute(app.state::<AppState>(), request)
    }
    fn snapshot(app: &tauri::App<MockRuntime>) -> Value {
        serde_json::to_value(commands::snapshot(&app.state::<AppState>().lock_inner())).unwrap()
    }

    #[test]
    fn typed_tempo_and_name_edits_use_the_existing_undo_and_redo_owner() {
        let app = app();
        let before = snapshot(&app);
        assert!(app.state::<AppState>().audio_simulated());
        let initial_tempo = before["project"]["properties"]["tempo"].clone();
        run(
            &app,
            ToolId::ProjectSetTempo,
            json!({"tempo":initial_tempo}),
        )
        .unwrap();
        assert!(
            app.state::<AppState>().lock_inner().undo.undo.is_empty(),
            "an unchanged tempo must not create history"
        );
        let changed = run(
            &app,
            ToolId::ProjectSetTempo,
            json!({"tempo":150, "legacy_extension":true}),
        )
        .unwrap();
        assert_eq!(changed["project"]["properties"]["tempo"], 150.0);
        assert!(changed["revision"].as_u64().unwrap() > before["revision"].as_u64().unwrap());
        assert_eq!(app.state::<AppState>().lock_inner().undo.undo.len(), 1);
        let undone = run(&app, ToolId::Undo, Value::Null).unwrap();
        assert_eq!(undone["project"], before["project"]);
        let redone = run(&app, ToolId::Redo, json!({"ignored":true})).unwrap();
        assert_eq!(redone["project"], changed["project"]);
        let named = run(&app, ToolId::ProjectSetName, json!({"name":"Typed edit"})).unwrap();
        assert_eq!(named["project"]["name"], "Typed edit");
        assert_eq!(app.state::<AppState>().lock_inner().undo.undo.len(), 2);
        let stable = snapshot(&app);
        assert!(run(&app, ToolId::ProjectSetTempo, json!({"tempo":19})).is_err());
        assert_eq!(snapshot(&app), stable);
    }

    #[test]
    fn typed_dsl_edits_keep_validation_and_optimistic_failure_atomic() {
        let app = app();
        let before = snapshot(&app);
        let invalid = run(
            &app,
            ToolId::ProjectEdit,
            json!({"path":"doc", "dsl":"tempo: 132"}),
        )
        .unwrap();
        assert_eq!(invalid["ok"], false);
        assert_eq!(invalid["valid"], false);
        assert_eq!(snapshot(&app), before);
        assert!(
            run(
                &app,
                ToolId::ProjectEdit,
                json!({"path":"project.properties", "dsl":"tempo: 132", "expected_dsl":"stale"})
            )
            .is_err()
        );
        assert_eq!(snapshot(&app), before);
        assert!(app.state::<AppState>().lock_inner().undo.undo.is_empty());
        let applied = run(
            &app,
            ToolId::ProjectEdit,
            json!({"path":"project.properties", "dsl":"tempo: 132", "expected_dsl":null}),
        )
        .unwrap();
        assert_eq!(applied["snapshot"]["project"]["properties"]["tempo"], 132.0);
        assert!(!applied["diff"]["hunks"].as_array().unwrap().is_empty());
        assert_eq!(app.state::<AppState>().lock_inner().undo.undo.len(), 1);
        let undone = run(&app, ToolId::Undo, json!({})).unwrap();
        assert_eq!(undone["project"], before["project"]);
    }

    #[test]
    fn typed_history_keeps_allowed_undo_and_refused_redo_in_an_open_transaction() {
        let app = app();
        let initial = snapshot(&app);
        run(&app, ToolId::ProjectSetTempo, json!({"tempo":150})).unwrap();
        let before = snapshot(&app);
        app.state::<AppState>()
            .begin_transaction("Open batch".into(), "owner".into())
            .unwrap();
        let undone = run(&app, ToolId::Undo, json!({})).unwrap();
        assert_eq!(undone["project"], initial["project"]);
        assert_eq!(
            app.state::<AppState>().batch_owner().as_deref(),
            Some("owner")
        );
        let after_undo = snapshot(&app);
        assert!(run(&app, ToolId::Redo, json!({})).is_err());
        assert_eq!(snapshot(&app), after_undo);
        app.state::<AppState>().rollback_transaction().unwrap();
        assert_eq!(snapshot(&app)["project"], before["project"]);
        assert_eq!(app.state::<AppState>().lock_inner().undo.undo.len(), 1);
    }

    #[test]
    fn typed_mutation_schemas_keep_legacy_fields_and_strict_panel_validation() {
        let external = json!({"path":"project.properties", "dsl":"tempo: 132", "expected_dsl":null, "legacy_extension":true});
        assert!(EditArgs::decode(&external).unwrap().expected_dsl.is_none());
        let definition = crate::tool_registry::catalog()
            .into_iter()
            .find(|tool| tool.id == ToolId::ProjectEdit)
            .unwrap();
        assert_eq!(definition.schema["required"], json!(["path", "dsl"]));
        assert!(crate::tool_registry::validate_args(&definition, &external).is_err());
        let revision = &definition.schema["properties"]["expected_revision"];
        assert_eq!(revision["type"], "integer");
        assert!(revision.get("minimum").is_none());
        assert!(revision.get("format").is_none());
        let error = SetTempoArgs::decode(&json!({"tempo":"fast"})).unwrap_err();
        assert_eq!(error.tool, ToolId::ProjectSetTempo);
        assert_eq!(error.field.as_deref(), Some("tempo"));
    }

    #[test]
    fn a_validated_new_project_path_creates_the_file_and_its_history_marker() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Fresh.ocp");
        crate::edit_history::enable_persistence_for_test();
        let path = validate_new_project_path(target.to_str().unwrap()).unwrap();
        let state = AppState::new();
        let snapshot = commands::create_project_in(&mut state.lock_inner(), Some(path)).unwrap();
        assert_eq!(snapshot.project.name.as_deref(), Some("Fresh"));
        assert_eq!(
            snapshot.file_path.as_deref(),
            Some(target.to_str().unwrap())
        );
        assert!(target.is_file(), "the tool did not write the project");
        let marker = crate::edit_history::journal::read_marker(
            &crate::edit_history::journal::dir_for(&target),
        )
        .expect("the new project has a history marker");
        assert_eq!(
            marker.sha256,
            crate::project_data::content_sha256(&target),
            "the marker names the file the write produced"
        );
        assert_eq!(marker.path, crate::project_data::canonical_path(&target));
        // The validated path refuses a file it would replace.
        let error = validate_new_project_path(target.to_str().unwrap()).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn typed_project_file_schemas_preserve_each_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        for id in [ToolId::ProjectNew, ToolId::ProjectOpen, ToolId::ProjectSave] {
            let expected = expected
                .iter()
                .find(|row| row["name"] == id.name())
                .unwrap();
            let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
            assert_eq!(actual, expected, "{}", id.name());
        }
    }

    #[test]
    fn typed_project_file_requests_decode_without_opening_a_file() {
        let new = ProjectNewArgs::decode(&json!({
            "path": "/songs/Fresh.ocp",
            "template": "starter",
            "expected_revision": 3,
        }))
        .unwrap();
        assert_eq!(new.path, "/songs/Fresh.ocp");
        assert_eq!(new.template.as_deref(), Some("starter"));
        // The path is required, and a relative one is refused after decoding.
        let missing = ProjectNewArgs::decode(&json!({})).unwrap_err();
        assert_eq!(
            missing.code,
            crate::tool_registry::arguments::ArgumentErrorCode::Missing
        );
        assert_eq!(missing.field.as_deref(), Some("path"));
        assert!(validate_new_project_path("song.ocp").is_err());
        assert!(validate_new_project_path("/songs/Fresh.wav").is_err());
        let open = ProjectOpenArgs::decode(&json!({
            "path": "song.ocp",
            "expected_revision": 3,
        }))
        .unwrap();
        assert_eq!(open.path, "song.ocp");
        assert!(ProjectOpenArgs::decode(&json!({})).is_err());
        let save = ProjectSaveArgs::decode(&json!({"path": null})).unwrap();
        assert!(save.path.is_none());
    }

    #[test]
    fn typed_import_schemas_preserve_the_provider_contract() {
        let expected: Vec<Value> =
            serde_json::from_str(include_str!("../tool_registry/schema-contract.json")).unwrap();
        let actual = crate::tool_registry::mcp_tools();
        let id = ToolId::ImportAlignedStems;
        let expected = expected
            .iter()
            .find(|row| row["name"] == id.name())
            .unwrap();
        let actual = actual.iter().find(|row| row["name"] == id.name()).unwrap();
        assert_eq!(actual, expected, "{}", id.name());
    }

    #[test]
    fn typed_import_requires_paths_and_the_revision() {
        let request = ImportAlignedStemsArgs::decode(&json!({
            "paths": ["handoff/Drums.wav"],
            "expected_revision": 4,
        }))
        .unwrap();
        assert_eq!(request.paths, vec!["handoff/Drums.wav".to_string()]);
        assert_eq!(request.expected_revision.0, 4);
        let error =
            ImportAlignedStemsArgs::decode(&json!({"paths": ["handoff/Drums.wav"]})).unwrap_err();
        assert_eq!(error.field.as_deref(), Some("expected_revision"));
    }
}
