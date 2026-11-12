//! Out-of-process VST3 editor host.
//!
//! The DAW spawns the same executable in `--editor-host` mode with one bundle,
//! an optional component-state file and a ready-file path. The plugin, its
//! editor view and its window live in this process; a crash here is reported by
//! the parent as an exit status instead of taking the DAW down.
//!
//! Each platform owns its window system: AppKit on macOS, Xlib on Linux and
//! Win32 on Windows. All three take the same command line and report the
//! attached size through the same ready file, so the parent does not depend on
//! which one it spawned.
//!
//! On macOS the editor is created from `applicationDidFinishLaunching`, after
//! AppKit has finished launching and its run loop is about to run. Some editors
//! create internal objects from that launch sequence and are not ready before
//! it.
//!
//! Exit codes: 0 when the editor window closes, 1 on a host-side failure, and
//! the signal number when the plugin crashes.

use std::path::PathBuf;

/// Command-line switch that turns the DAW binary into the editor host.
pub const MODE_ARG: &str = "--editor-host";

/// Size the host opens an editor window with when the view reports none before
/// it is attached. The plugin replaces it from `getSize`/`resizeView`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
const DEFAULT_EDITOR_SIZE: (i32, i32) = (1024, 768);

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

thread_local! {
    static MAIN_QUEUE: std::cell::RefCell<Option<crate::application_main::ApplicationMainQueue>> = const { std::cell::RefCell::new(None) };
}

struct MainLoopScope(std::marker::PhantomData<std::rc::Rc<()>>);
impl MainLoopScope {
    fn install(queue: crate::application_main::ApplicationMainQueue) -> Result<Self, String> {
        queue
            .executor()
            .check_ui_authority()
            .map_err(|error| error.to_string())?;
        MAIN_QUEUE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return Err("The editor main loop is already installed".into());
            }
            *slot = Some(queue);
            Ok(Self(std::marker::PhantomData))
        })
    }
}
impl Drop for MainLoopScope {
    fn drop(&mut self) {
        shutdown_main();
        MAIN_QUEUE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn pump_main() -> Result<(), String> {
    MAIN_QUEUE.with(|slot| {
        slot.borrow()
            .as_ref()
            .ok_or_else(|| "The editor main loop is not installed".to_owned())?
            .pump()
    })
}

fn shutdown_main() {
    MAIN_QUEUE.with(|slot| {
        if let Some(queue) = slot.borrow().as_ref()
            && let Err(error) = queue.executor().begin_shutdown().and_then(|_| queue.pump())
        {
            eprintln!("oc-vst3-editor-host: main-loop shutdown: {error}");
        }
    });
}

/// One parsed command line, shared by every platform backend.
#[derive(Clone)]
struct Config {
    plugin: PathBuf,
    title: String,
    state_file: Option<PathBuf>,
    ready_file: Option<PathBuf>,
    edit_socket: Option<PathBuf>,
    delay_ms: u64,
}

fn parse_config() -> Result<Config, String> {
    let mut plugin = None;
    let mut title = "Plugin Editor".to_string();
    let mut state_file = None;
    let mut ready_file = None;
    let mut edit_socket = None;
    let mut delay_ms = 0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            MODE_ARG => {}
            "--plugin" => plugin = Some(PathBuf::from(args.next().ok_or("--plugin value")?)),
            "--title" => title = args.next().ok_or("--title value")?,
            "--state-file" => {
                state_file = Some(PathBuf::from(args.next().ok_or("--state-file value")?))
            }
            "--ready-file" => {
                ready_file = Some(PathBuf::from(args.next().ok_or("--ready-file value")?))
            }
            "--edit-socket" => {
                edit_socket = Some(PathBuf::from(args.next().ok_or("--edit-socket value")?))
            }
            "--delay-ms" => {
                delay_ms = args
                    .next()
                    .ok_or("--delay-ms value")?
                    .parse()
                    .map_err(|_| "--delay-ms is not a number")?
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Config {
        plugin: plugin.ok_or(
            "usage: oc-vst3-editor-host --plugin <bundle> [--title <t>] \
             [--state-file <s>] [--ready-file <r>] [--edit-socket <p>] [--delay-ms <n>]",
        )?,
        title,
        state_file,
        ready_file,
        edit_socket,
        delay_ms,
    })
}

/// Report a host-side failure and leave with the failure status.
///
/// The parent tells 0 (the window closed) from a non-zero status, so every
/// backend reports a failed attach through this one exit path.
fn fail(error: &str) -> ! {
    eprintln!("oc-vst3-editor-host: {error}");
    crate::exit_process(1);
}

/// Text the parent parses to learn that the editor is attached.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn ready_text(size: (i32, i32)) -> String {
    format!("{} {}", size.0, size.1)
}

/// Publish the attached size; the parent waits for this file.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn write_ready_file(path: Option<&std::path::Path>, size: (i32, i32)) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    std::fs::write(path, ready_text(size))
        .map_err(|error| format!("could not write ready file '{}': {error}", path.display()))
}

/// Maximum processor-state bytes accepted by the disposable editor host.
/// Project resources use the same 64 MiB per-file ceiling.
const MAX_EDITOR_STATE_BYTES: usize = crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES;

/// The editor requires a supplied state to contain bytes, while the shared I/O
/// owner also serves legacy raw-state callers that accept empty chunks.
fn require_editor_state(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("supplied plugin state is empty".into());
    }
    Ok(bytes)
}

#[cfg(test)]
fn read_state_bytes(reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>, String> {
    require_editor_state(crate::plugin_state_io::read_state_bytes(reader, limit)?)
}

fn read_editor_state(path: &std::path::Path) -> Result<Vec<u8>, String> {
    require_editor_state(crate::plugin_state_io::read_regular_state_file(
        path,
        MAX_EDITOR_STATE_BYTES,
    )?)
}

/// A supplied project state must be applied before attaching any editor view.
/// Injection keeps the same failure policy testable without loading a plugin.
fn apply_supplied_state(
    path: Option<&std::path::Path>,
    read: impl FnOnce(&std::path::Path) -> Result<Vec<u8>, String>,
    apply: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let bytes = read(path)
        .map_err(|error| format!("could not load project state '{}': {error}", path.display()))?;
    apply(&bytes)
        .map_err(|error| format!("plugin refused project state '{}': {error}", path.display()))?;
    log::debug!("oc-vst3-editor-host: applied {} state bytes", bytes.len());
    Ok(())
}

/// Load the plugin and apply the parent's state before a view exists.
///
/// A supplied state that cannot be read or applied aborts startup, so an editor
/// never silently opens an unrelated default preset in its place.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn load_instance(config: &Config) -> Result<crate::vst3_instance::Vst3Instance, String> {
    use crate::vst3_instance::Vst3Instance;

    eprintln!("oc-vst3-editor-host: loading {}", config.plugin.display());
    // The editor runs in its own process with the inherited user home. Project
    // state is applied explicitly before a view is attached; a home alias is
    // neither filesystem confinement nor a safe per-instance setting.
    // A null context makes the instance create its own host services,
    // including the `IMessage` factory editors need while attaching.
    let mut instance = Vst3Instance::load_on_main_thread(&config.plugin, 48_000, 512)?;
    eprintln!("oc-vst3-editor-host: loaded");
    // The module built its process-global UI state while it loaded, so a guard
    // registered now runs ahead of that state's destructors.
    crate::guard_plugin_teardown();
    apply_supplied_state(config.state_file.as_deref(), read_editor_state, |bytes| {
        instance.set_state_bytes(bytes)
    })?;
    #[cfg(unix)]
    if let Some(path) = &config.edit_socket {
        let ids = instance.param_ids();
        match crate::param_edit::EditSocket::connect(path, &ids) {
            Ok(socket) => {
                if instance.bind_edit_socket(socket) {
                    eprintln!(
                        "oc-vst3-editor-host: forwarding edits for {} params",
                        ids.len()
                    );
                } else {
                    eprintln!("oc-vst3-editor-host: this plugin has no host handler for edits");
                }
            }
            // The editor opens without the forwarder; the parent only loses
            // live knob moves, which it reports on its own socket timeout.
            Err(error) => {
                eprintln!("oc-vst3-editor-host: could not connect the edit socket: {error}")
            }
        }
    }
    #[cfg(not(unix))]
    if config.edit_socket.is_some() {
        eprintln!("oc-vst3-editor-host: the edit socket is unix-only and is ignored here");
    }
    Ok(instance)
}

/// The view's size, or `None` while it reports that it is not initialized.
///
/// A Qt editor answers `kNotInitialized` until its widget is in a window and
/// reports the real size from `resizeView`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn view_size(view: &vst3::ComPtr<vst3::Steinberg::IPlugView>) -> Option<(i32, i32)> {
    use vst3::Steinberg::{IPlugViewTrait, ViewRect, kResultOk};

    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `view` is a live IPlugView and `rect` outlives the call.
    if unsafe { view.getSize(&mut rect) } != kResultOk {
        return None;
    }
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    (width > 0 && height > 0).then_some((width, height))
}

/// Run the out-of-process editor host. The parent passes the plugin, title,
/// optional component state and ready-file path; this call blocks on the
/// platform's window loop until the editor window closes.
pub fn run_cli(queue: crate::application_main::ApplicationMainQueue) -> Result<(), String> {
    let _main = MainLoopScope::install(queue)?;
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(target_os = "linux")]
    linux::run();
    #[cfg(target_os = "windows")]
    windows::run();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(
        "oc-vst3-editor-host: this platform has no plugin-editor window backend".to_string(),
    );

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn supplied_state_fifo_is_refused_without_waiting_for_a_writer() {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, OpenOptionsExt},
        };
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        struct Root(std::path::PathBuf);
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "oc-editor-state-fifo-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => break Root(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fixture root: {error}"),
            }
        };
        let fifo = root.0.join("state.fifo");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the NUL-terminated path is live, and this private root owns
        // the new FIFO; its guard removes it after the reader is joined.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let reader_path = fifo.clone();
        let reader = std::thread::spawn(move || {
            let result = super::read_editor_state(&reader_path);
            let _ = send.send(result);
        });
        let timely = receive.recv_timeout(std::time::Duration::from_secs(1));
        // Rescue a regressed blocking reader before joining it, so a failing
        // test does not leave a stuck thread or a filesystem artifact.
        let rescue = timely.is_err().then(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
                .unwrap()
        });
        let result = match timely {
            Ok(result) => result,
            Err(_) => {
                let _ = receive.recv_timeout(std::time::Duration::from_secs(1));
                reader.join().unwrap();
                drop(rescue);
                panic!("state-file open waited for a FIFO writer");
            }
        };
        reader.join().unwrap();
        assert!(result.unwrap_err().contains("regular file"));
    }

    #[cfg(windows)]
    #[test]
    fn supplied_state_windows_character_device_is_refused() {
        let error = super::read_editor_state(std::path::Path::new("NUL")).unwrap_err();
        assert!(error.contains("regular file"), "{error}");
    }

    #[test]
    fn supplied_state_is_optional_and_applies_exact_read_bytes() {
        super::apply_supplied_state(
            None,
            |_| panic!("no state to read"),
            |_| panic!("no state to apply"),
        )
        .unwrap();
        let path = std::path::Path::new("project.state");
        let mut applied = false;
        super::apply_supplied_state(
            Some(path),
            |requested| {
                assert_eq!(requested, path);
                super::read_state_bytes(&[1, 2, 3, 4][..], 4)
            },
            |bytes| {
                assert_eq!(bytes, &[1, 2, 3, 4]);
                applied = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(applied);
    }

    #[test]
    fn supplied_state_read_and_plugin_errors_abort_before_attach() {
        let path = std::path::Path::new("project.state");
        let error = super::apply_supplied_state(
            Some(path),
            |_| Err("permission denied".into()),
            |_| panic!("unreadable state must not be applied"),
        )
        .unwrap_err();
        assert!(error.contains("project.state") && error.contains("permission denied"));
        let error = super::apply_supplied_state(
            Some(path),
            |_| Ok(vec![7]),
            |_| Err("unsupported state version".into()),
        )
        .unwrap_err();
        assert!(error.contains("project.state") && error.contains("unsupported state version"));
        assert!(error.contains("refused"));
    }

    #[test]
    fn supplied_state_reader_enforces_empty_limit_and_io_failures() {
        let path = std::path::Path::new("project.state");
        for bytes in [&[][..], &[1, 2, 3, 4, 5][..]] {
            let error = super::apply_supplied_state(
                Some(path),
                |_| super::read_state_bytes(bytes, 4),
                |_| panic!("invalid state must not reach the plugin"),
            )
            .unwrap_err();
            assert!(error.contains("project.state"));
            assert!(error.contains("empty") || error.contains("4-byte limit"));
        }
        struct Fails;
        impl std::io::Read for Fails {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        let error = super::read_state_bytes(Fails, 4).unwrap_err();
        assert!(error.contains("read failed"));
        // A stream that never ends is cut off after limit + 1 bytes.
        assert!(
            super::read_state_bytes(std::io::repeat(1), 4)
                .unwrap_err()
                .contains("4-byte limit")
        );
    }

    #[test]
    fn editor_mode_argument_is_stable() {
        assert_eq!(super::MODE_ARG, "--editor-host");
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn the_ready_file_holds_the_attached_size() {
        assert_eq!(super::ready_text((1190, 740)), "1190 740");
    }
}
