/**
 * The two preferences-bundle actions in the preferences popover: each opens the
 * native picker for its direction and hands the chosen path to the dialog.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openDialogMock, saveDialogMock, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { preferencesUi } from "../../stores/preferencesUi";
import { audioIoStore } from "../../stores/audioIo";
import { preferencesStore } from "../../stores/preferencesBundle";
import { Toolbar } from "../../components/Toolbar";

function mockCommands() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") {
      return {
        can_undo: false,
        can_redo: false,
        undo_label: null,
        redo_label: null,
        undo_labels: [],
        redo_labels: [],
      };
    }
    if (cmd === "get_midi_config") {
      return { input: null, monitor: false, active: false, audition_active: false, error: null };
    }
    if (cmd === "list_midi_ports") return { supported: true, inputs: [], outputs: [], error: null };
    if (cmd === "get_midi_activity") {
      return { running: false, events: 0, last: null, error: null, audition_active: false };
    }
    if (cmd === "get_audio_io" || cmd === "list_audio_io") {
      return {
        status: {
          selection: {
            output_device: null,
            output_device_name: null,
            sample_rate: null,
            buffer_frames: null,
          },
          opened: null,
          devices: [],
          default_output: null,
          missing_device: false,
          notices: [],
          failure: null,
          stream_open: false,
          input: {
            input_device: null,
            input_device_name: null,
            input_channels: "mono",
            input_monitor: false,
            input_monitor_gain_db: 0,
          },
          inputs: [],
          default_input: null,
          opened_input: null,
          missing_input: false,
          input_notices: [],
          input_failure: null,
        },
        callback_frames: 0,
        latency_frames: 0,
        latency_seconds: 0,
        underruns: 0,
        input: {
          open: false,
          device: null,
          channels: 1,
          sample_rate: 0,
          level_peak: 0,
          clipped: false,
          monitoring: false,
          monitor_gain_db: 0,
          error: null,
        },
        input_latency_frames: 0,
      };
    }
    return makeSnapshot();
  });
}

async function openPreferences(container: HTMLElement) {
  fireEvent.click(container.querySelector(".preferences-btn") as HTMLElement);
  await vi.waitFor(() => expect(container.querySelector(".preferences-modal")).toBeTruthy());
  fireEvent.click(container.querySelector('[data-page="storage"]') as HTMLElement);
  await vi.waitFor(() => expect(container.querySelector(".preferences-export-btn")).toBeTruthy());
}

function actionButton(container: HTMLElement, label: string): HTMLButtonElement {
  const found = [...container.querySelectorAll("button")]
    .find((element) => (element.textContent ?? "").trim() === label);
  if (!found) throw new Error(`no button labelled ${label}`);
  return found as HTMLButtonElement;
}

beforeEach(async () => {
  preferencesUi.close();
  i18n.setLocale("en");
  audioIoStore.dispose();
  preferencesStore.reset();
  invokeMock.mockReset();
  saveDialogMock.mockReset();
  openDialogMock.mockReset();
  mockCommands();
  await store.initProject();
});

afterEach(() => {
  audioIoStore.dispose();
  preferencesStore.reset();
});

describe("the preferences-bundle actions", () => {
  it("opens the save picker and requests an export of the chosen path", async () => {
    saveDialogMock.mockResolvedValue("/tmp/prefs.json");
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.click(actionButton(container, i18n.t("preferences.export")));

    await vi.waitFor(() => expect(preferencesStore.request()).not.toBeNull());
    expect(saveDialogMock).toHaveBeenCalled();
    expect(preferencesStore.request()).toEqual({ kind: "export", path: "/tmp/prefs.json" });
    // The popover steps out of the way of the dialog.
    expect(container.querySelector(".preferences-modal")).toBeFalsy();
  });

  it("opens the open picker and requests an import of the chosen path", async () => {
    openDialogMock.mockResolvedValue("/tmp/other.json");
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.click(actionButton(container, i18n.t("preferences.import")));

    await vi.waitFor(() => expect(preferencesStore.request()).not.toBeNull());
    expect(openDialogMock).toHaveBeenCalledWith(
      expect.objectContaining({ multiple: false }),
    );
    expect(preferencesStore.request()).toEqual({ kind: "import", path: "/tmp/other.json" });
  });

  it("reports a picker that cannot open", async () => {
    saveDialogMock.mockRejectedValue(new Error("no native dialog"));
    const { baseElement: container } = render(() => <Toolbar />);
    await openPreferences(container);

    fireEvent.click(actionButton(container, i18n.t("preferences.export")));

    await vi.waitFor(() => expect(preferencesStore.pickError()).not.toBeNull());
    expect(preferencesStore.pickError()).toBe(
      i18n.t("preferences.pickExportFailed", { error: "Error: no native dialog" }),
    );
    expect(container.querySelector(".preferences-bundle-error-hint")?.textContent).toContain(
      "no native dialog",
    );
    expect(preferencesStore.request()).toBeNull();
  });
});
