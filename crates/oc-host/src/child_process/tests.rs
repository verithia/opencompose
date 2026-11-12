use super::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

const ENTRY: &str = "child_process::tests::control_process_fixture_entry";
const FLOOD: usize = 1024 * 1024;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = oc_audio::private_fs::test_root().join(format!(
            "control-process-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(mode: &str, root: &Path) -> CommandSpec {
    let mut command = CommandSpec::new(std::env::current_exe().unwrap());
    // libtest consumes these filters; the fixture reads only its own explicit
    // metadata values. No process-global environment or extra binary is needed.
    command
        .args(["--exact", ENTRY, "--nocapture", "--skip"])
        .arg(format!("fixture-mode={mode}"))
        .arg("--skip")
        .arg(format!("fixture-root={}", root.display()));
    command
}
fn fixture_arg(prefix: &str) -> Option<String> {
    std::env::args().find_map(|arg| arg.strip_prefix(prefix).map(str::to_owned))
}

#[test]
fn control_process_fixture_entry() {
    if !std::env::args().any(|arg| arg == "--exact") {
        return;
    }
    let Some(mode) = fixture_arg("fixture-mode=") else {
        return;
    };
    let root = PathBuf::from(fixture_arg("fixture-root=").unwrap());
    match mode.as_str() {
        "flood" => {
            let out = vec![0xa3; FLOOD];
            let err = vec![0xb7; FLOOD];
            std::io::stdout().write_all(&out).unwrap();
            std::io::stderr().write_all(&err).unwrap();
            std::io::stderr().write_all(b"diagnostic-tail").unwrap();
        }
        #[cfg(unix)]
        "direct-escape" => {
            // SAFETY: this fixture changes only its own process group. The
            // owner must kill this child by its reserved PID, never its new group.
            let changed = unsafe { libc::setpgid(0, libc::getpgid(libc::getppid())) };
            assert_eq!(changed, 0);
            std::fs::write(root.join("ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(5));
        }
        "token" => {
            std::fs::write(root.join("ready"), b"ready").unwrap();
            std::io::stdout().write_all(b"token-ready").unwrap();
            std::io::stdout().flush().unwrap();
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
        }
        "noise" => loop {
            if std::io::stderr().write_all(&[0xb7; 4096]).is_err() {
                break;
            }
        },
        "parent" | "escape-parent" => {
            let child_mode = if mode == "parent" {
                "descendant"
            } else {
                "escape-descendant"
            };
            let spec = fixture(child_mode, &root);
            let mut command = std::process::Command::new(spec.get_program());
            command
                .args(spec.get_args())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
            let mut child = command.spawn().unwrap();
            let until = Instant::now() + Duration::from_secs(3);
            while !root.join("ready").is_file() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(2));
            }
            if !root.join("ready").is_file() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("descendant failed readiness handshake");
            }
            // This child is intentionally owned by the outer process group/Job.
            // Its inherited pipes remain open after the direct parent exits.
            std::process::exit(0);
        }
        "descendant" | "escape-descendant" => {
            #[cfg(unix)]
            if mode == "escape-descendant" {
                // SAFETY: the fixture is a freshly spawned non-group-leader child;
                // it changes only its own session to exercise the documented limit.
                assert_ne!(unsafe { libc::setsid() }, -1);
            }
            std::fs::write(root.join("ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(2));
            let _ = std::fs::write(root.join("late"), b"late");
        }
        _ => panic!("unknown fixture mode"),
    }
}

#[test]
fn bounded_capture_drains_both_large_pipes_and_retains_diagnostic_tail() {
    let dir = Directory::new();
    let output = run(
        fixture("flood", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_secs(10)),
        None,
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output
            .captured
            .stdout
            .iter()
            .filter(|&&x| x == 0xa3)
            .count(),
        FLOOD
    );
    assert_eq!(
        output.captured.stderr_bytes,
        (FLOOD + b"diagnostic-tail".len()) as u64
    );
    assert_eq!(output.captured.stderr.len(), 8192);
    assert!(output.captured.stderr.ends_with(b"diagnostic-tail"));
}

#[test]
fn bounded_capture_refuses_output_limit_and_completes_cleanup() {
    let dir = Directory::new();
    let mut options = RunOptions::new(Duration::from_secs(10));
    options.stdout_limit = 4096;
    let failure = run(fixture("flood", &dir.0), StdioMode::Capture, options, None).unwrap_err();
    assert_eq!(failure.kind, FailureKind::OutputLimit);
    assert_eq!(failure.captured.stdout.len(), options.stdout_limit);
    assert!(failure.cleanup_error.is_none(), "{failure}");
}

#[test]
fn bounded_capture_keeps_stdin_token_alive_until_deadline_cleanup() {
    let dir = Directory::new();
    let failure = run(
        fixture("token", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_millis(500)),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Deadline);
    assert!(
        failure
            .captured
            .stdout
            .windows(b"token-ready".len())
            .any(|x| x == b"token-ready")
    );
    assert!(failure.cleanup_error.is_none(), "{failure}");
}

#[test]
fn bounded_capture_continuous_diagnostics_cannot_starve_deadline() {
    let dir = Directory::new();
    let failure = run(
        fixture("noise", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_millis(500)),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Deadline);
    assert!(failure.captured.stderr_bytes > 8192);
    assert_eq!(failure.captured.stderr.len(), 8192);
    assert!(failure.cleanup_error.is_none(), "{failure}");
}

#[test]
fn bounded_capture_parent_exit_stops_ordinary_pipe_holding_descendant() {
    let dir = Directory::new();
    let output = run(
        fixture("parent", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_secs(5)),
        None,
    )
    .unwrap();
    assert!(output.status.success());
    assert!(dir.0.join("ready").is_file());
    std::thread::sleep(Duration::from_millis(2100));
    assert!(
        !dir.0.join("late").exists(),
        "ordinary descendant survived group/Job cleanup"
    );
}

#[cfg(unix)]
#[test]
fn bounded_capture_escaped_pipe_holder_does_not_extend_execution_deadline() {
    let dir = Directory::new();
    let started = Instant::now();
    let failure = run(
        fixture("escape-parent", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_millis(500)),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Deadline);
    assert!(
        failure.cleanup_error.is_none(),
        "direct child cleanup: {failure}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(dir.0.join("ready").is_file());
    // Escaped sessions are outside process-group containment. Their lifetime is
    // finite in this fixture; wait for their marker before deleting their root.
    let until = Instant::now() + Duration::from_secs(4);
    while !dir.0.join("late").is_file() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dir.0.join("late").is_file());
}

#[test]
fn bounded_capture_preserves_read_error_identity() {
    let mut capture = CapturedOutput::default();
    let error = io::Error::from_raw_os_error(5);
    let (kind, error) = read_piece(
        Err(error),
        &[],
        true,
        &mut capture,
        RunOptions::new(Duration::from_secs(1)),
    )
    .unwrap_err();
    assert_eq!(kind, FailureKind::StdoutRead);
    assert_eq!(error.raw_os_error(), Some(5));
    assert!(capture.stdout.is_empty());
}

#[test]
fn bounded_capture_rejects_invalid_options_and_pre_cancel_before_spawn() {
    let missing = || CommandSpec::new("nonexistent-control-process-executable");
    let failure = run(
        missing(),
        StdioMode::Capture,
        RunOptions::new(Duration::MAX),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::InvalidRequest);
    let cancelled = AtomicBool::new(true);
    let failure = run(
        missing(),
        StdioMode::Capture,
        RunOptions::new(Duration::from_secs(1)),
        Some(&cancelled),
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Cancelled);
    let failure = run(
        missing(),
        StdioMode::Capture,
        RunOptions::new(Duration::from_secs(1)),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Spawn);
    assert_eq!(failure.source.unwrap().kind(), io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn bounded_capture_stops_direct_child_that_changes_its_process_group() {
    let dir = Directory::new();
    let started = Instant::now();
    let failure = run(
        fixture("direct-escape", &dir.0),
        StdioMode::Capture,
        RunOptions::new(Duration::from_millis(500)),
        None,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Deadline);
    assert!(dir.0.join("ready").is_file());
    assert!(failure.cleanup_error.is_none(), "{failure}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "direct child was not stopped promptly"
    );
}

#[test]
fn bounded_capture_running_cancellation_cleans_the_admitted_child() {
    let dir = Directory::new();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            run(
                fixture("token", &dir.0),
                StdioMode::Capture,
                RunOptions::new(Duration::from_secs(10)),
                Some(&cancel),
            )
        });
        let until = Instant::now() + Duration::from_secs(3);
        while !dir.0.join("ready").is_file() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::Release);
        let failure = running.join().unwrap().unwrap_err();
        assert!(
            dir.0.join("ready").is_file(),
            "child never completed readiness handshake: {failure}"
        );
        assert_eq!(failure.kind, FailureKind::Cancelled);
        assert!(failure.cleanup_error.is_none(), "{failure}");
    });
}
