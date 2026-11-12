/**
 * The destructive calls a conversation answered "always allow" for. The choice
 * belongs to the conversation, not to the workspace: a fresh conversation has
 * no entry, so its first destructive call asks again. A `confirm` tool named
 * here runs without a card; the session's permission mode still governs every
 * other call.
 *
 * The list is a preference, not project data, so it lives in the browser's
 * storage beside the theme and the locale rather than in the saved session.
 */
const KEY = "opencompose.ai.allowed-tools";

/** Conversations kept; the oldest entry is dropped first. */
const MAX_CONVERSATIONS = 64;

type Allowed = Record<string, string[]>;

function read(): Allowed {
  if (typeof window === "undefined" || !window.localStorage) return {};
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    const allowed: Allowed = {};
    for (const [conversation, tools] of Object.entries(value as Record<string, unknown>)) {
      if (!Array.isArray(tools)) continue;
      const names = tools.filter((tool): tool is string => typeof tool === "string" && !!tool);
      if (names.length) allowed[conversation] = names;
    }
    return allowed;
  } catch {
    return {};
  }
}

function write(allowed: Allowed): void {
  if (typeof window === "undefined" || !window.localStorage) return;
  try {
    window.localStorage.setItem(KEY, JSON.stringify(allowed));
  } catch {
    // A full or blocked store costs the memory, never the turn.
  }
}

/** The tools this conversation has allowed, in the order they were allowed. */
export function allowedTools(conversation: string): string[] {
  return conversation ? read()[conversation] ?? [] : [];
}

/** Records one allowed tool and returns the conversation's new list. */
export function allowTool(conversation: string, tool: string): string[] {
  if (!conversation) return [];
  const stored = read();
  const next = stored[conversation]?.includes(tool)
    ? stored[conversation]
    : [...(stored[conversation] ?? []), tool];
  // The conversation goes last, so dropping the oldest entries is a slice.
  const entries = Object.entries(stored).filter(([id]) => id !== conversation);
  entries.push([conversation, next]);
  write(Object.fromEntries(entries.slice(-MAX_CONVERSATIONS)));
  return next;
}

/** Drops every stored choice; for a test or a settings reset. */
export function clearAllowedTools(): void {
  if (typeof window === "undefined" || !window.localStorage) return;
  try {
    window.localStorage.removeItem(KEY);
  } catch {
    // Nothing to clear.
  }
}
