/**
 * Display formatting for meter readings. An absent reading prints a dash: a
 * meter never invents 0 dBFS, and callers can tell "no reading" from "silence".
 */

const NO_READING = "—";
/** Bottom of the drawn bar scale, in dBFS. */
export const BAR_FLOOR_DB = -60;
/** Top of the drawn bar scale; the clip hold covers anything above it. */
export const BAR_CEILING_DB = 0;

/** One dBFS value, one decimal, with an explicit sign above unity. */
export function formatLevelDb(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return NO_READING;
  const text = value.toFixed(1);
  return value > 0 ? `+${text}` : text;
}

/** One correlation value in `-1..=1`, two decimals and a sign. */
export function formatCorrelation(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return NO_READING;
  const clamped = Math.min(1, Math.max(-1, value));
  return `${clamped >= 0 ? "+" : ""}${clamped.toFixed(2)}`;
}

/** A dBFS reading as the percentage its bar fills, floor to ceiling. */
export function levelPercent(value: number | null | undefined): number {
  if (value === null || value === undefined || !Number.isFinite(value)) return 0;
  const span = BAR_CEILING_DB - BAR_FLOOR_DB;
  return Math.min(100, Math.max(0, ((value - BAR_FLOOR_DB) / span) * 100));
}

/** A LUFS reading with one decimal; absent prints a dash. */
export function formatLufs(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return NO_READING;
  return value.toFixed(1);
}
