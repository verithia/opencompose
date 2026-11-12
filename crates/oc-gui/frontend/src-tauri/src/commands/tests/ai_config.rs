use crate::commands::ai::*;

const LEGACY: &str = r#"active_provider: openai-gpt4o
providers:
  - id: openai-gpt4o
    name: OpenAI GPT-4o
    endpoint: https://api.openai.com/v1/chat/completions
    model: gpt-4o
    context_length: 128000
    max_tokens: 16384
    temperature: 0.3
    capabilities:
      text: true
      audio_in: false
      audio_out: false
      native_tool_calling: false
      streaming: true
  - id: gemini-2.5-pro
    name: Gemini 2.5 Pro
    endpoint: https://generativelanguage.googleapis.com/v1beta/openai/chat/completions
    model: gemini-2.5-pro-preview-06-05
    capabilities:
      streaming: true
"#;

#[test]
fn legacy_yaml_migrates_to_chat_completions_without_rewriting_identity() {
    let mut config = parse_ai_config(LEGACY).unwrap();
    normalize_ai_config(&mut config);
    assert_eq!(config.providers.len(), 2);
    for provider in &config.providers {
        assert_eq!(provider.kind, ProviderKind::OpenAiChatCompletions);
        assert_eq!(provider.auth, AuthKind::ApiKey);
    }
    assert_eq!(config.providers[0].id, "openai-gpt4o");
    assert_eq!(
        config.providers[0].endpoint,
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(config.providers[1].id, "gemini-2.5-pro");
    assert_eq!(
        config.providers[1].endpoint,
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
    );
}

#[test]
fn unknown_kind_and_auth_load_as_safe_defaults() {
    let yaml = LEGACY.replace(
        "    model: gpt-4o\n",
        "    kind: some_future_protocol\n    auth: oauth_codex\n    model: gpt-4o\n",
    );
    let mut config = parse_ai_config(&yaml).unwrap();
    normalize_ai_config(&mut config);
    assert_eq!(
        config.providers[0].kind,
        ProviderKind::OpenAiChatCompletions
    );
    assert_eq!(config.providers[0].auth, AuthKind::ApiKey);
}

#[test]
fn saving_v2_round_trips_wire_and_mirrors_the_legacy_kind() {
    let mut config = parse_ai_config(LEGACY).unwrap();
    normalize_ai_config(&mut config);
    config.providers[1].wire = Some(Wire::GeminiInteractions);
    normalize_ai_config(&mut config);
    let yaml = serde_yaml::to_string(&config).unwrap();
    assert!(yaml.contains("wire: openai-chat-completions"), "{yaml}");
    assert!(yaml.contains("wire: gemini-interactions"), "{yaml}");
    assert!(yaml.contains("kind: openai_chat_completions"), "{yaml}");
    assert!(yaml.contains("kind: gemini_interactions"), "{yaml}");
    assert!(yaml.contains("auth: api_key"), "{yaml}");
    let reread = parse_ai_config(&yaml).unwrap();
    assert_eq!(reread.providers[1].wire(), Wire::GeminiInteractions);
    assert_eq!(reread.providers[0].wire(), Wire::OpenAiChatCompletions);
    assert_eq!(reread.providers[1].kind, ProviderKind::GeminiInteractions);
}

#[test]
fn oauth_fields_round_trip_without_touching_api_key_entries() {
    let yaml = LEGACY.replace(
        "    model: gpt-4o\n",
        "    auth: oauth\n    auth_provider: openai_chat_gpt\n    model: gpt-4o\n",
    );
    let mut config = parse_ai_config(&yaml).unwrap();
    normalize_ai_config(&mut config);
    assert_eq!(config.version, 2);
    assert_eq!(config.providers[0].auth, AuthKind::OAuth);
    assert_eq!(
        config.providers[0].auth_provider,
        Some(AuthProvider::OpenAiChatGpt)
    );
    assert_eq!(config.providers[1].auth, AuthKind::ApiKey);
    assert_eq!(config.providers[1].auth_provider, None);
    let saved = serde_yaml::to_string(&config).unwrap();
    assert!(saved.contains("auth: oauth"));
    assert!(saved.contains("auth_provider: openai_chat_gpt"));
    assert!(!saved.contains("allow_custom_endpoint"));
    let reread = parse_ai_config(&saved).unwrap();
    assert_eq!(reread.providers[0].auth, AuthKind::OAuth);
}

#[test]
fn a_file_saved_before_thinking_levels_defaults_to_medium() {
    let config = parse_ai_config(LEGACY).unwrap();
    for provider in &config.providers {
        assert_eq!(provider.thinking_level, "medium");
    }
    let saved = serde_yaml::to_string(&config).unwrap();
    assert!(saved.contains("thinking_level: medium"), "{saved}");
    let reread = parse_ai_config(&saved).unwrap();
    for provider in &reread.providers {
        assert_eq!(provider.thinking_level, "medium");
    }
}

#[test]
fn keychain_account_is_id_and_endpoint_only() {
    use sha2::{Digest, Sha256};
    let config = parse_ai_config(LEGACY).unwrap();
    let provider = &config.providers[1];
    let account = format!(
        "{}:{:x}",
        provider.id,
        Sha256::digest(provider.endpoint.as_bytes())
    );
    assert!(account.starts_with("gemini-2.5-pro:"));
}

#[test]
fn oauth_is_supported_for_chatgpt_only() {
    assert!(AuthProvider::OpenAiChatGpt.supported());
    for unsupported in [
        AuthProvider::Anthropic,
        AuthProvider::GeminiCli,
        AuthProvider::Xai,
        AuthProvider::Unknown,
    ] {
        assert!(!unsupported.supported(), "{unsupported:?}");
    }
}

#[test]
fn a_saved_wire_is_the_protocol_and_normalizes_the_kind() {
    let yaml = LEGACY.replace(
        "    model: gpt-4o\n",
        "    kind: openai_chat_completions\n    wire: gemini-generate\n    model: gemini-4-argon\n",
    );
    let mut config = parse_ai_config(&yaml).unwrap();
    normalize_ai_config(&mut config);
    assert_eq!(config.providers[0].kind, ProviderKind::GeminiGenerate);
    assert_eq!(config.providers[0].wire(), Wire::GeminiGenerate);
    let saved = serde_yaml::to_string(&config).unwrap();
    assert!(saved.contains("wire: gemini-generate"), "{saved}");
}

#[test]
fn entries_without_a_wire_fall_back_to_the_saved_kind_once() {
    let mut config = parse_ai_config(LEGACY).unwrap();
    normalize_ai_config(&mut config);
    assert_eq!(config.providers[0].wire(), Wire::OpenAiChatCompletions);
    // The migrated value is persisted, so the legacy field is never consulted
    // again: a later edit of `kind` alone cannot change the protocol.
    let saved = serde_yaml::to_string(&config).unwrap();
    assert!(saved.contains("wire: openai-chat-completions"), "{saved}");
    let mut reread = parse_ai_config(&saved).unwrap();
    reread.providers[0].kind = ProviderKind::GeminiInteractions;
    assert_eq!(reread.providers[0].wire(), Wire::OpenAiChatCompletions);
}

/// A config saved while retired integrations shipped still loads: their entries
/// are dropped by tag, the other providers keep their settings, and the active
/// id moves to an entry that exists.
#[test]
fn a_saved_entry_of_a_removed_integration_is_dropped_and_the_rest_loads() {
    let yaml = r#"version: 2
active_provider: retired-hub
providers:
  - id: retired-hub
    name: Retired hub
    kind: antigravity
    wire: antigravity
    auth: oauth
    auth_provider: antigravity
    endpoint: https://retired.example
    model: retired-model
  - id: openai
    name: OpenAI
    wire: openai-responses
    endpoint: https://api.openai.com/v1/responses
    model: gpt-6.1-sol
  - id: retired-chat
    name: Retired chat
    kind: openai_responses
    wire: openai-responses
    auth: oauth
    auth_provider: github_copilot
    endpoint: https://retired-chat.example/responses
    model: gpt-6.1-sol
"#;
    let mut config = parse_ai_config(yaml).unwrap();
    normalize_ai_config(&mut config);
    let ids: Vec<&str> = config.providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["openai"]);
    assert_eq!(config.active_provider, "openai");
    assert_eq!(config.providers[0].wire(), Wire::OpenAiResponses);
    assert_eq!(
        config.providers[0].endpoint,
        "https://api.openai.com/v1/responses"
    );
    assert!(crate::ai_service::validate_config(&config).is_ok());

    // An unrelated unknown tag is still a current entry with safe defaults,
    // not a retired one.
    let future = LEGACY.replace(
        "    model: gpt-4o\n",
        "    auth_provider: some_future_oauth\n    model: gpt-4o\n",
    );
    assert_eq!(parse_ai_config(&future).unwrap().providers.len(), 2);
}
