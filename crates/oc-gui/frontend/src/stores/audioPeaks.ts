/**
 * Frontend peak cache: one payload per media path, decoded once and shared by
 * every clip that references the file. Requests are deduplicated while in flight.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import { decodePeakBase64, type PeakPyramid } from "../utils/waveform";

const pyramids = new Map<string, PeakPyramid>();
const pending = new Map<string, Promise<PeakPyramid | null>>();
/// Files that produced no payload: the media is absent or unreadable.
const missing = new Set<string>();
const [version, setVersion] = createSignal(0);
const [error, setError] = createSignal<string | null>(null);

/** Reactive counter; read it to redraw when a payload lands or a file fails. */
export const peaksVersion = version;

export function peaksFor(file: string): PeakPyramid | null {
  return pyramids.get(file) ?? null;
}

/** Whether a media file produced no waveform, so its block has nothing to edit. */
export function peaksMissing(file: string): boolean {
  version();
  return missing.has(file);
}

export function loadPeaks(file: string): Promise<PeakPyramid | null> {
  const cached = pyramids.get(file);
  if (cached) return Promise.resolve(cached);
  if (missing.has(file)) return Promise.resolve(null);
  const inFlight = pending.get(file);
  if (inFlight) return inFlight;
  const request = api
    .audioClipPeaks(file)
    .then((payload) => {
      const pyramid = decodePeakBase64(payload);
      if (!pyramid) {
        missing.add(file);
        setError(`Unreadable peak payload for ${file}`);
        setVersion(version() + 1);
        return null;
      }
      pyramids.set(file, pyramid);
      setVersion(version() + 1);
      return pyramid;
    })
    .catch((reason) => {
      missing.add(file);
      setError(String(reason));
      setVersion(version() + 1);
      return null;
    })
    .finally(() => pending.delete(file));
  pending.set(file, request);
  return request;
}

export function peaksError(): string | null {
  return error();
}

export function resetPeaks(): void {
  pyramids.clear();
  pending.clear();
  missing.clear();
  setError(null);
  setVersion(version() + 1);
}
