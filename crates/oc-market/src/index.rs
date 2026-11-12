//! The pinned index key and the detached-signature rule.
//!
//! Both the GUI index client and `ocp plugin install` verify a manifest with
//! [`verify_manifest_signature`], so a signature they disagree about cannot
//! exist.

mod store;
mod verified;
#[cfg(test)]
mod verified_tests;
pub use store::{CacheState, IndexStore, PublicationGuard, StoredIndex, read_bounded_file};
pub use verified::*;

use ring::signature::{ED25519, UnparsedPublicKey};
use sha2::{Digest, Sha256};

/// The release trust configuration. An empty file disables remote catalogs;
/// bundled catalogs remain available without a configured signing authority.
pub const PUBLIC_KEY_CONFIGURATION: &str = include_str!("../plugin-index.pub");

/// Read the explicitly configured release authority without a development fallback.
pub fn configured_public_key() -> Result<Option<Vec<u8>>, String> {
    release_public_key(PUBLIC_KEY_CONFIGURATION)
}

fn release_public_key(configuration: &str) -> Result<Option<Vec<u8>>, String> {
    if configuration.trim().is_empty() {
        return Ok(None);
    }
    let key = hex_decode(configuration)?;
    if key.len() != 32 {
        return Err("Plugin-index public key must be 32 bytes".into());
    }
    if key_id(&key) == "ed25519:25c212f44e75789d" {
        return Err(
            "The retired development key cannot be the release plugin-index authority".into(),
        );
    }
    Ok(Some(key))
}

/// Bind disk state to the configured authority and exact set of index origins.
pub fn trust_scope(public_key: &[u8], origins: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(public_key);
    for origin in origins {
        let origin = origin.trim_end_matches('/');
        digest.update((origin.len() as u64).to_le_bytes());
        digest.update(origin.as_bytes());
    }
    crate::verify::hex(&digest.finalize())
}

/// Byte length of a detached Ed25519 signature.
pub const SIGNATURE_BYTES: usize = 64;

const KEY_ID_BYTES: usize = 8;

/// Decode a hex-encoded public key or digest.
pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected an even number of hex digits".into());
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks(2) {
        let value = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
        out.push(u8::from_str_radix(value, 16).map_err(|error| error.to_string())?);
    }
    Ok(out)
}

/// The key id a manifest must name for this public key.
pub fn key_id(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    format!("ed25519:{}", crate::verify::hex(&digest[..KEY_ID_BYTES]))
}

/// Verify a detached Ed25519 signature over the exact manifest bytes.
///
/// This checks the signature only: the caller still decides whether the
/// manifest's declared `key_id`, schema and version are acceptable.
pub fn verify_manifest_signature(
    manifest: &[u8],
    signature: &[u8],
    public_key: &[u8],
) -> Result<(), String> {
    if signature.len() != SIGNATURE_BYTES {
        return Err(format!(
            "signature is {} bytes, expected {SIGNATURE_BYTES}",
            signature.len()
        ));
    }
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(manifest, signature)
        .map_err(|_| "manifest signature does not match the pinned public key".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn keypair() -> (Vec<u8>, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        (pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec())
    }

    #[test]
    fn a_real_signature_verifies_and_a_flipped_byte_does_not() {
        let (pkcs8, public_key) = keypair();
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();
        let manifest = br#"{"schema":1}"#;
        let signature = pair.sign(manifest).as_ref().to_vec();
        assert!(verify_manifest_signature(manifest, &signature, &public_key).is_ok());
        assert!(verify_manifest_signature(br#"{"schema":2}"#, &signature, &public_key).is_err());
        let mut tampered = signature.clone();
        tampered[0] ^= 1;
        assert!(verify_manifest_signature(manifest, &tampered, &public_key).is_err());
    }

    #[test]
    fn a_short_signature_is_named() {
        let (_, public_key) = keypair();
        let error = verify_manifest_signature(b"{}", &[0; 8], &public_key).unwrap_err();
        assert!(error.contains("expected 64"), "{error}");
    }

    #[test]
    fn release_trust_has_no_implicit_development_authority() {
        let _ = configured_public_key().expect("committed release trust is valid or unconfigured");
        assert!(release_public_key("").unwrap().is_none());
        assert!(release_public_key("not a key").is_err());
        assert!(
            release_public_key("44d58a323c81778e7360a2edafa07c3590ffdc4859f1fb9c314b53fc1e93ccb9")
                .is_err()
        );
        let (_, public_key) = keypair();
        assert_eq!(
            release_public_key(&crate::verify::hex(&public_key)).unwrap(),
            Some(public_key)
        );
    }

    #[test]
    fn cache_scope_changes_with_authority_or_origin() {
        let (_, key) = keypair();
        let (_, other) = keypair();
        assert_eq!(
            trust_scope(&key, &["https://example.com/"]),
            trust_scope(&key, &["https://example.com"])
        );
        assert_ne!(
            trust_scope(&key, &["https://example.com"]),
            trust_scope(&other, &["https://example.com"])
        );
        assert_ne!(
            trust_scope(&key, &["https://example.com"]),
            trust_scope(&key, &["https://other.example.com"])
        );
    }

    #[test]
    fn odd_hex_is_rejected() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }
}
