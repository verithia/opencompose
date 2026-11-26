/**
 * Frontend peak cache: one payload per media path, decoded once and shared by
 * every clip that references the file. Requests are deduplicated while in flight.
 */
import { batch, createSignal, type Accessor, type Setter } from "solid-js";
import * as api from "./api";
import { decodePeakBase64, type PeakPyramid } from "../utils/waveform";

const pyramids = new Map<string, PeakPyramid>();
const pending = new Map<string, Promise<PeakPyramid | null>>();
const fileVersions = new Map<string, [Accessor<number>, Setter<number>]>();
let generation = 0;
/// Files that produced no payload: the media is absent or unreadable.
const missing = new Set<string>();
const [version, setVersion] = createSignal(0);
const [error, setError] = createSignal<string | null>(null);

/** Compatibility notification for consumers that explicitly need all media. */
export const peaksVersion = version;

function fileVersion(file: string): [Accessor<number>, Setter<number>] {
  let entry = fileVersions.get(file);
  if (!entry) {
    entry = createSignal(0);
    fileVersions.set(file, entry);
  }
  return entry;
}

function notify(file: string): void {
  batch(() => {
    fileVersion(file)[1]((value) => value + 1);
    setVersion((value) => value + 1);
  });
}

/** Reactive for this file only; unrelated peak arrivals do not repaint its clips. */
export function peaksFor(file: string): PeakPyramid | null {
  fileVersion(file)[0]();
  return pyramids.get(file) ?? null;
}

/** Whether a media file produced no waveform, so its block has nothing to edit. */
export function peaksMissing(file: string): boolean {
  fileVersion(file)[0]();
  return missing.has(file);
}

export function loadPeaks(file: string): Promise<PeakPyramid | null> {
  const cached = pyramids.get(file);
  if (cached) return Promise.resolve(cached);
  if (missing.has(file)) return Promise.resolve(null);
  const inFlight = pending.get(file);
  if (inFlight) return inFlight;
  const requestedGeneration = generation;
  const request = api
    .audioClipPeaks(file)
    .then((payload) => {
      if (generation !== requestedGeneration) return null;
      const pyramid = decodePeakBase64(payload);
      if (!pyramid) {
        missing.add(file);
        setError(`Unreadable peak payload for ${file}`);
        notify(file);
        return null;
      }
      pyramids.set(file, pyramid);
      notify(file);
      return pyramid;
    })
    .catch((reason) => {
      if (generation !== requestedGeneration) return null;
      missing.add(file);
      setError(String(reason));
      notify(file);
      return null;
    })
    .finally(() => {
      if (pending.get(file) === request) pending.delete(file);
    });
  pending.set(file, request);
  return request;
}

export function peaksError(): string | null {
  return error();
}

export function resetPeaks(): void {
  generation += 1;
  batch(() => {
    pyramids.clear();
    pending.clear();
    missing.clear();
    setError(null);
    const previous = [...fileVersions.values()];
    fileVersions.clear();
    // Active computations resubscribe to fresh entries when this batch ends;
    // unmounted documents must not retain their per-file signals indefinitely.
    for (const [, publish] of previous) publish((value) => value + 1);
    setVersion((value) => value + 1);
  });
}
