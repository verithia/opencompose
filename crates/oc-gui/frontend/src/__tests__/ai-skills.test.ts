import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { runAgent, type ToolDef } from "../ai/agent";

// `list_skills` / `read_skill` are served by the Rust loader over the same
// dispatcher as `/call` and `/mcp`. The panel keeps no skill table of its own,
// so these tests assert the IPC path and the backend's JSON, not a local copy.

const tools = (): ToolDef[] => [
  {name:"list_skills",description:"",area:"skills",inputSchema:{type:"object",properties:{},required:[],additionalProperties:false}},
  {name:"read_skill",description:"",area:"skills",inputSchema:{type:"object",properties:{id:{type:"string"}},required:["id"],additionalProperties:false}},
];

const INDEX = [{
  id: "dsl-subtree-paths",
  title: "DSL subtree paths",
  when: "Before extracting or replacing any subtree.",
  category: "editing",
  version: 1,
  source: "bundled",
}];

function backend(toolResults: Record<string, unknown>) {
  vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd !== "ai_call_tool") throw new Error(`unexpected command ${cmd}`);
    const typed = (args ?? {}) as { tool: string };
    if (!(typed.tool in toolResults)) throw new Error(`unknown skill tool ${typed.tool}`);
    return toolResults[typed.tool];
  });
}

const call = (name: string, args: object, id: string) => ({
  id, type: "function" as const, function: {name, arguments: JSON.stringify(args)},
});

describe("skills over the backend dispatcher", () => {
  it("fetches the index and a body through the shared tool dispatcher", async () => {
    vi.mocked(invoke).mockClear();
    backend({ list_skills: INDEX, read_skill: {id:"dsl-subtree-paths",title:"DSL subtree paths",version:1,body:"BODY",tools:["project_extract"]} });
    const request = vi.fn()
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("list_skills", {}, "index")] })
      .mockResolvedValueOnce({ role: "assistant", content: null, tool_calls: [call("read_skill", {id: "dsl-subtree-paths"}, "body")] })
      .mockResolvedValueOnce({ role: "assistant", content: "Read the path grammar before editing." });
    const result = await runAgent({providerId:"test",nativeTools:true,messages:[],expectedDsl:"baseline",tools:tools(),cancelled:()=>false,report:()=>{},request});
    expect(result.proposal).toBeUndefined();

    // Both calls went to Rust, in order, with the id the model asked for.
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["ai_call_tool", {tool:"list_skills", args:{}}],
      ["ai_call_tool", {tool:"read_skill", args:{id:"dsl-subtree-paths"}}],
    ]);
    const results = request.mock.calls[2][0].filter((m: {role: string}) => m.role === "tool");
    expect(results.map((m: {tool_call_id: string}) => m.tool_call_id)).toEqual(["index", "body"]);
    expect(JSON.parse(results[0].content)).toEqual(INDEX);
    expect(JSON.parse(results[1].content).body).toBe("BODY");
  });

  it("returns a backend refusal as a tool result instead of a local throw", async () => {
    vi.mocked(invoke).mockClear();
    backend({ list_skills: INDEX, read_skill: { error: "unknown skill 'nope'; call list_skills for the exact ids" } });
    const reports: {text: string; failed: boolean}[] = [];
    const request = vi.fn()
      .mockResolvedValueOnce({role:"assistant",content:JSON.stringify({tool:"read_skill",args:{id:"nope"}})})
      .mockResolvedValueOnce({role:"assistant",content:"I will list the ids first."});
    await runAgent({providerId:"test",nativeTools:false,messages:[],expectedDsl:"baseline",tools:tools(),cancelled:()=>false,report:(text,failed)=>reports.push({text,failed}),request});
    expect(vi.mocked(invoke).mock.calls).toEqual([["ai_call_tool", {tool:"read_skill", args:{id:"nope"}}]]);
    expect(reports[0].text).toContain("unknown skill 'nope'");
    expect(reports[0].failed).toBe(false);
  });
});
