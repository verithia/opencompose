/**
 * MIDI import / export dialog tests — mapping, options, summary, loss list.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, saveDialogMock } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import type {
  MidiExportReport,
  MidiImportProgress,
  MidiImportResult,
  MidiImportSummary,
  MidiTrackSummary,
} from "../../stores/midi";
import { MidiImportDialog } from "../../components/MidiImportDialog";
import { MidiExportDialog } from "../../components/MidiExportDialog";

const midiMock = vi.hoisted(() => ({
  preview: vi.fn(),
  start: vi.fn(),
  report: vi.fn(),
  write: vi.fn(),
}));

vi.mock("../../stores/midi", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../stores/midi")>();
  return {
    ...actual,
    previewMidiImport: midiMock.preview,
    startMidiImport: midiMock.start,
    reportMidiExport: midiMock.report,
    writeMidiExport: midiMock.write,
  };
});

function summaryTrack(sourceTrack: number, overrides?: Partial<MidiTrackSummary>): MidiTrackSummary {
  return {
    source_track: sourceTrack,
    source_track_name: null,
    target_track: null,
    name: `Source ${sourceTrack + 1}`,
    channel: 1,
    program: 0,
    program_name: "Acoustic Grand Piano",
    preset: null,
    instrument: null,
    note_count: 8,
    lanes: [],
    first_bar: 1,
    last_bar: 4,
    ignored: [],
    dropped: false,
    ...overrides,
  };
}

function summary(tracks: MidiTrackSummary[], overrides?: Partial<MidiImportSummary>): MidiImportSummary {
  return {
    format: 1,
    source_ppq: 480,
    target_ppq: 480,
    max_tick_error: 0,
    ticks_shifted: 0,
    tempo_changes: 2,
    meter_changes: 1,
    key_changes: 0,
    sustain_expanded_notes: 0,
    overlapping_notes: 0,
    zero_length_notes: 0,
    unterminated_notes: 0,
    tracks,
    warnings: [],
    ...overrides,
  };
}

async function bootProject(tracks = 2) {
  const names = ["Piano", "Drums", "Bass"];
  const projectTracks = Array.from({ length: tracks }, (_, index) => {
    const track = makeTrack(names[index] ?? `Project ${index + 1}`, index);
    if (index === 1) track.instrument = { category: "drums", name: "acoustic_kit" };
    return track;
  });
  const snapshot = makeSnapshot({ tracks: projectTracks });
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    return snapshot;
  });
  await store.initProject();
}

beforeEach(async () => {
  i18n.setLocale("en");
  vi.clearAllMocks();
  await bootProject(2);
});

describe("MidiImportDialog", () => {
  it("renders a scrollable mapping table for a 35-track file and remaps one row", async () => {
    midiMock.preview.mockResolvedValue(summary(Array.from({ length: 35 }, (_, index) => summaryTrack(index))));
    const view = render(() => (
      <MidiImportDialog path="/tmp/album.mid" onClose={() => {}} onImported={() => {}} />
    ));

    await waitFor(() => expect(view.container.querySelectorAll(".midi-table tbody tr")).toHaveLength(35));
    // The table lives in a scroll container with a sticky header.
    expect(view.container.querySelector(".midi-table-wrap")).toBeInTheDocument();
    expect(view.container.querySelectorAll(".midi-table thead th").length).toBeGreaterThan(0);

    const firstRow = view.container.querySelector(".midi-table tbody tr") as HTMLTableRowElement;
    const select = firstRow.querySelector(".midi-target-select") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "existing:0" } });
    await waitFor(() => {
      const row = view.container.querySelector(".midi-table tbody tr") as HTMLTableRowElement;
      expect((row.querySelector(".midi-target-select") as HTMLSelectElement).value).toBe("existing:0");
      expect(row.querySelector(".midi-name-input")).toBeFalsy();
    });
  });

  it("sends an existing-track target as an append mapping", async () => {
    midiMock.preview.mockResolvedValue(summary([summaryTrack(0)]));
    midiMock.start.mockReturnValue({
      id: "job-append",
      result: Promise.resolve({
        summary: summary([summaryTrack(0, { target_track: 0 })]),
        snapshot: makeSnapshot({ tracks: [makeTrack("Piano", 0)] }),
      } satisfies MidiImportResult),
      cancel: vi.fn(async () => {}),
    });

    const view = render(() => (
      <MidiImportDialog path="/tmp/append.mid" onClose={() => {}} onImported={() => {}} />
    ));
    await waitFor(() => expect(view.container.querySelectorAll(".midi-table tbody tr")).toHaveLength(1));

    const select = view.container.querySelector(".midi-target-select") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "existing:0" } });
    await waitFor(() => expect(select.value).toBe("existing:0"));

    fireEvent.click(view.container.querySelector(".midi-import-confirm") as HTMLButtonElement);
    await waitFor(() => expect(midiMock.start).toHaveBeenCalledTimes(1));
    expect(midiMock.start).toHaveBeenCalledWith(
      "/tmp/append.mid",
      expect.objectContaining({
        mappings: [{ source_track: 0, channel: 1, target: { ExistingTrack: 0 }, name: null }],
      }),
      expect.any(Function),
    );
  });

  it("split-by-channel toggling asks for a split preview and changes the target rows", async () => {
    midiMock.preview.mockImplementation(async (_path: string, options: { split_channels: boolean }) =>
      options.split_channels
        ? summary([
            summaryTrack(0, { channel: 1, name: "Bass" }),
            summaryTrack(0, { channel: 2, name: "Bass" }),
            summaryTrack(1, { channel: 10, name: "Kit", lanes: ["sustain"] }),
          ])
        : summary([summaryTrack(0, { name: "Bass" }), summaryTrack(1, { name: "Kit" })]));

    const view = render(() => (
      <MidiImportDialog path="/tmp/split.mid" onClose={() => {}} onImported={() => {}} />
    ));
    await waitFor(() => expect(view.container.querySelectorAll(".midi-table tbody tr")).toHaveLength(2));

    fireEvent.click(view.getByLabelText("Split by channel"));
    await waitFor(() => expect(view.container.querySelectorAll(".midi-table tbody tr")).toHaveLength(3));
    expect(midiMock.preview).toHaveBeenLastCalledWith(
      "/tmp/split.mid",
      expect.objectContaining({ split_channels: true }),
    );
  });

  it("shows ignored event types, tempo/meter changes and the max tick error", async () => {
    const tracks = [
      summaryTrack(0, { ignored: [{ kind: "sysex", count: 2 }, { kind: "text", count: 1 }] }),
      summaryTrack(1, { ignored: [{ kind: "sysex", count: 1 }] }),
    ];
    midiMock.preview.mockResolvedValue(summary(tracks, {
      tempo_changes: 3,
      meter_changes: 2,
      max_tick_error: 7,
      ticks_shifted: 4,
    }));
    const view = render(() => (
      <MidiImportDialog path="/tmp/ignored.mid" onClose={() => {}} onImported={() => {}} />
    ));

    await waitFor(() => expect(view.getByText("sysex ×3")).toBeInTheDocument());
    expect(view.getByText("text ×1")).toBeInTheDocument();
    expect(view.getByText("Tempo changes").closest(".midi-summary-line")?.textContent).toContain("3");
    expect(view.getByText("Meter changes").closest(".midi-summary-line")?.textContent).toContain("2");
    expect(view.getByText("7 ticks")).toBeInTheDocument();
    expect(view.getByText("Ticks shifted").closest(".midi-summary-line")?.textContent).toContain("4");
  });

  it("disables import while a new-track name collides with an existing track", async () => {
    midiMock.preview.mockResolvedValue(summary([summaryTrack(0)]));
    const view = render(() => (
      <MidiImportDialog path="/tmp/collide.mid" onClose={() => {}} onImported={() => {}} />
    ));

    const confirm = await view.findByRole("button", { name: /Import 1 tracks?/ });
    await waitFor(() => expect(confirm).not.toBeDisabled());
    const input = view.container.querySelector(".midi-name-input") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "Piano" } });

    await waitFor(() => expect(confirm).toBeDisabled());
    expect(view.getByText(/Name collisions: 1/)).toBeInTheDocument();
  });

  it("selects all / none and imports the chosen mappings through the adapter", async () => {
    midiMock.preview.mockResolvedValue(summary([summaryTrack(0), summaryTrack(1)]));
    const imported = vi.fn();
    const snapshot = makeSnapshot({ tracks: [makeTrack("Piano", 0), makeTrack("Source 1", 1), makeTrack("Source 2", 2)] });
    midiMock.start.mockReturnValue({
      id: "job-1",
      result: Promise.resolve({ summary: summary([summaryTrack(0, { target_track: 1 })], { tracks: [summaryTrack(0, { target_track: 1 })] }), snapshot } satisfies MidiImportResult),
      cancel: vi.fn(async () => {}),
    });

    const view = render(() => (
      <MidiImportDialog path="/tmp/two.mid" onClose={() => {}} onImported={imported} />
    ));
    await waitFor(() => expect(view.container.querySelectorAll(".midi-table tbody tr")).toHaveLength(2));

    fireEvent.click(view.getByText("Select none"));
    const confirm = view.container.querySelector(".midi-import-confirm") as HTMLButtonElement;
    await waitFor(() => expect(confirm).toBeDisabled());
    fireEvent.click(view.getByText("Select all"));
    await waitFor(() => expect(confirm).not.toBeDisabled());

    fireEvent.click(confirm);
    await waitFor(() => expect(imported).toHaveBeenCalledTimes(1));
    expect(midiMock.start).toHaveBeenCalledWith(
      "/tmp/two.mid",
      expect.objectContaining({
        drop_empty: true,
        expand_sustain: true,
        mappings: expect.arrayContaining([
          expect.objectContaining({ source_track: 0, channel: 1, target: "NewTrack" }),
        ]),
      }),
      expect.any(Function),
    );
    expect(await view.findByText("1 tracks imported")).toBeInTheDocument();
  });

  it("shows fractional progress with the stage label while importing", async () => {
    midiMock.preview.mockResolvedValue(summary([summaryTrack(0)]));
    midiMock.start.mockImplementation(
      (_path: string, _options: unknown, onProgress: (progress: MidiImportProgress) => void) => {
        onProgress({ stage: "notes", fraction: 0.42 });
        return {
          id: "job-progress",
          result: new Promise(() => {}),
          cancel: vi.fn(async () => {}),
        };
      },
    );

    const view = render(() => (
      <MidiImportDialog path="/tmp/progress.mid" onClose={() => {}} onImported={() => {}} />
    ));
    const confirm = view.container.querySelector(".midi-import-confirm") as HTMLButtonElement;
    await waitFor(() => expect(confirm).not.toBeDisabled());
    fireEvent.click(confirm);

    await waitFor(() => expect(view.getByText(/Converting notes/)).toBeInTheDocument());
    expect(view.getByText(/42%/)).toBeInTheDocument();
    const fill = view.container.querySelector(".midi-progress-fill") as HTMLElement;
    expect(fill.style.width).toBe("42%");
  });

  it("shows the typed cancellation instead of a red error", async () => {
    midiMock.preview.mockResolvedValue(summary([summaryTrack(0)]));
    midiMock.start.mockReturnValue({
      id: "job-cancelled",
      result: Promise.reject({ kind: "cancelled" }),
      cancel: vi.fn(async () => {}),
    });

    const view = render(() => (
      <MidiImportDialog path="/tmp/cancel.mid" onClose={() => {}} onImported={() => {}} />
    ));
    const confirm = view.container.querySelector(".midi-import-confirm") as HTMLButtonElement;
    await waitFor(() => expect(confirm).not.toBeDisabled());
    fireEvent.click(confirm);

    expect(await view.findByText("Import cancelled. No tracks were added.")).toBeInTheDocument();
    expect(view.container.querySelector(".midi-error-text")).toBeNull();
  });
});

function exportReport(overrides?: Partial<MidiExportReport>): MidiExportReport {
  return {
    format: 1,
    ppq: 480,
    written: [
      { track: 0, name: "Piano", channel: 1, notes: 32, widened_notes: 0, automation_events: 4, program: 0, smf_track: 1 },
      { track: 1, name: "Drums", channel: 10, notes: 64, widened_notes: 2, automation_events: 0, program: null, smf_track: 2 },
    ],
    losses: [
      { kind: "articulation", count: 3, detail: "3 notes carry staccato, exported as plain notes." },
      { kind: "plugin_parameter", count: 8, detail: "8 plugin parameters have no MIDI CC." },
    ],
    ...overrides,
  };
}

describe("MidiExportDialog", () => {
  it("renders the loss list kinds and reasons from the report", async () => {
    midiMock.report.mockResolvedValue(exportReport());
    const view = render(() => <MidiExportDialog onClose={() => {}} />);

    expect(await view.findByText("Articulation & keyswitches")).toBeInTheDocument();
    expect(view.getByText("Plugin parameter")).toBeInTheDocument();
    expect(view.getByText("3 notes carry staccato, exported as plain notes.")).toBeInTheDocument();
    expect(view.getByText("8 plugin parameters have no MIDI CC.")).toBeInTheDocument();
  });

  it("defaults the drum track to channel 10 and leaves other tracks on 1..9/11..16", async () => {
    midiMock.report.mockResolvedValue(exportReport());
    const view = render(() => <MidiExportDialog onClose={() => {}} />);

    await waitFor(() => expect(midiMock.report).toHaveBeenCalled());
    const drums = view.getByLabelText("Channel Drums") as HTMLSelectElement;
    const piano = view.getByLabelText("Channel Piano") as HTMLSelectElement;
    expect(drums.value).toBe("10");
    expect(piano.value).toBe("1");
  });

  it("writes the file only after a report is available", async () => {
    midiMock.report.mockResolvedValue(exportReport());
    midiMock.write.mockResolvedValue(exportReport());
    saveDialogMock.mockResolvedValueOnce("/tmp/song.mid");
    const view = render(() => <MidiExportDialog onClose={() => {}} />);

    const confirm = await view.findByRole("button", { name: "Export…" });
    await waitFor(() => expect(confirm).not.toBeDisabled());
    fireEvent.click(confirm);

    await waitFor(() => expect(midiMock.write).toHaveBeenCalledTimes(1));
    expect(midiMock.write).toHaveBeenCalledWith(
      expect.objectContaining({ format: 1, ppq: 480 }),
      "/tmp/song.mid",
    );
    expect(await view.findByText("Written: song.mid")).toBeInTheDocument();
  });

  it("switches to a track range and rebuilds the channel map for that selection", async () => {
    midiMock.report.mockResolvedValue(exportReport());
    const view = render(() => <MidiExportDialog onClose={() => {}} />);
    await waitFor(() => expect(midiMock.report).toHaveBeenCalled());

    fireEvent.click(view.getByRole("radio", { name: "Track range" }));
    await waitFor(() => expect(midiMock.report).toHaveBeenLastCalledWith(
      expect.objectContaining({ scope: { Tracks: { start: 0, end: 2 } } }),
    ));
  });
});
