//! Cancellable, accounted summary requests with a bounded output allowance.

use super::*;

/// Writes one model summary for a compaction, accounted as its own request in
/// the same conversation. It reads no store state; the caller planned the
/// input under the store lock and commits the answer only if the view is
/// unchanged.
pub(crate) async fn compaction_summary<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    provider: AiProviderEntry,
    session_id: &str,
    input: String,
    cancel: Option<&ai::agent_loop::Cancel>,
) -> Result<String, ai::sessions::CompactionFailure> {
    use ai::session_usage::Purpose;
    if cancel.is_some_and(ai::agent_loop::Cancel::is_cancelled) {
        return Err(ai::sessions::CompactionFailure::Cancelled);
    }
    let p = provider;
    let stats = ai::timing::TurnStatsHandle::new(&p.id, &p.model, p.wire());
    let request = ai::session_usage::title_id()
        .map_err(|_| ai::sessions::CompactionFailure::ProviderError)?;
    let lease = ai::session_usage::SessionLease::admit(
        ai::sessions::Store::config(),
        session_id,
        &request,
        Purpose::Compaction,
    )
    .map_err(|_| ai::sessions::CompactionFailure::ProviderError)?;
    stats.set_accounting(lease);
    let outcome = summary_until_cancelled(
        send(
            p,
            TurnRequest {
                messages: compaction_request(input),
                use_tools: false,
                toolsets: None,
                pager: false,
                output_limit: Some(ai::sessions::SUMMARY_OUTPUT_TOKENS),
            },
            TurnChannels::default(),
            &stats,
        ),
        cancel,
    )
    .await;
    let _ = ai::session_usage::checkpoint_final(
        &stats,
        app,
        &request,
        ai::session_usage::Purpose::Compaction,
    )
    .await;
    outcome
}

/// Cancellation drops credential resolution or the in-flight HTTP request,
/// then returns to the caller so its accounting is finalized before release.
async fn summary_until_cancelled(
    request: impl Future<Output = Result<Value, AiError>>,
    cancel: Option<&ai::agent_loop::Cancel>,
) -> Result<String, ai::sessions::CompactionFailure> {
    let outcome = match cancel {
        Some(cancel) => tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ai::sessions::CompactionFailure::Cancelled),
            reply = request => reply,
        },
        None => request.await,
    };
    compaction_text(outcome)
}

/// The summary request body both the production path and its test send.
fn compaction_request(input: String) -> Value {
    json!([
        {"role": "system", "content": ai::compaction_prompt::COMPACTION_PROMPT},
        {"role": "user", "content": input},
    ])
}

/// The accounting and result handling shared by the credential-resolving path
/// and the explicit-credential test path.
fn compaction_text(
    outcome: Result<Value, AiError>,
) -> Result<String, ai::sessions::CompactionFailure> {
    let reply = outcome.map_err(|error| {
        if error.kind == "cancelled" {
            ai::sessions::CompactionFailure::Cancelled
        } else {
            ai::sessions::CompactionFailure::ProviderError
        }
    })?;
    let text = reply["content"].as_str().unwrap_or_default().trim();
    if text.is_empty() {
        Err(ai::sessions::CompactionFailure::Empty)
    } else {
        Ok(text.to_owned())
    }
}

/// Test path: one summary request over a canned server with a fixed credential,
/// so the request shape and accounting run without touching the OS key store.
#[cfg(test)]
pub(crate) async fn compaction_summary_with_credential(
    provider: AiProviderEntry,
    session_id: &str,
    input: String,
    credential: Credential,
    cancel: Option<&ai::agent_loop::Cancel>,
) -> Result<String, ai::sessions::CompactionFailure> {
    use ai::session_usage::Purpose;
    let stats = ai::timing::TurnStatsHandle::new(&provider.id, &provider.model, provider.wire());
    let request = ai::session_usage::title_id()
        .map_err(|_| ai::sessions::CompactionFailure::ProviderError)?;
    let lease = ai::session_usage::SessionLease::admit(
        ai::sessions::Store::config(),
        session_id,
        &request,
        Purpose::Compaction,
    )
    .map_err(|_| ai::sessions::CompactionFailure::ProviderError)?;
    stats.set_accounting(lease);
    let outcome = summary_until_cancelled(
        send_http(
            provider,
            TurnRequest {
                messages: compaction_request(input),
                use_tools: false,
                toolsets: None,
                pager: false,
                output_limit: Some(ai::sessions::SUMMARY_OUTPUT_TOKENS),
            },
            TurnChannels::default(),
            credential,
            |_| async { Err(AiError::config("unexpected refresh")) },
            &stats,
        ),
        cancel,
    )
    .await;
    let _ = ai::session_usage::checkpoint(&stats, true).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent_loop::Cancel;

    fn provider(base: String) -> AiProviderEntry {
        AiProviderEntry {
            id: "summary-fixture".into(),
            name: "Summary fixture".into(),
            kind: crate::commands::ProviderKind::OpenAiChatCompletions,
            wire: Some(crate::commands::Wire::OpenAiChatCompletions),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: format!("{base}v1/chat/completions"),
            model: "summary-fixture-model".into(),
            models: Vec::new(),
            context_length: 64_000,
            max_tokens: 32_000,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }

    #[test]
    fn cancellation_during_initial_compaction_closes_the_request_without_publishing() {
        use tauri::test::{mock_builder, mock_context, noop_assets};
        let root = tempfile::tempdir().unwrap();
        let store = ai::sessions::Store::new(root.path().join("sessions"));
        let _root = ai::sessions::test_root(store.root().to_path_buf());
        let session = store.create(None, None, None, None).unwrap();
        let mut older = Vec::new();
        for _ in 0..45 {
            older.push(json!({"role":"user","content":"keep working"}));
            older.push(json!({"role":"assistant","content":"old work ".repeat(500)}));
        }
        store.append_turn(&session.id, &older, None).unwrap();
        let lease = ai::session_usage::SessionLease::admit(
            ai::sessions::Store::new(store.root()),
            &session.id,
            "initial-turn",
            ai::session_usage::Purpose::Chat,
        )
        .unwrap();
        let incarnation = lease.update().incarnation;
        let app = mock_builder()
            .manage(crate::state::AppState::new())
            .build(mock_context(noop_assets()))
            .unwrap();
        let (base, ready, server) = ai::test_support::held_response();
        let p = provider(base);
        let cancel = Cancel::default();
        let incoming =
            json!([{"role":"system","content":"system"},{"role":"user","content":"continue"}]);
        let error = tauri::async_runtime::block_on(async {
            let operation = crate::commands::prepare_turn_using(
                app.handle(),
                &p,
                &incoming,
                crate::commands::SavedTurnBinding {
                    id: &session.id,
                    request_id: None,
                    incarnation: &incarnation,
                    project: None,
                    tool_tokens: 0,
                    surface: None,
                    cancel: Some(&cancel),
                },
                |input| {
                    compaction_summary_with_credential(
                        p.clone(),
                        &session.id,
                        input,
                        Credential::ApiKey(Some("fixture-secret".into())),
                        Some(&cancel),
                    )
                },
            );
            let stop = async {
                ready.await.unwrap();
                cancel.cancel();
            };
            let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(operation, stop)
            })
            .await
            .expect("Stop must interrupt the summary request");
            outcome.unwrap_err()
        });
        assert_eq!(error.kind(), "cancelled");
        assert!(
            server.join().unwrap(),
            "cancellation closes the provider connection"
        );
        let saved = store.load(&session.id).unwrap();
        assert!(!saved.compacted);
        assert_eq!(saved.messages.last().unwrap()["content"], "continue");
        assert!(saved.meta.compacted_at.is_none());
        assert_eq!(
            saved.usage.unwrap().pending_requests,
            1,
            "only the parent turn remains admitted"
        );
    }

    #[test]
    fn a_cancelled_summary_does_not_poll_the_provider() {
        let cancel = Cancel::default();
        cancel.cancel();
        let polled = std::sync::atomic::AtomicBool::new(false);
        let result = tauri::async_runtime::block_on(summary_until_cancelled(
            async {
                polled.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(json!({"content":"late"}))
            },
            Some(&cancel),
        ));
        assert_eq!(result, Err(ai::sessions::CompactionFailure::Cancelled));
        assert!(!polled.load(std::sync::atomic::Ordering::Relaxed));
    }
}
