use super::*;

struct Move {
    source: PathBuf,
    destination: PathBuf,
    backup: PathBuf,
}

impl PreparedCompanion {
    /// Publish companion data and bundles as one rollback operation.
    pub fn publish(self, overwrite: bool) -> Result<InstallOutcome, Error> {
        self.publish_with(overwrite, |source, destination| {
            fs::rename(source, destination)
        })
    }

    pub(super) fn publish_with(
        self,
        overwrite: bool,
        rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<InstallOutcome, Error> {
        self.paths.check_unchanged()?;
        let _lock = scope_lock(&self.paths.content_dir, true)?;
        self.paths.check_unchanged()?;
        let previous = self.plugin_stage.path().join(".previous");
        let mut moves = Vec::new();
        for (index, entry) in fs::read_dir(self.plugin_stage.path())
            .map_err(destination)?
            .enumerate()
        {
            let entry = entry.map_err(destination)?;
            moves.push(Move {
                source: entry.path(),
                destination: self.paths.plugin_dir.join(entry.file_name()),
                backup: previous.join(index.to_string()),
            });
        }
        let parent = self
            .paths
            .content_dir
            .parent()
            .ok_or_else(|| destination("Invalid content destination"))?;
        let content_backup =
            crate::temporary::directory_in(parent, ".ocp-factory-backup-").map_err(destination)?;
        moves.push(Move {
            source: self.content_stage.path().to_path_buf(),
            destination: self.paths.content_dir.clone(),
            backup: content_backup.path().join("content"),
        });
        for item in &moves {
            match fs::symlink_metadata(&item.destination) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(destination(format!(
                        "Refusing to replace symlink {}",
                        item.destination.display()
                    )));
                }
                Ok(_) if !overwrite => {
                    return Ok(InstallOutcome::NeedsConfirmation {
                        path: item.destination.display().to_string(),
                        message: "Plugin or factory content exists. Replace both together?".into(),
                    });
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(destination(e)),
            }
        }
        fs::create_dir(&previous).map_err(destination)?;
        let mut completed: Vec<(&Move, bool)> = Vec::new();
        for item in &moves {
            let mut backed_up = false;
            let result = (|| {
                if item.destination.exists() {
                    rename(&item.destination, &item.backup)?;
                    backed_up = true;
                }
                rename(&item.source, &item.destination)
            })();
            if let Err(error) = result {
                let mut failures = Vec::new();
                if backed_up && let Err(e) = rename(&item.backup, &item.destination) {
                    failures.push(e.to_string());
                }
                for (previous, backed_up) in completed.iter().rev() {
                    if let Err(e) = remove(&previous.destination) {
                        failures.push(e.to_string());
                        continue;
                    }
                    if *backed_up && let Err(e) = rename(&previous.backup, &previous.destination) {
                        failures.push(e.to_string());
                    }
                }
                if !failures.is_empty() {
                    let plugins = self.plugin_stage.keep();
                    let content = content_backup.keep();
                    return Err(Error::new(
                        ErrorCode::Rollback,
                        format!(
                            "Publication failed: {error}; rollback failed: {}; backups retained at {} and {}",
                            failures.join("; "),
                            plugins.display(),
                            content.display()
                        ),
                    ));
                }
                return Err(Error::new(
                    ErrorCode::Publication,
                    format!("Publication failed; previous plugin and content restored: {error}"),
                ));
            }
            completed.push((item, backed_up));
        }
        Ok(InstallOutcome::Installed {
            path: self.paths.plugin_dir.display().to_string(),
            message: format!(
                "Installed {} and its verified factory content into {}.",
                self.name,
                self.paths.content_dir.display()
            ),
        })
    }
}

fn remove(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
