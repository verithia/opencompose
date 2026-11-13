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
    /// Cancel when this tool is dispatched, after it ran.
    cancel_on: Option<String>,
    /// Fail the store write after this many records.
    record_budget: Option<usize>,
    invalid: bool,
}

struct Fake {
    world: Mutex<World>,
    store: Store,
    session: String,
    cancel: Cancel,
    _root: tempfile::TempDir,
}

impl Fake {
    fn new(replies: Vec<Value>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let session = store.create(None, None, Some("Fixture"), None).unwrap().id;
        Self {
            world: Mutex::new(World {
                replies: replies.into_iter().map(Ok).collect(),
                dsl: "@project".into(),
                ..World::default()
            }),
            store,
            session,
            cancel: Cancel::default(),
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
        TurnSpec {
            wire: vec![json!({"role":"system","content":"sys"}), user],
            permission,
            label: "AI edit".into(),
            audio: None,
            scope: self.session.clone(),
            toolsets: None,
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
    async fn begin_batch(&self, label: &str) -> Result<(), String> {
        self.call(
            "transaction_begin",
            &json!({ "label": label }),
            &Cancel::default(),
        )
        .await
        .map(|_| ())
        .map_err(|refusal| refusal.to_string())
    }

    async fn commit_batch(&self) -> Result<(), String> {
        self.call("transaction_commit", &json!({}), &Cancel::default())
            .await
            .map(|_| ())
            .map_err(|refusal| refusal.to_string())
    }

    async fn send(&self, wire: &[Value], _round: usize) -> Result<Value, AiError> {
        let mut world = self.world();
        world.sent.push(wire.to_vec());
        world
            .replies
            .pop_front()
            .unwrap_or_else(|| Ok(json!({"role":"assistant","content":"done"})))
    }

    async fn call(&self, tool: &str, args: &Value, _cancel: &Cancel) -> Result<Value, Value> {
        let mut world = self.world();
        world.calls.push(tool.to_owned());
        if world.cancel_on.as_deref() == Some(tool) {
            self.cancel.cancel();
        }
        match tool {
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
                world.revision += 1;
                world.dsl = format!("@project r{}", world.revision);
                Ok(
                    json!({"revision": world.revision, "dsl_source": world.dsl, "project": {"tracks": []}}),
                )
            }
            "big_read" => Ok(json!({"rows": "x".repeat(20_000)})),
            "refused" => Err(json!({"error": "refused: nope"})),
            _ => Ok(json!({"ok": true})),
        }
    }

    async fn ask(&self, request: ApprovalRequest) -> Option<Decision> {
        let mut world = self.world();
        world.asked.push(request);
        world.decisions.pop_front().flatten()
    }

    fn allowed(&self) -> Vec<String> {
        Vec::new()
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

    async fn started(&self, call_id: &str) -> Result<(), String> {
        self.store.mark_started(&self.session, call_id)
    }

    async fn close_round(&self) -> Result<Vec<Value>, String> {
        self.store.close_round(&self.session)
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

    async fn validate_edit(&self, _path: &str, _dsl: &str) -> Result<(), Vec<String>> {
        if self.world().invalid {
            Err(vec!["bad".into()])
        } else {
            Ok(())
        }
    }

    async fn apply_edit(&self, _path: &str, dsl: &str, expected: &str) -> Result<Document, String> {
        let mut world = self.world();
        if world.dsl != expected {
            return Err("changed".into());
        }
        if world.batch.is_none() {
            return Err("outside the turn's batch".into());
        }
        world.revision += 1;
        world.dsl = dsl.to_owned();
        Ok(Document {
            dsl: world.dsl.clone(),
            revision: world.revision,
            track_count: 1,
        })
    }

    async fn subtree(&self, _path: &str) -> String {
        "old".into()
    }

    async fn undo(&self) -> Result<(), String> {
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
    let notices = fake
        .saved()
        .iter()
        .filter(|message| message["message_meta"]["kind"] == "round_budget")
        .count();
    assert_eq!(notices, ROUNDS_LEFT_NOTICE);
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
        always: false,
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
        target: None,
        always: false,
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
        async fn commit_batch(&self) -> Result<(), String> {
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
        async fn close_round(&self) -> Result<Vec<Value>, String> {
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
        ) -> Result<Document, String> {
            self.0.apply_edit(path, dsl, expected).await
        }
        async fn subtree(&self, path: &str) -> String {
            self.0.subtree(path).await
        }
        async fn undo(&self) -> Result<(), String> {
            self.0.undo().await
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
