/**
 * Conversation titles by id, so the edit history can name the conversation
 * whose agent made an edit. The cache is filled by the panel when it has a
 * fresh list, and by this module on the first miss, so the History tab works
 * without the AI panel ever mounting.
 */
import { createSignal } from "solid-js";
import { aiApi } from "./api";
import { sessionList } from "../ai/sessions";

const [titles, setTitles] = createSignal<ReadonlyMap<string, string>>(new Map());
let loading: Promise<void> | null = null;
let attempted = false;

/** Replace the known titles with a fresh list. */
export function rememberSessionTitles(sessions: readonly { id: string; title: string }[]) {
  attempted = true;
  setTitles(new Map(sessions.map((session) => [session.id, session.title])));
}

function loadOnce(): void {
  if (attempted || loading) return;
  loading = aiApi.listSessions()
    .then((payload) => rememberSessionTitles(sessionList(payload)))
    .catch(() => { attempted = true; })
    .finally(() => { loading = null; });
}

/** The title of conversation `id`, loading the list once when it is unknown. */
export function sessionTitle(id: string): string | undefined {
  const found = titles().get(id);
  if (found === undefined) loadOnce();
  return found;
}
