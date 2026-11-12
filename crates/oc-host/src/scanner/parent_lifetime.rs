//! A scanner helper must not outlive the application that owns its request.

use std::io::{self, Read};

pub(super) const WATCH_STDIN_ARG: &str = "--watch-parent-stdin";

pub(super) fn watch_if_requested(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|arg| arg == WATCH_STDIN_ARG) {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("scan-parent-lifetime".into())
        .spawn(|| {
            wait_for_disconnect(io::stdin());
            // SAFETY: This dedicated scanner owns no user document. The parent
            // no longer owns its request, and native code may hold log/stdio
            // locks or be partially initialized. Skip flushing and all teardown.
            unsafe { libc::_exit(2) };
        })
        .map(|_| ())
        .map_err(|error| format!("Cannot monitor scan parent lifetime: {error}"))
}

fn wait_for_disconnect(mut input: impl Read) {
    let mut byte = [0];
    loop {
        match input.read(&mut byte) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // The pipe is a lifetime token, never a data channel. EOF, an I/O
            // failure or unexpected input all retire the disposable helper.
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "Subprocess entry for the parent-lifetime test"]
    fn parent_lifetime_child_entry() {
        let ready = std::env::var_os("OCP_TEST_SCAN_PARENT_READY")
            .expect("Only the parent-lifetime test launches this entry");
        // A stuck native call can retain a standard-output lock. Retirement
        // must not wait for flushing that lock before ending the helper.
        let _output = std::io::stdout().lock();
        watch_if_requested(&[WATCH_STDIN_ARG.into()]).unwrap();
        std::fs::write(ready, b"ready").unwrap();
        loop {
            std::thread::park();
        }
    }

    fn wait_until(child: &mut Child, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "helper exited before readiness"
            );
            assert!(Instant::now() < deadline, "helper did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn scanner_helper_exits_when_its_parent_pipe_closes() {
        let directory = oc_audio::private_fs::test_config_dir()
            .join(format!("scan-parent-watch-{}", std::process::id()));
        // This private scratch root already has the process cleanup guard.
        std::fs::create_dir_all(&directory).unwrap();
        let ready = directory.join("ready");
        let _ = std::fs::remove_file(&ready);
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "scanner::parent_lifetime::tests::parent_lifetime_child_entry",
                    "--ignored",
                    "--nocapture",
                ])
                .env("OCP_TEST_SCAN_PARENT_READY", &ready)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_until(&mut child.0, || Path::new(&ready).is_file());
        assert!(child.0.try_wait().unwrap().is_none());
        drop(child.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "disconnected helper survived its owner"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(2));
        let _ = std::fs::remove_dir_all(directory);
    }
}
