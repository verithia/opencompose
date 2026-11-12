//! Deletes credentials that the saved configuration no longer references:
//! removed providers, changed ids/endpoints and switched auth kinds.
use super::auth::{API_KEY_SERVICE, OAUTH_SERVICE, api_key_account, oauth_account};
use super::credential_store::{self, KeyringSecrets};
use crate::commands::{AiProviderEntry, AuthKind};

/// Removes credentials that no longer belong to a provider.
pub trait CredentialSink {
    fn delete(&self, service: &'static str, account: &str) -> Result<(), String>;
}

/// Credential sink backed by the OS keyring.
pub struct KeyringSink;

impl CredentialSink for KeyringSink {
    fn delete(&self, service: &'static str, account: &str) -> Result<(), String> {
        credential_store::delete_entry(&KeyringSecrets::new(service), account)
            .map_err(|_| "Could not remove an OS credential".to_string())
    }
}

/// Credentials referenced by `old` whose `(service, account)` is absent from `new`.
pub fn stale_credentials(
    old: &[AiProviderEntry],
    new: &[AiProviderEntry],
) -> Vec<(&'static str, String)> {
    let needed: Vec<_> = new.iter().flat_map(credential_refs).collect();
    let mut stale = Vec::new();
    for credential in old.iter().flat_map(credential_refs) {
        if !needed.contains(&credential) && !stale.contains(&credential) {
            stale.push(credential);
        }
    }
    stale
}

fn credential_refs(entry: &AiProviderEntry) -> Vec<(&'static str, String)> {
    match entry.auth {
        AuthKind::ApiKey => vec![(API_KEY_SERVICE, api_key_account(entry))],
        AuthKind::OAuth => oauth_account(entry)
            .map(|account| vec![(OAUTH_SERVICE, account)])
            .unwrap_or_default(),
        AuthKind::Unknown => Vec::new(),
    }
}

/// Delete credentials for providers removed from the config.
pub fn delete_stale_with(
    sink: &dyn CredentialSink,
    old: &[AiProviderEntry],
    new: &[AiProviderEntry],
) -> Result<(), String> {
    for (service, account) in stale_credentials(old, new) {
        sink.delete(service, &account)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AuthProvider, ProviderKind, Wire};
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingSink {
        calls: RefCell<Vec<(&'static str, String)>>,
        fail: bool,
    }

    impl CredentialSink for RecordingSink {
        fn delete(&self, service: &'static str, account: &str) -> Result<(), String> {
            if self.fail {
                return Err("locked".into());
            }
            self.calls.borrow_mut().push((service, account.to_owned()));
            Ok(())
        }
    }

    fn entry(id: &str, endpoint: &str, auth: AuthKind) -> AiProviderEntry {
        AiProviderEntry {
            id: id.into(),
            name: id.into(),
            kind: ProviderKind::OpenAiResponses,
            wire: Some(Wire::OpenAiResponses),
            auth,
            auth_provider: (auth == AuthKind::OAuth).then_some(AuthProvider::OpenAiChatGpt),
            oauth_allowed_hosts: Vec::new(),
            endpoint: endpoint.into(),
            model: "gpt-6.1-sol".into(),
            models: Vec::new(),
            context_length: 128000,
            max_tokens: 4096,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: Default::default(),
        }
    }

    fn api(id: &str, endpoint: &str) -> AiProviderEntry {
        entry(id, endpoint, AuthKind::ApiKey)
    }

    fn oauth(id: &str, endpoint: &str) -> AiProviderEntry {
        entry(id, endpoint, AuthKind::OAuth)
    }

    #[test]
    fn removal_deletes_both_credential_services() {
        let key = api("api-provider", "https://api.openai.com/v1/responses");
        let token = oauth("oauth-provider", "https://api.openai.com/v1/responses");
        let expected_key = api_key_account(&key);
        let expected_token = oauth_account(&token).unwrap();
        let sink = RecordingSink::default();
        delete_stale_with(&sink, &[key, token], &[]).unwrap();
        let calls = sink.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .any(|(service, account)| *service == API_KEY_SERVICE && *account == expected_key)
        );
        assert!(
            calls
                .iter()
                .any(|(service, account)| *service == OAUTH_SERVICE && *account == expected_token)
        );
    }

    #[test]
    fn rename_and_endpoint_change_orphan_the_old_accounts() {
        let old = api("old-id", "https://api.openai.com/v1/responses");
        let renamed = api("new-id", "https://api.openai.com/v1/responses");
        let moved = api("old-id", "https://gateway.example/v1/responses");
        for replacement in [renamed, moved] {
            let sink = RecordingSink::default();
            delete_stale_with(&sink, std::slice::from_ref(&old), &[replacement]).unwrap();
            assert_eq!(
                sink.calls.borrow().as_slice(),
                &[(API_KEY_SERVICE, api_key_account(&old))]
            );
        }
    }

    #[test]
    fn switching_auth_kind_deletes_the_other_credential() {
        let endpoint = "https://api.openai.com/v1/responses";
        let old_api = api("fixture", endpoint);
        let old_oauth = oauth("fixture", endpoint);
        let sink = RecordingSink::default();
        delete_stale_with(
            &sink,
            std::slice::from_ref(&old_api),
            std::slice::from_ref(&old_oauth),
        )
        .unwrap();
        assert_eq!(
            sink.calls.borrow().as_slice(),
            &[(API_KEY_SERVICE, api_key_account(&old_api))]
        );
        let sink = RecordingSink::default();
        delete_stale_with(&sink, std::slice::from_ref(&old_oauth), &[old_api]).unwrap();
        assert_eq!(
            sink.calls.borrow().as_slice(),
            &[(OAUTH_SERVICE, oauth_account(&old_oauth).unwrap())]
        );
    }

    #[test]
    fn unchanged_providers_keep_their_credentials() {
        let providers = vec![
            api("api", "https://api.openai.com/v1/responses"),
            oauth("oauth", "https://api.openai.com/v1/responses"),
        ];
        assert!(stale_credentials(&providers, &providers).is_empty());
    }

    #[test]
    fn a_failed_delete_is_reported_and_stops_the_prune() {
        let sink = RecordingSink {
            fail: true,
            ..Default::default()
        };
        let error = delete_stale_with(
            &sink,
            &[api("a", "https://api.openai.com/v1/responses")],
            &[],
        )
        .unwrap_err();
        assert_eq!(error, "locked");
    }
}
