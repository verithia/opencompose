import { listen } from "@tauri-apps/api/event";
import type { ProjectSnapshot } from "../types/project";
import { store } from "./project";
import { log } from "./log";

export interface ExternalProjectChange {
  snapshot: ProjectSnapshot;
  replaces_document: boolean;
}

/** External tool edits already own their backend history entry. */
export function mountExternalProjectChanges(startupReady: Promise<unknown>): () => void {
  let alive = true;
  let ready = false;
  let pending: ExternalProjectChange | undefined;
  let unlisten: (() => void) | undefined;
  void listen<ExternalProjectChange>("agent_project_changed", ({ payload }) => {
    if (!alive) return;
    if (ready) store.applyExternalSnapshot(payload.snapshot, payload.replaces_document);
    else if (!pending || payload.snapshot.revision > pending.snapshot.revision) {
      pending = {
        ...payload,
        replaces_document: payload.replaces_document || !!pending?.replaces_document
          && pending.snapshot.file_path === payload.snapshot.file_path,
      };
    }
  }).then(release => {
    if (alive) unlisten = release;
    else release();
  }).catch(error => { if (alive) log.warn("project", String(error)); });
  void startupReady.then(() => {
    if (!alive) return;
    ready = true;
    if (pending) store.applyExternalSnapshot(pending.snapshot, pending.replaces_document);
    pending = undefined;
  }).catch(error => { if (alive) log.warn("project", String(error)); });
  return () => { alive = false; pending = undefined; unlisten?.(); };
}
