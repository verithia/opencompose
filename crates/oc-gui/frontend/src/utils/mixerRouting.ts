/**
 * Mixer routing helpers shared by the routing toolbar and its tests.
 * Pure functions only: no store access, no Tauri calls.
 */
import type { Bus, Project } from "../types/project";
import type { I18nKey } from "../stores/i18n";

/** Linear send gain ceiling, matching the DSL validator (+12 dB). */
export const SEND_MAX_AMOUNT = 4;

export function amountToDb(amount: number): number {
  return amount <= 0 ? -Infinity : 20 * Math.log10(amount);
}

export function dbToAmount(db: number): number {
  return Math.max(0, Math.min(SEND_MAX_AMOUNT, 10 ** (db / 20)));
}

/** A bus with no routes returns to master by the DSL's legacy default. */
export function effectiveBusRoutes(bus: Bus): string[] {
  return bus.routes.length === 0 ? ["master"] : bus.routes;
}

/** First free "Bus N" name, so the create button never needs a typed name. */
export function nextBusName(buses: { name: string }[]): string {
  const taken = new Set(buses.map((bus) => bus.name));
  for (let index = 1; ; index += 1) {
    const name = `Bus ${index}`;
    if (!taken.has(name)) return name;
  }
}

export interface BusReferences {
  trackOutputs: number[];
  sends: number[];
  busRoutes: string[];
}

export function busTotalReferences(refs: BusReferences): number {
  return refs.trackOutputs.length + refs.sends.length + refs.busRoutes.length;
}

/** Everything that would be re-routed or dropped by deleting this bus. */
export function busReferences(project: Project, name: string): BusReferences {
  return {
    trackOutputs: project.tracks
      .map((track, index) => (track.mix.output === name ? index : -1))
      .filter((index) => index >= 0),
    sends: project.tracks
      .map((track, index) => (track.mix.sends.some((send) => send.target === name) ? index : -1))
      .filter((index) => index >= 0),
    busRoutes: project.buses
      .filter((bus) => bus.name !== name && effectiveBusRoutes(bus).includes(name))
      .map((bus) => bus.name),
  };
}

/** "2 track outputs, 1 send and 1 bus route", localized by the caller. */
export function describeBusReferences(
  refs: BusReferences,
  t: (key: I18nKey, values?: Record<string, string | number>) => string,
): string {
  const parts: string[] = [];
  const add = (count: number, one: I18nKey, many: I18nKey) => {
    if (count === 1) parts.push(t(one, { count }));
    else if (count > 1) parts.push(t(many, { count }));
  };
  add(refs.trackOutputs.length, "routing.references.trackOutput", "routing.references.trackOutputs");
  add(refs.sends.length, "routing.references.send", "routing.references.sends");
  add(refs.busRoutes.length, "routing.references.busRoute", "routing.references.busRoutes");
  if (parts.length <= 1) return parts.join("");
  const first = parts.slice(0, -1).join(t("routing.references.separator"));
  return t("routing.references.pair", { first, second: parts[parts.length - 1] });
}
