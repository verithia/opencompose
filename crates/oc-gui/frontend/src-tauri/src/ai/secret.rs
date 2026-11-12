//! Secret string whose heap buffer is overwritten when dropped. `Debug` never
//! prints the value, and serde keeps the wire representation plain.
use serde::{Deserialize, Serialize};
use std::ops::Deref;
use zeroize::Zeroizing;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
/// Zeroizing API credential.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wrap a credential value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Credential text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether the credential is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Deref for Secret {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&String> for Secret {
    fn from(value: &String) -> Self {
        Self::new(value.as_str())
    }
}

impl From<&Secret> for Secret {
    fn from(value: &Secret) -> Self {
        value.clone()
    }
}

impl PartialEq<str> for Secret {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Secret {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trips_through_serde_and_redacts_debug() {
        let secret = Secret::new("fixture-secret");
        assert_eq!(secret.as_str(), "fixture-secret");
        assert_eq!(secret, "fixture-secret");
        assert!(!format!("{secret:?}").contains("fixture-secret"));
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"fixture-secret\"");
        assert_eq!(serde_json::from_str::<Secret>(&json).unwrap(), secret);
    }
}
