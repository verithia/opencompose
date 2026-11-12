use oc_host::child_process::{self, CommandSpec, FailureKind, RunOptions, StdioMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(super) const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STDERR_LIMIT: usize = 8 * 1024;

#[cfg(all(windows, test))]
mod windows;

pub(super) fn check_cancelled(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err("separation cancelled".into())
    } else {
        Ok(())
    }
}

pub(super) fn run(
    name: &str,
    command: CommandSpec,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    check_cancelled(cancel)?;
    let options = RunOptions {
        stdout_limit: 0,
        stderr_limit: STDERR_LIMIT,
        ..RunOptions::new(timeout)
    };
    let output =
        child_process::run(command, StdioMode::Diagnostics, options, cancel).map_err(|error| {
            let message = match error.kind {
                FailureKind::Cancelled => "separation cancelled".into(),
                FailureKind::Deadline => format!(
                    "{name} exceeded its {}-second separation deadline",
                    timeout.as_secs_f64()
                ),
                FailureKind::Spawn => format!("{name} could not start: {error}"),
                _ => format!("{name} could not be monitored: {error}"),
            };
            if matches!(error.kind, FailureKind::Cancelled | FailureKind::Deadline)
                && let Some(cleanup) = error.cleanup_error
            {
                format!("{message}; process cleanup failed: {cleanup}")
            } else {
                message
            }
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{name} exited with {}: {}",
        output.status,
        last_error_line(&output.captured.stderr)
    ))
}

fn last_error_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no error output")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn separator_diagnostics_select_the_last_nonempty_line() {
        assert_eq!(
            last_error_line(b"first error\n\n  last error  \n"),
            "last error"
        );
        assert_eq!(last_error_line(b" \n"), "no error output");
    }

    #[cfg(unix)]
    #[test]
    fn separator_process_stops_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let escaped = dir.path().join("escaped");
        let mut command = CommandSpec::new("/bin/sh");
        command
            .args([
                "-c",
                "(/bin/sleep 1; printf late > \"$1\") & wait",
                "separator-fixture",
            ])
            .arg(&escaped);
        let started = Instant::now();
        let error = run("test separator", command, Duration::from_millis(100), None).unwrap_err();
        assert!(error.contains("separation deadline"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            !escaped.exists(),
            "the separator's descendant survived the deadline"
        );
    }
}
