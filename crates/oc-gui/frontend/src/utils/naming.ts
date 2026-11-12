/**
 * Clip/pattern naming. Every creation path derives a non-empty, unique name:
 * the source name is preserved, otherwise a `name 2` / `name 3` suffix is used.
 */

/** Strip characters the DSL cannot quote (`"` and control chars) and collapse whitespace. */
export function sanitizeName(raw: string): string {
  return raw
    .replace(/[\u0000-\u001f"]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** Return `base`, or the first free `base 2`, `base 3`, ... */
export function uniqueName(base: string, existing: Iterable<string>): string {
  const taken = new Set(existing);
  const clean = sanitizeName(base) || "Clip";
  if (!taken.has(clean)) return clean;
  for (let n = 2; ; n++) {
    const candidate = `${clean} ${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}
