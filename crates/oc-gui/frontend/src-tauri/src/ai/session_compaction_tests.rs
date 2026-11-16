//! Compaction and context bounding: what a summary keeps of the newest turn,
//! how the store refuses or reports, and how a size trim keeps the summary.

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
        assert!(!should_compact(&view, 0, window), "{window}");
    }
}

#[test]
fn attachments_count_toward_the_compaction_threshold() {
    let mut view = vec![user(&"words ".repeat(400)), reply("ok")];
    let text = estimate_tokens(&view);
    let window = text * 2;
    assert!(!should_compact(&view, 0, window));
    view[0]["media"] = json!([image(), image()]);
    assert!(media_tokens(&view) >= 2 * IMAGE_TOKENS);
    assert!(should_compact(&view, 0, window));
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
        private_fs::lock(&lock_path, false, true)
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
