/**
 * The build chip must reach the panel while the transport is stopped: a
 * structural edit rebuilds every instrument, and the transport poll that used
 * to feed the chip only runs during playback.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, TRANSPORT_PLAYING, TRANSPORT_STOPPED } from "../setup";
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
      expect(chip!.querySelector(".build-progress-text:not(.build-progress-compact)")!.textContent)
        .toBe("Preparing instruments · Serum 2 (1/3)");
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

  it("drops the report a paused transport still carries once the load has finished", async () => {
    const loading = { stage: "background", label: "", done: 1, total: 26, generation: 7, progress: 964 };
    let finished = false;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "project_build_progress") return finished ? null : loading;
      if (cmd === "transport_play" || cmd === "get_transport_state") {
        return { ...TRANSPORT_PLAYING(), build: loading };
      }
      // The pause lands while the last instrument is still loading.
      if (cmd === "transport_pause") return { ...TRANSPORT_STOPPED(), position_seconds: 3, build: loading };
      return makeSnapshot();
    });
    const view = render(() => <App />);
    const chip = () => view.container.querySelector(".status-chip.build-progress");
    await store.play();
    await waitFor(() => expect(chip()).toBeTruthy());
    await store.pause();
    finished = true;
    // The dedicated poll sees the load end; the paused transport keeps its copy.
    await waitFor(() => expect(chip()).toBeNull(), { timeout: 3000 });
    expect(store.transport().build).toEqual(loading);
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(chip()).toBeNull();
  });
});
