import { createSignal } from "solid-js";
import type { ProjectSnapshot } from "../types/project";
import * as api from "./api";
import { log } from "./log";

export function createUndoStore(
  applySnapshot: (snapshot: ProjectSnapshot) => void,
  setError: (message: string) => void,
  context: () => string | number = () => 0,
) {
  const [undoState, setUndoState] = createSignal<{
    canUndo: boolean; canRedo: boolean;
    undoLabel: string | null; redoLabel: string | null;
    undoLabels: string[]; redoLabels: string[];
  }>({ canUndo: false, canRedo: false, undoLabel: null, redoLabel: null, undoLabels: [], redoLabels: [] });

  /** Ordered backend history, oldest first; `applied` levels are undoable. */
  const [undoHistory, setUndoHistory] = createSignal<api.UndoHistoryEntry[]>([]);

  let stateRequest = 0;
  let historyRequest = 0;
  const ownsContext = (owner: string | number) => owner === context();

  async function readHistory(request: number, owner: string | number) {
    if (request !== historyRequest || !ownsContext(owner)) return;
    try {
      const history = await api.getUndoHistory();
      if (request === historyRequest && ownsContext(owner)) {
        setUndoHistory(Array.isArray(history) ? history : []);
      }
    } catch { /* ignore */ }
  }

  async function refreshUndoState() {
    const request = ++stateRequest;
    const history = ++historyRequest;
    const owner = context();
    try {
      const state = await api.getUndoState();
      if (request !== stateRequest || !ownsContext(owner)) return;
      setUndoState({
        canUndo: state.can_undo,
        canRedo: state.can_redo,
        undoLabel: state.undo_label,
        redoLabel: state.redo_label,
        undoLabels: state.undo_labels,
        redoLabels: state.redo_labels,
      });
    } catch { /* ignore */ }
    if (request === stateRequest && ownsContext(owner)) await readHistory(history, owner);
  }

  async function refreshUndoHistory() {
    await readHistory(++historyRequest, context());
  }

  async function undo() {
    const owner = context();
    try {
      const snap = await api.undoProject();
      if (!ownsContext(owner)) return;
      applySnapshot(snap);
      log.info("undo", `Undo: ${undoState().undoLabel ?? "unknown"}`);
      await refreshUndoState();
    } catch (e) {
      if (!ownsContext(owner)) return;
      const msg = String(e);
      if (!msg.includes("Nothing to undo")) setError(msg);
    }
  }

  async function redo() {
    const owner = context();
    try {
      const snap = await api.redoProject();
      if (!ownsContext(owner)) return;
      applySnapshot(snap);
      log.info("undo", `Redo: ${undoState().redoLabel ?? "unknown"}`);
      await refreshUndoState();
    } catch (e) {
      if (!ownsContext(owner)) return;
      const msg = String(e);
      if (!msg.includes("Nothing to redo")) setError(msg);
    }
  }

  /** Jump to an absolute history level; level 0 is the state before any edit. */
  async function restoreUndoLevel(level: number) {
    const owner = context();
    try {
      const snap = await api.restoreUndoLevel(level);
      if (!ownsContext(owner)) return;
      applySnapshot(snap);
      log.info("undo", `Restore level ${level}`);
      await refreshUndoState();
    } catch (e) {
      if (!ownsContext(owner)) return;
      setError(String(e));
    }
  }

  function clearUndoHistory() {
    setUndoState({ canUndo: false, canRedo: false, undoLabel: null, redoLabel: null, undoLabels: [], redoLabels: [] });
    setUndoHistory([]);
    void refreshUndoState();
  }

  return {
    undoState,
    undoHistory,
    refreshUndoState,
    refreshUndoHistory,
    undo,
    redo,
    restoreUndoLevel,
    clearUndoHistory,
  };
}
