import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { decodeCurve, curvePath, curveAsset } from "../../devices/transferCurve";
import { presetParams } from "../../components/DevicePanel";
import { DEVICE_CATALOG } from "../../data/deviceCatalog.gen";
import { TransferCurveEditor } from "../../components/TransferCurveEditor";
import { store } from "../../stores/project";
import { captureParameterAdmission } from "../../stores/parameterAdmission";
import { i18n } from "../../stores/i18n";
import type { Effect, ParameterTargetStamp } from "../../types/project";
vi.mock("../../stores/project", () => ({ store: { parameterRequestOwner: vi.fn(), setEffectParams: vi.fn(async () => {}) } }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
function effect(text = curveAsset().default_json): Effect { return { effect_type: "TransferShaper", params: [["curve", { String: text }]], bypass: false, sidechain_source: null }; }
function owner() { vi.mocked(store.parameterRequestOwner).mockImplementation(() => captureParameterAdmission("fixture" as ParameterTargetStamp, () => true)); }
it("validates owning bounds and displays large curves without rewriting samples", () => {
 const samples = Array.from({ length: curveAsset().max_samples }, (_, i) => i / (curveAsset().max_samples - 1));
 const c = decodeCurve(JSON.stringify({ version: 1, domain: "odd", samples }));
 expect(curvePath(c).length).toBeLessThan(30_000); expect(c.samples).toEqual(samples);
 expect(() => decodeCurve('{"version":1,"domain":"odd","samples":[1,1]}')).toThrow();
 expect(() => decodeCurve('{"version":1,"domain":"odd","samples":[0,17]}')).toThrow();
});
it.each([0.8,1,1.5,2])("supports keyboard editing at scale %s and leaves untouched bytes intact", async scale => {
 owner(); const text = ' { "version":1, "domain":"odd", "samples":[0,0.75,1] } ';
 const view = render(() => <div style={{ "font-size": `${scale}em` }}><TransferCurveEditor target="track:0" index={0} effect={effect(text)} /></div>);
 expect(view.container.querySelector("svg")?.getAttribute("viewBox")).toBe("0 0 320 120");
 expect(store.setEffectParams).not.toHaveBeenCalled();
 const range = view.container.querySelector('input[type="range"]')!;
 fireEvent.input(range,{target:{value:"0.5"}});fireEvent.change(range,{target:{value:"0.5"}});
 await waitFor(() => expect(store.setEffectParams).toHaveBeenCalledTimes(1));
 const updates=vi.mocked(store.setEffectParams).mock.calls[0][2];
 expect(decodeCurve(updates[0][1]).samples).toEqual([0,0.5,1]);
});
it("drops an asynchronous import when its target changes", async () => {
 owner();let finish!:(text:string)=>void;const text=new Promise<string>(resolve=>{finish=resolve;});
 const [target,setTarget]=createSignal("track:0");
 const view=render(()=><TransferCurveEditor target={target()} index={0} effect={effect()} />);
 fireEvent.change(view.container.querySelector('input[type="file"]')!,{target:{files:[{size:50,text:()=>text}]}});
 setTarget("track:1");finish(curveAsset().default_json);await text;await Promise.resolve();
 expect(store.setEffectParams).not.toHaveBeenCalled();
});
it("cancels pointer edits without a mutation",()=>{
 owner();const view=render(()=><TransferCurveEditor target="track:0" index={0} effect={effect()} />);
 const range=view.container.querySelector('input[type="range"]')!;
 fireEvent.pointerDown(range,{button:0,pointerId:3});fireEvent.input(range,{target:{value:"0.3"}});fireEvent.pointerCancel(window,{pointerId:3});
 expect(store.setEffectParams).not.toHaveBeenCalled();
});
it("imports the original asset text rather than reserializing its samples",async()=>{
 owner();const text=' { "version":1,"domain":"odd","samples":[0,0.3,1] }\n';
 const view=render(()=><TransferCurveEditor target="track:0" index={0} effect={effect()} />);
 fireEvent.change(view.container.querySelector('input[type="file"]')!,{target:{files:[{size:text.length,text:async()=>text}]}});
 await waitFor(()=>expect(store.setEffectParams).toHaveBeenCalledTimes(1));
 expect(vi.mocked(store.setEffectParams).mock.calls[0][2]).toEqual([["curve",text]]);
});

it("exports original bytes and uses localized labels in both theme surfaces",async()=>{
 owner();const locale=i18n.locale();i18n.setLocale("zh-Hans");
 const original=' { "version":1,"domain":"odd","samples":[0,1] }\n';
 const create=vi.fn((_blob: Blob | MediaSource)=>"blob:curve");const revoke=vi.fn();
 URL.createObjectURL=create;URL.revokeObjectURL=revoke;
 const click=vi.spyOn(HTMLAnchorElement.prototype,"click").mockImplementation(()=>{});
 for(const theme of ["light","dark"]){
  document.documentElement.dataset.theme=theme;
  const view=render(()=><TransferCurveEditor target="track:0" index={0} effect={effect(original)} />);
  fireEvent.click(view.getByText("导出曲线"));
  const blob=create.mock.calls.at(-1)![0] as unknown as Blob;
  const reader=new FileReader();const bytes=new Promise<string>(resolve=>{reader.onload=()=>resolve(String(reader.result));});reader.readAsText(blob);
  expect(await bytes).toBe(original);view.unmount();
 }
 expect(store.setEffectParams).not.toHaveBeenCalled();await waitFor(()=>expect(revoke).toHaveBeenCalledTimes(2));
 click.mockRestore();i18n.setLocale(locale);delete document.documentElement.dataset.theme;
});

it("includes untouched asset bytes in device presets",()=>{
 const text=' { "version":1,"domain":"odd","samples":[0,0.5,1] } ';
 const device=DEVICE_CATALOG.devices.find(d=>d.id==="transfer-shaper")!;
 expect(presetParams(device,effect(text)).curve).toBe(text);
});

it("shows the external asset path without drawing the default curve", () => {
  const file = "import-data/curves/source.json";
  const external: Effect = { ...effect(), params: [["curve_file", { String: file }]] };
  const view = render(() => <TransferCurveEditor target="track:0" index={0} effect={external} />);
  expect(view.getByText(file)).toBeVisible();
  expect(view.container.querySelector("svg")).toBeNull();
  expect(store.setEffectParams).not.toHaveBeenCalled();
});
