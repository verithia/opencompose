/**
 * The sidechain key list follows the validator's owner rule: a track chain
 * reads a track only, while a bus or the master reads tracks and buses.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { loadAuditProject, mountSurface } from "./ui-audit-support";
import { makeEffect, makeSnapshot, makeTrack } from "./setup";
import { ParametricEqEditor } from "../components/ParametricEqEditor";
import type { DeviceDef } from "../types/devices";

const device: DeviceDef = { id: "dynamic-eq", name: "Dynamic EQ", category: "eq", params: [] };

function project() {
  return makeSnapshot({
    tracks: [makeTrack("lead", 0), makeTrack("drum", 1)],
    buses: [
      { name: "reverb", routes: ["master"], effects: [] },
      { name: "plate", routes: ["master"], effects: [] },
    ],
  });
}

function editor(target: string) {
  return (
    <ParametricEqEditor target={target} index={0} effect={makeEffect("DynamicEq", [])} device={device} />
  );
}

function sidechainSelect(container: HTMLElement): HTMLSelectElement {
  const select = container.querySelector<HTMLSelectElement>("select.peq-sidechain");
  if (!select) throw new Error("the editor has no sidechain dropdown");
  return select;
}

function groupLabels(select: HTMLSelectElement): string[] {
  return [...select.querySelectorAll("optgroup")].map((group) => group.getAttribute("label") ?? "");
}

function groupValues(select: HTMLSelectElement, label: string): string[] {
  const group = [...select.querySelectorAll("optgroup")].find(
    (candidate) => candidate.getAttribute("label") === label,
  );
  return [...(group?.querySelectorAll("option") ?? [])].map((option) => option.value);
}

function optionValues(select: HTMLSelectElement): string[] {
  return [...select.querySelectorAll("option")].map((option) => option.value);
}

beforeEach(async () => {
  await loadAuditProject(project());
});

describe("sidechain key list", () => {
  it("offers no bus to a track chain", async () => {
    const view = await mountSurface(() => editor("track:0"));
    const select = sidechainSelect(view.container);

    expect(groupLabels(select)).toEqual(["Tracks"]);
    expect(optionValues(select)).toEqual(["", "drum"]);
    expect(optionValues(select)).not.toContain("reverb");
    expect(optionValues(select)).not.toContain("plate");
  });

  it("offers tracks and buses to a bus chain", async () => {
    const view = await mountSurface(() => editor("bus:reverb"));
    const select = sidechainSelect(view.container);

    expect(groupLabels(select)).toEqual(["Tracks", "Buses"]);
    expect(groupValues(select, "Tracks")).toEqual(["lead", "drum"]);
    expect(groupValues(select, "Buses")).toEqual(["plate"]);
  });

  it("offers tracks and buses to the master chain", async () => {
    const view = await mountSurface(() => editor("master"));
    const select = sidechainSelect(view.container);

    expect(groupLabels(select)).toEqual(["Tracks", "Buses"]);
    expect(groupValues(select, "Buses")).toEqual(["reverb", "plate"]);
  });
});
