import { render, fireEvent, screen, cleanup, waitFor } from "@solidjs/testing-library";
import { afterEach, expect, it, vi } from "vitest";
import { PerformanceEditor } from "../components/PerformanceEditor";
import type { Track } from "../types/project";

afterEach(cleanup);
const track = (count = 2) => ({ name: "Bass performance", performance: { events: Array.from({ length: count }, (_, i) => ({
  quarter: i / 4, channel: 2, message: { type: "note_off", note: 60, velocity: 37 },
})) } }) as Track;

it("shows and edits release velocity without altering timing or command order", async () => {
  const onEdit = vi.fn().mockResolvedValue(undefined);
  render(() => <PerformanceEditor track={track()} trackIndex={3} onEdit={onEdit} />);
  const input = screen.getByLabelText("Event 2 velocity");
  fireEvent.focus(input);
  fireEvent.change(input, { target: { value: "73" } });
  await waitFor(() => expect(onEdit).toHaveBeenCalledTimes(1));
  expect(onEdit.mock.calls[0]).toEqual([3, "Bass performance", 1,
    { quarter: 0.25, channel: 2, message: { type: "note_off", note: 60, velocity: 37 } },
    { quarter: 0.25, channel: 2, message: { type: "note_off", note: 60, velocity: 73 } }]);
});

it("rejects nonnumeric input and exposes backend rejection", async () => {
  const onEdit = vi.fn().mockRejectedValue("event order invalid");
  render(() => <PerformanceEditor track={track()} trackIndex={0} onEdit={onEdit} />);
  fireEvent.change(screen.getByLabelText("Event 1 channel"), { target: { value: "2abc" } });
  expect(onEdit).not.toHaveBeenCalled();
  expect(screen.getByRole("alert")).toHaveTextContent("finite number");
  fireEvent.change(screen.getByLabelText("Event 2 quarter"), { target: { value: "-1" } });
  await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("event order invalid"));
});

it("paginates dense timelines without changing absolute event indices", async () => {
  const onEdit = vi.fn().mockResolvedValue(undefined);
  render(() => <PerformanceEditor track={track(130)} trackIndex={0} onEdit={onEdit} />);
  expect(screen.queryByLabelText("Event 65 quarter")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
  fireEvent.change(screen.getByLabelText("Event 65 channel"), { target: { value: "3" } });
  await waitFor(() => expect(onEdit).toHaveBeenCalled());
  expect(onEdit.mock.calls[0][2]).toBe(64);
});
