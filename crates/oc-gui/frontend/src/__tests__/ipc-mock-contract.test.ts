import { describe, expect, it } from "vitest";
import { invokeMock, TRANSPORT_STOPPED } from "./setup";

describe("default IPC test boundary", () => {
  it("rejects a misspelled command instead of returning a project", async () => {
    await expect(invokeMock("get_projet")).rejects.toThrow("Unexpected Tauri command in test: get_projet");
  });

  it("returns the documented shape for an explicitly supported command", async () => {
    await expect(invokeMock("get_transport_state")).resolves.toEqual(TRANSPORT_STOPPED());
  });
});
