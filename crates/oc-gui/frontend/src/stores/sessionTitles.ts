/**
 * Conversation titles by id, as the AI panel last listed them, so the edit
 * history can name the conversation whose agent made an edit.
 */
import { createSignal } from "solid-js";

const [titles, setTitles] = createSignal<ReadonlyMap<string, string>>(new Map());

/** Replace the known titles with the panel's newest list. */
export function rememberSessionTitles(sessions: readonly { id: string; title: string }[]) {
  setTitles(new Map(sessions.map((session) => [session.id, session.title])));
}

/** The title of conversation `id`, if the panel has listed it. */
export function sessionTitle(id: string): string | undefined {
  return titles().get(id);
}
