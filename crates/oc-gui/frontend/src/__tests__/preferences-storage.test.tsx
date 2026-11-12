/**
 * Storage page: SoundFont count and folder, plugin counts by format, the sample
 * cache with its clear action, the audition archive with its purge, and the
 * preferences file path.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { askDialogMock, expectInvoked, invokeMock } from "./setup";
import { StoragePage } from "../components/preferences/StoragePage";
import { storageStore } from "../stores/storage";
import { preferencesStore } from "../stores/preferencesBundle";
import { i18n } from "../stores/i18n";
import type { StorageReport } from "../stores/api";

function report(overrides: Partial<StorageReport> = {}): StorageReport {
  return {
    soundfonts: {
      dir: "/Users/me/.opencompose/soundfonts",
      installed: 3,
      files: 4,
      bytes: 4_000_000,
    },
    plugins: {
      by_format: [
        { format: "CLAP", count: 2, bytes: 60_000_000 },
        { format: "VST3", count: 5, bytes: 120_000_000 },
      ],
      folders: [{ path: "/Library/Audio/Plug-Ins/VST3", count: 5, bytes: 120_000_000 }],
      bytes: 180_000_000,
    },
    sessions: {
      root: "/Users/me/.opencompose/ai/sessions",
      workspaces: [
        {
          path: "/Users/me/songs",
          key: "abc123",
          sessions: [
            { id: "s1", title: "intro idea", bytes: 5_000_000, updated: "2026-10-28T09:00:00Z" },
            { id: "s2", title: "punchier kick", bytes: 3_000_000, updated: "2026-10-29T09:00:00Z" },
          ],
          bytes: 8_000_000,
        },
      ],
      audio_bytes: 3_500_000,
      auditions: {
        media: {
          dir: "/Users/me/.opencompose/ai/sessions/audition-media",
          files: 2,
          bytes: 2_000_000,
        },
        renders: {
          dir: "/Users/me/.opencompose/ai/sessions/audition-renders",
          files: 1,
          bytes: 1_500_000,
        },
        budget_bytes: 256 * 1024 * 1024,
      },
      total_bytes: 9_000_000,
    },
    samples: {
      dir: "/Users/me/song/media/samples",
      files: 4,
      bytes: 3 * 1024 * 1024,
    },
    preferences: "/Users/me/.opencompose/gui-settings.yaml",
    ...overrides,
  };
}

let cleared = false;
let clearedAuditions = false;

function mockStorage(initial: StorageReport = report()) {
  cleared = false;
  clearedAuditions = false;
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "storage_report") return initial;
    if (cmd === "clear_sample_cache") {
      cleared = true;
      return {
        ...initial,
        samples: { ...initial.samples, files: 0, bytes: 0 },
      };
    }
    if (cmd === "clear_audition_archive") {
      clearedAuditions = true;
      return {
        ...initial,
        sessions: {
          ...initial.sessions,
          audio_bytes: 0,
          auditions: {
            ...initial.sessions.auditions,
            media: { ...initial.sessions.auditions.media, files: 0, bytes: 0 },
            renders: { ...initial.sessions.auditions.renders, files: 0, bytes: 0 },
          },
        },
      };
    }
    if (cmd === "open_storage_folder") return undefined;
    return undefined;
  });
}

async function renderPage() {
  const rendered = render(() => <StoragePage onClose={() => {}} />);
  await vi.waitFor(() =>
    expect(rendered.container.querySelector(".preferences-soundfont-count")).toBeTruthy(),
  );
  return rendered;
}

beforeEach(() => {
  i18n.setLocale("en");
  storageStore.reset();
  preferencesStore.reset();
  invokeMock.mockReset();
  askDialogMock.mockReset();
  mockStorage();
});

afterEach(() => cleanup());

describe("the Storage page", () => {
  it("reports the SoundFont count and its folder", async () => {
    const { container } = await renderPage();

    expect(container.querySelector(".preferences-soundfont-count")?.textContent).toBe(
      i18n.t("preferences.storage.installedCount", { count: 3 }),
    );
    expect(container.querySelector(".preferences-soundfonts-path")?.textContent).toContain(
      "/Users/me/.opencompose/soundfonts",
    );
  });

  it("reports plugin counts by format and the searched folders", async () => {
    const { container } = await renderPage();

    const formats = [...container.querySelectorAll(".preferences-plugin-format")].map(
      (node) => node.textContent,
    );
    // Each chip now carries its own size as well as its count.
    expect(formats[0]?.startsWith(i18n.t("preferences.storage.formatCount", { count: 2, format: "CLAP" }))).toBe(true);
    expect(formats[1]?.startsWith(i18n.t("preferences.storage.formatCount", { count: 5, format: "VST3" }))).toBe(true);
    expect(container.textContent).toContain("/Library/Audio/Plug-Ins/VST3");
  });

  it("shows cache disk usage without double-counting open files and rescans or clears unused files", async () => {
    const mib = 1024 * 1024;
    const pcm: NonNullable<StorageReport["clip_pcm"]> = {
      decoded: {
        dir: "/cache/decoded",
        cached_bytes: 20 * mib,
        open_cached_bytes: 5 * mib,
        live_temporary_bytes: 3 * mib,
        temporary_bytes: 5 * mib,
        budget_bytes: 100 * mib,
      },
      processed: {
        dir: "/cache/processed",
        cached_bytes: 7 * mib,
        open_cached_bytes: 0,
        live_temporary_bytes: 0,
        budget_bytes: 80 * mib,
      },
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "storage_report") return report({ clip_pcm: pcm });
      if (command === "storage_scan") {
        return { ...pcm, decoded: { ...pcm.decoded, live_temporary_bytes: 0, temporary_bytes: 0 } };
      }
      if (command === "clear_clip_pcm_cache") {
        return {
          decoded: { ...pcm.decoded, cached_bytes: 5 * mib, live_temporary_bytes: 0, temporary_bytes: 0 },
          processed: { ...pcm.processed, cached_bytes: 0 },
        };
      }
      return undefined;
    });
    const { container } = await renderPage();
    const block = container.querySelector(".preferences-clip-cache")!;
    expect(block.querySelector(".preferences-size")?.textContent).toBe("32.0 MB");
    expect(block.textContent).toContain(i18n.t("preferences.storage.clipCacheUsage", {
      cached: "20.0 MB", open: "5.0 MB", temporary: "5.0 MB", playing: "3.0 MB", budget: "100.0 MB",
    }));
    fireEvent.click(block.querySelector(".preferences-scan")!);
    await vi.waitFor(() => {
      expectInvoked("storage_scan", { section: "clip_pcm" });
      expect(block.querySelector(".preferences-size")?.textContent).toBe("27.0 MB");
    });
    const clear = block.querySelector(".preferences-clear-clip-cache")!;
    askDialogMock.mockResolvedValueOnce(false);
    fireEvent.click(clear);
    await vi.waitFor(() => expect(askDialogMock).toHaveBeenCalledTimes(1));
    expect(invokeMock.mock.calls.some(([command]) => command === "clear_clip_pcm_cache")).toBe(false);
    askDialogMock.mockResolvedValueOnce(true);
    fireEvent.click(clear);
    await vi.waitFor(() => {
      expectInvoked("clear_clip_pcm_cache");
      expect(block.querySelector(".preferences-size")?.textContent).toBe("5.0 MB");
    });
  });

  it("shows the sample cache size, clears it, and opens the folder", async () => {
    askDialogMock.mockResolvedValue(true);
    const { container } = await renderPage();

    expect(container.querySelector(".preferences-sample-count")?.textContent).toBe(
      i18n.t("preferences.storage.sampleUsage", { files: 4, size: "3.0 MB" }),
    );

    fireEvent.click(container.querySelector(".preferences-open-samples")!);
    await vi.waitFor(() =>
      expectInvoked("open_storage_folder", { path: "/Users/me/song/media/samples" }),
    );

    fireEvent.click(container.querySelector(".preferences-clear-samples")!);
    await vi.waitFor(() => expect(cleared).toBe(true));
    await vi.waitFor(() =>
      expect(container.querySelector(".preferences-sample-count")?.textContent).toBe(
        i18n.t("preferences.storage.sampleUsage", { files: 0, size: "0.0 MB" }),
      ),
    );
    expect(container.querySelector(".preferences-cleared")?.textContent).toBe(
      i18n.t("preferences.storage.cleared"),
    );
  });

  it("leaves the sample cache alone when the operator cancels", async () => {
    askDialogMock.mockResolvedValue(false);
    const { container } = await renderPage();

    fireEvent.click(container.querySelector(".preferences-clear-samples")!);
    await vi.waitFor(() => expect(askDialogMock).toHaveBeenCalled());
    expect(askDialogMock.mock.calls[0][0]).toBe(
      i18n.t("preferences.storage.clearSamplesConfirm"),
    );
    expect(cleared).toBe(false);
    expect(
      invokeMock.mock.calls.some(([command]) => command === "clear_sample_cache"),
    ).toBe(false);
  });

  it("states that the sample cache needs a saved project", async () => {
    storageStore.reset();
    invokeMock.mockReset();
    mockStorage(report({ samples: { dir: null, files: 0, bytes: 0 } }));
    const { container } = await renderPage();

    expect(container.querySelector(".preferences-sample-count")?.textContent).toBe(
      i18n.t("preferences.storage.noProject"),
    );
    expect(container.querySelector(".preferences-clear-samples")).toBeNull();
  });

  it("shows both audition directories with their budget and purges them after a confirmation", async () => {
    askDialogMock.mockResolvedValue(true);
    const { container } = await renderPage();

    const media = container.querySelector(".preferences-audition-media");
    const renders = container.querySelector(".preferences-audition-renders");
    expect(media?.textContent).toContain("/ai/sessions/audition-media");
    expect(renders?.textContent).toContain("/ai/sessions/audition-renders");
    expect(media?.textContent).toContain(
      i18n.t("preferences.storage.auditionUsage", {
        files: 2,
        size: "1.9 MB",
        budget: "256.0 MB",
      }),
    );
    expect(renders?.textContent).toContain(
      i18n.t("preferences.storage.auditionUsage", {
        files: 1,
        size: "1.4 MB",
        budget: "256.0 MB",
      }),
    );

    fireEvent.click(container.querySelector(".preferences-clear-auditions")!);
    await vi.waitFor(() => expect(askDialogMock).toHaveBeenCalled());
    await vi.waitFor(() => expect(clearedAuditions).toBe(true));
    await vi.waitFor(() =>
      expect(container.querySelector(".preferences-audition-media")?.textContent).toContain(
        i18n.t("preferences.storage.auditionUsage", {
          files: 0,
          size: "0.0 MB",
          budget: "256.0 MB",
        }),
      ),
    );
    expect(container.querySelector(".preferences-auditions-cleared")?.textContent).toBe(
      i18n.t("preferences.storage.auditionsCleared"),
    );
  });

  it("shows the preferences file path", async () => {
    const { container } = await renderPage();

    expect(container.textContent).toContain("/Users/me/.opencompose/gui-settings.yaml");
    expect(container.querySelector(".preferences-export-btn")).toBeTruthy();
    expect(container.querySelector(".preferences-import-btn")).toBeTruthy();
  });
});
