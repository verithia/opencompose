/**
 * The session's permission for a call that changes the project DSL, and the one
 * place that answers what a call needs. A new tool cannot bypass the gate: the
 * registry's `mutation` flag and the panel-local `edit_subtree` are the whole
 * set of modifying calls, and every surface reads the decision from here.
 */
import type { ToolDef } from "./agent";

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

/** What the mode requires before a DSL-modifying call runs. */
export type DslNeed = "none" | "approval" | "review";

/** Whether one call changes the DSL, and what the session's mode needs for it. */
export interface DslCall {
  modifies: boolean;
  need: DslNeed;
}

/**
 * The decision for one call. A read-only tool answers `{modifies:false}` in
 * every mode. A `confirm` tool asks unless `allowed` already carries it: those
 * calls delete an object the user built, so the mode cannot silently lower that
 * floor, and the choices are "ask", "always allow this tool in this
 * conversation", or an explicit `require_approval` mode that asks for every
 * change. Every other modifying call asks under `require_approval`, is reviewed
 * under `autoreview`, and runs directly under `write`.
 */
export function dslCall(
  tool: Pick<ToolDef, "name" | "mutation" | "approval"> | undefined,
  mode: PermissionMode,
  allowed: readonly string[] = [],
): DslCall {
  if (!tool) return { modifies: false, need: "none" };
  // A `confirm` tool also changes the project, so a tool added with that policy
  // but without `mutation: true` is still gated.
  const modifies =
    tool.mutation === true || tool.name === "edit_subtree" || tool.approval === "confirm";
  if (!modifies) return { modifies: false, need: "none" };
  if (tool.approval === "confirm" && !allowed.includes(tool.name)) {
    return { modifies: true, need: "approval" };
  }
  return {
    modifies: true,
    need: mode === "require_approval" ? "approval" : mode === "autoreview" ? "review" : "none",
  };
}

/**
 * The subtree or object a call's arguments name, when they name one. It is the
 * path the approval card shows and the hint a review row renders.
 */
export function callPath(
  tool: string | undefined,
  args: Record<string, unknown>,
): string | undefined {
  const named = (value: unknown) =>
    typeof value === "string" && value.trim() ? value.trim() : undefined;
  if (tool === "edit_subtree") return named(args.path);
  for (const key of ["path", "target", "track"]) {
    const value = named(args[key]);
    if (value) return value;
  }
  return undefined;
}
