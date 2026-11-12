/**
 * The preferences bundle dialog: an import is validated first, replaces only
 * after the confirmation, and lists every field it skipped with its reason.
 */
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PreferencesDialog } from "../../components/PreferencesDialog";
import { i18n } from "../../stores/i18n";
import {
  applyPreferences,
  exportPreferencesFile,
  importPreferencesFile,
  type PreferencesBundle,
  type PreferencesImportPreview,
} from "../../stores/preferencesBundle";

vi.mock("../../stores/preferencesBundle", () => ({
  exportPreferencesFile: vi.fn(),
  importPreferencesFile: vi.fn(),
  applyPreferences: vi.fn(),
  preferencesStore: { request: () => null, pickError: () => null, close: () => {} },
}));

async function settle(rounds = 4) {
  for (let index = 0; index < rounds; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function bundle(): PreferencesBundle {
  return {
    schema: "opencompose.preferences.v1",
    exported_at: "2026-10-31T10:00:00.000Z",
    app_version: "0.1.0",
    theme: null,
    locale: null,
    ui_scale: null,
    audio: null,
    midi: { input: null, channel: 3, monitor: null },
    export: null,
    grid: null,
    keymap: null,
    workspace: null,
  };
}

function preview(overrides: Partial<PreferencesImportPreview> = {}): PreferencesImportPreview {
  return {
    bundle: bundle(),
    applied: Array.from({ length: 12 }, (_, index) => `setting.${index}`),
    skipped: [],
    errors: [],
    ...overrides,
  };
}

function button(root: ParentNode, label: string): HTMLButtonElement {
  const found = [...root.querySelectorAll("button")]
    .find((element) => (element.textContent ?? "").trim() === label);
  if (!found) throw new Error(`no button labelled ${label}`);
  return found as HTMLButtonElement;
}

function text(root: ParentNode): string {
  return (root.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** The dialog portals into the body, so the body is the query root. */
const body = () => document.body;

beforeEach(() => {
  i18n.setLocale("en");
  vi.mocked(applyPreferences).mockReset();
  vi.mocked(exportPreferencesFile).mockReset();
  vi.mocked(importPreferencesFile).mockReset();
});

afterEach(() => {
  cleanup();
});

describe("importing a bundle", () => {
  it("asks before replacing and applies only after the confirmation", async () => {
    vi.mocked(importPreferencesFile).mockResolvedValue(preview());
    vi.mocked(applyPreferences).mockResolvedValue({ applied: ["midi.monitor"], skipped: [] });
    const onClose = vi.fn();
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={onClose} />
    ));
    await settle();

    expect(text(body())).toContain("Replace 12 settings? Changes apply immediately.");
    expect(applyPreferences).not.toHaveBeenCalled();

    fireEvent.click(button(body(), i18n.t("preferences.apply")));
    await settle();

    expect(applyPreferences).toHaveBeenCalledWith(bundle());
    expect(text(body())).toContain(i18n.t("preferences.applied", { count: 1 }));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes without applying when the confirmation is cancelled", async () => {
    vi.mocked(importPreferencesFile).mockResolvedValue(preview());
    const onClose = vi.fn();
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={onClose} />
    ));
    await settle();

    fireEvent.click(button(body(), i18n.t("preferences.cancel")));
    expect(applyPreferences).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("lists every skipped field with the reason the store gave", async () => {
    vi.mocked(importPreferencesFile).mockResolvedValue(preview({
      skipped: [{ field: "workspace.recent_directories", reason: "this build cannot write the recent-directory list" }],
    }));
    vi.mocked(applyPreferences).mockResolvedValue({
      applied: ["theme", "locale"],
      skipped: [
        { field: "audio.sample_rate", reason: "96000 Hz is not offered by 'Built-in Output'" },
        { field: "midi.channel", reason: "this build has no MIDI channel preference to write" },
      ],
    });
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();
    fireEvent.click(button(body(), i18n.t("preferences.apply")));
    await settle();

    const rendered = text(body());
    expect(rendered).toContain(i18n.t("preferences.applied", { count: 2 }));
    expect(rendered).toContain(i18n.t("preferences.skippedTitle", { count: 3 }));
    expect(rendered).toContain("96000 Hz is not offered by 'Built-in Output'");
    expect(rendered).toContain("midi.channel");
    expect(rendered).toContain("workspace.recent_directories");
  });

  it("shows the backend message for JSON it cannot read", async () => {
    vi.mocked(importPreferencesFile).mockRejectedValue(new Error(
      "Not an OpenCompose preferences file: /tmp/prefs.json is not JSON (expected value at line 1 column 1).",
    ));
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();

    expect(text(body())).toContain("Not an OpenCompose preferences file");
    expect(text(body())).not.toContain(i18n.t("preferences.apply"));
    expect(applyPreferences).not.toHaveBeenCalled();
  });

  it("shows a refusal the validator returned and applies nothing", async () => {
    vi.mocked(importPreferencesFile).mockResolvedValue(preview({
      applied: [],
      errors: ["Not an OpenCompose preferences file: schema 'opencompose.preferences.v2' is not opencompose.preferences.v1."],
    }));
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();

    expect(text(body())).toContain(
      "Not an OpenCompose preferences file: schema 'opencompose.preferences.v2' is not opencompose.preferences.v1.",
    );
    expect(applyPreferences).not.toHaveBeenCalled();
  });

  it("says so when the file carries nothing this build can apply", async () => {
    vi.mocked(importPreferencesFile).mockResolvedValue(preview({ applied: [] }));
    render(() => (
      <PreferencesDialog request={{ kind: "import", path: "/tmp/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();

    expect(text(body())).toContain(i18n.t("preferences.nothing"));
    expect(applyPreferences).not.toHaveBeenCalled();
  });
});

describe("exporting a bundle", () => {
  it("writes the file and reports how many settings it carried", async () => {
    vi.mocked(exportPreferencesFile).mockResolvedValue(9);
    render(() => (
      <PreferencesDialog request={{ kind: "export", path: "/tmp/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();

    expect(exportPreferencesFile).toHaveBeenCalledWith("/tmp/prefs.json");
    expect(text(body())).toContain(
      i18n.t("preferences.exported", { count: 9, path: "/tmp/prefs.json" }),
    );
  });

  it("shows the path the writer could not use", async () => {
    vi.mocked(exportPreferencesFile).mockRejectedValue(
      new Error("Could not write /read-only/prefs.json: Permission denied (os error 13)."),
    );
    render(() => (
      <PreferencesDialog request={{ kind: "export", path: "/read-only/prefs.json" }} onClose={vi.fn()} />
    ));
    await settle();

    expect(text(body())).toContain("Could not write /read-only/prefs.json");
  });
});
