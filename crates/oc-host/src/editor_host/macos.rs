//! AppKit backend: an `NSWindow` on the main run loop.
//!
//! The editor attaches from `applicationDidFinishLaunching`, because some
//! editors create internal objects from that launch sequence. The window closes
//! through AppKit, which then ends the run loop.

use std::cell::RefCell;
use std::io::Write;

use crate::param_edit::EditSocket;
use crate::vst3_instance::{Vst3GuiHandle, Vst3Instance};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

use super::{Config, fail};

struct Prepared {
    instance: Vst3Instance,
    config: Config,
}

struct Session {
    instance: Vst3Instance,
    _gui: Vst3GuiHandle,
}

thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
    static PREPARED: RefCell<Option<Prepared>> = const { RefCell::new(None) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct EditorHostDelegate;

    unsafe impl NSObjectProtocol for EditorHostDelegate {}

    unsafe impl NSApplicationDelegate for EditorHostDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let config = CONFIG
                .with(|slot| slot.borrow_mut().take())
                .ok_or_else(|| "no configuration".to_string());
            let config = match config {
                Ok(config) => config,
                Err(error) => fail(&error),
            };
            if let Err(error) = prepare_editor(config.clone()) {
                fail(&error);
            }
            if config.delay_ms > 0 {
                let delay = config.delay_ms as f64 / 1000.0;
                // SAFETY: This main-thread delegate implements runAttach; AppKit retains the receiver while its no-argument delayed selector is pending.
                let _: () = unsafe {
                    msg_send![
                        self,
                        performSelector: sel!(runAttach),
                        withObject: Option::<&objc2::runtime::AnyObject>::None,
                        afterDelay: delay,
                    ]
                };
            } else {
                if let Err(error) = attach_editor() {
                    fail(&error);
                }
                self.schedule_keep_alive();
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            true
        }
    }

    impl EditorHostDelegate {
        #[unsafe(method(runAttach))]
        fn run_attach(&self) {
            if let Err(error) = attach_editor() {
                fail(&error);
            }
            self.schedule_keep_alive();
        }

        /// One silent block for the instance that owns the open editor, and
        /// the restart notifications the plugin sent since the last turn.
        ///
        /// A program change inside the editor lands here: re-reading the
        /// controller values and publishing them is what keeps the parent's
        /// inspector off the previous values.
        #[unsafe(method(keepAlive))]
        fn keep_alive(&self) {
            if let Err(error) = super::pump_main() { fail(&error); }
            SESSION.with(|slot| {
                if let Some(session) = slot.borrow_mut().as_mut() {
                    let _ = session.instance.poll_host_events();
                    let _ = session.instance.process_block(512);
                }
            });
            self.schedule_keep_alive();
        }
    }
);

impl EditorHostDelegate {
    /// Keep the processor running while the window is open; an editor whose
    /// instance never processes shows an "audio processing disabled" banner.
    fn schedule_keep_alive(&self) {
        // SAFETY: This main-thread delegate implements keepAlive; AppKit schedules its no-argument selector with a null object argument.
        let _: () = unsafe {
            msg_send![
                self,
                performSelector: sel!(keepAlive),
                withObject: Option::<&objc2::runtime::AnyObject>::None,
                afterDelay: 0.0107_f64,
            ]
        };
    }
}

/// Load the plugin and apply the parent's processor state, before the editor
/// exists. Mirrors a host that loads a project and opens the editor later.
fn prepare_editor(config: Config) -> Result<(), String> {
    eprintln!("oc-vst3-editor-host: loading {}", config.plugin.display());
    // The editor runs in its own process with the inherited user home. Project
    // state is applied explicitly before a view is attached.
    // A null context makes the instance create its own host services,
    // including the `IMessage` factory editors need while attaching.
    let mut instance = Vst3Instance::load_on_main_thread(&config.plugin, 48_000, 512)?;
    eprintln!("oc-vst3-editor-host: loaded");
    // The module built its process-global UI state while it loaded, so a guard
    // registered now runs ahead of that state's destructors.
    crate::guard_plugin_teardown();
    super::apply_supplied_state(
        config.state_file.as_deref(),
        super::read_editor_state,
        |bytes| instance.set_state_bytes(bytes),
    )?;
    if let Some(path) = &config.edit_socket {
        let ids = instance.param_ids();
        let socket = EditSocket::connect(path, &ids)
            .map_err(|error| format!("could not connect the edit socket: {error}"))?;
        if instance.bind_edit_socket(socket) {
            eprintln!(
                "oc-vst3-editor-host: forwarding edits for {} params",
                ids.len()
            );
        } else {
            eprintln!("oc-vst3-editor-host: this plugin has no host handler for edits");
        }
    }
    PREPARED.with(|slot| {
        *slot.borrow_mut() = Some(Prepared { instance, config });
    });
    Ok(())
}

fn attach_editor() -> Result<(), String> {
    let prepared = PREPARED
        .with(|slot| slot.borrow_mut().take())
        .ok_or("no prepared plugin")?;
    let config = prepared.config.clone();
    let gui = prepared
        .instance
        .try_gui_handle()?
        .ok_or("plugin has no edit controller")?;
    eprintln!("oc-vst3-editor-host: creating editor");
    let size = gui.open(&config.title)?;
    eprintln!("oc-vst3-editor-host: attached {}x{}", size.0, size.1);
    if let Some(path) = &config.ready_file {
        let text = format!("{} {}", size.0, size.1);
        std::fs::write(path, text)
            .map_err(|error| format!("could not write ready file: {error}"))?;
    }
    let _ = std::io::stdout().flush();
    SESSION.with(|slot| {
        *slot.borrow_mut() = Some(Session {
            instance: prepared.instance,
            _gui: gui,
        });
    });
    Ok(())
}

/// Run the platform editor event loop.
pub fn run() {
    // The window closes through `AppKit`, but a plugin that installs its own
    // exit path still must not run its static destructors; see
    // [`crate::guard_plugin_teardown`].
    crate::guard_plugin_teardown();
    let config = match super::parse_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("oc-vst3-editor-host: {error}");
            crate::exit_process(2);
        }
    };
    CONFIG.with(|slot| *slot.borrow_mut() = Some(config));
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("oc-vst3-editor-host: must run on the main thread");
        crate::exit_process(2);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = EditorHostDelegate::alloc(mtm).set_ivars(());
    // SAFETY: The allocated main-thread delegate has initialized ivars; NSObject init returns ownership of this same compatible class.
    let delegate: Retained<EditorHostDelegate> = unsafe { msg_send![super(delegate), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.activate();
    app.run();
    super::shutdown_main();
    eprintln!("oc-vst3-editor-host: editor window closed");
    // Leave without running the hosted module's teardown.
    crate::exit_process(0);
}
