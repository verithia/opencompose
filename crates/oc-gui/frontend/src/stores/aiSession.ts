/**
 * What the AI panel must not lose when the panel itself goes away. Closing the
 * sidebar or switching tabs unmounts it (`App.tsx`), so the composer draft, its
 * attachment chips, the messages queued behind a running turn, the edit
 * proposal waiting for an answer and the conversation it has open live here
 * instead of in the component.
 *
 * The same state also survives a reload of the window: the draft, the chips and
 * the queue are written per conversation to `localStorage`, together with the
 * marker naming the turn that was running. Nothing secret is written: the
 * chips carry the descriptor the picker produced (a path and its metadata), and
 * a conversation's own attachments already live in its saved messages.
 */
import { createSignal } from "solid-js";
import type { Proposal } from "../ai/agent";
import type { MediaAttachment } from "../ai/media";
import type { DiffResult } from "../ai/transcript";

/** A message composed while a turn ran; it goes out when that turn settles. */
export interface QueuedMessage {
  text: string;
  media: MediaAttachment[];
  /** The project path this message reads, when the composer scoped it. */
  reference?: string;
}

/** A validated replacement waiting for Accept or Reject, with the diff shown. */
export interface PendingProposal {
  proposal: Proposal;
  diff: DiffResult;
}

/** The turn this window was running, kept so a reloaded window can ask about it. */
export interface InFlightTurn {
  session: string;
  started: number;
}

/** What one conversation keeps in the store. */
interface ConversationState {
  draft?: string;
  attachments?: MediaAttachment[];
  queue?: QueuedMessage[];
  /** The staged goal the agent is working toward in this conversation. */
  goal?: string;
}

interface StoredState {
  v: 1;
  /** The conversation the panel had open, restored so a reload repaints it. */
  open: string | null;
  /**
   * The workspace and project file `open` belongs to. A conversation is
   * remembered per project path as well as globally, so opening another song
   * does not resume the previous song's chat.
   */
  openScope: string | null;
  /** Last conversation per workspace + project path. */
  openByScope: Record<string, string>;
  /** Composer and queue per conversation, so a switch does not mix them. */
  conversations: Record<string, ConversationState>;
  /** The turn that was running when the window went away, if any. */
  turn: InFlightTurn | null;
}

const STORAGE_KEY = "opencompose.ai.panel.v1";
/**
 * How long typing may go unwritten. The draft is the only value that changes
 * per keystroke, and writing the whole store on each one serialises a
 * conversation-sized object for every character.
 */
const DRAFT_PERSIST_MS = 500;

function emptyState(): StoredState {
  return { v: 1, open: null, openScope: null, openByScope: {}, conversations: {}, turn: null };
}

function textOf(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/** The chips a stored entry names, as the descriptor the picker produced. */
function attachmentsOf(value: unknown): MediaAttachment[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is MediaAttachment =>
    !!entry && typeof entry === "object"
    && (entry as MediaAttachment).kind !== undefined
    && typeof (entry as MediaAttachment).path === "string"
    && typeof (entry as MediaAttachment).name === "string");
}

function queueOf(value: unknown): QueuedMessage[] {
  if (!Array.isArray(value)) return [];
  const rows: QueuedMessage[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== "object") continue;
    const text = textOf((entry as QueuedMessage).text)?.trim();
    if (!text) continue;
    const reference = (entry as QueuedMessage).reference;
    rows.push({
      text,
      media: attachmentsOf((entry as QueuedMessage).media),
      ...(typeof reference === "string" && reference ? { reference } : {}),
    });
  }
  return rows;
}

/** Reads the store; a damaged or older entry reads as an empty one. */
function readState(): StoredState {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return emptyState();
    const parsed = JSON.parse(raw) as Partial<StoredState> | null;
    if (!parsed || parsed.v !== 1) return emptyState();
    const state = emptyState();
    state.open = textOf(parsed.open);
    state.openScope = textOf(parsed.openScope);
    const openByScope = parsed.openByScope;
    if (openByScope && typeof openByScope === "object") {
      for (const [scope, id] of Object.entries(openByScope)) {
        const value = textOf(id);
        if (scope && value) state.openByScope[scope] = value;
      }
    }
    const conversations = parsed.conversations;
    if (conversations && typeof conversations === "object") {
      for (const [id, entry] of Object.entries(conversations)) {
        // The empty id is the composer of a conversation not started yet.
        if (!entry || typeof entry !== "object") continue;
        const conversation: ConversationState = {};
        const draft = textOf(entry.draft);
        if (draft) conversation.draft = draft;
        const attachments = attachmentsOf(entry.attachments);
        if (attachments.length) conversation.attachments = attachments;
        const queue = queueOf(entry.queue);
        if (queue.length) conversation.queue = queue;
        const goal = textOf(entry.goal);
        if (goal) conversation.goal = goal;
        if (Object.keys(conversation).length) state.conversations[id] = conversation;
      }
    }
    const turn = parsed.turn;
    if (turn && typeof turn === "object") {
      const session = textOf(turn.session);
      const started = typeof turn.started === "number" ? turn.started : null;
      if (session && started !== null) state.turn = { session, started };
    }
    return state;
  } catch {
    return emptyState();
  }
}

function writeState(state: StoredState) {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // A full or unavailable store loses the draft, never the conversation.
  }
}

const [draft, setDraftSignal] = createSignal("");
const [attachments, setAttachmentsSignal] = createSignal<MediaAttachment[]>([]);
const [queue, setQueueSignal] = createSignal<QueuedMessage[]>([]);
const [goal, setGoalSignal] = createSignal("");
const [pending, setPending] = createSignal<PendingProposal | null>(null);
const [open, setOpenSignal] = createSignal<string | null>(null);
const [openScope, setOpenScopeSignal] = createSignal<string | null>(null);
const [turn, setTurnSignal] = createSignal<InFlightTurn | null>(null);

let stored = readState();
/** The pending draft write, if typing has not paused yet. */
let draftTimer: ReturnType<typeof setTimeout> | null = null;

/** The store's slot for a conversation; the empty one holds a composer not started yet. */
function slot(id: string | null): string {
  return id ?? "";
}

/** Writes the open conversation's composer and queue, and the turn marker. A
 * write supersedes any draft write it already contains. */
function persist() {
  if (draftTimer !== null) {
    clearTimeout(draftTimer);
    draftTimer = null;
  }
  const key = slot(open());
  const conversation: ConversationState = {};
  if (draft()) conversation.draft = draft();
  if (attachments().length) conversation.attachments = attachments();
  if (queue().length) conversation.queue = queue();
  if (goal()) conversation.goal = goal();
  if (Object.keys(conversation).length) stored.conversations[key] = conversation;
  else delete stored.conversations[key];
  stored.open = open();
  stored.openScope = openScope();
  stored.turn = turn();
  writeState(stored);
}

/** Schedules the write that carries the draft, replacing one already pending. */
function persistDraftSoon() {
  if (draftTimer !== null) clearTimeout(draftTimer);
  draftTimer = setTimeout(() => {
    draftTimer = null;
    persist();
  }, DRAFT_PERSIST_MS);
}

/** Writes a pending draft now: a page unload, an unmount, or a reload seam. */
function flushPersist() {
  if (draftTimer === null) return;
  persist();
}

// A page that goes away never runs the timer; flush so the next window sees the
// last keystroke. `pagehide` covers a reload, a navigation and a bfcache entry.
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", flushPersist);
}

/** Installs one conversation's composer and queue, dropping the previous one. */
function install(id: string | null) {
  const conversation = stored.conversations[slot(id)] ?? {};
  setDraftSignal(conversation.draft ?? "");
  setAttachmentsSignal(conversation.attachments ?? []);
  setQueueSignal(conversation.queue ?? []);
  setGoalSignal(conversation.goal ?? "");
}

setOpenSignal(stored.open);
setOpenScopeSignal(stored.openScope);
install(stored.open);
setTurnSignal(stored.turn);

export const aiSession = {
  /** The staged goal for the open conversation; empty when none is set. */
  goal,
  setGoal(value: string | ((previous: string) => string)) {
    setGoalSignal(value);
    persist();
  },
  /** Composer text. A conversation switch clears it; an unmount or a reload keeps it. */
  draft,
  setDraft(value: string | ((previous: string) => string)) {
    setDraftSignal(value);
    // The value is on screen at once; the write waits for a pause in typing.
    persistDraftSoon();
  },
  /** Chips waiting in the composer. A send consumes them, a failure gives them back. */
  attachments,
  setAttachments(value: MediaAttachment[] | ((previous: MediaAttachment[]) => MediaAttachment[])) {
    setAttachmentsSignal(value);
    persist();
  },
  /** Messages waiting behind a running turn, oldest first, with their chips. */
  queued: queue,
  setQueued(value: QueuedMessage[] | ((previous: QueuedMessage[]) => QueuedMessage[])) {
    setQueueSignal(value);
    persist();
  },
  /** The composer clears so every send has an effect. */
  clearQueue() {
    setQueueSignal([]);
    persist();
  },
  /** Takes the oldest message off the queue; exactly one caller gets it. */
  takeQueued(): QueuedMessage | null {
    const [next, ...rest] = queue();
    if (!next) return null;
    setQueueSignal(rest);
    persist();
    return next;
  },
  /** The proposal on screen; it survives an unmount like the draft does. */
  pending,
  setPending,
  /** Id of the conversation on screen; `null` before one is opened. */
  open,
  /** The workspace + project path the open conversation belongs to. */
  openScope,
  /** Legacy switch that leaves the scope unknown; the panel uses `setOpenScoped`. */
  setOpen(value: string | null) {
    setOpenSignal(value);
    setOpenScopeSignal(null);
    install(value);
    persist();
  },
  /** The last conversation this workspace + project had open. */
  rememberedOpen(scope: string): string | null {
    return stored.openByScope[scope] ?? null;
  },
  /** Remembers an id for one scope without changing what is on screen. */
  rememberOpen(scope: string, id: string) {
    if (!scope) return;
    stored.openByScope[scope] = id;
    const scopes = Object.keys(stored.openByScope);
    while (scopes.length > 64) {
      const oldest = scopes.shift()!;
      delete stored.openByScope[oldest];
    }
    writeState(stored);
  },
  /** Forgets one scope, so its next open starts a fresh conversation. */
  forgetOpen(scope: string) {
    if (!scope) return;
    delete stored.openByScope[scope];
    writeState(stored);
  },
  /** Switches the transcript and records it against this workspace + project. */
  setOpenScoped(scope: string, value: string | null) {
    setOpenSignal(value);
    setOpenScopeSignal(value ? scope : null);
    install(value);
    if (value) {
      this.rememberOpen(scope, value);
    } else {
      this.forgetOpen(scope);
    }
    persist();
  },
  /** The turn running when the window went away; the backend says if it still is. */
  turn,
  setTurn(value: InFlightTurn | null) {
    setTurnSignal(value);
    persist();
  },
  /**
   * Drops the in-memory state and reads the store again, as a window that has
   * just been reloaded does. The panel does not call this: the module loads the
   * store when it is first imported.
   */
  reload() {
    flushPersist();
    stored = readState();
    setPending(null);
    setOpenSignal(stored.open);
    setOpenScopeSignal(stored.openScope);
    install(stored.open);
    setTurnSignal(stored.turn);
  },
  /** Writes a draft that typing left pending; the page unload path uses it too. */
  flush: flushPersist,
  /** Test seam: a fresh window with nothing typed, queued, open or running. */
  reset() {
    if (draftTimer !== null) {
      clearTimeout(draftTimer);
      draftTimer = null;
    }
    stored = emptyState();
    writeState(stored);
    setDraftSignal("");
    setAttachmentsSignal([]);
    setQueueSignal([]);
    setPending(null);
    setOpenSignal(null);
    setOpenScopeSignal(null);
    setTurnSignal(null);
  },
};
