//! What the operator answered on the unsaved-changes prompt.
//!
//! Closing the window asks the panel from the panel's own close handler. Quitting
//! the application is asked from here, because the platform reaches this process
//! before the document goes. Both answers land in [`AppStateInner`] as the two
//! flags the write path reads: a discarded document never reaches the file, and
//! only an answered quit arms the last-resort write.
//!
//! [`AppStateInner`]: crate::state::AppStateInner

use tauri::{Manager, State};

use crate::commands::shared::{ProjectSnapshot, snapshot};
use crate::state::{AppState, AppStateInner};

/// Event that asks the panel to show the unsaved-changes prompt for a quit.
pub const QUIT_REQUESTED: &str = "quit_requested";

/// Id of the Quit menu item this process answers itself.
#[cfg(target_os = "macos")]
pub const QUIT_MENU_ID: &str = "opencompose-quit";

/// The operator answered Don't Save: drop the document without writing it.
///
/// The recovery copies stay. The answer is about the project file, and the crash
/// net is what still holds the work if the operator changes their mind.
pub(crate) fn discard(inner: &mut AppStateInner) -> ProjectSnapshot {
    inner.dirty = false;
    inner.discard_pending = true;
    inner.exit_ready = false;
    inner.autosave_at = None;
    inner.autosave_digest = 0;
    snapshot(inner)
}

/// Drop the document the operator chose not to keep.
#[tauri::command]
pub fn discard_unsaved(state: State<AppState>) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    if recording(&inner) {
        return Err("Stop recording before discarding project changes.".into());
    }
    Ok(discard(&mut inner))
}

fn recording(inner: &AppStateInner) -> bool {
    inner.audio.recorder.recording() || inner.audio.midi_recorder.recording()
}

#[derive(serde::Serialize)]
pub(crate) struct QuitPreparation {
    pub(crate) dirty: bool,
    pub(crate) recording: bool,
}

/// Read the close decision under the same lock as recording.
pub(crate) fn prepare(state: &AppState) -> QuitPreparation {
    let inner = state.lock_inner();
    QuitPreparation {
        dirty: inner.dirty && !inner.discard_pending,
        recording: recording(&inner),
    }
}

/// Prepare the app for quitting.
#[tauri::command]
pub fn prepare_quit(state: State<AppState>) -> QuitPreparation {
    prepare(state.inner())
}

pub(crate) fn arm_exit(inner: &mut AppStateInner) -> Result<(), String> {
    if recording(inner) {
        return Err("Stop recording before quitting.".into());
    }
    inner.exit_ready = true;
    Ok(())
}

pub(crate) fn needs_decision(inner: &AppStateInner) -> bool {
    recording(inner) || (inner.dirty && !inner.discard_pending && !inner.exit_ready)
}

/// Leave the process after the prompt was answered. Only this command arms the
/// exit write, so a discarded document is never published on the way out.
#[tauri::command]
pub fn quit_now(app: tauri::AppHandle) -> Result<(), String> {
    arm_exit(&mut app.state::<AppState>().lock_inner())?;
    app.exit(0);
    Ok(())
}

/// Guard native window close even if the panel has not registered its handler.
pub fn intercept_window_close(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    if window.label() != "main" {
        return;
    }
    let Some(state) = window.try_state::<AppState>() else {
        return;
    };
    if recording(&state.lock_inner()) {
        api.prevent_close();
        ask(window.app_handle());
    }
}

/// Ask the operator before an exit that would drop a take or unsaved document.
///
/// The event also arrives after the last window is destroyed, where no prompt
/// can be shown; preventing the exit there would leave the process running with
/// nothing on screen, so such an exit goes ahead and writes nothing.
pub fn intercept_exit(app: &tauri::AppHandle, api: &tauri::ExitRequestApi) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if app.get_webview_window("main").is_none() {
        return;
    }
    let asks = {
        let inner = state.lock_inner();
        needs_decision(&inner)
    };
    if !asks {
        return;
    }
    api.prevent_exit();
    ask(app);
}

/// The Quit menu item asks before dropping a take or unsaved document.
#[cfg(target_os = "macos")]
pub fn request(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let asks = {
        let inner = state.lock_inner();
        needs_decision(&inner)
    };
    if asks {
        ask(app);
    } else {
        app.exit(0);
    }
}

fn ask(app: &tauri::AppHandle) {
    use tauri::Emitter;
    if let Err(error) = app.emit(QUIT_REQUESTED, ()) {
        log::warn!("quit: the panel could not be asked: {error}");
    }
}

/// Replace the system Quit item, which runs the platform's own terminate and
/// never reaches this process, with one the panel answers.
///
/// Every step that fails leaves the system item in place: a quit that does not
/// ask is still a quit, and it does not write a discarded document.
#[cfg(target_os = "macos")]
pub fn install_quit_item(app: &tauri::App) {
    use tauri::menu::{MenuItem, MenuItemKind};

    let Some(MenuItemKind::Submenu(app_menu)) = app
        .menu()
        .and_then(|menu| menu.items().ok())
        .and_then(|items| items.into_iter().next())
    else {
        log::warn!("quit: no application menu; the system Quit item stays");
        return;
    };
    let system = app_menu.items().ok().and_then(|items| {
        items.into_iter().find_map(|item| match item {
            MenuItemKind::Predefined(item)
                if item.text().is_ok_and(|text| text.starts_with("Quit ")) =>
            {
                Some(item)
            }
            _ => None,
        })
    });
    let Some(system) = system else {
        log::warn!("quit: no system Quit item to replace");
        return;
    };
    let quit = match MenuItem::with_id(
        app,
        QUIT_MENU_ID,
        format!("Quit {}", app.package_info().name),
        true,
        Some("CmdOrCtrl+Q"),
    ) {
        Ok(item) => item,
        Err(error) => {
            log::warn!("quit: no Quit item: {error}");
            return;
        }
    };
    if let Err(error) = app_menu.remove(&system) {
        log::warn!("quit: the system Quit item stays: {error}");
        return;
    }
    if let Err(error) = app_menu.append(&quit) {
        log::warn!("quit: the Quit item was not added: {error}");
        // Leaving the menu without a Quit item would be a worse outcome than
        // the system behaviour, so put the item that was removed back.
        if app_menu.append(&system).is_err() {
            log::warn!("quit: the system Quit item could not be restored");
        }
    }
}
