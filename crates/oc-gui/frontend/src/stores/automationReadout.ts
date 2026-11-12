/**
 * The value a lane drives into a control, refreshed on the transport poll.
 *
 * A control registers the lane it displays only when the project actually has
 * that lane, so an unautomated project makes no extra calls. The backend owns
 * the "not driven" reasons (muted owner, stopped transport, before the first
 * point, no lane), so the badge and the tooltip cannot disagree with the audio.
 */
import { createSignal } from "solid-js";

import * as api from "./api";
import type { AutomationReadout } from "./api";

/** Which lane one mounted control displays. */
interface Watched {
  target: string;
  lane: string;
}

function id(target: string, lane: string): string {
  return `${target}\u0000${lane}`;
}

const [readouts, setReadouts] = createSignal<Record<string, AutomationReadout>>({});
const watched = new Map<string, Watched>();
let inFlight = false;
let lastPosition = 0;
let lastPlaying = false;

/** The last answer for one lane, or `undefined` before the first read. */
export function automationReadout(
  target: string,
  lane: string,
): AutomationReadout | undefined {
  return readouts()[id(target, lane)];
}

function same(a: AutomationReadout | undefined, b: AutomationReadout): boolean {
  if (!a || a.status !== b.status) return false;
  if (a.status === "driven" && b.status === "driven") {
    return a.value === b.value && a.domain === b.domain && a.smoothing_ms === b.smoothing_ms;
  }
  return a.status === "not_driven" && b.status === "not_driven" && a.reason === b.reason;
}

function publish(target: string, lane: string, answer: AutomationReadout): void {
  const key = id(target, lane);
  setReadouts((current) => (same(current[key], answer) ? current : { ...current, [key]: answer }));
}

/** Register the lane a control displays and read it at `positionBeats`. */
export function watchAutomation(target: string, lane: string, positionBeats: number): void {
  const key = id(target, lane);
  const known = watched.get(key);
  watched.set(key, { target, lane });
  if (known) return;
  void readAll([{ target, lane }], positionBeats);
}

/** Drop a lane no control displays any more, and its cached answer. */
export function unwatchAutomation(target: string, lane: string): void {
  const key = id(target, lane);
  watched.delete(key);
  setReadouts((current) => {
    if (!(key in current)) return current;
    const next = { ...current };
    delete next[key];
    return next;
  });
}

/**
 * Refresh every watched lane at the polled position. The transport poll calls
 * this at 20 Hz; an unchanged position and play state make it a no-op, so a
 * stopped transport costs nothing.
 */
export function pollAutomationReadouts(positionBeats: number, playing: boolean): void {
  if (positionBeats === lastPosition && playing === lastPlaying) return;
  lastPosition = positionBeats;
  lastPlaying = playing;
  if (watched.size === 0) return;
  void readAll([...watched.values()], positionBeats);
}

async function readAll(entries: Watched[], positionBeats: number): Promise<void> {
  if (inFlight || entries.length === 0) return;
  inFlight = true;
  const at = Number.isFinite(positionBeats) ? Math.max(0, positionBeats) : 0;
  try {
    const answers = await Promise.all(
      entries.map((entry) => api.automationValueAt(entry.target, entry.lane, at)),
    );
    answers.forEach((answer, index) => {
      const entry = entries[index];
      if (!entry || !answer) return;
      if (answer.status === "driven" || answer.status === "not_driven") {
        publish(entry.target, entry.lane, answer);
      }
    });
  } catch (error) {
    for (const entry of entries) {
      publish(entry.target, entry.lane, { status: "not_driven", reason: String(error) });
    }
  } finally {
    inFlight = false;
  }
}
