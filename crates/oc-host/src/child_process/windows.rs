//! Windows child ownership with atomic Job admission and bounded pipe reads.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Error};
use std::mem::{size_of, size_of_val};
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::ExitStatusExt,
};
use std::process::ExitStatus;
use std::ptr::{null, null_mut};

use super::{CommandSpec, StdioMode};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    SearchPathW,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// Owns the Job before creation and all parent pipe ends after successful admission.
pub struct Process {
    process: OwnedHandle,
    job: OwnedHandle,
    stdin: Option<OwnedHandle>,
    stdout: Option<File>,
    stderr: Option<File>,
    cleanup_confirmed: bool,
}
impl Process {
    pub fn spawn(command: CommandSpec, mode: StdioMode) -> io::Result<Self> {
        // Validate the complete bounded command before creating any native owner.
        let program = bounded_program(command.get_program())?;
        let mut line = Vec::with_capacity(MAX_COMMAND_LINE + 1);
        quote_arg(&mut line, command.get_program())?;
        for arg in command.get_args() {
            append_units(&mut line, b' ' as u16, 1)?;
            quote_arg(&mut line, arg)?;
        }
        line.push(0);
        let application = resolve_application(program)?;
        // SAFETY: all native output pointers refer to initialized writable storage;
        // successful handles enter unique OwnedHandle owners immediately.
        unsafe {
            let job = own(CreateJobObjectW(null(), null()))?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            checked(SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            ))?;
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let (stderr_read, stderr_write) = pipe(&security, true)?;
            let (stdin_child, stdout_child, stdin, stdout) = match mode {
                StdioMode::Capture => {
                    let (input_read, input_write) = pipe(&security, false)?;
                    let (output_read, output_write) = pipe(&security, true)?;
                    (
                        input_read,
                        output_write,
                        Some(input_write),
                        Some(File::from(output_read)),
                    )
                }
                StdioMode::Diagnostics => {
                    let input = null_device(&security)?;
                    let output = null_device(&security)?;
                    (input, output, None, None)
                }
            };
            let inherited = [
                stdin_child.as_raw_handle(),
                stdout_child.as_raw_handle(),
                stderr_write.as_raw_handle(),
            ];
            let jobs = [job.as_raw_handle()];
            let list = AttributeList::new()?;
            list.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)?;
            list.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = inherited[0];
            startup.StartupInfo.hStdOutput = inherited[1];
            startup.StartupInfo.hStdError = inherited[2];
            startup.lpAttributeList = list.raw;
            let mut info = PROCESS_INFORMATION::default();
            checked(CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            ))?;
            // CreateProcessW success establishes both owned handles; Job membership
            // was assigned atomically, so rollback closes a kill-on-close Job.
            let process = own(info.hProcess)?;
            let _thread = own(info.hThread)?;
            Ok(Self {
                process,
                job,
                stdin,
                stdout,
                stderr: Some(File::from(stderr_read)),
                cleanup_confirmed: false,
            })
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // SAFETY: the stable owned process handle stays live; the scalar output
        // is writable. Observing exit never releases the handle or Job identity.
        unsafe {
            match WaitForSingleObject(self.process.as_raw_handle(), 0) {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    checked(GetExitCodeProcess(self.process.as_raw_handle(), &mut code))?;
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                _ => Err(Error::last_os_error()),
            }
        }
    }
    pub fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_available(self.stdout.as_ref(), buffer)
    }
    pub fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_available(self.stderr.as_ref(), buffer)
    }
    /// Transfer Diagnostics stderr to its existing blocking diagnostic consumer.
    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }
    pub fn terminate(&mut self) -> io::Result<()> {
        self.stdin.take();
        // SAFETY: the Job handle is exclusively owned and remains valid.
        unsafe { checked(TerminateJobObject(self.job.as_raw_handle(), 1)) }
    }
    pub fn cleanup_complete(&mut self) -> io::Result<bool> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the owned Job stays live and the exact accounting structure is writable.
        unsafe {
            checked(QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&info) as u32,
                null_mut(),
            ))?;
        }
        self.cleanup_confirmed = info.ActiveProcesses == 0;
        Ok(self.cleanup_confirmed)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stdin.take();
        if !self.cleanup_confirmed {
            let _ = self.terminate();
        }
        // Closing the Job is the final kill-on-close fallback. No waiting,
        // reader threads or finalizer work is delegated to another owner.
    }
}

fn read_available(pipe: Option<&File>, buffer: &mut [u8]) -> io::Result<usize> {
    let Some(pipe) = pipe else {
        return Ok(0);
    };
    if buffer.is_empty() {
        return Ok(0);
    }
    let mut available = 0;
    // SAFETY: this owner is the sole reader. Peek only writes the byte count;
    // ReadFile receives at most both reported bytes and writable buffer capacity.
    unsafe {
        if PeekNamedPipe(
            pipe.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        ) == 0
        {
            return pipe_error(Error::last_os_error());
        }
        if available == 0 {
            return Err(Error::from(io::ErrorKind::WouldBlock));
        }
        let count = available.min(buffer.len().min(u32::MAX as usize) as u32);
        let mut read = 0;
        if ReadFile(
            pipe.as_raw_handle(),
            buffer.as_mut_ptr(),
            count,
            &mut read,
            null_mut(),
        ) == 0
        {
            return pipe_error(Error::last_os_error());
        }
        Ok(read as usize)
    }
}
fn pipe_error(error: Error) -> io::Result<usize> {
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
        _ => Err(error),
    }
}
unsafe fn pipe(
    security: &SECURITY_ATTRIBUTES,
    parent_reads: bool,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: writable output slots receive newly created pipe handles; the
    // caller keeps the security descriptor live for the synchronous call.
    unsafe {
        checked(CreatePipe(&mut read, &mut write, security, 0))?;
    }
    // SAFETY: CreatePipe succeeded and returned unique read/write handles.
    let read = unsafe { own(read)? };
    // SAFETY: the successful CreatePipe call returned this distinct write handle.
    let write = unsafe { own(write)? };
    let parent = if parent_reads {
        read.as_raw_handle()
    } else {
        write.as_raw_handle()
    };
    // SAFETY: parent is one of these live owned handles; only the child end is inheritable.
    unsafe {
        checked(SetHandleInformation(parent, HANDLE_FLAG_INHERIT, 0))?;
    }
    Ok((read, write))
}
unsafe fn null_device(security: &SECURITY_ATTRIBUTES) -> io::Result<OwnedHandle> {
    // SAFETY: NUL is a static terminated name, the security descriptor lives for
    // this call, and successful creation transfers its fresh handle to own.
    unsafe {
        own(CreateFileW(
            [b'N' as u16, b'U' as u16, b'L' as u16, 0].as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        ))
    }
}

struct AttributeList {
    raw: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl AttributeList {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: the first call only measures capacity. The second uses aligned owned storage.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes);
            if bytes == 0 {
                return Err(Error::last_os_error());
            }
            let mut storage = vec![0; bytes.div_ceil(size_of::<usize>())];
            let raw = storage.as_mut_ptr().cast();
            checked(InitializeProcThreadAttributeList(raw, 2, 0, &mut bytes))?;
            Ok(Self {
                raw,
                _storage: storage,
            })
        }
    }

    fn set(&self, key: u32, handles: &[HANDLE]) -> io::Result<()> {
        // SAFETY: the initialized list and its handle slices stay alive through process creation.
        unsafe {
            checked(UpdateProcThreadAttribute(
                self.raw,
                0,
                key as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null_mut(),
            ))
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: this list was initialized once and its storage has not been freed.
        unsafe { DeleteProcThreadAttributeList(self.raw) };
    }
}

unsafe fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(Error::last_os_error());
    }
    // SAFETY: each caller passes a fresh handle from a Win32 creation function.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}

// CreateProcessW includes the terminating NUL in its 32767-unit command limit.
const MAX_COMMAND_LINE: usize = 32_766;

fn input_limit() -> Error {
    Error::new(
        io::ErrorKind::InvalidInput,
        "child process command exceeds its UTF-16 limit",
    )
}
fn append_units(line: &mut Vec<u16>, unit: u16, count: usize) -> io::Result<()> {
    if line
        .len()
        .checked_add(count)
        .is_none_or(|length| length > MAX_COMMAND_LINE)
    {
        return Err(input_limit());
    }
    line.extend(std::iter::repeat_n(unit, count));
    Ok(())
}
fn bounded_program(program: &OsStr) -> io::Result<Vec<u16>> {
    // The bounded iterator prevents an arbitrarily long OsString from growing
    // temporary native storage before validation.
    let mut units: Vec<u16> = program.encode_wide().take(MAX_COMMAND_LINE + 2).collect();
    if units.is_empty() || units.len() > MAX_COMMAND_LINE || units.contains(&0) {
        return Err(Error::new(
            io::ErrorKind::InvalidInput,
            "child process program is empty, too long or contains NUL",
        ));
    }
    units.push(0);
    Ok(units)
}
/// Resolve bare native executables using the standard Windows search order.
/// Explicit paths are unchanged; shell scripts and command interpreters are not inferred.
fn resolve_application(program: Vec<u16>) -> io::Result<Vec<u16>> {
    if program.iter().any(|unit| matches!(*unit, 47 | 92 | 58)) {
        return Ok(program);
    }
    let mut output = [0u16; 32_768];
    let extension = [b'.' as u16, b'e' as u16, b'x' as u16, b'e' as u16, 0];
    // SAFETY: program and extension are terminated and live for the call;
    // output is a fixed writable buffer, and no pointer into it escapes.
    let count = unsafe {
        SearchPathW(
            null(),
            program.as_ptr(),
            extension.as_ptr(),
            output.len() as u32,
            output.as_mut_ptr(),
            null_mut(),
        )
    };
    if count == 0 {
        return Err(Error::last_os_error());
    }
    let count = count as usize;
    if count >= output.len() {
        return Err(input_limit());
    }
    Ok(output[..=count].to_vec())
}
fn quote_arg(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let slash = b'\\' as u16;
    let quote = b'"' as u16;
    append_units(line, quote, 1)?;
    let mut slashes = 0usize;
    for unit in arg.encode_wide() {
        if unit == 0 {
            return Err(Error::new(
                io::ErrorKind::InvalidInput,
                "child process argument contains NUL",
            ));
        }
        if unit == slash {
            slashes = slashes
                .checked_add(1)
                .filter(|count| *count <= MAX_COMMAND_LINE)
                .ok_or_else(input_limit)?;
            continue;
        }
        let escaped = if unit == quote {
            slashes
                .checked_mul(2)
                .and_then(|count| count.checked_add(1))
                .ok_or_else(input_limit)?
        } else {
            slashes
        };
        append_units(line, slash, escaped)?;
        append_units(line, unit, 1)?;
        slashes = 0;
    }
    append_units(line, slash, slashes.checked_mul(2).ok_or_else(input_limit)?)?;
    append_units(line, quote, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_quotes_spaces_quotes_and_trailing_slashes() {
        for (input, expected) in [
            ("", "\"\""),
            ("two words", "\"two words\""),
            ("a\"b", "\"a\\\"b\""),
            ("C:\\folder with spaces\\", "\"C:\\folder with spaces\\\\\""),
        ] {
            let mut line = Vec::new();
            quote_arg(&mut line, OsStr::new(input)).unwrap();
            assert_eq!(String::from_utf16(&line).unwrap(), expected);
        }
    }
    #[test]
    fn windows_command_limits_refuse_before_native_process_creation() {
        let oversized = "x".repeat(MAX_COMMAND_LINE + 2);
        let error = match Process::spawn(CommandSpec::new(&oversized), StdioMode::Capture) {
            Err(error) => error,
            Ok(_) => panic!("oversized program was accepted"),
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let mut command = CommandSpec::new("missing-executable-that-must-not-be-searched");
        command.arg(&oversized);
        let error = match Process::spawn(command, StdioMode::Capture) {
            Err(error) => error,
            Ok(_) => panic!("oversized argument was accepted"),
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let mut line = Vec::new();
        quote_arg(&mut line, OsStr::new(&"x".repeat(MAX_COMMAND_LINE - 2))).unwrap();
        assert_eq!(line.len(), MAX_COMMAND_LINE);
        assert_eq!(
            append_units(&mut line, 32, 1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(line.len(), MAX_COMMAND_LINE);
        let mut escaped = Vec::new();
        assert_eq!(
            quote_arg(&mut escaped, OsStr::new(&"\\".repeat(MAX_COMMAND_LINE)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(escaped.len() <= MAX_COMMAND_LINE);
    }

    #[test]
    fn windows_nul_program_and_arguments_are_refused() {
        use std::os::windows::ffi::OsStringExt;
        let nul = std::ffi::OsString::from_wide(&[b'x' as u16, 0, b'y' as u16]);
        assert_eq!(
            bounded_program(&nul).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            quote_arg(&mut Vec::new(), &nul).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn windows_bare_native_executable_uses_standard_search() {
        let mut command = CommandSpec::new("cmd");
        command.args(["/D", "/C", "exit", "0"]);
        let output = super::super::run(
            command,
            StdioMode::Capture,
            super::super::RunOptions::new(std::time::Duration::from_secs(5)),
            None,
        )
        .unwrap();
        assert!(output.status.success());
    }
}
