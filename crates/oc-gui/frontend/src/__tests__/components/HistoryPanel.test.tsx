/**
 * History tab: names, authors and scopes, jumps, paging older entries, the
 * space the history occupies and clearing older entries.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import type { HistoryPage, UndoHistoryEntry } from "../../stores/api";
import type { Change, ChangeSet } from "../../types/change";
import { expectInvoked, invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { HistoryPanel, formatTimestamp } from "../../components/HistoryPanel";
import { i18n } from "../../stores/i18n";
import { formatSavedAt } from "../../stores/recovery";
import { updatedLabel } from "../../ai/sessions";
import { rememberSessionTitles } from "../../stores/sessionTitles";

const dialog = vi.hoisted(() => ({ ask: vi.fn(async () => true) }));
vi.mock("@tauri-apps/plugin-dialog", () => dialog);

const UNDO_STATE = { can_undo: true, can_redo: true };

function set(...changes: Change[]): ChangeSet {
  return { changes, more: 0 };
}

function entry(
  index: number,
  changes: ChangeSet,
  scopes: UndoHistoryEntry["scopes"],
  applied = true,
  extra: Partial<UndoHistoryEntry> = {},
): UndoHistoryEntry {
  return {
    index, changes, cause: "edit", scopes, timestamp: 1_700_000_000_000 + index * 1000, applied,
    author: { kind: "person" }, ...extra,
  };
}

let history: UndoHistoryEntry[];
let bytes: number;

function page(before?: number | null): HistoryPage {
  const end = before ?? history.length;
  const start = Math.max(0, end - 64);
  return {
    entries: history.slice(start, end).reverse(),
    total: history.length,
    applied: history.filter((item) => item.applied).length,
    journaled: true,
  };
}

beforeEach(async () => {
  invokeMock.mockClear();
  dialog.ask.mockClear();
  bytes = 2048;
  history = [
    entry(1, set({ kind: "tempo", from: 120, to: 128 }), ["transport"]),
    entry(2, set({ kind: "track_added", track: "drums" }), ["project"], true, {
      author: { kind: "agent", session: "s1" },
    }),
    entry(3, set({ kind: "clips_edited", track: "lead", span: null }), ["project"], false),
  ];
  rememberSessionTitles([{ id: "s1", title: "Drum ideas" }]);
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as { before?: number | null; level?: number; keep?: number };
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return UNDO_STATE;
    if (cmd === "history_page") return page(typed.before);
    if (cmd === "history_info") {
      return { total: history.length, applied: 2, journaled: true, bytes, path: "/config/history/0123456789abcdef" };
    }
    if (cmd === "history_jump") return { status: "applied", snapshot: makeSnapshot() };
    if (cmd === "history_clear_older") {
      const keep = typed.keep ?? 0;
      const dropped = history.length - keep;
      history = history.slice(dropped).map((item, at) => ({ ...item, index: at + 1 }));
      bytes = 1024;
      return { status: "cleared", dropped, freed: 1024, info: { total: history.length, applied: 0, journaled: true, bytes } };
    }
    return makeSnapshot();
  });
  await store.initProject();
});

describe("HistoryPanel", () => {
  it("names each entry with its typed changes and author, newest first", async () => {
    const { container } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry[data-level]").length).toBe(4));
    expect([...container.querySelectorAll(".undo-history-name")].map((node) => node.textContent)).toEqual([
      "Edit clips on “lead” (You)",
      "Add track “drums” (Agent - Drum ideas)",
      "Change tempo from 120 to 128 BPM (You)",
      "Start of history",
    ]);
    expect(container.querySelector('[data-scope="transport"]')?.textContent).toBe("transport");
    expect(container.querySelectorAll(".undo-history-entry.redo").length).toBe(1);
    expect(container.querySelector(".undo-history-entry.current")?.getAttribute("data-level")).toBe("2");
    await waitFor(() => expect(container.querySelector(".history-size")?.textContent).toBe("2.0 KB on disk"));
  });

  it("names a span in bars", async () => {
    history = [entry(
      1,
      set({ kind: "clips_edited", track: "lead", span: { start: 0, end: 16 } }),
      ["project"],
      true,
    )];
    const { container } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry[data-level]").length).toBe(2));
    expect(container.querySelector(".undo-history-name")?.textContent)
      .toBe("Edit clips on “lead” in bars 1–4 (You)");
  });

  it("jumps to a level through the backend command", async () => {
    const { container, getByLabelText } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(4));
    fireEvent.click(getByLabelText("Restore level 2"));
    expectInvoked("history_jump", { level: 2 });
  });

  it("loads older entries a page at a time", async () => {
    history = Array.from({ length: 70 }, (_, at) => entry(
      at + 1, set({ kind: "track_changed", track: `edit ${at + 1}` }), ["project"],
    ));
    const { container, getByText } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(64));
    expect(container.querySelector('[data-level="0"]')).toBeNull();
    fireEvent.click(getByText("Load older entries"));
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(71));
    expectInvoked("history_page", { before: 6, limit: null });
    expect(container.querySelector('[data-level="1"]')).not.toBeNull();
  });

  it("follows a new edit that dropped the redo levels", async () => {
    const { container } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry.redo").length).toBe(1));
    history = [
      entry(1, set({ kind: "tempo", from: 120, to: 140 }), ["transport"]),
      entry(2, set({ kind: "tempo", from: 140, to: 150 }), ["transport"]),
    ];
    await store.setTempo(140);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry.redo").length).toBe(0));
    expect(container.querySelector('[data-level="3"]')).toBeNull();
  });

  it("clears older entries after confirmation and reports the space freed", async () => {
    const { container, getByLabelText, getByText } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(4));
    fireEvent.input(getByLabelText("Keep newest"), { target: { value: "1" } });
    fireEvent.click(getByText("Clear older history"));
    await waitFor(() => expectInvoked("history_clear_older", { keep: 1 }));
    expect(dialog.ask).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(container.querySelector(".history-report")?.textContent)
      .toBe("Dropped 2 entries, freed 1.0 KB"));
  });

  it("asks before clearing and clears nothing when the person says no", async () => {
    dialog.ask.mockResolvedValueOnce(false);
    const { container, getByLabelText, getByText } = render(() => <HistoryPanel />);
    await waitFor(() => expect(container.querySelectorAll(".undo-history-entry").length).toBe(4));
    fireEvent.input(getByLabelText("Keep newest"), { target: { value: "1" } });
    fireEvent.click(getByText("Clear older history"));
    await waitFor(() => expect(dialog.ask).toHaveBeenCalledTimes(1));
    expect(invokeMock.mock.calls.some(([name]) => name === "history_clear_older")).toBe(false);
  });
});

describe("times in the app's language", () => {
  const at = Date.UTC(2026, 10, 16, 9, 30);
  it("follows the selected locale rather than the system one", () => {
    try {
      i18n.setLocale("zh-Hans");
      expect(formatTimestamp(at)).toBe(new Date(at).toLocaleString("zh-Hans", {
        month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
      }));
      expect(formatSavedAt(at)).toBe(new Date(at).toLocaleString("zh-Hans"));
      expect(updatedLabel(new Date(at).toISOString())).toBe(new Date(at).toLocaleString("zh-Hans", {
        year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit",
      }));
      expect(formatTimestamp(at)).not.toBe(new Date(at).toLocaleString("en", {
        month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
      }));
    } finally {
      i18n.setLocale("en");
    }
  });
});
