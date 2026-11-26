use vst3::Steinberg::{kNotImplemented, kResultOk, tresult};

/// Processing notifications are optional; component activation is not.
pub(super) fn processing_notification_succeeded(status: tresult) -> bool {
    status == kResultOk || status == kNotImplemented
}
