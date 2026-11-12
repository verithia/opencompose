/**
 * BusActions — node-level delete with the explicit re-route confirmation.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { TRANSPORT_STOPPED, expectInvoked, invokeMock, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { BusActions } from "../../components/BusActions";

const UNDO_NONE = {
  can_undo: false, can_redo: false,
  undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
};

function snapshot() {
  return makeSnapshot({
    tracks: [{
      ...makeTrack("Keys", 0),
      mix: {
        volume_db: 0, pan: 0, mute: false, solo: false,
        sends: [{ target: "space", amount: 0.5 }],
      },
    }],
    buses: [
      { name: "space", routes: ["master"], effects: [] },
      { name: "room", routes: ["master"], effects: [] },
    ],
  });
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return UNDO_NONE;
    return snapshot();
  });
  await store.initProject();
});

function renderActions(busName: string) {
  const deleted: string[] = [];
  const view = render(() => (
    <BusActions busName={busName} onDeleted={(focus) => deleted.push(focus)} />
  ));
  return { ...view, deleted };
}

describe("BusActions", () => {
  it("deletes an unused bus directly and reports the focus target", async () => {
    const { getByLabelText, deleted } = renderActions("room");
    fireEvent.click(getByLabelText("Delete selected bus"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("remove_bus", { name: "room", reroute: false });
    await expect.poll(() => deleted).toEqual(["master"]);
  });

  it("asks before deleting a bus that is still referenced", async () => {
    const { getByLabelText, getByRole, queryByRole } = renderActions("space");
    fireEvent.click(getByLabelText("Delete selected bus"));

    const dialog = getByRole("alertdialog");
    expect(dialog.textContent).toContain("1 send");
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_bus")).toBe(false);

    fireEvent.click(getByLabelText("Cancel bus delete"));
    expect(queryByRole("alertdialog")).toBeNull();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_bus")).toBe(false);
  });

  it("re-routes and deletes only after the explicit confirmation", async () => {
    const { getByLabelText } = renderActions("space");
    fireEvent.click(getByLabelText("Delete selected bus"));
    fireEvent.click(getByLabelText("Re-route and delete bus"));
    await expect.poll(() => invokeMock.mock.calls.length).toBeGreaterThan(0);
    expectInvoked("remove_bus", { name: "space", reroute: true });
  });

  it("closes the dialog on Escape and returns focus to the delete button", async () => {
    const { getByLabelText, getByRole, queryByRole } = renderActions("space");
    const trigger = getByLabelText("Delete selected bus");
    fireEvent.click(trigger);
    fireEvent.keyDown(getByRole("alertdialog"), { key: "Escape" });
    expect(queryByRole("alertdialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("never uses the track header icon button inside the delete dialog", () => {
    const { getByLabelText, container } = renderActions("space");
    fireEvent.click(getByLabelText("Delete selected bus"));
    expect(container.querySelector(".track-btn")).toBeNull();
    expect(container.querySelectorAll(".routing-btn").length).toBeGreaterThanOrEqual(3);
  });
});
