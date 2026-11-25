import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { Timeline } from "../../components/Timeline";
import { TooltipLayer } from "../../components/Tooltip";
import { clipInformation } from "../../stores/clipInfo";
import { store } from "../../stores/project";
import { INJECTED_PRESET_PREFIX, type PresetDef } from "../../types/project";
import { effectivePatternSound, patternSoundLabel } from "../../utils/patternSound";
import { invokeMock, makePattern, makeSnapshot, makeTrack } from "../setup";

const preset = (name: string, overrides: Partial<PresetDef> = {}): PresetDef => ({
  name, source: "sampler", plugin: "builtin.sampler", program: null, patch: null,
  file: null, root_note: null, extends: null, params: [], effects: [], ...overrides,
});
afterEach(cleanup);

it("resolves authored overrides, inherited assets and injected host fallback without changing bindings", () => {
  const track = { ...makeTrack("stable track", 0), preset: "Host", engine: "host engine" };
  const pattern = { ...makePattern("Hook"), preset: "Child", engine: "unused pattern engine" };
  const project = { presets: [preset("Host"), preset("Base", { file: "samples/keys.wav" }), preset("Child", { source: null, plugin: null, extends: "Base" })] };
  const original = JSON.stringify({ project, track, pattern });
  const own = effectivePatternSound(project, track, pattern);
  expect(own.origin).toBe("pattern");
  expect(own.preset?.file).toBe("samples/keys.wav");
  expect(own.engine).toBe("host engine");
  for (const injected of [null, `${INJECTED_PRESET_PREFIX}kit`]) {
    const fallback = effectivePatternSound(project, track, { ...pattern, preset: injected, instrument: {category:"Audio",name:"unused"} });
    expect(fallback.origin).toBe("track");
    expect(patternSoundLabel(fallback)).toBe("Host");
    expect(fallback.instrument).toEqual(track.instrument);
  }
  expect(patternSoundLabel(effectivePatternSound(project, track, { ...pattern, preset: "Absent" }))).toBe("Absent (not found)");
  expect(effectivePatternSound(project, track, { ...pattern, preset: "Absent" }).preset).toBeNull();
  expect(JSON.stringify({ project, track, pattern })).toBe(original);
});

it("keeps two clip overrides, fallback and missing sound consistent in the body, tooltip and Info", async () => {
  const names = ["Soft keys", "Bright lead", `${INJECTED_PRESET_PREFIX}kit`, "Absent"];
  const track = { ...makeTrack("stable track", 0), preset: "Host", phrases: [],
    clips: names.map((_,index) => ({pattern:`Part ${index}`,time_range:{
      start:{bar:index+1,beat:1,subdivision:0},end:{bar:index+2,beat:1,subdivision:0},
    }})),
  };
  const snapshot = makeSnapshot({ tracks:[track], patterns:names.map((name,index)=>({...makePattern(`Part ${index}`),preset:name})), presets:[preset("Host"),preset("Soft keys"),preset("Bright lead")] });
  invokeMock.mockImplementation(async()=>snapshot);
  await store.initProject();
  invokeMock.mockClear();
  const before=JSON.stringify(store.project);
  const view=render(()=> <><Timeline /><TooltipLayer /></>);
  const blocks=view.container.querySelectorAll<HTMLElement>(".phrase-block.clip-block");
  expect(blocks).toHaveLength(4);
  for (const [index,label] of ["Soft keys","Bright lead","Host","Absent (not found)"].entries()) {
    const badge=blocks[index].querySelector(".clip-sound-override");
    expect(badge?.textContent.trim() ?? null).toBe(index===2?null:`↳ ${label}`);
    const info=clipInformation(`track:0/clip:${index}`)!;
    expect(info.rows.map(row=>row.value)).toContain(label);
    const origin=index===2?"Uses the track sound":"Uses the pattern preset instead of the track sound";
    expect(info.rows.map(row=>row.value)).toContain(origin);
    fireEvent.focusIn(blocks[index]);
    const tooltip=document.querySelector('[role="tooltip"]')!;
    expect(tooltip).toHaveTextContent(label);
    expect(tooltip).toHaveTextContent(origin);
    fireEvent.focusOut(blocks[index]);
  }
  expect(JSON.stringify(store.project)).toBe(before);
  expect(invokeMock.mock.calls.some(([command])=>String(command).startsWith("set_")||String(command).startsWith("edit_"))).toBe(false);
});
