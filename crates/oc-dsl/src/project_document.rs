//! Read-only project snapshots: exact DSL text or the existing OCPC AST wire.
use crate::ast::Project;
use crate::diff::compressed::{self, CodecError, CodecLimits};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Container format of a project document.
pub enum ProjectDocumentFormat {
    Text,
    Ocpc,
}

#[derive(Debug)]
/// A decoded project and its optional authored text.
pub struct DecodedProject {
    pub project: Project,
    /// Exact authored text when the input is DSL; binary snapshots have no text source.
    pub source: Option<String>,
    pub format: ProjectDocumentFormat,
}

#[derive(Debug, thiserror::Error)]
/// Why a project document could not be decoded.
pub enum ProjectDocumentError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("Project text is not valid UTF-8: {0}")]
    Utf8(#[source] std::str::Utf8Error),
    #[error("Project text parse error: {0}")]
    Parse(#[source] crate::error::ParseError),
}

/// Decode without injecting defaults, resolving resources, or mutating the AST.
/// Magic determines binary input; malformed binary never falls back to text parsing.
pub fn decode(bytes: &[u8], limits: CodecLimits) -> Result<DecodedProject, ProjectDocumentError> {
    if bytes.len() > limits.max_encoded_bytes {
        return Err(CodecError::EncodedLimit {
            limit: limits.max_encoded_bytes,
        }
        .into());
    }
    if bytes.starts_with(b"OCPC") || (!bytes.is_empty() && b"OCPC".starts_with(bytes)) {
        return Ok(DecodedProject {
            project: compressed::decode_project_with_limits(bytes, limits)?,
            source: None,
            format: ProjectDocumentFormat::Ocpc,
        });
    }
    if bytes.len() > limits.max_decoded_bytes {
        return Err(CodecError::DecodedLimit {
            limit: limits.max_decoded_bytes,
        }
        .into());
    }
    let text = std::str::from_utf8(bytes).map_err(ProjectDocumentError::Utf8)?;
    let project = crate::parse(text).map_err(ProjectDocumentError::Parse)?;
    Ok(DecodedProject {
        project,
        source: Some(text.to_owned()),
        format: ProjectDocumentFormat::Text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_document_preserves_exact_text_and_binary_only_ast_fields() {
        let source = "// authored comment\n@project \"Snapshot\"\n";
        let decoded = decode(source.as_bytes(), CodecLimits::default()).unwrap();
        assert_eq!(decoded.source.as_deref(), Some(source));
        assert_eq!(decoded.format, ProjectDocumentFormat::Text);
        let mut project = decoded.project;
        let mut pattern =
            crate::parse("@project\n@pattern p | instrument: synth/lead\n  events:\n    C4 q mf\n")
                .unwrap()
                .patterns
                .remove(0);
        let track = crate::parse("@project\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - reverb: mix=20%\n").unwrap().tracks.remove(0);
        pattern.effects = track.effects;
        assert!(!pattern.effects.is_empty());
        project.patterns.push(pattern);
        let bytes = compressed::encode_project(&project).unwrap();
        let binary = decode(&bytes, CodecLimits::default()).unwrap();
        assert_eq!(binary.project, project);
        assert_eq!(binary.format, ProjectDocumentFormat::Ocpc);
        assert!(binary.source.is_none());
    }

    #[test]
    fn project_document_refuses_invalid_or_over_limit_text_and_binary() {
        let limits = CodecLimits {
            max_encoded_bytes: 8,
            max_decoded_bytes: 4,
        };
        assert!(matches!(
            decode(b"@project!", limits),
            Err(ProjectDocumentError::Codec(CodecError::EncodedLimit { .. }))
        ));
        assert!(matches!(
            decode(b"12345", limits),
            Err(ProjectDocumentError::Codec(CodecError::DecodedLimit { .. }))
        ));
        assert!(matches!(
            decode(&[255], limits),
            Err(ProjectDocumentError::Utf8(_))
        ));
        assert!(matches!(
            decode(b"OC", limits),
            Err(ProjectDocumentError::Codec(CodecError::TruncatedHeader))
        ));
        assert!(matches!(
            decode(b"invalid", CodecLimits::default()),
            Err(ProjectDocumentError::Parse(_))
        ));
    }
}
