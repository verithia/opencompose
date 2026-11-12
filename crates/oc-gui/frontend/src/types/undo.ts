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

/** One undo level: applied levels are undoable, the rest are redo. */
export interface UndoHistoryEntry {
  index: number;
  name: string;
  scopes: UndoScope[];
  /** Milliseconds since the Unix epoch. */
  timestamp: number;
  applied: boolean;
}

/** Opaque token that closes one owned undo gesture. */
export type UndoGestureToken = string & { readonly __undoGestureToken: unique symbol };
