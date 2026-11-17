//! Private files under `~/.opencompose`; the audio-IO owner writes its
//! preferences with the same atomic writer.
//!
//! A test build never resolves the operator's home: [`config_dir`] answers a
//! process-keyed directory under the system temp directory. A test that needs a
//! specific file installs that path itself.
pub use oc_audio::private_fs::{
    append_private, ensure_dir, repair_file_mode, write_atomic, write_atomic_with,
};

/// The private configuration directory. In a test build this is the process
/// scratch root, never the operator's home.
pub fn config_dir() -> std::path::PathBuf {
    oc_audio::private_fs::config_dir()
}

use std::{io, path::Path};

/// Whether a lock is shared by readers or excludes every other holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockMode {
    Shared,
    Exclusive,
}

/// Whether [`lock`] waits for a holder to release or reports it at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockWait {
    Wait,
    NoWait,
}

/// The caller keeps this file open to hold the lock and leaves its path in place
/// so every process coordinates on the same file.
pub(crate) fn lock(path: &Path, mode: LockMode, wait: LockWait) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    lock_file(&file, mode, wait)?;
    Ok(file)
}

#[cfg(unix)]
fn lock_file(file: &std::fs::File, mode: LockMode, wait: LockWait) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mode = match mode {
        LockMode::Shared => libc::LOCK_SH,
        LockMode::Exclusive => libc::LOCK_EX,
    } | match wait {
        LockWait::Wait => 0,
        LockWait::NoWait => libc::LOCK_NB,
    };
    loop {
        // SAFETY: The borrowed File keeps this descriptor open through the call.
        if unsafe { libc::flock(file.as_raw_fd(), mode) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(windows)]
fn lock_file(file: &std::fs::File, mode: LockMode, wait: LockWait) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let flags = match mode {
        LockMode::Shared => 0,
        LockMode::Exclusive => LOCKFILE_EXCLUSIVE_LOCK,
    } | match wait {
        LockWait::Wait => 0,
        LockWait::NoWait => LOCKFILE_FAIL_IMMEDIATELY,
    };
    let mut offset = OVERLAPPED::default();
    // SAFETY: The File owns a synchronous handle and offset remains valid for
    // the call. Byte zero belongs to the separate, stable pool lock file.
    if unsafe { LockFileEx(file.as_raw_handle().cast(), flags, 0, 1, 0, &mut offset) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Err(io::ErrorKind::WouldBlock.into())
    } else {
        Err(error)
    }
}
