/**
 * The transcript pager: which page of the saved conversation is on screen,
 * which part of it is rendered, and the next older page. The panel keeps the
 * rows; this owns the window into them and refuses a page that no longer
 * matches the view it was asked for.
 */
import { createSignal, type Accessor } from "solid-js";
import {
  hiddenTranscriptCount,
  restoredTranscript,
  visibleTranscript,
  TRANSCRIPT_RENDER_LIMIT,
  type ChatMessage,
} from "./transcript";
import { sessionPageRead, type LoadedPage } from "./sessions";
import { errorMessage } from "./errors";

export interface TranscriptPagerDeps {
  messages: Accessor<ChatMessage[]>;
  setMessages: (updater: (previous: ChatMessage[]) => ChatMessage[]) => void;
  openId: Accessor<string | null>;
  epoch: Accessor<number>;
  paintRequest: Accessor<number>;
  locked: Accessor<boolean>;
  originalView: Accessor<boolean>;
  transcript: Accessor<HTMLDivElement | undefined>;
  /** Repaint the conversation after its saved view changed shape. */
  repaint: (id: string) => Promise<boolean>;
  readPage: (id: string, before: number, revision: string) => Promise<unknown>;
  onBegin?: () => void;
  onError: (message: string) => void;
  disposed?: () => boolean;
}

export function createTranscriptPager(deps: TranscriptPagerDeps) {
  const [pageStart, setPageStart] = createSignal(0);
  const [pageRevision, setPageRevision] = createSignal("");
  const [olderTokens, setOlderTokens] = createSignal(0);
  const [renderLimit, setRenderLimit] = createSignal(TRANSCRIPT_RENDER_LIMIT);
  let olderPageLoading = false;

  const reset = (): void => {
    setPageStart(0);
    setPageRevision("");
    setOlderTokens(0);
    setRenderLimit(TRANSCRIPT_RENDER_LIMIT);
  };

  /** A freshly painted conversation starts on its newest page. */
  const applyPage = (loaded: LoadedPage): void => {
    setPageStart(loaded.start);
    setPageRevision(loaded.revision);
    setOlderTokens(loaded.older_tokens);
    setRenderLimit(TRANSCRIPT_RENDER_LIMIT);
  };

  /** The original view carries the whole history, so no older page remains. */
  const resetForOriginal = (): void => {
    setPageStart(0);
    setPageRevision("");
    setOlderTokens(0);
  };

  const loadOlderPage = async (id: string): Promise<void> => {
    if (olderPageLoading || deps.originalView() || deps.locked() || pageStart() <= 0) return;
    const epoch = deps.epoch();
    // A later paint supersedes this page, and the page must still match it.
    const request = deps.paintRequest();
    const before = pageStart();
    const revision = pageRevision();
    olderPageLoading = true;
    deps.onBegin?.();
    try {
      const page = sessionPageRead(await deps.readPage(id, before, revision));
      // The conversation may lock while the page is in flight; repainting a
      // stale view would drop a live turn's rows.
      if (!page || deps.disposed?.() || deps.epoch() !== epoch || request !== deps.paintRequest()
          || deps.openId() !== id || deps.locked()) return;
      if (page.status === "stale") {
        await deps.repaint(id);
        return;
      }
      const older = restoredTranscript(page.messages, page.start);
      const transcript = deps.transcript();
      const oldTop = transcript?.scrollTop ?? 0;
      const oldHeight = transcript?.scrollHeight ?? 0;
      // Keep the existing row objects; only the page above them is new.
      deps.setMessages(previous => [...older, ...previous]);
      setPageStart(page.start);
      setPageRevision(page.revision);
      setOlderTokens(page.older_tokens);
      // The prepended rows must be visible once the page arrives.
      setRenderLimit(limit => limit + older.length);
      queueMicrotask(() => {
        if (!transcript || deps.disposed?.()) return;
        const added = transcript.scrollHeight - oldHeight;
        transcript.scrollTop = oldTop + added;
      });
    } catch (error) {
      deps.onError(errorMessage(error));
    } finally {
      olderPageLoading = false;
    }
  };

  return {
    pageStart,
    pageRevision,
    olderTokens,
    renderLimit,
    hiddenMessages: () => hiddenTranscriptCount(deps.messages().length, renderLimit()),
    visibleMessages: () => visibleTranscript(deps.messages(), renderLimit()),
    showMoreRows(): void {
      setRenderLimit(limit => limit + TRANSCRIPT_RENDER_LIMIT);
    },
    reset,
    applyPage,
    resetForOriginal,
    loadOlderPage,
  };
}
