import { openSynthRange, synthKnobTrigger, synthRangeInput } from "../helpers/synthRange";
import { createOwnedUndoFixture } from "../setup";
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { chooseDropdown, dropdownOptionValues, invokeMock, openDialogMock, saveDialogMock, makeSnapshot, makeTrack, selectPreset } from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";

const ownedUndo = createOwnedUndoFixture();

beforeEach(async () => {
  invokeMock.mockClear();
  openDialogMock.mockReset();
  saveDialogMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
  });
  await store.initProject();
});

describe("OpenSynth integration", () => {
  it("exposes the VST3 editor and state controls, not only CLAP", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins" || cmd === "load_plugin_for_preset") return [];
      if (cmd === "plugin_has_gui") return true;
      if (cmd === "open_plugin_gui") return [800,600];
      return makeSnapshot({presets:[{name:"Test Strings",source:"vst3",plugin:"fixture.vst3",
        program:null,patch:null,file:null,root_note:null,extends:null,params:[],effects:[]}],tracks:[makeTrack("Strings",0)]});
    });
    await store.initProject();
    const {container,findByText,getByText,unmount}=render(()=><PresetEditor />);
    selectPreset(container, "Test Strings");
    fireEvent.click(await findByText("Open editor"));
    await waitFor(()=>expect(invokeMock).toHaveBeenCalledWith("open_plugin_gui",{presetName:"Test Strings"}));
    expect(getByText("VST3 plugin")).toBeTruthy();
    unmount();
  });
  it("orders a quick release and retrigger behind the first native note", async () => {
    let releaseFirstNote!: () => void;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "open_synth_note_on" && !releaseFirstNote) {
        return new Promise<void>(resolve => { releaseFirstNote = resolve; });
      }
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
          program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
        }],
        tracks: [makeTrack("Synth", 0)],
      });
    });
    await store.initProject();
    const { container, getByLabelText } = render(() => <PresetEditor />);
    selectPreset(container, "OpenSynth Init");
    const key = getByLabelText("Preview C4");
    fireEvent.pointerDown(key, { pointerId: 1 });
    fireEvent.pointerUp(key, { pointerId: 1 });
    fireEvent.pointerDown(key, { pointerId: 2 });
    fireEvent.pointerUp(key, { pointerId: 2 });
    const notes = () => invokeMock.mock.calls.filter(([cmd]) => cmd.startsWith("open_synth_note_"));
    expect(notes()).toEqual([["open_synth_note_on", {
      presetName: "OpenSynth Init", note: 60, velocity: 108,
    }]]);
    releaseFirstNote();
    await waitFor(() => expect(notes().map(([cmd]) => cmd)).toEqual([
      "open_synth_note_on", "open_synth_note_off",
      "open_synth_note_on", "open_synth_note_off",
    ]));
  });

  it.each(["blur", "window blur", "unmount", "collapse", "tab"])("releases held audition on %s", async (exit) => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      return makeSnapshot({ presets: [{
        name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
        program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
      }], tracks: [makeTrack("Synth", 0)] });
    });
    await store.initProject();
    const { container, getByText, getByLabelText, unmount } = render(() => <PresetEditor />);
    selectPreset(container, "OpenSynth Init");
    const key = getByLabelText("Preview C4");
    fireEvent.keyDown(key, { key: "Enter" });
    if (exit === "blur") fireEvent.blur(key);
    else if (exit === "window blur") fireEvent(window, new Event("blur"));
    else if (exit === "unmount") unmount();
    else if (exit === "collapse") fireEvent.click(getByLabelText("Collapse OpenSynth"));
    else fireEvent.click(getByText("FX"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_synth_note_off", {
      presetName: "OpenSynth Init", note: 60,
    }));
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "open_synth_note_off")).toHaveLength(1);
  });

  it("loads factory patches atomically and edits playable voice modes", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
          program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
        }], tracks: [makeTrack("Synth", 0)],
      });
    });
    await store.initProject();
    const { container, getByText, getByLabelText } = render(() => <PresetEditor />);
    selectPreset(container, "OpenSynth Init");
    fireEvent.click(getByLabelText("Preset browser"));
    fireEvent.dblClick(screen.getByText("Neon Bass"));
    await waitFor(() => expect(container.querySelector(".synth-load-button")).not.toHaveAttribute("disabled"));
    fireEvent.click(getByText("VOICE"));
    const voice = (param: string, selector: string) =>
      container.querySelector<HTMLElement>(`.synth-voice-view [data-param="${param}"] ${selector}`)!;
    fireEvent.click(voice("voice.mono", "input"));
    chooseDropdown(voice("voice.polyphony", ".oc-dropdown-trigger"), "24 voices");
    const unison = voice("unison.voices", ".oc-dropdown-trigger");
    chooseDropdown(unison, "7 voices");
    // The menu must offer every count the engine accepts (1..16), not stop at 7.
    const voiceOptions = dropdownOptionValues(unison);
    expect(voiceOptions).toEqual(Array.from({ length: 16 }, (_, index) => String(index + 1)));
    const randomPhase = openSynthRange(container.querySelector<HTMLElement>("[data-param='osc.random_phase']")!);
    fireEvent.change(randomPhase, { target: { value: "0.8" } });
    const reference = openSynthRange(container.querySelector<HTMLElement>("[data-param='tuning.reference_hz']")!);
    fireEvent.change(reference, { target: { value: "0.525" } });

    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd, args]) => (cmd === "set_preset_params" || cmd === "set_preset_params_owned")
        && args.params.some(([name, value]: [string, string]) => name === "voice.mono" && value === "1"))).toBe(true);
      const factoryLoad = invokeMock.mock.calls.find(([cmd]) => (cmd === "set_preset_params" || cmd === "set_preset_params_owned"))!;
      expect(factoryLoad[1].replaceSynth).toBe(true);
      expect(factoryLoad[1].params).toEqual(expect.arrayContaining([
        ["wavetable.file", null], ["open_synth_state_file", null], ["step.rate", "4"], ["step.sync", "1"],
        ["step.division", "0"], ["step.smoothing", "0"],
        ...Array.from({ length: 16 }, (_, index) => [`step.${index + 1}`, "0"]),
      ]));
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "voice.mono", paramValue: "1",
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "voice.polyphony", paramValue: "24",
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "unison.voices", paramValue: "7",
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "osc.random_phase", paramValue: "0.8",
      });
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "tuning.reference_hz", paramValue: "442",
      });
    });
  });

  it("commits dropdown choices through the preset parameter path", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
          program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
        }], tracks: [makeTrack("Synth", 0)],
      });
    });
    await store.initProject();
    const { getByRole, container } = render(() => <PresetEditor />);
    selectPreset(container, "OpenSynth Init");

    const shape = container.querySelector<HTMLElement>("[data-param='osc.a.waveform'] .oc-dropdown-trigger")!;
    expect(shape).toBeTruthy();
    chooseDropdown(shape, "Square");
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "osc.a.waveform", paramValue: "2",
      });
    });

    fireEvent.click(getByRole("button", { name: "FILTER" }));
    const routing = container.querySelector<HTMLElement>("[data-param='filter.routing'] .oc-dropdown-trigger")!;
    chooseDropdown(routing, "Parallel");
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "filter.routing", paramValue: "1",
      });
    });
  });

  it("keeps every sound module behind the OSC, FILTER and AMP sub-tabs", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        presets: [{
          name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
          program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [],
        }], tracks: [makeTrack("Synth", 0)],
      });
    });
    await store.initProject();
    const { getByText, getByLabelText, container } = render(() => <PresetEditor />);
    selectPreset(container, "OpenSynth Init");

    const soundPage = () => container.querySelector(".synth-sound-page")!;
    expect([...container.querySelectorAll(".synth-osc-module")].map((module) => module.getAttribute("data-osc")))
      .toEqual(["a", "b", "c"]);
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='osc.a.wavetable_position']")!)).toBeTruthy();
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='osc.c.pan']")!)).toBeTruthy();
    expect(soundPage().querySelector("[data-param='osc.b.unison']")).toBeTruthy();
    // Oscillator A's unison count is the patch-level `unison.voices`; it used to
    // live only in the VOICE view, which made A look like it had no unison.
    const moduleA = soundPage().querySelector(".synth-osc-module[data-osc='a']")!;
    expect(moduleA.querySelector("[data-param='unison.voices']")).toBeTruthy();
    expect(moduleA.querySelector("[data-param='unison.detune']")).toBeTruthy();
    expect(container.querySelector(".synth-sample-strip")?.textContent).toContain("IMPORT PRESET");
    expect(container.querySelector(".synth-audition .synth-keyboard")?.children).toHaveLength(13);

    fireEvent.click(getByText("FILTER"));
    expect(container.querySelectorAll(".synth-filter-module")).toHaveLength(2);
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='filter2.cutoff']")!)).toBeTruthy();
    expect(soundPage().querySelector("[data-param='filter2.enabled']")).toBeTruthy();

    fireEvent.click(getByText("AMP"));
    expect(container.querySelectorAll(".synth-amp-module")).toHaveLength(4);
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='filter_env.curve']")!)).toBeTruthy();
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='lfo.rate']")!)).toBeTruthy();
    expect(synthKnobTrigger(soundPage().querySelector<HTMLElement>("[data-param='sub.level']")!)).toBeTruthy();

    fireEvent.click(getByText("OSC"));
    expect(container.querySelectorAll(".synth-osc-module")).toHaveLength(3);

    fireEvent.click(container.querySelector(".synth-subtab-link")!);
    expect(container.querySelector(".synth-sound-page")).toBeNull();
    expect(container.querySelector(".synth-matrix")).toBeTruthy();

    fireEvent.click(getByText("SOUND"));
    expect(container.querySelector(".synth-sound-page")).toBeTruthy();
    expect(container.querySelectorAll(".synth-osc-module")).toHaveLength(3);

    fireEvent.click(getByLabelText("Collapse OpenSynth"));
    expect(container.querySelector(".synth-sound-page")).toBeNull();
    expect(container.querySelector(".synth-compact-strip")).toBeTruthy();

    fireEvent.click(getByLabelText("Expand OpenSynth"));
    expect(container.querySelector(".synth-sound-page")).toBeTruthy();
    expect(container.querySelector(".synth-sound-page")?.getAttribute("data-page")).toBe("osc");
  });

  describe("MOD route graph", () => {
    const routedPreset = (...params: [string, { Number: number }][]) => makeSnapshot({
      presets: [{
        name: "OpenSynth Init", source: "builtin", plugin: "builtin.open_synth",
        program: null, patch: null, file: null, root_note: null, extends: null,
        params, effects: [],
      }],
      tracks: [makeTrack("Synth", 0)],
    });

    async function openModView(snapshot: ReturnType<typeof makeSnapshot>) {
      invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const ownership = ownedUndo.route(cmd, args);
      if (ownership) return ownership.value;
      cmd = ownedUndo.mutation(cmd, args);
        if (cmd === "scan_plugins") return [];
        if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
        return snapshot;
      });
      await store.initProject();
      const result = render(() => <PresetEditor />);
      selectPreset(result.container, "OpenSynth Init");
      const tab = [...result.container.querySelectorAll(".synth-view-tabs button")]
        .find((button) => button.textContent === "MOD")!;
      fireEvent.click(tab);
      return result;
    }

    it("draws one edge per active slot and encodes amount sign", async () => {
      const { container } = await openModView(routedPreset(
        ["mod.1.source", { Number: 0 }], ["mod.1.destination", { Number: 0 }], ["mod.1.amount", { Number: 0.5 }],
        ["mod.2.source", { Number: 1 }], ["mod.2.destination", { Number: 1 }], ["mod.2.amount", { Number: -0.25 }],
      ));
      const edges = [...container.querySelectorAll(".synth-route-edge")];
      expect(edges.map((edge) => edge.getAttribute("data-slot"))).toEqual(["1", "2"]);
      const line = (edge: Element) => edge.querySelector(".synth-route-line")!;
      // Positive route is solid and thicker than the faint negative one.
      expect(line(edges[0]).getAttribute("stroke-dasharray")).toBe("none");
      expect(line(edges[1]).getAttribute("stroke-dasharray")).toBe("4,3");
      expect(Number(line(edges[0]).getAttribute("stroke-width")))
        .toBeGreaterThan(Number(line(edges[1]).getAttribute("stroke-width")));
      // Only participating sources and destinations get a node.
      expect(container.querySelectorAll(".synth-route-node.source")).toHaveLength(2);
      expect(container.querySelectorAll(".synth-route-node.destination")).toHaveLength(2);
    });

    // Mounting the whole synth panel takes ~1.7 s alone and crosses vitest's
    // 5 s default when the suite runs in parallel on a loaded machine.
    it("shows the slot inspector for the clicked edge and clears the route", { timeout: 20000 }, async () => {
      const { container, getByLabelText } = await openModView(routedPreset(
        ["mod.3.source", { Number: 2 }], ["mod.3.destination", { Number: 4 }], ["mod.3.amount", { Number: 0.4 }],
      ));
      expect(container.querySelector(".synth-route-inspector")).toBeNull();
      fireEvent.click(container.querySelector('[data-slot="3"].synth-route-edge')!);
      const inspector = container.querySelector(".synth-route-inspector")!;
      expect(inspector.getAttribute("data-slot")).toBe("3");
      // The slot list below renders the same labels, so scope to the inspector.
      expect(inspector.querySelector('[data-param="mod.3.source"] .oc-dropdown-trigger')).toBeTruthy();
      expect(inspector.querySelector('[data-param="mod.3.destination"] .oc-dropdown-trigger')).toBeTruthy();
      expect(synthRangeInput(inspector.querySelector<HTMLElement>('[data-param="mod.3.amount"]')!)).toBeTruthy();

      fireEvent.click(getByLabelText("Remove route 3"));
      await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_preset_param_owned", {
        token: expect.any(String),
        name: "OpenSynth Init", paramName: "mod.3.amount", paramValue: "0",
      }));
    });

    it("offers a way back to the 64-slot list", async () => {
      const { container } = await openModView(routedPreset());
      expect(container.querySelector(".synth-route-empty")).toBeTruthy();
      expect(container.querySelector(".synth-slot-list .synth-matrix")).toBeTruthy();
    });
  });

});
