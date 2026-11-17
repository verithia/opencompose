/**
 * The project a saved conversation is bound to, beside the project that is
 * open. The backend compares the two canonically; the panel reads the answer
 * and never compares raw paths.
 */

export interface SessionBinding {
  bound: string | null;
  open: string | null;
  matches: boolean;
}

/** Validate the `sessions_binding` answer. */
export function sessionBinding(payload: unknown): SessionBinding | null {
  if (!payload || typeof payload !== "object") return null;
  const value = payload as { bound?: unknown; open?: unknown; matches?: unknown };
  if (value.bound !== null && typeof value.bound !== "string") return null;
  if (value.open !== null && typeof value.open !== "string") return null;
  if (typeof value.matches !== "boolean") return null;
  return { bound: value.bound, open: value.open, matches: value.matches };
}

/**
 * Whether the panel rebinds the conversation to the open project. The open
 * file is what the person works on, so the conversation follows it; the
 * backend saves the change as a notice the model reads with the next message.
 * With no project open there is nothing to follow.
 */
export function followsOpenProject(status: SessionBinding | null): boolean {
  return !!status && !status.matches && !!status.open;
}

/** The file name a binding line shows, never the whole path. */
export function projectName(path: string | null | undefined): string {
  if (!path) return "";
  return path.split(/[\\/]/).pop() || path;
}
