use super::*;
use crate::ai::auth::Credential;
use crate::ai::test_support::canned_by_request;
use crate::ai::{self, provider_for, request_from_wire};
use crate::commands::{AiCapabilities, AiProviderEntry, AuthKind, ProviderKind, Wire};

#[test]
fn a_trimmed_conversation_never_starts_with_an_orphan_result() {
    let call = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"a","type":"function","function":{"name":"tracks_add","arguments":"{}"}}
    ]});
    let big = "x".repeat(400);
    let view = vec![
        user("go"),
        call,
        json!({"role":"tool","tool_call_id":"a","content":big}),
        user("next"),
    ];
    // The budget fits the result and the last message, not the call.
    let kept = newest_that_fit(&view, 500);
    assert_eq!(turns::check_pairing(&kept), Ok(()));
    assert_eq!(kept.first().unwrap()["content"], "next");
}

#[test]
fn unavailable_handoff_retains_the_clip_without_archiving_a_reused_export() {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    let root = tempfile::tempdir().unwrap();
    let stale = root.path().join("reused.wav");
    std::fs::write(
        &stale,
        oc_engine::wav::mono_wav_bytes(&[0.75; 800], 16_000).unwrap(),
    )
    .unwrap();
    let mut messages = vec![json!({
        "role":"user", "message_meta":{"version":1,"kind":"audition"}, "content":"Audition",
        "input_audio":{"data":audition_clip(),"format":"wav","source_render_unavailable":true,"source_path":stale}
    })];
    persist_auditions(
        root.path(),
        &mut messages,
        &SessionAudioSettings {
            format: SessionAudioFormat::Wav,
            mp3_kbps: 128,
        },
    )
    .unwrap();
    assert!(messages[0].get("input_audio").is_none());
    assert_eq!(messages[0]["media"].as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["source_render_unavailable"], true);
    assert!(messages[0].get("source_render").is_none());
}

fn user(text: &str) -> Value {
    json!({ "role": "user", "content": text })
}

fn reply(text: &str) -> Value {
    json!({ "role": "assistant", "content": text })
}

fn expected_user(text: &str) -> Value {
    json!({"role":"user", "content":text, "message_meta":{"version":1,"kind":"user"}})
}

fn expected_reply(text: &str) -> Value {
    json!({"role":"assistant", "content":text, "message_meta":{"version":1,"kind":"assistant"}})
}

/// An assistant turn as a provider that requires thought signatures sends it.
fn tool_call(name: &str, arguments: &str, signature: &str) -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "provider_state": [{"type": "reasoning", "encrypted_content": "opaque-continuity"}],
        "tool_calls": [{
            "id": format!("call-{name}"),
            "type": "function",
            "function": { "name": name, "arguments": arguments },
            "extra_content": {"google": {"thought_signature": signature}},
            "signature": signature
        }]
    })
}

fn tool_result(name: &str, content: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": format!("call-{name}"), "content": content })
}

/// The workspace a fixture's project file lives in.
fn workspace(project: &Path) -> PathBuf {
    project.parent().unwrap().to_path_buf()
}

fn facts(project: &Path) -> ProjectFacts {
    ProjectFacts {
        path: Some(project.display().to_string()),
        dsl_digest: dsl_digest("@project\n  tempo: 128\n"),
        track_count: 18,
    }
}

fn conversation() -> Vec<Value> {
    let mut view = vec![user("make the kick punchier and add a pad")];
    for step in 0..8 {
        let name = if step % 2 == 0 {
            "tracks_mix"
        } else {
            "render_range"
        };
        let arguments = format!("{{\"track\":\"kick\",\"gain_db\":{}}}", -step);
        view.push(tool_call(name, &arguments, &format!("sig-{step}")));
        let result = if step == 3 {
            json!({ "error": "track 'pad' not found" }).to_string()
        } else {
            // Tool payloads are what a real turn spends its context on; the
            // summary has to be smaller than they are for compaction to pay.
            let notes: Vec<String> = (0..12)
                .map(|index| format!("floor(step={step}, band={index}): -{index}.5 dB"))
                .collect();
            json!({ "ok": true, "tracks": 18, "step": step, "notes": notes }).to_string()
        };
        view.push(tool_result(name, &result));
        view.push(user(&format!("and now step {step}")));
        view.push(reply(&format!("step {step} done")));
    }
    view.push(user("now the bass"));
    view
}

#[test]
fn repeated_compaction_preserves_standing_goal_and_artifact_values() {
    let (_, project) = fixture("artifact-intent");
    let goal = "Recreate the song with editable instruments and retain the separated vocal.";
    let view = vec![
        json!({"role":"user","content":goal}),
        json!({"role":"assistant","tool_calls":[{"id":"s","function":{"name":"separate_stems","arguments":"{\"path\":\"/source.wav\"}"}}]}),
        json!({"role":"tool","tool_call_id":"s","content":json!({"stems":[{"name":"vocals","path":"/complete/unique/vocals.wav"}]}).to_string()}),
    ];
    let first = summary_text(&view, view.len(), &[], &facts(&project), 1000, "fixture");
    let next = vec![
        json!({"role":"user","content":first}),
        json!({"role":"user","content":"Continue"}),
    ];
    let second = summary_text(&next, next.len(), &[], &facts(&project), 500, "again");
    assert!(second.contains(&format!("Standing intent: {goal}")));
    assert!(second.contains("/complete/unique/vocals.wav"));
    assert!(second.contains("/source.wav"));
}

#[test]
fn bounded_audio_replay_preserves_archive_and_prioritizes_explicit_attachment() {
    let clip = |name: &str| json!({"role":"user", "content":name, "media":[{"kind":"audio","name":name,"path":format!("/archive/{name}.wav"),"sha256":name,"bytes":9_000_000}]});
    let archive = vec![clip("old"), clip("recent"), clip("explicit")];
    let mut outbound = archive.clone();
    bound_media_replay(&mut outbound, &[clip("explicit")]);
    assert_eq!(
        archive
            .iter()
            .filter(|m| m["media"].as_array().unwrap().len() == 1)
            .count(),
        3
    );
    assert_eq!(outbound[2]["media"].as_array().unwrap().len(), 1);
    assert!(outbound[0]["media"].as_array().unwrap().is_empty());
    assert!(outbound[1]["media"].as_array().unwrap().is_empty());
    assert!(
        outbound[2]["content"]
            .as_str()
            .unwrap()
            .contains("2 older attachment(s)")
    );
    assert!(
        outbound[2]["content"]
            .as_str()
            .unwrap()
            .contains("/archive/old.wav")
    );
}

#[test]
fn two_sessions_for_one_workspace_and_one_detached_round_trip() {
    // `OCP_SESSIONS_KEEP=1` keeps the fixture tree and prints its root, so a
    // run can be inspected outside the test.
    let keep = std::env::var_os("OCP_SESSIONS_KEEP").is_some();
    let (store, project) = fixture("roundtrip");
    let place = workspace(&project);
    let first = store
        .create(Some(&place), Some(&project), Some("Intro idea"), None)
        .unwrap();
    let second = store
        .create(Some(&place), Some(&project), Some("Second idea"), None)
        .unwrap();
    let detached = store.create(None, None, Some("No project"), None).unwrap();

    assert_eq!(store.list(Scope::Workspace(Some(&place))).unwrap().len(), 2);
    assert_eq!(store.list(Scope::All).unwrap().len(), 3);
    assert_eq!(detached.workspace_key, DETACHED_KEY);

    let key = store.workspace_key(Some(&place));
    assert_eq!(key.len(), 16);
    assert!(store.root().join(&key).join(WORKSPACE_FILE).is_file());
    let record: Value = serde_json::from_slice(
        &std::fs::read(store.root().join(&key).join(WORKSPACE_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        record["workspace"],
        canonical_dir(&place).to_string_lossy().as_ref()
    );
    assert!(
        !store
            .root()
            .join(DETACHED_KEY)
            .join(WORKSPACE_FILE)
            .exists(),
        "a detached conversation has no workspace to record"
    );

    #[cfg(unix)]
    let first_dir = store.root().join(&key).join(&first.id);
    let context = facts(&project);
    for (session, text) in [(&first, "first"), (&second, "second")] {
        store
            .prepare(
                &session.id,
                &[user(text)],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    history: None,
                    overhead: 0,
                    context_length: 0,
                    max_output: 0,
                    prompt_tokens: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                    outside: None,
                    surface: None,
                },
            )
            .unwrap();
        store
            .record_reply(
                &session.id,
                &reply(&format!("{text} answer")),
                &context,
                "fixture-provider",
                "fixture-model",
            )
            .unwrap();
    }
    let loaded = store.load(&first.id).unwrap();
    assert_eq!(
        loaded.messages,
        vec![expected_user("first"), expected_reply("first answer")]
    );
    assert_eq!(loaded.meta.turns, 1);
    assert!(loaded.meta.tokens > 0);
    assert_eq!(loaded.meta.title, "Intro idea");
    assert_eq!(store.load(&second.id).unwrap().messages.len(), 2);
    assert!(store.load(&detached.id).unwrap().messages.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&first_dir), 0o700);
        assert_eq!(mode(&first_dir.join(META_FILE)), 0o600);
        assert_eq!(mode(&first_dir.join(MESSAGES_FILE)), 0o600);
        assert_eq!(mode(store.root()), 0o700);
    }
    if keep {
        println!("OCP_SESSIONS_ROOT {}", store.root().display());
        return;
    }
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_pruned_conversation_takes_the_audio_only_it_named() {
    let (store, project) = fixture("prune-audio");
    let hash = "c".repeat(64);
    let first = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("first"),
            None,
        )
        .unwrap();
    store
        .append_turn(
            &first.id,
            &[json!({"role":"user","content":format!("clip {hash}"),"message_meta":{"version":1,"kind":"user"}})],
            None,
        )
        .unwrap();
    std::fs::create_dir_all(clip_dir(store.root())).unwrap();
    let clip = clip_dir(store.root()).join(format!("{hash}.wav"));
    std::fs::write(&clip, b"RIFF").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    for index in 0..MAX_SESSIONS_PER_WORKSPACE {
        store
            .create(
                Some(&workspace(&project)),
                Some(&project),
                Some(&format!("session {index}")),
                None,
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(store.find(&first.id).is_err(), "the oldest was pruned");
    assert!(!clip.exists(), "its audio went with it");
}

#[test]
fn sessions_list_is_newest_first_and_prunes_the_oldest() {
    let (store, project) = fixture("prune");
    let mut created = Vec::new();
    for index in 0..=MAX_SESSIONS_PER_WORKSPACE {
        created.push(
            store
                .create(
                    Some(&workspace(&project)),
                    Some(&project),
                    Some(&format!("session {index}")),
                    None,
                )
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let listed = store
        .list(Scope::Workspace(Some(&workspace(&project))))
        .unwrap();
    assert_eq!(listed.len(), MAX_SESSIONS_PER_WORKSPACE);
    let ids: Vec<&str> = listed.iter().map(|meta| meta.id.as_str()).collect();
    assert!(
        !ids.contains(&created[0].id.as_str()),
        "the least recently updated session is the one dropped"
    );
    assert_eq!(ids[0], created[created.len() - 1].id);
    let mut sorted = listed.clone();
    sorted.sort_by(|left, right| {
        right
            .updated
            .cmp(&left.updated)
            .then_with(|| right.id.cmp(&left.id))
    });
    assert_eq!(sorted, listed);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn tool_result_pairing_survives_null_assistant_content_in_a_batch() {
    let (store, project) = fixture("multi-tool-result-pairing");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let question = user("Find a piano and inspect the project");
    store
        .prepare(
            &session.id,
            std::slice::from_ref(&question),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let call = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"font-call","type":"function","function":{"name":"soundfonts_list","arguments":"{}"}},
        {"id":"project-call","type":"function","function":{"name":"project_get","arguments":"{}"}}
    ]});
    store
        .record_reply(&session.id, &call, &context, "fixture", "model")
        .unwrap();
    let fonts = json!({"role":"tool","tool_call_id":"font-call","content":"{\"discovered\":[\"piano.sf3\"]}","message_meta":{"version":1,"kind":"tool"}});
    let project_result = json!({"role":"tool","tool_call_id":"project-call","content":"{\"project\":\"Evening Sketch\"}","message_meta":{"version":1,"kind":"tool"}});
    let prepared = store
        .prepare(
            &session.id,
            &[question, call, fonts.clone(), project_result.clone()],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.appended, 2);
    assert_eq!(&prepared.messages[2..], &[fonts, project_result]);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn tool_result_pairing_does_not_match_different_empty_assistant_calls() {
    let first = json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","function":{"name":"project_get","arguments":"{}"}}]});
    let second = json!({"role":"assistant","content":"","tool_calls":[{"id":"b","function":{"name":"soundfonts_list","arguments":"{}"}}]});
    assert!(!same_turn(&first, &second));
    assert!(same_turn(&first, &saved_message(&first).unwrap()));
    assert!(!same_turn(
        &json!({"role":"tool","content":"{}","tool_call_id":"a"}),
        &json!({"role":"tool","content":"{}","tool_call_id":"b"})
    ));
}

#[test]
fn a_reply_append_the_cap_trimmed_counts_the_view_it_kept() {
    let (store, project) = fixture("reply-cap");
    let store = store.with_session_cap(100_000);
    let session = store
        .create(Some(project.parent().unwrap()), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let filler = "x".repeat(60_000);
    store
        .append_turn(&session.id, &[user(&format!("old {filler}"))], None)
        .unwrap();
    store
        .record_reply(
            &session.id,
            &reply(&format!("new {filler}")),
            &context,
            "p",
            "m",
        )
        .unwrap();
    let view = store.view(&session.id).unwrap();
    let (_, meta) = store.find(&session.id).unwrap();
    assert!(
        view[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("dropped"),
        "the cap replaced the view and states what it dropped"
    );
    assert_eq!(meta.tokens, estimate_tokens(&view));
}

#[test]
fn autosave_appends_a_turn_once_and_never_replays_the_rebuilt_history() {
    let (store, project) = fixture("autosave");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let system = json!({ "role": "system", "content": "you are the panel" });

    let turn = json!([system, user("make it punchy")]);
    let prepared = store
        .prepare(
            &session.id,
            turn.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.appended, 1);
    assert!(prepared.live.is_none());
    assert_eq!(prepared.messages, vec![expected_user("make it punchy")]);

    let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
    store
        .record_reply(
            &session.id,
            &call,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    // Round 2 of the same turn replays the assistant call and adds one result.
    let round = json!([
        system,
        user("make it punchy"),
        call,
        tool_result("tracks_mix", "{\"ok\":true}")
    ]);
    let prepared = store
        .prepare(
            &session.id,
            round.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.appended, 1);
    assert_eq!(prepared.messages.len(), 3);
    assert_eq!(prepared.messages[2]["role"], "tool");

    // The next user turn rebuilds its own history from the panel's UI list.
    let final_answer = reply("Kick is at -3 dB.");
    store
        .record_reply(
            &session.id,
            &final_answer,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    let next = json!([
        system,
        user("make it punchy"),
        reply("Kick is at -3 dB."),
        user("now the bass")
    ]);
    let prepared = store
        .prepare(
            &session.id,
            next.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.appended, 1, "only the pending user message is new");
    assert_eq!(
        prepared.messages.last().unwrap(),
        &expected_user("now the bass")
    );
    assert_eq!(prepared.messages.len(), 5);

    // The same request twice appends nothing.
    let again = store
        .prepare(
            &session.id,
            next.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(again.appended, 0);
    assert_eq!(again.messages.len(), 5);
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.messages.len(), 5);
    // The saved transcript is the wire transcript: the tool row is there, in
    // order, with the call id its assistant turn used.
    assert_eq!(loaded.messages[1]["role"], "assistant");
    assert_eq!(loaded.messages[1]["tool_calls"][0]["id"], "call-tracks_mix");
    assert_eq!(loaded.messages[2]["role"], "tool");
    assert_eq!(loaded.messages[2]["tool_call_id"], "call-tracks_mix");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn compaction_leaves_no_tool_call_state_or_signature_to_replay() {
    let (store, project) = fixture("compact");
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("punchy kick"),
            None,
        )
        .unwrap();
    let context = facts(&project);
    let view = conversation();
    let before = estimate_tokens(&view);
    store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();

    let outcome = store.compact(&session.id, None, &context).unwrap();
    assert!(strips_replay_state(&outcome.messages));
    assert_eq!(outcome.dropped_from, 0);
    assert!(outcome.dropped_to > 0);
    assert!(outcome.tokens_after < outcome.tokens_before);
    assert_eq!(outcome.tokens_before, before);
    assert!(outcome.retained_pairs > 0);

    let text = outcome.messages[0]["content"].as_str().unwrap();
    assert!(text.contains("Retained recent tool calls"), "{text}");
    assert!(text.contains("tracks_mix → "), "{text}");
    assert!(text.contains("What was tried and failed"), "{text}");
    assert!(text.contains("track 'pad' not found"), "{text}");
    assert!(text.contains("Standing intent"), "{text}");
    assert!(
        text.contains(&dsl_digest("@project\n  tempo: 128\n")),
        "{text}"
    );
    assert!(text.contains("Dropped messages 0.."), "{text}");
    assert_eq!(outcome.messages[0]["role"], "user");
    assert_eq!(outcome.messages[0]["context"], "summary");
    // The pending user message survives: the model still has the question.
    assert_eq!(
        outcome.messages.last().unwrap(),
        &expected_user("now the bass")
    );

    let serialized = Value::Array(outcome.messages.clone()).to_string();
    for forbidden in [
        "tool_calls",
        "provider_state",
        "thought_signature",
        "signature",
        "encrypted_content",
        "extra_content",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "'{forbidden}' survived compaction: {serialized}"
        );
    }
    // The record states what it dropped and how the estimate moved. It is
    // appended after the turns it summarizes, which stay on disk.
    let dir = store.root().join(&session.workspace_key).join(&session.id);
    let contents = std::fs::read_to_string(dir.join(MESSAGES_FILE)).unwrap();
    let records: Vec<Value> = contents
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let record = records
        .iter()
        .find(|record| record["kind"] == "compaction")
        .expect("a compaction record");
    if std::env::var_os("OCP_SESSIONS_KEEP").is_some() {
        eprintln!("COMPACTION_RECORD {record}");
    }
    assert!(
        records.iter().any(|record| record["kind"] == "turn"),
        "the turns before the summary must stay on disk"
    );
    let (original, compactions, _) = read_original(&dir).unwrap();
    assert_eq!(compactions, 1);
    assert!(
        original.len() > outcome.messages.len(),
        "the original history is larger than the compacted view"
    );
    assert_eq!(record["kind"], "compaction");
    assert_eq!(record["dropped"]["from"], 0);
    assert_eq!(record["dropped"]["to"], outcome.dropped_to);
    assert_eq!(record["tokens_before"], outcome.tokens_before);
    assert_eq!(record["tokens_after"], outcome.tokens_after);
    assert_eq!(store.load(&session.id).unwrap().messages, outcome.messages);
    assert!(store.load(&session.id).unwrap().meta.compacted_at.is_some());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn compaction_keeps_a_local_audio_reference_from_folded_history() {
    let reference = json!({
        "kind": "audio", "name": "song.wav", "path": "/tmp/song.wav", "mime": "audio/wav",
        "bytes": crate::ai::media::MAX_FILE_BYTES + 1, "duration_secs": 100.0,
        "sha256": "a".repeat(64), "local_reference": true,
    });
    let view = [
        json!({"role":"user", "content":"Use this as the reference", "media":[reference]}),
        json!({"role":"assistant", "content":"A detailed listening report. ".repeat(1000)}),
        json!({"role":"user", "content":"Continue"}),
        json!({"role":"assistant", "content":"Continuing."}),
    ];
    let outcome = plan_compaction(&view, 0, &ProjectFacts::default(), "fixture").unwrap();
    assert_eq!(outcome.messages[0]["media"][0], reference);
    let mut replay = outcome.messages;
    bound_media_replay(&mut replay, &[]);
    assert_eq!(replay[0]["media"][0], reference);
}

#[test]
fn a_compacted_session_keeps_real_user_prose_and_bounds_auditions() {
    let (store, project) = fixture("auditions-bounded");
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("listen"),
            None,
        )
        .unwrap();
    let context = facts(&project);
    let descriptor = |index: usize| {
        json!({
            "kind": "audio",
            "name": format!("audition-{index}.wav"),
            "path": format!("/tmp/audition-media/audition-{index}.wav"),
            "mime": "audio/wav",
            "bytes": 1_000 + index as u64,
            "duration_secs": 8.0,
            "sha256": format!("{:064x}", index + 1),
        })
    };
    let mut view = vec![user("make the kick punchier")];
    for index in 0..34usize {
        view.push(tool_call(
            "audition_range",
            &format!("{{\"start_bar\":{},\"end_bar\":{}}}", index + 1, index + 2),
            &format!("sig-{index}"),
        ));
        view.push(tool_result(
            "audition_range",
            &json!({"ok": true, "range": index}).to_string(),
        ));
        view.push(json!({
            "role": "user",
            "content": if index == 33 {
                "A/B reference excerpt. Verified playback match.".into()
            } else {
                format!("You asked to hear {index}–{} s: this is that render.", index + 1)
            },
            "context": "audition",
            "media": [descriptor(index)],
        }));
        if index == 32 {
            view.last_mut().unwrap()["source_render"] = json!(
                store
                    .root()
                    .join("audition-renders")
                    .join(format!("{}.wav", "a".repeat(64)))
            );
        }
        if index == 33 {
            view.push(json!({
                "role": "user",
                "content": "A/B candidate excerpt. Verified playback match.",
                "context": "audition",
                "media": [descriptor(34)],
            }));
        }
        if index % 7 == 0 {
            view.push(user(&format!("real ask {index}")));
        }
        view.push(reply(&format!("round {index} done")));
    }
    view.push(user("now the bass"));
    let dir = store.find(&session.id).unwrap().0;
    // A version-one archive predates explicit message metadata. Reading it
    // migrates the saved labels once; new writers never infer these kinds.
    let mut legacy = turn_record("2026-11-03T08:00:00Z", view);
    for message in &mut legacy.messages {
        message.as_object_mut().unwrap().remove("message_meta");
    }
    let _ = append_records(&dir, &[legacy]).unwrap();

    let outcome = store.compact(&session.id, None, &context).unwrap();
    assert!(strips_replay_state(&outcome.messages));
    assert!(outcome.retained_pairs <= RETAINED_TOOL_PAIRS);

    // The summary is marked as host context; it is not a user turn.
    let summaries: Vec<&Value> = outcome
        .messages
        .iter()
        .filter(|message| message["context"] == CONTEXT_SUMMARY)
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["role"], "user");

    // Regular auditions share a row per round; A and B keep their labels.
    let auditions: Vec<&Value> = outcome
        .messages
        .iter()
        .filter(|message| message["context"] == CONTEXT_AUDITION)
        .collect();
    assert!(!auditions.is_empty());
    assert!(
        auditions.len() <= RETAINED_TOOL_PAIRS + 1,
        "{} audition rows",
        auditions.len()
    );
    for row in &auditions {
        assert_eq!(row["role"], "user");
        assert!(
            row["media"]
                .as_array()
                .is_some_and(|media| !media.is_empty())
        );
        assert!(row["content"].as_str().is_some_and(|content| {
            content.starts_with("You asked to hear ") || content.starts_with("A/B ")
        }));
    }
    let paired = retained_comparison_pair(&outcome.messages).unwrap();
    assert_eq!(paired[0]["media"][0]["name"], "audition-33.wav");
    assert_eq!(paired[1]["media"][0]["name"], "audition-34.wav");
    assert!(
        auditions
            .iter()
            .any(|row| audition_render_reference(row).is_some())
    );

    // Every other user row is a real turn the person wrote.
    let prose: Vec<&str> = outcome
        .messages
        .iter()
        .filter(|message| message["role"] == "user" && message.get("context").is_none())
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert!(!prose.is_empty());
    for content in &prose {
        assert!(
            content.starts_with("real ask ") || *content == "now the bass",
            "{content}"
        );
    }

    let summary = summaries[0]["content"].as_str().unwrap();
    assert!(
        summary.contains("Discarded audition deliveries: 28 clips"),
        "{summary}"
    );
    assert!(
        summary.contains("Audio kept on disk: 7 clip(s)"),
        "{summary}"
    );
    assert!(
        !summary.contains("Earlier user asks:\n- \"You asked to hear"),
        "{summary}"
    );
    let serialized = Value::Array(outcome.messages.clone()).to_string();
    assert!(!serialized.contains("tool_calls"), "{serialized}");
    assert!(!serialized.contains("provider_state"), "{serialized}");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

/// The operator's stored session, copied outside the repository. Run it
/// with `OCP_CTX_ROWS_SESSION=<copy>/messages.jsonl --ignored`.
#[test]
#[ignore = "needs OCP_CTX_ROWS_SESSION=<copy of messages.jsonl>"]
fn the_operator_session_compacts_to_bounded_context_and_real_prose() {
    let path = std::env::var("OCP_CTX_ROWS_SESSION")
        .expect("set OCP_CTX_ROWS_SESSION to the copied messages.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut view = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: Record = serde_json::from_str(line).unwrap();
        if record.kind == RecordKind::Turn {
            view.extend(record.messages);
        }
    }
    canonicalize_media(&mut view);
    let outcome = plan_compaction(&view, 0, &ProjectFacts::default(), "operator").unwrap();
    assert!(strips_replay_state(&outcome.messages));

    let summaries: Vec<&Value> = outcome
        .messages
        .iter()
        .filter(|message| message["context"] == CONTEXT_SUMMARY)
        .collect();
    assert_eq!(summaries.len(), 1);
    let summary = summaries[0]["content"].as_str().unwrap();
    assert!(
        summary.contains("Discarded audition deliveries: "),
        "{summary}"
    );

    let auditions = outcome
        .messages
        .iter()
        .filter(|message| message["context"] == CONTEXT_AUDITION)
        .count();
    assert!(
        auditions <= RETAINED_TOOL_PAIRS,
        "{auditions} audition rows"
    );

    for message in &outcome.messages {
        if message["role"] != "user" || message.get("context").is_some() {
            continue;
        }
        let content = message["content"].as_str().unwrap_or_default();
        assert!(
            !content.starts_with("You asked to hear ") && !is_round_nudge(content),
            "{content}"
        );
    }
    let serialized = Value::Array(outcome.messages.clone()).to_string();
    assert!(!serialized.contains("tool_calls"), "{serialized}");
    assert!(!serialized.contains("provider_state"), "{serialized}");
}

#[test]
fn renaming_a_session_marks_it_named() {
    let (store, project) = fixture("named");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    assert!(!session.named);
    let renamed = store.rename(&session.id, "Punchier kick").unwrap();
    assert!(renamed.named);
    assert_eq!(renamed.title, "Punchier kick");
    assert!(store.load(&session.id).unwrap().meta.named);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_session_permission_round_trips_through_its_metadata() {
    let (store, project) = fixture("permission-roundtrip");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    assert_eq!(session.permission, SessionPermission::Write);

    let saved = store
        .set_permission(&session.id, SessionPermission::Autoreview)
        .unwrap();
    assert_eq!(saved.permission, SessionPermission::Autoreview);
    assert_eq!(
        store.load(&session.id).unwrap().meta.permission,
        SessionPermission::Autoreview
    );
    // The list carries the same value, and the file spells it on the wire.
    assert_eq!(
        store
            .list(Scope::Workspace(Some(&workspace(&project))))
            .unwrap()[0]
            .permission,
        SessionPermission::Autoreview
    );
    let dir = store.find(&session.id).unwrap().0;
    let record: Value =
        serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).unwrap()).unwrap();
    assert_eq!(record["permission"], "autoreview");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn an_unknown_session_permission_is_refused() {
    // The wire boundary is the enum: a value outside the three modes does
    // not deserialize, so it cannot reach the store.
    for value in ["everything", "", "WRITE", "autoreviews"] {
        assert!(
            serde_json::from_value::<SessionPermission>(json!(value)).is_err(),
            "'{value}' was accepted"
        );
    }
    for (permission, wire) in [
        (SessionPermission::RequireApproval, "require_approval"),
        (SessionPermission::Autoreview, "autoreview"),
        (SessionPermission::Write, "write"),
    ] {
        assert_eq!(
            serde_json::from_value::<SessionPermission>(json!(wire)).unwrap(),
            permission
        );
        assert_eq!(serde_json::to_value(permission).unwrap(), json!(wire));
    }
}

#[test]
fn a_session_saved_before_the_permission_field_reads_the_default() {
    let (store, project) = fixture("permission-legacy");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let dir = store.find(&session.id).unwrap().0;
    // A metadata file written by a build that had no permission field.
    let mut record: Value =
        serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("permission");
    std::fs::write(
        dir.join(META_FILE),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        store.load(&session.id).unwrap().meta.permission,
        SessionPermission::Write
    );
}

#[test]
fn two_sessions_keep_independent_permissions() {
    let (store, project) = fixture("permission-independent");
    let place = workspace(&project);
    let first = store
        .create(Some(&place), Some(&project), Some("first"), None)
        .unwrap();
    let second = store
        .create(Some(&place), Some(&project), Some("second"), None)
        .unwrap();
    store
        .set_permission(&first.id, SessionPermission::RequireApproval)
        .unwrap();
    store
        .set_permission(&second.id, SessionPermission::Write)
        .unwrap();

    // A reopened store reads the same two values, one per session.
    let reopened = Store::new(store.root().to_owned());
    assert_eq!(
        reopened.load(&first.id).unwrap().meta.permission,
        SessionPermission::RequireApproval
    );
    assert_eq!(
        reopened.load(&second.id).unwrap().meta.permission,
        SessionPermission::Write
    );
    // Changing one leaves the other alone.
    reopened
        .set_permission(&second.id, SessionPermission::Autoreview)
        .unwrap();
    assert_eq!(
        reopened.load(&first.id).unwrap().meta.permission,
        SessionPermission::RequireApproval
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_saved_snapshot_survives_a_reload_and_a_sweep() {
    let (store, project) = fixture("saved-snapshot");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let (dir, _) = store.find(&session.id).unwrap();
    let kept_sha = store
        .snapshot_dsl(&session.id, "@project \"kept\"\n")
        .unwrap();
    let orphan_sha = store
        .snapshot_dsl(&session.id, "@project \"orphan\"\n")
        .unwrap();
    let history = vec![json!({
        "role": "user",
        "content": "keep this take",
        "dsl_sha": kept_sha,
    })];
    store
        .prepare(
            &session.id,
            &history,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 1_000_000,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();

    // A reopened conversation carries the snapshot its row reverts to.
    let loaded = store.load(&session.id).unwrap();
    let saved = loaded
        .messages
        .iter()
        .find(|message| message["content"] == "keep this take")
        .expect("the user message was saved");
    assert_eq!(saved["dsl_sha"], json!(kept_sha));

    // The sweep reads the saved messages, so a saved snapshot is a reference
    // and an unreferenced version is still collected.
    assert_eq!(store.collect_unreferenced().unwrap(), 1);
    assert!(
        dir.join("versions")
            .join(format!("{kept_sha}.ocp"))
            .is_file()
    );
    assert!(
        !dir.join("versions")
            .join(format!("{orphan_sha}.ocp"))
            .is_file()
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

fn add_padded_wav_metadata(wav: &mut Vec<u8>) {
    wav.splice(12..12, *b"JUNK\x03\0\0\0abc\0");
    let riff_size = (wav.len() as u32 - 8).to_le_bytes();
    wav[4..8].copy_from_slice(&riff_size);
}

#[test]
fn a_stereo_render_is_archived_beside_the_clip_it_produced() {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    use base64::Engine as _;
    let (store, _project) = fixture("audition-stereo");
    let root = store.root().to_path_buf();
    std::fs::create_dir_all(root.join("audition-media")).unwrap();

    // A 48 kHz stereo render, as the audition path writes it.
    let frames = 4_800usize;
    let mut pcm = Vec::with_capacity(frames * 4);
    for frame in 0..frames {
        let value = ((frame as f32) * 0.05).sin();
        let sample = (value * 12_000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    add_padded_wav_metadata(&mut wav);
    let source = root.join("stereo-source.wav");
    std::fs::write(&source, &wav).unwrap();

    // The delivered clip is the mono 16 kHz copy; the message carries the
    // render it came from.
    let mono: Vec<f32> = (0..1_600).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let clip = oc_engine::wav::mono_wav_bytes(&mono, 16_000).unwrap();
    let mut messages = vec![json!({
        "role": "user",
        "content": "You asked to hear 0-8 s: this is that render.",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
            "data": base64::engine::general_purpose::STANDARD.encode(&clip),
            "format": "wav",
            "source_path": source.display().to_string(),
        }
    })];
    let evicted = persist_auditions(
        &root,
        &mut messages,
        &SessionAudioSettings {
            format: SessionAudioFormat::Wav,
            mp3_kbps: 128,
        },
    )
    .unwrap();
    assert!(evicted.is_empty(), "a small archive evicts nothing");
    // The delivery is the mono 16 kHz clip the model heard, byte for byte.
    let media = messages[0]["media"].as_array().expect("archived media");
    assert_eq!(media.len(), 1, "one delivery attachment");
    let path = media[0]["path"].as_str().expect("media path");
    assert_eq!(
        std::fs::read(path).unwrap(),
        clip,
        "the wire points at the clip's own bytes"
    );
    let decoded = oc_engine::audio_clip::decode(Path::new(path)).unwrap();
    assert_eq!(
        decoded.sample_rate, 16_000,
        "the delivery keeps the clip's rate"
    );
    // The full-rate render stays on disk for the operator, and no request
    // resolves it: it is recorded as a path, not as a media descriptor.
    let render = messages[0]["source_render"]
        .as_str()
        .expect("the operator's render is recorded");
    assert!(render.contains("audition-renders"), "{render}");
    let render = oc_engine::audio_clip::decode(Path::new(render)).unwrap();
    assert_eq!(
        render.sample_rate, 44_100,
        "the render archive is a 44.1 kHz file"
    );
    let _ = std::fs::remove_file(&source);
    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

/// A 120 s stereo render far above the inline limit, delivered as the
/// bounded mono 16 kHz clip the listening tool produced.
#[test]
fn an_audition_replays_its_bounded_clip_not_the_full_rate_render() {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    use base64::Engine as _;
    let (store, project) = fixture("audition-bound");
    let root = store.root().to_path_buf();
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let context = facts(&project);
    let settings = SessionAudioSettings {
        format: SessionAudioFormat::Wav,
        mp3_kbps: 128,
    };

    // 120 s of 44.1 kHz stereo: 21 MB, well over the 12 MiB inline limit.
    let frames = 120 * 44_100;
    let mut pcm = Vec::with_capacity(frames * 4);
    for frame in 0..frames {
        let sample = (((frame as f32) * 0.001).sin() * 9_000.0) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&44_100u32.to_le_bytes());
    wav.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    assert!(wav.len() as u64 > crate::ai::media::MAX_FILE_BYTES);
    let source = place.join("render.wav");
    std::fs::write(&source, &wav).unwrap();

    // What the listening tool sent: the first 30 s of that window, mono 16 kHz.
    let mono: Vec<f32> = (0..30 * 16_000)
        .map(|index| (index as f32 * 0.001).sin() * 0.3)
        .collect();
    let clip = oc_engine::wav::mono_wav_bytes(&mono, 16_000).unwrap();
    let mut messages = vec![json!({
        "role": "user",
        "content": "You asked to hear 0–120 s: this is that render.",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
            "data": base64::engine::general_purpose::STANDARD.encode(&clip),
            "format": "wav",
            "source_path": source.display().to_string(),
        }
    })];
    let evicted = persist_auditions(&root, &mut messages, &settings).unwrap();
    assert!(evicted.is_empty(), "a small archive evicts nothing");

    // The archive keeps both: the bounded delivery a request reads, and the
    // full-rate render only the operator opens.
    let descriptor = messages[0]["media"][0].clone();
    assert_eq!(
        std::fs::read(descriptor["path"].as_str().unwrap()).unwrap(),
        clip,
        "the wire points at the clip's own bytes"
    );
    assert!(
        descriptor["bytes"].as_u64().unwrap() <= crate::ai::media::MAX_FILE_BYTES,
        "the delivery is within the inline bound: {descriptor}"
    );
    let render = messages[0]["source_render"]
        .as_str()
        .expect("the operator's render is recorded");
    assert!(render.contains("audition-renders"), "{render}");
    assert!(std::fs::metadata(render).unwrap().len() > crate::ai::media::MAX_FILE_BYTES);
    assert_eq!(
        std::fs::read(render).unwrap(),
        wav,
        "archive preserves the original WAV bytes"
    );

    // Save it, then reload: the session is what a later request reads.
    store
        .prepare(
            &session.id,
            &messages,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let reopened = Store::new(root.clone());
    let loaded = reopened.load(&session.id).unwrap();
    let saved = loaded
        .messages
        .iter()
        .find(|message| message["context"] == CONTEXT_AUDITION)
        .expect("the audition is saved");
    assert!(
        saved.get("input_audio").is_none(),
        "the inline bytes are replaced by the archive"
    );
    assert_eq!(saved["media"][0], descriptor);
    assert_eq!(saved["source_render"], json!(render));
    let saved_index = loaded
        .messages
        .iter()
        .position(|message| message == saved)
        .unwrap();
    let retained = retained_media_row(&loaded.messages, saved_index, saved_index + 1).unwrap();
    assert_eq!(retained["source_render"], json!(render));

    // The request a later turn builds carries the clip's bytes and none of
    // the render's.
    let mut outbound = loaded.messages.clone();
    describe_audition_renders(&root, &mut outbound);
    let mut request = ai::ProviderRequest {
        system: None,
        turns: ai::turns_from_wire(&Value::Array(outbound)).unwrap(),
        tools: Vec::new(),
        max_tokens: 1024,
        temperature: 0.0,
        thinking_level: "medium".into(),
        stream: false,
    };
    let notices = ai::media::resolve(&mut request).unwrap();
    assert!(notices.is_empty(), "the archived clip did not change");
    assert!(request.turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
        matches!(block, ai::Block::Text(text) if text.contains("audition_clip_source") && text.contains(render))
    }));
    let data = request
        .turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .find_map(|block| match block {
            ai::Block::Media { data, .. } => Some(data),
            _ => None,
        })
        .expect("the delivery is sent");
    let ai::media::MediaData::Inline { base64, source } = data else {
        panic!("the delivery was not resolved");
    };
    let body = base64::engine::general_purpose::STANDARD
        .decode(base64)
        .unwrap();
    assert!(body.len() as u64 <= crate::ai::media::MAX_FILE_BYTES);
    assert_eq!(body, clip, "the request carries the clip, not the render");
    assert_eq!(source.bytes, clip.len() as u64);

    // The clip's own bounds: mono, 16 kHz, 30 s. The render's 120 s and two
    // channels never reach the wire.
    let find = |needle: &[u8]| body.windows(4).position(|window| window == needle);
    let fmt = find(b"fmt ").expect("a format chunk");
    let channels = u16::from_le_bytes([body[fmt + 10], body[fmt + 11]]);
    let rate = u32::from_le_bytes(body[fmt + 12..fmt + 16].try_into().unwrap());
    let data = find(b"data").expect("a data chunk");
    let data_len = u32::from_le_bytes(body[data + 4..data + 8].try_into().unwrap());
    let seconds = f64::from(data_len) / f64::from(rate) / f64::from(channels) / 2.0;
    assert_eq!(channels, 1, "the delivery stays mono");
    assert_eq!(rate, 16_000, "the delivery stays at the clip rate");
    assert!(
        (seconds - 30.0).abs() < 0.01,
        "the clip's duration, not the render's: {seconds} s"
    );

    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

/// A sparse file of `bytes`: the budget reads its length, and writing 300
/// MiB of real bytes would make the test slower than the sweep it checks.
fn sparse(path: &Path, bytes: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(path).unwrap().set_len(bytes).unwrap();
}

/// A short valid WAV a turn can inline as its audition clip.
fn audition_clip() -> String {
    let samples: Vec<f32> = (0..1_600)
        .map(|index| (index as f32 * 0.05).sin() * 0.2)
        .collect();
    let wav = oc_engine::wav::mono_wav_bytes(&samples, 16_000).unwrap();
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(&wav)
}

#[test]
fn audition_collection_waits_for_transcript_publication_in_another_process() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CHILD_ROOT: &str = "OCP_TEST_AUDITION_COLLECT_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) {
        let error = private_fs::lock(
            &root.join(".sessions.lock"),
            private_fs::LockMode::Exclusive,
            private_fs::LockWait::NoWait,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        std::fs::write(root.join("collector-ready"), []).unwrap();
        assert_eq!(Store::new(root).collect_unreferenced().unwrap(), 1);
        return;
    }

    let store = Store::new(oc_audio::private_fs::test_root().join("audition-collector-lock"));
    let session = store.create(None, None, None, None).unwrap();
    let (dir, _) = store.find(&session.id).unwrap();
    let lock = store.mutation_lock().unwrap();
    let mut messages = vec![json!({
        "role": "user", "content": "The audition",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {"data": audition_clip(), "format": "wav"},
    })];
    persist_auditions(store.root(), &mut messages, &Default::default()).unwrap();
    let clip = std::fs::read_dir(clip_dir(store.root()))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let orphan = render_dir(store.root()).join(format!("{}.wav", "a".repeat(64)));
    sparse(&orphan, 32);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ai::sessions::tests::audition_collection_waits_for_transcript_publication_in_another_process",
        ])
        .env(CHILD_ROOT, store.root())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let ready = store.root().join("collector-ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.is_file() && child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let published = ready.is_file() && clip.is_file();
    let result = append_records(&dir, &[turn_record(&now(), messages)]);
    drop(lock);
    if !published {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    let _ = result.unwrap();
    assert!(published && status.success());
    assert!(clip.is_file());
    assert!(!orphan.exists());
}

#[test]
fn a_truncated_wav_does_not_evict_for_the_declared_frame_count() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let source = root.join("source.wav");
    let rate = 48_000;
    let frames = 20_000;
    let mut bytes = oc_engine::wav::pcm_wav_bytes(&vec![0.3; frames * 2], rate, 2).unwrap();
    bytes.truncate(bytes.len() - 4);
    std::fs::write(&source, bytes).unwrap();
    let decoded = oc_engine::audio_clip::decode(&source).unwrap();
    assert!(decoded.frames() < frames);
    let quality = oc_engine::resample::ResampleQuality::DEFAULT_RENDER;
    let resampled = oc_engine::resample::resample(&decoded.samples, rate, 44_100, quality).unwrap();
    let expected = oc_engine::wav::pcm_wav_bytes(&resampled, 44_100, 2).unwrap();

    let old = render_dir(root).join(format!("{}.wav", "a".repeat(64)));
    sparse(&old, AUDITION_ARCHIVE_BUDGET - expected.len() as u64);
    let store = Store::new(root.to_path_buf());
    let _lock = store.mutation_lock().unwrap();
    let archived = ArchiveSweep::new(root)
        .render(&source, &Default::default())
        .unwrap();
    assert_eq!(std::fs::read(archived).unwrap(), expected);
    assert!(
        old.is_file(),
        "the real conversion fits beside the existing archive"
    );
}

#[test]
fn audition_archive_budgets_the_resampled_wav_size() {
    let root = oc_audio::private_fs::test_root().join("audition-wav-budget");
    let old = render_dir(&root).join(format!("{}.wav", "b".repeat(64)));
    sparse(&old, AUDITION_ARCHIVE_BUDGET - 9_000);
    let source = root.join("source.wav");
    let bytes = oc_engine::wav::pcm_wav_bytes(&vec![0.0; 3_200], 16_000, 2).unwrap();
    assert!(bytes.len() < 9_000);
    std::fs::write(&source, bytes).unwrap();
    let store = Store::new(root.clone());
    let _lock = store.mutation_lock().unwrap();
    let output = ArchiveSweep::new(&root)
        .render(&source, &Default::default())
        .unwrap();
    assert!(output.metadata().unwrap().len() > 9_000);
    assert!(!old.exists());
    assert!(archived_bytes(&root) <= AUDITION_ARCHIVE_BUDGET);
}

#[test]
fn the_sweep_collects_audition_renders_and_keeps_referenced_files() {
    let (store, project) = fixture("audition-renders-swept");
    let root = store.root().to_path_buf();
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("archive"),
            None,
        )
        .unwrap();
    let (dir, _) = store.find(&session.id).unwrap();
    let kept_clip = "a".repeat(64);
    let kept_render = "b".repeat(64);
    write_records(
        &dir,
        &[turn_record(
            &now(),
            vec![json!({
                "role": "user",
                "content": format!("clip {kept_clip}, render {kept_render}"),
                "context": "audition",
                "source_render": root
                    .join("audition-renders")
                    .join(format!("{kept_render}.wav"))
                    .display()
                    .to_string(),
            })],
        )],
    )
    .unwrap();
    for (dir_name, hash, bytes) in [
        ("audition-media", kept_clip.clone(), 4_096),
        ("audition-renders", kept_render.clone(), 8_192),
        ("audition-media", "c".repeat(64), 100 * 1024 * 1024),
        ("audition-renders", "d".repeat(64), 200 * 1024 * 1024),
    ] {
        sparse(&root.join(dir_name).join(format!("{hash}.wav")), bytes);
    }

    let before = audition_archive(&root);
    assert_eq!(before.media.files, 2);
    assert_eq!(before.renders.files, 2);
    assert!(
        before.media.bytes + before.renders.bytes > before.budget_bytes,
        "the fixture is over budget"
    );

    // Only the two files no message names are collected: a render is swept
    // exactly like a clip, and both referenced files survive.
    assert_eq!(store.collect_unreferenced().unwrap(), 2);
    let after = audition_archive(&root);
    assert_eq!(after.media.files, 1);
    assert_eq!(after.renders.files, 1);
    assert!(after.media.bytes + after.renders.bytes < after.budget_bytes);
    assert!(
        root.join("audition-media")
            .join(format!("{kept_clip}.wav"))
            .is_file()
    );
    assert!(
        root.join("audition-renders")
            .join(format!("{kept_render}.wav"))
            .is_file()
    );
    assert!(
        !root
            .join("audition-media")
            .join(format!("{}.wav", "c".repeat(64)))
            .exists()
    );
    assert!(
        !root
            .join("audition-renders")
            .join(format!("{}.wav", "d".repeat(64)))
            .exists()
    );

    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn a_full_archive_evicts_the_oldest_unreferenced_file_for_a_new_audition() {
    let (store, project) = fixture("audition-archive-full");
    let root = store.root().to_path_buf();
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("full"),
            None,
        )
        .unwrap();
    let (dir, _) = store.find(&session.id).unwrap();
    // A clip a live conversation names must survive the eviction.
    let kept_clip = "a".repeat(64);
    write_records(
        &dir,
        &[turn_record(
            &now(),
            vec![json!({
                "role": "user",
                "content": format!("listen to {kept_clip}"),
                "context": "audition",
            })],
        )],
    )
    .unwrap();
    sparse(
        &root.join("audition-media").join(format!("{kept_clip}.wav")),
        1_024,
    );
    // The oldest file in the archive is an unreferenced render; 200 MiB of
    // unreferenced clips follow it, so the archive is full and both
    // directories count against the one budget.
    let oldest_render = "f".repeat(64);
    sparse(
        &root
            .join("audition-renders")
            .join(format!("{oldest_render}.wav")),
        100 * 1024 * 1024,
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    sparse(
        &root
            .join("audition-media")
            .join(format!("{}.wav", "b".repeat(64))),
        100 * 1024 * 1024,
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    sparse(
        &root
            .join("audition-media")
            .join(format!("{}.wav", "c".repeat(64))),
        100 * 1024 * 1024,
    );

    let incoming = vec![json!({
        "role": "user",
        "content": "listen",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": { "data": audition_clip(), "format": "wav" },
    })];
    let prepared = store
        .prepare(
            &session.id,
            &incoming,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 1_000_000,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .expect("a full archive does not refuse the turn");

    // The turn result names what it removed, oldest first.
    assert_eq!(
        prepared.evicted_auditions.len(),
        1,
        "{:?}",
        prepared.evicted_auditions
    );
    assert_eq!(prepared.evicted_auditions[0].dir, "audition-renders");
    assert_eq!(
        prepared.evicted_auditions[0].file,
        format!("{oldest_render}.wav")
    );
    assert!(
        !root
            .join("audition-renders")
            .join(format!("{oldest_render}.wav"))
            .exists()
    );
    assert!(
        root.join("audition-media")
            .join(format!("{kept_clip}.wav"))
            .is_file()
    );

    // The new clip is archived, and the saved turn still carries it.
    let saved = prepared.messages.last().expect("the turn is saved");
    assert_eq!(saved["context"], "audition");
    let path = saved["media"][0]["path"]
        .as_str()
        .expect("the delivery names its clip");
    assert!(Path::new(path).is_file(), "{path}");

    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn an_audition_render_reference_survives_its_file_and_reports_missing() {
    let (store, project) = fixture("audition-render-missing");
    let root = store.root().to_path_buf();
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("gone"),
            None,
        )
        .unwrap();
    let (dir, _) = store.find(&session.id).unwrap();
    let render = root
        .join("audition-renders")
        .join(format!("{}.wav", "e".repeat(64)));
    std::fs::create_dir_all(render.parent().unwrap()).unwrap();
    std::fs::write(&render, vec![0u8; 128]).unwrap();
    write_records(
        &dir,
        &[turn_record(
            &now(),
            vec![json!({
                "role": "user",
                "content": "listen",
                "context": "audition",
                "source_render": render.display().to_string(),
            })],
        )],
    )
    .unwrap();

    // While the render is there the row carries no missing flag.
    let loaded = store.load(&session.id).unwrap();
    assert!(loaded.messages[0].get("source_render_missing").is_none());

    std::fs::remove_file(&render).unwrap();
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(
        loaded.messages[0]["source_render"],
        json!(render.display().to_string()),
        "the descriptor keeps naming the render"
    );
    assert_eq!(loaded.messages[0]["source_render_missing"], json!(true));

    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn clearing_the_audition_archive_removes_both_directories() {
    let (store, _project) = fixture("audition-clear");
    let root = store.root().to_path_buf();
    for (dir_name, hash) in [
        ("audition-media", "a".repeat(64)),
        ("audition-renders", "b".repeat(64)),
    ] {
        sparse(&root.join(dir_name).join(format!("{hash}.wav")), 1_024);
    }

    assert_eq!(store.clear_audition_archive().unwrap(), 2);
    let archive = audition_archive(&root);
    assert_eq!(archive.media.files, 0);
    assert_eq!(archive.renders.files, 0);
    assert_eq!(archive.budget_bytes, AUDITION_ARCHIVE_BUDGET);

    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn an_mp3_bitrate_is_clamped_to_the_rate_table() {
    // MPEG-2 (24 kHz) writes at most 160 kbps; MPEG-1 carries 192.
    assert_eq!(mp3_bitrate(24_000, 192), 160);
    assert_eq!(mp3_bitrate(24_000, 320), 160);
    assert_eq!(mp3_bitrate(48_000, 192), 192);
    assert_eq!(mp3_bitrate(48_000, 64), 64);
}

#[test]
fn missing_full_audition_render_keeps_the_excerpt_and_records_the_warning() {
    let (store, project) = fixture("audition-missing-full");
    let root = store.root().to_path_buf();
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), None, None)
        .unwrap();
    let incoming = vec![json!({
        "role": "user",
        "content": "You asked to hear 0–1 s: this is that render.",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
            "data": audition_clip(), "format": "wav",
            "source_path": place.join("missing.wav").to_string_lossy()
        }
    })];
    let prepared = store
        .prepare(
            &session.id,
            &incoming,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let loaded = Store::new(root.clone()).load(&session.id).unwrap();
    assert_eq!(prepared.saved_auditions, loaded.messages);
    assert!(prepared.saved_auditions[0].get("input_audio").is_none());
    assert_eq!(loaded.messages[0]["source_render_unavailable"], true);
    let excerpt = loaded.messages[0]["media"][0]["path"].as_str().unwrap();
    assert!(Path::new(excerpt).is_file());
    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn a_repeated_audition_caption_keeps_distinct_audio_turns() {
    use base64::Engine as _;
    let (store, project) = fixture("same-audition-caption");
    let session = store.create(None, None, None, None).unwrap();
    let context = facts(&project);
    let audition = |sample| {
        let raw = oc_engine::wav::mono_wav_bytes(&[sample; 256], 16_000).unwrap();
        json!({
            "role": "user",
            "content": "You asked to hear this range.",
            "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
                "format": "wav",
                "data": base64::engine::general_purpose::STANDARD.encode(raw),
            }
        })
    };
    for sample in [0.25, -0.25, -0.25] {
        store
            .prepare(
                &session.id,
                &[audition(sample)],
                None,
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    history: None,
                    overhead: 0,
                    context_length: 0,
                    max_output: 0,
                    prompt_tokens: 0,
                    facts: &context,
                    provider: "fixture",
                    model: "model",
                    outside: None,
                    surface: None,
                },
            )
            .unwrap();
    }
    let messages = store.load(&session.id).unwrap().messages;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["content"], messages[1]["content"]);
    let media_path = |message: &Value| PathBuf::from(message["media"][0]["path"].as_str().unwrap());
    let first = media_path(&messages[0]);
    let second = media_path(&messages[1]);
    assert_ne!(first, second);
    store.collect_unreferenced().unwrap();
    assert!(first.is_file() && second.is_file());
}

#[test]
fn archived_mp3_reuses_only_the_same_effective_encoding() {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.wav");
    let _lock = Store::new(root.path().to_path_buf())
        .mutation_lock()
        .unwrap();
    let mut archive = ArchiveSweep::new(root.path());
    let mut encode = |bitrate| {
        archive
            .render(
                &source,
                &SessionAudioSettings {
                    format: SessionAudioFormat::Mp3,
                    mp3_kbps: bitrate,
                },
            )
            .unwrap()
    };
    for rate in [48_000, 24_000] {
        let pcm = oc_engine::wav::pcm_wav_bytes(&[0.25; 8192], rate, 2).unwrap();
        std::fs::write(&source, pcm).unwrap();
        let compact = encode(128);
        let full = encode(256);
        assert_ne!(compact, full);
        assert_eq!(compact, encode(128));
        assert_eq!(full, encode(if rate == 48_000 { 265 } else { 320 }));
        assert_eq!(full.extension().unwrap(), "mp3");
        assert!(full.metadata().unwrap().len() > compact.metadata().unwrap().len());
        let bytes = std::fs::read(full).unwrap();
        let table = oc_engine::encode::mp3::Mp3Version::for_sample_rate(rate)
            .unwrap()
            .bitrates();
        let header_index = usize::from(bytes[2] >> 4);
        assert_eq!(
            table[header_index - 1],
            if rate == 48_000 { 256 } else { 160 }
        );
    }
}

#[test]
fn a_raw_mp3_fallback_does_not_override_the_wav_archive() {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("loud.wav");
    oc_engine::wav::write_stereo_f32_wav(&source, &[2.0; 8192], 48_000).unwrap();
    let _lock = Store::new(root.path().to_path_buf())
        .mutation_lock()
        .unwrap();
    let mut archive = ArchiveSweep::new(root.path());
    let raw = archive
        .render(
            &source,
            &SessionAudioSettings {
                format: SessionAudioFormat::Mp3,
                mp3_kbps: 192,
            },
        )
        .unwrap();
    assert_eq!(raw.extension().unwrap(), "wav");
    assert_eq!(
        std::fs::read(&raw).unwrap(),
        std::fs::read(&source).unwrap()
    );
    let wav = archive.render(&source, &Default::default()).unwrap();
    assert_ne!(raw, wav);
    assert_eq!(
        oc_engine::wav::open_reader(&wav)
            .unwrap()
            .spec()
            .sample_rate,
        44_100
    );
}

#[test]
fn mp3_render_preference_preserves_the_exact_audition_for_reopened_turns() {
    for source_rate in [48_000, 96_000] {
        verify_reopened_mp3_audition(source_rate);
    }
}

fn verify_reopened_mp3_audition(source_rate: u32) {
    use crate::gui_settings::{SessionAudioFormat, SessionAudioSettings};
    use base64::Engine as _;
    let (store, project) = fixture(&format!("audition-container-{source_rate}"));
    let root = store.root().to_path_buf();
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), None, None)
        .unwrap();
    let settings = SessionAudioSettings {
        format: SessionAudioFormat::Mp3,
        mp3_kbps: 192,
    };

    let stereo: Vec<f32> = (0..24_000)
        .flat_map(|frame| [(frame as f32 * 0.05).sin() * 0.3, 0.1])
        .collect();
    let clip = oc_engine::wav::pcm_wav_bytes(&stereo, 24_000, 2).unwrap();
    let render = place.join("render.wav");
    let mut full_rate = oc_engine::wav::pcm_wav_bytes(&stereo, source_rate, 2).unwrap();
    add_padded_wav_metadata(&mut full_rate);
    std::fs::write(&render, &full_rate).unwrap();

    let mut messages = vec![json!({
        "role": "user",
        "content": "You asked to hear 0–1 s: this is that render.",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {
            "data": base64::engine::general_purpose::STANDARD.encode(&clip),
            "format": "wav",
            "source_path": render.to_string_lossy(),
        }
    })];
    persist_auditions(&root, &mut messages, &settings).unwrap();
    let media_path = Path::new(messages[0]["media"][0]["path"].as_str().unwrap());
    assert_eq!(media_path.extension().unwrap(), "wav");
    assert_eq!(std::fs::read(media_path).unwrap(), clip);
    let archived_render = Path::new(messages[0]["source_render"].as_str().unwrap());
    assert_eq!(archived_render.extension().unwrap(), "mp3");
    let decoded = oc_engine::audio_clip::decode(archived_render).unwrap();
    assert_eq!(decoded.sample_rate, 48_000);
    assert_eq!(decoded.frames(), 24_000 * 48_000 / source_rate as usize);

    store
        .prepare(
            &session.id,
            &messages,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let loaded = Store::new(root.clone()).load(&session.id).unwrap();
    let mut request = ai::ProviderRequest {
        system: None,
        turns: ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap(),
        tools: Vec::new(),
        max_tokens: 1024,
        temperature: 0.0,
        thinking_level: "medium".into(),
        stream: false,
    };
    assert!(ai::media::resolve(&mut request).unwrap().is_empty());
    let data = request
        .turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .find_map(|block| match block {
            ai::Block::Media { data, .. } => Some(data),
            _ => None,
        })
        .unwrap();
    let ai::media::MediaData::Inline { base64, .. } = data else {
        panic!("the reopened audition must resolve to inline audio");
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(base64)
            .unwrap(),
        clip
    );
    std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[test]
fn drop_history_keeps_the_chosen_compaction_and_collects_orphans() {
    let (store, project) = fixture("drop-history");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let dir = store.find(&session.id).unwrap().0;
    let keep_sha = "a".repeat(64);
    let drop_sha = "b".repeat(64);
    let first = Record {
        v: VERSION,
        kind: RecordKind::Turn,
        at: now(),
        dropped: None,
        tokens_before: None,
        tokens_after: None,
        truncated: None,
        messages: vec![json!({"role":"user","content":"old","dsl_sha":drop_sha})],
    };
    let compaction = Record {
        v: VERSION,
        kind: RecordKind::Compaction,
        at: now(),
        dropped: Some(Dropped { from: 0, to: 1 }),
        tokens_before: Some(10),
        tokens_after: Some(2),
        truncated: None,
        messages: vec![json!({"role":"user","content":"summary","dsl_sha":keep_sha})],
    };
    let _ = append_records(&dir, &[first, compaction]).unwrap();

    // One referenced asset and one orphan, in each content store.
    std::fs::create_dir_all(store.root().join("audition-media")).unwrap();
    std::fs::write(
        store
            .root()
            .join("audition-media")
            .join(format!("{keep_sha}.wav")),
        b"x",
    )
    .unwrap();
    std::fs::write(
        store
            .root()
            .join("audition-media")
            .join(format!("{drop_sha}.wav")),
        b"x",
    )
    .unwrap();
    let versions = dir.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    std::fs::write(versions.join(format!("{keep_sha}.ocp")), b"@project\n").unwrap();
    std::fs::write(versions.join(format!("{drop_sha}.ocp")), b"@project\n").unwrap();

    let outcome = store.drop_history(&session.id, 1).unwrap();
    assert_eq!(outcome["kept_records"], 1);
    assert!(outcome["removed_assets"].as_u64().unwrap() >= 2);
    assert!(
        store
            .root()
            .join("audition-media")
            .join(format!("{keep_sha}.wav"))
            .is_file()
    );
    assert!(
        !store
            .root()
            .join("audition-media")
            .join(format!("{drop_sha}.wav"))
            .is_file()
    );
    assert!(versions.join(format!("{keep_sha}.ocp")).is_file());
    assert!(!versions.join(format!("{drop_sha}.ocp")).is_file());
    // Asking for a compaction that does not exist is a refusal, not a wipe.
    assert!(store.drop_history(&session.id, 2).is_err());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_dsl_snapshot_round_trips_and_refuses_a_foreign_id() {
    let (store, project) = fixture("dsl-snapshot");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let dsl = "@project\n  tempo: 120\n";
    let sha = store.snapshot_dsl(&session.id, dsl).unwrap();
    assert_eq!(store.dsl_version(&session.id, &sha).unwrap(), dsl);
    // The same document is one file, however many messages carry it.
    assert_eq!(store.snapshot_dsl(&session.id, dsl).unwrap(), sha);
    // A path-shaped or unknown id is refused rather than read.
    assert!(store.dsl_version(&session.id, "../../etc/passwd").is_err());
    assert!(store.dsl_version(&session.id, "not-a-hash").is_err());
    assert!(store.dsl_version(&session.id, &"0".repeat(64)).is_err());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[tokio::test]
async fn a_provider_that_rejects_a_replayed_signature_accepts_the_compacted_conversation() {
    const REJECTION: &str = r#"{"error":{"message":"Gemini 4+ requires the original thought signature on the first tool call of each step in the current turn; preserve tool_calls[].extra_content.google.thought_signature.","type":"invalid_request_error"}}"#;
    const ANSWER: &str = r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"Bass is in."},"finish_reason":"stop"}]}"#;
    let (store, project) = fixture("accepts");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let view = conversation();
    store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    // A provider that refuses a replayed signature, exactly as the wire does.
    let (base, server) = canned_by_request(2, |request| {
        if request.contains("thought_signature") {
            (400, REJECTION.to_string())
        } else {
            (200, ANSWER.to_string())
        }
    });
    let entry = AiProviderEntry {
        id: "signature-fixture".into(),
        name: "Signature fixture".into(),
        kind: ProviderKind::OpenAiChatCompletions,
        wire: Some(Wire::OpenAiChatCompletions),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: format!("{base}v1/chat/completions"),
        model: "google/gemini-4-argon".into(),
        models: Vec::new(),
        context_length: 32_000,
        max_tokens: 4_096,
        temperature: 0.3,
        thinking_level: "medium".into(),
        capabilities: AiCapabilities {
            text: true,
            streaming: false,
            native_tool_calling: true,
            ..Default::default()
        },
    };
    let send = |messages: Value| async {
        let (request, _) = request_from_wire(&entry, messages, false, None).unwrap();
        let provider =
            provider_for(&entry, Credential::ApiKey(Some("fixture-secret".into()))).unwrap();
        let mut emitted = false;
        let stats = ai::timing::TurnStatsHandle::new(&entry.id, &entry.model, entry.wire());
        ai::run(provider.as_ref(), &request, &None, &mut emitted, &stats).await
    };
    // The saved conversation still replays its tool call with the signature,
    // so this provider refuses it.
    let error = send(json!(store.view(&session.id).unwrap()))
        .await
        .unwrap_err();
    assert!(error.message.contains("thought signature"), "{error:?}");

    let outcome = store.compact(&session.id, None, &context).unwrap();
    let answer = send(json!(outcome.messages)).await.unwrap();
    assert_eq!(answer["content"], "Bass is in.");
    let requests = server.join().unwrap();
    let messages = |request: &str| {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: Value = serde_json::from_str(body).expect("the request body is JSON");
        body["messages"].to_string()
    };
    assert!(
        messages(&requests[0]).contains("thought_signature"),
        "the saved conversation replays the signature: {}",
        messages(&requests[0])
    );
    let compacted = messages(&requests[1]);
    for forbidden in [
        "thought_signature",
        "signature",
        "tool_calls",
        "provider_state",
    ] {
        assert!(
            !compacted.contains(forbidden),
            "the compacted request carried '{forbidden}': {compacted}"
        );
    }
    assert!(compacted.contains("Conversation summary"), "{compacted}");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn the_token_estimate_is_monotone() {
    let mut view = Vec::new();
    let mut previous = 0;
    for message in conversation() {
        view.push(message);
        let tokens = estimate_tokens(&view);
        assert!(tokens > previous, "{tokens} !> {previous}");
        previous = tokens;
    }
    assert_eq!(
        estimate_tokens(&[user("abcd")]),
        estimate_tokens(&[user("abcd")]),
        "the estimate is a function of the messages"
    );
    // Non-ASCII text costs more per character than ASCII.
    assert!(estimate_tokens(&[user("一二三四")]) > estimate_tokens(&[user("abcd")]));
}

#[test]
fn auto_compaction_runs_before_a_turn_over_the_threshold() {
    let (store, project) = fixture("auto");
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("long job"),
            None,
        )
        .unwrap();
    let context = facts(&project);
    let view = conversation();
    let before = estimate_tokens(&view);
    // A context window this conversation alone fills past the fraction.
    let window = before.max(1);
    let prepared = store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: window,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let compacted = prepared
        .compacted
        .expect("the turn compacts before sending");
    assert!(strips_replay_state(&prepared.messages));
    assert_eq!(prepared.messages, compacted.messages);
    assert!(compacted.tokens_after < compacted.tokens_before);
    assert!(prepared.meta.compacted_at.is_some());
    assert_eq!(prepared.meta.tokens, compacted.tokens_after);
    assert_eq!(
        prepared.messages.last().unwrap(),
        &expected_user("now the bass")
    );
    // A conversation under the threshold is left alone.
    let small = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("short"),
            None,
        )
        .unwrap();
    let prepared = store
        .prepare(
            &small.id,
            &[user("hello")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 1_000_000,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(prepared.compacted.is_none());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn an_oversized_session_stays_under_the_cap_and_says_what_it_dropped() {
    // The production cap is a gigabyte; the trimming path is exercised
    // against a small one so the test stays cheap.
    let (store, project) = fixture("cap");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let cap = 2 * 1024 * 1024u64;
    let filler = "x".repeat(100_000);
    let mut records = Vec::new();
    for index in 0..40 {
        records.push(Record {
            v: VERSION,
            kind: RecordKind::Turn,
            at: now(),
            dropped: None,
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages: vec![
                user(&format!("{index} {filler}")),
                reply(&format!("{index} {filler}")),
            ],
        });
    }
    let dir = store.find(&session.id).unwrap().0;
    let _ = append_records_under(&dir, &records, cap).unwrap();
    let size = std::fs::metadata(dir.join(MESSAGES_FILE)).unwrap().len();
    assert!(size <= cap, "{size} bytes");
    let (view, _) = read_view(&dir).unwrap();
    let note = view[0]["content"].as_str().unwrap_or_default();
    assert!(note.contains("dropped"), "{note}");
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_recorded_path_re_associates_a_moved_workspace() {
    let (store, project) = fixture("moved");
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("before the move"),
            None,
        )
        .unwrap();
    let place = workspace(&project);
    let moved = oc_test_support::temp_dir("oc-ai-sessions", "moved-workspace");
    std::fs::rename(place.join("song.ocp"), moved.join("song.ocp")).unwrap();
    assert_ne!(
        store.workspace_key(Some(&moved)),
        store.workspace_key(Some(&place)),
        "a different directory has a different key"
    );
    let key = store.reassociate(&place, &moved).unwrap();
    assert!(
        store
            .root()
            .join(&key)
            .join(&session.id)
            .join(META_FILE)
            .is_file()
    );
    let listed = store.list(Scope::Workspace(Some(&moved))).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, session.id);
    let record: Value = serde_json::from_slice(
        &std::fs::read(store.root().join(&key).join(WORKSPACE_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(record["paths"].as_array().unwrap().len(), 2);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_fork_copies_the_conversation_and_keeps_the_parent_id() {
    let (store, project) = fixture("fork");
    let parent = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("parent"),
            None,
        )
        .unwrap();
    let context = facts(&project);
    let mut first = user("first");
    first["history_at"] = json!({"project": "/songs/song.ocp", "level": 2, "at": 5});
    store
        .prepare(
            &parent.id,
            &[first],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    store
        .record_reply(&parent.id, &reply("answer"), &context, "p", "m")
        .unwrap();
    let fork = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            None,
            Some(Fork {
                parent: &parent.id,
                through: None,
            }),
        )
        .unwrap();
    assert_eq!(fork.parent.as_deref(), Some(parent.id.as_str()));
    assert_eq!(fork.title, "parent (fork)");
    let inherited = store.load(&fork.id).unwrap().messages;
    assert_eq!(inherited.len(), 2);
    assert_eq!(
        inherited[0]["history_at"],
        json!({"project": "/songs/song.ocp", "level": 2, "at": 5})
    );
    // The fork is independent: a new turn leaves the parent untouched.
    store
        .prepare(
            &fork.id,
            &[user("second")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(store.load(&parent.id).unwrap().messages.len(), 2);
    assert_eq!(store.load(&fork.id).unwrap().messages.len(), 3);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_goal_is_one_typed_row_that_a_fork_copies() {
    let (store, project) = fixture("goal-row");
    let place = workspace(&project);
    let parent = store
        .create(Some(&place), Some(&project), Some("parent"), None)
        .unwrap();
    let context = facts(&project);
    store
        .prepare(
            &parent.id,
            &[user("first")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    store
        .record_reply(&parent.id, &reply("on it"), &context, "p", "m")
        .unwrap();
    store
        .set_goal(&parent.id, Some("finish the bridge"), false)
        .unwrap();
    let view = store.view(&parent.id).unwrap();
    let (goal, _) = crate::ai::session_state::current_state(&view);
    assert_eq!(goal.unwrap().text, "finish the bridge");
    assert_eq!(
        view.iter()
            .filter(|row| message_protocol::state(row).is_some())
            .count(),
        1
    );

    let fork = store
        .create(
            Some(&place),
            Some(&project),
            None,
            Some(Fork {
                parent: &parent.id,
                through: None,
            }),
        )
        .unwrap();
    let (goal, _) = crate::ai::session_state::current_state(&store.view(&fork.id).unwrap());
    assert_eq!(goal.unwrap().text, "finish the bridge");

    // A cut before the goal row copies the conversation without it.
    let before_goal = store
        .create(
            Some(&place),
            Some(&project),
            None,
            Some(Fork {
                parent: &parent.id,
                through: Some(2),
            }),
        )
        .unwrap();
    let (goal, _) = crate::ai::session_state::current_state(&store.view(&before_goal.id).unwrap());
    assert!(goal.is_none());
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_goal_set_during_an_open_round_waits_for_the_next_turn() {
    let (store, project) = fixture("goal-pending");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("pending"), None)
        .unwrap();
    let context = facts(&project);
    store
        .prepare(
            &session.id,
            &[user("first")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    store
        .append_turn(
            &session.id,
            &[json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "open-1",
                    "type": "function",
                    "function": {"name": "tracks_list", "arguments": "{}"}
                }]
            })],
            None,
        )
        .unwrap();
    store.set_goal(&session.id, None, false).unwrap();
    assert!(
        store
            .view(&session.id)
            .unwrap()
            .iter()
            .all(|row| { message_protocol::state(row).is_none() }),
        "a state row may not follow an open round"
    );
    assert_eq!(
        store.find(&session.id).unwrap().1.pending_state,
        Some(SessionState::GoalCleared {
            author: StateAuthor::Person
        })
    );
    assert_eq!(
        store.open_page(&session.id, 10).unwrap().state,
        vec![SessionState::GoalCleared {
            author: StateAuthor::Person
        }],
        "a page carries the state the next turn will write"
    );
    // The next admission closes the open round and writes the state row
    // before the new message.
    store
        .prepare(
            &session.id,
            &[user("second")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "p",
                model: "m",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let (_, meta) = store.find(&session.id).unwrap();
    assert!(meta.pending_state.is_none());
    let view = store.view(&session.id).unwrap();
    let states: Vec<usize> = view
        .iter()
        .enumerate()
        .filter(|(_, row)| message_protocol::state(row).is_some())
        .map(|(index, _)| index)
        .collect();
    assert_eq!(states.len(), 1);
    let second = view
        .iter()
        .position(|row| row["content"] == "second")
        .unwrap();
    assert!(states[0] < second, "the state row leads the new message");
    assert_eq!(turns::check_pairing(&view), Ok(()));
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_fork_cut_is_trimmed_to_an_answered_turn() {
    let mut messages = vec![
        user("first"),
        serde_json::json!({
            "role": "assistant",
            "content": "Trying a gain cut.",
            "tool_calls": [{ "id": "call-1", "type": "function", "function": { "name": "x", "arguments": "{}" } }],
        }),
    ];
    trim_unanswered_call(&mut messages);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");

    // An answer ends the turn, so nothing is trimmed.
    let mut answered = vec![user("first"), reply("Done.")];
    trim_unanswered_call(&mut answered);
    assert_eq!(answered.len(), 2);
}

#[test]
fn a_fork_can_stop_at_a_chosen_turn() {
    let (store, project) = fixture("fork-through");
    let place = workspace(&project);
    let parent = store
        .create(Some(&place), Some(&project), Some("parent"), None)
        .unwrap();
    let context = facts(&project);
    for (question, answer) in [("first", "one"), ("second", "two")] {
        store
            .prepare(
                &parent.id,
                &[user(question)],
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    history: None,
                    overhead: 0,
                    context_length: 0,
                    max_output: 0,
                    prompt_tokens: 0,
                    facts: &context,
                    provider: "p",
                    model: "m",
                    outside: None,
                    surface: None,
                },
            )
            .unwrap();
        store
            .record_reply(&parent.id, &reply(answer), &context, "p", "m")
            .unwrap();
    }
    assert_eq!(store.view(&parent.id).unwrap().len(), 4);

    let stopped = store
        .create(
            Some(&place),
            Some(&project),
            None,
            Some(Fork {
                parent: &parent.id,
                through: Some(2),
            }),
        )
        .unwrap();
    assert_eq!(stopped.parent.as_deref(), Some(parent.id.as_str()));
    assert_eq!(store.load(&stopped.id).unwrap().messages.len(), 2);

    // A cut that names no message is refused instead of copying everything,
    // and the refusal leaves no session behind.
    for through in [0, 5] {
        let refused = store.create(
            Some(&place),
            Some(&project),
            None,
            Some(Fork {
                parent: &parent.id,
                through: Some(through),
            }),
        );
        assert!(refused.is_err(), "through {through} was accepted");
    }
    assert_eq!(store.list(Scope::Workspace(Some(&place))).unwrap().len(), 2);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_rejected_proposal_is_summarized_as_text_not_replayed() {
    let (store, project) = fixture("proposal");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("fill"), None)
        .unwrap();
    let context = facts(&project);
    let mut view = vec![
        user("add a fill before the chorus"),
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call-edit",
                "type": "function",
                "function": {"name": "edit_subtree", "arguments": "{\"path\":\"track:kick\",\"new_dsl\":\"@track kick\"}"},
                "extra_content": {"google": {"thought_signature": "sig-edit"}}
            }]
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call-edit",
            "content": "{\"status\":\"pending_user_confirmation\",\"applied\":false}"
        }),
        user("actually never mind"),
    ];
    for step in 0..12 {
        view.push(user(&format!(
            "listen to bars {step} to {} and tell me how the low end sits against the pad",
            step + 2
        )));
        view.push(reply(&format!(
            "Bars {step}-{}: the kick and the bass overlap under 200 Hz, so the pad loses its                  bottom octave and the mix reads heavy on small speakers. A high-pass on the pad                  at 180 Hz and 1 dB less on the bass bus would clear it.",
            step + 2
        )));
    }
    store
        .prepare(
            &session.id,
            &view,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let outcome = store.compact(&session.id, None, &context).unwrap();
    let text = outcome.messages[0]["content"].as_str().unwrap();
    assert!(
        text.contains("proposed a replacement for track:kick"),
        "{text}"
    );
    assert!(text.contains("not applied"), "{text}");
    assert!(strips_replay_state(&outcome.messages));
    let serialized = Value::Array(outcome.messages.clone()).to_string();
    for forbidden in ["tool_calls", "provider_state", "thought_signature"] {
        assert!(!serialized.contains(forbidden), "{forbidden}: {serialized}");
    }
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn restored_session_audio_keeps_latest_instruction_and_matching_tool_calls() {
    let (store, project) = fixture("restored-audio-prefix");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let context = facts(&project);
    let prepare = |messages: &[Value]| {
        store
            .prepare(
                &session.id,
                messages,
                Some(&project),
                crate::ai::sessions::PrepareContext {
                    snapshot: None,
                    history: None,
                    overhead: 0,
                    context_length: 0,
                    max_output: 0,
                    prompt_tokens: 0,
                    facts: &context,
                    provider: "fixture-provider",
                    model: "fixture-model",
                    outside: None,
                    surface: None,
                },
            )
            .unwrap()
    };
    prepare(&[user("Original setup instruction")]);
    store
        .record_reply(
            &session.id,
            &json!({"role":"assistant","content":"Setup finished"}),
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    prepare(&[user("An earlier question")]);
    store
        .record_reply(
            &session.id,
            &json!({"role":"assistant","content":"Earlier answer"}),
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    let latest = expected_user("ONLY listen now; do not edit or rediscover instruments");
    prepare(std::slice::from_ref(&latest));
    let calls = json!({"role":"assistant","content":null,"tool_calls":[
        {"id":"audio-a","type":"function","function":{"name":"audition_range","arguments":"{\"start_bar\":1,\"end_bar\":9}"}},
        {"id":"audio-b","type":"function","function":{"name":"audition_range","arguments":"{\"start_bar\":9,\"end_bar\":17}"}}
    ]});
    store
        .record_reply(
            &session.id,
            &calls,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    let incoming = vec![
        latest.clone(),
        calls,
        json!({"role":"tool","tool_call_id":"audio-a","content":"first rendered range"}),
        json!({"role":"tool","tool_call_id":"audio-b","content":"second rendered range"}),
        json!({"role":"user","content":"First clip", "message_meta": {"version":1,"kind":"audition"}, "input_audio":{"format":"wav","data":"UklGRg=="}}),
        json!({"role":"user","content":"Second clip", "message_meta": {"version":1,"kind":"audition"}, "input_audio":{"format":"wav","data":"UklGRg=="}}),
    ];
    let prepared = prepare(&incoming);
    let live = prepared.messages.clone();
    assert_eq!(live.len(), 10);
    assert_eq!(live[4], latest);
    assert_eq!(live[5]["tool_calls"][0]["id"], "audio-a");
    assert_eq!(live[6]["tool_call_id"], "audio-a");
    assert_eq!(live[7]["tool_call_id"], "audio-b");
    assert!(live[8]["media"][0]["path"].is_string());
    assert_eq!(live[8]["media"], live[9]["media"]);
    assert!(
        prepared
            .messages
            .iter()
            .all(|message| message.get("input_audio").is_none())
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn audition_audio_survives_session_reopen_compaction_and_reports_missing_files() {
    let (store, project) = fixture("audio");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let context = facts(&project);
    let clip = json!({
        "role": "user",
        "content": "You asked to hear 0-8 s: this is that render.",
        "message_meta": {"version":1,"kind":"audition"}, "input_audio": {"data": "UklGRg==", "format": "wav"}
    });
    // A listening round, so the clip belongs to a retained tool block and
    // compaction keeps one audio row for it.
    let call = tool_call(
        "audition_range",
        "{\"start_bar\":1,\"end_bar\":9}",
        "sig-listen",
    );
    let result = tool_result("audition_range", &"rendered range ".repeat(200));
    let turn = vec![
        user("render a window and listen"),
        call,
        result,
        clip.clone(),
    ];
    let prepared = store
        .prepare(
            &session.id,
            &turn,
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(prepared.live.is_none());
    assert!(
        prepared
            .messages
            .iter()
            .all(|message| message.get("input_audio").is_none())
    );
    let descriptor = prepared
        .messages
        .iter()
        .find_map(|message| message.get("media"))
        .unwrap()[0]
        .clone();
    let archived = prepared
        .messages
        .iter()
        .find(|message| message.get("media").is_some())
        .unwrap();
    assert_eq!(archived["context"], "audition");
    let audio_path = descriptor["path"].as_str().unwrap();

    assert_eq!(std::fs::read(audio_path).unwrap(), b"RIFF");
    let transcript =
        std::fs::read_to_string(store.find(&session.id).unwrap().0.join("messages.jsonl")).unwrap();
    assert!(!transcript.contains("UklGRg=="));
    assert!(!transcript.contains("audio clip not saved"));
    assert!(prepared.compacted.is_none());
    let next = store
        .prepare(
            &session.id,
            &[user("what did you hear?")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(next.live.is_none());
    assert!(
        next.messages
            .iter()
            .any(|message| message["media"][0] == descriptor)
    );
    let reopened = Store::new(store.root().to_owned());
    let loaded = reopened.load(&session.id).unwrap();
    assert!(
        loaded
            .messages
            .iter()
            .any(|message| message["media"][0] == descriptor)
    );
    reopened
        .record_reply(
            &session.id,
            &reply(&"heard ".repeat(2000)),
            &context,
            "p",
            "m",
        )
        .unwrap();
    reopened
        .record_reply(&session.id, &reply("Recent answer one"), &context, "p", "m")
        .unwrap();
    reopened
        .record_reply(&session.id, &reply("Recent answer two"), &context, "p", "m")
        .unwrap();
    let compacted = reopened.compact(&session.id, None, &context).unwrap();
    assert!(
        compacted
            .messages
            .iter()
            .any(|message| message["media"][0] == descriptor)
    );
    std::fs::remove_file(audio_path).unwrap();
    let missing = reopened.load(&session.id).unwrap();
    assert!(
        missing
            .messages
            .iter()
            .any(|message| message["media"][0]["missing"] == true)
    );

    assert!(
        next.messages
            .iter()
            .all(|message| message.get("input_audio").is_none())
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

/// A file the picker would accept, with bytes that need no decoder.
fn attachable(dir: &Path, name: &str) -> Value {
    let path = dir.join(name);
    std::fs::write(&path, b"attachment fixture bytes").unwrap();
    serde_json::to_value(crate::ai::media::attach(&path.to_string_lossy()).unwrap()).unwrap()
}

#[test]
fn a_user_message_round_trips_its_attachment_and_later_turns_replay_it() {
    let (store, project) = fixture("attachments");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let context = facts(&project);
    let descriptor = attachable(place.as_path(), "take.wav");
    assert_eq!(descriptor["kind"], "audio");
    let first = json!({"role":"user","content":"what do you hear?","media":[descriptor.clone()]});
    let prepared = store
        .prepare(
            &session.id,
            std::slice::from_ref(&first),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.appended, 1);

    // Store -> load returns the same seven fields, never the bytes.
    let loaded = store.load(&session.id).unwrap();
    let saved = &loaded.messages[0]["media"][0];
    for field in [
        "kind",
        "name",
        "path",
        "mime",
        "bytes",
        "duration_secs",
        "sha256",
    ] {
        assert_eq!(saved[field], descriptor[field], "{field}");
    }
    let raw = std::fs::read_to_string(
        store
            .root()
            .join(&session.workspace_key)
            .join(&session.id)
            .join(MESSAGES_FILE),
    )
    .unwrap();
    assert!(raw.contains("\"kind\":\"audio\""), "{raw}");
    assert!(!raw.contains("attachment fixture bytes"), "{raw}");

    // A later turn replays the saved message, descriptor included.
    let next = store
        .prepare(
            &session.id,
            &[user("and now?")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let replayed = next
        .messages
        .iter()
        .find(|message| message["content"] == "what do you hear?")
        .expect("the saved user message is replayed");
    assert_eq!(replayed["media"][0], descriptor);

    // The next turn's request re-reads the file: a changed clip is sent as
    // it is now and reported, and the untouched one is inlined verbatim.
    let replacement = b"a different take, edited on disk";
    std::fs::write(descriptor["path"].as_str().unwrap(), replacement).unwrap();
    let mut request = ai::ProviderRequest {
        system: None,
        turns: ai::turns_from_wire(&Value::Array(next.messages.clone())).unwrap(),
        tools: Vec::new(),
        max_tokens: 1024,
        temperature: 0.0,
        thinking_level: "medium".into(),
        stream: false,
    };
    let notices = ai::media::resolve(&mut request).unwrap();
    assert_eq!(notices.len(), 1);
    assert!(!notices[0].missing);
    let inline = request
        .turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .find_map(|block| match block {
            ai::Block::Media { data, .. } => Some(data),
            _ => None,
        })
        .expect("the replayed attachment is sent");
    let ai::media::MediaData::Inline { base64, .. } = inline else {
        panic!("the replayed attachment was not resolved");
    };
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(base64)
            .unwrap(),
        replacement.to_vec()
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_saved_attachment_for_a_deleted_file_loads_as_missing_and_is_not_sent() {
    let (store, project) = fixture("attachment-missing");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let context = facts(&project);
    let descriptor = attachable(place.as_path(), "gone.wav");
    let path = descriptor["path"].as_str().unwrap().to_owned();
    let first = json!({"role":"user","content":"what do you hear?","media":[descriptor]});
    store
        .prepare(
            &session.id,
            &[first],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    std::fs::remove_file(path).unwrap();

    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.messages[0]["media"][0]["missing"], Value::Bool(true));

    // The next request keeps the text and drops the block instead of failing.
    let next = store
        .prepare(
            &session.id,
            &[user("and now?")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let mut request = ai::ProviderRequest {
        system: None,
        turns: ai::turns_from_wire(&Value::Array(next.messages)).unwrap(),
        tools: Vec::new(),
        max_tokens: 1024,
        temperature: 0.0,
        thinking_level: "medium".into(),
        stream: false,
    };
    let notices = ai::media::resolve(&mut request).unwrap();
    assert!(
        notices
            .iter()
            .any(|notice| notice.missing && notice.name == "gone.wav")
    );
    assert!(
        request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .all(|block| !matches!(block, ai::Block::Media { .. })),
        "the missing attachment was not sent"
    );
    assert!(
        request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .any(|block| {
                matches!(block, ai::Block::Text(text) if text.contains("what do you hear?"))
            })
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn a_saved_descriptor_written_without_a_kind_is_repaired_on_read() {
    let (store, project) = fixture("legacy-media");
    let place = workspace(&project);
    let session = store
        .create(Some(&place), Some(&project), Some("listen"), None)
        .unwrap();
    let descriptor = attachable(place.as_path(), "take.wav");
    // An older build saved the six fields without the kind.
    let mut legacy = descriptor.clone();
    legacy.as_object_mut().unwrap().remove("kind");
    let message = json!({
        "role": "user",
        "content": "what do you hear?",
        "media": [legacy, json!({"name": "not an attachment"})],
    });
    let record = json!({
        "v": 1, "kind": "turn", "at": "2026-10-30T08:00:00.000Z", "messages": [message],
    });
    let dir = store.root().join(&session.workspace_key).join(&session.id);
    std::fs::write(dir.join(MESSAGES_FILE), format!("{}\n", record).as_bytes()).unwrap();

    // Reading repairs the kind from the MIME type and drops the entry that
    // cannot be a descriptor at all, so one bad record cannot fail a turn.
    let loaded = store.load(&session.id).unwrap();
    let media = loaded.messages[0]["media"].as_array().unwrap();
    assert_eq!(media.len(), 1, "{media:?}");
    assert_eq!(media[0]["kind"], "audio");
    assert_eq!(media[0]["path"], descriptor["path"]);

    // The repaired descriptor is what the next request replays.
    let prepared = store
        .prepare(
            &session.id,
            &[user("and now?")],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let turns = ai::turns_from_wire(&Value::Array(prepared.messages)).unwrap();
    assert!(
        turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .any(|block| matches!(block, ai::Block::Media { .. })),
        "the repaired attachment reaches the wire"
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

/// A window that reloads mid-turn stops the loop that owed the model its
/// tool results. The next turn pays every unpaid call with one honest
/// result, so the request never carries a call without an answer.
#[test]
fn a_round_the_window_left_unanswered_is_closed_before_the_next_turn() {
    let (store, project) = fixture("dangling-round");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let system = json!({ "role": "system", "content": "you are the panel" });
    let turn = json!([system, user("make it punchy")]);
    store
        .prepare(
            &session.id,
            turn.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    // The provider answered with a call the window never got to run.
    let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
    store
        .record_reply(
            &session.id,
            &call,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    assert_eq!(store.view(&session.id).unwrap().len(), 2);

    let next = json!([system, user("never mind, what changed?")]);
    let prepared = store
        .prepare(
            &session.id,
            next.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let messages = &prepared.messages;
    let closed = &messages[messages.len() - 2];
    assert_eq!(closed["role"], "tool");
    assert_eq!(closed["tool_call_id"], "call-tracks_mix");
    assert!(
        closed["content"].as_str().unwrap().contains("not run"),
        "a call that was never marked started reads as not run: {closed}"
    );
    assert_eq!(
        messages.last().unwrap()["content"],
        "never mind, what changed?"
    );

    // The closing is saved once, so a later turn does not repeat it.
    let saved = store.view(&session.id).unwrap();
    assert_eq!(
        saved
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
    let again = json!([system, user("and now?")]);
    let prepared = store
        .prepare(
            &session.id,
            again.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let closings = prepared
        .messages
        .iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("not run"))
        })
        .count();
    assert_eq!(closings, 1);
}

/// A provider that numbers calls per reply reuses an id in a later round.
/// A client that resends the earlier round's result does not answer the
/// open one, so the open one is closed and the turn goes out.
#[test]
fn a_resent_earlier_result_does_not_answer_an_open_round_with_the_same_id() {
    let (store, project) = fixture("reused-id-round");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let system = json!({ "role": "system", "content": "you are the panel" });
    let context_for = || crate::ai::sessions::PrepareContext {
        snapshot: None,
        history: None,
        overhead: 0,
        context_length: 0,
        max_output: 0,
        prompt_tokens: 0,
        facts: &context,
        provider: "fixture-provider",
        model: "fixture-model",
        outside: None,
        surface: None,
    };
    let ask = user("make it punchy");
    let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
    let result = tool_result("tracks_mix", "{\"ok\":true}");
    store
        .prepare(
            &session.id,
            &[system.clone(), ask.clone()],
            Some(&project),
            context_for(),
        )
        .unwrap();
    store
        .append_turn(&session.id, &[call.clone(), result.clone()], None)
        .unwrap();
    let again = user("once more");
    store
        .prepare(
            &session.id,
            &[system.clone(), again.clone()],
            Some(&project),
            context_for(),
        )
        .unwrap();
    // The next round reuses the id for another call, and the client dies
    // before its result.
    let reused = tool_call("tracks_mix", "{\"track\":\"snare\"}", "sig-1");
    store
        .record_reply(
            &session.id,
            &reused,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();

    let rebuilt = [system, ask, call, result, again, user("and now?")];
    let prepared = store
        .prepare(&session.id, &rebuilt, Some(&project), context_for())
        .expect("the open round is closed and the turn goes out");
    assert_eq!(turns::check_pairing(&prepared.messages), Ok(()));
    let closed = &prepared.messages[prepared.messages.len() - 2];
    assert_eq!(closed["role"], "tool");
    assert!(
        closed["content"].as_str().unwrap().contains("not run"),
        "{closed}"
    );
    assert_eq!(prepared.messages.last().unwrap()["content"], "and now?");
}

/// A round whose results all arrived is history, not a debt: nothing is
/// appended to it.
#[test]
fn a_round_that_got_its_results_is_left_alone() {
    let (store, project) = fixture("answered-round");
    let session = store
        .create(Some(&workspace(&project)), Some(&project), None, None)
        .unwrap();
    let context = facts(&project);
    let system = json!({ "role": "system", "content": "you are the panel" });
    let turn = json!([system, user("make it punchy")]);
    store
        .prepare(
            &session.id,
            turn.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let call = tool_call("tracks_mix", "{\"track\":\"kick\"}", "sig-0");
    store
        .record_reply(
            &session.id,
            &call,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    let round = json!([
        system,
        user("make it punchy"),
        call,
        tool_result("tracks_mix", "{\"ok\":true}")
    ]);
    let prepared = store
        .prepare(
            &session.id,
            round.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert_eq!(prepared.messages.len(), 3);

    let next = json!([system, user("now the bass")]);
    let prepared = store
        .prepare(
            &session.id,
            next.as_array().unwrap(),
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 0,
                max_output: 0,
                prompt_tokens: 0,
                facts: &context,
                provider: "fixture-provider",
                model: "fixture-model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(
        !prepared.messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("Not run"))
        }),
        "an answered round keeps its own results: {prepared:?}",
        prepared = prepared.messages
    );
}

/// A restored conversation keeps the signed reasoning of its assistant
/// turns, so the next round replays what the provider signed. A row saved
/// before the field existed still loads and builds an unsigned turn.
#[test]
fn a_saved_thinking_block_survives_restore_and_legacy_rows_still_build() {
    let (store, project) = fixture("thinking-replay");
    let session = store
        .create(
            Some(&workspace(&project)),
            Some(&project),
            Some("bass"),
            None,
        )
        .unwrap();
    let context = facts(&project);
    let signed = json!({
        "role": "assistant",
        "content": null,
        "thinking": [{"text": "Check the bass.", "signature": "sig-9"}],
        "tool_calls": [{
            "id": "call-bass",
            "type": "function",
            "function": {"name": "read_context", "arguments": "{\"path\":\"track:bass\"}"}
        }]
    });
    store
        .record_reply(
            &session.id,
            &signed,
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();

    let loaded = store.load(&session.id).unwrap();
    let saved = loaded
        .messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("the signed turn was saved");
    assert_eq!(
        saved["thinking"],
        json!([{"text": "Check the bass.", "signature": "sig-9"}])
    );
    let mut stripped = saved.clone();
    if let Value::Object(object) = &mut stripped {
        object.remove("thinking");
    }
    assert!(
        estimate_tokens(std::slice::from_ref(saved)) > estimate_tokens(&[stripped]),
        "the replayed reasoning counts toward the context estimate"
    );
    let turns = ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap();
    assert!(
        turns
            .iter()
            .any(|turn| turn.blocks.iter().any(|block| matches!(
                block,
                ai::Block::Thinking { text, signature: Some(signature), .. }
                    if text.as_str() == "Check the bass." && signature == "sig-9"
            ))),
        "the restored turn keeps its signature: {turns:?}"
    );

    // A conversation saved by a build that had no `thinking` field.
    store
        .record_reply(
            &session.id,
            &reply("Legacy answer"),
            &context,
            "fixture-provider",
            "fixture-model",
        )
        .unwrap();
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.skipped, 0);
    let turns = ai::turns_from_wire(&Value::Array(loaded.messages)).unwrap();
    assert!(turns.iter().any(|turn| {
        turn.blocks
            .iter()
            .any(|block| matches!(block, ai::Block::Text(text) if text.as_str() == "Legacy answer"))
    }));
    assert!(
        !turns.iter().any(|turn| {
            turn.blocks.iter().any(|block| {
                matches!(
                    block,
                    ai::Block::Thinking {
                        signature: None,
                        redacted: None,
                        ..
                    }
                )
            })
        }),
        "a legacy turn replays no unsigned reasoning block: {turns:?}"
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn new_notice_shaped_user_prose_stays_user_after_reopen() {
    let (store, project) = fixture("typed-user-prose");
    let session = store
        .create(None, Some(&project), Some("Goal"), None)
        .unwrap();
    let text = "Conversation summary written by the user.\nStanding intent: this is quoted text, not host metadata";
    store
        .prepare(
            &session.id,
            &[user(text)],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 1_000_000,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.messages[0]["message_meta"]["kind"], "user");
    assert_eq!(loaded.messages[0]["content"], text);
    let summary = summary_metadata(&loaded.messages, loaded.messages.len(), &[]);
    assert_eq!(
        summary.standing_intent.as_deref(),
        Some(collapse(text).as_str())
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn structured_summary_state_survives_nonstandard_prose_and_repeated_compaction() {
    let (store, project) = fixture("typed-summary-state");
    let mut row = json!({"role":"user", "context":"summary", "content":"Host summary body without English protocol headings"});
    let meta = MessageMeta::Summary(SummaryMeta {
        standing_intent: Some("Finish the editable arrangement".into()),
        discarded_auditions: Some(23),
        rollback_notice: Some("Host rollback outcome without a legacy prefix".into()),
        ..SummaryMeta::default()
    });
    message_protocol::tag(&mut row, meta);
    let view = vec![row, user("Continue")];
    let next_meta = summary_metadata(&view, view.len(), &[]);
    assert_eq!(
        next_meta.standing_intent.as_deref(),
        Some("Finish the editable arrangement")
    );
    assert_eq!(next_meta.discarded_auditions, Some(23));
    let body = summary_text(&view, view.len(), &[], &facts(&project), 1000, "now");
    assert!(body.contains("Host rollback outcome without a legacy prefix"));
    let mut next = json!({"role":"user","context":"summary","content":"Another display body"});
    message_protocol::tag(&mut next, MessageMeta::Summary(next_meta.clone()));
    assert_eq!(summary_metadata(&[next], 1, &[]), next_meta);
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn saved_message_metadata_preserves_replay_and_archive_fields() {
    let mut original = tool_call("project_get", "{}", "fixture-signed-thought");
    original["thinking"] = json!([{"text":"Exposed reasoning", "signature":"reasoning-signature"}]);
    let saved = saved_message(&original).unwrap();
    for key in ["tool_calls", "provider_state", "thinking"] {
        assert_eq!(saved[key], original[key]);
    }
    assert_eq!(saved["message_meta"]["kind"], "assistant");
    let raw = json!({"role":"user","context":"audition","content":"Listening delivery", "source_render":"/saved/render.wav"});
    let saved = saved_message(&raw).unwrap();
    assert_eq!(saved["source_render"], "/saved/render.wav");
    assert_eq!(saved["message_meta"]["kind"], "audition");
}

#[test]
fn a_person_row_keeps_a_well_formed_history_position_only() {
    let history = json!({"project": "/songs/song.ocp", "level": 3, "at": 9});
    let row = json!({
        "role": "user",
        "content": "tighten the low end",
        "message_meta": message_meta(MessageKind::User),
        "history_at": history,
    });
    let saved = saved_message(&row).unwrap();
    assert_eq!(saved["history_at"], history);
    assert!(crate::ai::is_local_message_field("history_at"));
    assert!(strips_replay_state(std::slice::from_ref(&saved)));
    assert!(crate::ai::message_extra(&saved).is_none());

    let assistant = json!({
        "role": "assistant",
        "content": "done",
        "history_at": history,
    });
    assert!(
        saved_message(&assistant)
            .unwrap()
            .get("history_at")
            .is_none(),
        "only a person's row records a position"
    );
    for bad in [
        json!({"project": "/songs/song.ocp", "at": 1}),
        json!({"project": "", "level": 0}),
        json!({"project": "/songs/song.ocp", "level": -1}),
        json!({"project": "/songs/song.ocp", "level": 1.5}),
        json!({"project": "/songs/song.ocp", "level": 0, "at": "nine"}),
    ] {
        let mut row = row.clone();
        row["history_at"] = bad;
        assert!(
            saved_message(&row).unwrap().get("history_at").is_none(),
            "a malformed position is dropped"
        );
    }
}

#[test]
fn a_saved_tool_row_keeps_local_changes_and_the_provider_never_reads_them() {
    let changes = json!({
        "changes": [{"kind": "tempo", "from": 120.0, "to": 132.0}],
        "more": 0,
    });
    let row = json!({
        "role": "tool",
        "tool_call_id": "call-1",
        "content": "{\"tempo\":132.0}",
        "changes": changes,
    });
    let saved = saved_message(&row).unwrap();
    assert_eq!(saved["changes"], changes);
    assert!(crate::ai::is_local_message_field("changes"));
    // The provider replay reads only the text, so no local fact travels.
    let turns = crate::ai::turns_from_wire(&json!([row])).unwrap();
    assert_eq!(
        turns[0].blocks,
        vec![crate::ai::Block::ToolResult {
            call_id: "call-1".into(),
            name: String::new(),
            content: "{\"tempo\":132.0}".into(),
            is_error: false,
        }]
    );
}

#[test]
fn comparison_retention_uses_kinds_instead_of_english_audio_labels() {
    let rows = [MessageKind::ComparisonReference, MessageKind::ComparisonCandidate]
        .map(|kind| {
            let mut row = json!({"role":"user","context":"audition", "content":"A localized audio label", "media":[{}]});
            message_protocol::tag(&mut row,MessageMeta::new(kind));
            row
        });
    assert!(retained_comparison_pair(&rows).is_some());
    let mut swapped = rows.clone();
    swapped.swap(0, 1);
    assert!(retained_comparison_pair(&swapped).is_none());
}

#[test]
fn standing_intent_keeps_a_users_reference_attachment_request() {
    let mut row = json!({"role":"user","content":"Recreate this song with editable instruments", "media":[{}]});
    message_protocol::tag_new(&mut row);
    assert_eq!(
        summary_metadata(&[row], 1, &[]).standing_intent.as_deref(),
        Some("Recreate this song with editable instruments")
    );
}

#[test]
fn future_message_envelopes_roundtrip_normal_session_persistence() {
    let (store, project) = fixture("future-message-envelope");
    let session = store
        .create(None, Some(&project), Some("Future message"), None)
        .unwrap();
    let mut assistant = tool_call("project_get", "{}", "signed-future-tool");
    assistant["thinking"] =
        json!([{"text":"Exposed future thought", "signature":"signed-future-thought"}]);
    assistant["message_meta"] =
        json!({"version":9,"kind":"future_assistant","opaque":{"keep":[1,"verbatim"]}});
    let mut user = json!({"role":"user", "content":"Conversation summary written by a person", "context":"audition", "source_render":"/saved/reference.wav", "message_meta":{"version":9,"kind":"future_user","new_field":true}, "media":[{"kind":"audio","path":"/saved/reference.wav","name":"reference.wav","mime":"audio/wav","bytes":44,"duration_secs":1.0,"sha256":"a".repeat(64)}]});
    user["media"] = canonical_media(&user["media"]);
    let saved = [
        saved_message(&user).unwrap(),
        saved_message(&assistant).unwrap(),
    ];
    let dir = store.find(&session.id).unwrap().0;
    let _ = append_records(&dir, &[turn_record("now", saved.to_vec())]).unwrap();
    let (loaded, _) = read_view(&dir).unwrap();
    assert_eq!(loaded, saved);
    assert_eq!(loaded[0]["message_meta"], user["message_meta"]);
    assert_eq!(loaded[1]["message_meta"], assistant["message_meta"]);
    for key in ["tool_calls", "thinking", "provider_state"] {
        assert_eq!(loaded[1][key], assistant[key]);
    }
    assert_eq!(loaded[0]["media"], user["media"]);
    assert_eq!(loaded[0]["source_render"], user["source_render"]);
    assert_eq!(message_protocol::kind(&loaded[0]), Some(MessageKind::User));
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn new_inline_user_reference_keeps_its_standing_intent() {
    let (store, project) = fixture("new-user-inline-reference");
    let session = store
        .create(None, Some(&project), Some("Reference"), None)
        .unwrap();
    let raw = json!({"role":"user","content":"Recreate the attached reference song", "input_audio":{"data":audition_clip(),"format":"wav"}});
    let prepared = store
        .prepare(
            &session.id,
            &[raw],
            Some(&project),
            crate::ai::sessions::PrepareContext {
                snapshot: None,
                history: None,
                overhead: 0,
                context_length: 1_000_000,
                max_output: 0,
                prompt_tokens: 0,
                facts: &facts(&project),
                provider: "fixture",
                model: "model",
                outside: None,
                surface: None,
            },
        )
        .unwrap();
    assert!(
        prepared.saved_auditions.is_empty(),
        "a user's reference is not a host listening delivery"
    );
    let loaded = store.load(&session.id).unwrap();
    assert_eq!(loaded.messages[0]["message_meta"]["kind"], "user");
    assert!(loaded.messages[0].get("context").is_none());
    assert_eq!(
        loaded.messages[0]["content"],
        "Recreate the attached reference song"
    );
    let path = loaded.messages[0]["media"][0]["path"].as_str().unwrap();
    assert!(Path::new(path).is_file(), "the reference audio stays saved");
    assert_eq!(
        summary_metadata(&loaded.messages, 1, &[])
            .standing_intent
            .as_deref(),
        Some("Recreate the attached reference song")
    );
    std::fs::remove_dir_all(store.root().parent().unwrap()).unwrap();
}

#[test]
fn repeated_user_reference_prose_keeps_distinct_audio_and_future_envelopes() {
    let mut first = expected_user("Use this reference");
    first["media"] = canonical_media(
        &json!([{"kind":"audio","name":"one.wav","path":"/one.wav","mime":"audio/wav","bytes":44,"sha256":"a".repeat(64)}]),
    );
    let mut second = first.clone();
    second["media"][0]["path"] = json!("/two.wav");
    assert!(!same_turn(&first, &second));
    assert!(same_turn(&first, &first.clone()));
    let mut future = first.clone();
    future["message_meta"] = json!({"version":9,"opaque":"first"});
    let mut changed = future.clone();
    changed["message_meta"]["opaque"] = json!("second");
    assert!(!same_turn(&future, &changed));
    assert!(same_turn(&future, &future.clone()));
}

#[test]
fn timestamps_are_rfc3339_utc() {
    assert_eq!(
        rfc3339_from(UNIX_EPOCH),
        "1970-01-01T00:00:00.000Z".to_string()
    );
    assert_eq!(
        rfc3339_from(UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_250)),
        "2023-11-14T22:13:20.250Z"
    );
}
