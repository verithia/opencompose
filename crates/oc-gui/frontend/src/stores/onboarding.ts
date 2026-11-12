/**
 * First-run state. The splash and guide appear once per machine: the marker is
 * localStorage because it describes this installation, not a project.
 */
import { createSignal } from "solid-js";

const STORAGE_KEY = "opencompose.onboarding.v1";

function readPending(): boolean {
  if (typeof window === "undefined" || !window.localStorage) return false;
  try {
    return window.localStorage.getItem(STORAGE_KEY) === null;
  } catch (_) {
    // A browser that refuses storage cannot remember a dismiss; do not trap the
    // operator in first-run on every launch.
    return false;
  }
}

const [pending, setPending] = createSignal(readPending());

function complete() {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({ completed_at: new Date().toISOString() }));
  } catch (_) {}
  setPending(false);
}

function reset() {
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch (_) {}
  setPending(true);
}

export const onboardingStore = { pending, complete, reset, storageKey: STORAGE_KEY };
