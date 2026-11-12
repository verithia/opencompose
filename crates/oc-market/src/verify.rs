//! Download verification: one size-and-sha256 rule for every artifact.

use sha2::{Digest, Sha256};

/// A size or hash that disagrees with what the catalog declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    Bytes { actual: u64, expected: u64 },
    Sha256,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes { actual, expected } => {
                write!(formatter, "Downloaded {actual} bytes, expected {expected}")
            }
            Self::Sha256 => write!(formatter, "Downloaded archive failed sha256 verification"),
        }
    }
}

impl std::error::Error for Mismatch {}

/// Hex spelling of a digest, lower case, no separators.
pub fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// sha256 of a complete artifact, in the spelling the catalog stores.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// The one verification rule for everything the catalog serves: a declared
/// size and a declared sha256, both checked when present. The resolver already
/// refuses a build with no pinned sha256, so a plan never reaches here
/// unhashed.
pub fn verify_download(
    actual_bytes: u64,
    actual_sha256: &str,
    expected_bytes: Option<u64>,
    expected_sha256: Option<&str>,
) -> Result<(), Mismatch> {
    if let Some(expected) = expected_bytes
        && actual_bytes != expected
    {
        return Err(Mismatch::Bytes {
            actual: actual_bytes,
            expected,
        });
    }
    if let Some(expected) = expected_sha256
        && actual_sha256 != expected
    {
        return Err(Mismatch::Sha256);
    }
    Ok(())
}

/// Check one downloaded artifact against a plan's pinned hash and size.
pub fn verify_artifact(
    bytes: &[u8],
    sha256: &str,
    expected_bytes: Option<u64>,
) -> Result<(), Mismatch> {
    verify_download(
        bytes.len() as u64,
        &sha256_hex(bytes),
        expected_bytes,
        Some(sha256),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_artifact_passes() {
        let bytes = b"archive";
        let sha = sha256_hex(bytes);
        assert!(verify_artifact(bytes, &sha, Some(bytes.len() as u64)).is_ok());
    }

    #[test]
    fn a_wrong_size_is_named() {
        let bytes = b"archive";
        let error = verify_artifact(bytes, &sha256_hex(bytes), Some(3)).unwrap_err();
        assert_eq!(
            error,
            Mismatch::Bytes {
                actual: 7,
                expected: 3
            }
        );
        assert!(error.to_string().contains("Downloaded 7 bytes"));
    }

    #[test]
    fn a_wrong_hash_is_refused() {
        let error = verify_artifact(b"archive", &"0".repeat(64), None).unwrap_err();
        assert_eq!(error, Mismatch::Sha256);
        assert!(error.to_string().contains("sha256"));
    }

    #[test]
    fn an_unhashed_artifact_is_not_checked() {
        // The resolver refuses unpinned builds, so this only documents the
        // rule at this layer: no declared hash means no hash check.
        assert!(verify_download(7, &sha256_hex(b"archive"), None, None).is_ok());
    }
}
