//! Guarded encoding followed by rollback-capable export publication.
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tempfile::{TempDir, TempPath};

pub(super) struct StagedStemExport {
    directory: TempDir,
    destination: PathBuf,
}

struct Publication {
    source: PathBuf,
    destination: PathBuf,
    backup: Option<TempPath>,
    published: bool,
}

fn existing_output(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            Err(format!("export file '{}' is a directory", path.display()))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

impl StagedStemExport {
    pub(super) fn new(destination: &Path) -> Result<Self, String> {
        let destination = std::path::absolute(destination).map_err(|error| error.to_string())?;
        let parent = destination
            .parent()
            .ok_or("stem directory needs a parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        if destination.exists() && !destination.is_dir() {
            return Err("stem export destination is not a directory".into());
        }
        // Resolve an existing destination (including a directory link) so the
        // staged files and their final renames stay on the same filesystem.
        let destination = if destination.exists() {
            destination
                .canonicalize()
                .map_err(|error| error.to_string())?
        } else {
            parent
                .canonicalize()
                .map_err(|error| error.to_string())?
                .join(
                    destination
                        .file_name()
                        .ok_or("stem directory needs a name")?,
                )
        };
        let directory = tempfile::Builder::new()
            .prefix(".oc-stems-stage-")
            .tempdir_in(destination.parent().unwrap())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            directory,
            destination,
        })
    }

    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }

    pub(super) fn publish(
        self,
        manifest: &str,
        mix_output: Option<(&str, &Path)>,
    ) -> Result<PathBuf, String> {
        self.publish_with_rename(manifest, mix_output, |from, to| fs::rename(from, to))
    }

    fn publish_with_rename(
        self,
        manifest: &str,
        mix_output: Option<(&str, &Path)>,
        mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<PathBuf, String> {
        let created = !self.destination.exists();
        if created {
            fs::create_dir(&self.destination).map_err(|error| error.to_string())?;
        }
        let result = self.publish_files(manifest, mix_output, &mut rename);
        if result.is_err() && created {
            fs::remove_dir(&self.destination).map_err(|cleanup| {
                format!(
                    "{}; export directory cleanup failed: {cleanup}",
                    result.as_ref().unwrap_err()
                )
            })?;
        }
        result
    }

    fn publish_files(
        &self,
        manifest: &str,
        mix_output: Option<(&str, &Path)>,
        rename: &mut impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<PathBuf, String> {
        let mut files = fs::read_dir(self.path())
            .map_err(|error| error.to_string())?
            .map(|entry| entry.map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        files.sort_by_key(|file| (file.file_name() == OsStr::new(manifest), file.file_name()));
        if !files
            .iter()
            .any(|file| file.file_name() == OsStr::new(manifest))
        {
            return Err("staged export has no manifest".into());
        }
        let mut items = Vec::with_capacity(files.len() + usize::from(mix_output.is_some()));
        for file in files {
            if !file
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err("staged stem export contains a non-file entry".into());
            }
            items.push(Publication {
                source: file.path(),
                destination: self.destination.join(file.file_name()),
                backup: None,
                published: false,
            });
        }
        // The CLI's standalone mix belongs to this same transaction. Its
        // encoded bytes are reused instead of metering and encoding PCM twice.
        let mut extra = None;
        if let Some((mix_name, target)) = mix_output {
            let target = std::path::absolute(target).map_err(|error| error.to_string())?;
            let parent = target.parent().ok_or("mix output needs a parent")?;
            let target = parent
                .canonicalize()
                .map_err(|error| error.to_string())?
                .join(target.file_name().ok_or("mix output needs a file name")?);
            let existing = items.iter().find(|item| {
                item.destination
                    .parent()
                    .and_then(|parent| parent.canonicalize().ok())
                    .map(|parent| parent.join(item.destination.file_name().unwrap()))
                    == Some(target.clone())
            });
            if let Some(existing) = existing {
                if existing.source.file_name() != Some(OsStr::new(mix_name)) {
                    return Err("mix output collides with another stem export file".into());
                }
            } else {
                let file = tempfile::Builder::new()
                    .prefix(".oc-mix-stage-")
                    .tempfile_in(parent)
                    .map_err(|error| error.to_string())?;
                fs::copy(self.path().join(mix_name), file.path())
                    .map_err(|error| error.to_string())?;
                let path = file.into_temp_path();
                items.insert(
                    items.len() - 1,
                    Publication {
                        source: path.to_path_buf(),
                        destination: target,
                        backup: None,
                        published: false,
                    },
                );
                extra = Some(path);
            }
        }
        // Refuse directory collisions before moving any existing output.
        for item in &items {
            existing_output(&item.destination)?;
        }
        let result = (|| {
            for item in &mut items {
                if existing_output(&item.destination)? {
                    let backup = tempfile::Builder::new()
                        .prefix(".oc-stems-backup-")
                        .tempfile_in(item.destination.parent().unwrap())
                        .map_err(|error| error.to_string())?
                        .into_temp_path();
                    fs::remove_file(&backup).map_err(|error| error.to_string())?;
                    rename(&item.destination, &backup).map_err(|error| error.to_string())?;
                    item.backup = Some(backup);
                }
            }
            for item in &mut items {
                rename(&item.source, &item.destination).map_err(|error| error.to_string())?;
                item.published = true;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for item in items.iter_mut().rev() {
                if item.published
                    && let Err(error) = fs::remove_file(&item.destination)
                {
                    failures.push(error.to_string());
                }
                if let Some(backup) = item.backup.take()
                    && let Err(error) = rename(&backup, &item.destination)
                {
                    failures.push(format!(
                        "{error}; recovery copy retained at '{}'",
                        backup.display()
                    ));
                    // Failed restoration must retain the user's original bytes.
                    std::mem::forget(backup);
                }
            }
            return Err(if failures.is_empty() {
                error
            } else {
                format!("{error}; rollback failed: {}", failures.join("; "))
            });
        }
        drop(extra);
        Ok(self.destination.join(manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_publication_failed_restore_retains_original_recovery_bytes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("stems");
        fs::create_dir(&output).unwrap();
        let output = output.canonicalize().unwrap();
        fs::write(output.join("mix.wav"), b"original").unwrap();
        let staged = StagedStemExport::new(&output).unwrap();
        fs::write(staged.path().join("mix.wav"), b"new").unwrap();
        fs::write(staged.path().join("stems.json"), b"manifest").unwrap();
        let stage_path = staged.path().to_path_buf();
        let error = staged
            .publish_with_rename("stems.json", None, |from, to| {
                let publishing_manifest = from.parent() == Some(stage_path.as_path())
                    && from.file_name() == Some(OsStr::new("stems.json"));
                let restoring_mix = from
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".oc-stems-backup-"))
                    && to == output.join("mix.wav");
                if publishing_manifest || restoring_mix {
                    return Err(io::Error::other("injected I/O failure"));
                }
                fs::rename(from, to)
            })
            .unwrap_err();
        assert!(error.contains("recovery copy retained"), "{error}");
        let backups = fs::read_dir(&output)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), b"original");
    }

    #[test]
    fn stem_publication_failure_restores_existing_and_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("stems");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("mix.wav"), b"old mix").unwrap();
        fs::write(output.join("stems.json"), b"old manifest").unwrap();
        fs::write(output.join("unrelated"), b"keep").unwrap();
        let standalone = root.path().join("opening.wav");
        fs::write(&standalone, b"old standalone").unwrap();
        let staged = StagedStemExport::new(&output).unwrap();
        fs::write(staged.path().join("mix.wav"), b"new mix").unwrap();
        fs::write(staged.path().join("stem.wav"), b"new stem").unwrap();
        fs::write(staged.path().join("stems.json"), b"new manifest").unwrap();
        let stage_path = staged.path().to_path_buf();
        let mut failed = false;
        assert!(
            staged
                .publish_with_rename("stems.json", Some(("mix.wav", &standalone)), |from, to| {
                    if !failed
                        && from.parent() == Some(stage_path.as_path())
                        && from.file_name() == Some(OsStr::new("stems.json"))
                    {
                        failed = true;
                        return Err(io::Error::other("injected publication failure"));
                    }
                    fs::rename(from, to)
                })
                .is_err()
        );
        assert!(failed);
        assert_eq!(fs::read(output.join("mix.wav")).unwrap(), b"old mix");
        assert_eq!(
            fs::read(output.join("stems.json")).unwrap(),
            b"old manifest"
        );
        assert_eq!(fs::read(output.join("unrelated")).unwrap(), b"keep");
        assert_eq!(fs::read(&standalone).unwrap(), b"old standalone");
        assert!(!output.join("stem.wav").exists());
        assert_eq!(fs::read_dir(&output).unwrap().count(), 3);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
