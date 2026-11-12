/**
 * Analyser subscription: the engine only runs the FFT while an EQ panel asks
 * for it, and the last release stops it.
 */
import * as api from "./api";

let subscribers = 0;
let target: string | null = null;

/** Subscribe the analyser to `nextTarget`; safe to call repeatedly. */
export function acquireSpectrum(nextTarget: string): () => void {
  subscribers += 1;
  if (target !== nextTarget) {
    target = nextTarget;
    void api.setSpectrum(nextTarget).catch(() => undefined);
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    subscribers = Math.max(0, subscribers - 1);
    if (subscribers === 0 && target !== null) {
      target = null;
      void api.setSpectrum(null).catch(() => undefined);
    }
  };
}

/** Only for tests: report whether the analyser is currently requested. */
export function spectrumSubscribers(): number {
  return subscribers;
}
