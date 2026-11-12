/**
 * The build chip must reach the panel while the transport is stopped: a
 * structural edit rebuilds every instrument, and the transport poll that used
 * to feed the chip only runs during playback.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "../setup";
import App from "../../App";
import { store } from "../../stores/project";

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "take_startup_project") return null;
    if (cmd === "scan_plugins") return [];
    if (cmd === "project_build_progress") {
      return { stage: "instruments", label: "Serum 2", done: 0, total: 3 };
    }
    // Stop everything: the chip cannot come from a transport poll.
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    return makeSnapshot();
  });
});

afterEach(cleanup);

describe("build progress while stopped", () => {
  it("polls the render thread's own report and shows the chip", async () => {
    const view = render(() => <App />);
    await waitFor(() => {
      const chip = view.container.querySelector(".status-chip.build-progress");
      expect(chip).toBeTruthy();
      expect(chip!.textContent).toBe("Preparing instruments · Serum 2 (1/3)");
    });
    expect(store.transport().playing).toBe(false);
  });

  it("ignores a reply that is not a build snapshot", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      // A backend without the command answers with the project snapshot.
      return makeSnapshot();
    });
    const view = render(() => <App />);
    await new Promise((resolve) => setTimeout(resolve, 500));
    expect(view.container.querySelector(".status-chip.build-progress")).toBeNull();
  });
});
