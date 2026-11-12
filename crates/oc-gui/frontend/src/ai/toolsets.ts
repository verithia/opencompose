/**
 * Toolsets: the named tool switches the panel offers per conversation.
 *
 * The Rust registry owns the definitions; this module only reads them and
 * derives the starting selection. An empty list means the backend does not
 * expose the switchboard (or the file could not be read), so the panel keeps
 * its whole surface instead of losing every tool.
 */
import { invoke } from "@tauri-apps/api/core";

export interface ToolsetDef {
  id: string;
  title: string;
  description: string;
  tools: string[];
  default_on: boolean;
}

export async function loadToolsets(): Promise<ToolsetDef[]> {
  try {
    const value = await invoke<unknown>("ai_toolsets");
    return Array.isArray(value) ? (value as ToolsetDef[]) : [];
  } catch {
    return [];
  }
}

export function defaultSelection(sets: ToolsetDef[]): string[] {
  return sets.filter(set => set.default_on).map(set => set.id);
}

/** Request argument: `undefined` keeps the whole surface, `[]` enables nothing. */
export function toolsetArg(selection: string[] | null): { toolsets?: string[] } {
  return selection ? { toolsets: selection } : {};
}
