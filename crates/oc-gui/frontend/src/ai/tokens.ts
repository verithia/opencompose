/**
 * Token-count text for the AI limit fields: a plain integer or a suffixed
 * count (`128K`, `1.5M`). Suffixes are decimal, the way context windows are
 * quoted (`1M` = 1,000,000), so a typed value means what it reads. Formatting
 * is lossless, so a displayed value parses back to the stored number.
 */

/** Largest count the backend stores, `u32::MAX`. */
export const MAX_TOKEN_COUNT = 4294967295;

const MULTIPLIERS: Record<string, number> = {
  k: 1000,
  m: 1000 * 1000,
  g: 1000 * 1000 * 1000,
};

const GRAMMAR = /^(\d+)(?:\.(\d+))?\s*([kmg])?$/i;

/** Returns the count, or null when the text is not a positive token count. */
export function parseTokenCount(text: string): number | null {
  const match = GRAMMAR.exec(text.trim());
  if (!match) return null;
  const [, whole, fraction, suffix] = match;
  if (fraction !== undefined && suffix === undefined) return null;
  const value = Number(`${whole}.${fraction ?? "0"}`) * (suffix ? MULTIPLIERS[suffix.toLowerCase()] : 1);
  const rounded = Math.round(value);
  if (!Number.isFinite(rounded) || rounded <= 0 || rounded > MAX_TOKEN_COUNT) return null;
  return rounded;
}

const UNITS: [string, number][] = [
  ["G", 1000 * 1000 * 1000],
  ["M", 1000 * 1000],
  ["K", 1000],
];

/**
 * `1000000 -> "1M"`, `1500000 -> "1.5M"`. Only a value the suffix can express
 * exactly (a whole or a half of the unit) is suffixed; anything else stays a
 * plain integer so the display never rounds.
 */
export function formatTokenCount(n: number): string {
  if (!Number.isInteger(n) || n <= 0) return String(n);
  for (const [suffix, size] of UNITS) {
    if (n < size) continue;
    const value = n / size;
    const fraction = value % 1;
    if (fraction === 0 || fraction === 0.5) return `${value}${suffix}`;
  }
  return String(n);
}
