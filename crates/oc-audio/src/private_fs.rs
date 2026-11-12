//! Private files under `~/.opencompose`: 0700 directories, 0600 files and
//! replace-by-rename. Windows uses the profile ACL instead of file modes.
//!
//! Rule: a test build never resolves the operator's home. [`config_dir`]
//! answers a process-private directory under the system temp directory. A test
//! that needs a specific file installs that path explicitly.
//!
//! `cfg(test)` does not reach a dependency or an integration test's library, so
//! [`running_under_test`] also recognises a Cargo test binary by its location
//! under `target/<profile>/deps`.
mod config_root;
pub use config_root::ConfigRootError;
use config_root::{ConfigRoot, ProfilePolicy};

static CONFIG_ROOT: ConfigRoot = ConfigRoot::new();

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Whether the current process is a test binary: a unit test knows at compile
/// time, an integration test or a dependency's test build is recognised by the
/// executable sitting under `target/<profile>/deps`.
pub fn running_under_test() -> bool {
    if cfg!(test) {
        return true;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .and_then(|dir| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "deps")
        })
        .unwrap_or(false)
}

/// The operator's home, or [`test_root`] in a test build.
pub fn home_dir() -> Option<PathBuf> {
    if running_under_test() {
        return Some(test_root());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The private configuration directory, `~/.opencompose`. A test build never
/// resolves the operator's home; it uses [`test_config_dir`] instead.
pub fn config_dir() -> PathBuf {
    if running_under_test() {
        return test_config_dir();
    }
    CONFIG_ROOT.resolve(default_config_dir)
}

/// Explicit startup-selected profile, without resolving the default home.
/// Callers with their own platform defaults (such as diagnostics) can opt into
/// profile isolation while leaving ordinary unset behavior unchanged.
pub fn explicit_config_dir() -> Option<PathBuf> {
    CONFIG_ROOT.explicit_path()
}

/// Optional preference location: an explicit profile works without a home;
/// otherwise preserve the absence of a default when no home is available.
pub fn available_config_dir() -> Option<PathBuf> {
    explicit_config_dir().or_else(|| home_dir().map(|_| config_dir()))
}

fn default_config_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencompose")
}

/// Select the process profile before helpers, configuration reads or worker
/// startup. `OPENCOMPOSE_CONFIG_DIR` must name an absolute dedicated directory:
/// a new root is created privately; reuse requires its private profile marker.
/// Existing unrelated directories are refused without changing permissions.
/// Windows overrides are supported only beneath canonical LOCALAPPDATA that
/// is itself beneath canonical USERPROFILE, using that profile's inherited ACL.
/// The environment is read, never mutated. Test processes always keep their
/// private scratch directory, regardless of an inherited override.
pub fn initialize_config_root_from_env() -> Result<(), ConfigRootError> {
    let test = running_under_test().then(test_config_dir);
    let supplied = if test.is_some() {
        None
    } else {
        std::env::var_os("OPENCOMPOSE_CONFIG_DIR")
    };
    CONFIG_ROOT.initialize(
        supplied.as_deref(),
        default_config_dir,
        ProfilePolicy {
            test_root: test,
            container: explicit_profile_container(),
        },
    )
}

#[cfg(not(windows))]
fn explicit_profile_container() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn explicit_profile_container() -> Option<PathBuf> {
    let profile = PathBuf::from(std::env::var_os("USERPROFILE")?)
        .canonicalize()
        .ok()?;
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .canonicalize()
        .ok()?;
    local.starts_with(&profile).then_some(local)
}

static TEST_ROOT: OnceLock<Mutex<Option<tempfile::TempDir>>> = OnceLock::new();

extern "C" fn clean_test_root_at_exit() {
    if let Some(root) = TEST_ROOT.get() {
        let mut root = root.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(dir) = root.take()
            && let Err(error) = dir.close()
        {
            log::warn!("Could not remove the test scratch home: {error}");
        }
    }
}

/// Remove the process-private test root before a guarded process exit.
///
/// A hosted plugin can end the process through `_exit` before ordinary
/// `atexit` handlers run. The host calls this cleanup first in a test build so
/// the scratch home does not outlive the test process. A test that simulates a
/// crashed writer sets `OCP_TEST_KEEP_SCRATCH` so its child leaves the fixture
/// for the parent to inspect.
pub fn cleanup_test_root() {
    if std::env::var_os("OCP_TEST_KEEP_SCRATCH").is_some() {
        return;
    }
    clean_test_root_at_exit();
}

/// Process-private scratch home, shared by this test binary until normal exit.
pub fn test_root() -> PathBuf {
    let root = TEST_ROOT.get_or_init(|| {
        let parent = std::path::absolute(std::env::temp_dir())
            .expect("resolve the system temporary directory");
        let dir = tempfile::Builder::new()
            .prefix("opencompose-test-")
            .tempdir_in(parent)
            .expect("create the test scratch home");
        // SAFETY: The callback has C ABI and owns no borrowed data. Static
        // guards are not dropped by Rust when the test executable exits.
        assert_eq!(unsafe { libc::atexit(clean_test_root_at_exit) }, 0);
        Mutex::new(Some(dir))
    });
    root.lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .expect("test scratch home is already closing")
        .path()
        .to_path_buf()
}

/// [`config_dir`] inside [`test_root`]. A caller that resolves a path of its own
/// uses this instead of the operator's home.
pub fn test_config_dir() -> PathBuf {
    test_root().join(".opencompose")
}

/// Create a private directory, mode 0700 on Unix.
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Reapply private file permissions, mode 0600 on Unix.
pub fn repair_file_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Appends one whole record to a private file, creating it 0600. One `write_all`
/// on an `O_APPEND` handle keeps the record contiguous; `sync_data` makes it
/// durable before the caller reports the turn saved.
pub fn append_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    ensure_dir(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    repair_file_mode(path);
    file.write_all(bytes)?;
    file.sync_data()
}

/// Write bytes to a private file and publish it atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    write_atomic_with(path, |file| file.write_all(bytes))
}

/// Write a private file incrementally and publish it only after the writer succeeds.
pub fn write_atomic_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    ensure_dir(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("opencompose");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    write_then_rename(path, &temporary, write)
}

/// Write `bytes` to `temporary`, then publish it at `path` with one `rename`.
///
/// `std::fs::rename` replaces an existing file on every platform; on Windows it
/// is `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`. A failed rename therefore
/// returns its original error and leaves the old file in place, so there is
/// deliberately no delete-then-rename path.
fn write_then_rename(
    path: &Path,
    temporary: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temporary)?;
    let written = write(&mut file).and_then(|()| file.sync_all());
    drop(file);
    let write = written.and_then(|()| std::fs::rename(temporary, path));
    if write.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    write
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "opencompose-private-fs-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn a_test_build_keeps_the_private_directory_out_of_the_operator_home() {
        assert!(running_under_test());
        let dir = config_dir();
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "the test build resolved the operator's home: {}",
            dir.display()
        );
        assert_eq!(dir, test_config_dir());
        assert_eq!(home_dir(), Some(test_root()));
    }

    #[test]
    fn the_test_scratch_home_is_removed_at_process_exit() {
        const CHILD: &str = "OCP_TEST_SCRATCH_HOME_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let root = test_root();
            std::fs::write(root.join("fixture"), b"test").unwrap();
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_fs::tests::the_test_scratch_home_is_removed_at_process_exit",
            ])
            .env(CHILD, "1")
            .env("TMPDIR", temp.path())
            .env("TEMP", temp.path())
            .env("TMP", temp.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(result.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn private_directories_and_files_are_locked_down_and_replaced_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fixture_dir();
        ensure_dir(&dir).unwrap();
        let mode = |path: &Path| path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        let path = dir.join("ai-providers.yaml");
        write_atomic(&path, b"first").unwrap();
        assert_eq!(mode(&path), 0o600);
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        repair_file_mode(&path);
        assert_eq!(mode(&path), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_incremental_write_keeps_the_existing_file_and_cleans_its_stage() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.wav");
        write_atomic(&path, b"old").unwrap();
        let result = write_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("write failed"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_write_keeps_the_existing_file() {
        let dir = fixture_dir();
        ensure_dir(&dir).unwrap();
        let path = dir.join("ai-providers.yaml");
        write_atomic(&path, b"old").unwrap();

        // A directory where the temporary file belongs makes the write fail
        // before the rename, which is the case that must not lose the old file.
        let temporary = dir.join("blocked.tmp");
        std::fs::create_dir(&temporary).unwrap();
        assert!(
            write_then_rename(&path, &temporary, |file| std::io::Write::write_all(
                file, b"new"
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"old");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
