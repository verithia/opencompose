//! Out-of-process editor helper: spawn, readiness, reaping.
//!
//! A plugin editor that crashes inside the plugin must not take the DAW down.
//! The helper runs the same executable in `--editor-host` mode; the parent waits
//! for its ready file, then keeps the child reaped and reports its exit status.
//! Every desktop backend of `oc_host::editor_host` takes the same command line,
//! so the parent spawns the helper the same way on macOS, Linux and Windows.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the helper may take to load the plugin and attach its editor.
const READY_TIMEOUT: Duration = Duration::from_secs(6);

/// How often the monitor reaps the helper. The UI polls the outcome at 4 Hz, so
/// a shorter tick buys nothing but keeps the kill path's lock hold short.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Exit reported by a helper that has finished.
#[derive(Debug, Clone)]
pub struct HelperExit {
    /// What the user should see in one sentence.
    pub message: String,
    /// Full status line for the log.
    pub detail: String,
    /// The editor closed through its own window rather than failing.
    pub closed_cleanly: bool,
}

/// A live editor helper.
///
/// Cloning shares the child: every clone sees the same outcome and stops the
/// same process. Dropping a handle does not stop the process; call
/// [`Self::kill`] when the editor should close.
#[derive(Clone)]
pub struct EditorHelper {
    child: Arc<Mutex<Option<Child>>>,
    outcome: Arc<Mutex<Option<HelperExit>>>,
}

impl EditorHelper {
    /// Take ownership of a spawned child and reap it on a monitor thread.
    ///
    /// `log_path` names the helper's log in the exit detail; `directory` is the
    /// helper's private directory, removed once the helper exits.
    pub(crate) fn track(child: Child, log_path: PathBuf, directory: Option<PathBuf>) -> Self {
        let pid = child.id();
        let helper = Self {
            child: Arc::new(Mutex::new(Some(child))),
            outcome: Arc::new(Mutex::new(None)),
        };
        let slot = Arc::clone(&helper.child);
        let outcome = Arc::clone(&helper.outcome);
        std::thread::spawn(move || watch(pid, slot, outcome, log_path, directory));
        helper
    }

    /// The exit report once the helper has finished.
    pub fn outcome(&self) -> Option<HelperExit> {
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Ask the helper to close. The monitor thread reaps it.
    pub fn kill(&self) {
        let mut slot = self.child.lock().unwrap_or_else(|e| e.into_inner());
        // A cleared slot means the monitor already reaped the child, so its pid
        // is free to be reused and must not be signalled again.
        let Some(child) = slot.as_mut() else {
            return;
        };
        #[cfg(unix)]
        {
            // SAFETY: this handle still owns the child; the monitor clears the
            // slot only after reaping it, which keeps `id()` from being reused.
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        }
        #[cfg(not(unix))]
        {
            // Windows has no signal: terminate the process the helper runs in.
            let _ = child.kill();
        }
    }
}

/// The command that runs this executable as an editor host.
///
/// The helper is the app binary in `--editor-host` mode, so every platform
/// spawns it the same way and no bundle path is involved.
fn helper_command(
    plugin: &Path,
    preset_name: &str,
    ready_file: &Path,
    state_file: Option<&Path>,
    edit_socket: Option<&Path>,
) -> Result<Command, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not resolve the editor host executable: {error}"))?;
    let mut command = Command::new(exe);
    command
        .arg(oc_host::editor_host::MODE_ARG)
        .arg("--plugin")
        .arg(plugin)
        .arg("--title")
        .arg(preset_name)
        .arg("--ready-file")
        .arg(ready_file);
    if let Some(state_file) = state_file {
        command.arg("--state-file").arg(state_file);
    }
    if let Some(edit_socket) = edit_socket {
        command.arg("--edit-socket").arg(edit_socket);
    }
    Ok(command)
}

/// Start the helper for `plugin` and wait until its editor is attached.
///
/// Returns the editor size on success. A helper that exits during startup is
/// reaped here and returned as an error; a helper that is still running after
/// the ready file appears is monitored in the background.
pub fn spawn(
    plugin: &Path,
    preset_name: &str,
    state: Option<&[u8]>,
) -> Result<(EditorHelper, (u32, u32)), String> {
    let directory = std::env::temp_dir().join(format!(
        "ocp-editor-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create the editor host directory: {error}"))?;
    let ready_file = directory.join("ready");
    let state_file = directory.join("state.bin");
    let log_path = directory.join("editor.log");
    let log = std::fs::File::create(&log_path)
        .map_err(|error| format!("could not create the editor host log: {error}"))?;
    let log_err = log
        .try_clone()
        .map_err(|error| format!("could not duplicate the editor host log: {error}"))?;

    if let Some(bytes) = state {
        std::fs::write(&state_file, bytes)
            .map_err(|error| format!("could not write the editor state: {error}"))?;
    }
    // The helper's controller owns the editor; its edits come back over this
    // socket and reach the audio through the preset's edit bus. Only Unix has
    // the socket type, so elsewhere the editor reports through polling alone.
    #[cfg(unix)]
    let edit_socket = {
        let path = directory.join("edits");
        oc_host::param_edit::listen_for_edits(&path, preset_name)?;
        Some(path)
    };
    #[cfg(not(unix))]
    let edit_socket: Option<PathBuf> = None;
    let mut command = helper_command(
        plugin,
        preset_name,
        &ready_file,
        state.map(|_| state_file.as_path()),
        edit_socket.as_deref(),
    )?;
    command
        // The helper is isolated by design: the in-process gate stays in place,
        // while the child is allowed to attempt the attach.
        .env("OCP_VST3_EDITOR_FORCE", "1")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // Configure only the child, preserving any explicit operator setting.
    if std::env::var_os("QT_LOGGING_RULES").is_none()
        && std::env::var_os("OCP_QT_LOGGING").is_none()
    {
        command.env(
            "QT_LOGGING_RULES",
            "default.warning=false;default.debug=false;qt.qml.*=false",
        );
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the editor host: {error}"))?;
    let pid = child.id();
    log::info!(
        "[editor-helper] pid={pid} plugin={} log={}",
        plugin.display(),
        log_path.display()
    );

    let started = Instant::now();
    loop {
        if ready_file.exists() {
            let text = std::fs::read_to_string(&ready_file).unwrap_or_default();
            let Some(size) = parse_size(&text) else {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&directory);
                return Err("The plugin editor host reported an invalid size.".to_string());
            };
            return Ok((EditorHelper::track(child, log_path, Some(directory)), size));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit = describe(status, &log_path);
                log::warn!("[editor-helper] pid={pid} {}", exit.detail);
                let _ = std::fs::remove_dir_all(&directory);
                return Err(exit.message);
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&directory);
                return Err(format!("could not poll the editor host: {error}"));
            }
        }
        if started.elapsed() >= READY_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&directory);
            return Err(
                "The plugin editor did not open in time; edit its parameters in the inspector \
                 instead."
                    .to_string(),
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn parse_size(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace();
    let width: u32 = parts.next()?.parse().ok()?;
    let height: u32 = parts.next()?.parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

/// What one poll of the child found.
enum Polled {
    Running,
    /// The child is gone and its slot was cleared; this monitor already
    /// recorded the exit.
    Reaped,
    Exited(ExitStatus),
    Failed(String),
}

/// Non-blocking poll, so [`EditorHelper::kill`] can take the slot between ticks.
fn poll(slot: &Mutex<Option<Child>>) -> Polled {
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(child) = guard.as_mut() else {
        return Polled::Reaped;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            guard.take();
            Polled::Exited(status)
        }
        Ok(None) => Polled::Running,
        Err(error) => {
            guard.take();
            Polled::Failed(format!("wait failed: {error}"))
        }
    }
}

fn watch(
    pid: u32,
    slot: Arc<Mutex<Option<Child>>>,
    outcome: Arc<Mutex<Option<HelperExit>>>,
    log_path: PathBuf,
    directory: Option<PathBuf>,
) {
    loop {
        match poll(&slot) {
            Polled::Running => std::thread::sleep(POLL_INTERVAL),
            Polled::Reaped => return,
            Polled::Exited(status) => {
                let exit = describe(status, &log_path);
                log::info!("[editor-helper] pid={pid} {}", exit.detail);
                if let Some(directory) = directory {
                    let _ = std::fs::remove_dir_all(directory);
                }
                *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(exit);
                return;
            }
            Polled::Failed(detail) => {
                log::warn!("[editor-helper] pid={pid} {detail}");
                *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(HelperExit {
                    message: "The plugin editor stopped without reporting a status.".to_string(),
                    detail,
                    closed_cleanly: false,
                });
                return;
            }
        }
    }
}

fn describe(status: ExitStatus, log_path: &Path) -> HelperExit {
    // Unix reports the signal that killed the helper; Windows reports only an
    // exit code, so the crash branch is unreachable there.
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;

    let detail = if let Some(signal) = signal {
        format!("crashed with signal {signal} (log: {})", log_path.display())
    } else {
        format!(
            "exited with status {} (log: {})",
            status.code().unwrap_or(-1),
            log_path.display()
        )
    };
    let message = if status.success() {
        "The plugin editor window closed.".to_string()
    } else if signal.is_some() {
        "The plugin editor crashed; edit its parameters in the inspector instead.".to_string()
    } else {
        "The plugin editor closed before it could open; edit its parameters in the inspector \
         instead."
            .to_string()
    };
    HelperExit {
        message,
        detail,
        closed_cleanly: status.success(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn test_log_path() -> PathBuf {
        std::env::temp_dir().join("ocp-editor-helper-test.log")
    }

    #[test]
    fn ready_file_reports_a_valid_editor_size() {
        assert_eq!(parse_size("1190 740"), Some((1190, 740)));
        assert_eq!(parse_size("0 740"), None);
        assert_eq!(parse_size("not a size"), None);
    }

    #[test]
    fn a_crashed_helper_reports_that_the_inspector_still_works() {
        let exit = describe(ExitStatus::from_raw(11), Path::new("/tmp/editor.log"));
        assert!(exit.message.contains("crashed"));
        assert!(exit.message.contains("inspector"));
        assert!(!exit.closed_cleanly);
        assert!(exit.detail.contains("signal 11"));
    }

    #[test]
    fn a_closed_window_is_a_clean_exit() {
        let exit = describe(ExitStatus::from_raw(0), Path::new("/tmp/editor.log"));
        assert!(exit.closed_cleanly);
        assert!(exit.message.contains("closed"));
    }

    #[test]
    fn the_helper_runs_this_executable_in_editor_host_mode() {
        let command = helper_command(
            Path::new("/plugins/Kontakt.vst3"),
            "Kontakt",
            Path::new("/tmp/ocp-editor-ready"),
            None,
            None,
        )
        .expect("command");
        let exe = std::env::current_exe().expect("current executable");
        assert_eq!(Path::new(command.get_program()), exe.as_path());
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], oc_host::editor_host::MODE_ARG);
        assert_eq!(args[1], "--plugin");
        assert_eq!(args[2], "/plugins/Kontakt.vst3");
        assert!(args.contains(&"--ready-file".to_string()));
        assert!(args.contains(&"--title".to_string()));
    }

    #[test]
    fn a_helper_that_exits_is_reported_without_waiting_for_the_ui() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .expect("spawn");
        let helper = EditorHelper::track(child, test_log_path(), None);
        let deadline = Instant::now() + POLL_INTERVAL * 20;
        while helper.outcome().is_none() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        let exit = helper.outcome().expect("the monitor reports the exit");
        assert!(!exit.closed_cleanly);
        assert!(exit.message.contains("inspector"));
    }

    #[test]
    fn a_killed_helper_is_reaped_and_reported() {
        let child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let helper = EditorHelper::track(child, test_log_path(), None);
        assert!(helper.outcome().is_none());
        helper.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while helper.outcome().is_none() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        assert!(helper.outcome().is_some(), "the helper was not reaped");
    }
}
