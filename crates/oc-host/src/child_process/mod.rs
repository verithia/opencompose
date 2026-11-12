//! Bounded, off-audio-thread control processes with inherited environment and directory.
//!
//! Process groups and Jobs provide cleanup ownership, not an execution sandbox.
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::Process;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::Process;

/// A command that explicitly inherits the caller's environment and directory.
/// Standard handles belong to the process owner, not to command configuration.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
}
impl CommandSpec {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
        }
    }
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
        self
    }
    pub fn get_program(&self) -> &OsStr {
        &self.program
    }
    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    /// A held stdin lifetime token and captured stdout/stderr.
    Capture,
    /// Null stdin/stdout and captured stderr.
    Diagnostics,
}

#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    pub timeout: Duration,
    pub cleanup_timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}
impl RunOptions {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            cleanup_timeout: Duration::from_secs(2),
            stdout_limit: 8 * 1024 * 1024,
            stderr_limit: 8 * 1024,
        }
    }
}

#[derive(Default)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    /// Only the bounded tail is retained; discarded diagnostic bytes are counted.
    pub stderr: Vec<u8>,
    pub stderr_bytes: u64,
}
impl std::fmt::Debug for CapturedOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedOutput")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_tail_bytes", &self.stderr.len())
            .field("stderr_bytes", &self.stderr_bytes)
            .finish()
    }
}
#[derive(Debug)]
pub struct RunOutput {
    pub status: ExitStatus,
    pub captured: CapturedOutput,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    InvalidRequest,
    Spawn,
    Poll,
    StdoutRead,
    StderrRead,
    OutputLimit,
    Deadline,
    Cancelled,
    Cleanup,
}
#[derive(Debug)]
pub struct RunFailure {
    pub kind: FailureKind,
    pub source: Option<io::Error>,
    pub cleanup_error: Option<io::Error>,
    pub status: Option<ExitStatus>,
    pub captured: CapturedOutput,
}
impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "child process {:?}", self.kind)?;
        if let Some(error) = &self.source {
            write!(
                f,
                ": {error} (kind={:?}, os_code={:?})",
                error.kind(),
                error.raw_os_error()
            )?;
        }
        if let Some(error) = &self.cleanup_error {
            write!(f, "; cleanup: {error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for RunFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .or(self.cleanup_error.as_ref())
            .map(|error| error as _)
    }
}

const MAX_STDOUT: usize = 16 * 1024 * 1024;
const MAX_STDERR: usize = 1024 * 1024;
const QUANTUM: usize = 4096;
const POLL_INTERVAL: Duration = Duration::from_millis(2);

fn read_piece(
    result: io::Result<usize>,
    bytes: &[u8],
    stdout: bool,
    capture: &mut CapturedOutput,
    options: RunOptions,
) -> Result<(bool, bool), (FailureKind, io::Error)> {
    let kind = if stdout {
        FailureKind::StdoutRead
    } else {
        FailureKind::StderrRead
    };
    let count = match result {
        Ok(0) => return Ok((true, false)),
        Ok(count) if count <= bytes.len() => count,
        Ok(_) => {
            return Err((
                kind,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pipe read exceeded supplied buffer",
                ),
            ));
        }
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || error.kind() == io::ErrorKind::Interrupted =>
        {
            return Ok((false, false));
        }
        Err(error) => return Err((kind, error)),
    };
    if stdout {
        let available = options.stdout_limit.saturating_sub(capture.stdout.len());
        capture
            .stdout
            .extend_from_slice(&bytes[..count.min(available)]);
        if count > available {
            return Err((
                FailureKind::OutputLimit,
                io::Error::new(io::ErrorKind::InvalidData, "stdout exceeds capture limit"),
            ));
        }
    } else {
        capture.stderr_bytes = capture
            .stderr_bytes
            .checked_add(count as u64)
            .ok_or_else(|| {
                (
                    FailureKind::OutputLimit,
                    io::Error::new(io::ErrorKind::InvalidData, "stderr byte count overflow"),
                )
            })?;
        if options.stderr_limit == 0 {
            return Ok((false, true));
        }
        let bytes = &bytes[..count];
        if count >= options.stderr_limit {
            capture.stderr.clear();
            capture
                .stderr
                .extend_from_slice(&bytes[count - options.stderr_limit..]);
        } else {
            let remove = (capture.stderr.len() + count).saturating_sub(options.stderr_limit);
            capture.stderr.drain(..remove);
            capture.stderr.extend_from_slice(bytes);
        }
    }
    Ok((false, true))
}

fn cleanup(process: &mut Process, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid cleanup deadline"))?;
    process.terminate()?;
    loop {
        if process.cleanup_complete()? {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "child cleanup did not complete within its budget",
            ));
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

/// Collect both pipes fairly under one execution-and-EOF deadline.
/// Cleanup has a separate explicit budget. No pipe-reader thread is created.
pub fn run(
    command: CommandSpec,
    mode: StdioMode,
    options: RunOptions,
    cancel: Option<&AtomicBool>,
) -> Result<RunOutput, RunFailure> {
    let fail = |kind, source, captured| RunFailure {
        kind,
        source,
        cleanup_error: None,
        status: None,
        captured,
    };
    let start = Instant::now();
    let deadline = start.checked_add(options.timeout);
    if options.timeout.is_zero()
        || deadline.is_none()
        || start.checked_add(options.cleanup_timeout).is_none()
        || options.stdout_limit > MAX_STDOUT
        || options.stderr_limit > MAX_STDERR
    {
        return Err(fail(
            FailureKind::InvalidRequest,
            Some(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid child deadline or capture capacity",
            )),
            CapturedOutput::default(),
        ));
    }
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(fail(
            FailureKind::Cancelled,
            None,
            CapturedOutput::default(),
        ));
    }
    let mut captured = CapturedOutput::default();
    if captured
        .stdout
        .try_reserve_exact(options.stdout_limit)
        .is_err()
        || captured
            .stderr
            .try_reserve_exact(options.stderr_limit)
            .is_err()
    {
        return Err(fail(
            FailureKind::InvalidRequest,
            Some(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "could not reserve bounded capture buffers",
            )),
            captured,
        ));
    }
    let mut process = Process::spawn(command, mode)
        .map_err(|error| fail(FailureKind::Spawn, Some(error), CapturedOutput::default()))?;
    let deadline = deadline.unwrap();
    let mut status = None;
    let mut stdout_done = mode == StdioMode::Diagnostics;
    let mut stderr_done = false;
    let mut terminated = false;
    let mut buffer = [0; QUANTUM];
    let failure = loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break Some((FailureKind::Cancelled, None));
        }
        if Instant::now() >= deadline {
            break Some((FailureKind::Deadline, None));
        }
        if status.is_none() {
            match process.try_wait() {
                Ok(value) => status = value,
                Err(error) => break Some((FailureKind::Poll, Some(error))),
            }
        }
        // Parent exit must not leave ordinary descendants holding captured pipes.
        // Unix retains the unreaped leader identity until this group termination.
        if status.is_some() && !terminated {
            if let Err(error) = process.terminate() {
                break Some((FailureKind::Cleanup, Some(error)));
            }
            terminated = true;
        }
        let mut progress = false;
        if !stdout_done {
            match read_piece(
                process.read_stdout(&mut buffer),
                &buffer,
                true,
                &mut captured,
                options,
            ) {
                Ok((eof, read)) => {
                    stdout_done = eof;
                    progress |= read;
                }
                Err((kind, error)) => break Some((kind, Some(error))),
            }
        }
        if !stderr_done {
            match read_piece(
                process.read_stderr(&mut buffer),
                &buffer,
                false,
                &mut captured,
                options,
            ) {
                Ok((eof, read)) => {
                    stderr_done = eof;
                    progress |= read;
                }
                Err((kind, error)) => break Some((kind, Some(error))),
            }
        }
        if status.is_some() && stdout_done && stderr_done {
            break None;
        }
        if !progress {
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    };
    let cleanup_error = cleanup(&mut process, options.cleanup_timeout).err();
    if let Some((kind, source)) = failure {
        return Err(RunFailure {
            kind,
            source,
            cleanup_error,
            status,
            captured,
        });
    }
    if let Some(error) = cleanup_error {
        return Err(RunFailure {
            kind: FailureKind::Cleanup,
            source: Some(error),
            cleanup_error: None,
            status,
            captured,
        });
    }
    Ok(RunOutput {
        status: status.expect("completed process has an observed status"),
        captured,
    })
}

#[cfg(test)]
mod tests;
