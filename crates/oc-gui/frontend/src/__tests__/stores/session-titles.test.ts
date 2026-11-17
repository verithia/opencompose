import { describe, expect, it, vi } from "vitest";
import { invokeMock } from "../setup";
import { sessionTitle } from "../../stores/sessionTitles";

describe("conversation titles", () => {
  it("loads the list on the first unknown id without the AI panel", async () => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (command: string) =>
      command === "sessions_list" ? [{ id: "s1", title: "Drum ideas" }] : null);

    expect(sessionTitle("s1")).toBeUndefined();
    await vi.waitFor(() => expect(sessionTitle("s1")).toBe("Drum ideas"));
    expect(invokeMock.mock.calls.filter(([command]) => command === "sessions_list")).toHaveLength(1);
    expect(sessionTitle("missing")).toBeUndefined();
    expect(invokeMock.mock.calls.filter(([command]) => command === "sessions_list")).toHaveLength(1);
  });
});
