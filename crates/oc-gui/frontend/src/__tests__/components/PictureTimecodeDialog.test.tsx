import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Timeline } from "../../components/Timeline";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { PictureTimecodeDialog } from "../../components/PictureTimecodeDialog";
import { getLastCallArgs, invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";

const frame = {
  timecode: "01:00:08:01", frame_offset: 193, project_seconds: 193 / 24,
  frame_seconds: 193 / 24, offset_to_frame_ms: 0,
  quarter: 193 / 12, nearest_position: { bar: 5, beat: 1, subdivision: 0 },
  offset_to_grid_ms: 1000 / 24,
};

afterEach(cleanup);
beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "picture_timecode") return { points: [frame] };
    if (command === "transport_seek" || command === "get_transport_state") return TRANSPORT_STOPPED();
    const snapshot = makeSnapshot();
    snapshot.project.properties.tempo = 120;
    if (command === "set_picture_clock") {
      snapshot.project.properties.picture_clock = args?.clock as typeof snapshot.project.properties.picture_clock;
      snapshot.revision = Number(args?.expectedRevision) + 1;
    }
    return snapshot;
  });
  await store.initProject();
});

it("converts a frame label and seeks to its exact time between grid positions", async () => {
  const close = vi.fn();
  render(() => <PictureTimecodeDialog rate="24" origin="01:00:00:00" onClock={() => {}} onClose={close} />);
  fireEvent.input(screen.getByLabelText("Target timecode"), { target: { value: "01:00:08:01" } });
  fireEvent.click(screen.getByRole("button", { name: "Convert" }));
  await screen.findByRole("status");
  expect(getLastCallArgs("picture_timecode")).toEqual({
    request: { rate: "24", origin: "01:00:00:00", points: [{ timecode: "01:00:08:01" }] },
  });
  fireEvent.click(screen.getByRole("button", { name: "Move playhead" }));
  await waitFor(() => expect(getLastCallArgs("transport_seek")).toEqual({ beat: 193 / 12 }));
  await waitFor(() => expect(close).toHaveBeenCalledOnce());
});

it("drops a resolved report when the document is replaced", async () => {
  const close = vi.fn();
  render(() => <PictureTimecodeDialog rate="24" origin="01:00:00:00" onClock={() => {}} onClose={close} />);
  fireEvent.input(screen.getByLabelText("Target timecode"), { target: { value: "01:00:08:01" } });
  fireEvent.click(screen.getByRole("button", { name: "Convert" }));
  await screen.findByRole("status");

  await store.initProject();

  await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  expect(close).not.toHaveBeenCalled();
});

it("drops an error reply that lands after the document is replaced", async () => {
  let rejectPicture: ((error: unknown) => void) | undefined;
  invokeMock.mockImplementation((command: string) => {
    if (command === "picture_timecode") {
      return new Promise((_, reject) => { rejectPicture = reject; });
    }
    if (command === "transport_seek" || command === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot();
  });
  render(() => <PictureTimecodeDialog rate="24" origin="01:00:00:00" onClock={() => {}} onClose={() => {}} />);
  fireEvent.input(screen.getByLabelText("Target timecode"), { target: { value: "01:00:08:01" } });
  fireEvent.click(screen.getByRole("button", { name: "Convert" }));
  await waitFor(() => expect(rejectPicture).toBeTypeOf("function"));

  await store.initProject();
  rejectPicture!(new Error("late picture failure"));

  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
});

it("keeps the dialog open when the seek is refused", async () => {
  const close = vi.fn();
  invokeMock.mockImplementation((command: string) => {
    if (command === "picture_timecode") return Promise.resolve({ points: [frame] });
    if (command === "transport_seek") return Promise.reject(new Error("seek refused"));
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot();
  });
  render(() => <PictureTimecodeDialog rate="24" origin="01:00:00:00" onClock={() => {}} onClose={close} />);
  fireEvent.input(screen.getByLabelText("Target timecode"), { target: { value: "01:00:08:01" } });
  fireEvent.click(screen.getByRole("button", { name: "Convert" }));
  await screen.findByRole("status");

  fireEvent.click(screen.getByRole("button", { name: "Move playhead" }));

  await waitFor(() => expect(getLastCallArgs("transport_seek")).toEqual({ beat: 193 / 12 }));
  await waitFor(() => expect(close).not.toHaveBeenCalled());
  expect(screen.getByRole("dialog", { name: "Picture timecode" })).toBeTruthy();
});

it("reads the arrangement playhead with the configured origin", async () => {
  render(() => <PictureTimecodeDialog rate="24" origin="01:00:00:00" onClock={() => {}} onClose={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Read playhead" }));
  await screen.findByRole("status");
  expect(getLastCallArgs("picture_timecode")).toEqual({
    request: { rate: "24", origin: "01:00:00:00", points: [{ seconds: 0 }] },
  });
  expect((screen.getByLabelText("Target timecode") as HTMLInputElement).value).toBe(frame.timecode);
});

it("opens picture timecode from the timeline ruler without seeking", async () => {
  const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
  fireEvent.contextMenu(container.querySelector(".timeline-ruler")!);
  fireEvent.click(await screen.findByText("Picture timecode"));
  const dialog = await screen.findByRole("dialog", { name: "Picture timecode" });
  expect(getLastCallArgs("transport_seek")).toBeUndefined();
  fireEvent.keyDown(dialog, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Picture timecode" })).toBeNull();
});

it("saves the clock, reopens the ruler dialog with it, and clears it", async () => {
  const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
  const open = async () => {
    fireEvent.contextMenu(container.querySelector(".timeline-ruler")!);
    fireEvent.click(await screen.findByText("Picture timecode"));
    return screen.findByRole("dialog", { name: "Picture timecode" });
  };
  const dialog = await open();
  fireEvent.change(screen.getByLabelText("Picture frame rate"), { target: { value: "30000/1001" } });
  fireEvent.input(screen.getByLabelText("Timecode at project start"), { target: { value: "01:00:00;00" } });
  fireEvent.click(screen.getByRole("button", { name: "Save project clock" }));
  await screen.findByRole("button", { name: "Clear saved clock" });
  expect(getLastCallArgs("set_picture_clock")).toEqual({
    clock: { rate: "30000/1001", origin: "01:00:00;00" }, expectedRevision: 0,
  });
  fireEvent.keyDown(dialog, { key: "Escape" });
  await open();
  expect((screen.getByLabelText("Picture frame rate") as HTMLSelectElement).value).toBe("30000/1001");
  expect((screen.getByLabelText("Timecode at project start") as HTMLInputElement).value).toBe("01:00:00;00");
  fireEvent.click(screen.getByRole("button", { name: "Clear saved clock" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Clear saved clock" })).toBeNull());
  expect(getLastCallArgs("set_picture_clock")).toEqual({ clock: null, expectedRevision: 1 });
  expect(getLastCallArgs("transport_seek")).toBeUndefined();
});
