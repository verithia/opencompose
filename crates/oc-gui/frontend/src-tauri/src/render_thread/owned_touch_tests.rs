//! Transport requests retire only touches that exist at their control boundary.

use super::{RenderCommand, RenderThreadHandle};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

fn park(handle: &RenderThreadHandle) -> Sender<()> {
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    handle.send(RenderCommand::TestBarrier {
        entered: entered_tx,
        release: release_rx,
    });
    reached(entered_rx);
    release_tx
}
fn reached(receiver: Receiver<()>) {
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn queued_stop_cannot_erase_a_new_owner_admitted_after_its_request() {
    let handle = RenderThreadHandle::spawn(8000);
    handle.send(RenderCommand::ProjectChanged {
        project: Box::new(oc_dsl::parse("@project \"Touch transport\"\n").unwrap()),
        soundfont_path: Default::default(),
        root: None,
        warm: false,
    });
    handle.wait_project_loaded(Duration::from_secs(5)).unwrap();
    let release_worker = park(&handle);
    let registry = handle.touch();
    registry.begin("legacy", 0.1);
    let old = registry.begin_owned("effect", 0.2).unwrap();
    handle.send(RenderCommand::Stop);
    // The worker is blocked, proving retirement is synchronous at request.
    assert_eq!(registry.status("legacy"), None);
    assert_eq!(registry.status("effect"), None);
    handle.send(RenderCommand::Play {
        start_frame: 0,
        resume: false,
        end_frame: None,
    });
    let fresh = registry.begin_owned("effect", 0.4).unwrap();
    assert!(!registry.update_owned("effect", &old, 0.9));
    release_worker.send(()).unwrap();
    // FIFO barrier proves the older Stop and subsequent Play were consumed.
    let after_stop = park(&handle);
    assert!(registry.is_held("effect"));
    assert_eq!(registry.status("effect").unwrap().hand, 0.4);
    assert!(registry.update_owned("effect", &fresh, 0.5));
    after_stop.send(()).unwrap();
}

#[test]
fn pause_request_retires_owned_provenance_but_preserves_legacy_touch() {
    let handle = RenderThreadHandle::spawn(8000);
    let release_worker = park(&handle);
    let registry = handle.touch();
    registry.begin("legacy", 0.1);
    let old = registry.begin_owned("effect", 0.2).unwrap();
    handle.send(RenderCommand::Pause);
    assert!(registry.is_held("legacy"));
    assert_eq!(registry.status("effect"), None);
    let fresh = registry.begin_owned("effect", 0.4).unwrap();
    release_worker.send(()).unwrap();
    let after_pause = park(&handle);
    assert!(!registry.release_owned("effect", &old));
    assert!(registry.update_owned("effect", &fresh, 0.5));
    after_pause.send(()).unwrap();
}
