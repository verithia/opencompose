/** OS-open notifications wake a durable backend queue; document guards run here. */
import { createEffect, createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { store } from "./project";

interface ProjectOpenRequest { id: number; path: string }
function request(value: unknown): ProjectOpenRequest | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as Partial<ProjectOpenRequest>;
  return typeof candidate.id === "number" && Number.isSafeInteger(candidate.id) && candidate.id > 0
    && typeof candidate.path === "string" && /\.ocp$/i.test(candidate.path)
    && (/^\//.test(candidate.path) || /^[a-z]:[\\/]/i.test(candidate.path) || /^\\\\/.test(candidate.path))
    ? candidate as ProjectOpenRequest : null;
}

/** Register before startup completes; consume only afterward, with acknowledgment. */
export function mountOsProjectOpen(startupReady: Promise<unknown>, blocked: () => boolean = () => false): () => void {
  const [ready, setReady] = createSignal(false);
  let alive = true, running = false, rerun = false;
  let unlisten: (() => void) | undefined;
  const drain = async () => {
    rerun = true;
    if (running || !ready() || blocked()) return;
    running = true;
    try {
      do {
        rerun = false;
        while (alive && !blocked()) {
          const pending = request(await invoke<unknown>("peek_project_open_request"));
          if (!pending || !alive || blocked()) break;
          try {
            // Duplicate delivery of the current document is only a focus request.
            if (pending.path !== store.filePath() && await store.confirmDiscardChanges() && alive) {
              await store.openFile(pending.path);
            }
          } finally {
            if (alive) await invoke("acknowledge_project_open_request", { id: pending.id });
          }
        }
      } while (alive && rerun && !blocked());
    } catch (error) {
      if (alive) store.setError(String(error));
    } finally { running = false; }
  };
  void listen("project_open_requested", () => { void drain(); }).then(dispose => {
    if (!alive) dispose();
    else { unlisten = dispose; void drain(); }
  }).catch(error => { if (alive) store.setError(String(error)); });
  void startupReady.then(() => { if (alive) setReady(true); }).catch(error => {
    if (alive) store.setError(String(error));
  });
  createEffect(() => { if (ready() && !blocked()) void drain(); });
  return () => { alive = false; unlisten?.(); };
}
