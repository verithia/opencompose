//! Project binding: the first turn, a refusal on another open file, explicit
//! rebinding, and the typed notice the model reads after a rebind or a move.

use super::*;
use crate::ai::host_notice::{BindCause, HostNotice};
use crate::edit_history::{Author, EditCause, HistoryMark, OutsideEntry};
use oc_dsl::change::{Change, ChangeSet};

fn user(text: &str) -> Value {
    json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
}

fn context(facts: &ProjectFacts) -> PrepareContext<'_> {
    PrepareContext {
        snapshot: None,
        overhead: 0,
        context_length: 0,
        facts,
        provider: "fixture-provider",
        model: "fixture-model",
        outside: None,
        surface: None,
    }
}

fn surface(names: &[&str], permission: SessionPermission) -> ToolSurface {
    ToolSurface {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        permission,
    }
}

fn tool_surface_notices(messages: &[Value]) -> Vec<HostNotice> {
    messages
        .iter()
        .filter_map(message_protocol::notice)
        .filter(|notice| notice.kind() == "tool_surface")
        .collect()
}

fn facts(project: &Path) -> ProjectFacts {
    ProjectFacts {
        path: Some(project.display().to_string()),
        dsl_digest: dsl_digest("@project\n"),
        track_count: 0,
    }
}

fn canonical(path: &Path) -> String {
    canonical_path(path).to_string_lossy().to_string()
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                files.push((path.display().to_string(), bytes));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn an_unbound_session_binds_silently_at_its_first_turn() {
    let (store, project) = fixture("bind-silent");
    let place = project.parent().unwrap().to_path_buf();
    let session = store
        .create(Some(&place), None, Some("unbound"), None)
        .unwrap();
    assert_eq!(session.project_path, None);
    let project_facts = facts(&project);
    let prepared = store
        .prepare(
            &session.id,
            &[user("first ask")],
            Some(&project),
            context(&project_facts),
        )
        .unwrap();
    assert_eq!(
        prepared.meta.project_path.as_deref(),
        Some(canonical(&project).as_str())
    );
    assert_eq!(prepared.messages.len(), 1);
    assert_eq!(prepared.messages[0]["content"], "first ask");
    assert!(
        message_protocol::notice(&prepared.messages[0]).is_none(),
        "the first binding tells the model nothing"
    );
    let (_, saved) = store.find(&session.id).unwrap();
    assert_eq!(
        saved.project_path.as_deref(),
        Some(canonical(&project).as_str())
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_turn_on_another_open_file_is_refused_before_any_write() {
    let (store, first) = fixture("bind-refused");
    let second = first.parent().unwrap().join("second.ocp");
    std::fs::write(&second, "@project\n").unwrap();
    let place = first.parent().unwrap().to_path_buf();
    let session = store
        .create(Some(&place), Some(&first), Some("bound"), None)
        .unwrap();
    let before_tree = tree(store.root());
    let before_meta = store.find(&session.id).unwrap().1;
    let before_view = store.view(&session.id).unwrap();
    let project_facts = facts(&second);
    let refused = store.prepare_admitted(
        &session.id,
        &[user("wrong file")],
        Some(&second),
        context(&project_facts),
        None,
        |_| Ok::<(), String>(()),
    );
    let mismatch = match refused {
        Err(PrepareFailure::Binding(mismatch)) => mismatch,
        other => panic!("expected a binding refusal, got {other:?}"),
    };
    assert_eq!(mismatch.bound, canonical(&first));
    assert_eq!(mismatch.open, canonical(&second));
    assert_eq!(store.find(&session.id).unwrap().1, before_meta);
    assert_eq!(store.view(&session.id).unwrap(), before_view);
    assert_eq!(tree(store.root()), before_tree, "nothing was written");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn bind_then_a_turn_writes_one_notice_before_the_message_and_clears_pending() {
    let (store, first) = fixture("bind-notice");
    let second = first.parent().unwrap().join("second.ocp");
    std::fs::write(&second, "@project\n").unwrap();
    let place = first.parent().unwrap().to_path_buf();
    let session = store
        .create(Some(&place), Some(&first), Some("bound"), None)
        .unwrap();
    let bound = store
        .bind(&session.id, Some(&second), BindCause::Chosen)
        .unwrap();
    assert_eq!(
        bound.project_path.as_deref(),
        Some(canonical(&second).as_str())
    );
    let pending = bound.pending_binding.expect("a pending binding");
    assert_eq!(pending.from.as_deref(), Some(canonical(&first).as_str()));
    assert_eq!(pending.cause, BindCause::Chosen);
    let project_facts = facts(&second);
    let prepared = store
        .prepare(
            &session.id,
            &[user("keep going")],
            Some(&second),
            context(&project_facts),
        )
        .unwrap();
    assert_eq!(prepared.messages.len(), 2, "{:?}", prepared.messages);
    let notice = message_protocol::notice(&prepared.messages[0]).expect("a typed notice");
    match notice {
        HostNotice::ProjectBound { from, to, cause } => {
            assert_eq!(from.as_deref(), Some(canonical(&first).as_str()));
            assert_eq!(to, canonical(&second));
            assert_eq!(cause, BindCause::Chosen);
        }
        _ => panic!("expected a binding notice"),
    }
    assert_eq!(prepared.messages[1]["content"], "keep going");
    let text = prepared.messages[0]["content"].as_str().unwrap();
    assert!(text.contains(&canonical(&first)));
    assert!(text.contains(&canonical(&second)));
    let (_, saved) = store.find(&session.id).unwrap();
    assert_eq!(saved.pending_binding, None);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_second_rebind_before_a_turn_keeps_the_first_from_path() {
    let (store, first) = fixture("bind-twice");
    let second = first.parent().unwrap().join("second.ocp");
    let third = first.parent().unwrap().join("third.ocp");
    for path in [&second, &third] {
        std::fs::write(path, "@project\n").unwrap();
    }
    let place = first.parent().unwrap().to_path_buf();
    let session = store
        .create(Some(&place), Some(&first), Some("bound"), None)
        .unwrap();
    store
        .bind(&session.id, Some(&second), BindCause::Chosen)
        .unwrap();
    let rebound = store
        .bind(&session.id, Some(&third), BindCause::Chosen)
        .unwrap();
    assert_eq!(
        rebound
            .pending_binding
            .as_ref()
            .and_then(|pending| pending.from.as_deref()),
        Some(canonical(&first).as_str())
    );
    let project_facts = facts(&third);
    let prepared = store
        .prepare(
            &session.id,
            &[user("keep going")],
            Some(&third),
            context(&project_facts),
        )
        .unwrap();
    let notice = message_protocol::notice(&prepared.messages[0]).expect("a typed notice");
    match notice {
        HostNotice::ProjectBound { from, to, .. } => {
            assert_eq!(from.as_deref(), Some(canonical(&first).as_str()));
            assert_eq!(to, canonical(&third));
        }
        _ => panic!("expected a binding notice"),
    }
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn relocating_a_project_sets_a_moved_binding_the_next_turn_reports() {
    let (store, first) = fixture("bind-moved");
    let moved = first.parent().unwrap().join("moved").join("song.ocp");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::write(&moved, "@project\n").unwrap();
    let place = first.parent().unwrap().to_path_buf();
    let session = store
        .create(Some(&place), Some(&first), Some("bound"), None)
        .unwrap();
    assert_eq!(store.relocate_project(&first, &moved).unwrap(), 1);
    let (_, relocated) = store.find(&session.id).unwrap();
    assert_eq!(
        relocated.project_path.as_deref(),
        Some(canonical(&moved).as_str())
    );
    let pending = relocated.pending_binding.expect("a pending moved binding");
    assert_eq!(pending.from.as_deref(), Some(canonical(&first).as_str()));
    assert_eq!(pending.cause, BindCause::Moved);
    let project_facts = facts(&moved);
    let prepared = store
        .prepare(
            &session.id,
            &[user("after the move")],
            Some(&moved),
            context(&project_facts),
        )
        .unwrap();
    let notice = message_protocol::notice(&prepared.messages[0]).expect("a typed notice");
    match notice {
        HostNotice::ProjectBound { from, to, cause } => {
            assert_eq!(from.as_deref(), Some(canonical(&first).as_str()));
            assert_eq!(to, canonical(&moved));
            assert_eq!(cause, BindCause::Moved);
        }
        _ => panic!("expected a moved binding notice"),
    }
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn binding_to_the_project_it_already_has_changes_nothing() {
    let (store, first) = fixture("bind-noop");
    let session = store
        .create(
            Some(first.parent().unwrap()),
            Some(&first),
            Some("bound"),
            None,
        )
        .unwrap();
    let bound = store
        .bind(&session.id, Some(&first), BindCause::Chosen)
        .unwrap();
    assert_eq!(bound.pending_binding, None);
    assert_eq!(bound.updated, session.updated);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

fn outside_notice() -> HostNotice {
    HostNotice::OutsideChanges {
        entries: vec![OutsideEntry {
            author: Author::Person,
            cause: EditCause::Edit,
            changes: ChangeSet {
                changes: vec![Change::Tempo {
                    from: 120.0,
                    to: 132.0,
                }],
                more: 0,
            },
        }],
        more: 0,
        undone: 0,
        rewritten: false,
    }
}

#[test]
fn an_outside_changes_notice_is_saved_before_the_turn_message() {
    let (store, project) = fixture("outside-notice");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("outside"),
            None,
        )
        .unwrap();
    let project_facts = facts(&project);
    let mut context = context(&project_facts);
    context.outside = Some(outside_notice());
    let prepared = store
        .prepare(&session.id, &[user("keep going")], Some(&project), context)
        .unwrap();
    assert_eq!(prepared.messages.len(), 2, "{:?}", prepared.messages);
    assert_eq!(prepared.messages[1]["content"], "keep going");
    let notice = message_protocol::notice(&prepared.messages[0]).expect("a typed notice");
    let HostNotice::OutsideChanges {
        entries,
        more,
        undone,
        rewritten,
    } = notice.clone()
    else {
        panic!("expected outside changes, got {notice:?}");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].author, Author::Person);
    assert_eq!(entries[0].cause, EditCause::Edit);
    assert_eq!(more, 0);
    assert_eq!(undone, 0);
    assert!(!rewritten);
    assert_eq!(prepared.messages[0]["content"], notice.model_text());
    assert_eq!(
        prepared
            .messages
            .iter()
            .filter(|message| message_protocol::notice(message)
                .is_some_and(|notice| notice.kind() == "outside_changes"))
            .count(),
        1
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_refused_admission_writes_no_outside_changes_notice() {
    let (store, project) = fixture("outside-refused");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("outside"),
            None,
        )
        .unwrap();
    let before_tree = tree(store.root());
    let before_view = store.view(&session.id).unwrap();
    let project_facts = facts(&project);
    let mut context = context(&project_facts);
    context.outside = Some(outside_notice());
    let refused = store.prepare_admitted(
        &session.id,
        &[user("too large")],
        Some(&project),
        context,
        None,
        |_| Err("too large".to_owned()),
    );
    assert!(matches!(refused, Err(PrepareFailure::Admitted(_))));
    assert_eq!(store.view(&session.id).unwrap(), before_view);
    assert_eq!(tree(store.root()), before_tree, "nothing was written");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_history_mark_is_recorded_for_the_bound_project_and_cleared_on_rebind() {
    let (store, first) = fixture("history-mark");
    let second = first.parent().unwrap().join("second.ocp");
    std::fs::write(&second, "@project\n").unwrap();
    let session = store
        .create(
            Some(first.parent().unwrap()),
            Some(&first),
            Some("marked"),
            None,
        )
        .unwrap();
    let mark = HistoryMark {
        at: Some(7),
        max_id: 9,
    };
    store
        .record_history_mark(&session.id, &first, mark)
        .unwrap();
    let saved = store.find(&session.id).unwrap().1;
    assert_eq!(
        saved.history_mark,
        Some(SavedHistoryMark {
            project: canonical(&first),
            at: Some(7),
            max_id: 9,
        })
    );
    // A turn that ended on another open file does not move the mark.
    store
        .record_history_mark(
            &session.id,
            &second,
            HistoryMark {
                at: None,
                max_id: 1,
            },
        )
        .unwrap();
    assert_eq!(
        store.find(&session.id).unwrap().1.history_mark,
        saved.history_mark
    );

    // A rebind tells the model about the new file and drops the old cursor.
    store
        .bind(&session.id, Some(&second), BindCause::Chosen)
        .unwrap();
    assert_eq!(store.find(&session.id).unwrap().1.history_mark, None);

    // A move is the same boundary.
    let moved = second.parent().unwrap().join("moved").join("song.ocp");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::write(&moved, "@project\n").unwrap();
    store
        .record_history_mark(&session.id, &second, mark)
        .unwrap();
    assert_eq!(store.relocate_project(&second, &moved).unwrap(), 1);
    assert_eq!(store.find(&session.id).unwrap().1.history_mark, None);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_first_turn_records_the_tool_surface_without_a_notice() {
    let (store, project) = fixture("surface-first");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("surface"),
            None,
        )
        .unwrap();
    let project_facts = facts(&project);
    let mut context = context(&project_facts);
    context.surface = Some(surface(
        &["tracks_add", "tracks_list"],
        SessionPermission::Write,
    ));
    let prepared = store
        .prepare(&session.id, &[user("first")], Some(&project), context)
        .unwrap();
    assert!(tool_surface_notices(&prepared.messages).is_empty());
    assert_eq!(
        store.find(&session.id).unwrap().1.tool_surface,
        Some(surface(
            &["tracks_add", "tracks_list"],
            SessionPermission::Write
        ))
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_changed_surface_writes_one_notice_and_updates_the_meta() {
    let (store, project) = fixture("surface-changed");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("surface"),
            None,
        )
        .unwrap();
    let project_facts = facts(&project);
    let mut first = context(&project_facts);
    first.surface = Some(surface(
        &["tracks_add", "tracks_list"],
        SessionPermission::Write,
    ));
    store
        .prepare(&session.id, &[user("first")], Some(&project), first)
        .unwrap();
    let mut second = context(&project_facts);
    second.surface = Some(surface(
        &["tracks_list", "tracks_remove"],
        SessionPermission::Write,
    ));
    let prepared = store
        .prepare(&session.id, &[user("second")], Some(&project), second)
        .unwrap();
    let notices = tool_surface_notices(&prepared.messages);
    assert_eq!(notices.len(), 1);
    let HostNotice::ToolSurface {
        added,
        removed,
        permission,
    } = notices[0].clone()
    else {
        panic!("expected a tool surface notice");
    };
    assert_eq!(added, ["tracks_remove"]);
    assert_eq!(removed, ["tracks_add"]);
    assert_eq!(permission, None);
    assert_eq!(
        store.find(&session.id).unwrap().1.tool_surface,
        Some(surface(
            &["tracks_list", "tracks_remove"],
            SessionPermission::Write
        ))
    );

    let mut third = context(&project_facts);
    third.surface = Some(surface(
        &["tracks_list", "tracks_remove"],
        SessionPermission::Write,
    ));
    let prepared = store
        .prepare(&session.id, &[user("third")], Some(&project), third)
        .unwrap();
    assert_eq!(
        tool_surface_notices(&prepared.messages).len(),
        1,
        "an unchanged surface adds no notice"
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_permission_change_alone_writes_only_the_permission_sentence() {
    let (store, project) = fixture("surface-permission");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("surface"),
            None,
        )
        .unwrap();
    let project_facts = facts(&project);
    let mut first = context(&project_facts);
    first.surface = Some(surface(&["tracks_list"], SessionPermission::Write));
    store
        .prepare(&session.id, &[user("first")], Some(&project), first)
        .unwrap();
    let mut second = context(&project_facts);
    second.surface = Some(surface(&["tracks_list"], SessionPermission::Autoreview));
    let prepared = store
        .prepare(&session.id, &[user("second")], Some(&project), second)
        .unwrap();
    let notices = tool_surface_notices(&prepared.messages);
    assert_eq!(notices.len(), 1);
    let HostNotice::ToolSurface {
        added,
        removed,
        permission,
    } = notices[0].clone()
    else {
        panic!("expected a tool surface notice");
    };
    assert!(added.is_empty());
    assert!(removed.is_empty());
    assert_eq!(
        permission,
        Some(crate::ai::host_notice::PermissionChange {
            from: SessionPermission::Write,
            to: SessionPermission::Autoreview,
        })
    );
    let row = prepared
        .messages
        .iter()
        .find(|message| message["message_meta"]["notice"]["kind"] == "tool_surface")
        .expect("the permission change stands in its own notice row");
    assert_eq!(
        row["content"],
        "Permission mode changed from write to autoreview."
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn notices_stand_in_order_before_the_turn_message() {
    let (store, first) = fixture("surface-order");
    let second = first.parent().unwrap().join("second.ocp");
    std::fs::write(&second, "@project\n").unwrap();
    let session = store
        .create(
            Some(first.parent().unwrap()),
            Some(&first),
            Some("surface"),
            None,
        )
        .unwrap();
    let first_facts = facts(&first);
    let mut first_context = context(&first_facts);
    first_context.surface = Some(surface(&["tracks_add"], SessionPermission::Write));
    store
        .prepare(&session.id, &[user("first")], Some(&first), first_context)
        .unwrap();
    store
        .bind(&session.id, Some(&second), BindCause::Chosen)
        .unwrap();
    let second_facts = facts(&second);
    let mut context = context(&second_facts);
    context.outside = Some(outside_notice());
    context.surface = Some(surface(&["tracks_remove"], SessionPermission::Write));
    let prepared = store
        .prepare(&session.id, &[user("second")], Some(&second), context)
        .unwrap();
    let kinds: Vec<&str> = prepared
        .messages
        .iter()
        .filter_map(message_protocol::notice)
        .map(|notice| notice.kind())
        .collect();
    assert_eq!(kinds, ["project_bound", "outside_changes", "tool_surface"]);
    assert_eq!(prepared.messages.last().unwrap()["content"], "second");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_refused_admission_saves_neither_the_notice_nor_the_new_surface() {
    let (store, project) = fixture("surface-refused");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("surface"),
            None,
        )
        .unwrap();
    let project_facts = facts(&project);
    let mut first = context(&project_facts);
    first.surface = Some(surface(&["tracks_add"], SessionPermission::Write));
    store
        .prepare(&session.id, &[user("first")], Some(&project), first)
        .unwrap();
    let before_meta = store.find(&session.id).unwrap().1;
    let before_view = store.view(&session.id).unwrap();
    let before_tree = tree(store.root());
    let mut second = context(&project_facts);
    second.surface = Some(surface(&["tracks_remove"], SessionPermission::Autoreview));
    let refused = store.prepare_admitted(
        &session.id,
        &[user("refused")],
        Some(&project),
        second,
        None,
        |_| Err("too large".to_owned()),
    );
    assert!(matches!(refused, Err(PrepareFailure::Admitted(_))));
    assert_eq!(store.find(&session.id).unwrap().1, before_meta);
    assert_eq!(store.view(&session.id).unwrap(), before_view);
    assert_eq!(tree(store.root()), before_tree, "nothing was written");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn an_oversized_saved_tool_surface_is_dropped_on_load() {
    let (store, project) = fixture("surface-invalid");
    let session = store
        .create(
            Some(project.parent().unwrap()),
            Some(&project),
            Some("surface"),
            None,
        )
        .unwrap();
    let (dir, meta) = store.find(&session.id).unwrap();
    let mut saved = serde_json::to_value(meta).unwrap();
    saved["tool_surface"] = serde_json::json!({
        "names": (0..crate::ai::host_notice::MAX_TOOL_SURFACE_NAMES + 1)
            .map(|index| format!("tool_{index}"))
            .collect::<Vec<_>>(),
        "permission": "write",
    });
    std::fs::write(dir.join(META_FILE), serde_json::to_vec(&saved).unwrap()).unwrap();
    let (_, loaded) = store.find(&session.id).unwrap();
    assert_eq!(loaded.tool_surface, None);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}
