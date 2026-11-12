import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { store } from "../../stores/project";
import { resetAutomationWriteForTests, setAutomationWriteArmed } from "../../stores/automationWrite";
import type { ParameterTargetStamp, PresetDef, ProjectSnapshot } from "../../types/project";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

const FIRST = "synth-menu-first" as ParameterTargetStamp;
const SECOND = "synth-menu-second" as ParameterTargetStamp;
const PRESET: PresetDef = { name: "Lead", source: "builtin", plugin: "builtin.open_synth", program: null,
  patch: null, file: null, root_note: null, extends: null, params: [], effects: [] };
let current: ProjectSnapshot;
let nextToken: number;
const members = new Set<string>();
function replaceStamp() {
  members.clear();
  current = { ...current, parameter_target_stamp: SECOND, revision: current.revision + 1 };
  store.applyAiSnapshot(current);
}
function calls(command: string) { return invokeMock.mock.calls.filter(([name]) => name === command); }
function setParameter(key: string, raw: string) {
  const preset = current.project.presets[0];
  current = { ...current, revision: current.revision + 1, project: { ...current.project,
    presets: [{ ...preset, params: [...preset.params.filter(([name]) => name !== key), [key, { Number: Number(raw) }]] }] } };
}
function mount() {
  return render(() => <OpenSynthEditor preset={store.project.presets[0]}
    setParam={(name, value, token) => store.setPresetParam("Lead", name, value, token)}
    setParams={(params, replace, token) => store.setPresetParams("Lead", params, replace, token)}
    macroLearn={async () => ({ error: null, result: null })} loadPatch={async () => null}
    noteOn={async () => {}} noteOff={async () => {}} auditionPatch={async () => {}} stopAudition={async () => {}}
    auditioning={() => null} auditionStatus={() => null} libraryPatches={() => []} libraryWarnings={() => []}
    refreshLibrary={async () => null} patchActions={{ rename: async () => null, duplicate: async () => null,
      saveAs: async () => null, saveToLibrary: async () => null, randomize: async () => null }} />);
}
function trigger(key: string) {
  return document.querySelector<HTMLButtonElement>(`[data-param="${key}"] .oc-dropdown-trigger`)!;
}
async function openMenu(key: string) {
  const button = trigger(key);
  if (button.getAttribute("aria-expanded") === "true") fireEvent.click(button);
  fireEvent.click(button);
  // Placement is measured in a microtask; the portal is hidden until it settles.
  return screen.findAllByRole("option");
}
function mono() { return document.querySelector<HTMLInputElement>('[data-param="voice.mono"] input[type="checkbox"]')!; }
function openVoice() { fireEvent.click(screen.getByRole("button", { name: "VOICE" })); }

beforeEach(async () => {
  resetAutomationWriteForTests(); members.clear(); nextToken = 0;
  current = { ...makeSnapshot({ groups: [], markers: [], presets: [PRESET], tracks: [{ ...makeTrack("Lead", 0), preset: "Lead" }] }), parameter_target_stamp: FIRST };
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "scan_plugins") return [];
    if (command === "get_project") return current;
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "get_undo_state") return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    if (command === "begin_parameter_gesture" || command === "begin_guarded_synth_param_gesture") {
      if (args?.expectedStamp !== current.parameter_target_stamp) throw new Error("Parameter target changed before gesture admission");
      const token = `synth-owner-${++nextToken}`; members.add(token); return token;
    }
    if (command === "end_owned_undo_gesture") return members.delete(String(args?.token));
    if (command === "set_preset_param_owned") {
      if (!members.has(String(args?.token))) throw new Error("Expired owner");
      setParameter(String(args?.paramName), String(args?.paramValue)); return current;
    }
    if (command === "update_owned_synth_param_gesture") {
      if (!members.has(String(args?.token))) throw new Error("Expired owner");
      setParameter(String(args?.paramKey), String(args?.physical)); return current;
    }
    if (command === "end_owned_synth_param_gesture") return { closed: members.delete(String(args?.token)), snapshot: current };
    return current;
  });
  await store.initProject();
});
afterEach(() => { cleanup(); resetAutomationWriteForTests(); });

describe("synth discrete parameter admission", () => {
  it.each(["osc.a.waveform", "osc.b.unison"])("retains the menu-open owner for %s and permits a fresh choice", async key => {
    mount();
    const button = trigger(key); const original = button.textContent;
    const oldChoice = (await openMenu(key))[2];
    replaceStamp();
    fireEvent.click(oldChoice);
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_preset_param_owned")).toHaveLength(0);
    expect(current.project.presets[0].params).toEqual([]);
    expect(trigger(key).textContent).toBe(original);
    fireEvent.click((await openMenu(key))[2]);
    await vi.waitFor(() => expect(calls("set_preset_param_owned")).toHaveLength(1));
    expect(calls("begin_parameter_gesture")[0][1]).toMatchObject({ expectedStamp: SECOND });
    expect(calls("set_preset_param_owned")[0][1]).toMatchObject({ paramName: key });
  });

  it("does not select into a replacement document that keeps the preset name", async () => {
    mount(); const button = trigger("osc.a.waveform"); const original = button.textContent;
    const choice = (await openMenu("osc.a.waveform"))[2];
    current = { ...current, parameter_target_stamp: SECOND, project: { ...current.project,
      properties: { ...current.project.properties, tempo: 155 } } };
    await store.initProject();
    fireEvent.click(choice);
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_preset_param_owned")).toHaveLength(0);
    expect(store.project.properties.tempo).toBe(155);
    expect(store.project.presets[0].params).toEqual([]);
    expect(trigger("osc.a.waveform").textContent).toBe(original);
  });

  it("does not recapture a retired menu for armed automation", async () => {
    mount(); setAutomationWriteArmed(true);
    const oldChoice = (await openMenu("osc.a.waveform"))[2];
    replaceStamp(); fireEvent.click(oldChoice);
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(calls("begin_guarded_synth_param_gesture")).toHaveLength(0);
    expect(calls("begin_owned_synth_param_gesture")).toHaveLength(0);
    expect(calls("set_preset_param_owned")).toHaveLength(0);
    expect(current.project.presets[0].params).toEqual([]);
    fireEvent.click((await openMenu("osc.a.waveform"))[2]);
    await vi.waitFor(() => expect(calls("end_owned_synth_param_gesture")).toHaveLength(1));
    expect(calls("begin_guarded_synth_param_gesture")[0][1]).toMatchObject({ expectedStamp: SECOND, paramKey: "osc.a.waveform" });
    expect(calls("update_owned_synth_param_gesture")).toHaveLength(1);
  });

  it.each(["pointer", "keyboard", "input"])("rejects a stale checkbox %s and restores its rendered canonical value", async kind => {
    mount(); openVoice(); const input = mono(); expect(input.checked).toBe(false);
    if (kind === "pointer") fireEvent.pointerDown(input, { button: 0 });
    else if (kind === "keyboard") { input.focus(); fireEvent.keyDown(input, { key: " " }); }
    else fireEvent.input(input, { target: { checked: true } });
    replaceStamp(); fireEvent.change(input, { target: { checked: true } });
    for (let index = 0; index < 5; index += 1) await Promise.resolve();
    expect(input.checked).toBe(false);
    expect(calls("begin_parameter_gesture")).toHaveLength(0);
    expect(calls("set_preset_param_owned")).toHaveLength(0);
    expect(current.project.presets[0].params).toEqual([]);
    fireEvent.input(input, { target: { checked: true } }); fireEvent.change(input, { target: { checked: true } });
    await vi.waitFor(() => expect(calls("end_owned_undo_gesture")).toHaveLength(1));
    expect(input.checked).toBe(true);
    expect(calls("set_preset_param_owned")).toHaveLength(1);
    expect(calls("begin_parameter_gesture")[0][1]).toMatchObject({ expectedStamp: SECOND });
    expect(current.project.presets[0].params).toContainEqual(["voice.mono", { Number: 1 }]);
  });

  it("accepts a fresh checkbox AX change without a preceding pointer or key", async () => {
    mount(); openVoice(); const input = mono();
    fireEvent.change(input, { target: { checked: true } });
    await vi.waitFor(() => expect(calls("end_owned_undo_gesture")).toHaveLength(1));
    expect(input.checked).toBe(true);
    expect(calls("begin_parameter_gesture")[0][1]).toMatchObject({ expectedStamp: FIRST });
    expect(current.project.presets[0].params).toContainEqual(["voice.mono", { Number: 1 }]);
  });

  it("does not dispatch a queued checkbox mutation after its source unmounts", async () => {
    const view = mount(); openVoice(); const input = mono();
    fireEvent.change(input, { target: { checked: true } });
    view.unmount();
    for (let index = 0; index < 8; index += 1) await Promise.resolve();
    expect(calls("set_preset_param_owned")).toHaveLength(0);
    expect(current.project.presets[0].params).toEqual([]);
  });
});
