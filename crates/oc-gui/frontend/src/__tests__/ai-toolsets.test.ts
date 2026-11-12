import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "./setup";
import { defaultSelection, loadToolsets, toolsetArg } from "../ai/toolsets";

const SETS = [
  { id: "dsl-edit", title: "DSL edit", description: "Subtree edits", tools: ["edit_subtree"], default_on: true },
  { id: "project-edit", title: "Project edit", description: "Tracks and mix", tools: ["tracks_mix"], default_on: true },
  { id: "transport", title: "Transport", description: "Playhead", tools: ["transport_play"], default_on: false },
];

describe("toolsets", () => {
  beforeEach(() => { invokeMock.mockClear(); });

  it("starts from the sets the registry marks default", () => {
    expect(defaultSelection(SETS)).toEqual(["dsl-edit", "project-edit"]);
    expect(defaultSelection([])).toEqual([]);
  });

  it("distinguishes an unknown selection from an empty one", () => {
    // Unknown: keep the whole surface, so an older backend never loses tools.
    expect(toolsetArg(null)).toEqual({});
    // Empty: the user deselected everything, which is an answer, not an error.
    expect(toolsetArg([])).toEqual({ toolsets: [] });
    expect(toolsetArg(["dsl-edit"])).toEqual({ toolsets: ["dsl-edit"] });
  });

  it("survives a backend without the switchboard", async () => {
    invokeMock.mockRejectedValueOnce(new Error("unknown command ai_toolsets"));
    expect(await loadToolsets()).toEqual([]);
    invokeMock.mockResolvedValueOnce({});
    expect(await loadToolsets()).toEqual([]);
    invokeMock.mockResolvedValueOnce(SETS);
    expect(await loadToolsets()).toEqual(SETS);
  });
});
