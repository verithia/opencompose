//! Provider-neutral sign-in failures: the internal flow error and the shape a
//! polled attempt reports to the panel.
use super::RefreshError;
use serde::Serialize;

#[derive(Debug)]
/// A failed interactive OAuth flow.
pub struct FlowError {
    pub message: String,
    pub expired: bool,
}

impl FlowError {
    /// A failure the user retries by starting a new sign-in.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            expired: false,
        }
    }

    /// The sign-in window lapsed before the browser returned.
    pub fn expired() -> Self {
        Self {
            message: "The sign-in window expired. Start again.".into(),
            expired: true,
        }
    }
}

impl From<RefreshError> for FlowError {
    fn from(error: RefreshError) -> Self {
        Self::new(error.message)
    }
}

/// What kind of failure ended a sign-in attempt. The panel keys on this tag,
/// never on the message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInErrorKind {
    /// The sign-in window lapsed; a new attempt can succeed.
    Expired,
    /// Any other failure; the message names it.
    Failed,
}

/// A sign-in failure in the shape the panel reads off a polled attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignInError {
    pub kind: SignInErrorKind,
    pub message: String,
}

impl From<FlowError> for SignInError {
    fn from(error: FlowError) -> Self {
        Self {
            kind: if error.expired {
                SignInErrorKind::Expired
            } else {
                SignInErrorKind::Failed
            },
            message: error.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_tag_is_stable_on_the_wire() {
        let expired = SignInError::from(FlowError::expired());
        assert_eq!(
            serde_json::to_value(&expired).unwrap(),
            serde_json::json!({
                "kind": "expired",
                "message": "The sign-in window expired. Start again."
            })
        );
        let failed = SignInError::from(FlowError::from(RefreshError::terminal("denied")));
        assert_eq!(failed.kind, SignInErrorKind::Failed);
        assert_eq!(serde_json::to_value(&failed).unwrap()["kind"], "failed");
    }
}
