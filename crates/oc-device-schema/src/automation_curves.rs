//! Bounds for compact analytic automation; independent of any source format.
pub const MIN_CURVATURE: f64 = -64.0;
pub const MAX_CURVATURE: f64 = 64.0;
pub const DEFAULT_CURVATURE: f64 = 0.0;
/// Keep prepared exponential arguments far from floating-point overflow.
pub const MAX_EXPONENT_ARGUMENT: f64 = 64.0;
pub const DEFAULT_GAIN_PPQ: u32 = 4;
pub const MIN_INPUT_QUANTIZATION_STEPS: u32 = 1;
pub const MAX_INPUT_QUANTIZATION_STEPS: u32 = 1_000_000;

pub fn valid_curvature(value: f64) -> bool {
    value.is_finite() && (MIN_CURVATURE..=MAX_CURVATURE).contains(&value)
}

pub fn render_typescript() -> String {
    let gain = crate::find_device("gain").unwrap().param("factor").unwrap();
    let (gain_min, gain_max) = (gain.min, gain.max);
    format!(
        "// @generated from oc-device-schema::automation_curves. Do not edit by hand.\nexport const AUTOMATION_CURVES = {{\n  curvature: {{ min: {MIN_CURVATURE}, max: {MAX_CURVATURE}, default: {DEFAULT_CURVATURE} }},\n  gain: {{ min: {gain_min}, max: {gain_max}, defaultPpq: {DEFAULT_GAIN_PPQ} }},\n  inputQuantizationSteps: {{ min: {MIN_INPUT_QUANTIZATION_STEPS}, max: {MAX_INPUT_QUANTIZATION_STEPS} }},\n  maxExponentArgument: {MAX_EXPONENT_ARGUMENT},\n}} as const;\n"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_automation_schema_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../oc-gui/frontend/src/data/automationCurves.gen.ts");
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            super::render_typescript()
        );
    }
}
