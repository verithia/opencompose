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
import type { MediaAttachment } from "../ai/media";

/** A message composed while a turn ran; it goes out when that turn settles. */
export interface QueuedMessage {
  /**
   * Names the row across a reload and a remount, where object identity does
   * not survive: a delivery is acknowledged by this id.
   */
  id: string;
  text: string;
  media: MediaAttachment[];
  /** The project path this message reads, when the composer scoped it. */
  reference?: string;
}

/** A queued row as a caller writes it; the store gives it an id if it has none. */
export type QueuedDraft = Omit<QueuedMessage, "id"> & { id?: string };

/**
 * The queued row a steer is delivering into a running turn. `before` counts the
 * person's rows with the same text that the transcript held when the steer
 * went out, so a window that lost the reply can tell from the saved
 * conversation whether the message arrived.
 */
export interface SteerMark {
  /** The queued row's id; the backend saves it with the delivered message. */
  id: string;
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
  /** The queued row a steer is delivering, so no second steer sends it again. */
  steering?: SteerMark;
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

let queuedCounter = 0;
/** A fresh row id; a runtime without `randomUUID` still gets a unique one. */
function newQueuedId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  queuedCounter += 1;
  return `q-${Date.now().toString(36)}-${queuedCounter.toString(36)}`;
}

/** Gives every row an id; a row that already has one keeps it. */
function withIds(rows: readonly QueuedDraft[]): QueuedMessage[] {
  return rows.map(row => typeof row.id === "string" && isRowId(row.id) ? row as QueuedMessage : { ...row, id: newQueuedId() });
}

function queueOf(value: unknown): QueuedMessage[] {
  if (!Array.isArray(value)) return [];
  const rows: QueuedMessage[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== "object") continue;
    const text = textOf((entry as QueuedMessage).text)?.trim();
    if (!text) continue;
    const reference = (entry as QueuedMessage).reference;
    // A row stored without an id is given one here and keeps it from the next write.
    const stored = textOf((entry as QueuedMessage).id);
    const id = stored && isRowId(stored) ? stored : newQueuedId();
    rows.push({
      id,
      text,
      media: attachmentsOf((entry as QueuedMessage).media),
      ...(typeof reference === "string" && reference ? { reference } : {}),
    });
  }
  return rows;
}

function steerOf(value: unknown): SteerMark | null {
  if (!value || typeof value !== "object") return null;
  const id = textOf((value as SteerMark).id);
  return id && isRowId(id) ? { id } : null;
}

/** An id the backend keeps with a steered message: up to 64 of `[A-Za-z0-9._-]`. */
export function isRowId(id: string): boolean {
  return /^[A-Za-z0-9._-]{1,64}$/.test(id);
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
        // A mark is only meaningful while the row it names is still queued.
        const steering = steerOf(entry.steering);
        if (steering && queue.some(row => row.id === steering.id)) conversation.steering = steering;
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
const [steering, setSteeringSignal] = createSignal<SteerMark | null>(null);
const [open, setOpenSignal] = createSignal<string | null>(null);
const [openScope, setOpenScopeSignal] = createSignal<string | null>(null);
const [turn, setTurnSignal] = createSignal<InFlightTurn | null>(null);

let stored = readState();
/**
 * Steers whose reply this page is still waiting for. A mark the store read back
 * without one of these was left by a page that went away, so its reply is lost.
 */
const liveSteers = new Set<string>();
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
  const mark = steering();
  if (mark) conversation.steering = mark;
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
  setSteeringSignal(conversation.steering ?? null);
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
  setQueued(value: QueuedDraft[] | ((previous: QueuedMessage[]) => QueuedDraft[])) {
    setQueueSignal(previous => withIds(typeof value === "function" ? value(previous) : value));
    persist();
  },
  /**
   * A delivery belongs to its conversation even if its originating panel closed
   * or the store was read again, so the row is found by its id.
   */
  acknowledgeQueued(id: string, rowId: string) {
    liveSteers.delete(rowId);
    if (open() === id) {
      setQueueSignal(previous => previous.filter(item => item.id !== rowId));
      if (steering()?.id === rowId) setSteeringSignal(null);
      persist();
      return;
    }
    const conversation = stored.conversations[slot(id)];
    if (!conversation) return;
    if (conversation.queue) {
      conversation.queue = conversation.queue.filter(item => item.id !== rowId);
      if (!conversation.queue.length) delete conversation.queue;
    }
    if (conversation.steering?.id === rowId) delete conversation.steering;
    writeState(stored);
  },
  /** The open conversation's row a steer is delivering, or `null`. */
  steering,
  /** Whether this page is still waiting for the reply of the steer naming `rowId`. */
  steerLive(rowId: string): boolean {
    return liveSteers.has(rowId);
  },
  /** Marks a row of conversation `id` as being steered, before the request goes out. */
  beginSteer(id: string, mark: SteerMark) {
    liveSteers.add(mark.id);
    if (open() === id) {
      setSteeringSignal(mark);
      persist();
      return;
    }
    const conversation = stored.conversations[slot(id)] ?? (stored.conversations[slot(id)] = {});
    conversation.steering = mark;
    writeState(stored);
  },
  /** The steer naming `rowId` ended without delivering; its row stays queued. */
  endSteer(id: string, rowId: string) {
    liveSteers.delete(rowId);
    if (open() === id) {
      if (steering()?.id !== rowId) return;
      setSteeringSignal(null);
      persist();
      return;
    }
    const conversation = stored.conversations[slot(id)];
    if (conversation?.steering?.id !== rowId) return;
    delete conversation.steering;
    if (!Object.keys(conversation).length) delete stored.conversations[slot(id)];
    writeState(stored);
  },
  /** The composer clears so every send has an effect. */
  clearQueue() {
    setQueueSignal([]);
    setSteeringSignal(null);
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
    const same = open() === value && openScope() === (value ? scope : null);
    if (!same) {
      flushPersist();
      setOpenSignal(value);
      setOpenScopeSignal(value ? scope : null);
      install(value);
    }
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
    setSteeringSignal(null);
    liveSteers.clear();
    setOpenSignal(null);
    setOpenScopeSignal(null);
    setTurnSignal(null);
  },
};
