//! The host services every hosted plugin sees, independent of any one plugin.
//!
//! These cases use only the public host API, so they run without an installed
//! bundle: `Vst3Host` is the component handler a plugin queries.

use std::sync::Arc;

use oc_host::host_app::Vst3Host;
use vst3::Steinberg::Vst::{
    IComponentHandler, IComponentHandler2, IComponentHandler2Trait, IComponentHandlerTrait,
    RestartFlags_,
};
use vst3::Steinberg::{FIDString, kResultOk};

/// `IComponentHandler` and `IComponentHandler2` are one contract: a plugin that
/// reports a restart, a dirty document or an editor request must be seen by the
/// host rather than silently answered with `kResultOk`.
#[test]
fn the_host_handler_records_every_component_handler2_callback() {
    let host = Vst3Host::new();
    let sink = host.edit_sink();
    let handler = host
        .to_com_ptr::<IComponentHandler>()
        .expect("the host exposes IComponentHandler");
    let handler2 = handler
        .cast::<IComponentHandler2>()
        .expect("the host exposes IComponentHandler2 on the same object");

    let flags = RestartFlags_::kParamValuesChanged | RestartFlags_::kIoChanged;
    // SAFETY: `handler` is a live component handler.
    assert_eq!(unsafe { handler.restartComponent(flags) }, kResultOk);
    assert_eq!(sink.take_restart(), flags);
    assert_eq!(sink.take_restart(), 0, "a restart is delivered once");

    // SAFETY: a plugin may call `setDirty` from its own edit path.
    assert_eq!(unsafe { handler2.setDirty(0) }, kResultOk);
    assert!(!sink.take_dirty(), "false does not mark the document dirty");
    // SAFETY: same contract as above.
    assert_eq!(unsafe { handler2.setDirty(1) }, kResultOk);
    assert!(sink.take_dirty());
    assert!(!sink.take_dirty(), "the dirty flag is taken once");

    let name = c"editor";
    assert_eq!(
        // SAFETY: the live COM handler and NUL-terminated name outlive the call.
        unsafe { handler2.requestOpenEditor(name.as_ptr() as FIDString) },
        kResultOk
    );
    assert_eq!(sink.take_editor_request().as_deref(), Some("editor"));
    assert!(sink.take_editor_request().is_none());

    // A group edit is a gesture around many edits, not a no-op.
    let bus = oc_host::param_edit::bus_for_preset("handler-group-edit", [1_u32]);
    sink.bind_bus(Arc::clone(&bus));
    // SAFETY: the group-edit callbacks are part of the same live handler.
    assert_eq!(unsafe { handler2.startGroupEdit() }, kResultOk);
    assert!(bus.gesture_active(), "a group edit opens a gesture");
    // SAFETY: same contract as above.
    assert_eq!(unsafe { handler2.finishGroupEdit() }, kResultOk);
    assert!(!bus.gesture_active(), "finishing a group edit closes it");
}

/// A bundle the loader refuses must report the loader's own reason, not a
/// generic failure: a host that cannot load a plugin has to say why.
#[test]
fn a_bundle_the_loader_refuses_reports_the_loader_error() {
    let error = match oc_host::vst3_instance::Vst3Instance::load(
        std::path::Path::new("/nonexistent/opencompose-conformance.vst3"),
        48_000,
        512,
    ) {
        Ok(_) => panic!("a missing bundle cannot load"),
        Err(error) => error,
    };

    assert!(error.contains("Failed to load VST3"), "{error}");
    assert!(error.contains("opencompose-conformance.vst3"), "{error}");
    assert!(error.contains("does not exist"), "{error}");

    // A file that exists but is not a loadable image reaches the loader, whose
    // own message must survive instead of a generic failure.
    let junk = std::env::temp_dir().join(format!("opencompose-loader-{}.vst3", std::process::id()));
    std::fs::write(&junk, b"not a shared library").unwrap();
    let error = match oc_host::vst3_instance::Vst3Instance::load(&junk, 48_000, 512) {
        Ok(_) => panic!("a text file cannot load as a VST3 module"),
        Err(error) => error,
    };
    let _ = std::fs::remove_file(&junk);
    let reason = error
        .split_once(": ")
        .map(|(_, reason)| reason.trim().to_string())
        .unwrap_or_default();
    assert!(reason.len() > 8, "the loader's message is missing: {error}");
}
