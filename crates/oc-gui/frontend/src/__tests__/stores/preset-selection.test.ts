import { beforeEach, describe, expect, it, vi } from "vitest";
import { store } from "../../stores/project";
import { spaceCommand } from "../../stores/panels";
import type { UndoGestureToken } from "../../stores/api";
import { invokeMock, makeSnapshot } from "../setup";

const snapshot = (revision: number, hasPreset = true) => ({
  ...makeSnapshot(hasPreset ? {} : { presets: [] }),
  revision,
});

function defaultReply(command: string) {
  switch (command) {
    case "get_project": return snapshot(10);
    case "set_loop_region": return { enabled: false, start_beats: null, end_beats: null };
    case "get_undo_state": return { can_undo: false, can_redo: false, undo_label: null, redo_label: null };
    case "get_undo_history": return [];
    default: throw new Error(`Unexpected preset-selection command: ${command}`);
  }
}

beforeEach(async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string) => defaultReply(command));
  await store.initProject();
});

describe("owned preset request routing", () => {
  const token = "stack:document:gesture" as UndoGestureToken;
  const name = "Piano 1";

  it.each([
    {
      command: "set_preset_param_owned",
      run: () => store.setPresetParam(name, "amp.attack", "0.1", token),
      args: { name, paramName: "amp.attack", paramValue: "0.1", token },
    },
    {
      command: "set_preset_params_owned",
      run: () => store.setPresetParams(name, [["amp.attack", "0.2"]], false, token),
      args: { name, params: [["amp.attack", "0.2"]], token },
    },
    {
      command: "set_preset_params_owned",
      run: () => store.loadSynthPatch(name, "Owned patch", [], [], token),
      args: { name, params: [], replaceSynth: true, effects: [], patchLabel: "Owned patch", token },
    },
    {
      command: "randomize_preset_owned",
      run: () => store.randomizePreset(name, token),
      args: { name, seed: expect.any(Number), token },
    },
    {
      command: "synth_macro_learn_owned",
      run: () => store.macroLearn(name, 1, "filter.cutoff", token),
      args: { name, macroIndex: 1, control: "filter.cutoff", token },
    },
  ])("forwards the live token through $command", async ({ command, run, args }) => {
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (requested: string) => {
      if (requested === command) return requested === "synth_macro_learn_owned"
        ? { snapshot: snapshot(11), result: { status: "bound", slot: 1, destination: 0, source: 3 } }
        : snapshot(11);
      return defaultReply(requested);
    });
    await run();
    expect(invokeMock).toHaveBeenCalledWith(command, args);
    expect(store.projectRevision).toBe(11);
  });

  it("does not publish a delayed owned edit into a replacement document", async () => {
    let finish!: (value: ReturnType<typeof snapshot>) => void;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "set_preset_param_owned") return new Promise(resolve => { finish = resolve; });
      return defaultReply(command);
    });
    const oldWrite = store.setPresetParam(name, "amp.attack", "0.3", token);
    await vi.waitFor(() => expect(finish).toBeDefined());
    await store.initProject();
    finish({ ...snapshot(999), project: { ...snapshot(999).project, name: "Stale document" } });
    expect(await oldWrite).toContain("changed");
    expect(store.project.name).toBe("Test Project");
    expect(store.projectRevision).toBe(10);
  });
});

describe("preset selection ownership", () => {
  it("clears a preset pick on local document replacement even when its name survives", async () => {
    store.setSelectedPresetName("Piano 1");
    expect(store.selectedPresetName()).toBe("Piano 1");
    await store.initProject();
    expect(store.selectedPresetName()).toBeNull();
  });

  it("clears a preset pick on an external document replacement with the same names", () => {
    store.setSelectedPresetName("Piano 1");
    expect(store.applyExternalSnapshot(snapshot(11), true)).toBe(true);
    expect(store.selectedPresetName()).toBeNull();
  });

  it("drops a deleted preset without resurrecting its selection when undo restores it", () => {
    store.setSelectedPresetName("Piano 1");
    expect(store.applyExternalSnapshot(snapshot(11, false))).toBe(true);
    expect(store.selectedPresetName()).toBeNull();
    expect(store.applyExternalSnapshot(snapshot(12))).toBe(true);
    expect(store.selectedPresetName()).toBeNull();
  });

  it("keeps an existing selection through an ordinary edit", () => {
    store.setSelectedPresetName("Piano 1");
    expect(store.applyExternalSnapshot(snapshot(11))).toBe(true);
    expect(store.selectedPresetName()).toBe("Piano 1");
  });

  it("never offers a nonexistent preset as the Space target", () => {
    store.setSelectedPresetName("Missing preset");
    expect(store.selectedPresetName()).toBeNull();
    expect(spaceCommand("bottom", "presets", store.selectedPresetName(), null, false))
      .toEqual({ kind: "transport" });
  });
});
