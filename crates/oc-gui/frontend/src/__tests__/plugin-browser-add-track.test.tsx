import { beforeEach, expect, it } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack } from "./setup";
import { store } from "../stores/project";
import { PluginBrowser } from "../components/PluginBrowser";

beforeEach(() => invokeMock.mockClear());
function backend(format: string, failPreset = false) {
  let snapshot = makeSnapshot({ presets: [] });
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "scan_plugins") return [{id:"exact.plugin.id",name:"Test Instrument",vendor:"Vendor",format,category:"Instrument",path:"/plugin",num_params:0}];
    if (cmd === "soundfonts_list") return {dir:"/fonts",items:[]};
    if (cmd === "list_sampler_assets") return [];
    if (cmd === "project_compatibility_report") return {compatible:true,issues:[],resolved_plugins:[]};
    // The panel adds the preset and its track in one command, so one failed
    // call leaves neither behind.
    if (cmd === "add_instrument_track") {
      if (failPreset) throw new Error("preset refused");
      snapshot = makeSnapshot({
        ...snapshot.project,
        presets: [...snapshot.project.presets, {
          ...makeSnapshot().project.presets[0],
          name: String(args?.presetName),
          source: String(args?.source),
          plugin: String(args?.plugin),
        }],
        tracks: [...snapshot.project.tracks, {
          ...makeTrack(String(args?.trackName), 1),
          preset: String(args?.presetName),
        }],
      });
    }
    return snapshot;
  });
}
it.each([["Vst3","vst3"],["Clap","clap"],["Vst2","vst2"],["Au","au"],["Lv2","lv2"],["Internal","builtin"]])("adds %s with an exact assigned plugin preset", async (format,source) => {
  backend(format); await store.initProject();
  const view=render(()=><PluginBrowser/>);
  const row=await view.findByRole("button",{name:/Test Instrument/});
  fireEvent.keyDown(row,{key:"Enter"});
  fireEvent.doubleClick(row); // An overlapping activation must not create a second preset.
  await waitFor(()=>expect(invokeMock.mock.calls.some(([cmd])=>cmd==="add_instrument_track")).toBe(true));
  await waitFor(()=>expect(store.project.tracks[0]?.preset).toBe("Test Instrument"));
  expect(store.project.presets[0]).toMatchObject({name:"Test Instrument",source,plugin:"exact.plugin.id"});
  fireEvent.doubleClick(row);
  await waitFor(()=>expect(store.project.tracks[1]?.preset).toBe("Test Instrument 2"));
  expect(store.project.presets).toHaveLength(2);
});
it("leaves neither a preset nor a track when the add is refused", async()=>{
  backend("Vst3",true); await store.initProject();
  const view=render(()=><PluginBrowser/>);fireEvent.doubleClick(await view.findByRole("button",{name:/Test Instrument/}));
  await waitFor(()=>expect(store.error()).toContain("preset refused"));
  // One command owns both halves, so a refusal cannot leave a stray track.
  expect(store.project.presets).toHaveLength(0);
  expect(store.project.tracks).toHaveLength(0);
});

it("offers copy path and reveal on a plugin row", async () => {
  backend("Vst3");
  await store.initProject();
  const { ContextMenuLayer } = await import("../components/ContextMenu");
  const view = render(() => <><PluginBrowser /><ContextMenuLayer /></>);
  const row = await view.findByRole("button", { name: /Test Instrument/ });
  fireEvent.contextMenu(row, { clientX: 20, clientY: 20 });
  const labels = [...document.querySelectorAll(".context-menu-item")]
    .map((item) => item.textContent);
  expect(labels).toContain("Add as instrument track");
  expect(labels).toContain("Copy path");
  expect(labels).toContain("Reveal in file manager");

  const reveal = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((item) => item.textContent === "Reveal in file manager")!;
  fireEvent.click(reveal);
  await waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "reveal_path")).toBe(true));
  expect(invokeMock).toHaveBeenCalledWith("reveal_path", { path: "/plugin" });
});
