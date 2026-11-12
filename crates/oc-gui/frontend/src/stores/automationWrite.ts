/**
 * The one automation write path: a session-wide arm, a time-driven sampler and
 * the report line the status bar shows.
 *
 * Every surface — the synth editor, the device panel and the hosted-plugin
 * panel — hands this store a surface describing one parameter. The store owns
 * everything the surfaces must agree on: whether a write is armed,
 * which grid cell a sample lands in, how many points a gesture produced, the
 * single undo entry, and the sentence the operator reads afterwards.
 *
 * The arm is session state, never project state: it is one signal shared by
 * every surface and it does not reach the DSL, the renderer or a saved file.
 */
import { createSignal } from "solid-js";

import { gridDivision } from "./grid";
import { i18n } from "./i18n";
import type { AutomationPosition } from "./synthAutomation";
import { formatAutomationPosition, writeCell, writeGridLabel } from "../utils/automationSampler";

/**
 * One parameter a gesture writes. `open`/`point`/`close` ride the surface's own
 * backend transaction, so a hosted plugin and a built-in synth keep their own
 * commands while the sampling, the deduplication and the report stay here.
 */
export interface AutomationWriteSurface {
  /** Stable identity of the written parameter; one gesture writes one key. */
  key: string;
  /** Target the report line names, e.g. `filter.cutoff` or `compression.threshold`. */
  label: string;
  /** The control's value right now, in the unit its surface writes in. */
  value(): number;
  /** Storable position of a snapped quarter note. */
  position(quarter: number): AutomationPosition;
  /** Open the surface's transaction; called before the first point. */
  open(): Promise<void>;
  /** Write one point at a position the sampler already snapped. */
  point(position: AutomationPosition, value: number): Promise<void>;
  /** Close the transaction; the backend turns it into one undo entry. */
  close(): Promise<void>;
}

/** Which arm state the writer is in; every surface reads this one signal. */
const [armed, setArmedSignal] = createSignal(false);
/** The sentence the status bar shows after a gesture, or `null` before one. */
const [report, setReport] = createSignal<string | null>(null);

export const automationWriteArmed = armed;
export const automationWriteReport = report;

/** Turn the global arm on or off. Turning it off closes an open gesture. */
export function setAutomationWriteArmed(next: boolean): void {
  setArmedSignal(next);
  if (!next) endAutomationWrite();
}

export function toggleAutomationWriteArm(): void {
  setAutomationWriteArmed(!armed());
}

export function clearAutomationWriteReport(): void {
  setReport(null);
}

interface WrittenPoint {
  value: number;
  position: AutomationPosition;
}

const writeHandleBrand: unique symbol = Symbol("automation-write-owner");
/** One interaction's authority, never reused even for the same parameter. */
export interface AutomationWriteHandle {
  readonly [writeHandleBrand]: true;
}

interface Session {
  handle: AutomationWriteHandle;
  surface: AutomationWriteSurface;
  division: number;
  /** Last point per grid cell; the report counts these. */
  cells: Map<number, WrittenPoint>;
  /** Samples the running gesture has taken. */
  samples: number;
  /** Serialised backend calls, so a point never overtakes its open. */
  queue: Promise<void>;
  opening: Promise<void> | null;
  failure: string | null;
}

let session: Session | null = null;
// A closed session may still own queued backend work. Keep its completion
// available after session becomes null and order the next open behind it.
let closing: Promise<void> = Promise.resolve();

function fail(current: Session, error: unknown): void {
  if (current.failure === null) current.failure = String(error);
}

/** Enqueue one backend call; the first failure is the one reported. */
function enqueue(current: Session, call: () => Promise<void>): void {
  current.queue = current.queue.then(call).catch((error: unknown) => fail(current, error));
}

function sampleValue(current: Session, quarter: number, value: number): void {
  if (!Number.isFinite(value)) return;
  current.samples += 1;
  const { cell, quarter: snapped } = writeCell(current.division, quarter);
  const known = current.cells.get(cell);
  if (known && known.value === value) return;
  const position = current.surface.position(snapped);
  current.cells.set(cell, { value, position });
  enqueue(current, async () => {
    current.opening ??= current.surface.open();
    await current.opening;
    await current.surface.point(position, value);
  });
}

/** Open a new interaction, closing any previous owner even on the same key. */
export function openAutomationWrite(surface: AutomationWriteSurface): AutomationWriteHandle | null {
  if (!armed()) {
    setReport(i18n.t("automation.write.off"));
    return null;
  }
  endAutomationWrite();
  const handle: AutomationWriteHandle = Object.freeze({ [writeHandleBrand]: true as const });
  session = {
    handle,
    surface,
    division: gridDivision(),
    cells: new Map(),
    samples: 0,
    queue: closing,
    opening: null,
    failure: null,
  };
  return handle;
}

/** Compatibility admission; new controls retain openAutomationWrite's handle. */
export function beginAutomationWrite(surface: AutomationWriteSurface): boolean {
  if (armed() && session?.surface.key === surface.key) return true;
  return openAutomationWrite(surface) !== null;
}

/** Compatibility query for a surface; input events use handle identity instead. */
export function isAutomationWriting(surface: AutomationWriteSurface): boolean {
  return session?.surface.key === surface.key;
}

export function isAutomationWriteActive(handle: AutomationWriteHandle): boolean {
  return session?.handle === handle;
}

/** An old input/timer cannot sample or close a newer same-key interaction. */
export function sampleAutomationWrite(
  handle: AutomationWriteHandle,
  quarter: number,
  value?: number,
): boolean {
  if (!session || session.handle !== handle || !armed()) return false;
  sampleValue(session, quarter, value ?? session.surface.value());
  return true;
}

export function closeAutomationWrite(handle: AutomationWriteHandle): Promise<void> {
  return session?.handle === handle ? endAutomationWrite() : closing;
}

/** Record one sample of the open gesture at `quarter`. */
export function writeAutomationSample(quarter: number): void {
  if (!session || !armed()) return;
  sampleValue(session, quarter, session.surface.value());
}

/** Write exactly one point: a press with no move, at the playhead. */
export function writeAutomationOnce(
  surface: AutomationWriteSurface,
  value: number,
  quarter: number,
): AutomationWriteHandle | null {
  const handle = openAutomationWrite(surface);
  if (!handle || !session) return null;
  sampleValue(session, quarter, value);
  void closeAutomationWrite(handle);
  return handle;
}

/**
 * Close the current writer for global disarm/document/patch operations. The
 * optional surface keeps older callers compatible by key; event-owned controls
 * must use closeAutomationWrite(handle) to distinguish same-key interactions.
 */
export function endAutomationWrite(surface?: AutomationWriteSurface): Promise<void> {
  const current = session;
  if (!current) return closing;
  if (surface && current.surface.key !== surface.key) return closing;
  session = null;
  const points = current.cells.size;
  closing = current.queue.then(async () => {
    // `opening` is set by the queued first point, so a gesture that never
    // wrote one has nothing to close and therefore no undo entry.
    if (current.opening) {
      try {
        await current.surface.close();
      } catch (error) {
        fail(current, error);
      }
    }
    reportGesture(current, points);
  });
  return closing;
}

function reportGesture(current: Session, points: number): void {
  if (current.failure !== null) {
    setReport(i18n.t("automation.write.failed", { reason: current.failure }));
    return;
  }
  if (!armed() && points === 0) {
    setReport(i18n.t("automation.write.off"));
    return;
  }
  if (points === 0) {
    // A press that never moved records nothing, and says nothing.
    if (current.samples > 0) setReport(i18n.t("automation.write.empty"));
    return;
  }
  if (points === 1) {
    const [only] = [...current.cells.values()];
    setReport(
      i18n.t("automation.write.recordedOne", {
        position: formatAutomationPosition(only.position),
      }),
    );
    return;
  }
  setReport(
    i18n.t("automation.write.recorded", {
      count: String(points),
      target: current.surface.label,
      grid: writeGridLabel(current.division),
    }),
  );
}

/**
 * Sample the open gesture from the transport poll. A still finger therefore
 * records a horizontal line: every cell the playhead crosses holds the value
 * the control had at that tick.
 */
export function pollAutomationWrites(positionBeats: number, playing: boolean): void {
  if (!playing || !session) return;
  writeAutomationSample(positionBeats);
}

/** Drop the arm and any open gesture; the test harness resets between mounts. */
export function resetAutomationWriteForTests(): void {
  session = null;
  closing = Promise.resolve();
  setArmedSignal(false);
  setReport(null);
}
