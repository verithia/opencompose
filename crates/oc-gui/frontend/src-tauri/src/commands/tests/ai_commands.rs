//! The AI command layer's failure and edge paths: what each command refuses,
//! what it leaves behind, and what it must never print.
//!
//! The saved config and the per-model layer are process-global, so every test
//! redirects both at a fixture path. Take the config guard first, then the layer:
//! that order is the only one used here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ai::limits::{self, InputModalities, ModelLimits};
use crate::ai_service::{
    TestConfigPathGuard, ai_key_status, ai_remove_key, ai_set_key, test_config_path,
};
use crate::commands::ai::*;
use crate::commands::{
    AiProviderConfig, AiProviderDraft, AiProviderEntry, AuthKind, ProviderKind, Wire,
    ai_add_provider, ai_add_provider_with, ai_fetch_models, ai_model_limits, load_ai_config,
    save_ai_config, save_ai_config_with,
};

/// A value no error string, log line or file may ever carry.
const KEY: &str = "sk-fixture-SECRET-7f3a";

const BASE_YAML: &str = r#"version: 2
active_provider: fixture
providers:
  - id: fixture
    name: Fixture
    kind: openai_chat_completions
    auth: api_key
    endpoint: https://llm.acme.test/v1/chat/completions
    model: fixture-model
    max_tokens: 4096
    temperature: 0.3
"#;

struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    limits: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture_dir(label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "oc-ai-command-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Redirects both stores at a fresh directory, without writing either file.
fn fixture(label: &str) -> (Fixture, TestConfigPathGuard, limits::TestPathGuard) {
    let dir = fixture_dir(label);
    let config = dir.join("ai-providers.yaml");
    let limits = dir.join("ai").join("models.json");
    let config_guard = test_config_path(config.clone());
    let limits_guard = limits::test_path(limits.clone());
    (
        Fixture {
            dir,
            config,
            limits,
        },
        config_guard,
        limits_guard,
    )
}

fn config_at(label: &str, yaml: &str) -> (Fixture, TestConfigPathGuard, limits::TestPathGuard) {
    let (fixture, config_guard, limits_guard) = fixture(label);
    crate::private_fs::write_atomic(&fixture.config, yaml.as_bytes()).unwrap();
    (fixture, config_guard, limits_guard)
}

fn entry(id: &str) -> AiProviderEntry {
    AiProviderEntry {
        id: id.into(),
        name: id.into(),
        kind: ProviderKind::OpenAiChatCompletions,
        wire: Some(Wire::OpenAiChatCompletions),
        auth: AuthKind::ApiKey,
        auth_provider: None,
        oauth_allowed_hosts: Vec::new(),
        endpoint: "https://llm.acme.test/v1/chat/completions".into(),
        model: "fixture-model".into(),
        models: Vec::new(),
        context_length: 1_000_000,
        max_tokens: 16_000,
        temperature: 0.3,
        thinking_level: "medium".into(),
        capabilities: AiCapabilities::default(),
    }
}

fn config_of(providers: Vec<AiProviderEntry>) -> AiProviderConfig {
    AiProviderConfig {
        version: 2,
        active_provider: providers.first().map(|p| p.id.clone()).unwrap_or_default(),
        providers,
        warnings: Vec::new(),
    }
}

fn limits(context: u64, max_output: u64) -> ModelLimits {
    ModelLimits {
        context,
        max_output,
        input: InputModalities {
            text: true,
            ..Default::default()
        },
        price: None,
    }
}

fn draft(name: &str, models: &[&str]) -> AiProviderDraft {
    AiProviderDraft {
        name: name.into(),
        wire: Wire::OpenAiChatCompletions,
        endpoint: "https://llm.acme.test/v1/chat/completions".into(),
        auth: AuthKind::ApiKey,
        models: models.iter().map(|model| (*model).into()).collect(),
        limits: BTreeMap::new(),
    }
}

fn with_limits(mut draft: AiProviderDraft, model: &str, value: ModelLimits) -> AiProviderDraft {
    draft.limits.insert(model.to_string(), value);
    draft
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

// ============================================================================
// load_ai_config
// ============================================================================

#[test]
fn a_missing_file_loads_as_defaults_and_creates_nothing() {
    let (fixture, _config, _limits) = fixture("missing");
    let loaded = load_ai_config().unwrap();
    assert_eq!(loaded.version, 2);
    assert!(loaded.active_provider.is_empty());
    assert!(loaded.providers.is_empty());
    assert!(loaded.warnings.is_empty());
    assert!(!fixture.config.exists(), "a load must not create the file");
}

#[test]
fn a_corrupt_file_is_a_readable_error() {
    let (fixture, _config, _limits) = config_at("corrupt", "providers: [ {id: broken\n");
    let error = load_ai_config().unwrap_err();
    assert!(error.contains("Failed to parse YAML"), "{error}");
    assert!(!error.contains(KEY), "{error}");
    // The broken file is left exactly as the operator wrote it.
    assert_eq!(
        std::fs::read_to_string(&fixture.config).unwrap(),
        "providers: [ {id: broken\n"
    );
}

#[test]
fn a_file_with_no_providers_loads_and_the_next_save_refuses_it() {
    let (fixture, _config, _limits) = config_at(
        "no-providers",
        "version: 2\nactive_provider: ''\nproviders: []\n",
    );
    let loaded = load_ai_config().unwrap();
    assert!(loaded.providers.is_empty());
    assert!(loaded.warnings.is_empty());
    let before = bytes(&fixture.config);
    let error = save_ai_config(loaded).unwrap_err();
    assert!(error.contains("1–32"), "{error}");
    assert_eq!(bytes(&fixture.config), before);
}

#[test]
fn thirty_three_providers_load_and_the_save_refuses_them() {
    let providers = (0..33).map(|index| entry(&format!("p{index}"))).collect();
    let yaml = serde_yaml::to_string(&config_of(providers)).unwrap();
    let (fixture, _config, _limits) = config_at("33-providers", &yaml);
    let loaded = load_ai_config().unwrap();
    assert_eq!(loaded.providers.len(), 33);
    // The structural error is not a load failure: the file still opens so the
    // panel can show it, and the next save names the problem.
    assert!(loaded.warnings.is_empty());
    let before = bytes(&fixture.config);
    let error = save_ai_config(loaded).unwrap_err();
    assert!(error.contains("1–32"), "{error}");
    assert_eq!(bytes(&fixture.config), before);
}

#[test]
fn an_unknown_protocol_and_auth_load_as_defaults_and_save_healed() {
    let yaml = BASE_YAML.replace(
        "    kind: openai_chat_completions\n    auth: api_key\n",
        "    kind: some_future_protocol\n    auth: oauth_codex\n",
    );
    let (fixture, _config, _limits) = config_at("unknown-kind", &yaml);
    let loaded = load_ai_config().unwrap();
    assert_eq!(
        loaded.providers[0].kind,
        ProviderKind::OpenAiChatCompletions
    );
    assert_eq!(loaded.providers[0].auth, AuthKind::ApiKey);
    save_ai_config(loaded).unwrap();
    let text = std::fs::read_to_string(&fixture.config).unwrap();
    assert!(text.contains("kind: openai_chat_completions"), "{text}");
    assert!(text.contains("auth: api_key"), "{text}");
}

#[test]
fn an_invalid_entry_is_refused_without_touching_the_file() {
    let (fixture, _config, _limits) = config_at("invalid-entry", BASE_YAML);
    let before = bytes(&fixture.config);
    let mut hot = config_of(vec![entry("fixture")]);
    hot.providers[0].temperature = 3.0;
    let error = save_ai_config(hot).unwrap_err();
    // The message names the entry, so a form with twelve providers says which
    // one to open instead of "something in the settings is wrong".
    assert!(error.contains("\"fixture\""), "{error}");
    assert!(error.contains("temperature"), "{error}");
    assert_eq!(bytes(&fixture.config), before);
    // A duplicate id names the id that repeats.
    let error = save_ai_config(config_of(vec![entry("fixture"), entry("fixture")])).unwrap_err();
    assert!(error.contains("saved twice"), "{error}");
    assert!(error.contains("\"fixture\""), "{error}");
    assert_eq!(bytes(&fixture.config), before);
}

#[test]
fn a_save_leaves_one_complete_file_behind() {
    let (fixture, _config, _limits) = fixture("atomic");
    let config = config_of(vec![entry("fixture"), entry("second")]);
    let saved = save_ai_config(config).unwrap();
    assert_eq!(saved.path, fixture.config.display().to_string());
    // A replace-by-rename leaves no temporary file next to the config.
    let names: Vec<String> = std::fs::read_dir(&fixture.dir)
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["ai-providers.yaml"]);
    let reloaded = load_ai_config().unwrap();
    assert_eq!(reloaded.providers.len(), 2);
    assert_eq!(reloaded.active_provider, "fixture");
}

#[test]
fn two_saves_at_once_leave_one_complete_file() {
    let (fixture, _config, _limits) = fixture("concurrent");
    let first = config_of(vec![entry("fixture")]);
    let mut second = first.clone();
    second.providers[0].name = "Second Name".into();
    std::thread::scope(|scope| {
        for config in [first, second] {
            scope.spawn(move || {
                for _ in 0..10 {
                    save_ai_config(config.clone()).unwrap();
                }
            });
        }
    });
    // Whichever write landed last, the file is whole and parses.
    let reloaded = load_ai_config().unwrap();
    assert_eq!(reloaded.providers.len(), 1);
    assert!(["fixture", "Second Name"].contains(&reloaded.providers[0].name.as_str()));
    let names: Vec<String> = std::fs::read_dir(&fixture.dir)
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["ai-providers.yaml"]);
}

#[derive(Default)]
struct RecordingSink(std::cell::RefCell<Vec<String>>);

impl crate::ai::credential_lifecycle::CredentialSink for RecordingSink {
    fn delete(&self, service: &'static str, account: &str) -> Result<(), String> {
        self.0.borrow_mut().push(format!("{service}:{account}"));
        Ok(())
    }
}

struct FailingSink;

impl crate::ai::credential_lifecycle::CredentialSink for FailingSink {
    fn delete(&self, _service: &'static str, _account: &str) -> Result<(), String> {
        Err("locked".into())
    }
}

#[test]
fn a_failed_save_never_prunes_a_credential() {
    let (fixture, _config, _limits) = fixture("prune-order");
    let mut previous = config_of(vec![entry("gone"), entry("fixture")]);
    previous.active_provider = "fixture".into();
    save_ai_config_with(
        previous,
        crate::private_fs::write_atomic,
        &RecordingSink::default(),
    )
    .unwrap();
    let before = bytes(&fixture.config);
    // The next settings drop `gone` and cannot be written.
    let next = config_of(vec![entry("fixture")]);
    let sink = RecordingSink::default();
    let error = save_ai_config_with(
        next.clone(),
        |_, _| Err(std::io::Error::other("disk full")),
        &sink,
    )
    .unwrap_err();
    assert!(error.contains("existing settings were retained"), "{error}");
    assert!(
        sink.0.borrow().is_empty(),
        "a failed save must not delete the key of a provider the file still lists"
    );
    assert_eq!(bytes(&fixture.config), before);
    // The successful save prunes it, and the file is the one that dropped it.
    save_ai_config_with(next.clone(), crate::private_fs::write_atomic, &sink).unwrap();
    assert_eq!(sink.0.borrow().len(), 1);
    assert!(sink.0.borrow()[0].contains("gone"));
    assert_eq!(load_ai_config().unwrap().providers.len(), 1);
    // A prune that fails is a log line, not a lost save.
    let saved = save_ai_config_with(
        config_of(vec![entry("fixture")]),
        crate::private_fs::write_atomic,
        &FailingSink,
    )
    .unwrap();
    assert!(saved.path.ends_with("ai-providers.yaml"));
}

// ============================================================================
// ai_add_provider
// ============================================================================

#[test]
fn an_override_write_failure_leaves_no_provider_and_no_config_change() {
    let (fixture, _config, _limits) = config_at("override-write-fails", BASE_YAML);
    // A directory where the layer belongs makes every write to it fail.
    std::fs::create_dir_all(&fixture.limits).unwrap();
    let before = bytes(&fixture.config);
    let error = ai_add_provider(with_limits(
        draft("Override Fail", &["acme-large"]),
        "acme-large",
        limits(64_000, 8_192),
    ))
    .unwrap_err();
    assert!(error.contains("models.json"), "{error}");
    assert_eq!(bytes(&fixture.config), before);
    assert!(limits::get("override-fail", "acme-large").is_none());
    let reloaded = load_ai_config().unwrap();
    assert_eq!(reloaded.providers.len(), 1);
    assert_eq!(reloaded.active_provider, "fixture");
}

#[test]
fn a_provider_write_failure_rolls_the_override_back() {
    let (fixture, _config, _limits) = config_at("provider-write-fails", BASE_YAML);
    let before = bytes(&fixture.config);
    let error = ai_add_provider_with(
        with_limits(
            draft("Provider Fail", &["acme-large"]),
            "acme-large",
            limits(64_000, 8_192),
        ),
        |_| Err("the settings file is read-only".into()),
    )
    .unwrap_err();
    assert_eq!(error, "the settings file is read-only");
    assert_eq!(bytes(&fixture.config), before);
    assert!(limits::get("provider-fail", "acme-large").is_none());
    let saved = limits::parse(&std::fs::read_to_string(&fixture.limits).unwrap()).unwrap();
    assert!(!saved.providers.contains_key("provider-fail"));
}

#[test]
fn an_interrupted_add_still_loads_and_the_next_add_wins() {
    let (fixture, _config, _limits) = config_at("interrupted", BASE_YAML);
    // The process stopped between the two writes: a layer exists for a provider
    // the config does not name.
    limits::put("interrupted", "interrupted-model", limits(1_000, 100)).unwrap();
    let loaded = load_ai_config().unwrap();
    assert_eq!(loaded.providers.len(), 1);
    assert!(loaded.warnings.is_empty());
    // The merged catalog still resolves: the orphan layer has no provider to
    // attach to, so nothing is offered for it.
    let merged = crate::ai::catalog::merged();
    assert!(
        !merged
            .providers
            .iter()
            .any(|provider| provider.id == "interrupted")
    );
    // Adding the same name writes its own row and replaces the stale values.
    ai_add_provider(with_limits(
        draft("Interrupted", &["interrupted-model"]),
        "interrupted-model",
        limits(64_000, 8_192),
    ))
    .unwrap();
    let applied = limits::get("interrupted", "interrupted-model").unwrap();
    assert_eq!(applied.context, 64_000);
    assert_eq!(applied.max_output, 8_192);
    assert_eq!(load_ai_config().unwrap().active_provider, "interrupted");
    assert!(fixture.config.exists());
}

#[test]
fn the_thirty_third_provider_is_refused_and_leaves_no_layer() {
    let providers = (0..32).map(|index| entry(&format!("p{index}"))).collect();
    let yaml = serde_yaml::to_string(&config_of(providers)).unwrap();
    let (fixture, _config, _limits) = config_at("32-providers", &yaml);
    let before = bytes(&fixture.config);
    let error = ai_add_provider(with_limits(
        draft("Thirty Third", &["acme-large"]),
        "acme-large",
        limits(64_000, 8_192),
    ))
    .unwrap_err();
    assert!(error.contains("1–32"), "{error}");
    assert_eq!(bytes(&fixture.config), before);
    assert!(limits::get("thirty-third", "acme-large").is_none());
}

#[test]
fn the_command_suffixes_a_duplicate_name() {
    let (fixture, _config, _limits) = config_at("duplicate-name", BASE_YAML);
    let first = ai_add_provider(draft("Duplicate Name", &["acme-large"])).unwrap();
    let second = ai_add_provider(draft("Duplicate Name", &["acme-large"])).unwrap();
    assert_eq!(first.provider_id, "duplicate-name");
    assert_eq!(second.provider_id, "duplicate-name-2");
    assert_eq!(second.config.active_provider, "duplicate-name-2");
    let reloaded = load_ai_config().unwrap();
    assert_eq!(reloaded.providers.len(), 3);
    let names: Vec<&str> = reloaded
        .providers
        .iter()
        .map(|provider| provider.name.as_str())
        .collect();
    assert_eq!(names, ["Fixture", "Duplicate Name", "Duplicate Name"]);
    assert!(!bytes(&fixture.config).is_empty());
}

#[test]
fn a_rejected_draft_leaves_the_file_and_the_layer_untouched() {
    let (fixture, _config, _limits) = config_at("rejected-drafts", BASE_YAML);
    let before = bytes(&fixture.config);
    let mut too_many: Vec<String> = (0..33).map(|index| format!("m{index}")).collect();
    too_many.truncate(33);
    let models: Vec<&str> = too_many.iter().map(String::as_str).collect();
    let cases: Vec<(AiProviderDraft, &str)> = vec![
        (draft("", &["acme-large"]), "name"),
        (
            AiProviderDraft {
                auth: AuthKind::OAuth,
                ..draft("Acme", &["acme-large"])
            },
            "OAuth",
        ),
        (
            AiProviderDraft {
                endpoint: "http://llm.acme.test/v1".into(),
                ..draft("Acme", &["acme-large"])
            },
            "HTTPS",
        ),
        (
            AiProviderDraft {
                endpoint: "not a url".into(),
                ..draft("Acme", &["acme-large"])
            },
            "endpoint",
        ),
        (draft("Acme", &[]), "model id"),
        (draft("Acme", &models), "32"),
        (
            with_limits(draft("Acme", &["acme-large"]), "other", limits(1, 1)),
            "other",
        ),
    ];
    for (draft, needle) in cases {
        let error =
            ai_add_provider(with_limits(draft, "acme-large", limits(32_000, 4_096))).unwrap_err();
        assert!(
            error.to_lowercase().contains(&needle.to_lowercase()),
            "{needle}: {error}"
        );
        assert_eq!(bytes(&fixture.config), before, "{needle}");
    }
    // The layer is clean: no rejected draft left a row behind.
    for id in ["acme", "acme-2", "provider"] {
        assert!(
            !limits::overrides().providers.contains_key(id),
            "{id} left a layer behind"
        );
    }
}

// ============================================================================
// ai_model_limits
// ============================================================================

#[test]
fn the_limits_view_keeps_bundled_and_user_values_apart() {
    let (_fixture, _config, _limits) = fixture("limits-view");
    let bundled = ai_model_limits("openai".into(), "gpt-6.1-sol".into());
    assert!(bundled.bundled.is_some());
    assert!(bundled.overridden.is_none());
    limits::put("openai", "gpt-6.1-sol", limits(32_000, 4_000)).unwrap();
    let both = ai_model_limits("openai".into(), "gpt-6.1-sol".into());
    let bundled_value = both.bundled.as_ref().unwrap();
    let overridden = both.overridden.as_ref().unwrap();
    assert_eq!(overridden.context, 32_000);
    assert_eq!(overridden.max_output, 4_000);
    assert_ne!(bundled_value.context, overridden.context);
    // An unknown provider and model is an empty view, not an error.
    let unknown = ai_model_limits("no-such-provider".into(), "no-such-model".into());
    assert!(unknown.bundled.is_none());
    assert!(unknown.overridden.is_none());
    assert_eq!(unknown.provider_id, "no-such-provider");
    assert_eq!(unknown.model_id, "no-such-model");
    limits::remove("openai", "gpt-6.1-sol").unwrap();
}

#[test]
fn a_corrupt_config_does_not_stop_the_limits_view() {
    let (_fixture, _config, _limits) = config_at("limits-corrupt", "providers: [ {id: x\n");
    let view = ai_model_limits("openai".into(), "gpt-6.1-sol".into());
    assert!(view.bundled.is_some());
    assert!(view.overridden.is_none());
}

// ============================================================================
// ai_fetch_models
// ============================================================================

fn chat_endpoint(base: &str) -> String {
    format!("{base}v1/chat/completions")
}

#[tokio::test]
async fn fetching_refuses_oauth_before_opening_a_socket() {
    // Port 9 is closed: a network attempt would report a connection failure.
    let error = ai_fetch_models(
        "http://127.0.0.1:9/v1/chat/completions".into(),
        Wire::OpenAiChatCompletions,
        AuthKind::OAuth,
        Some(KEY.into()),
    )
    .await
    .unwrap_err();
    assert!(error.contains("OAuth is available only"), "{error}");
    assert!(!error.contains(KEY), "{error}");
}

#[tokio::test]
async fn every_listing_status_maps_to_one_actionable_message() {
    let cases: [(u16, &[&str]); 6] = [
        (400, &["400", "base URL", "protocol"]),
        (401, &["401", "API key"]),
        (403, &["403", "API key"]),
        (404, &["404", "base URL"]),
        (429, &["429", "Retry later"]),
        (500, &["500"]),
    ];
    for (status, needles) in cases {
        let (base, server) = crate::ai::test_support::canned(status, "the key must not appear");
        let error = ai_fetch_models(
            chat_endpoint(&base),
            Wire::OpenAiChatCompletions,
            AuthKind::ApiKey,
            Some(KEY.into()),
        )
        .await
        .unwrap_err();
        for needle in needles {
            assert!(error.contains(needle), "{status}: {needle}: {error}");
        }
        assert!(!error.contains(KEY), "{status}: {error}");
        server.join().unwrap();
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let target_url = format!("http://{}/v1/models", target.local_addr().unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buffer = [0u8; 4096];
        let _ = socket.read(&mut buffer);
        write!(
            socket,
            "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let error = ai_fetch_models(
        chat_endpoint(&base),
        Wire::OpenAiChatCompletions,
        AuthKind::ApiKey,
        Some(KEY.into()),
    )
    .await
    .unwrap_err();
    assert!(error.contains("302"), "{error}");
    assert!(!error.contains(KEY), "{error}");
    server.join().unwrap();
    // The listing request must not be replayed against the redirect target.
    match target.accept() {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
        Ok(_) => panic!("the redirect target was contacted"),
    }
}

#[tokio::test]
async fn a_local_endpoint_with_a_trailing_slash_lists_its_base() {
    let (base, server) = crate::ai::test_support::canned(200, r#"{"data":[{"id":"acme-large"}]}"#);
    let listing = ai_fetch_models(
        format!("{base}v1/chat/completions/"),
        Wire::OpenAiChatCompletions,
        AuthKind::ApiKey,
        None,
    )
    .await
    .unwrap();
    assert_eq!(listing.models[0].id, "acme-large");
    assert_eq!(listing.total, 1);
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /v1/models"), "{request}");
    // No key was supplied, so none is sent.
    assert!(
        !request.to_ascii_lowercase().contains("authorization"),
        "{request}"
    );
}

#[tokio::test]
async fn a_listing_over_the_cap_is_cut_and_counted() {
    let entries: Vec<String> = (0..501)
        .map(|index| format!(r#"{{"id":"m{index:03}"}}"#))
        .collect();
    let body = format!("[{}]", entries.join(",")).into_bytes();
    let (base, server) = crate::ai::test_support::canned_bytes(200, body);
    let listing = ai_fetch_models(
        chat_endpoint(&base),
        Wire::OpenAiChatCompletions,
        AuthKind::ApiKey,
        Some(KEY.into()),
    )
    .await
    .unwrap();
    assert_eq!(listing.total, 501);
    assert_eq!(listing.models.len(), 500);
    server.join().unwrap();
}

#[tokio::test]
async fn a_body_over_the_read_cap_is_refused() {
    let (base, server) =
        crate::ai::test_support::canned_bytes(200, vec![b'a'; crate::ai::RESPONSE_BYTES_LIMIT + 1]);
    let started = std::time::Instant::now();
    let error = ai_fetch_models(
        chat_endpoint(&base),
        Wire::OpenAiChatCompletions,
        AuthKind::ApiKey,
        Some(KEY.into()),
    )
    .await
    .unwrap_err();
    assert!(error.contains("16 MiB"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    server.join().unwrap();
}

/// A provider that accepts the connection and never answers. The listing path
/// must give up on its own budget instead of holding the dialog open.
#[ignore = "waits out the real 20 s listing timeout"]
#[tokio::test]
async fn a_hung_provider_times_out_instead_of_hanging() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(40));
        drop(socket);
    });
    let started = std::time::Instant::now();
    let error = ai_fetch_models(
        chat_endpoint(&base),
        Wire::OpenAiChatCompletions,
        AuthKind::ApiKey,
        Some(KEY.into()),
    )
    .await
    .unwrap_err();
    assert!(error.to_lowercase().contains("timed out"), "{error}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the listing did not give up on its own budget"
    );
    assert!(!error.contains(KEY), "{error}");
    server.join().unwrap();
}

// ============================================================================
// Keys
// ============================================================================

#[tokio::test]
async fn a_key_that_cannot_be_valid_is_refused_before_the_store() {
    let overlong = "x".repeat(9000);
    for bad in ["", "   ", overlong.as_str(), "line\nbreak", "nul\0byte"] {
        let error = ai_set_key("fixture".into(), bad.to_string())
            .await
            .unwrap_err();
        assert_eq!(error, "Invalid API key");
    }
}

#[tokio::test]
async fn a_key_for_an_unknown_provider_is_refused() {
    let (_fixture, _config, _limits) = config_at("key-unknown", BASE_YAML);
    for error in [
        ai_set_key("no-such-provider".into(), KEY.into())
            .await
            .unwrap_err(),
        ai_key_status("no-such-provider".into()).await.unwrap_err(),
        ai_remove_key("no-such-provider".into()).await.unwrap_err(),
    ] {
        assert!(error.contains("Save provider settings first"), "{error}");
        assert!(!error.contains(KEY), "{error}");
    }
}

#[tokio::test]
async fn a_saved_key_is_reported_as_present_and_never_returned() {
    let (_fixture, _config, _limits) = config_at("key-round-trip", BASE_YAML);
    match ai_set_key("fixture".into(), KEY.into()).await {
        Ok(()) => {
            assert!(ai_key_status("fixture".into()).await.unwrap());
            ai_remove_key("fixture".into()).await.unwrap();
            assert!(!ai_key_status("fixture".into()).await.unwrap());
            // Removing an absent key is not an error.
            ai_remove_key("fixture".into()).await.unwrap();
        }
        Err(error) => {
            // Only an environment without a usable store may skip this test; a
            // reachable store that refuses the write is a defect, not a skip.
            assert!(!error.contains(KEY), "{error}");
            assert!(
                error == crate::ai::auth::STORE_UNAVAILABLE
                    || error == crate::ai::auth::STORE_LOCKED,
                "the credential store is reachable but the write failed: {error}"
            );
            eprintln!(
                "SKIP a_saved_key_is_reported_as_present_and_never_returned: {error} \
                 (headless Linux has no Secret Service; a locked keyring needs the user)"
            );
        }
    }
}

// ============================================================================
// The sweep: no failure path may print the key
// ============================================================================

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[tokio::test]
async fn no_failure_path_prints_or_writes_the_key() {
    let (fixture, _config, _limits) = config_at("leak-sweep", BASE_YAML);
    let mut errors: Vec<String> = Vec::new();

    errors.push(
        ai_set_key("fixture".into(), String::new())
            .await
            .unwrap_err(),
    );
    errors.push(
        ai_set_key("fixture".into(), format!("{KEY}\n"))
            .await
            .unwrap_err(),
    );
    errors.push(ai_key_status("no-such-provider".into()).await.unwrap_err());
    errors.push(ai_remove_key("no-such-provider".into()).await.unwrap_err());
    errors.push(
        ai_add_provider(with_limits(
            AiProviderDraft {
                endpoint: "http://llm.acme.test/v1".into(),
                ..draft("Acme", &["acme-large"])
            },
            "acme-large",
            limits(64_000, 8_192),
        ))
        .unwrap_err(),
    );
    for status in [400, 401, 403, 404, 429, 500] {
        let (base, server) = crate::ai::test_support::canned(status, "echo the key back");
        errors.push(
            ai_fetch_models(
                chat_endpoint(&base),
                Wire::OpenAiChatCompletions,
                AuthKind::ApiKey,
                Some(KEY.into()),
            )
            .await
            .unwrap_err(),
        );
        server.join().unwrap();
    }
    let (base, server) = crate::ai::test_support::canned(200, "<html>login</html>");
    errors.push(
        ai_fetch_models(
            chat_endpoint(&base),
            Wire::OpenAiChatCompletions,
            AuthKind::ApiKey,
            Some(KEY.into()),
        )
        .await
        .unwrap_err(),
    );
    server.join().unwrap();
    let (base, server) = crate::ai::test_support::canned(200, r#"{"data":[]}"#);
    errors.push(
        ai_fetch_models(
            chat_endpoint(&base),
            Wire::OpenAiChatCompletions,
            AuthKind::ApiKey,
            Some(KEY.into()),
        )
        .await
        .unwrap_err(),
    );
    server.join().unwrap();
    errors.push(save_ai_config(config_of(vec![entry("fixture"), entry("fixture")])).unwrap_err());
    errors.push(ai_model_limits("openai".into(), "gpt-6.1-sol".into()).provider_id);

    assert!(errors.len() >= 12);
    for error in &errors {
        assert!(
            !error.contains(KEY),
            "a failure path printed the key: {error}"
        );
        assert!(
            !error.contains("echo the key back"),
            "a provider body reached an error: {error}"
        );
    }

    // Nothing on disk carries it either.
    for file in files_under(&fixture.dir) {
        let text = String::from_utf8_lossy(&bytes(&file)).into_owned();
        assert!(!text.contains(KEY), "{} carries the key", file.display());
    }
}
