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

/** The mismatch the panel must resolve before a turn, or null. */
export interface BindingMismatch {
  bound: string;
  open: string | null;
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

/** The bar a conversation that works on another file shows, or null. */
export function bindingBar(status: SessionBinding | null): BindingMismatch | null {
  if (!status || status.matches || !status.bound) return null;
  return { bound: status.bound, open: status.open };
}

/** The file name a binding line shows, never the whole path. */
export function projectName(path: string | null | undefined): string {
  if (!path) return "";
  return path.split(/[\\/]/).pop() || path;
}
