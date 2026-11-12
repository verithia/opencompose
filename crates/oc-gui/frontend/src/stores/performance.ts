/**
 * Render-performance setting and the read-only facts the Performance page
 * shows. The backend owns the persisted value; this store mirrors the last
 * snapshot it answered.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import type { PerformanceFacts, PerformanceSettings } from "./api";

const [facts, setFacts] = createSignal<PerformanceFacts | null>(null);
const [saving, setSaving] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

/** A backend older than the page answers without the render facts. */
function isFacts(value: unknown): value is PerformanceFacts {
  const facts = value as PerformanceFacts | null;
  return Boolean(facts && typeof facts.cores === "number" && typeof facts.block_frames === "number");
}

/** Read the setting and its facts; the page calls this on mount. */
async function load(): Promise<void> {
  try {
    const next = await api.performanceFacts();
    if (!isFacts(next)) throw new Error("the backend answered without render facts");
    setFacts(next);
    setError(null);
  } catch (failure) {
    setError(String(failure));
  }
}

/** Persist one change and adopt the snapshot the backend answers. */
async function save(settings: PerformanceSettings): Promise<boolean> {
  setSaving(true);
  try {
    const next = await api.setPerformanceSettings(settings);
    if (!isFacts(next)) throw new Error("the backend answered without render facts");
    setFacts(next);
    setError(null);
    return true;
  } catch (failure) {
    setError(String(failure));
    return false;
  } finally {
    setSaving(false);
  }
}

function reset(): void {
  setFacts(null);
  setSaving(false);
  setError(null);
}

export const performanceStore = { facts, saving, error, load, save, reset };
