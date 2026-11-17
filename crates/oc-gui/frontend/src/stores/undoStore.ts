import { createSignal } from "solid-js";
import type { ProjectSnapshot } from "../types/project";
import type { HistoryBrief, HistoryStep, UndoHistoryEntry } from "../types/undo";
import * as api from "./api";

/** How long the footer keeps an undo's line. */
export const HISTORY_NOTICE_MS = 4000;

/** What the footer says after a history step. */
export type HistoryNotice =
  | { kind: "undid" | "redid"; brief: HistoryBrief }
  | { kind: "agent_editing" };

/** The pages of the history the History tab has loaded, newest first. */
export interface HistoryView {
  entries: UndoHistoryEntry[];
  total: number;
  applied: number;
  journaled: boolean;
}

const EMPTY_VIEW: HistoryView = { entries: [], total: 0, applied: 0, journaled: false };

export function createUndoStore(
  applySnapshot: (snapshot: ProjectSnapshot) => void,
  setError: (message: string) => void,
  context: () => string | number = () => 0,
) {
  const [undoState, setUndoState] = createSignal<{ canUndo: boolean; canRedo: boolean }>({
    canUndo: false,
    canRedo: false,
  });

  const [historyView, setHistoryView] = createSignal<HistoryView>(EMPTY_VIEW);
  const [notice, setNotice] = createSignal<HistoryNotice | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  /** Open History views; with none, an edit reads no history page. */
  let watchers = 0;

  let stateRequest = 0;
  let historyRequest = 0;
  const ownsContext = (owner: string | number) => owner === context();

  function showNotice(next: HistoryNotice) {
    setNotice(next);
    if (noticeTimer !== undefined) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => { setNotice(null); noticeTimer = undefined; }, HISTORY_NOTICE_MS);
  }

  /** Read the newest page, replacing what was loaded. */
  async function readHistory(request: number, owner: string | number) {
    if (request !== historyRequest || !ownsContext(owner)) return;
    try {
      const page = await api.historyPage();
      if (request === historyRequest && ownsContext(owner) && page && Array.isArray(page.entries)) {
        setHistoryView({ ...page });
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
      setUndoState({ canUndo: state.can_undo, canRedo: state.can_redo });
    } catch { /* ignore */ }
    if (watchers > 0 && request === stateRequest && ownsContext(owner)) await readHistory(history, owner);
  }

  async function refreshHistory() {
    await readHistory(++historyRequest, context());
  }

  /** Append the page older than the oldest loaded entry. */
  async function loadOlderHistory() {
    const current = historyView();
    const oldest = current.entries.at(-1);
    if (!oldest || oldest.index <= 1) return;
    const request = historyRequest;
    const owner = context();
    try {
      const page = await api.historyPage(oldest.index - 1);
      if (request !== historyRequest || !ownsContext(owner) || historyView() !== current) return;
      setHistoryView({ ...page, entries: [...current.entries, ...page.entries] });
    } catch (e) {
      if (ownsContext(owner)) setError(String(e));
    }
  }

  /** A History view is open; returns the call that closes it. */
  function watchHistory(): () => void {
    watchers += 1;
    void refreshHistory();
    let closed = false;
    return () => {
      if (closed) return;
      closed = true;
      watchers -= 1;
    };
  }

  async function step(
    run: () => Promise<HistoryStep<ProjectSnapshot>>,
    kind: "undid" | "redid" | "jumped",
  ) {
    const owner = context();
    try {
      const answer = await run();
      if (!ownsContext(owner)) return;
      if (answer.status === "refused") {
        if (answer.reason === "agent_editing") showNotice({ kind: "agent_editing" });
        return;
      }
      applySnapshot(answer.snapshot);
      if (kind !== "jumped" && answer.brief) showNotice({ kind, brief: answer.brief });
      await refreshUndoState();
    } catch (e) {
      if (!ownsContext(owner)) return;
      setError(String(e));
    }
  }

  const undo = () => step(api.historyUndo, "undid");
  const redo = () => step(api.historyRedo, "redid");
  /** Jump so `level` entries are applied; 0 is before the oldest kept one. */
  const restoreUndoLevel = (level: number) => step(() => api.historyJump(level), "jumped");

  function clearUndoHistory() {
    setUndoState({ canUndo: false, canRedo: false });
    setHistoryView(EMPTY_VIEW);
    void refreshUndoState();
  }

  return {
    undoState,
    historyView,
    historyNotice: notice,
    refreshUndoState,
    refreshHistory,
    loadOlderHistory,
    watchHistory,
    undo,
    redo,
    restoreUndoLevel,
    clearUndoHistory,
  };
}
