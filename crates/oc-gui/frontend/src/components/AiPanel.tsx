import { composerSizer } from "../ai/composerSize";
import { rememberSessionTitles } from "../stores/sessionTitles";
import { AiComposer } from "./AiComposer";
import { ProposalReviewBar } from "./ProposalReviewBar";
import { followTranscript } from "../ai/transcriptScroll";
import { AiMarkdown } from "./AiMarkdown";
import { ToolIcon } from "./ToolIcon";
import { AiToolContent } from "./AiToolContent";
import { AiToolArguments, AiToolResult } from "./AiToolResult";
import { AiStepGroup } from "./AiStepGroup";
import { AiAudition } from "./AiAudition";
import { LazyDetails } from "./LazyDetails";
import { openContextMenu } from "./ContextMenu";
import { For, Index, Show, createEffect, createSignal, createMemo, on, onMount, onCleanup, untrack } from "solid-js";
import { aiApi } from "../stores/api";
import { store } from "../stores/project";
import { previewStore } from "../stores/preview";
import { log } from "../stores/log";
import type { ApprovalRequest, TurnStop, UserQuestion, WireMessage } from "../ai/wire";
import type { RecallOther, RecallRefusal, TurnEnvelope, TurnOutcome } from "../ai/api";
import { changeSetText } from "../stores/changeText";
import { hasChangeFacts, isChangeSet } from "../types/change";
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
import { MEDIA_EXTENSIONS, attachMedia, audioAccepted, acceptedModalities, formatBytes, formatDuration, legacyAuditionText, imageAccepted, type MediaAttachment, type MediaNotice } from "../ai/media";
import { copyToClipboard, downloadText, exportFileName, exportJson, exportMarkdown } from "../ai/export";
import { errorBindingMismatch, errorKind, errorMessage } from "../ai/errors";
import { followsOpenProject, projectName, sessionBinding, type SessionBinding } from "../ai/binding";
import {
  activeTurn,
  archiveNotice,
  compactionFailureNotice,
  compactionNotice,
  directoryName,
  loadedPage,
  loadedSession,
  sessionArchiveEviction,
  sessionCompaction,
  sessionCompactionFailure,
  sessionList,
  sessionPageRead,
  workspaceScope,
  type ActiveTurn,
  type SessionMeta,
} from "../ai/sessions";
import type { SessionState } from "../ai/messageProtocol";
import {
  canContinue,
  canRetry,
  elapsedLabel,
  restoredTranscript,
  currentSessionState,
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
import { authorName } from "../stores/historyText";
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

/** A backend turn this view did not start: it follows it, or waits for its stop. */
interface InheritedTurn {
  requestId: string;
  session: string;
  /** The workspace epoch it was found in; another epoch is another workspace. */
  epoch: number;
}

/** The two documents the export menu offers, in the order it shows them. */
const EXPORT_FORMATS: { format: ExportFormat; label: I18nKey }[] = [
  { format: "markdown", label: "ai.export.markdown" },
  { format: "json", label: "ai.export.json" },
];

/** The localized line and full tool list a tool-surface notice shows. */
function toolSurfaceRow(surface: NonNullable<ChatMessage["surface"]>): { text: string; names: string } {
  const parts: string[] = [];
  if (surface.added.length) parts.push(i18n.t("ai.toolSurface.added", { count: surface.added.length }));
  if (surface.removed.length) parts.push(i18n.t("ai.toolSurface.removed", { count: surface.removed.length }));
  const text: string[] = [];
  if (parts.length) text.push(i18n.t("ai.toolSurface.changed", { detail: parts.join(", ") }));
  if (surface.permission) {
    text.push(i18n.t("ai.toolSurface.permission", {
      mode: i18n.t(`ai.permission.${surface.permission.to}` as I18nKey),
    }));
  }
  const names = [
    ...surface.added.map(name => `+ ${name}`),
    ...surface.removed.map(name => `- ${name}`),
    ...(surface.permission ? [`${surface.permission.from} -> ${surface.permission.to}`] : []),
  ].join("\n");
  return { text: text.join("; "), names };
}

/** The localized line a saved goal or plan change shows. */
function sessionStateRow(state: SessionState): string {
  if (state.kind === "goal_set") return i18n.t("ai.state.goalSet", { goal: state.text });
  if (state.kind === "goal_cleared") return i18n.t("ai.state.goalCleared");
  return i18n.t("ai.state.planUpdated", { count: state.items.length });
}

/** How often a window that inherited a running turn asks whether it finished. */
const TURN_POLL_MS = 400;
/** How long a stopped inherited turn may take to close before the panel repaints anyway. */
const TURN_SETTLE_MS = 10_000;

export function AiPanel(props: { onClose?: () => void }) {
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  const [sessionStates, setSessionStates] = createSignal<SessionState[]>([]);
  const sessionStateView = createMemo(() => currentSessionState(
    sessionStates().map(state => ({ role: "context" as const, content: "", state })),
  ));
  const rememberState = (state: SessionState): void => {
    const goal = state.kind !== "plan_set";
    setSessionStates(previous => [
      ...previous.filter(saved => (saved.kind !== "plan_set") !== goal), state,
    ]);
  };
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
  /** A question the model asked, in the same slot as an approval card. */
  const [userQuestion, setUserQuestion] = createSignal<(UserQuestion & { requestId: string }) | null>(null);
  const [replyText, setReplyText] = createSignal("");
  /** When the turn in flight started, so the pending row can count seconds. */
  const [turnStartedAt, setTurnStartedAt] = createSignal<number | null>(null);
  /** When Stop was pressed; the pending row says so until the turn settles. */
  const [stopRequestedAt, setStopRequestedAt] = createSignal<number | null>(null);
  /**
   * The turn the backend still runs after this window was reloaded, named by
   * the backend itself. The pending row needs the loop of the window that
   * started the turn, so this turn is reported instead of imitated.
   */
  const [reattached, setReattached] = createSignal<InheritedTurn | null>(null);
  /**
   * The inherited turn a Stop is waiting on. It is not followed, but it
   * still writes the conversation and the project until the backend lets go
   * of it, so the panel stays as guarded as while it followed the turn.
   */
  const [stoppingInherited, setStoppingInherited] = createSignal<InheritedTurn | null>(null);
  /**
   * A marker the backend does not back: nothing runs. `finished` means the
   * saved conversation ends on the turn's answer; `lost` means the panel cannot
   * tell how the turn ended.
   */
  const [reloadNotice, setReloadNotice] = createSignal<"lost" | "finished" | null>(null);
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
  /** The open conversation's binding as last read, so a send that needs no
   * rebind starts without waiting on the backend. */
  const [binding, setBinding] = createSignal<SessionBinding | null>(null);
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
  /** Where the rows on screen begin in the whole saved view. */
  const [pageStart, setPageStart] = createSignal(0);
  /** The view shape the loaded page belongs to; an older page must match it. */
  const [pageRevision, setPageRevision] = createSignal("");
  /** The panel's estimate of the turns above the page on screen. */
  const [olderTokens, setOlderTokens] = createSignal(0);
  /** Empties the screen and forgets the page it showed, for a switch or a new
   * conversation: no older page of the previous one remains to load. */
  const clearTranscript = () => {
    setMessages([]);
    setSessionStates([]);
    setPageStart(0);
    setPageRevision("");
    setOlderTokens(0);
    setRenderLimit(TRANSCRIPT_RENDER_LIMIT);
  };
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
  ], loading() || reattached() !== null || stoppingInherited() !== null));
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
  /** The id of the queued row being edited in place; `null` when none is. */
  const [queuedEditing, setQueuedEditing] = createSignal<string | null>(null);
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
  /** Delivery holds its queue row until the backend confirms it was saved; the
   * mark lives in the store, so a reopened panel sees a steer still in flight. */
  const steering = () => aiSession.steering() !== null;
  /** Steer marks this panel cleared without a steer reply; their end sends nothing. */
  const quietSteerEnds = new Set<string>();
  /**
   * Anything that would change the open conversation waits while a turn,
   * inherited or not, still writes it, or while a session action such as a
   * revert rewrites it.
   */
  const conversationLocked = () =>
    loading() || reattached() !== null || stoppingInherited() !== null || sessionBusy();
  /** The id of the turn this panel runs; `ai_cancel` and the events name it. */
  let requestId: string | null = null;
  const cancel = async () => {
    stoppedTokens.add(turnToken);
    // The pending row says the stop is on its way instead of leaving the turn
    // looking healthy; a second press re-sends the cancel.
    setStopRequestedAt((previous) => previous ?? Date.now());
    // The backend drops the question when its turn stops.
    setApproval(null);
    setUserQuestion(null);
    // This window owned the turn, so the marker it kept is spent: a window that
    // comes back must not ask about a turn this one stopped.
    if (requestId) {
      aiSession.setTurn(null);
      await aiApi.cancel(requestId).catch(() => {});
    }
  };
  /** Shows a question a turn asked; the card answers it by call id. */
  const showQuestion = (turn: string, request: ApprovalRequest) => {
    setUserQuestion(null);
    setApproval({
      ...request,
      requestId: turn,
      // The path the call names is the target when the registry has no better
      // name for it, so a subtree replacement says which subtree it touches.
      target: approvalTarget(request.tool, request.args, store.project) ?? request.path ?? null,
    });
  };
  /** Shows a question the model asked; the card replies by call id. */
  const showUserQuestion = (turn: string, request: UserQuestion) => {
    setApproval(null);
    setReplyText("");
    setUserQuestion({ ...request, requestId: turn });
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
  /** Sends the question card's answer; the backend's turn goes on with it. */
  const answerQuestion = (option?: number) => {
    const request = userQuestion();
    setUserQuestion(null);
    if (!request) return;
    const text = replyText().trim();
    setReplyText("");
    void aiApi.replyTurn(request.requestId, request.call_id, {
      ...(option !== undefined ? { option } : {}),
      ...(text ? { text } : {}),
    }).catch((error) => log.warn("ai", `answer not delivered: ${errorMessage(error)}`));
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
    // The backend owns the turn; closing this view only detaches its listeners.
    // Keep its marker so a reopened panel can recover progress and approvals.
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
  const estimateBaseTokens = createMemo(() => textTokens(estimateBase()) + olderTokens());
  /** The estimate for a draft: the floor the composer shows, and the send guard. */
  const estimateFor = (draft: string) => estimateFromBase(estimateBaseTokens(), draft);
  /** The same estimate split into the parts a compaction would remove or keep. */
  const contextBreakdown = createMemo<ContextBreakdown>(() => {
    const ratio = contextMeter.ratio();
    const history = messages().map(message => message.content).join("\n");
    return {
      system: Math.round((contextMeter.systemChars() / 4) * ratio),
      tools: Math.round((contextMeter.toolChars() / 4) * ratio),
      history: Math.round((textTokens(history) + olderTokens()) * ratio),
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
    // A late message of the last turn belongs to the conversation it ran in.
    finishedTurn = null;
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
      rememberSessionTitles(next);
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
    // A turn this view inherited belongs to the workspace it was found in: the
    // panel stops following it, and neither its card nor its settling follows
    // the panel here. The backend keeps running it.
    stopFollowing();
    setStoppingInherited(null);
    setApproval(null);
    setReloadNotice(null);
    finishedTurn = null;
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
    clearTranscript();
    const epoch = scopeEpoch;
    void refreshSessions().then((ok) => {
      if (ok) void restoreScope(sessionScope(), epoch);
    });
  }, { defer: true }));
  // Another document in the same directory is another project: an edit target
  // such as `track:kick` names a track in the file that was open before it.
  createEffect(on(() => store.filePath(), () => {
    aiWorkspace.setTargetPath("project");
    void followOpenProject(openId());
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
  /**
   * Rebinds a conversation to the project that is open. Opening another file
   * moves the open conversation with it, and a conversation opened from the
   * list follows at its next send; the backend compares the paths and tells the
   * model with the next message. A failure is reported and leaves the binding,
   * so the backend still refuses a turn on the wrong file.
   */
  const followOpenProject = async (id: string | null): Promise<void> => {
    if (!id) return;
    try {
      const status = sessionBinding(await aiApi.sessionBinding(id));
      if (followsOpenProject(status)) {
        await aiApi.bindSession(id);
        if (!disposed && openId() === id) setBinding({ bound: status!.open, open: status!.open, matches: true });
      } else if (!disposed && openId() === id) {
        setBinding(status);
      }
    } catch (e) {
      log.warn("ai", `conversation not bound to the open project: ${errorMessage(e)}`);
    }
  };

  /** Reads where a conversation just painted is bound, without rebinding it:
   * viewing a conversation leaves it on its file until the person sends. */
  const readBinding = async (id: string): Promise<void> => {
    try {
      const status = sessionBinding(await aiApi.sessionBinding(id));
      if (!disposed && openId() === id) setBinding(status);
    } catch {
      if (!disposed && openId() === id) setBinding(null);
    }
  };

  /** Paints a saved conversation. The draft is the caller's business: a switch
   * clears it, a remount keeps it. */
  const paintSession = async (id: string): Promise<boolean> => {
    const epoch = scopeEpoch;
    setBinding(null);
    const request = ++paintRequest;
    const usageRead = beginUsageHydration(id);
    pendingUsageRead = usageRead;
    try {
      let loaded = loadedPage(await aiApi.openSession(id));
      if (!loaded || disposed || epoch !== scopeEpoch || request !== paintRequest) return false;
      if (usageHydrationNeedsReconcile(usageRead, loaded.usage)) {
        loaded = loadedPage(await aiApi.openSession(id));
        if (!loaded || disposed || epoch !== scopeEpoch || request !== paintRequest) return false;
      }
      const restored = restoredTranscript(loaded.messages, loaded.start);
      setMessages(restored);
      setSessionStates(loaded.state ?? restored.flatMap(row => row.state ? [row.state] : []));
      setPageStart(loaded.start);
      setPageRevision(loaded.revision);
      setOlderTokens(loaded.older_tokens);
      setRenderLimit(TRANSCRIPT_RENDER_LIMIT);
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
      // A goal saved by the panel before goals were session state is written
      // once as a typed row, then dropped from the window's own store.
      const storedGoal = aiSession.goal();
      if (storedGoal) {
        const hasSavedGoal = sessionStates().some(state => state.kind !== "plan_set");
        try {
          if (!hasSavedGoal) {
            await aiApi.setSessionGoal(loaded.meta.id, storedGoal);
            if (disposed || epoch !== scopeEpoch || request !== paintRequest) return false;
            setMessages(previous => [...previous, {
              role: "context",
              content: "",
              state: { kind: "goal_set", text: storedGoal, author: "person" },
              timestamp: Date.now(),
            }]);
            rememberState({ kind: "goal_set", text: storedGoal, author: "person" });
          }
          aiSession.setGoal("");
        } catch {
          // A failed migration keeps the local value for the next paint.
        }
      }
      void readBinding(loaded.meta.id);
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
   * The `/` palette: local conversation commands. `/compact` and `/clear` ask
   * before they reshape history; `/goal` stages text in the composer.
   */
  const COMMANDS = [
    { name: "compact", summary: "ai.command.compact" },
    { name: "clear", summary: "ai.command.clear" },
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

  /** Shows a state change at once; the saved row paints it again on reload. */
  const applySessionState = (state: SessionState): void => {
    rememberState(state);
    setMessages(previous => [...previous, { role: "context", content: "", state, timestamp: Date.now() }]);
  };

  /** Saves a person's goal change, or stages it until a conversation exists. */
  const saveGoal = async (text: string | null): Promise<boolean> => {
    const id = openId();
    const state: SessionState = text === null
      ? { kind: "goal_cleared", author: "person" }
      : { kind: "goal_set", text, author: "person" };
    if (!id) {
      // A conversation starts with its first message; the staged goal is
      // written once that conversation exists, before its first turn.
      aiSession.setGoal(text ?? "");
      applySessionState(state);
      return true;
    }
    try {
      await aiApi.setSessionGoal(id, text);
      aiSession.setGoal("");
      applySessionState(state);
      return true;
    } catch (error) {
      notify(i18n.t("ai.goal.failed", { error: errorMessage(error) }), "error");
      return false;
    }
  };

  const clearGoal = async (): Promise<void> => {
    await saveGoal(null);
  };

  const runCommand = async (name: string): Promise<void> => {
    const draft = input().trim();
    if (name === "goal") {
      // The goal is one typed session row, so a rewind returns to the earlier
      // goal and a fork carries the one it copied.
      const text = draft.slice(1).replace(/^\s*goal\s*/, "").trim();
      if (!text) {
        setInput("/goal ");
        return;
      }
      setInput("");
      if (await saveGoal(text)) {
        notify(i18n.t("ai.command.goalSet", { goal: text }), "success");
      }
      return;
    }
    if (name === "compact") {
      const id = openId();
      if (!id) return;
      setInput("");
      await compactSession(id);
      return;
    }
    if (name === "clear") {
      const id = openId();
      if (!id) return;
      setInput("");
      const { ask } = await import("@tauri-apps/plugin-dialog");
      const confirmed = await ask(
        i18n.t("ai.command.clearConfirm"),
        { title: i18n.t("ai.command.clear"), kind: "warning" },
      );
      if (!confirmed) return;
      try {
        const result = await aiApi.clearSessionHistory(id);
        notify(
          i18n.t("ai.command.cleared", {
            records: result.dropped_records,
            size: formatBytes(result.freed_bytes),
          }),
          "success",
        );
        setOriginalView(false);
        await paintSession(id);
      } catch (error) {
        notify(errorMessage(error), "error");
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

  /** The sentence a confirmation shows, built from the grouped authors. */
  const recallOtherText = (other: RecallOther): string =>
    other.count === 1
      ? i18n.t("ai.recall.otherEdit", { author: authorName(other.author) })
      : i18n.t("ai.recall.otherEdits", {
        count: String(other.count),
        author: authorName(other.author),
      });

  const recallConfirmText = (others: RecallOther[]): string =>
    i18n.t("ai.recall.confirm", {
      edits: others.map(recallOtherText).join(", "),
    });

  const recallRefusalText = (reason: RecallRefusal): string => {
    switch (reason) {
      case "turn_running": return i18n.t("ai.revert.turnRunning");
      case "batch_open": return i18n.t("ai.recall.refused.batchOpen");
      case "other_project": return i18n.t("ai.recall.refused.otherProject");
      case "no_recorded_position": return i18n.t("ai.recall.refused.noRecordedPosition");
      case "position_cleared": return i18n.t("ai.recall.refused.positionCleared");
      case "not_a_person_message": return i18n.t("ai.recall.refused.notAPersonMessage");
      case "conversation_changed": return i18n.t("ai.recall.refused.conversationChanged");
    }
  };

  /**
   * Recall a person's message: the message and everything after it leave the
   * conversation. With `undoEdits` the project's history cursor also moves
   * back to the position the message recorded, as one history jump that a
   * redo reverses.
   *
   * The cut takes a wire index, not a row index: a restored row carries the
   * message it came from, and a row the live turn painted is matched against
   * the saved wire, because rows hide envelopes and delivery messages.
   */
  const recallMessage = async (msg: ChatMessage, undoEdits: boolean): Promise<void> => {
    const id = openId();
    if (!id || msg.role !== "user") return;
    const epoch = scopeEpoch;
    // A running turn keeps writing into the history this would rewrite.
    if (loading() || reattached() || stoppingInherited()) {
      notify(i18n.t("ai.revert.turnRunning"), "error");
      return;
    }
    // The original view shows the history a compaction replaced, whose indices
    // do not address the saved wire a recall cuts.
    if (originalView() || sessionBusy()) return;
    // Older rows recorded no position, so there is nothing to move the cursor to.
    if (undoEdits && !msg.historyAt) {
      notify(i18n.t("ai.recall.noPosition"), "error");
      return;
    }
    // The conversation stays on screen until the reply lands: switching away
    // would leave a moved history without the transcript that goes with it.
    setSessionBusy(true);
    try {
      let keep = msg.wireIndex;
      if (keep === undefined) {
        const loaded = loadedSession(await aiApi.loadSession(id));
        keep = (loaded && savedWireIndex(msg, loaded.messages)) ?? undefined;
      }
      if (keep === undefined) {
        notify(i18n.t("ai.recall.unsaved"), "error");
        return;
      }
      let outcome = await aiApi.recall(id, keep, undoEdits, false);
      if (outcome.status === "needs_confirmation") {
        const { ask } = await import("@tauri-apps/plugin-dialog");
        const confirmed = await ask(recallConfirmText(outcome.others), {
          title: i18n.t("ai.recall.andUndoEdits"),
        });
        if (!confirmed) return;
        outcome = await aiApi.recall(id, keep, true, true);
      }
      if (outcome.status === "refused") {
        notify(recallRefusalText(outcome.reason), "error");
        return;
      }
      if (outcome.status !== "recalled") return;
      // The backend already moved this document, so it is published for the
      // project it names even when the panel moved on or closed; only the
      // repaint of a conversation that is off screen is skipped.
      if (outcome.snapshot?.file_path === store.filePath()) store.applyAiSnapshot(outcome.snapshot);
      // The recall stands either way; a file that did not take it is the one
      // thing the person must hear, so it is never replaced by a success.
      const unsaved = outcome.unsaved;
      if (unsaved) notify(i18n.t("ai.recall.fileUnsaved", { error: unsaved }), "error");
      if (disposed || epoch !== scopeEpoch || openId() !== id) return;
      setOriginalView(false);
      if (!(await paintSession(id)) || disposed || epoch !== scopeEpoch || openId() !== id) return;
      setInput(outcome.message.content);
      setAttachments(outcome.message.media ?? []);
      if (!unsaved) {
        notify(i18n.t(undoEdits ? "ai.recall.doneUndo" : "ai.recall.done"), "success");
      }
    } catch (error) {
      notify(i18n.t("ai.recall.failed", { error: errorMessage(error) }), "error");
    } finally {
      setSessionBusy(false);
    }
  };

  /** Paint the history before compaction, including the turns it replaced. */
  const paintOriginal = async (id: string): Promise<void> => {
    // A turn or a session action still writes the transcript this would
    // replace, and a late row anchors on the rows it shows.
    if (conversationLocked()) return;
    const epoch = scopeEpoch;
    // One paint owns the transcript: a later paint of either kind supersedes it.
    const request = ++paintRequest;
    try {
      const loaded = await aiApi.originalSession(id);
      if (disposed || epoch !== scopeEpoch || request !== paintRequest) return;
      setMessages(restoredTranscript(loaded.messages));
      // The original view carries the whole history, so no older page remains.
      setPageStart(0);
      setPageRevision("");
      setOlderTokens(0);
      setOriginalView(true);
      queueMicrotask(() => transcriptFollow?.resume());
    } catch {
      // A session that cannot be read keeps the compacted view.
    }
  };

  /** Whether an older page is already on its way. */
  let olderPageLoading = false;
  /**
   * Prepends the turns before the page on screen. The saved view may have
   * changed shape since it was painted, in which case the whole conversation
   * is read again instead of gluing an older page onto rows it does not
   * match.
   */
  const loadOlderPage = async (id: string): Promise<void> => {
    if (olderPageLoading || originalView() || conversationLocked() || pageStart() <= 0) return;
    const epoch = scopeEpoch;
    // A later paint supersedes this page, and the page must still match it.
    const request = paintRequest;
    const before = pageStart();
    const revision = pageRevision();
    olderPageLoading = true;
    setSessionError(null);
    try {
      const page = sessionPageRead(await aiApi.sessionPage(id, before, revision));
      if (!page || disposed || epoch !== scopeEpoch || request !== paintRequest || openId() !== id) return;
      if (page.status === "stale") {
        await paintSession(id);
        return;
      }
      const older = restoredTranscript(page.messages, page.start);
      const transcript = transcriptElement;
      const oldTop = transcript?.scrollTop ?? 0;
      const oldHeight = transcript?.scrollHeight ?? 0;
      // Keep the existing row objects; only the page above them is new.
      setMessages(previous => [...older, ...previous]);
      setPageStart(page.start);
      setPageRevision(page.revision);
      setOlderTokens(page.older_tokens);
      // The prepended rows must be visible once the page arrives.
      setRenderLimit(limit => limit + older.length);
      queueMicrotask(() => {
        if (!transcript || disposed) return;
        const added = transcript.scrollHeight - oldHeight;
        transcript.scrollTop = oldTop + added;
      });
    } catch (error) {
      setSessionError(errorMessage(error));
    } finally {
      olderPageLoading = false;
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
  /**
   * Questions that expired while the panel was re-attaching, by request id. An
   * `expired` event can overtake the `ai_turn_questions` reply, which would
   * otherwise bring back a card the backend has already refused.
   */
  let attachExpired: Map<string, Set<string>> | null = null;
  const stopFollowing = () => {
    if (turnPoll !== null) { clearInterval(turnPoll); turnPoll = null; }
    const inherited = reattached();
    if (inherited && approval()?.requestId === inherited.requestId) setApproval(null);
    setReattached(null);
  };
  /** Follows a backend turn this view did not start, until it ends. */
  const follow = (turn: InheritedTurn) => {
    setReattached(turn);
    if (turnPoll === null) turnPoll = setInterval(() => void followTurn(), TURN_POLL_MS);
  };
  /** Edits made without this view still belong on the visible project. */
  const refreshTurnProject = async (epoch: number) => {
    const file = store.filePath();
    try {
      const snapshot = await aiApi.getProject();
      if (!disposed && epoch === scopeEpoch && store.filePath() === file && snapshot.file_path === file) {
        store.applyAiSnapshot(snapshot);
      }
    } catch (error) {
      log.warn("ai", `background project could not be refreshed: ${errorMessage(error)}`);
    }
  };
  /** Whether the conversation on screen holds the message a steer delivered. */
  const steerDelivered = (rowId: string) =>
    messages().some(message => message.role === "user" && message.messageMeta?.steer_id === rowId);
  /** Whether the transcript on screen ends on an answer, not on a call or a question. */
  const endsOnAnswer = (rows: ChatMessage[]): boolean => {
    const last = [...rows].reverse().find(row => row.role !== "system");
    return !!last && last.role === "assistant" && !last.checkpoint && last.content.trim() !== "";
  };
  /**
   * A steer mark whose reply this page never receives: the page that sent it
   * went away. Once the steered turn has ended and its saved conversation is on
   * screen, that conversation says whether the message arrived; a message that
   * did not arrive goes back to waiting as an ordinary queued row.
   */
  const settleLostSteer = (painted: boolean) => {
    const mark = aiSession.steering();
    const session = openId();
    if (!mark || !session || aiSession.steerLive(mark.id)) return;
    // Settling a lost mark is bookkeeping, not a reply: the rows it leaves wait
    // for Send now, unless the caller settled a turn and flushes on its own.
    quietSteerEnds.add(mark.id);
    if (painted && steerDelivered(mark.id)) aiSession.acknowledgeQueued(session, mark.id);
    else aiSession.endSteer(session, mark.id);
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
    if (!marker || !sessions().some(session => session.id === marker.session)) {
      // Nothing to follow, so the conversation on screen is the saved one.
      settleLostSteer(true);
      return;
    }
    const epoch = scopeEpoch;
    const expired = new Map<string, Set<string>>();
    attachExpired = expired;
    try {
      const active = await readActiveTurn();
      if (disposed || epoch !== scopeEpoch) return;
      await refreshTurnProject(epoch);
      if (disposed || epoch !== scopeEpoch) return;
      if (!active || active.session_id !== marker.session) {
        // Nothing is running: the marker is cleared so it cannot lie later. The
        // saved conversation is read again, because the turn may have written
        // its answer after the first paint; an answer at its end means the turn
        // finished, anything else leaves its outcome unknown.
        aiSession.setTurn(null);
        const painted = openId() === marker.session
          && await paintSession(marker.session).catch(() => false);
        if (disposed || epoch !== scopeEpoch) return;
        setReloadNotice(painted && endsOnAnswer(messages()) ? "finished" : "lost");
        settleLostSteer(painted);
        return;
      }
      const turn: InheritedTurn = { requestId: active.request_id, session: marker.session, epoch };
      follow(turn);
      // A card that was open when the window went away is still waiting, unless
      // its wait ran out while this reply was on its way.
      const waiting = await aiApi.turnQuestions(active.request_id).catch(() => []);
      if (disposed || epoch !== scopeEpoch || reattached() !== turn) return;
      const gone = expired.get(active.request_id);
      const open = waiting.find(question => !gone?.has(question.call_id));
      if (open?.kind === "user_question") showUserQuestion(active.request_id, open);
      else if (open) showQuestion(active.request_id, open);
    } finally {
      if (attachExpired === expired) attachExpired = null;
    }
  };
  /** Asks whether the inherited turn is still running, and settles it when not. */
  const followTurn = async () => {
    const current = reattached();
    if (!current) return;
    const active = await readActiveTurn();
    // The panel stopped following this turn while the answer was on its way.
    if (reattached() !== current) return;
    if (active?.request_id === current.requestId) return;
    stopFollowing();
    await settleInheritedTurn(current);
  };
  /** The inherited turn finished: its answer is in the saved conversation. */
  const settleInheritedTurn = async (turn: InheritedTurn) => {
    // The turn ended, so no window may report it as still running.
    aiSession.setTurn(null);
    // A turn found in another workspace never paints or opens its conversation here.
    if (disposed || turn.epoch !== scopeEpoch) return;
    const painted = await paintSession(turn.session).catch(() => false);
    await refreshTurnProject(turn.epoch);
    if (disposed || turn.epoch !== scopeEpoch || openId() !== turn.session) return;
    setReloadNotice(null);
    setMessages(prev => [...prev, {
      role: "system",
      content: i18n.t(painted ? "ai.reload.settled" : "ai.reload.settledUnpainted"),
      timestamp: Date.now(),
    }]);
    settleLostSteer(painted);
    void flushQueued();
  };
  /**
   * Stops the backend turn this view follows, then waits for its saved results
   * and batch to settle. Until then the turn still writes the conversation and
   * the project, so the panel keeps them guarded as while it followed the turn.
   */
  const stopInheritedTurn = async () => {
    const current = reattached();
    if (!current) return;
    stopFollowing();
    setStoppingInherited(current);
    // The marker stays until the backend lets go of the turn: a window that
    // reloads during the wait still finds a turn that writes the conversation.
    const stale = () => disposed || current.epoch !== scopeEpoch || stoppingInherited() !== current;
    try {
      await aiApi.cancel(current.requestId).catch(() => {});
      // The turn closes its round and commits before it lets go of its slot;
      // the saved conversation is complete once the backend stops reporting it.
      let running = true;
      for (let waited = 0; ; waited += TURN_POLL_MS) {
        const active = await readActiveTurn();
        if (stale()) return;
        running = active?.request_id === current.requestId;
        if (!running || waited >= TURN_SETTLE_MS) break;
        await new Promise((resolve) => setTimeout(resolve, TURN_POLL_MS));
        if (stale()) return;
      }
      // The backend has let go of the turn, so no window may report it.
      if (!running) aiSession.setTurn(null);
      await refreshTurnProject(current.epoch);
      if (stale()) return;
      const painted = await paintSession(current.session).catch(() => false);
      if (stale()) return;
      setReloadNotice(null);
      setStoppingInherited(null);
      if (running) {
        // The backend still holds the turn after the wait: the panel follows it
        // again, so the queue keeps waiting behind it and Stop stays offered.
        follow(current);
        return;
      }
      setMessages(prev => [...prev, {
        role: "system",
        content: i18n.t(painted ? "ai.reload.stopped" : "ai.reload.stoppedUnpainted"),
        timestamp: Date.now(),
      }]);
      settleLostSteer(painted);
      // A settled turn sends the message waiting for it, as any stop does.
      void flushQueued();
    } finally {
      if (stoppingInherited() === current) setStoppingInherited(null);
    }
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
    if (conversationLocked()) return;
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
   * will not settle. A turn this view inherited has its own Stop, and a session
   * action in flight finishes first, so both refuse a new conversation. */
  const newConversationBlocked = () => !!reattached() || !!stoppingInherited() || sessionBusy();
  const startConversation = () => {
    if (newConversationBlocked()) return;
    if (loading()) {
      void cancel();
      abandonTurn();
    }
    clearTranscript();
    setSessionError(null);
    aiSession.setOpenScoped(sessionScope(), null);
    setOpenId(null);
    setBinding(null);
    // A conversation with no stored setting reads as the default.
    aiWorkspace.adoptPermission(DEFAULT_PERMISSION);
    setAllowed([]);
    setInput("");
    setAttachments([]);
    aiSession.clearQueue();
    setSessionsOpen(false);
  };
  /**
   * Forks a conversation from the list and opens the fork. While the open
   * conversation is locked, its own fork waits, and a fork of another one is
   * created and listed without leaving the conversation on screen.
   */
  const forkSession = async (id: string) => {
    if (conversationLocked()) {
      if (id === openId()) return;
      setSessionError(null);
      try {
        await aiApi.newSession(undefined, id);
        await refreshSessions();
      } catch (e) { setSessionError(errorMessage(e)); }
      return;
    }
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
    if (!id || conversationLocked() || originalView()) return;
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
    // Deleting the open conversation empties the screen, which is a switch.
    if (openId() === id && conversationLocked()) return;
    setSessionError(null);
    try {
      await aiApi.deleteSession(id);
      if (openId() === id) {
        clearTranscript();
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
    if (conversationLocked()) return;
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
   * The document to copy or download. Markdown is the transcript on screen
   * with the saved turns above the loaded page in front of it, so a running
   * turn's rows and unloaded turns both appear; the original view already
   * shows its whole history. JSON is the stored wire. Both carry the metadata
   * `sessions_load` returns.
   */
  const buildExport = async (format: ExportFormat): Promise<{ name: string; text: string } | null> => {
    const id = openId();
    if (!id) return null;
    const loaded = loadedSession(await aiApi.loadSession(id));
    if (!loaded) return null;
    if (format === "json") {
      return { name: exportFileName(loaded.meta, "json"), text: exportJson(loaded.meta, loaded.messages) };
    }
    const above = originalView() ? [] : restoredTranscript(loaded.messages.slice(0, pageStart()));
    const rows = [...above, ...messages()];
    return { name: exportFileName(loaded.meta, "md"), text: exportMarkdown(loaded.meta, rows) };
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
      .then((answer) => {
        if (answer?.snapshot.project) store.applyAiSnapshot(answer.snapshot);
        if (answer?.unsaved) notify(i18n.t("ai.recover.unsaved", { error: answer.unsaved }), "error");
        return answer;
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
      const failure = sessionCompactionFailure(payload);
      if (failure) {
        log.warn("ai", `session ${failure.session_id} compaction summary failed: ${failure.reason}`);
        setMessages(previous => [...previous, { role: "system", content: compactionFailureNotice(failure), timestamp: Date.now() }]);
        return;
      }
      const compaction = sessionCompaction(payload);
      if (!compaction) return;
      log.info("ai", `session ${compaction.session_id} compacted ${compaction.dropped_from}..${compaction.dropped_to}`);
      setMessages(previous => [...previous, { role: "system", content: compactionNotice(compaction), timestamp: Date.now() }]);
    });
    stopTurnEvents = await aiApi.listenTurn(onTurnEvent);
    stopProjectEvents = await aiApi.listenTurnProject((snapshot) => {
      // A stop of an inherited turn waits for that turn to commit, so its edits
      // still reach the project on screen.
      if (!disposed && (loading() || reattached() || stoppingInherited()) && snapshot.file_path === store.filePath()) {
        store.applyAiSnapshot(snapshot);
      }
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
  /**
   * The turn this panel runs: its id, its token, the messages it saved and the
   * last row it painted.
   */
  let turnEvents: { id: string; token: number; live: WireMessage[]; last: ChatMessage | undefined } | null = null;
  /**
   * The turn this panel ran last, until the next one settles, the panel
   * abandons it, or another conversation or workspace opens. Tauri does not
   * order events with command replies, so a message the turn saved can arrive
   * after `ai_turn_start` answered, even after the next turn posted; it still
   * belongs right after that turn's rows.
   */
  let finishedTurn: { id: string; session: string; live: WireMessage[]; last: ChatMessage | undefined } | null = null;
  /**
   * One event from a running turn. A turn this window inherited from a reload
   * can only ask; the transcript repaints from the store when it ends.
   */
  const onTurnEvent = (event: TurnEnvelope) => {
    if (disposed) return;
    // A question nobody answered in time was refused; its card goes away.
    if (event.type === "expired") {
      if (attachExpired) {
        const calls = attachExpired.get(event.request_id) ?? new Set<string>();
        calls.add(event.call_id);
        attachExpired.set(event.request_id, calls);
      }
      const open = approval();
      if (open && open.requestId === event.request_id && open.call_id === event.call_id) setApproval(null);
      const asking = userQuestion();
      if (asking && asking.requestId === event.request_id && asking.call_id === event.call_id) setUserQuestion(null);
      return;
    }
    const inherited = reattached();
    if (inherited && event.request_id === inherited.requestId) {
      if (event.type === "ask") showQuestion(event.request_id, event.request);
      return;
    }
    const late = finishedTurn;
    if (late && event.request_id === late.id) {
      if (event.type === "message") paintLate(late, event);
      return;
    }
    const turn = turnEvents;
    if (!turn || event.request_id !== turn.id || !liveTurn(turn.token)) return;
    switch (event.type) {
      case "message":
        paintSaved(turn, event.message);
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
      case "ask_user":
        showUserQuestion(event.request_id, event.request);
        break;
      case "preview":
        void paintPreview(event.path, event.before, event.after);
        break;
      case "wire_folded": {
        const row: ChatMessage = {
          role: "system",
          content: i18n.t("ai.turn.wireFolded", { folded: event.folded, before: event.before_rows, after: event.after_rows }),
          timestamp: Date.now(),
        };
        turn.last = row;
        setMessages(prev => [...prev, row]);
        break;
      }
      case "media":
        applyMediaNotices(event.notices);
        break;
    }
  };
  /**
   * The rows a reload would paint for one saved message. The turn's earlier
   * messages join a result to its call.
   */
  const savedRows = (live: WireMessage[], message: WireMessage): ChatMessage[] => {
    live.push(message);
    const index = live.length - 1;
    return restoredTranscript(live)
      .filter(row => row.wireIndex === index)
      .map(row => ({ ...row, wireIndex: undefined, timestamp: Date.now() }));
  };
  /**
   * A message the finished turn saved that arrived after its reply. Its rows go
   * right after the turn's last row, ahead of a stop or failure row and of
   * anything sent since. A transcript without that row was
   * repainted from the saved conversation, which already carries the message.
   */
  const paintLate = (late: NonNullable<typeof finishedTurn>, event: TurnEnvelope & { type: "message" }) => {
    if (event.session_id !== late.session || openId() !== late.session) return;
    const rows = savedRows(late.live, event.message);
    if (!rows.length) return;
    const anchor = late.last;
    const current = messages();
    const at = anchor ? current.indexOf(anchor) : -1;
    if (anchor && at < 0) return;
    late.last = rows[rows.length - 1];
    rows.forEach(row => { if (row.state) rememberState(row.state); });
    setMessages([...current.slice(0, at + 1), ...rows, ...current.slice(at + 1)]);
  };
  /** One message the running turn saved, painted as its rows. */
  const paintSaved = (turn: NonNullable<typeof turnEvents>, message: WireMessage) => {
    const rows = savedRows(turn.live, message);
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
    if (!rows.length) return;
    turn.last = rows[rows.length - 1];
    rows.forEach(row => { if (row.state) rememberState(row.state); });
    setMessages(prev => [...prev, ...rows]);
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
    if (stop.commit_error && stop.commit_failure === "save") {
      return i18n.t("ai.failure.saveFailed", { edits: stop.edits, error: stop.commit_error });
    }
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

  /** A request refused before admission returns its unsent draft. */
  const restoreComposer = (question: ChatMessage | null) => {
    if (!question) return;
    if (!input().trim()) setInput(question.content);
    const media = question.media ?? [];
    if (media.length && !attachments().length) setAttachments(media);
  };

  /** The anchor a stopped row keeps: what the turn sent and how it ended. */
  const stoppedTurn = (sent: WireMessage[], stop: TurnStop | undefined, retryable: boolean | undefined, textProtocol: boolean): StoppedTurn => ({
    messages: sent,
    edits: stop?.edits ?? 0,
    ...(stop ? { stop } : {}),
    ...(retryable === false ? { retryable: false } : {}),
    textProtocol,
  });

  /**
   * The row a finished turn earns. An answer is already on screen as the
   * message the turn saved; a stop or a failure says what happened and offers
   * Retry or Continue.
   */
  const settleOutcome = (outcome: TurnOutcome, sent: WireMessage[], textProtocol: boolean) => {
    const error = outcome.error;
    if (error && errorKind(error) === "auth_expired") setAuthError(errorMessage(error));
    // The turn's batch commits as one edit, so its facts are reported once here
    // instead of on the deferred transaction_commit row.
    const changed = isChangeSet(outcome.changes) && hasChangeFacts(outcome.changes)
      ? i18n.t("ai.turn.changed", {
        changes: changeSetText(outcome.changes, { properties: store.project.properties }),
      })
      : null;
    if (!outcome.failed) {
      if (changed) {
        setMessages(prev => [...prev, {
          role: "system",
          content: changed,
          timestamp: Date.now(),
        }]);
      }
      return;
    }
    const reason = error ? i18n.t("ai.turn.error", { error: errorMessage(error) }) : null;
    const content = outcome.stop
      ? [reason, stopNotice(outcome.stop), changed].filter(Boolean).join("\n")
      : [reason ?? outcome.text, changed].filter(Boolean).join("\n");
    setMessages(prev => [...prev, {
      role: "system",
      content,
      timestamp: Date.now(),
      ...(outcome.stop ? {} : { error: true }),
      resume: stoppedTurn(sent, outcome.stop, error?.retryable, textProtocol),
    }]);
  };

  /**
   * The live turn. Every write a turn makes after an await is fenced by its
   * token, so a panel that moved on (another workspace, a fresh conversation)
   * never receives the rows of a turn it abandoned.
   */
  const liveTurn = (token: number) => !disposed && token === turnToken;
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
    finishedTurn = null;
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
    /** The queued row this turn sends; a turn that fails to start puts it back. */
    queued?: QueuedMessage;
  }): Promise<boolean> => {
    const resumed = turn.resume;
    // Captured once: a later draft or a switch of reference must not move the
    // context of a turn that already started.
    const reference = turn.reference ?? null;
    const question = turn.user ?? (resumed ? userTurnOf(resumed.messages) : null);
    if (!question) return false;
    // True once the request reaches the backend, or once a turn that failed to
    // start gave its queued row back: a caller that took a message off the
    // queue puts it back only when neither happened.
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
    let handedBack = false;
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
    // A goal staged before the first message becomes a typed row before the
    // turn that may act on it is prepared.
    if (sessionId) {
      const stagedGoal = aiSession.goal();
      if (stagedGoal) {
        try {
          await aiApi.setSessionGoal(sessionId, stagedGoal);
          if (!liveTurn(token)) return posted;
          aiSession.setGoal("");
        } catch {
          // The turn still proceeds without the goal; the local value stays.
        }
      }
    }
    // A window that reloads while this turn runs keeps this marker; the next
    // mount asks the backend whether it still holds the turn.
    if (sessionId) aiSession.setTurn({ session: sessionId, started: startedAt });
    try {
      if (!sessionId) throw startError ?? new Error(i18n.t("ai.turn.notStarted"));
      log.info("ai", `Using provider: ${provider.name} (${provider.model})`);
      // A resumed turn sends the system prompt it stopped with; a fresh turn
      // reads the project and builds one for the model's capabilities.
      let sent = resumed?.messages;
      // The prompt and the backend's parser read one fact: the protocol the
      // prompt was built for travels with the turn.
      const textProtocol = (sent && resumed?.textProtocol) ?? !provider.capabilities.nativeToolCalling;
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
        // The saved goal and plan ride in the backend's derived context row,
        // so the prompt does not repeat them and a rewind changes both.
        sent = buildMessages(systemPrompt, question);
        // Every request carries this text, so the context figure counts it.
        setRequestOverhead(systemPrompt, tools);
      }
      const outbound = resumed ? sent.filter(message => message.role === "system") : sent;
      posted = true;
      noteSentEstimate(textTokens(JSON.stringify(outbound)), id, sessionId);
      // The previous turn's late messages keep their place: they carry that
      // turn's request id and land after its last row, ahead of this question.
      turnEvents = { id, token, live: [], last: messages().at(-1) };
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
          textProtocol,
        },
        (text) => { if (!cancelledFor(token)) setPartial(value => value + text); },
        (text) => { if (!cancelledFor(token)) setReasoning(value => appendThinking(value, text)); },
        (name) => { if (!cancelledFor(token)) setPreparingTool(name); },
      );
      if (!liveTurn(token)) return posted;
      settleOutcome(outcome, sent, textProtocol);
    } catch (e) {
      if (cancelledFor(token)) return posted;
      // A file opened while the send was on its way is not a turn failure:
      // nothing was saved, the conversation follows the open file, and the
      // message returns so the next send carries the binding notice.
      if (errorBindingMismatch(e)) {
        await followOpenProject(sessionId);
        if (turn.queued) {
          const row = turn.queued;
          aiSession.setQueued((previous) => [row, ...previous.filter((entry) => entry.id !== row.id)]);
        } else {
          restoreComposer(question);
        }
        handedBack = true;
        posted = false;
        log.warn("ai", "conversation followed the open project; the message returns");
        return posted;
      }
      // A failure before the turn started saved nothing, so the message goes
      // back where it came from instead of offering a Retry: a queued row to
      // the head of the queue, whatever the composer holds, and a typed
      // message to the composer.
      const message = errorMessage(e);
      if (errorKind(e) === "auth_expired") setAuthError(message);
      setMessages((prev) => [...prev, { role: "system", content: i18n.t("ai.turn.error", { error: message }), timestamp: Date.now() }]);
      if (turn.queued) {
        const row = turn.queued;
        aiSession.setQueued((previous) => [row, ...previous.filter((entry) => entry.id !== row.id)]);
        handedBack = true;
        posted = true;
      } else {
        restoreComposer(question);
      }
      log.error("ai", message);
    } finally {
      if (turnEvents?.id === id) {
        const settled = turnEvents;
        finishedTurn = liveTurn(token) && sessionId ? { id, session: sessionId, live: settled.live, last: settled.last } : null;
        turnEvents = null;
      }
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
        // steer owns its own send, so this path stands aside for it, and a row
        // that just failed to start waits for the person instead of retrying.
        if (!steering() && !handedBack) void flushQueued();
      }
    }
    return posted;
  };

  /**
   * Sends one message: the user row, the context warning and the turn. Returns
   * false when no turn took it and it was not given back, so a caller that took
   * it off the queue can put it back instead of dropping it.
   */
  const sendUserMessage = async (
    text: string,
    media: MediaAttachment[],
    reference: string | null,
    origin?: FlushOrigin,
    queued?: QueuedMessage,
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
    const ran = await executeTurn({ user: userMsg, reference, ...(queued ? { queued } : {}) });
    if (ran && !sessionNamed() && openId()) void nameSession(openId()!);
    return ran;
  };

  /** The composer field, grown to fit a multi-line draft up to a fixed cap. */
  let inputElement: HTMLTextAreaElement | undefined;
  const composerSize = composerSizer();
  const growInput = composerSize.resize;
  onCleanup(() => composerSize.dispose());
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
    if (steeringRow(item)) return;
    aiSession.setQueued((previous) => previous.filter((entry) => entry.id !== item.id));
    setQueuedEditing(null);
  };

  /** Opens one queued row for editing; the text is a draft until it commits. */
  const beginQueuedEdit = (item: QueuedMessage) => {
    if (steeringRow(item)) return;
    setQueuedDraft(item.text);
    setQueuedEditing(item.id);
  };
  /** Enter or blur writes the edit back; Escape closes it uncommitted. */
  const commitQueuedEdit = () => {
    const editing = queuedEditing();
    if (!editing) return;
    const text = queuedDraft().trim();
    setQueuedEditing(null);
    if (!text) return;
    aiSession.setQueued((previous) => previous.map((entry) => entry.id === editing ? { ...entry, text } : entry));
  };
  /** Whether a steer is delivering this row; it is neither edited nor sent again. */
  const steeringRow = (item: QueuedMessage) => aiSession.steering()?.id === item.id;

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
    if (reattached() || stoppingInherited()) return i18n.t("ai.queue.waitingReload");
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
    // A conversation opened from the list may work on another file; it
    // follows the open one before the turn, so the model reads the notice.
    if (followsOpenProject(binding())) await followOpenProject(openId());
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
    // turn this window inherited is still a turn, until its stop has settled:
    // it is queued behind it too.
    if (loading() || reattached() || stoppingInherited()) {
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
    if (flushing || steering()) return;
    const next: QueuedMessage | undefined = aiSession.queued()[0];
    if (disposed || !next || loading() || reattached() || stoppingInherited() || !getActiveProvider().model.trim()) return;
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
    const ran = await sendUserMessage(taken.text, taken.media, knownReference(taken.reference ?? null), origin, taken);
    if (!ran) {
      // The panel moved on before the turn started: the message waits at the
      // head of the queue instead of disappearing with the abandoned turn.
      flushing = false;
      aiSession.setQueued((previous) => [taken, ...previous]);
    }
  };

  /**
   * Deliver into the running loop; keep the durable queue row until saved. A
   * row that reads a project path is not steered: its context is attached when
   * the turn finishes and the row goes out as an ordinary send, so the person's
   * own words are all the conversation records for it.
   */
  const steerQueued = async (): Promise<void> => {
    const next = aiSession.queued()[0];
    const inherited = reattached();
    const id = inherited?.requestId ?? (loading() ? requestId : null);
    // The steered turn's own conversation, not whatever the screen shows; the
    // queue belongs to the conversation on screen, so the two must agree.
    const session = inherited?.session ?? openId();
    if (!next || !id || !session || session !== openId() || steering() || stoppingInherited() || next.reference) return;
    const refusal = imageRefusal(next.media);
    if (refusal) { setMediaError(refusal); return; }
    const origin = scopeEpoch;
    if (queuedEditing() === next.id) setQueuedEditing(null);
    aiSession.beginSteer(session, { id: next.id });
    try {
      await aiApi.steerTurn(id, session, {
        id: next.id,
        content: next.text,
        media: next.media.map(item => usesLocalReference(item) ? { ...item, local_reference: true } : item),
      });
      aiSession.acknowledgeQueued(session, next.id);
      if (disposed || origin !== scopeEpoch) return;
      setMediaError(null);
    } catch (error) {
      // A final answer can close admission first; the row stays queued and
      // goes out normally after that turn settles.
      aiSession.endSteer(session, next.id);
      if (!disposed && origin === scopeEpoch && errorKind(error) !== "steer_closed") setMediaError(errorMessage(error));
    }
  };
  // A steer whose reply ends it hands the queue back, and the turn it waited
  // for may have settled meanwhile: the queue goes on from here, whichever
  // panel sent it. A lost mark this panel settled hands nothing back: its row
  // waits for Send now like any row found with nothing running.
  createEffect<{ open: string | null; mark: string | null }>((before) => {
    const now = { open: aiSession.open(), mark: aiSession.steering()?.id ?? null };
    const quiet = before.mark !== null && quietSteerEnds.delete(before.mark);
    if (before.mark && !now.mark && before.open === now.open && !quiet) untrack(() => void flushQueued());
    return now;
  }, { open: aiSession.open(), mark: aiSession.steering()?.id ?? null });

  /** Retry resends the stopped turn; Continue resumes it where it stopped. */
  const resumeTurn = async (row: ChatMessage, action: "retry" | "continue") => {
    const resume = row.resume;
    // One turn at a time includes the turn this window inherited.
    if (!resume || loading() || reattached() || stoppingInherited()) return;
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
    // Any applied edit is committed, which withholds this Retry; so none landed.
    return i18n.t("ai.retry.note");
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
                const busy = loading() || !!reattached() || !!stoppingInherited();
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
                  // Only a person's message is a point to recall to: the
                  // backend cuts there and can move the history cursor back.
                  ...(msg.role === "user" ? [{
                    label: i18n.t("ai.recall.message"),
                    // A running turn keeps writing into the history this
                    // rewrites, so the item is withheld until it settles.
                    disabled: busy || originalView(),
                    hint: busy
                      ? i18n.t("ai.revert.turnRunning")
                      : originalView()
                        ? i18n.t("ai.revert.originalView")
                        : i18n.t("ai.recall.messageHint"),
                    onSelect: () => void recallMessage(msg, false),
                  }, {
                    label: i18n.t("ai.recall.andUndoEdits"),
                    // A row saved before positions were recorded has none to
                    // move the cursor back to.
                    disabled: busy || originalView() || !msg.historyAt,
                    hint: busy
                      ? i18n.t("ai.revert.turnRunning")
                      : originalView()
                        ? i18n.t("ai.revert.originalView")
                        : !msg.historyAt
                          ? i18n.t("ai.recall.noPosition")
                          : i18n.t("ai.recall.andUndoEditsHint"),
                    onSelect: () => void recallMessage(msg, true),
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
                <Show when={msg.binding || msg.surface || msg.state} fallback={
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
                    <AiAudition message={msg} />
                  </Show>
                }>
                  <Show when={msg.state} fallback={
                    <Show when={msg.surface} fallback={
                      <div class="ai-binding-row" role="status" title={msg.binding!.to}>
                        {i18n.t(msg.binding!.cause === "moved" ? "ai.binding.moved" : "ai.binding.bound", { name: projectName(msg.binding!.to) })}
                      </div>
                    }>
                      <div class="ai-binding-row" role="status" title={toolSurfaceRow(msg.surface!).names}>
                        {toolSurfaceRow(msg.surface!).text}
                      </div>
                    </Show>
                  }>
                    <div class="ai-state-row" role="status">{sessionStateRow(msg.state!)}</div>
                  </Show>
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
                    summary={<>
                      <span class="ai-tool-icon"><ToolIcon name={msg.tool!.name} /></span>
                      <span class="ai-tool-name">{activitySummary(msg.tool!, msg.content, { properties: store.project.properties })}</span>
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
              <Show when={msg.context !== "audition" && msg.media?.length}>
                <div class="ai-message-media">
                  <For each={msg.media}>
                    {(attachment) => (
                          <span class="ai-media-tag" title={attachment.path} classList={{ missing: !!attachment.missing }}>
                            {attachment.name} · {attachment.missing
                              ? i18n.t("ai.media.missingState")
                              : <>{attachment.local_reference ? i18n.t("ai.media.localReference") : attachment.mime} · {formatDuration(attachment.duration_secs) ?? i18n.t("ai.media.unknownDuration")} · {formatBytes(attachment.bytes)}</>}
                          </span>
                    )}
                  </For>
                </div>
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
        locked={conversationLocked()}
        newBlocked={newConversationBlocked() ? i18n.t("ai.sessions.newBlocked") : null}
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
        <Show when={hiddenMessages() > 0 || pageStart() > 0 || (compactedSession() && !originalView())}>
          <button
            type="button"
            class="ai-transcript-earlier"
            classList={{ visible: atTranscriptTop() }}
            aria-hidden={!atTranscriptTop()}
            tabIndex={atTranscriptTop() ? 0 : -1}
            onClick={() => {
              if (hiddenMessages() > 0) {
                setRenderLimit((limit) => limit + TRANSCRIPT_RENDER_LIMIT);
              } else if (pageStart() > 0) {
                if (openId()) void loadOlderPage(openId()!);
              } else if (openId()) {
                void paintOriginal(openId()!);
              }
            }}
          >{hiddenMessages() > 0
            ? i18n.t("ai.transcript.earlier", { count: hiddenMessages() })
            : pageStart() > 0
              ? i18n.t("ai.transcript.earlierTurns")
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

      {/* A question from the model uses the same slot as an approval card. */}
      <Show when={userQuestion()}>
        <div class="ai-approval-card ai-question-card" role="alert" aria-label={i18n.t("ai.question.title")}>
          <div class="ai-approval-title">{i18n.t("ai.question.title")}</div>
          <div class="ai-question-text selectable">{userQuestion()!.question}</div>
          <Show when={(userQuestion()!.options?.length ?? 0) > 0}>
            <div class="ai-approval-actions">
              <For each={userQuestion()!.options!}>
                {(option, index) => (
                  <button class="ai-question-option" onClick={() => answerQuestion(index())}>{option}</button>
                )}
              </For>
            </div>
          </Show>
          <Show when={userQuestion()!.free_text}>
            <div class="ai-question-free">
              <input
                class="ai-question-input"
                type="text"
                aria-label={i18n.t("ai.question.placeholder")}
                placeholder={i18n.t("ai.question.placeholder")}
                value={replyText()}
                onInput={(event) => setReplyText(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    answerQuestion();
                  }
                }}
              />
              <button
                class="ai-question-submit"
                disabled={!replyText().trim()}
                onClick={() => answerQuestion()}
              >{i18n.t("ai.question.submit")}</button>
            </div>
          </Show>
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
                  <Show when={queuedEditing() === item.id} fallback={
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
                    disabled={steeringRow(item)}
                    onClick={() => beginQueuedEdit(item)}
                  >{i18n.t("ai.queue.edit")}</button>
                  <button
                    type="button"
                    class="ai-queue-remove"
                    disabled={steeringRow(item)}
                    onClick={() => removeQueued(item)}
                  >{i18n.t("ai.queue.remove")}</button>
                  {/* The visible word is the accessible name; the title says
                      what the button does, or why a row with a project path
                      waits for the turn to finish. */}
                  <Show when={index() === 0 && (loading() || reattached()?.session === openId())}>
                    <button
                      type="button"
                      class="ai-queue-steer"
                      title={i18n.t(item.reference ? "ai.queue.steerReference" : "ai.queue.steerHint")}
                      disabled={steering() || !!item.reference} onClick={() => void steerQueued()}
                    >{i18n.t("ai.queue.steer")}</button>
                  </Show>
                  <Show when={index() === 0 && !loading() && !reattached() && !stoppingInherited()}>
                    <button
                      type="button"
                      class="ai-queue-send"
                      title={i18n.t("ai.queue.sendNowHint")}
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
      <Show when={reattached() || stoppingInherited()}>
        <div class="ai-turn-orphan" role="status">
          <span class="ai-turn-orphan-text">{i18n.t(reattached() ? "ai.reload.running" : "ai.turn.stopping")}</span>
          <button
            type="button"
            class="ai-turn-orphan-stop"
            disabled={!reattached()}
            onClick={() => void stopInheritedTurn()}
          >{i18n.t("ai.reload.stop")}</button>
        </div>
      </Show>
      {/* The marker a closed or reloaded panel left behind and the backend does
          not back: nothing is running, so nothing is painted as running. A turn
          that reached its answer says so; one that did not offers a fresh read. */}
      <Show when={reloadNotice()}>
        <div class="ai-turn-reloaded" role="status">
          <span class="ai-turn-reloaded-text">{i18n.t(reloadNotice() === "finished" ? "ai.reload.finished" : "ai.reload.lost")}</span>
          <Show when={reloadNotice() === "lost"}>
            <button
              type="button"
              class="ai-turn-reloaded-open"
              onClick={() => void reloadTranscript()}
            >{i18n.t("ai.reload.reload")}</button>
          </Show>
          <button
            type="button"
            class="ai-turn-reloaded-dismiss"
            onClick={() => setReloadNotice(null)}
          >{i18n.t("ai.reload.dismiss")}</button>
        </div>
      </Show>

      <AiComposer
        blockedNotice={blockedNotice()}
        goal={sessionStateView().goal ?? ""}
        plan={sessionStateView().plan ?? []}
        onClearGoal={() => void clearGoal()}
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
        onInput={(element) => setInput(element.value)}
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
