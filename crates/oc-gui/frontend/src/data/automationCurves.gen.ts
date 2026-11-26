// @generated from oc-device-schema::automation_curves. Do not edit by hand.
export const AUTOMATION_CURVES = {
  curvature: { min: -64, max: 64, default: 0 },
  gain: { min: 0, max: 16, defaultPpq: 4 },
  inputQuantizationSteps: { min: 1, max: 1000000 },
  maxExponentArgument: 64,
} as const;
