import { afterEach, beforeEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { SetupSoundCheck } from "../../components/SetupSoundCheck";
import { store } from "../../stores/project";
import { preferencesUi } from "../../stores/preferencesUi";
import { invokeMock, makeSnapshot, makeAuditionTransport, AUDIO_IO_EMPTY, TRANSPORT_STOPPED } from "../setup";

let currentAudition: ReturnType<typeof makeAuditionTransport> | null;
const unexpectedCommands: string[] = [];
beforeEach(async () => {
  currentAudition = null;
  unexpectedCommands.length = 0;
  invokeMock.mockImplementation(async (command, args) => {
    switch (command) {
      case "get_project": return makeSnapshot();
      case "get_transport_state": return currentAudition ?? TRANSPORT_STOPPED();
      case "get_audio_io": return AUDIO_IO_EMPTY;
      case "set_loop_region": {
        const loop = args as { startBeats: number | null; endBeats: number | null; enabled: boolean };
        return { start_beats: loop.startBeats, end_beats: loop.endBeats, enabled: loop.enabled };
      }
      case "plugin_catalog": return { platform: "macos-aarch64", entries: [], installed: [] };
      case "plugin_index_status": return null;
      case "get_undo_state": return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      case "audition_factory_patch": {
        currentAudition = makeAuditionTransport((args as { label: string }).label, true);
        return undefined;
      }
      case "resume_audition": {
        currentAudition = makeAuditionTransport(currentAudition?.audition?.preset ?? "", false);
        return currentAudition;
      }
      case "pause_audition": {
        currentAudition = makeAuditionTransport(currentAudition?.audition?.preset ?? "", true);
        return currentAudition;
      }
      case "stop_audition": currentAudition = null; return undefined;
      case "transport_stop": currentAudition = null; return TRANSPORT_STOPPED();
      default: unexpectedCommands.push(command); throw new Error(`Unexpected setup IPC command: ${command}`);
    }
  });
  await store.initProject();
  await store.stop();
  invokeMock.mockClear();
});
afterEach(async () => { cleanup(); await store.stop(); preferencesUi.close(); expect(unexpectedCommands).toEqual([]); });
it("plays a built-in sound only on request and leaves the project intact", async () => {
  const source = store.dslSource();
  render(() => <SetupSoundCheck />);
  expect(invokeMock.mock.calls.some(([command]) => command === "audition_factory_patch")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Play test sound" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("audition_factory_patch", expect.objectContaining({
    params: expect.arrayContaining([["osc.a.level", "0.12"], ["osc.a.waveform", "0"]]),
  })));
  expect(store.dslSource()).toBe(source);
  fireEvent.click(screen.getByRole("button", { name: "Stop test sound" }));
  await waitFor(() => expect(store.auditionActive()).toBe(false));
});
it("can stop while loading and retry after failure", async () => {
  let reject!: (reason: Error) => void;
  const original = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation((command, ...args) => command === "audition_factory_patch"
    ? new Promise((_, fail) => { reject = fail; }) : original(command, ...args));
  render(() => <SetupSoundCheck />);
  fireEvent.click(screen.getByRole("button", { name: "Play test sound" }));
  await waitFor(() => expect(reject).toBeTypeOf("function"));
  fireEvent.click(screen.getByRole("button", { name: "Stop test sound" }));
  await waitFor(() => expect(store.auditionActive()).toBe(false));
  reject(new Error("Obsolete render"));
  await Promise.resolve();
  fireEvent.click(screen.getByRole("button", { name: "Play test sound" }));
  await waitFor(() => expect(store.auditionActive()).toBe(true));
  reject(new Error("Output unavailable"));
  await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Output unavailable"));
  expect(screen.getByRole("button", { name: "Play test sound" })).toBeEnabled();
});
it("opens output settings and does not stop an unrelated audition when leaving", async () => {
  render(() => <SetupSoundCheck />);
  fireEvent.click(screen.getByRole("button", { name: "Audio settings" }));
  expect(preferencesUi.page()).toBe("audio");
  await store.auditionFactoryPatch("Other sound", []);
  invokeMock.mockClear();
  cleanup();
  expect(invokeMock.mock.calls.some(([command]) => command === "stop_audition")).toBe(false);
});

it("stops its own sound when leaving setup", async () => {
  render(() => <SetupSoundCheck />);
  fireEvent.click(screen.getByRole("button", { name: "Play test sound" }));
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "audition_factory_patch")).toBe(true));
  invokeMock.mockClear();
  cleanup();
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "stop_audition")).toBe(true));
});
