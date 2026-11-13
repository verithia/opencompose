/**
 * The session's permission modes for calls that change the project DSL. The
 * backend loop gates every call from the stored mode; the panel only shows and
 * stores it.
 */

/** What a session may do to the DSL without asking. Wire names are these. */
export type PermissionMode = "require_approval" | "autoreview" | "write";

export const PERMISSION_MODES: readonly PermissionMode[] = [
  "require_approval",
  "autoreview",
  "write",
];

/**
 * The mode a session with no stored setting reads as. It governs automatic
 * writes only: a `confirm` call asks in every mode, because a fresh
 * conversation has nobody who has seen a deletion before.
 */
export const DEFAULT_PERMISSION: PermissionMode = "write";

/** A stored or received value as a mode; anything unknown reads as the default. */
export function permissionOf(value: unknown): PermissionMode {
  return value === "require_approval" || value === "autoreview" || value === "write"
    ? value
    : DEFAULT_PERMISSION;
}
