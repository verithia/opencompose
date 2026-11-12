/**
 * Toolbar live MIDI preferences — device selection, monitor toggle, activity,
 * and backend-absent behaviour.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, expectInvoked, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { preferencesUi } from "../../stores/preferencesUi";
import { midiStore } from "../../stores/midiLive";
import { Toolbar } from "../../components/Toolbar";

const PORTS = {
  supported: true,
  inputs: [
    { index: 0, name: "Keys", id: "in-0" },
    { index: 1, name: "Pads", id: "in-1" },
  ],
  outputs: [],
  error: null,
};

const ACTIVITY = {
  running: true,
  events: 3,
  last: {
    kind: "note_on",
    channel: 0,
    note: 60,
    velocity: 100,
    controller: null,
    value: null,
    millis: 20,
  },
  error: null,
  audition_active: false,
};

interface MockOptions {
  config?: Record<string, unknown>;
  ports?: Record<string, unknown>;
  activity?: Record<string, unknown>;
  setConfig?: (request: { input?: string | null; monitor?: boolean }) => Record<string, unknown>;
}

function mockCommands(options: MockOptions = {}) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    if (cmd === "get_midi_config") {
      return options.config ?? { input: null, monitor: false, active: false, audition_active: false, error: null };
    }
    if (cmd === "list_midi_ports") return options.ports ?? PORTS;
    if (cmd === "get_midi_activity") {
      return options.activity ?? { running: false, events: 0, last: null, error: null, audition_active: false };
    }
    if (cmd === "set_midi_config") {
      const request = (args ?? {}) as { input?: string | null; monitor?: boolean };
      if (options.setConfig) return options.setConfig(request);
      return {
        input: request.input ?? null,
        monitor: Boolean(request.monitor),
        active: Boolean(request.monitor && request.input),
        audition_active: false,
        error: null,
      };
    }
    return makeSnapshot();
  });
}

async function openPreferences(container: HTMLElement) {
  fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
  await vi.waitFor(() => expect(container.querySelector(".preferences-modal")).toBeTruthy());
  fireEvent.click(container.querySelector('[data-page="audio"]') as HTMLElement);
  await vi.waitFor(() =>
    expect(container.querySelector(".midi-input-select, .midi-unavailable")).toBeTruthy(),
  );
}

beforeEach(async () => {
  preferencesUi.close();
  i18n.setLocale("en");
  midiStore.dispose();
  invokeMock.mockReset();
  mockCommands();
  await store.initProject();
});

afterEach(() => {
  midiStore.dispose();
});

describe("Toolbar live MIDI preferences", () => {
  it("lists input ports and keeps the monitor disabled until one is selected", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    const select = container.querySelector(".midi-input-select") as HTMLSelectElement;
    expect(Array.from(select.options).map((option) => option.value)).toEqual(["", "Keys", "Pads"]);
    expect((container.querySelector(".midi-monitor-toggle") as HTMLInputElement).disabled).toBe(true);
  });

  it("persists a selected input without enabling the monitor", async () => {
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.change(container.querySelector(".midi-input-select") as HTMLSelectElement, {
      target: { value: "Keys" },
    });

    await vi.waitFor(() => expectInvoked("set_midi_config", { input: "Keys", monitor: false }));
    await vi.waitFor(() => {
      expect((container.querySelector(".midi-monitor-toggle") as HTMLInputElement).disabled).toBe(false);
    });
  });

  it("starts monitoring and shows the last note and velocity", async () => {
    mockCommands({ activity: ACTIVITY });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.change(container.querySelector(".midi-input-select") as HTMLSelectElement, {
      target: { value: "Keys" },
    });
    await vi.waitFor(() => expectInvoked("set_midi_config", { input: "Keys", monitor: false }));

    fireEvent.click(container.querySelector(".midi-monitor-toggle") as HTMLInputElement);

    await vi.waitFor(() => expectInvoked("set_midi_config", { input: "Keys", monitor: true }));
    await vi.waitFor(() => {
      expect(container.querySelector(".midi-activity")?.textContent).toContain("Last note: C4 · vel 100");
    });
  });

  it("shows an unavailable state when the MIDI backend is absent", async () => {
    mockCommands({
      ports: { supported: false, inputs: [], outputs: [], error: "MIDI support is unavailable" },
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    expect(container.querySelector(".midi-input-select")).toBeFalsy();
    expect(container.querySelector(".midi-unavailable")?.textContent).toContain("unavailable");
    expect(container.querySelector(".midi-monitor-toggle")).toBeFalsy();
  });

  it("keeps a disconnected saved device visible as unavailable", async () => {
    mockCommands({
      config: { input: "Gone", monitor: false, active: false, audition_active: false, error: null },
    });
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    const select = container.querySelector(".midi-input-select") as HTMLSelectElement;
    expect(select.value).toBe("Gone");
    expect(Array.from(select.options).map((option) => option.textContent)).toContain("Gone (unavailable)");
  });

  it("translates the live MIDI controls", async () => {
    mockCommands({
      ports: { supported: false, inputs: [], outputs: [], error: null },
    });
    const { baseElement: container } = render(() => <Toolbar />);
    i18n.setLocale("zh-Hans");
    await openPreferences(container);

    expect(container.querySelector(".midi-live-section")?.textContent).toContain("MIDI 输入");
    expect(container.querySelector(".midi-unavailable")?.textContent).toContain("MIDI 不可用");
  });
});
