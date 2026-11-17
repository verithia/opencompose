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

/// The release trust configuration: one hex Ed25519 public key per line, with
/// `#` comments. An empty configuration disables remote catalogs; bundled
/// catalogs remain available without a configured signing authority.
pub const PUBLIC_KEY_CONFIGURATION: &str = include_str!("../plugin-index.pub");

/// Most keys one trust configuration may list.
pub const MAX_TRUSTED_KEYS: usize = 8;

const RETIRED_DEVELOPMENT_KEY_ID: &str = "ed25519:25c212f44e75789d";

/// The Ed25519 public keys an index signature may come from.
///
/// A manifest names its signer by `key_id`, so verification selects exactly
/// one key; listing a spare key lets a release keep trusting the index after
/// the primary key is replaced, without a second verification path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKeys(Vec<[u8; 32]>);

impl TrustedKeys {
    /// A non-empty, duplicate-free set of at most [`MAX_TRUSTED_KEYS`] keys.
    pub fn new(keys: Vec<[u8; 32]>) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("A plugin-index trust configuration needs at least one key".into());
        }
        if keys.len() > MAX_TRUSTED_KEYS {
            return Err(format!(
                "A plugin-index trust configuration lists at most {MAX_TRUSTED_KEYS} keys"
            ));
        }
        for (index, key) in keys.iter().enumerate() {
            if keys[..index].contains(key) {
                return Err(format!("Plugin-index key {} is listed twice", key_id(key)));
            }
        }
        Ok(Self(keys))
    }

    /// One key given as raw bytes.
    pub fn single(key: &[u8]) -> Result<Self, String> {
        Self::new(vec![key_bytes(key)?])
    }

    /// Parse one hex key per line; blank lines and `#` comments are ignored.
    /// An empty configuration yields `None`.
    pub fn parse(configuration: &str) -> Result<Option<Self>, String> {
        let mut keys = Vec::new();
        for (number, line) in configuration.lines().enumerate() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let key = hex_decode(line)
                .and_then(|key| key_bytes(&key))
                .map_err(|error| format!("Plugin-index key on line {}: {error}", number + 1))?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Ok(None);
        }
        Self::new(keys).map(Some)
    }

    /// The key a manifest's `key_id` names, if it is trusted.
    pub fn find(&self, key_id: &str) -> Option<&[u8; 32]> {
        self.0
            .iter()
            .find(|key| self::key_id(key.as_slice()) == key_id)
    }

    /// The keys in configuration order.
    pub fn keys(&self) -> &[[u8; 32]] {
        &self.0
    }

    /// The key ids in configuration order.
    pub fn key_ids(&self) -> Vec<String> {
        self.0.iter().map(|key| key_id(key)).collect()
    }
}

fn key_bytes(key: &[u8]) -> Result<[u8; 32], String> {
    key.try_into().map_err(|_| {
        format!(
            "Plugin-index public key must be 32 bytes, got {}",
            key.len()
        )
    })
}

/// Read the explicitly configured release authority without a development fallback.
pub fn configured_keys() -> Result<Option<TrustedKeys>, String> {
    release_keys(PUBLIC_KEY_CONFIGURATION)
}

fn release_keys(configuration: &str) -> Result<Option<TrustedKeys>, String> {
    let keys = TrustedKeys::parse(configuration)?;
    if keys
        .as_ref()
        .is_some_and(|keys| keys.find(RETIRED_DEVELOPMENT_KEY_ID).is_some())
    {
        return Err(
            "The retired development key cannot be a release plugin-index authority".into(),
        );
    }
    Ok(keys)
}

/// Bind disk state to the configured authority and exact set of index origins.
pub fn trust_scope(keys: &TrustedKeys, origins: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update((keys.keys().len() as u64).to_le_bytes());
    for key in keys.keys() {
        digest.update(key);
    }
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

    fn trusted(key: &[u8]) -> TrustedKeys {
        TrustedKeys::single(key).unwrap()
    }

    #[test]
    fn release_trust_has_no_implicit_development_authority() {
        let _ = configured_keys().expect("committed release trust is valid or unconfigured");
        assert!(release_keys("").unwrap().is_none());
        assert!(release_keys("# no keys yet\n\n").unwrap().is_none());
        assert!(release_keys("not a key").is_err());
        assert!(
            release_keys("44d58a323c81778e7360a2edafa07c3590ffdc4859f1fb9c314b53fc1e93ccb9")
                .is_err()
        );
        let (_, public_key) = keypair();
        assert_eq!(
            release_keys(&crate::verify::hex(&public_key)).unwrap(),
            Some(trusted(&public_key))
        );
    }

    #[test]
    fn the_committed_release_trust_lists_a_primary_and_a_spare_key() {
        let keys = configured_keys()
            .unwrap()
            .expect("release keys are configured");
        assert_eq!(keys.keys().len(), 2, "{:?}", keys.key_ids());
    }

    #[test]
    fn the_committed_plugin_index_verifies_against_the_release_keys() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin-index");
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        let keys = configured_keys().unwrap().expect("release keys are configured");
        let manifest = read(MANIFEST_FILE);
        let signature = read(SIGNATURE_FILE);
        let declared = authenticate(&manifest, &signature, &keys).unwrap();
        let catalog = read(&declared.catalog.path);
        let policy = Policy {
            keys,
            origins: Vec::new(),
            app_version: Some(env!("CARGO_PKG_VERSION").into()),
        };
        let index = verify_parts(&policy, manifest, signature, catalog, String::new(), None)
            .unwrap_or_else(|error| panic!("committed index: {error}"));
        assert!(!index.document.entries.is_empty());
        // The directory holds the manifest, its signature, the current catalog
        // and at most the previous one, which in-flight clients may still fetch.
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let catalogs = names
            .iter()
            .filter(|name| name.starts_with("catalog-") && name.ends_with(".json.zst"))
            .count();
        assert!((1..=2).contains(&catalogs), "{names:?}");
        assert_eq!(names.len(), catalogs + 2, "{names:?}");
        assert!(names.contains(&declared.catalog.path));
    }

    #[test]
    fn a_trust_list_selects_the_key_a_manifest_names() {
        let (_, primary) = keypair();
        let (_, spare) = keypair();
        let configuration = format!(
            "# primary\n{}\n\n{}  # spare\n",
            crate::verify::hex(&primary),
            crate::verify::hex(&spare)
        );
        let keys = release_keys(&configuration).unwrap().unwrap();
        assert_eq!(keys.key_ids(), vec![key_id(&primary), key_id(&spare)]);
        assert_eq!(
            keys.find(&key_id(&spare)).unwrap().as_slice(),
            spare.as_slice()
        );
        assert!(keys.find("ed25519:0000000000000000").is_none());
    }

    #[test]
    fn a_trust_list_refuses_duplicates_short_keys_and_overflow() {
        let (_, key) = keypair();
        let hex = crate::verify::hex(&key);
        let error = release_keys(&format!("{hex}\n{hex}\n")).unwrap_err();
        assert!(error.contains("listed twice"), "{error}");
        let error = release_keys(&format!("{hex}\nabcd\n")).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        let many = (0..=MAX_TRUSTED_KEYS)
            .map(|_| crate::verify::hex(&keypair().1))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(release_keys(&many).is_err());
    }

    #[test]
    fn cache_scope_changes_with_authority_or_origin() {
        let (_, key) = keypair();
        let (_, other) = keypair();
        let key = trusted(&key);
        let other = trusted(&other);
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
        let both = TrustedKeys::new(vec![key.keys()[0], other.keys()[0]]).unwrap();
        assert_ne!(
            trust_scope(&key, &["https://example.com"]),
            trust_scope(&both, &["https://example.com"])
        );
    }

    #[test]
    fn odd_hex_is_rejected() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }
}
