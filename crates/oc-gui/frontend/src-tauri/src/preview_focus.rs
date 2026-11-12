//! Coordination between HTML media and transport started outside the webview.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{Emitter, State, WebviewWindow};

use crate::state::AppState;

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
const START_CANCELED: &str = "Playback was canceled while waiting for the audio preview.";

#[derive(Clone)]
struct Lease {
    window: String,
    token: String,
}

#[derive(Default)]
struct FocusState {
    preview: Option<Lease>,
    native_starts: usize,
    cancel_epoch: u64,
}

#[derive(Default)]
pub(crate) struct PreviewFocus {
    state: Mutex<FocusState>,
    changed: Condvar,
}

impl PreviewFocus {
    fn lock(&self) -> MutexGuard<'_, FocusState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn claim(&self, window: &str, token: &str) -> bool {
        let mut state = self.lock();
        if state.native_starts != 0
            || state
                .preview
                .as_ref()
                .is_some_and(|lease| lease.window != window)
        {
            return false;
        }
        // A reloaded page in this window can replace its stale lease. The
        // frontend pauses its old player before sending any replacement claim.
        state.preview = Some(Lease {
            window: window.into(),
            token: token.into(),
        });
        true
    }

    fn release(&self, window: &str, token: Option<&str>) {
        let mut state = self.lock();
        if state.preview.as_ref().is_some_and(|lease| {
            lease.window == window && token.is_none_or(|token| lease.token == token)
        }) {
            state.preview = None;
            self.changed.notify_all();
        }
    }

    pub(crate) fn window_closed(&self, window: &str) {
        self.release(window, None);
    }

    pub(crate) fn cancel_pending_start(&self) {
        let mut state = self.lock();
        state.cancel_epoch = state.cancel_epoch.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(crate) fn start_epoch(&self) -> u64 {
        self.lock().cancel_epoch
    }

    pub(crate) fn start_native<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        epoch: u64,
    ) -> Result<NativeStart<'_>, String> {
        self.start(epoch, HANDOFF_TIMEOUT, |lease| {
            app.emit_to(
                lease.window.as_str(),
                "native-playback-starting",
                &lease.token,
            )
            .map_err(|error| format!("Cannot pause the audio preview: {error}"))
        })
    }

    fn start(
        &self,
        epoch: u64,
        timeout: Duration,
        notify: impl FnOnce(&Lease) -> Result<(), String>,
    ) -> Result<NativeStart<'_>, String> {
        let preview = {
            let mut state = self.lock();
            if state.cancel_epoch != epoch {
                return Err(START_CANCELED.into());
            }
            state.native_starts += 1;
            state.preview.clone()
        };
        let guard = NativeStart(self);
        if let Some(preview) = preview {
            notify(&preview)?;
            let (state, _) = self
                .changed
                .wait_timeout_while(self.lock(), timeout, |state| {
                    state.preview.is_some() && state.cancel_epoch == epoch
                })
                .unwrap_or_else(|error| error.into_inner());
            if state.cancel_epoch != epoch {
                return Err(START_CANCELED.into());
            }
            if state.preview.is_some() {
                return Err("The audio preview did not pause. Stop it and retry playback.".into());
            }
        }
        Ok(guard)
    }
}

pub(crate) struct NativeStart<'a>(&'a PreviewFocus);

impl Drop for NativeStart<'_> {
    fn drop(&mut self) {
        self.0.lock().native_starts -= 1;
    }
}

#[tauri::command]
pub(crate) fn claim_web_audio<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
    token: String,
) -> bool {
    !token.is_empty() && token.len() <= 64 && state.preview_focus.claim(window.label(), &token)
}

#[tauri::command]
pub(crate) fn release_web_audio<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
    token: String,
) {
    state.preview_focus.release(window.label(), Some(&token));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn transport_play_ipc_targets_the_claiming_window_and_waits_for_its_ack() {
        check_playback_handoff("transport_play", serde_json::json!({}));
    }

    #[test]
    fn track_preview_ipc_waits_for_the_preview_owner() {
        check_playback_handoff("preview_track", serde_json::json!({ "trackIndex": 0 }));
    }

    #[test]
    fn track_preview_resume_ipc_waits_for_the_preview_owner() {
        check_playback_handoff(
            "resume_track_preview",
            serde_json::json!({ "trackIndex": 0 }),
        );
    }

    fn check_playback_handoff(command: &str, args: serde_json::Value) {
        use serde_json::json;
        use tauri::test::{MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets};
        use tauri::{Listener, WebviewWindowBuilder};

        fn request(
            window: &WebviewWindow<MockRuntime>,
            command: &str,
            args: serde_json::Value,
        ) -> tauri::ipc::InvokeResponseBody {
            get_ipc_response(
                window,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "tauri://localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(args),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_owned(),
                },
            )
            .unwrap()
        }

        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.audio.enable_simulated_output();
            inner.project = oc_dsl::parse(
                "@project\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track lead | preset: tone, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 w mf\n",
            )
            .unwrap();
        }
        let app = mock_builder()
            .manage(state)
            .invoke_handler(tauri::generate_handler![
                claim_web_audio,
                release_web_audio,
                crate::commands::transport_play,
                crate::commands::preview_track,
                crate::commands::resume_track_preview
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let main = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let other = WebviewWindowBuilder::new(&app, "other", Default::default())
            .build()
            .unwrap();
        if command == "resume_track_preview" {
            let prepared = request(
                &main,
                "preview_track",
                json!({ "trackIndex": 0, "startPaused": true }),
            )
            .deserialize::<serde_json::Value>()
            .unwrap();
            assert_eq!(prepared["playing"], false);
        }
        assert!(
            request(&main, "claim_web_audio", json!({ "token": "preview" }))
                .deserialize::<bool>()
                .unwrap()
        );
        let (notified, event) = mpsc::channel();
        main.listen("native-playback-starting", move |event| {
            notified
                .send(serde_json::from_str::<String>(event.payload()).unwrap())
                .unwrap();
        });
        let (unexpected, wrong_window) = mpsc::channel();
        other.listen("native-playback-starting", move |_| {
            unexpected.send(()).unwrap();
        });
        std::thread::scope(|scope| {
            let (ready, playing) = mpsc::channel();
            let caller = other.clone();
            scope.spawn(move || {
                let snapshot = request(&caller, command, args)
                    .deserialize::<serde_json::Value>()
                    .unwrap();
                ready.send(snapshot).unwrap();
            });
            let token = event.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(token, "preview");
            assert!(wrong_window.try_recv().is_err());
            request(&other, "release_web_audio", json!({ "token": token }));
            assert!(
                !request(&main, "claim_web_audio", json!({ "token": "new" }))
                    .deserialize::<bool>()
                    .unwrap()
            );
            assert!(playing.try_recv().is_err());
            request(&main, "release_web_audio", json!({ "token": token }));
            let snapshot = playing.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(snapshot["playing"], true);
        });
    }

    #[test]
    fn external_play_waits_for_the_matching_preview_and_fences_new_claims() {
        let focus = PreviewFocus::default();
        assert!(focus.claim("main", "playing"));
        std::thread::scope(|scope| {
            let (notified, event) = mpsc::channel();
            let (started, native) = mpsc::channel();
            let (finish, done) = mpsc::channel();
            let focus = &focus;
            scope.spawn(move || {
                let _guard = focus
                    .start(focus.start_epoch(), Duration::from_secs(2), |lease| {
                        notified
                            .send((lease.window.clone(), lease.token.clone()))
                            .unwrap();
                        Ok(())
                    })
                    .unwrap();
                started.send(()).unwrap();
                done.recv_timeout(Duration::from_secs(2)).unwrap();
            });
            assert_eq!(
                event.recv_timeout(Duration::from_secs(2)).unwrap(),
                ("main".into(), "playing".into())
            );
            assert!(!focus.claim("main", "new"));
            focus.release("other", Some("playing"));
            focus.release("main", Some("old"));
            assert!(native.try_recv().is_err());
            focus.release("main", Some("playing"));
            native.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(!focus.claim("main", "still too early"));
            finish.send(()).unwrap();
        });
        assert!(focus.claim("main", "now"));
    }

    #[test]
    fn a_missing_ack_refuses_play_and_a_closed_window_releases_the_lease() {
        let focus = PreviewFocus::default();
        assert!(focus.claim("main", "stale"));
        assert!(
            focus
                .start(focus.start_epoch(), Duration::ZERO, |_| Ok(()))
                .is_err()
        );
        assert!(!focus.claim("other", "blocked"));
        focus.window_closed("main");
        let _guard = focus
            .start(focus.start_epoch(), Duration::ZERO, |_| {
                panic!("no preview to notify")
            })
            .unwrap();
        assert!(!focus.claim("other", "native has priority"));
    }

    #[test]
    fn stop_cancels_a_waiting_external_start() {
        let focus = PreviewFocus::default();
        assert!(focus.claim("main", "preview"));
        let result = focus.start(focus.start_epoch(), Duration::from_secs(2), |_| {
            focus.cancel_pending_start();
            focus.release("main", Some("preview"));
            Ok(())
        });
        assert!(matches!(result, Err(error) if error.contains("canceled")));
        assert!(focus.claim("main", "new"));
    }

    #[test]
    fn stop_cancels_a_start_before_its_worker_runs() {
        let focus = PreviewFocus::default();
        assert!(focus.claim("main", "preview"));
        let queued = focus.start_epoch();
        focus.cancel_pending_start();
        let result = focus.start(queued, Duration::ZERO, |_| {
            panic!("canceled before dispatch")
        });
        assert!(matches!(result, Err(error) if error == START_CANCELED));
        assert!(focus.claim("main", "still available"));
    }
}
