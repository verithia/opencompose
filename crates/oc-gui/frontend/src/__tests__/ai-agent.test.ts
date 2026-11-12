import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { readFileSync } from "node:fs";
import { MAX_TOOL_CALLS_PER_BATCH, MAX_TOOL_ROUNDS, READ_TOOLCALL_TOOL, ROUNDS_LEFT_NOTICE, WIRE_KEPT_TOOL_PAIRS, WIRE_MESSAGE_LIMIT, WIRE_TEXT_LIMIT, parseArguments, runAgent, textToolCalls, type ApprovalRequest, type ToolCall, type ToolDef, type TurnTrace, type WireMessage } from "../ai/agent";
import { setInvoke } from "../ai/transport";

const call = (name: string, args: unknown, id="call-1"): ToolCall => ({id,type:"function",function:{name,arguments:JSON.stringify(args)}});
const def = (name: string, area: string, properties: Record<string, any> = {}, required: string[] = [], mutation = false, approval: "automatic" | "confirm" = "automatic"): ToolDef =>
  ({name, description:name, area, mutation, approval, inputSchema:{type:"object",properties,required,additionalProperties:false}});
const tools = (): ToolDef[] => [
  def("project_extract", "project", {path:{type:"string"}}, ["path"]),
  def("plugin_search", "plugin", {query:{type:"string"},limit:{type:"integer"}}),
  def("render_range", "render", {path:{type:"string"},start:{type:"number"},end:{type:"number"},stems:{type:"boolean"}}, ["path","start","end"]),
  def("audition_range", "render", {path:{type:"string"},start:{type:"number"},end:{type:"number"},start_bar:{type:"integer"},end_bar:{type:"integer"},tracks:{type:"array",items:{type:"integer"}}}),
  def("audio_analyze", "analysis", {path:{type:"string"}}, ["path"]),
  def("audio_compare", "analysis", {reference:{type:"string"},candidate:{type:"string"},listen:{type:"boolean"},listening_only:{type:"boolean"},duration:{type:"number"},channel:{type:"string"},clip_sample_rate:{type:"number"}}, ["reference","candidate"]),
  def("transaction_begin", "transaction", {label:{type:"string"}}, [], true),
  def("transaction_commit", "transaction"),
  def("transaction_rollback", "transaction"),
  def("tracks_list", "tracks"),
  def("edit_subtree", "project", {path:{type:"string"},new_dsl:{type:"string"}}, ["path","new_dsl"]),
  def("tracks_add", "tracks", {name:{type:"string"}}, ["name"], true),
];

/**
 * A fake DAW transport: a project string plus an undo stack, so a turn is
 * checked against the rules the backend enforces — one transaction at a time,
 * commit keeps one undo entry, rollback restores the snapshot, and a refused
 * mutation returns its error while the batch keeps what already landed.
 */
function fakeDaw(options: {
  mutating?: Set<string>;
  fail?: (tool: string, count: number) => boolean;
  rollbackError?: string;
  commitError?: string;
  onCall?: (tool: string, count: number) => void;
} = {}) {
  const daw = {
    project: "snapshot",
    snapshot: null as string | null,
    changed: false,
    undo: [] as string[],
    commands: [] as string[],
    calls: [] as string[],
  };
  const transport = async <T>(command: string, payload?: Record<string, unknown>): Promise<T> => {
    if (command === "get_project") return { dsl_source: daw.project } as T;
    if (command === "ai_batch_recover") {
      // Only the batch is rolled back; the panel is told what it restored.
      if (daw.snapshot === null) return null as T;
      daw.project = daw.snapshot;
      daw.snapshot = null;
      daw.changed = false;
      return { dsl_source: daw.project } as T;
    }
    if (command !== "ai_call_tool") throw new Error(`unexpected command ${command}`);
    const { tool, args } = payload as { tool: string; args: Record<string, unknown> };
    daw.commands.push(tool);
    if (tool === "transaction_begin") {
      if (daw.snapshot !== null) throw { error: "a project transaction is already active" };
      daw.snapshot = daw.project;
      daw.changed = false;
      return { active: true, label: args.label } as T;
    }
    if (tool === "transaction_commit") {
      if (options.commitError) throw { error: options.commitError };
      if (daw.snapshot === null) throw { error: "no active project transaction" };
      // A batch that changed nothing publishes no revision and no entry.
      if (daw.changed) daw.undo.push(daw.snapshot);
      daw.snapshot = null;
      daw.changed = false;
      return { committed: true, revision: 1 } as T;
    }
    if (tool === "transaction_rollback") {
      if (options.rollbackError) throw { error: options.rollbackError };
      if (daw.snapshot === null) throw { error: "no active project transaction" };
      daw.project = daw.snapshot;
      daw.snapshot = null;
      daw.changed = false;
      return { rolled_back: true, revision: 1 } as T;
    }
    daw.calls.push(tool);
    options.onCall?.(tool, daw.calls.length);
    if (options.fail?.(tool, daw.calls.length)) {
      throw { error: `${tool} refused` };
    }
    if (options.mutating?.has(tool)) {
      daw.project = `${daw.project}+${tool}`;
      daw.changed = true;
      return { revision: 1 } as T;
    }
    return {} as T;
  };
  return { daw, transport };
}

const mock = vi.mocked(invoke);
beforeEach(() => { mock.mockReset(); });
afterEach(() => { setInvoke(null); });
const base = () => ({providerId:"test",nativeTools:true,messages:[{role:"user",content:"edit"}],expectedDsl:"before",tools:tools(),cancelled:()=>false,report:vi.fn()});
describe("bounded AI tools", () => {
  it("keeps the frontend batch bound aligned with native validation", () => {
    const native = readFileSync("src-tauri/src/ai/mod.rs", "utf8");
    expect(Number(native.match(/pub const MAX_TOOL_CALLS_PER_BATCH: usize = (\d+);/)?.[1])).toBe(MAX_TOOL_CALLS_PER_BATCH);
  });
  it("dispatches a bounded large batch with distinct call IDs", async () => {
    mock.mockResolvedValue({} as any);
    const batch = Array.from({length:MAX_TOOL_CALLS_PER_BATCH}, (_, i) => call("tracks_list", {}, `read-${i}`));
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:batch}).mockResolvedValueOnce({role:"assistant",content:"Done"});
    const reportTool = vi.fn();
    expect((await runAgent({...base(),request,reportTool})).text).toBe("Done");
    expect(reportTool).toHaveBeenCalledTimes(MAX_TOOL_CALLS_PER_BATCH);
    expect(request.mock.calls[1][0].filter((m:any)=>m.role === "tool").map((m:any)=>m.tool_call_id)).toEqual(batch.map(c=>c.id));
  });
  it("rejects an oversized batch before executing any calls", async () => {
    const batch = Array.from({length:MAX_TOOL_CALLS_PER_BATCH + 1}, (_, i) => call("tracks_list", {}, `read-${i}`));
    const request = vi.fn().mockResolvedValue({role:"assistant",content:null,tool_calls:batch});
    const reportTool = vi.fn();
    await expect(runAgent({...base(),request,reportTool})).rejects.toThrow(`limit is ${MAX_TOOL_CALLS_PER_BATCH}`);
    expect(reportTool).not.toHaveBeenCalled();
  });

  it("keeps an oversized successful mutation and adopts its DSL for the next edit", async () => {
    const snapshot = {project:{tracks:[{notes:"x".repeat(600_000)}]},dsl_source:"updated DSL",revision:8,dirty:true,track_count:1};
    mock.mockImplementation(async (command:string,payload?:any) => {
      if (command === "get_project") return snapshot as any;
      if (command === "ai_validate_edit") return {valid:true} as any;
      if (command === "ai_apply_edit") return {...snapshot, dsl_source:"after-edit", revision:9} as any;
      if (payload?.tool === "tracks_add") return snapshot as any;
      return {} as any;
    });
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_add",{name:"Pad"})]})
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"next DSL"},"edit")]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const reportTool=vi.fn();
    const result = await runAgent({...base(),request,reportTool,permission:"write"});
    expect(result.text).toBe("Done");
    // The replacement this turn builds next is checked against what the turn
    // itself wrote, not against the document it started on.
    expect(mock).toHaveBeenCalledWith("ai_apply_edit",{newDsl:"next DSL",targetPath:"project",expectedDsl:"updated DSL"});
    expect(mock.mock.calls.some(([,args]) => (args as any)?.tool === "transaction_rollback")).toBe(false);
    expect(mock.mock.calls.some(([,args]) => (args as any)?.tool === "transaction_commit")).toBe(true);
    const wire = JSON.parse(request.mock.calls[1][0].find((m:any)=>m.role==="tool").content);
    expect(wire).toMatchObject({revision:8,dsl_source:"updated DSL",project_ast_omitted:true});
    expect(reportTool.mock.calls[0][1].length).toBeLessThan(500_000);
    expect(reportTool.mock.calls[0][2]).toBe(false);
  });
  it("returns bounded actionable oversized reads, omitting giant DSL without invalid JSON", async () => {
    const snapshot = {project:{tracks:[]},dsl_source:"x".repeat(600_000),revision:9,dirty:false,track_count:0};
    mock.mockImplementation(async (_command:string,payload?:any) => payload?.tool === "plugin_search" ? {items:["x".repeat(600_000)]} as any : snapshot as any);
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("project_get",{}),call("plugin_search",{query:"piano"},"search")]})
      .mockResolvedValueOnce({role:"assistant",content:"I will inspect a smaller result."});
    const result = await runAgent({...base(),tools:[...tools(),def("project_get","project")],request});
    expect(result.failed).not.toBe(true);
    const rows=request.mock.calls[1][0].filter((m:any)=>m.role==="tool");
    expect(JSON.parse(rows[0].content)).toMatchObject({revision:9,dirty:false,track_count:0,dsl_source_omitted:true,tool_failed:false});
    expect(JSON.parse(rows[1].content)).toMatchObject({result_truncated:true,tool_failed:false});
    expect(rows.every((r:any)=>r.content.length<500_000)).toBe(true);
  });

  it("names a truncated result by its call id and pages through it whole", async () => {
    // Bigger than one page, so the model has to ask twice with the id it was given.
    const items = ["y".repeat(43_000)];
    const stored = JSON.stringify({items});
    mock.mockImplementation(async (_command:string,payload?:any) => payload?.tool === "plugin_search" ? {items} as any : {} as any);
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("plugin_search",{query:"piano"},"search-1")]})
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[
        call("read_toolcall",{id:"search-1",offset:0,limit:32_000},"read-1"),
        call("read_toolcall",{id:"search-1",offset:32_000,limit:32_000},"read-2"),
      ]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const result = await runAgent({...base(),tools:[...tools(),READ_TOOLCALL_TOOL],request});
    expect(result.text).toBe("Done");

    const rows = (round:number) => request.mock.calls[round][0] as any[];
    const truncated = JSON.parse(rows(1).find(m=>m.tool_call_id==="search-1").content);
    // The model-facing copy carries the id, not the transcript's copy alone.
    expect(truncated).toMatchObject({result_truncated:true,tool_call_id:"search-1"});
    expect(truncated.note).toContain("read_toolcall");

    const [first, second] = ["read-1","read-2"].map(id => JSON.parse(rows(2).find(m=>m.tool_call_id===id).content));
    expect(first.result_truncated).toBeUndefined();
    expect(first.text.length).toBe(32_000);
    expect(first.total).toBe(stored.length);
    // Two pages with that id are the whole result, in order.
    expect(first.text + second.text).toBe(stored);
    expect(second.error).toBeUndefined();
  });

  it("sends compact device parameter rows while raw catalog details stay available", async () => {
    const param = {id:"mix",min:0,max:100,default:30,unit:"%",steps:null,curve:"linear",name:"Mix"};
    const catalog = {devices:[{id:"reverb",category:"time",name:"Reverb",params:[param],ports:[{id:"in"}]}]};
    mock.mockResolvedValue(catalog);
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("devices_catalog",{})]}).mockResolvedValueOnce({role:"assistant",content:"Done"});
    const reportTool = vi.fn();
    await runAgent({...base(),tools:[...tools(),def("devices_catalog","devices")],request,reportTool});
    const wire = JSON.parse(request.mock.calls[1][0].find((m:any) => m.role === "tool").content);
    expect(wire.parameter_columns).toEqual(["id","min","max","default","unit","steps","curve"]);
    expect(wire.devices[0]).toEqual({id:"reverb",category:"time",params:[["mix",0,100,30,"%",null,"linear"]]});
    expect(wire.ui_metadata_omitted).toBe(true);
    expect(JSON.parse(reportTool.mock.calls[0][1])).toEqual(catalog);
  });
  it("preserves authoritative DSL keywords when compacting device discovery", async () => {
    mock.mockResolvedValue({devices:[{id:"de-esser",dsl_id:"de_esser",category:"dynamics",params:[]}]});
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("devices_catalog",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),tools:[...tools(),def("devices_catalog","devices")],request});
    const wire = JSON.parse(request.mock.calls[1][0].find((m:any) => m.role === "tool").content);
    expect(wire.devices[0]).toEqual({id:"de-esser",dsl_id:"de_esser",category:"dynamics",params:[]});
  });
  it("keeps complete project_get DSL and metadata without its duplicate AST on model wire", async () => {
    const snapshot = {project:{tracks:[{name:"piano"}]},dsl_source:'@project "song" {}',revision:3,track_count:1};
    mock.mockResolvedValue(snapshot);
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("project_get",{})]}).mockResolvedValueOnce({role:"assistant",content:"Done"});
    const reportTool = vi.fn(), onProject = vi.fn();
    await runAgent({...base(),tools:[...tools(),def("project_get","project")],request,reportTool,onProject});
    expect(JSON.parse(request.mock.calls[1][0].find((m:any) => m.role === "tool").content)).toEqual({dsl_source:snapshot.dsl_source,revision:3,track_count:1,project_ast_omitted:true});
    expect(JSON.parse(reportTool.mock.calls[0][1])).toEqual(snapshot);
    expect(onProject).toHaveBeenCalledWith(snapshot);
  });

  it("compacts only mutation snapshot wire while preserving raw details and full hydration", async () => {
    const snapshot = { project:{tracks:[{name:"piano", notes:"note".repeat(1000)}]}, dsl_source:'@project "Current" {}', revision:2, dirty:true, track_count:1 };
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command === "get_project") return snapshot as any;
      if (["tracks_add", "project_extract"].includes(payload?.tool)) return snapshot as any;
      return {} as any;
    });
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_add",{name:"piano"},"mutation"),call("project_extract",{path:"project"},"read")]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const reportTool = vi.fn(), onProject = vi.fn();
    await runAgent({...base(),request,reportTool,onProject});
    const rows = request.mock.calls[1][0].filter((m:any) => m.role === "tool");
    expect(JSON.parse(rows[0].content)).toEqual({dsl_source:snapshot.dsl_source,revision:2,dirty:true,track_count:1,project_ast_omitted:true});
    expect(JSON.parse(rows[1].content)).toEqual(snapshot);
    expect(JSON.parse(reportTool.mock.calls[0][1])).toEqual(snapshot);
    expect(onProject).toHaveBeenCalledWith(snapshot);
  });

  it("refreshes the mixer after a compact effect bypass receipt", async () => {
    const args = { target: "track:0", index: 0, bypass: true };
    const receipt = { ...args, effect: "reverb", revision: 2 };
    const snapshot = { project: { tracks: [{ name: "Vocal", effects: [{ bypass: true }] }] }, revision: 2 };
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command === "get_project") return snapshot as any;
      return (payload?.tool === "effects_set_bypass" ? receipt : {}) as any;
    });
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("effects_set_bypass",args)]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const onProject = vi.fn();
    const tool = def("effects_set_bypass", "effects", {target:{type:"string"},index:{type:"integer"},bypass:{type:"boolean"}}, ["target","index","bypass"], true);
    await runAgent({...base(), tools:[...tools(), tool], request, onProject});
    expect(JSON.parse(request.mock.calls[1][0].find((message: WireMessage) => message.role === "tool").content)).toEqual(receipt);
    expect(onProject).toHaveBeenCalledWith(snapshot);
  });

  it("leaves a name outside the advertised surface to the registry", () => {
    expect(parseArguments(call("save_file",{}),tools())).toEqual({});
    expect(()=>parseArguments(call("edit_subtree",{path:"project",new_dsl:42}),tools())).toThrow();
    expect(()=>parseArguments(call("edit_subtree",{path:"project"}),tools())).toThrow();
    expect(()=>parseArguments(call("edit_subtree",{path:"project",new_dsl:"@project",extra:1}),tools())).toThrow();
  });
  it("only recognizes complete text-tool JSON, including escaped braces", () => {
    expect(textToolCalls('I will inspect first.\n{"tool":"tracks_list","args":{}}')).toHaveLength(1);
    expect(textToolCalls('```json\n{"tool":"project_extract","args":{"path":"track:x"}}\n```')).toHaveLength(1);
    // Nested args keep their braces; only the outer object is one call.
    const nested = textToolCalls('Calling now: {"tool":"tracks_add","args":{"name":"pad","x":{"y":1}}} done');
    expect(nested).toHaveLength(1);
    expect(JSON.parse(nested[0].function.arguments)).toEqual({name:"pad",x:{y:1}});
    expect(textToolCalls('no tool call here')).toEqual([]);
    expect(parseArguments(call("edit_subtree",{path:"project",new_dsl:'@project "{x}"'}),tools()).new_dsl).toContain("{x}");
  });
  it("dispatches DAW tools through ai_call_tool", async () => {
    mock.mockResolvedValue({entries:[],total:0,source:"bundled"});
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("plugin_search",{query:"dex",tags:["reverb"],platform:"macos-aarch64",format:"vst3",installed_only:true,limit:5})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"plugin_search",args:{query:"dex",tags:["reverb"],platform:"macos-aarch64",format:"vst3",installed_only:true,limit:5}});
  });
  it("lets the registry name the toolset that would offer an unadvertised tool", async () => {
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command !== "ai_call_tool") return {} as any;
      if (payload?.tool === "transport_play") {
        throw { error: "refused: 'transport_play' is not offered by the current toolsets (dsl-edit); enable 'transport' (Transport) in the panel's Toolset switch" };
      }
      throw { error: `unknown tool '${payload?.tool}'` };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("transport_play",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const result = await runAgent({...base(),request});
    expect(result.text).toBe("Done");
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"transport_play",args:{}});
    const rows = request.mock.calls[1][0].filter((message: any) => message.role === "tool");
    expect(rows[0].content).toContain("not offered by the current toolsets");
    expect(rows[0].content).toContain("'transport' (Transport)");
    expect(rows[0].content).not.toContain("Unknown tool");
  });
  it("still answers a name no toolset carries as unknown", async () => {
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command !== "ai_call_tool") return {} as any;
      throw { error: `unknown tool '${payload?.tool}'` };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("save_file",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    const rows = request.mock.calls[1][0].filter((message: any) => message.role === "tool");
    expect(rows[0].content).toContain("unknown tool 'save_file'");
  });
  it("refuses a local edit the surface withheld instead of applying it", async () => {
    const withheld = tools().filter(tool => tool.name !== "edit_subtree");
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command === "get_project") return {dsl_source:"before"} as any;
      if (command === "ai_validate_edit") return {valid:true} as any;
      if (command === "ai_apply_edit") return {dsl_source:"after",revision:2,track_count:1} as any;
      if (command !== "ai_call_tool") return {} as any;
      if (payload?.tool === "transaction_begin") return {active:true,label:"turn"} as any;
      if (payload?.tool === "transaction_commit") return {committed:true,revision:1} as any;
      if (payload?.tool === "transaction_rollback") return {rolled_back:true,revision:1} as any;
      throw { error: "refused: 'edit_subtree' is not offered by the current toolsets (dsl-edit); enable 'dsl-edit' (DSL edit) in the panel's Toolset switch" };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project \"x\""})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    const result = await runAgent({...base(),tools:withheld,request,autoApply:true});
    expect(result.proposal).toBeUndefined();
    expect(mock.mock.calls.map(c=>c[0])).not.toContain("ai_apply_edit");
    expect(mock).toHaveBeenCalledWith("ai_call_tool",expect.objectContaining({tool:"edit_subtree"}));
    const rows = request.mock.calls[1][0].filter((message: any) => message.role === "tool");
    expect(rows[0].content).toContain("not offered by the current toolsets");
  });
  it("returns native tool results with matching IDs before continuing", async () => {
    mock.mockResolvedValue({dsl_text:"current"});
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("project_extract",{path:"project"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    expect((await runAgent({...base(),request})).text).toBe("Done");
    expect(request.mock.calls[1][0]).toContainEqual({role:"tool",message_meta:{version:1,kind:"tool"},tool_call_id:"call-1",content:'{"dsl_text":"current"}'});
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"project_extract",args:{path:"project"}});
  });
  it("validates and proposes without invoking mutation when there is no approver", async () => {
    mock.mockImplementation(async command => command==="get_project" ? {dsl_source:"before"} : {valid:true});
    const request=vi.fn().mockResolvedValue({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project"})]});
    // A caller with no approver and no review callback gets the proposal back
    // instead of a silent apply.
    const result=await runAgent({...base(),request,permission:"require_approval"});
    expect(result.proposal).toEqual({path:"project",dsl:"@project",expectedDsl:"before"});
    expect(mock.mock.calls.map(c=>c[0])).not.toContain("ai_apply_edit");
    expect(request).toHaveBeenCalledTimes(1);
  });
  it("hands a thrown tool failure to the model as its text, not as an empty object", async () => {
    mock.mockImplementation(async (command: string) => {
      if (command === "ai_call_tool") throw new Error("Project changed during request");
      return {};
    });
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_list",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    expect(request.mock.calls[1][0]).toContainEqual({role:"tool",message_meta:{version:1,kind:"tool"},tool_call_id:"call-1",content:'{"error":"Project changed during request"}'});
  });
  it("keeps the backend's error text, skill and hint verbatim", async () => {
    const failure = {error:"refused: 'render_offline' is outside the panel",skill:"dsl-subtree-paths",hint:"use audition_range"};
    mock.mockImplementation(async (command: string) => {
      if (command === "ai_call_tool") throw failure;
      return {};
    });
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_list",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    const content = request.mock.calls[1][0].at(-1).content;
    expect(content).toBe(JSON.stringify(failure));
    expect(JSON.parse(content)).toEqual(failure);
  });
  it("applies a validated replacement when the session has nobody to accept it", async () => {
    mock.mockImplementation(async (command: string) => {
      if (command === "get_project") return {dsl_source:"before"};
      if (command === "ai_validate_edit") return {valid:true};
      if (command === "ai_apply_edit") return {dsl_source:"after",revision:7,track_count:2};
      return {};
    });
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Applied and rendered."});
    const result=await runAgent({...base(),autoApply:true,request});
    expect(result.proposal).toBeUndefined();
    expect(result.text).toBe("Applied and rendered.");
    expect(mock).toHaveBeenCalledWith("ai_apply_edit",{newDsl:"@project",targetPath:"project",expectedDsl:"before"});
    expect(request.mock.calls[1][0]).toContainEqual({role:"tool",message_meta:{version:1,kind:"tool"},tool_call_id:"call-1",content:'{"status":"applied","applied":true,"path":"project","revision":7,"track_count":2}'});
  });
  it("checks the next replacement in an autonomous turn against what it just wrote", async () => {
    let source = "before";
    mock.mockImplementation(async (command: string) => {
      if (command === "get_project") return {dsl_source:source};
      if (command === "ai_validate_edit") return {valid:true};
      if (command === "ai_apply_edit") return {dsl_source:(source="after"),revision:8,track_count:2};
      return {};
    });
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project"})]})
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"bus:drums",new_dsl:"@bus drums"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Both applied."});
    const result=await runAgent({...base(),autoApply:true,request});
    expect(result.text).toBe("Both applied.");
    const applies=mock.mock.calls.filter(c=>c[0]==="ai_apply_edit");
    expect(applies).toHaveLength(2);
    expect(applies[1]).toEqual(["ai_apply_edit",{newDsl:"@bus drums",targetPath:"bus:drums",expectedDsl:"after"}]);
  });
  it("adopts the document a mutating tool reported, so the next edit is not stale", async () => {
    let source = "before";
    mock.mockImplementation(async (command: string) => {
      if (command === "ai_call_tool") {
        source = "after-track-add";
        return {revision:5,index:2,name:"drums"};
      }
      if (command === "get_project") return {dsl_source:source};
      if (command === "ai_validate_edit") return {valid:true};
      if (command === "ai_apply_edit") return {dsl_source:"after-edit",revision:6,track_count:3};
      return {};
    });
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_add",{name:"drums"})]})
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done."});
    const result=await runAgent({...base(),autoApply:true,request});
    expect(result.text).toBe("Done.");
    expect(mock).toHaveBeenCalledWith("ai_apply_edit",{newDsl:"@project",targetPath:"project",expectedDsl:"after-track-add"});
  });
  it("replays provider continuity on the next tool round", async () => {
    mock.mockResolvedValue({dsl_text:"current"});
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("project_extract",{path:"project"})],provider_state:[{type:"reasoning",encrypted_content:"opaque"}]})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    expect(request.mock.calls[1][0]).toContainEqual(expect.objectContaining({
      role:"assistant",provider_state:[{type:"reasoning",encrypted_content:"opaque"}],
    }));
  });
  it("replays signed thinking on the next tool round", async () => {
    mock.mockResolvedValue({dsl_text:"current"});
    const thinking=[{text:"Check the bass.",signature:"sig-1"}];
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("project_extract",{path:"project"})],thinking})
      .mockResolvedValueOnce({role:"assistant",content:"Done"});
    await runAgent({...base(),request});
    expect(request.mock.calls[1][0]).toContainEqual(expect.objectContaining({role:"assistant",thinking}));
  });
  it("rejects stale edits, returns errors to model, never proposes", async () => {
    mock.mockResolvedValue({dsl_source:"changed"});
    const request=vi.fn().mockResolvedValueOnce({role:"assistant",tool_calls:[call("edit_subtree",{path:"project",new_dsl:"@project"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Please retry"});
    expect((await runAgent({...base(),request})).proposal).toBeUndefined();
    expect(mock).not.toHaveBeenCalledWith("ai_validate_edit",expect.anything());
  });
  it("does not dispatch tools after cancellation and says that nothing ran", async () => {
    let cancelled=false;
    const request=vi.fn(async()=> {cancelled=true;return {role:"assistant",tool_calls:[call("tracks_list",{})],content:null};});
    const result = await runAgent({...base(),request,cancelled:()=>cancelled});
    expect(result.failed).toBe(true);
    expect(result.stop).toEqual({cancelled:true,done:0,planned:0,edits:0});
    expect(mock).not.toHaveBeenCalled();
  });
  it("stops at the tool-round cap and says so in the final message", async () => {
    mock.mockResolvedValue({tracks:[]});
    // Different work each round, so the cap (not the repeat guard) ends it.
    let step=0;
    const request=vi.fn(async()=>({role:"assistant",content:null,tool_calls:[call("tracks_list",{step:step++})]}));
    const result=await runAgent({...base(),request});
    expect(result.text).toBe(`stopped after ${MAX_TOOL_ROUNDS} tool rounds; the work is unfinished; the last action was tracks_list`);
    expect(result.failed).toBe(true);
    expect(MAX_TOOL_ROUNDS).toBe(64);
    expect(request).toHaveBeenCalledTimes(MAX_TOOL_ROUNDS);
  });
  it("reports the cap exit as a failure, not as an answer", async () => {
    mock.mockResolvedValue({tracks:[]});
    let step=0;
    const request=vi.fn(async()=>({role:"assistant",content:null,tool_calls:[call("tracks_list",{step:step++})]}));
    const report=vi.fn();
    const result=await runAgent({...base(),request,report});
    expect(request).toHaveBeenCalledTimes(MAX_TOOL_ROUNDS);
    expect(result.text).toContain("the work is unfinished");
    expect(result.text).toContain("the last action was tracks_list");
    expect(report.mock.calls.at(-1)).toEqual([result.text, true]);
  });
  it("tells the model how many rounds are left, once per count and only near the end", async () => {
    mock.mockResolvedValue({tracks:[]});
    // Snapshot each request's history: the loop keeps pushing into one array.
    const wire: any[][] = [];
    let step=0;
    const request=vi.fn(async(history: any[])=>{
      wire.push([...history]);
      return {role:"assistant",content:null,tool_calls:[call("tracks_list",{step:step++})]};
    });
    await runAgent({...base(),request});
    expect(wire).toHaveLength(MAX_TOOL_ROUNDS);

    const remaining = (content: unknown) => {
      const match = typeof content === "string" && content.match(/^You have (\d+) tool rounds? left in this turn\./);
      return match ? Number(match[1]) : null;
    };
    const noticesIn = (history: any[]) => history.filter(m => m.role === "user" && remaining(m.content) !== null);
    const first = MAX_TOOL_ROUNDS - ROUNDS_LEFT_NOTICE;
    wire.forEach((history, index) => {
      const notices = noticesIn(history);
      // Nothing before the last ROUNDS_LEFT_NOTICE rounds; one more per request after.
      expect(notices).toHaveLength(index < first ? 0 : index - first + 1);
    });

    const history = wire.at(-1)!;
    expect(noticesIn(history).map(m => remaining(m.content))).toEqual([4,3,2,1]);
    expect(noticesIn(history).every(m => m.message_meta?.version === 1 && m.message_meta.kind === "round_budget")).toBe(true);
    // Every notice is a plain user message directly after that round's tool
    // result, so the assistant/tool pairing stays intact.
    history.forEach((message, index) => {
      if (message.role === "tool") return;
      if (message.role === "user" && remaining(message.content) !== null) expect(history[index-1].role).toBe("tool");
    });
    for (const [index, message] of history.entries()) {
      if (message.role !== "assistant" || !message.tool_calls) continue;
      message.tool_calls.forEach((call: ToolCall, offset: number) => {
        expect(history[index+1+offset]).toMatchObject({role:"tool", tool_call_id:call.id});
      });
    }
  });
  it("adds no round notice to a turn that answers in two rounds", async () => {
    mock.mockResolvedValue({tracks:[]});
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("tracks_list",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Done."});
    const result=await runAgent({...base(),request});
    expect(result.text).toBe("Done.");
    const history = request.mock.calls[1][0] as any[];
    expect(history.some(m => m.role === "user" && String(m.content).startsWith("You have"))).toBe(false);
  });
  it("still stops on a repeated call once the round notices have started", async () => {
    mock.mockResolvedValue({tracks:[]});
    let step=0;
    const request=vi.fn(async(_history: any[])=>{
      // Round 13 repeats round 12's call, several notices into the turn.
      const current = step++;
      return {role:"assistant",content:null,tool_calls:[call("tracks_add",{name:String(current === MAX_TOOL_ROUNDS - ROUNDS_LEFT_NOTICE ? current - 1 : current)})]};
    });
    const report=vi.fn();
    const result=await runAgent({...base(),request,report});
    expect(request).toHaveBeenCalledTimes(MAX_TOOL_ROUNDS - ROUNDS_LEFT_NOTICE + 1);
    expect(result.text).toContain("repeated the same call twice (tracks_add");
    expect(result.failed).toBe(true);
    expect(report).toHaveBeenLastCalledWith(expect.stringContaining("repeated the same call twice"), true);
    const history = request.mock.calls.at(-1)![0] as any[];
    expect(history.some(m => m.role === "user" && String(m.content).startsWith(`You have ${ROUNDS_LEFT_NOTICE} tool rounds left`))).toBe(true);
  });
  it("round-trips the four high-value areas through the panel path", async () => {
    mock.mockImplementation(async (_command: string, payload: any) => {
      const { tool } = payload as { tool: string };
      if (tool === "render_range") return { path: "/tmp/range.wav", frames: 44100, stems: [] };
      if (tool === "audio_analyze") return { level: { integrated_lufs: -14.2 } };
      if (tool === "audio_compare") return { score: { value: 87 } };
      if (tool === "transaction_begin") return { active: true, label: "pass", revision: 3 };
      if (tool === "transaction_commit") return { committed: true, revision: 4 };
      throw new Error(`unexpected ${tool}`);
    });
    const report = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[
        call("transaction_begin",{label:"pass"},"1"),
        call("render_range",{path:"/tmp/range.wav",start:48,end:72,stems:false},"2"),
        call("audio_analyze",{path:"/tmp/range.wav"},"3"),
        call("audio_compare",{reference:"/tmp/a.wav",candidate:"/tmp/range.wav"},"4"),
        call("transaction_commit",{},"5"),
      ]})
      .mockResolvedValueOnce({role:"assistant",content:"Measured and committed."});
    const result = await runAgent({...base(),request,report});
    expect(result.text).toBe("Measured and committed.");
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"render_range",args:{path:"/tmp/range.wav",start:48,end:72,stems:false}});
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"audio_compare",args:{reference:"/tmp/a.wav",candidate:"/tmp/range.wav"}});
    expect(mock).toHaveBeenCalledWith("ai_call_tool",{tool:"transaction_begin",args:{label:"pass"}});
    expect(report).toHaveBeenCalledTimes(5);
  });
  it.each([
    [{ channels: 2 as const, sample_rate: 24_000 }, "stereo"],
    [{ channels: 1 as const, sample_rate: 16_000 }, "mono"],
  ] as const)("delivers two ordered clips in the model format %j without embedding audio in tool output", async (audioInput, channel) => {
    const reference = "UkVG";
    const candidate = "Q0FO";
    mock.mockResolvedValue({ listening: {
      status: "matched", delta_lu: 0.012,
      reference_audio: { base64: reference }, candidate_audio: { base64: candidate },
    } });
    const args = { reference: "/tmp/a.wav", candidate: "/tmp/b.wav", duration: 2, listen: true, listening_only: true, channel: "stereo", clip_sample_rate: 48_000 };
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [call("audio_compare", args)] })
      .mockResolvedValueOnce({ role: "assistant", content: "Heard both." });
    await runAgent({ ...base(), request, audioInput });
    expect(mock).toHaveBeenCalledWith("ai_call_tool", {
      tool: "audio_compare", args: { ...args, channel, clip_sample_rate: audioInput.sample_rate },
    });
    const history = request.mock.calls[1][0] as any[];
    const clips = history.filter(message => message.input_audio);
    expect(clips.map(clip => clip.message_meta?.kind)).toEqual(["comparison_reference", "comparison_candidate"]);
    expect(clips.map(clip => clip.input_audio)).toEqual([
      { data: reference, format: "wav" }, { data: candidate, format: "wav" },
    ]);
    expect(clips[0].content).toContain("A/B reference excerpt.");
    expect(clips[1].content).toContain("0.012 LU");
    const result = JSON.parse(history.find(message => message.role === "tool").content);
    expect(result.listening.reference_audio).toEqual({ bytes: 3 });
    expect(result.listening.candidate_audio).toEqual({ bytes: 3 });
  });

  it("keeps metrics available without encoding a pair for a text-only model", async () => {
    mock.mockResolvedValue({ score: 72 });
    const args = { reference: "/tmp/a.wav", candidate: "/tmp/b.wav", duration: 2, listen: true };
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [call("audio_compare", args)] })
      .mockResolvedValueOnce({ role: "assistant", content: "Compared the metrics." });
    await runAgent({ ...base(), request, audioInput: false });
    expect(mock).toHaveBeenCalledWith("ai_call_tool", {
      tool: "audio_compare", args: { ...args, listen: false },
    });
    const history = request.mock.calls[1][0] as any[];
    expect(history.some(message => message.input_audio)).toBe(false);
    expect(JSON.parse(history.find(message => message.role === "tool").content))
      .toEqual({ score: 72, listening: { status: "unsupported" } });
  });

  it("skips a listening-only comparison for a text-only model", async () => {
    const args = { reference: "/tmp/a.wav", candidate: "/tmp/b.wav", duration: 2, listen: true, listening_only: true };
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [call("audio_compare", args)] })
      .mockResolvedValueOnce({ role: "assistant", content: "Audio is unavailable." });
    await runAgent({ ...base(), request, audioInput: false });
    expect(mock.mock.calls.some(([command, input]) => command === "ai_call_tool" && (input as {tool: string}).tool === "audio_compare")).toBe(false);
    const history = request.mock.calls[1][0] as WireMessage[];
    expect(history.some(message => message.input_audio)).toBe(false);
    expect(JSON.parse(history.find(message => message.role === "tool")!.content!))
      .toEqual({ listening_only: true, listening: { status: "unsupported" } });
  });

  it("cancels an in-flight separator using its own call ID", async () => {
    const controller = new AbortController();
    const request = vi.fn().mockResolvedValueOnce({
      role: "assistant", tool_calls: [call("separate_stems", { path: "/tmp/vocal.wav" })],
    });
    let finishSeparator: (value: unknown) => void = () => {};
    let issuedId: string | undefined;
    mock.mockImplementation(async (command: string, args?: any) => {
      if (command === "ai_call_tool" && args.tool === "separate_stems") {
        issuedId = args.cancellationId;
        return new Promise(resolve => { finishSeparator = resolve; });
      }
      if (command === "ai_cancel_separator") {
        expect(args.cancellationId).toBe(issuedId);
        finishSeparator({ error: "separation cancelled" });
      }
      return {};
    });
    const tool = def("separate_stems", "analysis", { path: { type: "string" } }, ["path"]);
    const running = runAgent({ ...base(), request, tools: [...tools(), tool], toolSignal: controller.signal, cancelled: () => controller.signal.aborted });
    await vi.waitFor(() => expect(issuedId).toBeTypeOf("string"));
    controller.abort();
    expect((await running).stop).toMatchObject({ cancelled: true, inFlight: "separate_stems" });
    expect(request).toHaveBeenCalledTimes(1);
  });

  it("delivers a requested audition as one extra user message with the base64 intact", async () => {
    const base64 = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA=";
    mock.mockResolvedValue({
      path:"/tmp/audition.wav",start_s:0,end_s:4,duration_s:4,tracks:[0],
      audio:{mime:"audio/wav",sample_rate:16000,channels:1,base64},
    });
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("audition_range",{start:0,end:4})]})
      .mockResolvedValueOnce({role:"assistant",content:"Heard it."});
    const result=await runAgent({...base(),request});
    expect(result.text).toBe("Heard it.");

    const history = request.mock.calls[1][0] as any[];
    const delivered = history.filter(m => m.role==="user" && m.input_audio);
    expect(delivered).toHaveLength(1);
    // The delivery also names the render the clip was cut from, so the session
    // archives the full-rate file instead of the mono preview.
    expect(delivered[0].input_audio).toEqual({
      data: base64,
      format: "wav",
      source_path: "/tmp/audition.wav",
    });
    expect(delivered[0].content).toBe("You asked to hear 0–4 s: this is that render.");
    expect(delivered[0].message_meta).toEqual({ version: 1, kind: "audition", audio_range: { start_seconds: 0, end_seconds: 4, source: false } });
    // The tool message still carries the result and its path, but the clip
    // rides once, in the delivery message.
    const toolMessage = history.find(m => m.role==="tool");
    expect(toolMessage.content).toContain('"path":"/tmp/audition.wav"');
    expect(toolMessage.content).not.toContain(base64);
    // Only the model's own call reached the DAW; the loop rendered nothing.
    expect(mock.mock.calls.map(c => (c[1] as any).tool)).toEqual(["audition_range"]);
  });
  it("delivers an archived passage without making another full-render copy", async () => {
    const base64 = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA=";
    const path = `/archive/audition-renders/${"a".repeat(64)}.wav`;
    mock.mockResolvedValue({
      archived_render: true, start_s: 35, end_s: 36,
      audio: { mime: "audio/wav", base64 },
    });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [
        call("audition_clip_source", { path, source_start_seconds: 35, duration_seconds: 1 }),
      ] })
      .mockResolvedValueOnce({ role: "assistant", content: "Heard the saved render." });
    await runAgent({ ...base(), request });
    const history = request.mock.calls[1][0] as any[];
    const delivery = history.find(message => message.input_audio);
    expect(delivery.content).toBe("You asked to hear source seconds 35–36: this is the saved arrangement render.");
    expect(delivery.input_audio).toEqual({ data: base64, format: "wav" });
  });

  it("delivers source audio at the model rate without archiving the original file", async () => {
    const base64 = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA=";
    mock.mockResolvedValue({
      clip_index: 2, start_s: 5, end_s: 6,
      audio: { mime: "audio/wav", sample_rate: 24000, channels: 2, base64 },
    });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [
        call("audition_clip_source", { clip_index: 2, source_start_seconds: 5, duration_seconds: 1 }),
      ] })
      .mockResolvedValueOnce({ role: "assistant", content: "Heard the source." });
    await runAgent({ ...base(), request, audioInput: { channels: 2, sample_rate: 24000 } });
    expect(mock).toHaveBeenCalledWith("ai_call_tool", {
      tool: "audition_clip_source",
      args: { clip_index: 2, source_start_seconds: 5, duration_seconds: 1, channel: "stereo", clip_sample_rate: 24000 },
    });
    const history = request.mock.calls[1][0] as any[];
    const delivered = history.filter(message => message.input_audio);
    expect(delivered).toHaveLength(1);
    expect(delivered[0].input_audio).toEqual({ data: base64, format: "wav" });
    expect(delivered[0].content).toContain("source seconds 5–6");
    expect(history.find(message => message.role === "tool").content).not.toContain(base64);
  });
  it("keeps source audio off the wire for a text-only model", async () => {
    mock.mockResolvedValue({ audio: { base64: "YXVkaW8=" }, start_s: 0, end_s: 1 });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", tool_calls: [call("audition_clip_source", { clip_index: 0 })] })
      .mockResolvedValueOnce({ role: "assistant", content: "I'll measure the attacks." });
    await runAgent({ ...base(), request, audioInput: false });
    const history = request.mock.calls[1][0] as any[];
    expect(history.some(message => message.input_audio)).toBe(false);
    expect(history.at(-1).content).toContain("audio_clip_transients");
  });
  it("adds no audio message when a tool result carries none", async () => {
    mock.mockResolvedValue({path:"/tmp/range.wav",start_s:0,end_s:1,duration_s:1,tracks:[0],stems:[],cache_hit:false});
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("render_range",{path:"/tmp/range.wav",start:0,end:1})]})
      .mockResolvedValueOnce({role:"assistant",content:"Measured."});
    await runAgent({...base(),request});
    const history = request.mock.calls[1][0] as any[];
    expect(history.some(m => m.input_audio)).toBe(false);
    expect(history.filter(m => m.role==="user")).toHaveLength(1);
    expect(history.filter(m => m.role==="tool")).toHaveLength(1);
  });
  it("never renders on its own: only the model's calls reach the DAW", async () => {
    mock.mockResolvedValue({tracks:[]});
    const request=vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("tracks_list",{})]})
      .mockResolvedValueOnce({role:"assistant",content:"Nothing to hear yet."});
    await runAgent({...base(),request});
    expect(mock.mock.calls.map(c => `${c[0]}:${(c[1] as any).tool}`)).toEqual(["ai_call_tool:tracks_list"]);
    expect(request.mock.calls[1][0].some((m: any) => m.input_audio)).toBe(false);
  });
  it("surfaces a refusal as a failed tool result", async () => {
    mock.mockRejectedValue("refused: 'render_offline' is excluded from the built-in AI panel");
    const report = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("render_range",{path:"x",start:0,end:1})]})
      .mockResolvedValueOnce({role:"assistant",content:"Cannot render offline."});
    await runAgent({...base(),request,report});
    expect(report).toHaveBeenCalledWith(
      expect.stringContaining("render_range: {\"error\":\"refused:"),
      true,
    );
    expect(request.mock.calls[1][0]).toContainEqual(expect.objectContaining({
      role:"tool",tool_call_id:"call-1",content:expect.stringContaining("refused:"),
    }));
  });
  it("hands a structured refusal's error, skill and hint to the model verbatim", async () => {
    const refusal = {error:"Unknown path segment: doc",hint:"read_skill('dsl-subtree-paths') teaches this",ok:false,skill:"dsl-subtree-paths"};
    mock.mockRejectedValue(refusal);
    const report = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("project_extract",{path:"doc"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Reading that skill."});
    await runAgent({...base(),request,report});
    const history = request.mock.calls[1][0] as any[];
    const tool = history.find(m => m.role === "tool");
    // The payload is not re-wrapped: `error` is untouched, `skill` and the
    // prose `hint` survive.
    expect(JSON.parse(tool.content)).toEqual(refusal);
    expect(tool.content).toContain('"skill":"dsl-subtree-paths"');
    expect(tool.content).toContain('"hint":"read_skill(\'dsl-subtree-paths\') teaches this"');
    expect(report).toHaveBeenCalledWith(expect.stringContaining('"skill":"dsl-subtree-paths"'), true);
  });
  it("allows a repeated read after compaction while still bounding repeated mutations", async () => {
    mock.mockResolvedValue({ dsl_source: "before", tracks: [] });
    const read = vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("project_extract",{path:"project"})]})
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("project_extract",{path:"project"},"call-2")]})
      .mockResolvedValueOnce({role:"assistant",content:"Rechecked the project."});
    expect((await runAgent({...base(),request:read})).text).toBe("Rechecked the project.");
    expect(read).toHaveBeenCalledTimes(3);
    const mutation = vi.fn()
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("tracks_add",{name:"piano"})]})
      .mockResolvedValueOnce({role:"assistant",tool_calls:[call("tracks_add",{name:"piano"},"call-2")]});
    const result = await runAgent({...base(),request:mutation});
    expect(result.failed).toBe(true);
    expect(result.text).toContain("repeated the same call twice (tracks_add");
    expect(mutation).toHaveBeenCalledTimes(2);
  });

  it("defers later calls in a batch once a proposal needs review", async () => {
    mock.mockImplementation(async command => command === "get_project" ? {dsl_source:"before"} : {valid:true});
    const reportTool = vi.fn();
    const trace: TurnTrace = {wire:[],edits:0};
    const request = vi.fn().mockResolvedValue({role:"assistant",content:null,tool_calls:[
      call("edit_subtree",{path:"project.properties",new_dsl:"tempo: 76"}),
      ...Array.from({length:MAX_TOOL_CALLS_PER_BATCH - 2}, (_, i) => call("tracks_add",{name:`track-${i}`},`deferred-${i}`)),
      call("tracks_add",{name:"piano"},"later"),
    ]});
    const result = await runAgent({...base(),request,reportTool,trace,permission:"require_approval"});
    expect(result.proposal?.expectedDsl).toBe("before");
    expect(reportTool).toHaveBeenCalledTimes(MAX_TOOL_CALLS_PER_BATCH);
    expect(mock).not.toHaveBeenCalledWith("ai_call_tool",expect.objectContaining({tool:"tracks_add"}));
    expect(trace.wire).toContainEqual(expect.objectContaining({role:"tool",tool_call_id:"later",content:expect.stringContaining('"applied":false')}));
    expect(reportTool).toHaveBeenLastCalledWith(expect.objectContaining({id:"later"}),expect.stringContaining('"deferred":true'),false);
  });

});

describe("a destructive call needs the user", () => {
  /** The panel surface plus one deletion the registry marks `confirm`. */
  const destructive = (): ToolDef[] => [
    ...tools(),
    def("tracks_remove", "tracks", {index:{type:"integer"}}, ["index"], true, "confirm"),
  ];
  const removeBass = (id = "call-1") => call("tracks_remove", {index:1}, id);

  it("pauses for approval before the call runs, then continues the same turn", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add", "tracks_remove"]) });
    setInvoke(transport);
    const asked: ApprovalRequest[] = [];
    const approve = vi.fn(async (request: ApprovalRequest) => {
      asked.push(request);
      return {approved:true, target:"Bass"};
    });
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Deleted the bass track."});
    const result = await runAgent({...base(), tools:destructive(), approve, request, permission:"require_approval"});

    // The loop asks with the call it is about to make, and nothing ran yet.
    expect(asked).toMatchObject([{tool:"tracks_remove", args:{index:1}, expectedDsl:"before"}]);
    expect(result.text).toBe("Deleted the bass track.");
    // Approving continues the same turn: one batch, one undo entry.
    expect(daw.commands).toEqual(["transaction_begin", "tracks_remove", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("asks for a reversible edit too, because the gate covers every mutating call", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:true}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_add", {name:"lead"})]})
      .mockResolvedValueOnce({role:"assistant",content:"Added the lead."});
    await runAgent({...base(), tools:destructive(), approve, request, permission:"require_approval"});
    expect(approve).toHaveBeenCalledTimes(1);
    expect(approve).toHaveBeenCalledWith(expect.objectContaining({tool:"tracks_add"}));
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
  });

  it("refuses the call the user declined, keeps the turn alive, and changes nothing", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:false, target:"Bass"}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Left the bass track alone."});
    const report = vi.fn();
    const result = await runAgent({...base(), tools:destructive(), approve, request, report, permission:"require_approval"});

    // The turn lives: the model reads a normal tool result and answers.
    expect(result.text).toBe("Left the bass track alone.");
    expect(result.failed).toBeUndefined();
    const tool = (request.mock.calls[1][0] as any[]).find(message => message.role === "tool");
    const declined = JSON.parse(tool.content);
    expect(declined).toMatchObject({declined:true, tool:"tracks_remove", target:"Bass"});
    expect(declined.error).toContain('the user declined tracks_remove for "Bass"');
    expect(declined.error).toContain("nothing in the project changed");
    // No transaction was opened, so the project is byte-identical and the undo
    // stack is empty: rejecting is not an edit that got rolled back.
    expect(daw.commands).toEqual([]);
    expect(daw.project).toBe("snapshot");
    expect(daw.undo).toEqual([]);
    expect(report).toHaveBeenCalledWith(expect.stringContaining('"declined":true'), false);
  });

  it("names the arguments when the call has no name to resolve", async () => {
    const { transport } = fakeDaw();
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:false}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Okay."});
    await runAgent({...base(), tools:destructive(), approve, request, permission:"require_approval"});
    const tool = (request.mock.calls[1][0] as any[]).find(message => message.role === "tool");
    expect(JSON.parse(tool.content).error).toContain('the user declined tracks_remove (arguments: {"index":1})');
  });

  it("refuses a call when nobody can approve it", async () => {
    // require_approval with no approver wired: silence is never an approval.
    const refusedFixture = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(refusedFixture.transport);
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"I will leave it and ask."});
    const report = vi.fn();
    const result = await runAgent({...base(), tools:destructive(), request, report, permission:"require_approval"});

    // Refused with a result the model can act on, and the turn continues.
    expect(result.text).toBe("I will leave it and ask.");
    expect(result.failed).toBeUndefined();
    const tool = (request.mock.calls[1][0] as any[]).find(message => message.role === "tool");
    const refused = JSON.parse(tool.content);
    expect(refused).toMatchObject({declined:false, refused:true, tool:"tracks_remove", arguments:{index:1}});
    expect(refused.error).toContain("no user is available to approve tracks_remove");
    expect(refused.error).toContain("nothing in the project changed");
    expect(report).toHaveBeenCalledWith(expect.stringContaining('"refused":true'), false);
    expect(refusedFixture.daw.commands).toEqual([]);
    expect(refusedFixture.daw.project).toBe("snapshot");
    expect(refusedFixture.daw.undo).toEqual([]);
  });

  it("asks under the default permission, which no caller has to pass", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:true, target:"Bass"}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Deleted the bass track."});
    // No `permission` and no `autoApply`: a fresh conversation's own default.
    const result = await runAgent({...base(), tools:destructive(), approve, request});
    expect(approve).toHaveBeenCalledTimes(1);
    expect(result.text).toBe("Deleted the bass track.");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_remove", "transaction_commit"]);
  });

  it("asks under write mode too, and applies the call once it is accepted", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:true}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Deleted the bass track."});
    await runAgent({...base(), tools:destructive(), approve, request, permission:"write"});
    // `write` decides what an automatic write does, not whether a deletion asks.
    expect(approve).toHaveBeenCalledTimes(1);
    expect(daw.commands).toEqual(["transaction_begin", "tracks_remove", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("does not ask a read-only call in any mode", async () => {
    const { transport } = fakeDaw();
    setInvoke(transport);
    const approve = vi.fn(async () => ({approved:true}));
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[call("tracks_list", {})]})
      .mockResolvedValueOnce({role:"assistant",content:"Two tracks."});
    const result = await runAgent({...base(), tools:destructive(), approve, request});
    expect(approve).not.toHaveBeenCalled();
    expect(result.text).toBe("Two tracks.");
  });

  it("skips the card for a tool this conversation already allowed", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(transport);
    const allowed: string[] = [];
    // Answering "always allow" is the surface's job; the loop only reads the list.
    const approve = vi.fn(async () => {
      allowed.push("tracks_remove");
      return { approved: true };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[
        call("tracks_remove", {index:0}, "call-1"),
        call("tracks_remove", {index:1}, "call-2"),
      ]})
      .mockResolvedValueOnce({role:"assistant",content:"Deleted both."});
    const result = await runAgent({...base(), tools:destructive(), approve, request, allowedTools: () => allowed});
    expect(result.text).toBe("Deleted both.");
    // One card for the first deletion; the second ran on the answer it produced.
    expect(approve).toHaveBeenCalledTimes(1);
    expect(daw.commands).toEqual(["transaction_begin", "tracks_remove", "tracks_remove", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("runs an allowed tool on the next turn without any approver", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_remove"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[removeBass()]})
      .mockResolvedValueOnce({role:"assistant",content:"Deleted the bass track."});
    // No `approve`: only a lifted floor can let this call through.
    const result = await runAgent({...base(), tools:destructive(), request, allowedTools: () => ["tracks_remove"]});
    expect(result.text).toBe("Deleted the bass track.");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_remove", "transaction_commit"]);
  });

  it("cancels while the card is open: the call never runs and the earlier edit stays", async () => {
    let cancelled = false;
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add", "tracks_remove"]) });
    setInvoke(transport);
    // The first call is approved and runs; the card for the second is settled
    // as a decline, and the loop then sees the cancel.
    let cards = 0;
    const approve = vi.fn(async () => {
      cards += 1;
      if (cards === 1) return { approved: true };
      cancelled = true;
      return { approved: false };
    });
    const request = vi.fn().mockResolvedValueOnce({role:"assistant",content:null,tool_calls:[
      call("tracks_add", {name:"lead"}, "call-1"),
      removeBass("call-2"),
    ]});
    const result = await runAgent({...base(), tools:destructive(), approve, request, cancelled:() => cancelled, permission:"require_approval"});

    expect(result.failed).toBe(true);
    expect(result.stop).toEqual({cancelled:true, done:1, planned:2, edits:1, committed:true});
    // The earlier edit stays as the turn's one entry; the call the card was
    // asking about was never dispatched.
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
  });
});

describe("a rollback the model asks for", () => {
  /** The panel surface: the rollback's own policy is `confirm`, as the registry says. */
  const destructive = (): ToolDef[] => [
    ...tools().filter(tool => tool.name !== "transaction_rollback"),
    def("transaction_rollback", "transaction", {}, [], false, "confirm"),
  ];
  const rollback = (id = "call-2") => call("transaction_rollback", {}, id);
  /** The tool messages the last request would replay, oldest first. */
  const wire = (request: ReturnType<typeof vi.fn>) =>
    (request.mock.calls.at(-1)![0] as { role: string; content: string }[])
      .filter(message => message.role === "tool");

  it("asks first, discards the batch, and does not close it again at the turn end", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const asked: ApprovalRequest[] = [];
    const approve = vi.fn(async (request: ApprovalRequest) => {
      asked.push(request);
      return { approved: true };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [rollback()] })
      .mockResolvedValueOnce({ role: "assistant", content: "Rolled the turn back." });
    const result = await runAgent({ ...base(), tools: destructive(), approve, request });

    // The card is the only gate that runs before the batch is discarded.
    expect(asked).toMatchObject([{ tool: "transaction_rollback", args: {} }]);
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_rollback"]);
    expect(daw.undo).toEqual([]);
    expect(daw.project).toBe("snapshot");
    // The model reads that the batch it asked to discard is gone, and the
    // earlier result no longer claims the edit is still applied.
    expect(JSON.parse(wire(request).at(-1)!.content)).toMatchObject({ rolled_back: true });
    expect(JSON.parse(wire(request)[0].content)).toMatchObject({ applied: false, rolled_back: true });
    expect(result.text).toBe("Rolled the turn back.");
    // The turn closed the batch itself, so nothing may commit or roll it back again.
    const closing = daw.commands.filter(command => command !== "tracks_add");
    expect(closing).toEqual(["transaction_begin", "transaction_rollback"]);
  });

  it("keeps the batch and its edits when the rollback is declined", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: false }));
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [rollback()] })
      .mockResolvedValueOnce({ role: "assistant", content: "Kept the edit." });
    const result = await runAgent({ ...base(), tools: destructive(), approve, request });

    // The declined call never reached the panel's tool path, and the batch the
    // turn already opened still commits its one entry.
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(JSON.parse(wire(request).at(-1)!.content)).toMatchObject({
      declined: true, tool: "transaction_rollback",
    });
    expect(result.text).toBe("Kept the edit.");
  });

  it("opens a fresh batch for the mutation that follows an approved rollback", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: true }));
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [rollback()] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "b" }, "call-3")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Started over." });
    const result = await runAgent({ ...base(), tools: destructive(), approve, request });

    // The discarded edits are gone, and the new one is the turn's only entry.
    expect(daw.commands).toEqual([
      "transaction_begin", "tracks_add", "transaction_rollback",
      "transaction_begin", "tracks_add", "transaction_commit",
    ]);
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(result.text).toBe("Started over.");
  });
});

describe("session permission modes", () => {
  /**
   * A document, its undo stack, and the two calls the modes differ on: one
   * registry mutation and one subtree replacement.
   */
  function modeDaw(validate: () => { valid: boolean; errors?: string[] } = () => ({ valid: true })) {
    let project = "before";
    const undoStack: string[] = [];
    const calls: string[] = [];
    const commands: string[] = [];
    const applied: { newDsl: string; targetPath: string; expectedDsl: string }[] = [];
    const mutating = new Set(["tracks_add", "tracks_remove"]);
    const transport = async <T,>(command: string, payload?: Record<string, unknown>): Promise<T> => {
      commands.push(command);
      switch (command) {
        case "get_project":
          return { dsl_source: project, project: { tracks: [{ name: "kick" }] }, revision: 1 } as T;
        case "extract_subtree":
          return { dsl_text: "track kick\n  instrument: synth/kick\n" } as T;
        case "ai_validate_edit":
          return validate() as T;
        case "diff_dsl":
          return { hunks: [{ lines: [{ kind: "Added", text: "+ kick" }] }] } as T;
        case "ai_apply_edit": {
          const edit = payload as { newDsl: string; targetPath: string; expectedDsl: string };
          applied.push(edit);
          undoStack.push(project);
          project = edit.newDsl;
          return { dsl_source: project, revision: 2, track_count: 1 } as T;
        }
        case "undo":
          project = undoStack.pop() ?? project;
          return { dsl_source: project, revision: 3 } as T;
        case "ai_call_tool": {
          const { tool } = payload as { tool: string };
          calls.push(tool);
          if (tool === "transaction_begin") return { active: true } as T;
          if (tool === "transaction_commit") return { committed: true } as T;
          if (tool === "transaction_rollback") return { rolled_back: true } as T;
          if (mutating.has(tool)) {
            undoStack.push(project);
            project = `${project}+${tool}`;
            return { revision: 2 } as T;
          }
          return { ok: true } as T;
        }
        default:
          throw new Error(`unexpected command ${command}`);
      }
    };
    return { transport, calls, commands, applied, project: () => project };
  }

  it("pauses a mutating call under require_approval and returns a refusal when it is rejected", async () => {
    const { transport, calls } = modeDaw();
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: false, target: "kick" }));
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "lead" })] })
      .mockResolvedValueOnce({ role: "assistant", content: "Left it alone." });
    const result = await runAgent({ ...base(), approve, request, permission: "require_approval" });

    // The person is asked before anything is dispatched, and the model reads a
    // refusal it can act on rather than a failure.
    expect(approve).toHaveBeenCalledTimes(1);
    expect(approve).toHaveBeenCalledWith(expect.objectContaining({ tool: "tracks_add", args: { name: "lead" } }));
    expect(calls).toEqual([]);
    expect(result.text).toBe("Left it alone.");
    expect(result.failed).toBeUndefined();
    const tool = (request.mock.calls[1][0] as { role: string; content: string }[])
      .find(message => message.role === "tool")!;
    expect(JSON.parse(tool.content)).toMatchObject({ declined: true, tool: "tracks_add" });
  });

  it("applies a valid replacement under autoreview without asking and records its diff", async () => {
    const { transport, calls, applied } = modeDaw();
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: false }));
    const preview = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("edit_subtree", { path: "track:kick", new_dsl: "track kick\n  instrument: synth/bass\n" })] })
      .mockResolvedValueOnce({ role: "assistant", content: "Applied." });
    const result = await runAgent({ ...base(), approve, request, permission: "autoreview", previewEdit: preview });

    expect(approve).not.toHaveBeenCalled();
    expect(preview).toHaveBeenCalledWith(expect.objectContaining({
      tool: "edit_subtree",
      path: "track:kick",
      before: expect.stringContaining("track kick"),
      after: "track kick\n  instrument: synth/bass\n",
    }));
    expect(applied).toEqual([{ newDsl: "track kick\n  instrument: synth/bass\n", targetPath: "track:kick", expectedDsl: "before" }]);
    expect(calls).toContain("transaction_begin");
    expect(calls).toContain("transaction_commit");
    expect(result.text).toBe("Applied.");
  });

  it("refuses an invalid replacement under autoreview with the validator's message and never applies it", async () => {
    const errors = ["track 'kick' has no instrument"];
    const { transport, calls, commands } = modeDaw(() => ({ valid: false, errors }));
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: true }));
    const preview = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("edit_subtree", { path: "track:kick", new_dsl: "track kick\n" })] })
      .mockResolvedValueOnce({ role: "assistant", content: "The replacement was refused." });
    await runAgent({ ...base(), approve, request, permission: "autoreview", previewEdit: preview });

    expect(approve).not.toHaveBeenCalled();
    expect(preview).not.toHaveBeenCalled();
    expect(calls).not.toContain("transaction_begin");
    expect(commands).not.toContain("ai_apply_edit");
    const tool = (request.mock.calls[1][0] as { role: string; content: string }[])
      .find(message => message.role === "tool")!;
    expect(JSON.parse(tool.content)).toMatchObject({ valid: false, errors });
  });

  it("applies a mutating call under write without a prompt or a review row", async () => {
    const { transport, calls, project } = modeDaw();
    setInvoke(transport);
    const approve = vi.fn(async () => ({ approved: false }));
    const preview = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "lead" })] })
      .mockResolvedValueOnce({ role: "assistant", content: "Added the lead." });
    const result = await runAgent({ ...base(), approve, request, permission: "write", previewEdit: preview });

    expect(approve).not.toHaveBeenCalled();
    expect(preview).not.toHaveBeenCalled();
    expect(calls).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(project()).toBe("before+tracks_add");
    expect(result.text).toBe("Added the lead.");
  });

  it("never prompts a read-only tool in any mode", async () => {
    for (const permission of ["require_approval", "autoreview", "write"] as const) {
      const { transport, calls } = modeDaw();
      setInvoke(transport);
      const approve = vi.fn(async () => ({ approved: false }));
      const preview = vi.fn();
      const request = vi.fn()
        .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_list", {})] })
        .mockResolvedValueOnce({ role: "assistant", content: "Listed." });
      await runAgent({ ...base(), approve, request, permission, previewEdit: preview });
      expect(approve, permission).not.toHaveBeenCalled();
      expect(preview, permission).not.toHaveBeenCalled();
      expect(calls, permission).toEqual(["tracks_list"]);
    }
  });
});

describe("one turn is one transaction", () => {
  it("groups every edit into one transaction and one undo entry", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [
        call("tracks_add", { name: "a" }, "call-1"),
        call("tracks_add", { name: "b" }, "call-2"),
      ] })
      .mockResolvedValueOnce({ role: "assistant", content: "Added both." });
    const result = await runAgent({ ...base(), request });
    expect(result.text).toBe("Added both.");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "tracks_add", "transaction_commit"]);
    expect(daw.project).toBe("snapshot+tracks_add+tracks_add");
    // One entry, and it holds the pre-turn project: one undo restores it.
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.snapshot).toBeNull();
  });

  it("leaves no transaction when the turn only reads", async () => {
    const { daw, transport } = fakeDaw();
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_list", {})] })
      .mockResolvedValueOnce({ role: "assistant", content: "Two tracks." });
    await runAgent({ ...base(), request });
    expect(daw.commands).toEqual(["tracks_list"]);
    expect(daw.undo).toEqual([]);
  });

  it("reports a refused begin as the call's result and rolls the stranded batch back", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    // A batch an earlier turn left open: the begin this call needs is refused.
    daw.snapshot = "snapshot";
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Could not add the track." });
    const reportTool = vi.fn();
    const result = await runAgent({ ...base(), request, reportTool });

    // The failure is the call's result, not an exception outside the loop.
    expect(result.text).toBe("Could not add the track.");
    expect(result.failed).toBeUndefined();
    // The stranded panel batch was rolled back instead of left open, so the
    // next turn can begin its own.
    expect(daw.snapshot).toBeNull();
    expect(daw.project).toBe("snapshot");
    const refused = JSON.parse(reportTool.mock.calls[0][1]);
    expect(refused.error).toContain("a project transaction is already active");
    expect(refused.rolled_back).toBe(true);
    expect(refused.applied).toBeUndefined();
  });

  it("keeps a cancelled turn's mutation results applied on the wire", async () => {
    let cancelled = false;
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      onCall: (_tool, count) => { if (count === 2) cancelled = true; },
    });
    setInvoke(transport);
    const request = vi.fn().mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [
      call("tracks_add", { name: "a" }, "call-1"),
      call("tracks_add", { name: "b" }, "call-2"),
    ]});
    const trace: TurnTrace = { wire: [], edits: 0 };
    const result = await runAgent({ ...base(), request, cancelled: () => cancelled, trace });

    expect(result.failed).toBe(true);
    expect(result.stop).toMatchObject({ cancelled: true, edits: 2, committed: true });
    expect(daw.project).toBe("snapshot+tracks_add+tracks_add");
    // A Continue replays this wire: the results must still read as applied.
    const tools = trace.wire.filter(message => message.role === "tool");
    expect(tools).toHaveLength(2);
    for (const message of tools) {
      expect(JSON.parse(String(message.content))).toMatchObject({ revision: 1 });
      expect(JSON.parse(String(message.content)).rolled_back).toBeUndefined();
    }
  });

  it("cancels while a call is in flight: that call lands, then the whole turn rolls back", async () => {
    let cancelled = false;
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      onCall: (_tool, count) => { if (count === 3) cancelled = true; },
    });
    setInvoke(transport);
    const calls = ["a", "b", "c", "d", "e"].map((name, index) => call("tracks_add", { name }, `call-${index}`));
    const request = vi.fn().mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: calls });
    const report = vi.fn();
    const result = await runAgent({ ...base(), request, cancelled: () => cancelled, report });

    // Cancelled at call 3 of 5: the call already in flight completed and is
    // counted, and every applied edit stays in the turn's one entry.
    expect(result.failed).toBe(true);
    expect(result.stop).toEqual({ cancelled: true, done: 3, planned: 5, edits: 3, inFlight: "tracks_add", committed: true });
    expect(daw.commands).toEqual([
      "transaction_begin", "tracks_add", "tracks_add", "tracks_add", "transaction_commit",
    ]);
    expect(daw.project).toBe("snapshot+tracks_add+tracks_add+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
    // The in-flight result was reported before the stop, so the transcript
    // shows what completed.
    expect(report).toHaveBeenCalledTimes(3);
  });

  it("keeps the batch open when the commit itself fails", async () => {
    let cancelled = false;
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      commitError: "another batch is open",
      onCall: (_tool, count) => { if (count === 1) cancelled = true; },
    });
    setInvoke(transport);
    const request = vi.fn().mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] });
    const result = await runAgent({ ...base(), request, cancelled: () => cancelled });
    expect(result.stop).toEqual({
      cancelled: true, done: 1, planned: 1, edits: 1,
      inFlight: "tracks_add", commitError: "another batch is open",
    });
    // Nothing was rolled back: the project still has the edit, the batch is
    // still open for Continue or the message's revert action.
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
  });

  it("keeps the turn and its earlier edits when the backend refuses a mutation", async () => {
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      fail: (tool, count) => tool === "tracks_add" && count === 2,
    });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [
        call("tracks_add", { name: "a" }, "call-1"),
        call("tracks_add", { name: "b" }, "call-2"),
      ] })
      .mockResolvedValueOnce({ role: "assistant", content: "Added what I could." });
    const report = vi.fn();
    const result = await runAgent({ ...base(), request, report });

    // The refusal is a normal tool result, not the end of the turn.
    expect(result.text).toBe("Added what I could.");
    expect(result.failed).toBeUndefined();
    expect(result.stop).toBeUndefined();
    const tool = (request.mock.calls[1][0] as any[]).find(message => message.tool_call_id === "call-2");
    expect(JSON.parse(tool.content)).toEqual({ error: "tracks_add refused" });
    expect(report).toHaveBeenCalledWith('tracks_add: {"error":"tracks_add refused"}', true);
    // The batch kept the edit before the refusal and commits it once at the end.
    expect(daw.commands).toEqual([
      "transaction_begin", "tracks_add", "tracks_add", "transaction_commit",
    ]);
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.project).toBe("snapshot+tracks_add");
  });

  it("takes a different retry after a refusal, in the same turn", async () => {
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      fail: (tool, count) => tool === "tracks_add" && count === 1,
    });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "bass" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "bass2" }, "call-2")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Added the bass." });
    const result = await runAgent({ ...base(), request });

    expect(result.text).toBe("Added the bass.");
    expect(result.failed).toBeUndefined();
    expect(daw.commands).toEqual([
      "transaction_begin", "tracks_add", "tracks_add", "transaction_commit",
    ]);
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.project).toBe("snapshot+tracks_add");
  });

  it("leaves no undo entry when every mutation in the turn was refused", async () => {
    const { daw, transport } = fakeDaw({
      fail: (tool) => tool === "tracks_add",
    });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "b" }, "call-2")] })
      .mockResolvedValueOnce({ role: "assistant", content: "I could not add a track." });
    const result = await runAgent({ ...base(), request });

    expect(result.text).toBe("I could not add a track.");
    expect(result.failed).toBeUndefined();
    // The batch closes at the end of the turn, but nothing in it changed, so it
    // publishes no revision and no undo entry.
    expect(daw.commands).toEqual([
      "transaction_begin", "tracks_add", "tracks_add", "transaction_commit",
    ]);
    expect(daw.undo).toEqual([]);
    expect(daw.project).toBe("snapshot");
  });

  it("commits what landed when the round cap ends the turn, as one undo entry", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    let step = 0;
    const request = vi.fn(async () => ({
      role: "assistant", content: null,
      tool_calls: [call("tracks_add", { name: `t${step++}` }, `call-${step}`)],
    }));
    const result = await runAgent({ ...base(), request });
    expect(result.failed).toBe(true);
    expect(result.text).toContain(`stopped after ${MAX_TOOL_ROUNDS} tool rounds`);
    // A cap is a budget stop: the batch commits, so the notes stay and the turn
    // is still one undo entry.
    expect(result.text).toContain(`${MAX_TOOL_ROUNDS} applied edits were kept`);
    expect(result.stop).toEqual({
      cancelled: false, done: MAX_TOOL_ROUNDS, planned: MAX_TOOL_ROUNDS,
      edits: MAX_TOOL_ROUNDS, committed: true,
    });
    expect(daw.project).not.toBe("snapshot");
    expect(daw.undo).toHaveLength(1);
    expect(daw.commands.at(-1)).toBe("transaction_commit");
    expect(daw.commands).not.toContain("transaction_rollback");
  });

  it("reports the cap with nothing to commit when no edit had been applied", async () => {
    const { daw, transport } = fakeDaw();
    setInvoke(transport);
    let step = 0;
    const request = vi.fn(async () => ({
      role: "assistant", content: null,
      tool_calls: [call("tracks_list", { step: step++ }, `call-${step}`)],
    }));
    const result = await runAgent({ ...base(), request });
    expect(result.failed).toBe(true);
    expect(result.text).toBe(
      `stopped after ${MAX_TOOL_ROUNDS} tool rounds; the work is unfinished; the last action was tracks_list`,
    );
    // The stop facts still exist, so the panel can say where the turn stopped
    // and offer Continue; the project never opened a batch.
    expect(result.stop).toEqual({
      cancelled: false, done: MAX_TOOL_ROUNDS, planned: MAX_TOOL_ROUNDS, edits: 0, committed: true,
    });
    expect(daw.project).toBe("snapshot");
    expect(daw.commands).toHaveLength(MAX_TOOL_ROUNDS);
    expect(daw.commands.every(command => command === "tracks_list")).toBe(true);
  });

  it("adopts a transaction the model opens instead of nesting one", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("transaction_begin", { label: "pass" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-2")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("transaction_commit", {}, "call-3")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Done." });
    const result = await runAgent({ ...base(), request });
    expect(result.text).toBe("Done.");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("keeps one batch when the model commits in the middle of the turn", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("transaction_begin", {}, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-2")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("transaction_commit", {}, "call-3")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "b" }, "call-4")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Done." });
    const result = await runAgent({ ...base(), request });
    expect(result.text).toBe("Done.");
    // The model's own commit is answered by the loop, so its edits and the ones
    // after it stay in the same batch: one entry, restoring the pre-turn project.
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "tracks_add", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
    expect(daw.project).toBe("snapshot+tracks_add+tracks_add");
  });

  it("keeps the edits a failed turn had already applied", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockRejectedValueOnce(new Error("provider returned 500"));
    const report = vi.fn();
    const result = await runAgent({ ...base(), request, report });
    expect(result.failed).toBe(true);
    expect(result.stop).toMatchObject({ committed: true, failureKept: true, edits: 1 });
    expect(result.text).toContain("applied edits were kept");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
    expect(report).toHaveBeenLastCalledWith("provider returned 500; 1 applied edits were kept; Continue resumes from the last tool result", true);
  });

  it("keeps the edits a timed-out turn already landed", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockRejectedValueOnce({ kind: "timeout", message: "Provider request timed out" });
    const report = vi.fn();
    const result = await runAgent({ ...base(), request, report });
    expect(result.failed).toBe(true);
    expect(result.stop).toMatchObject({ committed: true, timedOut: true, edits: 1 });
    expect(result.text).toContain("applied edits were kept");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("reports the turn's own label on the one undo entry it commits", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    const labels: unknown[] = [];
    setInvoke(transport);
    const begin = transport;
    setInvoke(async (command, payload) => {
      if (command === "ai_call_tool" && (payload as any).tool === "transaction_begin") labels.push((payload as any).args.label);
      return begin(command, payload);
    });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Done." });
    await runAgent({ ...base(), request, label: "Add one track" });
    expect(labels).toEqual(["Add one track"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });
});

describe("a stopped turn can be sent again", () => {
  it("records the wire it held and the edits a stop kept", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const trace: TurnTrace = { wire: [], edits: 0 };
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockRejectedValueOnce(new Error("connection reset"));
    const result = await runAgent({ ...base(), request, trace });
    expect(result.failed).toBe(true);
    expect(result.stop).toMatchObject({ committed: true, failureKept: true, edits: 1 });

    // A Continue resumes this array: the question, the call and its result.
    expect(trace.wire).toEqual(request.mock.calls.at(-1)![0]);
    expect(trace.wire.map(message => message.role)).toEqual(["user", "assistant", "tool"]);
    // The turn kept its one edit as the entry a manual revert can restore.
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("resumes a kept turn's wire without opening a second batch", async () => {
    const { daw, transport } = fakeDaw({ mutating: new Set(["tracks_add"]) });
    setInvoke(transport);
    const stopped: TurnTrace = { wire: [], edits: 0 };
    const first = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] })
      .mockRejectedValueOnce(new Error("connection reset"));
    await runAgent({ ...base(), request: first, trace: stopped });
    expect(stopped.wire.map(message => message.role)).toEqual(["user", "assistant", "tool"]);

    const result = await runAgent({
      ...base(),
      messages: stopped.wire,
      request: async () => ({ role: "assistant", content: "Added." }),
      trace: { wire: [], edits: 0 },
    });
    expect(result.text).toBe("Added.");
    expect(daw.commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(daw.undo).toEqual(["snapshot"]);
  });

  it("reports a commit that failed without rolling the edit back", async () => {
    let cancelled = false;
    const { daw, transport } = fakeDaw({
      mutating: new Set(["tracks_add"]),
      commitError: "another batch is open",
      onCall: (_tool, count) => { if (count === 1) cancelled = true; },
    });
    setInvoke(transport);
    const trace: TurnTrace = { wire: [], edits: 0 };
    const request = vi.fn().mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "a" }, "call-1")] });
    const result = await runAgent({ ...base(), request, cancelled: () => cancelled, trace });
    expect(result.stop?.commitError).toBe("another batch is open");
    expect(trace.restoreError).toBeUndefined();
    expect(daw.project).toBe("snapshot+tracks_add");
  });
});

describe("an empty reply is a failure, not an answer", () => {
  it("returns a failed turn and keeps the wire a Retry resends", async () => {
    mock.mockResolvedValue({});
    const trace: TurnTrace = { wire: [], edits: 0 };
    const request = vi.fn().mockResolvedValue({ role: "assistant", content: "  \n" });
    const result = await runAgent({ ...base(), request, trace });

    expect(result.failed).toBe(true);
    expect(result.text).toBe("Provider returned no text.");
    expect(result.proposal).toBeUndefined();
    // The turn still holds its wire, so the panel can offer Retry.
    expect(trace.wire.map(message => message.role)).toEqual(["user"]);
  });

  it("commits the batch the empty reply ends, so the edits are one entry", async () => {
    const commands: string[] = [];
    mock.mockImplementation(async (command: string, payload?: any) => {
      if (command !== "ai_call_tool") return {};
      commands.push(payload.tool);
      if (payload.tool === "transaction_begin") return { active: true, label: payload.args.label };
      if (payload.tool === "transaction_commit") return { committed: true };
      return { revision: 1 };
    });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("tracks_add", { name: "bass" })] })
      .mockResolvedValueOnce({ role: "assistant", content: "" });
    const result = await runAgent({ ...base(), request });

    expect(result.failed).toBe(true);
    expect(result.stop).toBeUndefined();
    expect(commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
  });

  it("names each call before it is dispatched", async () => {
    mock.mockResolvedValue({ ok: true });
    const onTool = vi.fn();
    const request = vi.fn()
      .mockResolvedValueOnce({
        role: "assistant", content: null,
        tool_calls: [call("tracks_list", {}, "call-1"), call("plugin_search", { query: "dex" }, "call-2")],
      })
      .mockResolvedValueOnce({ role: "assistant", content: "Done." });
    await runAgent({ ...base(), request, onTool });

    expect(onTool.mock.calls.map(([tool]) => tool)).toEqual(["tracks_list", "plugin_search"]);
  });
});

describe("the in-turn wire budget", () => {
  /** Every call a wire carries and the results that answer it, checked both ways. */
  const pairs = (wire: any[]) => {
    const calls = new Set<string>();
    for (const message of wire) for (const call of message.tool_calls ?? []) calls.add(call.id);
    const answers = new Set<string>();
    for (const message of wire) {
      if (message.role !== "tool") continue;
      expect(calls.has(message.tool_call_id)).toBe(true);
      answers.add(message.tool_call_id);
    }
    expect([...calls].sort()).toEqual([...answers].sort());
    return calls;
  };
  const rounds = (wire: any[]) => [...new Set<string>(
    wire.filter((m: any) => m.role === "tool").map((m: any) => String(m.tool_call_id).split("-")[0]),
  )].sort((left, right) => Number(left.slice(1)) - Number(right.slice(1)));
  /** The batch the fake model asks for; ids name the round and the call. */
  const batch = (round: number, size = 8): ToolCall[] =>
    Array.from({length:size}, (_, index) => call("tracks_list", {}, `r${round}-${index}`));

  it("keeps the loop's caps aligned with native validation", () => {
    const native = readFileSync("src-tauri/src/ai/mod.rs", "utf8");
    const sessions = readFileSync("src-tauri/src/ai/sessions.rs", "utf8");
    const size = (expression: string) =>
      expression.split("*").reduce((total, part) => total * Number(part.trim()), 1);
    expect(Number(native.match(/pub const MAX_WIRE_MESSAGES: usize = (\d+);/)?.[1])).toBe(WIRE_MESSAGE_LIMIT);
    expect(size(native.match(/pub const LIMIT: usize = ([\d\s*]+);/)?.[1] ?? "0")).toBe(WIRE_TEXT_LIMIT);
    expect(Number(sessions.match(/pub const RETAINED_TOOL_PAIRS: usize = (\d+);/)?.[1])).toBe(WIRE_KEPT_TOOL_PAIRS);
  });

  it("folds a turn that accumulates 120 tool results before every send", async () => {
    mock.mockResolvedValue({} as any);
    const wires: any[][] = [];
    const report = vi.fn();
    let round = 0;
    const request = vi.fn(async (wire: any[]) => {
      // Snapshot: the loop keeps sending the same array.
      wires.push(JSON.parse(JSON.stringify(wire)));
      round += 1;
      if (round <= 15) return {role:"assistant",content:null,tool_calls:batch(round)};
      return {role:"assistant",content:"Done"};
    });
    const result = await runAgent({...base(),request,report});
    expect(result.text).toBe("Done");
    expect(wires).toHaveLength(16);
    // Fifteen rounds of eight calls are the 120 results the batch expects.
    expect(round).toBe(16);
    for (const wire of wires) {
      expect(wire.length).toBeLessThanOrEqual(WIRE_MESSAGE_LIMIT);
      expect(JSON.stringify(wire).length).toBeLessThanOrEqual(WIRE_TEXT_LIMIT);
      // The turn's first ask survives every fold.
      expect(wire[0]).toMatchObject({role:"user",content:"edit"});
      pairs(wire);
    }
    // The first folded wire keeps the newest pairs whole and drops the rest.
    const folded = wires.find((wire) => wire.some((message) => message.context === "summary"))!;
    expect(rounds(folded)).toEqual(["r6","r7","r8","r9","r10","r11"]);
    expect(folded.filter((m:any) => m.role === "tool")).toHaveLength(WIRE_KEPT_TOOL_PAIRS * 8);
    // The compaction itself is a transcript row, not just a smaller wire.
    expect(report.mock.calls.some(([text]) => /^wire compacted before send/.test(text))).toBe(true);
  });

  it("drops a call together with its results, never one without the other", async () => {
    // An old pair alone is over the text budget: only the whole pair may go.
    const messages: WireMessage[] = [
      {role:"user",content:"edit"},
      {role:"assistant",content:null,tool_calls:[call("plugin_search",{query:"x"},"old")]},
      {role:"tool",tool_call_id:"old",content:"z".repeat(WIRE_TEXT_LIMIT - 40_000)},
      {role:"user",content:"continue"},
    ];
    mock.mockResolvedValue({} as any);
    const report = vi.fn();
    const request = vi.fn().mockResolvedValue({role:"assistant",content:"Done"});
    const result = await runAgent({...base(),messages,request,report});

    expect(result.text).toBe("Done");
    const wire = request.mock.calls[0][0] as any[];
    expect(JSON.stringify(wire).length).toBeLessThanOrEqual(WIRE_TEXT_LIMIT);
    expect(wire.map((m:any) => m.role)).toEqual(["user","user","user"]);
    expect(wire[1].context).toBe("summary");
    expect(wire[1].content).toContain("1 tool call/result pair");
    pairs(wire);
    expect(report.mock.calls.some(([text]) => /folded 1 older tool pair/.test(text))).toBe(true);
    // The caller's own array is untouched: the loop folds its copy.
    expect(messages).toHaveLength(4);
  });

  it("keeps a mutation that survived a fold and its applied result", async () => {
    const {daw, transport} = fakeDaw({mutating:new Set(["tracks_add"])});
    setInvoke(transport);
    let cancelled = false;
    let round = 0;
    const trace: TurnTrace = {wire: [], edits: 0};
    const request = vi.fn(async () => {
      round += 1;
      if (round === 6) return {role:"assistant",content:null,tool_calls:[call("tracks_add",{name:"pad"},"edit-1")]};
      if (round < 16) return {role:"assistant",content:null,tool_calls:batch(round)};
      cancelled = true;
      return {role:"assistant",content:"Done"};
    });
    const result = await runAgent({...base(),request,trace,cancelled:() => cancelled});

    expect(result.stop).toMatchObject({cancelled:true, edits:1, committed:true});
    expect(daw.project).toBe("snapshot+tracks_add");
    expect(daw.undo).toEqual(["snapshot"]);
    // The mutation's pair survived the fold; its result still reads as applied.
    const kept = trace.wire.find((message) => message.tool_call_id === "edit-1");
    expect(JSON.parse(kept!.content!)).toMatchObject({revision:1});
    expect(JSON.parse(kept!.content!).rolled_back).toBeUndefined();
  });

  it("keeps the compacted wire for the Retry of a failed turn", async () => {
    mock.mockResolvedValue({} as any);
    const trace: TurnTrace = {wire: [], edits: 0};
    let round = 0;
    const request = vi.fn(async () => {
      round += 1;
      if (round <= 15) return {role:"assistant",content:null,tool_calls:batch(round)};
      throw new Error("stream failed");
    });
    await expect(runAgent({...base(),request,trace})).rejects.toThrow("stream failed");

    expect(trace.wire.length).toBeLessThanOrEqual(WIRE_MESSAGE_LIMIT);
    expect(trace.wire.some((message) => message.context === "summary")).toBe(true);
    pairs(trace.wire as any[]);
  });

  it("folds old text when the byte budget is over before the row cap", async () => {
    const messages: WireMessage[] = [
      {role:"user",content:"edit"},
      {role:"assistant",content:"x".repeat(WIRE_TEXT_LIMIT - 4_096)},
      {role:"assistant",content:"older answer"},
    ];
    mock.mockResolvedValue({} as any);
    const request = vi.fn().mockResolvedValue({role:"assistant",content:"Done"});
    const result = await runAgent({...base(),messages,request});

    expect(result.text).toBe("Done");
    const wire = request.mock.calls[0][0] as any[];
    expect(JSON.stringify(wire).length).toBeLessThanOrEqual(WIRE_TEXT_LIMIT);
    expect(wire).toHaveLength(3);
    expect(wire[0]).toEqual({role:"user",message_meta:{version:1,kind:"user"},content:"edit"});
    expect(wire[1].context).toBe("summary");
    expect(wire[2]).toEqual({role:"assistant",message_meta:{version:1,kind:"assistant"},content:"older answer"});
  });

  it("refuses to send a wire it cannot fold under the cap", async () => {
    const messages: WireMessage[] = [{role:"user",content:"x".repeat(WIRE_TEXT_LIMIT + 10)}];
    const request = vi.fn();
    await expect(runAgent({...base(),messages,request})).rejects.toThrow(/cannot bring it under/);
    expect(request).not.toHaveBeenCalled();
  });
});
