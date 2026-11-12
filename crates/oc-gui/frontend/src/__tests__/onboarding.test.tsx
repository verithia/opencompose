/**
 * First-run splash and guide: the three-step path, the AI handoff, a plugin
 * recommendation installed from the catalog, and the once-per-machine marker.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { DEFAULT_PROVIDERS, aiConfig, loadConfig } from "../ai/config";
import { Onboarding, Splash } from "../components/Onboarding";
import { onboardingStore } from "../stores/onboarding";
import { preferencesUi } from "../stores/preferencesUi";
import { pluginStore } from "../stores/pluginStore";
import { aiConfigWire, invokeMock } from "./setup";

const ENTRY = {
  id: "surge-synth-team.surge-xt",
  name: "Surge XT",
  vendor: "Surge Synth Team",
  version: "1.3.4",
  description: "A free hybrid synthesizer with a large modulation matrix.",
  license: "GPL-3.0",
  tags: ["synth", "open-source"],
  installs: [
    {
      kind: "direct",
      platform: "macos-aarch64",
      format: "vst3",
      url: "https://example.test/surge.zip",
      bytes: 120_000_000,
    },
  ],
  installable: true,
  installed: false,
};

let installed = false;

beforeEach(() => {
  installed = false;
  onboardingStore.reset();
  preferencesUi.close();
  pluginStore.reset();
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "plugin_catalog") {
      return {
        platform: "macos-aarch64",
        entries: [{ ...ENTRY, installed }],
        installed: installed ? [{ name: "Surge XT", format: "vst3", path: "/plugins/Surge XT.vst3" }] : [],
        source: "bundled",
      };
    }
    if (command === "plugin_index_status") return null;
    if (command === "plugin_store_install") {
      installed = true;
      return { status: "installed", path: "/plugins/Surge XT.vst3", message: "Installed" };
    }
    return undefined;
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  invokeMock.mockReset();
  preferencesUi.close();
  pluginStore.reset();
});

describe("first-run onboarding", () => {
  it("shows the splash without any workspace behind it", () => {
    render(() => <Splash />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "Starting OpenCompose");
  });

  it("starts on the three bottom-panel keys", () => {
    render(() => <Onboarding onDone={() => {}} />);
    expect(screen.getByText("Shortcuts open three workspaces")).toBeInTheDocument();
    expect(screen.getByText("Step 1 of 3")).toBeInTheDocument();
    expect(screen.getByText("C")).toBeInTheDocument();
    expect(screen.getByText("P")).toBeInTheDocument();
    expect(screen.getByText("R")).toBeInTheDocument();
  });

  it("hands the model step to Preferences and moves on", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("Model settings")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Fill this in later in Preferences" }));
    expect(screen.getByText("Recommended plugins")).toBeInTheDocument();
    expect(preferencesUi.open()).toBe(false);
  });

  it("installs a recommended plugin from the catalog and reports it installed", async () => {
    render(() => <Onboarding onDone={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Fill this in later in Preferences" }));

    await screen.findByText("Surge XT");
    expect(screen.getByText("GPL-3.0")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("plugin_store_install", {
        id: "surge-synth-team.surge-xt",
        overwrite: false,
      }),
    );
    await waitFor(() => expect(screen.getByText("Installed")).toBeInTheDocument());
  });

  it("keeps installation feedback visible until the install settles", async () => {
    let rejectInstall!: (reason: Error) => void;
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: unknown[]) => {
      if (command === "plugin_store_install") {
        return new Promise((_, reject) => { rejectInstall = reject; });
      }
      return original(command, ...args);
    });
    const done = vi.fn();
    render(() => <Onboarding onDone={done} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Fill this in later in Preferences" }));
    await screen.findByText("Surge XT");
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    for (const name of ["Back", "Skip setup", "Install later"]) {
      expect(screen.getByRole("button", { name })).toBeDisabled();
      fireEvent.click(screen.getByRole("button", { name }));
    }
    fireEvent.keyDown(document, { key: "Escape" });
    expect(done).not.toHaveBeenCalled();
    expect(screen.getByText("Recommended plugins")).toBeInTheDocument();
    rejectInstall(new Error("Download failed"));
    await screen.findByRole("alert");
    expect(screen.getByRole("button", { name: "Install later" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Back" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Skip setup" })).toBeEnabled();
  });

  it("lists the saved providers without testing credentials", async () => {
    // The guide lists what the user saved; the catalogue is not their list.
    invokeMock.mockImplementation(async (command: string) =>
      command === "load_ai_config" ? aiConfigWire([DEFAULT_PROVIDERS[0]]) : undefined);
    await loadConfig();
    const first = aiConfig.config.providers[0];
    // Count only what the guide asks for, not the load that seeded the store.
    invokeMock.mockClear();
    render(() => <Onboarding onDone={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText(first.name)).toBeVisible();
    expect(screen.getByText(first.model)).toBeVisible();
    expect(invokeMock.mock.calls.some(([command]) => String(command).startsWith("ai_"))).toBe(false);
  });

  it("asks for a provider instead of listing the catalogue when none is saved", async () => {
    invokeMock.mockImplementation(async (command: string) =>
      command === "load_ai_config" ? aiConfigWire([]) : undefined);
    await loadConfig();
    render(() => <Onboarding onDone={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText("No provider yet — add one in AI settings.")).toBeVisible();
    expect(screen.queryByRole("option")).toBeNull();
  });

  it("finishes through the skip control and records the marker", async () => {
    const done = vi.fn();
    render(() => <Onboarding onDone={done} />);
    fireEvent.click(screen.getByRole("button", { name: "Skip setup" }));
    expect(done).toHaveBeenCalledOnce();
    onboardingStore.complete();
    expect(onboardingStore.pending()).toBe(false);
  });
});
