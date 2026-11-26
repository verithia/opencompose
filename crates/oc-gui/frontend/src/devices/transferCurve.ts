import { DEVICE_CATALOG } from "../data/deviceCatalog.gen";
export interface TransferCurve { version: 1; domain: "odd" | "bipolar"; samples: number[] }
export const curveAsset = () => DEVICE_CATALOG.assets!.find(asset => asset.device === "transfer-shaper")!;
/** Display parsing never rewrites the persisted asset. The backend owns admission. */
export function decodeCurve(text: string): TransferCurve {
  const schema = curveAsset();
  if (new TextEncoder().encode(text).length > schema.max_bytes) throw new Error("curve_bytes");
  const v: unknown = JSON.parse(text);
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error("curve_shape");
  const c = v as TransferCurve;
  if (Object.keys(v).some(key => !["version", "domain", "samples"].includes(key)) || c.version !== schema.version
    || !schema.domains.includes(c.domain) || !Array.isArray(c.samples) || c.samples.length < 2 || c.samples.length > schema.max_samples
    || c.samples.some(n => typeof n !== "number" || !Number.isFinite(n) || Math.abs(n) > schema.max_level)) throw new Error("curve_values");
  const middle = (c.samples.length - 1) / 2;
  const zero = c.domain === "odd" ? c.samples[0] : c.samples[Math.floor(middle)] + (c.samples[Math.ceil(middle)] - c.samples[Math.floor(middle)]) * (middle % 1);
  if (zero !== 0) throw new Error("curve_zero");
  return c;
}
export function curvePath(curve: TransferCurve, height = 120): string {
  const stride = Math.max(1, Math.ceil(curve.samples.length / 640));
  const level = curve.samples.reduce((max, value) => Math.max(max, Math.abs(value)), 1);
  const point = (i: number) => `${320 * i / (curve.samples.length - 1)},${height / 2 - curve.samples[i] / level * (height / 2 - 4)}`;
  let path = `M${point(0)}`;
  for (let i = stride; i < curve.samples.length - 1; i += stride) path += ` L${point(i)}`;
  return path + ` L${point(curve.samples.length - 1)}`;
}
