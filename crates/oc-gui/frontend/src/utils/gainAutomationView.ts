/** Read-only geometry adapts gain clips without changing their mixer stage. */
import type { AutomationLane, GainAutomation, Project } from "../types/project";
import { AUTOMATION_CURVES } from "../data/automationCurves.gen";
import {
  mappedAutomationValue,
  pointValue,
  type LaneDomain,
} from "./automationRow";

export function gainAutomationGroups(project: Project) {
  return [
    ...project.tracks.map((owner, index) => ({
      target: `track:${index}`,
      name: owner.mix.overrides?.display_name?.trim() || owner.name,
      curves: owner.gain_automation ?? [],
    })),
    ...project.buses.map((owner) => ({
      target: `bus:${owner.name}`,
      name: owner.name,
      curves: owner.gain_automation ?? [],
    })),
    ...(project.master
      ? [
          {
            target: "master",
            name: "master",
            curves: project.master.gain_automation ?? [],
          },
        ]
      : []),
  ].filter((group) => group.curves.length > 0);
}

/** This is drawing data only; no synthesized parameter name enters mutation APIs. */
export function gainAutomationView(
  curve: GainAutomation,
): Pick<AutomationLane, "points" | "value_mapping"> {
  const segments = curve.interpolation === "Segments";
  return {
    points: segments
      ? curve.points
      : curve.points.map((point) => ({
          ...point,
          curve: curve.interpolation === "Step" ? "stairs" : "linear",
        })),
    ...(segments && curve.value_mapping
      ? { value_mapping: curve.value_mapping }
      : {}),
  };
}

export function gainAutomationDomain(curve: GainAutomation): LaneDomain {
  const view = gainAutomationView(curve);
  let low = 1,
    high = 1;
  for (const point of view.points) {
    const value = mappedAutomationValue(
      pointValue(point.value) ?? 1,
      view.value_mapping,
    );
    if (Number.isFinite(value)) {
      low = Math.min(low, value);
      high = Math.max(high, value);
    }
  }
  const pad = Math.max((high - low) * 0.08, 0.01);
  return {
    min: Math.max(AUTOMATION_CURVES.gain.min, low - pad),
    max: Math.min(AUTOMATION_CURVES.gain.max, high + pad),
  };
}
