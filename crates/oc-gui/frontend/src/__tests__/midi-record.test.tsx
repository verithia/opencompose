/**
 * MIDI recording on the transport surface: what a press will record, the input
 * quantise and overdub settings, and the punch fields.
 *
 * The backend owns the punch range and the take settings, so every case drives
 * the control the user would touch and asserts the command and the state the
 * reply puts on screen.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent } from "@solidjs/testing-library";
import {
  loadAuditProject,
  mountSurface,
  resetSurface,
  settle,
} from "./ui-audit-support";
import { expectInvoked, invokeMock, makeSnapshot } from "./setup";
import { Toolbar } from "../components/Toolbar";
import { Timeline } from "../components/Timeline";
import { store } from "../stores/project";
import { recordStore, type RecordState } from "../stores/record";
import { i18n } from "../stores/i18n";

function record(overrides: Partial<RecordState> = {}): RecordState {
  return {
    armed: null,
    recording: false,
    seconds: 0,
    report: null,
    disabled_reason: null,
    count_in_remaining_frames: 0,
    mode: "audio",
    midi: { target: null, quantise: "off", overdub: false, input: null },
    audio_device: null,
    ...overrides,
  };
}

/** Answer the punch and take-settings commands the way the backend does. */
function answerSettings() {
  const base = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    if (command === "set_punch_region") {
      return {
        enabled: typed.enabled === true,
        start_beats: typeof typed.startBeats === "number" ? typed.startBeats : null,
        end_beats: typeof typed.endBeats === "number" ? typed.endBeats : null,
      };
    }
    if (command === "set_midi_record_settings") {
      return {
        target: null,
        quantise: typed.quantise ?? recordStore.quantise(),
        overdub: typeof typed.overdub === "boolean" ? typed.overdub : recordStore.overdub(),
        input: null,
      };
    }
    return base(command, args);
  });
}

beforeEach(async () => {
  i18n.setLocale("en");
  recordStore.dispose();
  await loadAuditProject();
  answerSettings();
  // The punch range is session state that survives a project load.
  await store.setPunchRegion(null, null, false);
  invokeMock.mockClear();
});

afterEach(() => {
  recordStore.dispose();
  resetSurface();
  i18n.setLocale("en");
});

describe("the arm chip says what a press will record", () => {
  it("names the pattern a MIDI take lands in", () => {
    recordStore.adoptRecord(
      record({
        armed: "Lead",
        mode: "midi",
        midi: { target: "Pattern 3", quantise: "off", overdub: false, input: "Keys" },
      }),
    );
    expect(recordStore.armLine()).toBe("Recording MIDI into Pattern 3");
  });

  it("says a new pattern is created when the playhead is over no clip", () => {
    recordStore.adoptRecord(
      record({ armed: "Lead", mode: "midi", midi: { target: null, quantise: "off", overdub: false, input: "Keys" } }),
    );
    expect(recordStore.armLine()).toBe("Recording MIDI into a new pattern for Lead");
  });

  it("names the device an audio take captures", () => {
    recordStore.adoptRecord(
      record({ armed: "Vox", mode: "audio", audio_device: "Scarlett 2i2" }),
    );
    expect(recordStore.armLine()).toBe("Recording audio from Scarlett 2i2");
  });

  it("says nothing while no track is armed, and nothing while a take runs", () => {
    expect(recordStore.armLine()).toBeNull();
    recordStore.adoptRecord(record({ armed: "Lead", mode: "midi", recording: true }));
    expect(recordStore.armLine()).toBeNull();
  });

  it("renders the MIDI chip in the transport bar", async () => {
    recordStore.adoptRecord(
      record({
        armed: "Lead",
        mode: "midi",
        midi: { target: "Pattern 3", quantise: "off", overdub: false, input: "Keys" },
      }),
    );
    const view = await mountSurface(() => <Toolbar />);
    const chip = view.container.querySelector("[data-record-arm-chip]");
    expect(chip?.textContent).toBe("Recording MIDI into Pattern 3");
  });

  it("renders the audio chip in the transport bar", async () => {
    recordStore.adoptRecord(
      record({ armed: "Vox", mode: "audio", audio_device: "Scarlett 2i2" }),
    );
    const view = await mountSurface(() => <Toolbar />);
    const chip = view.container.querySelector("[data-record-arm-chip]");
    expect(chip?.textContent).toBe("Recording audio from Scarlett 2i2");
  });
});

describe("stopping a take", () => {
  it("adopts the project the committed notes were written into", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "stop_recording") {
        return {
          project: makeSnapshot({ name: "with take" }),
          record: record({ report: "Recorded 2 notes into hook" }),
        };
      }
      return makeSnapshot();
    });
    recordStore.adoptRecord(
      record({
        armed: "Lead",
        mode: "midi",
        midi: { target: "hook", quantise: "off", overdub: false, input: "Keys" },
      }),
    );
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "stop_recording") {
        return {
          project: makeSnapshot({ name: "with take" }),
          record: record({
            armed: "Lead",
            mode: "midi",
            midi: { target: "hook", quantise: "off", overdub: false, input: "Keys" },
            report: "Recorded 2 notes into hook",
          }),
        };
      }
      return makeSnapshot();
    });
    await recordStore.stop();
    expect(store.project.name).toBe("with take");
    expect(recordStore.report()).toBe("Recorded 2 notes into hook");
    // The pattern the take wrote is what the piano roll now shows.
    expect(store.selectedPatternName()).toBe("hook");
  });
});


/** The take settings and punch fields live in the transport-options popover. */
function openTransportOptions(container: HTMLElement) {
  const trigger = container.querySelector<HTMLButtonElement>(".transport-options-toggle");
  expect(trigger).toBeTruthy();
  if (trigger!.getAttribute("aria-expanded") !== "true") fireEvent.click(trigger!);
}

describe("the MIDI take settings", () => {
  it("sends the quantise the user picked and adopts it", async () => {
    const view = await mountSurface(() => <Toolbar />);
    openTransportOptions(view.container);
    const select = document.body.querySelector<HTMLSelectElement>(".record-quantise")!;
    expect(select).toBeTruthy();
    fireEvent.change(select, { target: { value: "sixteenth" } });
    await settle();
    expectInvoked("set_midi_record_settings", { quantise: "sixteenth", overdub: null });
    expect(recordStore.quantise()).toBe("sixteenth");
  });

  it("toggles overdub without touching the quantise", async () => {
    recordStore.adoptRecord(
      record({ armed: "Lead", mode: "midi", midi: { target: "hook", quantise: "eighth", overdub: false, input: "Keys" } }),
    );
    const view = await mountSurface(() => <Toolbar />);
    openTransportOptions(view.container);
    const button = document.body.querySelector<HTMLButtonElement>(".record-overdub")!;
    expect(button.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(button);
    await settle();
    expectInvoked("set_midi_record_settings", { quantise: null, overdub: true });
    expect(recordStore.overdub()).toBe(true);
  });
});

describe("the punch fields", () => {
  it("shows the live range and sends both bounds when one is edited", async () => {
    const view = await mountSurface(() => <Toolbar />);
    openTransportOptions(view.container);
    const punchIn = document.body.querySelector<HTMLInputElement>(".record-punch-in")!;
    const punchOut = document.body.querySelector<HTMLInputElement>(".record-punch-out")!;
    expect(punchIn.value).toBe("0");
    expect(punchOut.value).toBe("8");
    fireEvent.change(punchIn, { target: { value: "2" } });
    await settle();
    expectInvoked("set_punch_region", { startBeats: 2, endBeats: 8, enabled: false });
    expect(store.punchRegion()).toEqual({ start: 2, end: 8 });
    expect(store.punch().enabled).toBe(false);
  });

  it("arms and disarms the range from the toggle", async () => {
    const view = await mountSurface(() => <Toolbar />);
    openTransportOptions(view.container);
    const toggle = document.body.querySelector<HTMLButtonElement>(".record-punch-toggle")!;
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(toggle);
    await settle();
    expectInvoked("set_punch_region", { startBeats: 0, endBeats: 8, enabled: true });
    expect(store.punch().enabled).toBe(true);
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
  });

  it("shows the backend's refusal verbatim and keeps the old range", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "set_punch_region") {
        throw new Error("The punch out must be at least a 64th note after the punch in.");
      }
      return makeSnapshot();
    });
    const ok = await store.setPunchRegion(4, 3, true);
    expect(ok).toBe(false);
    expect(store.punchError()).toContain("64th note");
    expect(store.punchRegion()).toBeNull();
  });

  it("draws the armed range on the ruler", async () => {
    await store.setPunchRegion(2, 6, true);
    const view = await mountSurface(() => <Timeline />);
    const band = view.container.querySelector<HTMLElement>("[data-punch-band]");
    expect(band).toBeTruthy();
    expect(band!.classList.contains("punch-armed")).toBe(true);
  });
});
