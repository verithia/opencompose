/**
 * MIDI import adapter — progress polling and the typed cancellation outcome.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock } from "../setup";
import {
  DEFAULT_IMPORT_OPTIONS,
  isMidiImportCancelled,
  midiImportErrorMessage,
  startMidiImport,
  type MidiImportProgress,
} from "../../stores/midi";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

describe("startMidiImport", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("polls backend progress and settles with the import result", async () => {
    vi.useFakeTimers();
    const apply = deferred<unknown>();
    const progress = vi.fn();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "midi_import_progress") {
        return Promise.resolve({ stage: "notes", fraction: 0.4 } satisfies MidiImportProgress);
      }
      if (cmd === "midi_import_apply") return apply.promise;
      return Promise.resolve(false);
    });

    const job = startMidiImport("/tmp/large.mid", DEFAULT_IMPORT_OPTIONS, progress);
    await vi.advanceTimersByTimeAsync(120);
    expect(progress).toHaveBeenCalledWith({ stage: "notes", fraction: 0.4 });

    apply.resolve({ summary: { tracks: [] }, snapshot: {} });
    await expect(job.result).resolves.toEqual({ summary: { tracks: [] }, snapshot: {} });
    await vi.advanceTimersByTimeAsync(500);
    expect(progress).toHaveBeenCalledTimes(1);
  });

  it("exposes the typed cancellation outcome", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "midi_import_apply") return Promise.reject({ kind: "cancelled" });
      return Promise.resolve(null);
    });

    const job = startMidiImport("/tmp/large.mid", DEFAULT_IMPORT_OPTIONS, () => {});
    const error = await job.result.catch((reason) => reason);
    expect(isMidiImportCancelled(error)).toBe(true);
    expect(midiImportErrorMessage(error)).toBe("MIDI import cancelled");
    expect(midiImportErrorMessage({ kind: "failed", message: "bad file" })).toBe("bad file");
  });
});
