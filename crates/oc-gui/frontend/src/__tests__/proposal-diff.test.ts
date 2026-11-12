import { describe, expect, it, vi, afterEach } from "vitest";
import { proposalOldDsl } from "../ai/proposalDiff";
import { setInvoke } from "../ai/transport";
afterEach(() => setInvoke(null));
describe("proposal diff insertion", () => {
  it("shows a validated new named global pattern as an addition", async () => {
    const invoke = vi.fn(async (command:string) => {
      if (command === "extract_subtree") throw "Pattern 'bass_verse_a' not found";
      return {dsl_source:"current",project:{patterns:[]}};
    });
    setInvoke(invoke as never);
    expect(await proposalOldDsl("pattern:bass_verse_a", "current")).toBe("");
  });
  it("preserves existing subtree text", async () => {
    setInvoke(vi.fn(async () => ({dsl_text:"old pattern"})) as never);
    expect(await proposalOldDsl("pattern:existing", "current")).toBe("old pattern");
  });
  it("preserves transport, nested target and stale-snapshot errors", async () => {
    for (const [path,error] of [["pattern:p","connection lost"],["track:Bass.pattern:p","Pattern 'p' not found"],["pattern:4","Pattern index 4 out of range"]]) {
      const invoke=vi.fn(async()=>{throw error;}); setInvoke(invoke as never);
      await expect(proposalOldDsl(path,"current")).rejects.toBe(error);
      expect(invoke).toHaveBeenCalledTimes(1);
    }
    for (const snapshot of [{dsl_source:"changed",project:{patterns:[]}},{dsl_source:"current",project:{patterns:[{name:"p"}]}}]) {
      setInvoke(vi.fn(async(command:string)=>{if(command==="extract_subtree")throw "Pattern 'p' not found";return snapshot;}) as never);
      await expect(proposalOldDsl("pattern:p","current")).rejects.toBe("Pattern 'p' not found");
    }
  });
});
