import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { createOwnedUndoFixture, invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { DrumKitPanel } from "../../components/DrumKitPanel";
import type { ParamValue, PresetDef } from "../../types/project";

const KIT_NAME = "Basic Kit";

function kitPreset(): PresetDef {
  const params: [string, ParamValue][] = [
    ["pad.kick.note", { Number: 36 }],
    ["pad.kick.gain", { Db: 0 }],
    ["pad.kick.one_shot", { Bool: true }],
    ["pad.snare.note", { Number: 38 }],
    ["pad.snare.one_shot", { Bool: true }],
    ["layer.kick.0.low", { Number: 1 }],
    ["layer.kick.0.high", { Number: 127 }],
    ["layer.kick.0.sample", { String: "samples/kick.wav" }],
    ["layer.snare.0.low", { Number: 1 }],
    ["layer.snare.0.high", { Number: 127 }],
    ["layer.snare.0.sample", { String: "samples/snare.wav" }],
  ];
  return {
    name: KIT_NAME,
    source: "builtin",
    plugin: "builtin.drumkit",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params,
    effects: [],
  };
}

function snapshot() {
  const track = {
    ...makeTrack("Drums", 0),
    instrument: { category: "drums", name: "drumkit" },
    preset: KIT_NAME,
    phrases: [],
  };
  return makeSnapshot({ presets: [kitPreset()], tracks: [track] });
}

beforeEach(async () => {
  invokeMock.mockClear();
  const owned = createOwnedUndoFixture();
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const ownership = owned.route(cmd, args);
    if (ownership) return ownership.value;
    cmd = owned.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "get_midi_activity") {
      return { running: false, events: 0, last: null, error: null, audition_active: false };
    }
    if (cmd === "get_project" || cmd === "set_preset_params") return snapshot();
    if (cmd === "set_loop_region" || cmd === "preview_drum_pad" || cmd === "release_web_audio") return undefined;
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    throw new Error(`Unexpected drum-panel fixture command: ${cmd}`);
  });
  await store.initProject();
  store.setSelectedTrackIndex(0);
});

describe("DrumKitPanel", () => {
  it("renders a 16-slot grid with names and samples", () => {
    const { container, getByText } = render(() => <DrumKitPanel />);
    expect(container.querySelectorAll(".drum-pad").length).toBe(16);
    expect(getByText("kick")).toBeInTheDocument();
    expect(getByText("snare")).toBeInTheDocument();
    expect(getByText("kick.wav")).toBeInTheDocument();
  });

  it("auditions a pad through the backend and flashes it", async () => {
    const { getByText, container } = render(() => <DrumKitPanel />);
    fireEvent.click(getByText("kick"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("preview_drum_pad", {
        presetName: KIT_NAME,
        note: 36,
        velocity: 110,
        startPaused: true,
      }),
    );
    const pad = container.querySelector('[data-pad-slot="0"]');
    expect(pad?.classList.contains("flashing")).toBe(true);
  });

  it("edits a pad through the preset mutation path", async () => {
    const { getByText, getByLabelText } = render(() => <DrumKitPanel />);
    fireEvent.click(getByText("kick"));
    const gain = getByLabelText("Gain dB") as HTMLInputElement;
    fireEvent.change(gain, { target: { value: "-6" } });
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "set_preset_params_owned");
      expect(call).toBeTruthy();
      expect(call![1].token).toEqual(expect.any(String));
      const params = call![1].params as [string, string | null][];
      expect(call![1].name).toBe(KIT_NAME);
      expect(params).toContainEqual(["pad.kick.gain", "-6dB"]);
    });
  });

  it("loads a dropped sample onto the pad under the cursor", async () => {
    const { container } = render(() => <DrumKitPanel />);
    const snareSlot = container.querySelector('[data-pad-slot="1"]')!;
    const file = new File(["x"], "snare_hit.wav", { type: "audio/wav" });
    fireEvent.drop(snareSlot, { dataTransfer: { files: [file] } });
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "set_preset_params_owned");
      expect(call).toBeTruthy();
      expect(call![1].token).toEqual(expect.any(String));
      const params = call![1].params as [string, string | null][];
      expect(params).toContainEqual(["layer.snare.0.sample", "snare_hit.wav"]);
    });
  });
});
