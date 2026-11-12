import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, expectInvoked } from "../setup";
import { store } from "../../stores/project";
import { ChannelStrips } from "../../components/ChannelStrips";

const SEND_AMOUNT = 10 ** (-6 / 20);

function projectSnapshot() {
  return makeSnapshot({
    tracks: [
      {
        ...makeTrack("Piano", 0),
        mix: {
          volume_db: -3,
          pan: 0.25,
          sends: [{ target: "reverb_bus", amount: SEND_AMOUNT, pre_fader: true }],
          mute: false,
          solo: false,
          polarity_inverted: false,
          overrides: { display_name: "Keys", color: "#ff8800", solo_safe: false },
        },
      },
      makeTrack("Drums", 1),
    ],
    buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
    master: { effects: [], gain_automation: [], loudness_target: null, volume_db: 0 },
  });
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "preset_output_pairs") return 4;
    if (cmd === "preset_output_ports") {
      return [
        { channels: 2, first_pair: 0, pairs: 1 },
        { channels: 2, first_pair: 1, pairs: 1 },
      ];
    }
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    return projectSnapshot();
  });
  await store.initProject();
});

const renderStrips = () =>
  render(() => <ChannelStrips selectedNode="track:0" onSelectNode={() => {}} />);

describe("ChannelStrips", () => {
  it("renders a send row with level in dB and pre tap state", () => {
    const { getByLabelText } = renderStrips();
    const level = getByLabelText("Send level to reverb_bus") as HTMLInputElement;
    expect(Number(level.value)).toBeCloseTo(-6, 1);
    const tap = getByLabelText("Send to reverb_bus pre-fader");
    expect(tap.getAttribute("aria-pressed")).toBe("true");
  });

  it("toggles pre/post through one project mutation", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.click(getByLabelText("Send to reverb_bus pre-fader"));
    await vi.waitFor(() => {
      expectInvoked("set_send", {
        trackIndex: 0,
        busName: "reverb_bus",
        amount: null,
        preFader: false,
      });
    });
  });

  it("updates the send level in dB", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.change(getByLabelText("Send level to reverb_bus"), { target: { value: "-12" } });
    await vi.waitFor(() => {
      const call = (invokeMock.mock.calls as [string, Record<string, unknown>][])
        .filter(([cmd]) => cmd === "set_send")
        .pop();
      expect(call?.[1].trackIndex).toBe(0);
      expect(call?.[1].busName).toBe("reverb_bus");
      expect(call?.[1].amount as number).toBeCloseTo(10 ** (-12 / 20), 6);
    });
  });

  it("adds a send from another channel", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.change(getByLabelText("Add send target for Drums"), {
      target: { value: "reverb_bus" },
    });
    fireEvent.click(getByLabelText("Add send from Drums"));
    await vi.waitFor(() => {
      expectInvoked("add_send", {
        trackIndex: 1,
        busName: "reverb_bus",
        amount: 0.25,
        preFader: false,
      });
    });
  });

  it("toggles solo-safe and polarity as channel overrides", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.click(getByLabelText("Solo safe Piano"));
    await vi.waitFor(() => {
      expectInvoked("set_track_channel_overrides", {
        trackIndex: 0,
        displayName: null,
        color: null,
        soloSafe: true,
        polarityInverted: null,
      });
    });
    fireEvent.click(getByLabelText("Invert polarity Piano"));
    await vi.waitFor(() => {
      expectInvoked("set_track_channel_overrides", {
        trackIndex: 0,
        displayName: null,
        color: null,
        soloSafe: null,
        polarityInverted: true,
      });
    });
  });

  it("renames the track without changing its routing identity", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.change(getByLabelText("Channel name for Piano"), {
      target: { value: "Keys Wide" },
    });
    // The strip edits the track's one name, so the timeline and graph agree.
    await vi.waitFor(() => {
      expectInvoked("rename_track", { trackIndex: 0, newName: "Keys Wide" });
    });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_track_channel_overrides")).toBe(false);
  });

  it("shows the bus return into the master", () => {
    const { container } = renderStrips();
    const bus = container.querySelector('[data-channel-strip="bus:reverb_bus"]');
    expect(bus?.textContent).toContain("Return");
    expect(bus?.textContent).toContain("master");
  });

  it("shows the master strip and marks the selected channel", () => {
    const { container, getByLabelText } = renderStrips();
    expect(getByLabelText("Master fader")).toBeTruthy();
    expect(
      container.querySelector('[data-channel-strip="track:0"]')?.classList.contains("selected"),
    ).toBe(true);
  });

  it("edits the per-send pan through one project mutation", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.change(getByLabelText("Send pan to reverb_bus"), { target: { value: "-0.4" } });
    await vi.waitFor(() => {
      expectInvoked("set_send", {
        trackIndex: 0,
        busName: "reverb_bus",
        amount: null,
        preFader: null,
        pan: -0.4,
        sourceOwner: null,
        sourceOutput: null,
      });
    });
  });

  it("edits the bus return fader, mute and solo", async () => {
    const { getByLabelText } = renderStrips();
    fireEvent.change(getByLabelText("Bus fader reverb_bus"), { target: { value: "-4.5" } });
    await vi.waitFor(() => {
      expectInvoked("update_bus_mix", {
        name: "reverb_bus",
        volumeDb: -4.5,
        mute: null,
        solo: null,
      });
    });
    fireEvent.click(getByLabelText("Mute bus reverb_bus"));
    await vi.waitFor(() => {
      expectInvoked("update_bus_mix", {
        name: "reverb_bus",
        volumeDb: null,
        mute: true,
        solo: null,
      });
    });
    fireEvent.click(getByLabelText("Solo bus reverb_bus"));
    await vi.waitFor(() => {
      expectInvoked("update_bus_mix", {
        name: "reverb_bus",
        volumeDb: null,
        mute: null,
        solo: true,
      });
    });
  });

  it("routes a plugin track to a declared output port", async () => {
    const snapshot = makeSnapshot({
      presets: [{
        name: "Kit",
        source: "vst3",
        plugin: "test.kit",
        program: null,
        patch: null,
        file: null,
        root_note: null,
        extends: null,
        params: [],
        effects: [],
      }],
      tracks: [{ ...makeTrack("Kit", 1), preset: "Kit" }],
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preset_output_ports") {
        return [
          { channels: 2, first_pair: 0, pairs: 1 },
          { channels: 2, first_pair: 1, pairs: 1 },
        ];
      }
      return snapshot;
    });
    await store.initProject();

    const { getByLabelText } = renderStrips();
    const port = await vi.waitFor(() => {
      const select = getByLabelText("Output port Kit") as HTMLSelectElement;
      expect(select.options.length).toBe(2);
      return select;
    });
    fireEvent.change(port, { target: { value: "1" } });
    await vi.waitFor(() => {
      expectInvoked("set_track_output_port", { trackIndex: 0, port: 1 });
    });
    // The fetch must not load a heavy instrument just to list its ports.
    expectInvoked("preset_output_ports", { presetName: "Kit", load: false });
  });

  it("creates a plugin auxiliary send from the strip", async () => {
    const snapshot = makeSnapshot({
      presets: [{
        name: "Kit",
        source: "vst3",
        plugin: "test.kit",
        program: null,
        patch: null,
        file: null,
        root_note: null,
        extends: null,
        params: [],
        effects: [],
      }],
      tracks: [{ ...makeTrack("Kit", 1), preset: "Kit" }],
      buses: [{ name: "drums", routes: ["master"], effects: [] }],
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preset_output_pairs") return 4;
      if (cmd === "preset_output_ports") {
        return [
          { channels: 2, first_pair: 0, pairs: 1 },
          { channels: 2, first_pair: 1, pairs: 1 },
          { channels: 2, first_pair: 2, pairs: 1 },
          { channels: 2, first_pair: 3, pairs: 1 },
        ];
      }
      return snapshot;
    });
    await store.initProject();

    const { getByLabelText } = renderStrips();
    fireEvent.click(getByLabelText("Load auxiliary outputs for Kit"));
    await vi.waitFor(() => {
      expectInvoked("preset_output_pairs", { presetName: "Kit", load: true });
    });
    await vi.waitFor(() => {
      expect(getByLabelText("Send source for Kit")).toBeTruthy();
    });
    fireEvent.change(getByLabelText("Send source for Kit"), { target: { value: "3" } });
    fireEvent.change(getByLabelText("Add send target for Kit"), { target: { value: "drums" } });
    fireEvent.click(getByLabelText("Add send from Kit"));
    await vi.waitFor(() => {
      expectInvoked("add_send", {
        trackIndex: 0,
        busName: "drums",
        amount: 0.25,
        preFader: false,
        sourceOwner: "Kit",
        sourceOutput: 3,
        pan: null,
      });
    });
  });
});
