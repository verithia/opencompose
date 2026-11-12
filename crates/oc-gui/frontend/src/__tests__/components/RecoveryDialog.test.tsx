/**
 * Recovery UI: the File menu gate, the row list, restore pending/failure and
 * the discard confirmation. Backend behaviour lives in the Rust tests.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { askDialogMock, invokeMock, makeSnapshot } from "../setup";
import { i18n } from "../../stores/i18n";
import { recoveryStore } from "../../stores/recovery";
import { store } from "../../stores/project";
import { Toolbar } from "../../components/Toolbar";
import { RecoveryDialog } from "../../components/RecoveryDialog";
import type { RecoveryEntry } from "../../types/project";

const windowMock = vi.hoisted(() => ({
  setTitle: vi.fn(),
  close: vi.fn(),
  destroy: vi.fn(),
  onCloseRequested: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => windowMock }));

function entry(overrides: Partial<RecoveryEntry> = {}): RecoveryEntry {
  return {
    kind: "recovery",
    path: "/home/.opencompose/recovery/abc/0000000002000-abc.ocp",
    project_name: "Night Drive",
    saved_at_ms: 1_700_000_000_000,
    size: 4096,
    newer_than_disk: true,
    restore_disabled_reason: null,
    ...overrides,
  };
}

async function loadEntries(entries: RecoveryEntry[]) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "list_recoveries") return entries;
    return makeSnapshot();
  });
  await recoveryStore.refresh();
}

beforeEach(async () => {
  i18n.setLocale("en");
  invokeMock.mockClear();
  askDialogMock.mockReset();
  recoveryStore.close();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "list_recoveries") return [];
    return makeSnapshot();
  });
  await recoveryStore.refresh();
});

describe("recovery menu", () => {
  it("disables Recover autosave when there is no backup at all", async () => {
    await loadEntries([]);
    render(() => <Toolbar />);
    fireEvent.click(document.body.querySelector('[aria-label="More file actions"]')!);

    const item = document.body.querySelector(".toolbar-menu-item:disabled");
    expect(item).toHaveTextContent("Recover autosave…");
    expect(item).toHaveAttribute("title", "No autosave newer than the saved project.");
  });

  it("disables Recover autosave with a reason when no copy is newer", async () => {
    await loadEntries([entry({ newer_than_disk: false, restore_disabled_reason: "older" })]);
    render(() => <Toolbar />);
    fireEvent.click(document.body.querySelector('[aria-label="More file actions"]')!);

    const item = document.body.querySelector(".toolbar-menu-item:disabled");
    expect(item).toHaveTextContent("Recover autosave…");
    expect(item).toHaveAttribute("title", "No autosave newer than the saved project.");
  });

  it("enables the item for a newer copy and opens the dialog", async () => {
    await loadEntries([entry()]);
    render(() => <Toolbar />);
    fireEvent.click(document.body.querySelector('[aria-label="More file actions"]')!);

    const item = [...document.body.querySelectorAll(".toolbar-menu-item")]
      .find((node) => node.textContent?.includes("Recover autosave…"))!;
    expect(item.hasAttribute("disabled")).toBe(false);
    fireEvent.click(item);
    await waitFor(() => expect(recoveryStore.open()).toBe(true));
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "list_recoveries").length).toBeGreaterThan(0),
    );
  });

  it("enables the item for a project that keeps only a previous version", async () => {
    await loadEntries([
      entry({ kind: "backup", path: "/home/song.ocp.bak", newer_than_disk: false }),
    ]);
    render(() => <Toolbar />);
    fireEvent.click(document.body.querySelector('[aria-label="More file actions"]')!);

    const item = [...document.body.querySelectorAll(".toolbar-menu-item")]
      .find((node) => node.textContent?.includes("Recover autosave…"))!;
    expect(item.hasAttribute("disabled")).toBe(false);
    fireEvent.click(item);
    await waitFor(() => expect(recoveryStore.open()).toBe(true));
  });
});

describe("recovery dialog", () => {
  it("renders one row per copy, newest first, and explains a disabled restore", async () => {
    await loadEntries([
      entry({ path: "/r/new.ocp", project_name: "Newest", saved_at_ms: 2_000 }),
      entry({
        path: "/r/old.ocp",
        project_name: "Older",
        saved_at_ms: 1_000,
        newer_than_disk: false,
        restore_disabled_reason: "The saved project is newer than this copy.",
      }),
    ]);
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    const rows = document.body.querySelectorAll(".recovery-row");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("Newest");
    expect(rows[0].textContent).toContain("autosaved");
    expect(rows[1].textContent).toContain("Older");
    const restores = document.body.querySelectorAll(".recovery-restore");
    expect(restores[0].hasAttribute("disabled")).toBe(false);
    expect(restores[1]).toBeDisabled();
    expect(restores[1]).toHaveAttribute("title", "The saved project is newer than this copy.");
  });

  it("disables restore for a stale copy even when the backend gives no reason", async () => {
    await loadEntries([
      entry({ newer_than_disk: false, restore_disabled_reason: null }),
    ]);
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    const restore = document.body.querySelector(".recovery-restore")!;
    expect(restore).toBeDisabled();
    expect(restore).toHaveAttribute("title", "The saved project is newer than this copy.");
  });

  it("renders the previous version row and restores it", async () => {
    const previous = entry({
      kind: "backup",
      path: "/home/song.ocp.bak",
      saved_at_ms: 1_600_000_000_000,
      newer_than_disk: false,
    });
    await loadEntries([previous]);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "restore_recovery") {
        return { ...makeSnapshot(), dirty: true, file_path: "/home/song.ocp", restored_from: previous.path };
      }
      if (cmd === "list_recoveries") return [previous];
      return makeSnapshot();
    });
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    const row = document.body.querySelector(".recovery-row")!;
    expect(row).toHaveTextContent("previous version");
    expect(row).toHaveTextContent("Night Drive");
    const restore = document.body.querySelector(".recovery-restore")!;
    expect(restore).toBeEnabled();
    expect(document.body.querySelector(".recovery-discard")).toBeNull();

    fireEvent.click(restore);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("restore_recovery", { path: previous.path }),
    );
    await waitFor(() => expect(store.dirty()).toBe(true));
  });

  it("lists an unreadable previous version with its parse error", async () => {
    const reason = "The previous version cannot be read: expected project";
    await loadEntries([
      entry({ kind: "backup", path: "/home/song.ocp.bak", newer_than_disk: false, restore_disabled_reason: reason }),
    ]);
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    const restore = document.body.querySelector(".recovery-restore")!;
    expect(restore).toBeDisabled();
    expect(restore).toHaveAttribute("title", reason);
  });

  it("shows Restoring… while the copy loads and applies the snapshot", async () => {
    await loadEntries([entry()]);
    let resolveRestore: ((snapshot: unknown) => void) | undefined;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "restore_recovery") {
        return new Promise((resolve) => { resolveRestore = resolve; });
      }
      if (cmd === "list_recoveries") return [entry()];
      return makeSnapshot();
    });
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    fireEvent.click(document.body.querySelector(".recovery-restore")!);
    await waitFor(() =>
      expect(document.body.querySelector(".recovery-restore")).toHaveTextContent("Restoring…"),
    );
    expect(document.body.querySelector(".recovery-restore")).toBeDisabled();

    resolveRestore!({ ...makeSnapshot(), dirty: true, file_path: "/home/song.ocp" });
    await waitFor(() => expect(store.dirty()).toBe(true));
    expect(invokeMock).toHaveBeenCalledWith("restore_recovery", {
      path: "/home/.opencompose/recovery/abc/0000000002000-abc.ocp",
    });
  });

  it("shows the audit failure sentence and keeps the backup listed", async () => {
    const failure =
      "Could not restore 0000000002000-abc.ocp: boom. The backup is kept.";
    await loadEntries([entry()]);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "restore_recovery") throw new Error(failure);
      if (cmd === "list_recoveries") return [entry()];
      return makeSnapshot();
    });
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    fireEvent.click(document.body.querySelector(".recovery-restore")!);
    await waitFor(() => expect(document.body.querySelector("[role=alert]")).toHaveTextContent(failure));
    expect(document.body.querySelectorAll(".recovery-row")).toHaveLength(1);
    expect(document.body.querySelector(".recovery-restore")).toBeEnabled();
  });

  it("asks before discarding one copy and deletes only that copy", async () => {
    const copy = entry();
    await loadEntries([copy]);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_recoveries") return [];
      return undefined;
    });
    askDialogMock.mockResolvedValueOnce(true);
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    fireEvent.click(document.body.querySelector(".recovery-discard")!);
    await waitFor(() => expect(askDialogMock).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("discard_recovery", { path: copy.path }),
    );
  });

  it("keeps the copy when the discard confirmation is declined", async () => {
    await loadEntries([entry()]);
    askDialogMock.mockResolvedValueOnce(false);
    render(() => (
      <RecoveryDialog prompt={false} onClose={() => {}} />
    ));

    fireEvent.click(document.body.querySelector(".recovery-discard")!);
    await waitFor(() => expect(askDialogMock).toHaveBeenCalledTimes(1));
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "discard_recovery")).toHaveLength(0);
  });

  it("prompts at startup only for a copy newer than the saved file", async () => {
    await loadEntries([
      entry({ newer_than_disk: false, restore_disabled_reason: "older" }),
    ]);
    await recoveryStore.checkOnStartup();
    expect(recoveryStore.promptOpen()).toBe(false);

    await loadEntries([entry()]);
    await recoveryStore.checkOnStartup();
    expect(recoveryStore.promptOpen()).toBe(true);
    recoveryStore.close();
  });

  it("prompts at startup with the newest copy", async () => {
    await loadEntries([entry()]);
    render(() => (
      <RecoveryDialog prompt onClose={() => {}} />
    ));
    expect(document.body.querySelector(".recovery-title")).toHaveTextContent("Recover unsaved work?");
    expect(document.body.querySelector(".recovery-body")).toHaveTextContent("Night Drive");
    expect(document.body.querySelector(".recovery-row")).toHaveTextContent("Night Drive");
  });
});
