//! Private, exclusively allocated temporary resources shared by installers.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::{NamedTempFile, TempDir};

fn validate_prefix(prefix: &str) -> io::Result<()> {
    if prefix.is_empty()
        || prefix.len() > 80
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid temporary resource prefix",
        ));
    }
    Ok(())
}

/// Atomically reserves a private directory. The guard removes only its own allocation.
pub fn directory_in(parent: &Path, prefix: &str) -> io::Result<TempDir> {
    validate_prefix(prefix)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

/// Atomically reserves a private file without truncating a preexisting candidate.
pub fn file_in(parent: &Path, prefix: &str) -> io::Result<NamedTempFile> {
    validate_prefix(prefix)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o600));
    }
    builder.tempfile_in(parent)
}

/// An installer remains guarded until its external handoff succeeds.
#[derive(Debug)]
pub struct PreparedInstaller {
    directory: TempDir,
    path: PathBuf,
}
impl PreparedInstaller {
    /// Path of the staged installer.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retains the directory only after the opener accepts the installer. The
    /// external process may read it after its launcher has already returned.
    pub fn handoff<E>(self, open: impl FnOnce(&Path) -> Result<(), E>) -> Result<PathBuf, E> {
        open(&self.path)?;
        let path = self.path;
        let _ = self.directory.keep();
        Ok(path)
    }
}

/// Writes an installer into a new private directory, preserving its file extension.
pub fn prepare_installer_in(
    parent: &Path,
    name: &str,
    bytes: &[u8],
) -> io::Result<PreparedInstaller> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
                )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsafe installer filename",
        ));
    }
    let directory = directory_in(parent, "opencompose-installer-")?;
    let path = directory.path().join(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options.open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    Ok(PreparedInstaller { directory, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn parallel_resources_with_fixed_names_never_share_paths_or_bytes() {
        let parent = tempfile::tempdir().unwrap();
        let allocations = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..32)
                .map(|number| {
                    let parent = parent.path();
                    scope.spawn(move || {
                        let bytes = vec![number; 16];
                        let installer = prepare_installer_in(parent, "setup.pkg", &bytes).unwrap();
                        assert_eq!(fs::read(installer.path()).unwrap(), bytes);
                        let directory = directory_in(parent, ".stage-").unwrap();
                        let mut file = file_in(parent, ".probe-").unwrap();
                        file.write_all(&bytes).unwrap();
                        assert_eq!(fs::read(file.path()).unwrap(), bytes);
                        (installer, directory, file)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        let paths: HashSet<_> = allocations
            .iter()
            .flat_map(|(installer, directory, file)| {
                [
                    installer.path().parent().unwrap().to_path_buf(),
                    directory.path().to_path_buf(),
                    file.path().to_path_buf(),
                ]
            })
            .collect();
        assert_eq!(paths.len(), 96);
        let unrelated = parent.path().join("unrelated");
        fs::write(&unrelated, b"preserve").unwrap();
        drop(allocations);
        assert!(paths.iter().all(|path| !path.exists()));
        assert_eq!(fs::read(unrelated).unwrap(), b"preserve");
    }

    #[test]
    fn failed_handoff_cleans_and_successful_handoff_retains_its_private_file() {
        let parent = tempfile::tempdir().unwrap();
        let failed = prepare_installer_in(parent.path(), "setup.exe", b"first").unwrap();
        let failed_directory = failed.path().parent().unwrap().to_path_buf();
        assert_eq!(
            failed
                .handoff(|_| Err::<(), _>("opener refused"))
                .unwrap_err(),
            "opener refused"
        );
        assert!(!failed_directory.exists());
        let successful = prepare_installer_in(parent.path(), "setup.exe", b"second").unwrap();
        let retained = successful
            .handoff(|path| {
                assert_eq!(fs::read(path).unwrap(), b"second");
                Ok::<(), io::Error>(())
            })
            .unwrap();
        assert_eq!(fs::read(retained).unwrap(), b"second");
    }

    #[test]
    fn concurrent_writability_probes_leave_existing_files_unchanged() {
        let parent = tempfile::tempdir().unwrap();
        let sentinel = parent.path().join(".ocp-write-probe-existing");
        fs::write(&sentinel, b"preserve").unwrap();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..32)
                .map(|_| {
                    let path = parent.path();
                    scope.spawn(move || assert!(crate::path_is_writable(&path.join("not-created"))))
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
        });
        assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
        assert!(!crate::path_is_writable(&sentinel));
    }

    #[test]
    fn invalid_names_and_prefixes_do_not_allocate_resources() {
        let parent = tempfile::tempdir().unwrap();
        for name in [
            "",
            ".",
            "..",
            "../setup.pkg",
            "a/b.exe",
            "a\\b.exe",
            "C:setup.exe",
            "bad\nname",
            "bad?name",
        ] {
            assert_eq!(
                prepare_installer_in(parent.path(), name, b"payload")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        for prefix in ["", "../", "a/b", "a\\b", "bad\nname"] {
            assert_eq!(
                directory_in(parent.path(), prefix).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                file_in(parent.path(), prefix).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn allocations_are_private_to_the_current_user() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let directory = directory_in(parent.path(), ".private-").unwrap();
        let file = file_in(parent.path(), ".private-").unwrap();
        let installer = prepare_installer_in(parent.path(), "setup.pkg", b"payload").unwrap();
        for path in [directory.path(), installer.path().parent().unwrap()] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        }
        for path in [file.path(), installer.path()] {
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
        }
    }
}
