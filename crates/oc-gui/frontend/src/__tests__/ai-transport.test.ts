import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { invoke, setInvoke } from "../ai/transport";

const mock = vi.mocked(tauriInvoke);
beforeEach(() => { mock.mockReset(); setInvoke(null); });

describe("agent transport", () => {
  it("reaches the app through the Tauri invoke when no transport is installed", async () => {
    mock.mockResolvedValue({dsl_source: "before"});
    await expect(invoke("get_project")).resolves.toEqual({dsl_source: "before"});
    expect(mock).toHaveBeenCalledWith("get_project", undefined);
  });

  it("hands the same command and arguments to an installed transport, and restores the app path", async () => {
    const headless = vi.fn().mockResolvedValue({ok: true});
    setInvoke(headless);
    await expect(invoke("ai_call_tool", {tool: "tracks_list", args: {}})).resolves.toEqual({ok: true});
    expect(headless).toHaveBeenCalledWith("ai_call_tool", {tool: "tracks_list", args: {}});
    expect(mock).not.toHaveBeenCalled();
    setInvoke(null);
    mock.mockResolvedValue({ok: true});
    await invoke("ai_call_tool", {tool: "tracks_list", args: {}});
    expect(mock).toHaveBeenCalledTimes(1);
  });
});
