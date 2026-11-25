//! Build the inline asset before a graph can become audible.
use super::AudioEffect;
use oc_dsl::ast::ParamValue;
use oc_dsp::schema::transfer_curve::TransferCurve;
use oc_dsp::transfer_shaper::TransferShaper;

pub(super) fn create(params: &[(String, ParamValue)]) -> Result<Box<dyn AudioEffect>, String> {
    let device = oc_dsp::schema::find_device("transfer-shaper").expect("registered device");
    let mut seen = std::collections::HashSet::new();
    let mut curve = TransferCurve::default();
    for (name, value) in params {
        if name == crate::idle_gate::SMART_DISABLE_PARAM {
            continue;
        }
        if !seen.insert(name) {
            return Err(format!("duplicate effect parameter: {name}"));
        }
        super::validate_static_value(device, name, value)?;
        if let ("curve", ParamValue::String(text)) = (name.as_str(), value) {
            curve = TransferCurve::decode(text).map_err(|e| e.to_string())?;
        }
    }
    let mut fx = TransferShaper::new(curve).map_err(|e| e.to_string())?;
    for (name, value) in params {
        if name == crate::idle_gate::SMART_DISABLE_PARAM {
            continue;
        }
        if name != "curve" {
            fx.set_param(name, super::validate_static_param(device, name, value)?);
        }
    }
    Ok(Box::new(fx))
}

struct FailedCurve {
    cause: String,
    reported: std::sync::atomic::AtomicBool,
}
impl AudioEffect for FailedCurve {
    fn processing_failed(&self) -> bool {
        true
    }
    fn take_processing_failure(&self) -> oc_audio::ProcessingFailureReport {
        if self
            .reported
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            oc_audio::ProcessingFailureReport::Reported
        } else {
            oc_audio::ProcessingFailureReport::Ready(self.cause.clone())
        }
    }
    fn process(&mut self, buffer: &mut [f32], _: u32) {
        buffer.fill(0.0);
    }
    fn reset(&mut self) {}
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn name(&self) -> &str {
        "Invalid transfer curve"
    }
}
pub(super) fn failed(error: String) -> Box<dyn AudioEffect> {
    Box::new(FailedCurve {
        cause: error,
        reported: std::sync::atomic::AtomicBool::new(false),
    })
}
