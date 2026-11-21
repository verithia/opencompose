/**
 * The local content scan: one run of `local_content_scan` at a time, with the
 * progress it reports. The first-run guide starts it as soon as it opens, so
 * its scan page usually shows a finished result or a scan already under way;
 * the Folders page in Preferences reads and rescans the same result.
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
/** Bumped on every published result, so a late recount never replaces a newer one. */
let revision = 0;

function publish(next: LocalContentSummary | null) {
  revision += 1;
  setSummary(next);
}

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
  // A folder added or removed while the scan runs publishes its own result,
  // and the scan's answer still lists the folders it started with. It is then
  // replaced by a recount, which reads the current folder list and the plugin
  // list this scan has just published.
  let seen = revision;
  try {
    let result = await api.localContentScan(rescan);
    while (id === generation && seen !== revision) {
      seen = revision;
      result = await api.localContentSummary();
    }
    if (id === generation) publish(result ?? null);
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
  update: (next: LocalContentSummary) => publish(next),
  /**
   * Recount the searched folders without scanning plugins. A scan under way
   * already produces a fresh result, so nothing is read then.
   */
  load: async () => {
    if (running()) return;
    const id = generation;
    const seen = revision;
    try {
      const result = await api.localContentSummary();
      if (id === generation && seen === revision) publish(result ?? null);
    } catch (cause) {
      if (id === generation && seen === revision) setError(String(cause));
    }
  },
  /**
   * Search one more folder, show the recount at once, then scan again so
   * plugins in it are read. Throws a `FolderChangeError` when the backend
   * refuses the folder.
   */
  addFolder: async (kind: "plugins" | "sounds", path: string) => {
    publish(kind === "plugins" ? await api.pluginFolderAdd(path) : await api.soundFolderAdd(path));
    await run(false);
  },
  /** Forget the result; tests and a fresh guide use it. */
  reset: () => {
    generation += 1;
    publish(null);
    setProgress({ plugins: null, sounds: null });
    setRunning(false);
    setError(null);
  },
};
