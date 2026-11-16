// UndoState matches the Rust UndoState struct
export interface UndoState {
  can_undo: boolean;
  can_redo: boolean;
  undo_label: string | null;
  redo_label: string | null;
  undo_labels: string[];
  redo_labels: string[];
}

/** Aspect tags a history entry can carry; ids match `UndoScope` in Rust. */
export type UndoScope =
  | "project"
  | "mixer_routing"
  | "mixer_levels"
  | "preset"
  | "automation"
  | "plugin_state"
  | "transport";

/** Who made an edit; matches `edit_history::Author` in Rust. */
export type HistoryAuthor =
  | { kind: "person" }
  | { kind: "agent"; session?: string }
  | { kind: "client"; owner: string }
  | { kind: "outside" };

/** What the footer names after an undo or a redo. */
export interface HistoryBrief {
  label: string;
  subject?: string;
  author: HistoryAuthor;
}

/** Why a history step from the GUI did not run. */
export type HistoryRefusal = "agent_editing" | "nothing_to_undo" | "nothing_to_redo";

/** The answer to an undo, a redo or a jump. */
export type HistoryStep<Snapshot> =
  | { status: "applied"; snapshot: Snapshot; brief?: HistoryBrief }
  | { status: "refused"; reason: HistoryRefusal };

/** One undo level: applied levels are undoable, the rest are redo. */
export interface UndoHistoryEntry {
  /** 1 is the oldest kept edit. */
  index: number;
  name: string;
  subject?: string;
  author: HistoryAuthor;
  scopes: UndoScope[];
  /** Milliseconds since the Unix epoch. */
  timestamp: number;
  applied: boolean;
}

/** Opaque token that closes one owned undo gesture. */
export type UndoGestureToken = string & { readonly __undoGestureToken: unique symbol };

/** One page of the history, newest first. */
export interface HistoryPage {
  entries: UndoHistoryEntry[];
  total: number;
  applied: number;
  /** Whether the history is kept on disk for the project file. */
  journaled: boolean;
}

/** Where the history is kept and the space it occupies. */
export interface HistoryInfo {
  total: number;
  applied: number;
  journaled: boolean;
  bytes: number;
  path?: string;
}

export type HistoryClearOutcome =
  | { status: "cleared"; dropped: number; freed: number; info: HistoryInfo }
  | { status: "refused"; reason: HistoryRefusal };
