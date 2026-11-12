//! Explicit project and conversation ownership for saved HTTP provider turns.

use super::*;
use crate::ai::{session_usage, sessions};
use crate::commands::AiProviderEntry;
use std::path::Path;

pub(super) struct Binding {
    id: String,
    project: std::path::PathBuf,
}

impl Binding {
    pub(super) fn parse(value: &Value) -> Result<Option<Self>, String> {
        match (value.get("session_id"), value.get("project_path")) {
            (None | Some(Value::Null), None | Some(Value::Null)) => Ok(None),
            (Some(Value::String(id)), Some(Value::String(project)))
                if !id.is_empty() && id.len() <= 100 && Path::new(project).is_absolute() =>
            {
                Ok(Some(Self {
                    id: id.clone(),
                    project: project.into(),
                }))
            }
            _ => Err("Saved turns require session_id and an absolute project_path".into()),
        }
    }

    fn validate_project(&self, actual: Option<&Path>) -> Result<(), String> {
        let expected = self
            .project
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let actual = actual
            .ok_or("No project is open")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if expected != actual {
            return Err("The saved turn belongs to another project".into());
        }
        Ok(())
    }
}

pub(super) struct SavedTurn {
    app: AppHandle,
    id: String,
    request_id: String,
    incarnation: String,
    pub(super) messages: Value,
}

impl SavedTurn {
    pub(super) fn prepare(
        app: &AppHandle,
        binding: &Binding,
        provider: &AiProviderEntry,
        messages: &Value,
        stats: &crate::ai::timing::TurnStatsHandle,
    ) -> Result<Self, AiError> {
        binding
            .validate_project(app.state::<AppState>().lock_inner().file_path.as_deref())
            .map_err(|error| AiError::new("session_save", error))?;
        let token = access_token::make_token()
            .map_err(|_| AiError::config("Request identity unavailable"))?;
        let request_id = format!("headless-{token}");
        let lease = session_usage::SessionLease::admit(
            sessions::Store::config(),
            &binding.id,
            &request_id,
            session_usage::Purpose::Chat,
        )
        .map_err(|error| AiError::new(error.code.as_str(), error.message))?;
        let incarnation = lease.update().incarnation;
        stats.set_accounting(lease);
        let prepared =
            commands::prepare_turn_for(app, provider, messages, &binding.id, &incarnation)
                .map_err(|error| AiError::new("session_save", error))?
                .ok_or_else(|| {
                    AiError::new("session_save", "The saved conversation cannot be replayed")
                })?;
        Ok(Self {
            app: app.clone(),
            id: binding.id.clone(),
            request_id,
            incarnation,
            messages: prepared.messages,
        })
    }

    pub(super) async fn finish(
        self,
        provider: &AiProviderEntry,
        stats: &crate::ai::timing::TurnStatsHandle,
        reply: Option<&Value>,
    ) -> Result<(), AiError> {
        session_usage::checkpoint_final(
            stats,
            &self.app,
            &self.request_id,
            session_usage::Purpose::Chat,
        )
        .await;
        if let Some(reply) = reply {
            commands::record_reply_for(
                &self.app,
                &self.id,
                reply,
                provider,
                Some(&self.incarnation),
            )
            .map_err(|error| AiError::new("session_save", error))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_completion_requires_explicit_project_and_session() {
        let absolute = std::env::temp_dir().join("song.ocp");
        assert!(Binding::parse(&json!({})).unwrap().is_none());
        for value in [
            json!({"session_id":"x"}),
            json!({"project_path":absolute}),
            json!({"session_id":"x","project_path":"song.ocp"}),
            json!({"session_id":7,"project_path":absolute}),
        ] {
            assert!(Binding::parse(&value).is_err());
        }
        assert!(
            Binding::parse(&json!({"session_id":"x","project_path":absolute}))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn saved_completion_refuses_another_open_project() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.ocp");
        let second = root.path().join("second.ocp");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();
        let binding = Binding {
            id: "session".into(),
            project: first.clone(),
        };
        assert!(binding.validate_project(Some(&first)).is_ok());
        assert!(binding.validate_project(Some(&second)).is_err());
        assert!(binding.validate_project(None).is_err());
    }
}
