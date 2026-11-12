import { invoke } from "./transport";

/** Call only after proposal validation. Only named global patterns support
 * insertion: missing track children, numeric indices and transport failures
 * must remain errors. Verify the unchanged snapshot before showing an add diff. */
export async function proposalOldDsl(path: string, expectedDsl: string): Promise<string> {
  try {
    const extracted = await invoke<{dsl_text: string}>("extract_subtree", {path});
    return extracted.dsl_text;
  } catch (error) {
    const name = /^pattern:([^.:]+)$/.exec(path)?.[1];
    const message = error instanceof Error ? error.message : String(error);
    if (!name || /^\d+$/.test(name) || message !== `Pattern '${name}' not found`) throw error;
    const snapshot = await invoke<{dsl_source: string; project: {patterns: {name:string}[]}}>("get_project");
    if (snapshot.dsl_source !== expectedDsl || !Array.isArray(snapshot.project?.patterns)
      || snapshot.project.patterns.some(pattern => pattern.name === name)) throw error;
    return "";
  }
}
