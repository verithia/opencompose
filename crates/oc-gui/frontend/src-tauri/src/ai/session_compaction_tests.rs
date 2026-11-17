//! Compaction and context bounding: what a summary keeps of the newest turn,
//! how the store refuses or reports, and how a size trim keeps the summary.

use super::summary::{MAX_SUMMARY_BYTES, previous_summary};
use super::*;
use std::collections::BTreeMap;

fn user(text: &str) -> Value {
    json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
}

fn reply(text: &str) -> Value {
    json!({"role":"assistant","content":text,"message_meta":{"version":1,"kind":"assistant"}})
}

fn round(ids: &[&str]) -> Value {
    json!({
        "role":"assistant","content":null,"message_meta":{"version":1,"kind":"assistant"},
        "tool_calls": ids.iter().map(|id| json!({
            "id": id, "type":"function",
            "function":{"name":format!("tool_{id}"),"arguments":format!("{{\"which\":\"{id}\"}}")}
        })).collect::<Vec<_>>()
    })
}

fn result(id: &str) -> Value {
    json!({
        "role":"tool","tool_call_id":id,"message_meta":{"version":1,"kind":"tool"},
        "content": json!({"ok": true, "answer": format!("result-{id}"), "rows": "r".repeat(600)}).to_string()
    })
}

fn change(id: &str) -> Value {
    json!({
        "role":"assistant","content":null,"message_meta":{"version":1,"kind":"assistant"},
        "tool_calls": [{"id": id, "type":"function",
            "function":{"name":"tracks_add","arguments":"{}"}}]
    })
}

fn changed(id: &str, marker: &str) -> Value {
    json!({
        "role":"tool","tool_call_id":id,"message_meta":{"version":1,"kind":"tool"},
        "content": json!({"ok": true, "marker": marker}).to_string()
    })
}

fn image() -> Value {
    json!({"kind":"image","name":"shot.png","path":"/attachments/shot.png","mime":"image/png",
           "bytes":2048,"sha256":"a".repeat(64)})
}

fn audio() -> Value {
    json!({"kind":"audio","name":"take.wav","path":"/attachments/take.wav","mime":"audio/wav",
           "bytes":4096,"duration_secs":3.0,"sha256":"b".repeat(64)})
}

fn facts() -> ProjectFacts {
    ProjectFacts {
        path: Some("/songs/song.ocp".into()),
        dsl_digest: dsl_digest("@project\n"),
        track_count: 2,
    }
}

/// An ask followed by eight answered tool rounds.
fn rounds() -> Vec<Value> {
    let mut view = vec![user("make the kick punchier")];
    for step in 0..8 {
        let id = format!("c{step}");
        view.push(round(&[&id]));
        view.push(result(&id));
    }
    view
}

#[test]
fn the_pending_ask_after_a_tool_round_keeps_its_image_or_audio() {
    for media in [image(), audio()] {
        let mut view = rounds();
        // The person's message arrives directly after the last round's result.
        let mut ask = user("what about this one?");
        ask["media"] = json!([media.clone()]);
        view.push(ask);
        let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
        let last = compaction.messages.last().unwrap();
        assert_eq!(last["content"], "what about this one?");
        assert_eq!(last["media"], json!([canonical_media(&json!([media]))[0]]));
        assert_eq!(message_kind(last), Some(MessageKind::User));
        assert!(last.get("context").is_none(), "{last}");
        assert!(
            !compaction.messages.iter().any(is_audition),
            "the attachment is not turned into a host audition"
        );
        assert!(strips_replay_state(&compaction.messages));
    }
}

#[test]
fn retained_user_rows_keep_their_snapshot_history_position_and_steering_id() {
    let mut view = rounds();
    let mut steer = user("and softer highs");
    steer["message_meta"]["steer_id"] = json!("row-7");
    view.push(steer);
    let mut ask = user("now the bass");
    ask["dsl_sha"] = json!("c".repeat(64));
    ask["dsl_project"] = json!("/songs/song.ocp");
    ask["history_at"] = json!({"project": "/songs/song.ocp", "level": 4, "at": 11});
    view.push(ask);
    let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
    let rows = &compaction.messages;
    let steer = rows
        .iter()
        .find(|row| row["content"] == "and softer highs")
        .unwrap();
    assert_eq!(steer["message_meta"]["steer_id"], "row-7");
    let ask = rows.last().unwrap();
    assert_eq!(ask["dsl_sha"], "c".repeat(64));
    assert_eq!(ask["dsl_project"], "/songs/song.ocp");
    assert_eq!(
        ask["history_at"],
        json!({"project": "/songs/song.ocp", "level": 4, "at": 11})
    );
    assert!(strips_replay_state(rows));
}

#[test]
fn recall_to_the_newest_message_works_after_auto_compaction_with_a_snapshot() {
    let (store, project) = fixture("compaction-revert");
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    let window = estimate_tokens(&view);
    let history = crate::ai::sessions::HistoryAt {
        project: crate::ai::sessions::canonical_path(&project)
            .to_string_lossy()
            .into_owned(),
        level: 1,
        at: Some(42),
        max_id: 42,
    };
    let prepared = store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            PrepareContext {
                snapshot: Some("@project\n  tempo: 120\n"),
                history: Some(&history),
                overhead: 0,
                context_length: window,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(),
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(prepared.compacted.is_some(), "the turn compacts");
    let index = prepared.messages.len() - 1;
    assert_eq!(prepared.messages[index]["content"], "now the bass");
    let point = store.recall_point(&session.id, index).unwrap();
    assert_eq!(
        point.history_at,
        Some(history),
        "compaction kept the position"
    );
    assert_eq!(
        store
            .dsl_version(
                &session.id,
                prepared.messages[index]["dsl_sha"].as_str().unwrap()
            )
            .unwrap(),
        "@project\n  tempo: 120\n"
    );
    assert_eq!(store.cut_before_user(&session.id, &point).unwrap(), index);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn retained_pairs_name_every_call_of_a_parallel_batch() {
    let mut view = vec![user("check all three")];
    view.push(round(&["a", "b", "c"]));
    for id in ["a", "b", "c"] {
        view.push(result(id));
    }
    view.push(reply("done"));
    view.push(user("next"));
    let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
    let summary = compaction.messages[0]["content"].as_str().unwrap();
    for id in ["a", "b", "c"] {
        assert!(summary.contains(&format!("tool_{id}")), "{summary}");
        assert!(summary.contains(&format!("result-{id}")), "{summary}");
    }
}

#[test]
fn a_placeholder_window_never_compacts_a_small_conversation() {
    let view = vec![user("hello"), reply("hi"), user("again")];
    for window in [0, 1] {
        assert!(!should_compact(&view, 0, window, 0, 0), "{window}");
    }
}

#[test]
fn attachments_count_toward_the_compaction_threshold() {
    let mut view = vec![user(&"words ".repeat(400)), reply("ok")];
    let text = estimate_tokens(&view);
    let window = text * 2;
    assert!(!should_compact(&view, 0, window, 0, 0));
    view[0]["media"] = json!([image(), image()]);
    assert!(media_tokens(&view) >= 2 * IMAGE_TOKENS);
    assert!(should_compact(&view, 0, window, 0, 0));
}

#[test]
fn a_disk_only_audio_reference_costs_no_tokens() {
    let mut local = audio();
    local["local_reference"] = json!(true);
    let mut row = user("listen");
    row["media"] = json!([local]);
    assert_eq!(media_tokens(&[row]), 0);
    let mut row = user("listen");
    row["media"] = json!([audio()]);
    assert_eq!(
        media_tokens(&[row]),
        96,
        "three seconds at 32 tokens a second"
    );
}

#[test]
fn a_size_trim_keeps_the_latest_compaction_summary() {
    let mut view = rounds();
    view.push(user("now the bass"));
    let mut conversation = plan_compaction(&view, 0, &facts(), "s").unwrap().messages;
    assert!(is_compaction_summary(&conversation[0]));
    let big = "x".repeat(4_000);
    for step in 0..20 {
        let id = format!("t{step}");
        conversation.push(round(&[&id]));
        conversation.push(json!({"role":"tool","tool_call_id":id,"content":big}));
    }
    let kept = newest_that_fit(&conversation, 30_000);
    assert!(kept.len() < conversation.len(), "the trim drops rows");
    assert!(is_compaction_summary(&kept[0]), "{}", kept[0]);
    assert_eq!(turns::check_pairing(&kept), Ok(()));
}

#[test]
fn a_failed_auto_compaction_is_reported_and_the_turn_still_prepares() {
    let (store, project) = fixture("compaction-failure");
    let session = store.create(None, None, None, None).unwrap();
    // More rows than auto-compaction allows, all inside the newest rounds a
    // summary keeps whole, so there is nothing older to fold.
    let mut view = Vec::new();
    for batch in 0..3 {
        let ids: Vec<String> = (0..30).map(|call| format!("b{batch}c{call}")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        view.push(round(&ids));
        view.extend(ids.iter().map(|id| result(id)));
    }
    assert!(view.len() > MAX_AUTO_COMPACT_MESSAGES);
    let prepared = store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(),
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(prepared.compacted.is_none());
    assert!(prepared.compaction_error.is_some());
    assert_eq!(prepared.messages.len(), view.len());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

/// Every file under the store root with its bytes; the lock file is the one
/// a read takes too, so it is left out.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name != ".sessions.lock")
            {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn a_refused_admission_saves_nothing() {
    let (store, project) = fixture("admission-refused");
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let project_facts = facts();
    let context = || PrepareContext {
        snapshot: Some("@project\n"),
        history: None,
        overhead: 0,
        context_length: 0,
        max_output: 0,
        prompt_tokens: 0,
        facts: &project_facts,
        provider: "p",
        model: "m",
        outside: None,
        surface: None,
    };
    store
        .prepare(&session.id, &[user("first")], Some(&project), context())
        .unwrap();
    let before = tree(store.root());
    let mut seen = Vec::new();
    let refused = store.prepare_admitted(
        &session.id,
        &[user(&"pasted ".repeat(1_000))],
        Some(&project),
        PrepareContext {
            snapshot: Some("@project\n  tempo: 90\n"),
            history: None,
            outside: None,
            ..context()
        },
        None,
        |planned| {
            seen = planned.to_vec();
            Err("too large".to_owned())
        },
    );
    assert_eq!(
        refused.unwrap_err(),
        PrepareFailure::Admitted("too large".to_owned())
    );
    assert_eq!(
        seen.len(),
        2,
        "admission sees the conversation it would send"
    );
    assert_eq!(tree(store.root()), before, "nothing was written");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

/// Reads every session's metadata from a fixed copy, so the file on disk can
/// be made unwritable without hiding the session.
#[derive(Debug)]
struct FixedMeta(SessionMeta);

impl lookup::Io for FixedMeta {
    fn workspaces(&self, root: &Path) -> lookup::Workspaces {
        lookup::Disk.workspaces(root)
    }
    fn metadata(&self, _path: &Path) -> Result<Option<SessionMeta>, lookup::Error> {
        Ok(Some(self.0.clone()))
    }
}

#[test]
fn a_landed_compaction_succeeds_even_when_its_metadata_cannot_be_written() {
    let (store, project) = fixture("compaction-meta");
    let session = store.create(None, None, None, None).unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(),
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let (dir, meta) = store.find(&session.id).unwrap();
    // A directory where the metadata file goes makes its atomic replace fail.
    std::fs::remove_file(dir.join(META_FILE)).unwrap();
    std::fs::create_dir(dir.join(META_FILE)).unwrap();
    let blocked = Store::with_lookup(store.root(), Arc::new(FixedMeta(meta)));
    let outcome = blocked.compact(&session.id, None, &facts()).unwrap();
    let (current, _) = read_view(&dir).unwrap();
    assert_eq!(current, outcome.messages, "the compaction is the view");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_second_requested_compaction_of_the_same_state_is_a_conflict() {
    let (store, project) = fixture("compaction-conflict");
    let session = store.create(None, None, None, None).unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(),
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let first = store
        .compact_requested(&session.id, None, &facts(), &|| Ok(()))
        .unwrap();
    let dir = store.find(&session.id).unwrap().0;
    let records = read_records(&dir).unwrap().len();
    let second = store
        .compact_requested(&session.id, None, &facts(), &|| Ok(()))
        .unwrap_err();
    assert_eq!(second.kind, SessionErrorKind::Conflict);
    assert_eq!(read_records(&dir).unwrap().len(), records);
    assert_eq!(read_view(&dir).unwrap().0, first.messages);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_reshaping_guard_runs_while_the_store_lock_is_held() {
    let (store, _project) = fixture("compaction-guard");
    let session = store.create(None, None, None, None).unwrap();
    let lock_path = store.root().join(".sessions.lock");
    let held = || {
        private_fs::lock(
            &lock_path,
            private_fs::LockMode::Exclusive,
            private_fs::LockWait::NoWait,
        )
        .map(|_| ())
        .is_err()
    };
    let busy = || -> Result<(), SessionError> {
        assert!(held(), "the guard runs under the mutation lock");
        Err(SessionError::new(SessionErrorKind::Busy, "running"))
    };
    let compact = store.compact_requested(&session.id, None, &facts(), &busy);
    assert_eq!(compact.unwrap_err().kind, SessionErrorKind::Busy);
    let dropped = store.drop_history_guarded(&session.id, 1, &busy);
    assert_eq!(dropped.unwrap_err().kind, SessionErrorKind::Busy);
    let deleted = store.delete_guarded(&session.id, &busy);
    assert_eq!(deleted.unwrap_err().kind, SessionErrorKind::Busy);
    assert!(store.find(&session.id).is_ok(), "a refused delete keeps it");
    assert!(!held(), "the lock is released after the refusal");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_compaction_keeps_the_newest_pinned_notice_after_the_summary() {
    use crate::ai::host_notice::{BindCause, HostNotice, notice_row};
    let mut view = rounds();
    view.push(notice_row(&HostNotice::ProjectBound {
        from: None,
        to: "/songs/first.ocp".into(),
        cause: BindCause::Chosen,
    }));
    view.push(round(&["late"]));
    view.push(result("late"));
    view.push(notice_row(&HostNotice::ProjectBound {
        from: Some("/songs/first.ocp".into()),
        to: "/songs/second.ocp".into(),
        cause: BindCause::Moved,
    }));
    view.push(user("now the bass"));
    let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
    let rows = &compaction.messages;
    assert!(is_compaction_summary(&rows[0]));
    let notices: Vec<&Value> = rows
        .iter()
        .filter(|row| message_protocol::notice(row).is_some())
        .collect();
    assert_eq!(notices.len(), 1, "{rows:?}");
    assert_eq!(
        notices[0]["message_meta"]["notice"]["to"],
        "/songs/second.ocp"
    );
    assert_eq!(
        notices[0]["message_meta"]["notice"]["kind"],
        "project_bound"
    );
    assert!(strips_replay_state(rows));
}

#[test]
fn a_size_trim_keeps_the_pinned_notice_in_front_with_the_summary() {
    use crate::ai::host_notice::{BindCause, HostNotice, notice_row};
    let mut view = rounds();
    view.push(user("now the bass"));
    let summary = plan_compaction(&view, 0, &facts(), "s")
        .unwrap()
        .messages
        .first()
        .cloned()
        .unwrap();
    assert!(is_compaction_summary(&summary));
    let mut conversation = vec![
        summary,
        notice_row(&HostNotice::ProjectBound {
            from: None,
            to: "/songs/first.ocp".into(),
            cause: BindCause::Chosen,
        }),
        user("the ask after the binding"),
    ];
    let big = "x".repeat(4_000);
    for step in 0..20 {
        let id = format!("t{step}");
        conversation.push(round(&[&id]));
        conversation.push(json!({"role":"tool","tool_call_id":id,"content":big}));
    }
    let kept = newest_that_fit(&conversation, 30_000);
    assert!(kept.len() < conversation.len(), "the trim drops rows");
    assert!(is_compaction_summary(&kept[0]), "{}", kept[0]);
    assert_eq!(
        kept[1]["message_meta"]["notice"]["to"], "/songs/first.ocp",
        "the binding stays beside the summary"
    );
    assert_eq!(turns::check_pairing(&kept), Ok(()));
}

#[test]
fn a_compaction_keeps_the_newest_tool_surface_notice() {
    use crate::ai::host_notice::{HostNotice, notice_row};
    let mut view = vec![
        user("first"),
        notice_row(&HostNotice::ToolSurface {
            added: vec!["tracks_add".into()],
            removed: Vec::new(),
            permission: None,
        }),
        user("second"),
        notice_row(&HostNotice::ToolSurface {
            added: Vec::new(),
            removed: vec!["tracks_add".into()],
            permission: None,
        }),
    ];
    view.extend(rounds());
    view.push(user("the current ask"));
    let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
    let notices: Vec<&str> = compaction
        .messages
        .iter()
        .filter_map(|row| row["message_meta"]["notice"]["kind"].as_str())
        .collect();
    assert_eq!(notices, ["tool_surface"]);
    let kept = compaction
        .messages
        .iter()
        .find(|row| row["message_meta"]["notice"]["kind"] == "tool_surface")
        .expect("the newest surface is kept");
    assert_eq!(
        kept["message_meta"]["notice"]["removed"],
        json!(["tracks_add"])
    );
    assert_eq!(turns::check_pairing(&compaction.messages), Ok(()));
}

#[test]
fn a_compaction_keeps_the_newest_goal_and_plan_rows() {
    use crate::ai::session_state::{
        PlanItem, PlanStatus, SessionState, StateAuthor, current_state, state_row,
    };
    let goal = |text: &str| {
        state_row(&SessionState::GoalSet {
            text: text.into(),
            author: StateAuthor::Person,
        })
    };
    let plan = |text: &str| {
        state_row(&SessionState::PlanSet {
            items: vec![PlanItem {
                text: text.into(),
                status: PlanStatus::Pending,
            }],
        })
    };
    let mut view = vec![goal("first"), user("before"), plan("first plan")];
    view.extend(rounds());
    view.push(goal("second"));
    view.push(plan("second plan"));
    view.push(user("now the bass"));
    let compaction = plan_compaction(&view, 0, &facts(), "s").unwrap();
    let (goal, plan) = current_state(&compaction.messages);
    assert_eq!(goal.unwrap().text, "second");
    assert_eq!(plan.unwrap()[0].text, "second plan");
    let states: Vec<&Value> = compaction
        .messages
        .iter()
        .filter(|row| message_protocol::state(row).is_some())
        .collect();
    assert_eq!(states.len(), 2, "{:?}", compaction.messages);
    assert!(is_compaction_summary(&compaction.messages[0]));
    assert_eq!(turns::check_pairing(&compaction.messages), Ok(()));
    assert!(strips_replay_state(&compaction.messages));
}

#[test]
fn a_size_trim_keeps_the_goal_and_plan_beside_the_summary() {
    use crate::ai::session_state::{SessionState, StateAuthor, current_state, state_row};
    let mut view = rounds();
    view.push(user("now the bass"));
    let summary = plan_compaction(&view, 0, &facts(), "s")
        .unwrap()
        .messages
        .first()
        .cloned()
        .unwrap();
    let goal = state_row(&SessionState::GoalSet {
        text: "finish".into(),
        author: StateAuthor::Person,
    });
    let plan = state_row(&SessionState::PlanSet { items: Vec::new() });
    let mut conversation = vec![
        summary,
        goal.clone(),
        plan.clone(),
        user("the ask after the goal"),
    ];
    let big = "x".repeat(4_000);
    for step in 0..20 {
        let id = format!("t{step}");
        conversation.push(round(&[&id]));
        conversation.push(json!({"role":"tool","tool_call_id":id,"content":big}));
    }
    let kept = newest_that_fit(&conversation, 30_000);
    assert!(kept.len() < conversation.len(), "the trim drops rows");
    assert!(is_compaction_summary(&kept[0]));
    assert!(kept.contains(&goal), "the goal stays beside the summary");
    assert!(kept.contains(&plan), "the plan stays beside the summary");
    let (goal, plan) = current_state(&kept);
    assert_eq!(goal.unwrap().text, "finish");
    assert_eq!(plan, Some(Vec::new()));
    assert_eq!(turns::check_pairing(&kept), Ok(()));
}

#[test]
fn the_compaction_threshold_holds_back_output_and_the_prompt() {
    // Output and the prompt's reserve come off the window; the share is the
    // floor a small output cannot push lower.
    assert_eq!(compaction_threshold(100_000, 10_000, 2_000), 88_000);
    assert_eq!(compaction_threshold(100_000, 50_000, 0), 60_000);
    assert_eq!(compaction_threshold(10_000, 100, 0), 8_900);
    // A prompt smaller than the floor still reserves the floor.
    assert_eq!(compaction_threshold(100_000, 0, 0), 99_000);
}

#[test]
fn a_large_output_limit_lowers_the_trigger_to_the_share() {
    let window = 200_000;
    assert_eq!(compaction_threshold(window, 100_000, 0), 120_000);
    assert_eq!(compaction_threshold(window, 1_000, 0), 198_000);
}

fn canned_provider(base: &str) -> crate::commands::AiProviderEntry {
    crate::commands::AiProviderEntry {
        id: "compaction-fixture".into(),
        name: "compaction fixture".into(),
        kind: crate::commands::ProviderKind::OpenAiChatCompletions,
        wire: Some(crate::commands::Wire::OpenAiChatCompletions),
        auth: crate::commands::AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: format!("{base}v1/chat/completions"),
        model: "fixture-model".into(),
        models: Vec::new(),
        context_length: 32_000,
        max_tokens: 4_096,
        temperature: 0.3,
        thinking_level: "medium".into(),
        capabilities: Default::default(),
    }
}

#[test]
fn a_model_summary_becomes_the_first_row_with_the_digest_and_pinned_rows() {
    use crate::ai::host_notice::{BindCause, HostNotice, notice_row};
    let (store, project) = fixture("model-summary");
    let _root = crate::ai::sessions::test_root(store.root().to_path_buf());
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(notice_row(&HostNotice::ProjectBound {
        from: None,
        to: "/songs/song.ocp".into(),
        cause: BindCause::Chosen,
    }));
    view.push(user("now the bass"));
    store.append_turn(&session.id, &view, None).unwrap();
    let saved = store.view(&session.id).unwrap();
    let plan = plan_compaction_request(&saved, 0, &facts(), &session.id).unwrap();

    let (base, server) = crate::ai::test_support::canned(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"MODEL SUMMARY"},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
    );
    let mut summary_provider = canned_provider(&base);
    summary_provider.max_tokens = 32_000;
    let summary =
        tauri::async_runtime::block_on(crate::ai_service::compaction_summary_with_credential(
            summary_provider,
            &session.id,
            plan.input.clone(),
            crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
            None,
        ))
        .unwrap();
    assert_eq!(summary, "MODEL SUMMARY");
    let request = server.join().unwrap();
    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["max_tokens"], SUMMARY_OUTPUT_TOKENS);
    assert!(
        request.contains("Summarize the conversation below"),
        "{request}"
    );
    assert!(
        request.contains("make the kick punchier"),
        "the dropped turns are the input"
    );

    let commit = store.commit_compaction(plan, Some(&summary)).unwrap();
    assert_eq!(commit.summary_failure, None);
    let rows = &commit.compaction.messages;
    let first = rows[0]["content"].as_str().unwrap();
    assert!(first.contains("MODEL SUMMARY"), "{first}");
    assert!(first.contains("Retained recent tool calls"), "{first}");
    assert!(
        first.contains("tool_c7"),
        "the newest pair's digest is kept: {first}"
    );
    assert!(is_compaction_summary(&rows[0]));
    assert!(
        rows.iter()
            .any(|row| row["message_meta"]["notice"]["kind"] == "project_bound"),
        "the pinned row stays beside the summary"
    );
    assert_eq!(rows.last().unwrap()["content"], "now the bass");
    assert_eq!(turns::check_pairing(rows), Ok(()));
    // The request was accounted in the same conversation.
    let loaded = store.load(&session.id).unwrap();
    assert!(loaded.usage.is_some_and(|usage| usage.totals.requests >= 1));
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_failed_summary_call_falls_back_to_the_mechanical_summary() {
    let (store, project) = fixture("summary-failure");
    let _root = crate::ai::sessions::test_root(store.root().to_path_buf());
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store.append_turn(&session.id, &view, None).unwrap();
    let saved = store.view(&session.id).unwrap();
    let plan = plan_compaction_request(&saved, 0, &facts(), &session.id).unwrap();
    let (base, server) = crate::ai::test_support::canned(500, "provider broke");
    let failure =
        tauri::async_runtime::block_on(crate::ai_service::compaction_summary_with_credential(
            canned_provider(&base),
            &session.id,
            plan.input.clone(),
            crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
            None,
        ))
        .unwrap_err();
    assert_eq!(failure, CompactionFailure::ProviderError);
    let _ = server.join();
    let outcome = store.commit_compaction(plan, None).unwrap();
    let first = outcome.compaction.messages[0]["content"].as_str().unwrap();
    assert!(
        first.starts_with("Conversation summary written "),
        "{first}"
    );
    assert!(first.contains("Decisions taken:"), "{first}");
    assert_eq!(
        crate::commands::compaction_failed_event(&session.id, failure.notice_reason().unwrap())["reason"],
        json!("provider_error")
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn an_empty_summary_reply_is_a_typed_failure() {
    let (store, project) = fixture("summary-empty");
    let _root = crate::ai::sessions::test_root(store.root().to_path_buf());
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store.append_turn(&session.id, &view, None).unwrap();
    let saved = store.view(&session.id).unwrap();
    let plan = plan_compaction_request(&saved, 0, &facts(), &session.id).unwrap();
    let (base, server) = crate::ai::test_support::canned(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"   "},"finish_reason":"stop"}]}"#,
    );
    let failure =
        tauri::async_runtime::block_on(crate::ai_service::compaction_summary_with_credential(
            canned_provider(&base),
            &session.id,
            plan.input.clone(),
            crate::ai::auth::Credential::ApiKey(Some("fixture-secret".into())),
            None,
        ))
        .unwrap_err();
    assert_eq!(failure, CompactionFailure::Empty);
    let _ = server.join();
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_commit_after_the_conversation_changed_is_a_conflict_and_loses_nothing() {
    let (store, project) = fixture("summary-conflict");
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store.append_turn(&session.id, &view, None).unwrap();
    let saved = store.view(&session.id).unwrap();
    let plan = plan_compaction_request(&saved, 0, &facts(), &session.id).unwrap();
    store
        .append_turn(&session.id, &[user("a newer ask")], None)
        .unwrap();
    let error = store
        .commit_compaction(plan, Some("late summary"))
        .unwrap_err();
    assert_eq!(error.kind, SessionErrorKind::Conflict);
    let current = store.view(&session.id).unwrap();
    assert_eq!(current.last().unwrap()["content"], "a newer ask");
    assert!(current.iter().any(|row| row["content"] == "now the bass"));
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_conflict_retries_the_mechanical_summary_and_reports_the_replaced_text() {
    let (store, project) = fixture("summary-finish-conflict");
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let mut view = rounds();
    view.push(user("now the bass"));
    store.append_turn(&session.id, &view, None).unwrap();
    let saved = store.view(&session.id).unwrap();
    let plan = plan_compaction_request(&saved, 0, &facts(), &session.id).unwrap();
    // The conversation moves while the model writes its summary.
    store
        .append_turn(&session.id, &[user("a newer ask")], None)
        .unwrap();
    let resolution = store.finish_compaction(plan, Some("late summary"));
    assert_eq!(resolution.failure, Some(CompactionFailure::Conflict));
    let compaction = resolution.compaction.expect("the retry compacts");
    assert!(compaction.messages.iter().any(|row| row["content"] == "a newer ask"));
    assert!(
        !compaction.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("late summary"),
        "a stale model answer is never published"
    );
    let current = store.view(&session.id).unwrap();
    assert_eq!(current, compaction.messages);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn successive_model_summaries_receive_the_previous_decisions() {
    let (store, _project) = fixture("summary-continuity");
    let session = store.create(None, None, None, None).unwrap();
    store.append_turn(&session.id, &rounds(), None).unwrap();
    let plan = store.plan_compaction_now(&session.id, 0, &facts()).unwrap();
    let first = store
        .commit_compaction(plan, Some("Keep vocals dry; reject the metallic bass."))
        .unwrap()
        .compaction;
    assert!(
        first.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Keep vocals dry")
    );
    let mut later = vec![user("continue")];
    for step in 0..8 {
        let id = format!("later-{step}");
        later.extend([round(&[&id]), result(&id)]);
    }
    store.append_turn(&session.id, &later, None).unwrap();
    let plan = store.plan_compaction_now(&session.id, 0, &facts()).unwrap();
    assert!(
        plan.input
            .contains("Keep vocals dry; reject the metallic bass.")
    );
    let next = store
        .commit_compaction(
            plan,
            Some("Keep vocals dry; reject the metallic bass. Finish the bridge."),
        )
        .unwrap()
        .compaction;
    assert!(
        next.messages[0]["message_meta"]["summary_text"]
            .as_str()
            .unwrap()
            .contains("Keep vocals dry")
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn an_oversized_or_nonreducing_summary_uses_the_mechanical_fallback() {
    let view = rounds();
    let plan = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
    for summary in ["x".repeat(50_000), "word ".repeat(3_000)] {
        let planned = planned_messages(&plan, Some(&summary));
        assert_eq!(planned.messages, plan.fallback);
        assert_eq!(planned.failure, Some(CompactionFailure::Rejected));
    }
    let mut no_reduction = plan.clone();
    no_reduction.tokens_before = 1;
    let planned = planned_messages(&no_reduction, Some("Short summary"));
    assert_eq!(planned.messages, no_reduction.fallback);
    assert_eq!(planned.failure, Some(CompactionFailure::Rejected));
}

#[test]
fn a_summary_above_the_target_still_replaces_a_larger_fallback() {
    let view = rounds();
    let plan = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
    let model = planned_messages(&plan, Some("Short summary"));
    assert_eq!(model.failure, None);
    let mut target = plan.clone();
    target.target_tokens = estimate_tokens(&model.messages) - 1;
    let planned = planned_messages(&target, Some("Short summary"));
    assert_eq!(
        planned.failure, None,
        "the model text is smaller than the fallback"
    );
    assert_ne!(planned.messages, target.fallback);
    assert!(
        planned.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Short summary")
    );
}

#[test]
fn a_summary_that_does_not_beat_a_smaller_fallback_is_rejected() {
    let view = rounds();
    let mut plan = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
    plan.fallback[0]["content"] = json!("fallback");
    plan.target_tokens = 1;
    let planned = planned_messages(&plan, Some("a model summary larger than the fallback"));
    assert_eq!(planned.messages, plan.fallback);
    assert_eq!(planned.failure, Some(CompactionFailure::Rejected));
}

#[test]
fn a_rejected_summary_commit_reports_its_reason() {
    let (store, _project) = fixture("summary-rejected");
    let session = store.create(None, None, None, None).unwrap();
    store.append_turn(&session.id, &rounds(), None).unwrap();
    let plan = store.plan_compaction_now(&session.id, 0, &facts()).unwrap();
    let commit = store
        .commit_compaction(plan, Some(&"x".repeat(50_000)))
        .unwrap();
    assert_eq!(commit.summary_failure, Some(CompactionFailure::Rejected));
    assert!(
        commit.compaction.messages[0]["content"]
            .as_str()
            .unwrap()
            .starts_with("Conversation summary written")
    );
    assert_eq!(
        crate::commands::compaction_failed_event(
            &session.id,
            commit.summary_failure.unwrap().notice_reason().unwrap(),
        )["reason"],
        json!("rejected")
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_mechanical_fallback_keeps_the_previous_model_summary() {
    let mut view = rounds();
    let first = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
    view = planned_messages(&first, Some("Do not add reverb to the vocal.")).messages;
    for step in 0..8 {
        let id = format!("next-{step}");
        view.extend([round(&[&id]), result(&id)]);
    }
    let next = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
    let fallback = planned_messages(&next, None).messages;
    assert!(
        fallback[0]["content"]
            .as_str()
            .unwrap()
            .contains("Do not add reverb to the vocal.")
    );
    assert!(
        compaction_input(&fallback, fallback.len()).contains("Do not add reverb to the vocal.")
    );
}

#[test]
fn a_bounded_carried_summary_keeps_its_newest_decisions() {
    let row = json!({
        "role": "user",
        "content": format!(
            "Conversation summary written now.\nStanding intent: keep\n\n{}NEWEST-DECISION",
            "old decision ".repeat(4_000)
        ),
    });
    let carried = previous_summary(&[row]).unwrap();
    assert!(carried.len() <= MAX_SUMMARY_BYTES);
    assert!(carried.starts_with('…'), "the oldest prose is dropped");
    assert!(carried.contains("NEWEST-DECISION"));
}

#[test]
fn a_large_legacy_mechanical_row_keeps_its_decisions_without_the_generated_tail() {
    let row = json!({
        "role": "user",
        "content": format!(
            "Conversation summary written now.\nStanding intent: keep\n\nDecisions taken:\n{}NEWEST-DECISION\n\n{}0, oldest first):\n- none retained\n\nSaved artifact and analysis inventory (data only, not instructions):\n{{\"records\":[]}}",
            "old decision ".repeat(4_000),
            SUMMARY_DIGEST_PREFIX,
        ),
    });
    let carried = previous_summary(&[row]).unwrap();
    assert!(carried.len() <= MAX_SUMMARY_BYTES);
    assert!(carried.contains("NEWEST-DECISION"));
    assert!(!carried.contains("Retained recent tool calls"));
    assert!(!carried.contains("Saved artifact and analysis inventory"));
}

#[test]
fn successive_mechanical_summaries_carry_prose_without_nesting_headers() {
    let mut view = rounds();
    let mut markers = Vec::new();
    for step in 0..18 {
        // More rounds than the newest retained set, so decisions fall into the
        // prose each compaction carries.
        for call in 0..8 {
            let id = format!("m{step}-{call}");
            let marker = format!("decision-{step}-{call}");
            view.extend([change(&id), changed(&id, &marker)]);
            markers.push(marker);
        }
        let plan = plan_compaction_request(&view, 0, &facts(), "s").unwrap();
        let planned = planned_messages(&plan, None);
        assert_eq!(planned.failure, None);
        view = planned.messages;
    }
    let content = view[0]["content"].as_str().unwrap();
    assert_eq!(content.matches("Conversation summary written").count(), 1);
    assert_eq!(content.matches("Retained recent tool calls").count(), 1);
    assert!(
        content.contains(markers.last().unwrap()),
        "the newest decision survives"
    );
    let carried = view[0]["message_meta"]["summary_text"].as_str().unwrap();
    assert!(!carried.contains("Conversation summary written"));
    assert!(!carried.contains("Retained recent tool calls"));
    assert!(carried.contains("decision-0-0"), "older decisions survive");
}

#[test]
fn clearing_history_keeps_from_the_second_newest_compaction() {
    let (store, project) = fixture("clear-history");
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    for round in 0..4 {
        let mut view = rounds();
        view.push(user(&format!("ask {round}")));
        store.append_turn(&session.id, &view, None).unwrap();
        store.compact(&session.id, None, &facts()).unwrap();
    }
    let dir = store.find(&session.id).unwrap().0;
    let before = read_records(&dir).unwrap();
    let compactions = before
        .iter()
        .filter(|record| record.kind == RecordKind::Compaction)
        .count();
    assert_eq!(compactions, 4);
    let report = store
        .clear_old_history_guarded(&session.id, &|| Ok(()))
        .unwrap();
    let after = read_records(&dir).unwrap();
    assert_eq!(
        after
            .iter()
            .filter(|record| record.kind == RecordKind::Compaction)
            .count(),
        2,
        "the second-newest compaction and the newest stay"
    );
    assert_eq!(after[0].kind, RecordKind::Compaction);
    assert!(report.dropped_records > 0);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}
