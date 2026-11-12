/**
 * The per-project note that rides in the system prompt.
 *
 * The store keys it by the project's canonical path under
 * `~/.opencompose/ai/projects`, so two projects in one directory keep separate
 * notes. Reading it must never fail a turn: an unreadable or missing note is
 * simply no note.
 */
import { invoke } from "@tauri-apps/api/core";
import { PROJECT_NOTE_MAX_CHARS } from "./system-prompt";

export interface ProjectInstructions {
  /** The canonical project path the note belongs to; null when none is open. */
  project: string | null;
  text: string;
  updated?: string;
  /** The cap the store enforces; the panel shows this number, not its own. */
  max_chars: number;
}

export const NO_INSTRUCTIONS: ProjectInstructions = {
  project: null, text: "", max_chars: PROJECT_NOTE_MAX_CHARS,
};

/** A payload from the store, with any field it did not carry read as empty. */
export function instructionsOf(payload: unknown): ProjectInstructions {
  const value = (payload ?? {}) as Partial<ProjectInstructions>;
  return {
    project: typeof value.project === "string" ? value.project : null,
    text: typeof value.text === "string" ? value.text : "",
    ...(typeof value.updated === "string" ? { updated: value.updated } : {}),
    max_chars: typeof value.max_chars === "number" ? value.max_chars : PROJECT_NOTE_MAX_CHARS,
  };
}

/** The note for the open project, empty when it has none or cannot be read. */
export async function readProjectInstructions(project: string | null): Promise<ProjectInstructions> {
  try {
    return instructionsOf(await invoke("ai_project_instructions_get", { project }));
  } catch {
    return NO_INSTRUCTIONS;
  }
}

/**
 * Saves the note and answers what the store kept. A refusal (too long, or a
 * project with no file) is thrown, so the caller can show its message.
 */
export async function writeProjectInstructions(
  project: string | null,
  text: string,
): Promise<ProjectInstructions> {
  return instructionsOf(await invoke("ai_project_instructions_set", { project, text }));
}

/**
 * Whether the note is over the limit the store enforces. The store counts
 * characters, not UTF-16 units, so a note written with emoji is measured the
 * same way here as it will be there.
 */
export function noteIsOverLimit(text: string, limit: number): boolean {
  return [...text.trim()].length > limit;
}
