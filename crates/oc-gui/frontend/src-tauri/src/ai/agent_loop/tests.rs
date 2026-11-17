//! The loop against a scripted host: a provider that replies from a list, a
//! project that counts edits, and a real session store on disk.

use super::*;
use crate::ai::sessions::Store;
use crate::ai::sessions::turns::check_pairing;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Default)]
struct World {
    replies: VecDeque<Result<Value, AiError>>,
    sent: Vec<Vec<Value>>,
    calls: Vec<String>,
    dsl: String,
    revision: u64,
    batch: Option<String>,
    committed: usize,
    events: Vec<TurnEvent>,
    decisions: VecDeque<Option<Decision>>,
    asked: Vec<ApprovalRequest>,
    user_replies: VecDeque<Option<Reply>>,
    user_questions: Vec<UserQuestion>,
    /// Cancel when this tool is dispatched, after it ran.
    cancel_on: Option<String>,
    /// Fail the store write after this many records.
    record_budget: Option<usize>,
    invalid: bool,
    /// Documents from this revision on do not validate.
    invalid_from: Option<u64>,
    /// Mutations succeed without changing the document.
    noop: bool,
    /// The call whose result carries typed edit facts.
    typed_changes_on: Option<String>,
    /// The model writes its calls in text.
    text_protocol: bool,
    undos: usize,
    steer_on_tool: Option<String>,
    /// Enqueue a steer during this send (1 is the first).
    steer_on_send: Option<usize>,
    /// Make the batch close fail at this step.
    commit_failure: Option<CommitFailure>,
    /// The facts the open batch would describe at close.
    turn_changes: Option<oc_dsl::change::ChangeSet>,
    /// Someone else edits the project while this tool runs.
    edit_elsewhere_on: Option<String>,
    /// The model's input window in tokens; 0 when unknown.
    context_length: u64,
    hold_compaction: bool,
    /// Why a modifying call cannot run on the open project.
    binding_refusal: Option<String>,
}

struct Fake {
    world: Mutex<World>,
    store: Store,
    session: String,
    cancel: Cancel,
    steering: steering::Steering,
    compaction_started: tokio::sync::Notify,
    _root: tempfile::TempDir,
}

impl Fake {
    /// The fake validator: every document is invalid with `invalid`, and
    /// documents from revision `invalid_from` on are too.
    fn errors_of(&self, dsl: &str) -> Vec<String> {
        let world = self.world();
        let revision = dsl
            .strip_prefix("@project r")
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        if world.invalid || world.invalid_from.is_some_and(|from| revision >= from) {
            vec!["bad".into()]
        } else {
            Vec::new()
        }
    }

    fn new(replies: Vec<Value>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let session = store.create(None, None, Some("Fixture"), None).unwrap().id;
        Self {
            world: Mutex::new(World {
                replies: replies.into_iter().map(Ok).collect(),
                dsl: "@project".into(),
                text_protocol: true,
                ..World::default()
            }),
            store,
            session,
            cancel: Cancel::default(),
            steering: steering::Steering::default(),
            compaction_started: tokio::sync::Notify::new(),
            _root: root,
        }
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    fn spec(&self, permission: SessionPermission) -> TurnSpec {
        let user =
            json!({"role":"user","content":"make it","message_meta":{"version":1,"kind":"user"}});
        self.store
            .append_turn(&self.session, std::slice::from_ref(&user), None)
            .unwrap();
        let (text_protocol, context_length) = {
            let world = self.world();
            (world.text_protocol, world.context_length)
        };
        TurnSpec {
            wire: vec![json!({"role":"system","content":"sys"}), user],
            permission,
            label: "AI edit".into(),
            audio: None,
            scope: self.session.clone(),
            toolsets: None,
            text_protocol,
            context_length,
            max_output: 0,
        }
    }

    async fn run(&self, permission: SessionPermission) -> TurnOutcome {
        let spec = self.spec(permission);
        run_turn(self, spec, self.cancel.clone()).await
    }

    fn saved(&self) -> Vec<Value> {
        self.store.view(&self.session).unwrap()
    }

    fn results(&self) -> Vec<(String, Value)> {
        self.saved()
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| {
                (
                    message["tool_call_id"].as_str().unwrap().to_owned(),
                    serde_json::from_str(message["content"].as_str().unwrap())
                        .unwrap_or(Value::Null),
                )
            })
            .collect()
    }
}

impl Host for Fake {
    fn steering(&self) -> Option<&steering::Steering> {
        Some(&self.steering)
    }
    async fn begin_batch(&self, label: &str) -> Result<(), String> {
        if let Some(refusal) = self.world().binding_refusal.clone() {
            return Err(refusal);
        }
        self.call(
            "transaction_begin",
            &json!({ "label": label }),
            &Cancel::default(),
        )
        .await
        .map(|_| ())
        .map_err(|refusal| refusal.to_string())
    }

    async fn batch_changes(&self) -> Option<oc_dsl::change::ChangeSet> {
        self.world().turn_changes.clone()
    }

    async fn commit_batch(&self) -> Result<(), CommitError> {
        let failure = self.world().commit_failure;
        if let Some(failure) = failure {
            // A save failure is a commit that landed and a file that did not.
            if failure == CommitFailure::Save {
                self.world().committed += 1;
            }
            return Err(CommitError {
                failure,
                message: "the disk is full".into(),
            });
        }
        self.call("transaction_commit", &json!({}), &Cancel::default())
            .await
            .map(|_| ())
            .map_err(|refusal| CommitError::commit(refusal.to_string()))
    }

    async fn send(&self, wire: &[Value], _round: usize) -> Result<Value, AiError> {
        let mut world = self.world();
        world.sent.push(wire.to_vec());
        if world.steer_on_send == Some(world.sent.len()) {
            world.steer_on_send = None;
            // The fake reads no receipt; the loop's delivery is checked on the wire.
            drop(self.steering.enqueue(json!({"role":"user", "content":"make the drop longer", "message_meta":message_meta(MessageKind::User)})).unwrap());
        }
        world
            .replies
            .pop_front()
            .unwrap_or_else(|| Ok(json!({"role":"assistant","content":"done"})))
    }

    async fn call(&self, tool: &str, args: &Value, _cancel: &Cancel) -> Result<Value, Value> {
        let mut world = self.world();
        world.calls.push(tool.to_owned());
        if world.steer_on_tool.as_deref() == Some(tool) {
            world.steer_on_tool = None;
            // The fake reads no receipt; the loop's delivery is checked on the wire.
            drop(self.steering.enqueue(json!({"role":"user", "content":"soften the highs", "message_meta":message_meta(MessageKind::User)})).unwrap());
        }
        if world.cancel_on.as_deref() == Some(tool) {
            self.cancel.cancel();
        }
        if world.edit_elsewhere_on.as_deref() == Some(tool) {
            world.edit_elsewhere_on = None;
            world.revision += 1;
            world.dsl = format!("@project r{}", world.revision);
        }
        let result = match tool {
            "transaction_begin" => {
                world.batch = Some(args["label"].as_str().unwrap_or_default().to_owned());
                Ok(json!({"active": true}))
            }
            "transaction_commit" => {
                world
                    .batch
                    .take()
                    .ok_or_else(|| json!({"error":"no batch"}))?;
                world.committed += 1;
                Ok(json!({"committed": true}))
            }
            "transaction_rollback" => {
                world.batch = None;
                Ok(json!({"rolled_back": true}))
            }
            "tracks_add" | "project_set_tempo" | "tracks_remove" => {
                if world.batch.is_none() {
                    return Err(json!({"error":"outside the turn's batch"}));
                }
                if world.noop {
                    return Ok(json!({"ok": true}));
                }
                world.revision += 1;
                world.dsl = format!("@project r{}", world.revision);
                Ok(
                    json!({"revision": world.revision, "dsl_source": world.dsl, "project": {"tracks": []}}),
                )
            }
            "big_read" => Ok(json!({"rows": "x".repeat(20_000)})),
            "refused" => Err(json!({"error": "refused: nope"})),
            _ => Ok(json!({"ok": true})),
        };
        let mut result = result?;
        if world.typed_changes_on.as_deref() == Some(tool) {
            result["changes"] = json!({
                "changes": [{"kind": "tempo", "from": 120.0, "to": 132.0}],
                "more": 0,
            });
        }
        Ok(result)
    }

    async fn ask(&self, request: ApprovalRequest) -> Option<Decision> {
        let mut world = self.world();
        world.asked.push(request);
        world.decisions.pop_front().flatten()
    }

    async fn ask_user(&self, request: UserQuestion) -> Option<Reply> {
        let mut world = self.world();
        world.user_questions.push(request);
        world.user_replies.pop_front().flatten()
    }

    fn allowed(&self) -> Vec<String> {
        Vec::new()
    }

    async fn compact(&self, cancel: &Cancel) -> Result<Vec<Value>, CompactionFailure> {
        let hold = self.world().hold_compaction;
        if hold {
            self.compaction_started.notify_one();
            cancel.cancelled().await;
            return Err(CompactionFailure::Cancelled);
        }
        let dsl = self.world().dsl.clone();
        let facts = crate::ai::sessions::ProjectFacts {
            path: None,
            dsl_digest: crate::ai::sessions::dsl_digest(&dsl),
            track_count: 1,
        };
        self.store
            .compact(&self.session, None, &facts)
            .map(|compaction| compaction.messages)
            .map_err(|_| CompactionFailure::Conflict)
    }

    async fn record(&self, messages: &[Value]) -> Result<Vec<Value>, String> {
        {
            let mut world = self.world();
            if let Some(budget) = world.record_budget.as_mut() {
                if *budget == 0 {
                    return Err("disk full".into());
                }
                *budget -= 1;
            }
        }
        self.store.append_turn(&self.session, messages, None)
    }

    async fn stored_result(&self, call: &str) -> Option<String> {
        self.store.tool_result(&self.session, call).ok().flatten()
    }

    async fn started(&self, call_id: &str) -> Result<(), String> {
        self.store.mark_started(&self.session, call_id)
    }

    async fn close_round(&self) -> Result<Vec<crate::ai::sessions::turns::Closed>, String> {
        self.store.close_round_typed(&self.session)
    }

    fn emit(&self, event: TurnEvent) {
        self.world().events.push(event);
    }

    async fn refresh(&self) -> Document {
        let world = self.world();
        Document {
            dsl: world.dsl.clone(),
            revision: world.revision,
            track_count: 1,
        }
    }

    async fn validate_edit(&self, _path: &str, dsl: &str) -> Result<(), Vec<String>> {
        let errors = self.errors_of(dsl);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    async fn apply_edit(&self, _path: &str, dsl: &str, expected: &str) -> Result<Edited, String> {
        let mut world = self.world();
        if world.dsl != expected {
            return Err("changed".into());
        }
        if world.batch.is_none() {
            return Err("outside the turn's batch".into());
        }
        world.revision += 1;
        world.dsl = dsl.to_owned();
        // A fixture document that parses is described like the app's.
        let changes = match (oc_dsl::parse(expected), oc_dsl::parse(dsl)) {
            (Ok(before), Ok(after)) => crate::agent_api::describe_edit(&before, &after),
            _ => None,
        };
        Ok(Edited {
            document: Document {
                dsl: world.dsl.clone(),
                revision: world.revision,
                track_count: 1,
            },
            changes,
        })
    }

    async fn subtree(&self, _path: &str) -> String {
        "old".into()
    }

    async fn introduced_errors(&self, before: &str, after: &str) -> Result<Vec<String>, String> {
        let errors = |dsl: &str| self.errors_of(dsl);
        let existing = errors(before);
        Ok(errors(after)
            .into_iter()
            .filter(|error| !existing.contains(error))
            .collect())
    }

    async fn undo(&self, written: u64) -> Result<(), String> {
        let mut world = self.world();
        if world.revision != written {
            return Err("moved".into());
        }
        world.undos += 1;
        Ok(())
    }
}

fn round(calls: &[(&str, &str)]) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": calls.iter().map(|(id, name)| json!({
            "id": id, "type": "function", "function": {"name": name, "arguments": "{}"}
        })).collect::<Vec<_>>(),
    })
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn an_answered_turn_saves_every_round_and_commits_once() {
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add"), ("b", "project_get")]),
        round(&[("c", "project_set_tempo")]),
        json!({"role":"assistant","content":"All set."}),
    ]);
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert_eq!((outcome.text.as_str(), outcome.failed), ("All set.", false));
    let world = fake.world();
    assert_eq!(
        world
            .calls
            .iter()
            .filter(|c| *c == "transaction_begin")
            .count(),
        1
    );
    assert_eq!(world.committed, 1);
    assert!(world.batch.is_none());
    let saved = fake.saved();
    assert_eq!(check_pairing(&saved), Ok(()));
    assert_eq!(saved.last().unwrap()["content"], "All set.");
    // The second request carried the first round's results exactly as saved.
    let second = &world.sent[1];
    assert_eq!(&second[1..], &saved[..second.len() - 1]);
}

#[test]
fn a_file_switch_mid_turn_refuses_every_mutation_before_a_batch_opens() {
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add")]),
        json!({"role":"assistant","content":"nothing applied"}),
    ]);
    fake.world().binding_refusal = Some("The conversation is bound to another file".into());
    let outcome = block_on(fake.run(SessionPermission::Write));
    let world = fake.world();
    assert!(!world.calls.iter().any(|call| call == "tracks_add"));
    assert!(
        !world.calls.iter().any(|call| call == "transaction_begin"),
        "no batch opens on the file the conversation left"
    );
    assert_eq!(world.revision, 0, "the document is untouched");
    assert_eq!(
        world.sent.len(),
        2,
        "the model gets the refusal and answers"
    );
    assert_eq!(outcome.text, "nothing applied");
    assert!(!outcome.failed);
}

#[test]
fn the_turn_outcome_carries_the_committed_batches_changes_once() {
    use oc_dsl::change::{Change, ChangeSet};
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add"), ("b", "transaction_commit")]),
        json!({"role":"assistant","content":"All set."}),
    ]);
    fake.world().turn_changes = Some(ChangeSet {
        changes: vec![Change::Tempo {
            from: 120.0,
            to: 132.0,
        }],
        more: 0,
    });
    // The loop answers the model's commit itself; were it dispatched, the fake
    // would also attach the batch facts to that row.
    fake.world().typed_changes_on = Some("transaction_commit".into());
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(!outcome.failed);
    assert_eq!(
        outcome.changes,
        Some(ChangeSet {
            changes: vec![Change::Tempo {
                from: 120.0,
                to: 132.0,
            }],
            more: 0,
        })
    );
    // The deferred commit row itself carries no facts; the turn reports them once.
    assert!(fake.results()[1].1.get("changes").is_none());

    // A read-only turn has no facts to report.
    let plain = Fake::new(vec![json!({"role":"assistant","content":"Nothing to do."})]);
    assert_eq!(block_on(plain.run(SessionPermission::Write)).changes, None);
}

#[test]
fn plan_update_records_typed_rows_and_refreshes_the_derived_context_row() {
    let call = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "p1",
            "type": "function",
            "function": {
                "name": "plan_update",
                "arguments": json!({
                    "goal": "finish the bridge",
                    "items": [
                        {"text": "write", "status": "done"},
                        {"text": "mix", "status": "in_progress"}
                    ]
                }).to_string()
            }
        }]
    });
    let fake = Fake::new(vec![call, json!({"role":"assistant","content":"Planned."})]);
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert_eq!((outcome.text.as_str(), outcome.failed), ("Planned.", false));
    let saved = fake.saved();
    assert_eq!(check_pairing(&saved), Ok(()));
    let state_kinds: Vec<&str> = saved
        .iter()
        .filter_map(|row| row["message_meta"]["state"]["kind"].as_str())
        .collect();
    assert_eq!(state_kinds, ["goal_set", "plan_set"]);
    let (goal, plan) = crate::ai::session_state::current_state(&saved);
    assert_eq!(goal.unwrap().text, "finish the bridge");
    assert_eq!(plan.unwrap().len(), 2);
    assert!(
        saved
            .iter()
            .all(|row| row["context"] != crate::ai::session_state::CONTEXT_STATE),
        "the derived row is never saved"
    );
    // The next request carries the refreshed row after its system prompt.
    let world = fake.world();
    let second = &world.sent[1];
    let context = second
        .iter()
        .position(|row| row["context"] == crate::ai::session_state::CONTEXT_STATE)
        .expect("the next round carries the state");
    assert_eq!(second[context]["role"], "user");
    assert_eq!(
        second[context]["content"],
        "Goal: finish the bridge\nPlan:\n1. [done] write\n2. [in progress] mix"
    );
    assert!(context < crate::ai::agent_loop::wire::turn_ask(second).unwrap());
}

#[test]
fn a_failed_batch_commit_reports_no_changes() {
    use oc_dsl::change::{Change, ChangeSet};
    let fake = Fake::new(vec![round(&[("a", "tracks_add")])]);
    fake.world().turn_changes = Some(ChangeSet {
        changes: vec![Change::Tempo {
            from: 120.0,
            to: 132.0,
        }],
        more: 0,
    });
    fake.world().commit_failure = Some(CommitFailure::Commit);
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(outcome.failed);
    assert_eq!(
        outcome.stop.as_ref().and_then(|stop| stop.commit_failure),
        Some(CommitFailure::Commit)
    );
    assert!(outcome.changes.is_none());
}

#[test]
fn cancel_mid_round_keeps_completed_results_and_marks_the_rest_not_run() {
    let fake = Fake::new(vec![round(&[
        ("a", "tracks_add"),
        ("b", "project_set_tempo"),
        ("c", "tracks_add"),
    ])]);
    fake.world().cancel_on = Some("project_set_tempo".into());
    let outcome = block_on(fake.run(SessionPermission::Write));
    let stop = outcome.stop.expect("a stop");
    assert!(stop.cancelled && stop.committed);
    assert_eq!(
        (stop.done, stop.planned, stop.not_run, stop.edits),
        (2, 3, 1, 2)
    );
    assert_eq!(stop.in_flight.as_deref(), Some("project_set_tempo"));
    let results = fake.results();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0].1["revision"], 1,
        "the real result of a, not a placeholder"
    );
    assert_eq!(
        results[1].1["revision"], 2,
        "b ran before the cancel landed and keeps its result"
    );
    assert_eq!(results[2].1["not_run"], true);
    assert_eq!(
        fake.world().committed,
        1,
        "the edits stay as the turn's one entry"
    );
    assert_eq!(check_pairing(&fake.saved()), Ok(()));
}

fn ask_user_round(id: &str, args: Value) -> Value {
    json!({"role":"assistant","content":null,"tool_calls":[{
        "id":id,"type":"function",
        "function":{"name":"ask_user","arguments":args.to_string()}
    }]})
}

#[test]
fn ask_user_answers_with_a_chosen_option() {
    let fake = Fake::new(vec![
        ask_user_round(
            "q",
            json!({"question":"Which feel?","options":["Warm","Bright"]}),
        ),
        json!({"role":"assistant","content":"ok"}),
    ]);
    fake.world().user_replies.push_back(Some(Reply {
        option: Some(1),
        ..Reply::default()
    }));
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(!outcome.failed, "{outcome:?}");
    let world = fake.world();
    assert_eq!(world.user_questions.len(), 1);
    assert_eq!(world.user_questions[0].question, "Which feel?");
    assert!(world.user_questions[0].free_text);
    drop(world);
    let result = &fake.results()[0].1;
    assert_eq!(result["answered"], true);
    assert_eq!(result["option"], "Bright");
    assert!(result.get("text").is_none());
    assert_eq!(fake.world().calls.len(), 0, "ask_user is not dispatched");
}

#[test]
fn ask_user_answers_with_free_text() {
    let fake = Fake::new(vec![
        ask_user_round("q", json!({"question":"What mood?","free_text":true})),
        json!({"role":"assistant","content":"ok"}),
    ]);
    fake.world().user_replies.push_back(Some(Reply {
        text: Some("Late-night jazz".into()),
        ..Reply::default()
    }));
    block_on(fake.run(SessionPermission::Write));
    let result = &fake.results()[0].1;
    assert_eq!(result["answered"], true);
    assert_eq!(result["text"], "Late-night jazz");
    assert!(result.get("option").is_none());
}

#[test]
fn an_expired_question_answers_with_the_typed_reason() {
    let fake = Fake::new(vec![
        ask_user_round("q", json!({"question":"Continue?"})),
        json!({"role":"assistant","content":"ok"}),
    ]);
    fake.world().user_replies.push_back(Some(Reply::expired()));
    block_on(fake.run(SessionPermission::Write));
    let result = &fake.results()[0].1;
    assert_eq!(result["answered"], false);
    assert_eq!(result["reason"], "expired");
}

#[test]
fn nobody_to_ask_answers_a_question_immediately() {
    let fake = Fake::new(vec![
        ask_user_round("q", json!({"question":"Continue?"})),
        json!({"role":"assistant","content":"ok"}),
    ]);
    block_on(fake.run(SessionPermission::Write));
    let result = &fake.results()[0].1;
    assert_eq!(result["answered"], false);
    assert_eq!(result["reason"], "no_user");
    assert_eq!(fake.world().user_questions.len(), 1);
}

#[test]
fn ask_user_refuses_a_question_over_its_bounds() {
    let long = "x".repeat(crate::agent_tools::question::MAX_QUESTION_CHARS + 1);
    let fake = Fake::new(vec![
        ask_user_round("q", json!({"question":long})),
        json!({"role":"assistant","content":"ok"}),
    ]);
    block_on(fake.run(SessionPermission::Write));
    let result = &fake.results()[0].1;
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|text| text.contains("500"))
    );
    assert!(fake.world().user_questions.is_empty());
}

#[test]
fn the_round_cap_commits_and_closes_with_real_results() {
    let replies = (0..MAX_TOOL_ROUNDS)
        .map(|index| {
            let id = format!("r{index}");
            json!({"role":"assistant","content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":"project_get","arguments":format!("{{\"n\":{index}}}")}}]})
        })
        .collect();
    let fake = Fake::new(replies);
    let outcome = block_on(fake.run(SessionPermission::Write));
    let stop = outcome.stop.expect("a stop");
    assert!(!stop.cancelled);
    assert_eq!((stop.done, stop.not_run), (MAX_TOOL_ROUNDS, 0));
    assert!(outcome.text.starts_with("stopped after 64 tool rounds"));
    let results = fake.results();
    assert_eq!(results.len(), MAX_TOOL_ROUNDS);
    assert!(results.iter().all(|(_, body)| body["ok"] == true));
    let notices: Vec<_> = fake
        .saved()
        .into_iter()
        .filter(|message| message["message_meta"]["kind"] == "round_budget")
        .collect();
    assert_eq!(notices.len(), ROUNDS_LEFT_NOTICE);
    for notice in notices {
        assert_eq!(notice["message_meta"]["notice"]["kind"], "budget");
        assert!(notice["message_meta"]["notice"]["rounds_left"].is_u64());
        assert!(notice["content"].as_str().unwrap().contains("tool round"));
    }
}

#[test]
fn context_share_reports_each_threshold_once_and_only_above_it() {
    let fixed = crate::ai::agent_loop::wire::offered_tool_tokens(None, true);
    let mut second = round(&[]);
    second["tool_calls"] = json!(
        (0..3)
            .map(|index| json!({
                "id": format!("b{index}"),
                "type": "function",
                "function": {"name": "big_read", "arguments": "{}"},
            }))
            .collect::<Vec<_>>()
    );
    let fake = Fake::new(vec![
        round(&[("a", "project_get")]),
        second,
        json!({"role":"assistant","content":"ok"}),
    ]);
    // 62% at the first round leaves room below the fold limit; the 20k results
    // push the same turn past 75% without changing the measured shape.
    {
        let mut world = fake.world();
        world.context_length = (fixed as f64 / 0.62) as u64;
    }
    block_on(fake.run(SessionPermission::Write));
    let shares: Vec<u8> = fake
        .saved()
        .iter()
        .filter(|message| message["message_meta"]["kind"] == "round_budget")
        .filter_map(|message| message["message_meta"]["notice"]["context_share"].as_u64())
        .map(|share| share as u8)
        .collect();
    assert!(!shares.is_empty(), "the measured share is reported");
    assert!(shares[0] >= 60 && shares[0] < 75, "{shares:?}");
    assert!(shares.iter().any(|share| *share >= 75), "{shares:?}");
    assert_eq!(
        shares
            .iter()
            .filter(|share| **share >= 60 && **share < 75)
            .count(),
        1,
        "the 60% threshold is reported once: {shares:?}"
    );
}

#[test]
fn a_context_share_below_the_threshold_stays_silent() {
    let fixed = crate::ai::agent_loop::wire::offered_tool_tokens(None, true);
    let fake = Fake::new(vec![
        round(&[("a", "project_get")]),
        json!({"role":"assistant","content":"ok"}),
    ]);
    {
        let mut world = fake.world();
        world.context_length = fixed * 2;
    }
    block_on(fake.run(SessionPermission::Write));
    assert!(
        !fake.saved().iter().any(|message| {
            message["message_meta"]["kind"] == "round_budget"
                && message["message_meta"]["notice"]["context_share"].is_u64()
        }),
        "under 60% no context row is written"
    );
}

#[test]
fn a_provider_failure_after_edits_keeps_them_and_the_saved_round() {
    let fake = Fake::new(vec![round(&[("a", "tracks_add")])]);
    fake.world()
        .replies
        .push_back(Err(AiError::new("timeout", "The model request timed out")));
    let outcome = block_on(fake.run(SessionPermission::Write));
    let stop = outcome.stop.expect("a stop");
    assert!(stop.timed_out && stop.committed && !stop.failure_kept);
    assert_eq!(outcome.error.unwrap().kind, "timeout");
    assert_eq!(fake.results()[0].1["revision"], 1);
    assert_eq!(fake.world().committed, 1);
}

#[test]
fn a_failure_before_any_edit_is_a_plain_error() {
    let fake = Fake::new(Vec::new());
    fake.world()
        .replies
        .push_back(Err(AiError::new("auth", "bad key")));
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(outcome.failed && outcome.stop.is_none());
    assert_eq!(outcome.text, "bad key");
    assert!(
        !fake
            .world()
            .calls
            .iter()
            .any(|c| c.starts_with("transaction"))
    );
}

#[test]
fn a_store_failure_stops_before_the_unsaved_call_runs() {
    let fake = Fake::new(vec![round(&[("a", "tracks_add"), ("b", "tracks_add")])]);
    // The assistant round and a's result save; b's result write fails.
    fake.world().record_budget = Some(2);
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert_eq!(outcome.error.as_ref().unwrap().kind, "session_save");
    let results = fake.results();
    assert_eq!(results[0].1["revision"], 1);
    assert_eq!(
        results[1].1["outcome_unknown"], true,
        "b ran but its result was not saved"
    );
    assert_eq!(fake.world().committed, 1);
    assert_eq!(check_pairing(&fake.saved()), Ok(()));
}

#[test]
fn a_repeated_mutation_stops_and_keeps_what_landed() {
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add")]),
        round(&[("b", "tracks_add")]),
    ]);
    let outcome = block_on(fake.run(SessionPermission::Write));
    let stop = outcome.stop.expect("a stop");
    assert!(stop.failure_kept && stop.committed);
    assert!(outcome.text.contains("repeated the same call twice"));
    assert_eq!(
        fake.world()
            .calls
            .iter()
            .filter(|c| *c == "tracks_add")
            .count(),
        1
    );
    assert_eq!(check_pairing(&fake.saved()), Ok(()));
}

#[test]
fn a_declined_confirm_call_never_opens_the_batch() {
    let fake = Fake::new(vec![round(&[("a", "tracks_remove")])]);
    fake.world().decisions.push_back(Some(Decision {
        approved: false,
        target: Some("Bass".into()),
        ..Decision::default()
    }));
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(!outcome.failed);
    let world = fake.world();
    assert_eq!(world.asked[0].tool, "tracks_remove");
    assert!(
        world.calls.is_empty(),
        "nothing was dispatched: {:?}",
        world.calls
    );
    drop(world);
    assert_eq!(fake.results()[0].1["declined"], true);
}

#[test]
fn an_expired_question_refuses_its_call_and_never_applies_a_replacement() {
    let replace = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"r","type":"function","function":{"name":"tracks_remove","arguments":"{\"index\":0}"}},
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments":"{\"path\":\"project\",\"new_dsl\":\"@new\"}"}}
    ]});
    let fake = Fake::new(vec![replace, json!({"role":"assistant","content":"ok"})]);
    fake.world().decisions.push_back(Some(Decision::expired()));
    fake.world().decisions.push_back(Some(Decision::expired()));
    let outcome = block_on(fake.run(SessionPermission::RequireApproval));
    assert!(!outcome.failed, "{outcome:?}");
    let results = fake.results();
    for (_, result) in &results[..2] {
        assert_eq!(result["expired"], true);
        assert_eq!(result["declined"], false);
    }
    let world = fake.world();
    assert!(world.calls.is_empty(), "{:?}", world.calls);
    assert_ne!(
        world.dsl, "@new",
        "an expiry is not the nobody-attends path"
    );
    assert_eq!(world.committed, 0);
}

#[test]
fn nobody_to_ask_refuses_a_confirm_call_and_applies_a_valid_replacement() {
    let replace = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"r","type":"function","function":{"name":"tracks_remove","arguments":"{\"index\":0}"}},
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments":"{\"path\":\"project\",\"new_dsl\":\"@new\"}"}}
    ]});
    let fake = Fake::new(vec![replace]);
    let outcome = block_on(fake.run(SessionPermission::RequireApproval));
    assert!(!outcome.failed);
    let results = fake.results();
    assert_eq!(results[0].1["refused"], true);
    assert_eq!(results[1].1["applied"], true);
    assert_eq!(fake.world().dsl, "@new");
    assert_eq!(fake.world().committed, 1);
}

#[test]
fn an_invalid_replacement_never_reaches_the_person() {
    let edit = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments":"{\"path\":\"project\",\"new_dsl\":\"@x\"}"}}
    ]});
    let fake = Fake::new(vec![edit]);
    fake.world().invalid = true;
    block_on(fake.run(SessionPermission::RequireApproval));
    assert!(fake.world().asked.is_empty());
    assert_eq!(fake.results()[0].1["valid"], false);
    assert!(fake.world().batch.is_none() && fake.world().committed == 0);
}

#[test]
fn autoreview_previews_a_replacement_before_applying_it() {
    let edit = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments":"{\"path\":\"track:a\",\"new_dsl\":\"@x\"}"}}
    ]});
    let fake = Fake::new(vec![edit]);
    block_on(fake.run(SessionPermission::Autoreview));
    let world = fake.world();
    let preview = world
        .events
        .iter()
        .position(|event| matches!(event, TurnEvent::Preview { .. }))
        .unwrap();
    let result = world
        .events
        .iter()
        .position(
            |event| matches!(event, TurnEvent::Message { message } if message["role"] == "tool"),
        )
        .unwrap();
    assert!(preview < result);
    assert_eq!(world.dsl, "@x");
}

#[test]
fn a_model_commit_inside_the_turn_is_answered_by_the_loop() {
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add"), ("b", "transaction_commit")]),
        json!({"role":"assistant","content":"ok"}),
    ]);
    block_on(fake.run(SessionPermission::Write));
    assert_eq!(fake.world().committed, 1, "only the loop's own commit ran");
    assert_eq!(fake.results()[1].1["committed"], false);
}

#[test]
fn a_model_rollback_is_saved_as_a_host_notice() {
    let fake = Fake::new(vec![
        round(&[("a", "tracks_add")]),
        round(&[("b", "transaction_rollback")]),
        json!({"role":"assistant","content":"ok"}),
    ]);
    fake.world().decisions.push_back(Some(Decision {
        approved: true,
        ..Decision::default()
    }));
    block_on(fake.run(SessionPermission::Write));
    let world = fake.world();
    let last = world.sent.last().unwrap();
    assert_eq!(world.committed, 0, "nothing is left to commit");
    assert!(
        last.iter()
            .any(|m| m["message_meta"]["kind"] == "host_notice")
    );
    drop(world);
    assert!(
        fake.saved()
            .iter()
            .any(|m| m["message_meta"]["kind"] == "host_notice")
    );
}

#[test]
fn a_large_result_is_paged_only_inside_its_conversation() {
    let page = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"p","type":"function","function":{"name":"read_toolcall","arguments":"{\"id\":\"big\",\"offset\":10,\"limit\":5}"}}
    ]});
    let fake = Fake::new(vec![round(&[("big", "big_read")]), page]);
    block_on(fake.run(SessionPermission::Write));
    let results = fake.results();
    assert_eq!(results[0].1["result_truncated"], true);
    assert_eq!(results[1].1["text"], "xxxxx");
    assert!(results::with_results(|store| store.page("elsewhere", "big", None, None)).is_err());
}

#[test]
fn a_large_result_is_read_from_the_saved_conversation_once_the_cache_lost_it() {
    let page = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"p","type":"function","function":{"name":"read_toolcall","arguments":"{\"id\":\"big\",\"offset\":10,\"limit\":5}"}}
    ]});
    let done = json!({"role":"assistant","content":"done"});
    let fake = Fake::new(vec![
        round(&[("big", "big_read")]),
        done.clone(),
        page,
        done,
    ]);
    block_on(fake.run(SessionPermission::Write));
    let saved = fake.saved();
    let row = saved.iter().find(|m| m["tool_call_id"] == "big").unwrap();
    assert!(row["result_sha"].is_string());
    assert!(row.get("full_result").is_none());
    // A restart, or eviction by other conversations, empties the cache.
    results::with_results(|store| store.forget(&fake.session));
    block_on(fake.run(SessionPermission::Write));
    let results = fake.results();
    assert_eq!(results.last().unwrap().1["text"], "xxxxx", "{results:?}");
}

#[test]
fn a_text_protocol_call_runs_and_its_result_is_a_user_message() {
    let fake = Fake::new(vec![
        json!({"role":"assistant","content":"Checking.\n```json\n{\"tool\":\"project_get\",\"args\":{}}\n```"}),
        json!({"role":"assistant","content":"done"}),
    ]);
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(!outcome.failed);
    let saved = fake.saved();
    assert!(
        saved
            .iter()
            .any(|m| m["message_meta"]["kind"] == "application_result")
    );
    assert_eq!(check_pairing(&saved), Ok(()));
}

#[test]
fn text_protocol_calls_are_named_and_paired_across_turns_of_one_conversation() {
    let call = json!({"role":"assistant","content":"{\"tool\":\"project_get\",\"args\":{}}"});
    let fake = Fake::new(vec![
        call.clone(),
        json!({"role":"assistant","content":"first"}),
        call,
        json!({"role":"assistant","content":"second"}),
    ]);
    for _ in 0..2 {
        let outcome = block_on(fake.run(SessionPermission::Write));
        assert!(!outcome.failed, "{outcome:?}");
    }
    let saved = fake.saved();
    assert_eq!(check_pairing(&saved), Ok(()));
    let asked: Vec<&str> = saved
        .iter()
        .filter(|m| m["role"] == "assistant")
        .filter_map(|m| m["message_meta"]["text_call"]["id"].as_str())
        .collect();
    let answered: Vec<&str> = saved
        .iter()
        .filter(|m| m["message_meta"]["kind"] == "application_result")
        .filter_map(|m| m["message_meta"]["text_call"]["id"].as_str())
        .collect();
    assert_eq!(asked.len(), 2);
    assert_ne!(asked[0], asked[1], "each turn names its own calls");
    assert_eq!(asked, answered);
}

#[test]
fn a_reply_after_cancel_is_saved_and_none_of_its_calls_run() {
    let fake = Fake::new(vec![round(&[("a", "tracks_add"), ("b", "tracks_add")])]);
    struct CancelOnSend<'a>(&'a Fake);
    impl Host for CancelOnSend<'_> {
        async fn send(&self, wire: &[Value], round: usize) -> Result<Value, AiError> {
            let reply = self.0.send(wire, round).await;
            self.0.cancel.cancel();
            reply
        }
        async fn begin_batch(&self, label: &str) -> Result<(), String> {
            self.0.begin_batch(label).await
        }
        async fn commit_batch(&self) -> Result<(), CommitError> {
            self.0.commit_batch().await
        }
        async fn call(&self, tool: &str, args: &Value, cancel: &Cancel) -> Result<Value, Value> {
            self.0.call(tool, args, cancel).await
        }
        async fn ask(&self, request: ApprovalRequest) -> Option<Decision> {
            self.0.ask(request).await
        }
        fn allowed(&self) -> Vec<String> {
            self.0.allowed()
        }
        async fn record(&self, messages: &[Value]) -> Result<Vec<Value>, String> {
            self.0.record(messages).await
        }
        async fn started(&self, call: &str) -> Result<(), String> {
            self.0.started(call).await
        }
        async fn close_round(&self) -> Result<Vec<crate::ai::sessions::turns::Closed>, String> {
            self.0.close_round().await
        }
        fn emit(&self, event: TurnEvent) {
            self.0.emit(event)
        }
        async fn refresh(&self) -> Document {
            self.0.refresh().await
        }
        async fn validate_edit(&self, path: &str, dsl: &str) -> Result<(), Vec<String>> {
            self.0.validate_edit(path, dsl).await
        }
        async fn apply_edit(
            &self,
            path: &str,
            dsl: &str,
            expected: &str,
        ) -> Result<Edited, String> {
            self.0.apply_edit(path, dsl, expected).await
        }
        async fn subtree(&self, path: &str) -> String {
            self.0.subtree(path).await
        }
        async fn introduced_errors(
            &self,
            before: &str,
            after: &str,
        ) -> Result<Vec<String>, String> {
            self.0.introduced_errors(before, after).await
        }
        async fn undo(&self, written: u64) -> Result<(), String> {
            self.0.undo(written).await
        }
    }
    let spec = fake.spec(SessionPermission::Write);
    let outcome = block_on(run_turn(&CancelOnSend(&fake), spec, fake.cancel.clone()));
    let stop = outcome.stop.unwrap();
    assert!(stop.cancelled);
    assert_eq!((stop.done, stop.not_run, stop.edits), (0, 2, 0));
    assert!(fake.world().calls.is_empty());
    assert!(
        fake.results()
            .iter()
            .all(|(_, body)| body["not_run"] == true)
    );
}

#[test]
fn an_unoffered_replacement_is_refused_by_its_toolset() {
    let edit = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments":"{\"path\":\"project\",\"new_dsl\":\"@x\"}"}}
    ]});
    let fake = Fake::new(vec![edit]);
    let mut spec = fake.spec(SessionPermission::Write);
    spec.toolsets = Some(vec!["project-edit".into()]);
    block_on(run_turn(&fake, spec, fake.cancel.clone()));
    assert_eq!(fake.world().dsl, "@project");
    assert!(
        fake.results()[0].1["error"]
            .as_str()
            .unwrap()
            .contains("edit_subtree")
    );
}

#[tokio::test]
async fn steering_finishes_all_tool_results_without_cancelling_or_splitting_batch() {
    let fake = Fake::new(vec![
        json!({"role":"assistant", "content":null,"tool_calls":[
            {"id":"a","type":"function","function":{"name":"tracks_add","arguments":"{\"name\":\"bass\"}"}},
            {"id":"b","type":"function","function":{"name":"project_set_tempo","arguments":"{\"tempo\":138}"}}
        ]}),
        json!({"role":"assistant","content":"done"}),
    ]);
    fake.world().steer_on_tool = Some("tracks_add".into());
    let outcome = fake.run(SessionPermission::Write).await;
    assert!(!outcome.failed, "{outcome:?}");
    assert!(!fake.cancel.is_cancelled());
    let world = fake.world();
    assert_eq!(world.committed, 1);
    assert_eq!(world.sent.len(), 2);
    let second = &world.sent[1];
    let steer = second
        .iter()
        .position(|row| row["content"] == "soften the highs")
        .unwrap();
    assert_eq!(second[steer - 1]["tool_call_id"], "b");
    assert_eq!(second[steer - 2]["tool_call_id"], "a");
    drop(world);
    check_pairing(&fake.saved()).unwrap();
    assert_eq!(
        fake.saved()
            .iter()
            .filter(|row| row["content"] == "soften the highs")
            .count(),
        1
    );
}

#[tokio::test]
async fn steering_arriving_during_a_final_reply_continues_the_same_turn() {
    let fake = Fake::new(vec![
        json!({"role":"assistant","content":"first answer"}),
        json!({"role":"assistant","content":"revised answer"}),
    ]);
    fake.world().steer_on_send = Some(1);
    let outcome = fake.run(SessionPermission::Write).await;
    assert!(!outcome.failed, "{outcome:?}");
    assert_eq!(outcome.text, "revised answer");
    assert_eq!(fake.world().sent.len(), 2);
    assert_eq!(
        fake.saved()
            .iter()
            .map(|row| row["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["user", "assistant", "user", "assistant"]
    );
    assert!(fake.steering.enqueue(json!({})).is_err());
}

#[tokio::test]
async fn steering_save_failure_rejects_delivery_and_does_not_send_the_message() {
    let fake = Fake::new(vec![json!({"role":"assistant","content":"first answer"})]);
    fake.world().steer_on_send = Some(1);
    fake.world().record_budget = Some(1);
    let outcome = fake.run(SessionPermission::Write).await;
    assert!(outcome.failed);
    assert_eq!(fake.world().sent.len(), 1);
    assert!(
        !fake
            .saved()
            .iter()
            .any(|row| row["content"] == "make the drop longer")
    );
    assert!(fake.steering.enqueue(json!({})).is_err());
}

#[test]
fn a_turn_tells_a_failed_commit_from_a_committed_batch_its_file_missed() {
    for failure in [CommitFailure::Commit, CommitFailure::Save] {
        let fake = Fake::new(vec![
            round(&[("a", "tracks_add")]),
            json!({"role":"assistant","content":"done"}),
        ]);
        fake.world().commit_failure = Some(failure);
        let outcome = block_on(fake.run(SessionPermission::Write));
        assert!(outcome.failed, "{outcome:?}");
        let stop = outcome.stop.unwrap();
        assert_eq!(stop.commit_failure, Some(failure));
        assert_eq!(stop.edits, 1);
        // Only a commit that failed leaves the edits outside the project.
        assert_eq!(
            stop.committed,
            failure == CommitFailure::Save,
            "{failure:?}"
        );
        let expected = match failure {
            CommitFailure::Save => "committed, but the project file was not written",
            CommitFailure::Commit => "could not be committed",
        };
        assert!(outcome.text.contains(expected), "{}", outcome.text);
    }
}

#[tokio::test]
async fn a_steer_during_the_last_round_s_answer_is_refused_and_the_answer_stands() {
    let mut replies: Vec<Value> = (0..MAX_TOOL_ROUNDS - 1)
        .map(|n| round(&[(&format!("c{n}"), "tracks_list")]))
        .collect();
    replies.push(json!({"role":"assistant","content":"final answer"}));
    let fake = Fake::new(replies);
    fake.world().steer_on_send = Some(MAX_TOOL_ROUNDS);
    let outcome = fake.run(SessionPermission::Write).await;
    assert!(!outcome.failed, "{outcome:?}");
    assert_eq!(outcome.text, "final answer");
    assert!(
        !fake
            .saved()
            .iter()
            .any(|row| row["content"] == "make the drop longer"),
        "a message with no round to answer it is not saved"
    );
}

#[test]
fn a_review_takes_back_only_an_edit_the_call_made_and_only_for_its_own_errors() {
    // The call writes a document that does not validate: it is taken back.
    let fake = Fake::new(vec![round(&[("a", "tracks_add")])]);
    fake.world().invalid_from = Some(1);
    block_on(fake.run(SessionPermission::Autoreview));
    assert_eq!(fake.world().undos, 1);
    assert_eq!(fake.results()[0].1["undone"], true);

    // A call that changed nothing never undoes an earlier edit, even when the
    // project does not validate.
    let fake = Fake::new(vec![round(&[("a", "project_set_tempo")])]);
    {
        let mut world = fake.world();
        world.invalid = true;
        world.noop = true;
    }
    block_on(fake.run(SessionPermission::Autoreview));
    assert_eq!(fake.world().undos, 0);

    // A change someone else made earlier in the turn is not the call's either,
    // though it moved the text away from the turn's last edit.
    let fake = Fake::new(vec![
        round(&[("r", "tracks_list")]),
        round(&[("a", "project_set_tempo")]),
    ]);
    {
        let mut world = fake.world();
        world.invalid_from = Some(1);
        world.noop = true;
        world.edit_elsewhere_on = Some("tracks_list".into());
    }
    block_on(fake.run(SessionPermission::Autoreview));
    assert_eq!(fake.world().undos, 0);
    assert!(
        fake.results()
            .iter()
            .all(|(_, result)| result.get("undone").is_none())
    );

    // Errors the project already had are not the call's.
    let fake = Fake::new(vec![round(&[("a", "tracks_add")])]);
    fake.world().invalid = true;
    block_on(fake.run(SessionPermission::Autoreview));
    assert_eq!(fake.world().undos, 0);
    assert!(fake.results()[0].1.get("undone").is_none());
}

#[test]
fn a_call_quoted_in_a_native_model_s_answer_never_runs() {
    let fake = Fake::new(vec![json!({"role":"assistant",
        "content":"You could run {\"tool\":\"tracks_add\",\"args\":{\"name\":\"x\"}} yourself."})]);
    fake.world().text_protocol = false;
    let outcome = block_on(fake.run(SessionPermission::Write));
    assert!(!outcome.failed, "{outcome:?}");
    assert!(outcome.text.starts_with("You could run"));
    assert!(fake.world().calls.is_empty(), "{:?}", fake.world().calls);
}

#[test]
fn a_reported_input_past_the_window_stops_the_turn_before_the_next_send() {
    let window = wire::offered_tool_tokens(None, true) + 5_000;
    let mut first = round(&[("a", "project_get")]);
    // The estimate of this small wire is far below the window; the provider's
    // own count for it is not.
    first["usage"] = json!({"input": window + 1});
    let fake = Fake::new(vec![first, json!({"role":"assistant","content":"done"})]);
    let mut spec = fake.spec(SessionPermission::Write);
    spec.context_length = window;
    let outcome = block_on(run_turn(&fake, spec, fake.cancel.clone()));
    assert!(outcome.failed);
    assert_eq!(outcome.error.unwrap().kind, "context_overflow");
    assert_eq!(fake.world().sent.len(), 1, "the oversized wire is not sent");
    assert_eq!(check_pairing(&fake.saved()), Ok(()));
}

#[test]
fn typed_changes_leave_the_result_and_stay_on_the_saved_row() {
    let mut result = json!({
        "revision": 1,
        "changes": {
            "changes": [{"kind": "tempo", "from": 120.0, "to": 132.0}],
            "more": 0,
        },
    });
    let changes = take_changes(&mut result).expect("the result carries the facts");
    assert!(result.get("changes").is_none(), "{result}");
    let call = Call {
        id: "c1".into(),
        name: "project_set_tempo".into(),
        arguments: "{}".into(),
    };
    let mut message = result_message(&call, true, &result.to_string());
    message["changes"] = changes;
    assert_eq!(message["changes"]["changes"][0]["kind"], "tempo");
    assert!(!message["content"].as_str().unwrap().contains("changes"));
}

#[test]
fn an_edit_subtree_row_saves_the_typed_changes_of_its_edit() {
    let new = "@project \"S\"\n  tempo: 132\n";
    let args = json!({"path": "project", "new_dsl": new}).to_string();
    let edit = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"e","type":"function","function":{"name":"edit_subtree","arguments": args}}
    ]});
    let fake = Fake::new(vec![edit]);
    fake.world().dsl = "@project \"S\"\n  tempo: 100\n".into();
    block_on(fake.run(SessionPermission::Write));
    assert_eq!(fake.world().dsl, new);
    let saved = fake.saved();
    let row = saved
        .iter()
        .find(|message| message["tool_call_id"] == "e")
        .unwrap();
    assert_eq!(
        row["changes"]["changes"],
        json!([{"kind": "tempo", "from": 100.0, "to": 132.0}])
    );
    assert!(!row["content"].as_str().unwrap().contains("changes"));
}

#[test]
fn a_mutating_tool_row_saves_typed_changes_and_a_read_only_row_does_not() {
    let fake = Fake::new(vec![round(&[
        ("a", "project_set_tempo"),
        ("b", "tracks_list"),
    ])]);
    fake.world().typed_changes_on = Some("project_set_tempo".into());
    block_on(fake.run(SessionPermission::Write));
    let saved = fake.saved();
    let changed = saved
        .iter()
        .find(|message| message["tool_call_id"] == "a")
        .unwrap();
    assert_eq!(
        changed["changes"]["changes"],
        json!([{"kind": "tempo", "from": 120.0, "to": 132.0}])
    );
    assert!(!changed["content"].as_str().unwrap().contains("changes"));
    let read = saved
        .iter()
        .find(|message| message["tool_call_id"] == "b")
        .unwrap();
    assert!(read.get("changes").is_none(), "{read}");
}

#[test]
fn a_mid_turn_compaction_keeps_the_open_round_and_pairing() {
    let fake = Fake::new(vec![
        round(&[("a", "project_get")]),
        json!({"role":"assistant","content":"done"}),
    ]);
    let mut older = Vec::new();
    for step in 0..24 {
        let id = format!("old{step}");
        older.push(round(&[(&id, "project_get")]));
        older.push(json!({
            "role":"tool",
            "tool_call_id": id,
            "content": json!({"ok": true, "data": "x".repeat(600)}).to_string(),
        }));
    }
    fake.store.append_turn(&fake.session, &older, None).unwrap();
    let mut wire = vec![json!({"role":"system","content":"sys"})];
    wire.extend(fake.saved());
    let person =
        json!({"role":"user","content":"make it","message_meta":{"version":1,"kind":"user"}});
    wire.push(person.clone());
    fake.store
        .append_turn(&fake.session, std::slice::from_ref(&person), None)
        .unwrap();
    let spec = TurnSpec {
        wire,
        permission: SessionPermission::Write,
        label: "AI edit".into(),
        audio: None,
        scope: fake.session.clone(),
        // An empty set keeps the fixed tool-list cost small, so the
        // conversation itself drives the trigger.
        toolsets: Some(Vec::new()),
        text_protocol: true,
        context_length: 4_000,
        max_output: 0,
    };
    let outcome = block_on(run_turn(&fake, spec, fake.cancel.clone()));
    assert!(!outcome.failed, "{outcome:?}");
    let sent = fake.world().sent.clone();
    let compacted = sent
        .iter()
        .find(|wire| wire.len() > 1 && wire[1]["context"] == "summary")
        .expect("a later round was sent after a compaction");
    assert_eq!(
        compacted[1]["message_meta"]["kind"], "summary",
        "the first saved row is the compaction summary"
    );
    assert!(
        compacted[1]["message_meta"]
            .get("discarded_auditions")
            .is_some(),
        "the store's summary metadata is kept"
    );
    // The open round is recorded after the compaction and survives verbatim.
    let second = &sent[1];
    assert!(
        second.iter().any(|row| row["tool_call_id"] == "a"),
        "the open round's result stays in the next request: {second:?}"
    );
    let saved = fake.saved();
    assert_eq!(check_pairing(&saved), Ok(()));
    assert!(saved.iter().any(|row| row["tool_call_id"] == "a"));
}

#[test]
fn cancellation_during_compaction_stops_before_another_provider_round() {
    let fake = Fake::new(vec![
        json!({"role":"assistant","content":"must not be sent"}),
    ]);
    fake.world().hold_compaction = true;
    let mut spec = fake.spec(SessionPermission::Write);
    spec.toolsets = Some(Vec::new());
    spec.context_length = 4_000;
    let padding = json!({"role":"assistant","content":"old ".repeat(5_000)});
    fake.store
        .append_turn(&fake.session, std::slice::from_ref(&padding), None)
        .unwrap();
    spec.wire.push(padding);
    let before = fake.saved();
    let outcome = block_on(async {
        let stop = async {
            fake.compaction_started.notified().await;
            fake.cancel.cancel();
        };
        let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(run_turn(&fake, spec, fake.cancel.clone()), stop)
        })
        .await
        .expect("Stop must not wait for a summary response");
        outcome
    });
    assert!(outcome.stop.is_some_and(|stop| stop.cancelled));
    assert!(fake.world().sent.is_empty());
    assert_eq!(fake.saved(), before);
}
