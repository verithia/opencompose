/**
 * Undo history panel — names, scopes, jump-to-level and redo truncation.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type { UndoHistoryEntry } from "../../stores/api";
import { expectInvoked, invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { UndoHistoryPanel } from "../../components/UndoHistoryPanel";

const UNDO_STATE = {
  can_undo: true,
  can_redo: true,
  undo_label: "clip move",
  redo_label: null,
  undo_labels: ["clip move"],
  redo_labels: [],
};

function entry(
  index: number,
  name: string,
  scopes: UndoHistoryEntry["scopes"],
  applied = true,
): UndoHistoryEntry {
  return { index, name, scopes, timestamp: 1_700_000_000_000 + index * 1000, applied };
}

let history: UndoHistoryEntry[];

beforeEach(async () => {
  invokeMock.mockClear();
  history = [
    entry(1, "tempo", ["transport"]),
    entry(2, "mixer levels", ["mixer_levels"]),
    entry(3, "clip move", ["project"], false),
  ];
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return UNDO_STATE;
    if (cmd === "get_undo_history") return history;
    if (cmd === "restore_undo_level") {
      expect((args as { level?: number }).level).toBeTypeOf("number");
      return makeSnapshot();
    }
    return makeSnapshot();
  });
  await store.initProject();
});

describe("UndoHistoryPanel", () => {
  it("renders every level with its caller name and scope tags", async () => {
    const { container, getByText } = render(() => <UndoHistoryPanel open={true} onClose={() => {}} />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(3));
    expect(getByText("History")).toBeTruthy();
    expect(container.querySelectorAll(".undo-history-name")[2]?.textContent).toBe("tempo");
    expect([...container.querySelectorAll(".undo-history-name")].map((node) => node.textContent))
      .toEqual(["clip move", "mixer levels", "tempo"]);
    expect(container.querySelector('[data-scope="mixer_levels"]')?.textContent).toBe("mixer levels");
    expect(container.querySelector('[data-scope="transport"]')?.textContent).toBe("transport");
    // Level 3 is undone, so it is offered as a redo target.
    expect(container.querySelectorAll(".undo-history-entry.redo").length).toBe(1);
  });

  it("jumps to a level through the backend command", async () => {
    const { container, getByLabelText } = render(() => <UndoHistoryPanel open={true} onClose={() => {}} />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(3));
    fireEvent.click(getByLabelText("Restore level 2"));
    expectInvoked("restore_undo_level", { level: 2 });
    await waitFor(() => expect(store.undoHistory().length).toBe(3));
  });

  it("drops the redo levels after a new edit", async () => {
    const { container } = render(() => <UndoHistoryPanel open={true} onClose={() => {}} />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(3));

    // A new edit clears redo in the backend; the panel follows the next refresh.
    history = [
      entry(1, "tempo", ["transport"]),
      entry(2, "clip move", ["project"]),
    ];
    await store.setTempo(140);

    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(2));
    expect(container.querySelectorAll(".undo-history-entry.redo").length).toBe(0);
    expect(container.querySelector('[data-level="3"]')).toBeNull();
    expectInvoked("set_tempo");
  });
});
