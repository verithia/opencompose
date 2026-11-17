/** A byte count in the app's language: one decimal, binary units. */
import { i18n } from "../stores/i18n";

const UNITS = [
  { scale: 1024 ** 3, suffix: "GiB" },
  { scale: 1024 ** 2, suffix: "MiB" },
  { scale: 1024, suffix: "KiB" },
] as const;

export function bytesText(bytes: number): string {
  const value = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  const format = (amount: number) =>
    new Intl.NumberFormat(i18n.locale(), {
      minimumFractionDigits: 1,
      maximumFractionDigits: 1,
    }).format(amount);
  for (const unit of UNITS) {
    if (value >= unit.scale) return `${format(value / unit.scale)} ${unit.suffix}`;
  }
  return `${new Intl.NumberFormat(i18n.locale()).format(Math.round(value))} B`;
}
