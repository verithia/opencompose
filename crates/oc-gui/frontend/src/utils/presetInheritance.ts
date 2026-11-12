import type { PresetDef } from "../types/project";

/** Resolve a display snapshot. All edits still target the authored child. */
export function resolvePresetForDisplay(
  preset: PresetDef,
  definitions: readonly PresetDef[],
): PresetDef | null {
  if (!preset.extends) return preset;
  const chain = [preset];
  const seen = new Set([preset.name]);
  let parentName: string | null = preset.extends;
  while (parentName) {
    if (seen.has(parentName)) return null;
    const parent = definitions.find((candidate) => candidate.name === parentName);
    if (!parent) return null;
    chain.push(parent);
    seen.add(parentName);
    parentName = parent.extends;
  }

  const resolved = { ...chain.pop()! };
  resolved.params = [...resolved.params];
  resolved.effects = [...resolved.effects];
  for (const child of chain.reverse()) {
    for (const field of ["source", "plugin", "program", "patch", "file", "root_note"] as const) {
      // These fields retain the parent's value unless the child authors one.
      const value = child[field];
      if (value != null) Object.assign(resolved, { [field]: value });
    }
    for (const param of child.params) {
      const index = resolved.params.findIndex(([key]) => key === param[0]);
      if (index < 0) resolved.params.push(param);
      else resolved.params[index] = param;
    }
    resolved.effects.push(...child.effects);
  }
  return { ...resolved, name: preset.name, extends: null };
}
