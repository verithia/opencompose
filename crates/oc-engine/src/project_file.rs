//! Bounded regular-file acquisition for text and compressed project snapshots.
//! The caller retains the original path as its resource-root authority.
use oc_dsl::diff::compressed::CodecLimits;
use oc_dsl::project_document::{DecodedProject, ProjectDocumentError};
use std::io::Read;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
/// Why a project file could not be loaded.
pub enum ProjectFileError {
    #[error("Cannot {operation} project file: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("Project input must be a regular file")]
    NonRegularFile,
    #[error(transparent)]
    Document(#[from] ProjectDocumentError),
}

/// Load and decode a project file.
pub fn load(path: &Path) -> Result<DecodedProject, ProjectFileError> {
    load_with_limits(path, CodecLimits::default())
}

/// Load a project file under explicit codec limits.
pub fn load_with_limits(
    path: &Path,
    limits: CodecLimits,
) -> Result<DecodedProject, ProjectFileError> {
    let io = |operation, source| ProjectFileError::Io { operation, source };
    // Check both the path and the acquired handle; Unix nonblocking open also
    // prevents a racing regular-file-to-FIFO replacement from hanging here.
    let metadata = std::fs::metadata(path).map_err(|error| io("inspect", error))?;
    if !metadata.is_file() {
        return Err(ProjectFileError::NonRegularFile);
    }
    if metadata.len() > limits.max_encoded_bytes as u64 {
        return Err(encoded_limit(limits));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| io("open", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io("inspect opened", error))?;
    if !metadata.is_file() {
        return Err(ProjectFileError::NonRegularFile);
    }
    if metadata.len() > limits.max_encoded_bytes as u64 {
        return Err(encoded_limit(limits));
    }
    let bytes = read_bounded(file, limits)?;
    oc_dsl::project_document::decode(&bytes, limits).map_err(ProjectFileError::Document)
}

fn encoded_limit(limits: CodecLimits) -> ProjectFileError {
    ProjectDocumentError::Codec(oc_dsl::diff::compressed::CodecError::EncodedLimit {
        limit: limits.max_encoded_bytes,
    })
    .into()
}

fn read_bounded(mut reader: impl Read, limits: CodecLimits) -> Result<Vec<u8>, ProjectFileError> {
    let mut bytes = Vec::new();
    let mut scratch = [0_u8; 8192];
    loop {
        let available = limits
            .max_encoded_bytes
            .saturating_sub(bytes.len())
            .saturating_add(1)
            .min(scratch.len());
        let count = match reader.read(&mut scratch[..available]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(ProjectFileError::Io {
                    operation: "read",
                    source,
                });
            }
        };
        if count == 0 {
            return Ok(bytes);
        }
        if count > limits.max_encoded_bytes.saturating_sub(bytes.len()) {
            return Err(encoded_limit(limits));
        }
        bytes.extend_from_slice(&scratch[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_file_read_bound_covers_growth_and_exact_limit() {
        let limits = CodecLimits {
            max_encoded_bytes: 8,
            max_decoded_bytes: 8,
        };
        assert_eq!(read_bounded(&b"12345678"[..], limits).unwrap(), b"12345678");
        assert!(matches!(
            read_bounded(&b"123456789"[..], limits),
            Err(ProjectFileError::Document(ProjectDocumentError::Codec(
                oc_dsl::diff::compressed::CodecError::EncodedLimit { .. }
            )))
        ));
    }

    #[test]
    fn project_file_loader_uses_content_and_refuses_nonregular_inputs() {
        let root = oc_audio::private_fs::test_root().join("project-file-loader");
        std::fs::create_dir_all(&root).unwrap();
        assert!(matches!(load(&root), Err(ProjectFileError::NonRegularFile)));
        let path = root.join("声-snapshot.ocp");
        let project = oc_dsl::parse("@project\n").unwrap();
        std::fs::write(
            &path,
            oc_dsl::diff::compressed::encode_project(&project).unwrap(),
        )
        .unwrap();
        assert_eq!(load(&path).unwrap().project, project);
        let limits = CodecLimits {
            max_encoded_bytes: 1,
            max_decoded_bytes: 1,
        };
        assert!(matches!(
            load_with_limits(&path, limits),
            Err(ProjectFileError::Document(_))
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_file_loader_refuses_fifo_without_opening_it() {
        use std::os::unix::ffi::OsStrExt;
        let path = oc_audio::private_fs::test_root().join("project-file-loader-fifo");
        let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            // SAFETY: native is a live NUL-terminated pathname, and mkfifo retains no pointer.
            unsafe { libc::mkfifo(native.as_ptr(), 0o600) },
            0
        );
        let result = load(&path);
        std::fs::remove_file(path).unwrap();
        assert!(matches!(result, Err(ProjectFileError::NonRegularFile)));
    }
}
