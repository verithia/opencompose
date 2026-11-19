/**
 * The first-run scan: one run of `local_content_scan` at a time, with the
 * progress it reports. The guide starts it as soon as it opens, so the scan
 * page usually shows a finished result or a scan already under way.
 */
import { createSignal } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import * as api from "./api";
import type { LocalContentSummary, LocalScanProgress } from "./api";

export interface ScanProgressState {
  plugins: LocalScanProgress | null;
  sounds: LocalScanProgress | null;
}

const [summary, setSummary] = createSignal<LocalContentSummary | null>(null);
const [progress, setProgress] = createSignal<ScanProgressState>({ plugins: null, sounds: null });
const [running, setRunning] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

/** Only the latest run may write the signals; an older one finishing late is dropped. */
let generation = 0;

async function run(rescan: boolean): Promise<void> {
  const id = ++generation;
  setRunning(true);
  setError(null);
  setProgress({ plugins: null, sounds: null });
  let stop: (() => void) | undefined;
  try {
    stop = await listen<LocalScanProgress>(api.LOCAL_SCAN_PROGRESS_EVENT, (event) => {
      if (id !== generation) return;
      const payload = event.payload;
      setProgress((previous) => ({ ...previous, [payload.phase]: payload }));
    });
  } catch {
    // Without events the page still shows an indeterminate bar and the result.
  }
  try {
    const result = await api.localContentScan(rescan);
    if (id === generation) setSummary(result ?? null);
  } catch (cause) {
    if (id === generation) setError(String(cause));
  } finally {
    stop?.();
    if (id === generation) setRunning(false);
  }
}

export const localScan = {
  summary,
  progress,
  running,
  error,
  /** Start a scan; `rescan` inspects every plugin again. */
  start: (rescan = false) => run(rescan),
  /** Replace the result after a folder change, without a new scan. */
  update: (next: LocalContentSummary) => setSummary(next),
  /**
   * Search one more folder, show the recount at once, then scan again so
   * plugins in it are read. Throws when the backend refuses the folder.
   */
  addFolder: async (kind: "plugins" | "sounds", path: string) => {
    setSummary(kind === "plugins" ? await api.pluginFolderAdd(path) : await api.soundFolderAdd(path));
    await run(false);
  },
  /** Forget the result; tests and a fresh guide use it. */
  reset: () => {
    generation += 1;
    setSummary(null);
    setProgress({ plugins: null, sounds: null });
    setRunning(false);
    setError(null);
  },
};
