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

/// The caller keeps this file open to hold the lock and leaves its path in place
/// so every process coordinates on the same file.
pub(crate) fn lock(path: &Path, shared: bool, nonblocking: bool) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    lock_file(&file, shared, nonblocking)?;
    Ok(file)
}

#[cfg(unix)]
fn lock_file(file: &std::fs::File, shared: bool, nonblocking: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mode = if shared { libc::LOCK_SH } else { libc::LOCK_EX }
        | if nonblocking { libc::LOCK_NB } else { 0 };
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
fn lock_file(file: &std::fs::File, shared: bool, nonblocking: bool) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let flags = if shared { 0 } else { LOCKFILE_EXCLUSIVE_LOCK }
        | if nonblocking {
            LOCKFILE_FAIL_IMMEDIATELY
        } else {
            0
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
