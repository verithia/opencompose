use super::ProjectPatch;
use crate::ast::Project;

const MAGIC: &[u8; 4] = b"OCPC";
const VERSION: u8 = 1;

/// Bounds for the complete wire document and its decoded JSON payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecLimits {
    pub max_encoded_bytes: usize,
    pub max_decoded_bytes: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 128 * 1024 * 1024,
            max_decoded_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Why an OCPC encode or decode failed.
pub enum CodecError {
    #[error("OCPC encoded size exceeds {limit} bytes")]
    EncodedLimit { limit: usize },
    #[error("OCPC decoded size exceeds {limit} bytes")]
    DecodedLimit { limit: usize },
    #[error("Data too short for OCPC format")]
    TruncatedHeader,
    #[error("Invalid magic bytes (expected 'OCPC')")]
    InvalidMagic,
    #[error("Unsupported OCPC version: {0} (expected 1)")]
    UnsupportedVersion(u8),
    #[error("Invalid OCPC DEFLATE payload: {0}")]
    Decompression(#[source] flate2::DecompressError),
    #[error("Truncated OCPC DEFLATE payload")]
    TruncatedPayload,
    #[error("Trailing bytes after OCPC DEFLATE payload")]
    TrailingBytes,
    #[error("OCPC decoded size mismatch: declared {declared}, actual {actual}")]
    SizeMismatch { declared: usize, actual: usize },
    #[error("OCPC JSON error: {0}")]
    Json(#[source] serde_json::Error),
    #[error("OCPC compression error: {0}")]
    Compression(#[source] std::io::Error),
}

/// Serialize a project using the bounded OCPC wire owner.
pub fn encode_project(project: &Project) -> Result<Vec<u8>, String> {
    encode_project_with_limits(project, CodecLimits::default()).map_err(|error| error.to_string())
}

/// Encode a project under explicit size limits.
pub fn encode_project_with_limits(
    project: &Project,
    limits: CodecLimits,
) -> Result<Vec<u8>, CodecError> {
    encode_value(project, limits)
}

/// Deserialize a project using the bounded OCPC wire owner.
pub fn decode_project(data: &[u8]) -> Result<Project, String> {
    decode_project_with_limits(data, CodecLimits::default()).map_err(|error| error.to_string())
}

/// Decode a project under explicit size limits.
pub fn decode_project_with_limits(data: &[u8], limits: CodecLimits) -> Result<Project, CodecError> {
    let payload = decode_payload(data, limits)?;
    serde_json::from_slice(&payload).map_err(CodecError::Json)
}

/// Encode a project patch with default limits.
pub fn encode_patch(patch: &ProjectPatch) -> Result<Vec<u8>, String> {
    encode_patch_with_limits(patch, CodecLimits::default()).map_err(|error| error.to_string())
}

/// Encode a project patch under explicit size limits.
pub fn encode_patch_with_limits(
    patch: &ProjectPatch,
    limits: CodecLimits,
) -> Result<Vec<u8>, CodecError> {
    encode_value(patch, limits)
}

/// Decode a project patch with default limits.
pub fn decode_patch(data: &[u8]) -> Result<ProjectPatch, String> {
    decode_patch_with_limits(data, CodecLimits::default()).map_err(|error| error.to_string())
}

/// Decode a project patch under explicit size limits.
pub fn decode_patch_with_limits(
    data: &[u8],
    limits: CodecLimits,
) -> Result<ProjectPatch, CodecError> {
    let payload = decode_payload(data, limits)?;
    serde_json::from_slice(&payload).map_err(CodecError::Json)
}

/// A streaming serializer sink that refuses a payload before extending past its bound.
struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("OCPC byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_value(value: &impl serde::Serialize, limits: CodecLimits) -> Result<Vec<u8>, CodecError> {
    use std::io::Write;
    let mut json = BoundedBytes {
        bytes: Vec::new(),
        limit: limits.max_decoded_bytes.min(u32::MAX as usize),
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut json, value) {
        return Err(if json.exceeded {
            CodecError::DecodedLimit { limit: json.limit }
        } else {
            CodecError::Json(error)
        });
    }
    if limits.max_encoded_bytes < 9 {
        return Err(CodecError::EncodedLimit {
            limit: limits.max_encoded_bytes,
        });
    }
    let sink = BoundedBytes {
        bytes: Vec::new(),
        limit: limits.max_encoded_bytes - 9,
        exceeded: false,
    };
    let mut encoder = flate2::write::DeflateEncoder::new(sink, flate2::Compression::default());
    if let Err(error) = encoder.write_all(&json.bytes) {
        return Err(if encoder.get_ref().exceeded {
            CodecError::EncodedLimit {
                limit: limits.max_encoded_bytes,
            }
        } else {
            CodecError::Compression(error)
        });
    }
    if let Err(error) = encoder.try_finish() {
        return Err(if encoder.get_ref().exceeded {
            CodecError::EncodedLimit {
                limit: limits.max_encoded_bytes,
            }
        } else {
            CodecError::Compression(error)
        });
    }
    let compressed = encoder.finish().map_err(CodecError::Compression)?;
    let mut output = Vec::with_capacity(9 + compressed.bytes.len());
    output.extend_from_slice(MAGIC);
    output.push(VERSION);
    output.extend_from_slice(&(json.bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(&compressed.bytes);
    Ok(output)
}

fn decode_payload(data: &[u8], limits: CodecLimits) -> Result<Vec<u8>, CodecError> {
    if data.len() > limits.max_encoded_bytes {
        return Err(CodecError::EncodedLimit {
            limit: limits.max_encoded_bytes,
        });
    }
    if data.len() < 9 {
        return Err(CodecError::TruncatedHeader);
    }
    if &data[..4] != MAGIC {
        return Err(CodecError::InvalidMagic);
    }
    if data[4] != VERSION {
        return Err(CodecError::UnsupportedVersion(data[4]));
    }
    let expected = u32::from_le_bytes(data[5..9].try_into().expect("fixed header slice")) as usize;
    if expected > limits.max_decoded_bytes {
        return Err(CodecError::DecodedLimit {
            limit: limits.max_decoded_bytes,
        });
    }
    let compressed = &data[9..];
    let mut decoder = flate2::Decompress::new(false);
    let mut output = Vec::new();
    let mut scratch = [0_u8; 8192];
    loop {
        let input_before = decoder.total_in();
        let output_before = decoder.total_out();
        // One extra byte proves an understated header without inflating the rest.
        let available = expected
            .saturating_sub(output.len())
            .saturating_add(1)
            .min(scratch.len());
        let status = decoder
            .decompress(
                &compressed[input_before as usize..],
                &mut scratch[..available],
                // Streaming output is deliberately smaller than many valid payloads.
                // Finish requires sufficient output space in some backends; None
                // still reports StreamEnd once the raw DEFLATE end marker is read.
                flate2::FlushDecompress::None,
            )
            .map_err(CodecError::Decompression)?;
        let written = (decoder.total_out() - output_before) as usize;
        output.extend_from_slice(&scratch[..written]);
        if output.len() > expected {
            return Err(CodecError::SizeMismatch {
                declared: expected,
                actual: output.len(),
            });
        }
        if status == flate2::Status::StreamEnd {
            if decoder.total_in() as usize != compressed.len() {
                return Err(CodecError::TrailingBytes);
            }
            if output.len() != expected {
                return Err(CodecError::SizeMismatch {
                    declared: expected,
                    actual: output.len(),
                });
            }
            return Ok(output);
        }
        if decoder.total_in() == input_before && decoder.total_out() == output_before {
            return Err(CodecError::TruncatedPayload);
        }
    }
}
#[cfg(test)]
mod bounded_tests {
    use super::*;

    fn wire(json: &[u8], declared: u32) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(json).unwrap();
        let mut data = b"OCPC\x01".to_vec();
        data.extend_from_slice(&declared.to_le_bytes());
        data.extend_from_slice(&encoder.finish().unwrap());
        data
    }

    #[test]
    fn bounded_codec_project_patch_exact_limits_and_legacy_adapters() {
        let project = crate::parse("@project \"Before\"\n").unwrap();
        let bytes = encode_project(&project).unwrap();
        let limits = CodecLimits {
            max_encoded_bytes: bytes.len(),
            max_decoded_bytes: serde_json::to_vec(&project).unwrap().len(),
        };
        assert_eq!(decode_project_with_limits(&bytes, limits).unwrap(), project);
        assert_eq!(encode_project_with_limits(&project, limits).unwrap(), bytes);
        assert!(matches!(
            decode_project_with_limits(
                &bytes,
                CodecLimits {
                    max_encoded_bytes: bytes.len() - 1,
                    ..limits
                }
            ),
            Err(CodecError::EncodedLimit { .. })
        ));
        assert!(matches!(
            decode_project_with_limits(
                &bytes,
                CodecLimits {
                    max_decoded_bytes: limits.max_decoded_bytes - 1,
                    ..limits
                }
            ),
            Err(CodecError::DecodedLimit { .. })
        ));
        assert!(matches!(
            encode_project_with_limits(
                &project,
                CodecLimits {
                    max_encoded_bytes: bytes.len() - 1,
                    ..limits
                }
            ),
            Err(CodecError::EncodedLimit { .. })
        ));
        assert!(matches!(
            encode_project_with_limits(
                &project,
                CodecLimits {
                    max_decoded_bytes: limits.max_decoded_bytes - 1,
                    ..limits
                }
            ),
            Err(CodecError::DecodedLimit { .. })
        ));
        let mut edited = project.clone();
        edited.name = Some("After".into());
        let patch = super::super::diff(&project, &edited).unwrap().unwrap();
        let patch_bytes = encode_patch(&patch).unwrap();
        let decoded = decode_patch_with_limits(&patch_bytes, CodecLimits::default()).unwrap();
        assert_eq!(encode_patch(&decoded).unwrap(), patch_bytes);
        assert_eq!(decode_project(&bytes).unwrap(), project);
        assert_eq!(
            decode_patch(&patch_bytes).unwrap().changes.len(),
            patch.changes.len()
        );
    }

    #[test]
    fn bounded_codec_refuses_size_lies_truncation_and_trailing_frames() {
        let project = crate::parse("@project\n").unwrap();
        let json = serde_json::to_vec(&project).unwrap();
        let bytes = wire(&json, json.len() as u32);
        for len in 0..9 {
            assert!(matches!(
                decode_project_with_limits(&bytes[..len], CodecLimits::default()),
                Err(CodecError::TruncatedHeader)
            ));
        }
        for len in 9..bytes.len() {
            assert!(
                decode_project_with_limits(&bytes[..len], CodecLimits::default()).is_err(),
                "accepted truncated payload at {len}"
            );
        }
        let mut invalid = bytes.clone();
        invalid[0] = b'X';
        assert!(matches!(
            decode_project_with_limits(&invalid, CodecLimits::default()),
            Err(CodecError::InvalidMagic)
        ));
        invalid = bytes.clone();
        invalid[4] = 2;
        assert!(matches!(
            decode_project_with_limits(&invalid, CodecLimits::default()),
            Err(CodecError::UnsupportedVersion(2))
        ));
        for declared in [json.len() as u32 - 1, json.len() as u32 + 1] {
            assert!(matches!(
                decode_project_with_limits(&wire(&json, declared), CodecLimits::default()),
                Err(CodecError::SizeMismatch { .. })
            ));
        }
        invalid = bytes.clone();
        invalid.extend_from_slice(&[0]);
        assert!(matches!(
            decode_project_with_limits(&invalid, CodecLimits::default()),
            Err(CodecError::TrailingBytes)
        ));
        invalid = bytes.clone();
        invalid.extend_from_slice(&bytes[9..]);
        assert!(matches!(
            decode_project_with_limits(&invalid, CodecLimits::default()),
            Err(CodecError::TrailingBytes)
        ));
        assert!(matches!(
            decode_project_with_limits(&wire(b"!", 1), CodecLimits::default()),
            Err(CodecError::Json(_))
        ));
    }

    #[test]
    fn bounded_codec_refuses_huge_header_and_stops_compressed_expansion() {
        let mut header = b"OCPC\x01".to_vec();
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_project_with_limits(
                &header,
                CodecLimits {
                    max_encoded_bytes: 128,
                    max_decoded_bytes: 32
                }
            ),
            Err(CodecError::DecodedLimit { limit: 32 })
        ));
        let expanded = vec![b' '; 64 * 1024];
        let dishonest = wire(&expanded, 16);
        assert!(matches!(
            decode_project_with_limits(&dishonest, CodecLimits::default()),
            Err(CodecError::SizeMismatch {
                declared: 16,
                actual: 17
            })
        ));
        let mut project = crate::parse("@project\n").unwrap();
        project.name = Some("long-name-".repeat(10_000));
        let bytes = encode_project(&project).unwrap();
        assert_eq!(decode_project(&bytes).unwrap(), project);
    }
}
