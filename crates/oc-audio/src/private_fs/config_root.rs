//! Startup-only selection of a dedicated private configuration profile.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MARKER: &str = ".opencompose-profile";
const MARKER_CONTENT: &[u8] = b"opencompose-private-config-v1\n";

#[derive(Debug)]
/// Why a private configuration root could not be resolved.
pub enum ConfigRootError {
    EmptyOverride,
    RelativeOverride,
    AlreadyInitialized,
    NotPrivateDirectory,
    UnsupportedLocation,
    Io(io::Error),
}

impl std::fmt::Display for ConfigRootError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOverride => formatter.write_str("OPENCOMPOSE_CONFIG_DIR must not be empty"),
            Self::RelativeOverride => formatter.write_str("OPENCOMPOSE_CONFIG_DIR must be an absolute path"),
            Self::AlreadyInitialized => formatter.write_str("The process configuration root is already selected"),
            Self::UnsupportedLocation => formatter.write_str("The explicit configuration profile must be contained in a supported private profile location"),
            Self::NotPrivateDirectory => formatter.write_str("The configuration override must be a dedicated private OpenCompose profile directory"),
            Self::Io(error) => write!(formatter, "Cannot initialize the configuration profile: {error}"),
        }
    }
}
impl std::error::Error for ConfigRootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for ConfigRootError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Default)]
pub(super) struct ProfilePolicy {
    pub(super) test_root: Option<PathBuf>,
    pub(super) container: Option<PathBuf>,
}

struct SelectedRoot {
    path: PathBuf,
    explicit: bool,
}

#[derive(Default)]
pub(super) struct ConfigRoot {
    selected: OnceLock<SelectedRoot>,
    admission: Mutex<()>,
}

impl ConfigRoot {
    pub(super) const fn new() -> Self {
        Self {
            selected: OnceLock::new(),
            admission: Mutex::new(()),
        }
    }

    pub(super) fn initialize(
        &self,
        supplied: Option<&OsStr>,
        fallback: impl FnOnce() -> PathBuf,
        policy: ProfilePolicy,
    ) -> Result<(), ConfigRootError> {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.selected.get().is_some() {
            return Err(ConfigRootError::AlreadyInitialized);
        }
        let explicit = policy.test_root.is_none() && supplied.is_some();
        let root = if let Some(root) = policy.test_root {
            root
        } else if let Some(value) = supplied {
            if value.is_empty() {
                return Err(ConfigRootError::EmptyOverride);
            }
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(ConfigRootError::RelativeOverride);
            }
            validate_location(&path, policy.container.as_deref())?;
            prepare_dedicated_root(&path)?;
            path
        } else {
            fallback()
        };
        // Both initial selection paths share the admission lock.
        self.selected
            .set(SelectedRoot {
                path: root,
                explicit,
            })
            .map_err(|_| ConfigRootError::AlreadyInitialized)
    }

    pub(super) fn explicit_path(&self) -> Option<PathBuf> {
        self.selected
            .get()
            .filter(|root| root.explicit)
            .map(|root| root.path.clone())
    }

    pub(super) fn resolve(&self, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
        if let Some(root) = self.selected.get() {
            return root.path.clone();
        }
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.selected
            .get_or_init(|| SelectedRoot {
                path: fallback(),
                explicit: false,
            })
            .path
            .clone()
    }
}

fn validate_location(path: &Path, container: Option<&Path>) -> Result<(), ConfigRootError> {
    #[cfg(windows)]
    if container.is_none() {
        return Err(ConfigRootError::UnsupportedLocation);
    }
    if let Some(container) = container {
        let parent = path
            .parent()
            .ok_or(ConfigRootError::UnsupportedLocation)?
            .canonicalize()?;
        let name = path
            .file_name()
            .ok_or(ConfigRootError::UnsupportedLocation)?;
        let container = container.canonicalize()?;
        if !parent.join(name).starts_with(container) {
            return Err(ConfigRootError::UnsupportedLocation);
        }
    }
    Ok(())
}

fn private_metadata(metadata: &std::fs::Metadata, directory: bool) -> bool {
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid reads the calling process identity and takes no pointers.
        let uid = unsafe { libc::geteuid() };
        metadata.uid() == uid && metadata.mode() & 0o077 == 0
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reparse points cannot establish ownership of the directory or marker.
        metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn verify_dedicated_root(path: &Path) -> Result<(), ConfigRootError> {
    let root = std::fs::symlink_metadata(path)?;
    if !private_metadata(&root, true) {
        return Err(ConfigRootError::NotPrivateDirectory);
    }
    let marker = path.join(MARKER);
    let metadata = std::fs::symlink_metadata(&marker).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ConfigRootError::NotPrivateDirectory
        } else {
            ConfigRootError::Io(error)
        }
    })?;
    if !private_metadata(&metadata, false) || metadata.len() != MARKER_CONTENT.len() as u64 {
        return Err(ConfigRootError::NotPrivateDirectory);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(marker)?;
    if !private_metadata(&file.metadata()?, false) {
        return Err(ConfigRootError::NotPrivateDirectory);
    }
    let mut contents = Vec::with_capacity(MARKER_CONTENT.len() + 1);
    file.take((MARKER_CONTENT.len() + 1) as u64)
        .read_to_end(&mut contents)?;
    if contents != MARKER_CONTENT {
        return Err(ConfigRootError::NotPrivateDirectory);
    }
    Ok(())
}

fn prepare_dedicated_root(path: &Path) -> Result<(), ConfigRootError> {
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {
            let marker = path.join(MARKER);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let result = (|| -> io::Result<()> {
                let mut file = options.open(&marker)?;
                file.write_all(MARKER_CONTENT)?;
                file.sync_all()
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(marker);
                let _ = std::fs::remove_dir(path);
            }
            result?;
            verify_dedicated_root(path)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => verify_dedicated_root(path),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(container: &Path) -> ProfilePolicy {
        ProfilePolicy {
            test_root: None,
            container: Some(container.to_path_buf()),
        }
    }

    #[test]
    fn config_profile_owner_creates_and_reopens_only_dedicated_roots() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profile");
        let owner = ConfigRoot::default();
        owner
            .initialize(
                Some(path.as_os_str()),
                || panic!("explicit profile must not read the fallback"),
                policy(temp.path()),
            )
            .unwrap();
        assert_eq!(owner.resolve(|| panic!("selected root is immutable")), path);
        assert_eq!(owner.explicit_path(), Some(path.clone()));
        std::fs::write(path.join("gui-settings.yaml"), b"scale: 1").unwrap();
        let reopened = ConfigRoot::default();
        reopened
            .initialize(
                Some(path.as_os_str()),
                || panic!("no fallback"),
                policy(temp.path()),
            )
            .unwrap();
        assert_eq!(reopened.resolve(|| panic!("no fallback")), path);
        assert_eq!(
            std::fs::read(path.join("gui-settings.yaml")).unwrap(),
            b"scale: 1"
        );
    }
    #[test]
    fn config_profile_owner_refuses_invalid_and_late_overrides_without_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let owner = ConfigRoot::default();
        assert!(matches!(
            owner.initialize(
                Some(OsStr::new("")),
                || panic!("no fallback"),
                policy(temp.path())
            ),
            Err(ConfigRootError::EmptyOverride)
        ));
        assert!(matches!(
            owner.initialize(
                Some(OsStr::new("relative")),
                || panic!("no fallback"),
                policy(temp.path())
            ),
            Err(ConfigRootError::RelativeOverride)
        ));
        let fallback = temp.path().join("default");
        assert_eq!(owner.resolve(|| fallback.clone()), fallback);
        let late = temp.path().join("late");
        assert!(matches!(
            owner.initialize(
                Some(late.as_os_str()),
                || panic!("no fallback"),
                policy(temp.path())
            ),
            Err(ConfigRootError::AlreadyInitialized)
        ));
        assert!(!late.exists());
        assert_eq!(owner.resolve(|| panic!("no fallback")), fallback);
    }
    #[test]
    fn config_profile_owner_tests_ignore_inherited_overrides_and_preserve_default() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        let owner = ConfigRoot::default();
        owner
            .initialize(
                Some(OsStr::new("")),
                || panic!("test must not resolve operator"),
                ProfilePolicy {
                    test_root: Some(scratch.clone()),
                    container: None,
                },
            )
            .unwrap();
        assert_eq!(owner.resolve(|| panic!("no operator")), scratch);
        let default_owner = ConfigRoot::default();
        let fallback = temp.path().join("unchanged-default");
        default_owner
            .initialize(None, || fallback.clone(), policy(temp.path()))
            .unwrap();
        assert_eq!(
            default_owner.resolve(|| panic!("no second resolution")),
            fallback
        );
        assert_eq!(default_owner.explicit_path(), None);
        assert!(
            !fallback.exists(),
            "default resolution does not mutate the filesystem"
        );
    }
    #[cfg(unix)]
    #[test]
    fn config_profile_owner_refusal_preserves_existing_permissions_and_contents() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("unrelated");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(existing.join("keep"), b"untouched").unwrap();
        let owner = ConfigRoot::default();
        assert!(matches!(
            owner.initialize(
                Some(existing.as_os_str()),
                || panic!("no operator fallback"),
                policy(temp.path())
            ),
            Err(ConfigRootError::NotPrivateDirectory)
        ));
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(std::fs::read(existing.join("keep")).unwrap(), b"untouched");
        assert!(!existing.join(MARKER).exists());
        let link = temp.path().join("alias");
        symlink(&existing, &link).unwrap();
        assert!(matches!(
            owner.initialize(
                Some(link.as_os_str()),
                || panic!("no fallback"),
                policy(temp.path())
            ),
            Err(ConfigRootError::NotPrivateDirectory)
        ));
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(
                owner.initialize(
                    Some(existing.as_os_str()),
                    || panic!("no fallback"),
                    policy(temp.path())
                ),
                Err(ConfigRootError::NotPrivateDirectory)
            ),
            "private mode alone is not a profile marker"
        );
    }
    #[test]
    fn config_profile_owner_canonical_parent_containment_refuses_escape_without_creation() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("container");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&container).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let escaped = container.join("..").join("outside").join("profile");
        let owner = ConfigRoot::default();
        assert!(matches!(
            owner.initialize(
                Some(escaped.as_os_str()),
                || panic!("no fallback"),
                policy(&container)
            ),
            Err(ConfigRootError::UnsupportedLocation)
        ));
        assert!(!outside.join("profile").exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, container.join("link")).unwrap();
            let linked = container.join("link/profile");
            assert!(matches!(
                owner.initialize(
                    Some(linked.as_os_str()),
                    || panic!("no fallback"),
                    policy(&container)
                ),
                Err(ConfigRootError::UnsupportedLocation)
            ));
            assert!(!outside.join("profile").exists());
        }
    }
}
