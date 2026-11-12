import type { I18nKey } from "../stores/i18n";
import type { Project } from "../types/project";

/**
 * What the approval card shows about one destructive call. The loop decides
 * *whether* to ask (the registry's `approval` field); this module only turns
 * the call's arguments into the object and the consequence the user reads.
 */

/** The object a destructive call names, resolved against the open project. */
export function approvalTarget(tool: string, args: Record<string, unknown>, project: Project): string | null {
  const name = (value: unknown) => (typeof value === "string" && value.trim() ? value : null);
  const index = typeof args.index === "number" ? args.index : null;
  switch (tool) {
    case "tracks_remove":
      return (index === null ? null : project.tracks[index]?.name) ?? null;
    case "sections_remove":
    case "presets_remove":
    case "buses_remove":
      return name(args.name);
    case "effects_remove": {
      const target = name(args.target);
      if (!target || index === null) return target;
      const effect = target === "master"
        ? project.master?.effects[index]
        : target.startsWith("track:")
          ? project.tracks[Number(target.slice("track:".length))]?.effects[index]
          : target.startsWith("bus:")
            ? project.buses.find(bus => bus.name === target.slice("bus:".length))?.effects[index]
            : undefined;
      return effect ? `${target} ${effect.effect_type} ${index + 1}` : `${target} ${index + 1}`;
    }
    case "plugins_apply_state":
      return name(args.preset);
    case "project_new":
      return project.name;
    case "project_open":
      return name(args.path);
    default:
      // Every other DSL-modifying call names the subtree or object it touches in
      // one of these fields; a call that names nothing keeps the generic line.
      return name(args.path) ?? name(args.target) ?? name(args.track) ?? name(args.name);
  }
}

/** The consequence line for a destructive tool; other edits say they change the DSL. */
const EFFECT: Readonly<Record<string, I18nKey>> = {
  tracks_remove: "ai.approval.effect.tracks_remove",
  buses_remove: "ai.approval.effect.buses_remove",
  sections_remove: "ai.approval.effect.sections_remove",
  presets_remove: "ai.approval.effect.presets_remove",
  effects_remove: "ai.approval.effect.effects_remove",
  project_new: "ai.approval.effect.project_new",
  project_open: "ai.approval.effect.project_open",
};

export function approvalEffect(tool: string): I18nKey {
  return EFFECT[tool] ?? "ai.approval.effect.dsl";
}
