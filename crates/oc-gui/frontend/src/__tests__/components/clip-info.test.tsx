import { afterEach, beforeEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { ContextMenuLayer, closeContextMenu, openContextMenu } from "../../components/ContextMenu";
import { ClipDetailView } from "../../components/ClipDetailView";
import { clipInformation, clipInfoUi, displayMediaPath } from "../../stores/clipInfo";
import { clipMenuItems } from "../../stores/clipActions";
import { store } from "../../stores/project";
import { invokeMock, makeNote, makePattern, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

beforeEach(async () => {
  const snapshot = makeSnapshot({ tracks: [{ ...makeTrack("Keys", 0), preset: "Track sound", clips: [{ pattern: "Hook", time_range: { start: {bar:1,beat:1,subdivision:0}, end: {bar:3,beat:1,subdivision:0} }, offset_quarters: 1 }] }],
    patterns: [{ ...makePattern("Hook"), preset: "Sampler", events: [makeNote()], hits: [] }],
    presets: [{ name:"Sampler", source:"sampler", plugin:"builtin.sampler", patch:null, program:null, file:null, root_note:null, extends:"Base sampler", params:[], effects:[] },
      { name:"Base sampler", source:"sampler", plugin:"builtin.sampler", patch:null, program:0, file:"samples/Long sample name.wav", root_note:null, extends:null, params:[], effects:[{ effect_type: "Gain", params: [] }] }],
  });
  snapshot.file_path = "/songs/Example/song.ocp";
  snapshot.project.properties.audio_clips = [{name:"Vocal",track:"Keys",file:"../media/vocal.wav",time_range:{start:{bar:1,beat:1,subdivision:0},end:{bar:5,beat:1,subdivision:0}},offset_seconds:3,gain_db:-6,fade_in_seconds:0.1,fade_out_seconds:0.2,stretch:1.2,pitch:-2}];
  invokeMock.mockImplementation(async cmd => cmd === "get_transport_state" ? TRANSPORT_STOPPED() : snapshot);
  await store.initProject();
  clipInfoUi.inspect(null);
  invokeMock.mockClear();
});
afterEach(() => { closeContextMenu(); cleanup(); });
it("keeps pattern provenance in the tooltip data, not in the menu", () => {
  render(() => <><ContextMenuLayer /><ClipDetailView /></>);
  const before = store.selectedTrackIndex();
  const info = clipInformation("track:0/clip:0")!;
  const values = info.rows.map((row) => row.value);
  expect(values).toContain("Sampler");
  expect(values).toContain("/songs/Example/samples/Long sample name.wav");
  openContextMenu({x:20,y:20,items:clipMenuItems("track:0/clip:0")});
  // The menu is actions only; the rows are one click away in the inspector.
  expect(document.querySelector(".context-menu-info")).toBeNull();
  fireEvent.click(screen.getByRole("menuitem", {name:"Inspect clip"}));
  expect(screen.getByRole("region", {name:"Inspect clip"})).toHaveTextContent("/songs/Example/samples/Long sample name.wav");
  expect(store.selectedTrackIndex()).toBe(before);
});
it("shows audio file path and processing instead of an unrelated track preset", () => {
  const info = clipInformation("audio:0")!;
  expect(info.rows.map(row => row.value)).toContain("/songs/media/vocal.wav");
  expect(info.rows.map(row => row.value)).toContain("-6 dB");
  expect(info.rows.map(row => row.value)).toContain("-2 st");
  expect(info.rows.map(row => row.value)).not.toContain("Track sound");
  openContextMenu({x:20,y:20,items:clipMenuItems("audio:0")});
});
it("opens useful audio inspection without showing the note editor", () => {
  render(() => <><ContextMenuLayer /><ClipDetailView /></>);
  openContextMenu({x:20,y:20,items:clipMenuItems("audio:0")});
  fireEvent.click(screen.getByRole("menuitem", {name:"Inspect clip"}));
  const inspector = screen.getByRole("region", {name:"Inspect clip"});
  expect(inspector).toHaveTextContent("Vocal");
  expect(inspector).toHaveTextContent("/songs/media/vocal.wav");
  expect(document.querySelector(".clip-detail-notes")).toBeNull();
});
it("does not invent paths for unsaved projects and handles Windows document paths", () => {
  expect(displayMediaPath("samples/a.wav", null)).toBe("samples/a.wav");
  expect(displayMediaPath("samples/a.wav", "C:\\Music\\song.ocp")).toBe("C:\\Music\\samples\\a.wav");
  expect(displayMediaPath("/absolute/a.wav", "/songs/a.ocp")).toBe("/absolute/a.wav");
});
it("keeps a source path selectable and copyable in the inspector", () => {
  render(() => <><ContextMenuLayer /><ClipDetailView /></>);
  openContextMenu({x:20,y:20,items:clipMenuItems("audio:0")});
  fireEvent.click(screen.getByRole("menuitem", {name:"Inspect clip"}));
  const path = screen.getByText("/songs/media/vocal.wav");
  const range = document.createRange(); range.selectNodeContents(path);
  window.getSelection()!.removeAllRanges(); window.getSelection()!.addRange(range);
  const event = new KeyboardEvent("keydown", {key:"c",ctrlKey:true,bubbles:true,cancelable:true});
  path.dispatchEvent(event);
  // The browser's own copy keeps working; the app does not swallow Cmd+C here.
  expect(event.defaultPrevented).toBe(false);
  window.getSelection()!.removeAllRanges();
});

it("shows a file and program inherited from a base preset", () => {
  const rows = clipInformation("track:0/clip:0")!.rows;
  expect(rows).toEqual(expect.arrayContaining([
    expect.objectContaining({ value: "Sampler" }),
    expect.objectContaining({ value: "/songs/Example/samples/Long sample name.wav", path: true }),
    expect.objectContaining({ label: "Program", value: "0" }),
    expect.objectContaining({ label: "Effect stages", value: "1" }),
  ]));
});
