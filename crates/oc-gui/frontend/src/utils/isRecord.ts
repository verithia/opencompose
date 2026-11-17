/** A plain object, as untrusted JSON values are narrowed at the boundary. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
