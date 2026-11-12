/**
 * Project-path references typed with `@` in the composer.
 *
 * A reference scopes one request's DSL context. It does not change the edit
 * target in Settings: the model reads the chosen subtree, while the tool that
 * applies an edit still names its own `target_path`.
 */
import type { Project } from "../types/project";

export interface ProjectReference {
  /** Path accepted by `ai_build_context` and `project_extract`. */
  path: string;
  kind: "project" | "track" | "preset" | "pattern" | "bus";
}

/** True when a name can be written as one path segment without quoting. */
function pathName(name: string): boolean {
  return name.length > 0 && !name.includes(".") && !name.includes("@");
}

/**
 * The paths a project exposes, in the order the palette shows them: the whole
 * project first, then tracks, presets, patterns and buses.
 */
export function projectReferences(project: Project): ProjectReference[] {
  const references: ProjectReference[] = [{ path: "project", kind: "project" }];
  const add = (kind: ProjectReference["kind"], names: Iterable<string>) => {
    const seen = new Set<string>();
    for (const name of names) {
      if (!pathName(name) || seen.has(name)) continue;
      seen.add(name);
      references.push({ path: `${kind}:${name}`, kind });
    }
  };
  add("track", project.tracks.map(track => track.name));
  add("preset", project.presets.map(preset => preset.name));
  add("pattern", project.patterns.map(pattern => pattern.name));
  add("bus", project.buses.map(bus => bus.name));
  return references;
}

/** The text after a trailing `@`, or `null` when the caret is not opening one. */
export function referenceQuery(draft: string): string | null {
  const match = /(^|\s)@([^\s@]*)$/.exec(draft);
  return match ? match[2].toLowerCase() : null;
}

/** Remove the trailing `@query` token after a reference is chosen or cancelled. */
export function withoutReferenceQuery(draft: string): string {
  return draft.replace(/(^|\s)@([^\s@]*)$/, "$1").trimEnd();
}

/** Filter by the path text, case-insensitively, in project order. */
export function filterReferences(
  references: readonly ProjectReference[],
  query: string,
): ProjectReference[] {
  const needle = query.toLowerCase();
  return references.filter(reference => reference.path.toLowerCase().includes(needle));
}
