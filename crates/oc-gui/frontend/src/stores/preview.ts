/**
 * The edit preview, as the panel and the editors see it.
 *
 * A preview is a mode, not a document: the backend holds the base and the
 * proposal, and the document the store shows is whichever side was last
 * installed. This module keeps the side, the highlight set and the busy flag,
 * and it is the only place the panel, the timeline and the piano roll read
 * them from. Highlighting is a CSS class driven by this store, never a second
 * rendering path: the editors keep painting the document the backend gave them.
 */
import { createSignal } from "solid-js";
import * as api from "./api";
import { i18n } from "./i18n";
import { store } from "./project";
import type { PreviewHighlight, PreviewState } from "./api";

const EMPTY: PreviewHighlight = {
  tracks: [],
  patterns: [],
  inline_patterns: [],
  presets: [],
  clips: [],
  effects: [],
  notes: [],
};

const [preview, setPreview] = createSignal<PreviewState | null>(null);
const [busy, setBusy] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

/** The changed subtree whose bars contain `bar`, or `null` when none does. */
function barInSpan(highlight: PreviewHighlight, pattern: string, bar: number) {
  return highlight.notes.some(
    (span) => span.pattern === pattern && bar >= span.start_bar && bar <= span.end_bar,
  );
}

export const previewStore = {
  /** The open preview, or `null` when the document is not being previewed. */
  preview,
  /** True while a command is in flight; the bar's buttons wait for it. */
  busy,
  /** Why the last preview command failed, for the panel to show once. */
  error,
  /** Whether a preview currently holds the document. */
  open: () => preview() !== null,
  /** The side on screen, or `null` when no preview is open. */
  side: () => preview()?.side ?? null,
  /** The changed subtrees of the open preview. */
  highlight: (): PreviewHighlight => preview()?.highlight ?? EMPTY,

  /** True when the proposal rewrites this track's subtree. */
  changesTrack(name: string): boolean {
    if (preview() === null) return false;
    return previewStore.highlight().tracks.includes(name);
  },

  /** True when the proposal rewrites this global pattern. */
  changesPattern(name: string): boolean {
    if (preview() === null) return false;
    const highlight = previewStore.highlight();
    return (
      highlight.patterns.includes(name)
      || highlight.inline_patterns.some((entry) => entry.endsWith(`/pattern:${name}`))
    );
  },

  /**
   * True when the clip at `index` of `track` changed, matched against the side
   * on screen so a clip the proposal moved highlights on both sides.
   */
  changesClip(track: string, index: number): boolean {
    if (preview() === null) return false;
    const side = previewStore.side();
    return previewStore
      .highlight()
      .clips.some((clip) => {
        if (clip.track !== track) return false;
        const at = side === "base" ? clip.base_index : clip.proposal_index;
        return at === index;
      });
  },

  /**
   * True when the note starting in `bar` of `pattern` is inside a changed span.
   * A pattern change the backend could not localise to bars (a hit, an effect)
   * marks the whole pattern instead.
   */
  changesNote(pattern: string, bar: number): boolean {
    if (preview() === null) return false;
    const highlight = previewStore.highlight();
    if (highlight.notes.some((span) => span.pattern === pattern)) {
      return barInSpan(highlight, pattern, bar);
    }
    return previewStore.changesPattern(pattern);
  },

  /** Read the backend's preview after a reload; `null` closes the local one. */
  async refresh() {
    try {
      const state = await api.previewState();
      setPreview(isPreviewState(state) ? state : null);
    } catch {
      setPreview(null);
    }
  },

  /**
   * Open a preview of `proposal` and show it. A refusal — a stale base, another
   * preview, a document the DSP refuses — leaves the session untouched and
   * surfaces the backend's explanation.
   */
  async begin(proposal: { path: string; dsl: string; expectedDsl: string }) {
    if (busy()) return false;
    setBusy(true);
    setError(null);
    try {
      const snapshot = await api.previewBegin(proposal.dsl, proposal.path, proposal.expectedDsl);
      store.applyPreviewSnapshot(snapshot);
      setPreview(snapshot.preview);
      return true;
    } catch (failure) {
      setError(errorText(failure));
      return false;
    } finally {
      setBusy(false);
    }
  },

  /** Show the base or the proposal; a swap to the side on screen does nothing. */
  async show(side: "base" | "proposal") {
    if (busy() || preview() === null || previewStore.side() === side) return;
    setBusy(true);
    setError(null);
    try {
      const snapshot = await api.previewShow(side);
      store.applyPreviewSnapshot(snapshot);
      setPreview(snapshot.preview);
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  },

  /** Commit the proposal as one undo entry and close the bar. */
  async accept() {
    if (busy() || preview() === null) return false;
    setBusy(true);
    setError(null);
    try {
      store.applyPreviewSnapshot(await api.previewAccept());
      setPreview(null);
      void store.refreshPreviewHistory();
      return true;
    } catch (failure) {
      setError(errorText(failure));
      return false;
    } finally {
      setBusy(false);
    }
  },

  /** Restore the base document and discard the proposal. */
  async decline() {
    if (busy() || preview() === null) return false;
    setBusy(true);
    setError(null);
    try {
      store.applyPreviewSnapshot(await api.previewCancel());
      setPreview(null);
      return true;
    } catch (failure) {
      setError(errorText(failure));
      return false;
    } finally {
      setBusy(false);
    }
  },

  /** A fresh window with nothing previewed. */
  reset() {
    setPreview(null);
    setBusy(false);
    setError(null);
  },
};

/** Only a real preview state opens the bar; anything else closes it. */
function isPreviewState(value: unknown): value is PreviewState {
  if (!value || typeof value !== "object") return false;
  const candidate = value as PreviewState;
  return (
    (candidate.side === "base" || candidate.side === "proposal")
    && typeof candidate.target_path === "string"
    && !!candidate.highlight
    && Array.isArray(candidate.highlight.tracks)
  );
}

function errorText(failure: unknown): string {
  const message = failure instanceof Error ? failure.message : String(failure);
  return message === "Stop recording before previewing an edit."
    ? i18n.t("ai.preview.recording")
    : message;
}
