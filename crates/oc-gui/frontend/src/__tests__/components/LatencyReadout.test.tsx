/**
 * LatencyReadout — the three-part PDC readout with the 0/128/512 graph.
 * These numbers are the engine test's
 * `latency_readout_matches_the_plan_and_the_diagnostic`: a(0) and b(128) feed
 * bus X, a also goes to master, c(512) goes to master, so the master reference
 * is 512 and a's route into X carries the readable diagnostic.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack } from "../setup";
import { LatencyReadout } from "../../components/LatencyReadout";
import { Mixer } from "../../components/Mixer";
import { store } from "../../stores/project";
import type { TrackLatencyReadoutResponse } from "../../stores/api";

const READOUTS: Record<string, TrackLatencyReadoutResponse> = {
  a: {
    readout: {
      track: "a",
      reported_samples: 0,
      requested_offset_samples: 0,
      applied_offset_samples: 0,
      offset_clamped: false,
      effective_samples: 0,
      bypassed: false,
      destinations: [
        { destination: "master", reference_samples: 512, compensation_samples: 512, diagnostic_delta: null },
        { destination: "X", reference_samples: 128, compensation_samples: 128, diagnostic_delta: -128 },
      ],
    },
    note: null,
  },
  b: {
    readout: {
      track: "b",
      reported_samples: 128,
      requested_offset_samples: 0,
      applied_offset_samples: 0,
      offset_clamped: false,
      effective_samples: 128,
      bypassed: false,
      destinations: [
        { destination: "X", reference_samples: 128, compensation_samples: 0, diagnostic_delta: null },
      ],
    },
    note: null,
  },
  c: {
    readout: {
      track: "c",
      reported_samples: 512,
      requested_offset_samples: 0,
      applied_offset_samples: 0,
      offset_clamped: false,
      effective_samples: 512,
      bypassed: false,
      destinations: [
        { destination: "master", reference_samples: 512, compensation_samples: 0, diagnostic_delta: null },
      ],
    },
    note: null,
  },
};

function assertNumbers(container: HTMLElement, expected: TrackLatencyReadoutResponse) {
  const readout = expected.readout;
  expect(container.querySelector(".latency-readout")?.getAttribute("data-track")).toBe(readout.track);
  expect(container.querySelector('[data-latency="reported"]')?.textContent).toBe(
    String(readout.reported_samples),
  );
  expect(container.querySelector('[data-latency="manual"]')?.textContent).toBe(
    readout.applied_offset_samples > 0
      ? `+${readout.applied_offset_samples}`
      : String(readout.applied_offset_samples),
  );
  const rows = [...container.querySelectorAll(".latency-readout-destination")];
  expect(rows.length).toBe(readout.destinations.length);
  readout.destinations.forEach((destination, index) => {
    const row = rows[index];
    expect(row.getAttribute("data-destination")).toBe(destination.destination);
    expect(row.querySelector('[data-latency="reference"]')?.textContent).toBe(
      String(destination.reference_samples),
    );
    expect(row.querySelector('[data-latency="compensation"]')?.textContent).toBe(
      String(destination.compensation_samples),
    );
    const diagnostic = row.querySelector('[data-latency="diagnostic"]');
    if (destination.diagnostic_delta === null) {
      expect(diagnostic).toBeNull();
    } else {
      expect(diagnostic?.textContent).toContain(String(destination.diagnostic_delta));
    }
  });
}

describe("LatencyReadout", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("shows the reported latency and the manual offset as separate numbers", () => {
    const { container } = render(() => <LatencyReadout readout={READOUTS.a} />);
    assertNumbers(container, READOUTS.a);
    // Two separate fields, never one summed number.
    expect(container.querySelectorAll('[data-latency="reported"], [data-latency="manual"]').length).toBe(2);
  });

  it("matches the 0/128/512 plan for every track", () => {
    for (const name of ["a", "b", "c"]) {
      const { container } = render(() => <LatencyReadout readout={READOUTS[name]} />);
      assertNumbers(container, READOUTS[name]);
    }
  });

  it("names the destination reference and the compensation for each route", () => {
    const { container } = render(() => <LatencyReadout readout={READOUTS.a} />);
    const rows = [...container.querySelectorAll(".latency-readout-destination")];
    expect(rows.map((row) => row.getAttribute("data-destination"))).toEqual(["master", "X"]);
    expect(rows[0].textContent).toContain("reference 512 smp");
    expect(rows[0].textContent).toContain("compensation 512 smp");
    expect(rows[1].textContent).toContain("reference 128 smp");
    expect(rows[1].textContent).toContain("compensation 128 smp");
    expect(rows[1].textContent).toContain("-128 smp");
  });

  it("reports a clamped offset, a bypassed pass and the plugin note", () => {
    const clamped: TrackLatencyReadoutResponse = {
      readout: {
        ...READOUTS.b.readout,
        requested_offset_samples: 200,
        applied_offset_samples: 128,
        offset_clamped: true,
        bypassed: true,
      },
      note: "hosted plugin latency is not part of an inspector readout",
    };
    const { container } = render(() => <LatencyReadout readout={clamped} />);
    assertNumbers(container, clamped);
    expect(container.querySelector('[data-latency="clamped"]')).toBeTruthy();
    expect(container.querySelector('[data-latency="bypassed"]')).toBeTruthy();
    expect(container.querySelector('[data-latency="note"]')?.textContent).toContain(
      "hosted plugin latency",
    );
  });

  it("renders nothing without a readout", () => {
    const { container } = render(() => <LatencyReadout readout={null} />);
    expect(container.querySelector(".latency-readout")).toBeNull();
  });

  it("is wired into the track inspector", async () => {
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "track_latency_readout") return READOUTS.a;
      if (cmd === "get_transport_state") {
        return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      }
      if (cmd === "get_undo_state") {
        return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      }
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    await store.initProject();
    const { container, findByText } = render(() => <Mixer />);
    await findByText("Latency");
    assertNumbers(container, READOUTS.a);
    expect(invokeMock).toHaveBeenCalledWith("track_latency_readout", { trackIndex: 0 });
  });
});
