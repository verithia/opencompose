//! The saved-conversation commands through their inner functions: which project
//! a listing is scoped to, which session the next turn saves into, the facts a
//! summary names, and the refusals the surface reports.

use std::path::{Path, PathBuf};

use crate::ai::sessions;
use crate::commands::{
    compact_for, current_for, facts, list_for, new_for, new_fork, open_for, sessions_delete,
    sessions_load, sessions_open, sessions_page, sessions_rename, sessions_set_permission,
};
use crate::state::AppState;
use crate::workspaces;

const PROJECT: &str = r#"@ocp "0.2.0"

@project "sessions fixture"
  tempo: 128

@track kick | instrument: synth/kick
  @phrase four [1:1 - 5:1]
    C1 q
"#;

struct Fixture {
    dir: PathBuf,
    _guard: sessions::TestRootGuard,
    _workspaces: workspaces::TestRootGuard,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(label: &str) -> Fixture {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "oc-command-sessions-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = sessions::test_root(dir.join("sessions"));
    let workspace_guard = workspaces::test_root(dir.join("state"));
    Fixture {
        dir,
        _guard: guard,
        _workspaces: workspace_guard,
    }
}

fn state_with_project(dir: &Path, source: &str) -> AppState {
    let state = AppState::new();
    let path = dir.join("song.ocp");
    std::fs::write(&path, source).unwrap();
    let project = oc_dsl::parse(source).expect("fixture parses");
    {
        let mut inner = state.lock_inner();
        inner.dsl_source = oc_dsl::serialize(&project);
        inner.project = project;
        inner.file_path = Some(path);
    }
    state
}

#[test]
fn a_listing_is_scoped_to_the_open_project_unless_all_is_asked_for() {
    let fixture = fixture("scope");
    let state = state_with_project(&fixture.dir, PROJECT);
    let open = new_for(&state, None, Some("open project"), None).unwrap();
    // A different directory is a different workspace.
    let other_dir = fixture.dir.join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other = other_dir.join("other.ocp");
    std::fs::write(&other, PROJECT).unwrap();
    let elsewhere = new_for(
        &state,
        Some(other.to_str().unwrap()),
        Some("other project"),
        None,
    )
    .unwrap();
    {
        let mut inner = state.lock_inner();
        inner.file_path = None;
    }
    let detached = new_for(&state, None, Some("no project"), None).unwrap();
    assert_eq!(detached.workspace_key, sessions::DETACHED_KEY);

    let scoped = list_for(&state, None, false).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, detached.id);
    let all = list_for(&state, None, true).unwrap();
    let ids: Vec<&str> = all.iter().map(|meta| meta.id.as_str()).collect();
    for id in [&open.id, &elsewhere.id, &detached.id] {
        assert!(ids.contains(&id.as_str()), "{id} missing from {ids:?}");
    }
    assert_ne!(open.workspace_key, elsewhere.workspace_key);
    assert_eq!(
        open.workspace.as_deref(),
        Some(fixture.dir.canonicalize().unwrap().to_str().unwrap())
    );
    // A listing by explicit path sees only that workspace's conversation.
    let only_other: Vec<_> = list_for(&state, Some(other.to_str().unwrap()), false).unwrap();
    assert_eq!(only_other.len(), 1);
    assert_eq!(only_other[0].id, elsewhere.id);
}

#[test]
fn the_project_facts_name_the_path_the_digest_and_the_track_count() {
    let fixture = fixture("facts");
    let state = state_with_project(&fixture.dir, PROJECT);
    let inner = state.lock_inner();
    let facts = facts(&inner);
    assert_eq!(
        facts.path.as_deref(),
        Some(inner.file_path.as_deref().unwrap().to_str().unwrap())
    );
    assert_eq!(facts.dsl_digest, sessions::dsl_digest(&inner.dsl_source));
    assert_eq!(facts.track_count, inner.project.tracks.len());
    assert_eq!(facts.dsl_digest.len(), 16);
    assert_eq!(facts.track_count, 1);
}

#[test]
fn renaming_and_deleting_act_on_the_saved_conversation() {
    let fixture = fixture("edit");
    let state = state_with_project(&fixture.dir, PROJECT);
    let session = new_for(&state, None, Some("before"), None).unwrap();
    let renamed = sessions_rename(session.id.clone(), "  after   the fix  ".into()).unwrap();
    assert_eq!(renamed.title, "after the fix");
    assert_eq!(
        sessions_load(session.id.clone()).unwrap().meta.title,
        "after the fix"
    );
    sessions_delete(session.id.clone()).unwrap();
    let error = sessions_load(session.id.clone()).unwrap_err();
    assert!(error.contains("No matching saved session"), "{error}");
    assert!(list_for(&state, None, false).unwrap().is_empty());
    // Deleting twice is a refusal, not a panic.
    assert!(sessions_delete(session.id).is_err());
}

#[test]
fn the_panel_opens_a_conversation_at_its_newest_turns_and_pages_back_through_the_commands() {
    use serde_json::json;
    let fixture = fixture("pages");
    let state = state_with_project(&fixture.dir, PROJECT);
    let session = new_for(&state, None, Some("pages"), None).unwrap();
    let store = sessions::Store::config();
    for index in 0..3 {
        store
            .append_turn(
                &session.id,
                &[
                    json!({"role":"user","content":format!("ask {index}"),
                        "message_meta":{"version":1,"kind":"user"}}),
                    json!({"role":"assistant","content":format!("done {index}"),
                        "message_meta":{"version":1,"kind":"assistant"}}),
                ],
                None,
            )
            .unwrap();
    }
    let opened = serde_json::to_value(sessions_open(session.id.clone(), Some(1)).unwrap()).unwrap();
    assert_eq!(opened["start"], 4);
    assert_eq!(opened["total"], 6);
    assert_eq!(opened["messages"].as_array().unwrap().len(), 2);
    assert_eq!(opened["meta"]["id"], json!(session.id));
    let revision = opened["revision"].as_str().unwrap().to_owned();
    let page =
        serde_json::to_value(sessions_page(session.id.clone(), 4, None, revision.clone()).unwrap())
            .unwrap();
    assert_eq!(page["status"], "page");
    assert_eq!(page["start"], 0);
    assert_eq!(page["messages"][0]["content"], "ask 0");
    let stale =
        serde_json::to_value(sessions_page(session.id.clone(), 4, None, "1:2:3".into()).unwrap())
            .unwrap();
    assert_eq!(stale, json!({"status": "stale"}));
    assert!(sessions_page(session.id, 4, None, "x".repeat(257)).is_err());
}

#[test]
fn a_session_permission_is_set_through_the_command_and_lists_back() {
    use serde_json::json;
    use sessions::SessionPermission;

    let fixture = fixture("permission");
    let state = state_with_project(&fixture.dir, PROJECT);
    let session = new_for(&state, None, Some("permission"), None).unwrap();

    let saved = sessions_set_permission(session.id.clone(), SessionPermission::Autoreview).unwrap();
    assert_eq!(saved.permission, SessionPermission::Autoreview);
    let listed = list_for(&state, None, false).unwrap();
    assert_eq!(listed[0].permission, SessionPermission::Autoreview);

    // An unknown value does not deserialize, so it cannot reach the store.
    assert!(serde_json::from_value::<SessionPermission>(json!("always")).is_err());
    assert_eq!(
        sessions_load(session.id.clone()).unwrap().meta.permission,
        SessionPermission::Autoreview
    );

    // Another session keeps its own mode, which defaults to write.
    let other = new_for(&state, None, Some("other"), None).unwrap();
    assert_eq!(other.permission, SessionPermission::Write);
}

#[test]
fn compacting_an_empty_conversation_is_refused_by_name() {
    let fixture = fixture("compact");
    let state = state_with_project(&fixture.dir, PROJECT);
    let session = new_for(&state, None, Some("empty"), None).unwrap();
    let error = compact_for(&state, &session.id, Some(200)).unwrap_err();
    assert!(error.contains("nothing older"), "{error}");
    assert!(compact_for(&state, "not-a-session", None).is_err());
}

#[test]
fn a_workspace_holds_one_set_of_sessions_across_its_project_files() {
    let fixture = fixture("workspace");
    let state = state_with_project(&fixture.dir, PROJECT);
    let first = new_for(&state, None, Some("on the first file"), None).unwrap();
    // The operator opens another .ocp in the same directory: the workspace's set
    // does not change, and the session records the file it now works on.
    let other = fixture.dir.join("second.ocp");
    std::fs::write(&other, PROJECT).unwrap();
    {
        let mut inner = state.lock_inner();
        inner.file_path = Some(other.clone());
    }
    let listed = list_for(&state, None, false).unwrap();
    assert_eq!(listed.len(), 1, "one workspace, one set of sessions");
    assert_eq!(listed[0].id, first.id);
    assert_eq!(
        listed[0].workspace.as_deref(),
        Some(fixture.dir.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(
        listed[0].project_path.as_deref(),
        Some(first.project_path.as_deref().unwrap())
    );

    // A different directory is a different set.
    let elsewhere = fixture.dir.join("another");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let third = elsewhere.join("song.ocp");
    std::fs::write(&third, PROJECT).unwrap();
    let other_session = new_for(
        &state,
        Some(third.to_str().unwrap()),
        Some("elsewhere"),
        None,
    )
    .unwrap();
    assert_ne!(other_session.workspace_key, first.workspace_key);
    let here = list_for(&state, None, false).unwrap();
    assert_eq!(here.len(), 1);
    assert_eq!(here[0].id, first.id);
}

#[test]
fn a_chosen_workspace_lists_its_conversations_with_no_project_open() {
    let fixture = fixture("chosen");
    // No project is open: the chosen directory is the only scope there is.
    let state = AppState::new();
    let chosen = open_for(&state, fixture.dir.to_str().unwrap()).unwrap();
    assert_eq!(
        chosen.path.as_deref(),
        Some(fixture.dir.canonicalize().unwrap().to_str().unwrap())
    );
    assert!(!chosen.detached);
    assert!(chosen.projects.is_empty(), "the fixture has no .ocp yet");

    let session = new_for(&state, None, Some("no project open"), None).unwrap();
    assert_ne!(session.workspace_key, sessions::DETACHED_KEY);
    let listed = list_for(&state, None, false).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, session.id);
    assert_eq!(
        listed[0].workspace.as_deref(),
        Some(fixture.dir.canonicalize().unwrap().to_str().unwrap())
    );

    // A second directory is a second set; the first one keeps its conversation.
    let elsewhere = fixture.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere_project = elsewhere.join("other.ocp");
    std::fs::write(&elsewhere_project, PROJECT).unwrap();
    let elsewhere_session = new_for(
        &state,
        Some(elsewhere_project.to_str().unwrap()),
        Some("elsewhere"),
        None,
    )
    .unwrap();
    open_for(&state, elsewhere.to_str().unwrap()).unwrap();
    let scoped = list_for(&state, None, false).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, elsewhere_session.id);
    let all = list_for(&state, None, true).unwrap();
    assert_eq!(all.len(), 2);
    assert!(
        all.iter().any(|meta| meta.id == session.id),
        "the first workspace was not touched"
    );

    // A caller that names a project explicitly still gets that project's
    // directory, even while another workspace is chosen.
    let explicit = list_for(&state, Some(elsewhere_project.to_str().unwrap()), false).unwrap();
    assert_eq!(explicit.len(), 1);
    assert_eq!(explicit[0].id, elsewhere_session.id);

    // Continue without a workspace: with no project either, that is detached.
    crate::commands::detached_for(&state);
    assert_eq!(current_for(&state).path, None);
    assert!(current_for(&state).detached);
    assert!(list_for(&state, None, false).unwrap().is_empty());
}

#[test]
fn a_workspace_lists_its_project_files_and_a_file_adopts_its_directory() {
    let fixture = fixture("projects");
    let state = AppState::new();
    std::fs::write(fixture.dir.join("b.ocp"), PROJECT).unwrap();
    std::fs::write(fixture.dir.join("a.ocp"), PROJECT).unwrap();
    std::fs::write(fixture.dir.join("notes.txt"), "not a project").unwrap();
    std::fs::create_dir_all(fixture.dir.join("sub")).unwrap();
    std::fs::write(fixture.dir.join("sub").join("deep.ocp"), PROJECT).unwrap();

    let chosen = open_for(&state, fixture.dir.to_str().unwrap()).unwrap();
    let names: Vec<&str> = chosen
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    assert_eq!(names, ["a.ocp", "b.ocp"], "only this directory, by name");
    assert!(
        chosen
            .projects
            .iter()
            .all(|project| Path::new(&project.path).is_file())
    );

    // Naming a project file adopts the directory that holds it.
    let file = fixture.dir.join("b.ocp");
    let adopted = open_for(&state, file.to_str().unwrap()).unwrap();
    assert_eq!(adopted.path, chosen.path);
    assert_eq!(current_for(&state).name.as_deref(), chosen.name.as_deref());

    // A path that does not exist is refused by name.
    let missing = fixture.dir.join("gone");
    let error = open_for(&state, missing.to_str().unwrap()).unwrap_err();
    assert!(error.contains("workspace"), "{error}");
}

#[test]
fn a_fork_message_count_reaches_the_store_and_a_bare_count_is_refused() {
    let fixture = fixture("fork-through");
    let state = state_with_project(&fixture.dir, PROJECT);
    let parent = new_for(&state, None, Some("parent"), None).unwrap();

    // A count without a parent names no conversation to cut.
    assert!(new_fork(&state, None, None, None, Some(1)).is_err());

    // The parent has no saved message yet, so the cut is refused instead of
    // being read as a whole copy, and it leaves no session behind.
    assert!(new_fork(&state, None, Some("child"), Some(&parent.id), Some(1)).is_err());
    assert_eq!(list_for(&state, None, false).unwrap().len(), 1);

    // The same parent without a count forks the whole conversation.
    let whole = new_fork(&state, None, Some("whole"), Some(&parent.id), None).unwrap();
    assert_eq!(whole.parent.as_deref(), Some(parent.id.as_str()));
    assert_eq!(list_for(&state, None, false).unwrap().len(), 2);
}

#[test]
fn a_headless_turn_bound_to_another_open_file_is_refused_as_session_binding() {
    use crate::ai::session_usage::{Purpose, SessionLease};
    use crate::ai_service::{AiError, AiErrorDetail};
    use crate::commands::{
        AiCapabilities, AiProviderEntry, AuthKind, ProviderKind, SavedTurnBinding, Wire,
        prepare_turn_for,
    };
    use serde_json::json;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    let fixture = fixture("headless-binding");
    let state = state_with_project(&fixture.dir, PROJECT);
    let open = fixture.dir.join("song.ocp");
    let other = fixture.dir.join("other.ocp");
    std::fs::write(&other, PROJECT).unwrap();
    let session = new_for(
        &state,
        Some(other.to_str().unwrap()),
        Some("other file"),
        None,
    )
    .unwrap();
    let store = sessions::Store::config();
    let lease = SessionLease::admit(store, &session.id, "binding-request", Purpose::Chat).unwrap();
    let incarnation = lease.update().incarnation;
    let provider = AiProviderEntry {
        id: "p".into(),
        name: "p".into(),
        kind: ProviderKind::OpenAiChatCompletions,
        wire: Some(Wire::OpenAiChatCompletions),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: "https://example.test/v1/chat/completions".into(),
        model: "fixture-model".into(),
        models: Vec::new(),
        context_length: 1_000_000,
        max_tokens: 16_000,
        temperature: 0.3,
        thinking_level: "medium".into(),
        capabilities: AiCapabilities::default(),
    };
    let messages = json!([
        {"role":"system","content":"prompt"},
        {"role":"user","content":"hi","message_meta":{"version":1,"kind":"user"}},
    ]);
    let app = mock_builder()
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap();
    let error = tauri::async_runtime::block_on(prepare_turn_for(
        &app.handle().clone(),
        &provider,
        &messages,
        SavedTurnBinding {
            id: &session.id,
            incarnation: &incarnation,
            project: Some(&open),
            tool_tokens: 0,
            surface: None,
        },
    ))
    .unwrap_err();
    assert_eq!(error.kind(), "session_binding");
    let error: AiError = error.into();
    assert_eq!(error.kind, "session_binding");
    assert_eq!(
        error.detail,
        Some(AiErrorDetail::BindingMismatch {
            bound: other.canonicalize().unwrap().to_string_lossy().to_string(),
            open: open.canonicalize().unwrap().to_string_lossy().to_string(),
        })
    );
    let (_, saved) = sessions::Store::config().find(&session.id).unwrap();
    assert_eq!(
        saved.project_path.as_deref(),
        Some(other.canonicalize().unwrap().to_string_lossy().as_ref())
    );
    drop(lease);
}

#[test]
fn the_binding_commands_compare_canonically_and_bind_to_the_open_file() {
    let fixture = fixture("binding-commands");
    let state = state_with_project(&fixture.dir, PROJECT);
    let first = fixture.dir.join("song.ocp");
    let second = fixture.dir.join("second.ocp");
    std::fs::write(&second, PROJECT).unwrap();
    let session = new_for(&state, None, Some("bound"), None).unwrap();
    {
        let mut inner = state.lock_inner();
        inner.file_path = Some(second.clone());
    }
    let status = crate::commands::binding_for(&state, &session.id).unwrap();
    assert_eq!(
        status.bound.as_deref(),
        Some(first.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(
        status.open.as_deref(),
        Some(second.canonicalize().unwrap().to_str().unwrap())
    );
    assert!(!status.matches);
    let bound = crate::commands::bind_for(&state, &session.id).unwrap();
    assert_eq!(
        bound.project_path.as_deref(),
        Some(second.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(
        bound
            .pending_binding
            .as_ref()
            .and_then(|pending| pending.from.as_deref()),
        Some(first.canonicalize().unwrap().to_str().unwrap())
    );
    assert!(
        crate::commands::binding_for(&state, &session.id)
            .unwrap()
            .matches
    );
}

fn outside_provider() -> crate::commands::AiProviderEntry {
    use crate::commands::{AiCapabilities, AuthKind, ProviderKind, Wire};
    crate::commands::AiProviderEntry {
        id: "p".into(),
        name: "p".into(),
        kind: ProviderKind::OpenAiChatCompletions,
        wire: Some(Wire::OpenAiChatCompletions),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: "https://example.test/v1/chat/completions".into(),
        model: "fixture-model".into(),
        models: Vec::new(),
        context_length: 1_000_000,
        max_tokens: 16_000,
        temperature: 0.3,
        thinking_level: "medium".into(),
        capabilities: AiCapabilities::default(),
    }
}

#[test]
fn a_prepared_turn_reports_edits_made_outside_its_conversation() {
    use crate::ai::agent_loop::wire;
    use crate::ai::session_usage::{Purpose, SessionLease};
    use crate::commands::{SavedTurnBinding, prepare_turn_for, update_dsl_inner};
    use serde_json::json;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    let fixture = fixture("outside-turn");
    let state = state_with_project(&fixture.dir, PROJECT);
    let open = fixture.dir.join("song.ocp");
    let session = new_for(&state, None, Some("outside"), None).unwrap();
    let store = sessions::Store::config();
    {
        let mut inner = state.lock_inner();
        update_dsl_inner(PROJECT.replace("tempo: 128", "tempo: 120"), &mut inner).unwrap();
        let mark = inner.undo.mark();
        store.record_history_mark(&session.id, &open, mark).unwrap();
        update_dsl_inner(PROJECT.replace("tempo: 128", "tempo: 132"), &mut inner).unwrap();
    }
    let lease = SessionLease::admit(store, &session.id, "outside-request", Purpose::Chat).unwrap();
    let incarnation = lease.update().incarnation;
    let provider = outside_provider();
    let messages = json!([
        {"role":"system","content":"prompt"},
        {"role":"user","content":"hi","message_meta":{"version":1,"kind":"user"}},
    ]);
    let app = mock_builder()
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap();
    let prepared = tauri::async_runtime::block_on(prepare_turn_for(
        &app.handle().clone(),
        &provider,
        &messages,
        SavedTurnBinding {
            id: &session.id,
            incarnation: &incarnation,
            project: Some(&open),
            tool_tokens: wire::offered_tool_tokens(None, false),
            surface: None,
        },
    ))
    .unwrap();
    let rows = prepared.messages.as_array().unwrap();
    assert_eq!(rows.last().unwrap()["content"], "hi");
    let notice_row = rows
        .iter()
        .find(|row| row["message_meta"]["notice"]["kind"] == "outside_changes")
        .expect("an outside changes row");
    let notice: crate::ai::host_notice::HostNotice =
        serde_json::from_value(notice_row["message_meta"]["notice"].clone()).unwrap();
    match notice {
        crate::ai::host_notice::HostNotice::OutsideChanges {
            entries,
            more,
            undone,
            rewritten,
        } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].author, crate::edit_history::Author::Person);
            assert_eq!(more, 0);
            assert_eq!(undone, 0);
            assert!(!rewritten);
        }
        other => panic!("expected outside changes, got {other:?}"),
    }
    assert!(
        notice_row["content"]
            .as_str()
            .is_some_and(|text| text.contains("Edits by others, newest first:"))
    );
    drop(lease);
}

#[test]
fn a_pending_binding_suppresses_the_outside_changes_notice() {
    use crate::ai::agent_loop::wire;
    use crate::ai::host_notice::BindCause;
    use crate::ai::session_usage::{Purpose, SessionLease};
    use crate::commands::{SavedTurnBinding, prepare_turn_for, update_dsl_inner};
    use serde_json::json;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    let fixture = fixture("outside-suppressed");
    let state = state_with_project(&fixture.dir, PROJECT);
    let first = fixture.dir.join("song.ocp");
    let second = fixture.dir.join("second.ocp");
    std::fs::write(&second, PROJECT).unwrap();
    let session = new_for(&state, None, Some("moved"), None).unwrap();
    let store = sessions::Store::config();
    store
        .bind(&session.id, Some(&second), BindCause::Chosen)
        .unwrap();
    {
        let mut inner = state.lock_inner();
        inner.file_path = Some(second.clone());
        update_dsl_inner(PROJECT.replace("tempo: 128", "tempo: 120"), &mut inner).unwrap();
        let mark = inner.undo.mark();
        store
            .record_history_mark(&session.id, &second, mark)
            .unwrap();
        update_dsl_inner(PROJECT.replace("tempo: 128", "tempo: 132"), &mut inner).unwrap();
    }
    let lease = SessionLease::admit(store, &session.id, "moved-request", Purpose::Chat).unwrap();
    let incarnation = lease.update().incarnation;
    let provider = outside_provider();
    let messages = json!([
        {"role":"system","content":"prompt"},
        {"role":"user","content":"hi","message_meta":{"version":1,"kind":"user"}},
    ]);
    let app = mock_builder()
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap();
    let prepared = tauri::async_runtime::block_on(prepare_turn_for(
        &app.handle().clone(),
        &provider,
        &messages,
        SavedTurnBinding {
            id: &session.id,
            incarnation: &incarnation,
            project: Some(&second),
            tool_tokens: wire::offered_tool_tokens(None, false),
            surface: None,
        },
    ))
    .unwrap();
    let rows = prepared.messages.as_array().unwrap();
    assert_eq!(rows.last().unwrap()["content"], "hi");
    assert!(
        rows.iter()
            .any(|row| row["message_meta"]["notice"]["kind"] == "project_bound")
    );
    assert!(
        rows.iter()
            .all(|row| row["message_meta"]["notice"]["kind"] != "outside_changes")
    );
    drop(lease);
    let _ = first;
}
