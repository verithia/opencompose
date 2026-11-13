import { AiComposer } from "./AiComposer";
import { ProposalReviewBar } from "./ProposalReviewBar";
import { followTranscript } from "../ai/transcriptScroll";
import { AiMarkdown } from "./AiMarkdown";
import { ToolIcon } from "./ToolIcon";
import { AiToolContent } from "./AiToolContent";
import { AiToolArguments, AiToolResult, autoOpenResult } from "./AiToolResult";
import { AiStepGroup } from "./AiStepGroup";
import { LazyDetails } from "./LazyDetails";
import { openContextMenu } from "./ContextMenu";
import { For, Index, Show, createEffect, createSignal, createMemo, on, onMount, onCleanup } from "solid-js";
import { aiApi, openSavedAuditionRender, previewSavedAudition } from "../stores/api";
import { store } from "../stores/project";
import { previewStore } from "../stores/preview";
import { log } from "../stores/log";
import type { ApprovalRequest, TurnStop, WireMessage } from "../ai/wire";
import type { TurnEnvelope, TurnOutcome } from "../ai/api";
import { approvalEffect, approvalTarget } from "../ai/approval";
import { allowTool, allowedTools } from "../ai/approvalMemory";
import {
  DEFAULT_PERMISSION,
  type PermissionMode,
} from "../ai/permission";
import { SessionSwitcher, type SessionMenu } from "./SessionSwitcher";
import { buildSystemPrompt } from "../ai/system-prompt";
import { contextMeter, contextUsed, estimateFromBase, resetContextCalibration, setCompactAt, setRequestOverhead, textTokens } from "../stores/aiContext";
import { beginUsageHydration, cancelUsageHydration, hydrateSessionUsage, usageHydrationNeedsReconcile, noteSentEstimate, resetSentEstimate, selectStatsSession, startAiStatsSession } from "../stores/aiStats";
import { readProjectInstructions } from "../ai/projectInstructions";
import { aiConfig, DEFAULT_THINKING_LEVEL, getActiveProvider, hasProvider, loadConfig, hasAnyKeyDraft, hasUnsavedConfig, wireOf } from "../ai/config";
import { modelFor } from "../ai/catalog";
import { activeModelGroup, activeModelKnown, applyModelChoice, listModelChoices, modelIdentity, modelKey, modelLevels } from "../ai/modelPicker";
import { InlineAudioPreview } from "./InlineAudioPreview";
import { MEDIA_EXTENSIONS, attachMedia, audioAccepted, acceptedModalities, formatBytes, formatDuration, auditionLabel, legacyAuditionText, imageAccepted, type MediaAttachment, type MediaNotice } from "../ai/media";
import { copyToClipboard, downloadText, exportFileName, exportJson, exportMarkdown } from "../ai/export";
import { errorKind, errorMessage } from "../ai/errors";
import {
  activeTurn,
  archiveNotice,
  compactionNotice,
  directoryName,
  loadedSession,
  sessionArchiveEviction,
  sessionCompaction,
  sessionList,
  workspaceScope,
  type ActiveTurn,
  type SessionMeta,
} from "../ai/sessions";
import {
  canContinue,
  canRetry,
  elapsedLabel,
  restoredTranscript,
  savedWireIndex,
  stopStalled,
  userTurnOf,
  hiddenTranscriptCount,
  stepTranscript,
  visibleTranscript,
  TRANSCRIPT_RENDER_LIMIT,
  type ChatMessage,
  type DiffResult,
  type TranscriptMessage,
  type TranscriptStep,
  type StoppedTurn,
} from "../ai/transcript";
import { filterReferences, projectReferences, referenceQuery, withoutReferenceQuery, type ProjectReference } from "../ai/references";
import { i18n, type I18nKey } from "../stores/i18n";
import { aiLimits, effectiveLimits, type ModelLimitsView } from "../stores/aiLimits";
import { aiSession, type QueuedMessage } from "../stores/aiSession";
import { workspaceStore } from "../stores/workspace";
import { aiWorkspace } from "../stores/aiWorkspace";
import { preferencesUi } from "../stores/preferencesUi";
import { useTooltip } from "./Tooltip";
import { Dropdown } from "./Dropdown";
import { AiStats, type ContextBreakdown } from "./AiStats";
import { activitySummary, appendThinking, preparingActivity, preparingToolFromText, readableName, thinkingPreview } from "../ai/activity";
import { AiThinking, thinkingState } from "./AiThinking";

/**
 * AI Assistant Panel — chat interface for LLM-powered project editing.
 *
 * The backend selects the saved wire protocol; capabilities only gate which
 * tools and modalities are offered.
 */

type ExportFormat = "markdown" | "json";

/** The workspace and conversation a queued message was taken from. */
interface FlushOrigin {
  /** Bumped by every workspace change, so a stale value names another panel. */
  scope: number;
  conversation: string | null;
}

/** The two documents the export menu offers, in the order it shows them. */
const EXPORT_FORMATS: { format: ExportFormat; label: I18nKey }[] = [
  { format: "markdown", label: "ai.export.markdown" },
  { format: "json", label: "ai.export.json" },
];

/** How often a window that inherited a running turn asks whether it finished. */
const TURN_POLL_MS = 400;
/** How long a stopped inherited turn may take to close before the panel repaints anyway. */
const TURN_SETTLE_MS = 10_000;

export function AiPanel(props: { onClose?: () => void }) {
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  let transcriptElement: HTMLDivElement | undefined;
  let transcriptFollow: ReturnType<typeof followTranscript> | undefined;
  onMount(() => {
    if (!transcriptElement) return;
    transcriptFollow = followTranscript(transcriptElement);
    // The "earlier" affordance appears only when the reader reaches the top,
    // so a long conversation is not permanently capped with a button.
    const onScroll = () => setAtTranscriptTop(transcriptElement!.scrollTop <= 16);
    transcriptElement.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
    onCleanup(() => transcriptElement?.removeEventListener("scroll", onScroll));
  });
  onCleanup(() => transcriptFollow?.dispose());
  /** The draft lives outside the component: a tab switch unmounts the panel. */
  const input = aiSession.draft;
  const setInput = aiSession.setDraft;
  const targetPath = aiWorkspace.targetPath;
  const [loading, setLoading] = createSignal(false);
  /** The call a turn is paused on, and the turn that asked; `target` is
   * resolved once, so the card and the decline the model reads name the same
   * object. */
  const [approval, setApproval] = createSignal<(ApprovalRequest & { target: string | null; requestId: string }) | null>(null);
  /** When the turn in flight started, so the pending row can count seconds. */
  const [turnStartedAt, setTurnStartedAt] = createSignal<number | null>(null);
  /** When Stop was pressed; the pending row says so until the turn settles. */
  const [stopRequestedAt, setStopRequestedAt] = createSignal<number | null>(null);
  /**
   * The turn the backend still runs after this window was reloaded, named by
   * the backend itself. The pending row needs a loop this window no longer
   * has, so this turn is reported instead of imitated.
   */
  const [reattached, setReattached] = createSignal<{ requestId: string; session: string } | null>(null);
  /** A marker the backend does not back: nothing runs, and the panel says so. */
  const [reloadNotice, setReloadNotice] = createSignal<"lost" | null>(null);
  /** The call the loop is running right now, named in the pending row. */
  const [currentTool, setCurrentTool] = createSignal<string | null>(null);
  const [clock, setClock] = createSignal(Date.now());
  /** The settings button opens the one Preferences surface on its AI page. */
  const openSettings = (
    focus: "connection" | "toolsets" | "auth" = "connection",
    from?: HTMLElement | null,
  ) => preferencesUi.openOn("ai", focus, from ?? (document.activeElement as HTMLElement | null));
  const notify = aiWorkspace.notify;

  const [authError, setAuthError] = createSignal<string | null>(null);
  const [savingConfig, setSavingConfig] = createSignal(false);
  const [applying, setApplying] = createSignal(false);
  const [partial, setPartial] = createSignal("");
  const [reasoning, setReasoning] = createSignal("");
  const [preparingTool, setPreparingTool] = createSignal<string | null>(null);
  const toolPreparation = () => preparingTool() || preparingToolFromText(partial());
  const catalog = aiWorkspace.catalog;
  const toolsets = aiWorkspace.toolsets;
  const selectedSets = aiWorkspace.selectedSets;
  const attachments = aiSession.attachments;
  const setAttachments = aiSession.setAttachments;
  const [mediaError, setMediaError] = createSignal<string | null>(null);
  const [mediaNotice, setMediaNotice] = createSignal<string | null>(null);
  /** This workspace's saved conversations, newest first, and the one on screen. */
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [sessionsOpen, setSessionsOpen] = createSignal(false);
  const [openId, setOpenId] = createSignal<string | null>(aiSession.open());
  /** What this conversation may do to the DSL without asking. A session with no
   * stored setting reads as the default, and a fresh conversation starts there. */
  const permission = aiWorkspace.permission;
  /** The destructive tools this conversation allowed for good, so one answer on
   * the card stops the next call of that tool from asking again. */
  const [allowed, setAllowed] = createSignal<readonly string[]>([]);
  const [renaming, setRenaming] = createSignal<{ id: string; title: string } | null>(null);
  /** The row menu opened from a right click on a conversation. */
  /** A long conversation renders its newest slice; the wire still carries all. */
  const [renderLimit, setRenderLimit] = createSignal(TRANSCRIPT_RENDER_LIMIT);
  /** The open conversation was compacted, so its older turns are behind a chip. */
  const [compactedSession, setCompactedSession] = createSignal(false);
  /** Whether this conversation already carries a chosen title. */
  const [sessionNamed, setSessionNamed] = createSignal(false);
  /** True while the transcript is scrolled to its top. */
  const [atTranscriptTop, setAtTranscriptTop] = createSignal(false);
  const hiddenMessages = createMemo(() => hiddenTranscriptCount(messages().length, renderLimit()));
  const visibleMessages = createMemo(() => visibleTranscript(messages(), renderLimit()));
  const liveReasoningMessage: ChatMessage = {
    role: "assistant",
    content: "",
    get reasoning() { return reasoning(); },
    liveReasoning: true,
  };
  /** Tool work and exposed reasoning share a step between assistant messages. */
  const transcriptEntries = createMemo(() => stepTranscript([
    ...visibleMessages(),
    ...(loading() && reasoning().trim() ? [liveReasoningMessage] : []),
  ], loading()));
  /** What the running step says when folded: the tool, else the latest thinking. */
  const liveActivity = createMemo(() => {
    if (toolPreparation()) return preparingActivity(toolPreparation()!);
    const tool = currentTool();
    if (tool) return readableName(tool);
    return thinkingPreview(reasoning() || partial()) || null;
  });
  /** 0-based row the `/` palette highlights. */
  const [commandIndex, setCommandIndex] = createSignal(0);
  /** The project path one request reads, chosen from the `@` palette. */
  const [scope, setScope] = createSignal<string | null>(null);
  const [referenceIndex, setReferenceIndex] = createSignal(0);
  /** The row menu opened from a right click on a message. */
  /** True while the transcript shows history a compaction replaced. */
  const [originalView, setOriginalView] = createSignal(false);
  const [sessionMenu, setSessionMenu] = createSignal<SessionMenu | null>(null);

  // SessionSwitcher owns keyboard dismissal; an outside press closes the row menu.
  createEffect(() => {
    if (!sessionMenu()) return;
    const dismiss = (event: Event) => {
      if (event instanceof PointerEvent) {
        const target = event.target as Element | null;
        if (target?.closest?.(".ai-session-menu")) return;
      }
      setSessionMenu(null);
    };
    window.addEventListener("pointerdown", dismiss, true);
    onCleanup(() => {
      window.removeEventListener("pointerdown", dismiss, true);
    });
  });
  const [deleting, setDeleting] = createSignal<string | null>(null);
  const [sessionError, setSessionError] = createSignal<string | null>(null);
  const [sessionBusy, setSessionBusy] = createSignal(false);
  /** The export menu above the transcript, and what the last action said. */
  const [exportOpen, setExportOpen] = createSignal(false);
  const [exportBusy, setExportBusy] = createSignal(false);
  const [exportNote, setExportNote] = createSignal<string | null>(null);
  /** The queued row being edited in place, by identity; `null` when none is. */
  const [queuedEditing, setQueuedEditing] = createSignal<QueuedMessage | null>(null);
  /** The text that row holds until Enter or blur commits it. */
  const [queuedDraft, setQueuedDraft] = createSignal("");
  const seenNotices = new Set<string>();
  /** Which turn owns the panel; a token that stops owning it never acts again. */
  let turnToken = 0;
  /** Tokens a person stopped; Stop spends one token and no later turn revives it. */
  const stoppedTokens = new Set<number>();
  /**
   * A flush holds the message it took until its turn owns the composer. The
   * window between the take and `loading` becoming true is otherwise open to a
   * second flush, which would take the next row and supersede the first turn.
   */
  let flushing = false;
  /** A steer asked for the cancel: the turn's own flush waits for the send. */
  let steering = false;
  /** The id of the turn this panel runs; `ai_cancel` and the events name it. */
  let requestId: string | null = null;
  const cancel = async () => {
    stoppedTokens.add(turnToken);
    // The pending row says the stop is on its way instead of leaving the turn
    // looking healthy; a second press re-sends the cancel.
    setStopRequestedAt((previous) => previous ?? Date.now());
    // The backend drops the question when its turn stops.
    setApproval(null);
    // This window owned the turn, so the marker it kept is spent: a window that
    // comes back must not ask about a turn this one stopped.
    if (requestId) {
      aiSession.setTurn(null);
      await aiApi.cancel(requestId).catch(() => {});
    }
  };
  /** Shows a question a turn asked; the card answers it by call id. */
  const showQuestion = (turn: string, request: ApprovalRequest) => {
    setApproval({
      ...request,
      requestId: turn,
      // The path the call names is the target when the registry has no better
      // name for it, so a subtree replacement says which subtree it touches.
      target: approvalTarget(request.tool, request.args, store.project) ?? request.path ?? null,
    });
  };
  /** The call's arguments as the card shows them: bounded, one line. */
  const argumentLine = (args: Record<string, unknown>): string => {
    const text = JSON.stringify(args) ?? "{}";
    return text.length > 300 ? `${text.slice(0, 300)}…` : text;
  };
  /**
   * Remembers a tool for the rest of the conversation and returns its new list.
   * A conversation with no id yet cannot keep the choice: its session is created
   * by the turn that is running, and this card cannot name it.
   */
  const rememberAllowed = (tool: string): readonly string[] => {
    const id = openId();
    const next = id ? allowTool(id, tool) : allowed();
    setAllowed(next);
    return next;
  };
  /** Sends the card's answer; the backend's turn goes on with it. */
  const answerApproval = (approved: boolean, always = false) => {
    const request = approval();
    setApproval(null);
    if (!request) return;
    if (approved && always) rememberAllowed(request.tool);
    void aiApi.answerTurn(request.requestId, request.call_id, {
      approved,
      always: approved && always,
      ...(request.target ? { target: request.target } : {}),
    }).catch((error) => log.warn("ai", `answer not delivered: ${errorMessage(error)}`));
  };
  /** The backend's autosave announcement; set while the panel is mounted. */
  let stopSessionEvents: (() => void) | null = null;
  /** The running turn's events and the document it changes. */
  let stopTurnEvents: (() => void) | null = null;
  let stopProjectEvents: (() => void) | null = null;
  /** The panel is gone: a queued message waits instead of sending from here. */
  let disposed = false;
  let pendingUsageRead: ReturnType<typeof beginUsageHydration> | null = null;
  onCleanup(() => {
    disposed = true;
    if (pendingUsageRead) cancelUsageHydration(pendingUsageRead);
    pendingUsageRead = null;
    aiWorkspace.setPanelBusy(false);
    aiWorkspace.setContextEstimate(0);
    void cancel();
    stopFollowing();
    stopSessionEvents?.();
    stopTurnEvents?.();
    stopProjectEvents?.();
    // The draft may still be inside its debounce window.
    aiSession.flush();
  });
  const currentModel = createMemo(() => modelFor(catalog(), wireOf(getActiveProvider()), getActiveProvider().model));
  const thinkingLevels = createMemo(() =>
    modelLevels(catalog(), getActiveProvider().id, getActiveProvider().model));
  /** The badge the selected row carries, on the trigger as well as in the list. */
  const modelBadge = () => currentModel()?.status === "deprecated" ? i18n.t("ai.model.deprecated") : undefined;
  /** The dialog and this header share one list and one update path. */
  const modelChoices = createMemo(() => listModelChoices(catalog()));
  /** The transcript and the project text, not the draft: this half is counted
   * once per change instead of once per keystroke. */
  const estimateBase = createMemo(() =>
    `${messages().map(message => message.content).join("\n")}\n${store.dslSource()}`);
  const estimateBaseTokens = createMemo(() => textTokens(estimateBase()));
  /** The estimate for a draft: the floor the composer shows, and the send guard. */
  const estimateFor = (draft: string) => estimateFromBase(estimateBaseTokens(), draft);
  /** The same estimate split into the parts a compaction would remove or keep. */
  const contextBreakdown = createMemo<ContextBreakdown>(() => {
    const ratio = contextMeter.ratio();
    const history = messages().map(message => message.content).join("\n");
    return {
      system: Math.round((contextMeter.systemChars() / 4) * ratio),
      tools: Math.round((contextMeter.toolChars() / 4) * ratio),
      history: Math.round(textTokens(history) * ratio),
      project: Math.round(textTokens(store.dslSource()) * ratio),
      draft: Math.round(textTokens(input()) * ratio),
      attachments: attachments().filter(item => !item.missing).length,
      attachmentBytes: attachments()
        .filter(item => !item.missing)
        .reduce((sum, item) => sum + item.bytes, 0),
    };
  });
  // The provider's last reported prompt is the floor: the heuristic can only
  // raise it, never pretend the conversation is smaller than the model saw.
  const contextEstimate = createMemo(() => contextUsed(estimateFor(input())));
  createEffect(() => aiWorkspace.setPanelBusy(loading()));
  createEffect(() => aiWorkspace.setContextEstimate(contextEstimate()));
  /** The merged limits of the model on screen: context and input modalities. */
  const [activeLimits, setActiveLimits] = createSignal<ModelLimitsView | null>(null);
  let limitsRequest = 0;
  // The composer's context figure and the pre-send guard read the merged limit
  // of the model the panel shows, so keep it current without opening settings.
  createEffect(() => {
    const provider = getActiveProvider();
    const request = ++limitsRequest;
    void aiLimits.loadModelLimits(provider.id, provider.model)
      .then(view => {
        // A reply for a selection that is no longer on screen describes another
        // model's window: publishing it would budget the next request wrongly.
        if (request !== limitsRequest) return;
        setActiveLimits(view);
        aiLimits.setActiveContext(aiLimits.effectiveLimits(view)?.context ?? null);
      })
      .catch(() => {
        if (request !== limitsRequest) return;
        setActiveLimits(null);
        aiLimits.setActiveContext(null);
      });
  });
  // Another model is another tokenizer: the calibration and the reported prompt
  // learned from the old one would misjudge the next request.
  createEffect(on(
    () => `${getActiveProvider().id}:${getActiveProvider().model}`,
    () => { resetContextCalibration(); resetSentEstimate(); },
    { defer: true },
  ));
  /**
   * Another conversation starts from nothing: the context meter has no reported
   * prompt for it, and the stats line describes the old one.
   */
  selectStatsSession(openId());
  onCleanup(startAiStatsSession());
  let shownConversation = aiSession.open();
  createEffect(() => {
    const id = openId();
    if (id === shownConversation) return;
    shownConversation = id;
    resetContextCalibration();
    selectStatsSession(id);
  });
  const attachHint = () => i18n.t("ai.media.attachHint", {
    formats: MEDIA_EXTENSIONS.join(", "),
  });
  const canTakeAudio = createMemo(() => audioAccepted(currentModel(), getActiveProvider()));
  const canTakeImage = createMemo(() => imageAccepted(effectiveLimits(activeLimits())));
  const usesLocalReference = (attachment: MediaAttachment) =>
    attachment.kind === "audio" && (attachment.local_reference || !canTakeAudio());
  const hasImage = createMemo(() => attachments().some(attachment => attachment.kind === "image" && !attachment.missing));
  /**
   * A saved attachment whose file is gone is not sent, so it never blocks the
   * turn; a live image on a model without image input refuses before the request.
   */
  const imageRefusal = (media: MediaAttachment[]): string | null => {
    const image = media.find(attachment => attachment.kind === "image" && !attachment.missing);
    if (!image || canTakeImage()) return null;
    return i18n.t("ai.media.imageUnsupported", {
      model: currentModel()?.name ?? getActiveProvider().model,
      modalities: acceptedModalities(effectiveLimits(activeLimits())?.input),
    });
  };
  const attachFile = async () => {
    if (loading()) return;
    setMediaError(null);
    setMediaNotice(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: false,
        filters: [{ name: i18n.t("ai.media.mediaFiles"), extensions: MEDIA_EXTENSIONS }],
      });
      if (typeof picked !== "string") return;
      const attachment = await attachMedia(picked);
      setAttachments(previous => [...previous, attachment]);
    } catch (e) {
      setMediaError(i18n.t("ai.media.attachFailed", { error: errorMessage(e) }));
    }
  };
  const removeAttachment = (path: string) => {
    setAttachments(previous => previous.filter(attachment => attachment.path !== path));
    setMediaNotice(null);
  };
  /** A file re-hashed on send, or one found gone: refresh the row and say so. */
  const applyMediaNotices = (notices?: MediaNotice[]) => {
    if (!notices?.length) return;
    for (const notice of notices) {
      const key = `${notice.path}:${notice.sha256}:${notice.missing ? "missing" : "changed"}`;
      if (seenNotices.has(key)) continue;
      seenNotices.add(key);
      if (notice.missing) {
        setMessages(previous => previous.map(message => message.media?.some(attachment => attachment.path === notice.path)
          ? { ...message, media: message.media.map(attachment => attachment.path === notice.path ? { ...attachment, missing: true } : attachment) }
          : message));
        setAttachments(previous => previous.map(attachment => attachment.path === notice.path ? { ...attachment, missing: true } : attachment));
        setMediaError(i18n.t("ai.media.missing", { name: notice.name }));
        continue;
      }
      setMessages(previous => previous.map(message => message.media?.some(attachment => attachment.path === notice.path)
        ? { ...message, media: message.media.map(attachment => attachment.path === notice.path ? { ...attachment, sha256: notice.sha256 } : attachment) }
        : message));
      setMediaNotice(i18n.t("ai.media.changed", { name: notice.name }));
    }
  };
  /** The open conversation's title, or the fresh one the composer would start. */
  const openTitle = createMemo(() =>
    sessions().find(session => session.id === openId())?.title ?? i18n.t("ai.sessions.new"));
  /** The directory the list is scoped to: the chosen workspace, else the open
   * project's, else the one the saved conversations name. */
  const workspacePath = createMemo(() =>
    workspaceStore.scopePath() ?? workspaceScope(sessions(), store.filePath()));
  /** The same scope, without asking the saved conversations: it is what the
   * switcher watches, so a workspace switch reloads the list. */
  const projectDir = createMemo(() =>
    workspaceStore.scopePath() ?? workspaceScope([], store.filePath()));
  /**
   * The cache key for the last conversation: the workspace and the project
   * file. Two songs in one folder each keep the chat they were last using.
   */
  const sessionScope = createMemo(() =>
    `${projectDir() ?? ""}\n${store.filePath() ?? ""}`);
  /** The header's primary identity: the directory's own name. */
  const workspaceName = createMemo(() =>
    directoryName(workspacePath()) ?? i18n.t("ai.sessions.detached"));
  const modelLabel = createMemo(() =>
    modelIdentity(getActiveProvider().id, getActiveProvider().model));
  let scopeEpoch = 0;
  let listRequest = 0;
  let paintRequest = 0;
  const refreshSessions = async (): Promise<boolean> => {
    const epoch = scopeEpoch;
    const request = ++listRequest;
    try {
      const next = sessionList(await aiApi.listSessions());
      if (disposed || epoch !== scopeEpoch || request !== listRequest) return false;
      setSessions(next);
      return true;
    } catch (e) {
      if (!disposed && epoch === scopeEpoch && request === listRequest) setSessionError(errorMessage(e));
      return false;
    }
  };
  /**
   * Sets the open conversation's permission and stores it. A conversation not
   * started yet keeps the choice for the session its first turn creates.
   */
  const choosePermission = (mode: PermissionMode): void => {
    setSessionError(null);
    void aiWorkspace.choosePermission(mode)
      .then(() => refreshSessions())
      .catch((error) => setSessionError(errorMessage(error)));
  };
  /** Stores the panel's mode on a session the first turn just created. */
  const persistPermission = async (id: string): Promise<void> => {
    if (permission() === DEFAULT_PERMISSION) return;
    try {
      await aiApi.setSessionPermission(id, permission());
    } catch (error) {
      setSessionError(errorMessage(error));
    }
  };
  // Another directory is another workspace: its conversations are another list,
  // and the transcript on screen belongs to the one the panel just left. A turn
  // still running belongs to that one as well, so it is stopped instead of
  // landing its rows in a conversation the user has just left.
  createEffect(on(projectDir, () => {
    scopeEpoch += 1;
    if (pendingUsageRead) cancelUsageHydration(pendingUsageRead);
    pendingUsageRead = null;
    paintRequest += 1;
    setSessions([]);
    if (loading()) {
      void cancel();
      abandonTurn();
    }
    // Keep the conversation the user just left under its own project path
    // before the new workspace clears the screen.
    if (aiSession.open() && aiSession.openScope()) {
      aiSession.rememberOpen(aiSession.openScope()!, aiSession.open()!);
    }
    aiSession.setOpen(null);
    setOpenId(null);
    aiWorkspace.adoptPermission(DEFAULT_PERMISSION);
    aiWorkspace.setTargetPath("project");
    setAllowed([]);
    setMessages([]);
    const epoch = scopeEpoch;
    void refreshSessions().then((ok) => {
      if (ok) void restoreScope(sessionScope(), epoch);
    });
  }, { defer: true }));
  // Another document in the same directory is another project: an edit target
  // such as `track:kick` names a track in the file that was open before it.
  createEffect(on(() => store.filePath(), () => {
    aiWorkspace.setTargetPath("project");
  }, { defer: true }));
  /**
   * A target restored from `localStorage` was saved against whichever document
   * was open then. A track it names that this project does not have cannot
   * address anything, so it falls back to the whole project.
   */
  const validateTarget = () => {
    const target = aiWorkspace.targetPath();
    const name = target.startsWith("track:") ? target.slice("track:".length).split(".")[0] : "";
    if (!name || store.project.tracks.some(track => track.name === name)) return;
    aiWorkspace.setTargetPath("project");
    log.warn("ai", `edit target ${target} is not in ${store.filePath() ?? "this project"}; using the whole project`);
  };
  /** Paints a saved conversation. The draft is the caller's business: a switch
   * clears it, a remount keeps it. */
  const paintSession = async (id: string): Promise<boolean> => {
    const epoch = scopeEpoch;
    const request = ++paintRequest;
    const usageRead = beginUsageHydration(id);
    pendingUsageRead = usageRead;
    try {
      let loaded = loadedSession(await aiApi.loadSession(id));
      if (!loaded || disposed || epoch !== scopeEpoch || request !== paintRequest) return false;
      if (usageHydrationNeedsReconcile(usageRead, loaded.usage)) {
        loaded = loadedSession(await aiApi.loadSession(id));
        if (!loaded || disposed || epoch !== scopeEpoch || request !== paintRequest) return false;
      }
      setMessages(restoredTranscript(loaded.messages));
      setCompactedSession(loaded.compacted === true);
      setSessionNamed(loaded.meta.named === true);
      // The conversation's stored mode; one saved before the field existed reads
      // as the default.
      aiWorkspace.adoptPermission(loaded.meta.permission);
      // Destructive calls this conversation already allowed keep their answer.
      setAllowed(allowedTools(loaded.meta.id));
      setOriginalView(false);
      // Restoring deliberately returns to the latest turn and resumes following.
      queueMicrotask(() => {
        transcriptFollow?.resume();
      });
      aiSession.setOpenScoped(sessionScope(), loaded.meta.id);
      setOpenId(loaded.meta.id);
      selectStatsSession(loaded.meta.id);
      hydrateSessionUsage(loaded.meta.id, loaded.usage, loaded.usage_error, usageRead);
      return true;
    } finally {
      cancelUsageHydration(usageRead);
      if (pendingUsageRead === usageRead) pendingUsageRead = null;
    }
  };
  /**
   * Picks the conversation this scope should open: the one still in the store
   * when it belongs here, else the last one cached for this workspace + file.
   * A remembered id that no longer lists is forgotten instead of retried.
   */
  async function restoreScope(scope: string, epoch: number): Promise<void> {
    if (disposed || epoch !== scopeEpoch) return;
    const list = sessions();
    const previous = aiSession.open();
    const previousScope = aiSession.openScope();
    const remembered = aiSession.rememberedOpen(scope);
    const previousFits = !!previous
      && (previousScope === null || previousScope === scope)
      && list.some(session => session.id === previous);
    const rememberedFits = !!remembered && list.some(session => session.id === remembered);
    const candidate = previousFits ? previous : rememberedFits ? remembered : null;
    if (!candidate) {
      if (previous && !previousFits) {
        aiSession.setOpen(null);
        setOpenId(null);
      }
      return;
    }
    try {
      if (!(await paintSession(candidate)) && epoch === scopeEpoch) {
        aiSession.forgetOpen(scope);
        aiSession.setOpenScoped(scope, null);
        setOpenId(null);
      }
    } catch {
      aiSession.forgetOpen(scope);
      aiSession.setOpenScoped(scope, null);
      setOpenId(null);
    }
  }

  /**
   * The `/` palette: local conversation commands. `/compact` runs at once;
   * `/drop_history <n>` needs its count, so choosing it fills the composer.
   */
  const COMMANDS = [
    { name: "compact", summary: "ai.command.compact" },
    { name: "drop_history", summary: "ai.command.dropHistory" },
    { name: "goal", summary: "ai.command.goal" },
  ] as const;

  const commandQuery = createMemo(() => {
    const draft = input();
    if (!draft.startsWith("/") || draft.includes("\n")) return null;
    return draft.slice(1).trimStart();
  });
  const commandMatches = createMemo(() => {
    const query = commandQuery();
    if (query === null) return [];
    const head = query.split(/\s+/)[0] ?? "";
    return COMMANDS.filter((command) => command.name.startsWith(head));
  });

  /**
   * The `@` palette: project paths the next request can be scoped to. A path
   * that no longer exists in the open project is dropped rather than sent.
   */
  const referenceMatches = createMemo(() => {
    // A draft that starts with `/` is a command, so the command palette owns
    // the arrow keys and Enter for as long as that token is being typed.
    if (commandQuery() !== null) return [];
    const query = referenceQuery(input());
    if (query === null) return [];
    return filterReferences(projectReferences(store.project), query);
  });
  /** The path as the open project knows it: a stale one reads the project. */
  const knownReference = (path: string | null): string | null => {
    if (!path) return null;
    return projectReferences(store.project).some(reference => reference.path === path) ? path : null;
  };
  const activeScope = createMemo(() => knownReference(scope()));
  const chooseReference = (reference: ProjectReference) => {
    setInput(withoutReferenceQuery(input()));
    setScope(reference.path);
    setReferenceIndex(0);
  };
  const clearScope = () => {
    setScope(null);
    setReferenceIndex(0);
  };

  const runCommand = async (name: string): Promise<void> => {
    const draft = input().trim();
    const argument = draft.slice(1).trimStart().split(/\s+/)[1];
    if (name === "goal") {
      // The staged plan for this conversation: one step per round instead of a
      // whole track in one call.
      const text = draft.slice(1).replace(/^\s*goal\s*/, "").trim();
      if (!text) {
        setInput("/goal ");
        return;
      }
      setInput("");
      aiSession.setGoal(text);
      notify(i18n.t("ai.command.goalSet", { goal: text }), "success");
      return;
    }
    if (name === "compact") {
      const id = openId();
      if (!id) return;
      setInput("");
      await compactSession(id);
      return;
    }
    if (name === "drop_history") {
      const keep = Number(argument);
      if (!Number.isInteger(keep) || keep < 1) {
        setInput("/drop_history ");
        return;
      }
      const id = openId();
      if (!id) return;
      setInput("");
      try {
        const result = await aiApi.dropSessionHistory(id, keep);
        notify(
          i18n.t("ai.command.dropped", {
            records: result.dropped_records,
            assets: result.removed_assets,
          }),
          "success",
        );
        setOriginalView(false);
        await paintSession(id);
      } catch (error) {
        notify(String(error), "error");
      }
    }
  };

  /**
   * Name a new conversation from its first exchange. The store opens a session
   * with the first user message as its title, which reads like a prompt in the
   * list; the backend replaces it with a short summary and keeps the old title
   * if the provider cannot answer. Runs once per conversation.
   */

  let namingAttempts = 0;
  const nameSession = async (id: string): Promise<void> => {
    if (sessionNamed() || disposed || namingAttempts >= 3) return;
    namingAttempts += 1;
    try {
      await aiApi.nameSession(id, getActiveProvider().id);
      setSessionNamed(true);
      await refreshSessions();
    } catch (error) {
      // Keep the provisional title and try again on the next turn, a bounded
      // number of times, so a provider that cannot answer is not asked forever.
      log.warn("ai", `naming ${id} failed: ${String(error)}`);
    }
  };

  /**
   * Revert to before a person's message: the message and everything after it
   * leave the conversation, and the project returns to the snapshot it was
   * sent against. The backend does both as one step and one undo entry.
   *
   * The cut takes a wire index, not a row index: a restored row carries the
   * message it came from, and a row the live turn painted is matched against
   * the saved wire, because rows hide envelopes and delivery messages.
   */
  const revertToBefore = async (msg: ChatMessage): Promise<void> => {
    const id = openId();
    if (!id || msg.role !== "user" || messages().indexOf(msg) === 0) return;
    // A running turn keeps writing into the history this would rewrite.
    if (loading() || reattached()) {
      notify(i18n.t("ai.revert.turnRunning"), "error");
      return;
    }
    // The original view shows the history a compaction replaced, whose indices
    // do not address the saved wire a revert cuts.
    if (originalView()) return;
    try {
      let keep = msg.wireIndex;
      if (keep === undefined) {
        const loaded = loadedSession(await aiApi.loadSession(id));
        keep = (loaded && savedWireIndex(msg, loaded.messages)) ?? undefined;
      }
      if (keep === undefined) {
        notify(i18n.t("ai.revert.unsaved"), "error");
        return;
      }
      const reverted = await aiApi.revertBefore(id, keep);
      store.applyAiSnapshot(reverted.snapshot);
      setOriginalView(false);
      await paintSession(id);
      notify(i18n.t("ai.revert.beforeDone"), "success");
    } catch (error) {
      notify(i18n.t("ai.revert.beforeFailed", { error: errorMessage(error) }), "error");
    }
  };

  /** Paint the history before compaction, including the turns it replaced. */
  const paintOriginal = async (id: string): Promise<void> => {
    const epoch = scopeEpoch;
    try {
      const loaded = await aiApi.originalSession(id);
      if (disposed || epoch !== scopeEpoch) return;
      setMessages(restoredTranscript(loaded.messages));
      setOriginalView(true);
      queueMicrotask(() => transcriptFollow?.resume());
    } catch {
      // A session that cannot be read keeps the compacted view.
    }
  };

  /** The turn the backend reports running, or `null` when it reports none. */
  const readActiveTurn = async (): Promise<ActiveTurn | null> => {
    try {
      return activeTurn(await aiApi.readActiveTurn());
    } catch {
      return null;
    }
  };
  let turnPoll: ReturnType<typeof setInterval> | null = null;
  const stopFollowing = () => {
    if (turnPoll !== null) { clearInterval(turnPoll); turnPoll = null; }
    const inherited = reattached();
    if (inherited && approval()?.requestId === inherited.requestId) setApproval(null);
    setReattached(null);
  };
  /**
   * The turn this window was running when it went away. The store kept a marker
   * for it; only the backend's own answer keeps that marker alive, because a
   * marker that outlived its turn must never paint a turn as running. A turn
   * the backend still holds is followed to its end, and the conversation is
   * repainted from the saved one, where its answer was written.
   */
  const restoreInFlightTurn = async () => {
    const marker = aiSession.turn();
    if (!marker || !sessions().some(session => session.id === marker.session)) return;
    const epoch = scopeEpoch;
    const active = await readActiveTurn();
    if (disposed || epoch !== scopeEpoch) return;
    if (!active || active.session_id !== marker.session) {
      // Nothing is running: the marker is cleared so it cannot lie later.
      aiSession.setTurn(null);
      setReloadNotice("lost");
      return;
    }
    setReattached({ requestId: active.request_id, session: marker.session });
    turnPoll = setInterval(() => void followTurn(), TURN_POLL_MS);
    // A card that was open when the window went away is still waiting.
    const waiting = await aiApi.turnQuestions(active.request_id).catch(() => []);
    if (!disposed && reattached()?.requestId === active.request_id && waiting[0]) {
      showQuestion(active.request_id, waiting[0]);
    }
  };
  /** Asks whether the inherited turn is still running, and settles it when not. */
  const followTurn = async () => {
    const current = reattached();
    if (!current) return;
    const active = await readActiveTurn();
    if (active?.request_id === current.requestId) return;
    stopFollowing();
    await settleInheritedTurn(current.session);
  };
  /** The inherited turn finished: its answer is in the saved conversation. */
  const settleInheritedTurn = async (session: string) => {
    aiSession.setTurn(null);
    const painted = await paintSession(session).catch(() => false);
    if (disposed) return;
    setReloadNotice(null);
    setMessages(prev => [...prev, {
      role: "system",
      content: i18n.t(painted ? "ai.reload.settled" : "ai.reload.settledUnpainted"),
      timestamp: Date.now(),
    }]);
    void flushQueued();
  };
  /**
   * Stops the turn the reloaded window inherited, through the same honest
   * cancel as Stop. The loop that would have used the answer died with the
   * window, so the turn is ended rather than left to write into nothing. A
   * steer flushes the queue itself, so it asks for no flush here.
   */
  const stopInheritedTurn = async (flush = true) => {
    const current = reattached();
    if (!current) return;
    stopFollowing();
    aiSession.setTurn(null);
    await aiApi.cancel(current.requestId).catch(() => {});
    // The turn closes its round and commits before it lets go of its slot;
    // the saved conversation is complete once the backend stops reporting it.
    for (let waited = 0; waited < TURN_SETTLE_MS; waited += TURN_POLL_MS) {
      const active = await readActiveTurn();
      if (disposed || active?.request_id !== current.requestId) break;
      await new Promise((resolve) => setTimeout(resolve, TURN_POLL_MS));
    }
    if (disposed) return;
    const painted = await paintSession(current.session).catch(() => false);
    if (disposed) return;
    setReloadNotice(null);
    setMessages(prev => [...prev, {
      role: "system",
      content: i18n.t(painted ? "ai.reload.stopped" : "ai.reload.stoppedUnpainted"),
      timestamp: Date.now(),
    }]);
    // A settled turn sends the message waiting for it, as any stop does.
    if (flush) void flushQueued();
  };
  /** Reads the saved conversation again, for an answer saved after the mount. */
  const reloadTranscript = async () => {
    const id = aiSession.turn()?.session ?? openId();
    if (!id) {
      setReloadNotice(null);
      return;
    }
    const painted = await paintSession(id).catch(() => false);
    if (disposed) return;
    setReloadNotice(null);
    if (!painted) return;
    setMessages(prev => [...prev, {
      role: "system",
      content: i18n.t("ai.reload.reloaded"),
      timestamp: Date.now(),
    }]);
  };

  const switchSession = async (id: string) => {
    if (loading() || sessionBusy()) return;
    setSessionBusy(true);
    setSessionError(null);
    try {
      if (await paintSession(id)) {
        setInput("");
        setAttachments([]);
        aiSession.clearQueue();
        setSessionsOpen(false);
      }
    } catch (e) { setSessionError(errorMessage(e)); }
    finally { setSessionBusy(false); }
  };
  /** A new conversation starts on the first message, which titles it; the
   * backend's autosave then has a session to append the turn to. A turn still
   * running is stopped first: a fresh conversation is a way out of one that
   * will not settle. */
  const startConversation = () => {
    if (loading()) {
      void cancel();
      abandonTurn();
    }
    setMessages([]);
    setSessionError(null);
    aiSession.setOpenScoped(sessionScope(), null);
    setOpenId(null);
    // A conversation with no stored setting reads as the default.
    aiWorkspace.adoptPermission(DEFAULT_PERMISSION);
    setAllowed([]);
    setInput("");
    setAttachments([]);
    aiSession.clearQueue();
    setSessionsOpen(false);
  };
  const forkSession = async (id: string) => {
    if (loading() || sessionBusy()) return;
    setSessionBusy(true);
    setSessionError(null);
    try {
      const meta = await aiApi.newSession(undefined, id);
      if (!meta || typeof meta.id !== "string") return;
      await refreshSessions();
      if (await paintSession(meta.id)) {
        setInput("");
        setAttachments([]);
        aiSession.clearQueue();
        setSessionsOpen(false);
      }
    } catch (e) { setSessionError(errorMessage(e)); }
    finally { setSessionBusy(false); }
  };
  /**
   * Fork the open conversation at one completed turn. The child keeps the saved
   * messages up to and including that row, so branching leaves the parent's
   * later turns out of the new conversation.
   */
  /**
   * Whether the row is the answer a turn ends on. A tool row or another
   * assistant row after it means the turn is still running, and a fork there
   * would cut the answer out of the conversation it was writing into.
   */
  const endsTurn = (msg: ChatMessage): boolean => {
    const rows = messages();
    const next = rows[rows.indexOf(msg) + 1];
    return !next || (next.role !== "tool" && next.role !== "assistant");
  };

  const forkFromTurn = async (msg: ChatMessage): Promise<void> => {
    const id = openId();
    if (!id || loading() || reattached() || originalView() || sessionBusy()) return;
    let keep = msg.wireIndex;
    if (keep === undefined) {
      const loaded = loadedSession(await aiApi.loadSession(id));
      keep = (loaded && savedWireIndex(msg, loaded.messages)) ?? undefined;
    }
    if (keep === undefined) {
      notify(i18n.t("ai.fork.unsaved"), "error");
      return;
    }
    setSessionBusy(true);
    setSessionError(null);
    try {
      const meta = await aiApi.newSession(undefined, id, keep + 1);
      if (!meta || typeof meta.id !== "string") return;
      await refreshSessions();
      if (await paintSession(meta.id)) {
        setInput("");
        setAttachments([]);
        aiSession.clearQueue();
        setSessionsOpen(false);
        notify(i18n.t("ai.fork.done", { title: meta.title }), "success");
      }
    } catch (e) { setSessionError(errorMessage(e)); }
    finally { setSessionBusy(false); }
  };

  const beginRename = (session: SessionMeta) => {
    setDeleting(null);
    setRenaming({ id: session.id, title: session.title });
  };
  const commitRename = async () => {
    const draft = renaming();
    if (!draft) return;
    setRenaming(null);
    const title = draft.title.trim();
    if (!title) return;
    setSessionError(null);
    try {
      await aiApi.renameSession(draft.id, title);
      await refreshSessions();
    } catch (e) { setSessionError(errorMessage(e)); }
  };
  const deleteSession = async (id: string) => {
    setDeleting(null);
    setSessionError(null);
    try {
      await aiApi.deleteSession(id);
      if (openId() === id) {
        setMessages([]);
        aiSession.setOpenScoped(sessionScope(), null);
        aiSession.clearQueue();
        setOpenId(null);
      }
      await refreshSessions();
    } catch (e) { setSessionError(errorMessage(e)); }
  };
  /** Folds the conversation and says what it dropped, in the same line an
   * automatic compaction writes. */
  const compactSession = async (id: string) => {
    if (loading() || sessionBusy()) return;
    setSessionBusy(true);
    setSessionError(null);
    try {
      if (openId() !== id) {
        if (!(await paintSession(id))) return;
        setInput("");
        setAttachments([]);
        aiSession.clearQueue();
      }
      const result = await aiApi.compactSession(id);
      if (!result || typeof result.tokens_after !== "number") return;
      setMessages(previous => [...previous, { role: "system", content: compactionNotice(result), timestamp: Date.now() }]);
      await refreshSessions();
    } catch (e) { setSessionError(errorMessage(e)); }
    finally { setSessionBusy(false); }
  };
  /**
   * The document to copy or download. Markdown is the transcript on screen,
   * JSON is the stored wire; both carry the metadata `sessions_load` returns.
   */
  const buildExport = async (format: ExportFormat): Promise<{ name: string; text: string } | null> => {
    const id = openId();
    if (!id) return null;
    const loaded = loadedSession(await aiApi.loadSession(id));
    if (!loaded) return null;
    return format === "markdown"
      ? { name: exportFileName(loaded.meta, "md"), text: exportMarkdown(loaded.meta, messages()) }
      : { name: exportFileName(loaded.meta, "json"), text: exportJson(loaded.meta, loaded.messages) };
  };
  const runExport = async (format: ExportFormat, noteKey: I18nKey, deliver: (name: string, text: string) => Promise<void>) => {
    setExportBusy(true);
    setExportNote(null);
    try {
      const built = await buildExport(format);
      if (!built) return;
      await deliver(built.name, built.text);
      setExportNote(i18n.t(noteKey, { name: built.name }));
    } catch (e) {
      setExportNote(i18n.t("ai.export.failed", { error: errorMessage(e) }));
    } finally {
      setExportBusy(false);
    }
  };
  const copyExport = (format: ExportFormat) =>
    runExport(format, "ai.export.copied", async (_name, text) => { await copyToClipboard(text); });
  const downloadExport = (format: ExportFormat) =>
    runExport(format, "ai.export.saved", async (name, text) => {
      downloadText(name, text, format === "markdown" ? "text/markdown" : "application/json");
    });

  onMount(async () => {
    // A panel with no conversation starts at the default mode; a restored
    // conversation adopts its own stored permission when it paints.
    if (!aiSession.open()) aiWorkspace.adoptPermission(DEFAULT_PERMISSION);
    validateTarget();
    // A turn that died with its batch open left it to this conversation: it
    // commits, so the edits that landed stay. Start the recovery first so it
    // precedes the first read, but let the provider config and the catalog
    // load in parallel, so the model picker is filled on the first paint.
    const recovered = aiApi.recoverBatch()
      .then((snapshot) => {
        if (snapshot?.project) store.applyAiSnapshot(snapshot);
        return snapshot;
      })
      .catch((e) => {
        log.warn("ai", `panel batch recovery failed: ${errorMessage(e)}`);
        return null;
      });
    // A remounted panel finds the preview the backend still holds.
    void previewStore.refresh();
    try {
      await loadConfig();
      await aiWorkspace.ensureCatalog(true);
    } catch (e) { notify(errorMessage(e), "error"); }
    // Nothing may start a turn against a document the recovery is about to
    // publish, so mount finishes only after the recovery did.
    await recovered;
    // A compacted conversation is reported in the transcript, so the summary is
    // never a silent loss of what the user saw.
    stopSessionEvents = await aiApi.listenSession(payload => {
      const saved = payload as { request_id?: string; session_id?: string; saved_auditions?: WireMessage[] } | null;
      if (saved?.request_id && saved.request_id === requestId && saved.session_id === openId()
          && Array.isArray(saved.saved_auditions)) {
        const rows = restoredTranscript(saved.saved_auditions)
          .filter(row => row.context === "audition")
          .map(row => ({ ...row, wireIndex: undefined, timestamp: Date.now() }));
        setMessages(previous => [...previous, ...rows]);
        return;
      }
      // A full audition archive made room for this turn's listening clips: the
      // transcript names what went, so the operator's copy is not a silent loss.
      const evicted = sessionArchiveEviction(payload);
      if (evicted) {
        log.info("ai", `session ${evicted.session_id} evicted ${evicted.evicted.length} archive file(s)`);
        setMessages(previous => [...previous, { role: "system", content: archiveNotice(evicted), timestamp: Date.now() }]);
        return;
      }
      const compaction = sessionCompaction(payload);
      if (!compaction) return;
      log.info("ai", `session ${compaction.session_id} compacted ${compaction.dropped_from}..${compaction.dropped_to}`);
      setMessages(previous => [...previous, { role: "system", content: compactionNotice(compaction), timestamp: Date.now() }]);
    });
    stopTurnEvents = await aiApi.listenTurn(onTurnEvent);
    stopProjectEvents = await aiApi.listenTurnProject((snapshot) => {
      if (!disposed && loading()) store.applyAiSnapshot(snapshot);
    });
    // A panel unmounted while the listeners registered ran its cleanup with
    // nothing to remove; they are removed here instead.
    if (disposed) {
      stopSessionEvents?.();
      stopTurnEvents?.();
      stopProjectEvents?.();
      return;
    }
    await aiWorkspace.ensureToolsets(true);
    const epoch = scopeEpoch;
    // A conversation restored from another project's cache is not this
    // project's transcript. Drop it before the list answers, so a scope race
    // cannot leave the previous song's id on screen.
    if (aiSession.open() && aiSession.openScope() && aiSession.openScope() !== sessionScope()) {
      aiSession.setOpen(null);
      setOpenId(null);
    }
    if (!(await refreshSessions()) || disposed || epoch !== scopeEpoch) return;
    // A panel that was unmounted (sidebar closed, tab switched) comes back on
    // the conversation this workspace/project had open, repainted from the
    // store or from the per-scope cache.
    await restoreScope(sessionScope(), epoch);
    if (disposed || epoch !== scopeEpoch) return;
    // A reloaded window finds the turn it was running only if the backend still
    // has it; the marker alone is not evidence.
    await restoreInFlightTurn();
  });
  /** Opens the settings page on the first entry the catalog cannot describe;
   * its model is picked from the header selector. */
  const openUndescribedProvider = () => {
    const warning = aiConfig.warnings()[0];
    if (warning) aiConfig.setActiveProvider(warning.provider_id);
    openSettings();
  };
  /** Picking a model from another provider switches both in one action. */
  const selectModel = (value: string) => {
    if (applyModelChoice(catalog(), value)) setAuthError(null);
  };
  /** The turn this panel runs: its id, its token and the messages it saved. */
  let turnEvents: { id: string; token: number; live: WireMessage[] } | null = null;
  /**
   * One event from a running turn. A turn this window inherited from a reload
   * can only ask; the transcript repaints from the store when it ends.
   */
  const onTurnEvent = (event: TurnEnvelope) => {
    if (disposed) return;
    const inherited = reattached();
    if (inherited && event.request_id === inherited.requestId) {
      if (event.type === "ask") showQuestion(event.request_id, event.request);
      return;
    }
    const turn = turnEvents;
    if (!turn || event.request_id !== turn.id || !liveTurn(turn.token)) return;
    switch (event.type) {
      case "message":
        paintSaved(turn.live, event.message);
        break;
      case "running":
        setCurrentTool(event.tool);
        break;
      case "ask":
        showQuestion(event.request_id, event.request);
        // A replacement shows the change it asks about, as a review does.
        if (event.request.after !== undefined) {
          void paintPreview(event.request.path, event.request.before ?? "", event.request.after);
        }
        break;
      case "preview":
        void paintPreview(event.path, event.before, event.after);
        break;
      case "notice":
        setMessages(prev => [...prev, {
          role: "system", content: event.text, timestamp: Date.now(), ...(event.failed ? { error: true } : {}),
        }]);
        break;
      case "media":
        applyMediaNotices(event.notices);
        break;
    }
  };
  /**
   * One message the turn saved, as the rows a reload would paint for it. The
   * turn's earlier messages join a result to its call.
   */
  const paintSaved = (live: WireMessage[], message: WireMessage) => {
    live.push(message);
    const index = live.length - 1;
    const rows: ChatMessage[] = restoredTranscript(live)
      .filter(row => row.wireIndex === index)
      .map(row => ({ ...row, wireIndex: undefined, timestamp: Date.now() }));
    if (message.role === "assistant") {
      // Streamed reasoning the saved message does not carry still gets its row.
      const streamed = reasoning().trim();
      if (streamed && !rows.some(row => row.reasoning)) {
        rows.unshift({ role: "assistant", content: "", reasoning: reasoning(), timestamp: Date.now() });
      }
      setPartial("");
      setReasoning("");
      setPreparingTool(null);
    }
    if (message.role === "tool") setCurrentTool(null);
    if (rows.length) setMessages(prev => [...prev, ...rows]);
  };

  /**
   * The request carries the system prompt and the pending user message. The
   * conversation itself lives in the backend's saved session, so every turn —
   * tool calls and tool results included — is replayed from there instead of
   * being rebuilt here from the prose rows the transcript shows.
   */
  const buildMessages = (systemPrompt: string, userMessage: ChatMessage) => {
    const history: WireMessage[] = [
      { role: "system", content: systemPrompt },
      {
        role: "user",
        message_meta: { version: 1, kind: "user" },
        content: userMessage.content,
        ...(userMessage.media?.length ? { media: userMessage.media } : {}),
        ...(userMessage.dslSha ? { dsl_sha: userMessage.dslSha } : {}),
      },
    ];
    return history;
  };

  /**
   * The one transcript line a stopped turn earns. The loop reports what landed;
   * the wording is the panel's, so it is translated with the rest of the UI.
   */
  const stopNotice = (stop: TurnStop): string => {
    if (stop.commit_error) return i18n.t("ai.failure.commitFailed", { error: stop.commit_error });
    if (stop.timed_out) {
      return stop.edits > 0
        ? i18n.t("ai.timeout.kept", { edits: stop.edits })
        : i18n.t("ai.timeout.keptNone");
    }
    if (stop.cancelled) {
      return stop.edits > 0
        ? i18n.t("ai.cancel.kept", { edits: stop.edits })
        : i18n.t("ai.cancel.keptNone");
    }
    if (stop.failure_kept) return i18n.t("ai.failure.kept", { edits: stop.edits });
    return stop.edits > 0
      ? i18n.t("ai.cap.kept", { edits: stop.edits })
      : i18n.t("ai.cap.keptNone");
  };

  /** The composer gets back the message a failed or stopped turn used. */
  const restoreComposer = (question: ChatMessage | null) => {
    if (!question) return;
    if (!input().trim()) setInput(question.content);
    const media = question.media ?? [];
    if (media.length && !attachments().length) setAttachments(media);
  };

  /** The anchor a stopped row keeps: what the turn sent and how it ended. */
  const stoppedTurn = (sent: WireMessage[], stop: TurnStop | undefined, retryable?: boolean): StoppedTurn => ({
    messages: sent,
    edits: stop?.edits ?? 0,
    ...(stop ? { stop } : {}),
    ...(retryable === false ? { retryable: false } : {}),
  });

  /**
   * The row a finished turn earns. An answer is already on screen as the
   * message the turn saved; a stop or a failure says what happened and offers
   * Retry or Continue.
   */
  const settleOutcome = (outcome: TurnOutcome, sent: WireMessage[], question: ChatMessage) => {
    const error = outcome.error;
    if (error && errorKind(error) === "auth_expired") setAuthError(errorMessage(error));
    if (!outcome.failed) return;
    const reason = error ? `Error: ${errorMessage(error)}` : null;
    const content = outcome.stop
      ? [reason, stopNotice(outcome.stop)].filter(Boolean).join("\n")
      : reason ?? outcome.text;
    setMessages(prev => [...prev, {
      role: "system",
      content,
      timestamp: Date.now(),
      ...(outcome.stop ? {} : { error: true }),
      resume: stoppedTurn(sent, outcome.stop, error?.retryable),
    }]);
    restoreComposer(question);
  };

  /**
   * The live turn. Every write a turn makes after an await is fenced by its
   * token, so a panel that moved on (another workspace, a fresh conversation)
   * never receives the rows of a turn it abandoned.
   */
  const liveTurn = (token: number) => token === turnToken;
  /** A turn that may still act: its token is live and nobody stopped it. */
  const cancelledFor = (token: number) => stoppedTokens.has(token) || !liveTurn(token);

  /** Seconds since a moment, from the pending row's own clock. */
  const secondsSince = (start: number | null) =>
    start === null ? 0 : Math.max(0, (clock() - start) / 1000);

  /** What the turn is doing, as the pending row's one line. The stop states
   * are the panel's own; every other phase speaks the same language as the
   * row, so the waiting line names the model and the tool line names the call. */
  const phaseText = (): string => {
    if (stopRequestedAt() !== null) {
      const stopped = secondsSince(stopRequestedAt());
      return stopStalled(stopped)
        ? i18n.t("ai.turn.stalled", { seconds: elapsedLabel(stopped) })
        : i18n.t("ai.turn.stopping");
    }
    return thinkingState({
      loading: true,
      partial: "",
      streaming: !!partial(),
      approval: !!approval(),
      lastTool: currentTool(),
      preparingTool: toolPreparation(),
      identity: modelLabel(),
    })?.label ?? i18n.t("ai.turn.waiting");
  };
  // The pending row counts while a turn runs, and stops when it settles.
  createEffect(() => {
    if (turnStartedAt() === null) return;
    const timer = setInterval(() => setClock(Date.now()), 500);
    onCleanup(() => clearInterval(timer));
  });

  /** The panel moved on: the next turn owns the composer, the old one is fenced. */
  const abandonTurn = () => {
    stoppedTokens.add(turnToken);
    turnToken += 1;
    setLoading(false);
    setTurnStartedAt(null);
    setStopRequestedAt(null);
    setPartial("");
  };

  /** The diff row a replacement paints. */
  const proposalRow = (path: string, diff: DiffResult): ChatMessage =>
    ({ role: "assistant", content: i18n.t("ai.edit.validatedFor", { path }), diff, timestamp: Date.now() });

  /**
   * A replacement the turn asks about or is about to apply, painted as its
   * diff, so the transcript records what changed.
   */
  const paintPreview = async (path: string | undefined, before: string, after: string): Promise<void> => {
    const diff = await aiApi.diffDsl(before, after);
    if (disposed) return;
    setMessages((prev) => [...prev, proposalRow(path ?? "project", diff)]);
  };

  /**
   * The one entry every turn goes through: Send, Retry and Continue. The
   * backend runs the turn and saves every message; the panel paints its
   * events. A resumed turn sends only its system prompt, because the saved
   * conversation already holds the question and what followed it.
   */
  const executeTurn = async (turn: {
    user?: ChatMessage;
    resume?: StoppedTurn;
    /** The project path this request reads; the whole project when absent. */
    reference?: string | null;
  }): Promise<boolean> => {
    const resumed = turn.resume;
    // Captured once: a later draft or a switch of reference must not move the
    // context of a turn that already started.
    const reference = turn.reference ?? null;
    const question = turn.user ?? (resumed ? userTurnOf(resumed.messages) : null);
    if (!question) return false;
    // True once the request reaches the backend: a caller that took a message
    // off the queue keeps it only when this turn never sent it.
    let posted = false;
    const provider = getActiveProvider();
    const model = currentModel();
    const supportsAudio = audioAccepted(model, provider);
    // This turn owns the composer from here on: an older turn's late rows and
    // its queue flush are dropped instead of landing in this one.
    const token = ++turnToken;
    // Cancel is on screen from the first await, so the turn owns an id at once;
    // cancelling before it starts removes nothing on the backend.
    const id = crypto.randomUUID();
    requestId = id;
    const startedAt = Date.now();
    setAuthError(null);
    setMediaError(null);
    setMediaNotice(null);
    setCurrentTool(null);
    setStopRequestedAt(null);
    setTurnStartedAt(startedAt);
    setClock(startedAt);
    setPartial("");
    setReasoning("");
    setPreparingTool(null);

    setLoading(true);
    // The composer is this turn's from here on, so the flush that took the
    // message may release the queue to the turn that follows it.
    flushing = false;
    // The first message of a conversation starts it: the backend saves the
    // turn into a conversation, and the store has a title for it.
    let startError: unknown = null;
    if (!openId()) {
      try {
        const meta = await aiApi.newSession(question.content);
        if (!liveTurn(token)) return posted;
        if (meta && typeof meta.id === "string") {
          aiSession.setOpenScoped(sessionScope(), meta.id);
          setOpenId(meta.id);
          // The mode shown here applies to the conversation this turn started.
          await persistPermission(meta.id);
          if (!liveTurn(token)) return posted;
          void refreshSessions();
        }
      } catch (e) {
        startError = e;
        log.warn("ai", `conversation not started: ${errorMessage(e)}`);
      }
    }
    const sessionId = openId();
    // A window that reloads while this turn runs keeps this marker; the next
    // mount asks the backend whether it still holds the turn.
    if (sessionId) aiSession.setTurn({ session: sessionId, started: startedAt });
    try {
      if (!sessionId) throw startError ?? new Error("The conversation could not be started");
      log.info("ai", `Using provider: ${provider.name} (${provider.model})`);
      // A resumed turn sends the system prompt it stopped with; a fresh turn
      // reads the project and builds one for the model's capabilities.
      let sent = resumed?.messages;
      if (!sent) {
        const tools = await aiApi.listTools(selectedSets());
        if (!liveTurn(token)) return posted;
        const context = await aiApi.buildContext(reference ?? targetPath(), question.content);
        if (!liveTurn(token)) return posted;
        // The backend owns the skill index; it is read only when the tools that
        // can serve it are offered, and a failure just drops the index.
        const skillTools = tools.some(tool => tool.name === "list_skills") && tools.some(tool => tool.name === "read_skill");
        const skillIndex = skillTools ? await aiApi.listSkills().catch(() => undefined) : undefined;
        if (!liveTurn(token)) return posted;
        // The project's own note, read fresh so a save in Settings reaches the
        // very next turn; a project without one adds nothing to the prompt.
        const note = await readProjectInstructions(store.filePath());
        if (!liveTurn(token)) return posted;

        // Build system prompt adapted to model capabilities
        const systemPrompt = buildSystemPrompt({
          projectName: store.project.name || "Untitled",
          projectDsl: context.dsl_context || store.dslSource(),
          tempo: store.project.properties.tempo,
          timeSig: `${store.project.properties.time_signature.numerator}/${store.project.properties.time_signature.denominator}`,
          key: store.project.properties.key
            ? `${store.project.properties.key.root}${store.project.properties.key.accidental === "Sharp" ? "#" : store.project.properties.key.accidental === "Flat" ? "b" : ""} ${store.project.properties.key.mode.toLowerCase()}`
            : "not set",
          trackCount: store.project.tracks.length,
          busCount: store.project.buses.length,
          projectNote: note.text,
        }, { ...provider.capabilities, audioIn: supportsAudio }, tools, Array.isArray(skillIndex) ? skillIndex : undefined);
        // A goal turns the turn into one step of a plan: the loop creates the
        // track first and adds its patterns after, instead of one whole block.
        const goal = aiSession.goal();
        const guided = goal
          ? `${systemPrompt}\n\nCurrent goal: ${goal}\nWork one step at a time: create or verify the object first, then add its content in a later call. Do not send a whole track DSL in one call when the goal is still being built.`
          : systemPrompt;
        sent = buildMessages(guided, question);
        // Every request carries this text, so the context figure counts it.
        setRequestOverhead(systemPrompt, tools);
      }
      const outbound = resumed ? sent.filter(message => message.role === "system") : sent;
      posted = true;
      noteSentEstimate(textTokens(JSON.stringify(outbound)), id, sessionId);
      turnEvents = { id, token, live: [] };
      const outcome = await aiApi.startTurn(
        {
          providerId: provider.id,
          requestId: id,
          sessionId,
          messages: outbound,
          toolsets: selectedSets(),
          label: i18n.t("ai.undo.turn"),
          // Destructive tools this conversation already answered "always
          // allow" for; the backend adds the answers given during the turn.
          allowed: allowed(),
        },
        (text) => { if (!cancelledFor(token)) setPartial(value => value + text); },
        (text) => { if (!cancelledFor(token)) setReasoning(value => appendThinking(value, text)); },
        (name) => { if (!cancelledFor(token)) setPreparingTool(name); },
      );
      if (!liveTurn(token)) return posted;
      settleOutcome(outcome, sent, question);
    } catch (e) {
      if (cancelledFor(token)) return posted;
      // A failure before the turn started saved nothing, so the message goes
      // back to the composer instead of offering a Retry.
      const message = errorMessage(e);
      if (errorKind(e) === "auth_expired") setAuthError(message);
      setMessages((prev) => [...prev, { role: "system", content: `Error: ${message}`, timestamp: Date.now() }]);
      restoreComposer(question);
      log.error("ai", message);
    } finally {
      if (turnEvents?.id === id) turnEvents = null;
      if (liveTurn(token)) {
        requestId = null;
        setPartial("");
        setReasoning("");
        setPreparingTool(null);
        setApproval(null);
        setLoading(false);
        setTurnStartedAt(null);
        setStopRequestedAt(null);
        setCurrentTool(null);
        // The turn settled here, so no window may report it as still running.
        aiSession.setTurn(null);
        // The turn moved this conversation's title, turn count and time.
        void refreshSessions();
        // A message composed while the turn ran goes out now, exactly once. A
        // steer owns its own send, so this path stands aside for it.
        if (!steering) void flushQueued();
      }
    }
    return posted;
  };

  /**
   * Sends one message: the user row, the context warning and the turn. Returns
   * false when no turn took it, so a caller that took it off the queue can put
   * it back instead of dropping it.
   */
  const sendUserMessage = async (
    text: string,
    media: MediaAttachment[],
    reference: string | null,
    origin?: FlushOrigin,
  ): Promise<boolean> => {
    const refusal = imageRefusal(media);
    if (refusal) {
      setMediaError(refusal);
      return false;
    }
    // The estimate is a heuristic, so an overflow warns and still sends: only
    // the provider can authoritatively reject the request. It counts the text
    // being sent, not whatever draft the composer holds now.
    const estimatedBeforeSend = estimateFor(text);
    // Snapshot the project before the message goes out, so this row can roll
    // the document back to what it was when the user asked.
    let dslSha: string | undefined;
    const sessionId = openId();
    if (sessionId) {
      try {
        dslSha = (await aiApi.saveSessionSnapshot(sessionId, store.dslSource())).sha;
      } catch {
        // A session that cannot store the snapshot still sends the message.
      }
    }
    // The panel moved to another workspace or conversation while the snapshot
    // was on its way: the message no longer belongs to this turn.
    if (origin && (origin.scope !== scopeEpoch || origin.conversation !== openId())) return false;
    const userMsg: ChatMessage = {
      role: "user",
      content: text,
      timestamp: Date.now(),
      ...(media.length ? { media: media.map(item => usesLocalReference(item)
        ? { ...item, local_reference: true } : item) } : {}),
      ...(dslSha ? { dslSha } : {}),
    };
    transcriptFollow?.resume();
    setMessages((prev) => [...prev, userMsg]);
    const budget = aiLimits.contextBudget(estimatedBeforeSend, aiLimits.activeContext());
    if (budget?.over) {
      log.warn("ai", `estimated context ${budget.used} exceeds ${budget.context}`);
      setMessages((prev) => [...prev, { role: "system", content: i18n.t("ai.limits.over"), timestamp: Date.now() }]);
    }
    const ran = await executeTurn({ user: userMsg, reference });
    if (ran && !sessionNamed() && openId()) void nameSession(openId()!);
    return ran;
  };

  /** The composer field, grown to fit a multi-line draft up to a fixed cap. */
  let inputElement: HTMLTextAreaElement | undefined;
  const growInput = (element: HTMLTextAreaElement | undefined) => {
    if (!element || !element.scrollHeight) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 160)}px`;
  };
  // A restored draft is longer than the empty field the panel mounted with.
  createEffect(() => { input(); queueMicrotask(() => growInput(inputElement)); });

  /** Appends one message to the queue and clears the composer: a send always
   * has an effect, whatever the turn is doing. */
  const enqueue = (text: string, reference: string | null): void => {
    aiSession.setQueued((previous) => [
      ...previous,
      { text, media: attachments(), ...(reference ? { reference } : {}) },
    ]);
    setInput("");
    setAttachments([]);
    setQueuedEditing(null);
  };

  /** Removes one queued message; the editor never points past the end. */
  const removeQueued = (item: QueuedMessage) => {
    aiSession.setQueued((previous) => previous.filter((entry) => entry !== item));
    setQueuedEditing(null);
  };

  /** Opens one queued row for editing; the text is a draft until it commits. */
  const beginQueuedEdit = (item: QueuedMessage) => {
    setQueuedDraft(item.text);
    setQueuedEditing(item);
  };
  /** Enter or blur writes the edit back; Escape closes it uncommitted. */
  const commitQueuedEdit = () => {
    const editing = queuedEditing();
    if (!editing) return;
    const text = queuedDraft().trim();
    setQueuedEditing(null);
    if (!text) return;
    aiSession.setQueued((previous) => previous.map((entry) => entry === editing ? { ...entry, text } : entry));
  };

  /** Why Send is unavailable, or null when it is available. A provider draft is
   * not a blocker: sending writes it first. An empty composer during a turn is
   * not one either: the button is the way out of that turn. */
  const sendBlocked = (): I18nKey | null => {
    // The preview holds the document read-only; the composer says what the
    // backend will say to any mutation.
    if (previewStore.open()) return "ai.preview.readOnly";
    if (!hasProvider()) return "ai.sendBlocked.noProvider";
    if (!getActiveProvider().model.trim()) return "ai.sendBlocked.noModel";
    // A typed key lives in memory until Settings saves it; a request with it
    // unsaved would fail against the credential store's old value.
    if (hasAnyKeyDraft()) return "ai.settings.unsaved";
    if (!input().trim() && !loading()) return "ai.sendBlocked.empty";
    return null;
  };
  /** The one composer button: with nothing typed it stops the turn that is
   * running, and every other state sends. */
  const sendAction = (): "send" | "cancel" =>
    input().trim() === "" && loading() ? "cancel" : "send";
  /** The button's accessible name, one label per state. */
  const sendLabel = () => i18n.t(sendAction() === "cancel" ? "ai.turn.stop" : "ai.input.send");
  /** The line above the composer. A disabled Send always says why, in the same
   * place, so the reason cannot sit off-screen under the transcript. */
  const blockedNotice = (): I18nKey | null => sendBlocked();
  const sendTitle = () => {
    const blocked = sendBlocked();
    if (blocked) return i18n.t(blocked);
    return loading() ? i18n.t("ai.queue.placeholder") : i18n.t("ai.input.send");
  };
  /** Why the queued rows are still waiting, in the state that actually holds. */
  const queueHint = (): string => {
    if (reattached()) return i18n.t("ai.queue.waitingReload");
    if (loading()) return i18n.t("ai.queue.hint");
    return i18n.t("ai.queue.idle");
  };

  const onComposerKey = (e: KeyboardEvent) => {
    const references = referenceMatches();
    if (references.length > 0) {
      const index = Math.min(referenceIndex(), references.length - 1);
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setReferenceIndex((index + 1) % references.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setReferenceIndex((index - 1 + references.length) % references.length);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        chooseReference(references[index]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setInput(withoutReferenceQuery(input()));
        setReferenceIndex(0);
        return;
      }
    }
    const matches = commandMatches();
    if (matches.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setCommandIndex((index) => (index + 1) % matches.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setCommandIndex((index) => (index - 1 + matches.length) % matches.length);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        void runCommand(matches[commandIndex()]?.name ?? matches[0].name);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setInput("");
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void handleSend(); }
  };

  const handleSend = async () => {
    const msg = input().trim();
    if (!msg || !getActiveProvider().model.trim()) return;
    // The scope belongs to this one request, so the send takes it and the
    // composer forgets it. A refused send keeps both the text and the scope.
    const reference = activeScope();
    // Enter reaches this path without the disabled button: the same refusal the
    // composer shows has to hold here.
    if (hasAnyKeyDraft()) return;
    // The turn sends the provider and model on screen, so a draft that was never
    // written would label the request with something the backend does not have.
    // The failure banner names the reason and the draft stays in the composer.
    if (hasUnsavedConfig() && !(await aiConfig.flushSave())) return;
    // A turn in flight takes the message as a queued one. The composer clears
    // either way, so a send is never a click that leaves the draft behind. A
    // turn this window inherited is still a turn: it is queued behind it too.
    if (loading() || reattached()) {
      enqueue(msg, reference);
      clearScope();
      return;
    }
    const attached = attachments();
    const refusal = imageRefusal(attached);
    if (refusal) {
      setMediaError(refusal);
      return;
    }
    setInput("");
    clearScope();
    setAttachments([]);
    setMediaError(null);
    setMediaNotice(null);
    await sendUserMessage(msg, attached, reference);
  };

  /**
   * Sends the oldest queued message once the turn has settled: after a normal
   * answer, a stop, a failure, or the answer to a proposal. The row leaves the
   * queue before the request goes out, so one message is sent once.
   */
  const flushQueued = async (): Promise<void> => {
    // One flush holds the row it took until its turn sets `loading`; without
    // this, two calls in that window take two rows and the first turn is
    // abandoned with its message gone.
    if (flushing) return;
    const next: QueuedMessage | undefined = aiSession.queued()[0];
    if (disposed || !next || loading() || reattached() || !getActiveProvider().model.trim()) return;
    // A refused image keeps the row queued instead of losing the message.
    const refusal = imageRefusal(next.media);
    if (refusal) {
      setMediaError(refusal);
      return;
    }
    flushing = true;
    // A provider draft that was never written would send this message with a
    // model the backend does not have; a failed write leaves it queued.
    if (hasUnsavedConfig() && !(await aiConfig.flushSave())) {
      flushing = false;
      return;
    }
    const origin: FlushOrigin = { scope: scopeEpoch, conversation: openId() };
    const taken = aiSession.takeQueued();
    setQueuedEditing(null);
    setMediaError(null);
    setMediaNotice(null);
    if (!taken) {
      flushing = false;
      return;
    }
    const ran = await sendUserMessage(taken.text, taken.media, knownReference(taken.reference ?? null), origin);
    if (!ran) {
      // The panel moved on before the turn started: the message waits at the
      // head of the queue instead of disappearing with the abandoned turn.
      flushing = false;
      aiSession.setQueued((previous) => [taken, ...previous]);
    }
  };

  /** Steer: stop the current turn through the honest cancel, then send. */
  const steerQueued = async (): Promise<void> => {
    if (!aiSession.queued().length) return;
    // The turn this window inherited is one it cannot see; stopping it is the
    // only honest way to send the message now.
    if (reattached()) {
      await stopInheritedTurn(false);
      await flushQueued();
      return;
    }
    if (!loading()) return;
    steering = true;
    try { await cancel(); } finally { steering = false; }
    await flushQueued();
  };

  /** Retry resends the stopped turn; Continue resumes it where it stopped. */
  const resumeTurn = async (row: ChatMessage, action: "retry" | "continue") => {
    const resume = row.resume;
    // One turn at a time includes the turn this window inherited.
    if (!resume || loading() || reattached()) return;
    const question = userTurnOf(resume.messages);
    const refusal = question ? imageRefusal(question.media ?? []) : null;
    if (refusal) {
      setMediaError(refusal);
      return;
    }
    // One actionable row: this row hands its turn to the one that runs now.
    // An unsaved answer is not part of the conversation, so a retry replaces
    // its row instead of leaving the same answer above the new one.
    setMessages(prev => prev
      .filter(message => !(row.warning && message.warning))
      .map(message => message === row ? { ...message, resume: undefined } : message));
    if (action === "continue") {
      const stop = resume.stop;
      setMessages(prev => [...prev, {
        role: "system",
        content: stop
          ? i18n.t("ai.continue.resumed", { done: stop.done, planned: stop.planned })
          : i18n.t("ai.continue.resumedSaved"),
        timestamp: Date.now(),
      }]);
    }
    await executeTurn({ resume });
  };

  /** Why Retry is safe: it resends the last request, not the turn's calls. */
  const retryNote = (resume: StoppedTurn): string => {
    if (resume.stop?.failure_kept || resume.stop?.timed_out) {
      return i18n.t(resume.edits === 1 ? "ai.retry.keptOne" : "ai.retry.kept", { edits: resume.edits });
    }
    return i18n.t(resume.edits === 1 ? "ai.retry.noteOne" : "ai.retry.note", { edits: resume.edits });
  };

  /** Where a Continue picks the conversation up. */
  const continueNote = (resume: StoppedTurn): string => resume.stop
    ? i18n.t("ai.continue.note", { done: resume.stop.done, planned: resume.stop.planned })
    : i18n.t("ai.continue.noteSaved");

  /** A preview command the backend refused keeps its explanation in the panel. */
  const previewFailed = (key: I18nKey) => {
    setMessages((prev) => [...prev, {
      role: "system",
      content: i18n.t(key, { error: previewStore.error() ?? "" }),
      error: true,
      timestamp: Date.now(),
    }]);
  };
  /** The path the open preview is about. */
  const previewPath = () => previewStore.preview()?.target_path ?? "";

  /** Keep the auditioned side the backend still holds. */
  const handlePreviewAccept = async () => {
    if (applying() || previewStore.busy()) return;
    setApplying(true);
    transcriptFollow?.resume();
    const accepted = await previewStore.accept();
    setApplying(false);
    if (!accepted) {
      previewFailed("ai.preview.acceptFailed");
      return;
    }
    setMessages((prev) => [...prev, { role: "system", content: i18n.t("ai.edit.applied"), timestamp: Date.now() }]);
    void flushQueued();
  };

  /** Put the base document back. */
  const handlePreviewDecline = async () => {
    if (applying() || previewStore.busy()) return;
    setApplying(true);
    transcriptFollow?.resume();
    const declined = await previewStore.decline();
    setApplying(false);
    if (!declined) {
      previewFailed("ai.preview.declineFailed");
      return;
    }
    setMessages((prev) => [...prev, { role: "system", content: i18n.t("ai.edit.rejected"), timestamp: Date.now() }]);
    void flushQueued();
  };

  // Quick actions: the chip label and the message it sends are separate keys,
  // because the action becomes the user's own row in the transcript.
  const quickActions: { label: I18nKey; action: I18nKey }[] = [
    { label: "ai.quick.describe", action: "ai.quick.describePrompt" },
    { label: "ai.quick.reverb", action: "ai.quick.reverbPrompt" },
    { label: "ai.quick.drums", action: "ai.quick.drumsPrompt" },
    { label: "ai.quick.mix", action: "ai.quick.mixPrompt" },
  ];

  /** `null` is the whole surface (an older backend); `[]` is a real answer. */
  const CHIP_LIMIT = 4;
  const enabledSets = createMemo(() => {
    const selection = selectedSets();
    if (selection === null) return null;
    const ids = new Set(selection);
    return toolsets().filter(set => ids.has(set.id));
  });
  const visibleSets = createMemo(() => enabledSets()?.slice(0, CHIP_LIMIT) ?? []);
  const hiddenSets = createMemo(() => enabledSets()?.slice(CHIP_LIMIT) ?? []);

  const renderMessage = (msg: ChatMessage) => (
            <div
              class="ai-message"
              classList={{ [msg.role]: true, checkpoint: !!msg.checkpoint, audition: msg.context === "audition" }}
              onContextMenu={msg.role === "context" ? undefined : (event) => {
                event.preventDefault();
                const busy = loading() || !!reattached();
                const items = [
                  ...(msg.role === "assistant" && !msg.checkpoint && endsTurn(msg) ? [{
                    label: i18n.t("ai.fork.here"),
                    disabled: busy || originalView() || sessionBusy(),
                    hint: busy
                      ? i18n.t("ai.revert.turnRunning")
                      : originalView()
                        ? i18n.t("ai.revert.originalView")
                        : i18n.t("ai.fork.hint"),
                    onSelect: () => void forkFromTurn(msg),
                  }] : []),
                  // Only a person's message is a point to revert to: the backend
                  // restores the project it was sent against and cuts there.
                  ...(msg.role === "user" ? [{
                    label: i18n.t("ai.revert.before"),
                    // A running turn keeps writing into the history this
                    // rewrites, so the item is withheld until it settles.
                    disabled: messages().indexOf(msg) === 0 || busy || originalView(),
                    hint: busy
                      ? i18n.t("ai.revert.turnRunning")
                      : originalView()
                        ? i18n.t("ai.revert.originalView")
                        : messages().indexOf(msg) === 0
                          ? i18n.t("ai.revert.beforeFirst")
                          : i18n.t("ai.revert.actionHint"),
                    onSelect: () => void revertToBefore(msg),
                  }] : []),
                ];
                if (items.length) openContextMenu({ x: event.clientX, y: event.clientY, items });
              }}
            >
              {/* The user's row is a chat block on the right; its position says
                  who wrote it, so it carries no role cell. Host context says so
                  on its own caption instead. */}
              <Show when={!msg.reasoning && msg.role !== "user" && msg.role !== "context"}><div class="ai-message-role">{i18n.t(`ai.role.${msg.role}`)}</div></Show>
              <Show when={msg.reasoning}>
                <AiThinking loading={!!msg.liveReasoning && loading()} partial="" approval={false} lastTool={null} identity="" reasoning={msg.reasoning} />
              </Show>
              <Show when={msg.role === "tool"} fallback={
                <Show when={msg.context === "audition"} fallback={
                  <Show when={msg.checkpoint} fallback={<div class="ai-message-content"><Show when={msg.role === "assistant"} fallback={legacyAuditionText(msg.content, i18n.t("ai.media.legacyAudition"))}><AiMarkdown content={msg.content} /></Show></div>}>
                    <LazyDetails
                      class="ai-checkpoint"
                      open={false}
                      summary={i18n.t("ai.sessions.summary")}
                    >
                      <div class="ai-message-content"><Show when={msg.role === "assistant"} fallback={legacyAuditionText(msg.content, i18n.t("ai.media.legacyAudition"))}><AiMarkdown content={msg.content} /></Show></div>
                    </LazyDetails>
                  </Show>
                }>
                  <div class="ai-context-caption">
                    <span class="ai-context-label">{i18n.t("ai.context.audition")}</span>
                    <span>{legacyAuditionText(msg.content, i18n.t("ai.media.legacyAudition"))}</span>
                  </div>
                </Show>
              }>
                {/* A restored tool row: the call's name, what it was asked and what came back. */}
                <Show when={msg.tool} fallback={
                  <LazyDetails
                    class="ai-tool-result"
                    classList={{ error: msg.error }}
                    summary={<>{msg.content.split(":", 1)[0]} {i18n.t(msg.error ? "ai.activity.failed" : "ai.activity.result")}</>}
                  >
                    <AiToolContent content={msg.content} />
                  </LazyDetails>
                }>
                  <LazyDetails
                    class="ai-tool-result"
                    classList={{ error: msg.error }}
                    open={autoOpenResult(msg.tool!.name)}
                    summary={<>
                      <span class="ai-tool-icon"><ToolIcon name={msg.tool!.name} /></span>
                      <span class="ai-tool-name">{activitySummary(msg.tool!, msg.content)}</span>
                      <Show when={msg.error}><span class="ai-tool-error-label"> · {i18n.t("ai.activity.failed")}</span></Show>
                    </>}
                  >
                    <div class="ai-tool-detail">
                      <div class="ai-tool-detail-head">
                        <code class="ai-tool-detail-name">{msg.tool!.name}</code>
                        <Show when={msg.tool!.call_id}>
                          <span class="ai-tool-call-id" title={msg.tool!.call_id}>{msg.tool!.call_id}</span>
                        </Show>
                      </div>
                      <Show when={msg.tool!.args}><div>{i18n.t("ai.activity.arguments")}</div><AiToolArguments content={msg.tool!.args!} /></Show>
                      <div>{i18n.t("ai.activity.result")}</div><AiToolResult tool={msg.tool!.name} content={msg.content} args={msg.tool!.args} sessionId={openId() ?? undefined} />
                    </div>
                  </LazyDetails>
                </Show>
              </Show>
              <Show when={msg.warning}>
                <div class="ai-session-warning" role="status">{msg.warning}</div>
              </Show>
              <Show when={msg.auditionRenderUnavailable}>
                <div class="ai-session-warning" role="status">{i18n.t(
                  msg.context === "audition" && msg.media?.some(attachment => !attachment.missing && auditionLabel(attachment, msg.content, msg.messageMeta))
                    ? "ai.media.auditionRenderUnavailable"
                    : "ai.media.auditionRenderMissing",
                )}</div>
              </Show>
              <Show when={msg.media?.length}>
                <div class="ai-message-media">
                  <For each={msg.media}>
                    {(attachment) => {
                      const label = () => auditionLabel(attachment, msg.content, msg.messageMeta);
                      return (
                        <>
                          <span class="ai-media-tag" title={attachment.path} classList={{ missing: !!attachment.missing }}>
                            {label() ? i18n.t(label()!.key, label()!.params) : attachment.name} · {attachment.missing
                              ? i18n.t("ai.media.missingState")
                              : <>{attachment.local_reference ? i18n.t("ai.media.localReference") : attachment.mime} · {formatDuration(attachment.duration_secs) ?? i18n.t("ai.media.unknownDuration")} · {formatBytes(attachment.bytes)}</>}
                          </span>
                          <Show when={msg.context === "audition" && label() && !attachment.missing}>
                            <InlineAudioPreview label={i18n.t("ai.media.loadAudition")}
                              resetKey={attachment.path} load={() => previewSavedAudition(attachment.path)} />
                          </Show>
                        </>
                      );
                    }}
                  </For>
                </div>
              </Show>
              <Show when={msg.auditionRenderPath}>
                {(path) => <InlineAudioPreview label={i18n.t("ai.media.loadAuditionRender")}
                  resetKey={path()} load={() => openSavedAuditionRender(path())} />}
              </Show>

              {/* Diff viewer */}
              <Show when={msg.diff}>
                <div class="ai-diff">
                  <For each={msg.diff!.hunks}>
                    {(hunk) => (
                      <For each={hunk.lines}>
                        {(line) => (
                          <div
                            class="ai-diff-line"
                            classList={{
                              added: line.kind === "Added",
                              removed: line.kind === "Removed",
                            }}
                          >
                            {line.kind === "Added" ? "+ " : line.kind === "Removed" ? "- " : "  "}
                            {line.text}
                          </div>
                        )}
                      </For>
                    )}
                  </For>
                </div>
              </Show>

              {/* A stopped turn's row: resend it, or resume it where it stopped.
                  A stop whose rollback failed has neither action: the project is
                  not in a state a retry may start from. */}
              <Show when={msg.resume && (canRetry(msg.resume!) || canContinue(msg.resume!))}>
                <div class="ai-turn-actions">
                  <Show when={canRetry(msg.resume!)}>
                    <button
                      type="button"
                      class="ai-turn-retry"
                      disabled={loading() || hasUnsavedConfig()}
                      onClick={() => void resumeTurn(msg, "retry")}
                    >{i18n.t("ai.retry.action")}</button>
                  </Show>
                  <Show when={canContinue(msg.resume!)}>
                    <button
                      type="button"
                      class="ai-turn-continue"
                      disabled={loading() || hasUnsavedConfig()}
                      onClick={() => void resumeTurn(msg, "continue")}
                    >{i18n.t("ai.continue.action")}</button>
                  </Show>
                  <Show when={canRetry(msg.resume!) && msg.resume!.edits > 0}>
                    <span class="ai-turn-note">{retryNote(msg.resume!)}</span>
                  </Show>
                  <Show when={canContinue(msg.resume!)}>
                    <span class="ai-turn-note">{continueNote(msg.resume!)}</span>
                  </Show>
                </div>
              </Show>
            </div>
  );


  return (
    <div class="ai-panel">
      {/* Header: where the conversation lives and what it is about, then the
          settings and close controls. The model lives under the composer, so
          the transcript starts one row higher. The workspace name is the
          identity; the full path is one hover away, so a narrow panel never
          shows a clipped path as the label. */}
      <div class="ai-panel-header">
        <span class="ai-panel-title">AI</span>
        <div class="ai-header-identity">
          <button
            type="button"
            class="ai-header-scope"
            aria-label={i18n.t("workspace.switch")}
            onClick={() => workspaceStore.openChooser()}
            {...useTooltip(() => workspacePath() ?? i18n.t("ai.sessions.detached"))}
          >
            <span class="ai-header-scope-name">{workspaceName()}</span>
          </button>
          <span class="ai-header-title" title={openTitle()}>{openTitle()}</span>
        </div>
        <button
          class="toolbar-btn text-btn"
          aria-label={i18n.t("ai.settings.open")}
          onClick={(event) => openSettings("connection", event.currentTarget)}
          {...useTooltip(() => i18n.t("ai.settings.open"))}
          style={{ "font-size": "10px", height: "22px", padding: "0 6px" }}
        >{i18n.t("ai.settings.button")}</button>
        <Show when={props.onClose}>
          <button
            class="toolbar-btn"
            aria-label={i18n.t("ai.header.close")}
            onClick={() => props.onClose?.()}
            {...useTooltip(() => i18n.t("ai.header.close"))}
            style={{ "font-size": "12px", width: "22px", height: "22px" }}
          >{"×"}</button>
        </Show>
      </div>

      <SessionSwitcher
        title={openTitle()}
        activeSessionId={openId()}
        sessions={sessions()}
        workspacePath={workspacePath()}
        open={sessionsOpen()}
        busy={loading() || sessionBusy()}
        exportOpen={exportOpen()}
        exportDisabled={sessionBusy() || !openId()}
        permission={permission()}
        renaming={renaming()}
        deleting={deleting()}
        menu={sessionMenu()}
        error={sessionError()}
        onToggle={() => setSessionsOpen(open => !open)}
        onNew={startConversation}
        onToggleExport={() => setExportOpen(open => !open)}
        onPermissionChange={choosePermission}
        onChooseWorkspace={() => workspaceStore.openChooser()}
        onSwitch={switchSession}
        onShowMenu={setSessionMenu}
        onRename={beginRename}
        onRenameDraft={setRenaming}
        onCommitRename={commitRename}
        onConfirmDelete={setDeleting}
        onDelete={deleteSession}
        onFork={forkSession}
        onCompact={compactSession}
        onOriginal={paintOriginal}
      />

      {/* The transcript as a document: copy it, or download it named after the conversation. */}
      <Show when={exportOpen()}>
        <div class="ai-export-menu" role="group" aria-label={i18n.t("ai.export.action")}>
          <For each={EXPORT_FORMATS}>
            {(entry) => (
              <div class="ai-export-row">
                <span class="ai-export-format">{i18n.t(entry.label)}</span>
                <button
                  type="button"
                  class={`ai-export-copy ai-export-copy-${entry.format}`}
                  disabled={exportBusy()}
                  onClick={() => void copyExport(entry.format)}
                >{i18n.t("ai.export.copy")}</button>
                <button
                  type="button"
                  class={`ai-export-download ai-export-download-${entry.format}`}
                  disabled={exportBusy()}
                  onClick={() => void downloadExport(entry.format)}
                >{i18n.t("ai.export.download")}</button>
              </div>
            )}
          </For>
          <Show when={exportNote()}><div class="ai-export-note selectable">{exportNote()}</div></Show>
        </div>
      </Show>

      <div class="ai-provider-summary">
        <Show
          when={hasProvider()}
          fallback={
            <button
              type="button"
              class="ai-provider-summary-text ai-provider-summary-empty"
              onClick={(event) => openSettings("connection", event.currentTarget)}
            >
              {i18n.t("ai.provider.none")}
            </button>
          }
        >
          <span class="ai-provider-summary-text">
            {i18n.t("ai.provider.summary", {
              count: aiConfig.config.providers.length,
              name: getActiveProvider().name,
            })}
          </span>
        </Show>
      </div>

      {/* Informational: a saved model the catalog has no metadata for. One line,
          clickable to the provider whose model can be picked. */}
      <Show when={aiConfig.warnings().length}>
        <div class="ai-catalog-warning">
          <button
            type="button"
            class="ai-catalog-warning-text"
            title={aiConfig.warnings().map((warning) => warning.message).join("\n")}
            onClick={openUndescribedProvider}
          >
            {aiConfig.warnings().length === 1
              ? i18n.t("ai.model.undescribed", { count: 1 })
              : i18n.t("ai.model.undescribedPlural", { count: aiConfig.warnings().length })}
          </button>
          <button
            type="button"
            class="ai-catalog-warning-dismiss"
            aria-label={i18n.t("ai.model.dismissWarning")}
            onClick={() => aiConfig.dismissWarnings()}
          >{"×"}</button>
        </div>
      </Show>

      {/* Thumbnail of the enabled sets; the row opens the modal. */}
      {/* A div, not a button: the chips inside carry their own hover and focus
          handlers, and WebKit does not reliably deliver those through a button. */}
      <div
        role="button"
        tabindex="0"
        class="ai-toolsets-strip"
        aria-label={i18n.t("ai.toolsets.open")}
        onClick={(event) => openSettings("toolsets", event.currentTarget)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          openSettings("toolsets", event.currentTarget as HTMLElement);
        }}
      >
        <Show when={enabledSets()} fallback={
          <span class="ai-toolset-chip" {...useTooltip(() => i18n.t("ai.toolsets.allHint"))}>
            {i18n.t("ai.toolsets.all")}
          </span>
        }>
          <Show when={enabledSets()!.length} fallback={
            <span class="ai-toolset-chip ai-toolset-chip-empty" {...useTooltip(() => i18n.t("ai.toolsets.noneHint"))}>
              {i18n.t("ai.toolsets.none")}
            </span>
          }>
            <For each={visibleSets()}>
              {(set) => (
                <span
                  class="ai-toolset-chip"
                  {...useTooltip(() => i18n.t("ai.toolsets.chipHint", { title: set.title, description: set.description }))}
                >{set.id}</span>
              )}
            </For>
            <Show when={hiddenSets().length}>
              <span class="ai-toolset-chip ai-toolset-chip-more" {...useTooltip(() => ({
                title: i18n.t("ai.toolsets.hiddenTitle", { count: hiddenSets().length }),
                rows: hiddenSets().map(set => ({ label: set.id, value: set.description })),
              }))}>+{hiddenSets().length}</span>
            </Show>
          </Show>
        </Show>
      </div>

      {/* Opened from the settings dialog's connection column: one dialog, wide
          enough for a per-model table, stacked above the settings overlay. */}
      {/* Quick actions (show when no messages) */}
      <Show when={messages().length === 0}>
        <div class="ai-quick-actions">
          <For each={quickActions}>
            {(qa) => (
              <button
                class="ai-quick-btn"
                onClick={() => { setInput(i18n.t(qa.action)); handleSend(); }}
              >
                {i18n.t(qa.label)}
              </button>
            )}
          </For>
        </div>
      </Show>

      {/* Chat messages */}
      <div class="ai-messages selectable" ref={transcriptElement}>
        <Show when={originalView()}>
          <div class="ai-original-banner" role="status">
            <span>{i18n.t("ai.sessions.originalBanner")}</span>
            <button
              type="button"
              class="ai-original-back"
              onClick={() => {
                setOriginalView(false);
                if (openId()) void paintSession(openId()!);
              }}
            >{i18n.t("ai.sessions.originalBack")}</button>
          </div>
        </Show>
        <Show when={hiddenMessages() > 0 || (compactedSession() && !originalView())}>
          <button
            type="button"
            class="ai-transcript-earlier"
            classList={{ visible: atTranscriptTop() }}
            aria-hidden={!atTranscriptTop()}
            tabIndex={atTranscriptTop() ? 0 : -1}
            onClick={() => {
              if (hiddenMessages() > 0) {
                setRenderLimit((limit) => limit + TRANSCRIPT_RENDER_LIMIT);
              } else if (openId()) {
                void paintOriginal(openId()!);
              }
            }}
          >{hiddenMessages() > 0
            ? i18n.t("ai.transcript.earlier", { count: hiddenMessages() })
            : i18n.t("ai.transcript.earlierHistory")}</button>
        </Show>
        <Index each={transcriptEntries()}>
          {(entry) => (
            <Show
              when={entry().kind === "step"}
              fallback={renderMessage((entry() as TranscriptMessage).message)}
            >
              <AiStepGroup
                rows={(entry() as TranscriptStep).rows}
                start={(entry() as TranscriptStep).start}
                end={(entry() as TranscriptStep).end}
                toolCalls={(entry() as TranscriptStep).toolCalls}
                running={(entry() as TranscriptStep).running}
                latest={(entry() as TranscriptStep).running ? liveActivity() ?? undefined : undefined}
                now={() => clock()}
                render={renderMessage}
              />
            </Show>
          )}
        </Index>

        <Show when={loading() && partial()}>
          <div class="ai-message assistant ai-streaming">
            <div class="ai-message-content selectable">
              <AiMarkdown content={partial()} />
              <span class="ai-thinking-caret" aria-hidden="true" />
            </div>
          </div>
        </Show>

        {/* The turn in flight is the transcript's last row: one content cell,
            because the dots already say who is working. It scrolls with the
            conversation instead of sitting in a strip above the composer. */}
        <Show when={loading()}>
          <div class="ai-message ai-pending">
            <div class="ai-message-content">
              <AiThinking
                loading
                partial=""
                streaming={!!partial()}
                approval={!!approval()}
                lastTool={currentTool()}
                identity={modelLabel()}
                label={phaseText()}
                preparingTool={toolPreparation()}
              />
            </div>
          </div>
        </Show>
      </div>

      {/* One DSL-modifying call waits here: the turn is paused, not cancelled. */}
      <Show when={approval()}>
        <div class="ai-approval-card" role="alert" aria-label={i18n.t("ai.approval.title")}>
          <div class="ai-approval-title">{i18n.t("ai.approval.title")}</div>
          <div class="ai-approval-call selectable">{approval()!.tool}</div>
          <div class="ai-approval-target">
            {i18n.t("ai.approval.target", { target: approval()!.target ?? i18n.t("ai.approval.unknownTarget") })}
          </div>
          <div class="ai-approval-arguments">
            {i18n.t("ai.approval.arguments")}
            <code class="selectable">{argumentLine(approval()!.args)}</code>
          </div>
          <div class="ai-approval-effect">{i18n.t(approvalEffect(approval()!.tool))}</div>
          <div class="ai-approval-actions">
            <button class="ai-approval-approve" onClick={() => answerApproval(true)}>{i18n.t("ai.approval.approve")}</button>
            <button class="ai-approval-always" onClick={() => answerApproval(true, true)}>{i18n.t("ai.approval.always")}</button>
            <button class="ai-approval-reject" onClick={() => answerApproval(false)}>{i18n.t("ai.approval.reject")}</button>
          </div>
        </div>
      </Show>

      <ProposalReviewBar
        applying={applying()}
        previewPath={previewPath()}
        onPreviewAccept={handlePreviewAccept}
        onPreviewDecline={handlePreviewDecline}
      />

      <Show when={authError()}>
        <div class="ai-auth-banner">
          <span class="selectable">{authError()}</span>
          <button
            class="toolbar-btn text-btn"
            onClick={(event) => openSettings("auth", event.currentTarget)}
            disabled={hasUnsavedConfig()}
          >{i18n.t("ai.auth.reauth")}</button>
        </div>
      </Show>
      {/* Attachments */}
      <Show when={attachments().length || mediaError() || mediaNotice()}>
        <div class="ai-attachments">
          <For each={attachments()}>
            {(attachment) => (
              <div class="ai-attachment-chip" data-media-path={attachment.path} classList={{ missing: !!attachment.missing }}>
                <span class="ai-attachment-name" title={attachment.path}>{attachment.name}</span>
                <span class="ai-attachment-meta">
                  {attachment.missing
                    ? i18n.t("ai.media.missingState")
                    : <>{usesLocalReference(attachment) ? i18n.t("ai.media.localReference") : attachment.mime} · {formatDuration(attachment.duration_secs) ?? i18n.t("ai.media.unknownDuration")} · {formatBytes(attachment.bytes)}</>}
                </span>
                <button
                  class="ai-attachment-remove"
                  aria-label={i18n.t("ai.media.remove", { name: attachment.name })}
                  onClick={() => removeAttachment(attachment.path)}
                >{"×"}</button>
              </div>
            )}
          </For>
        </div>
        <Show when={hasImage() && !canTakeImage()}>
          <div class="ai-media-warning" role="status">
            {i18n.t("ai.media.imageUnsupported", {
              model: currentModel()?.name ?? getActiveProvider().model,
              modalities: acceptedModalities(effectiveLimits(activeLimits())?.input),
            })}
          </div>
        </Show>
        <Show when={mediaError()}><div class="ai-media-error selectable">{mediaError()}</div></Show>
        <Show when={mediaNotice()}><div class="ai-media-notice selectable">{mediaNotice()}</div></Show>
      </Show>
      {/* A message composed while the turn ran waits here, above the composer.
          The oldest one is the next to go out; Steer sends it now, and a panel
          that came back with nothing running offers Send now instead. */}
      <Show when={aiSession.queued().length}>
        <div class="ai-queue-list" role="group" aria-label={i18n.t("ai.queue.label")}>
          <For each={aiSession.queued()}>
            {(item, index) => (
              <div class="ai-queue-row">
                <div class="ai-queue-main">
                  <Show when={queuedEditing() === item} fallback={
                    <span class="ai-queue-text">{item.text}</span>
                  }>
                    <input
                      class="ai-queue-field"
                      aria-label={i18n.t("ai.queue.label")}
                      value={queuedDraft()}
                      onInput={(e) => setQueuedDraft(e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); commitQueuedEdit(); }
                        else if (e.key === "Escape") { e.preventDefault(); setQueuedEditing(null); }
                      }}
                      onBlur={() => commitQueuedEdit()}
                      ref={(element) => queueMicrotask(() => element.focus())}
                    />
                  </Show>
                  <Show when={item.media.length}>
                    <span class="ai-queue-media">{item.media.map((attachment) => attachment.name).join(", ")}</span>
                  </Show>
                </div>
                <div class="ai-queue-actions">
                  <button
                    type="button"
                    class="ai-queue-edit"
                    onClick={() => beginQueuedEdit(item)}
                  >{i18n.t("ai.queue.edit")}</button>
                  <button
                    type="button"
                    class="ai-queue-remove"
                    onClick={() => removeQueued(item)}
                  >{i18n.t("ai.queue.remove")}</button>
                  <Show when={index() === 0 && (loading() || reattached())}>
                    <button
                      type="button"
                      class="ai-queue-steer"
                      title={i18n.t("ai.queue.steerHint")}
                      aria-label={i18n.t("ai.queue.steerHint")}
                      onClick={() => void steerQueued()}
                    >{i18n.t("ai.queue.steer")}</button>
                  </Show>
                  <Show when={index() === 0 && !loading() && !reattached()}>
                    <button
                      type="button"
                      class="ai-queue-send"
                      title={i18n.t("ai.queue.sendNowHint")}
                      aria-label={i18n.t("ai.queue.sendNowHint")}
                      onClick={() => void flushQueued()}
                    >{i18n.t("ai.queue.sendNow")}</button>
                  </Show>
                </div>
              </div>
            )}
          </For>
          <span class="ai-queue-hint">{queueHint()}</span>
        </div>
      </Show>

      {/* The turn this window inherited from a reload. It is on screen only
          because the backend reports it: the pending row needs a loop this
          window does not have. */}
      <Show when={reattached()}>
        <div class="ai-turn-orphan" role="status">
          <span class="ai-turn-orphan-text">{i18n.t("ai.reload.running")}</span>
          <button
            type="button"
            class="ai-turn-orphan-stop"
            onClick={() => void stopInheritedTurn()}
          >{i18n.t("ai.reload.stop")}</button>
        </div>
      </Show>
      {/* The marker a reload left behind and the backend does not back: nothing
          is running, so nothing is painted as running. */}
      <Show when={reloadNotice()}>
        <div class="ai-turn-reloaded" role="status">
          <span class="ai-turn-reloaded-text">{i18n.t("ai.reload.lost")}</span>
          <button
            type="button"
            class="ai-turn-reloaded-open"
            onClick={() => void reloadTranscript()}
          >{i18n.t("ai.reload.reload")}</button>
          <button
            type="button"
            class="ai-turn-reloaded-dismiss"
            onClick={() => setReloadNotice(null)}
          >{i18n.t("ai.reload.dismiss")}</button>
        </div>
      </Show>

      <AiComposer
        blockedNotice={blockedNotice()}
        goal={aiSession.goal()}
        onClearGoal={() => aiSession.setGoal("")}
        commands={commandMatches()}
        commandIndex={commandIndex()}
        onCommandHover={setCommandIndex}
        onCommand={runCommand}
        references={referenceMatches()}
        referenceIndex={Math.min(referenceIndex(), Math.max(referenceMatches().length - 1, 0))}
        onReferenceHover={setReferenceIndex}
        onReference={chooseReference}
        scope={activeScope()}
        onClearScope={clearScope}
        attachHint={attachHint()}
        attachDisabled={loading() || previewStore.open()}
        onAttach={attachFile}
        loading={loading()}
        input={input()}
        inputDisabled={!!approval() || previewStore.open()}
        inputRef={(element) => { inputElement = element; queueMicrotask(() => growInput(element)); }}
        onInput={(element) => { setInput(element.value); growInput(element); }}
        onInputKey={onComposerKey}
        sendAction={sendAction()}
        sendLabel={sendLabel()}
        sendTitle={sendTitle()}
        sendDisabled={!!sendBlocked()}
        onSend={handleSend}
        onCancel={cancel}
      />

      {/* The model and its effort level are the composer's footer: the last row
          of the panel, where a change applies to the next message. One line at
          the narrowest panel width; the value truncates. */}
      <div class="ai-model-bar">
        <Dropdown
          label={i18n.t("ai.header.model")}
          value={modelKey(activeModelGroup(catalog()), getActiveProvider().model)}
          options={modelChoices()}
          onSelect={selectModel}
          hideLabel
          class="ai-model-select ai-model-picker"
          triggerAttrs={{ "data-model-known": activeModelKnown(catalog()) ? "true" : "false" }}
          triggerProps={useTooltip(() => modelLabel())}
          badge={modelBadge()}
          disabled={loading()}
        />
        <Show when={thinkingLevels().length}>
          <Dropdown
            label={i18n.t("ai.header.thinking")}
            value={getActiveProvider().thinkingLevel ?? DEFAULT_THINKING_LEVEL}
            options={thinkingLevels().map((level) => ({ value: level, label: level }))}
            onSelect={(level) => {
              aiConfig.updateProvider(getActiveProvider().id, { thinkingLevel: level });
              void aiConfig.flushSave();
            }}
            hideLabel
            class="ai-model-select ai-model-effort"
            disabled={loading()}
          />
        </Show>
      </div>
      <AiStats
        used={contextEstimate()}
        breakdown={contextBreakdown()}
        compactAt={contextMeter.compactAt()}
        onCompactAtChange={setCompactAt}
        onCompact={openId() ? () => { const id = openId(); if (id) void compactSession(id); } : undefined}
      />
      <Show when={!getActiveProvider().model.trim()}>
        <div class="ai-model-notice">
          <small>{i18n.t("ai.model.missing")}</small>
        </div>
      </Show>
      <Show when={aiConfig.saveError()}>
        <div class="ai-save-notice">
          <small>{i18n.t("ai.saveNotice", { message: aiConfig.saveError() ?? "" })}</small>
          <button
            class="toolbar-btn text-btn"
            disabled={savingConfig()}
            onClick={() => {
              setSavingConfig(true);
              void aiConfig.flushSave()
                .then((ok) => { if (!ok) notify(aiConfig.saveError() ?? "", "error"); })
                .finally(() => setSavingConfig(false));
            }}
          >{i18n.t("ai.saveAction")}</button>
        </div>
      </Show>
    </div>
  );
}
