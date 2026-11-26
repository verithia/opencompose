import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { AutomationLaneRow, type AutomationLaneRowProps } from "../../components/TimelineAutomation";
import { EffectAutomation } from "../../components/EffectAutomation";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import { automationLaneKey, automationLaneName, supportsExponentialAutomation } from "../../utils/automationTarget";
import { exponentialShape, laneDomain, lanePolyline, mappedAutomationValue, pointXY, unmappedAutomationValue } from "../../utils/automationRow";
import type { AutomationLane } from "../../types/project";
import { i18n } from "../../stores/i18n";
import { store } from "../../stores/project";
import { AUTOMATION_CURVES } from "../../data/automationCurves.gen";
import { getLastCallArgs, invokeMock, makeSnapshot, makeTrack } from "../setup";

const position=(quarter:number)=>({bar:1,beat:quarter+1,subdivision:0});
const at=(p:ReturnType<typeof position>)=>p.beat-1;
const lane=(overrides:Partial<AutomationLane>={}):AutomationLane=>({param:"fx1.unknown",points:[
  {position:position(0),value:{Number:0},curve:"exponential",curvature:-4},
  {position:position(2),value:{Number:1}},
],...overrides});
const props=(l:AutomationLane):AutomationLaneRowProps=>({lane:l,laneIndex:0,pxPerBeat:100,width:400,selectedPoint:null,
  onSelectPoint:vi.fn(),onAddPoint:vi.fn(),onMovePoint:vi.fn(),onDeletePoint:vi.fn(),onCurve:vi.fn(),onTension:vi.fn(),onToggleMute:vi.fn(),
  blockSelected:false,onSelectBlock:vi.fn(),onMoveBlock:vi.fn(),onResizeBlock:vi.fn(),onDuplicateBlock:vi.fn(),onCopyBlock:vi.fn(),onSplitBlock:vi.fn(),onDeleteBlock:vi.fn(),
});
beforeEach(()=>i18n.setLocale("en"));
afterEach(()=>{closeContextMenu();cleanup();});

it("evaluates signed exponential endpoints, neutral and cropped-segment closure",()=>{
  for(const k of [AUTOMATION_CURVES.curvature.min,-4,0,4,AUTOMATION_CURVES.curvature.max]) {
    expect(exponentialShape(0,k)).toBe(0);expect(exponentialShape(1,k)).toBe(1);
    expect(Number.isFinite(exponentialShape(.5,k))).toBe(true);
  }
  expect(exponentialShape(.37,0)).toBe(.37);
  const k=-4,a=.2,b=.8;
  for(const u of [.125,.5,.875]) {
    const expected=(exponentialShape(a+(b-a)*u,k)-exponentialShape(a,k))/(exponentialShape(b,k)-exponentialShape(a,k));
    expect(exponentialShape(u,k*(b-a))).toBeCloseTo(expected,13);
  }
});

it("draws two authored anchors as a curved physical mapping, preserving step jumps",()=>{
  const l=lane({value_mapping:{curve:"exponential",curvature:2,scale:-2,offset:3}});
  const original=JSON.stringify(l), domain=laneDomain(l),height=38;
  const vertices=lanePolyline(l,at,100,domain,height).split(" ").map(x=>x.split(",").map(Number));
  expect(l.points).toHaveLength(2);expect(vertices).toHaveLength(17);
  const physical=mappedAutomationValue(exponentialShape(.5,-4),l.value_mapping);
  const expectedY=height-(physical-domain.min)/(domain.max-domain.min)*height;
  expect(vertices[8][0]).toBe(100);expect(vertices[8][1]).toBeCloseTo(expectedY,2);
  expect(pointXY(l.points[0],at,100,domain,height,l.value_mapping).y).toBeCloseTo(vertices[0][1],2);
  const held=lane({points:[{...l.points[0],curve:"stairs"},l.points[1]],value_mapping:l.value_mapping});
  const step=lanePolyline(held,at,100,laneDomain(held),height).split(" ");
  expect(step).toHaveLength(3);expect(step[1].split(",")[0]).toBe("200.00");
  expect(step[0].split(",")[1]).toBe(step[1].split(",")[1]);
  // Mapping applies to the interpolated source, not merely to its endpoints.
  const straight=lane({points:[{...l.points[0],curve:"linear"},l.points[1]],value_mapping:l.value_mapping});
  const mappedLine=lanePolyline(straight,at,100,laneDomain(straight),height).split(" ").map(x=>x.split(",").map(Number));
  const mappedMiddle=mappedAutomationValue(.5,straight.value_mapping);
  expect(mappedLine[8][1]).toBeCloseTo(height-(mappedMiddle-domain.min)/(domain.max-domain.min)*height,2);
  expect(mappedLine[8][1]).not.toBeCloseTo((mappedLine[0][1]+mappedLine.at(-1)![1])/2,1);
  expect(JSON.stringify(l)).toBe(original);
});

it("inverts negative-scale and exponential mappings without rewriting authored values",()=>{
  for(const m of [{curve:"linear" as const,scale:-3,offset:2},{curve:"exponential" as const,curvature:4,scale:2,offset:-1}]) {
    for(const v of [0,.25,.5,1])expect(unmappedAutomationValue(mappedAutomationValue(v,m),m)).toBeCloseTo(v,13);
  }
  expect(unmappedAutomationValue(.5,{curve:"linear",scale:0})).toBeNull();
  expect(unmappedAutomationValue(-2,{curve:"exponential",curvature:4})).toBeNull();
});

it("selects an exponential segment from the real point menu without generated anchors",()=>{
  const p=props(lane());const view=render(()=> <><AutomationLaneRow {...p}/><ContextMenuLayer/></>);
  expect(view.container.querySelectorAll(".automation-point")).toHaveLength(2);
  fireEvent.contextMenu(view.container.querySelector(".automation-point")!);
  fireEvent.click([...document.querySelectorAll('.context-menu-item')].find(x=>x.textContent?.includes("Exponential"))!);
  expect(p.onCurve).toHaveBeenCalledWith(0,"exponential",null,-4);
  expect(p.lane.points).toHaveLength(2);
});

it("drags a mapped handle by inverse transfer and commits only the raw anchor",()=>{
  const l=lane({value_mapping:{curve:"exponential",curvature:2,scale:-2,offset:3}});const p=props(l);
  const view=render(()=> <AutomationLaneRow {...p}/>);const element=view.container.querySelector<HTMLElement>(".automation-lane")!;
  element.getBoundingClientRect=()=>({left:0,top:0}as DOMRect);
  const physical=mappedAutomationValue(.5,l.value_mapping),d=laneDomain(l),y=38-(physical-d.min)/(d.max-d.min)*38;
  fireEvent.pointerDown(view.container.querySelector(".automation-point")!,{button:0,pointerId:19,clientX:0,clientY:0});
  fireEvent.pointerMove(document,{pointerId:19,clientX:100,clientY:y});
  fireEvent.pointerUp(document,{pointerId:19,clientX:100,clientY:y});
  expect(p.onMovePoint).toHaveBeenCalledTimes(1);expect(p.onMovePoint).toHaveBeenCalledWith(0,1,expect.closeTo(.5,12));
  expect(l.points[0].value).toEqual({Number:0});
  fireEvent.dblClick(element,{clientX:100,clientY:y});
  expect(p.onAddPoint).toHaveBeenCalledWith(1,expect.closeTo(.5,12));
});

it("visibly refuses noninvertible mapped value drags without losing selection or curve",()=>{
  const p=props(lane({value_mapping:{curve:"linear",scale:0,offset:.5}}));const view=render(()=> <AutomationLaneRow {...p}/>);
  expect(view.container.querySelector('[role="note"]')).toHaveAttribute("aria-label",expect.stringContaining("Point dragging is unavailable"));
  fireEvent.pointerDown(view.container.querySelector(".automation-point")!,{button:0,pointerId:20});
  fireEvent.pointerMove(document,{pointerId:20,clientX:100,clientY:20});fireEvent.pointerUp(document,{pointerId:20});
  expect(p.onSelectPoint).toHaveBeenCalledWith(0);expect(p.onMovePoint).not.toHaveBeenCalled();
  expect(view.container.querySelector(".automation-polyline")?.getAttribute("points")).toBeTruthy();
});

it("edits the real rack coefficient through typed IPC and rejects an out-of-domain number",async()=>{
  const snapshot=makeSnapshot({tracks:[{...makeTrack("synth",0),automation:[lane()]}]});
  store.applyAiSnapshot(snapshot);invokeMock.mockClear();invokeMock.mockResolvedValue(snapshot);
  const view=render(()=> <EffectAutomation target="track:0"/>);const input=view.container.querySelector<HTMLInputElement>(".effect-automation-curvature")!;
  expect(input.min).toBe(String(AUTOMATION_CURVES.curvature.min));expect(input.max).toBe(String(AUTOMATION_CURVES.curvature.max));
  fireEvent.change(input,{target:{value:"3.5"}});
  await vi.waitFor(()=>expect(getLastCallArgs("set_automation_point_curve")).toMatchObject({curve:"exponential",curvature:3.5,tension:null,pointIndex:0,laneIndex:0}));
  invokeMock.mockClear();fireEvent.change(input,{target:{value:String(AUTOMATION_CURVES.curvature.max+1)}});
  expect(invokeMock).not.toHaveBeenCalled();
});

it("keeps exponential unavailable on non-effect targets while preserving legacy edits",()=>{
  const p=props(lane({param:"volume",points:[{position:position(0),value:{Number:0}},{position:position(2),value:{Number:1}}]}));
  const view=render(()=> <><AutomationLaneRow {...p}/><ContextMenuLayer/></>);
  fireEvent.contextMenu(view.container.querySelector(".automation-point")!);
  const option=[...document.querySelectorAll<HTMLButtonElement>('.context-menu-item')].find(x=>x.textContent?.includes("Exponential"))!;
  expect(option).toHaveAttribute("aria-disabled","true");fireEvent.click(option);expect(p.onCurve).not.toHaveBeenCalled();
});

it("admits only formal UTF-8 encoded instrument parameters, retaining effect-only mappings",()=>{
  for(const name of ["Cutoff", "Width / tone", "Tone \u03a9", "\ufeffCutoff"]) {
    const param=automationLaneName(name);
    expect(automationLaneKey(param)).toBe(name);
    expect(supportsExponentialAutomation({param})).toBe(true);
    expect(supportsExponentialAutomation({param,value_mapping:{curve:"linear"}})).toBe(false);
  }
  for(const param of ["pluginparam_", "pluginparam_1g", "pluginparam_c0af", "pluginparam_ff", "pluginparam_123", "pluginparam_ab.cd", "volume", "pan", "modulation"]) {
    expect(supportsExponentialAutomation({param})).toBe(false);
  }
  expect(supportsExponentialAutomation({param:"fx1.factor",value_mapping:{curve:"exponential",curvature:2}})).toBe(true);
});

it("offers the exponential command on an encoded hosted-parameter row",()=>{
  const p=props(lane({param:automationLaneName("Cutoff")}));
  const view=render(()=> <><AutomationLaneRow {...p}/><ContextMenuLayer/></>);
  fireEvent.contextMenu(view.container.querySelector(".automation-point")!);
  const option=[...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")].find(x=>x.textContent?.includes("Exponential"))!;
  expect(option).not.toHaveAttribute("aria-disabled","true");fireEvent.click(option);
  expect(p.onCurve).toHaveBeenCalledWith(0,"exponential",null,-4);
});

it("retains coincident authored discontinuity knots, their order, handles and exact vertical edge",()=>{
  const l=lane({preserve_discontinuities:true,points:[
    {position:position(0),value:{Number:0}},
    {position:position(1),value:{Number:0}},
    {position:position(1),value:{Number:1}},
    {position:position(2),value:{Number:1}},
  ]});
  const original=JSON.stringify(l);const p=props(l);p.selectedPoint=2;
  const view=render(()=> <AutomationLaneRow {...p}/>);
  const handles=[...view.container.querySelectorAll<HTMLElement>(".automation-point")];
  expect(handles).toHaveLength(4);expect(handles[1].style.left).toBe("100px");expect(handles[2].style.left).toBe("100px");
  expect(parseFloat(handles[1].style.top)).toBeGreaterThan(parseFloat(handles[2].style.top));
  expect(handles[2]).toHaveClass("selected");
  const vertices=view.container.querySelector(".automation-polyline")!.getAttribute("points")!.split(" ").map(x=>x.split(",").map(Number));
  expect(vertices).toHaveLength(4);expect(vertices.flat().every(Number.isFinite)).toBe(true);
  expect(vertices[1][0]).toBe(vertices[2][0]);expect(vertices[1][1]).not.toBe(vertices[2][1]);
  fireEvent.pointerDown(handles[2],{button:0,pointerId:27,clientX:100,clientY:2});
  fireEvent.pointerMove(document,{pointerId:27,clientX:150,clientY:10});fireEvent.pointerUp(document,{pointerId:27});
  expect(p.onMovePoint).toHaveBeenCalledWith(2,1.5,expect.any(Number));
  expect(JSON.stringify(l)).toBe(original);
});

it("matches the finite Rust subnormal-curvature limit and inverse without division underflow",()=>{
  for(const k of [Number.MIN_VALUE,-Number.MIN_VALUE,1e-310,-1e-310]) {
    const mapping={curve:"exponential" as const,curvature:k};
    for(const u of [0,.125,.5,.9,1]) {
      expect(exponentialShape(u,k)).toBe(u);
      expect(mappedAutomationValue(u,mapping)).toBe(u);
      expect(unmappedAutomationValue(u,mapping)).toBe(u);
    }
    const affine={...mapping,scale:-2,offset:3};
    expect(mappedAutomationValue(.5,affine)).toBe(2);
    expect(unmappedAutomationValue(2,affine)).toBe(.5);
    const tiny=lane({points:[{position:position(0),value:{Number:0},curve:"exponential",curvature:k},{position:position(2),value:{Number:1}}],value_mapping:mapping});
    const polyline=lanePolyline(tiny,at,100,laneDomain(tiny),38).split(" ").map(vertex=>vertex.split(",").map(Number));
    expect(polyline.flat().every(Number.isFinite)).toBe(true);
    expect(polyline[8]).toEqual([100,19]);
  }
  for(const k of [AUTOMATION_CURVES.curvature.min,AUTOMATION_CURVES.curvature.max]) {
    expect(unmappedAutomationValue(0,{curve:"exponential",curvature:k})).toBe(0);
    expect(unmappedAutomationValue(1,{curve:"exponential",curvature:k})).toBe(1);
  }
});

it("keeps a fine-tick x for vertical drags, commits release once and abandons cancelled gestures",()=>{
  const snapshot=makeSnapshot();snapshot.project.properties.time_signature={numerator:7,denominator:8};
  snapshot.project.properties.time_sig_map=[{position:{bar:3,beat:1,subdivision:0},numerator:3,denominator:4}];
  store.applyAiSnapshot(snapshot);
  const l=lane({ppq:96000,points:[
    {position:{bar:2,beat:7,subdivision:47999},value:{Number:0},curve:"exponential",curvature:-4},
    {position:{bar:3,beat:1,subdivision:48001},value:{Number:1}},
  ]});
  const original=JSON.stringify(l),quarter=7-1/96000;
  for(const cancel of [false,true]) {
    const p={...props(l),pxPerBeat:24};const view=render(()=> <AutomationLaneRow {...p}/>);
    const point=view.container.querySelector(".automation-point")!;
    fireEvent.pointerDown(point,{button:0,pointerId:40,clientX:168,clientY:35});
    fireEvent.pointerMove(document,{pointerId:40,clientX:168,clientY:12});
    if(cancel)fireEvent.pointerCancel(document,{pointerId:40});
    fireEvent.pointerUp(document,{pointerId:40,clientX:168,clientY:12});
    fireEvent.pointerUp(document,{pointerId:40,clientX:168,clientY:12});
    if(cancel)expect(p.onMovePoint).not.toHaveBeenCalled();
    else {
      expect(p.onMovePoint).toHaveBeenCalledTimes(1);
      expect(p.onMovePoint).toHaveBeenCalledWith(0,expect.closeTo(quarter,12),expect.any(Number));
    }
    expect(JSON.stringify(l)).toBe(original);view.unmount();
  }
});
