//! Bound the Intel worker's initialization and state restore before audio IPC.

use super::*;
use crate::scanner::{BundleIdentity, bundle_identity};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

const RETRY_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(super) enum StartupError {
    Io(&'static str, io::Error),
    Timeout(&'static str, io::Error),
    Refused(String),
    CoolingDown,
}

impl StartupError {
    fn exchange_io(stage: &'static str, error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Self::Timeout(stage, error)
        } else {
            Self::Io(stage, error)
        }
    }
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(stage, error) => {
                write!(formatter, "Intel plugin worker setup timed out while {stage}: {error}")
            }
            Self::Io(stage, error) => {
                write!(formatter, "Intel plugin worker setup failed while {stage}: {error}")
            }
            Self::Refused(error) => formatter.write_str(error),
            Self::CoolingDown => formatter.write_str(
                "Intel plugin worker setup skipped after a timeout for this state. Retry after one minute or rescan plugins",
            ),
        }
    }
}

struct Deadline<'a> {
    socket: &'a mut UnixStream,
    expires: Instant,
}

impl Deadline<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        self.expires
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "setup deadline elapsed"))
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(bytes)
    }
}

impl Write for Deadline<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

pub(super) fn exchange<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    header: &[u8],
    state: &[u8],
    expires: Instant,
) -> Result<T, StartupError> {
    let mut socket = Deadline {
        socket: stream,
        expires,
    };
    write_header_bytes(&mut socket, header)
        .and_then(|_| socket.write_all(state))
        .map_err(|error| StartupError::exchange_io("sending setup and state", error))?;
    let reply = read_header::<Result<T, String>>(&mut socket).map_err(|error| {
        StartupError::exchange_io("waiting for initialization and state restoration", error)
    })?;
    socket
        .remaining()
        .map_err(|error| StartupError::exchange_io("reading the setup response", error))?;
    reply.map_err(StartupError::Refused)
}

#[derive(Hash, PartialEq, Eq)]
pub(super) struct SetupKey {
    plugin: BundleIdentity,
    helper: BundleIdentity,
    payload: [u8; 32],
}

impl SetupKey {
    pub(super) fn new(plugin: &Path, helper: &Path, header: &[u8], state: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(header.len().to_le_bytes());
        digest.update(header);
        digest.update(state);
        Self {
            plugin: bundle_identity(plugin),
            helper: bundle_identity(helper),
            payload: digest.finalize().into(),
        }
    }
}

#[derive(Default)]
enum Health {
    #[default]
    Cold,
    Ready(Instant),
    TimedOut(Instant),
}

impl Health {
    fn current(&self, now: Instant) -> bool {
        match self {
            Self::Cold => false,
            Self::Ready(until) | Self::TimedOut(until) => *until > now,
        }
    }
}

type Gate = Arc<Mutex<Health>>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

struct SetupAdmission {
    entries: Mutex<HashMap<SetupKey, Gate>>,
    retry_delay: Duration,
}

static SETUP: LazyLock<SetupAdmission> = LazyLock::new(|| SetupAdmission::new(RETRY_DELAY));

impl SetupAdmission {
    fn new(retry_delay: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            retry_delay,
        }
    }

    fn run<T>(
        &self,
        key: SetupKey,
        launch: impl FnOnce() -> Result<T, StartupError>,
    ) -> Result<T, StartupError> {
        let gate = {
            let now = Instant::now();
            let mut entries = locked(&self.entries);
            entries.retain(|_, gate| Arc::strong_count(gate) > 1 || locked(gate).current(now));
            Arc::clone(entries.entry(key).or_default())
        };
        let health = locked(&gate);
        let now = Instant::now();
        let cold = match &*health {
            Health::TimedOut(until) if *until > now => return Err(StartupError::CoolingDown),
            Health::Ready(until) if *until > now => {
                // Each successful caller needs its own worker. Only the first
                // attempt for an unproven setup holds the gate during launch.
                drop(health);
                None
            }
            _ => Some(health),
        };
        let result = launch();
        let mut health = cold.unwrap_or_else(|| locked(&gate));
        let now = Instant::now();
        if matches!(result, Err(StartupError::Timeout(..))) {
            *health = Health::TimedOut(now + self.retry_delay);
        } else if result.is_ok() && !matches!(&*health, Health::TimedOut(until) if *until > now) {
            *health = Health::Ready(now + self.retry_delay);
        }
        result
    }

    fn allow_retry(&self) {
        // A manual rescan invalidates prior verdicts. An in-progress setup
        // retains its gate and can still publish its eventual outcome.
        locked(&self.entries).retain(|_, gate| Arc::strong_count(gate) > 1);
    }
}

pub(super) fn admit<T>(
    key: SetupKey,
    launch: impl FnOnce() -> Result<T, StartupError>,
) -> Result<T, StartupError> {
    SETUP.run(key, launch)
}

pub(super) fn allow_retry() {
    SETUP.allow_retry();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(state: &[u8]) -> SetupKey {
        SetupKey::new(
            Path::new("fixture.vst"),
            Path::new("helper"),
            b"init",
            state,
        )
    }

    fn timeout() -> StartupError {
        StartupError::Timeout("waiting", io::ErrorKind::TimedOut.into())
    }

    #[test]
    fn concurrent_identical_state_starts_only_one_stalled_worker() {
        let root = oc_test_support::temp_dir("vst2_bridge", "startup-timeout");
        let launches = root.join("launches");
        let admission = SetupAdmission::new(RETRY_DELAY);
        let ready = Barrier::new(4);
        let launched = AtomicUsize::new(0);
        std::thread::scope(|threads| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    threads.spawn(|| {
                        ready.wait();
                        admission.run(key(b"failing-preset"), || {
                            launched.fetch_add(1, Ordering::Relaxed);
                            let mut worker = Command::new("/bin/sh");
                            worker
                                .arg("-c")
                                .arg("printf '%s\\n' \"$$\" >> \"$1\"; exec /bin/sleep 30")
                                .arg("fixture")
                                .arg(&launches);
                            connect_worker::<u32>(
                                worker,
                                b"{}",
                                b"failing-preset",
                                BridgeMode::Offline,
                                Duration::from_millis(250),
                            )
                            .map(|_| ())
                        })
                    })
                })
                .collect();
            let outcomes: Vec<_> = threads
                .into_iter()
                .map(|worker| worker.join().unwrap().unwrap_err())
                .collect();
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|result| matches!(
                        result,
                        StartupError::Timeout(
                            "waiting for initialization and state restoration",
                            _
                        )
                    ))
                    .count(),
                1
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|result| matches!(result, StartupError::CoolingDown))
                    .count(),
                3
            );
        });
        assert_eq!(launched.load(Ordering::Relaxed), 1);
        let pids = std::fs::read_to_string(launches).unwrap();
        let pid = pids.trim().parse::<libc::pid_t>().unwrap();
        // SAFETY: Signal zero only queries the logged child ID. The production
        // owner has already killed and reaped its child before returning.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires the Intel helper and the cross-built stall_restore_mode fixture"]
    fn translated_vst2_timeout_is_scoped_to_the_saved_state_that_stalled() {
        let binary = PathBuf::from(std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_STALL").unwrap());
        let directory = oc_test_support::temp_dir("vst2_bridge", "restore-marker");
        let marker = directory.join("restore.log");
        let helper = helper_path().unwrap();
        let admission = SetupAdmission::new(RETRY_DELAY);
        let load = |state: &[u8], timeout| {
            let header = encode_header(&Init {
                version: VERSION,
                role: Role::Insert,
                plugin: binary.clone(),
                sample_rate: 48_000,
                state_bytes: state.len(),
                parameters: Vec::new(),
                insert_transport: None,
                vst3: None,
            })
            .unwrap();
            let key = SetupKey::new(&binary, &helper, &header, state);
            admission.run(key, || {
                let mut command = Command::new("/usr/bin/arch");
                command
                    .arg("-x86_64")
                    .arg(&helper)
                    .env("OCP_TEST_VST2_RESTORE_MARKER", &marker);
                connect_worker::<Metadata>(command, &header, state, BridgeMode::Offline, timeout)
                    .map(|(_, _, metadata)| metadata)
            })
        };
        assert!(load(b"", TIMEOUT).is_ok());
        assert!(matches!(
            load(b"OCF2stalling", Duration::from_secs(2)),
            Err(StartupError::Timeout(
                "waiting for initialization and state restoration",
                _
            ))
        ));
        assert!(matches!(
            load(b"OCF2stalling", TIMEOUT),
            Err(StartupError::CoolingDown)
        ));
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "restore entered\n"
        );

        let mut good = b"OCF2".to_vec();
        good.extend(0.25_f32.to_le_bytes());
        good.extend(0.0_f32.to_le_bytes());
        assert_eq!(load(&good, TIMEOUT).unwrap().parameters[0].1, 0.25);
    }

    #[test]
    fn partial_responses_share_one_setup_deadline() {
        let (mut host, mut worker) = UnixStream::pair().unwrap();
        std::thread::scope(|threads| {
            threads.spawn(move || {
                let init: String = read_header(&mut worker).unwrap();
                assert_eq!(init, "fixture");
                let mut state = [0; 5];
                worker.read_exact(&mut state).unwrap();
                assert_eq!(&state, b"state");
                let reply = encode_header(&Ok::<_, String>("fragmented reply")).unwrap();
                let framed = (reply.len() as u32).to_le_bytes().into_iter().chain(reply);
                for byte in framed {
                    if worker.write_all(&[byte]).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let result = exchange::<String>(
                &mut host,
                br#""fixture""#,
                b"state",
                Instant::now() + Duration::from_millis(150),
            );
            assert!(
                matches!(
                    result,
                    Err(StartupError::Timeout(
                        "waiting for initialization and state restoration",
                        _
                    ))
                ),
                "{result:?}"
            );
            drop(host);
        });
    }

    #[test]
    fn successes_restore_the_processing_socket_timeout() {
        for (mode, expected) in [
            (BridgeMode::Offline, TIMEOUT),
            (BridgeMode::Streaming, STREAM_TIMEOUT),
        ] {
            let mut helper = Command::new("/bin/sh");
            helper
                .arg("-c")
                .arg("/bin/dd bs=1 count=6 >/dev/null 2>/dev/null; printf '\\011\\000\\000\\000{\"Ok\":42}' >&0");
            let (_worker, stream, result) =
                connect_worker::<u32>(helper, b"{}", b"", mode, Duration::from_secs(1)).unwrap();
            assert_eq!(result, 42);
            assert_eq!(stream.read_timeout().unwrap(), Some(expected));
            assert_eq!(stream.write_timeout().unwrap(), Some(expected));
        }
    }

    #[test]
    fn timeout_does_not_hide_a_changed_state_or_an_explicit_retry() {
        let admission = SetupAdmission::new(RETRY_DELAY);
        assert!(matches!(
            admission.run(key(b"first"), || Err::<(), _>(timeout())),
            Err(StartupError::Timeout(..))
        ));
        assert!(matches!(
            admission.run(key(b"first"), || panic!("cooldown ignored")),
            Err::<(), _>(StartupError::CoolingDown)
        ));
        assert_eq!(admission.run(key(b"second"), || Ok(9)).unwrap(), 9);
        admission.allow_retry();
        assert_eq!(admission.run(key(b"first"), || Ok(10)).unwrap(), 10);
        let short = SetupAdmission::new(Duration::ZERO);
        let _ = short.run(key(b"expires"), || Err::<(), _>(timeout()));
        assert!(short.run(key(b"expires"), || Ok(())).is_ok());
    }

    #[test]
    fn changing_the_plugin_or_helper_binary_bypasses_the_prior_timeout() {
        let root = oc_test_support::temp_dir("vst2_bridge", "startup-identity");
        let plugin = root.join("fixture.vst");
        let helper = root.join("helper");
        std::fs::write(&plugin, b"original").unwrap();
        std::fs::write(&helper, b"original").unwrap();
        let key = || SetupKey::new(&plugin, &helper, b"init", b"same-state");
        let admission = SetupAdmission::new(RETRY_DELAY);
        let _ = admission.run(key(), || Err::<(), _>(timeout()));
        std::fs::write(&plugin, b"new plugin build").unwrap();
        assert!(matches!(
            admission.run(key(), || Err::<(), _>(timeout())),
            Err(StartupError::Timeout(..))
        ));
        std::fs::write(&helper, b"new worker build").unwrap();
        assert!(admission.run(key(), || Ok(())).is_ok());
    }

    #[test]
    fn worker_refusals_and_launch_errors_do_not_create_a_cooldown() {
        let admission = SetupAdmission::new(RETRY_DELAY);
        let result = admission.run(key(b"plugin"), || {
            Err::<(), _>(StartupError::Refused("explicit plugin refusal".into()))
        });
        assert_eq!(result.unwrap_err().to_string(), "explicit plugin refusal");
        let result = admission.run(key(b"plugin"), || {
            Err::<(), _>(StartupError::Io(
                "launching the helper",
                io::ErrorKind::WouldBlock.into(),
            ))
        });
        assert!(matches!(result, Err(StartupError::Io(..))));
        assert!(admission.run(key(b"plugin"), || Ok(())).is_ok());
    }

    #[test]
    fn a_previously_successful_setup_allows_independent_workers() {
        use std::sync::mpsc;

        let admission = SetupAdmission::new(RETRY_DELAY);
        admission.run(key(b"known"), || Ok(())).unwrap();
        std::thread::scope(|threads| {
            let (started, start_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let shared = &admission;
            let worker = threads.spawn(move || {
                shared.run(key(b"known"), || {
                    started.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(7)
                })
            });
            start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(admission.run(key(b"known"), || Ok(8)).unwrap(), 8);
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), 7);
        });
    }
}
