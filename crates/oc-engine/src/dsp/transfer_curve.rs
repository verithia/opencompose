//! Build the inline asset before a graph can become audible.
use super::AudioEffect;
use oc_dsl::ast::ParamValue;
use oc_dsp::schema::transfer_curve::{MAX_BYTES, TransferCurve};
use oc_dsp::transfer_shaper::TransferShaper;
use std::path::Path;

pub(super) fn create(
    params: &[(String, ParamValue)],
    resource_root: Option<&Path>,
) -> Result<Box<dyn AudioEffect>, String> {
    let device = oc_dsp::schema::find_device("transfer-shaper").expect("registered device");
    let mut seen = std::collections::HashSet::new();
    let mut curve = None;
    for (name, value) in params {
        if name == crate::idle_gate::SMART_DISABLE_PARAM {
            continue;
        }
        if !seen.insert(name) {
            return Err(format!("duplicate effect parameter: {name}"));
        }
        super::validate_static_value(device, name, value)?;
        if matches!(name.as_str(), "curve" | "curve_file") {
            if curve.is_some() {
                return Err("declare only one of curve and curve_file".into());
            }
            let ParamValue::String(text) = value else {
                unreachable!("asset type was validated");
            };
            curve = Some(if name == "curve" {
                TransferCurve::decode(text).map_err(|error| error.to_string())?
            } else {
                read_curve(text, resource_root)?
            });
        }
    }
    let mut fx = TransferShaper::new(curve.unwrap_or_default()).map_err(|e| e.to_string())?;
    for (name, value) in params {
        if name == crate::idle_gate::SMART_DISABLE_PARAM {
            continue;
        }
        if !matches!(name.as_str(), "curve" | "curve_file") {
            fx.set_param(name, super::validate_static_param(device, name, value)?);
        }
    }
    Ok(Box::new(fx))
}

fn read_curve(file: &str, root: Option<&Path>) -> Result<TransferCurve, String> {
    let path = Path::new(file);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.ok_or("relative curve_file requires a project resource directory")?
            .join(path)
    };
    let result = (|| {
        let bytes = oc_audio::bounded_file::read_regular_state_file(&resolved, MAX_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        TransferCurve::decode(text).map_err(|error| error.to_string())
    })();
    result.map_err(|error| format!("cannot load transfer curve '{file}': {error}"))
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
