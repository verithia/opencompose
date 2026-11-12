import { createOwnedUndoFixture } from "../setup";
import { readCachedPluginPanel } from "../../stores/pluginParamsCache";
import { openSynthRange, synthKnobTrigger } from "../helpers/synthRange";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, openDialogMock, saveDialogMock, makeSnapshot, makeTrack, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";

let parameterFixture = createOwnedUndoFixture();
beforeEach(async () => {
  parameterFixture = createOwnedUndoFixture();
  invokeMock.mockClear();
  openDialogMock.mockReset();
  saveDialogMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
  });
  await store.initProject();
});

describe("PresetEditor", () => {
  it("edits a child with inherited OpenSynth values and keeps parent inserts read-only", async () => {
    const base = makeSnapshot().project.presets[0];
    const snapshot = makeSnapshot({ presets: [
      {
        ...base, name: "Parent", source: "builtin", plugin: "builtin.open_synth",
        params: [["osc.a.level", { Number: 0.37 }]],
        effects: [{ effect_type: "Delay", params: [] }],
      },
      {
        ...base, name: "Child", source: null, plugin: null, program: null, extends: "Parent",
        effects: [{ effect_type: "Chorus", params: [] }],
      },
    ] });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "begin_parameter_gesture") return "inherited-child-owner";
      if (cmd === "end_owned_undo_gesture") return true;
      return snapshot;
    });
    await store.initProject();
    const { container, getByRole, getByText } = render(() => <PresetEditor />);
    selectPreset(container, "Child");
    const level = container.querySelector<HTMLElement>('[data-param="osc.a.level"]');
    expect(level?.dataset.automationValue).toBe("0.37");
    const knob = synthKnobTrigger(level!);
    knob.focus();
    fireEvent.keyDown(knob, { key: "Enter" });
    expect(knob).toHaveAttribute("aria-expanded", "true");
    fireEvent.change(openSynthRange(level!), { target: { value: "0.5" } });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
      token: "inherited-child-owner", name: "Child", paramName: "osc.a.level", paramValue: "0.5",
    }));
    fireEvent.click(getByRole("button", { name: "FX" }));
    expect(getByText("Inherited inserts")).toBeInTheDocument();
    const fx = container.querySelector(".synth-fx-view")!;
    expect(fx.querySelectorAll("[data-effect-slot]")).toHaveLength(1);
    expect(fx.querySelector("[data-effect-slot='0']")).toHaveTextContent("Chorus");
    expect(fx.querySelector(".effects-rack")).toHaveTextContent("Delay");
    fireEvent.click(fx.querySelector<HTMLButtonElement>("[data-effect-slot='0'] .effect-slot-remove")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("remove_effect", {
      target: "preset:Child", effectIndex: 0,
    }));
    fireEvent.click(getByRole("button", { name: "Patch actions" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Save to library…" }));
    const dialog = getByRole("dialog", { name: "Save to library" });
    fireEvent.input(dialog.querySelector("input")!, { target: { value: "Library child" } });
    fireEvent.click(getByRole("button", { name: "Save" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("save_patch_to_library",
      expect.objectContaining({ presetName: "Child", name: "Library child" }),
    ));
  });

  it("warms only the current native patch after selection and edits settle", async () => {
    const initial = makeSnapshot();
    const native = { ...initial.project.presets[0], source: "builtin" as const, plugin: "builtin.open_synth" };
    let snapshot = makeSnapshot({ presets: [
      { ...native, name: "First keys" },
      { ...native, name: "Second keys" },
    ] });
    invokeMock.mockImplementation(async (cmd: string) => cmd === "scan_plugins" ? [] : snapshot);
    await store.initProject();
    vi.useFakeTimers();
    const { container, unmount } = render(() => <PresetEditor />);
    try {
      selectPreset(container, "First keys");
      await vi.advanceTimersByTimeAsync(100);
      selectPreset(container, "Second keys");
      await vi.advanceTimersByTimeAsync(100);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "warm_open_synth_keyboard")).toEqual([]);
      snapshot = makeSnapshot({ presets: [
        snapshot.project.presets[0],
        { ...snapshot.project.presets[1], params: [["output.gain", { Number: 0.3 }]] },
      ] });
      await store.initProject();
      await vi.advanceTimersByTimeAsync(100);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "warm_open_synth_keyboard")).toEqual([]);
      await vi.advanceTimersByTimeAsync(100);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "warm_open_synth_keyboard"))
        .toEqual([["warm_open_synth_keyboard", { presetName: "Second keys" }]]);
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "open_synth_note_on")).toBe(false);
      selectPreset(container, "First keys");
      unmount();
      await vi.advanceTimersByTimeAsync(200);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "warm_open_synth_keyboard")).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows accessible pause/resume actions for the owning preset", async () => {
    await store.stop();
    const { container } = render(() => <PresetEditor />);
    const button = container.querySelector(".preset-preview-btn") as HTMLButtonElement;
    expect(button).toBeTruthy();
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "pause_audition" || cmd === "resume_audition") return {
        playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0,
        audition: { preset: "Piano 1", status: "ready", paused: cmd === "pause_audition", position_seconds: 1, ready_ms: 0, instances: 1, loads: 1, error: null },
      };
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    });
    fireEvent.click(button);
    await waitFor(() => expect(button.getAttribute("aria-label")).toBe("Pause preview"));
    expect(button.textContent).toBe("Ⅱ");
    fireEvent.click(button);
    await waitFor(() => expect(button.getAttribute("aria-label")).toBe("Resume preview"));
    expect(button.textContent).toBe("▶");
    fireEvent.click(button);
    await waitFor(() => expect(button.getAttribute("aria-label")).toBe("Pause preview"));
    await store.stop();
  });

  it("loads a hosted plugin when an existing preset changes source without changing its name", async () => {
    let snapshot = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "plugin_editor_status") return null;
      return snapshot;
    });
    await store.initProject();
    const { getByText, getByRole } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano 1"));
    snapshot = { ...snapshot, project: { ...snapshot.project, presets: snapshot.project.presets.map(p => ({ ...p, source: "vst3", plugin: "vst3:test" })) } };
    await store.initProject();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("load_plugin_for_preset", { presetName: "Piano 1" }));
    await waitFor(() => expect(getByRole("button", { name: "Open editor" })).toBeTruthy());
  });

  it.each([true, false])("loads with a suspended animation frame (manual=%s)", async (manual) => {
    const snapshot = makeSnapshot();
    snapshot.project.presets = snapshot.project.presets.map(p => ({ ...p, source: "vst3", plugin: `vst3:test-manual-${manual}` }));
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "plugin_editor_status") return null;
      return snapshot;
    });
    await store.initProject();
    vi.stubGlobal("requestAnimationFrame", () => 123);
    vi.stubGlobal("cancelAnimationFrame", () => {});
    try {
      const { getByText, getByRole, unmount } = render(() => <PresetEditor />);
      fireEvent.click(getByText("Piano 1"));
      if (manual) fireEvent.click(getByRole("button", { name: "Reload parameters" }));
      await waitFor(() => expect(getByRole("button", { name: "Open editor" })).toBeTruthy());
      expect(invokeMock).toHaveBeenCalledWith("load_plugin_for_preset", { presetName: "Piano 1" });
      unmount();
    } finally { vi.unstubAllGlobals(); }
  });

  it("records exposed plugin parameters with explicit gesture boundaries", async () => {
    const snapshot = makeSnapshot({
      presets: [{
        name: "Synth",
        source: "clap",
        plugin: "org.example.synth",
        program: null,
        patch: null,
        file: null,
        root_note: null,
        extends: null,
        params: [],
        effects: [],
      }],
      tracks: [{ ...makeTrack("Lead", 0), preset: "Synth" }],
    });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 5.5, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "load_plugin_for_preset" || cmd === "get_plugin_params") return [{
        id: 7,
        name: "Cutoff",
        module: "Filter",
        min: 20,
        max: 20_000,
        default: 1_000,
        current: 1_000,
        display: "1000 Hz",
        flags: 0,
      }];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "begin_guarded_plugin_param_gesture") return "direct-plugin-owner";
      if (cmd === "end_owned_plugin_param_gesture") return { closed: true, snapshot };
      return snapshot;
    });
    await store.initProject();
    const { getByText, getAllByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Synth"));
    await waitFor(() => expect(getAllByText("Filter").length).toBeGreaterThan(1));
    fireEvent.click(getAllByText("Filter").at(-1)!);
    const slider = getByLabelText("Automate Cutoff");
    fireEvent.pointerDown(slider);
    fireEvent.input(slider, { target: { value: "5000" } });
    fireEvent.change(slider, { target: { value: "5000" } });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("begin_guarded_plugin_param_gesture", { expectedStamp: "fixture-parameter-target",
        presetName: "Synth",
        trackIndex: 0,
        paramName: "Cutoff",
      });
      expect(invokeMock).toHaveBeenCalledWith("update_owned_plugin_param_gesture", {
        token: "direct-plugin-owner",
        presetName: "Synth",
        paramName: "Cutoff",
        value: 5000,
        position: { bar: 1, beat: 1, subdivision: 0 },
      });
      expect(invokeMock).toHaveBeenCalledWith("end_owned_plugin_param_gesture", {
        token: "direct-plugin-owner",
        presetName: "Synth",
        paramName: "Cutoff",
      });
    });
  });

  it("updates a SoundFont GM program in place", () => {
    const { getByText, getByRole } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.click(getByRole("button", { name: "Bright Acoustic Piano" }));

    expect(invokeMock).toHaveBeenCalledWith("set_preset_program", { name: "Piano 1", program: 1 });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_preset")).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "add_preset")).toBe(false);
  });

  it("shows the preparing state while the GM program lands", async () => {
    let release!: (snapshot: ReturnType<typeof makeSnapshot>) => void;
    const pending = new Promise<ReturnType<typeof makeSnapshot>>((resolve) => { release = resolve; });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "set_preset_program") return pending;
      if (cmd === "get_transport_state") {
        return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      }
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    await store.initProject();
    const { container, getByText, getByRole } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano 1"));
    const tile = getByRole("button", { name: "Bright Acoustic Piano" });
    fireEvent.click(tile);

    // The resync opens the device and prepares the bank: the click must say so.
    await waitFor(() => expect(tile).toBeDisabled());
    expect(tile.getAttribute("aria-busy")).toBe("true");
    expect(getByText("Preparing Bright Acoustic Piano…")).toBeTruthy();
    // Every other program is disabled too, so a second click cannot race it.
    const tiles = [...container.querySelectorAll<HTMLButtonElement>(".preset-program-btn")];
    expect(tiles.length).toBeGreaterThan(1);
    expect(tiles.every((button) => button.disabled)).toBe(true);

    release(makeSnapshot({ tracks: [makeTrack("Piano", 0)] }));
    await waitFor(() => expect(tile).not.toBeDisabled());
  });

  it("maps CC controls to plugin parameters on presets", async () => {
    const { getByText, getByLabelText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("CC number"), { target: { value: "64" } });
    fireEvent.input(getByLabelText("Plugin parameter name"), { target: { value: "Sustain Pedal" } });
    fireEvent.click(getByLabelText("Map CC parameter"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano 1",
        paramName: "cc_64_param",
        paramValue: "Sustain Pedal",
      });
    });
  });

  it("shows validation errors for invalid CC mappings", async () => {
    const { getByText, getByLabelText, queryByText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("CC number"), { target: { value: "12abc" } });
    fireEvent.input(getByLabelText("Plugin parameter name"), { target: { value: "Sustain Pedal" } });
    fireEvent.click(getByLabelText("Map CC parameter"));

    expect(queryByText("CC number must be 0-127.")).not.toBeNull();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_param" || cmd === "set_preset_param_owned")).toBe(false);
  });

  it("edits existing preset params in place", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "Piano 1",
          source: "soundfont",
          plugin: null,
          program: 0,
          patch: null,
          file: null,
          root_note: null,
          extends: null,
          params: [["cc_64_param", { String: "Sustain Pedal" }]],
          effects: [],
        }],
        tracks: [makeTrack("Piano", 0)],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("Param value cc_64_param"), { target: { value: "Damper" } });
    fireEvent.click(getByLabelText("Apply param cc_64_param"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano 1",
        paramName: "cc_64_param",
        paramValue: "Damper",
      });
    });
  });

  it("renames existing preset params atomically", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "Piano 1",
          source: "soundfont",
          plugin: null,
          program: 0,
          patch: null,
          file: null,
          root_note: null,
          extends: null,
          params: [["cc_64_param", { String: "Sustain Pedal" }]],
          effects: [],
        }],
        tracks: [makeTrack("Piano", 0)],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("Param name cc_64_param"), { target: { value: "cc_66_param" } });
    fireEvent.input(getByLabelText("Param value cc_64_param"), { target: { value: "Sostenuto" } });
    fireEvent.click(getByLabelText("Apply param cc_64_param"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_params_owned", { token: expect.any(String),
        name: "Piano 1",
        params: [["cc_64_param", null], ["cc_66_param", "Sostenuto"]],
      });
    });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_preset_param" || cmd === "set_preset_param_owned")).toBe(false);
  });

  it("picks loaded plugin parameters for CC mappings", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [
        { id: 1, name: "Filter Cutoff", module: "Main", min: 0, max: 1, default: 0.5, current: 0.25, display: "25%", flags: 0 },
        { id: 2, name: "Sustain Pedal", module: "Main", min: 0, max: 1, default: 0, current: 0, display: "Off", flags: 0 },
      ];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "ClapSynth", source: "clap", plugin: "vendor.clap", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("ClapSynth"));
    await waitFor(() => getByLabelText("Plugin parameter picker"));
    fireEvent.input(getByLabelText("CC number"), { target: { value: "74" } });
    fireEvent.change(getByLabelText("Plugin parameter picker"), { target: { value: "Filter Cutoff" } });
    fireEvent.click(getByLabelText("Map CC parameter"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "ClapSynth",
        paramName: "cc_74_param",
        paramValue: "Filter Cutoff",
      });
    });
  });

  it("sets VST3 state file preset params", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "Synth", source: "vst3", plugin: "vendor.synth", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Synth"));
    fireEvent.change(getByLabelText("VST3 state file"), { target: { value: "/tmp/synth.vstpreset" } });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Synth",
        paramName: "vst3_state_file",
        paramValue: "/tmp/synth.vstpreset",
      });
    });
  });

  it("browses for VST3 state files", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "Synth", source: "vst3", plugin: "vendor.synth", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    openDialogMock.mockResolvedValue("/tmp/browser-state.vstpreset");
    await store.initProject();

    const { getByText, getAllByText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Synth"));
    fireEvent.click(getAllByText("Browse")[0]);

    await waitFor(() => {
      expect(openDialogMock).toHaveBeenCalledWith({
        multiple: false,
        filters: [{ name: "Instrument settings file", extensions: ["vstpreset", "bin"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Synth",
        paramName: "vst3_state_file",
        paramValue: "/tmp/browser-state.vstpreset",
      });
    });
  });

  it("adds scanned VST3 instruments as VST3 presets", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [{
        id: "vendor.vital",
        name: "Vital",
        vendor: "Vendor",
        format: "Vst3",
        path: "/Library/Audio/Plug-Ins/VST3/Vital.vst3",
        category: "Instrument",
      }];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    await store.initProject();

    const { getByLabelText, getByPlaceholderText, getByText } = render(() => <PresetEditor />);
    fireEvent.click(getByLabelText("Add preset"));
    fireEvent.input(getByPlaceholderText("Search sounds…"), { target: { value: "Vital" } });
    await waitFor(() => getByText("Vital"));
    fireEvent.click(getByText("Vital"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("add_preset", {
        name: "Vital 1",
        source: "vst3",
        program: null,
        plugin: "vendor.vital",
        patch: null,
        file: null,
      });
    });
  });

  it("sets CLAP state file preset params", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "ClapSynth", source: "clap", plugin: "vendor.clap", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("ClapSynth"));
    fireEvent.change(getByLabelText("CLAP state file"), { target: { value: "/tmp/synth.clapstate" } });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "ClapSynth",
        paramName: "clap_state_file",
        paramValue: "/tmp/synth.clapstate",
      });
    });
  });

  it("saves loaded CLAP state files", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "ClapSynth", source: "clap", plugin: "vendor.clap", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    saveDialogMock.mockResolvedValue("/tmp/captured.clapstate");
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("ClapSynth"));
    fireEvent.click(getByLabelText("Save CLAP state file"));

    await waitFor(() => {
      expect(saveDialogMock).toHaveBeenCalledWith({
        filters: [{ name: "CLAP state file", extensions: ["clapstate", "bin"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_file", {
        presetName: "ClapSynth",
        path: "/tmp/captured.clapstate",
      });
    });
  });

  it("embeds loaded CLAP state in the project", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "ClapSynth", source: "clap", plugin: "vendor.clap", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("ClapSynth"));
    fireEvent.click(getByLabelText("Embed CLAP state"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_embedded", {
        presetName: "ClapSynth",
      });
    });
  });

  it("saves and embeds VST3 state through shared plugin-state commands", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "Vital 1", source: "vst3", plugin: "vendor.vital", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    saveDialogMock.mockResolvedValue("/tmp/captured.vstpreset");
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Vital 1"));
    fireEvent.click(getByLabelText("Save VST3 state file"));
    fireEvent.click(getByLabelText("Embed VST3 state"));

    await waitFor(() => {
      expect(saveDialogMock).toHaveBeenCalledWith({
        filters: [{ name: "VST3 state file", extensions: ["vstpreset", "bin"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_file", {
        presetName: "Vital 1",
        path: "/tmp/captured.vstpreset",
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_embedded", {
        presetName: "Vital 1",
      });
    });
  });

  it("saves and embeds AU state through the shared plugin-state commands", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "Piano", source: "au", plugin: "Apple: DLSMusicDevice", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    saveDialogMock.mockResolvedValue("/tmp/piano.aupreset");
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano"));
    fireEvent.change(getByLabelText("AU state file"), { target: { value: "/tmp/piano.aupreset" } });
    fireEvent.click(getByLabelText("Save AU state file"));
    fireEvent.click(getByLabelText("Embed AU state"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano",
        paramName: "au_state_file",
        paramValue: "/tmp/piano.aupreset",
      });
      expect(saveDialogMock).toHaveBeenCalledWith({
        filters: [{ name: "AU state file", extensions: ["aupreset", "bin"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_file", {
        presetName: "Piano",
        path: "/tmp/piano.aupreset",
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_embedded", {
        presetName: "Piano",
      });
    });
  });

  it("saves and embeds LV2 state through the shared plugin-state commands", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{ name: "Gain", source: "lv2", plugin: "http://example.test/gain", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] }],
      });
    });
    saveDialogMock.mockResolvedValue("/tmp/gain.lv2state");
    await store.initProject();

    const { getByText, getByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Gain"));
    fireEvent.change(getByLabelText("LV2 state file"), { target: { value: "/tmp/gain.lv2state" } });
    fireEvent.click(getByLabelText("Save LV2 state file"));
    fireEvent.click(getByLabelText("Embed LV2 state"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Gain",
        paramName: "lv2_state_file",
        paramValue: "/tmp/gain.lv2state",
      });
      expect(saveDialogMock).toHaveBeenCalledWith({
        filters: [{ name: "LV2 state file", extensions: ["lv2state", "bin"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_file", {
        presetName: "Gain",
        path: "/tmp/gain.lv2state",
      });
      expect(invokeMock).toHaveBeenCalledWith("save_plugin_state_embedded", {
        presetName: "Gain",
      });
    });
  });

  it("shows no plugin state group for a built-in engine", async () => {
    const { getByText, queryByText, queryByLabelText } = render(() => <PresetEditor />);
    fireEvent.click(getByText("Piano 1"));

    expect(queryByText("Plugin state")).toBeNull();
    expect(queryByLabelText("AU state file")).toBeNull();
  });

  it("sets sustain-aware articulation release trigger params", async () => {
    const { getByText, getByLabelText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("Release note"), { target: { value: "D0" } });
    fireEvent.click(getByLabelText("Sustain-aware release"));
    fireEvent.click(getByLabelText("Map release trigger"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_params_owned", { token: expect.any(String),
        name: "Piano 1",
        params: [
          ["articulation_legato_release_note", "D0"],
          ["articulation_legato_release_velocity", "12"],
          ["articulation_legato_release_duration_ms", "30"],
          ["articulation_legato_release_sustain_aware", "true"],
        ],
      });
    });
  });

  it("sets microtuning note offset params", async () => {
    const { getByText, getByLabelText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("Microtuning note"), { target: { value: "C4" } });
    fireEvent.input(getByLabelText("Microtuning cents"), { target: { value: "-12.5" } });
    fireEvent.click(getByLabelText("Map microtuning"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano 1",
        paramName: "microtuning_C4_cents",
        paramValue: "-12.5",
      });
    });
  });

  it("sets 14-bit CC mapping params", async () => {
    const { getByText, getByLabelText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("14-bit control"), { target: { value: "dynamics" } });
    fireEvent.input(getByLabelText("14-bit CC number"), { target: { value: "1" } });
    fireEvent.click(getByLabelText("Map 14-bit CC"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano 1",
        paramName: "control_dynamics_cc14",
        paramValue: "1",
      });
    });
  });

  it("sets Scala tuning file params", async () => {
    const { getByText, getByLabelText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.input(getByLabelText("Scala tuning file"), { target: { value: "/tmp/tuning.scl" } });
    fireEvent.input(getByLabelText("Scala root note"), { target: { value: "C4" } });
    fireEvent.click(getByLabelText("Map Scala tuning"));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_params_owned", { token: expect.any(String),
        name: "Piano 1",
        params: [
          ["scala_file", "/tmp/tuning.scl"],
          ["scala_root_note", "C4"],
        ],
      });
    });
  });


  it("gives a VST2 preset the generated panel instead of the flat table", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "load_plugin_for_preset" || cmd === "get_plugin_params") return [
        { id: 0, name: "Threshold", module: "VST2", min: 0, max: 1, default: 0.5, current: 0.73, display: "-12.3 dB", flags: 0 },
        { id: 1, name: "Ratio", module: "VST2", min: 0, max: 1, default: 0.5, current: 0.56, display: "4.0:1", flags: 0 },
      ];
      if (cmd === "plugin_has_gui") return false;
      if (cmd === "plugin_parameter_panel") {
        return {
          preset: "Comp",
          format: "vst2",
          surface: {
            kind: "parameters",
            parameters: [{
              index: 0,
              name: "Threshold",
              unit: "dB",
              min: 0,
              max: 1,
              current: 0.73,
              display: "-12.3 dB",
              normalized: 0.73,
              default: null,
              steps: null,
              automatable: true,
              by_passing: false,
              group: null,
            }],
          },
        };
      }
      return makeSnapshot({
        presets: [{
          name: "Comp",
          source: "vst2",
          plugin: "vst2:comp",
          program: null,
          patch: null,
          file: null,
          root_note: null,
          extends: null,
          params: [],
          effects: [],
        }],
        tracks: [{ ...makeTrack("Bus", 0), preset: "Comp" }],
      });
    });
    await store.initProject();

    const { container } = render(() => <PresetEditor />);
    selectPreset(container, "Comp");

    await waitFor(
      () => expect(container.querySelector(".plugin-panel")).not.toBeNull(),
      { timeout: 3000 },
    );
    // One control, and no second flat table with the same parameters.
    expect(container.querySelectorAll(".plugin-panel-row")).toHaveLength(1);
    expect(container.querySelectorAll(".plugin-param-row")).toHaveLength(0);
    expect(container.querySelector(".plugin-panel-value")?.textContent).toBe("-12.3 dB");
  });

  it("re-reads plugin parameters only when the plugin reports a change", async () => {
    let paramsChanged = false;
    const snapshot = makeSnapshot({
      presets: [{
        name: "Synth",
        source: "vst3",
        plugin: "fixture.vst3",
        program: null,
        patch: null,
        file: null,
        root_note: null,
        extends: null,
        params: [],
        effects: [],
      }],
      tracks: [{ ...makeTrack("Lead", 0), preset: "Synth" }],
    });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const admission = parameterFixture.route(cmd, args);
      if (admission) return admission.value;
      cmd = parameterFixture.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "load_plugin_for_preset") return [{
        id: 1, name: "Gain", module: "VST3", min: 0, max: 1,
        default: 0, current: 0.5, display: "0.5000", flags: 0,
      }];
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "plugin_editor_status") return null;
      if (cmd === "open_plugin_gui") return [800, 600];
      if (cmd === "poll_plugin_gui_closed") return false;
      if (cmd === "poll_plugin_host_events") return {
        events: paramsChanged ? ["ParamValuesChanged"] : [],
        dirty: false,
        open_editor: null,
        params_changed: paramsChanged,
      };
      if (cmd === "get_plugin_params") return [{
        id: 1, name: "Gain", module: "VST3", min: 0, max: 1,
        default: 0, current: 0.75, display: "0.7500", flags: 0,
      }];
      return snapshot;
    });
    await store.initProject();
    const { container, findByText } = render(() => <PresetEditor />);
    selectPreset(container, "Synth");
    fireEvent.click(await findByText("Open editor"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_plugin_gui", { presetName: "Synth" }));

    // Two poll rounds with nothing to report: the panel must not enumerate.
    await waitFor(
      () => expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "poll_plugin_host_events").length,
      ).toBeGreaterThanOrEqual(2),
      { timeout: 3000 },
    );
    expect(invokeMock).not.toHaveBeenCalledWith("get_plugin_params", { presetName: "Synth" });

    // The plugin reports a change: the panel re-reads its parameters.
    paramsChanged = true;
    await waitFor(
      () => expect(invokeMock).toHaveBeenCalledWith("get_plugin_params", { presetName: "Synth" }),
      { timeout: 3000 },
    );
  });

  it("browses for Scala tuning files", async () => {
    openDialogMock.mockResolvedValue("/tmp/browser-tuning.scl");
    const { getByText, getAllByText } = render(() => <PresetEditor />);

    fireEvent.click(getByText("Piano 1"));
    fireEvent.click(getAllByText("Browse")[0]);

    await waitFor(() => {
      expect(openDialogMock).toHaveBeenCalledWith({
        multiple: false,
        filters: [{ name: "Instrument settings file", extensions: ["scl"] }],
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", { token: expect.any(String),
        name: "Piano 1",
        paramName: "scala_file",
        paramValue: "/tmp/browser-tuning.scl",
      });
    });
  });
});


it("keeps a refreshed same-name plugin inventory when an older gesture close resolves", async () => {
  const base = makeSnapshot().project.presets[0];
  const snapshot = makeSnapshot({ presets: [{ ...base, name: "Inventory Race", source: "clap", plugin: "fixture.inventory", params: [] }],
    tracks: [{ ...makeTrack("Lead", 0), preset: "Inventory Race" }] });
  let queries = 0;
  let finish!: (reply: unknown) => void;
  const ending = new Promise<unknown>(resolve => { finish = resolve; });
  const parameter = (value: number) => [{ id: 1, name: "Cutoff", module: "Filter", min: 0, max: 1,
    default: 0.2, current: value, display: value.toFixed(4), flags: 0 }];
  invokeMock.mockImplementation(async command => {
    if (command === "scan_plugins") return [];
    if (command === "load_plugin_for_preset") return parameter(++queries === 1 ? 0.2 : 0.9);
    if (command === "plugin_has_gui") return false;
    if (command === "begin_guarded_plugin_param_gesture") return "inventory-owner";
    if (command === "end_owned_plugin_param_gesture") return ending;
    return snapshot;
  });
  await store.initProject();
  const { container, getAllByText, findByLabelText } = render(() => <PresetEditor />);
  selectPreset(container, "Inventory Race");
  await waitFor(() => expect(getAllByText("Filter").length).toBeGreaterThan(1));
  fireEvent.click(getAllByText("Filter").at(-1)!);
  const input = await findByLabelText("Automate Cutoff");
  fireEvent.pointerDown(input);
  fireEvent.input(input, { target: { value: "0.4" } });
  fireEvent.change(input, { target: { value: "0.4" } });
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("end_owned_plugin_param_gesture", {
    presetName: "Inventory Race", paramName: "Cutoff", token: "inventory-owner",
  }));
  fireEvent.click(container.querySelector(".preset-reload-params")!);
  await waitFor(() => expect((container.querySelector(".plugin-param-slider-track") as HTMLInputElement)?.value).toBe("0.9"));
  finish({ closed: true, snapshot: { ...snapshot, project: { ...snapshot.project, name: "Stale Close" }, revision: 1000000 } });
  await ending;
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
  expect(store.project.name).toBe(snapshot.project.name);
  expect((container.querySelector(".plugin-param-slider-track") as HTMLInputElement).value).toBe("0.9");
  const cached = readCachedPluginPanel(store.project.presets[0]);
  expect(cached?.ok && cached.params[0].current).toBe(0.9);
});

describe("stopping a preparing preview", () => {
  it("turns the preview button into a stop while the load runs", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_preset") return undefined;
      if (cmd === "stop_audition") return undefined;
      if (cmd === "get_transport_state") {
        return {
          playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0,
          audition_active: true,
          audition: {
            paused: false, finished: false, position_seconds: 0, preset: "Piano 1",
            status: "loading", ready_ms: null, instances: 0, loads: 0, error: null,
          },
        };
      }
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    await store.initProject();
    const { container } = render(() => <PresetEditor />);
    await store.previewPreset("Piano 1");
    const button = await waitFor(() => {
      const element = container.querySelector<HTMLElement>(".preset-preview-btn.loading");
      expect(element).toBeTruthy();
      return element!;
    });
    expect(button.textContent).toBe("×");
    expect(button.getAttribute("aria-label")).toBe("Cancel the preview");
    fireEvent.click(button);
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "stop_audition")).toBe(true));
  });
});
