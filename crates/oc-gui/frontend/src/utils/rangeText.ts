/** Bar and beat ranges in the app's language; shared by tool rows and history. */
import { i18n } from "../stores/i18n";

interface Position { bar: number; beat: number }

/** A `bar:beat` position; a subdivision stays in the raw bar:beat form. */
function positionOf(value: string): Position | null {
  const match = /^(\d+)(?::(\d+))?(?::(\d+))?$/.exec(value.trim());
  if (!match) return null;
  if (match[3] !== undefined && Number(match[3]) !== 0) return null;
  return { bar: Number(match[1]), beat: match[2] === undefined ? 1 : Number(match[2]) };
}

/** Two positions as bars, beats, or the raw bar:beat when neither fits. */
export function humanRange(start: string, end: string): string {
  const first = positionOf(start);
  const last = positionOf(end);
  if (first && last) {
    // DSL and tool ranges end exclusively; name the last bar or beat they cover.
    if (first.beat === 1 && last.beat === 1) {
      const to = last.bar - 1;
      if (to === first.bar) return i18n.t("ai.activity.barOne", { bar: first.bar });
      if (to > first.bar) return i18n.t("ai.activity.barRange", { start: first.bar, end: to });
    }
    if (first.bar === last.bar && last.beat > first.beat) {
      const to = last.beat - 1;
      if (to === first.beat) return i18n.t("ai.activity.barBeatOne", { bar: first.bar, beat: first.beat });
      return i18n.t("ai.activity.barBeats", { bar: first.bar, from: first.beat, to });
    }
    if (first.bar !== last.bar) {
      return i18n.t("ai.activity.barBeatRange", {
        fromBar: first.bar, fromBeat: first.beat, toBar: last.bar, toBeat: last.beat,
      });
    }
  }
  return i18n.t("ai.activity.rangeSpan", { start, end });
}

/** A clock position as the `bar:beat[:subdivision]` text `humanRange` reads. */
export function barBeatText(position: { bar: number; beat: number; subdivision: number }): string {
  return position.subdivision === 0
    ? `${position.bar}:${position.beat}`
    : `${position.bar}:${position.beat}:${position.subdivision}`;
}
